//! ALAC decode tests against ffmpeg fixtures.
//!
//! Fixtures (see the task report for the ffmpeg commands) are
//! real `.m4a` files encoded by ffmpeg 8.1.2 from the existing
//! WAV fixtures. Decoding must be bit-exact: ALAC is lossless,
//! so every integer sample must equal ffmpeg's decode of the
//! same file, sample for sample. The reference `.raw` files are
//! ffmpeg's own decode (`-f s16le` / `-f s32le`), checked in
//! alongside the fixtures.

#[path = "mp4_samples.rs"]
mod mp4_samples;

use codec_whatttttttttttt_core::{
    decode_alac_frame, parse_alac_config, AlacConfig, Codec, Decoder, Packet,
};
use codec_whatttttttttttt_core::{Error, Frame};
use mp4_samples::{find_box, load_mp4, read_s16le_raw, read_s32le_raw};

fn fixture_path(name: &str) -> String {
    format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Loads an `.m4a` fixture and returns its ALAC cookie plus its
/// packets (one ALAC frame per packet).
fn load_alac_fixture(name: &str) -> (AlacConfig, Vec<Vec<u8>>) {
    let mp4 = load_mp4(&fixture_path(name));
    let alac_box = find_box(&mp4.bytes, *b"alac").expect("fixture has no alac box");
    let config = parse_alac_config(alac_box).expect("fixture alac cookie parses");
    let packets = (0..mp4.len()).map(|i| mp4.sample(i).to_vec()).collect();
    (config, packets)
}

/// Decodes every packet with `decode_alac_frame` and returns the
/// concatenated integer samples.
fn decode_all_i32(config: &AlacConfig, packets: &[Vec<u8>]) -> Vec<i32> {
    let mut samples = Vec::new();
    for packet in packets {
        let frame = decode_alac_frame(packet, config).expect("packet decodes");
        samples.extend_from_slice(&frame.samples);
    }
    samples
}

#[test]
fn mono_8000_decodes_bit_exact() {
    let (config, packets) = load_alac_fixture("alac_mono_8000.m4a");
    assert_eq!(config.sample_rate, 8000);
    assert_eq!(config.channels, 1);
    assert_eq!(config.bit_depth, 16);
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].len(), 1062);
    let decoded = decode_all_i32(&config, &packets);
    let reference = read_s16le_raw(&fixture_path("alac_mono_8000_ref.raw"));
    assert_eq!(decoded.len(), reference.len());
    assert_eq!(decoded, reference);
}

#[test]
fn stereo_44100_decodes_bit_exact() {
    let (config, packets) = load_alac_fixture("alac_stereo_44100.m4a");
    assert_eq!(config.sample_rate, 44_100);
    assert_eq!(config.channels, 2);
    assert_eq!(config.bit_depth, 16);
    assert_eq!(packets.len(), 2);
    assert_eq!(packets[0].len(), 3325);
    assert_eq!(packets[1].len(), 307);
    let decoded = decode_all_i32(&config, &packets);
    let reference = read_s16le_raw(&fixture_path("alac_stereo_44100_ref.raw"));
    assert_eq!(decoded.len(), reference.len());
    assert_eq!(decoded, reference);
}

#[test]
fn noise_mono_8000_decodes_bit_exact() {
    let (config, packets) = load_alac_fixture("alac_noise_mono_8000.m4a");
    assert_eq!(packets.len(), 1);
    let decoded = decode_all_i32(&config, &packets);
    let reference = read_s16le_raw(&fixture_path("alac_noise_mono_8000_ref.raw"));
    assert_eq!(decoded, reference);
}

#[test]
fn ramp_mono_8000_decodes_bit_exact() {
    let (config, packets) = load_alac_fixture("alac_ramp_mono_8000.m4a");
    assert_eq!(packets.len(), 1);
    let decoded = decode_all_i32(&config, &packets);
    let reference = read_s16le_raw(&fixture_path("alac_ramp_mono_8000_ref.raw"));
    assert_eq!(decoded, reference);
}

#[test]
fn mono_8000_24bit_decodes_bit_exact() {
    // ffmpeg's alac encoder writes 24-bit ALAC for s32 input;
    // its s32le decode is the 24-bit sample shifted left by 8,
    // so the reference is shifted back before comparing.
    let (config, packets) = load_alac_fixture("alac_mono_8000_s32.m4a");
    assert_eq!(config.bit_depth, 24);
    assert_eq!(config.channels, 1);
    assert_eq!(packets.len(), 1);
    let decoded = decode_all_i32(&config, &packets);
    let reference: Vec<i32> = read_s32le_raw(&fixture_path("alac_mono_8000_s32_ref.raw"))
        .iter()
        .map(|&sample| sample >> 8)
        .collect();
    assert_eq!(decoded.len(), reference.len());
    assert_eq!(decoded, reference);
}

#[test]
fn stereo_44100_24bit_decodes_bit_exact() {
    let (config, packets) = load_alac_fixture("alac_stereo_44100_s32.m4a");
    assert_eq!(config.bit_depth, 24);
    assert_eq!(config.channels, 2);
    assert_eq!(packets.len(), 2);
    let decoded = decode_all_i32(&config, &packets);
    let reference: Vec<i32> = read_s32le_raw(&fixture_path("alac_stereo_44100_s32_ref.raw"))
        .iter()
        .map(|&sample| sample >> 8)
        .collect();
    assert_eq!(decoded.len(), reference.len());
    assert_eq!(decoded, reference);
}

#[test]
#[allow(
    clippy::cast_possible_truncation,
    reason = "test only: decoded ALAC samples are in [-1.0, 1.0], so the scaled value is in [-32768, 32768] and always fits i32"
)]
fn decoder_api_returns_matching_f32_frames() {
    // The public `Decoder` path must agree with the direct
    // frame decoder, sample for sample after the f32 round
    // trip (exact for 16-bit samples: i16/32768 is exact in f32).
    let (config, packets) = load_alac_fixture("alac_stereo_44100.m4a");
    let mut decoder = Decoder::from_alac_config(&config).expect("decoder builds");
    let reference = read_s16le_raw(&fixture_path("alac_stereo_44100_ref.raw"));
    let mut sample_index = 0usize;
    for (packet_index, packet) in packets.iter().enumerate() {
        let frame = decoder
            .decode(&Packet {
                codec: Codec::Alac,
                data: packet.clone(),
                timestamp: packet_index as u64 * 1_000_000,
                is_keyframe: true,
            })
            .expect("packet decodes through Decoder");
        let Frame::Audio(audio) = frame else {
            panic!("alac decode returns an audio frame");
        };
        assert_eq!(audio.sample_rate, 44_100);
        assert_eq!(audio.channels, 2);
        for &sample in &audio.samples {
            let expected = reference[sample_index];
            assert_eq!((sample * 32768.0).round() as i32, expected);
            sample_index += 1;
        }
    }
    assert_eq!(sample_index, reference.len());
}

#[test]
fn empty_packet_is_truncated() {
    let (config, _) = load_alac_fixture("alac_mono_8000.m4a");
    let err = decode_alac_frame(&[], &config).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn packet_cut_in_half_is_an_error() {
    let (config, packets) = load_alac_fixture("alac_mono_8000.m4a");
    let half = &packets[0][..packets[0].len() / 2];
    assert!(decode_alac_frame(half, &config).is_err());
}

#[test]
fn nonzero_unused_header_bits_are_invalid_data() {
    // Byte 2 of the mono packet holds the last 3 unused header
    // bits (top), the 4-bit element header, and the first count
    // bit; setting the top 3 bits must be rejected.
    let (config, packets) = load_alac_fixture("alac_mono_8000.m4a");
    let mut bad = packets[0].clone();
    bad[2] |= 0xE0;
    let err = decode_alac_frame(&bad, &config).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn partial_count_above_frame_length_is_invalid_data() {
    // Bytes 5-6 of the mono packet carry the low bits of the
    // partial-frame count; forcing the count to 8192 (above
    // the cookie frame length of 4096) must be rejected.
    let (config, packets) = load_alac_fixture("alac_mono_8000.m4a");
    let mut bad = packets[0].clone();
    bad[5] = 0x40;
    bad[6] = 0x01;
    let err = decode_alac_frame(&bad, &config).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn packet_for_wrong_codec_is_invalid_argument() {
    let (config, packets) = load_alac_fixture("alac_mono_8000.m4a");
    let mut decoder = Decoder::from_alac_config(&config).expect("decoder builds");
    let err = decoder
        .decode(&Packet {
            codec: Codec::Flac,
            data: packets[0].clone(),
            timestamp: 0,
            is_keyframe: true,
        })
        .unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
}

#[test]
fn cookie_with_bad_bit_depth_is_invalid_data() {
    let mut cookie = [0u8; 24];
    cookie[0..4].copy_from_slice(&4096u32.to_be_bytes());
    cookie[4] = 0; // compatibleVersion
    cookie[5] = 12; // bitDepth 12 is not 16/20/24/32
    cookie[6] = 40;
    cookie[7] = 10;
    cookie[8] = 14;
    cookie[9] = 1;
    cookie[10..12].copy_from_slice(&0u16.to_be_bytes());
    cookie[12..16].copy_from_slice(&8196u32.to_be_bytes());
    cookie[16..20].copy_from_slice(&128_000u32.to_be_bytes());
    cookie[20..24].copy_from_slice(&8000u32.to_be_bytes());
    let err = parse_alac_config(&cookie).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
}
