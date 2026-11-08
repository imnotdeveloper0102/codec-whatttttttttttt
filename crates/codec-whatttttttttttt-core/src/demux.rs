//! Common demuxer for WebM/Matroska and MP4 (Phase 6).
//!
//! `Demuxer::new` probes the container from its magic bytes — the
//! EBML header (`0x1A45DFA3`) for WebM/Matroska, `ftyp` at offset 4
//! for MP4 — and delegates to the matching demuxer. WAV and Ogg keep
//! their own demuxers (`WavDemuxer`, `OggDemuxer`); this module does
//! not wrap them, so existing callers are unaffected.

use std::io::Read;

use crate::byte_source::BoundedReader;
use crate::error::{Error, Result};
use crate::frame::{Codec, Metadata, Packet};
use crate::limits::MAX_MP4_FILE_BYTES;
use crate::matroska::MatroskaDemuxer;
use crate::mp4_box::Mp4Demuxer;

/// Whether a track carries video or audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    /// Video track.
    Video,
    /// Audio track.
    Audio,
}

/// One container track, as reported by both the `WebM` and MP4
/// demuxers. `codec_private` holds the codec configuration bytes
/// (`CodecPrivate` in Matroska, the `av1C`/`dOps`/`vpcC` box payload
/// in MP4) that a decoder constructor needs.
#[derive(Debug, Clone)]
pub struct TrackInfo {
    /// 0-based order in the file.
    pub index: usize,
    /// Container track number (Matroska track number, MP4 track ID).
    pub track_id: u64,
    /// Video or audio.
    pub kind: TrackKind,
    /// Library codec for the track.
    pub codec: Codec,
    /// Codec identifier string (`V_AV1`, `av01`, `A_OPUS`, ...).
    pub codec_id: String,
    /// Coded width in pixels, for video tracks.
    pub width: Option<u32>,
    /// Coded height in pixels, for video tracks.
    pub height: Option<u32>,
    /// Sample rate in Hz, for audio tracks.
    pub sample_rate: Option<u32>,
    /// Channel count, for audio tracks.
    pub channels: Option<u32>,
    /// Default frame duration in microseconds, when the container
    /// declares one (Matroska `DefaultDuration`).
    pub default_duration_us: Option<u64>,
    /// Track duration in microseconds, when the container declares
    /// one.
    pub duration_us: Option<u64>,
    /// Codec configuration bytes.
    pub codec_private: Vec<u8>,
}

/// Container kind, from probing the first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    /// WebM/Matroska (EBML header).
    Webm,
    /// MP4 (ISO BMFF, `ftyp` box).
    Mp4,
}

/// Probes the container kind from the first bytes: the EBML header
/// magic identifies WebM/Matroska, `ftyp` at offset 4 identifies MP4.
///
/// # Errors
///
/// Returns `Truncated` when fewer than 8 bytes are given, and
/// `Unsupported` when the magic matches neither container.
pub fn probe_container(data: &[u8]) -> Result<ContainerKind> {
    if data.len() < 8 {
        return Err(Error::Truncated {
            needed: 8,
            had: data.len(),
        });
    }
    if data[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return Ok(ContainerKind::Webm);
    }
    if &data[4..8] == b"ftyp" {
        return Ok(ContainerKind::Mp4);
    }
    Err(Error::Unsupported(
        "unknown container signature (not ebml or mp4)",
    ))
}

/// Demuxer for WebM/Matroska and MP4 files. Packets are produced in
/// the container's demux order with timestamps in microseconds.
#[derive(Debug)]
pub enum Demuxer {
    /// WebM/Matroska.
    Webm(MatroskaDemuxer),
    /// MP4.
    Mp4(Mp4Demuxer),
}

impl Demuxer {
    /// Reads and demuxes a WebM/Matroska or MP4 stream from
    /// `reader`. The whole file is bounded by the MP4/WebM limit
    /// (512 MiB, the larger of the two container limits).
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a cut-short file, `Unsupported` for
    /// an unknown container or codec, `InvalidData` for a corrupt
    /// container, and `AllocationLimit` for a file above the limit.
    pub fn new<R: Read>(reader: R) -> Result<Self> {
        let mut reader = BoundedReader::new(reader, MAX_MP4_FILE_BYTES)?;
        let bytes = reader.read_all_bounded()?;
        Self::from_bytes(&bytes)
    }

    /// Demuxes an already-read file.
    ///
    /// # Errors
    ///
    /// The same errors as `new`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        match probe_container(bytes)? {
            ContainerKind::Webm => Ok(Self::Webm(MatroskaDemuxer::parse(bytes)?)),
            ContainerKind::Mp4 => Ok(Self::Mp4(Mp4Demuxer::parse(bytes)?)),
        }
    }

    /// Container kind.
    #[must_use]
    pub fn container_kind(&self) -> ContainerKind {
        match self {
            Self::Webm(_) => ContainerKind::Webm,
            Self::Mp4(_) => ContainerKind::Mp4,
        }
    }

    /// Tracks in file order.
    #[must_use]
    pub fn tracks(&self) -> &[TrackInfo] {
        match self {
            Self::Webm(demuxer) => demuxer.tracks(),
            Self::Mp4(demuxer) => demuxer.tracks(),
        }
    }

    /// Duration in microseconds, when the container declares one.
    #[must_use]
    pub fn duration_us(&self) -> Option<u64> {
        match self {
            Self::Webm(demuxer) => demuxer.duration_us(),
            Self::Mp4(demuxer) => demuxer.duration_us(),
        }
    }

    /// Container metadata.
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        match self {
            Self::Webm(demuxer) => demuxer.metadata(),
            Self::Mp4(demuxer) => demuxer.metadata(),
        }
    }

    /// Number of packets in the file.
    #[must_use]
    pub fn packet_count(&self) -> usize {
        match self {
            Self::Webm(demuxer) => demuxer.packet_count(),
            Self::Mp4(demuxer) => demuxer.packet_count(),
        }
    }

    /// Returns the next packet, or `None` at end of stream.
    ///
    /// # Errors
    ///
    /// This implementation never fails after construction; the
    /// `Result` exists so a streaming demuxer can report read
    /// errors here.
    pub fn next_packet(&mut self) -> Result<Option<Packet>> {
        match self {
            Self::Webm(demuxer) => demuxer.next_packet(),
            Self::Mp4(demuxer) => demuxer.next_packet(),
        }
    }

    /// Seeks to the keyframe at or before `timestamp_us`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when the seek index (Cues or `stss`)
    /// is corrupt.
    pub fn seek(&mut self, timestamp_us: u64) -> Result<()> {
        match self {
            Self::Webm(demuxer) => demuxer.seek(timestamp_us),
            Self::Mp4(demuxer) => demuxer.seek(timestamp_us),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn probe_identifies_ebml_and_ftyp() {
        assert_eq!(
            probe_container(&[0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0]).unwrap(),
            ContainerKind::Webm
        );
        assert_eq!(
            probe_container(&[0, 0, 0, 0, b'f', b't', b'y', b'p']).unwrap(),
            ContainerKind::Mp4
        );
    }

    #[test]
    fn probe_short_input_returns_truncated() {
        let err = probe_container(&[0x1A, 0x45]).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn probe_unknown_magic_returns_unsupported() {
        let err = probe_container(b"RIFFxxxx").unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn demuxer_rejects_unknown_container() {
        let err = Demuxer::new(Cursor::new(b"RIFFxxxx".to_vec())).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }
}
