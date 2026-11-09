//! TPDF dither and first-order noise shaping for bit-depth reduction.
//!
//! Dropping low bits without dither makes the quantization error a
//! deterministic function of the signal: quiet passages gain harmonic
//! distortion and a constant low-level signal quantizes to a single
//! stuck value. Adding triangular-probability-density-function (TPDF)
//! noise before quantization decorrelates the error from the signal,
//! trading distortion for a constant, benign white noise floor.
//! First-order error feedback additionally pushes the remaining error
//! energy toward higher frequencies, where it is less audible.
//!
//! The only noise source is a small xorshift generator written here
//! instead of pulling in a `rand` crate: the workspace has no random
//! dependency, dither needs nothing more than a uniform source, and a
//! seeded generator makes every conversion exactly reproducible for
//! tests. It is a statistical noise source, not a cryptographic one.

/// Options for [`quantize_i24_to_i16`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DitherOptions {
    /// Add TPDF noise before quantization.
    pub dither: bool,
    /// Feed the previous quantization error back into the next sample.
    pub noise_shaping: bool,
    /// Seed for the noise generator; equal seeds give equal output.
    pub seed: u64,
}

impl Default for DitherOptions {
    fn default() -> Self {
        Self {
            dither: true,
            noise_shaping: false,
            seed: 0x9E37_79B9_7F4A_7C15,
        }
    }
}

/// One output LSB, in 24-bit input units: 16-bit output keeps the top
/// 16 of the 24 input bits.
const INPUT_UNITS_PER_OUTPUT_LSB: f64 = 256.0;
/// Largest magnitude a 24-bit sample can hold.
const I24_MAX: i64 = 8_388_607;
/// Smallest magnitude a 24-bit sample can hold.
const I24_MIN: i64 = -8_388_608;

/// A 64-bit xorshift generator: enough uniformity for dither noise,
/// small enough to live in the core without a dependency.
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        // A zero state never changes under xorshift, so a zero seed is
        // replaced with a fixed non-zero constant instead of stalling.
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// A uniform value in [0, 1).
    #[allow(
        clippy::cast_precision_loss,
        reason = "the top 53 bits of a u64 are exactly representable in f64"
    )]
    fn next_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / 9_007_199_254_740_992.0
    }
}

/// TPDF noise in units of one output LSB: the sum of two independent
/// uniforms in [0, 1), shifted to [-1, 1), has a triangular density,
/// zero mean, and an amplitude of exactly one output LSB.
fn tpdf_noise_lsb(rng: &mut XorShift64) -> f64 {
    rng.next_unit() + rng.next_unit() - 1.0
}

/// Quantizes 24-bit integer samples to 16-bit.
///
/// Each input is a signed 24-bit value in [-8388608, 8388607]; values
/// outside that range saturate to it. With `options.dither` the sample
/// gets TPDF noise of one output LSB before rounding; without it the
/// sample is rounded to the nearest output step. With
/// `options.noise_shaping` the previous sample's quantization error
/// is subtracted from the next sample before quantization, which is
/// first-order error feedback: the error is measured against the
/// adjusted input before dither, so the dither noise itself is not
/// fed back and stays white. The output error is computed from the
/// clamped output value, so saturation is reflected in the feedback
/// instead of accumulating silently.
///
/// The conversion is deterministic for a given input and seed: the
/// same seed always produces the same output, a different seed a
/// different noise sequence.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the rounded value is clamped to the i16 range before the cast"
)]
#[allow(
    clippy::cast_precision_loss,
    reason = "24-bit integers are exactly representable in f64"
)]
#[must_use]
pub fn quantize_i24_to_i16(samples: &[i32], options: &DitherOptions) -> Vec<i16> {
    let mut rng = XorShift64::new(options.seed);
    let mut previous_error: f64 = 0.0;
    let mut out = Vec::with_capacity(samples.len());
    for &sample in samples {
        let clamped = i64::from(sample).clamp(I24_MIN, I24_MAX) as f64;
        let adjusted = if options.noise_shaping {
            clamped - previous_error
        } else {
            clamped
        };
        let dithered = if options.dither {
            adjusted + tpdf_noise_lsb(&mut rng) * INPUT_UNITS_PER_OUTPUT_LSB
        } else {
            adjusted
        };
        let quantized = (dithered / INPUT_UNITS_PER_OUTPUT_LSB).round();
        let quantized = quantized.clamp(f64::from(i16::MIN), f64::from(i16::MAX));
        let value = quantized as i16;
        if options.noise_shaping {
            previous_error = quantized * INPUT_UNITS_PER_OUTPUT_LSB - adjusted;
        }
        out.push(value);
    }
    out
}

/// Quantizes f32 samples in [-1.0, 1.0] to 16-bit, via the 24-bit path.
///
/// The f32 input is first rounded to the nearest 24-bit integer, so
/// callers that still hold the original 24-bit samples should prefer
/// [`quantize_i24_to_i16`]: dithering from the original source keeps
/// the noise white, while dithering after this pre-rounding cannot
/// recover the bits already lost. Values outside [-1.0, 1.0] saturate.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the scaled value is clamped to the 24-bit range before the cast"
)]
#[must_use]
pub fn quantize_f32_to_i16(samples: &[f32], options: &DitherOptions) -> Vec<i16> {
    let as_i24: Vec<i32> = samples
        .iter()
        .map(|&sample| {
            let scaled = (sample.clamp(-1.0, 1.0) * 8_388_608.0).round();
            let clamped = scaled.clamp(-8_388_608.0, 8_388_607.0);
            clamped as i32
        })
        .collect();
    quantize_i24_to_i16(&as_i24, options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xorshift_is_deterministic_for_a_seed() {
        let mut first = XorShift64::new(42);
        let mut second = XorShift64::new(42);
        for _ in 0..16 {
            assert_eq!(first.next_u64(), second.next_u64());
        }
    }

    #[test]
    fn zero_seed_still_produces_noise() {
        let mut rng = XorShift64::new(0);
        assert_ne!(rng.next_u64(), 0);
    }

    #[test]
    fn tpdf_noise_stays_within_one_lsb() {
        let mut rng = XorShift64::new(7);
        for _ in 0..1000 {
            let noise = tpdf_noise_lsb(&mut rng);
            assert!((-1.0..1.0).contains(&noise));
        }
    }

    #[test]
    fn no_dither_rounds_a_half_step_up() {
        let options = DitherOptions {
            dither: false,
            noise_shaping: false,
            seed: 1,
        };
        assert_eq!(quantize_i24_to_i16(&[128], &options), vec![1]);
        assert_eq!(quantize_i24_to_i16(&[-128], &options), vec![-1]);
    }
}
