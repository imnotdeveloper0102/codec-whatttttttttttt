//! Integration tests for quality-label classification.

use codec_whatttttttttttt_core::{classify_quality, QualityLabel, QualityParams};

fn params(lossless: bool, bits: u32, rate: u32) -> QualityParams {
    QualityParams {
        lossless,
        bits_per_sample: bits,
        sample_rate: rate,
    }
}

#[test]
fn sixteen_bit_44100_lossless_is_lossless() {
    assert_eq!(
        classify_quality(params(true, 16, 44_100)),
        Some(QualityLabel::Lossless)
    );
}

#[test]
fn twenty_four_bit_96k_lossless_is_hi_res_lossless() {
    assert_eq!(
        classify_quality(params(true, 24, 96_000)),
        Some(QualityLabel::HiResLossless)
    );
}

#[test]
fn twenty_four_bit_192k_lossless_is_hi_res_lossless() {
    assert_eq!(
        classify_quality(params(true, 24, 192_000)),
        Some(QualityLabel::HiResLossless)
    );
}

#[test]
fn lossy_stream_is_not_classified() {
    assert_eq!(classify_quality(params(false, 16, 44_100)), None);
    assert_eq!(classify_quality(params(false, 24, 192_000)), None);
}

#[test]
fn thirty_two_bit_lossless_is_not_classified() {
    assert_eq!(classify_quality(params(true, 32, 48_000)), None);
    assert_eq!(classify_quality(params(true, 32, 192_000)), None);
}

#[test]
fn beyond_24_bit_192k_is_not_classified() {
    assert_eq!(classify_quality(params(true, 24, 384_000)), None);
    assert_eq!(classify_quality(params(true, 24, 192_001)), None);
}

#[test]
fn sixteen_bit_48k_boundary_is_lossless() {
    assert_eq!(
        classify_quality(params(true, 16, 48_000)),
        Some(QualityLabel::Lossless)
    );
}

#[test]
fn sixteen_bit_48_1k_is_hi_res_lossless() {
    assert_eq!(
        classify_quality(params(true, 16, 48_100)),
        Some(QualityLabel::HiResLossless)
    );
}

#[test]
fn twenty_four_bit_48k_is_hi_res_lossless() {
    assert_eq!(
        classify_quality(params(true, 24, 48_000)),
        Some(QualityLabel::HiResLossless)
    );
}

#[test]
fn zero_bit_depth_or_sample_rate_is_not_classified() {
    assert_eq!(classify_quality(params(true, 0, 44_100)), None);
    assert_eq!(classify_quality(params(true, 16, 0)), None);
}
