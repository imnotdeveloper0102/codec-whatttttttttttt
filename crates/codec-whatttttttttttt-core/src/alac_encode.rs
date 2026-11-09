//! ALAC frame encode.
//!
//! Format reference: Apple's ALAC reference implementation
//! (`macosforge/alac`, Apache License 2.0) — `codec/ALACEncoder.cpp`
//! (frame layout), `codec/ag_enc.c` (`dyn_comp`, Rice encode),
//! `codec/dp_enc.c` (`pc_block`, forward predictor) and
//! `codec/matrix_enc.c` (`mix16`, channel decorrelation). This
//! module is the inverse of the decode paths in `alac_frame.rs`
//! (`Decode`, `FillElement`) and `alac_predictor.rs`
//! (`dyn_decompress`, `unpc_block`, `unmix_stereo_16`): every
//! encoder step is written so the decoder's arithmetic
//! reproduces the input samples exactly.
//!
//! How a frame is built: one `encode` call is one ALAC frame
//! (one MP4 sample), holding 1..=4096 sample frames. Channel
//! pairs are coded as `ID_CPE` elements and a leftover channel
//! as `ID_SCE`; the mix is identity (`mix_bits` 0, `mix_res` 0),
//! so the coded channels are the left/right channels unchanged.
//! Each channel is coded with an order-1 predictor
//! (`den_shift` 9, initial coefficient 512, so the first
//! prediction is the previous sample) whose coefficient
//! adaptation is replicated exactly from `unpc_block`, then
//! adaptive-Golomb (Rice) coded with the same running mean
//! update and zero-run mode as `dyn_decompress` — including the
//! shared-bit quirk where the decoder rewinds one bit when the
//! remainder is below 2, so the encoder writes only `k - 1`
//! remainder bits and lets the next codeword supply the low
//! bit. When the Rice stream would be longer than raw samples
//! (white noise), the element is written escaped instead, so
//! encoding never expands the input by more than the element
//! headers.
//!
//! Input is the library's `f32` audio frame, quantized to
//! integer samples with round-to-nearest. Integer-valued inputs
//! (`i16 / 2^15`, `i24 / 2^23`) are exactly representable in
//! `f32` and round-trip exactly; the round trip
//! `decode(encode(pcm)) == pcm` is bit-exact on the integer
//! samples. ALAC is an open format: no license gate applies.

// Casts in this module are bounded by construction: values
// cast to a narrower integer come from bit writes of a known
// width, a validated bit depth (16/24), or the predictor's
// `i64` intermediates (the reference encoder's C code does the
// same arithmetic in 32-bit words with wrapping). The pedantic
// cast lints do not find real truncation risks here; the bounds
// are checked where the values are read, not where they are cast.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]

use crate::alac_frame::AlacConfig;
use crate::bitwriter::BitWriter;
use crate::error::{Error, Result};
use crate::frame::{AudioFrame, Codec, Packet};
use crate::limits::{MAX_CHANNELS, MAX_SAMPLE_RATE};

/// Samples per ALAC frame (the cookie frame length, as in
/// ffmpeg's writer).
const FRAME_LENGTH: u32 = 4096;
/// Rice history multiplier base (`pb`, 40 in ffmpeg's writer).
const COOKIE_PB: u32 = 40;
/// Rice initial history (`mb`, 10 in ffmpeg's writer).
const COOKIE_MB: u32 = 10;
/// Rice parameter cap (`kb`, 14 in ffmpeg's writer).
const COOKIE_KB: u32 = 14;
/// Per-channel Rice history multiplier (`pb_factor`, 4 keeps the
/// cookie `pb` unchanged: `(40 * 4) / 4 == 40`).
const RICE_PB_FACTOR: u32 = 4;
/// Predictor order: one active coefficient.
const PREDICTOR_ORDER: usize = 1;
/// Predictor denominator shift (`den_shift`).
const PREDICTOR_DEN_SHIFT: u32 = 9;
/// Predictor initial coefficient: `1 << den_shift`, so the
/// first prediction is the previous sample.
const PREDICTOR_INITIAL_COEF: i64 = 1 << PREDICTOR_DEN_SHIFT;
/// Element tags (`ID_SCE` 0, `ID_CPE` 1, `ID_END` 7).
const TAG_SCE: u32 = 0;
const TAG_CPE: u32 = 1;
const TAG_END: u32 = 7;
/// Mix parameters: identity (`mix_bits` 0, `mix_res` 0).
const MIX_BITS: u32 = 0;
const MIX_RES: u32 = 0;

/// `QBSHIFT` in `aglib.h`: the mean is tracked in Q9 fixed point.
const QB_SHIFT: u32 = 9;
/// `QB`, the fixed-point one (512).
const QB: u32 = 1 << QB_SHIFT;
/// `MMULSHIFT` in `aglib.h` (2).
const MMUL_SHIFT: u32 = 2;
/// `MDENSHIFT` in `aglib.h` (`QBSHIFT - MMULSHIFT - 1` = 6).
const MDEN_SHIFT: u32 = QB_SHIFT - MMUL_SHIFT - 1;
/// `MOFF` in `aglib.h` (`1 << (MDENSHIFT - 2)` = 16).
const MOFF: u32 = 1 << (MDEN_SHIFT - 2);
/// `BITOFF` in `aglib.h` (24).
const BITOFF: i32 = 24;
/// Largest unary prefix before the escape path (`MAX_PREFIX_16` /
/// `MAX_PREFIX_32` = 9).
const MAX_PREFIX: u32 = 9;
/// Mean clamp (`N_MAX_MEAN_CLAMP` / `N_MEAN_CLAMP_VAL` = 0xffff).
const MAX_MEAN_CLAMP: u32 = 0xffff;
/// Zero-run length at or above which the decoder clears the
/// one-offset for the next value (`dyn_decomp` in `ag_dec.c`).
const ZERO_RUN_OFFSET_CLEAR: usize = 65_535;

/// `lg3a` in `ag_dec.c`: `floor(log2(x + 3))`. Mirrors
/// `alac_predictor.rs` (whose `x + 3` is never zero, so `ilog2`
/// cannot panic here).
fn lg3a(x: u32) -> u32 {
    x.wrapping_add(3).ilog2()
}

/// Sign-extends `value` to `chan_bits` bits (`(value <<
/// chanshift) >> chanshift` in `dp_dec.c`). Mirrors
/// `alac_predictor.rs`.
fn sign_extend(value: i32, chan_bits: u32) -> i32 {
    let shift = 32 - chan_bits;
    value.wrapping_shl(shift).wrapping_shr(shift)
}

/// Zigzag-maps a signed residual to the unsigned value the Rice
/// coder carries: 0 -> 0, -1 -> 1, 1 -> 2, -2 -> 3, 2 -> 4.
/// The inverse of the sign step in `dyn_decompress`.
fn zigzag(value: i32) -> u32 {
    // Residuals are sign-extended `chan_bits` values (at most 25
    // bits here), so the `i64` math cannot overflow and the
    // result fits in `u32`.
    let magnitude = i64::from(value).abs();
    if value >= 0 {
        (magnitude * 2) as u32
    } else {
        (magnitude * 2 - 1) as u32
    }
}

/// Writes a unary prefix: `count` one bits followed by a zero.
fn write_unary(writer: &mut BitWriter, count: u32) -> Result<()> {
    if count > 0 {
        writer.write_bits((1u32 << count) - 1, count)?;
    }
    writer.write_bit(false);
    Ok(())
}

/// Writes the escape prefix: `MAX_PREFIX` one bits with no
/// terminating zero; the raw value follows immediately.
fn write_escape_prefix(writer: &mut BitWriter) -> Result<()> {
    writer.write_bits((1u32 << MAX_PREFIX) - 1, MAX_PREFIX)
}

/// Writes one Rice value, the inverse of `dyn_get_32bit` in
/// `ag_dec.c` (`k == 1` returns the prefix itself; otherwise the
/// remainder is `k` bits, of which the last is only consumed
/// when it is at least 2 — see the `k - 1` write below).
///
/// # Errors
///
/// Returns `InvalidData` when the divisor is zero, which cannot
/// happen for the `k` values this module produces.
fn write_rice_value(
    writer: &mut BitWriter,
    value: u32,
    divisor: u32,
    k: u32,
    max_bits: u32,
) -> Result<()> {
    if k == 1 {
        if value < MAX_PREFIX {
            write_unary(writer, value)?;
        } else {
            write_escape_prefix(writer)?;
            writer.write_bits(value, max_bits)?;
        }
        return Ok(());
    }
    if divisor == 0 {
        return Err(Error::InvalidData {
            offset: writer.bit_position() / 8,
            detail: "alac rice divisor must not be zero",
        });
    }
    let prefix = value / divisor;
    if prefix >= MAX_PREFIX {
        write_escape_prefix(writer)?;
        writer.write_bits(value, max_bits)?;
        return Ok(());
    }
    let remainder = value % divisor;
    write_unary(writer, prefix)?;
    if remainder >= 1 {
        writer.write_bits(remainder + 1, k)?;
    } else {
        // The decoder reads `k` remainder bits and then rewinds
        // one when the remainder is below 2, so only the top
        // `k - 1` zero bits are written here: the next codeword's
        // first bit is re-read as the remainder's low bit, and
        // the value still decodes to `prefix * divisor`.
        writer.write_bits(0, k - 1)?;
    }
    Ok(())
}

/// Writes one zero-run count, the inverse of `dyn_get` in
/// `ag_dec.c` (same remainder rule as `write_rice_value`, but
/// the escape path carries a raw 16-bit count).
///
/// # Errors
///
/// Returns `InvalidData` when the divisor or `k` is zero, which
/// cannot happen for the run parameters this module produces.
fn write_zero_run(writer: &mut BitWriter, count: u32, divisor: u32, k: u32) -> Result<()> {
    if divisor == 0 || k == 0 {
        return Err(Error::InvalidData {
            offset: writer.bit_position() / 8,
            detail: "alac zero-run rice parameters must not be zero",
        });
    }
    let prefix = count / divisor;
    if prefix >= MAX_PREFIX {
        write_escape_prefix(writer)?;
        writer.write_bits(count, 16)?;
        return Ok(());
    }
    let remainder = count % divisor;
    write_unary(writer, prefix)?;
    if remainder >= 1 {
        writer.write_bits(remainder + 1, k)?;
    } else {
        writer.write_bits(0, k - 1)?;
    }
    Ok(())
}

/// Adaptive-Golomb coder state (`AGParamRec` in `aglib.h`).
struct RiceCoder {
    /// Running mean, tracked in Q9 fixed point.
    mb: u32,
    /// History multiplier (`pb`), scaled per channel.
    pb: u32,
    /// Rice parameter cap (`kb`).
    kb: u32,
    /// `(1 << kb) - 1`, the mask for the zero-run parameter.
    wb: u32,
}

impl RiceCoder {
    /// Builds the coder from the cookie values, as
    /// `set_ag_params` does in `ag_dec.c`.
    fn new() -> Self {
        Self {
            mb: COOKIE_MB,
            pb: (COOKIE_PB * RICE_PB_FACTOR) / 4,
            kb: COOKIE_KB,
            wb: (1u32 << COOKIE_KB) - 1,
        }
    }
}

/// Rice-encodes `residuals` (`dyn_comp` in `ag_enc.c`), updating
/// the running mean and emitting zero runs exactly as
/// `dyn_decompress` expects to read them back.
///
/// # Errors
///
/// Returns `InvalidData` when a residual follows a zero run
/// with a zero value (unreachable when runs are counted from
/// the residuals themselves) or when a zero-run parameter is
/// out of range.
fn encode_residuals(
    writer: &mut BitWriter,
    residuals: &[i32],
    chan_bits: u32,
    coder: &mut RiceCoder,
) -> Result<()> {
    let num = residuals.len();
    let mut index = 0usize;
    // Offset the decoder adds to the next read value after a
    // zero run (`zmode` in `dyn_decomp`).
    let mut zmode: u32 = 0;
    while index < num {
        let zigzag_value = zigzag(residuals[index]);
        let value = zigzag_value.checked_sub(zmode).ok_or(Error::InvalidData {
            offset: writer.bit_position() / 8,
            detail: "alac residual after a zero run must be nonzero",
        })?;
        let m_before = coder.mb >> QB_SHIFT;
        let k = lg3a(m_before).min(coder.kb);
        let divisor = (1u32 << k) - 1;
        write_rice_value(writer, value, divisor, k, chan_bits)?;
        coder.mb = coder
            .pb
            .wrapping_mul(value.wrapping_add(zmode))
            .wrapping_add(coder.mb)
            .wrapping_sub((coder.pb.wrapping_mul(coder.mb)) >> QB_SHIFT);
        if value > MAX_MEAN_CLAMP {
            coder.mb = MAX_MEAN_CLAMP;
        }
        zmode = 0;
        index += 1;
        if ((coder.mb << MMUL_SHIFT) < QB) && index < num {
            let k_run =
                coder.mb.leading_zeros() as i32 - BITOFF + ((coder.mb + MOFF) >> MDEN_SHIFT) as i32;
            let k_run = u32::try_from(k_run).map_err(|_| Error::InvalidData {
                offset: writer.bit_position() / 8,
                detail: "alac zero-run rice parameter out of range",
            })?;
            let divisor_run = ((1u32 << k_run) - 1) & coder.wb;
            let mut run = 0usize;
            while index + run < num && residuals[index + run] == 0 {
                run += 1;
            }
            let run_count = u32::try_from(run).map_err(|_| Error::InvalidData {
                offset: writer.bit_position() / 8,
                detail: "alac zero run exceeds u32 range",
            })?;
            write_zero_run(writer, run_count, divisor_run, k_run)?;
            index += run;
            if run >= ZERO_RUN_OFFSET_CLEAR {
                zmode = 0;
            } else {
                zmode = 1;
            }
            coder.mb = 0;
        }
    }
    Ok(())
}

/// Forward predictor: the inverse of `unpc_block` in `dp_dec.c`
/// with mode 0 and one active coefficient.
///
/// The first sample is carried as-is; the second is the
/// difference from the first; later samples subtract the
/// prediction `top + ((sum + denhalf) >> den_shift)` and wrap
/// the difference into `chan_bits` bits, which is safe because
/// the decoder sign-extends the sum back. The coefficient is
/// adapted with exactly the decoder's rule, driven by the same
/// outputs and residual signs, so the decoder's adaptation
/// tracks this one sample for sample.
fn predict_forward(samples: &[i32], chan_bits: u32) -> Vec<i32> {
    let num = samples.len();
    let mut residuals = vec![0i32; num];
    if num == 0 {
        return residuals;
    }
    // Initial coefficients, written to the bitstream as the
    // predictor parameter block; the decoder adapts them from
    // here, and the loop below tracks the same adaptation.
    let mut coefs = [PREDICTOR_INITIAL_COEF; PREDICTOR_ORDER];
    residuals[0] = samples[0];
    let first = PREDICTOR_ORDER.min(num - 1);
    for j in 1..=first {
        residuals[j] = sign_extend(samples[j].wrapping_sub(samples[j - 1]), chan_bits);
    }
    let den_half: i64 = 1i64 << (PREDICTOR_DEN_SHIFT - 1);
    let lim = PREDICTOR_ORDER + 1;
    for j in lim..num {
        let top = i64::from(samples[j - lim]);
        let mut sum: i64 = 0;
        for k in 0..PREDICTOR_ORDER {
            sum += coefs[k] * (i64::from(samples[j - 1 - k]) - top);
        }
        let predicted = top + ((sum + den_half) >> PREDICTOR_DEN_SHIFT);
        let del = sign_extend((i64::from(samples[j]) - predicted) as i32, chan_bits);
        residuals[j] = del;
        let mut del0 = i64::from(del);
        let sign = del.signum();
        if sign > 0 {
            for k in (0..PREDICTOR_ORDER).rev() {
                let diff = top - i64::from(samples[j - 1 - k]);
                let diff_sign = diff.signum();
                coefs[k] -= diff_sign;
                del0 -= (PREDICTOR_ORDER - k) as i64 * ((diff_sign * diff) >> PREDICTOR_DEN_SHIFT);
                if del0 <= 0 {
                    break;
                }
            }
        } else if sign < 0 {
            for k in (0..PREDICTOR_ORDER).rev() {
                let diff = top - i64::from(samples[j - 1 - k]);
                let diff_sign = diff.signum();
                coefs[k] += diff_sign;
                del0 -=
                    (PREDICTOR_ORDER - k) as i64 * (((-diff_sign) * diff) >> PREDICTOR_DEN_SHIFT);
                if del0 >= 0 {
                    break;
                }
            }
        }
    }
    residuals
}

/// Common element header: instance tag, 12 unused zero bits,
/// and the 4-bit flag byte (partial-frame flag, `bytes_shifted`
/// 0, escape flag). A partial frame carries its sample count
/// as two 16-bit writes.
fn write_element_header(
    writer: &mut BitWriter,
    tag: u32,
    partial_frame: bool,
    escape: bool,
    num_samples: usize,
) -> Result<()> {
    writer.write_bits(tag, 3)?;
    writer.write_bits(0, 4)?;
    writer.write_bits(0, 12)?;
    let header = (u32::from(partial_frame) << 3) | u32::from(escape);
    writer.write_bits(header, 4)?;
    if partial_frame {
        writer.write_bits((num_samples >> 16) as u32, 16)?;
        writer.write_bits(num_samples as u32, 16)?;
    }
    Ok(())
}

/// One channel's predictor parameter block: mode 0, denominator
/// shift 9, Rice history factor 4, one coefficient (512).
fn write_channel_params(writer: &mut BitWriter) -> Result<()> {
    // Mode 0 in the high nibble, denominator shift 9 in the low.
    writer.write_bits(PREDICTOR_DEN_SHIFT, 8)?;
    writer.write_bits((RICE_PB_FACTOR << 5) | (PREDICTOR_ORDER as u32), 8)?;
    writer.write_signed(PREDICTOR_INITIAL_COEF, 16)?;
    Ok(())
}

/// Writes one compressed (Rice-coded) element: mix parameters,
/// one predictor block per channel, then the Rice residuals.
fn write_compressed_element(
    writer: &mut BitWriter,
    tag: u32,
    channels: &[&[i32]],
    chan_bits: u32,
    partial_frame: bool,
    num_samples: usize,
) -> Result<()> {
    write_element_header(writer, tag, partial_frame, false, num_samples)?;
    writer.write_bits(MIX_BITS, 8)?;
    writer.write_bits(MIX_RES, 8)?;
    for _ in channels {
        write_channel_params(writer)?;
    }
    for samples in channels {
        let residuals = predict_forward(samples, chan_bits);
        let mut coder = RiceCoder::new();
        encode_residuals(writer, &residuals, chan_bits, &mut coder)?;
    }
    Ok(())
}

/// Writes one raw sample as the escaped path reads it back:
/// the low `bits` bits for 16 bits or fewer, otherwise the
/// high 16 bits followed by the remaining low bits.
fn write_raw_sample(writer: &mut BitWriter, sample: i32, bits: u32) -> Result<()> {
    if bits <= 16 {
        let mask = if bits == 32 {
            u32::MAX
        } else {
            (1u32 << bits) - 1
        };
        writer.write_bits(sample as u32 & mask, bits)
    } else {
        let extra = bits - 16;
        let unsigned = sample as u32;
        writer.write_bits(unsigned >> extra, 16)?;
        writer.write_bits(unsigned & ((1u32 << extra) - 1), extra)
    }
}

/// Writes one escaped (raw-sample) element: samples are
/// interleaved per sample frame across the element's channels,
/// as the decoder reads them.
fn write_escaped_element(
    writer: &mut BitWriter,
    tag: u32,
    channels: &[&[i32]],
    bit_depth: u32,
    partial_frame: bool,
    num_samples: usize,
) -> Result<()> {
    write_element_header(writer, tag, partial_frame, true, num_samples)?;
    for j in 0..num_samples {
        for samples in channels {
            write_raw_sample(writer, samples[j], bit_depth)?;
        }
    }
    Ok(())
}

/// Builds one ALAC frame: channel pairs as `ID_CPE` elements,
/// a leftover channel as `ID_SCE`, then `ID_END`. When
/// `escaped` is set every element carries raw samples.
fn build_frame(
    channels: &[Vec<i32>],
    num_samples: usize,
    bit_depth: u32,
    escaped: bool,
) -> Result<Vec<u8>> {
    let mut writer = BitWriter::new();
    let partial_frame = num_samples < FRAME_LENGTH as usize;
    let mut index = 0usize;
    while index < channels.len() {
        if channels.len() - index >= 2 {
            let pair: [&[i32]; 2] = [&channels[index], &channels[index + 1]];
            let chan_bits = bit_depth + 1;
            if escaped {
                write_escaped_element(
                    &mut writer,
                    TAG_CPE,
                    &pair,
                    bit_depth,
                    partial_frame,
                    num_samples,
                )?;
            } else {
                write_compressed_element(
                    &mut writer,
                    TAG_CPE,
                    &pair,
                    chan_bits,
                    partial_frame,
                    num_samples,
                )?;
            }
            index += 2;
        } else {
            let single: [&[i32]; 1] = [&channels[index]];
            if escaped {
                write_escaped_element(
                    &mut writer,
                    TAG_SCE,
                    &single,
                    bit_depth,
                    partial_frame,
                    num_samples,
                )?;
            } else {
                write_compressed_element(
                    &mut writer,
                    TAG_SCE,
                    &single,
                    bit_depth,
                    partial_frame,
                    num_samples,
                )?;
            }
            index += 1;
        }
    }
    writer.write_bits(TAG_END, 3)?;
    Ok(writer.finish())
}

/// Builds the shorter of the Rice-coded and escaped frames.
fn encode_frame(channels: &[Vec<i32>], num_samples: usize, bit_depth: u32) -> Result<Vec<u8>> {
    let rice = build_frame(channels, num_samples, bit_depth, false)?;
    let escaped = build_frame(channels, num_samples, bit_depth, true)?;
    if escaped.len() < rice.len() {
        Ok(escaped)
    } else {
        Ok(rice)
    }
}

/// Converts one `f32` sample to an integer sample with the
/// scale `AlacDecodedFrame::to_audio_frame` inverts
/// (`sample * 2^(bits - 1)`), rounded to nearest and clamped
/// to the signed range. Integer-valued inputs round-trip
/// exactly; `f32::round` breaks ties away from zero.
fn f32_to_i32(sample: f32, bits_per_sample: u16) -> i32 {
    let shift = u32::from(bits_per_sample) - 1;
    let scale = (1u32 << shift) as f32;
    let max = (1i32 << shift) - 1;
    let min = -(1i32 << shift);
    let value = (sample.clamp(-1.0, 1.0) * scale).round() as i32;
    value.clamp(min, max)
}

/// Stateful ALAC encoder.
///
/// Each `encode` call produces one independently decodable
/// ALAC frame (one MP4 sample) of 1..=4096 sample frames; the
/// encoder keeps no state between calls. ALAC is an open
/// format, so no license is needed. Use `config()` /
/// `magic_cookie()` for the matching magic cookie (the
/// `ALACSpecificConfig` a muxer stores in the `alac` box).
pub struct AlacEncoder {
    sample_rate: u32,
    channels: u32,
    bits_per_sample: u16,
}

impl AlacEncoder {
    /// Creates an encoder for 16-bit or 24-bit ALAC.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the sample rate is zero
    /// or above 192,000, the channel count is zero or above 8,
    /// or the bit depth is not 16 or 24 (20/32-bit input is
    /// decode-only: 32-bit stereo cannot be coded at all, since
    /// a channel pair needs one bit more than the bit depth).
    pub fn new(sample_rate: u32, channels: u32, bits_per_sample: u16) -> Result<Self> {
        if sample_rate == 0 || sample_rate > MAX_SAMPLE_RATE {
            return Err(Error::InvalidArgument("alac sample rate out of range"));
        }
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(Error::InvalidArgument("alac channel count out of range"));
        }
        if !matches!(bits_per_sample, 16 | 24) {
            return Err(Error::InvalidArgument(
                "alac encoder supports 16-bit and 24-bit samples",
            ));
        }
        Ok(Self {
            sample_rate,
            channels,
            bits_per_sample,
        })
    }

    /// Sample rate in Hz.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Channel count.
    #[must_use]
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Bits per sample (16 or 24).
    #[must_use]
    pub fn bits_per_sample(&self) -> u16 {
        self.bits_per_sample
    }

    /// The magic cookie matching this encoder's output, as
    /// `parse_alac_config` reads it back.
    #[must_use]
    pub fn config(&self) -> AlacConfig {
        AlacConfig {
            frame_length: FRAME_LENGTH,
            compatible_version: 0,
            bit_depth: u32::from(self.bits_per_sample),
            pb: COOKIE_PB,
            mb: COOKIE_MB,
            kb: COOKIE_KB,
            channels: self.channels,
            max_run: 0,
            max_frame_bytes: 0,
            avg_bit_rate: 0,
            sample_rate: self.sample_rate,
        }
    }

    /// The 24-byte `ALACSpecificConfig` magic cookie for this
    /// encoder, in the layout `parse_alac_config` accepts.
    #[must_use]
    pub fn magic_cookie(&self) -> Vec<u8> {
        let config = self.config();
        let mut cookie = Vec::with_capacity(24);
        cookie.extend_from_slice(&config.frame_length.to_be_bytes());
        cookie.push(config.compatible_version);
        cookie.push(config.bit_depth as u8);
        cookie.push(config.pb as u8);
        cookie.push(config.mb as u8);
        cookie.push(config.kb as u8);
        cookie.push(config.channels as u8);
        cookie.extend_from_slice(&(config.max_run as u16).to_be_bytes());
        cookie.extend_from_slice(&config.max_frame_bytes.to_be_bytes());
        cookie.extend_from_slice(&config.avg_bit_rate.to_be_bytes());
        cookie.extend_from_slice(&config.sample_rate.to_be_bytes());
        cookie
    }

    /// Encodes one audio frame into an ALAC packet. The packet
    /// is independently decodable (`is_keyframe` is set) and
    /// carries the frame's timestamp.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the frame is not audio,
    /// its channel count or sample rate does not match the
    /// encoder, its sample count is not a whole number of
    /// frames, or it holds zero or more than 4096 sample frames.
    pub fn encode(&mut self, frame: &AudioFrame) -> Result<Packet> {
        if frame.channels != self.channels {
            return Err(Error::InvalidArgument(
                "frame channel count does not match encoder config",
            ));
        }
        if frame.sample_rate != self.sample_rate {
            return Err(Error::InvalidArgument(
                "frame sample rate does not match encoder config",
            ));
        }
        let channels = self.channels as usize;
        if frame.samples.len() % channels != 0 {
            return Err(Error::InvalidArgument(
                "frame sample count is not a whole number of frames",
            ));
        }
        let num_samples = frame.samples.len() / channels;
        if num_samples == 0 || num_samples > FRAME_LENGTH as usize {
            return Err(Error::InvalidArgument(
                "alac frame must hold 1 to 4096 sample frames",
            ));
        }
        let mut per_channel: Vec<Vec<i32>> = vec![Vec::with_capacity(num_samples); channels];
        for (index, &sample) in frame.samples.iter().enumerate() {
            per_channel[index % channels].push(f32_to_i32(sample, self.bits_per_sample));
        }
        let data = encode_frame(&per_channel, num_samples, u32::from(self.bits_per_sample))?;
        Ok(Packet {
            codec: Codec::Alac,
            data,
            timestamp: frame.timestamp,
            is_keyframe: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alac_predictor::{dyn_decompress, unpc_block, AgParams, AlacBitReader};

    #[test]
    fn zigzag_maps_signs() {
        assert_eq!(zigzag(0), 0);
        assert_eq!(zigzag(-1), 1);
        assert_eq!(zigzag(1), 2);
        assert_eq!(zigzag(-2), 3);
        assert_eq!(zigzag(32767), 65534);
        assert_eq!(zigzag(-32768), 65535);
    }

    #[test]
    fn predictor_forward_inverts_unpc_block() {
        // A sine-like sweep plus a step: the forward predictor
        // must produce residuals the decoder turns back into the
        // exact samples, including through coefficient adaptation.
        let samples: Vec<i32> = (0..512)
            .map(|i| {
                let phase = f64::from(i) * 0.11;
                (phase.sin() * 20000.0) as i32 + if i >= 256 { 5000 } else { 0 }
            })
            .collect();
        let residuals = predict_forward(&samples, 16);
        let out = unpc_block(&residuals, &[512], 1, 16, 9).unwrap();
        assert_eq!(out, samples);
    }

    #[test]
    fn predictor_forward_handles_constant_and_spikes() {
        let samples = vec![1000i32; 64];
        let residuals = predict_forward(&samples, 16);
        let out = unpc_block(&residuals, &[512], 1, 16, 9).unwrap();
        assert_eq!(out, samples);
        let samples: Vec<i32> = (0..64)
            .map(|i| if i % 2 == 0 { 32767 } else { -32768 })
            .collect();
        let residuals = predict_forward(&samples, 16);
        let out = unpc_block(&residuals, &[512], 1, 16, 9).unwrap();
        assert_eq!(out, samples);
    }

    #[test]
    fn rice_stream_round_trips_through_decoder() {
        // Silence, small values and full-scale spikes, so the
        // stream exercises the zero-run mode, the `k == 1` path
        // and the escape path in one buffer.
        let mut residuals: Vec<i32> = vec![0; 100];
        residuals[0] = 5;
        residuals[1] = -3;
        residuals[50] = 32767;
        residuals[51] = -32768;
        residuals[52] = 1;
        let mut writer = BitWriter::new();
        let mut coder = RiceCoder::new();
        encode_residuals(&mut writer, &residuals, 16, &mut coder).unwrap();
        let bytes = writer.finish();
        let mut padded = bytes.clone();
        padded.extend_from_slice(&[0u8; 8]);
        let mut reader = AlacBitReader::new(&padded, bytes.len());
        let params = AgParams::new(10, 40, 14).unwrap();
        let decoded = dyn_decompress(&mut reader, &params, residuals.len(), 16).unwrap();
        assert_eq!(decoded, residuals);
    }

    #[test]
    fn rice_stream_round_trips_24bit_stereo_width() {
        let residuals: Vec<i32> = (0..300)
            .map(|i| if i % 7 == 0 { 8_388_607 } else { (i % 11) - 5 })
            .collect();
        let mut writer = BitWriter::new();
        let mut coder = RiceCoder::new();
        encode_residuals(&mut writer, &residuals, 25, &mut coder).unwrap();
        let bytes = writer.finish();
        let mut padded = bytes.clone();
        padded.extend_from_slice(&[0u8; 8]);
        let mut reader = AlacBitReader::new(&padded, bytes.len());
        let params = AgParams::new(10, 40, 14).unwrap();
        let decoded = dyn_decompress(&mut reader, &params, residuals.len(), 25).unwrap();
        assert_eq!(decoded, residuals);
    }

    #[test]
    fn cookie_round_trips_through_parser() {
        let encoder = AlacEncoder::new(44_100, 2, 24).unwrap();
        let parsed = crate::alac_frame::parse_alac_config(&encoder.magic_cookie()).unwrap();
        assert_eq!(parsed, encoder.config());
    }

    #[test]
    fn invalid_encoder_arguments_are_rejected() {
        assert!(AlacEncoder::new(44_100, 2, 8).is_err());
        assert!(AlacEncoder::new(44_100, 2, 20).is_err());
        assert!(AlacEncoder::new(44_100, 2, 32).is_err());
        assert!(AlacEncoder::new(44_100, 0, 16).is_err());
        assert!(AlacEncoder::new(44_100, 9, 16).is_err());
        assert!(AlacEncoder::new(0, 2, 16).is_err());
        assert!(AlacEncoder::new(192_001, 2, 16).is_err());
    }
}
