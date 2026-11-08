//! Integration tests for FLAC encoding (RFC 9639, Phase 9).
//!
//! Every test round-trips through the Phase 2 decoder: encoding
//! must reproduce the original i16 PCM byte-identically (as f32,
//! the decoder's output format, where the i16 -> f32 scale is
//! exact). The encoded files are also checked with ffmpeg's FLAC
//! decoder externally (see `data/README.md`); this file asserts
//! the Rust round-trip only.

use std::io::Cursor;

use codec_whatttttttttttt_core::{
    decode_flac_frame, encode_flac_frame, encode_flac_stream, parse_flac_stream, parse_streaminfo,
    Codec, Decoder, Encoder, EncoderConfig, Error, FlacDemuxer, FlacEncoder, Frame,
};

fn decode_flac_bytes(bytes: &[u8]) -> Vec<f32> {
    let mut demuxer = FlacDemuxer::new(Cursor::new(bytes.to_vec())).unwrap();
    let mut decoder = Decoder::from_flac_streaminfo(demuxer.streaminfo()).unwrap();
    let mut samples = Vec::new();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        match decoder.decode(&packet).unwrap() {
            Frame::Audio(audio) => samples.extend_from_slice(&audio.samples),
            Frame::Video(_) => panic!("expected an audio frame"),
        }
    }
    samples
}

/// Exact i16 -> f32 scale the decoder uses (sample / 32768).
fn i16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|&value| f32::from(value) / 32768.0)
        .collect()
}

// Fixture generator: the sine values stay in [-20000, 20000]
// and the frame index in the low thousands, so both casts are
// exact; clippy cannot prove that from the types alone.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn sine_stereo_44100(frames: usize) -> Vec<i16> {
    let mut samples = Vec::with_capacity(frames * 2);
    for n in 0..frames {
        let t = n as f64 / 44100.0;
        let left = (2.0 * std::f64::consts::PI * 440.0 * t).sin();
        let right = (2.0 * std::f64::consts::PI * 660.0 * t).sin();
        samples.push((left * 20000.0) as i16);
        samples.push((right * 15000.0) as i16);
    }
    samples
}

// Fixture generator: same exact-cast reasoning as above.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn sine_mono_8000(frames: usize) -> Vec<i16> {
    (0..frames)
        .map(|n| {
            let t = n as f64 / 8000.0;
            ((2.0 * std::f64::consts::PI * 440.0 * t).sin() * 20000.0) as i16
        })
        .collect()
}

/// Deterministic pseudo-noise (linear congruential generator) so
/// the test does not depend on a random source.
fn noise_mono(frames: usize) -> Vec<i16> {
    let mut state: u32 = 0x1234_5678;
    (0..frames)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 16) as i16
        })
        .collect()
}

fn ramp_mono(frames: usize) -> Vec<i16> {
    (0..frames)
        .map(|n| {
            let value = (i32::try_from(n).unwrap_or(0) % 2000) - 1000;
            i16::try_from(value).unwrap_or(0)
        })
        .collect()
}

#[test]
fn round_trip_sine_stereo_is_byte_identical() {
    let samples = sine_stereo_44100(4410);
    let encoded = encode_flac_stream(&samples, 2, 44100).unwrap();
    assert_eq!(decode_flac_bytes(&encoded), i16_to_f32(&samples));
}

#[test]
fn round_trip_sine_mono_is_byte_identical() {
    let samples = sine_mono_8000(8000);
    let encoded = encode_flac_stream(&samples, 1, 8000).unwrap();
    assert_eq!(decode_flac_bytes(&encoded), i16_to_f32(&samples));
}

#[test]
fn round_trip_silence_is_byte_identical() {
    let samples = vec![0i16; 8000];
    let encoded = encode_flac_stream(&samples, 1, 8000).unwrap();
    assert_eq!(decode_flac_bytes(&encoded), i16_to_f32(&samples));
    // Silence compresses to constant subframes: far smaller than PCM.
    assert!(encoded.len() < samples.len() / 4);
}

#[test]
fn round_trip_noise_is_byte_identical() {
    let samples = noise_mono(8000);
    let encoded = encode_flac_stream(&samples, 1, 8000).unwrap();
    assert_eq!(decode_flac_bytes(&encoded), i16_to_f32(&samples));
}

#[test]
fn round_trip_ramp_is_byte_identical() {
    let samples = ramp_mono(8000);
    let encoded = encode_flac_stream(&samples, 1, 8000).unwrap();
    assert_eq!(decode_flac_bytes(&encoded), i16_to_f32(&samples));
}

#[test]
fn round_trip_long_signal_splits_into_4096_sample_frames() {
    // 10000 frames -> 4096 + 4096 + 1808.
    let samples = sine_mono_8000(10000);
    let encoded = encode_flac_stream(&samples, 1, 8000).unwrap();
    assert_eq!(decode_flac_bytes(&encoded), i16_to_f32(&samples));
    let stream = parse_flac_stream(&encoded).unwrap();
    assert_eq!(stream.streaminfo.total_samples, 10000);
    assert_eq!(stream.streaminfo.min_block_size, 1808);
    assert_eq!(stream.streaminfo.max_block_size, 4096);
}

#[test]
fn round_trip_exact_multiple_of_4096() {
    let samples = sine_mono_8000(8192);
    let encoded = encode_flac_stream(&samples, 1, 8000).unwrap();
    assert_eq!(decode_flac_bytes(&encoded), i16_to_f32(&samples));
    let stream = parse_flac_stream(&encoded).unwrap();
    assert_eq!(stream.streaminfo.min_block_size, 4096);
    assert_eq!(stream.streaminfo.max_block_size, 4096);
}

#[test]
fn round_trip_single_sample_frame() {
    let samples = vec![12345i16];
    let encoded = encode_flac_stream(&samples, 1, 8000).unwrap();
    assert_eq!(decode_flac_bytes(&encoded), i16_to_f32(&samples));
}

#[test]
fn streaminfo_fields_match_the_encoded_stream() {
    let samples = sine_stereo_44100(4410);
    let encoded = encode_flac_stream(&samples, 2, 44100).unwrap();
    let streaminfo = parse_streaminfo(&encoded[8..42]).unwrap();
    assert_eq!(streaminfo.sample_rate, 44100);
    assert_eq!(streaminfo.channels, 2);
    assert_eq!(streaminfo.bits_per_sample, 16);
    assert_eq!(streaminfo.total_samples, 4410);
    // 4410 samples split into frames of 4096 + 314.
    assert_eq!(streaminfo.min_block_size, 314);
    assert_eq!(streaminfo.max_block_size, 4096);
    assert_eq!(streaminfo.md5, [0; 16]);
    assert!(streaminfo.min_frame_size > 0);
    assert!(streaminfo.max_frame_size >= streaminfo.min_frame_size);
    // The stream header is fLaC + one last-block STREAMINFO block.
    assert_eq!(&encoded[0..4], b"fLaC");
    assert_eq!(encoded[4], 0x80);
    assert_eq!(encoded[5], 0);
    assert_eq!(encoded[6], 0);
    assert_eq!(encoded[7], 34);
}

#[test]
fn frame_header_fields_match_the_encoded_frame() {
    let samples = sine_mono_8000(1000);
    let frame = encode_flac_frame(&samples, 1, 8000, 0).unwrap();
    let streaminfo =
        parse_streaminfo(&encode_flac_stream(&samples, 1, 8000).unwrap()[8..42]).unwrap();
    let (header, header_len) =
        codec_whatttttttttttt_core::parse_frame_header(&frame, &streaminfo).unwrap();
    assert_eq!(header.block_size, 1000);
    assert_eq!(header.sample_rate, 8000);
    assert_eq!(header.channels, 1);
    assert_eq!(header.bits_per_sample, 16);
    assert_eq!(header.coded_number, 0);
    assert!(header.blocking_strategy_variable);
    assert!(header_len < frame.len());
    // The frame decodes to the original samples.
    let (decoded, consumed) = decode_flac_frame(&frame, &streaminfo).unwrap();
    assert_eq!(consumed, frame.len());
    assert_eq!(decoded.samples, i16_to_f32(&samples));
}

#[test]
fn frame_header_sample_number_accumulates() {
    let samples = sine_mono_8000(2000);
    let first = encode_flac_frame(&samples, 1, 8000, 0).unwrap();
    let second = encode_flac_frame(&samples, 1, 8000, 2000).unwrap();
    let streaminfo =
        parse_streaminfo(&encode_flac_stream(&samples, 1, 8000).unwrap()[8..42]).unwrap();
    let (first_header, _) =
        codec_whatttttttttttt_core::parse_frame_header(&first, &streaminfo).unwrap();
    let (second_header, _) =
        codec_whatttttttttttt_core::parse_frame_header(&second, &streaminfo).unwrap();
    assert_eq!(first_header.coded_number, 0);
    assert_eq!(second_header.coded_number, 2000);
}

#[test]
fn streaming_encoder_matches_one_shot() {
    let samples = sine_stereo_44100(9000);
    let one_shot = encode_flac_stream(&samples, 2, 44100).unwrap();
    let mut encoder = FlacEncoder::new(2, 44100).unwrap();
    let mut streamed = encoder.streaminfo_bytes();
    for chunk in samples.chunks(4096 * 2) {
        streamed.extend_from_slice(&encoder.encode(chunk).unwrap());
    }
    assert_eq!(streamed.len(), one_shot.len());
    // The streaming STREAMINFO has total 0 and min block 0, so the
    // bytes differ only in the header; the frames are identical.
    assert_eq!(&streamed[42..], &one_shot[42..]);
    assert_eq!(decode_flac_bytes(&streamed), i16_to_f32(&samples));
    assert_eq!(encoder.sample_number(), 9000);
}

#[test]
fn streaming_encoder_streaminfo_is_streaming_form() {
    let encoder = FlacEncoder::new(2, 44100).unwrap();
    let header = encoder.streaminfo_bytes();
    let streaminfo = parse_streaminfo(&header[8..42]).unwrap();
    assert_eq!(streaminfo.total_samples, 0);
    assert_eq!(streaminfo.min_block_size, 0);
    assert_eq!(streaminfo.max_block_size, 4096);
    assert_eq!(streaminfo.sample_rate, 44100);
    assert_eq!(streaminfo.channels, 2);
}

#[test]
fn empty_samples_returns_invalid_argument() {
    assert!(matches!(
        encode_flac_stream(&[], 1, 8000),
        Err(Error::InvalidArgument(_))
    ));
    let mut encoder = FlacEncoder::new(1, 8000).unwrap();
    assert!(matches!(
        encoder.encode(&[]),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn block_larger_than_4096_returns_invalid_argument() {
    let samples = vec![0i16; 4097];
    assert!(matches!(
        encode_flac_frame(&samples, 1, 8000, 0),
        Err(Error::InvalidArgument(_))
    ));
    let mut encoder = FlacEncoder::new(1, 8000).unwrap();
    assert!(matches!(
        encoder.encode(&samples),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn sample_count_not_a_whole_number_of_frames_returns_invalid_argument() {
    // 3 samples for 2 channels is not a whole number of frames.
    assert!(matches!(
        encode_flac_stream(&[1i16, 2, 3], 2, 8000),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn invalid_channel_count_returns_invalid_argument() {
    assert!(matches!(
        encode_flac_stream(&[1i16, 2], 0, 8000),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        encode_flac_stream(&[1i16, 2], 9, 8000),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        FlacEncoder::new(0, 8000),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn invalid_sample_rate_returns_invalid_argument() {
    assert!(matches!(
        encode_flac_stream(&[1i16], 1, 0),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        encode_flac_stream(&[1i16], 1, 192_001),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn sine_compresses_below_pcm_size() {
    let samples = sine_stereo_44100(4410);
    let encoded = encode_flac_stream(&samples, 2, 44100).unwrap();
    // Raw PCM would be 4410 frames * 2 channels * 2 bytes = 17640.
    assert!(encoded.len() < 17640);
}

#[test]
fn high_level_encoder_produces_decodable_flac_frames() {
    let samples_f32: Vec<f32> = sine_mono_8000(1000)
        .iter()
        .map(|&value| f32::from(value) / 32768.0)
        .collect();
    let mut encoder = Encoder::new(EncoderConfig {
        codec: Codec::Flac,
        sample_rate: 8000,
        channels: 1,
        ..EncoderConfig::default()
    })
    .unwrap();
    let frame = Frame::Audio(codec_whatttttttttttt_core::AudioFrame {
        sample_rate: 8000,
        channels: 1,
        samples: samples_f32,
        timestamp: 0,
    });
    let packet = encoder.encode(&frame).unwrap();
    assert_eq!(packet.codec, Codec::Flac);
    assert!(packet.is_keyframe);
    let streaminfo = parse_streaminfo(&encoder.flac_streaminfo_bytes()[8..42]).unwrap();
    let (decoded, _) = decode_flac_frame(&packet.data, &streaminfo).unwrap();
    let original = sine_mono_8000(1000);
    assert_eq!(decoded.samples, i16_to_f32(&original));
}

#[test]
fn high_level_encoder_rejects_oversize_frame() {
    let mut encoder = Encoder::new(EncoderConfig {
        codec: Codec::Flac,
        sample_rate: 8000,
        channels: 1,
        ..EncoderConfig::default()
    })
    .unwrap();
    let frame = Frame::Audio(codec_whatttttttttttt_core::AudioFrame {
        sample_rate: 8000,
        channels: 1,
        samples: vec![0.0; 4097],
        timestamp: 0,
    });
    assert!(matches!(
        encoder.encode(&frame),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn opus_vorbis_vp9_av1_encode_return_unsupported() {
    for codec in [Codec::Opus, Codec::Vorbis, Codec::Vp9, Codec::Av1] {
        let mut encoder = Encoder::new(EncoderConfig {
            codec,
            sample_rate: 8000,
            channels: 1,
            ..EncoderConfig::default()
        })
        .unwrap();
        let frame = Frame::Audio(codec_whatttttttttttt_core::AudioFrame {
            sample_rate: 8000,
            channels: 1,
            samples: vec![0.0],
            timestamp: 0,
        });
        assert!(
            matches!(encoder.encode(&frame), Err(Error::Unsupported(_))),
            "codec {codec:?} should return Unsupported"
        );
    }
}
