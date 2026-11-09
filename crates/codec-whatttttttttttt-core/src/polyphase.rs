//! Windowed-sinc polyphase resampler.
//!
//! The linear interpolator in `resample.rs` is kept for callers
//! that accept interpolation error. This module is the quality
//! path: a lowpass FIR designed as a windowed sinc, applied as a
//! polyphase filterbank so the cost per output sample is one tap
//! per phase instead of the whole prototype.
//!
//! # Filter design (why these numbers)
//!
//! The prototype is a windowed sinc (Smith, "The Scientist and
//! Engineer's Guide to Digital Signal Processing", ch. 16;
//! Oppenheim & Schafer, "Discrete-Time Signal Processing", ch. 7:
//! windowed-sinc FIR design, Kaiser window). For a rate change
//! `source -> target`, write the reduced ratio `target/source =
//! L/M` with `L` and `M` coprime. Conceptually the signal is
//! upsampled by `L` (zero-insertion), lowpassed, and downsampled
//! by `M`. The prototype cutoff is `0.5 * min(1, L/M) / L`
//! cycles/sample at the upsampled rate, i.e. half the lower of
//! the two Nyquist frequencies, so an upsample passes the whole
//! input band and a downsample rejects everything that would
//! alias into the output band.
//!
//! - Window: Kaiser, stopband target A = 90 dB. Kaiser (1974)
//!   gives `beta = 0.1102 * (A - 8.7)` for A > 50 dB, here
//!   beta = 8.95926. A Kaiser window (rather than Blackman or
//!   Hann) is chosen because beta trades main-lobe width against
//!   sidelobe level with one parameter, and 90 dB keeps the
//!   first sidelobe below the 16-bit quantization floor
//!   (~96 dB) in practice while the transition stays narrow.
//! - Taps per phase: 128 minimum, raised so the prototype holds
//!   at least 2048 taps (`taps_per_phase = max(128,
//!   ceil(2048 / L))`). The transition width of a Kaiser FIR
//!   scales as `(A - 8) / (2.285 * 2 * pi * N)` cycles/sample
//!   for N taps, so the 2048-tap floor bounds the transition
//!   width even when `L` is small (e.g. integer downsampling,
//!   where L = 1 would otherwise leave a 128-tap filter with a
//!   transition tens of kHz wide). 128 taps/phase keeps the
//!   44.1<->48 kHz transition about 2 kHz wide, so a 20 kHz tone
//!   sits in the flat passband and a 23 kHz tone at 48->44.1 kHz
//!   sits in the stopband instead of mid-transition. The price
//!   is compute: one multiply-accumulate per tap per output
//!   sample per channel.
//! - Gain: the prototype is scaled numerically so its tap sum
//!   equals `L`, compensating the 1/L gain loss of zero-insertion
//!   upsampling. DC therefore passes at unity gain (verified by
//!   the DC test) without relying on the analytic sinc sum.
//! - Edges: samples outside the input are treated as zero. The
//!   first and last outputs carry a transient roughly
//!   `delay_in_input_frames` long; quality tests trim the edges
//!   instead of pretending the transient does not exist.
//!
//! # Bit depth
//!
//! The resampler works on the library's internal format,
//! interleaved f32 in [-1.0, 1.0]. It is generic over source bit
//! depth only through `pcm`: decode I16/I24/I32/F32 to f32, call
//! this resampler, encode back. The f32 pipeline rounds 32-bit
//! integers to 24 significant bits (see `pcm.rs`), so a caller
//! needing bit-exact 32-bit integer output must not route i32
//! through f32 at all — that limitation is in `pcm`, not here,
//! and is not hidden by this module.

use crate::error::{Error, Result};
use crate::frame::AudioFrame;
use crate::limits::{MAX_ALLOCATION_BYTES, MAX_CHANNELS, MAX_SAMPLE_RATE};

/// Kaiser stopband target in dB; see the module docs.
const KAISER_ATTENUATION_DB: f64 = 90.0;
/// Kaiser beta for A > 50 dB: `0.1102 * (A - 8.7)`.
const KAISER_BETA: f64 = 0.1102 * (KAISER_ATTENUATION_DB - 8.7);
/// Minimum taps per polyphase phase.
const MIN_TAPS_PER_PHASE: usize = 128;
/// Minimum prototype length in taps (bounds transition width).
const MIN_PROTOTYPE_TAPS: usize = 2048;

/// Greatest common divisor (Euclid); both inputs are positive.
fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let rest = a % b;
        a = b;
        b = rest;
    }
    a
}

/// `sin(pi * x) / (pi * x)`, with `sinc(0) = 1`.
fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }
    let pix = std::f64::consts::PI * x;
    pix.sin() / pix
}

/// Modified Bessel function I0 by its power series
/// `I0(x) = sum (x^2/4)^k / (k!)^2`, summed until the term is
/// below 1e-15 of the partial sum (fewer than 30 terms for the
/// beta used here). No external dependency is pulled in for one
/// special function.
fn bessel_i0(x: f64) -> f64 {
    let y = x * x / 4.0;
    let mut sum = 1.0;
    let mut term = 1.0;
    for k in 1..50 {
        let kf = f64::from(k);
        term *= y / (kf * kf);
        sum += term;
        if term < 1e-15 * sum {
            break;
        }
    }
    sum
}

/// Kaiser window value at tap `n` of a length-`len` window.
#[allow(
    clippy::cast_precision_loss,
    reason = "window length is bounded by the allocation check (at most 16M taps), far below 2^53, so the usize-to-f64 cast is exact"
)]
fn kaiser_window(n: usize, len: usize) -> f64 {
    let center = (len - 1) as f64 / 2.0;
    if center == 0.0 {
        return 1.0;
    }
    let ratio = (n as f64 - center) / center;
    let arg = (1.0 - ratio * ratio).max(0.0).sqrt();
    bessel_i0(KAISER_BETA * arg) / bessel_i0(KAISER_BETA)
}

/// Designs the windowed-sinc prototype of `len` taps for the
/// reduced ratio `up/down`, normalized so the tap sum is `up`.
#[allow(
    clippy::cast_precision_loss,
    reason = "prototype length is bounded by the allocation check (at most 16M taps), far below 2^53, so the usize-to-f64 casts are exact"
)]
#[allow(
    clippy::cast_possible_truncation,
    reason = "the f64 prototype is stored as f32 taps: the truncation is the sample format's own precision, and DC gain is renormalized numerically afterwards only in f64"
)]
fn design_prototype(len: usize, up: u32, down: u32) -> Vec<f32> {
    let center = (len - 1) as f64 / 2.0;
    // Cutoff in cycles/sample at the upsampled rate: half the
    // lower Nyquist frequency, expressed at rate `up * source`.
    let ratio = f64::from(up) / f64::from(down);
    let cutoff = 0.5 * ratio.min(1.0) / f64::from(up);
    let mut taps: Vec<f64> = (0..len)
        .map(|n| {
            let distance = n as f64 - center;
            2.0 * cutoff * sinc(2.0 * cutoff * distance) * kaiser_window(n, len)
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    // The sinc sum is never zero for a lowpass with a positive
    // cutoff; the guard keeps a degenerate (zero-length or
    // all-zero) design from dividing by zero instead of panicking.
    if sum != 0.0 {
        let scale = f64::from(up) / sum;
        for tap in &mut taps {
            *tap *= scale;
        }
    }
    taps.iter().map(|&tap| tap as f32).collect()
}

/// Windowed-sinc polyphase resampler for one fixed rate pair.
///
/// Build once with `new`, then call `process` per block. The
/// resampler is stateless across calls: every call treats its
/// input as a whole signal with zero padding outside it, so
/// block boundaries carry the same edge transient as the signal
/// edges. Callers that split a stream into blocks and need
/// continuity must resample the whole stream in one call (or
/// keep the overlap themselves); this is documented rather than
/// hidden behind a stateful API that would silently seam.
pub struct PolyphaseResampler {
    source_rate: u32,
    target_rate: u32,
    channels: u32,
    /// Upsample factor L (reduced ratio).
    up: u32,
    /// Downsample factor M (reduced ratio).
    down: u32,
    taps_per_phase: usize,
    /// Windowed-sinc prototype, `up * taps_per_phase` taps.
    taps: Vec<f32>,
}

impl PolyphaseResampler {
    /// Builds a resampler for `source_rate -> target_rate`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a zero rate or channel count,
    /// `Overflow` for a rate above `MAX_SAMPLE_RATE`, and
    /// `AllocationLimit` when the prototype would exceed the
    /// per-call allocation limit (only for pathological rate
    /// pairs with a huge reduced upsample factor).
    pub fn new(source_rate: u32, target_rate: u32, channels: u32) -> Result<Self> {
        if source_rate == 0 || target_rate == 0 {
            return Err(Error::InvalidArgument(
                "polyphase sample rates must be positive",
            ));
        }
        if source_rate > MAX_SAMPLE_RATE || target_rate > MAX_SAMPLE_RATE {
            return Err(Error::Overflow("polyphase sample rate exceeds maximum"));
        }
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(Error::InvalidArgument(
                "polyphase channel count must be between 1 and 8",
            ));
        }
        let divisor = gcd(source_rate, target_rate);
        // Reduced ratio target/source = up/down.
        let up = target_rate / divisor;
        let down = source_rate / divisor;
        let taps_per_phase = MIN_TAPS_PER_PHASE.max(MIN_PROTOTYPE_TAPS.div_ceil(up as usize));
        let prototype_len = (up as usize).saturating_mul(taps_per_phase);
        if prototype_len.saturating_mul(4) > MAX_ALLOCATION_BYTES {
            return Err(Error::AllocationLimit {
                requested: prototype_len.saturating_mul(4),
                limit: MAX_ALLOCATION_BYTES,
            });
        }
        let taps = design_prototype(prototype_len, up, down);
        Ok(Self {
            source_rate,
            target_rate,
            channels,
            up,
            down,
            taps_per_phase,
            taps,
        })
    }

    /// Source sample rate in Hz.
    #[must_use]
    pub fn source_rate(&self) -> u32 {
        self.source_rate
    }

    /// Target sample rate in Hz.
    #[must_use]
    pub fn target_rate(&self) -> u32 {
        self.target_rate
    }

    /// Upsample factor L of the reduced ratio.
    #[must_use]
    pub fn upsample_factor(&self) -> u32 {
        self.up
    }

    /// Downsample factor M of the reduced ratio.
    #[must_use]
    pub fn downsample_factor(&self) -> u32 {
        self.down
    }

    /// Taps per polyphase phase (one multiply per output sample
    /// per channel per tap).
    #[must_use]
    pub fn taps_per_phase(&self) -> usize {
        self.taps_per_phase
    }

    /// Prototype length in taps (`up * taps_per_phase`).
    #[must_use]
    pub fn prototype_len(&self) -> usize {
        self.taps.len()
    }

    /// Filter group delay in input-sample units: the centre of
    /// the prototype sits this far into the signal. This is NOT
    /// the edge-transient length: the transient lasts the whole
    /// prototype (see `edge_transient_in_output_frames`), about
    /// twice the group delay, because the output is only free of
    /// zero padding once the full prototype window lies inside
    /// the input.
    #[allow(
        clippy::cast_precision_loss,
        reason = "prototype length is bounded by the allocation check (at most 16M taps), far below 2^53"
    )]
    #[must_use]
    pub fn delay_in_input_frames(&self) -> f64 {
        (self.taps.len().saturating_sub(1)) as f64 / 2.0 / f64::from(self.up)
    }

    /// Edge-transient length in output frames: the first and last
    /// this many outputs are still affected by the zero padding
    /// outside the input. The output at index `m` samples the
    /// prototype at position `m * down`, so the window is fully
    /// inside the input only once `m * down >= prototype_len -
    /// 1`; the ringing from the step at the edge decays to the
    /// passband ripple only after that point. Callers that need
    /// steady-state samples must trim at least this many frames
    /// from each end (plus a small margin).
    #[must_use]
    pub fn edge_transient_in_output_frames(&self) -> usize {
        self.taps
            .len()
            .saturating_sub(1)
            .div_ceil(self.down as usize)
    }

    /// Resamples interleaved f32 samples. The input length must
    /// be a whole number of frames for `channels`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the input is not a whole
    /// number of frames, and `AllocationLimit` when the output
    /// would exceed the per-call allocation limit.
    #[allow(
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "frame counts and the prototype length are bounded by the allocation checks (at most 16M), so the i64/usize/f32 casts cannot wrap, go negative, or lose more than the sample format's own precision; n stays in [0, in_frames) by the loop bounds"
    )]
    pub fn process(&self, input: &[f32]) -> Result<Vec<f32>> {
        let channels = self.channels as usize;
        if input.len() % channels != 0 {
            return Err(Error::InvalidArgument(
                "polyphase input is not a whole number of frames",
            ));
        }
        let in_frames = input.len() / channels;
        if in_frames == 0 {
            return Ok(Vec::new());
        }
        // Same output-length rule as `resample`: floor division,
        // at least one frame, in 64-bit arithmetic.
        let out_frames_u64 =
            (in_frames as u64 * u64::from(self.target_rate)) / u64::from(self.source_rate);
        let out_frames = usize::try_from(out_frames_u64)
            .map_err(|_| Error::Overflow("resampled frame count exceeds usize"))
            .map(|frames| frames.max(1))?;
        let out_samples = out_frames
            .checked_mul(channels)
            .ok_or(Error::Overflow("resampled frame too large"))?;
        if out_samples * 4 > MAX_ALLOCATION_BYTES {
            return Err(Error::AllocationLimit {
                requested: out_samples * 4,
                limit: MAX_ALLOCATION_BYTES,
            });
        }
        let mut output = Vec::with_capacity(out_samples);
        let prototype_len = self.taps.len() as i64;
        let up = i64::from(self.up);
        let down = i64::from(self.down);
        // Direct polyphase sum: output m is the convolution of
        // the upsampled input with the prototype, sampled at
        // position `m * down` in the upsampled domain. Only taps
        // whose prototype index `m*down - n*up` lands in
        // `[0, prototype_len)` contribute; n is clamped to the
        // input range and anything outside is zero padding.
        for m in 0..out_frames {
            let position = m as i64 * down;
            let n_lo = ceil_div(position - (prototype_len - 1), up).max(0);
            let n_hi = position.div_euclid(up).min(in_frames as i64 - 1);
            for channel in 0..channels {
                let mut acc = 0.0f64;
                let mut n = n_lo;
                while n <= n_hi {
                    let tap_index = position - n * up;
                    // `tap_index` is in range by the n bounds.
                    let tap_index = usize::try_from(tap_index).unwrap_or(0);
                    let tap = self.taps.get(tap_index).copied().unwrap_or(0.0);
                    let sample = input.get((n as usize) * channels + channel).copied();
                    if let Some(sample) = sample {
                        acc += f64::from(tap) * f64::from(sample);
                    }
                    n += 1;
                }
                output.push(acc as f32);
            }
        }
        Ok(output)
    }
}

/// Ceiling division for a possibly-negative numerator and a
/// positive denominator, via Euclidean division.
fn ceil_div(numerator: i64, denominator: i64) -> i64 {
    let quotient = numerator.div_euclid(denominator);
    let remainder = numerator.rem_euclid(denominator);
    if remainder == 0 {
        quotient
    } else {
        quotient + 1
    }
}

/// Resamples an `AudioFrame` to `target_sample_rate` with the
/// polyphase resampler. Same-rate input is returned unchanged.
///
/// # Errors
///
/// Returns `InvalidArgument` for a zero rate or channel count,
/// `Overflow` for a target rate above `MAX_SAMPLE_RATE`, and
/// `AllocationLimit` when the output would exceed the per-call
/// limit.
pub fn resample_polyphase(input: &AudioFrame, target_sample_rate: u32) -> Result<AudioFrame> {
    if input.sample_rate == 0 {
        return Err(Error::InvalidArgument(
            "source sample rate must be positive",
        ));
    }
    if target_sample_rate == 0 {
        return Err(Error::InvalidArgument(
            "target sample rate must be positive",
        ));
    }
    if target_sample_rate > MAX_SAMPLE_RATE {
        return Err(Error::Overflow("target sample rate exceeds maximum"));
    }
    if input.channels == 0 {
        return Err(Error::InvalidArgument("channel count must be positive"));
    }
    if target_sample_rate == input.sample_rate {
        return Ok(input.clone());
    }
    let resampler = PolyphaseResampler::new(input.sample_rate, target_sample_rate, input.channels)?;
    let samples = resampler.process(&input.samples)?;
    Ok(AudioFrame {
        sample_rate: target_sample_rate,
        channels: input.channels,
        samples,
        timestamp: input.timestamp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduced_ratio_for_44100_to_48000() {
        let resampler = PolyphaseResampler::new(44100, 48000, 1).unwrap();
        assert_eq!(resampler.upsample_factor(), 160);
        assert_eq!(resampler.downsample_factor(), 147);
        assert_eq!(resampler.taps_per_phase(), 128);
        assert_eq!(resampler.prototype_len(), 160 * 128);
    }

    #[test]
    fn small_upsample_factor_gets_2048_tap_floor() {
        // 192 kHz -> 48 kHz is L=1, M=4: without the floor the
        // prototype would be 128 taps and the transition tens of
        // kHz wide; the floor raises it to 2048 taps.
        let resampler = PolyphaseResampler::new(192_000, 48_000, 1).unwrap();
        assert_eq!(resampler.upsample_factor(), 1);
        assert_eq!(resampler.taps_per_phase(), 2048);
        assert_eq!(resampler.prototype_len(), 2048);
    }

    #[test]
    fn dc_passes_at_unity_gain_after_edge_trim() {
        let resampler = PolyphaseResampler::new(44100, 48000, 1).unwrap();
        let input = vec![0.5f32; 4410];
        let out = resampler.process(&input).unwrap();
        // The old trim used the group delay
        // (`delay_in_input_frames`, ~64 input frames, applied as
        // output frames) and failed at 0.49949643: the zero-padding
        // edge transient lasts the whole prototype window in
        // output frames — `(prototype_len - 1) / down`, about 139
        // frames here, roughly twice the group delay — and its
        // Gibbs ringing stays above 1e-4 until then (measured:
        // worst deviation 0.0079 at output frame 78, within 4e-7
        // by frame 139). Trimming the full transient plus a
        // 10-frame margin is the correct steady-state window; the
        // 1e-4 threshold itself is unchanged.
        let trim = resampler.edge_transient_in_output_frames() + 10;
        let middle = &out[trim..out.len() - trim];
        for &sample in middle {
            assert!((sample - 0.5).abs() < 1e-4, "dc drifted: {sample}");
        }
    }

    #[test]
    fn upsampling_2x_doubles_frame_count() {
        let resampler = PolyphaseResampler::new(8000, 16000, 1).unwrap();
        let out = resampler.process(&vec![0.25f32; 100]).unwrap();
        assert_eq!(out.len(), 200);
    }

    #[test]
    fn zero_rate_is_rejected() {
        assert!(matches!(
            PolyphaseResampler::new(0, 48000, 1),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn bessel_i0_matches_known_values() {
        // I0(0) = 1; I0(1) = 1.266_065_877_752_008_4.
        assert!((bessel_i0(0.0) - 1.0).abs() < 1e-12);
        assert!((bessel_i0(1.0) - 1.266_065_877_752_008_4).abs() < 1e-12);
    }
}
