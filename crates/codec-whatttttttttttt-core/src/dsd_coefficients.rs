//! FIR filter coefficients for DSD decimation and rational resampling.
//!
//! All taps are windowed-sinc low-pass designs: an ideal sinc
//! truncated by a Blackman window, then normalised so the taps sum
//! to exactly 1 (unit DC gain, so a constant +1 DSD stream decodes
//! to +1.0 PCM). The design follows the standard windowed-sinc
//! method described in Smith, "The Scientist and Engineer's Guide
//! to Digital Signal Processing", chapter 16 (windowed-sinc
//! filters), and Lyons, "Understanding Digital Signal Processing",
//! chapter 5 (FIR design by windowing). No tap value is tuned by
//! hand: every coefficient comes from the formula below, so the
//! filter can be regenerated and audited from this file alone.
//!
//! Why Blackman: a DSD bitstream carries most of its quantization
//! noise above 20 kHz, so the decimation filter's stopband matters
//! more than its transition sharpness. The Blackman window's
//! sidelobes sit near -58 dB, which keeps the shaped noise out of
//! the audio band without the passband ripple a sharper window
//! (or an equiripple design) would need a much longer filter to
//! match. The measured stopband numbers are in `tests/dsd.rs`,
//! not asserted from this comment.

/// Number of taps used per decimated output sample. The full
/// filter has `decimation * TAPS_PER_OUTPUT` taps; each output
/// sample therefore costs `TAPS_PER_OUTPUT` multiply-adds per
/// input sample, independent of the decimation factor. 32 was
/// chosen because 16 measured 2.4 dB of roll-off at 18 kHz for
/// 44.1 kHz output (see `tests/dsd.rs`); the ripple with 32 is
/// measured by that test.
pub const TAPS_PER_OUTPUT: usize = 32;

/// Taps used per output sample of the rational (polyphase)
/// resampler that carries the 44.1 kHz family to the 48 kHz
/// family. The resampler window spans this many input samples
/// on either side of the output position.
pub const RESAMPLE_HALF_WINDOW: usize = 16;

/// Blackman window at position `n` of a length-`len` window.
/// `n` runs from 0 to `len - 1`; the window is 0 at both ends,
/// which is what suppresses the sinc's sidelobes.
#[allow(
    clippy::cast_precision_loss,
    reason = "tap indices stay below 16384, exactly representable in f64"
)]
fn blackman(n: usize, len: usize) -> f64 {
    if len <= 1 {
        return 1.0;
    }
    let denom = (len - 1) as f64;
    let phase = 2.0 * std::f64::consts::PI * n as f64 / denom;
    0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos()
}

/// Windowed-sinc low-pass taps.
///
/// `num_taps` must be positive; `cutoff_hz` must be below half
/// of `sample_rate_hz`. The sinc is centred on the middle tap,
/// so the filter has linear phase with a group delay of
/// `(num_taps - 1) / 2` samples. The returned taps sum to 1.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "tap indices stay below 16384, exactly representable in f64; the f64-to-f32 step is the sample format's own precision"
)]
#[must_use]
pub fn windowed_sinc_taps(num_taps: usize, cutoff_hz: f64, sample_rate_hz: f64) -> Vec<f32> {
    debug_assert!(num_taps > 0);
    debug_assert!(cutoff_hz > 0.0 && cutoff_hz < sample_rate_hz / 2.0);
    let middle = (num_taps - 1) as f64 / 2.0;
    // The sinc's gain at DC is 2*fc/fs per tap before windowing.
    let gain = 2.0 * cutoff_hz / sample_rate_hz;
    let mut taps: Vec<f64> = Vec::with_capacity(num_taps);
    for n in 0..num_taps {
        let offset = n as f64 - middle;
        let sinc = if offset == 0.0 {
            1.0
        } else {
            let x = 2.0 * std::f64::consts::PI * cutoff_hz * offset / sample_rate_hz;
            x.sin() / x
        };
        taps.push(gain * sinc * blackman(n, num_taps));
    }
    // Normalise to unit DC gain: a constant input must pass
    // through unchanged, which is what a DSD stream of all-ones
    // (full-scale DC) needs to decode to +1.0.
    let sum: f64 = taps.iter().sum();
    if sum != 0.0 {
        for tap in &mut taps {
            *tap /= sum;
        }
    }
    taps.iter().map(|&tap| tap as f32).collect()
}

/// Taps for decimating a DSD stream by an integer factor.
///
/// `decimation` is the ratio of the DSD rate to the output rate
/// (64 for DSD64 to 44.1 kHz, 16 for DSD64 to 176.4 kHz).
/// The cutoff is 47% of the output rate: just below its Nyquist
/// frequency, so the audio band passes and the DSD noise above
/// the output Nyquist is rejected. The filter length is
/// `decimation * TAPS_PER_OUTPUT` taps.
#[must_use]
pub fn decimation_taps(dsd_rate_hz: u32, decimation: u32) -> Vec<f32> {
    let out_rate = f64::from(dsd_rate_hz) / f64::from(decimation);
    // 47% of the output rate: just below its Nyquist
    // frequency, so the audio band passes and the DSD noise above
    // the output Nyquist is rejected.
    let cutoff = 0.47 * out_rate;
    let num_taps = decimation as usize * TAPS_PER_OUTPUT;
    windowed_sinc_taps(num_taps, cutoff, f64::from(dsd_rate_hz))
}

/// Cutoff used by the rational resampler between two PCM rates:
/// 95% of the lower of the two Nyquist frequencies, so neither
/// side aliases into the other.
#[must_use]
pub fn resample_cutoff_hz(in_rate_hz: u32, out_rate_hz: u32) -> f64 {
    0.95 * (f64::from(in_rate_hz.min(out_rate_hz))) / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windowed_sinc_taps_sum_to_one() {
        let taps = windowed_sinc_taps(64, 20_000.0, 2_822_400.0);
        let sum: f32 = taps.iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "tap sum {sum}");
    }

    #[test]
    fn windowed_sinc_taps_are_symmetric() {
        let taps = windowed_sinc_taps(65, 20_000.0, 2_822_400.0);
        for i in 0..taps.len() / 2 {
            assert!((taps[i] - taps[taps.len() - 1 - i]).abs() < 1e-6);
        }
    }

    #[test]
    fn decimation_taps_length_is_decimation_times_thirty_two() {
        assert_eq!(decimation_taps(2_822_400, 64).len(), 64 * 32);
        assert_eq!(decimation_taps(2_822_400, 16).len(), 16 * 32);
        assert_eq!(decimation_taps(22_579_200, 32).len(), 32 * 32);
    }

    #[test]
    fn decimation_taps_sum_to_one_for_every_level() {
        for (rate, decimation) in [
            (2_822_400u32, 64u32),
            (5_644_800, 32),
            (11_289_600, 64),
            (22_579_200, 64),
        ] {
            let taps = decimation_taps(rate, decimation);
            let sum: f32 = taps.iter().sum();
            assert!((sum - 1.0).abs() < 1e-5, "rate {rate} tap sum {sum}");
        }
    }
}
