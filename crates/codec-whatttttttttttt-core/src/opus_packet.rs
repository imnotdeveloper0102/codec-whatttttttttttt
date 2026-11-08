//! Opus packet framing and Ogg Opus headers (RFC 6716 section 3,
//! RFC 7845 for the Ogg mapping).
//!
//! This module only parses framing: the TOC byte, frame packing
//! codes 0-3, and the `OpusHead`/`OpusTags` headers. It never decodes
//! audio samples; the CELT and SILK paths live in `celt.rs` and the
//! `Decoder` integration.
//!
//! Correction to the original task text: the Ogg Opus mapping is
//! RFC 7845, not RFC 8251 (RFC 8251 is the RTP payload format for
//! Opus).

use crate::error::{Error, Result};
use crate::limits;

/// Maximum Opus frame length in bytes (RFC 6716 section 3.2.1:
/// the two-byte length code tops out at 1275).
pub const MAX_OPUS_FRAME_BYTES: usize = 1275;

/// Maximum Opus packet duration in milliseconds (RFC 6716
/// section 3.4).
pub const MAX_OPUS_PACKET_DURATION_MS: u32 = 120;

/// Opus internal sample rate in Hz. All Opus audio is 48 kHz
/// internally (RFC 6716 section 1); the rate in `OpusHead` is
/// informational only.
pub const OPUS_INTERNAL_SAMPLE_RATE: u32 = 48_000;

/// Coding mode selected by the TOC configuration (RFC 6716 Table 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusMode {
    /// Linear-prediction mode (RFC 6716 section 4.2).
    Silk,
    /// SILK + CELT hybrid (RFC 6716 section 4.3 is CELT; the hybrid
    /// combination is section 4.4 in the reference).
    Hybrid,
    /// Constrained Energy Lapped Transform (RFC 6716 section 4.3).
    Celt,
}

/// Audio bandwidth selected by the TOC configuration (RFC 6716
/// Table 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusBandwidth {
    /// 4 kHz audio bandwidth.
    Narrowband,
    /// 6 kHz audio bandwidth.
    Mediumband,
    /// 8 kHz audio bandwidth.
    Wideband,
    /// 12 kHz audio bandwidth.
    SuperWideband,
    /// 20 kHz audio bandwidth.
    Fullband,
}

/// The parameters a TOC configuration number selects (RFC 6716
/// Table 2): mode, bandwidth, and frame duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusConfig {
    /// Configuration number, 0..=31.
    pub number: u8,
    /// Coding mode.
    pub mode: OpusMode,
    /// Audio bandwidth.
    pub bandwidth: OpusBandwidth,
    /// Frame duration in samples at the 48 kHz internal rate
    /// (2.5 ms = 120, 5 ms = 240, 10 ms = 480, 20 ms = 960,
    /// 40 ms = 1920, 60 ms = 2880).
    pub frame_duration_samples_48k: u32,
}

impl OpusConfig {
    /// Frame duration in milliseconds (2.5, 5, 10, 20, 40 or 60).
    /// The duration is at most 2880 samples (60 ms), so the `f32`
    /// conversion is exact.
    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn frame_duration_ms(&self) -> f32 {
        self.frame_duration_samples_48k as f32 / 48.0
    }
}

/// Looks up a TOC configuration number (RFC 6716 Table 2).
///
/// # Errors
///
/// Returns `InvalidData` when `number` is above 31.
pub fn opus_config(number: u8) -> Result<OpusConfig> {
    // Table 2, in configuration-number order: (mode, bandwidth,
    // frame duration in 48 kHz samples).
    const TABLE: [(OpusMode, OpusBandwidth, u32); 32] = [
        (OpusMode::Silk, OpusBandwidth::Narrowband, 480),
        (OpusMode::Silk, OpusBandwidth::Narrowband, 960),
        (OpusMode::Silk, OpusBandwidth::Narrowband, 1920),
        (OpusMode::Silk, OpusBandwidth::Narrowband, 2880),
        (OpusMode::Silk, OpusBandwidth::Mediumband, 480),
        (OpusMode::Silk, OpusBandwidth::Mediumband, 960),
        (OpusMode::Silk, OpusBandwidth::Mediumband, 1920),
        (OpusMode::Silk, OpusBandwidth::Mediumband, 2880),
        (OpusMode::Silk, OpusBandwidth::Wideband, 480),
        (OpusMode::Silk, OpusBandwidth::Wideband, 960),
        (OpusMode::Silk, OpusBandwidth::Wideband, 1920),
        (OpusMode::Silk, OpusBandwidth::Wideband, 2880),
        (OpusMode::Hybrid, OpusBandwidth::SuperWideband, 480),
        (OpusMode::Hybrid, OpusBandwidth::SuperWideband, 960),
        (OpusMode::Hybrid, OpusBandwidth::Fullband, 480),
        (OpusMode::Hybrid, OpusBandwidth::Fullband, 960),
        (OpusMode::Celt, OpusBandwidth::Narrowband, 120),
        (OpusMode::Celt, OpusBandwidth::Narrowband, 240),
        (OpusMode::Celt, OpusBandwidth::Narrowband, 480),
        (OpusMode::Celt, OpusBandwidth::Narrowband, 960),
        (OpusMode::Celt, OpusBandwidth::Wideband, 120),
        (OpusMode::Celt, OpusBandwidth::Wideband, 240),
        (OpusMode::Celt, OpusBandwidth::Wideband, 480),
        (OpusMode::Celt, OpusBandwidth::Wideband, 960),
        (OpusMode::Celt, OpusBandwidth::SuperWideband, 120),
        (OpusMode::Celt, OpusBandwidth::SuperWideband, 240),
        (OpusMode::Celt, OpusBandwidth::SuperWideband, 480),
        (OpusMode::Celt, OpusBandwidth::SuperWideband, 960),
        (OpusMode::Celt, OpusBandwidth::Fullband, 120),
        (OpusMode::Celt, OpusBandwidth::Fullband, 240),
        (OpusMode::Celt, OpusBandwidth::Fullband, 480),
        (OpusMode::Celt, OpusBandwidth::Fullband, 960),
    ];
    let (mode, bandwidth, duration) =
        *TABLE.get(usize::from(number)).ok_or(Error::InvalidData {
            offset: 0,
            detail: "Opus TOC configuration number exceeds 31",
        })?;
    Ok(OpusConfig {
        number,
        mode,
        bandwidth,
        frame_duration_samples_48k: duration,
    })
}

/// A parsed Opus packet (RFC 6716 section 3.2). `frames` borrows the
/// packet payload; a zero-length frame is a lost frame (DTX), not an
/// error — RFC 7845 section 3 explicitly allows zero-byte frames in
/// any code, and the decoder treats them as lost (PLC).
#[derive(Debug, Clone)]
pub struct OpusPacket<'a> {
    /// TOC configuration.
    pub config: OpusConfig,
    /// Channel count: 1 (mono) or 2 (stereo).
    pub channels: u8,
    /// The frames in the packet, in order, without padding.
    pub frames: Vec<&'a [u8]>,
    /// Padding bytes at the end of the packet (code 3 only).
    pub padding_bytes: usize,
}

impl OpusPacket<'_> {
    /// Number of frames in the packet.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Total duration of the packet in samples at the 48 kHz
    /// internal rate.
    #[must_use]
    pub fn total_duration_samples_48k(&self) -> u32 {
        self.config.frame_duration_samples_48k * u32::try_from(self.frames.len()).unwrap_or(0)
    }
}

/// Decodes one length-coded frame length (RFC 6716 section 3.2.1):
/// 0 = no frame, 1..=251 = length, 252..=255 = two-byte length
/// (`second_byte * 4 + first_byte`), maximum 1275 bytes. Returns the
/// length and the number of length bytes consumed.
///
/// # Errors
///
/// Returns `Truncated` when the length bytes run out.
fn read_frame_length(data: &[u8], offset: usize) -> Result<(usize, usize)> {
    let first = *data.get(offset).ok_or(Error::Truncated {
        needed: offset + 1,
        had: data.len(),
    })?;
    if first < 252 {
        return Ok((usize::from(first), 1));
    }
    let second = *data.get(offset + 1).ok_or(Error::Truncated {
        needed: offset + 2,
        had: data.len(),
    })?;
    Ok((usize::from(second) * 4 + usize::from(first), 2))
}

/// Parses the code 3 frame packing (RFC 6716 section 3.2.5): an
/// arbitrary number of frames, with an optional padding length and
/// VBR or CBR frame lengths. Returns the frames and the padding
/// byte count. `data` is the whole packet (for the N-2 bound) and
/// `payload` is the packet after the TOC byte.
fn parse_code3_frames<'a>(data: &[u8], payload: &'a [u8]) -> Result<(Vec<&'a [u8]>, usize)> {
    // The byte after the TOC holds M (frame count, bits 2-7 in
    // MSB-first numbering, i.e. the low six bits), p (padding flag,
    // bit 1 = 0x40) and v (VBR flag, bit 0 = 0x80).
    let count_byte = *payload.first().ok_or(Error::Truncated {
        needed: 2,
        had: data.len(),
    })?;
    let frame_count = usize::from(count_byte & 0x3F);
    let vbr = count_byte & 0x80 != 0;
    let has_padding = count_byte & 0x40 != 0;
    if frame_count == 0 {
        return Err(Error::InvalidData {
            offset: 1,
            detail: "Opus code 3 packet declares zero frames",
        });
    }
    let mut offset = 1usize; // into payload, past the count byte
                             // Padding length, if any, comes first: each byte 0..=254
                             // contributes its value; a byte 255 contributes 254 and
                             // another byte follows (there MUST be at least one more
                             // byte in that case [R6,R7]).
    let mut pad_bytes = 0usize;
    if has_padding {
        loop {
            let byte = *payload.get(offset).ok_or(Error::Truncated {
                needed: offset + 1,
                had: data.len(),
            })?;
            offset += 1;
            if byte == 255 {
                pad_bytes += 254;
            } else {
                pad_bytes += usize::from(byte);
                break;
            }
        }
    }
    // P is the padding-length indicator bytes plus the
    // padding bytes themselves; P MUST be no more than N-2
    // [R6,R7], where N is the total packet size.
    let pad_indicator_bytes = offset - 1;
    let total_padding = pad_indicator_bytes + pad_bytes;
    if total_padding > data.len().saturating_sub(2) {
        return Err(Error::InvalidData {
            offset: 1,
            detail: "Opus code 3 padding exceeds packet size",
        });
    }
    let mut lengths: Vec<usize> = Vec::with_capacity(frame_count);
    if vbr {
        // VBR: the padding length is followed by M-1
        // length-coded frame lengths; the last frame takes
        // the remaining bytes before the padding [R7].
        let mut used = 0usize;
        for _ in 0..frame_count - 1 {
            let (len, consumed) = read_frame_length(payload, offset)?;
            offset += consumed;
            used += len;
            lengths.push(len);
        }
        let remaining = payload.len().saturating_sub(offset + pad_bytes);
        if used > remaining {
            return Err(Error::InvalidData {
                offset: 1,
                detail: "Opus code 3 VBR lengths exceed remaining packet",
            });
        }
        lengths.push(remaining - used);
    } else {
        // CBR: no frame length is coded. R = N-2-P is the
        // number of bytes remaining after the padding, and
        // every frame is R/M bytes; R MUST be a non-negative
        // integer multiple of M [R6].
        let r_total = data.len() - 2 - total_padding;
        if r_total % frame_count != 0 {
            return Err(Error::InvalidData {
                offset: 1,
                detail: "Opus code 3 CBR length is not a multiple of frame count",
            });
        }
        let frame_len = r_total / frame_count;
        for _ in 0..frame_count {
            lengths.push(frame_len);
        }
    }
    for &len in &lengths {
        if len > MAX_OPUS_FRAME_BYTES {
            return Err(Error::InvalidData {
                offset: 1,
                detail: "Opus code 3 frame exceeds 1275 bytes",
            });
        }
    }
    let mut frames: Vec<&[u8]> = Vec::with_capacity(frame_count);
    let mut frame_start = offset;
    for &len in &lengths {
        let end = frame_start + len;
        frames.push(payload.get(frame_start..end).ok_or(Error::InvalidData {
            offset: 1,
            detail: "Opus code 3 frame extends past packet end",
        })?);
        frame_start = end;
    }
    Ok((frames, pad_bytes))
}

/// Parses an Opus packet (RFC 6716 sections 3.1-3.2) and enforces the
/// malformed-packet rules of section 3.4 (R1-R7).
///
/// # Errors
///
/// Returns `Truncated` for an empty packet or a packet cut short
/// while reading a length, and `InvalidData` for a packet that
/// violates a section 3.4 rule (implicit frame over 1275 bytes,
/// code 1 with an odd payload, code 2 with the first frame longer
/// than the remainder, code 3 with M == 0, a packet over 120 ms,
/// or inconsistent CBR/VBR lengths).
pub fn parse_opus_packet(data: &[u8]) -> Result<OpusPacket<'_>> {
    if data.is_empty() {
        return Err(Error::Truncated { needed: 1, had: 0 });
    }
    let toc = data[0];
    let config = opus_config(toc >> 3)?;
    let channels = if (toc >> 2) & 1 == 1 { 2 } else { 1 };
    let code = toc & 0b11;
    let payload = &data[1..];
    let mut frames: Vec<&[u8]> = Vec::new();
    let mut padding_bytes = 0usize;

    match code {
        0 => {
            // One frame: the whole payload (section 3.2.2). A
            // zero-length frame is a lost frame, not an error.
            if payload.len() > MAX_OPUS_FRAME_BYTES {
                return Err(Error::InvalidData {
                    offset: 1,
                    detail: "Opus code 0 frame exceeds 1275 bytes",
                });
            }
            frames.push(payload);
        }
        1 => {
            // Two frames of equal size (section 3.2.3): each is
            // (N-1)/2 bytes where N is the packet size, so the
            // payload length N-1 MUST be even [R3].
            if payload.len() % 2 != 0 {
                return Err(Error::InvalidData {
                    offset: 1,
                    detail: "Opus code 1 packet payload length is odd",
                });
            }
            let half = payload.len() / 2;
            if half > MAX_OPUS_FRAME_BYTES {
                return Err(Error::InvalidData {
                    offset: 1,
                    detail: "Opus code 1 frame exceeds 1275 bytes",
                });
            }
            frames.push(&payload[..half]);
            frames.push(&payload[half..]);
        }
        2 => {
            // Two frames of different sizes (section 3.2.4): the
            // first length is length-coded, the second takes the
            // rest.
            let (first_len, consumed) = read_frame_length(payload, 0)?;
            let rest = &payload[consumed..];
            if first_len > rest.len() {
                return Err(Error::InvalidData {
                    offset: 1,
                    detail: "Opus code 2 first frame longer than remaining packet",
                });
            }
            if first_len > MAX_OPUS_FRAME_BYTES || rest.len() - first_len > MAX_OPUS_FRAME_BYTES {
                return Err(Error::InvalidData {
                    offset: 1,
                    detail: "Opus code 2 frame exceeds 1275 bytes",
                });
            }
            frames.push(&rest[..first_len]);
            frames.push(&rest[first_len..]);
        }
        3 => {
            let (code3_frames, pad) = parse_code3_frames(data, payload)?;
            frames = code3_frames;
            padding_bytes = pad;
        }
        _ => unreachable!("TOC code is two bits"),
    }

    // Section 3.4: a packet MUST NOT contain more than 120 ms of
    // audio. Compared in 48 kHz samples (120 ms = 5760 samples) so
    // no float conversion is needed.
    let total_samples =
        config.frame_duration_samples_48k * u32::try_from(frames.len()).unwrap_or(0);
    if total_samples > MAX_OPUS_PACKET_DURATION_MS * OPUS_INTERNAL_SAMPLE_RATE / 1000 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "Opus packet duration exceeds 120 ms",
        });
    }

    Ok(OpusPacket {
        config,
        channels,
        frames,
        padding_bytes,
    })
}

/// The `OpusHead` identification header (RFC 7845 section 5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpusHead {
    /// Header version. MUST be 1.
    pub version: u8,
    /// Channel count. MUST NOT be 0.
    pub channels: u8,
    /// Pre-skip in 48 kHz samples: samples to discard from the
    /// start of the decoded stream.
    pub pre_skip: u16,
    /// Input sample rate in Hz. Informational; Opus always decodes
    /// at 48 kHz internally.
    pub input_sample_rate: u32,
    /// Output gain in Q7.8 dB (signed).
    pub output_gain: i16,
    /// Channel mapping family: 0 (mono/stereo), 1 (multichannel
    /// with mapping table), 255 (no defined channel meaning).
    /// Families 2..=254 are reserved and rejected.
    pub channel_mapping_family: u8,
    /// Stream count, for family 1 only.
    pub stream_count: u8,
    /// Coupled stream count, for family 1 only.
    pub coupled_count: u8,
    /// Channel mapping table, for family 1 only: `channels` entries,
    /// each below `stream_count + coupled_count`, or 255 for a
    /// silent channel (RFC 7845 section 5.1.1).
    pub channel_mapping: Vec<u8>,
}

impl OpusHead {
    /// Output gain in dB.
    #[must_use]
    pub fn output_gain_db(&self) -> f32 {
        f32::from(self.output_gain) / 256.0
    }
}

/// Parses an `OpusHead` header packet (RFC 7845 section 5.1).
///
/// # Errors
///
/// Returns `Truncated` when the packet is shorter than the header,
/// and `InvalidData` when the magic, version, channel count,
/// mapping family, or mapping table is invalid.
pub fn parse_opus_head(data: &[u8]) -> Result<OpusHead> {
    if data.len() < 19 {
        return Err(Error::Truncated {
            needed: 19,
            had: data.len(),
        });
    }
    if &data[..8] != b"OpusHead" {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "OpusHead magic mismatch",
        });
    }
    let version = data[8];
    if version != 1 {
        return Err(Error::InvalidData {
            offset: 8,
            detail: "OpusHead version must be 1",
        });
    }
    let channels = data[9];
    if channels == 0 {
        return Err(Error::InvalidData {
            offset: 9,
            detail: "OpusHead channel count must not be zero",
        });
    }
    let pre_skip = u16::from_le_bytes([data[10], data[11]]);
    let input_sample_rate = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    let output_gain = i16::from_le_bytes([data[16], data[17]]);
    let family = data[18];
    let (stream_count, coupled_count, mapping) = match family {
        0 => {
            if channels > 2 {
                return Err(Error::InvalidData {
                    offset: 18,
                    detail: "OpusHead family 0 allows at most 2 channels",
                });
            }
            (1, u8::from(channels == 2), Vec::new())
        }
        1 => {
            if data.len() < 21 + usize::from(channels) {
                return Err(Error::Truncated {
                    needed: 21 + usize::from(channels),
                    had: data.len(),
                });
            }
            let stream_count = data[19];
            let coupled_count = data[20];
            if stream_count == 0 || coupled_count > stream_count {
                return Err(Error::InvalidData {
                    offset: 19,
                    detail: "OpusHead family 1 stream counts are inconsistent",
                });
            }
            // The sum is widened to u16: both counts are u8, so the
            // sum can reach 510 and overflowed u8 (found by
            // fuzzing). Per RFC 7845 section 5.1.1 the sum is the
            // decoded channel count (M + N) and MUST be no larger
            // than 255. The output channel count C is NOT
            // constrained to match it: one decoded channel may feed
            // several outputs, and some may feed none.
            let decoded_channels = u16::from(stream_count) + u16::from(coupled_count);
            if decoded_channels > 255 {
                return Err(Error::InvalidData {
                    offset: 19,
                    detail: "OpusHead family 1 decoded channel count exceeds 255",
                });
            }
            let mapping: Vec<u8> = data[21..21 + usize::from(channels)].to_vec();
            for &entry in &mapping {
                // 255 means the channel is silent (RFC 7845 section
                // 5.1.1); any other entry names a decoded channel
                // below M + N.
                if entry != 255 && u16::from(entry) >= decoded_channels {
                    return Err(Error::InvalidData {
                        offset: 21,
                        detail: "OpusHead family 1 mapping entry out of range",
                    });
                }
            }
            (stream_count, coupled_count, mapping)
        }
        255 => (1, 0, Vec::new()),
        _ => {
            return Err(Error::InvalidData {
                offset: 18,
                detail: "OpusHead channel mapping family is reserved",
            });
        }
    };
    Ok(OpusHead {
        version,
        channels,
        pre_skip,
        input_sample_rate,
        output_gain,
        channel_mapping_family: family,
        stream_count,
        coupled_count,
        channel_mapping: mapping,
    })
}

/// The `OpusTags` comment header (RFC 7845 section 5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpusTags {
    /// Vendor string, as raw bytes (usually UTF-8).
    pub vendor: Vec<u8>,
    /// User comments, each `KEY=value` as raw bytes.
    pub comments: Vec<Vec<u8>>,
}

/// Parses an `OpusTags` header packet (RFC 7845 section 5.2).
///
/// # Errors
///
/// Returns `Truncated` when the packet ends mid-field, and
/// `InvalidData` when the magic is wrong.
pub fn parse_opus_tags(data: &[u8]) -> Result<OpusTags> {
    if data.len() < 8 {
        return Err(Error::Truncated {
            needed: 8,
            had: data.len(),
        });
    }
    if &data[..8] != b"OpusTags" {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "OpusTags magic mismatch",
        });
    }
    let mut offset = 8usize;
    let read_u32 = |data: &[u8], offset: &mut usize| -> Result<u32> {
        let bytes = data.get(*offset..*offset + 4).ok_or(Error::Truncated {
            needed: *offset + 4,
            had: data.len(),
        })?;
        *offset += 4;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    };
    let vendor_len =
        usize::try_from(read_u32(data, &mut offset)?).map_err(|_| Error::AllocationLimit {
            requested: usize::MAX,
            limit: limits::MAX_METADATA_BYTES,
        })?;
    if vendor_len > limits::MAX_METADATA_BYTES {
        return Err(Error::AllocationLimit {
            requested: vendor_len,
            limit: limits::MAX_METADATA_BYTES,
        });
    }
    let vendor = data
        .get(offset..offset + vendor_len)
        .ok_or(Error::Truncated {
            needed: offset + vendor_len,
            had: data.len(),
        })?
        .to_vec();
    offset += vendor_len;
    let comment_count = read_u32(data, &mut offset)?;
    if comment_count as usize > limits::MAX_METADATA_BYTES / 8 {
        return Err(Error::AllocationLimit {
            requested: comment_count as usize,
            limit: limits::MAX_METADATA_BYTES / 8,
        });
    }
    let mut comments = Vec::with_capacity(comment_count as usize);
    for _ in 0..comment_count {
        let len =
            usize::try_from(read_u32(data, &mut offset)?).map_err(|_| Error::AllocationLimit {
                requested: usize::MAX,
                limit: limits::MAX_METADATA_BYTES,
            })?;
        if len > limits::MAX_METADATA_BYTES {
            return Err(Error::AllocationLimit {
                requested: len,
                limit: limits::MAX_METADATA_BYTES,
            });
        }
        let comment = data.get(offset..offset + len).ok_or(Error::Truncated {
            needed: offset + len,
            had: data.len(),
        })?;
        comments.push(comment.to_vec());
        offset += len;
    }
    Ok(OpusTags { vendor, comments })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toc(config: u8, stereo: bool, code: u8) -> u8 {
        (config << 3) | (u8::from(stereo) << 2) | code
    }

    #[test]
    fn config_table_matches_rfc_6716_table_2() {
        let silk_nb_20 = opus_config(1).unwrap();
        assert_eq!(silk_nb_20.mode, OpusMode::Silk);
        assert_eq!(silk_nb_20.bandwidth, OpusBandwidth::Narrowband);
        assert_eq!(silk_nb_20.frame_duration_samples_48k, 960);
        let hybrid_fb_10 = opus_config(14).unwrap();
        assert_eq!(hybrid_fb_10.mode, OpusMode::Hybrid);
        assert_eq!(hybrid_fb_10.bandwidth, OpusBandwidth::Fullband);
        assert_eq!(hybrid_fb_10.frame_duration_samples_48k, 480);
        let celt_fb_20 = opus_config(31).unwrap();
        assert_eq!(celt_fb_20.mode, OpusMode::Celt);
        assert_eq!(celt_fb_20.bandwidth, OpusBandwidth::Fullband);
        assert_eq!(celt_fb_20.frame_duration_samples_48k, 960);
        assert_eq!(celt_fb_20.frame_duration_ms(), 20.0);
        let celt_nb_2_5 = opus_config(16).unwrap();
        assert_eq!(celt_nb_2_5.frame_duration_samples_48k, 120);
        assert_eq!(celt_nb_2_5.frame_duration_ms(), 2.5);
    }

    #[test]
    fn code_0_single_frame() {
        let packet = [toc(31, true, 0), 0xAA, 0xBB, 0xCC];
        let parsed = parse_opus_packet(&packet).unwrap();
        assert_eq!(parsed.frame_count(), 1);
        assert_eq!(parsed.channels, 2);
        assert_eq!(parsed.frames[0], &[0xAA, 0xBB, 0xCC]);
        assert_eq!(parsed.total_duration_samples_48k(), 960);
    }

    #[test]
    fn code_0_empty_frame_is_a_lost_frame() {
        // A packet that is only a TOC byte: DTX, one zero-length
        // frame (RFC 7845 section 3 allows zero-byte frames).
        let packet = [toc(31, false, 0)];
        let parsed = parse_opus_packet(&packet).unwrap();
        assert_eq!(parsed.frame_count(), 1);
        assert_eq!(parsed.frames[0].len(), 0);
    }

    #[test]
    fn code_1_two_equal_frames() {
        // N = 7 bytes total, payload 6, each frame 3 bytes.
        let packet = [toc(19, false, 1), 1, 2, 3, 4, 5, 6];
        let parsed = parse_opus_packet(&packet).unwrap();
        assert_eq!(parsed.frame_count(), 2);
        assert_eq!(parsed.frames[0], &[1, 2, 3]);
        assert_eq!(parsed.frames[1], &[4, 5, 6]);
    }

    #[test]
    fn code_1_odd_payload_is_invalid() {
        // N = 6 bytes total, payload 5 (odd) violates [R3].
        let packet = [toc(19, false, 1), 1, 2, 3, 4, 5];
        let err = parse_opus_packet(&packet).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn code_2_two_different_frames() {
        // First frame length 2 (one length byte), then 2 + 3 bytes.
        let packet = [toc(19, false, 2), 2, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        let parsed = parse_opus_packet(&packet).unwrap();
        assert_eq!(parsed.frame_count(), 2);
        assert_eq!(parsed.frames[0], &[0xAA, 0xBB]);
        assert_eq!(parsed.frames[1], &[0xCC, 0xDD, 0xEE]);
    }

    #[test]
    fn code_2_first_frame_longer_than_rest_is_invalid() {
        let packet = [toc(19, false, 2), 10, 0xAA, 0xBB];
        let err = parse_opus_packet(&packet).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn code_2_two_byte_length() {
        // Length code 252 + second byte 3 => 3*4 + 252 = 264 bytes.
        let mut packet = vec![toc(19, false, 2), 252, 3];
        packet.extend(std::iter::repeat_n(0x55u8, 264));
        packet.extend(std::iter::repeat_n(0x66u8, 10));
        let parsed = parse_opus_packet(&packet).unwrap();
        assert_eq!(parsed.frames[0].len(), 264);
        assert_eq!(parsed.frames[1].len(), 10);
    }

    #[test]
    fn code_3_vbr_three_frames() {
        // Count byte: M=3, VBR set. Lengths 2 and 3 coded, last frame
        // takes the rest (4 bytes).
        let packet = [toc(19, false, 3), 0x83, 2, 3, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let parsed = parse_opus_packet(&packet).unwrap();
        assert_eq!(parsed.frame_count(), 3);
        assert_eq!(parsed.frames[0], &[1, 2]);
        assert_eq!(parsed.frames[1], &[3, 4, 5]);
        assert_eq!(parsed.frames[2], &[6, 7, 8, 9]);
    }

    #[test]
    fn code_3_cbr_two_frames_with_padding() {
        // Count byte: M=2, CBR, padding set. Padding-length byte 2
        // means 2 padding bytes; R = N-2-P = 9-2-3 = 4, so each
        // frame is R/M = 2 bytes. (In CBR mode no frame length is
        // coded; R is derived from the packet size [R6].)
        let packet = [toc(19, false, 3), 0x42, 2, 0xAA, 0xBB, 0xCC, 0xDD, 0, 0];
        let parsed = parse_opus_packet(&packet).unwrap();
        assert_eq!(parsed.frame_count(), 2);
        assert_eq!(parsed.frames[0], &[0xAA, 0xBB]);
        assert_eq!(parsed.frames[1], &[0xCC, 0xDD]);
        assert_eq!(parsed.padding_bytes, 2);
    }

    #[test]
    fn code_3_padding_255_continues() {
        // Padding-length bytes 255, 0 mean 254 + 0 = 254 padding
        // bytes. M=1 CBR, R = N-2-P = 258-2-256 = 0: one
        // zero-length (DTX) frame.
        let mut packet = vec![toc(19, false, 3), 0x41, 255, 0];
        packet.extend(std::iter::repeat_n(0u8, 254));
        let parsed = parse_opus_packet(&packet).unwrap();
        assert_eq!(parsed.frame_count(), 1);
        assert_eq!(parsed.frames[0].len(), 0);
        assert_eq!(parsed.padding_bytes, 254);
    }

    #[test]
    fn code_3_cbr_non_multiple_is_invalid() {
        // M=2 CBR, no padding: N=6, R = 6-2-0 = 4, not divisible
        // by M=2... use N=5 instead: R = 3, not divisible by 2.
        let packet = [toc(19, false, 3), 0x02, 0xAA, 0xBB, 0xCC];
        let err = parse_opus_packet(&packet).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn code_3_vbr_with_padding() {
        // Count byte: M=2, VBR, padding set. Padding-length byte 1
        // means 1 padding byte; first frame length 2, second takes
        // the rest (3 bytes) before the padding.
        let packet = [
            toc(19, false, 3),
            0xC2,
            1,
            2,
            0xAA,
            0xBB,
            0xCC,
            0xDD,
            0xEE,
            0,
        ];
        let parsed = parse_opus_packet(&packet).unwrap();
        assert_eq!(parsed.frame_count(), 2);
        assert_eq!(parsed.frames[0], &[0xAA, 0xBB]);
        assert_eq!(parsed.frames[1], &[0xCC, 0xDD, 0xEE]);
        assert_eq!(parsed.padding_bytes, 1);
    }

    #[test]
    fn code_3_zero_frames_is_invalid() {
        let packet = [toc(19, false, 3), 0x00];
        let err = parse_opus_packet(&packet).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn packet_over_120_ms_is_invalid() {
        // 48 frames of 2.5 ms = 120 ms is allowed; 49 is not.
        let mut packet = vec![toc(16, false, 3), 0xB0]; // M=48, VBR
        packet.resize(packet.len() + 47, 0); // zero-length frames
                                             // Last frame takes the rest (0 bytes).
        let parsed = parse_opus_packet(&packet).unwrap();
        assert_eq!(parsed.frame_count(), 48);
        let mut packet = vec![toc(16, false, 3), 0xB1]; // M=49, VBR
        packet.resize(packet.len() + 48, 0);
        let err = parse_opus_packet(&packet).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn empty_packet_is_truncated() {
        let err = parse_opus_packet(&[]).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn truncated_code_3_is_truncated() {
        let packet = [toc(19, false, 3)];
        let err = parse_opus_packet(&packet).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn opus_head_family_0_stereo() {
        let mut head = Vec::new();
        head.extend_from_slice(b"OpusHead");
        head.push(1); // version
        head.push(2); // channels
        head.extend_from_slice(&312u16.to_le_bytes()); // pre-skip
        head.extend_from_slice(&48000u32.to_le_bytes()); // rate
        head.extend_from_slice(&0i16.to_le_bytes()); // gain
        head.push(0); // family 0
        let parsed = parse_opus_head(&head).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.channels, 2);
        assert_eq!(parsed.pre_skip, 312);
        assert_eq!(parsed.input_sample_rate, 48000);
        assert_eq!(parsed.output_gain_db(), 0.0);
        assert_eq!(parsed.stream_count, 1);
        assert_eq!(parsed.coupled_count, 1);
    }

    #[test]
    fn opus_head_family_1_mapping() {
        let mut head = Vec::new();
        head.extend_from_slice(b"OpusHead");
        head.push(1);
        head.push(4); // channels
        head.extend_from_slice(&0u16.to_le_bytes());
        head.extend_from_slice(&48000u32.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes());
        head.push(1); // family 1
        head.push(2); // streams
        head.push(1); // coupled
        head.extend_from_slice(&[0, 2, 1, 2]); // mapping
        let parsed = parse_opus_head(&head).unwrap();
        assert_eq!(parsed.stream_count, 2);
        assert_eq!(parsed.coupled_count, 1);
        assert_eq!(parsed.channel_mapping, vec![0, 2, 1, 2]);
    }

    #[test]
    fn opus_head_bad_version_is_invalid() {
        let mut head = vec![0u8; 19];
        head[..8].copy_from_slice(b"OpusHead");
        head[8] = 2;
        let err = parse_opus_head(&head).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn opus_head_zero_channels_is_invalid() {
        let mut head = vec![0u8; 19];
        head[..8].copy_from_slice(b"OpusHead");
        head[8] = 1;
        head[9] = 0;
        let err = parse_opus_head(&head).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn opus_tags_round_trip() {
        let mut tags = Vec::new();
        tags.extend_from_slice(b"OpusTags");
        tags.extend_from_slice(&4u32.to_le_bytes());
        tags.extend_from_slice(b"test");
        tags.extend_from_slice(&2u32.to_le_bytes());
        tags.extend_from_slice(&9u32.to_le_bytes());
        tags.extend_from_slice(b"ENCODER=x");
        tags.extend_from_slice(&8u32.to_le_bytes());
        tags.extend_from_slice(b"TITLE=Hi");
        let parsed = parse_opus_tags(&tags).unwrap();
        assert_eq!(parsed.vendor, b"test");
        assert_eq!(parsed.comments.len(), 2);
        assert_eq!(parsed.comments[0], b"ENCODER=x");
    }
}
