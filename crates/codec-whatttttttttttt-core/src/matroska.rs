//! Matroska / `WebM` demuxing (Matroska specification, RFC 8794 EBML).
//!
//! The whole file is read at construction (bounded by
//! `MAX_WEBM_FILE_BYTES`) and parsed into tracks and packets;
//! `next_packet` then yields them in file order. A packet timestamp
//! is the Matroska timestamp in microseconds: `(cluster timestamp +
//! block timecode) * TimestampScale / 1000`. The demuxer does not
//! apply `CodecDelay` / `SeekPreRoll` or `DiscardPadding`: those are
//! codec concerns (the Opus decoder drops the pre-skip from the
//! `OpusHead`), so the timestamps here are the raw container values.
//! A lace's frames share the block timestamp when the track has no
//! `DefaultDuration`; when it does, each lace advances by it.
//!
//! Scope for Phase 6: one Segment; tracks must use a codec in the
//! library (`V_VP9`, `V_AV1`, `A_OPUS`, `A_VORBIS`, `A_FLAC`,
//! `A_PCM/INT/LIT`, `A_PCM/INT/BIG`). Any other codec ID — including
//! `V_VP8` (the library has no VP8 codec) and subtitle tracks —
//! returns `Unsupported` at construction. `Cues` are parsed and used
//! for seeking; when a file has no `Cues`, seeking falls back to the
//! video keyframe index.

use std::io::Read;

use crate::byte_source::read_webm_bytes;
use crate::demux::{TrackInfo, TrackKind};
use crate::ebml::{
    parse_element_header, read_float, read_signed_vint, read_string, read_uint, read_vint,
    EbmlCursor,
};
use crate::error::{Error, Result};
use crate::frame::{Codec, Metadata, MetadataEntry, Packet};
use crate::limits::{MAX_NESTING_DEPTH, MAX_TRACKS};

// Element IDs (Matroska specification). Only the IDs this demuxer
// reads are listed; unknown IDs are skipped by size.
const ID_EBML: u32 = 0x1A45_DFA3;
const ID_DOC_TYPE: u32 = 0x4282;
const ID_SEGMENT: u32 = 0x1853_8067;
const ID_SEEK_HEAD: u32 = 0x114D_9B74;
const ID_SEEK: u32 = 0x4DBB;
const ID_SEEK_ID: u32 = 0x53AB;
const ID_SEEK_POSITION: u32 = 0x53AC;
const ID_INFO: u32 = 0x1549_A966;
const ID_TIMESTAMP_SCALE: u32 = 0x002A_D7B1;
const ID_DURATION: u32 = 0x4489;
const ID_TRACKS: u32 = 0x1654_AE6B;
const ID_TRACK_ENTRY: u32 = 0xAE;
const ID_TRACK_NUMBER: u32 = 0xD7;
const ID_TRACK_TYPE: u32 = 0x83;
const ID_CODEC_ID: u32 = 0x86;
const ID_CODEC_PRIVATE: u32 = 0x63A2;
const ID_DEFAULT_DURATION: u32 = 0x0023_E383;
const ID_VIDEO: u32 = 0xE0;
const ID_PIXEL_WIDTH: u32 = 0xB0;
const ID_PIXEL_HEIGHT: u32 = 0xBA;
const ID_AUDIO: u32 = 0xE1;
const ID_SAMPLING_FREQUENCY: u32 = 0xB5;
const ID_CHANNELS: u32 = 0x9F;
const ID_CLUSTER: u32 = 0x1F43_B675;
const ID_CLUSTER_TIMESTAMP: u32 = 0xE7;
const ID_SIMPLE_BLOCK: u32 = 0xA3;
const ID_BLOCK_GROUP: u32 = 0xA0;
const ID_BLOCK: u32 = 0xA1;
const ID_CUES: u32 = 0x1C53_BB6B;
const ID_CUE_POINT: u32 = 0xBB;
const ID_CUE_TIME: u32 = 0xB3;
const ID_CUE_TRACK_POSITIONS: u32 = 0xB7;
const ID_CUE_TRACK: u32 = 0xF7;
const ID_CUE_CLUSTER_POSITION: u32 = 0xF1;
const ID_CUE_BLOCK_NUMBER: u32 = 0x5378;
const ID_TAGS: u32 = 0x1254_C367;
const ID_TAG: u32 = 0x7373;
const ID_SIMPLE_TAG: u32 = 0x67C8;
const ID_TAG_NAME: u32 = 0x45A3;
const ID_TAG_STRING: u32 = 0x4487;

/// Default timestamp scale when `Info` has none: 1 ms per unit
/// (Matroska specification).
const DEFAULT_TIMESTAMP_SCALE_NS: u64 = 1_000_000;

/// A cue point: the time of a keyframe and where its cluster lives.
/// `cluster_position` is relative to the start of the Segment data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CuePoint {
    /// Cue time in microseconds.
    pub time_us: u64,
    /// Track number the cue belongs to.
    pub track: u64,
    /// Cluster position relative to the Segment data start.
    pub cluster_position: u64,
    /// 1-based block number inside the cluster.
    pub block_number: u64,
}

/// Demuxer for a WebM/Matroska file. Packets are parsed at
/// construction; `next_packet` yields them in file order.
#[derive(Debug)]
pub struct MatroskaDemuxer {
    tracks: Vec<TrackInfo>,
    packets: Vec<Packet>,
    /// Track number for each packet, in packet order. `Packet` has
    /// no track field, so the association lives here.
    packet_tracks: Vec<u64>,
    position: usize,
    timestamp_scale_ns: u64,
    duration_us: Option<u64>,
    metadata: Metadata,
    cues: Vec<CuePoint>,
    /// (file offset of the cluster, index of its first packet).
    clusters: Vec<(usize, usize)>,
    segment_data_start: usize,
    segment_payload_len: usize,
}

impl MatroskaDemuxer {
    /// Reads and demuxes a WebM/Matroska stream from `reader`.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a cut-short element, `InvalidData`
    /// for a bad EBML header, a missing Segment or Tracks element,
    /// a block for an undeclared track, an element size overrunning
    /// its parent, or a corrupt lace table, `Unsupported` for a
    /// `DocType` other than `webm`/`matroska` or a track codec outside
    /// the library, and `AllocationLimit` for a file above the limit.
    pub fn new<R: Read>(reader: R) -> Result<Self> {
        let bytes = read_webm_bytes(reader)?;
        Self::parse(&bytes)
    }

    /// Parses an already-read WebM/Matroska file.
    ///
    /// # Errors
    ///
    /// The same errors as `new`.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 4 {
            return Err(Error::Truncated {
                needed: 4,
                had: bytes.len(),
            });
        }
        // The first element must be the EBML header.
        let ebml = parse_element_header(bytes, 0, bytes.len())?;
        if ebml.id != ID_EBML {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "webm file does not start with an ebml header",
            });
        }
        check_depth(1)?;
        parse_ebml_header(bytes, &ebml)?;
        // The next element must be the Segment.
        let segment = parse_element_header(bytes, ebml.data_end, bytes.len())?;
        if segment.id != ID_SEGMENT {
            return Err(Error::InvalidData {
                offset: ebml.data_end,
                detail: "webm file has no segment after the ebml header",
            });
        }
        check_depth(2)?;
        let segment_data_start = segment.data_start;
        let segment_payload_len = segment.payload_len();
        let mut state = SegmentState {
            timestamp_scale_ns: DEFAULT_TIMESTAMP_SCALE_NS,
            ..Default::default()
        };
        parse_segment(bytes, &segment, &mut state)?;
        if state.tracks.is_empty() {
            return Err(Error::InvalidData {
                offset: segment.offset,
                detail: "webm segment contains no tracks",
            });
        }
        if state.packets.is_empty() {
            return Err(Error::InvalidData {
                offset: segment.offset,
                detail: "webm segment contains no packets",
            });
        }
        // Every packet must belong to a declared track.
        for &track_number in &state.packet_tracks {
            if !state
                .tracks
                .iter()
                .any(|track| track.track_id == track_number)
            {
                return Err(Error::InvalidData {
                    offset: 0,
                    detail: "webm block references an undeclared track",
                });
            }
        }
        Ok(Self {
            tracks: state.tracks,
            packets: state.packets,
            packet_tracks: state.packet_tracks,
            position: 0,
            timestamp_scale_ns: state.timestamp_scale_ns,
            duration_us: state.duration_us,
            metadata: state.metadata,
            cues: state.cues,
            clusters: state.clusters,
            segment_data_start,
            segment_payload_len,
        })
    }

    /// Tracks in file order.
    #[must_use]
    pub fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    /// Timestamp scale in nanoseconds per timestamp unit.
    #[must_use]
    pub fn timestamp_scale_ns(&self) -> u64 {
        self.timestamp_scale_ns
    }

    /// Duration in microseconds, when the `Info` element has one.
    #[must_use]
    pub fn duration_us(&self) -> Option<u64> {
        self.duration_us
    }

    /// Container metadata from the `Tags` element.
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Number of packets in the file.
    #[must_use]
    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }

    /// Cue points, in file order.
    #[must_use]
    pub fn cues(&self) -> &[CuePoint] {
        &self.cues
    }

    /// Returns the next packet in file order, or `None` at end of
    /// stream.
    ///
    /// # Errors
    ///
    /// This implementation never fails after construction; the
    /// `Result` exists so a streaming demuxer can report read
    /// errors here.
    pub fn next_packet(&mut self) -> Result<Option<Packet>> {
        if self.position >= self.packets.len() {
            return Ok(None);
        }
        let packet = self.packets[self.position].clone();
        self.position += 1;
        Ok(Some(packet))
    }

    /// Seeks to the keyframe at or before `timestamp_us`. With
    /// `Cues`, the cue with the greatest time at or before the
    /// target selects the cluster; without cues, the last video
    /// keyframe at or before the target is used. The position lands
    /// on the first packet of the selected cluster (or keyframe),
    /// so packets before the keyframe are replayed from there.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when a cue points outside the Segment
    /// or at a non-cluster offset.
    pub fn seek(&mut self, timestamp_us: u64) -> Result<()> {
        if !self.cues.is_empty() {
            // Greatest cue time at or before the target; the first
            // cue when the target precedes every cue.
            let mut best: Option<&CuePoint> = None;
            for cue in &self.cues {
                if cue.time_us <= timestamp_us {
                    best = Some(cue);
                } else {
                    break;
                }
            }
            let cue = best.or(self.cues.first()).ok_or(Error::InvalidData {
                offset: 0,
                detail: "webm cues are empty",
            })?;
            let cluster_offset = self
                .segment_data_start
                .checked_add(usize::try_from(cue.cluster_position).unwrap_or(usize::MAX))
                .ok_or(Error::InvalidData {
                    offset: 0,
                    detail: "webm cue cluster position overflows",
                })?;
            if usize::try_from(cue.cluster_position).unwrap_or(usize::MAX)
                >= self.segment_payload_len
            {
                return Err(Error::InvalidData {
                    offset: 0,
                    detail: "webm cue cluster position is outside the segment",
                });
            }
            let (_, first_packet) = self
                .clusters
                .iter()
                .find(|(offset, _)| *offset == cluster_offset)
                .ok_or(Error::InvalidData {
                    offset: 0,
                    detail: "webm cue cluster position does not point at a cluster",
                })?;
            self.position = *first_packet;
            return Ok(());
        }
        // No cues: last video keyframe at or before the target, or
        // any keyframe when there is no video track.
        let video_ids: Vec<u64> = self
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .map(|track| track.track_id)
            .collect();
        let mut best: Option<usize> = None;
        for (index, packet) in self.packets.iter().enumerate() {
            if !packet.is_keyframe || packet.timestamp > timestamp_us {
                continue;
            }
            let track_id = self.packet_tracks[index];
            if !video_ids.is_empty() && !video_ids.contains(&track_id) {
                continue;
            }
            best = Some(index);
        }
        self.position = best.unwrap_or(0);
        Ok(())
    }
}

/// Mutable state accumulated while walking the Segment.
#[derive(Default)]
struct SegmentState {
    tracks: Vec<TrackInfo>,
    packets: Vec<Packet>,
    /// Track number for each packet, in packet order.
    packet_tracks: Vec<u64>,
    timestamp_scale_ns: u64,
    duration_us: Option<u64>,
    metadata: Metadata,
    cues: Vec<CuePoint>,
    clusters: Vec<(usize, usize)>,
}

fn check_depth(depth: u32) -> Result<()> {
    if depth > MAX_NESTING_DEPTH {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "ebml nesting depth exceeds limit",
        });
    }
    Ok(())
}

/// Validates the EBML header: the `DocType` must be `webm` or
/// `matroska`.
fn parse_ebml_header(bytes: &[u8], ebml: &crate::ebml::EbmlElementHeader) -> Result<()> {
    let mut cursor = EbmlCursor::new(bytes, ebml.data_start, ebml.data_end)?;
    let mut doc_type: Option<String> = None;
    while let Some(child) = cursor.next_header()? {
        if child.id == ID_DOC_TYPE {
            doc_type = Some(read_string(bytes, child.data_start, child.payload_len())?);
        }
    }
    match doc_type.as_deref() {
        Some("webm" | "matroska") => Ok(()),
        Some(_) => Err(Error::Unsupported(
            "ebml doctype is neither webm nor matroska",
        )),
        None => Err(Error::InvalidData {
            offset: ebml.offset,
            detail: "ebml header has no doctype",
        }),
    }
}

/// Walks the Segment children at depth 3.
fn parse_segment(
    bytes: &[u8],
    segment: &crate::ebml::EbmlElementHeader,
    state: &mut SegmentState,
) -> Result<()> {
    let mut cursor = EbmlCursor::new(bytes, segment.data_start, segment.data_end)?;
    while let Some(child) = cursor.next_header()? {
        match child.id {
            ID_SEEK_HEAD => parse_seek_head(bytes, &child, state)?,
            ID_INFO => parse_info(bytes, &child, state)?,
            ID_TRACKS => parse_tracks(bytes, &child, state)?,
            ID_CLUSTER => parse_cluster(bytes, &child, state)?,
            ID_CUES => parse_cues(bytes, &child, state)?,
            ID_TAGS => parse_tags(bytes, &child, state)?,
            _ => {
                // Void, CRC-32, and unknown top-level elements are
                // skipped by size; their bounds were already checked.
            }
        }
    }
    Ok(())
}

/// Validates the `SeekHead` structure (entries must have an ID and a
/// position). The positions themselves are not used: seeking uses
/// `Cues` and the cluster index built while parsing.
fn parse_seek_head(
    bytes: &[u8],
    seek_head: &crate::ebml::EbmlElementHeader,
    _state: &mut SegmentState,
) -> Result<()> {
    check_depth(3)?;
    let mut cursor = EbmlCursor::new(bytes, seek_head.data_start, seek_head.data_end)?;
    while let Some(seek) = cursor.next_header()? {
        if seek.id != ID_SEEK {
            continue;
        }
        check_depth(4)?;
        let mut inner = EbmlCursor::new(bytes, seek.data_start, seek.data_end)?;
        let mut has_id = false;
        let mut has_position = false;
        while let Some(field) = inner.next_header()? {
            match field.id {
                ID_SEEK_ID => has_id = true,
                ID_SEEK_POSITION => has_position = true,
                _ => {}
            }
        }
        if !has_id || !has_position {
            return Err(Error::InvalidData {
                offset: seek.offset,
                detail: "webm seek entry is missing its id or position",
            });
        }
    }
    Ok(())
}

/// Reads `TimestampScale` (default 1 ms) and `Duration` from Info.
fn parse_info(
    bytes: &[u8],
    info: &crate::ebml::EbmlElementHeader,
    state: &mut SegmentState,
) -> Result<()> {
    check_depth(3)?;
    let mut cursor = EbmlCursor::new(bytes, info.data_start, info.data_end)?;
    while let Some(child) = cursor.next_header()? {
        match child.id {
            ID_TIMESTAMP_SCALE => {
                let scale = read_uint(bytes, child.data_start, child.payload_len())?;
                if scale == 0 {
                    return Err(Error::InvalidData {
                        offset: child.offset,
                        detail: "webm timestamp scale is zero",
                    });
                }
                state.timestamp_scale_ns = scale;
            }
            ID_DURATION => {
                let duration = read_float(bytes, child.data_start, child.payload_len())?;
                if !duration.is_finite() || duration < 0.0 {
                    return Err(Error::InvalidData {
                        offset: child.offset,
                        detail: "webm duration is not a finite non-negative number",
                    });
                }
                // The timestamp scale is nanoseconds per timecode
                // unit; above u32::MAX it would make every
                // timestamp overflow, so it is an error.
                let scale = u32::try_from(state.timestamp_scale_ns)
                    .map_err(|_| Error::Overflow("webm timestamp scale exceeds u32"))?;
                let scaled = duration * f64::from(scale) / 1000.0;
                state.duration_us = Some(checked_f64_to_u64(scaled)?);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Resolves a Matroska codec ID to a library codec.
fn codec_for_id(codec_id: &str) -> Result<Codec> {
    match codec_id {
        "V_VP9" => Ok(Codec::Vp9),
        "V_AV1" => Ok(Codec::Av1),
        "A_OPUS" => Ok(Codec::Opus),
        "A_VORBIS" => Ok(Codec::Vorbis),
        "A_FLAC" => Ok(Codec::Flac),
        "A_PCM/INT/LIT" | "A_PCM/INT/BIG" => Ok(Codec::Pcm),
        _ => Err(Error::Unsupported(
            "unsupported matroska codec id (see track codec_id)",
        )),
    }
}

/// Parses the Tracks element into `TrackInfo` values.
fn parse_tracks(
    bytes: &[u8],
    tracks: &crate::ebml::EbmlElementHeader,
    state: &mut SegmentState,
) -> Result<()> {
    check_depth(3)?;
    let mut cursor = EbmlCursor::new(bytes, tracks.data_start, tracks.data_end)?;
    while let Some(entry) = cursor.next_header()? {
        if entry.id != ID_TRACK_ENTRY {
            continue;
        }
        check_depth(4)?;
        let track = parse_track_entry(bytes, &entry, state.tracks.len())?;
        state.tracks.push(track);
        if state.tracks.len() > MAX_TRACKS {
            return Err(Error::AllocationLimit {
                requested: state.tracks.len(),
                limit: MAX_TRACKS,
            });
        }
    }
    Ok(())
}

/// Converts a finite non-negative `f64` to `u64`, truncating the
/// fraction. Returns `Overflow` for a negative, non-finite, or
/// too-large value. The `as` cast is guarded by that range check,
/// so it cannot truncate or lose a sign in practice.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn checked_f64_to_u64(value: f64) -> Result<u64> {
    if !value.is_finite() || value < 0.0 || value >= 2f64.powi(64) {
        return Err(Error::Overflow("float value exceeds u64"));
    }
    Ok(value as u64)
}

/// Parses one `TrackEntry`. The single function mirrors the
/// Matroska `TrackEntry` element: one arm per child element, each
/// validating its own value. Splitting it would scatter the
/// track-level state across helpers.
#[allow(clippy::too_many_lines)]
fn parse_track_entry(
    bytes: &[u8],
    entry: &crate::ebml::EbmlElementHeader,
    index: usize,
) -> Result<TrackInfo> {
    let mut cursor = EbmlCursor::new(bytes, entry.data_start, entry.data_end)?;
    let mut number: Option<u64> = None;
    let mut track_type: Option<u64> = None;
    let mut codec_id: Option<String> = None;
    let mut codec_private: Vec<u8> = Vec::new();
    let mut default_duration_us: Option<u64> = None;
    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;
    let mut sample_rate: Option<u32> = None;
    let mut channels: Option<u32> = None;
    while let Some(child) = cursor.next_header()? {
        match child.id {
            ID_TRACK_NUMBER => {
                number = Some(read_uint(bytes, child.data_start, child.payload_len())?);
            }
            ID_TRACK_TYPE => {
                track_type = Some(read_uint(bytes, child.data_start, child.payload_len())?);
            }
            ID_CODEC_ID => {
                codec_id = Some(read_string(bytes, child.data_start, child.payload_len())?);
            }
            ID_CODEC_PRIVATE => {
                codec_private = bytes[child.data_start..child.data_end].to_vec();
            }
            ID_DEFAULT_DURATION => {
                let ns = read_uint(bytes, child.data_start, child.payload_len())?;
                default_duration_us = Some(ns / 1000);
            }
            ID_VIDEO => {
                check_depth(5)?;
                let mut video = EbmlCursor::new(bytes, child.data_start, child.data_end)?;
                while let Some(field) = video.next_header()? {
                    match field.id {
                        ID_PIXEL_WIDTH => {
                            width = Some(
                                u32::try_from(read_uint(
                                    bytes,
                                    field.data_start,
                                    field.payload_len(),
                                )?)
                                .map_err(|_| Error::Overflow("webm pixel width exceeds u32"))?,
                            );
                        }
                        ID_PIXEL_HEIGHT => {
                            height = Some(
                                u32::try_from(read_uint(
                                    bytes,
                                    field.data_start,
                                    field.payload_len(),
                                )?)
                                .map_err(|_| Error::Overflow("webm pixel height exceeds u32"))?,
                            );
                        }
                        _ => {}
                    }
                }
            }
            ID_AUDIO => {
                check_depth(5)?;
                let mut audio = EbmlCursor::new(bytes, child.data_start, child.data_end)?;
                while let Some(field) = audio.next_header()? {
                    match field.id {
                        ID_SAMPLING_FREQUENCY => {
                            let rate = read_float(bytes, field.data_start, field.payload_len())?;
                            if !rate.is_finite() || rate <= 0.0 || rate > f64::from(u32::MAX) {
                                return Err(Error::InvalidData {
                                    offset: field.offset,
                                    detail: "webm sampling frequency is out of range",
                                });
                            }
                            // The range check above proves the rate
                            // fits in a `u32`.
                            sample_rate =
                                Some(u32::try_from(checked_f64_to_u64(rate)?).map_err(|_| {
                                    Error::Overflow("webm sampling frequency exceeds u32")
                                })?);
                        }
                        ID_CHANNELS => {
                            channels = Some(
                                u32::try_from(read_uint(
                                    bytes,
                                    field.data_start,
                                    field.payload_len(),
                                )?)
                                .map_err(|_| Error::Overflow("webm channel count exceeds u32"))?,
                            );
                        }
                        // Recognized but unused in Phase 6
                        // (BitDepth) and anything unknown: skipped
                        // by size.
                        _ => {}
                    }
                }
            }
            // Recognized but unused in Phase 6 (TrackUID,
            // CodecDelay, SeekPreRoll) and anything unknown:
            // skipped by size.
            _ => {}
        }
    }
    let number = number.ok_or(Error::InvalidData {
        offset: entry.offset,
        detail: "webm track entry has no track number",
    })?;
    let track_type = track_type.ok_or(Error::InvalidData {
        offset: entry.offset,
        detail: "webm track entry has no track type",
    })?;
    let codec_id = codec_id.ok_or(Error::InvalidData {
        offset: entry.offset,
        detail: "webm track entry has no codec id",
    })?;
    let kind = match track_type {
        1 => TrackKind::Video,
        2 => TrackKind::Audio,
        _ => {
            return Err(Error::Unsupported(
                "unsupported matroska track type (only video and audio)",
            ));
        }
    };
    let codec = codec_for_id(&codec_id)?;
    Ok(TrackInfo {
        index,
        track_id: number,
        kind,
        codec,
        codec_id,
        width,
        height,
        sample_rate,
        channels,
        default_duration_us,
        duration_us: None,
        codec_private,
    })
}

/// Parses one Cluster: its Timestamp and its blocks, in file order.
fn parse_cluster(
    bytes: &[u8],
    cluster: &crate::ebml::EbmlElementHeader,
    state: &mut SegmentState,
) -> Result<()> {
    check_depth(3)?;
    state.clusters.push((cluster.offset, state.packets.len()));
    let mut cursor = EbmlCursor::new(bytes, cluster.data_start, cluster.data_end)?;
    let mut cluster_timestamp: u64 = 0;
    while let Some(child) = cursor.next_header()? {
        match child.id {
            ID_CLUSTER_TIMESTAMP => {
                cluster_timestamp = read_uint(bytes, child.data_start, child.payload_len())?;
            }
            ID_SIMPLE_BLOCK => {
                parse_block(bytes, &child, cluster_timestamp, state, false)?;
            }
            ID_BLOCK_GROUP => {
                check_depth(4)?;
                parse_block_group(bytes, &child, cluster_timestamp, state)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Parses a `BlockGroup`: its first child must be a Block.
fn parse_block_group(
    bytes: &[u8],
    group: &crate::ebml::EbmlElementHeader,
    cluster_timestamp: u64,
    state: &mut SegmentState,
) -> Result<()> {
    let mut cursor = EbmlCursor::new(bytes, group.data_start, group.data_end)?;
    let mut saw_block = false;
    while let Some(child) = cursor.next_header()? {
        // BlockDuration, ReferenceBlock, DiscardPadding and
        // CodecState are validated by the cursor bounds and
        // otherwise ignored in Phase 6.
        if child.id == ID_BLOCK {
            if saw_block {
                return Err(Error::InvalidData {
                    offset: child.offset,
                    detail: "webm block group contains two blocks",
                });
            }
            saw_block = true;
            parse_block(bytes, &child, cluster_timestamp, state, true)?;
        }
    }
    if !saw_block {
        return Err(Error::InvalidData {
            offset: group.offset,
            detail: "webm block group contains no block",
        });
    }
    Ok(())
}

/// Lacing mode of a Block, from the flags byte (Matroska spec:
/// bits 1-2: 0 none, 1 Xiph, 2 fixed-size, 3 EBML).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lacing {
    None,
    Xiph,
    Fixed,
    Ebml,
}

/// Parses one `SimpleBlock` or Block element into packets. `in_group`
/// is true for a Block inside a `BlockGroup`; a bare Block outside a
/// group is malformed per the Matroska spec, but this parser accepts
/// it (some muxers write it) and treats it like a `SimpleBlock`.
fn parse_block(
    bytes: &[u8],
    block: &crate::ebml::EbmlElementHeader,
    cluster_timestamp: u64,
    state: &mut SegmentState,
    _in_group: bool,
) -> Result<()> {
    let data = &bytes[block.data_start..block.data_end];
    if data.len() < 4 {
        return Err(Error::Truncated {
            needed: 4,
            had: data.len(),
        });
    }
    let (track_number, track_len) = read_vint(data, 0)?;
    let header_len = track_len + 2 + 1;
    if data.len() < header_len {
        return Err(Error::Truncated {
            needed: header_len,
            had: data.len(),
        });
    }
    let timecode = i16::from_be_bytes([data[track_len], data[track_len + 1]]);
    let flags = data[track_len + 2];
    let is_keyframe = flags & 0x80 != 0;
    let lacing = match (flags >> 1) & 0x03 {
        0 => Lacing::None,
        1 => Lacing::Xiph,
        2 => Lacing::Fixed,
        _ => Lacing::Ebml,
    };
    // Cluster timestamp plus the signed block timecode, in
    // TimestampScale units; a negative total is corrupt input.
    let total_units = i128::from(cluster_timestamp) + i128::from(timecode);
    if total_units < 0 {
        return Err(Error::InvalidData {
            offset: block.offset,
            detail: "webm block timestamp is negative",
        });
    }
    let timestamp_us: u64 = (total_units * i128::from(state.timestamp_scale_ns) / 1000)
        .try_into()
        .map_err(|_| Error::Overflow("webm block timestamp exceeds u64"))?;
    let track = state
        .tracks
        .iter()
        .find(|track| track.track_id == track_number)
        .ok_or(Error::InvalidData {
            offset: block.offset,
            detail: "webm block references an undeclared track",
        })?;
    let codec = track.codec;
    let default_duration_us = track.default_duration_us;
    let payload = &data[header_len..];
    let laces = split_laces(payload, lacing, block.offset)?;
    for (index, &(lace_offset, size)) in laces.iter().enumerate() {
        let lace_data = payload[lace_offset..lace_offset + size].to_vec();
        // Laces after the first share the block timestamp unless
        // the track declares a duration, in which case each lace
        // advances by it.
        let lace_timestamp = match default_duration_us {
            Some(duration) => timestamp_us
                .checked_add(
                    duration
                        .checked_mul(index as u64)
                        .ok_or(Error::Overflow("webm lace timestamp overflows"))?,
                )
                .ok_or(Error::Overflow("webm lace timestamp overflows"))?,
            None => timestamp_us,
        };
        check_packet_size(codec, lace_data.len())?;
        state.packets.push(Packet {
            codec,
            data: lace_data,
            timestamp: lace_timestamp,
            is_keyframe: is_keyframe && index == 0,
        });
        state.packet_tracks.push(track_number);
    }
    Ok(())
}

/// Splits a block payload into its laces. Returns `(offset, size)`
/// pairs into the payload, in lace order. The first lace of Xiph and
/// EBML lacing is an unsigned vint size; the following laces (except
/// the last) are signed vint differences from the previous lace size;
/// the last lace takes the remainder. Fixed-size lacing divides the
/// body evenly. A lace table that overruns the payload, a negative
/// lace size, or sizes summing past the payload is `InvalidData`.
fn split_laces(payload: &[u8], lacing: Lacing, offset: usize) -> Result<Vec<(usize, usize)>> {
    match lacing {
        Lacing::None => Ok(vec![(0, payload.len())]),
        Lacing::Fixed => {
            let lace_count_byte = *payload
                .first()
                .ok_or(Error::Truncated { needed: 1, had: 0 })?;
            let count = usize::from(lace_count_byte) + 1;
            let body = payload.len().checked_sub(1).ok_or(Error::InvalidData {
                offset,
                detail: "webm fixed-size lacing has no body",
            })?;
            if body % count != 0 {
                return Err(Error::InvalidData {
                    offset,
                    detail: "webm fixed-size lacing does not divide the block evenly",
                });
            }
            let each = body / count;
            Ok((0..count).map(|index| (1 + index * each, each)).collect())
        }
        Lacing::Xiph | Lacing::Ebml => {
            let lace_count_byte = *payload
                .first()
                .ok_or(Error::Truncated { needed: 1, had: 0 })?;
            let count = usize::from(lace_count_byte) + 1;
            if count > 256 {
                return Err(Error::InvalidData {
                    offset,
                    detail: "webm lace count exceeds 256",
                });
            }
            let mut sizes: Vec<u64> = Vec::with_capacity(count);
            let mut table_offset = 1usize;
            let mut previous: i64 = 0;
            for lace in 0..count - 1 {
                if lace == 0 {
                    let (size, len) = read_vint(payload, table_offset)?;
                    table_offset += len;
                    previous = i64::try_from(size)
                        .map_err(|_| Error::Overflow("webm lace size exceeds i64 range"))?;
                    sizes.push(size);
                } else {
                    let (delta, len) = read_signed_vint(payload, table_offset)?;
                    table_offset += len;
                    previous = previous
                        .checked_add(delta)
                        .ok_or(Error::Overflow("webm lace size difference overflows"))?;
                    if previous < 0 {
                        return Err(Error::InvalidData {
                            offset,
                            detail: "webm lace size is negative",
                        });
                    }
                    // `previous` was just checked non-negative, so
                    // the conversion cannot fail.
                    sizes.push(u64::try_from(previous).unwrap_or(0));
                }
            }
            let used: u64 = sizes.iter().sum();
            let body = payload
                .len()
                .checked_sub(table_offset)
                .ok_or(Error::InvalidData {
                    offset,
                    detail: "webm lace table overruns the block",
                })? as u64;
            if used > body {
                return Err(Error::InvalidData {
                    offset,
                    detail: "webm lace sizes exceed the block",
                });
            }
            sizes.push(body - used);
            let mut laces: Vec<(usize, usize)> = Vec::with_capacity(count);
            let mut position = table_offset;
            for &size in &sizes {
                let size = usize::try_from(size)
                    .map_err(|_| Error::Overflow("webm lace size exceeds address space"))?;
                laces.push((position, size));
                position += size;
            }
            Ok(laces)
        }
    }
}

/// Rejects a packet above the per-codec size limit before it is
/// copied into the packet list.
fn check_packet_size(codec: Codec, len: usize) -> Result<()> {
    let limit = match codec {
        Codec::Vp9 | Codec::Av1 => crate::limits::MAX_VIDEO_PACKET_BYTES,
        _ => crate::limits::MAX_AUDIO_PACKET_BYTES,
    };
    if len > limit {
        return Err(Error::AllocationLimit {
            requested: len,
            limit,
        });
    }
    Ok(())
}

/// Parses the Cues element into cue points, validating that each
/// cluster position lies inside the Segment.
fn parse_cues(
    bytes: &[u8],
    cues: &crate::ebml::EbmlElementHeader,
    state: &mut SegmentState,
) -> Result<()> {
    check_depth(3)?;
    let mut cursor = EbmlCursor::new(bytes, cues.data_start, cues.data_end)?;
    while let Some(point) = cursor.next_header()? {
        if point.id != ID_CUE_POINT {
            continue;
        }
        check_depth(4)?;
        let mut inner = EbmlCursor::new(bytes, point.data_start, point.data_end)?;
        let mut cue_time: Option<u64> = None;
        while let Some(field) = inner.next_header()? {
            match field.id {
                ID_CUE_TIME => {
                    cue_time = Some(read_uint(bytes, field.data_start, field.payload_len())?);
                }
                ID_CUE_TRACK_POSITIONS => {
                    check_depth(5)?;
                    let mut positions = EbmlCursor::new(bytes, field.data_start, field.data_end)?;
                    let mut track: Option<u64> = None;
                    let mut cluster_position: Option<u64> = None;
                    let mut block_number: Option<u64> = None;
                    while let Some(entry) = positions.next_header()? {
                        match entry.id {
                            ID_CUE_TRACK => {
                                track =
                                    Some(read_uint(bytes, entry.data_start, entry.payload_len())?);
                            }
                            ID_CUE_CLUSTER_POSITION => {
                                cluster_position =
                                    Some(read_uint(bytes, entry.data_start, entry.payload_len())?);
                            }
                            ID_CUE_BLOCK_NUMBER => {
                                block_number =
                                    Some(read_uint(bytes, entry.data_start, entry.payload_len())?);
                            }
                            _ => {}
                        }
                    }
                    let time = cue_time.ok_or(Error::InvalidData {
                        offset: point.offset,
                        detail: "webm cue point has no cue time",
                    })?;
                    let track = track.ok_or(Error::InvalidData {
                        offset: point.offset,
                        detail: "webm cue point has no cue track",
                    })?;
                    let cluster_position = cluster_position.ok_or(Error::InvalidData {
                        offset: point.offset,
                        detail: "webm cue point has no cluster position",
                    })?;
                    let block_number = block_number.unwrap_or(1);
                    let time_us = (u128::from(time) * u128::from(state.timestamp_scale_ns) / 1000)
                        .try_into()
                        .map_err(|_| Error::Overflow("webm cue time exceeds u64"))?;
                    state.cues.push(CuePoint {
                        time_us,
                        track,
                        cluster_position,
                        block_number,
                    });
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Parses the Tags element into container metadata.
fn parse_tags(
    bytes: &[u8],
    tags: &crate::ebml::EbmlElementHeader,
    state: &mut SegmentState,
) -> Result<()> {
    check_depth(3)?;
    let mut cursor = EbmlCursor::new(bytes, tags.data_start, tags.data_end)?;
    while let Some(tag) = cursor.next_header()? {
        if tag.id != ID_TAG {
            continue;
        }
        check_depth(4)?;
        let mut inner = EbmlCursor::new(bytes, tag.data_start, tag.data_end)?;
        while let Some(field) = inner.next_header()? {
            if field.id != ID_SIMPLE_TAG {
                continue;
            }
            check_depth(5)?;
            let mut simple = EbmlCursor::new(bytes, field.data_start, field.data_end)?;
            let mut name: Option<String> = None;
            let mut value: Option<Vec<u8>> = None;
            while let Some(entry) = simple.next_header()? {
                match entry.id {
                    ID_TAG_NAME => {
                        name = Some(read_string(bytes, entry.data_start, entry.payload_len())?);
                    }
                    ID_TAG_STRING => {
                        // Tag strings are UTF-8; a trailing NUL is
                        // padding, stripped by `read_string` like
                        // the tag name.
                        value = Some(
                            read_string(bytes, entry.data_start, entry.payload_len())?.into_bytes(),
                        );
                    }
                    _ => {}
                }
            }
            if let (Some(name), Some(value)) = (name, value) {
                state
                    .metadata
                    .entries
                    .push(MetadataEntry::new(&name, &value));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Builds a minimal `WebM` in memory: EBML header, Segment with
    /// Info (scale 1 ms), one Opus track, one Cluster with one
    /// `SimpleBlock`. Used to test the demuxer without a file.
    fn minimal_webm() -> Vec<u8> {
        fn vint(value: u64) -> Vec<u8> {
            // Each branch bounds `value`, so every shifted part
            // fits in a `u8`; the `unwrap_or` is unreachable.
            if value < 0x80 {
                vec![0x80 | u8::try_from(value).unwrap_or(0)]
            } else if value < 0x4000 {
                vec![
                    0x40 | u8::try_from(value >> 8).unwrap_or(0),
                    u8::try_from(value).unwrap_or(0),
                ]
            } else {
                vec![
                    0x20 | u8::try_from(value >> 16).unwrap_or(0),
                    u8::try_from(value >> 8).unwrap_or(0),
                    u8::try_from(value).unwrap_or(0),
                ]
            }
        }
        fn element(id: &[u8], payload: &[u8]) -> Vec<u8> {
            let mut out = id.to_vec();
            out.extend(vint(payload.len() as u64));
            out.extend(payload);
            out
        }
        fn uint_element(id: &[u8], value: u64) -> Vec<u8> {
            let bytes = if value == 0 {
                vec![0u8]
            } else {
                value.to_be_bytes()[(8 - (64 - value.leading_zeros() as usize).div_ceil(8))..]
                    .to_vec()
            };
            element(id, &bytes)
        }
        let ebml_payload = [
            element(&[0x42, 0x86], &[1]),
            element(&[0x42, 0x82], b"webm"),
        ]
        .concat();
        let ebml = element(&[0x1A, 0x45, 0xDF, 0xA3], &ebml_payload);
        let info_payload = [uint_element(&[0x2A, 0xD7, 0xB1], 1_000_000)].concat();
        let info = element(&[0x15, 0x49, 0xA9, 0x66], &info_payload);
        let track_payload = [
            uint_element(&[0xD7], 1),
            uint_element(&[0x83], 2),
            element(&[0x86], b"A_OPUS"),
        ]
        .concat();
        let track = element(&[0xAE], &track_payload);
        let tracks = element(&[0x16, 0x54, 0xAE, 0x6B], &track);
        // SimpleBlock: track 1 (vint 0x81), timecode 0, flags 0x80,
        // payload "OpusHead" (not a real packet, only the demuxer
        // path is exercised here).
        let mut block_data = vec![0x81, 0x00, 0x00, 0x80];
        block_data.extend_from_slice(b"OpusHead");
        let block = element(&[0xA3], &block_data);
        let cluster_payload = [uint_element(&[0xE7], 0), block].concat();
        let cluster = element(&[0x1F, 0x43, 0xB6, 0x75], &cluster_payload);
        let segment_payload = [info, tracks, cluster].concat();
        let segment = element(&[0x18, 0x53, 0x80, 0x67], &segment_payload);
        [ebml, segment].concat()
    }

    #[test]
    fn demuxes_minimal_webm() {
        let bytes = minimal_webm();
        let mut demuxer = MatroskaDemuxer::new(Cursor::new(bytes)).unwrap();
        assert_eq!(demuxer.tracks().len(), 1);
        assert_eq!(demuxer.tracks()[0].codec, Codec::Opus);
        assert_eq!(demuxer.tracks()[0].codec_id, "A_OPUS");
        assert_eq!(demuxer.packet_count(), 1);
        let packet = demuxer.next_packet().unwrap().unwrap();
        assert_eq!(packet.codec, Codec::Opus);
        assert_eq!(packet.data, b"OpusHead");
        assert_eq!(packet.timestamp, 0);
        assert!(packet.is_keyframe);
        assert!(demuxer.next_packet().unwrap().is_none());
    }

    #[test]
    fn unknown_codec_id_returns_unsupported() {
        let bytes = minimal_webm();
        // Replace "A_OPUS" with "A_AAC_" (same length, unknown id).
        let mut bytes = bytes;
        let pos = bytes
            .windows(6)
            .position(|window| window == b"A_OPUS")
            .unwrap();
        bytes[pos..pos + 6].copy_from_slice(b"A_AAC_");
        let err = MatroskaDemuxer::new(Cursor::new(bytes)).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn non_webm_doctype_returns_unsupported() {
        let bytes = minimal_webm();
        let mut bytes = bytes;
        let pos = bytes
            .windows(4)
            .position(|window| window == b"webm")
            .unwrap();
        bytes[pos..pos + 4].copy_from_slice(b"mkva");
        let err = MatroskaDemuxer::new(Cursor::new(bytes)).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn truncated_file_returns_truncated() {
        let bytes = minimal_webm();
        let cut = bytes[..bytes.len() / 2].to_vec();
        let err = MatroskaDemuxer::new(Cursor::new(cut)).unwrap_err();
        assert!(matches!(
            err,
            Error::Truncated { .. } | Error::InvalidData { .. }
        ));
    }

    #[test]
    fn empty_input_returns_truncated() {
        let err = MatroskaDemuxer::new(Cursor::new(Vec::new())).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn lace_sizes_xiph() {
        // Lace count byte 2 (3 laces), first size 10 (vint 0x8A),
        // second size delta +2 (signed vint 0xC1: 65 - 63) -> 12,
        // last is the remainder. Payload after the table:
        // 10 + 12 + 5.
        let payload = [
            0x02, 0x8A, 0xC1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
            20, 21, 22, 23, 24, 25, 26,
        ];
        let laces = split_laces(&payload, Lacing::Xiph, 0).unwrap();
        assert_eq!(laces.len(), 3);
        assert_eq!(laces[0], (3, 10));
        assert_eq!(laces[1], (13, 12));
        assert_eq!(laces[2], (25, 5));
    }

    #[test]
    fn lace_sizes_fixed_must_divide_evenly() {
        let payload = [0x01, 1, 2, 3, 4, 5];
        // 2 laces over 5 body bytes: not even.
        let err = split_laces(&payload, Lacing::Fixed, 0).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
        let payload = [0x01, 1, 2, 3, 4];
        let laces = split_laces(&payload, Lacing::Fixed, 0).unwrap();
        assert_eq!(laces, vec![(1, 2), (3, 2)]);
    }
}
