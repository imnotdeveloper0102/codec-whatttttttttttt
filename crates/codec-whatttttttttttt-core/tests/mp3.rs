//! MP3 Layer III decode tests against ffmpeg fixtures.
//!
//! Fixtures are real MP3 files encoded with ffmpeg 8.1.2
//! (libmp3lame) from locally generated WAVs; the reference
//! `.ref.raw` files are ffmpeg's own decode (`-f f32le`) of the
//! same files. The commands are recorded in the task report.
//!
//! Conformance thresholds (ISO/IEC 11172-4 section 2.6.3, for the
//! 20 Hz-10 kHz sine sweep at -20 dB): full compliance is RMS <
//! 2^-15/sqrt(12) = 8.80967e-06 with max abs diff <= 2^-14 =
//! 6.10352e-05; limited accuracy is RMS < 2^-11/sqrt(12) =
//! 1.409547e-04. Two correct decoders should agree within the
//! same magnitude, so the tests assert the full-compliance
//! bounds and print the measured RMS/max. Measured 2026-11-08
//! (all at offset 2257): mono 96k RMS 1.84e-7 max 1.13e-6;
//! stereo 128k RMS 2.69e-7 max 1.91e-6; stereo 64k RMS 2.68e-7
//! max 1.37e-6; stereo 48k 320k RMS 2.74e-7 max 1.28e-6; sweep
//! 128k RMS 6.44e-8 max 6.26e-7; transient 128k RMS 1.98e-8
//! max 6.56e-7. Every fixture passes the full-compliance
//! bounds with at least 30x headroom.
//!
//! The decoder outputs raw frames, including the encoder-delay
//! priming samples; ffmpeg's decode trims that delay, so the
//! comparison aligns by searching the offset (in samples per
//! channel) that minimises the RMS difference. The offset is
//! around 2257: the 1152-sample Xing/LAME info frame plus the
//! 576-sample encoder delay plus ffmpeg's 528-sample decoder
//! delay (measured, not assumed).
//! channel) that minimises RMS, over 0..=1200.

use codec_whatttttttttttt_core::{
    AudioFrame, BitrateMode, Codec, Decoder, DecoderConfig, Encoder, EncoderConfig, Error, Frame,
    Mp3FrameHeader, Packet,
};

/// Full-compliance RMS bound, ISO/IEC 11172-4 section 2.6.3:
/// 2^-15/sqrt(12) = 8.80967e-06.
const FULL_COMPLIANCE_RMS: f64 = 8.809_67e-06;
/// Full-compliance max-abs-diff bound, ISO/IEC 11172-4 section 2.6.3:
/// 2^-14 = 6.10352e-05.
const FULL_COMPLIANCE_MAX: f32 = 6.103_52e-05;

fn fixture_path(name: &str) -> String {
    format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Reads a raw little-endian f32 reference file.
fn read_f32le_raw(path: &str) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(
        bytes.len() % 4,
        0,
        "{path}: reference length is not a multiple of 4"
    );
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .collect()
}

/// Splits a whole MP3 file into frames, skipping an ID3 tag or
/// any leading garbage: a candidate sync must parse as a header
/// whose frame length lands exactly on the next frame (or the
/// end of the file).
fn split_frames(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut i = 0usize;
    while i + 4 <= bytes.len() {
        if bytes[i] == 0xFF && bytes[i + 1] & 0xE0 == 0xE0 {
            if let Ok(header) = Mp3FrameHeader::parse(&bytes[i..]) {
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

/// Decodes every frame of a fixture file through `Decoder` and
/// returns the concatenated interleaved samples plus the
/// channels and sample rate of the last frame.
fn decode_file(name: &str) -> (Vec<f32>, u32, u32) {
    let bytes = std::fs::read(fixture_path(name)).unwrap();
    let frames = split_frames(&bytes);
    assert!(!frames.is_empty(), "{name}: no mp3 frames found");
    let mut decoder = Decoder::new(DecoderConfig {
        codec: Codec::Mp3,
        ..Default::default()
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

// `n` is a sample count of at most a few hundred thousand, exact in f64.
#[allow(clippy::cast_precision_loss)]
fn rms_and_max(a: &[f32], b: &[f32]) -> (f64, f32) {
    let n = a.len().min(b.len());
    let mut sum = 0.0f64;
    let mut max = 0.0f32;
    for i in 0..n {
        let diff = f64::from(a[i]) - f64::from(b[i]);
        sum += diff * diff;
        max = max.max((a[i] - b[i]).abs());
    }
    ((sum / n as f64).sqrt(), max)
}

/// Finds the per-channel offset that best aligns `decoded` with
/// `reference`, and returns (offset, rms, max) over the whole
/// overlap. Both slices are interleaved with `channels` channels.
///
/// The offset is not just the 576-sample LAME encoder delay:
/// the file starts with a 1152-sample Xing/LAME info frame and
/// ffmpeg also trims its own 528-sample decoder delay, so the
/// true alignment is around 1152 + 576 + 528 = 2256 samples.
/// The search covers 0..=2400 and runs on a window in the
/// middle of the reference (a full-reference search at every
/// offset would be needlessly slow).
fn align_and_compare(decoded: &[f32], reference: &[f32], channels: usize) -> (usize, f64, f32) {
    assert!(!reference.is_empty(), "reference is empty");
    // Window in the middle of the reference: past the priming
    // samples, present in every fixture (the shortest reference
    // here is 22050 samples per channel).
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
    assert_ne!(best.0, usize::MAX, "no alignment fits the reference");
    // Full compare at the winning offset.
    let offset = best.0;
    let start = offset * channels;
    let len = reference.len().min(decoded.len() - start);
    let (rms, max) = rms_and_max(&decoded[start..start + len], &reference[..len]);
    (offset, rms, max)
}

/// Decodes a fixture, compares it with ffmpeg's decode, prints
/// the measured alignment and errors, and asserts the
/// full-compliance bounds of ISO/IEC 11172-4 section 2.6.3.
fn check_fixture(mp3: &str, reference: &str, expected_channels: u32, expected_rate: u32) {
    let (samples, channels, sample_rate) = decode_file(mp3);
    assert_eq!(channels, expected_channels, "{mp3}: channels");
    assert_eq!(sample_rate, expected_rate, "{mp3}: sample rate");
    let reference = read_f32le_raw(&fixture_path(reference));
    let (offset, rms, max) = align_and_compare(&samples, &reference, channels as usize);
    println!("{mp3}: offset={offset} rms={rms:.3e} max={max:.3e}");
    assert!(
        rms < FULL_COMPLIANCE_RMS,
        "{mp3}: rms {rms:.3e} exceeds the full-compliance bound {FULL_COMPLIANCE_RMS:.3e}"
    );
    assert!(
        max <= FULL_COMPLIANCE_MAX,
        "{mp3}: max {max:.3e} exceeds the full-compliance bound {FULL_COMPLIANCE_MAX:.3e}"
    );
}

#[test]
fn stereo_128k_matches_ffmpeg() {
    check_fixture(
        "sine_stereo_44100_128k.mp3",
        "sine_stereo_44100_128k.ref.raw",
        2,
        44100,
    );
}

#[test]
fn stereo_64k_matches_ffmpeg() {
    check_fixture(
        "sine_stereo_44100_64k.mp3",
        "sine_stereo_44100_64k.ref.raw",
        2,
        44100,
    );
}

#[test]
fn stereo_48000_320k_matches_ffmpeg() {
    check_fixture(
        "sine_stereo_48000_320k.mp3",
        "sine_stereo_48000_320k.ref.raw",
        2,
        48000,
    );
}

#[test]
fn mono_96k_matches_ffmpeg() {
    check_fixture(
        "sine_mono_44100_96k.mp3",
        "sine_mono_44100_96k.ref.raw",
        1,
        44100,
    );
}

#[test]
fn sweep_128k_matches_ffmpeg() {
    check_fixture(
        "sweep_stereo_44100_128k.mp3",
        "sweep_stereo_44100_128k.ref.raw",
        2,
        44100,
    );
}

#[test]
fn transient_128k_matches_ffmpeg() {
    // Clicks force short/start/stop blocks; this is the main
    // check for the IMDCT window placement.
    check_fixture(
        "transient_stereo_44100_128k.mp3",
        "transient_stereo_44100_128k.ref.raw",
        2,
        44100,
    );
}

#[test]
fn header_parses_known_lame_values() {
    // First frame of a 128 kbps 44.1 kHz stereo Layer III stream.
    let header = Mp3FrameHeader::parse(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
    assert_eq!(header.bitrate_kbps, 128);
    assert_eq!(header.sample_rate, 44100);
    assert_eq!(header.channels(), 2);
    assert_eq!(header.frame_length, 417);
    assert_eq!(header.samples_per_frame, 1152);
}

#[test]
fn header_rejects_bad_sync_and_reserved_fields() {
    assert!(matches!(
        Mp3FrameHeader::parse(&[0x00, 0xFB, 0x90, 0x00]),
        Err(Error::InvalidData { .. })
    ));
    // Reserved version id 1.
    assert!(matches!(
        Mp3FrameHeader::parse(&[0xFF, 0xEB, 0x90, 0x00]),
        Err(Error::InvalidData { .. })
    ));
    // Bad bitrate index 15.
    assert!(matches!(
        Mp3FrameHeader::parse(&[0xFF, 0xFB, 0xF0, 0x00]),
        Err(Error::InvalidData { .. })
    ));
    // Truncated header.
    assert!(matches!(
        Mp3FrameHeader::parse(&[0xFF, 0xFB]),
        Err(Error::Truncated { .. })
    ));
}

#[test]
fn layer2_frame_returns_unsupported() {
    let bytes = std::fs::read(fixture_path("sine_stereo_44100_layer2.mp2")).unwrap();
    let frames = split_frames(&bytes);
    assert!(!frames.is_empty(), "layer2 fixture has no frames");
    let mut decoder = Decoder::new(DecoderConfig {
        codec: Codec::Mp3,
        ..Default::default()
    })
    .unwrap();
    let packet = Packet {
        codec: Codec::Mp3,
        data: frames[0].clone(),
        timestamp: 0,
        is_keyframe: true,
    };
    let err = decoder.decode(&packet).unwrap_err();
    assert!(
        matches!(err, Error::Unsupported(_)),
        "layer II must return Unsupported, got {err:?}"
    );
}

#[test]
fn bad_sync_packet_is_invalid_data() {
    let mut decoder = Decoder::new(DecoderConfig {
        codec: Codec::Mp3,
        ..Default::default()
    })
    .unwrap();
    let packet = Packet {
        codec: Codec::Mp3,
        data: vec![0x00, 0xFB, 0x90, 0x00],
        timestamp: 0,
        is_keyframe: true,
    };
    assert!(matches!(
        decoder.decode(&packet),
        Err(Error::InvalidData { .. })
    ));
}

#[test]
fn truncated_header_packet_is_truncated() {
    let mut decoder = Decoder::new(DecoderConfig {
        codec: Codec::Mp3,
        ..Default::default()
    })
    .unwrap();
    let packet = Packet {
        codec: Codec::Mp3,
        data: vec![0xFF, 0xFB],
        timestamp: 0,
        is_keyframe: true,
    };
    assert!(matches!(
        decoder.decode(&packet),
        Err(Error::Truncated { .. })
    ));
}

#[test]
fn truncated_main_data_packet_is_truncated() {
    let bytes = std::fs::read(fixture_path("sine_stereo_44100_128k.mp3")).unwrap();
    let frames = split_frames(&bytes);
    let mut cut = frames[0].clone();
    cut.truncate(cut.len() - 10);
    let mut decoder = Decoder::new(DecoderConfig {
        codec: Codec::Mp3,
        ..Default::default()
    })
    .unwrap();
    let packet = Packet {
        codec: Codec::Mp3,
        data: cut,
        timestamp: 0,
        is_keyframe: true,
    };
    assert!(matches!(
        decoder.decode(&packet),
        Err(Error::Truncated { .. })
    ));
}

#[test]
fn mid_stream_first_frame_is_invalid_data() {
    // The third frame borrows 21 bytes of main data from the
    // first two through the bit reservoir, so decoding it first
    // must fail instead of producing silence. (Frames 0 and 1
    // have `main_data_begin` 0 and would decode on their own.)
    let bytes = std::fs::read(fixture_path("sine_stereo_44100_128k.mp3")).unwrap();
    let frames = split_frames(&bytes);
    assert!(frames.len() > 2);
    let mut decoder = Decoder::new(DecoderConfig {
        codec: Codec::Mp3,
        ..Default::default()
    })
    .unwrap();
    let packet = Packet {
        codec: Codec::Mp3,
        data: frames[2].clone(),
        timestamp: 0,
        is_keyframe: true,
    };
    assert!(matches!(
        decoder.decode(&packet),
        Err(Error::InvalidData { .. })
    ));
}

#[test]
fn encode_produces_mp3_packets_and_flush_drains_tail() {
    let mut encoder = Encoder::new(EncoderConfig {
        codec: Codec::Mp3,
        sample_rate: 44100,
        channels: 2,
        bitrate_mode: Some(BitrateMode::Cbr(128)),
        ..EncoderConfig::default()
    })
    .unwrap();
    let frame = Frame::Audio(AudioFrame {
        sample_rate: 44100,
        channels: 2,
        samples: vec![0.0; 2304],
        timestamp: 0,
    });
    // The first call returns a silent priming frame: the
    // encoder carries one frame of delay, so the packet for
    // this input only appears after more input or a flush.
    let packet = encoder.encode(&frame).unwrap();
    assert_eq!(packet.codec, Codec::Mp3);
    assert_ne!(packet.data, [] as [u8; 0]);
    // Flush drains the buffered tail; the stream ends after it.
    let mut flushed = 0;
    while let Some(packet) = encoder.flush().unwrap() {
        assert_eq!(packet.codec, Codec::Mp3);
        flushed += 1;
    }
    assert!(flushed > 0, "flush must return the buffered tail");
    assert!(encoder.flush().unwrap().is_none());
}
