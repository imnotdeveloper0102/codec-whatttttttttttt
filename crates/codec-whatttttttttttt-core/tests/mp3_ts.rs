//! Integration tests for MP3 in MPEG-TS (ISO/IEC 13818-1
//! stream types 0x03/0x04).
//!
//! The fixture is a real file produced with ffmpeg 8.1.2 from a
//! local sine source (see the subagent report for the exact
//! command): `mp3ts_sine_stereo_44100_128k.ts` is a 1 s stereo
//! 44100 Hz sine encoded with libmp3lame at 128 kbps and muxed to
//! MPEG-TS; ffprobe reports one MP3 audio stream and 40 frames,
//! and the PMT carries stream type 0x03 (MPEG-1 audio). The
//! reference `.ref.raw` file is ffmpeg's own decode (`-f f32le`)
//! of the same TS file.
//!
//! The TS demuxer emits one packet per PES, and a PES can hold
//! more than one MP3 frame, so the test splits each PES payload
//! into frames (the same frame-splitting logic as `tests/mp3.rs`)
//! before feeding them to `Decoder`, which decodes one frame per
//! packet.

use codec_whatttttttttttt_core::{
    Codec, Decoder, DecoderConfig, Frame, Mp3FrameHeader, Packet, TsDemuxer, TsMuxer,
};

/// Full-compliance RMS bound, ISO/IEC 11172-4 section 2.6.3:
/// 2^-15/sqrt(12) = 8.80967e-06.
const FULL_COMPLIANCE_RMS: f64 = 8.809_67e-06;
/// Full-compliance max-abs-diff bound, ISO/IEC 11172-4 section 2.6.3:
/// 2^-14 = 6.10352e-05.
const FULL_COMPLIANCE_MAX: f32 = 6.103_52e-05;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
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

/// Splits PES payload bytes into MP3 frames: a candidate sync
/// must parse as a header whose frame length lands exactly on
/// the next frame (or the end of the payload).
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

/// Demuxes the fixture and returns all MP3 frames in file order.
fn demuxed_frames() -> Vec<Vec<u8>> {
    let bytes = fixture("mp3ts_sine_stereo_44100_128k.ts");
    let mut demuxer = TsDemuxer::parse(&bytes).unwrap();
    let mut frames = Vec::new();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        assert_eq!(packet.codec, Codec::Mp3);
        frames.extend(split_frames(&packet.data));
    }
    frames
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
/// `reference` (same windowed search as `tests/mp3.rs`: the
/// encoder/decoder delay puts the true alignment near 2256
/// samples), and returns (offset, rms, max) over the overlap.
fn align_and_compare(decoded: &[f32], reference: &[f32], channels: usize) -> (usize, f64, f32) {
    assert!(!reference.is_empty(), "reference is empty");
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
    let offset = best.0;
    let start = offset * channels;
    let (rms, max) = rms_and_max(&decoded[start..], &reference[..decoded.len() - start]);
    (offset, rms, max)
}

#[test]
fn ts_mp3_demux_finds_one_mp3_stream_type_0x03() {
    let bytes = fixture("mp3ts_sine_stereo_44100_128k.ts");
    assert_eq!(bytes.len(), 20_868);
    assert_eq!(bytes.len() % 188, 0);
    let demuxer = TsDemuxer::parse(&bytes).unwrap();
    let streams = demuxer.streams();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].stream_type, 0x03);
    assert_eq!(streams[0].codec, Some(Codec::Mp3));
    assert_eq!(demuxer.stats().continuity_errors, 0);
    assert_eq!(demuxer.stats().dropped_duplicates, 0);
    // ffmpeg packs several MP3 frames per PES (6 PES packets
    // for the 40 frames here), so the packet count is 6, not 40.
    assert_eq!(demuxer.packet_count(), 6);
    // The demuxed payloads hold exactly the 40 frames ffprobe
    // counts for the file.
    assert_eq!(demuxed_frames().len(), 40);
}

#[test]
fn ts_mp3_packets_decode_to_ffmpeg_reference() {
    let frames = demuxed_frames();
    assert_eq!(frames.len(), 40);
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
            Frame::Video(_) => panic!("mp3 decoded to a video frame"),
        }
    }
    assert_eq!(channels, 2);
    assert_eq!(sample_rate, 44_100);
    let reference = read_f32le_raw(&format!(
        "{}/tests/data/mp3ts_sine_stereo_44100_128k_ref.raw",
        env!("CARGO_MANIFEST_DIR")
    ));
    let (offset, rms, max) = align_and_compare(&samples, &reference, channels as usize);
    println!("mp3-in-ts: offset {offset}, RMS {rms:.3e}, max {max:.3e}");
    assert!(
        rms < FULL_COMPLIANCE_RMS,
        "RMS {rms:.3e} exceeds full-compliance bound {FULL_COMPLIANCE_RMS:.3e}"
    );
    assert!(
        max < FULL_COMPLIANCE_MAX,
        "max diff {max:.3e} exceeds full-compliance bound {FULL_COMPLIANCE_MAX:.3e}"
    );
}

#[test]
fn ts_mp3_mux_round_trip_preserves_real_frame() {
    let frames = demuxed_frames();
    let frame = frames.first().unwrap().clone();
    let mut muxer = TsMuxer::new();
    muxer.add_stream(0x101, Codec::Mp3).unwrap();
    let bytes = muxer.packet(0x101, &frame, Some(0), false).unwrap();
    assert_eq!(bytes.len() % 188, 0);
    let mut demuxer = TsDemuxer::parse(&bytes).unwrap();
    assert_eq!(demuxer.streams().len(), 1);
    assert_eq!(demuxer.streams()[0].stream_type, 0x03);
    assert_eq!(demuxer.streams()[0].codec, Some(Codec::Mp3));
    assert_eq!(demuxer.packet_count(), 1);
    let packet = demuxer.next_packet().unwrap().unwrap();
    assert_eq!(packet.codec, Codec::Mp3);
    assert_eq!(packet.data, frame);
    // A codec with no TS stream type is still rejected.
    let mut muxer = TsMuxer::new();
    let err = muxer.add_stream(0x101, Codec::Flac).unwrap_err();
    assert!(matches!(
        err,
        codec_whatttttttttttt_core::Error::Unsupported(_)
    ));
}
