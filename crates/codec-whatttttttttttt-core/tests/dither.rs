//! Integration tests for TPDF dither, noise shaping, and the 32-bit PCM path.

use codec_whatttttttttttt_core::{
    decode_i32_samples, encode_i32_samples, parse_wav_header, write_wav_header, DitherOptions,
    PcmFormat,
};
use codec_whatttttttttttt_core::{
    pcm::{decode_samples, encode_samples},
    quantize_f32_to_i16, quantize_i24_to_i16,
};

/// 128 input units is exactly half of one 16-bit output step, so an
/// undithered quantizer sticks on one value while a dithered one must
/// visit both neighbouring values.
const HALF_STEP_INPUT: i32 = 128;

fn no_dither() -> DitherOptions {
    DitherOptions {
        dither: false,
        noise_shaping: false,
        seed: 1,
    }
}

fn dithered(seed: u64) -> DitherOptions {
    DitherOptions {
        dither: true,
        noise_shaping: false,
        seed,
    }
}

#[test]
fn undithered_half_step_signal_sticks_on_one_value() {
    let input = vec![HALF_STEP_INPUT; 4096];
    let out = quantize_i24_to_i16(&input, &no_dither());
    assert!(out.iter().all(|&v| v == 1));
}

#[test]
fn dithered_half_step_signal_visits_both_neighbours() {
    let input = vec![HALF_STEP_INPUT; 4096];
    let out = quantize_i24_to_i16(&input, &dithered(1));
    assert!(out.contains(&0));
    assert!(out.contains(&1));
    assert!(out.iter().all(|&v| v == 0 || v == 1));
    // The dithered mean tracks the true value: half a step up.
    let count = u32::try_from(out.len()).unwrap_or(u32::MAX);
    let mean = out.iter().map(|&v| f64::from(v)).sum::<f64>() / f64::from(count);
    assert!((mean - 0.5).abs() < 0.05);
}

#[test]
fn same_seed_gives_same_dithered_output() {
    let input: Vec<i32> = (0..2048).map(|i| (i % 512 - 256) * 256).collect();
    let first = quantize_i24_to_i16(&input, &dithered(42));
    let second = quantize_i24_to_i16(&input, &dithered(42));
    assert_eq!(first, second);
}

#[test]
fn different_seeds_give_different_dithered_output() {
    let input: Vec<i32> = (0..2048).map(|i| (i % 512 - 256) * 256).collect();
    let first = quantize_i24_to_i16(&input, &dithered(42));
    let second = quantize_i24_to_i16(&input, &dithered(43));
    assert_ne!(first, second);
}

#[test]
fn noise_shaping_changes_the_output() {
    let input: Vec<i32> = (0..2048).map(|i| (i % 512 - 256) * 256).collect();
    let plain = quantize_i24_to_i16(&input, &dithered(7));
    let shaped = quantize_i24_to_i16(
        &input,
        &DitherOptions {
            dither: true,
            noise_shaping: true,
            seed: 7,
        },
    );
    assert_ne!(plain, shaped);
}

#[test]
fn noise_shaping_without_dither_alternates_on_a_half_step() {
    // Error feedback alone spreads the half-step error over time:
    // 1, 0, 1, 0 averages to the true half-step value.
    let input = vec![HALF_STEP_INPUT; 8];
    let out = quantize_i24_to_i16(
        &input,
        &DitherOptions {
            dither: false,
            noise_shaping: true,
            seed: 1,
        },
    );
    assert_eq!(out, vec![1, 0, 1, 0, 1, 0, 1, 0]);
}

#[test]
fn undithered_quantization_rounds_to_nearest_and_saturates() {
    let input = vec![0, 128, -128, 256, 8_388_607, -8_388_608, 127, -127];
    assert_eq!(
        quantize_i24_to_i16(&input, &no_dither()),
        vec![0, 1, -1, 1, 32_767, -32_768, 0, 0]
    );
}

#[test]
fn f32_quantization_is_deterministic_for_a_seed() {
    let input = vec![0.0, 0.25, -0.5, 0.999_999];
    let first = quantize_f32_to_i16(&input, &dithered(99));
    let second = quantize_f32_to_i16(&input, &dithered(99));
    assert_eq!(first, second);
}

#[test]
fn i32_samples_round_trip_bit_exact() {
    let original = vec![0, 1, -1, 123_456_789, -987_654_321, i32::MIN, i32::MAX];
    let bytes = encode_i32_samples(&original, 1).unwrap();
    assert_eq!(decode_i32_samples(&bytes, 1).unwrap(), original);
}

#[test]
fn i32_f32_path_round_trips_representable_values() {
    // Multiples of 256 fit in the 24-bit f32 significand exactly.
    let original = vec![0.0, 0.5, -0.25, 0.75];
    let bytes = encode_samples(&original, PcmFormat::I32, 1).unwrap();
    let decoded = decode_samples(&bytes, PcmFormat::I32, 1).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn i32_wav_header_round_trips() {
    let samples = vec![0.0, 0.5, -0.5, 0.25];
    let bytes = encode_samples(&samples, PcmFormat::I32, 1).unwrap();
    let mut wav = Vec::new();
    write_wav_header(
        &mut wav,
        PcmFormat::I32,
        1,
        44_100,
        u32::try_from(bytes.len()).unwrap(),
    )
    .unwrap();
    wav.extend_from_slice(&bytes);
    let header = parse_wav_header(&wav).unwrap();
    assert_eq!(header.format, PcmFormat::I32);
    assert_eq!(header.bits_per_sample, 32);
    assert_eq!(header.sample_rate, 44_100);
    let decoded = decode_samples(&wav[header.data_offset..], header.format, 1).unwrap();
    assert_eq!(decoded, samples);
}

#[test]
fn f32_wav_still_round_trips() {
    let samples = vec![0.0, 0.5, -0.5, 1.0, -1.0];
    let bytes = encode_samples(&samples, PcmFormat::F32, 1).unwrap();
    let decoded = decode_samples(&bytes, PcmFormat::F32, 1).unwrap();
    assert_eq!(decoded, samples);
}
