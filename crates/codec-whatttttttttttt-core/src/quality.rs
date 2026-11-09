//! Quality labels for lossless audio.
//!
//! "Lossless" and "Hi-Res Lossless" here are commercial labels, not
//! formats: they describe what the actual parameters of a stream are
//! (lossless compression, bit depth, sample rate), never which codec
//! produced it. Classification takes those parameters as plain values,
//! so no codec name is consulted or hard-coded.
//!
//! The boundaries this module applies:
//! - Lossless: lossless compression, at most 16-bit / 48 kHz.
//! - Hi-Res Lossless: lossless compression, above 16-bit / 48 kHz,
//!   at most 24-bit / 192 kHz.
//! - Anything else (lossy compression, or beyond 24-bit / 192 kHz)
//!   is not classified and yields `None`.

/// A quality label assigned by [`classify_quality`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QualityLabel {
    /// Lossless compression, at most 16-bit / 48 kHz.
    Lossless,
    /// Lossless compression, above 16-bit / 48 kHz, at most 24-bit / 192 kHz.
    HiResLossless,
}

/// The actual parameters of a stream, as measured, not as claimed by
/// a codec name or a file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QualityParams {
    /// True when the stream uses lossless compression.
    pub lossless: bool,
    /// Bits per sample of the decoded audio.
    pub bits_per_sample: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
}

/// Highest sample rate, in Hz, still labelled `Lossless`.
pub const LOSSLESS_MAX_SAMPLE_RATE: u32 = 48_000;
/// Highest bit depth still labelled `Lossless`.
pub const LOSSLESS_MAX_BITS_PER_SAMPLE: u32 = 16;
/// Highest sample rate, in Hz, still labelled `Hi-Res Lossless`.
pub const HI_RES_MAX_SAMPLE_RATE: u32 = 192_000;
/// Highest bit depth still labelled `Hi-Res Lossless`.
pub const HI_RES_MAX_BITS_PER_SAMPLE: u32 = 24;

/// Classifies a stream from its actual parameters.
///
/// Returns `None` for lossy streams and for lossless streams beyond
/// 24-bit / 192 kHz, which neither label covers. A zero bit depth or
/// a zero sample rate is not a real stream and also yields `None`.
#[must_use]
pub fn classify_quality(params: QualityParams) -> Option<QualityLabel> {
    if !params.lossless || params.bits_per_sample == 0 || params.sample_rate == 0 {
        return None;
    }
    if params.bits_per_sample <= LOSSLESS_MAX_BITS_PER_SAMPLE
        && params.sample_rate <= LOSSLESS_MAX_SAMPLE_RATE
    {
        return Some(QualityLabel::Lossless);
    }
    if params.bits_per_sample <= HI_RES_MAX_BITS_PER_SAMPLE
        && params.sample_rate <= HI_RES_MAX_SAMPLE_RATE
    {
        return Some(QualityLabel::HiResLossless);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(lossless: bool, bits: u32, rate: u32) -> QualityParams {
        QualityParams {
            lossless,
            bits_per_sample: bits,
            sample_rate: rate,
        }
    }

    #[test]
    fn sixteen_bit_48k_lossless_is_lossless() {
        assert_eq!(
            classify_quality(params(true, 16, 48_000)),
            Some(QualityLabel::Lossless)
        );
    }

    #[test]
    fn twenty_four_bit_96k_lossless_is_hi_res() {
        assert_eq!(
            classify_quality(params(true, 24, 96_000)),
            Some(QualityLabel::HiResLossless)
        );
    }

    #[test]
    fn lossy_is_unclassified() {
        assert_eq!(classify_quality(params(false, 16, 44_100)), None);
    }

    #[test]
    fn thirty_two_bit_is_unclassified() {
        assert_eq!(classify_quality(params(true, 32, 192_000)), None);
    }
}
