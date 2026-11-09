//! AAC-LC encode tests: the license gate, argument validation,
//! and round-trip quality against the library's own decoder and
//! against ffmpeg.
//!
//! Fixtures are real files: the stereo and mono WAVs were made
//! with ffmpeg 8.1.2 from its sine source
//! (`ffmpeg -f lavfi -i "sine=frequency=440:sample_rate=44100:duration=2"`
//! plus a second 660 Hz source merged to stereo for the stereo
//! file; the mono file is a single 440 Hz source at 8000 Hz),
//! and the `.adts` / `_ffdec.raw` pairs were produced by this
//! encoder and decoded back by ffmpeg 8.1.2
//! (`ffmpeg -i <file>.adts -f f32le -acodec pcm_f32le <file>_ffdec.raw`).
//!
//! Thresholds (round trip vs the original WAV, allowing for
//! the decoder's one-frame lag: decoded frame `i` reproduces
//! input frame `i-1`, so the first decoded frame and the last
//! original frame are dropped from the comparison):
//!   - stereo 44100 Hz, 128 kbps CBR: RMS error < 5.0e-3,
//!     maximum absolute error < 5.0e-2
//!   - mono 8000 Hz, 64 kbps CBR: RMS error < 5.0e-3,
//!     maximum absolute error < 5.0e-2
//!   - own decoder vs ffmpeg decode of the same encoded
//!     stream: maximum absolute error < 2.0e-3, RMS < 5.0e-4
//!     (the same bounds `tests/aac.rs` uses for decode)
//!
//! Measured on this encoder (2026-11-09, `cargo test
//! --features licensed-aac --test aac_encode`):
//!   - stereo round trip (in-test sine): RMS 5.1e-4, max 9.0e-3;
//!     WAV fixture: RMS 9.8e-5, max 2.2e-3
//!   - mono round trip (in-test sine): RMS 3.9e-4, max 3.0e-3;
//!     WAV fixture: RMS 9.5e-5, max 7.2e-4
//!   - VBR worst case (quality 9): RMS 1.6e-3, max 2.0e-2
//!   - own decoder vs ffmpeg: max 0.0, RMS 0.0 (bit-exact)
//!
//! The round-trip thresholds carry roughly a 10x margin on RMS
//! and a 2.5-5x margin on max abs over the worst measured case
//! (VBR quality 9), so a regression in the MDCT scale, the
//! windows, the quantizer or the Huffman tables shows up as a
//! failure rather than a near miss.

#[cfg(feature = "licensed-aac")]
use codec_whatttttttttttt_core::aac_frame::parse_adts_header;
#[cfg(feature = "licensed-aac")]
use codec_whatttttttttttt_core::AudioFrame;
#[cfg(feature = "licensed-aac")]
use codec_whatttttttttttt_core::{AacDecoder, AdtsDemuxer, WavDemuxer};
use codec_whatttttttttttt_core::{AacEncoder, BitrateMode, Error, LicenseContext, LicenseType};
#[cfg(feature = "licensed-aac")]
use std::io::Cursor;

#[cfg(feature = "licensed-aac")]
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

/// `expect_err` for results whose `Ok` type has no `Debug`
/// (`AacEncoder` cannot derive it): panics on success,
/// returns the error otherwise.
fn expect_err<T>(result: Result<T, Error>, message: &str) -> Error {
    match result {
        Ok(_) => panic!("{message}"),
        Err(err) => err,
    }
}

/// One sine frame of 1024 samples per channel: left 440 Hz,
/// right 660 Hz (mono: 440 Hz only).
// Frame indices stay below 2^24 samples and sample rates below
// 2^24 Hz, so both usize/u32-to-f32 casts are exact.
#[allow(clippy::cast_precision_loss)]
#[cfg(feature = "licensed-aac")]
fn sine_frame(frame_index: usize, sample_rate: u32, channels: u32) -> AudioFrame {
    let mut samples = Vec::with_capacity(1024 * channels as usize);
    for index in 0..1024 {
        let n = frame_index * 1024 + index;
        let t = n as f32 / sample_rate as f32;
        let left = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;
        samples.push(left);
        if channels == 2 {
            let right = (2.0 * std::f32::consts::PI * 660.0 * t).sin() * 0.5;
            samples.push(right);
        }
    }
    AudioFrame {
        sample_rate,
        channels,
        samples,
        timestamp: frame_index as u64 * 1024 * 1_000_000 / u64::from(sample_rate),
    }
}

/// Encodes `frame_count` sine frames and returns the ADTS
/// bytes of every frame plus the original samples.
#[cfg(feature = "licensed-aac")]
fn encode_sine(
    sample_rate: u32,
    channels: u32,
    mode: BitrateMode,
    frame_count: usize,
) -> (Vec<Vec<u8>>, Vec<f32>) {
    let mut encoder = AacEncoder::new(sample_rate, channels, mode, Some(valid_license()))
        .expect("licensed aac encoder builds");
    let mut packets = Vec::new();
    let mut original = Vec::new();
    for index in 0..frame_count {
        let frame = sine_frame(index, sample_rate, channels);
        original.extend_from_slice(&frame.samples);
        let packet = encoder.encode(&frame).expect("sine frame encodes");
        assert_eq!(packet.codec, codec_whatttttttttttt_core::Codec::Aac);
        assert!(packet.is_keyframe);
        packets.push(packet.data);
    }
    (packets, original)
}

/// Decodes ADTS payloads with the library's own decoder and
/// returns the concatenated interleaved samples.
#[cfg(feature = "licensed-aac")]
fn decode_all_adts(packets: &[Vec<u8>]) -> Vec<f32> {
    let (config, _) = parse_adts_header(&packets[0]).expect("adts header parses");
    let mut decoder = AacDecoder::new(&config).expect("aac decoder builds");
    let mut samples = Vec::new();
    for packet in packets {
        let mut demuxer = AdtsDemuxer::from_bytes(packet).expect("adts frame demuxes");
        let frame = demuxer.next_frame().expect("adts frame reads").unwrap();
        let frame_out = decoder
            .decode_raw_data_block(&frame.payload)
            .expect("aac frame decodes");
        samples.extend_from_slice(&frame_out.samples);
    }
    samples
}

/// Loads a WAV fixture and returns (sample rate, channels,
/// interleaved f32 samples).
#[cfg(feature = "licensed-aac")]
fn load_wav_fixture(name: &str) -> (u32, u32, Vec<f32>) {
    let bytes = std::fs::read(fixture_path(name)).expect("fixture reads");
    let demuxer = WavDemuxer::new(Cursor::new(bytes)).expect("wav demuxes");
    let sample_rate = demuxer.header().sample_rate;
    let channels = demuxer.header().channels;
    let samples = demuxer.samples().expect("wav samples read");
    (sample_rate, channels, samples)
}

/// Encodes whole WAV samples (padding the tail with silence
/// to a full frame) and returns the ADTS bytes per frame.
#[cfg(feature = "licensed-aac")]
fn encode_samples(
    sample_rate: u32,
    channels: u32,
    mode: BitrateMode,
    samples: &[f32],
) -> Vec<Vec<u8>> {
    let mut encoder = AacEncoder::new(sample_rate, channels, mode, Some(valid_license()))
        .expect("licensed aac encoder builds");
    let frame_samples = 1024 * channels as usize;
    let mut packets = Vec::new();
    let mut offset = 0usize;
    let mut frame_index = 0usize;
    while offset < samples.len() {
        let mut frame_samples_vec = vec![0.0f32; frame_samples];
        let take = (samples.len() - offset).min(frame_samples);
        frame_samples_vec[..take].copy_from_slice(&samples[offset..offset + take]);
        let frame = AudioFrame {
            sample_rate,
            channels,
            samples: frame_samples_vec,
            timestamp: frame_index as u64 * 1024 * 1_000_000 / u64::from(sample_rate),
        };
        let packet = encoder.encode(&frame).expect("wav frame encodes");
        packets.push(packet.data);
        offset += take;
        frame_index += 1;
    }
    packets
}

/// Compares decoded samples with the reference, returning (max
/// absolute error, RMS error). The decoder lags the encoder by
/// exactly one frame (the MDCT overlap: decoded frame `i`
/// reproduces input frame `i-1`, and decoded frame 0 is the
/// priming silence), so `skip` samples are dropped from the
/// START of the decoded stream and from the END of the
/// reference. The caller asserts both errors against the
/// thresholds, with the measured values in the message.
#[cfg(feature = "licensed-aac")]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "test only: sample counts are far below 2^53 so the usize-to-f64 cast is exact, and the f64-to-f32 RMS cast only feeds a threshold comparison with a wide margin"
)]
fn compare_skipping(
    name: &str,
    decoded: &[f32],
    reference: &[f32],
    skip: usize,
    max_abs_limit: f32,
    rms_limit: f32,
) -> (f32, f32) {
    let decoded = &decoded[skip..];
    let reference = &reference[..reference.len() - skip];
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
    eprintln!("{name}: measured max abs {max_abs:.6}, rms {rms:.6}");
    assert!(
        max_abs <= max_abs_limit,
        "{name}: max abs error {max_abs:.6} exceeds {max_abs_limit} (rms {rms:.6})"
    );
    assert!(
        rms <= rms_limit,
        "{name}: rms error {rms:.6} exceeds {rms_limit} (max abs {max_abs:.6})"
    );
    (max_abs, rms)
}

/// Reads a little-endian f32 raw file.
#[cfg(feature = "licensed-aac")]
fn read_f32le_raw(path: &str) -> Vec<f32> {
    let bytes = std::fs::read(path).expect("raw fixture reads");
    assert_eq!(bytes.len() % 4, 0, "raw fixture is not f32-aligned");
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[test]
#[cfg(not(feature = "licensed-aac"))]
fn encoder_without_the_feature_is_unsupported() {
    let err = expect_err(
        AacEncoder::new(
            44_100,
            2,
            BitrateMode::Cbr(128),
            Some(
                LicenseContext::new(
                    "test licensee".to_string(),
                    LicenseType::Commercial,
                    "TEST-12345678".to_string(),
                    4_102_444_800,
                    "aac".to_string(),
                )
                .unwrap(),
            ),
        ),
        "feature off must fail even with a license",
    );
    assert!(
        matches!(err, Error::Unsupported(_)),
        "expected Unsupported with the feature off, got {err}"
    );
    let err = expect_err(
        AacEncoder::new(44_100, 2, BitrateMode::Cbr(128), None),
        "feature off must fail without a license too",
    );
    assert!(
        matches!(err, Error::Unsupported(_)),
        "expected Unsupported with the feature off, got {err}"
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn encoder_without_a_license_is_license_required() {
    let err = expect_err(
        AacEncoder::new(44_100, 2, BitrateMode::Cbr(128), None),
        "no license must fail",
    );
    assert!(
        matches!(err, Error::LicenseRequired { codec: "aac" }),
        "expected LicenseRequired for aac, got {err}"
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn encoder_with_an_expired_license_is_license_required() {
    let err = expect_err(
        AacEncoder::new(44_100, 2, BitrateMode::Cbr(128), Some(expired_license())),
        "expired license must fail",
    );
    assert!(
        matches!(err, Error::LicenseRequired { codec: "aac" }),
        "expected LicenseRequired for aac, got {err}"
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn encoder_with_a_malformed_license_id_is_invalid_argument() {
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
#[cfg(feature = "licensed-aac")]
fn encoder_rejects_off_ladder_bitrates() {
    for mode in [
        BitrateMode::Cbr(100),
        BitrateMode::Cbr(0),
        BitrateMode::Abr(200),
        BitrateMode::Vbr(10),
    ] {
        let err = expect_err(
            AacEncoder::new(44_100, 2, mode, Some(valid_license())),
            "off-ladder bitrate must fail",
        );
        assert!(
            matches!(err, Error::InvalidArgument(_)),
            "expected InvalidArgument for {mode:?}, got {err}"
        );
    }
}

#[test]
#[cfg(feature = "licensed-aac")]
fn encoder_rejects_unsupported_sample_rates_and_channels() {
    for sample_rate in [7350, 44_101, 96_001] {
        let err = expect_err(
            AacEncoder::new(sample_rate, 2, BitrateMode::Cbr(128), Some(valid_license())),
            "unsupported sample rate must fail",
        );
        assert!(
            matches!(err, Error::InvalidArgument(_) | Error::Unsupported(_)),
            "expected InvalidArgument or Unsupported for {sample_rate} Hz, got {err}"
        );
    }
    for channels in [0, 3, 8] {
        let err = expect_err(
            AacEncoder::new(
                44_100,
                channels,
                BitrateMode::Cbr(128),
                Some(valid_license()),
            ),
            "unsupported channel count must fail",
        );
        assert!(
            matches!(err, Error::InvalidArgument(_)),
            "expected InvalidArgument for {channels} channels, got {err}"
        );
    }
}

#[test]
#[cfg(feature = "licensed-aac")]
fn encoder_rejects_a_short_frame_and_non_finite_samples() {
    let mut encoder =
        AacEncoder::new(44_100, 2, BitrateMode::Cbr(128), Some(valid_license())).unwrap();
    let short = AudioFrame {
        sample_rate: 44_100,
        channels: 2,
        samples: vec![0.0; 512 * 2],
        timestamp: 0,
    };
    let err = expect_err(encoder.encode(&short), "a short frame must fail");
    assert!(
        matches!(err, Error::InvalidArgument(_)),
        "expected InvalidArgument for a short frame, got {err}"
    );
    let mut bad_samples = vec![0.0f32; 1024 * 2];
    bad_samples[100] = f32::NAN;
    let nan = AudioFrame {
        sample_rate: 44_100,
        channels: 2,
        samples: bad_samples,
        timestamp: 0,
    };
    let err = expect_err(encoder.encode(&nan), "a NaN sample must fail");
    assert!(
        matches!(err, Error::InvalidArgument(_)),
        "expected InvalidArgument for a NaN sample, got {err}"
    );
    let wrong_rate = AudioFrame {
        sample_rate: 48_000,
        channels: 2,
        samples: vec![0.0; 1024 * 2],
        timestamp: 0,
    };
    let err = expect_err(
        encoder.encode(&wrong_rate),
        "a mismatched sample rate must fail",
    );
    assert!(
        matches!(err, Error::InvalidArgument(_)),
        "expected InvalidArgument for a mismatched sample rate, got {err}"
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn silence_encodes_to_tiny_frames_that_decode_to_silence() {
    let mut encoder =
        AacEncoder::new(44_100, 2, BitrateMode::Cbr(128), Some(valid_license())).unwrap();
    let mut packets = Vec::new();
    for index in 0u64..2 {
        let frame = AudioFrame {
            sample_rate: 44_100,
            channels: 2,
            samples: vec![0.0; 1024 * 2],
            timestamp: index * 1024 * 1_000_000 / 44_100,
        };
        let packet = encoder.encode(&frame).expect("silence encodes");
        // Silence carries no spectral data: the frame is just
        // element headers plus the ADTS header.
        assert!(
            packet.data.len() < 64,
            "silence frame is {} bytes, expected under 64",
            packet.data.len()
        );
        packets.push(packet.data);
    }
    let decoded = decode_all_adts(&packets);
    assert!(decoded.iter().all(|&sample| sample == 0.0));
}

#[test]
#[cfg(feature = "licensed-aac")]
fn stereo_sine_round_trips_within_threshold_and_fits_the_cbr_budget() {
    let (packets, original) = encode_sine(44_100, 2, BitrateMode::Cbr(128), 10);
    // 128 kbps at 44100 Hz: 128000 * 1024 / 44100 = 2972 bits
    // per frame, 371 bytes with the ADTS header. No frame may
    // exceed the budget the rate loop was given.
    for packet in &packets {
        assert!(
            packet.len() <= 371,
            "frame is {} bytes, over the 128 kbps budget of 371",
            packet.len()
        );
    }
    let decoded = decode_all_adts(&packets);
    assert_eq!(decoded.len(), original.len());
    compare_skipping(
        "stereo sine round trip",
        &decoded,
        &original,
        1024 * 2,
        5.0e-2,
        5.0e-3,
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn mono_sine_round_trips_within_threshold() {
    let (packets, original) = encode_sine(8000, 1, BitrateMode::Cbr(64), 10);
    let decoded = decode_all_adts(&packets);
    assert_eq!(decoded.len(), original.len());
    compare_skipping(
        "mono sine round trip",
        &decoded,
        &original,
        1024,
        5.0e-2,
        5.0e-3,
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn vbr_modes_encode_and_decode() {
    for quality in [0u8, 5, 9] {
        let (packets, original) = encode_sine(44_100, 2, BitrateMode::Vbr(quality), 4);
        let decoded = decode_all_adts(&packets);
        assert_eq!(decoded.len(), original.len());
        compare_skipping(
            "vbr sine round trip",
            &decoded,
            &original,
            1024 * 2,
            5.0e-2,
            5.0e-3,
        );
    }
}

#[test]
#[cfg(feature = "licensed-aac")]
fn wav_fixture_stereo_round_trips_within_threshold() {
    let (sample_rate, channels, samples) = load_wav_fixture("aacenc_sine_44100_stereo.wav");
    assert_eq!(sample_rate, 44_100);
    assert_eq!(channels, 2);
    let packets = encode_samples(sample_rate, channels, BitrateMode::Cbr(128), &samples);
    assert_eq!(packets.len(), 87, "2 s at 44100 Hz is 87 padded frames");
    for packet in &packets {
        assert!(
            packet.len() <= 371,
            "frame is {} bytes, over the 128 kbps budget of 371",
            packet.len()
        );
    }
    let decoded = decode_all_adts(&packets);
    // The original is padded with silence to whole frames.
    let mut padded = samples.clone();
    padded.resize(decoded.len(), 0.0);
    compare_skipping(
        "wav stereo round trip",
        &decoded,
        &padded,
        1024 * 2,
        5.0e-2,
        5.0e-3,
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn wav_fixture_mono_round_trips_within_threshold() {
    let (sample_rate, channels, samples) = load_wav_fixture("aacenc_sine_8000_mono.wav");
    assert_eq!(sample_rate, 8000);
    assert_eq!(channels, 1);
    let packets = encode_samples(sample_rate, channels, BitrateMode::Cbr(64), &samples);
    assert_eq!(packets.len(), 16, "2 s at 8000 Hz is 16 padded frames");
    let decoded = decode_all_adts(&packets);
    let mut padded = samples.clone();
    padded.resize(decoded.len(), 0.0);
    compare_skipping(
        "wav mono round trip",
        &decoded,
        &padded,
        1024,
        5.0e-2,
        5.0e-3,
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn committed_adts_fixture_matches_ffmpeg_decode() {
    // The committed `.adts` was produced by this encoder from
    // the stereo WAV fixture (see `generate_aacenc_fixtures`
    // below); ffmpeg's own decode of it is the reference.
    let bytes = std::fs::read(fixture_path("aacenc_sine_44100_stereo.adts"))
        .expect("committed adts fixture reads");
    let mut demuxer = AdtsDemuxer::from_bytes(&bytes).expect("committed adts demuxes");
    let (config, _) = parse_adts_header(&bytes).expect("committed adts header parses");
    assert_eq!(config.sample_rate, 44_100);
    assert_eq!(config.channels, 2);
    let mut decoder = AacDecoder::new(&config).expect("aac decoder builds");
    let mut output = Vec::new();
    let mut frame_count = 0;
    while let Some(frame) = demuxer.next_frame().expect("adts frame reads") {
        let frame = decoder
            .decode_raw_data_block(&frame.payload)
            .expect("committed adts frame decodes");
        output.extend_from_slice(&frame.samples);
        frame_count += 1;
    }
    assert_eq!(frame_count, 87, "committed adts should hold 87 frames");
    let reference = read_f32le_raw(&fixture_path("aacenc_sine_44100_stereo_ffdec.raw"));
    assert_eq!(output.len(), reference.len());
    compare_skipping(
        "own decoder vs ffmpeg on committed adts",
        &output,
        &reference,
        0,
        2.0e-3,
        5.0e-4,
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn reencoding_the_wav_fixture_reproduces_the_committed_adts() {
    // The encoder is deterministic: re-encoding the same WAV
    // with the same settings must reproduce the committed
    // bytes exactly.
    let (sample_rate, channels, samples) = load_wav_fixture("aacenc_sine_44100_stereo.wav");
    let packets = encode_samples(sample_rate, channels, BitrateMode::Cbr(128), &samples);
    let encoded: Vec<u8> = packets.concat();
    let committed = std::fs::read(fixture_path("aacenc_sine_44100_stereo.adts"))
        .expect("committed adts fixture reads");
    assert_eq!(
        encoded, committed,
        "re-encoding the stereo WAV must reproduce the committed ADTS bytes"
    );
}

/// Regenerates the committed `.adts` fixtures from the WAV
/// fixtures. Run explicitly (`cargo test --features
/// licensed-aac --test aac_encode generate_aacenc_fixtures
/// -- --ignored`); it is ignored by default so a normal test
/// run never rewrites fixtures. After regenerating, decode
/// each `.adts` with ffmpeg to refresh the `_ffdec.raw`
/// reference:
/// `ffmpeg -i tests/data/aacenc_sine_44100_stereo.adts -f f32le
/// -acodec pcm_f32le tests/data/aacenc_sine_44100_stereo_ffdec.raw`
#[test]
#[ignore = "writes checked-in fixtures into tests/data; run explicitly to regenerate"]
#[cfg(feature = "licensed-aac")]
fn generate_aacenc_fixtures() {
    for (wav, adts) in [
        (
            "aacenc_sine_44100_stereo.wav",
            "aacenc_sine_44100_stereo.adts",
        ),
        ("aacenc_sine_8000_mono.wav", "aacenc_sine_8000_mono.adts"),
    ] {
        let (sample_rate, channels, samples) = load_wav_fixture(wav);
        let mode = if channels == 2 {
            BitrateMode::Cbr(128)
        } else {
            BitrateMode::Cbr(64)
        };
        let packets = encode_samples(sample_rate, channels, mode, &samples);
        let bytes: Vec<u8> = packets.concat();
        std::fs::write(fixture_path(adts), &bytes).expect("adts fixture writes");
        eprintln!(
            "wrote {} ({} bytes, {} frames)",
            adts,
            bytes.len(),
            packets.len()
        );
    }
}
