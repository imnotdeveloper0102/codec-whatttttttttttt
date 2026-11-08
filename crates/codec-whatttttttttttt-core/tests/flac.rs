//! Integration tests for FLAC decoding (RFC 9639).
//!
//! Reference vectors are FLAC files encoded from the local WAV vectors
//! with ffmpeg 8.1.2 (Lavc FLAC encoder), so decoding them must
//! reproduce the original PCM samples exactly. See `data/README.md`.

use std::io::Cursor;

use codec_whatttttttttttt_core::{
    decode_flac_frame, parse_flac_stream, Decoder, Error, FlacDemuxer, Frame,
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

fn wav_samples(bytes: &[u8]) -> Vec<f32> {
    let demuxer = codec_whatttttttttttt_core::WavDemuxer::new(Cursor::new(bytes.to_vec())).unwrap();
    demuxer.samples().unwrap()
}

#[test]
fn parses_streaminfo_of_sine_flac() {
    let bytes = include_bytes!("data/sine_stereo_44100.flac");
    let stream = parse_flac_stream(bytes).unwrap();
    assert_eq!(stream.streaminfo.sample_rate, 44100);
    assert_eq!(stream.streaminfo.channels, 2);
    assert_eq!(stream.streaminfo.bits_per_sample, 16);
    assert_eq!(stream.streaminfo.total_samples, 4410);
    assert_eq!(stream.streaminfo.min_block_size, 4608);
    assert_eq!(stream.streaminfo.max_block_size, 4608);
}

#[test]
fn vorbis_comment_metadata_is_parsed() {
    let bytes = include_bytes!("data/sine_stereo_44100.flac");
    let stream = parse_flac_stream(bytes).unwrap();
    let vendor = stream.metadata.get("VENDOR").unwrap();
    assert!(vendor.as_str().unwrap_or("").starts_with("Lavf"));
    // ffmpeg writes the key in lowercase; keys are kept as written.
    let encoder = stream.metadata.get("encoder").unwrap();
    assert_eq!(encoder.as_str().unwrap(), "Lavf62.12.102");
}

#[test]
fn decodes_sine_flac_matches_original_wav() {
    // LPC subframes, left/side stereo (ffmpeg compression level 5).
    let flac = include_bytes!("data/sine_stereo_44100.flac");
    let wav = include_bytes!("data/valid_i16_stereo_44100.wav");
    assert_eq!(decode_flac_bytes(flac), wav_samples(wav));
}

#[test]
fn decodes_fixed_predictor_flac_matches_original_wav() {
    // Fixed-predictor subframes and mid/side stereo frames (ffmpeg
    // compression level 0).
    let flac = include_bytes!("data/sine_stereo_fixed.flac");
    let wav = include_bytes!("data/valid_i16_stereo_44100.wav");
    assert_eq!(decode_flac_bytes(flac), wav_samples(wav));
}

#[test]
fn decodes_silence_flac_constant_subframes() {
    let flac = include_bytes!("data/silence_mono_8000.flac");
    let samples = decode_flac_bytes(flac);
    assert_eq!(samples.len(), 800);
    assert!(samples.iter().all(|&sample| sample == 0.0));
}

#[test]
fn decodes_noise_flac_verbatim_subframes() {
    // White noise does not predict, so the encoder uses verbatim
    // subframes.
    let flac = include_bytes!("data/noise_mono_8000.flac");
    let wav = include_bytes!("data/noise_mono_8000.wav");
    assert_eq!(decode_flac_bytes(flac), wav_samples(wav));
}

#[test]
fn decodes_ramp_lpc_flac_matches_original_wav() {
    let flac = include_bytes!("data/ramp_mono_lpc.flac");
    let wav = include_bytes!("data/ramp_mono_8000.wav");
    assert_eq!(decode_flac_bytes(flac), wav_samples(wav));
}

#[test]
fn decodes_ramp_fixed_flac_matches_original_wav() {
    let flac = include_bytes!("data/ramp_mono_fixed.flac");
    let wav = include_bytes!("data/ramp_mono_8000.wav");
    assert_eq!(decode_flac_bytes(flac), wav_samples(wav));
}

#[test]
fn corrupt_header_crc_returns_invalid_data() {
    let bytes = include_bytes!("data/sine_stereo_44100.flac");
    let stream = parse_flac_stream(bytes).unwrap();
    let mut frame = bytes[stream.first_frame_offset..].to_vec();
    // Change the sample-rate code (44100 -> 32000); the header still
    // parses, so only the CRC-8 can catch the change.
    frame[2] ^= 0x01;
    let err = decode_flac_frame(&frame, &stream.streaminfo).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn corrupt_frame_crc_returns_invalid_data() {
    let bytes = include_bytes!("data/sine_stereo_44100.flac");
    // Flip a bit in the CRC-16 footer (last two bytes of the file).
    // The frame itself still decodes, so only the CRC can catch it.
    let mut corrupted = bytes.to_vec();
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0x01;
    let mut demuxer = FlacDemuxer::new(Cursor::new(corrupted)).unwrap();
    let err = demuxer.next_packet().unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn truncated_frame_returns_truncated() {
    let bytes = include_bytes!("data/sine_stereo_44100.flac");
    let stream = parse_flac_stream(bytes).unwrap();
    let cut = &bytes[..stream.first_frame_offset + 20];
    let mut demuxer = FlacDemuxer::new(Cursor::new(cut.to_vec())).unwrap();
    let err = demuxer.next_packet().unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn truncated_stream_returns_truncated() {
    let err = parse_flac_stream(b"fLaC\x00\x00\x00").unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn bad_marker_returns_invalid_data() {
    let err = parse_flac_stream(b"XXXX\x00\x00\x00\x22").unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn demuxer_packet_counts_match_frames() {
    let sine = include_bytes!("data/sine_stereo_44100.flac");
    let mut demuxer = FlacDemuxer::new(Cursor::new(sine.to_vec())).unwrap();
    let mut count = 0;
    while demuxer.next_packet().unwrap().is_some() {
        count += 1;
    }
    assert_eq!(count, 1);

    let silence = include_bytes!("data/silence_mono_8000.flac");
    let mut demuxer = FlacDemuxer::new(Cursor::new(silence.to_vec())).unwrap();
    let mut count = 0;
    while demuxer.next_packet().unwrap().is_some() {
        count += 1;
    }
    assert_eq!(count, 2);
}

#[test]
fn demuxer_packet_timestamps_increase() {
    let bytes = include_bytes!("data/silence_mono_8000.flac");
    let mut demuxer = FlacDemuxer::new(Cursor::new(bytes.to_vec())).unwrap();
    let first = demuxer.next_packet().unwrap().unwrap();
    let second = demuxer.next_packet().unwrap().unwrap();
    assert_eq!(first.timestamp, 0);
    // 576 frames at 8000 Hz is 72000 microseconds.
    assert_eq!(second.timestamp, 72_000);
}

#[test]
fn fuzz_lpc_prediction_overflow_returns_error() {
    // Fuzz crash (2026-11-08): an LPC subframe whose prediction
    // overflows i64. The decoder must return an error, not panic.
    // Fixture: fuzz artifact, kept whole — `cargo fuzz tmin`
    // could not reduce it (see tests/data/README.md).
    let bytes = include_bytes!("data/flac_lpc_overflow.flac");
    let stream = parse_flac_stream(bytes).unwrap();
    let result = decode_flac_frame(&bytes[stream.first_frame_offset..], &stream.streaminfo);
    assert!(result.is_err(), "expected an error, got {result:?}");
}
