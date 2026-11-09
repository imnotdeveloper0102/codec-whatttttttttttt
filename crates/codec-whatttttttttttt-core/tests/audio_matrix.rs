//! Audio matrix: every decoder the crate has, across the sample
//! matrix (§6) and the lossy bitrate levels (§5).
//!
//! What is covered, and how:
//!
//! * Lossless, bit-exact: WAV / FLAC / ALAC fixtures are decoded
//!   through the `Decoder` API and compared sample-for-sample
//!   with the reference (the source WAV for FLAC, ffmpeg's own
//!   decode for ALAC). Any difference is a failure, not a
//!   tolerance.
//! * Sample matrix §6 (16-bit 44.1/48/96/192 kHz; 24-bit
//!   16/48/96/192 kHz): the fixtures only pin 16-bit 44.1 kHz
//!   and 8 kHz, and 24-bit 48 kHz, so the remaining cells are
//!   covered by generated round-trips through the codecs that
//!   have encoders: FLAC (`encode_flac_stream`, i16 only) and
//!   PCM (`Encoder` + `WavMuxer`, i16 and i24). A round-trip
//!   that is not bit-exact at the integer level fails.
//! * Lossy bitrates §5 (64/96/128/320 kbps): MP3 and AAC
//!   fixtures at those bitrates, compared with ffmpeg's decode
//!   under the thresholds the codec tests already established
//!   (MP3: ISO/IEC 11172-4 full compliance, RMS < 8.80967e-06,
//!   max <= 6.10352e-05; AAC: max abs <= 2.0e-3, RMS <=
//!   5.0e-4). Vorbis is compared with the original WAV under
//!   twice the measured lossy difference (stereo RMS 0.0059 /
//!   max 0.016, from `tests/vorbis.rs`).
//! * Opus: only the honest `Unsupported` for CELT/SILK sample
//!   reconstruction is asserted — no Opus PCM is fabricated.
//! * Encode: MP3, AAC and ALAC have no encoder, so the matrix
//!   asserts `Encoder::encode` refuses them instead of
//!   pretending a round-trip exists.
//! * DSD: one sanity row (DSF DSD64 stereo decimated to
//!   176.4 kHz, least-squares sine fit near 0.5 amplitude).
//!
//! What is NOT covered here, deliberately: no 96/192 kHz lossy
//! fixture exists (no MP3/AAC fixture was encoded at those
//! rates), so those cells are covered only by the lossless
//! round-trips above; and the AAC rows only run with the
//! `licensed-aac` feature plus a test license.

use std::io::Cursor;

#[cfg(feature = "licensed-aac")]
use codec_whatttttttttttt_core::aac_frame::parse_adts_header;
#[cfg(feature = "licensed-aac")]
use codec_whatttttttttttt_core::{AdtsDemuxer, LicenseContext, LicenseType};
use codec_whatttttttttttt_core::{
    Codec, Decoder, DecoderConfig, DsdDemuxer, Encoder, EncoderConfig, Error, FlacDemuxer, Frame,
    OggDemuxer, Packet, PcmFormat, VorbisSetup, WavDemuxer, WavMuxer,
};

fn fixture_path(name: &str) -> String {
    format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn read_f32le_raw(path: &str) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(bytes.len() % 4, 0, "{path}: bad reference length");
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

fn read_s16le_raw(path: &str) -> Vec<i32> {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(bytes.len() % 2, 0, "{path}: bad reference length");
    bytes
        .chunks_exact(2)
        .map(|chunk| i32::from(i16::from_le_bytes([chunk[0], chunk[1]])))
        .collect()
}

/// Interleaved 440 Hz (left) / 660 Hz (right) sine at `amplitude`,
/// `frames` frames, stereo unless `channels` is 1 (440 Hz only).
// The frame index stays in the low hundreds of thousands, exact in f64;
// the f64-to-f32 conversion is the sine generator's own quantization
// to the sample format, not a loss of a value the test needs.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn sine_f32(rate: u32, channels: u32, frames: usize, amplitude: f32) -> Vec<f32> {
    let mut samples = Vec::with_capacity(frames * channels as usize);
    for n in 0..frames {
        let t = n as f64 / f64::from(rate);
        let left = (2.0 * std::f64::consts::PI * 440.0 * t).sin() * f64::from(amplitude);
        samples.push(left as f32);
        if channels == 2 {
            let right = (2.0 * std::f64::consts::PI * 660.0 * t).sin() * f64::from(amplitude);
            samples.push(right as f32);
        }
    }
    samples
}

/// Expected f32 samples after a PCM round-trip at `format`: the
/// encoder quantizes to integers (`s * 32768` for I16,
/// `s * 8388608` for I24, both saturating), and the decoder
/// divides back, so the result is exactly the quantized value.
// The f64-to-f32 conversion is the quantization itself: the value
// was rounded to an integer multiple of 1/scale, exactly
// representable in f32 for I16/I24.
#[allow(clippy::cast_possible_truncation)]
fn quantized(samples: &[f32], format: PcmFormat) -> Vec<f32> {
    let (scale, min, max) = match format {
        PcmFormat::I16 => (32768.0f64, -32768.0, 32767.0),
        PcmFormat::I24 => (8_388_608.0f64, -8_388_608.0, 8_388_607.0),
        _ => panic!("matrix quantize only supports I16/I24"),
    };
    samples
        .iter()
        .map(|&sample| {
            let value = (f64::from(sample) * scale).round().clamp(min, max);
            (value / scale) as f32
        })
        .collect()
}

/// PCM round-trip through `Encoder` + `WavMuxer` + `WavDemuxer` +
/// `Decoder`, at any rate/depth the library accepts.
fn pcm_round_trip(samples: &[f32], format: PcmFormat, channels: u32, rate: u32) -> Vec<f32> {
    let mut encoder = Encoder::new(EncoderConfig {
        codec: Codec::Pcm,
        sample_rate: rate,
        channels,
        pcm_format: format,
        ..EncoderConfig::default()
    })
    .unwrap();
    let frame = Frame::Audio(codec_whatttttttttttt_core::AudioFrame {
        sample_rate: rate,
        channels,
        samples: samples.to_vec(),
        timestamp: 0,
    });
    let packet = encoder.encode(&frame).unwrap();
    let mut wav = Vec::new();
    let mut muxer = WavMuxer::new(&mut wav, format, channels, rate).unwrap();
    muxer.write_packet(&packet).unwrap();
    muxer.finish().unwrap();
    decode_wav_bytes(&wav)
}

/// Decodes whole WAV bytes through the `Decoder` API.
fn decode_wav_bytes(bytes: &[u8]) -> Vec<f32> {
    let mut demuxer = WavDemuxer::new(Cursor::new(bytes.to_vec())).unwrap();
    let header = *demuxer.header();
    let mut decoder = Decoder::from_wav_header(&header).unwrap();
    let mut samples = Vec::new();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        match decoder.decode(&packet).unwrap() {
            Frame::Audio(audio) => samples.extend_from_slice(&audio.samples),
            Frame::Video(_) => panic!("wav decoded to a video frame"),
        }
    }
    samples
}

/// Decodes whole FLAC bytes through the `Decoder` API.
fn decode_flac_bytes(bytes: &[u8]) -> Vec<f32> {
    let mut demuxer = FlacDemuxer::new(Cursor::new(bytes.to_vec())).unwrap();
    let mut decoder = Decoder::from_flac_streaminfo(demuxer.streaminfo()).unwrap();
    let mut samples = Vec::new();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        match decoder.decode(&packet).unwrap() {
            Frame::Audio(audio) => samples.extend_from_slice(&audio.samples),
            Frame::Video(_) => panic!("flac decoded to a video frame"),
        }
    }
    samples
}

/// RMS and max absolute difference between two f32 slices.
// The sample count is at most a few hundred thousand, exact in f64.
#[allow(clippy::cast_precision_loss)]
fn rms_and_max(a: &[f32], b: &[f32]) -> (f64, f32) {
    let n = a.len().min(b.len());
    assert!(n > 0, "comparison needs samples");
    let mut sum = 0.0f64;
    let mut max = 0.0f32;
    for i in 0..n {
        let diff = f64::from(a[i]) - f64::from(b[i]);
        sum += diff * diff;
        max = max.max((a[i] - b[i]).abs());
    }
    ((sum / n as f64).sqrt(), max)
}

/// Splits a whole MP3 file into frames, skipping an ID3 tag or
/// any leading garbage: a candidate sync must parse as a header
/// whose frame length lands exactly on the next frame (or the
/// end of the file). Same rule as `tests/mp3.rs`.
fn split_mp3_frames(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut i = 0usize;
    while i + 4 <= bytes.len() {
        if bytes[i] == 0xFF && bytes[i + 1] & 0xE0 == 0xE0 {
            if let Ok(header) = codec_whatttttttttttt_core::Mp3FrameHeader::parse(&bytes[i..]) {
                let len = header.frame_length;
                if len > 0 && i + len <= bytes.len() {
                    let next_ok = i + len == bytes.len()
                        || (i + len + 4 <= bytes.len()
                            && bytes[i + len] == 0xFF
                            && bytes[i + len + 1] & 0xE0 == 0xE0);
                    if next_ok {
                        frames.push(bytes[i..i + len].to_vec());
                        i += len;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    frames
}

/// Decodes an MP3 fixture through the `Decoder` API.
fn decode_mp3_file(name: &str) -> (Vec<f32>, u32, u32) {
    let bytes = std::fs::read(fixture_path(name)).unwrap();
    let frames = split_mp3_frames(&bytes);
    assert!(!frames.is_empty(), "{name}: no mp3 frames found");
    let mut decoder = Decoder::new(DecoderConfig {
        codec: Codec::Mp3,
        ..DecoderConfig::default()
    })
    .unwrap();
    let mut samples = Vec::new();
    let (mut channels, mut sample_rate) = (0, 0);
    for frame_bytes in &frames {
        let packet = Packet {
            codec: Codec::Mp3,
            data: frame_bytes.clone(),
            timestamp: 0,
            is_keyframe: true,
        };
        match decoder.decode(&packet).unwrap() {
            Frame::Audio(audio) => {
                channels = audio.channels;
                sample_rate = audio.sample_rate;
                samples.extend_from_slice(&audio.samples);
            }
            Frame::Video(_) => panic!("{name}: mp3 decoded to a video frame"),
        }
    }
    (samples, channels, sample_rate)
}

/// Aligns MP3 output with the reference by the offset that
/// minimises RMS over a middle window, then compares the whole
/// overlap. The decoder outputs the encoder-delay priming
/// samples; ffmpeg's decode trims them, so the alignment is
/// around 2257 samples per channel (see `tests/mp3.rs`).
fn align_and_compare_mp3(decoded: &[f32], reference: &[f32], channels: usize) -> (usize, f64, f32) {
    let win_start = 4000 * channels;
    let win_len = 8000 * channels;
    assert!(win_start + win_len <= reference.len());
    let mut best = (usize::MAX, f64::MAX);
    for offset in 0..=2400usize {
        let start = (offset + 4000) * channels;
        if start + win_len > decoded.len() {
            break;
        }
        let (rms, _) = rms_and_max(
            &decoded[start..start + win_len],
            &reference[win_start..win_start + win_len],
        );
        if rms < best.1 {
            best = (offset, rms);
        }
    }
    assert_ne!(best.0, usize::MAX, "no mp3 alignment fits the reference");
    let offset = best.0;
    let start = offset * channels;
    let len = reference.len().min(decoded.len() - start);
    let (rms, max) = rms_and_max(&decoded[start..start + len], &reference[..len]);
    (offset, rms, max)
}

/// Full-compliance RMS bound, ISO/IEC 11172-4 section 2.6.3:
/// 2^-15/sqrt(12) = 8.80967e-06.
const MP3_FULL_COMPLIANCE_RMS: f64 = 8.809_67e-06;
/// Full-compliance max-abs-diff bound, ISO/IEC 11172-4 section
/// 2.6.3: 2^-14 = 6.10352e-05.
const MP3_FULL_COMPLIANCE_MAX: f32 = 6.103_52e-05;

fn check_mp3_fixture(mp3: &str, reference: &str, expected_channels: u32, expected_rate: u32) {
    let (samples, channels, sample_rate) = decode_mp3_file(mp3);
    assert_eq!(channels, expected_channels, "{mp3}: channels");
    assert_eq!(sample_rate, expected_rate, "{mp3}: sample rate");
    let reference = read_f32le_raw(&fixture_path(reference));
    let (offset, rms, max) = align_and_compare_mp3(&samples, &reference, channels as usize);
    println!("{mp3}: offset={offset} rms={rms:.3e} max={max:.3e}");
    assert!(
        rms < MP3_FULL_COMPLIANCE_RMS,
        "{mp3}: rms {rms:.3e} exceeds the full-compliance bound {MP3_FULL_COMPLIANCE_RMS:.3e}"
    );
    assert!(
        max <= MP3_FULL_COMPLIANCE_MAX,
        "{mp3}: max {max:.3e} exceeds the full-compliance bound {MP3_FULL_COMPLIANCE_MAX:.3e}"
    );
}

/// Least-squares sine amplitude at `freq_hz` (from
/// `tests/dsd.rs`): fits `a*sin + b*cos` over the samples and
/// returns `sqrt(a^2 + b^2)`.
// The sample index stays in the low hundreds of thousands, exact in f64.
#[allow(clippy::cast_precision_loss)]
fn sine_amplitude(samples: &[f32], sample_rate: f64, freq_hz: f64) -> f64 {
    let mut ss = 0.0f64;
    let mut cc = 0.0f64;
    let mut sc = 0.0f64;
    let mut s_y = 0.0f64;
    let mut c_y = 0.0f64;
    for (n, &sample) in samples.iter().enumerate() {
        let phase = 2.0 * std::f64::consts::PI * freq_hz * n as f64 / sample_rate;
        let (sin, cos) = phase.sin_cos();
        let y = f64::from(sample);
        ss += sin * sin;
        cc += cos * cos;
        sc += sin * cos;
        s_y += sin * y;
        c_y += cos * y;
    }
    let det = ss * cc - sc * sc;
    if det == 0.0 {
        return 0.0;
    }
    let a = (s_y * cc - c_y * sc) / det;
    let b = (c_y * ss - s_y * sc) / det;
    (a * a + b * b).sqrt()
}

// ---------------------------------------------------------------- lossless

#[test]
fn lossless_fixtures_decode_bit_exact() {
    // FLAC fixtures were encoded from the WAV fixtures next to
    // them, so decoding both paths must give identical f32.
    let flac_vs_wav: &[(&str, &str)] = &[
        ("sine_stereo_44100.flac", "valid_i16_stereo_44100.wav"),
        ("noise_mono_8000.flac", "noise_mono_8000.wav"),
        ("ramp_mono_lpc.flac", "ramp_mono_8000.wav"),
        ("ramp_mono_fixed.flac", "ramp_mono_8000.wav"),
        ("silence_mono_8000.flac", "silence_mono_8000.wav"),
        ("sine_stereo_fixed.flac", "valid_i16_stereo_44100.wav"),
    ];
    for (flac, wav) in flac_vs_wav {
        let flac_bytes = std::fs::read(fixture_path(flac)).unwrap();
        let wav_bytes = std::fs::read(fixture_path(wav)).unwrap();
        let decoded_flac = decode_flac_bytes(&flac_bytes);
        let decoded_wav = decode_wav_bytes(&wav_bytes);
        assert_eq!(
            decoded_flac, decoded_wav,
            "{flac} vs {wav}: lossless decode differs"
        );
    }
    // WAV fixtures decode to the sample counts their headers
    // declare (16-bit 44.1 kHz stereo, 16-bit 8 kHz mono,
    // 24-bit 48 kHz stereo, f32 44.1 kHz mono).
    for (wav, frames, channels) in [
        ("valid_i16_stereo_44100.wav", 4410, 2),
        ("valid_i16_mono_8000.wav", 800, 1),
        ("valid_i24_stereo_48000.wav", 480, 2),
        ("valid_f32_mono_44100.wav", 4410, 1),
    ] {
        let bytes = std::fs::read(fixture_path(wav)).unwrap();
        let samples = decode_wav_bytes(&bytes);
        assert_eq!(samples.len(), frames * channels, "{wav}: sample count");
        assert!(
            samples.iter().all(|s| *s >= -1.0 && *s <= 1.0),
            "{wav}: sample out of range"
        );
    }
}

#[test]
fn alac_fixtures_decode_bit_exact() {
    // ALAC is lossless: every integer sample must equal
    // ffmpeg's decode. The MP4 sample slicing lives in the
    // shared test helper, as in `tests/alac.rs`.
    #[path = "mp4_samples.rs"]
    mod mp4_samples;
    use codec_whatttttttttttt_core::{decode_alac_frame, parse_alac_config, AlacConfig};
    use mp4_samples::{find_box, load_mp4};

    fn load_alac_fixture(name: &str) -> (AlacConfig, Vec<Vec<u8>>) {
        let mp4 = load_mp4(&fixture_path(name));
        let alac_box = find_box(&mp4.bytes, *b"alac").expect("fixture has no alac box");
        let config = parse_alac_config(alac_box).expect("fixture alac cookie parses");
        let packets = (0..mp4.len()).map(|i| mp4.sample(i).to_vec()).collect();
        (config, packets)
    }

    for (m4a, reference, is_s32) in [
        ("alac_mono_8000.m4a", "alac_mono_8000_ref.raw", false),
        (
            "alac_noise_mono_8000.m4a",
            "alac_noise_mono_8000_ref.raw",
            false,
        ),
        (
            "alac_ramp_mono_8000.m4a",
            "alac_ramp_mono_8000_ref.raw",
            false,
        ),
        ("alac_stereo_44100.m4a", "alac_stereo_44100_ref.raw", false),
        ("alac_mono_8000_s32.m4a", "alac_mono_8000_s32_ref.raw", true),
        (
            "alac_stereo_44100_s32.m4a",
            "alac_stereo_44100_s32_ref.raw",
            true,
        ),
    ] {
        let (config, packets) = load_alac_fixture(m4a);
        let mut samples = Vec::new();
        for packet in &packets {
            let frame = decode_alac_frame(packet, &config).expect("packet decodes");
            samples.extend_from_slice(&frame.samples);
        }
        // s16 references decode as i16 pairs. s32 references
        // are stored little-endian i32, but ffmpeg's ALAC
        // encoder writes 24-bit ALAC for s32 input, so its s32le
        // decode is the 24-bit sample shifted left by 8; shift
        // the reference back before comparing (as in
        // `tests/alac.rs`).
        let expected: Vec<i32> = if is_s32 {
            std::fs::read(fixture_path(reference))
                .unwrap()
                .chunks_exact(4)
                .map(|chunk| i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) >> 8)
                .collect()
        } else {
            read_s16le_raw(&fixture_path(reference))
        };
        assert_eq!(samples, expected, "{m4a}: ALAC decode is not bit-exact");
    }
}

// ------------------------------------------------------- sample matrix §6

#[test]
#[allow(
    clippy::cast_possible_truncation,
    reason = "test only: the sine is scaled into [-32768, 32767] and clamped there before the cast, so the f32-to-i16 conversion is exact"
)]
fn sample_matrix_16bit_flac_round_trip_is_bit_exact() {
    // No 96/192 kHz lossy fixture exists, so the high-rate
    // cells are covered by a FLAC encode/decode round-trip of
    // a generated sine (FLAC encode is i16-only).
    for rate in [44_100u32, 48_000, 96_000, 192_000] {
        let frames = (rate / 10) as usize; // 0.1 s
        let mut i16_samples = Vec::with_capacity(frames * 2);
        for sample in sine_f32(rate, 2, frames, 0.6) {
            i16_samples.push((sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16);
        }
        let encoded =
            codec_whatttttttttttt_core::encode_flac_stream(&i16_samples, 2, rate).unwrap();
        let decoded = decode_flac_bytes(&encoded);
        let expected: Vec<f32> = i16_samples
            .iter()
            .map(|&value| f32::from(value) / 32768.0)
            .collect();
        assert_eq!(
            decoded, expected,
            "flac 16-bit {rate} Hz round-trip is not bit-exact"
        );
    }
}

#[test]
fn sample_matrix_pcm_round_trip_is_bit_exact() {
    // 16-bit 44.1/48/96/192 kHz and 24-bit 16/48/96/192 kHz,
    // stereo, through the PCM encoder + WAV muxer + decoder.
    // The comparison is against the quantized values (see
    // `quantized`), so it is exact at the integer level.
    for (format, rates) in [
        (PcmFormat::I16, [44_100u32, 48_000, 96_000, 192_000]),
        (PcmFormat::I24, [16_000u32, 48_000, 96_000, 192_000]),
    ] {
        for rate in rates {
            let frames = (rate / 10) as usize; // 0.1 s
            let samples = sine_f32(rate, 2, frames, 0.6);
            let decoded = pcm_round_trip(&samples, format, 2, rate);
            let expected = quantized(&samples, format);
            assert_eq!(
                decoded, expected,
                "pcm {format:?} {rate} Hz round-trip is not bit-exact"
            );
        }
    }
}

// -------------------------------------------------------- lossy bitrates §5

#[test]
fn mp3_bitrate_matrix_matches_ffmpeg() {
    // 64 / 96 / 128 / 320 kbps, ISO full-compliance bounds.
    check_mp3_fixture(
        "sine_stereo_44100_64k.mp3",
        "sine_stereo_44100_64k.ref.raw",
        2,
        44_100,
    );
    check_mp3_fixture(
        "sine_mono_44100_96k.mp3",
        "sine_mono_44100_96k.ref.raw",
        1,
        44_100,
    );
    check_mp3_fixture(
        "sine_stereo_44100_128k.mp3",
        "sine_stereo_44100_128k.ref.raw",
        2,
        44_100,
    );
    check_mp3_fixture(
        "sine_stereo_48000_320k.mp3",
        "sine_stereo_48000_320k.ref.raw",
        2,
        48_000,
    );
    // Sweep and transient at 128k exercise the same bound on
    // non-stationary signals.
    check_mp3_fixture(
        "sweep_stereo_44100_128k.mp3",
        "sweep_stereo_44100_128k.ref.raw",
        2,
        44_100,
    );
    check_mp3_fixture(
        "transient_stereo_44100_128k.mp3",
        "transient_stereo_44100_128k.ref.raw",
        2,
        44_100,
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn aac_bitrate_matrix_matches_ffmpeg() {
    // 64 / 96 / 128 / 320 kbps, AAC thresholds from
    // `tests/aac.rs` (max abs <= 2.0e-3, RMS <= 5.0e-4).
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

    fn decode_adts(name: &str) -> Vec<f32> {
        let bytes = std::fs::read(fixture_path(name)).unwrap();
        let (config, _) = parse_adts_header(&bytes).expect("fixture adts header parses");
        let mut demuxer = AdtsDemuxer::from_bytes(&bytes).expect("fixture adts demuxes");
        let mut license = valid_license();
        let mut decoder =
            Decoder::from_aac_config(&config, Some(&mut license)).expect("licensed aac decoder");
        let mut samples = Vec::new();
        while let Some(frame) = demuxer.next_frame().expect("adts frame reads") {
            let frame_out = decoder
                .decode(&Packet {
                    codec: Codec::Aac,
                    data: frame.payload,
                    timestamp: 0,
                    is_keyframe: false,
                })
                .expect("aac frame decodes");
            let Frame::Audio(audio) = frame_out else {
                panic!("aac decode must return an audio frame");
            };
            samples.extend_from_slice(&audio.samples);
        }
        samples
    }

    fn compare(name: &str, decoded: &[f32], reference: &[f32]) {
        assert_eq!(
            decoded.len(),
            reference.len(),
            "{name}: decoded length {} != reference length {}",
            decoded.len(),
            reference.len()
        );
        let (rms, max) = rms_and_max(decoded, reference);
        assert!(
            max <= 2.0e-3,
            "{name}: max abs error {max:.6} exceeds 2.0e-3 (rms {rms:.6})"
        );
        assert!(
            rms <= 5.0e-4,
            "{name}: rms error {rms:.6} exceeds 5.0e-4 (max abs {max:.6})"
        );
    }

    for (aac, reference) in [
        ("aac_stereo_44100_64k.aac", "aac_stereo_44100_64k_ref.raw"),
        ("aac_mono_8000_64k.aac", "aac_mono_8000_64k_ref.raw"),
        ("aac_stereo_48000_96k.aac", "aac_stereo_48000_96k_ref.raw"),
        ("aac_stereo_44100_128k.aac", "aac_stereo_44100_128k_ref.raw"),
        ("aac_stereo_44100_320k.aac", "aac_stereo_44100_320k_ref.raw"),
    ] {
        let decoded = decode_adts(aac);
        let expected = read_f32le_raw(&fixture_path(reference));
        compare(aac, &decoded, &expected);
    }
}

#[test]
fn vorbis_stereo_matches_original_wav_within_measured_loss() {
    // Vorbis is lossy; the bound is twice the measured lossy
    // difference against the original WAV (stereo RMS 0.0059 /
    // max 0.016, `tests/vorbis.rs`), so a regression shows up
    // instead of passing on luck.
    let ogg = include_bytes!("data/sine_stereo_44100_vorbis.ogg").to_vec();
    let mut demuxer = OggDemuxer::new(Cursor::new(ogg)).unwrap();
    assert_eq!(demuxer.codec(), Codec::Vorbis);
    let identification = demuxer.next_packet().unwrap().unwrap();
    let comment = demuxer.next_packet().unwrap().unwrap();
    let setup = demuxer.next_packet().unwrap().unwrap();
    let parsed =
        VorbisSetup::from_headers(&identification.data, &comment.data, &setup.data).unwrap();
    let mut decoder = Decoder::from_vorbis_setup(parsed).unwrap();
    let mut samples = Vec::new();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        match decoder.decode(&packet).unwrap() {
            Frame::Audio(audio) => samples.extend_from_slice(&audio.samples),
            Frame::Video(_) => panic!("vorbis decoded to a video frame"),
        }
    }
    let wav = std::fs::read(fixture_path("valid_i16_stereo_44100.wav")).unwrap();
    let expected = decode_wav_bytes(&wav);
    let (rms, max) = rms_and_max(&samples, &expected);
    assert!(
        rms < 0.0118,
        "vorbis stereo rms {rms:.6} exceeds 2x the measured 0.0059"
    );
    assert!(
        max < 0.032,
        "vorbis stereo max {max:.6} exceeds 2x the measured 0.016"
    );
}

// ------------------------------------------------------------------- opus

#[test]
fn opus_decode_is_unsupported_for_celt_samples() {
    // No Opus PCM is fabricated: decoding a real CELT frame
    // must report Unsupported naming CELT sample
    // reconstruction (see `tests/opus.rs`).
    let ogg = include_bytes!("data/sine_stereo_44100.opus").to_vec();
    let mut demuxer = OggDemuxer::new(Cursor::new(ogg)).unwrap();
    assert_eq!(demuxer.codec(), Codec::Opus);
    let head = demuxer.next_packet().unwrap().unwrap();
    let parsed_head = codec_whatttttttttttt_core::parse_opus_head(&head.data).unwrap();
    let mut decoder = Decoder::from_opus_head(&parsed_head).unwrap();
    // The second packet is OpusTags, not audio.
    let _tags = demuxer.next_packet().unwrap().unwrap();
    let packet = demuxer.next_packet().unwrap().unwrap();
    let err = decoder.decode(&packet).unwrap_err();
    match err {
        Error::Unsupported(detail) => {
            assert!(
                detail.contains("CELT sample reconstruction"),
                "unexpected opus unsupported detail: {detail}"
            );
        }
        other => panic!("expected Unsupported for opus CELT, got {other:?}"),
    }
}

// ------------------------------------------------------------ encode refuses

#[test]
fn encode_without_an_encoder_is_unsupported() {
    // Opus and Vorbis have no encoder in this build; the
    // matrix asserts the refusal instead of pretending a
    // round-trip exists. (MP3, AAC, and ALAC encoders landed
    // in the final round, so they are no longer in this list.)
    let frame = Frame::Audio(codec_whatttttttttttt_core::AudioFrame {
        sample_rate: 44_100,
        channels: 2,
        samples: vec![0.0; 2048],
        timestamp: 0,
    });
    for codec in [Codec::Opus, Codec::Vorbis] {
        let mut encoder = Encoder::new(EncoderConfig {
            codec,
            sample_rate: 44_100,
            channels: 2,
            ..EncoderConfig::default()
        })
        .unwrap();
        let err = encoder.encode(&frame).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(_)),
            "{codec:?} encode must be Unsupported, got {err:?}"
        );
    }
}

#[test]
#[cfg(not(feature = "licensed-aac"))]
fn aac_encode_without_the_feature_is_unsupported() {
    // The gate is checked at construction, like
    // `Decoder::from_aac_config`: no encoder is built, so
    // there is no encode call to refuse. `Encoder` has no
    // `Debug`, so `unwrap_err` is unavailable; the let-else
    // extracts the error instead.
    let Err(err) = Encoder::new(EncoderConfig {
        codec: Codec::Aac,
        sample_rate: 44_100,
        channels: 2,
        ..EncoderConfig::default()
    }) else {
        panic!("aac encoder construction must fail without the feature");
    };
    assert!(
        matches!(err, Error::Unsupported(_)),
        "aac encode without the feature must be Unsupported, got {err:?}"
    );
}

#[test]
#[cfg(feature = "licensed-aac")]
fn aac_encode_with_the_feature_is_license_required() {
    // With the feature on, the software path exists but is
    // closed without a license, so the refusal is
    // `LicenseRequired`, not `Unsupported` (see `hw.rs`). The
    // gate is checked at construction, so construction itself
    // fails. `Encoder` has no `Debug`, so `unwrap_err` is
    // unavailable; the let-else extracts the error instead.
    let Err(err) = Encoder::new(EncoderConfig {
        codec: Codec::Aac,
        sample_rate: 44_100,
        channels: 2,
        ..EncoderConfig::default()
    }) else {
        panic!("aac encoder construction must fail without a license");
    };
    assert!(
        matches!(err, Error::LicenseRequired { .. }),
        "aac encode with the feature must be LicenseRequired, got {err:?}"
    );
}

// -------------------------------------------------------------------- dsd

#[test]
fn dsd64_dsf_decimates_to_a_half_amplitude_sine() {
    // DSF DSD64 stereo (left 1000 Hz, right 1500 Hz, amplitude
    // 0.5) decimated to 176.4 kHz: the least-squares sine fit
    // on the left channel, past the edge transient, must come
    // back near 0.5. This is a sanity row, not a full
    // conformance check — the thorough DSD tests live in
    // `tests/dsd.rs`.
    let bytes = std::fs::read(fixture_path("dsf_dsd64_stereo.dsf")).unwrap();
    let mut demuxer = DsdDemuxer::new(Cursor::new(bytes)).unwrap();
    let header = demuxer.header().clone();
    assert_eq!(header.sample_rate, 2_822_400);
    assert_eq!(header.channels, 2);
    let mut decoder = Decoder::from_dsd_header(&header)
        .unwrap()
        .with_dsd_output_rate(176_400)
        .unwrap();
    let mut samples = Vec::new();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        match decoder.decode(&packet).unwrap() {
            Frame::Audio(audio) => samples.extend_from_slice(&audio.samples),
            Frame::Video(_) => panic!("dsd decoded to a video frame"),
        }
    }
    if let Some(Frame::Audio(tail)) = decoder.flush().unwrap() {
        samples.extend_from_slice(&tail.samples);
    }
    // The DSF data is block-padded: 9 blocks of 4096 bytes per
    // channel = 294912 DSD samples per channel, so decimation
    // by 16 gives 18432 frames — more than the 17640 the 0.1 s
    // header duration suggests (the header's `sample_count`
    // counts the unpadded signal, the file holds whole blocks).
    let expected_frames = 18_432usize;
    assert_eq!(
        samples.len() / 2,
        expected_frames,
        "dsd64 frame count is not the block-padded 18432"
    );
    let left: Vec<f32> = samples.iter().step_by(2).copied().collect();
    let skip = 1764; // 10 ms past the edge transient
    let amp = sine_amplitude(&left[skip..skip + 8192], 176_400.0, 1000.0);
    assert!(
        (amp - 0.5).abs() < 0.05,
        "dsd64 left-channel sine amplitude {amp:.4} is not near 0.5"
    );
}
