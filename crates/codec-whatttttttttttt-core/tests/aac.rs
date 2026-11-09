//! AAC-LC decode tests against ffmpeg fixtures.
//!
//! Fixtures are real files encoded by ffmpeg 8.1.2 (see the
//! task report for the exact commands). Decoding must match
//! ffmpeg's own decode of the same bitstream within the
//! conformance threshold below: AAC is lossy, so the
//! comparison is by error bounds, not bit-exactness, and the
//! threshold is stated here, in the test, so a failure names
//! the bound that broke.
//!
//! Threshold (ISO/IEC 13818-4 conformance style: the decoded
//! PCM must stay within a small absolute error of the
//! reference decoder's output; the standard's own vectors use
//! RMS limits in the same order of magnitude):
//!   - maximum absolute sample error: 2.0e-3
//!   - RMS error over the whole file: 5.0e-4
//!
//! Measured against ffmpeg 8.1.2 on the fixtures below
//! (2026-11-08, `cargo test --features licensed-aac --test
//! aac`), the worst case is max abs 7.9e-5 / RMS 3.0e-6
//! (burst, 128k, short windows) — the thresholds above carry
//! roughly a 25x margin on max abs and a 160x margin on RMS
//! over the measured error, so a regression in the IMDCT
//! scale, the windows, MS stereo, TNS or PNS shows up as a
//! failure rather than a near miss. All other fixtures
//! measure max abs <= 3e-6 and RMS <= 2e-6.
//!
//! MP4 note: ffmpeg's MP4 muxer writes an edit list that
//! skips the first AAC frame (encoder priming), so the
//! `.m4a` reference PCM starts at stream frame 1. The MP4
//! tests decode every sample but only compare from frame 1
//! onwards; frame 0 must still decode without error.

// Only the licensed-aac tests use the MP4 helper module;
// gating it keeps the feature-off build free of dead code.
#[cfg(feature = "licensed-aac")]
#[path = "mp4_samples.rs"]
mod mp4_samples;

use codec_whatttttttttttt_core::aac_frame::parse_adts_header;
#[cfg(feature = "licensed-aac")]
use codec_whatttttttttttt_core::aac_frame::parse_audio_specific_config;
use codec_whatttttttttttt_core::{
    AacConfig, AdtsDemuxer, Decoder, Error, LicenseContext, LicenseType,
};
#[cfg(feature = "licensed-aac")]
use codec_whatttttttttttt_core::{AacDecoder, Frame, Packet};
#[cfg(feature = "licensed-aac")]
use mp4_samples::{find_box, load_mp4, read_f32le_raw};

/// Maximum absolute sample error allowed vs ffmpeg.
#[cfg(feature = "licensed-aac")]
const MAX_ABS_ERROR: f32 = 2.0e-3;
/// Maximum RMS error allowed vs ffmpeg.
#[cfg(feature = "licensed-aac")]
const MAX_RMS_ERROR: f32 = 5.0e-4;

fn fixture_path(name: &str) -> String {
    format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// A well-formed, unexpired license invented for tests.
#[cfg(feature = "licensed-aac")]
fn valid_license() -> LicenseContext {
    LicenseContext::new(
        "test licensee".to_string(),
        LicenseType::Commercial,
        "TEST-12345678".to_string(),
        4_102_444_800,
        "aac".to_string(),
    )
    .unwrap()
}

/// An expired license invented for tests (epoch second 1).
#[cfg(feature = "licensed-aac")]
fn expired_license() -> LicenseContext {
    LicenseContext::new(
        "test licensee".to_string(),
        LicenseType::Commercial,
        "TEST-12345678".to_string(),
        1,
        "aac".to_string(),
    )
    .unwrap()
}

/// Decodes raw-data-block payloads with a licensed `Decoder`
/// and returns the concatenated interleaved samples.
#[cfg(feature = "licensed-aac")]
fn decode_all_with_decoder(config: &AacConfig, payloads: &[Vec<u8>]) -> Vec<f32> {
    let mut license = valid_license();
    let mut decoder =
        Decoder::from_aac_config(config, Some(&mut license)).expect("licensed aac decoder");
    let mut samples = Vec::new();
    for payload in payloads {
        let frame = decoder
            .decode(&Packet {
                codec: codec_whatttttttttttt_core::Codec::Aac,
                data: payload.clone(),
                timestamp: 0,
                is_keyframe: false,
            })
            .expect("aac frame decodes");
        let Frame::Audio(audio) = frame else {
            panic!("aac decode must return an audio frame");
        };
        samples.extend_from_slice(&audio.samples);
    }
    samples
}

/// Compares decoded samples with the reference, returning
/// (max absolute error, RMS error). The caller asserts both
/// against the thresholds, with the measured values in the
/// failure message.
#[cfg(feature = "licensed-aac")]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "test only: fixture sample counts are far below 2^53 so the usize-to-f64 cast is exact, and the f64-to-f32 RMS cast only feeds a threshold comparison with a 160x margin"
)]
fn compare(name: &str, decoded: &[f32], reference: &[f32]) -> (f32, f32) {
    assert_eq!(
        decoded.len(),
        reference.len(),
        "{name}: decoded length {} != reference length {}",
        decoded.len(),
        reference.len()
    );
    let mut max_abs: f32 = 0.0;
    let mut sum_sq: f64 = 0.0;
    for (got, want) in decoded.iter().zip(reference.iter()) {
        let diff = (got - want).abs();
        if diff > max_abs {
            max_abs = diff;
        }
        sum_sq += f64::from(diff * diff);
    }
    let rms = (sum_sq / decoded.len() as f64).sqrt() as f32;
    assert!(
        max_abs <= MAX_ABS_ERROR,
        "{name}: max abs error {max_abs:.6} exceeds {MAX_ABS_ERROR} (rms {rms:.6})"
    );
    assert!(
        rms <= MAX_RMS_ERROR,
        "{name}: rms error {rms:.6} exceeds {MAX_RMS_ERROR} (max abs {max_abs:.6})"
    );
    (max_abs, rms)
}

/// Loads an ADTS fixture and returns its config plus the raw
/// data block payload of every frame.
fn load_adts_fixture(name: &str) -> (AacConfig, Vec<Vec<u8>>) {
    let bytes = std::fs::read(fixture_path(name)).expect("fixture reads");
    let (config, _) = parse_adts_header(&bytes).expect("fixture adts header parses");
    let mut demuxer = AdtsDemuxer::from_bytes(&bytes).expect("fixture adts demuxes");
    let mut payloads = Vec::new();
    while let Some(frame) = demuxer.next_frame().expect("adts frame reads") {
        payloads.push(frame.payload);
    }
    assert!(!payloads.is_empty(), "{name}: no adts frames found");
    (config, payloads)
}

/// Reads the `AudioSpecificConfig` out of an MP4 `esds` box:
/// version/flags, then tag/length pairs. The `ASC` lives in
/// the decoder-specific-info tag (`0x05`), nested inside
/// the decoder config descriptor (`0x04`, after 13 fixed
/// bytes), nested inside the `ES` descriptor (`0x03`, after
/// the 3-byte `ES` id and flags). Lengths are base-128 (a
/// byte with the high bit set continues).
#[cfg(feature = "licensed-aac")]
fn aac_config_from_esds(esds: &[u8]) -> AacConfig {
    fn find_asc(data: &[u8], start: usize, end: usize) -> Option<&[u8]> {
        let mut offset = start;
        while offset + 2 <= end {
            let tag = data[offset];
            offset += 1;
            let mut length: usize = 0;
            loop {
                let byte = *data.get(offset)?;
                offset += 1;
                length = (length << 7) | usize::from(byte & 0x7F);
                if byte & 0x80 == 0 {
                    break;
                }
            }
            let payload_start = offset;
            let payload_end = offset + length;
            if payload_end > end {
                return None;
            }
            match tag {
                0x05 => return Some(&data[payload_start..payload_end]),
                // Descend into the nested descriptors, past
                // their fixed headers.
                0x03 => {
                    if let Some(asc) = find_asc(data, payload_start + 3, payload_end) {
                        return Some(asc);
                    }
                }
                0x04 => {
                    if let Some(asc) = find_asc(data, payload_start + 13, payload_end) {
                        return Some(asc);
                    }
                }
                _ => {}
            }
            offset = payload_end;
        }
        None
    }
    let asc = find_asc(esds, 8 + 4, esds.len()).expect("esds box has no decoder specific info");
    parse_audio_specific_config(asc).expect("esds asc parses")
}

/// Loads an MP4 fixture and returns its config plus its
/// samples (one raw data block per sample).
#[cfg(feature = "licensed-aac")]
fn load_mp4_fixture(name: &str) -> (AacConfig, Vec<Vec<u8>>) {
    let mp4 = load_mp4(&fixture_path(name));
    let esds = find_box(&mp4.bytes, *b"esds").expect("fixture has no esds box");
    let config = aac_config_from_esds(esds);
    let packets = (0..mp4.len()).map(|i| mp4.sample(i).to_vec()).collect();
    (config, packets)
}

#[test]
#[cfg(feature = "licensed-aac")]
fn adts_stereo_44100_128k_matches_ffmpeg() {
    let (config, payloads) = load_adts_fixture("aac_stereo_44100_128k.aac");
    assert_eq!(config.sample_rate, 44_100);
    assert_eq!(config.channels, 2);
    let decoded = decode_all_with_decoder(&config, &payloads);
    let reference = read_f32le_raw(&fixture_path("aac_stereo_44100_128k_ref.raw"));
    compare("adts stereo 44100 128k", &decoded, &reference);
}

#[test]
#[cfg(feature = "licensed-aac")]
fn adts_stereo_44100_64k_matches_ffmpeg() {
    let (config, payloads) = load_adts_fixture("aac_stereo_44100_64k.aac");
    let decoded = decode_all_with_decoder(&config, &payloads);
    let reference = read_f32le_raw(&fixture_path("aac_stereo_44100_64k_ref.raw"));
    compare("adts stereo 44100 64k", &decoded, &reference);
}

#[test]
#[cfg(feature = "licensed-aac")]
fn adts_stereo_44100_320k_matches_ffmpeg() {
    let (config, payloads) = load_adts_fixture("aac_stereo_44100_320k.aac");
    let decoded = decode_all_with_decoder(&config, &payloads);
    let reference = read_f32le_raw(&fixture_path("aac_stereo_44100_320k_ref.raw"));
    compare("adts stereo 44100 320k", &decoded, &reference);
}

#[test]
#[cfg(feature = "licensed-aac")]
fn adts_stereo_48000_96k_matches_ffmpeg() {
    let (config, payloads) = load_adts_fixture("aac_stereo_48000_96k.aac");
    assert_eq!(config.sample_rate, 48_000);
    let decoded = decode_all_with_decoder(&config, &payloads);
    let reference = read_f32le_raw(&fixture_path("aac_stereo_48000_96k_ref.raw"));
    compare("adts stereo 48000 96k", &decoded, &reference);
}

#[test]
#[cfg(feature = "licensed-aac")]
fn adts_mono_8000_64k_matches_ffmpeg() {
    let (config, payloads) = load_adts_fixture("aac_mono_8000_64k.aac");
    assert_eq!(config.sample_rate, 8000);
    assert_eq!(config.channels, 1);
    let decoded = decode_all_with_decoder(&config, &payloads);
    let reference = read_f32le_raw(&fixture_path("aac_mono_8000_64k_ref.raw"));
    compare("adts mono 8000 64k", &decoded, &reference);
}

#[test]
#[cfg(feature = "licensed-aac")]
fn adts_tone_44100_128k_matches_ffmpeg_over_many_frames() {
    let (config, payloads) = load_adts_fixture("aac_tone_44100_128k.aac");
    assert!(payloads.len() > 40, "tone fixture should have many frames");
    let decoded = decode_all_with_decoder(&config, &payloads);
    let reference = read_f32le_raw(&fixture_path("aac_tone_44100_128k_ref.raw"));
    compare("adts tone 44100 128k", &decoded, &reference);
}

#[test]
#[cfg(feature = "licensed-aac")]
fn adts_burst_44100_128k_matches_ffmpeg_and_uses_short_windows() {
    let (config, payloads) = load_adts_fixture("aac_burst_44100_128k.aac");
    let mut decoder = AacDecoder::new(&config).expect("aac decoder builds");
    let mut samples = Vec::new();
    for payload in &payloads {
        let frame = decoder
            .decode_raw_data_block(payload)
            .expect("burst frame decodes");
        samples.extend_from_slice(&frame.samples);
    }
    assert!(
        decoder.short_window_blocks() > 0,
        "burst fixture produced no short-window blocks; it does not exercise EIGHT_SHORT"
    );
    let reference = read_f32le_raw(&fixture_path("aac_burst_44100_128k_ref.raw"));
    compare("adts burst 44100 128k", &samples, &reference);
}

#[test]
#[cfg(feature = "licensed-aac")]
fn mp4_stereo_44100_128k_matches_ffmpeg_after_priming_frame() {
    let (config, packets) = load_mp4_fixture("aac_stereo_44100_128k.m4a");
    assert_eq!(config.sample_rate, 44_100);
    assert_eq!(config.channels, 2);
    let decoded = decode_all_with_decoder(&config, &packets);
    let reference = read_f32le_raw(&fixture_path("aac_stereo_44100_128k_m4a_ref.raw"));
    // The MP4 edit list skips stream frame 0 (encoder
    // priming); the reference starts at stream frame 1.
    let frame_samples = 1024 * config.channels as usize;
    assert_eq!(
        reference.len(),
        decoded.len() - frame_samples,
        "mp4 reference should cover every frame after the priming frame"
    );
    compare(
        "mp4 stereo 44100 128k",
        &decoded[frame_samples..],
        &reference,
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn mp4_stereo_48000_96k_matches_ffmpeg_after_priming_frame() {
    let (config, packets) = load_mp4_fixture("aac_stereo_48000_96k.m4a");
    let decoded = decode_all_with_decoder(&config, &packets);
    let reference = read_f32le_raw(&fixture_path("aac_stereo_48000_96k_m4a_ref.raw"));
    let frame_samples = 1024 * config.channels as usize;
    compare(
        "mp4 stereo 48000 96k",
        &decoded[frame_samples..],
        &reference,
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn mp4_mono_8000_64k_matches_ffmpeg_after_priming_frame() {
    let (config, packets) = load_mp4_fixture("aac_mono_8000_64k.m4a");
    let decoded = decode_all_with_decoder(&config, &packets);
    let reference = read_f32le_raw(&fixture_path("aac_mono_8000_64k_m4a_ref.raw"));
    let frame_samples = 1024 * config.channels as usize;
    compare("mp4 mono 8000 64k", &decoded[frame_samples..], &reference);
}

/// `expect_err` for results whose `Ok` type has no `Debug`
/// (`Decoder` cannot derive it): panics on success, returns
/// the error otherwise.
fn expect_err<T>(result: Result<T, Error>, message: &str) -> Error {
    match result {
        Ok(_) => panic!("{message}"),
        Err(err) => err,
    }
}

#[test]
#[cfg(feature = "licensed-aac")]
fn decoding_without_a_license_is_license_required() {
    let (config, payloads) = load_adts_fixture("aac_stereo_44100_128k.aac");
    let err = expect_err(
        Decoder::from_aac_config(&config, None),
        "no license must fail",
    );
    assert!(
        matches!(err, Error::LicenseRequired { codec: "aac" }),
        "expected LicenseRequired for aac, got {err}"
    );
    let _ = payloads;
}

#[test]
#[cfg(feature = "licensed-aac")]
fn decoding_with_an_expired_license_is_license_required() {
    let (config, _payloads) = load_adts_fixture("aac_stereo_44100_128k.aac");
    let mut license = expired_license();
    let err = expect_err(
        Decoder::from_aac_config(&config, Some(&mut license)),
        "expired license must fail",
    );
    assert!(
        matches!(err, Error::LicenseRequired { codec: "aac" }),
        "expected LicenseRequired for aac, got {err}"
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn constructing_a_license_with_a_malformed_id_is_invalid_argument() {
    // The id format is checked at construction time, so a
    // malformed id never reaches `validate`.
    let err = LicenseContext::new(
        "test licensee".to_string(),
        LicenseType::Commercial,
        "bad id!".to_string(),
        4_102_444_800,
        "aac".to_string(),
    )
    .expect_err("malformed license id must fail at construction");
    assert!(
        matches!(err, Error::InvalidArgument(_)),
        "expected InvalidArgument for a malformed license id, got {err}"
    );
}

#[test]
#[cfg(not(feature = "licensed-aac"))]
fn aac_decode_without_the_feature_is_unsupported() {
    let (config, _payloads) = load_adts_fixture("aac_stereo_44100_128k.aac");
    let mut license = LicenseContext::new(
        "test licensee".to_string(),
        LicenseType::Commercial,
        "TEST-12345678".to_string(),
        4_102_444_800,
        "aac".to_string(),
    )
    .unwrap();
    let err = expect_err(
        Decoder::from_aac_config(&config, Some(&mut license)),
        "feature off must fail",
    );
    assert!(
        matches!(err, Error::Unsupported(_)),
        "expected Unsupported with the feature off, got {err}"
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn empty_aac_packet_is_truncated() {
    let config = AacConfig {
        audio_object_type: 2,
        sample_rate: 44_100,
        channels: 2,
        frame_length: 1024,
    };
    let mut decoder = AacDecoder::new(&config).expect("aac decoder builds");
    let err = decoder
        .decode_raw_data_block(&[])
        .expect_err("empty packet must fail");
    assert!(
        matches!(err, Error::Truncated { .. }),
        "expected Truncated for an empty packet, got {err}"
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn aac_packet_cut_in_half_is_an_error() {
    let (config, payloads) = load_adts_fixture("aac_stereo_44100_128k.aac");
    let half = payloads[0].len() / 2;
    let mut decoder = AacDecoder::new(&config).expect("aac decoder builds");
    let err = decoder
        .decode_raw_data_block(&payloads[0][..half])
        .expect_err("half a packet must fail");
    assert!(
        matches!(err, Error::Truncated { .. } | Error::InvalidData { .. }),
        "expected Truncated or InvalidData for half a packet, got {err}"
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn aac_packet_with_a_bad_element_id_is_invalid_data() {
    let (config, payloads) = load_adts_fixture("aac_stereo_44100_128k.aac");
    // The first element id is the top 3 bits; 7 is ID_END, so
    // a block that ends immediately decodes zero channels,
    // which mismatches the stereo config.
    let mut first = payloads[0].clone();
    first[0] = (first[0] & 0x1F) | 0xE0;
    let mut decoder = AacDecoder::new(&config).expect("aac decoder builds");
    let err = decoder
        .decode_raw_data_block(&first)
        .expect_err("a block ending at once must fail");
    assert!(
        matches!(err, Error::InvalidData { .. }),
        "expected InvalidData for a block with no channels, got {err}"
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn sbr_audio_specific_config_is_unsupported() {
    // AOT 5 (SBR) with a 44100 Hz core: the config parser
    // must refuse it before any decode is attempted.
    let err = parse_audio_specific_config(&[0x2B, 0x90]).expect_err("sbr config must fail");
    assert!(
        matches!(err, Error::Unsupported(_)),
        "expected Unsupported for SBR, got {err}"
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn aac_decoder_rejects_7350_hz() {
    let config = AacConfig {
        audio_object_type: 2,
        sample_rate: 7350,
        channels: 2,
        frame_length: 1024,
    };
    let err = AacDecoder::new(&config).expect_err("7350 Hz must fail");
    assert!(
        matches!(err, Error::Unsupported(_)),
        "expected Unsupported for 7350 Hz, got {err}"
    );
}
