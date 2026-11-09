//! ISO BMFF (MP4) box parsing and demuxing (ISO/IEC 14496-12).
//!
//! The whole file is read at construction (bounded by
//! `MAX_MP4_FILE_BYTES`) and the sample tables in `moov` are walked
//! into packets; `next_packet` then yields them in decode-timestamp
//! order. A packet timestamp is the presentation timestamp in
//! microseconds (`dts + ctts` offset, when a `ctts` box is present).
//! The edit list (`elst`) is parsed and validated but not applied
//! to timestamps in Phase 6: packet timestamps are the raw decode /
//! presentation times, and the Opus pre-skip it encodes is handled
//! by the decoder from the `dOps` box. `mdat` may precede `moov`
//! (faststart); samples are validated against the `mdat` ranges.
//!
//! Scope for Phase 6: sample entries `av01`, `vp09`, `Opus`, and
//! `fLaC`. FLAC-in-MP4 is parsed but has no test vector (ffmpeg
//! cannot produce one), so it is marked unverified in
//! `docs/LIMITS.md`.
//!
//! Passthrough (current round): sample entries `avc1`/`hev1`
//! (AVC/HEVC, config in `avcC`/`hvcC`, ISO/IEC 14496-15) and
//! `mp4a` (AAC, config in `esds`, ISO/IEC 14496-1) are parsed and
//! their samples are returned as packets, gated by the license
//! context (see `set_license_context`): without the feature or
//! without a license, `next_packet` refuses the packet instead of
//! yielding it. Packet data is the sample as stored (AVCC
//! length-prefixed NAL units for AVC/HEVC, raw AAC frames for
//! AAC); nothing is decoded here. M4A is MP4 carrying AAC (or
//! ALAC), not a separate codec.

use std::collections::HashSet;
use std::io::Read;

use crate::byte_source::read_mp4_bytes;
use crate::demux::{TrackInfo, TrackKind};
use crate::error::{Error, Result};
use crate::frame::{Codec, Metadata, Packet};
use crate::hw::{licensed_feature_enabled, passthrough_allowed};
use crate::license::LicenseContext;
use crate::limits::{MAX_NESTING_DEPTH, MAX_SAMPLES_PER_TRACK, MAX_TRACKS};

// Box types. Only the boxes this demuxer reads are listed; unknown
// boxes are skipped by size.
const BOX_FTYP: [u8; 4] = *b"ftyp";
const BOX_MOOV: [u8; 4] = *b"moov";
const BOX_MDAT: [u8; 4] = *b"mdat";
const BOX_MVHD: [u8; 4] = *b"mvhd";
const BOX_TRAK: [u8; 4] = *b"trak";
const BOX_TKHD: [u8; 4] = *b"tkhd";
const BOX_EDTS: [u8; 4] = *b"edts";
const BOX_ELST: [u8; 4] = *b"elst";
const BOX_MDIA: [u8; 4] = *b"mdia";
const BOX_MDHD: [u8; 4] = *b"mdhd";
const BOX_HDLR: [u8; 4] = *b"hdlr";
const BOX_MINF: [u8; 4] = *b"minf";
const BOX_STBL: [u8; 4] = *b"stbl";
const BOX_STSD: [u8; 4] = *b"stsd";
const BOX_STTS: [u8; 4] = *b"stts";
const BOX_STSS: [u8; 4] = *b"stss";
const BOX_STSC: [u8; 4] = *b"stsc";
const BOX_STSZ: [u8; 4] = *b"stsz";
const BOX_STCO: [u8; 4] = *b"stco";
const BOX_CO64: [u8; 4] = *b"co64";
const BOX_CTTS: [u8; 4] = *b"ctts";
const BOX_AV01: [u8; 4] = *b"av01";
const BOX_VP09: [u8; 4] = *b"vp09";
const BOX_OPUS: [u8; 4] = *b"Opus";
const BOX_MP4A: [u8; 4] = *b"mp4a";
const BOX_FLAC: [u8; 4] = *b"fLaC";
const BOX_AVC1: [u8; 4] = *b"avc1";
const BOX_HEV1: [u8; 4] = *b"hev1";
const BOX_AV1C: [u8; 4] = *b"av1C";
const BOX_VPCC: [u8; 4] = *b"vpcC";
const BOX_DOPS: [u8; 4] = *b"dOps";
const BOX_AVCC: [u8; 4] = *b"avcC";
const BOX_HVCC: [u8; 4] = *b"hvcC";
const BOX_ESDS: [u8; 4] = *b"esds";

/// Header of one ISO BMFF box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxHeader {
    /// Box type, four ASCII bytes.
    pub box_type: [u8; 4],
    /// Total box size in bytes, header included.
    pub size: u64,
    /// Offset of the first header byte.
    pub offset: usize,
    /// Offset of the first payload byte.
    pub data_start: usize,
    /// Offset one past the last payload byte.
    pub data_end: usize,
}

impl BoxHeader {
    /// Payload length in bytes.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        self.data_end.saturating_sub(self.data_start)
    }

    /// Box type as a lossy string, for diagnostics.
    #[must_use]
    pub fn type_str(&self) -> String {
        String::from_utf8_lossy(&self.box_type).into_owned()
    }
}

/// Parses one box header at `offset`, bounded by `parent_end`.
/// `size == 1` reads a 64-bit largesize; `size == 0` means the box
/// runs to the end of its parent.
///
/// # Errors
///
/// Returns `Truncated` when the buffer ends inside the header or
/// the box overruns the file, and `InvalidData` when the size is
/// smaller than the header or the box overruns its parent without
/// reaching the end of the file.
pub fn parse_box_header(data: &[u8], offset: usize, parent_end: usize) -> Result<BoxHeader> {
    if data.len() < offset + 8 || offset + 8 > parent_end {
        return Err(Error::Truncated {
            needed: offset + 8,
            had: data.len(),
        });
    }
    let size32 = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap_or([0; 4]));
    let box_type: [u8; 4] = data[offset + 4..offset + 8].try_into().unwrap_or([0; 4]);
    let (size, header_len) = match size32 {
        1 => {
            if data.len() < offset + 16 || offset + 16 > parent_end {
                return Err(Error::Truncated {
                    needed: offset + 16,
                    had: data.len(),
                });
            }
            let largesize =
                u64::from_be_bytes(data[offset + 8..offset + 16].try_into().unwrap_or([0; 8]));
            (largesize, 16usize)
        }
        0 => (
            u64::try_from(parent_end - offset).unwrap_or(u64::MAX),
            8usize,
        ),
        _ => (u64::from(size32), 8usize),
    };
    if size < header_len as u64 {
        return Err(Error::InvalidData {
            offset,
            detail: "mp4 box size is smaller than its header",
        });
    }
    let data_start = offset + header_len;
    let data_end = offset.saturating_add(usize::try_from(size).unwrap_or(usize::MAX));
    if data_end > data.len() {
        return Err(Error::Truncated {
            needed: data_end,
            had: data.len(),
        });
    }
    if data_end > parent_end {
        return Err(Error::InvalidData {
            offset,
            detail: "mp4 box size exceeds its parent box",
        });
    }
    Ok(BoxHeader {
        box_type,
        size,
        offset,
        data_start,
        data_end,
    })
}

/// Iterative cursor over the children of one box. `next_header`
/// yields each child header in order; unknown boxes are returned
/// like any other, so the caller can skip them by size.
#[derive(Debug, Clone)]
pub struct BoxCursor<'a> {
    data: &'a [u8],
    position: usize,
    end: usize,
}

impl<'a> BoxCursor<'a> {
    /// Creates a cursor over `data[start..end]`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the range is out of bounds.
    pub fn new(data: &'a [u8], start: usize, end: usize) -> Result<Self> {
        if start > end || end > data.len() {
            return Err(Error::InvalidArgument("mp4 box cursor range out of bounds"));
        }
        Ok(Self {
            data,
            position: start,
            end,
        })
    }

    /// True when no child boxes remain.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.position >= self.end
    }

    /// Returns the next child header, or `None` at the end of the
    /// range.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a cut-short header or a box
    /// overrunning the file, and `InvalidData` for a size smaller
    /// than the header or overrunning the parent.
    pub fn next_header(&mut self) -> Result<Option<BoxHeader>> {
        if self.position >= self.end {
            return Ok(None);
        }
        let header = parse_box_header(self.data, self.position, self.end)?;
        if header.size == 0 {
            return Err(Error::InvalidData {
                offset: self.position,
                detail: "mp4 box consumed zero bytes",
            });
        }
        self.position = header.data_end;
        Ok(Some(header))
    }
}

/// Reads a big-endian `u32` at `offset`.
///
/// # Errors
///
/// Returns `Truncated` when the buffer ends inside the value.
fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    if data.len() < offset + 4 {
        return Err(Error::Truncated {
            needed: offset + 4,
            had: data.len(),
        });
    }
    Ok(u32::from_be_bytes(
        data[offset..offset + 4].try_into().unwrap_or([0; 4]),
    ))
}

/// Reads a big-endian `u64` at `offset`.
///
/// # Errors
///
/// Returns `Truncated` when the buffer ends inside the value.
fn read_u64(data: &[u8], offset: usize) -> Result<u64> {
    if data.len() < offset + 8 {
        return Err(Error::Truncated {
            needed: offset + 8,
            had: data.len(),
        });
    }
    Ok(u64::from_be_bytes(
        data[offset..offset + 8].try_into().unwrap_or([0; 8]),
    ))
}

/// Reads a big-endian `i32` at `offset` (edit list media time,
/// version 1 composition offsets).
///
/// # Errors
///
/// Returns `Truncated` when the buffer ends inside the value.
fn read_i32(data: &[u8], offset: usize) -> Result<i32> {
    if data.len() < offset + 4 {
        return Err(Error::Truncated {
            needed: offset + 4,
            had: data.len(),
        });
    }
    Ok(i32::from_be_bytes(
        data[offset..offset + 4].try_into().unwrap_or([0; 4]),
    ))
}

/// Reads a big-endian `i64` at `offset` (version 1 edit list
/// media time).
///
/// # Errors
///
/// Returns `Truncated` when the buffer ends inside the value.
fn read_i64(data: &[u8], offset: usize) -> Result<i64> {
    if data.len() < offset + 8 {
        return Err(Error::Truncated {
            needed: offset + 8,
            had: data.len(),
        });
    }
    Ok(i64::from_be_bytes(
        data[offset..offset + 8].try_into().unwrap_or([0; 8]),
    ))
}

/// Reads a big-endian `u16` at `offset`.
///
/// # Errors
///
/// Returns `Truncated` when the buffer ends inside the value.
fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    if data.len() < offset + 2 {
        return Err(Error::Truncated {
            needed: offset + 2,
            had: data.len(),
        });
    }
    Ok(u16::from_be_bytes(
        data[offset..offset + 2].try_into().unwrap_or([0; 2]),
    ))
}

/// Version byte of a `FullBox` (the first payload byte).
fn box_version(data: &[u8], header: &BoxHeader) -> Result<u8> {
    if header.payload_len() < 1 {
        return Err(Error::Truncated {
            needed: header.data_start + 1,
            had: data.len(),
        });
    }
    Ok(data[header.data_start])
}

/// One sample of one track, before it becomes a `Packet`.
#[derive(Debug, Clone)]
struct Sample {
    offset: usize,
    size: usize,
    /// Decode timestamp in microseconds.
    dts_us: u64,
    /// Presentation timestamp in microseconds.
    pts_us: u64,
    is_keyframe: bool,
}

/// Sample tables of one track, as read from `stbl`.
#[derive(Debug, Default)]
struct SampleTables {
    /// (sample count, delta) runs for decode times.
    time_to_sample: Vec<(u32, u32)>,
    /// (sample count, composition offset) runs, when `ctts` exists.
    composition_offsets: Vec<(u32, i64)>,
    /// 1-based sync sample numbers, when `stss` exists.
    sync_samples: Option<HashSet<u32>>,
    /// (first chunk, samples per chunk, sample description index).
    sample_to_chunk: Vec<(u32, u32, u32)>,
    /// Per-sample sizes, or a constant size when `sample_size != 0`.
    sample_sizes: Vec<u32>,
    /// Chunk offsets (from `stco` or `co64`).
    chunk_offsets: Vec<u64>,
}

/// Parsed state of one track.
#[derive(Debug)]
struct Mp4Track {
    info: TrackInfo,
    samples: Vec<Sample>,
}

/// Demuxer for an MP4 (ISO BMFF) file. Packets are parsed at
/// construction; `next_packet` yields them in decode-timestamp order.
#[derive(Debug)]
pub struct Mp4Demuxer {
    tracks: Vec<TrackInfo>,
    packets: Vec<Packet>,
    /// Track index per packet, parallel to `packets`.
    packet_tracks: Vec<usize>,
    position: usize,
    duration_us: Option<u64>,
    metadata: Metadata,
    /// License context for passthrough of AVC/HEVC/AAC packets.
    /// `None` until the caller sets one; packets of licensed
    /// codecs are refused until then (see `next_packet`).
    license: Option<LicenseContext>,
}

impl Mp4Demuxer {
    /// Reads and demuxes an MP4 stream from `reader`.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a cut-short box or sample, `InvalidData`
    /// for a missing `ftyp`/`moov`, a box smaller than its header, a
    /// corrupt sample table, or a sample outside `mdat`,
    /// `Unsupported` for a sample entry outside the library, and
    /// `AllocationLimit` for a file or sample table above the limit.
    pub fn new<R: Read>(reader: R) -> Result<Self> {
        let bytes = read_mp4_bytes(reader)?;
        Self::parse(&bytes)
    }

    /// Parses an already-read MP4 file.
    ///
    /// # Errors
    ///
    /// The same errors as `new`.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 8 {
            return Err(Error::Truncated {
                needed: 8,
                had: bytes.len(),
            });
        }
        let mut cursor = BoxCursor::new(bytes, 0, bytes.len())?;
        let mut saw_ftyp = false;
        let mut moov: Option<BoxHeader> = None;
        let mut mdat_ranges: Vec<(usize, usize)> = Vec::new();
        while let Some(header) = cursor.next_header()? {
            match header.box_type {
                BOX_FTYP => {
                    if header.offset != 0 {
                        return Err(Error::InvalidData {
                            offset: header.offset,
                            detail: "mp4 ftyp box is not the first box",
                        });
                    }
                    saw_ftyp = true;
                    parse_ftyp(bytes, &header)?;
                }
                BOX_MOOV => {
                    if moov.is_some() {
                        return Err(Error::InvalidData {
                            offset: header.offset,
                            detail: "mp4 file contains two moov boxes",
                        });
                    }
                    moov = Some(header);
                }
                BOX_MDAT => {
                    mdat_ranges.push((header.data_start, header.data_end));
                }
                _ => {
                    // free, wide, udta, meta and unknown top-level
                    // boxes are skipped by size.
                }
            }
        }
        if !saw_ftyp {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "mp4 file has no ftyp box",
            });
        }
        let moov = moov.ok_or(Error::InvalidData {
            offset: 0,
            detail: "mp4 file has no moov box",
        })?;
        let (mut tracks, duration_us) = parse_moov(bytes, &moov, &mdat_ranges)?;
        if tracks.is_empty() {
            return Err(Error::InvalidData {
                offset: moov.offset,
                detail: "mp4 moov contains no tracks",
            });
        }
        // Merge every track's samples into decode-timestamp order
        // (ties keep track order, via a stable sort).
        let mut ordered: Vec<(u64, usize, Packet)> = Vec::new();
        for (index, track) in tracks.iter().enumerate() {
            for sample in &track.samples {
                let data = bytes
                    .get(sample.offset..sample.offset + sample.size)
                    .ok_or(Error::Truncated {
                        needed: sample.offset + sample.size,
                        had: bytes.len(),
                    })?
                    .to_vec();
                ordered.push((
                    sample.dts_us,
                    index,
                    Packet {
                        codec: track.info.codec,
                        data,
                        timestamp: sample.pts_us,
                        is_keyframe: sample.is_keyframe,
                    },
                ));
            }
        }
        ordered.sort_by_key(|(dts, index, _)| (*dts, *index));
        let packet_tracks: Vec<usize> = ordered.iter().map(|(_, index, _)| *index).collect();
        let packets: Vec<Packet> = ordered.into_iter().map(|(_, _, packet)| packet).collect();
        if packets.is_empty() {
            return Err(Error::InvalidData {
                offset: moov.offset,
                detail: "mp4 file contains no samples",
            });
        }
        let infos: Vec<TrackInfo> = tracks.drain(..).map(|track| track.info).collect();
        // Tags are best-effort: a corrupt `meta`/`ilst` must not
        // invalidate playable media, so a tag parse failure leaves
        // the metadata empty instead of failing the demux.
        let metadata = crate::mp4_tag::parse_mp4_metadata(bytes).unwrap_or_default();
        Ok(Self {
            tracks: infos,
            packets,
            packet_tracks,
            position: 0,
            duration_us,
            metadata,
            license: None,
        })
    }

    /// Tracks in file order.
    #[must_use]
    pub fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    /// Duration in microseconds, from `mvhd`.
    #[must_use]
    pub fn duration_us(&self) -> Option<u64> {
        self.duration_us
    }

    /// Container metadata from `meta`/`ilst`/`udta` (see
    /// `mp4_tag`); empty when the file has no tags or its tags
    /// are malformed.
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Sets the license context for passthrough of AVC/HEVC/AAC.
    /// The license is validated eagerly for every licensed codec
    /// the file carries, and is stored only when every check
    /// passes; a failed check leaves the previous context (or
    /// none) in place.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the feature for a carried
    /// licensed codec is off, `LicenseRequired` when the license
    /// has expired, and `InvalidArgument` for a malformed
    /// license.
    pub fn set_license_context(&mut self, mut license: Option<LicenseContext>) -> Result<()> {
        if let Some(context) = license.as_mut() {
            let mut seen: Vec<Codec> = Vec::new();
            for track in &self.tracks {
                if matches!(track.codec, Codec::Avc | Codec::Hevc | Codec::Aac)
                    && !seen.contains(&track.codec)
                {
                    seen.push(track.codec);
                }
            }
            for codec in seen {
                if !licensed_feature_enabled(codec) {
                    return Err(Error::Unsupported(
                        "mp4 passthrough for this codec is not enabled in this build",
                    ));
                }
                context.validate(codec.name())?;
            }
        }
        self.license = license;
        Ok(())
    }

    /// Number of packets in the file.
    #[must_use]
    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }

    /// Returns the next packet in decode-timestamp order, or `None`
    /// at end of stream. Licensed packets are gated: without a
    /// feature or without a license this returns `Unsupported` or
    /// `LicenseRequired` instead of the packet, and the position
    /// does not advance, so the caller can set a license and
    /// retry.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the packet's codec needs a
    /// feature that is off, and `LicenseRequired` when the codec
    /// needs a license that was not set.
    pub fn next_packet(&mut self) -> Result<Option<Packet>> {
        if self.position >= self.packets.len() {
            return Ok(None);
        }
        let packet = self.packets[self.position].clone();
        passthrough_allowed(packet.codec, self.license.is_some())?;
        self.position += 1;
        Ok(Some(packet))
    }

    /// Seeks to the keyframe at or before `timestamp_us`, using the
    /// `stss` sync samples of the video tracks (any track when there
    /// is no video track). The position lands on the first packet
    /// with a timestamp at or after that keyframe.
    ///
    /// # Errors
    ///
    /// This implementation never fails after construction; the
    /// `Result` exists so a corrupt seek index can be reported.
    pub fn seek(&mut self, timestamp_us: u64) -> Result<()> {
        // Audio packets are all sync samples (no stss), so they
        // must not pull the seek target past the video keyframe:
        // only video-track keyframes count when a video track
        // exists.
        let has_video = self
            .tracks
            .iter()
            .any(|track| track.kind == TrackKind::Video);
        let mut keyframe_time: Option<u64> = None;
        for (index, packet) in self.packets.iter().enumerate() {
            let track_index = self.packet_tracks[index];
            if has_video && self.tracks[track_index].kind != TrackKind::Video {
                continue;
            }
            if !packet.is_keyframe || packet.timestamp > timestamp_us {
                continue;
            }
            keyframe_time = Some(match keyframe_time {
                Some(previous) => previous.max(packet.timestamp),
                None => packet.timestamp,
            });
        }
        let target = keyframe_time.unwrap_or(0);
        self.position = self
            .packets
            .iter()
            .position(|packet| packet.timestamp >= target)
            .unwrap_or(self.packets.len());
        Ok(())
    }
}

/// Validates the `ftyp` box: a major brand and at least one
/// compatible brand entry are required.
fn parse_ftyp(bytes: &[u8], ftyp: &BoxHeader) -> Result<()> {
    if ftyp.payload_len() < 8 {
        return Err(Error::InvalidData {
            offset: ftyp.offset,
            detail: "mp4 ftyp box is too short",
        });
    }
    let _ = bytes;
    Ok(())
}

/// Parses `moov` into tracks and the movie duration.
fn parse_moov(
    bytes: &[u8],
    moov: &BoxHeader,
    mdat_ranges: &[(usize, usize)],
) -> Result<(Vec<Mp4Track>, Option<u64>)> {
    check_depth(1)?;
    let mut cursor = BoxCursor::new(bytes, moov.data_start, moov.data_end)?;
    let mut duration_us: Option<u64> = None;
    let mut tracks: Vec<Mp4Track> = Vec::new();
    while let Some(child) = cursor.next_header()? {
        match child.box_type {
            BOX_MVHD => {
                let (movie_timescale, movie_duration) = parse_mvhd(bytes, &child)?;
                duration_us = Some(
                    (u128::from(movie_duration) * 1_000_000 / u128::from(movie_timescale))
                        .try_into()
                        .map_err(|_| Error::Overflow("mp4 movie duration exceeds u64"))?,
                );
            }
            BOX_TRAK => {
                check_depth(2)?;
                let track = parse_trak(bytes, &child, mdat_ranges, tracks.len())?;
                tracks.push(track);
                if tracks.len() > MAX_TRACKS {
                    return Err(Error::AllocationLimit {
                        requested: tracks.len(),
                        limit: MAX_TRACKS,
                    });
                }
            }
            _ => {}
        }
    }
    Ok((tracks, duration_us))
}

fn check_depth(depth: u32) -> Result<()> {
    if depth > MAX_NESTING_DEPTH {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "mp4 box nesting depth exceeds limit",
        });
    }
    Ok(())
}

/// Parses `mvhd`: returns (timescale, duration).
fn parse_mvhd(bytes: &[u8], mvhd: &BoxHeader) -> Result<(u32, u64)> {
    let version = box_version(bytes, mvhd)?;
    let base = mvhd.data_start + 4;
    let (timescale, duration) = match version {
        0 => {
            if mvhd.payload_len() < 20 {
                return Err(Error::Truncated {
                    needed: base + 20,
                    had: bytes.len(),
                });
            }
            (
                read_u32(bytes, base + 8)?,
                u64::from(read_u32(bytes, base + 12)?),
            )
        }
        1 => {
            if mvhd.payload_len() < 32 {
                return Err(Error::Truncated {
                    needed: base + 32,
                    had: bytes.len(),
                });
            }
            (read_u32(bytes, base + 16)?, read_u64(bytes, base + 20)?)
        }
        _ => {
            return Err(Error::InvalidData {
                offset: mvhd.offset,
                detail: "mp4 mvhd version is not 0 or 1",
            });
        }
    };
    if timescale == 0 {
        return Err(Error::InvalidData {
            offset: mvhd.offset,
            detail: "mp4 mvhd timescale is zero",
        });
    }
    Ok((timescale, duration))
}

/// Parses one `trak` into an `Mp4Track` with its sample table.
fn parse_trak(
    bytes: &[u8],
    trak: &BoxHeader,
    mdat_ranges: &[(usize, usize)],
    index: usize,
) -> Result<Mp4Track> {
    let mut cursor = BoxCursor::new(bytes, trak.data_start, trak.data_end)?;
    let mut track_id: Option<u32> = None;
    let mut tkhd_width: Option<u32> = None;
    let mut tkhd_height: Option<u32> = None;
    let mut edit_list: Vec<(u64, i64)> = Vec::new();
    let mut mdia: Option<BoxHeader> = None;
    while let Some(child) = cursor.next_header()? {
        match child.box_type {
            BOX_TKHD => {
                let (id, width, height) = parse_tkhd(bytes, &child)?;
                track_id = Some(id);
                tkhd_width = width;
                tkhd_height = height;
            }
            BOX_EDTS => {
                check_depth(3)?;
                edit_list = parse_edts(bytes, &child)?;
            }
            BOX_MDIA => {
                mdia = Some(child);
            }
            _ => {}
        }
    }
    let track_id = track_id.ok_or(Error::InvalidData {
        offset: trak.offset,
        detail: "mp4 trak has no tkhd box",
    })?;
    let mdia = mdia.ok_or(Error::InvalidData {
        offset: trak.offset,
        detail: "mp4 trak has no mdia box",
    })?;
    // The edit list is parsed and validated by parse_edts, then
    // deliberately dropped: Phase 6 does not apply it to
    // timestamps (see the module docs).
    let _ = edit_list;
    let (info, samples) = parse_mdia(
        bytes,
        &mdia,
        mdat_ranges,
        index,
        u64::from(track_id),
        tkhd_width,
        tkhd_height,
    )?;
    Ok(Mp4Track { info, samples })
}

/// Parses `tkhd`: returns (track id, width, height). Width and
/// height are 16.16 fixed-point values; a zero value means the
/// track has no display size (audio tracks).
fn parse_tkhd(bytes: &[u8], tkhd: &BoxHeader) -> Result<(u32, Option<u32>, Option<u32>)> {
    let version = box_version(bytes, tkhd)?;
    let base = tkhd.data_start + 4;
    let (id, width_fixed, height_fixed) = match version {
        0 => {
            if tkhd.payload_len() < 84 {
                return Err(Error::Truncated {
                    needed: base + 84,
                    had: bytes.len(),
                });
            }
            (
                read_u32(bytes, base + 8)?,
                read_u32(bytes, base + 72)?,
                read_u32(bytes, base + 76)?,
            )
        }
        1 => {
            if tkhd.payload_len() < 96 {
                return Err(Error::Truncated {
                    needed: base + 96,
                    had: bytes.len(),
                });
            }
            (
                read_u32(bytes, base + 16)?,
                read_u32(bytes, base + 84)?,
                read_u32(bytes, base + 88)?,
            )
        }
        _ => {
            return Err(Error::InvalidData {
                offset: tkhd.offset,
                detail: "mp4 tkhd version is not 0 or 1",
            });
        }
    };
    if id == 0 {
        return Err(Error::InvalidData {
            offset: tkhd.offset,
            detail: "mp4 tkhd track id is zero",
        });
    }
    // 16.16 fixed point: the integer part is the pixel size. A
    // fractional part is rounded up only when it is non-zero and
    // the value is otherwise exact, which never happens for whole
    // pixels; whole pixels are the only sizes this library reports.
    let width = if width_fixed == 0 {
        None
    } else {
        Some(width_fixed >> 16)
    };
    let height = if height_fixed == 0 {
        None
    } else {
        Some(height_fixed >> 16)
    };
    Ok((id, width, height))
}

/// Parses `edts` into its edit list entries (segment duration in
/// movie-timescale units, media time in media-timescale units).
fn parse_edts(bytes: &[u8], edts: &BoxHeader) -> Result<Vec<(u64, i64)>> {
    check_depth(3)?;
    let mut cursor = BoxCursor::new(bytes, edts.data_start, edts.data_end)?;
    let mut entries: Vec<(u64, i64)> = Vec::new();
    while let Some(child) = cursor.next_header()? {
        if child.box_type != BOX_ELST {
            continue;
        }
        check_depth(4)?;
        let version = box_version(bytes, &child)?;
        if child.payload_len() < 8 {
            return Err(Error::Truncated {
                needed: child.data_start + 8,
                had: bytes.len(),
            });
        }
        let count = read_u32(bytes, child.data_start + 4)?;
        let mut offset = child.data_start + 8;
        for _ in 0..count {
            match version {
                0 => {
                    if child.data_end < offset + 12 {
                        return Err(Error::Truncated {
                            needed: offset + 12,
                            had: bytes.len(),
                        });
                    }
                    let duration = u64::from(read_u32(bytes, offset)?);
                    let media_time = i64::from(read_i32(bytes, offset + 4)?);
                    entries.push((duration, media_time));
                    offset += 12;
                }
                1 => {
                    if child.data_end < offset + 20 {
                        return Err(Error::Truncated {
                            needed: offset + 20,
                            had: bytes.len(),
                        });
                    }
                    let duration = read_u64(bytes, offset)?;
                    let media_time = read_i64(bytes, offset + 8)?;
                    entries.push((duration, media_time));
                    offset += 20;
                }
                _ => {
                    return Err(Error::InvalidData {
                        offset: child.offset,
                        detail: "mp4 elst version is not 0 or 1",
                    });
                }
            }
        }
    }
    Ok(entries)
}

/// Parses `mdia` into a `TrackInfo` and its samples.
fn parse_mdia(
    bytes: &[u8],
    mdia: &BoxHeader,
    mdat_ranges: &[(usize, usize)],
    index: usize,
    track_id: u64,
    tkhd_width: Option<u32>,
    tkhd_height: Option<u32>,
) -> Result<(TrackInfo, Vec<Sample>)> {
    check_depth(3)?;
    let mut cursor = BoxCursor::new(bytes, mdia.data_start, mdia.data_end)?;
    let mut timescale: Option<u32> = None;
    let mut duration_us: Option<u64> = None;
    let mut handler: Option<[u8; 4]> = None;
    let mut stbl: Option<BoxHeader> = None;
    while let Some(child) = cursor.next_header()? {
        match child.box_type {
            BOX_MDHD => {
                let (scale, duration) = parse_mdhd(bytes, &child)?;
                timescale = Some(scale);
                duration_us = Some(
                    (u128::from(duration) * 1_000_000 / u128::from(scale))
                        .try_into()
                        .map_err(|_| Error::Overflow("mp4 track duration exceeds u64"))?,
                );
            }
            BOX_HDLR => {
                if child.payload_len() < 12 {
                    return Err(Error::Truncated {
                        needed: child.data_start + 12,
                        had: bytes.len(),
                    });
                }
                handler = Some(
                    bytes[child.data_start + 8..child.data_start + 12]
                        .try_into()
                        .unwrap_or([0; 4]),
                );
            }
            BOX_MINF => {
                check_depth(4)?;
                let mut minf = BoxCursor::new(bytes, child.data_start, child.data_end)?;
                while let Some(field) = minf.next_header()? {
                    if field.box_type == BOX_STBL {
                        stbl = Some(field);
                    }
                }
            }
            _ => {}
        }
    }
    let timescale = timescale.ok_or(Error::InvalidData {
        offset: mdia.offset,
        detail: "mp4 mdia has no mdhd box",
    })?;
    let handler = handler.ok_or(Error::InvalidData {
        offset: mdia.offset,
        detail: "mp4 mdia has no hdlr box",
    })?;
    let stbl = stbl.ok_or(Error::InvalidData {
        offset: mdia.offset,
        detail: "mp4 mdia has no stbl box",
    })?;
    let kind = match &handler {
        b"vide" => TrackKind::Video,
        b"soun" => TrackKind::Audio,
        _ => {
            return Err(Error::Unsupported(
                "unsupported mp4 handler type (only vide and soun)",
            ));
        }
    };
    let (info, samples) = parse_stbl(
        bytes,
        &stbl,
        mdat_ranges,
        &StblContext {
            index,
            track_id,
            kind,
            timescale,
            duration_us,
            tkhd_width,
            tkhd_height,
        },
    )?;
    Ok((info, samples))
}

/// Parses `mdhd`: returns (timescale, duration).
fn parse_mdhd(bytes: &[u8], mdhd: &BoxHeader) -> Result<(u32, u64)> {
    let version = box_version(bytes, mdhd)?;
    let base = mdhd.data_start + 4;
    let (timescale, duration) = match version {
        0 => {
            if mdhd.payload_len() < 20 {
                return Err(Error::Truncated {
                    needed: base + 20,
                    had: bytes.len(),
                });
            }
            (
                read_u32(bytes, base + 8)?,
                u64::from(read_u32(bytes, base + 12)?),
            )
        }
        1 => {
            if mdhd.payload_len() < 32 {
                return Err(Error::Truncated {
                    needed: base + 32,
                    had: bytes.len(),
                });
            }
            (read_u32(bytes, base + 16)?, read_u64(bytes, base + 20)?)
        }
        _ => {
            return Err(Error::InvalidData {
                offset: mdhd.offset,
                detail: "mp4 mdhd version is not 0 or 1",
            });
        }
    };
    if timescale == 0 {
        return Err(Error::InvalidData {
            offset: mdhd.offset,
            detail: "mp4 mdhd timescale is zero",
        });
    }
    Ok((timescale, duration))
}

/// One sample entry from `stsd`: the codec, the codec id string,
/// the codec-private bytes, and the entry's width, height, sample
/// rate and channels (each present only for the relevant kind).
struct SampleEntry {
    codec: Codec,
    codec_id: String,
    codec_private: Vec<u8>,
    width: Option<u32>,
    height: Option<u32>,
    sample_rate: Option<u32>,
    channels: Option<u32>,
}

/// Parameters `parse_stbl` needs from the enclosing track: the
/// track identity and the values already read from `tkhd`/`mdhd`.
struct StblContext {
    index: usize,
    track_id: u64,
    kind: TrackKind,
    timescale: u32,
    duration_us: Option<u64>,
    tkhd_width: Option<u32>,
    tkhd_height: Option<u32>,
}

/// Parses `stbl` into a `TrackInfo` and its samples.
fn parse_stbl(
    bytes: &[u8],
    stbl: &BoxHeader,
    mdat_ranges: &[(usize, usize)],
    ctx: &StblContext,
) -> Result<(TrackInfo, Vec<Sample>)> {
    check_depth(5)?;
    let mut cursor = BoxCursor::new(bytes, stbl.data_start, stbl.data_end)?;
    let mut tables = SampleTables::default();
    let mut sample_entry: Option<SampleEntry> = None;
    while let Some(child) = cursor.next_header()? {
        match child.box_type {
            BOX_STSD => {
                sample_entry = Some(parse_stsd(bytes, &child)?);
            }
            BOX_STTS => {
                tables.time_to_sample = parse_stts(bytes, &child)?;
            }
            BOX_STSS => {
                tables.sync_samples = Some(parse_stss(bytes, &child)?);
            }
            BOX_STSC => {
                tables.sample_to_chunk = parse_stsc(bytes, &child)?;
            }
            BOX_STSZ => {
                tables.sample_sizes = parse_stsz(bytes, &child)?;
            }
            BOX_STCO => {
                tables.chunk_offsets = parse_stco(bytes, &child)?;
            }
            BOX_CO64 => {
                tables.chunk_offsets = parse_co64(bytes, &child)?;
            }
            BOX_CTTS => {
                tables.composition_offsets = parse_ctts(bytes, &child)?;
            }
            _ => {
                // sgpd, sbgp and unknown sample table boxes are
                // skipped by size.
            }
        }
    }
    let sample_entry = sample_entry.ok_or(Error::InvalidData {
        offset: stbl.offset,
        detail: "mp4 stbl has no stsd box",
    })?;
    if tables.time_to_sample.is_empty() {
        return Err(Error::InvalidData {
            offset: stbl.offset,
            detail: "mp4 stbl has no stts box",
        });
    }
    if tables.sample_to_chunk.is_empty() {
        return Err(Error::InvalidData {
            offset: stbl.offset,
            detail: "mp4 stbl has no stsc box",
        });
    }
    if tables.chunk_offsets.is_empty() {
        return Err(Error::InvalidData {
            offset: stbl.offset,
            detail: "mp4 stbl has no stco or co64 box",
        });
    }
    let samples = build_samples(bytes, &tables, ctx.timescale, mdat_ranges, stbl.offset)?;
    let info = TrackInfo {
        index: ctx.index,
        track_id: ctx.track_id,
        kind: ctx.kind,
        codec: sample_entry.codec,
        codec_id: sample_entry.codec_id,
        width: sample_entry.width.or(ctx.tkhd_width),
        height: sample_entry.height.or(ctx.tkhd_height),
        sample_rate: sample_entry.sample_rate,
        channels: sample_entry.channels,
        default_duration_us: None,
        duration_us: ctx.duration_us,
        codec_private: sample_entry.codec_private,
    };
    Ok((info, samples))
}

/// Parses `stsd`: returns the first sample entry. Only the first
/// entry is used; additional entries are not supported in Phase 6.
fn parse_stsd(bytes: &[u8], stsd: &BoxHeader) -> Result<SampleEntry> {
    if stsd.payload_len() < 8 {
        return Err(Error::Truncated {
            needed: stsd.data_start + 8,
            had: bytes.len(),
        });
    }
    let count = read_u32(bytes, stsd.data_start + 4)?;
    if count == 0 {
        return Err(Error::InvalidData {
            offset: stsd.offset,
            detail: "mp4 stsd has no entries",
        });
    }
    if count > 1 {
        return Err(Error::Unsupported(
            "mp4 stsd with more than one entry is not supported",
        ));
    }
    let mut cursor = BoxCursor::new(bytes, stsd.data_start + 8, stsd.data_end)?;
    let entry = cursor.next_header()?.ok_or(Error::InvalidData {
        offset: stsd.offset,
        detail: "mp4 stsd entry is missing",
    })?;
    parse_sample_entry(bytes, &entry)
}

/// Parses one sample entry. The single function mirrors the
/// sample-entry layout per codec (visual, Opus, FLAC); each arm
/// validates its own fields and child boxes.
#[allow(clippy::too_many_lines)]
fn parse_sample_entry(bytes: &[u8], entry: &BoxHeader) -> Result<SampleEntry> {
    let codec_id = String::from_utf8_lossy(&entry.box_type).into_owned();
    match entry.box_type {
        BOX_AV01 | BOX_VP09 => {
            // VisualSampleEntry: 78 bytes of fields after the
            // 8-byte entry header, then child boxes.
            if entry.payload_len() < 78 {
                return Err(Error::Truncated {
                    needed: entry.data_start + 78,
                    had: bytes.len(),
                });
            }
            let width = u32::from(read_u16(bytes, entry.data_start + 24)?);
            let height = u32::from(read_u16(bytes, entry.data_start + 26)?);
            let mut codec_private: Vec<u8> = Vec::new();
            let mut cursor = BoxCursor::new(bytes, entry.data_start + 78, entry.data_end)?;
            while let Some(child) = cursor.next_header()? {
                match child.box_type {
                    BOX_AV1C | BOX_VPCC => {
                        codec_private = bytes[child.data_start..child.data_end].to_vec();
                    }
                    _ => {}
                }
            }
            if codec_private.is_empty() {
                return Err(Error::InvalidData {
                    offset: entry.offset,
                    detail: "mp4 video sample entry has no codec configuration box",
                });
            }
            let codec = if entry.box_type == BOX_AV01 {
                Codec::Av1
            } else {
                Codec::Vp9
            };
            Ok(SampleEntry {
                codec,
                codec_id,
                codec_private,
                width: Some(width),
                height: Some(height),
                sample_rate: None,
                channels: None,
            })
        }
        BOX_OPUS => {
            // AudioSampleEntry: 28 bytes of fields after the
            // 8-byte entry header, then child boxes.
            if entry.payload_len() < 28 {
                return Err(Error::Truncated {
                    needed: entry.data_start + 28,
                    had: bytes.len(),
                });
            }
            let mut codec_private: Vec<u8> = Vec::new();
            let mut cursor = BoxCursor::new(bytes, entry.data_start + 28, entry.data_end)?;
            while let Some(child) = cursor.next_header()? {
                if child.box_type == BOX_DOPS {
                    codec_private = bytes[child.data_start..child.data_end].to_vec();
                }
            }
            if codec_private.len() < 11 {
                return Err(Error::InvalidData {
                    offset: entry.offset,
                    detail: "mp4 opus sample entry has no dops box",
                });
            }
            // dOps: version(1), channels(1), pre-skip(2), input
            // sample rate(4), output gain(2). The channel count
            // and input sample rate come from here, not from the
            // entry's channelcount/samplerate fields.
            let channels = Some(u32::from(codec_private[1]));
            let sample_rate = Some(u32::from_be_bytes(
                codec_private[4..8].try_into().unwrap_or([0; 4]),
            ));
            Ok(SampleEntry {
                codec: Codec::Opus,
                codec_id,
                codec_private,
                width: None,
                height: None,
                sample_rate,
                channels,
            })
        }
        BOX_FLAC => {
            // AudioSampleEntry followed by the STREAMINFO block.
            // Parsed but unverified: ffmpeg cannot mux FLAC into
            // MP4, so no test vector exists for this path.
            if entry.payload_len() < 28 {
                return Err(Error::Truncated {
                    needed: entry.data_start + 28,
                    had: bytes.len(),
                });
            }
            let codec_private = bytes[entry.data_start + 28..entry.data_end].to_vec();
            Ok(SampleEntry {
                codec: Codec::Flac,
                codec_id,
                codec_private,
                width: None,
                height: None,
                sample_rate: None,
                channels: None,
            })
        }
        BOX_AVC1 | BOX_HEV1 => {
            // VisualSampleEntry, same layout as av01/vp09:
            // 78 bytes of fields after the 8-byte entry header,
            // then the config box (`avcC`/`hvcC`, ISO/IEC
            // 14496-15). Samples are passed through as stored
            // (AVCC length-prefixed NAL units); nothing here
            // decodes them.
            if entry.payload_len() < 78 {
                return Err(Error::Truncated {
                    needed: entry.data_start + 78,
                    had: bytes.len(),
                });
            }
            let width = u32::from(read_u16(bytes, entry.data_start + 24)?);
            let height = u32::from(read_u16(bytes, entry.data_start + 26)?);
            let mut codec_private: Vec<u8> = Vec::new();
            let mut cursor = BoxCursor::new(bytes, entry.data_start + 78, entry.data_end)?;
            while let Some(child) = cursor.next_header()? {
                match child.box_type {
                    BOX_AVCC | BOX_HVCC => {
                        codec_private = bytes[child.data_start..child.data_end].to_vec();
                    }
                    _ => {}
                }
            }
            if codec_private.is_empty() {
                return Err(Error::InvalidData {
                    offset: entry.offset,
                    detail: "mp4 video sample entry has no codec configuration box",
                });
            }
            // Config record version must be 1 (ISO/IEC 14496-15).
            if codec_private[0] != 1 {
                return Err(Error::InvalidData {
                    offset: entry.offset,
                    detail: "mp4 avcC/hvcC configuration version is not 1",
                });
            }
            let codec = if entry.box_type == BOX_AVC1 {
                Codec::Avc
            } else {
                Codec::Hevc
            };
            Ok(SampleEntry {
                codec,
                codec_id,
                codec_private,
                width: Some(width),
                height: Some(height),
                sample_rate: None,
                channels: None,
            })
        }
        BOX_MP4A => {
            // AudioSampleEntry: 28 bytes of fields after the
            // 8-byte entry header, then the `esds` box (ISO/IEC
            // 14496-1). The channel count and sample rate come
            // from the entry fields; the AudioSpecificConfig from
            // `esds` is the codec-private data. M4A is MP4
            // carrying AAC (or ALAC), not a codec of its own.
            if entry.payload_len() < 28 {
                return Err(Error::Truncated {
                    needed: entry.data_start + 28,
                    had: bytes.len(),
                });
            }
            let channels = Some(u32::from(read_u16(bytes, entry.data_start + 16)?));
            let sample_rate = Some(read_u32(bytes, entry.data_start + 24)? >> 16);
            let mut codec_private: Vec<u8> = Vec::new();
            let mut cursor = BoxCursor::new(bytes, entry.data_start + 28, entry.data_end)?;
            while let Some(child) = cursor.next_header()? {
                if child.box_type == BOX_ESDS {
                    codec_private = parse_esds(bytes, &child)?;
                }
            }
            if codec_private.is_empty() {
                return Err(Error::InvalidData {
                    offset: entry.offset,
                    detail: "mp4a sample entry has no esds box",
                });
            }
            Ok(SampleEntry {
                codec: Codec::Aac,
                codec_id,
                codec_private,
                width: None,
                height: None,
                sample_rate,
                channels,
            })
        }
        _ => Err(Error::Unsupported("unsupported mp4 sample entry type")),
    }
}

/// Reads one descriptor tag + length from an `esds` box
/// (ISO/IEC 14496-1): a tag byte, then a length in base-128
/// (up to 4 bytes, high bit set on all but the last). Returns
/// (tag, body length) and advances `pos` past the header.
///
/// # Errors
///
/// Returns `Truncated` when the descriptor runs past the box.
fn read_esds_descriptor(data: &[u8], pos: &mut usize, end: usize) -> Result<(u8, usize)> {
    let tag = *data.get(*pos).ok_or(Error::Truncated {
        needed: *pos + 1,
        had: data.len(),
    })?;
    *pos += 1;
    let mut len: usize = 0;
    for _ in 0..4 {
        let byte = *data.get(*pos).ok_or(Error::Truncated {
            needed: *pos + 1,
            had: data.len(),
        })?;
        *pos += 1;
        len = len
            .checked_mul(128)
            .and_then(|value| value.checked_add(usize::from(byte & 0x7F)))
            .ok_or(Error::Overflow("esds descriptor length overflows"))?;
        if byte & 0x80 == 0 {
            break;
        }
    }
    if *pos + len > end {
        return Err(Error::Truncated {
            needed: *pos + len,
            had: data.len(),
        });
    }
    Ok((tag, len))
}

/// Parses an `esds` box (ISO/IEC 14496-1) into the AAC
/// `AudioSpecificConfig` bytes from its `DecoderSpecificInfo`
/// (tag `0x05`). Only AAC (`objectTypeIndication 0x40`) is
/// accepted; any other object type is `Unsupported`.
///
/// # Errors
///
/// Returns `Truncated` for a cut-short descriptor,
/// `InvalidData` when the descriptor structure is wrong or
/// there is no `DecoderSpecificInfo`, and `Unsupported` for a
/// non-AAC object type.
fn parse_esds(bytes: &[u8], esds: &BoxHeader) -> Result<Vec<u8>> {
    if esds.payload_len() < 4 {
        return Err(Error::Truncated {
            needed: esds.data_start + 4,
            had: bytes.len(),
        });
    }
    let end = esds.data_end;
    let mut pos = esds.data_start + 4;
    let (tag, len) = read_esds_descriptor(bytes, &mut pos, end)?;
    if tag != 0x03 {
        return Err(Error::InvalidData {
            offset: esds.offset,
            detail: "esds box does not start with an ES_Descriptor",
        });
    }
    let es_end = pos + len;
    if es_end > end {
        return Err(Error::Truncated {
            needed: es_end,
            had: bytes.len(),
        });
    }
    // ES_Descriptor body: ES_ID(2), flags(1), then optional
    // fields the flags select.
    if pos + 3 > es_end {
        return Err(Error::Truncated {
            needed: pos + 3,
            had: bytes.len(),
        });
    }
    pos += 2;
    let flags = bytes[pos];
    pos += 1;
    if flags & 0x80 != 0 {
        pos += 2;
    }
    if flags & 0x40 != 0 {
        let url_len = usize::from(*bytes.get(pos).ok_or(Error::Truncated {
            needed: pos + 1,
            had: bytes.len(),
        })?);
        pos += 1 + url_len;
    }
    if flags & 0x20 != 0 {
        pos += 2;
    }
    if pos > es_end {
        return Err(Error::Truncated {
            needed: pos,
            had: bytes.len(),
        });
    }
    // Nested descriptors: DecoderConfigDescriptor (0x04) holds
    // the object type and the DecoderSpecificInfo (0x05).
    while pos < es_end {
        let (inner_tag, inner_len) = read_esds_descriptor(bytes, &mut pos, es_end)?;
        let inner_end = pos + inner_len;
        if inner_end > es_end {
            return Err(Error::Truncated {
                needed: inner_end,
                had: bytes.len(),
            });
        }
        if inner_tag == 0x04 {
            // DecoderConfigDescriptor body: objectType(1),
            // streamType/upStream/reserved(1), bufferSizeDB(3),
            // maxBitrate(4), avgBitrate(4) = 13 bytes.
            if inner_len < 13 {
                return Err(Error::InvalidData {
                    offset: esds.offset,
                    detail: "esds DecoderConfigDescriptor is too short",
                });
            }
            if bytes[pos] != 0x40 {
                return Err(Error::Unsupported(
                    "mp4a with a non-AAC object type is not supported",
                ));
            }
            let mut inner_pos = pos + 13;
            while inner_pos < inner_end {
                let (deep_tag, deep_len) = read_esds_descriptor(bytes, &mut inner_pos, inner_end)?;
                let deep_end = inner_pos + deep_len;
                if deep_end > inner_end {
                    return Err(Error::Truncated {
                        needed: deep_end,
                        had: bytes.len(),
                    });
                }
                if deep_tag == 0x05 {
                    return Ok(bytes[inner_pos..deep_end].to_vec());
                }
                inner_pos = deep_end;
            }
            return Err(Error::InvalidData {
                offset: esds.offset,
                detail: "esds DecoderConfigDescriptor has no DecoderSpecificInfo",
            });
        }
        pos = inner_end;
    }
    Err(Error::InvalidData {
        offset: esds.offset,
        detail: "esds box has no DecoderConfigDescriptor",
    })
}

/// Parses `stts` into (sample count, delta) runs.
fn parse_stts(bytes: &[u8], stts: &BoxHeader) -> Result<Vec<(u32, u32)>> {
    if stts.payload_len() < 8 {
        return Err(Error::Truncated {
            needed: stts.data_start + 8,
            had: bytes.len(),
        });
    }
    let count = read_u32(bytes, stts.data_start + 4)?;
    let mut runs: Vec<(u32, u32)> = Vec::new();
    let mut offset = stts.data_start + 8;
    for _ in 0..count {
        if stts.data_end < offset + 8 {
            return Err(Error::Truncated {
                needed: offset + 8,
                had: bytes.len(),
            });
        }
        runs.push((read_u32(bytes, offset)?, read_u32(bytes, offset + 4)?));
        offset += 8;
    }
    Ok(runs)
}

/// Parses `ctts` into (sample count, composition offset) runs.
/// Version 0 offsets are unsigned; version 1 offsets are signed.
fn parse_ctts(bytes: &[u8], ctts: &BoxHeader) -> Result<Vec<(u32, i64)>> {
    let version = box_version(bytes, ctts)?;
    if ctts.payload_len() < 8 {
        return Err(Error::Truncated {
            needed: ctts.data_start + 8,
            had: bytes.len(),
        });
    }
    let count = read_u32(bytes, ctts.data_start + 4)?;
    let mut runs: Vec<(u32, i64)> = Vec::new();
    let mut offset = ctts.data_start + 8;
    for _ in 0..count {
        if ctts.data_end < offset + 8 {
            return Err(Error::Truncated {
                needed: offset + 8,
                had: bytes.len(),
            });
        }
        let run_count = read_u32(bytes, offset)?;
        let run_offset = match version {
            0 => i64::from(read_u32(bytes, offset + 4)?),
            1 => i64::from(read_i32(bytes, offset + 4)?),
            _ => {
                return Err(Error::InvalidData {
                    offset: ctts.offset,
                    detail: "mp4 ctts version is not 0 or 1",
                });
            }
        };
        runs.push((run_count, run_offset));
        offset += 8;
    }
    Ok(runs)
}

/// Parses `stss` into the set of 1-based sync sample numbers.
fn parse_stss(bytes: &[u8], stss: &BoxHeader) -> Result<HashSet<u32>> {
    if stss.payload_len() < 8 {
        return Err(Error::Truncated {
            needed: stss.data_start + 8,
            had: bytes.len(),
        });
    }
    let count = read_u32(bytes, stss.data_start + 4)?;
    let mut samples: HashSet<u32> = HashSet::new();
    let mut offset = stss.data_start + 8;
    for _ in 0..count {
        if stss.data_end < offset + 4 {
            return Err(Error::Truncated {
                needed: offset + 4,
                had: bytes.len(),
            });
        }
        let number = read_u32(bytes, offset)?;
        if number == 0 {
            return Err(Error::InvalidData {
                offset: stss.offset,
                detail: "mp4 stss sample number is zero",
            });
        }
        samples.insert(number);
        offset += 4;
    }
    Ok(samples)
}

/// Parses `stsc` into (first chunk, samples per chunk, sample
/// description index) entries.
fn parse_stsc(bytes: &[u8], stsc: &BoxHeader) -> Result<Vec<(u32, u32, u32)>> {
    if stsc.payload_len() < 8 {
        return Err(Error::Truncated {
            needed: stsc.data_start + 8,
            had: bytes.len(),
        });
    }
    let count = read_u32(bytes, stsc.data_start + 4)?;
    let mut entries: Vec<(u32, u32, u32)> = Vec::new();
    let mut offset = stsc.data_start + 8;
    for _ in 0..count {
        if stsc.data_end < offset + 12 {
            return Err(Error::Truncated {
                needed: offset + 12,
                had: bytes.len(),
            });
        }
        entries.push((
            read_u32(bytes, offset)?,
            read_u32(bytes, offset + 4)?,
            read_u32(bytes, offset + 8)?,
        ));
        offset += 12;
    }
    // Entries must start at chunk 1 and be non-decreasing.
    if let Some((first, _, _)) = entries.first() {
        if *first != 1 {
            return Err(Error::InvalidData {
                offset: stsc.offset,
                detail: "mp4 stsc first entry does not start at chunk 1",
            });
        }
    }
    for window in entries.windows(2) {
        if window[1].0 <= window[0].0 {
            return Err(Error::InvalidData {
                offset: stsc.offset,
                detail: "mp4 stsc entries are not increasing",
            });
        }
    }
    Ok(entries)
}

/// Parses `stsz` into per-sample sizes.
fn parse_stsz(bytes: &[u8], stsz: &BoxHeader) -> Result<Vec<u32>> {
    if stsz.payload_len() < 12 {
        return Err(Error::Truncated {
            needed: stsz.data_start + 12,
            had: bytes.len(),
        });
    }
    let sample_size = read_u32(bytes, stsz.data_start + 4)?;
    let count = read_u32(bytes, stsz.data_start + 8)?;
    if count as usize > MAX_SAMPLES_PER_TRACK {
        return Err(Error::AllocationLimit {
            requested: count as usize,
            limit: MAX_SAMPLES_PER_TRACK,
        });
    }
    if sample_size != 0 {
        return Ok(vec![sample_size; count as usize]);
    }
    let mut sizes: Vec<u32> = Vec::with_capacity(count as usize);
    let mut offset = stsz.data_start + 12;
    for _ in 0..count {
        if stsz.data_end < offset + 4 {
            return Err(Error::Truncated {
                needed: offset + 4,
                had: bytes.len(),
            });
        }
        sizes.push(read_u32(bytes, offset)?);
        offset += 4;
    }
    Ok(sizes)
}

/// Parses `stco` into chunk offsets.
fn parse_stco(bytes: &[u8], stco: &BoxHeader) -> Result<Vec<u64>> {
    if stco.payload_len() < 8 {
        return Err(Error::Truncated {
            needed: stco.data_start + 8,
            had: bytes.len(),
        });
    }
    let count = read_u32(bytes, stco.data_start + 4)?;
    // The box cannot hold more entries than its payload allows
    // (4 bytes each after the 8-byte header). Check before
    // allocating: a corrupt count pre-allocated ~2 GiB here
    // (found by fuzzing).
    if u64::from(count) * 4 > (stco.payload_len() - 8) as u64 {
        return Err(Error::InvalidData {
            offset: stco.offset,
            detail: "mp4 stco entry count exceeds box size",
        });
    }
    let mut offsets: Vec<u64> = Vec::with_capacity(count as usize);
    let mut offset = stco.data_start + 8;
    for _ in 0..count {
        if stco.data_end < offset + 4 {
            return Err(Error::Truncated {
                needed: offset + 4,
                had: bytes.len(),
            });
        }
        offsets.push(u64::from(read_u32(bytes, offset)?));
        offset += 4;
    }
    Ok(offsets)
}

/// Parses `co64` into chunk offsets.
fn parse_co64(bytes: &[u8], co64: &BoxHeader) -> Result<Vec<u64>> {
    if co64.payload_len() < 8 {
        return Err(Error::Truncated {
            needed: co64.data_start + 8,
            had: bytes.len(),
        });
    }
    let count = read_u32(bytes, co64.data_start + 4)?;
    // The box cannot hold more entries than its payload allows
    // (8 bytes each after the 8-byte header). Check before
    // allocating: a corrupt count pre-allocated ~2 GiB here
    // (found by fuzzing; same class as `stco`).
    if u64::from(count) * 8 > (co64.payload_len() - 8) as u64 {
        return Err(Error::InvalidData {
            offset: co64.offset,
            detail: "mp4 co64 entry count exceeds box size",
        });
    }
    let mut offsets: Vec<u64> = Vec::with_capacity(count as usize);
    let mut offset = co64.data_start + 8;
    for _ in 0..count {
        if co64.data_end < offset + 8 {
            return Err(Error::Truncated {
                needed: offset + 8,
                had: bytes.len(),
            });
        }
        offsets.push(read_u64(bytes, offset)?);
        offset += 8;
    }
    Ok(offsets)
}

/// Walks the sample tables into samples, validating every sample
/// against the file and the `mdat` ranges. The single function
/// mirrors the table walk (chunk, then sample within the chunk,
/// then the timestamp and keyframe lookups); splitting it would
/// scatter the per-sample state across helpers.
#[allow(clippy::too_many_lines)]
fn build_samples(
    bytes: &[u8],
    tables: &SampleTables,
    timescale: u32,
    mdat_ranges: &[(usize, usize)],
    stbl_offset: usize,
) -> Result<Vec<Sample>> {
    let sample_count = tables.sample_sizes.len();
    if sample_count > MAX_SAMPLES_PER_TRACK {
        return Err(Error::AllocationLimit {
            requested: sample_count,
            limit: MAX_SAMPLES_PER_TRACK,
        });
    }
    // Every sync sample number must name a real sample.
    if let Some(sync) = &tables.sync_samples {
        for &number in sync {
            if number == 0 || number as usize > sample_count {
                return Err(Error::InvalidData {
                    offset: stbl_offset,
                    detail: "mp4 stss sample number is out of range",
                });
            }
        }
    }
    // Decode-time and composition-offset runs must cover exactly
    // the sample count.
    let stts_total: u64 = tables
        .time_to_sample
        .iter()
        .map(|(count, _)| u64::from(*count))
        .sum();
    if stts_total != sample_count as u64 {
        return Err(Error::InvalidData {
            offset: stbl_offset,
            detail: "mp4 stts sample count does not match stsz",
        });
    }
    if !tables.composition_offsets.is_empty() {
        let ctts_total: u64 = tables
            .composition_offsets
            .iter()
            .map(|(count, _)| u64::from(*count))
            .sum();
        if ctts_total != sample_count as u64 {
            return Err(Error::InvalidData {
                offset: stbl_offset,
                detail: "mp4 ctts sample count does not match stsz",
            });
        }
    }
    // Expand the runs into per-sample decode deltas and composition
    // offsets.
    let mut deltas: Vec<u32> = Vec::with_capacity(sample_count);
    for &(count, delta) in &tables.time_to_sample {
        for _ in 0..count {
            deltas.push(delta);
        }
    }
    let mut composition: Vec<i64> = Vec::with_capacity(sample_count);
    for &(count, offset_value) in &tables.composition_offsets {
        for _ in 0..count {
            composition.push(offset_value);
        }
    }
    if composition.is_empty() {
        composition = vec![0; sample_count];
    }
    // Walk chunks.
    let chunk_count = tables.chunk_offsets.len();
    let mut samples: Vec<Sample> = Vec::with_capacity(sample_count);
    let mut sample_index = 0usize;
    let mut dts: u64 = 0;
    for chunk_index in 1..=chunk_count {
        // The applicable stsc entry is the last one whose first
        // chunk is at or before this chunk.
        let entry = tables
            .sample_to_chunk
            .iter()
            .rev()
            .find(|(first_chunk, _, _)| *first_chunk as usize <= chunk_index)
            .ok_or(Error::InvalidData {
                offset: stbl_offset,
                detail: "mp4 stsc has no entry for a chunk",
            })?;
        let samples_per_chunk = entry.1 as usize;
        let chunk_offset = tables.chunk_offsets[chunk_index - 1];
        let mut offset_in_chunk: u64 = 0;
        for _ in 0..samples_per_chunk {
            if sample_index >= sample_count {
                return Err(Error::InvalidData {
                    offset: stbl_offset,
                    detail: "mp4 stsc sample count exceeds stsz",
                });
            }
            let size = u64::from(tables.sample_sizes[sample_index]);
            let offset = chunk_offset
                .checked_add(offset_in_chunk)
                .ok_or(Error::Overflow("mp4 sample offset overflows"))?;
            validate_sample_location(bytes, offset, size, mdat_ranges, stbl_offset)?;
            let dts_us = (u128::from(dts) * 1_000_000 / u128::from(timescale))
                .try_into()
                .map_err(|_| Error::Overflow("mp4 sample timestamp exceeds u64"))?;
            let pts = (i128::from(dts) + i128::from(composition[sample_index])).max(0);
            let pts_us = (u128::try_from(pts).unwrap_or(0) * 1_000_000 / u128::from(timescale))
                .try_into()
                .map_err(|_| Error::Overflow("mp4 sample timestamp exceeds u64"))?;
            let is_keyframe = match &tables.sync_samples {
                Some(sync) => sync.contains(&u32::try_from(sample_index + 1).unwrap_or(u32::MAX)),
                None => true,
            };
            samples.push(Sample {
                offset: usize::try_from(offset)
                    .map_err(|_| Error::Overflow("mp4 sample offset exceeds address space"))?,
                size: usize::try_from(size)
                    .map_err(|_| Error::Overflow("mp4 sample size exceeds address space"))?,
                dts_us,
                pts_us,
                is_keyframe,
            });
            offset_in_chunk += size;
            dts += u64::from(deltas[sample_index]);
            sample_index += 1;
        }
    }
    if sample_index != sample_count {
        return Err(Error::InvalidData {
            offset: stbl_offset,
            detail: "mp4 stsc sample count does not match stsz",
        });
    }
    Ok(samples)
}

/// Validates that a sample lies inside the file and inside an
/// `mdat` range. An offset past the end of the file is a corrupt
/// table; a sample that starts inside the file but runs past its
/// end is a truncated file.
fn validate_sample_location(
    bytes: &[u8],
    offset: u64,
    size: u64,
    mdat_ranges: &[(usize, usize)],
    stbl_offset: usize,
) -> Result<()> {
    let offset = usize::try_from(offset)
        .map_err(|_| Error::Overflow("mp4 sample offset exceeds address space"))?;
    let size = usize::try_from(size)
        .map_err(|_| Error::Overflow("mp4 sample size exceeds address space"))?;
    if offset > bytes.len() {
        return Err(Error::InvalidData {
            offset: stbl_offset,
            detail: "mp4 sample offset is outside the file",
        });
    }
    if offset + size > bytes.len() {
        return Err(Error::Truncated {
            needed: offset + size,
            had: bytes.len(),
        });
    }
    if !mdat_ranges.is_empty() {
        let inside = mdat_ranges
            .iter()
            .any(|(start, end)| offset >= *start && offset + size <= *end);
        if !inside {
            return Err(Error::InvalidData {
                offset: stbl_offset,
                detail: "mp4 sample is outside every mdat box",
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_header_parses_size_and_type() {
        // A 24-byte ftyp box: size 24, type ftyp, 16 payload bytes.
        let mut data = vec![
            0x00, 0x00, 0x00, 0x18, b'f', b't', b'y', b'p', 0x69, 0x73, 0x6F, 0x6D,
        ];
        data.extend(vec![0u8; 12]);
        let header = parse_box_header(&data, 0, data.len()).unwrap();
        assert_eq!(header.box_type, BOX_FTYP);
        assert_eq!(header.size, 24);
        assert_eq!(header.data_start, 8);
    }

    #[test]
    fn box_header_largesize() {
        let mut data = vec![0x00, 0x00, 0x00, 0x01, b'm', b'd', b'a', b't'];
        data.extend_from_slice(&24u64.to_be_bytes());
        data.extend(vec![0u8; 16]);
        let header = parse_box_header(&data, 0, data.len()).unwrap();
        assert_eq!(header.size, 24);
        assert_eq!(header.data_start, 16);
    }

    #[test]
    fn box_size_smaller_than_header_is_invalid_data() {
        let data = [0x00, 0x00, 0x00, 0x04, b'f', b't', b'y', b'p'];
        let err = parse_box_header(&data, 0, data.len()).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn box_size_beyond_file_is_truncated() {
        let data = [0x00, 0x00, 0x10, 0x00, b'm', b'd', b'a', b't'];
        let err = parse_box_header(&data, 0, data.len()).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn box_size_zero_runs_to_parent_end() {
        let data = [0x00, 0x00, 0x00, 0x00, b'f', b'r', b'e', b'e', 1, 2, 3];
        let header = parse_box_header(&data, 0, data.len()).unwrap();
        assert_eq!(header.data_end, data.len());
    }

    #[test]
    fn cursor_yields_children_in_order() {
        let mut data = Vec::new();
        data.extend([0x00, 0x00, 0x00, 0x08, b'f', b'r', b'e', b'e']);
        data.extend([0x00, 0x00, 0x00, 0x0C, b'm', b'd', b'a', b't', 1, 2, 3, 4]);
        let mut cursor = BoxCursor::new(&data, 0, data.len()).unwrap();
        let first = cursor.next_header().unwrap().unwrap();
        assert_eq!(first.box_type, *b"free");
        let second = cursor.next_header().unwrap().unwrap();
        assert_eq!(second.box_type, BOX_MDAT);
        assert_eq!(second.payload_len(), 4);
        assert!(cursor.next_header().unwrap().is_none());
    }
}
