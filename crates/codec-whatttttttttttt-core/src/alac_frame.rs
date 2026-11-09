//! ALAC frame decode.
//!
//! Format reference: Apple's ALAC reference implementation
//! (`macosforge/alac`, Apache License 2.0) — `codec/ALACDecoder.cpp`
//! (`Decode`, `FillElement`, `DataStreamElement`),
//! `codec/ag_dec.c` (Rice decode, in `alac_predictor.rs`),
//! `codec/dp_dec.c` (predictor, in `alac_predictor.rs`) and
//! `codec/matrix_dec.c` (`unmix16`, in `alac_predictor.rs`).
//! The magic-cookie layout is `ALACMagicCookieDescription.txt`
//! in the same repository: `frameLength` (u32 BE),
//! `compatibleVersion`, `bitDepth`, `pb`, `mb`, `kb`,
//! `numChannels`, `maxRun` (u16 BE), `maxFrameBytes`,
//! `avgBitRate`, `sampleRate` (all u32 BE after `maxRun`).
//!
//! A frame is a sequence of elements, each starting with a
//! 3-bit tag (`ID_SCE` 0, `ID_CPE` 1, `ID_CCE` 2, `ID_LFE` 3,
//! `ID_DSE` 4, `ID_PCE` 5, `ID_FIL` 6, `ID_END` 7), a 4-bit
//! instance tag, 12 unused bits (must be zero), and a 4-bit
//! header: 1-bit partial-frame flag, 2-bit shift-off
//! (`bytesShifted`, never 3) and 1-bit escape flag. A partial
//! frame carries its sample count as two 16-bit reads. A
//! compressed element carries mix parameters, one predictor
//! parameter block per channel (mode, denominator shift, Rice
//! history factor, coefficient count, coefficients), Rice-coded
//! residuals, and a predictor pass; an escaped element carries
//! raw samples. Channel coupling (`ID_CCE`) and program config
//! (`ID_PCE`) are not decoded and return `Unsupported`.
//!
//! The decoder is lossless and bit-exact: integer samples are
//! returned at the cookie bit depth (16/20/24/32), so a test
//! can compare them with ffmpeg's ALAC decode sample for
//! sample. The `f32` conversion in `AlacDecodedFrame` divides
//! by `2^(bit_depth - 1)`, the same scale `pcm.rs` uses.

// Casts in this module are bounded by construction: values
// cast to a narrower integer come from bit reads of a known
// width, a validated bit depth (16/20/24/32), or a shift
// amount the cookie bounds, and the `i64` intermediates are
// the reference decoder's own arithmetic (its C code does
// the same in 32-bit words). The pedantic cast lints do not
// find real truncation risks here; the bounds are checked
// where the values are read, not where they are cast.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_precision_loss)]

use crate::alac_predictor::{dyn_decompress, unpc_block, AgParams, AlacBitReader};
use crate::error::{Error, Result};
use crate::frame::{AudioFrame, Frame};

/// Largest ALAC frame length accepted (Apple's README: packets
/// above 16,384 sample frames are not supported).
const MAX_ALAC_FRAME_LENGTH: u32 = 16_384;
/// Zero bytes appended to a packet so the Rice decoder's
/// lookahead never reads out of bounds (see
/// `alac_predictor.rs`).
const RICE_PADDING_BYTES: usize = 8;

/// ALAC magic cookie (`ALACSpecificConfig`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlacConfig {
    /// Samples per frame (4096 in ffmpeg's writer).
    pub frame_length: u32,
    /// Compatible version (0).
    pub compatible_version: u8,
    /// Bits per sample: 16, 20, 24 or 32.
    pub bit_depth: u32,
    /// Rice history multiplier base (`pb`, 40 in ffmpeg).
    pub pb: u32,
    /// Rice initial history (`mb`, 10 in ffmpeg).
    pub mb: u32,
    /// Rice parameter cap (`kb`, 14 in ffmpeg).
    pub kb: u32,
    /// Channel count, 1..=8.
    pub channels: u32,
    /// Maximum zero run (`maxRun`, 0 in ffmpeg's writer).
    pub max_run: u32,
    /// Largest coded frame, in bytes (0 means unknown).
    pub max_frame_bytes: u32,
    /// Average bitrate, in bits per second (0 means unknown).
    pub avg_bit_rate: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
}

fn read_be_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data.get(offset..offset + 4).ok_or(Error::Truncated {
        needed: offset + 4,
        had: data.len(),
    })?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_be_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data.get(offset..offset + 2).ok_or(Error::Truncated {
        needed: offset + 2,
        had: data.len(),
    })?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Parses an ALAC magic cookie.
///
/// Accepts the 24-byte `ALACSpecificConfig` on its own, the
/// 36-byte `alac` atom (`size`, `alac`, version, config), and
/// the `frma`-wrapped form Apple's `Init` skips over (a 12-byte
/// `frma` atom followed by the `alac` atom).
///
/// # Errors
///
/// Returns `Truncated` when fewer than 24 config bytes are
/// present, and `InvalidArgument` when the frame length is
/// zero or above 16,384, the bit depth is not 16/20/24/32,
/// the channel count is zero or above 8, or the sample rate
/// is zero.
pub fn parse_alac_config(data: &[u8]) -> Result<AlacConfig> {
    let mut offset = 0usize;
    if data.len() >= 12 && &data[4..8] == b"frma" {
        offset += 12;
    }
    if data.len() >= offset + 12 && &data[offset + 4..offset + 8] == b"alac" {
        offset += 12;
    }
    let config = data.get(offset..).ok_or(Error::Truncated {
        needed: offset + 24,
        had: data.len(),
    })?;
    if config.len() < 24 {
        return Err(Error::Truncated {
            needed: 24,
            had: config.len(),
        });
    }
    let frame_length = read_be_u32(config, 0)?;
    let bit_depth = u32::from(config[5]);
    let channels = u32::from(config[9]);
    let sample_rate = read_be_u32(config, 20)?;
    if frame_length == 0 || frame_length > MAX_ALAC_FRAME_LENGTH {
        return Err(Error::InvalidArgument(
            "alac frame length must be between 1 and 16384",
        ));
    }
    if !matches!(bit_depth, 16 | 20 | 24 | 32) {
        return Err(Error::InvalidArgument(
            "alac bit depth must be 16, 20, 24, or 32",
        ));
    }
    if channels == 0 || channels > 8 {
        return Err(Error::InvalidArgument(
            "alac channel count must be between 1 and 8",
        ));
    }
    if sample_rate == 0 {
        return Err(Error::InvalidArgument("alac sample rate must not be zero"));
    }
    Ok(AlacConfig {
        frame_length,
        compatible_version: config[4],
        bit_depth,
        pb: u32::from(config[6]),
        mb: u32::from(config[7]),
        kb: u32::from(config[8]),
        channels,
        max_run: u32::from(read_be_u16(config, 10)?),
        max_frame_bytes: read_be_u32(config, 12)?,
        avg_bit_rate: read_be_u32(config, 16)?,
        sample_rate,
    })
}

/// One decoded ALAC frame: integer samples at the cookie bit
/// depth, interleaved in bitstream channel order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlacDecodedFrame {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u32,
    /// Bits per sample (16, 20, 24 or 32).
    pub bit_depth: u32,
    /// Frames (one sample per channel) in this frame.
    pub frame_count: usize,
    /// Interleaved integer samples.
    pub samples: Vec<i32>,
}

impl AlacDecodedFrame {
    /// Converts the integer samples to the library's `f32`
    /// audio frame (`sample / 2^(bit_depth - 1)`).
    #[must_use]
    pub fn to_audio_frame(&self, timestamp: u64) -> AudioFrame {
        let scale = 1.0f32 / (1u64 << (self.bit_depth - 1)) as f32;
        AudioFrame {
            sample_rate: self.sample_rate,
            channels: self.channels,
            samples: self.samples.iter().map(|&s| s as f32 * scale).collect(),
            timestamp,
        }
    }
}

/// Predictor parameter block for one channel, read from the
/// bitstream after the mix parameters.
#[derive(Debug, Clone)]
struct ChannelParams {
    mode: u32,
    den_shift: u32,
    pb_factor: u32,
    coefs: Vec<i16>,
}

fn read_channel_params(reader: &mut AlacBitReader<'_>) -> Result<ChannelParams> {
    let header = reader.read_bits(8)?;
    let mode = header >> 4;
    let den_shift = header & 0xF;
    let header = reader.read_bits(8)?;
    let pb_factor = header >> 5;
    let num = (header & 0x1F) as usize;
    let mut coefs = Vec::with_capacity(num);
    for _ in 0..num {
        coefs.push(reader.read_signed(16)? as i16);
    }
    Ok(ChannelParams {
        mode,
        den_shift,
        pb_factor,
        coefs,
    })
}

/// Runs one channel through Rice decode and the predictor.
/// `mode != 0` first runs the running-sum mode (31) in place,
/// then the general mode over the result, as in Apple's
/// `Decode`.
fn decode_channel(
    reader: &mut AlacBitReader<'_>,
    config: &AlacConfig,
    params: &ChannelParams,
    num_samples: usize,
    chan_bits: u32,
) -> Result<Vec<i32>> {
    let ag = AgParams::new(config.mb, (config.pb * params.pb_factor) / 4, config.kb)?;
    let residuals = dyn_decompress(reader, &ag, num_samples, chan_bits)?;
    if params.mode == 0 {
        unpc_block(
            &residuals,
            &params.coefs,
            params.coefs.len(),
            chan_bits,
            params.den_shift,
        )
    } else {
        let intermediate = unpc_block(&residuals, &[], 31, chan_bits, 0)?;
        unpc_block(
            &intermediate,
            &params.coefs,
            params.coefs.len(),
            chan_bits,
            params.den_shift,
        )
    }
}

/// Common element header: instance tag, 12 unused bits
/// (must be zero), and the 4-bit flag byte. Returns
/// `(partial_frame, bytes_shifted, escape_flag)`.
fn read_element_header(reader: &mut AlacBitReader<'_>) -> Result<(bool, u32, bool)> {
    let _instance_tag = reader.read_bits(4)?;
    let unused = reader.read_bits(12)?;
    if unused != 0 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "alac element unused header bits must be zero",
        });
    }
    let header = reader.read_bits(4)?;
    let partial_frame = (header >> 3) & 1 == 1;
    let bytes_shifted = (header >> 1) & 0x3;
    if bytes_shifted == 3 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "alac bytes_shifted must not be 3",
        });
    }
    let escape_flag = header & 1 == 1;
    Ok((partial_frame, bytes_shifted, escape_flag))
}

/// Reads the partial-frame sample count (two 16-bit reads)
/// and validates it against the cookie frame length.
fn read_partial_count(reader: &mut AlacBitReader<'_>, config: &AlacConfig) -> Result<usize> {
    let high = reader.read_bits(16)? as usize;
    let low = reader.read_bits(16)? as usize;
    let count = (high << 16) | low;
    if count == 0 || count as u32 > config.frame_length {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "alac partial frame sample count out of range",
        });
    }
    Ok(count)
}

/// Reads raw (escaped) samples of `chan_bits` bits into
/// `out`, sign-extended. Above 16 bits the value is read as a
/// 16-bit high part plus the remaining low bits, as in
/// Apple's escaped path.
fn read_raw_samples(
    reader: &mut AlacBitReader<'_>,
    num_samples: usize,
    chan_bits: u32,
    out: &mut Vec<i32>,
) -> Result<()> {
    out.reserve(num_samples);
    if chan_bits <= 16 {
        let shift = 32 - chan_bits;
        for _ in 0..num_samples {
            let value = reader.read_bits(chan_bits)?;
            out.push(((value << shift) as i32) >> shift);
        }
    } else {
        let extra = chan_bits - 16;
        let shift = 32 - chan_bits;
        for _ in 0..num_samples {
            let high = reader.read_bits(16)?;
            let value = (((high as i32) << 16) >> shift) | reader.read_bits(extra)? as i32;
            out.push(value);
        }
    }
    Ok(())
}

/// Decodes one single-channel element (SCE/LFE) into
/// `channels_out[channel_index]`.
#[allow(
    clippy::too_many_lines,
    reason = "mirrors Apple's per-element decode; splitting it would hide the bitstream order"
)]
fn decode_single_channel(
    reader: &mut AlacBitReader<'_>,
    config: &AlacConfig,
    num_samples: &mut usize,
    channels_out: &mut [Vec<i32>],
    channel_index: usize,
) -> Result<()> {
    let (partial_frame, bytes_shifted, escape_flag) = read_element_header(reader)?;
    if partial_frame {
        *num_samples = read_partial_count(reader, config)?;
    }
    let chan_bits = config.bit_depth - bytes_shifted * 8;
    if chan_bits == 0 || chan_bits > 32 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "alac channel bit width out of range",
        });
    }
    let mut mix: Vec<i32> = Vec::new();
    if escape_flag {
        read_raw_samples(reader, *num_samples, chan_bits, &mut mix)?;
    } else {
        let mix_bits = reader.read_bits(8)?;
        let mix_res = reader.read_signed(8)?;
        let _ = (mix_bits, mix_res);
        let params = read_channel_params(reader)?;
        let mut shift_reader: Option<AlacBitReader<'_>> = None;
        if bytes_shifted != 0 {
            shift_reader = Some(reader.clone());
            reader.advance((bytes_shifted as usize * 8) * *num_samples)?;
        }
        mix = decode_channel(reader, config, &params, *num_samples, chan_bits)?;
        if let Some(mut shift_reader) = shift_reader {
            let shift = bytes_shifted * 8;
            let mut shift_values: Vec<u32> = Vec::with_capacity(*num_samples);
            for _ in 0..*num_samples {
                shift_values.push(shift_reader.read_bits(shift)?);
            }
            // Apple's 16-bit output ignores the shift buffer;
            // 20-bit output left-justifies without it; 24/32-bit
            // output ORs the shifted low bits back in.
            match config.bit_depth {
                24 | 32 => {
                    for (index, value) in mix.iter_mut().enumerate() {
                        let shifted = i64::from(*value) << shift;
                        *value = (shifted | i64::from(shift_values[index])) as i32;
                    }
                }
                _ => {}
            }
        }
    }
    channels_out[channel_index] = mix;
    Ok(())
}

/// Decodes one channel-pair element (CPE) into
/// `channels_out[channel_index]` and `[channel_index + 1]`.
#[allow(
    clippy::too_many_lines,
    reason = "mirrors Apple's per-element decode; splitting it would hide the bitstream order"
)]
fn decode_channel_pair(
    reader: &mut AlacBitReader<'_>,
    config: &AlacConfig,
    num_samples: &mut usize,
    channels_out: &mut [Vec<i32>],
    channel_index: usize,
) -> Result<()> {
    let (partial_frame, bytes_shifted, escape_flag) = read_element_header(reader)?;
    if partial_frame {
        *num_samples = read_partial_count(reader, config)?;
    }
    let chan_bits = config.bit_depth - bytes_shifted * 8 + 1;
    if chan_bits == 0 || chan_bits > 32 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "alac channel bit width out of range",
        });
    }
    let mut buffer_u: Vec<i32> = Vec::new();
    let mut buffer_v: Vec<i32> = Vec::new();
    let mut mix_bits: u32 = 0;
    let mut mix_res: i32 = 0;
    let mut shift_values: Vec<u32> = Vec::new();
    if escape_flag {
        let escaped_bits = config.bit_depth;
        let shift = 32 - escaped_bits;
        if escaped_bits <= 16 {
            for _ in 0..*num_samples {
                let value = reader.read_bits(escaped_bits)?;
                buffer_u.push(((value << shift) as i32) >> shift);
                let value = reader.read_bits(escaped_bits)?;
                buffer_v.push(((value << shift) as i32) >> shift);
            }
        } else {
            let extra = escaped_bits - 16;
            for _ in 0..*num_samples {
                let high = reader.read_bits(16)?;
                let value = (((high as i32) << 16) >> shift) | reader.read_bits(extra)? as i32;
                buffer_u.push(value);
                let high = reader.read_bits(16)?;
                let value = (((high as i32) << 16) >> shift) | reader.read_bits(extra)? as i32;
                buffer_v.push(value);
            }
        }
    } else {
        mix_bits = reader.read_bits(8)?;
        mix_res = reader.read_signed(8)?;
        let params_u = read_channel_params(reader)?;
        let params_v = read_channel_params(reader)?;
        let mut shift_reader: Option<AlacBitReader<'_>> = None;
        if bytes_shifted != 0 {
            shift_reader = Some(reader.clone());
            reader.advance((bytes_shifted as usize * 8) * 2 * *num_samples)?;
        }
        buffer_u = decode_channel(reader, config, &params_u, *num_samples, chan_bits)?;
        buffer_v = decode_channel(reader, config, &params_v, *num_samples, chan_bits)?;
        if let Some(mut shift_reader) = shift_reader {
            let shift = bytes_shifted * 8;
            for _ in 0..(*num_samples * 2) {
                shift_values.push(shift_reader.read_bits(shift)?);
            }
        }
    }
    let mut left: Vec<i32> = Vec::with_capacity(*num_samples);
    let mut right: Vec<i32> = Vec::with_capacity(*num_samples);
    let shift = bytes_shifted * 8;
    for index in 0..*num_samples {
        let (l, r) = if mix_res != 0 {
            let u_val = i64::from(buffer_u[index]);
            let v_val = i64::from(buffer_v[index]);
            let l = u_val + v_val - ((i64::from(mix_res) * v_val) >> mix_bits);
            (l, l - v_val)
        } else {
            (i64::from(buffer_u[index]), i64::from(buffer_v[index]))
        };
        match config.bit_depth {
            16 => {
                left.push(i32::from(l as i16));
                right.push(i32::from(r as i16));
            }
            24 | 32 if bytes_shifted != 0 => {
                let shift_u = i64::from(shift_values[index * 2]);
                let shift_v = i64::from(shift_values[index * 2 + 1]);
                left.push(((l << shift) | shift_u) as i32);
                right.push(((r << shift) | shift_v) as i32);
            }
            _ => {
                left.push(l as i32);
                right.push(r as i32);
            }
        }
    }
    channels_out[channel_index] = left;
    channels_out[channel_index + 1] = right;
    Ok(())
}

/// Skips a fill element: a 4-bit count, or 4-bit plus 8-bit
/// count minus one when the 4-bit count is 15.
fn skip_fill_element(reader: &mut AlacBitReader<'_>) -> Result<()> {
    let mut count = reader.read_bits(4)? as usize;
    if count == 15 {
        count += reader.read_bits(8)? as usize;
        count = count.saturating_sub(1);
    }
    reader.advance(count * 8)
}

/// Skips a data-stream element: instance tag, align flag,
/// 8-bit count (plus 8 more when 255), optional byte
/// alignment, then the data bytes.
fn skip_data_stream_element(reader: &mut AlacBitReader<'_>) -> Result<()> {
    let _instance_tag = reader.read_bits(4)?;
    let align_flag = reader.read_bit()?;
    let mut count = reader.read_bits(8)? as usize;
    if count == 255 {
        count += reader.read_bits(8)? as usize;
    }
    if align_flag {
        reader.align_to_byte();
    }
    reader.advance(count * 8)
}

/// Decodes one ALAC frame (one MP4 sample) into integer
/// samples at the cookie bit depth.
///
/// # Errors
///
/// Returns `Truncated` when the packet ends mid-element,
/// `InvalidData` for a malformed element header, a partial
/// count outside the cookie frame length, or an out-of-range
/// channel width, and `Unsupported` for channel coupling or
/// program-config elements.
pub fn decode_alac_frame(data: &[u8], config: &AlacConfig) -> Result<AlacDecodedFrame> {
    if data.is_empty() {
        return Err(Error::Truncated { needed: 1, had: 0 });
    }
    if data.len() > config.max_frame_bytes as usize && config.max_frame_bytes != 0 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "alac frame exceeds cookie max frame bytes",
        });
    }
    let mut padded: Vec<u8> = Vec::with_capacity(data.len() + RICE_PADDING_BYTES);
    padded.extend_from_slice(data);
    padded.extend_from_slice(&[0u8; RICE_PADDING_BYTES]);
    let mut reader = AlacBitReader::new(&padded, data.len());
    let channels = config.channels as usize;
    let mut channels_out: Vec<Vec<i32>> = vec![Vec::new(); channels];
    let mut num_samples = config.frame_length as usize;
    let mut channel_index: usize = 0;
    while channel_index < channels {
        if reader.bit_position() >= reader.limit_bits() {
            return Err(Error::Truncated { needed: 1, had: 0 });
        }
        let tag = reader.read_bits(3)?;
        match tag {
            0 | 3 => {
                decode_single_channel(
                    &mut reader,
                    config,
                    &mut num_samples,
                    &mut channels_out,
                    channel_index,
                )?;
                channel_index += 1;
            }
            1 => {
                if channel_index + 2 > channels {
                    return Err(Error::InvalidData {
                        offset: reader.bit_position() / 8,
                        detail: "alac channel pair exceeds channel count",
                    });
                }
                decode_channel_pair(
                    &mut reader,
                    config,
                    &mut num_samples,
                    &mut channels_out,
                    channel_index,
                )?;
                channel_index += 2;
            }
            2 | 5 => {
                return Err(Error::Unsupported(
                    "alac channel coupling and program config elements are not decoded",
                ));
            }
            4 => {
                skip_data_stream_element(&mut reader)?;
            }
            6 => {
                skip_fill_element(&mut reader)?;
            }
            7 => {
                reader.align_to_byte();
                break;
            }
            _ => {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "alac element tag out of range",
                });
            }
        }
    }
    let frame_count = num_samples;
    let mut samples: Vec<i32> = Vec::with_capacity(frame_count * channels);
    for index in 0..frame_count {
        for channel_out in channels_out.iter().take(channels) {
            samples.push(channel_out.get(index).copied().unwrap_or(0));
        }
    }
    Ok(AlacDecodedFrame {
        sample_rate: config.sample_rate,
        channels: config.channels,
        bit_depth: config.bit_depth,
        frame_count,
        samples,
    })
}

/// Decodes one ALAC frame straight to the library's audio
/// frame type.
///
/// # Errors
///
/// Returns the same errors as `decode_alac_frame`.
pub fn decode_alac_audio_frame(data: &[u8], config: &AlacConfig, timestamp: u64) -> Result<Frame> {
    let decoded = decode_alac_frame(data, config)?;
    Ok(Frame::Audio(decoded.to_audio_frame(timestamp)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie_16bit_mono_8000() -> AlacConfig {
        AlacConfig {
            frame_length: 4096,
            compatible_version: 0,
            bit_depth: 16,
            pb: 40,
            mb: 10,
            kb: 14,
            channels: 1,
            max_run: 0,
            max_frame_bytes: 8196,
            avg_bit_rate: 128_000,
            sample_rate: 8000,
        }
    }

    #[test]
    fn parses_24_byte_cookie() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&4096u32.to_be_bytes());
        bytes.extend_from_slice(&[0, 16, 40, 10, 14, 1]);
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&8196u32.to_be_bytes());
        bytes.extend_from_slice(&128_000u32.to_be_bytes());
        bytes.extend_from_slice(&8000u32.to_be_bytes());
        let config = parse_alac_config(&bytes).unwrap();
        assert_eq!(config, cookie_16bit_mono_8000());
    }

    #[test]
    fn parses_36_byte_atom_cookie() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&36u32.to_be_bytes());
        bytes.extend_from_slice(b"alac");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&4096u32.to_be_bytes());
        bytes.extend_from_slice(&[0, 16, 40, 10, 14, 1]);
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&8196u32.to_be_bytes());
        bytes.extend_from_slice(&128_000u32.to_be_bytes());
        bytes.extend_from_slice(&8000u32.to_be_bytes());
        let config = parse_alac_config(&bytes).unwrap();
        assert_eq!(config, cookie_16bit_mono_8000());
    }

    #[test]
    fn rejects_bad_bit_depth() {
        let mut config = cookie_16bit_mono_8000();
        config.bit_depth = 8;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&4096u32.to_be_bytes());
        bytes.extend_from_slice(&[0, 8, 40, 10, 14, 1]);
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&8196u32.to_be_bytes());
        bytes.extend_from_slice(&128_000u32.to_be_bytes());
        bytes.extend_from_slice(&8000u32.to_be_bytes());
        assert!(matches!(
            parse_alac_config(&bytes),
            Err(Error::InvalidArgument(_))
        ));
        let _ = config;
    }

    #[test]
    fn empty_frame_is_truncated() {
        let err = decode_alac_frame(&[], &cookie_16bit_mono_8000()).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn f32_scale_matches_bit_depth() {
        let frame = AlacDecodedFrame {
            sample_rate: 8000,
            channels: 1,
            bit_depth: 16,
            frame_count: 2,
            samples: vec![32767, -32768],
        };
        let audio = frame.to_audio_frame(0);
        assert!((audio.samples[0] - 32767.0 / 32768.0).abs() < 1e-6);
        assert_eq!(audio.samples[1], -1.0);
    }
}
