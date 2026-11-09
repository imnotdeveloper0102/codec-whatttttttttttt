//! ALAC encode tests: bit-exact round trips through the library's
//! own decoder, invalid-argument handling, and decode of the
//! checked-in `alacenc_*` fixtures (encoded by `AlacEncoder`,
//! muxed into `.m4a`, and verified against ffmpeg's decode — see
//! the task report for the ffmpeg commands and the measured
//! numbers).
//!
//! Bit-exactness is checked on integer samples: the `f32`
//! `AudioFrame` input is built from integer samples with the
//! exact scale (`i16 / 2^15`, `i24 / 2^23`), and the decoded
//! integer samples must equal the input, sample for sample.
//! ALAC is lossless, so any difference is an encoder bug.

#[path = "mp4_samples.rs"]
mod mp4_samples;

use codec_whatttttttttttt_core::{
    decode_alac_frame, parse_alac_config, AlacEncoder, AudioFrame, Codec, Decoder, Error,
};
use mp4_samples::{find_box, load_mp4, read_s16le_raw, read_s32le_raw};

fn fixture_path(name: &str) -> String {
    format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Builds an `AudioFrame` from interleaved integer samples. The
/// scale is exact in `f32` for 16-bit and 24-bit integers, so
/// the encoder's quantization returns the same integers.
// Samples are 16- or 24-bit, below 2^24, so the i32-to-f32 cast
// is exact.
#[allow(clippy::cast_precision_loss)]
fn audio_frame(samples: &[i32], bits: u16, channels: u32, sample_rate: u32) -> AudioFrame {
    let scale = 1.0f32 / 2f32.powi(i32::from(bits) - 1);
    AudioFrame {
        sample_rate,
        channels,
        samples: samples
            .iter()
            .map(|&sample| sample as f32 * scale)
            .collect(),
        timestamp: 0,
    }
}

/// Encodes interleaved integer samples in 4096-frame packets and
/// decodes every packet with the library decoder, returning the
/// concatenated integer samples.
fn encode_decode_round_trip(
    samples: &[i32],
    bits: u16,
    channels: u32,
    sample_rate: u32,
) -> Vec<i32> {
    let mut encoder = AlacEncoder::new(sample_rate, channels, bits).unwrap();
    let config = encoder.config();
    let channels_usize = channels as usize;
    let total_frames = samples.len() / channels_usize;
    assert_eq!(samples.len(), total_frames * channels_usize);
    let mut decoded = Vec::new();
    let mut offset = 0usize;
    while offset < total_frames {
        let take = (total_frames - offset).min(4096);
        let chunk = &samples[offset * channels_usize..(offset + take) * channels_usize];
        let frame = audio_frame(chunk, bits, channels, sample_rate);
        let packet = encoder.encode(&frame).unwrap();
        assert_eq!(packet.codec, Codec::Alac);
        assert!(packet.is_keyframe);
        let out = decode_alac_frame(&packet.data, &config).unwrap();
        assert_eq!(out.frame_count, take);
        assert_eq!(out.channels, channels);
        assert_eq!(out.bit_depth, u32::from(bits));
        decoded.extend_from_slice(&out.samples);
        offset += take;
    }
    decoded
}

/// Deterministic full-range pseudo-noise (xorshift32), so the
/// noise cases are reproducible without a fixture file.
fn noise_samples(frames: usize, channels: usize, bits: u16) -> Vec<i32> {
    let max = (1i64 << (u32::from(bits) - 1)) - 1;
    let mut state: u32 = 0x1234_5678;
    (0..frames * channels)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let value = (i64::from(state) % (2 * max + 1)) - max;
            i32::try_from(value).unwrap_or(0)
        })
        .collect()
}

/// A sine at `freq` Hz, amplitude 0.5 of full scale, per channel
/// (second channel at 1.5x the frequency when stereo).
// Frame counts stay far below 2^53, and the rounded sine stays
// within +/-2^22, so both casts are exact for the values here.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn sine_samples(frames: usize, channels: usize, bits: u16, sample_rate: u32) -> Vec<i32> {
    let amplitude = 2f64.powi(i32::from(bits) - 2);
    let mut samples = Vec::with_capacity(frames * channels);
    for frame in 0..frames {
        for channel in 0..channels {
            let freq = if channel == 0 { 440.0 } else { 660.0 };
            let phase = 2.0 * std::f64::consts::PI * freq * frame as f64 / f64::from(sample_rate);
            samples.push((phase.sin() * amplitude).round() as i32);
        }
    }
    samples
}

/// A linear ramp from full negative to full positive, per channel.
// Frame counts are a few thousand at most and the ramp stays
// within +/-2^23, so the usize-to-i64 casts cannot wrap and the
// i64-to-i32 cast cannot truncate.
#[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
fn ramp_samples(frames: usize, channels: usize, bits: u16) -> Vec<i32> {
    let max = (1i64 << (u32::from(bits) - 1)) - 1;
    let mut samples = Vec::with_capacity(frames * channels);
    for frame in 0..frames {
        let value = if frames <= 1 {
            0
        } else {
            (-max + (2 * max * frame as i64) / (frames as i64 - 1)) as i32
        };
        for _ in 0..channels {
            samples.push(value);
        }
    }
    samples
}

#[test]
fn silence_round_trips_bit_exact_16bit_mono() {
    let samples = vec![0i32; 5000];
    assert_eq!(encode_decode_round_trip(&samples, 16, 1, 8000), samples);
}

#[test]
fn silence_round_trips_bit_exact_24bit_stereo() {
    let samples = vec![0i32; 5000 * 2];
    assert_eq!(encode_decode_round_trip(&samples, 24, 2, 44_100), samples);
}

#[test]
fn sine_round_trips_bit_exact_16bit_mono() {
    let samples = sine_samples(5000, 1, 16, 8000);
    assert_eq!(encode_decode_round_trip(&samples, 16, 1, 8000), samples);
}

#[test]
fn sine_round_trips_bit_exact_16bit_stereo() {
    let samples = sine_samples(5000, 2, 16, 44_100);
    assert_eq!(encode_decode_round_trip(&samples, 16, 2, 44_100), samples);
}

#[test]
fn sine_round_trips_bit_exact_24bit_mono() {
    let samples = sine_samples(5000, 1, 24, 8000);
    assert_eq!(encode_decode_round_trip(&samples, 24, 1, 8000), samples);
}

#[test]
fn sine_round_trips_bit_exact_24bit_stereo() {
    let samples = sine_samples(5000, 2, 24, 44_100);
    assert_eq!(encode_decode_round_trip(&samples, 24, 2, 44_100), samples);
}

#[test]
fn noise_round_trips_bit_exact_16bit_mono() {
    let samples = noise_samples(4096, 1, 16);
    assert_eq!(encode_decode_round_trip(&samples, 16, 1, 8000), samples);
}

#[test]
fn noise_round_trips_bit_exact_16bit_stereo() {
    let samples = noise_samples(4096, 2, 16);
    assert_eq!(encode_decode_round_trip(&samples, 16, 2, 44_100), samples);
}

#[test]
fn noise_round_trips_bit_exact_24bit_mono() {
    let samples = noise_samples(4096, 1, 24);
    assert_eq!(encode_decode_round_trip(&samples, 24, 1, 8000), samples);
}

#[test]
fn noise_round_trips_bit_exact_24bit_stereo() {
    let samples = noise_samples(2048, 2, 24);
    assert_eq!(encode_decode_round_trip(&samples, 24, 2, 44_100), samples);
}

#[test]
fn ramp_round_trips_bit_exact_16bit_stereo() {
    let samples = ramp_samples(4096, 2, 16);
    assert_eq!(encode_decode_round_trip(&samples, 16, 2, 44_100), samples);
}

#[test]
fn ramp_round_trips_bit_exact_24bit_mono() {
    let samples = ramp_samples(4096, 1, 24);
    assert_eq!(encode_decode_round_trip(&samples, 24, 1, 8000), samples);
}

#[test]
fn single_sample_and_two_sample_frames_round_trip() {
    assert_eq!(encode_decode_round_trip(&[32767], 16, 1, 8000), vec![32767]);
    assert_eq!(
        encode_decode_round_trip(&[-32768, 32767], 16, 1, 8000),
        vec![-32768, 32767]
    );
    assert_eq!(
        encode_decode_round_trip(&[8_388_607, -8_388_608], 24, 1, 8000),
        vec![8_388_607, -8_388_608]
    );
}

#[test]
fn full_scale_extremes_round_trip() {
    let samples: Vec<i32> = (0..512)
        .map(|i| if i % 2 == 0 { 32767 } else { -32768 })
        .collect();
    assert_eq!(encode_decode_round_trip(&samples, 16, 1, 8000), samples);
    let samples: Vec<i32> = (0..512)
        .map(|i| if i % 2 == 0 { 8_388_607 } else { -8_388_608 })
        .collect();
    assert_eq!(encode_decode_round_trip(&samples, 24, 1, 8000), samples);
}

#[test]
fn exact_4096_frame_packet_round_trips() {
    let samples = sine_samples(4096, 2, 16, 44_100);
    assert_eq!(encode_decode_round_trip(&samples, 16, 2, 44_100), samples);
}

#[test]
fn partial_frame_round_trips() {
    let samples = sine_samples(100, 1, 16, 8000);
    assert_eq!(encode_decode_round_trip(&samples, 16, 1, 8000), samples);
}

#[test]
fn three_channels_round_trip() {
    let samples = sine_samples(1000, 3, 16, 44_100);
    assert_eq!(encode_decode_round_trip(&samples, 16, 3, 44_100), samples);
}

#[test]
#[allow(
    clippy::cast_possible_truncation,
    reason = "decoded samples lie in [-1, 1], so the scaled value fits i32"
)]
fn decoder_api_returns_matching_f32_frames() {
    let samples = sine_samples(1000, 2, 16, 44_100);
    let mut encoder = AlacEncoder::new(44_100, 2, 16).unwrap();
    let config = encoder.config();
    let frame = audio_frame(&samples, 16, 2, 44_100);
    let packet = encoder.encode(&frame).unwrap();
    let mut decoder = Decoder::from_alac_config(&config).unwrap();
    let output = decoder.decode(&packet).unwrap();
    let codec_whatttttttttttt_core::Frame::Audio(audio) = output else {
        panic!("alac decode returns an audio frame");
    };
    assert_eq!(audio.sample_rate, 44_100);
    assert_eq!(audio.channels, 2);
    for (index, &sample) in audio.samples.iter().enumerate() {
        assert_eq!((sample * 32768.0).round() as i32, samples[index]);
    }
}

#[test]
fn magic_cookie_parses_back_to_config() {
    for (sample_rate, channels, bits) in [(8000, 1, 16), (44_100, 2, 16), (48_000, 2, 24)] {
        let encoder = AlacEncoder::new(sample_rate, channels, bits).unwrap();
        let cookie = encoder.magic_cookie();
        assert_eq!(cookie.len(), 24);
        let parsed = parse_alac_config(&cookie).unwrap();
        assert_eq!(parsed, encoder.config());
    }
}

#[test]
fn invalid_encoder_construction_is_rejected() {
    assert!(matches!(
        AlacEncoder::new(44_100, 2, 8),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        AlacEncoder::new(44_100, 2, 20),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        AlacEncoder::new(44_100, 2, 32),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        AlacEncoder::new(44_100, 0, 16),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        AlacEncoder::new(44_100, 9, 16),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        AlacEncoder::new(0, 2, 16),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        AlacEncoder::new(192_001, 2, 16),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn mismatched_frame_is_rejected() {
    let mut encoder = AlacEncoder::new(8000, 1, 16).unwrap();
    let stereo = audio_frame(&[0, 0], 16, 2, 8000);
    assert!(matches!(
        encoder.encode(&stereo),
        Err(Error::InvalidArgument(_))
    ));
    let wrong_rate = audio_frame(&[0], 16, 1, 44_100);
    assert!(matches!(
        encoder.encode(&wrong_rate),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn empty_and_oversize_frames_are_rejected() {
    let mut encoder = AlacEncoder::new(8000, 1, 16).unwrap();
    let empty = audio_frame(&[], 16, 1, 8000);
    assert!(matches!(
        encoder.encode(&empty),
        Err(Error::InvalidArgument(_))
    ));
    let oversize = audio_frame(&vec![0i32; 4097], 16, 1, 8000);
    assert!(matches!(
        encoder.encode(&oversize),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn silence_compresses_well_below_raw() {
    let mut encoder = AlacEncoder::new(8000, 1, 16).unwrap();
    let frame = audio_frame(&vec![0i32; 4096], 16, 1, 8000);
    let packet = encoder.encode(&frame).unwrap();
    // Raw 16-bit samples would be 8192 bytes; the zero-run mode
    // must bring a silent frame far below that.
    assert!(
        packet.data.len() < 100,
        "silence packet: {} bytes",
        packet.data.len()
    );
}

#[test]
fn noise_never_expands_far_beyond_raw() {
    // The escape fallback caps the size at raw samples plus the
    // element headers; allow one extra byte per channel block
    // for the headers and the final byte alignment.
    let mut encoder = AlacEncoder::new(8000, 1, 16).unwrap();
    let samples = noise_samples(4096, 1, 16);
    let frame = audio_frame(&samples, 16, 1, 8000);
    let packet = encoder.encode(&frame).unwrap();
    assert!(
        packet.data.len() <= 4096 * 2 + 16,
        "noise packet: {} bytes",
        packet.data.len()
    );
}

/// Loads an `alacenc_*` fixture and returns its ALAC cookie plus
/// its packets (one ALAC frame per packet).
fn load_alacenc_fixture(name: &str) -> (codec_whatttttttttttt_core::AlacConfig, Vec<Vec<u8>>) {
    let mp4 = load_mp4(&fixture_path(name));
    let alac_box = find_box(&mp4.bytes, *b"alac").expect("fixture has no alac box");
    let config = parse_alac_config(alac_box).expect("fixture alac cookie parses");
    let packets = (0..mp4.len()).map(|i| mp4.sample(i).to_vec()).collect();
    (config, packets)
}

/// Decodes every packet with `decode_alac_frame` and returns the
/// concatenated integer samples.
fn decode_all_i32(
    config: &codec_whatttttttttttt_core::AlacConfig,
    packets: &[Vec<u8>],
) -> Vec<i32> {
    let mut samples = Vec::new();
    for packet in packets {
        let frame = decode_alac_frame(packet, config).expect("packet decodes");
        samples.extend_from_slice(&frame.samples);
    }
    samples
}

#[test]
fn fixture_sine_mono_8000_decodes_bit_exact() {
    let (config, packets) = load_alacenc_fixture("alacenc_sine_mono_8000.m4a");
    assert_eq!(config.sample_rate, 8000);
    assert_eq!(config.channels, 1);
    assert_eq!(config.bit_depth, 16);
    let decoded = decode_all_i32(&config, &packets);
    let reference = read_s16le_raw(&fixture_path("alacenc_sine_mono_8000_ref.raw"));
    assert_eq!(decoded.len(), reference.len());
    assert_eq!(decoded, reference);
}

#[test]
fn fixture_sine_stereo_44100_decodes_bit_exact() {
    let (config, packets) = load_alacenc_fixture("alacenc_sine_stereo_44100.m4a");
    assert_eq!(config.sample_rate, 44_100);
    assert_eq!(config.channels, 2);
    assert_eq!(config.bit_depth, 16);
    let decoded = decode_all_i32(&config, &packets);
    let reference = read_s16le_raw(&fixture_path("alacenc_sine_stereo_44100_ref.raw"));
    assert_eq!(decoded.len(), reference.len());
    assert_eq!(decoded, reference);
}

#[test]
fn fixture_noise_mono_8000_decodes_bit_exact() {
    let (config, packets) = load_alacenc_fixture("alacenc_noise_mono_8000.m4a");
    assert_eq!(config.sample_rate, 8000);
    assert_eq!(config.channels, 1);
    assert_eq!(config.bit_depth, 16);
    let decoded = decode_all_i32(&config, &packets);
    let reference = read_s16le_raw(&fixture_path("alacenc_noise_mono_8000_ref.raw"));
    assert_eq!(decoded.len(), reference.len());
    assert_eq!(decoded, reference);
}

#[test]
fn fixture_sine_mono_8000_24bit_decodes_bit_exact() {
    // ffmpeg's s32le decode of a 24-bit ALAC file is the 24-bit
    // sample shifted left by 8, so the reference is shifted back
    // before comparing (same convention as `tests/alac.rs`).
    let (config, packets) = load_alacenc_fixture("alacenc_sine_mono_8000_24bit.m4a");
    assert_eq!(config.sample_rate, 8000);
    assert_eq!(config.channels, 1);
    assert_eq!(config.bit_depth, 24);
    let decoded = decode_all_i32(&config, &packets);
    let reference: Vec<i32> = read_s32le_raw(&fixture_path("alacenc_sine_mono_8000_24bit_ref.raw"))
        .iter()
        .map(|&sample| sample >> 8)
        .collect();
    assert_eq!(decoded.len(), reference.len());
    assert_eq!(decoded, reference);
}
