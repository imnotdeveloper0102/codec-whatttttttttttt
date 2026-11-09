//! Integration tests for Opus CELT sample reconstruction
//! (RFC 6716 section 4.3).
//!
//! Fixtures are real Opus files encoded with ffmpeg 8.1.2
//! (libopus) from synthetic WAVs; see `tests/data/README.md`
//! for the exact commands. The reference PCM is ffmpeg's own
//! decode of the same files (`-f f32le`), so the comparison is
//! against an independent decoder, not against this crate.
//!
//! The CELT fixtures are 48 kHz stereo (CELT-only, fullband,
//! 960 samples/frame, 101 frames, pre-skip 312). The 8 kHz mono
//! fixture in `tests/data` is SILK, which stays `Unsupported`
//! and is deliberately not used here.
//!
//! Thresholds were measured, not guessed: the reference C
//! decoder (RFC 6716 Appendix A) matches ffmpeg to about
//! 1e-6 RMS on these fixtures, so a correct port should land
//! in the same range; the asserts below allow roughly 100x
//! that headroom for float rounding differences.

use std::io::Cursor;

use codec_whatttttttttttt_core::opus_packet::OpusMode;
use codec_whatttttttttttt_core::{
    celt_end_band, parse_opus_head, parse_opus_packet, CeltDecoder, Codec, OggDemuxer,
};

/// Pre-skip in 48 kHz samples (from `OpusHead`).
const PRE_SKIP: usize = 312;
/// Samples per channel in the reference PCM.
const REF_SAMPLES_PER_CHANNEL: usize = 96_000;
/// RMS threshold (measured reference-vs-ffmpeg is ~1e-6).
const RMS_THRESHOLD: f32 = 1e-4;
/// Max absolute sample difference threshold.
const MAX_THRESHOLD: f32 = 1e-3;

/// Decode one Ogg Opus file (CELT-only) to interleaved `f32` PCM.
fn decode_opus_celt(bytes: &[u8]) -> Vec<f32> {
    let mut demuxer = OggDemuxer::new(Cursor::new(bytes)).unwrap();
    assert_eq!(demuxer.codec(), Codec::Opus);
    let head_packet = demuxer.next_packet().unwrap().unwrap();
    let head = parse_opus_head(&head_packet.data).unwrap();
    assert_eq!(head.channels, 2);
    assert_eq!(head.pre_skip, u16::try_from(PRE_SKIP).unwrap());
    // OpusTags (ignored).
    let _ = demuxer.next_packet().unwrap().unwrap();

    let mut decoder = CeltDecoder::new(2).unwrap();
    let mut pcm = Vec::new();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        let parsed = parse_opus_packet(&packet.data).unwrap();
        assert_eq!(parsed.config.mode, OpusMode::Celt);
        assert_eq!(parsed.channels, 2);
        let end_band = celt_end_band(parsed.config.bandwidth);
        let frame_size = parsed.config.frame_duration_samples_48k as usize;
        for frame in &parsed.frames {
            let out = decoder.decode_frame(frame, end_band, frame_size).unwrap();
            pcm.extend_from_slice(&out);
        }
    }
    pcm
}

/// Read a little-endian f32 PCM file.
fn read_ref_f32(bytes: &[u8]) -> Vec<f32> {
    assert_eq!(bytes.len() % 4, 0);
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Compare decoded PCM (after dropping pre-skip) against the
/// ffmpeg reference. Returns `(rms, max_abs)`.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn compare(name: &str, opus_bytes: &[u8], ref_bytes: &[u8]) -> (f32, f32) {
    let pcm = decode_opus_celt(opus_bytes);
    let reference = read_ref_f32(ref_bytes);
    assert_eq!(reference.len(), REF_SAMPLES_PER_CHANNEL * 2);
    // Drop pre-skip (312 samples/channel = 624 interleaved
    // values), then compare the first REF_SAMPLES_PER_CHANNEL
    // samples/channel; the tail is encoder end padding that
    // ffmpeg trims.
    let start = PRE_SKIP * 2;
    let end = start + REF_SAMPLES_PER_CHANNEL * 2;
    assert!(
        pcm.len() >= end,
        "{name}: decoded {} values, need {end}",
        pcm.len()
    );
    let decoded = &pcm[start..end];
    let mut sum_sq = 0.0f64;
    let mut max_abs = 0.0f32;
    for (a, b) in decoded.iter().zip(reference.iter()) {
        let d = f64::from(*a - *b);
        sum_sq += d * d;
        max_abs = max_abs.max((a - b).abs());
    }
    let rms = (sum_sq / decoded.len() as f64).sqrt() as f32;
    (rms, max_abs)
}

fn check_fixture(name: &str, opus: &[u8], reference: &[u8]) {
    let (rms, max_abs) = compare(name, opus, reference);
    // Report the measured values on failure so the threshold
    // can be judged, not guessed.
    assert!(
        rms < RMS_THRESHOLD,
        "{name}: RMS diff {rms:.3e} exceeds threshold {RMS_THRESHOLD:.3e}"
    );
    assert!(
        max_abs < MAX_THRESHOLD,
        "{name}: max abs diff {max_abs:.3e} exceeds threshold {MAX_THRESHOLD:.3e}"
    );
}

#[test]
fn celt_sine_stereo_matches_ffmpeg() {
    check_fixture(
        "sine",
        include_bytes!("data/celt_sine_stereo_48000.opus"),
        include_bytes!("data/celt_sine_stereo_48000_ref.raw"),
    );
}

#[test]
fn celt_noise_stereo_matches_ffmpeg() {
    check_fixture(
        "noise",
        include_bytes!("data/celt_noise_stereo_48000.opus"),
        include_bytes!("data/celt_noise_stereo_48000_ref.raw"),
    );
}

#[test]
fn celt_music_stereo_matches_ffmpeg() {
    check_fixture(
        "music",
        include_bytes!("data/celt_music_stereo_48000.opus"),
        include_bytes!("data/celt_music_stereo_48000_ref.raw"),
    );
}

#[test]
fn celt_speech_stereo_matches_ffmpeg() {
    check_fixture(
        "speech",
        include_bytes!("data/celt_speech_stereo_48000.opus"),
        include_bytes!("data/celt_speech_stereo_48000_ref.raw"),
    );
}

/// A truncated CELT frame returns a clean error, not a panic.
/// A 1-byte frame is below the trust boundary (2..=1275 bytes) and
/// is rejected. A frame cut in half does NOT error: the range
/// decoder reads zeros past the end of the frame, exactly as the
/// reference does (RFC 6716 section 4.1.2.1), so it decodes to
/// garbage rather than failing. The requirement is that it never
/// panics, not that it errors.
#[test]
fn celt_truncated_frame_returns_error_not_panic() {
    let opus = include_bytes!("data/celt_sine_stereo_48000.opus");
    let mut demuxer = OggDemuxer::new(Cursor::new(opus.to_vec())).unwrap();
    let _ = demuxer.next_packet().unwrap().unwrap();
    let _ = demuxer.next_packet().unwrap().unwrap();
    let packet = demuxer.next_packet().unwrap().unwrap();
    let parsed = parse_opus_packet(&packet.data).unwrap();
    let end_band = celt_end_band(parsed.config.bandwidth);
    let frame_size = parsed.config.frame_duration_samples_48k as usize;
    let frame = parsed.frames[0];
    let mut decoder = CeltDecoder::new(2).unwrap();
    // A 1-byte frame is below the trust boundary (2..=1275).
    assert!(decoder
        .decode_frame(&frame[..1], end_band, frame_size)
        .is_err());
    // A frame cut in half decodes (to garbage) without panicking.
    let half = &frame[..frame.len() / 2];
    let _ = decoder.decode_frame(half, end_band, frame_size);
}

/// A corrupt CELT frame (flipped bits) returns a clean error or
/// decodes without panicking; it must never panic.
#[test]
fn celt_corrupt_frame_does_not_panic() {
    let opus = include_bytes!("data/celt_sine_stereo_48000.opus");
    let mut demuxer = OggDemuxer::new(Cursor::new(opus.to_vec())).unwrap();
    let _ = demuxer.next_packet().unwrap().unwrap();
    let _ = demuxer.next_packet().unwrap().unwrap();
    let packet = demuxer.next_packet().unwrap().unwrap();
    let parsed = parse_opus_packet(&packet.data).unwrap();
    let end_band = celt_end_band(parsed.config.bandwidth);
    let frame_size = parsed.config.frame_duration_samples_48k as usize;
    let mut decoder = CeltDecoder::new(2).unwrap();
    for flip_at in [0usize, 5, 20, 60] {
        let mut frame = parsed.frames[0].to_vec();
        if flip_at < frame.len() {
            frame[flip_at] ^= 0xFF;
        }
        // Either an error or some PCM, but never a panic.
        let _ = decoder.decode_frame(&frame, end_band, frame_size);
    }
}
