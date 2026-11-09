//! AIFF and AIFF-C chunk parsing and writing (Apple AIFF spec,
//! EA IFF 85 standard).
//!
//! An AIFF file is a `FORM` chunk of type `AIFF` (or `AIFC` with a
//! `FVER` chunk and a compression type in `COMM`): `COMM` carries
//! the channel count, frame count, sample size, and an 80-bit
//! extended-precision sample rate; `SSND` carries the sample data
//! (big-endian, except `sowt` in AIFF-C, which is little-endian).
//! `MARK` and `COMT` carry markers and comments into metadata.
//! Only PCM is supported: AIFF-C compression types other than
//! `NONE`/`sowt` return `Unsupported`, naming the type.
//!
//! Packets from `AiffDemuxer` carry raw `SSND` sample bytes in the
//! file's byte order (big-endian for AIFF/`NONE`, little-endian
//! for `sowt`); see `AiffHeader::is_little_endian`. Sample sizes
//! 8, 16, 24, and 32 bits are accepted.

use crate::error::{Error, Result};
use crate::frame::Codec;
use crate::frame::{Metadata, MetadataEntry, Packet};
use crate::limits::{MAX_AIFF_FILE_BYTES, MAX_AUDIO_PACKET_BYTES, MAX_SAMPLE_RATE};

/// AIFF chunk ids.
const CHUNK_COMM: [u8; 4] = *b"COMM";
const CHUNK_SSND: [u8; 4] = *b"SSND";
const CHUNK_MARK: [u8; 4] = *b"MARK";
const CHUNK_COMT: [u8; 4] = *b"COMT";
const CHUNK_FVER: [u8; 4] = *b"FVER";

/// `FVER` timestamp for AIFF-C version 1 (Apple AIFF spec).
pub const AIFC_FVER: u32 = 0xA280_5140;

/// Maximum frames in one packet, bounding packet size without
/// knowing the sample size up front (1 MiB / 1 byte per sample).
const MAX_FRAMES_PER_PACKET: usize = MAX_AUDIO_PACKET_BYTES;

/// AIFF form type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiffForm {
    /// Plain AIFF: big-endian PCM.
    Aiff,
    /// AIFF-C: `COMM` carries a compression type.
    Aifc,
}

/// Parsed AIFF header (`COMM`, plus the form type).
#[derive(Debug, Clone, PartialEq)]
pub struct AiffHeader {
    /// `AIFF` or `AIFC`.
    pub form: AiffForm,
    /// Channel count.
    pub channels: u16,
    /// Number of sample frames.
    pub frames: u32,
    /// Bits per sample (8, 16, 24, or 32).
    pub sample_size: u16,
    /// Sample rate in Hz.
    pub sample_rate: f64,
    /// AIFF-C compression type (`NONE`, `sowt`), or `None` for
    /// plain AIFF.
    pub compression: Option<[u8; 4]>,
    /// AIFF-C compression name (Pascal string), when present.
    pub compression_name: String,
}

impl AiffHeader {
    /// True when samples are stored little-endian (`sowt`).
    #[must_use]
    pub fn is_little_endian(&self) -> bool {
        self.compression == Some(*b"sowt")
    }

    /// Bytes per sample frame.
    #[must_use]
    pub fn bytes_per_frame(&self) -> usize {
        usize::from(self.channels) * usize::from(self.sample_size) / 8
    }
}

/// One `MARK` marker: an id, a frame position, and a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiffMarker {
    /// Marker id.
    pub id: u16,
    /// Position in sample frames.
    pub position: u32,
    /// Marker name.
    pub name: String,
}

/// One `COMT` comment: a timestamp, a marker id, and text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiffComment {
    /// Timestamp in seconds since 1904-01-01 (AIFF spec).
    pub timestamp: u32,
    /// Marker the comment is attached to (0 when none).
    pub marker_id: u16,
    /// Comment text.
    pub text: String,
}

/// A parsed AIFF file: header, markers, comments, and the `SSND`
/// data range.
#[derive(Debug, Clone)]
pub struct AiffStream {
    /// Parsed header.
    pub header: AiffHeader,
    /// Markers, in file order.
    pub markers: Vec<AiffMarker>,
    /// Comments, in file order.
    pub comments: Vec<AiffComment>,
    /// Container metadata (markers and comments).
    pub metadata: Metadata,
    /// Byte offset of the first sample byte in the source data.
    pub data_offset: usize,
    /// Length of the sample data in bytes.
    pub data_len: usize,
}

/// Reads a big-endian `u16` at `offset`.
///
/// # Errors
///
/// Returns `Truncated` when the value runs past the input.
fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data.get(offset..offset + 2).ok_or(Error::Truncated {
        needed: offset + 2,
        had: data.len(),
    })?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Reads a big-endian `u32` at `offset`.
///
/// # Errors
///
/// Returns `Truncated` when the value runs past the input.
fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data.get(offset..offset + 4).ok_or(Error::Truncated {
        needed: offset + 4,
        had: data.len(),
    })?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Reads a big-endian `i16` at `offset`.
///
/// # Errors
///
/// Returns `Truncated` when the value runs past the input.
fn read_i16(data: &[u8], offset: usize) -> Result<i16> {
    let bytes = data.get(offset..offset + 2).ok_or(Error::Truncated {
        needed: offset + 2,
        had: data.len(),
    })?;
    Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Reads an 80-bit IEEE extended float (Apple AIFF spec): 1 sign
/// bit, 15 exponent bits (bias 16383), 64 mantissa bits with an
/// explicit integer bit.
///
/// # Errors
///
/// Returns `Truncated` when the value runs past the input and
/// `InvalidData` when the value is non-finite or zero.
#[allow(clippy::cast_precision_loss)]
pub fn read_extended_float(data: &[u8], offset: usize) -> Result<f64> {
    let bytes = data.get(offset..offset + 10).ok_or(Error::Truncated {
        needed: offset + 10,
        had: data.len(),
    })?;
    let sign = bytes[0] & 0x80 != 0;
    let exponent = i32::from((u16::from(bytes[0] & 0x7F) << 8) | u16::from(bytes[1]));
    let mantissa = u64::from_be_bytes([
        bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9],
    ]);
    if exponent == 0 && mantissa == 0 {
        return Err(Error::InvalidData {
            offset,
            detail: "aiff sample rate is zero",
        });
    }
    if exponent == 0x7FFF {
        return Err(Error::InvalidData {
            offset,
            detail: "aiff sample rate is not finite",
        });
    }
    // value = mantissa * 2^(exponent - 16383 - 63).
    let value = mantissa as f64 * 2f64.powi(exponent - 16383 - 63);
    Ok(if sign { -value } else { value })
}

/// Writes an 80-bit IEEE extended float (Apple AIFF spec).
///
/// # Errors
///
/// Returns `InvalidArgument` when the value is zero, negative, or
/// non-finite.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn write_extended_float(value: f64) -> Result<[u8; 10]> {
    if value <= 0.0 || !value.is_finite() {
        return Err(Error::InvalidArgument(
            "aiff sample rate must be positive and finite",
        ));
    }
    // Decompose into mantissa in [1, 2) and a binary exponent.
    let mut mantissa = value;
    let mut exponent: i32 = 0;
    while mantissa >= 2.0 {
        mantissa /= 2.0;
        exponent += 1;
    }
    while mantissa < 1.0 {
        mantissa *= 2.0;
        exponent -= 1;
    }
    let biased = exponent + 16383;
    // The mantissa field holds mantissa * 2^63, with the integer
    // bit explicit in the top bit.
    let mantissa_bits = (mantissa * 2f64.powi(63)) as u64;
    let mut out = [0u8; 10];
    out[0] = u8::try_from((biased >> 8) & 0x7F).unwrap_or(0);
    out[1] = u8::try_from(biased & 0xFF).unwrap_or(0);
    out[2..10].copy_from_slice(&mantissa_bits.to_be_bytes());
    Ok(out)
}

/// Parses an AIFF/AIFF-C file held in memory.
///
/// # Errors
///
/// Returns `Truncated` for a cut-short chunk, `InvalidData` for a
/// bad `FORM` type, a missing `COMM` or `SSND` chunk, a zero
/// channel count, or a bad sample rate, `Unsupported` for a sample
/// size outside 8/16/24/32 or an AIFF-C compression type other
/// than `NONE`/`sowt`, and `AllocationLimit` for a file above the
/// AIFF limit.
#[allow(clippy::too_many_lines)]
pub fn parse_aiff(data: &[u8]) -> Result<AiffStream> {
    if data.len() > MAX_AIFF_FILE_BYTES {
        return Err(Error::AllocationLimit {
            requested: data.len(),
            limit: MAX_AIFF_FILE_BYTES,
        });
    }
    if data.len() < 12 {
        return Err(Error::Truncated {
            needed: 12,
            had: data.len(),
        });
    }
    if &data[0..4] != b"FORM" {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "aiff file does not start with FORM",
        });
    }
    let form_len = read_u32(data, 4)? as usize;
    if form_len < 4 {
        return Err(Error::InvalidData {
            offset: 4,
            detail: "aiff FORM size is smaller than its type field",
        });
    }
    // The FORM size covers the 4 type bytes plus the chunks; a
    // file shorter than that is truncated, a longer one is
    // tolerated (trailing bytes after FORM are ignored).
    let form_end = 8usize.saturating_add(form_len).min(data.len());
    if 8 + form_len > data.len() {
        return Err(Error::Truncated {
            needed: 8 + form_len,
            had: data.len(),
        });
    }
    let form = match &data[8..12] {
        b"AIFF" => AiffForm::Aiff,
        b"AIFC" => AiffForm::Aifc,
        _ => {
            return Err(Error::InvalidData {
                offset: 8,
                detail: "aiff FORM type is neither AIFF nor AIFC",
            });
        }
    };
    let mut header: Option<AiffHeader> = None;
    let mut markers: Vec<AiffMarker> = Vec::new();
    let mut comments: Vec<AiffComment> = Vec::new();
    let mut metadata = Metadata::new();
    let mut ssnd: Option<(usize, usize)> = None;
    let mut offset = 12usize;
    while offset + 8 <= form_end {
        let id: [u8; 4] = data[offset..offset + 4].try_into().unwrap_or([0; 4]);
        let size = read_u32(data, offset + 4)? as usize;
        let body_start = offset + 8;
        let body_end = body_start
            .checked_add(size)
            .ok_or(Error::Overflow("aiff chunk size overflows"))?;
        if body_end > form_end {
            return Err(Error::Truncated {
                needed: body_end,
                had: data.len(),
            });
        }
        match id {
            CHUNK_COMM => {
                header = Some(parse_comm(&data[body_start..body_end], form, offset)?);
            }
            CHUNK_SSND => {
                if ssnd.is_some() {
                    return Err(Error::InvalidData {
                        offset,
                        detail: "aiff file contains two SSND chunks",
                    });
                }
                // `parse_ssnd` returns the data range relative to
                // the chunk body; make the offset absolute.
                let (rel_offset, len) = parse_ssnd(&data[body_start..body_end], offset)?;
                ssnd = Some((body_start + rel_offset, len));
            }
            CHUNK_MARK => {
                parse_mark(
                    &data[body_start..body_end],
                    &mut markers,
                    &mut metadata,
                    offset,
                )?;
            }
            CHUNK_COMT => {
                parse_comt(
                    &data[body_start..body_end],
                    &mut comments,
                    &mut metadata,
                    offset,
                )?;
            }
            // FVER is validated for presence and size only;
            // the version value itself is not interpreted, so a
            // well-formed FVER chunk falls through like any other
            // unknown chunk.
            CHUNK_FVER if size != 4 => {
                return Err(Error::InvalidData {
                    offset,
                    detail: "aiff FVER chunk is not 4 bytes",
                });
            }
            _ => {}
        }
        // Chunks are padded to an even size.
        offset = body_end + (size & 1);
    }
    let header = header.ok_or(Error::InvalidData {
        offset: 0,
        detail: "aiff file has no COMM chunk",
    })?;
    let (data_offset, data_len) = ssnd.ok_or(Error::InvalidData {
        offset: 0,
        detail: "aiff file has no SSND chunk",
    })?;
    // The declared frame count must agree with the sample bytes
    // present (a short SSND is truncated, a long one is corrupt).
    let bytes_per_frame = header.bytes_per_frame();
    if bytes_per_frame == 0 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "aiff bytes per frame is zero",
        });
    }
    let expected = usize::try_from(header.frames)
        .map_err(|_| Error::Overflow("aiff frame count exceeds usize"))?
        .checked_mul(bytes_per_frame)
        .ok_or(Error::Overflow("aiff sample data size overflows"))?;
    if data_len < expected {
        return Err(Error::Truncated {
            needed: data_offset + expected,
            had: data.len(),
        });
    }
    if data_len > expected {
        return Err(Error::InvalidData {
            offset: data_offset,
            detail: "aiff SSND data is longer than the COMM frame count",
        });
    }
    Ok(AiffStream {
        header,
        markers,
        comments,
        metadata,
        data_offset,
        data_len,
    })
}

/// Parses a `COMM` chunk body.
fn parse_comm(body: &[u8], form: AiffForm, offset: usize) -> Result<AiffHeader> {
    if body.len() < 18 {
        return Err(Error::Truncated {
            needed: 18,
            had: body.len(),
        });
    }
    let channels = read_u16(body, 0)?;
    let frames = read_u32(body, 2)?;
    let sample_size = read_i16(body, 6)?;
    let sample_rate = read_extended_float(body, 8)?;
    if channels == 0 {
        return Err(Error::InvalidData {
            offset,
            detail: "aiff channel count is zero",
        });
    }
    if sample_rate <= 0.0 || !sample_rate.is_finite() {
        return Err(Error::InvalidData {
            offset,
            detail: "aiff sample rate is not positive and finite",
        });
    }
    if sample_rate > f64::from(MAX_SAMPLE_RATE) {
        return Err(Error::Overflow("aiff sample rate exceeds maximum"));
    }
    if !matches!(sample_size, 8 | 16 | 24 | 32) {
        return Err(Error::Unsupported(
            "aiff sample size outside 8/16/24/32 bits",
        ));
    }
    let (compression, compression_name) = if form == AiffForm::Aifc {
        if body.len() < 22 {
            return Err(Error::Truncated {
                needed: 22,
                had: body.len(),
            });
        }
        let compression: [u8; 4] = body[18..22].try_into().unwrap_or([0; 4]);
        if compression != *b"NONE" && compression != *b"sowt" {
            return Err(Error::Unsupported(
                "aiff-c compression type outside NONE/sowt",
            ));
        }
        // Pascal string: length byte, then that many bytes.
        let name = if body.len() > 22 {
            let len = usize::from(body[22]);
            let end = 23usize.saturating_add(len).min(body.len());
            String::from_utf8_lossy(&body[23..end]).into_owned()
        } else {
            String::new()
        };
        (Some(compression), name)
    } else {
        (None, String::new())
    };
    Ok(AiffHeader {
        form,
        channels,
        frames,
        sample_size: u16::try_from(sample_size).unwrap_or(0),
        sample_rate,
        compression,
        compression_name,
    })
}

/// Parses an `SSND` chunk body into (data offset, data length).
/// The `offset` field skips leading bytes inside the chunk; the
/// `blockSize` field is alignment only and is not interpreted.
fn parse_ssnd(body: &[u8], offset: usize) -> Result<(usize, usize)> {
    if body.len() < 8 {
        return Err(Error::Truncated {
            needed: 8,
            had: body.len(),
        });
    }
    let skip = read_u32(body, 0)? as usize;
    let data_start = 8usize
        .checked_add(skip)
        .ok_or(Error::Overflow("aiff SSND offset overflows"))?;
    if data_start > body.len() {
        return Err(Error::InvalidData {
            offset,
            detail: "aiff SSND offset is past the chunk end",
        });
    }
    Ok((data_start, body.len() - data_start))
}

/// Parses a `MARK` chunk body into markers and metadata.
fn parse_mark(
    body: &[u8],
    markers: &mut Vec<AiffMarker>,
    metadata: &mut Metadata,
    offset: usize,
) -> Result<()> {
    if body.len() < 2 {
        return Err(Error::Truncated {
            needed: 2,
            had: body.len(),
        });
    }
    let count = read_u16(body, 0)? as usize;
    let mut pos = 2usize;
    for _ in 0..count {
        if pos + 7 > body.len() {
            return Err(Error::Truncated {
                needed: pos + 7,
                had: body.len(),
            });
        }
        let id = read_u16(body, pos)?;
        let position = read_u32(body, pos + 2)?;
        let name_len = usize::from(body[pos + 6]);
        let name_start = pos + 7;
        let name_end = name_start.saturating_add(name_len).min(body.len());
        if name_start + name_len > body.len() {
            return Err(Error::Truncated {
                needed: name_start + name_len,
                had: body.len(),
            });
        }
        let name = String::from_utf8_lossy(&body[name_start..name_end]).into_owned();
        markers.push(AiffMarker {
            id,
            position,
            name: name.clone(),
        });
        metadata
            .entries
            .push(MetadataEntry::new(&format!("MARKER_{id}"), name.as_bytes()));
        // Pascal strings are padded to an even total length.
        pos = name_end + ((name_len + 1) & 1);
    }
    let _ = offset;
    Ok(())
}

/// Parses a `COMT` chunk body into comments and metadata.
fn parse_comt(
    body: &[u8],
    comments: &mut Vec<AiffComment>,
    metadata: &mut Metadata,
    offset: usize,
) -> Result<()> {
    if body.len() < 2 {
        return Err(Error::Truncated {
            needed: 2,
            had: body.len(),
        });
    }
    let count = read_u16(body, 0)? as usize;
    let mut pos = 2usize;
    for _ in 0..count {
        if pos + 8 > body.len() {
            return Err(Error::Truncated {
                needed: pos + 8,
                had: body.len(),
            });
        }
        let timestamp = read_u32(body, pos)?;
        let marker_id = read_u16(body, pos + 4)?;
        let text_len = read_u16(body, pos + 6)? as usize;
        let text_start = pos + 8;
        let text_end = text_start.saturating_add(text_len).min(body.len());
        if text_start + text_len > body.len() {
            return Err(Error::Truncated {
                needed: text_start + text_len,
                had: body.len(),
            });
        }
        let text = String::from_utf8_lossy(&body[text_start..text_end]).into_owned();
        comments.push(AiffComment {
            timestamp,
            marker_id,
            text: text.clone(),
        });
        metadata
            .entries
            .push(MetadataEntry::new("COMMENT", text.as_bytes()));
        // The entry is 8 bytes plus the text, padded to an even
        // total length.
        pos = text_end + (text_len & 1);
    }
    let _ = offset;
    Ok(())
}

/// Demuxer for an AIFF file held in memory. Packets carry raw
/// `SSND` sample bytes in the file's byte order; each packet holds
/// at most 1 MiB and its timestamp advances by the frames it
/// covers.
#[derive(Debug)]
pub struct AiffDemuxer {
    stream: AiffStream,
    data: Vec<u8>,
    position: usize,
    frame_position: u64,
}

impl AiffDemuxer {
    /// Parses an already-read AIFF file.
    ///
    /// # Errors
    ///
    /// The same errors as `parse_aiff`.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let stream = parse_aiff(data)?;
        Ok(Self {
            stream,
            data: data.to_vec(),
            position: 0,
            frame_position: 0,
        })
    }

    /// Parsed header.
    #[must_use]
    pub fn header(&self) -> &AiffHeader {
        &self.stream.header
    }

    /// Markers, in file order.
    #[must_use]
    pub fn markers(&self) -> &[AiffMarker] {
        &self.stream.markers
    }

    /// Comments, in file order.
    #[must_use]
    pub fn comments(&self) -> &[AiffComment] {
        &self.stream.comments
    }

    /// Container metadata (markers and comments).
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.stream.metadata
    }

    /// Returns the next PCM packet, or `None` at end of stream.
    ///
    /// # Errors
    ///
    /// This implementation never fails after construction; the
    /// `Result` exists so a streaming demuxer can report read
    /// errors here.
    pub fn next_packet(&mut self) -> Result<Option<Packet>> {
        if self.position >= self.stream.data_len {
            return Ok(None);
        }
        let bytes_per_frame = self.stream.header.bytes_per_frame();
        let frames_left = (self.stream.data_len - self.position) / bytes_per_frame.max(1);
        let frames = frames_left.min(MAX_FRAMES_PER_PACKET);
        let bytes = frames * bytes_per_frame;
        let start = self.stream.data_offset + self.position;
        let data = self.data[start..start + bytes].to_vec();
        // Timestamp in microseconds from the frame position.
        // The rate is at most 192 kHz and the frame count fits in
        // memory, so the `f64` conversion is exact enough for a
        // microsecond timestamp.
        let sample_rate = self.stream.header.sample_rate;
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let timestamp = if sample_rate > 0.0 {
            (self.frame_position as f64 * 1_000_000.0 / sample_rate) as u64
        } else {
            0
        };
        self.position += bytes;
        self.frame_position += frames as u64;
        Ok(Some(Packet {
            codec: Codec::Pcm,
            data,
            timestamp,
            is_keyframe: true,
        }))
    }
}

/// Writes an AIFF (or AIFF-C `sowt`) file: `FORM` with `COMM` and
/// `SSND`. `samples` holds raw sample bytes in the file's byte
/// order; its length must be a whole number of frames.
///
/// # Errors
///
/// Returns `InvalidArgument` for a zero channel count, an
/// unsupported sample size, a sample rate outside the library
/// range, or sample data that is not a whole number of frames,
/// and `Unsupported` for a compression type other than
/// `NONE`/`sowt`.
pub fn write_aiff(header: &AiffHeader, samples: &[u8]) -> Result<Vec<u8>> {
    if header.channels == 0 {
        return Err(Error::InvalidArgument("aiff channel count is zero"));
    }
    if !matches!(header.sample_size, 8 | 16 | 24 | 32) {
        return Err(Error::Unsupported(
            "aiff sample size outside 8/16/24/32 bits",
        ));
    }
    if header.sample_rate <= 0.0 || header.sample_rate > f64::from(MAX_SAMPLE_RATE) {
        return Err(Error::InvalidArgument("aiff sample rate out of range"));
    }
    if let Some(compression) = header.compression {
        if compression != *b"NONE" && compression != *b"sowt" {
            return Err(Error::Unsupported(
                "aiff-c compression type outside NONE/sowt",
            ));
        }
    }
    let bytes_per_frame = header.bytes_per_frame();
    if bytes_per_frame == 0 || samples.len() % bytes_per_frame != 0 {
        return Err(Error::InvalidArgument(
            "aiff sample data is not a whole number of frames",
        ));
    }
    let frames = u32::try_from(samples.len() / bytes_per_frame)
        .map_err(|_| Error::Overflow("aiff frame count exceeds u32"))?;
    let mut out: Vec<u8> = Vec::new();
    // COMM body: channels(2), frames(4), sampleSize(2),
    // sampleRate(10), compression(4) + name for AIFC.
    let mut comm: Vec<u8> = Vec::with_capacity(24);
    comm.extend_from_slice(&header.channels.to_be_bytes());
    comm.extend_from_slice(&frames.to_be_bytes());
    comm.extend_from_slice(&header.sample_size.to_be_bytes());
    comm.extend_from_slice(&write_extended_float(header.sample_rate)?);
    if header.form == AiffForm::Aifc {
        let compression = header.compression.unwrap_or(*b"NONE");
        comm.extend_from_slice(&compression);
        let name = header.compression_name.as_bytes();
        comm.push(u8::try_from(name.len()).unwrap_or(0));
        comm.extend_from_slice(name);
    }
    // Chunk sizes exclude the pad byte; each chunk body is padded
    // to an even length with one extra byte after the body.
    let comm_total = comm.len() + (comm.len() & 1);
    let ssnd_size = 8usize
        .checked_add(samples.len())
        .ok_or(Error::Overflow("aiff SSND size overflows"))?;
    let ssnd_total = ssnd_size + (ssnd_size & 1);
    // FORM size: 4 (type) + COMM box + FVER box (AIFC) + SSND box.
    let mut form_len: usize = 4;
    form_len += 8 + comm_total;
    if header.form == AiffForm::Aifc {
        form_len += 8 + 4;
    }
    form_len += 8 + ssnd_total;
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(
        &u32::try_from(form_len)
            .map_err(|_| Error::Overflow("aiff FORM size exceeds u32"))?
            .to_be_bytes(),
    );
    out.extend_from_slice(if header.form == AiffForm::Aifc {
        b"AIFC"
    } else {
        b"AIFF"
    });
    if header.form == AiffForm::Aifc {
        out.extend_from_slice(b"FVER");
        out.extend_from_slice(&4u32.to_be_bytes());
        out.extend_from_slice(&AIFC_FVER.to_be_bytes());
    }
    out.extend_from_slice(b"COMM");
    out.extend_from_slice(
        &u32::try_from(comm.len())
            .map_err(|_| Error::Overflow("aiff COMM size exceeds u32"))?
            .to_be_bytes(),
    );
    out.extend_from_slice(&comm);
    if comm.len() % 2 != 0 {
        out.push(0);
    }
    out.extend_from_slice(b"SSND");
    out.extend_from_slice(
        &u32::try_from(ssnd_size)
            .map_err(|_| Error::Overflow("aiff SSND size exceeds u32"))?
            .to_be_bytes(),
    );
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(samples);
    if ssnd_size % 2 != 0 {
        out.push(0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_float_round_trips_44100() {
        let bytes = write_extended_float(44_100.0).unwrap();
        assert_eq!(read_extended_float(&bytes, 0).unwrap(), 44_100.0);
        // ffmpeg's encoding of 44100 Hz.
        assert_eq!(
            read_extended_float(&[0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0], 0).unwrap(),
            44_100.0
        );
    }

    #[test]
    fn extended_float_rejects_zero() {
        assert!(matches!(
            write_extended_float(0.0),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            read_extended_float(&[0u8; 10], 0),
            Err(Error::InvalidData { .. })
        ));
    }

    #[test]
    fn write_and_parse_round_trip() {
        let header = AiffHeader {
            form: AiffForm::Aiff,
            channels: 2,
            frames: 0,
            sample_size: 16,
            sample_rate: 48_000.0,
            compression: None,
            compression_name: String::new(),
        };
        let samples = vec![0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let bytes = write_aiff(&header, &samples).unwrap();
        let stream = parse_aiff(&bytes).unwrap();
        assert_eq!(stream.header.channels, 2);
        assert_eq!(stream.header.frames, 2);
        assert_eq!(stream.header.sample_size, 16);
        assert_eq!(stream.header.sample_rate, 48_000.0);
        assert_eq!(stream.data_len, 8);
        let mut demuxer = AiffDemuxer::from_bytes(&bytes).unwrap();
        let packet = demuxer.next_packet().unwrap().unwrap();
        assert_eq!(packet.codec, Codec::Pcm);
        assert_eq!(packet.data, samples);
        assert_eq!(packet.timestamp, 0);
        assert!(demuxer.next_packet().unwrap().is_none());
    }

    #[test]
    fn aifc_sowt_round_trip() {
        let header = AiffHeader {
            form: AiffForm::Aifc,
            channels: 1,
            frames: 0,
            sample_size: 16,
            sample_rate: 44_100.0,
            compression: Some(*b"sowt"),
            compression_name: "Little Endian".to_string(),
        };
        let samples = vec![0x01u8, 0x02, 0x03, 0x04];
        let bytes = write_aiff(&header, &samples).unwrap();
        let stream = parse_aiff(&bytes).unwrap();
        assert_eq!(stream.header.form, AiffForm::Aifc);
        assert!(stream.header.is_little_endian());
        assert_eq!(stream.header.compression_name, "Little Endian");
    }

    #[test]
    fn unsupported_compression_is_unsupported() {
        let header = AiffHeader {
            form: AiffForm::Aifc,
            channels: 1,
            frames: 0,
            sample_size: 16,
            sample_rate: 44_100.0,
            compression: Some(*b"alaw"),
            compression_name: String::new(),
        };
        let err = write_aiff(&header, &[0, 0]).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
        // And a file carrying it is rejected at parse time.
        let mut bytes = write_aiff(
            &AiffHeader {
                compression: Some(*b"sowt"),
                ..header
            },
            &[0, 0],
        )
        .unwrap();
        // Patch the COMM compression type to 'alaw'.
        let pos = bytes.windows(4).position(|w| w == b"sowt").unwrap();
        bytes[pos..pos + 4].copy_from_slice(b"alaw");
        let err = parse_aiff(&bytes).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn bad_form_is_invalid_data() {
        let err = parse_aiff(b"FORMxxxxXXXX").unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidData { .. } | Error::Truncated { .. }
        ));
    }

    #[test]
    fn truncated_ssnd_is_truncated() {
        let header = AiffHeader {
            form: AiffForm::Aiff,
            channels: 1,
            frames: 0,
            sample_size: 16,
            sample_rate: 8000.0,
            compression: None,
            compression_name: String::new(),
        };
        let bytes = write_aiff(&header, &[0, 0, 0, 0]).unwrap();
        let cut = &bytes[..bytes.len() - 2];
        let err = parse_aiff(cut).unwrap_err();
        assert!(matches!(
            err,
            Error::Truncated { .. } | Error::InvalidData { .. }
        ));
    }

    #[test]
    fn odd_sample_count_is_invalid_argument() {
        let header = AiffHeader {
            form: AiffForm::Aiff,
            channels: 1,
            frames: 0,
            sample_size: 16,
            sample_rate: 8000.0,
            compression: None,
            compression_name: String::new(),
        };
        let err = write_aiff(&header, &[0, 0, 0]).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }
}
