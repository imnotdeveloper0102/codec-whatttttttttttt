//! MP3 Layer III encode tests: round-trip through this
//! library's `Mp3Decoder`, and cross-checks against ffmpeg
//! 8.1.2 decode of the encoded fixtures.
//!
//! Fixtures: the `mp3enc_*.wav` sources are 1 s tones made
//! with ffmpeg (`sine` lavfi source, `volume` to 0.5/0.4/0.25/
//! 0.15); the `mp3enc_*.mp3` files are the output of
//! `Mp3Encoder` on those WAVs (see the task report for the
//! exact commands). The `mp3enc_*_ref.raw` files are ffmpeg's
//! decode (`-f f32le`) of the encoded MP3s. Thresholds below
//! are the measured round-trip errors, not conformance bounds:
//! this is a basic encoder (long blocks only, no psychoacoustic
//! model), so its error is orders of magnitude above the
//! ISO/IEC 11172-4 decoder-compliance bounds, and the tests
//! assert the measured values with headroom instead of
//! pretending otherwise.
//!
//! The encoder emits silent priming frames at the start
//! (one frame for MPEG-1, three for MPEG-2/2.5) and the last
//! frames only on `flush`, so the decoded stream is the
//! priming silence followed by the input frames in order;
//! the round-trip comparison skips the priming samples.
//! Two samples per granule (rows 0 and 16) are not
//! controllable by that granule's spectrum — the polyphase
//! synthesis computes them from the filterbank history alone —
//! so the first granule of a stream decodes them as 0; the
//! comparison below includes them, and the thresholds account
//! for it.

use codec_whatttttttttttt_core::{
    AudioFrame, BitrateMode, Codec, Decoder, DecoderConfig, Error, Frame, Mp3Decoder, Mp3Encoder,
    Mp3FrameHeader, Packet,
};

fn fixture_path(name: &str) -> String {
    format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Reads a 16-bit PCM WAV written by ffmpeg (`pcm_s16le`)
/// into interleaved f32 samples in [-1, 1], plus its channel
/// count and sample rate. Chunks are walked, since ffmpeg
/// writes a LIST/INFO chunk before `data`.
fn read_wav_pcm16(name: &str) -> (Vec<f32>, u32, u32) {
    let bytes = std::fs::read(fixture_path(name)).unwrap();
    assert!(bytes.len() >= 44, "{name}: wav too short");
    assert_eq!(&bytes[0..4], b"RIFF", "{name}: not a RIFF file");
    assert_eq!(&bytes[8..12], b"WAVE", "{name}: not a WAVE file");
    let channels = u32::from(u16::from_le_bytes(bytes[22..24].try_into().unwrap()));
    let sample_rate = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
    let bits = u16::from_le_bytes(bytes[34..36].try_into().unwrap());
    assert_eq!(bits, 16, "{name}: expected 16-bit pcm");
    // Walk the chunks: ffmpeg writes a LIST/INFO chunk before
    // `data`, so the data offset is not fixed at 44.
    let mut pos = 12usize;
    let mut data_start = None;
    let mut data_len = 0usize;
    while pos + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if &bytes[pos..pos + 4] == b"data" {
            data_start = Some(pos + 8);
            data_len = size;
            break;
        }
        pos += 8 + size + (size % 2);
    }
    let data_start = data_start.expect("{name}: no data chunk");
    let data = &bytes[data_start..data_start + data_len];
    let samples = data
        .chunks_exact(2)
        .map(|chunk| f32::from(i16::from_le_bytes(chunk.try_into().unwrap())) / 32768.0)
        .collect();
    (samples, channels, sample_rate)
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

/// Encodes whole frames of `samples` (interleaved) at
/// `sample_rate`/`channels`/`mode`, returning the packets.
/// The input length must be a whole number of frames; the
/// caller trims it.
fn encode_all(samples: &[f32], sample_rate: u32, channels: u32, mode: BitrateMode) -> Vec<Packet> {
    let mut encoder = Mp3Encoder::new(sample_rate, channels, mode).unwrap();
    let samples_per_frame = encoder.samples_per_frame();
    let frame_samples = samples_per_frame * channels as usize;
    assert_eq!(
        samples.len() % frame_samples,
        0,
        "input is not a whole number of frames"
    );
    let mut packets = Vec::new();
    let mut timestamp = 0u64;
    for chunk in samples.chunks_exact(frame_samples) {
        let frame = AudioFrame {
            sample_rate,
            channels,
            samples: chunk.to_vec(),
            timestamp,
        };
        timestamp += (samples_per_frame as u64) * 1_000_000 / u64::from(sample_rate);
        packets.push(encoder.encode(&frame).unwrap());
    }
    // The last frames are only returned by `flush`.
    packets.extend(encoder.flush().unwrap());
    packets
}

/// Decodes packets in order with a fresh `Mp3Decoder` and
/// returns the concatenated interleaved samples.
fn decode_all(packets: &[Packet]) -> Vec<f32> {
    let mut decoder = Mp3Decoder::new();
    let mut samples = Vec::new();
    for packet in packets {
        let frame = decoder.decode_frame(&packet.data).unwrap();
        samples.extend_from_slice(&frame.samples);
    }
    samples
}

/// Encodes `wav` at `mode`, round-trips through the library
/// decoder, prints the measured error, and asserts the given
/// bounds. Returns the packets for further checks.
fn round_trip_check(wav: &str, mode: BitrateMode, max_rms: f64, max_abs: f32) -> Vec<Packet> {
    let (samples, channels, sample_rate) = read_wav_pcm16(wav);
    let encoder_probe = Mp3Encoder::new(sample_rate, channels, mode).unwrap();
    let frame_samples = encoder_probe.samples_per_frame() * channels as usize;
    let usable = samples.len() / frame_samples * frame_samples;
    let packets = encode_all(&samples[..usable], sample_rate, channels, mode);
    assert!(!packets.is_empty(), "{wav}: no packets encoded");
    // Every packet is exactly one CBR frame.
    let frame_length = encoder_probe.frame_length();
    for packet in &packets {
        assert_eq!(packet.codec, Codec::Mp3);
        assert_eq!(packet.data.len(), frame_length, "{wav}: packet length");
        assert!(packet.is_keyframe);
        let header = Mp3FrameHeader::parse(&packet.data).unwrap();
        assert_eq!(header.bitrate_kbps, mode.bitrate_bps().unwrap() / 1000);
        assert_eq!(header.sample_rate, sample_rate);
    }
    // The decoded stream starts with the encoder's
    // silent priming frames, then the input frames in order.
    let priming = encoder_probe.priming_frames() * encoder_probe.samples_per_frame();
    let priming_samples = priming * channels as usize;
    assert_eq!(
        packets.len(),
        usable / frame_samples + encoder_probe.priming_frames(),
        "{wav}: packet count"
    );
    let decoded = decode_all(&packets);
    assert!(
        decoded.len() >= priming_samples + usable,
        "{wav}: decoded stream too short"
    );
    let (rms, max) = rms_and_max(
        &decoded[priming_samples..priming_samples + usable],
        &samples[..usable],
    );
    println!(
        "{wav} {mode:?}: frames={} priming={priming} rms={rms:.4} max={max:.4}",
        packets.len()
    );
    assert!(
        rms < max_rms,
        "{wav} {mode:?}: round-trip rms {rms:.4} exceeds {max_rms}"
    );
    assert!(
        max < max_abs,
        "{wav} {mode:?}: round-trip max {max:.4} exceeds {max_abs}"
    );
    packets
}

#[test]
fn stereo_sine_128k_round_trips() {
    round_trip_check(
        "mp3enc_sine_stereo_44100.wav",
        BitrateMode::Cbr(128),
        0.02,
        0.25,
    );
}

#[test]
fn stereo_sine_64k_round_trips() {
    round_trip_check(
        "mp3enc_sine_stereo_44100.wav",
        BitrateMode::Cbr(64),
        0.02,
        0.25,
    );
}

#[test]
fn mono_sine_96k_round_trips() {
    round_trip_check(
        "mp3enc_sine_mono_44100.wav",
        BitrateMode::Cbr(96),
        0.02,
        0.25,
    );
}

#[test]
fn mono_tones_320k_round_trips() {
    round_trip_check(
        "mp3enc_tones_mono_44100.wav",
        BitrateMode::Cbr(320),
        0.02,
        0.25,
    );
}

#[test]
fn mpeg2_mono_sine_64k_round_trips() {
    round_trip_check(
        "mp3enc_sine_mono_22050.wav",
        BitrateMode::Cbr(64),
        0.02,
        0.25,
    );
}

#[test]
fn invalid_bitrate_is_rejected() {
    let err = Mp3Encoder::new(44100, 2, BitrateMode::Cbr(100)).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
    assert!(err.to_string().contains("mp3"));
}

#[test]
fn invalid_sample_rate_is_rejected() {
    let err = Mp3Encoder::new(12345, 2, BitrateMode::Cbr(128)).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
    assert!(err.to_string().contains("mp3"));
}

#[test]
fn bitrate_320k_rejected_at_mpeg2_rate() {
    // ISO/IEC 11172-3 Table B.1 caps MPEG-2/2.5 Layer III at
    // 160 kbps, so 320 kbps at 22050 Hz has no valid header.
    let err = Mp3Encoder::new(22050, 1, BitrateMode::Cbr(320)).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
    assert!(err.to_string().contains("320"));
}

#[test]
fn vbr_and_abr_are_unsupported() {
    let err = Mp3Encoder::new(44100, 2, BitrateMode::Vbr(2)).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
    let err = Mp3Encoder::new(44100, 2, BitrateMode::Abr(128)).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
}

#[test]
fn bad_channel_count_is_rejected() {
    let err = Mp3Encoder::new(44100, 3, BitrateMode::Cbr(128)).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
    let err = Mp3Encoder::new(44100, 0, BitrateMode::Cbr(128)).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
}

#[test]
fn wrong_frame_length_is_rejected() {
    let mut encoder = Mp3Encoder::new(44100, 1, BitrateMode::Cbr(128)).unwrap();
    let short = AudioFrame {
        sample_rate: 44100,
        channels: 1,
        samples: vec![0.0; 100],
        timestamp: 0,
    };
    assert!(matches!(
        encoder.encode(&short),
        Err(Error::InvalidArgument(_))
    ));
    let mismatched = AudioFrame {
        sample_rate: 48000,
        channels: 1,
        samples: vec![0.0; 1152],
        timestamp: 0,
    };
    assert!(matches!(
        encoder.encode(&mismatched),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn encoded_stereo_128k_matches_ffmpeg_decode() {
    // The checked-in `mp3enc_sine_stereo_44100_128k.mp3` was
    // produced by this encoder from `mp3enc_sine_stereo_44100.wav`;
    // the reference is ffmpeg's decode of that file. ffmpeg
    // trims its own decoder delay, so the comparison aligns by
    // searching the offset (in samples per channel) that
    // minimises the RMS difference, as in tests/mp3.rs.
    let bytes = std::fs::read(fixture_path("mp3enc_sine_stereo_44100_128k.mp3")).unwrap();
    let reference = read_f32le_raw(&fixture_path("mp3enc_sine_stereo_44100_128k_ref.raw"));
    let mut decoder = Decoder::new(DecoderConfig {
        codec: Codec::Mp3,
        ..Default::default()
    })
    .unwrap();
    let mut samples = Vec::new();
    let mut offset = 0usize;
    while offset + 4 <= bytes.len() {
        let header = Mp3FrameHeader::parse(&bytes[offset..]).unwrap();
        let len = header.frame_length;
        let packet = Packet {
            codec: Codec::Mp3,
            data: bytes[offset..offset + len].to_vec(),
            timestamp: 0,
            is_keyframe: true,
        };
        match decoder.decode(&packet).unwrap() {
            Frame::Audio(audio) => samples.extend_from_slice(&audio.samples),
            Frame::Video(_) => panic!("mp3 decoded to a video frame"),
        }
        offset += len;
    }
    assert!(!samples.is_empty(), "no frames decoded");
    let channels = 2usize;
    let win_start = 4000 * channels;
    let win_len = 4000 * channels;
    let mut best = (usize::MAX, f64::MAX);
    for align in 0..=1200usize {
        let start = (align + 4000) * channels;
        if start + win_len > samples.len() || win_start + win_len > reference.len() {
            break;
        }
        let (rms, _) = rms_and_max(
            &samples[start..start + win_len],
            &reference[win_start..win_start + win_len],
        );
        if rms < best.1 {
            best = (align, rms);
        }
    }
    assert_ne!(best.0, usize::MAX, "no alignment fits the reference");
    let (rms, max) = rms_and_max(&samples[best.0 * channels..], &reference);
    println!(
        "ffmpeg cross-check: offset={} rms={rms:.4} max={max:.4}",
        best.0
    );
    assert!(rms < 0.02, "ffmpeg cross-check rms {rms:.4} exceeds 0.02");
    assert!(max < 0.25, "ffmpeg cross-check max {max:.4} exceeds 0.25");
}
