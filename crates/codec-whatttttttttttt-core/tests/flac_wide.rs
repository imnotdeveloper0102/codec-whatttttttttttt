//! Integration tests for wide FLAC decode (RFC 9639): 20- and
//! 24-bit samples and more than two channels.
//!
//! The 24-bit fixtures are real files encoded with ffmpeg 8.1.2
//! (Lavc FLAC encoder) from local CC0 sources (see the subagent
//! report for the exact commands); the `.ref.raw` files are
//! ffmpeg's own decode (`-f f32le`) of the same files, so the
//! decode must match them exactly: both sides divide by the same
//! power of two (2^23 for 24-bit, 2^19 for 20-bit), which is
//! exact in f32.
//!
//! ffmpeg's FLAC encoder cannot write 20-bit files (it rounds
//! `-bits_per_raw_sample 20` up to 24), so the 20-bit fixture is
//! a minimal file crafted by hand in this round: STREAMINFO plus
//! one verbatim frame of 16 samples at 8000 Hz, mono, 20-bit
//! (see the subagent report for the generator). ffmpeg decodes
//! it to the expected samples with max diff 0.0.

use std::io::Cursor;

use codec_whatttttttttttt_core::flac_frame::{
    encode_flac_frame_wide, encode_flac_stream_wide, FlacEncoder,
};
use codec_whatttttttttttt_core::{
    decode_flac_frame, encode_flac_stream, parse_flac_stream, parse_streaminfo, Decoder, Error,
    FlacDemuxer, Frame,
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

fn read_f32le_raw(name: &str) -> Vec<f32> {
    let bytes = std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    assert_eq!(
        bytes.len() % 4,
        0,
        "{name}: reference length is not a multiple of 4"
    );
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .collect()
}

fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(
        a.len(),
        b.len(),
        "length mismatch: {} != {}",
        a.len(),
        b.len()
    );
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x - y).abs())
        .fold(0.0f32, f32::max)
}

#[test]
fn flac24_stereo_decodes_exactly() {
    let bytes = include_bytes!("data/flac24_sine_stereo_44100.flac");
    let stream = parse_flac_stream(bytes).unwrap();
    assert_eq!(stream.streaminfo.sample_rate, 44_100);
    assert_eq!(stream.streaminfo.channels, 2);
    assert_eq!(stream.streaminfo.bits_per_sample, 24);
    assert_eq!(stream.streaminfo.total_samples, 22_050);
    let decoded = decode_flac_bytes(bytes);
    let reference = read_f32le_raw("flac24_sine_stereo_44100_ref.raw");
    let diff = max_abs_diff(&decoded, &reference);
    println!("flac 24-bit stereo max abs diff: {diff}");
    assert!(
        diff == 0.0,
        "24-bit stereo decode differs from ffmpeg by {diff}"
    );
}

#[test]
fn flac24_first_frame_header_reports_24_bit() {
    let bytes = include_bytes!("data/flac24_sine_stereo_44100.flac");
    let stream = parse_flac_stream(bytes).unwrap();
    let (header, _) =
        decode_flac_frame(&bytes[stream.first_frame_offset..], &stream.streaminfo).unwrap();
    assert_eq!(header.sample_rate, 44_100);
    assert_eq!(header.channels, 2);
    // The first frame holds one 4608-sample block per channel.
    assert_eq!(
        header.samples.len(),
        usize::from(stream.streaminfo.max_block_size) * 2
    );
    // Every sample is an exact multiple of 2^-23: the 24-bit
    // values survive the f32 conversion without rounding.
    for &sample in &header.samples {
        let scaled = sample * 8_388_608.0;
        assert_eq!(
            scaled,
            scaled.round(),
            "sample {sample} is not a multiple of 2^-23"
        );
    }
    let peak = header
        .samples
        .iter()
        .map(|&sample| sample.abs())
        .fold(0.0f32, f32::max);
    assert!(peak > 0.08 && peak < 0.09, "unexpected peak {peak}");
}

#[test]
fn flac51_24bit_decodes_exactly() {
    let bytes = include_bytes!("data/flac51_sine_44100.flac");
    let stream = parse_flac_stream(bytes).unwrap();
    assert_eq!(stream.streaminfo.sample_rate, 44_100);
    assert_eq!(stream.streaminfo.channels, 6);
    assert_eq!(stream.streaminfo.bits_per_sample, 24);
    assert_eq!(stream.streaminfo.total_samples, 22_050);
    let decoded = decode_flac_bytes(bytes);
    let reference = read_f32le_raw("flac51_sine_44100_ref.raw");
    let diff = max_abs_diff(&decoded, &reference);
    println!("flac 5.1 24-bit max abs diff: {diff}");
    assert!(
        diff == 0.0,
        "5.1 24-bit decode differs from ffmpeg by {diff}"
    );
    // The six channels carry different sine frequencies, so the
    // first frame of each channel differs from the others.
    let channels = 6usize;
    let frames = decoded.len() / channels;
    assert!(frames > 0);
    let channel_0: Vec<f32> = (0..frames).map(|i| decoded[i * channels]).collect();
    let channel_1: Vec<f32> = (0..frames).map(|i| decoded[i * channels + 1]).collect();
    assert_ne!(channel_0, channel_1);
}

#[test]
fn flac20_crafted_verbatim_frame_decodes_exactly() {
    let bytes = include_bytes!("data/flac20_verbatim_mono_8000.flac");
    let stream = parse_flac_stream(bytes).unwrap();
    assert_eq!(stream.streaminfo.sample_rate, 8000);
    assert_eq!(stream.streaminfo.channels, 1);
    assert_eq!(stream.streaminfo.bits_per_sample, 20);
    assert_eq!(stream.streaminfo.total_samples, 16);
    let (header, consumed) =
        decode_flac_frame(&bytes[stream.first_frame_offset..], &stream.streaminfo).unwrap();
    assert_eq!(header.channels, 1);
    assert_eq!(header.samples.len(), 16);
    assert_eq!(consumed, bytes.len() - stream.first_frame_offset);
    let decoded = decode_flac_bytes(bytes);
    let reference = read_f32le_raw("flac20_verbatim_mono_8000_ref.raw");
    let diff = max_abs_diff(&decoded, &reference);
    println!("flac 20-bit crafted max abs diff: {diff}");
    assert!(
        diff == 0.0,
        "20-bit crafted decode differs from expected by {diff}"
    );
    // The crafted samples span the whole 20-bit range,
    // including both extremes.
    assert!(decoded.iter().any(|&sample| sample > 0.999));
    assert!(decoded.iter().any(|&sample| sample < -0.999));
}

// A sine at the given bit depth, interleaved, as i32 samples.
// The cast is exact: the scaled sine stays inside the bit
// depth's range and far below 2^31.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn sine_i32(frames: usize, channels: usize, sample_rate: u32, amplitude: f64) -> Vec<i32> {
    let mut samples = Vec::with_capacity(frames * channels);
    for n in 0..frames {
        for channel in 0..channels {
            let frequency = 440.0 + 110.0 * channel as f64;
            let value =
                (2.0 * std::f64::consts::PI * frequency * n as f64 / f64::from(sample_rate)).sin()
                    * amplitude;
            samples.push(value as i32);
        }
    }
    samples
}

// The conversion is exact: the samples are below 2^24 and
// `f32` has a 24-bit mantissa, and the scale is a power of two;
// the bit width is 20 or 24, so the u32-to-i32 cast cannot wrap.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]
fn i32_to_f32(samples: &[i32], bits_per_sample: u32) -> Vec<f32> {
    let scale = 1.0f32 / 2f32.powi(bits_per_sample as i32 - 1);
    samples
        .iter()
        .map(|&sample| sample as f32 * scale)
        .collect()
}

#[test]
fn flac24_stereo_encode_round_trips_exactly() {
    let samples = sine_i32(22_050, 2, 44_100, 7_000_000.0);
    let encoded = encode_flac_stream_wide(&samples, 2, 44_100, 24).unwrap();
    let stream = parse_flac_stream(&encoded).unwrap();
    assert_eq!(stream.streaminfo.bits_per_sample, 24);
    assert_eq!(stream.streaminfo.channels, 2);
    assert_eq!(stream.streaminfo.total_samples, 22_050);
    let decoded = decode_flac_bytes(&encoded);
    let expected = i32_to_f32(&samples, 24);
    let diff = max_abs_diff(&decoded, &expected);
    println!("flac 24-bit stereo encode round-trip max abs diff: {diff}");
    assert!(diff == 0.0, "24-bit encode round-trip differs by {diff}");
}

#[test]
fn flac20_mono_encode_round_trips_exactly() {
    // Includes both extremes of the 20-bit range.
    let samples = [
        0, 1, -1, 524_287, -524_288, 123_456, -123_456, 262_144, -262_144, 100_000, -100_000, 7,
        -7, 524_286, -524_287, 42,
    ];
    let encoded = encode_flac_stream_wide(&samples, 1, 8000, 20).unwrap();
    let stream = parse_flac_stream(&encoded).unwrap();
    assert_eq!(stream.streaminfo.bits_per_sample, 20);
    assert_eq!(stream.streaminfo.total_samples, 16);
    let decoded = decode_flac_bytes(&encoded);
    let expected = i32_to_f32(&samples, 20);
    let diff = max_abs_diff(&decoded, &expected);
    println!("flac 20-bit mono encode round-trip max abs diff: {diff}");
    assert!(diff == 0.0, "20-bit encode round-trip differs by {diff}");
}

#[test]
fn flac51_24bit_encode_round_trips_exactly() {
    let samples = sine_i32(22_050, 6, 44_100, 7_000_000.0);
    let encoded = encode_flac_stream_wide(&samples, 6, 44_100, 24).unwrap();
    let stream = parse_flac_stream(&encoded).unwrap();
    assert_eq!(stream.streaminfo.bits_per_sample, 24);
    assert_eq!(stream.streaminfo.channels, 6);
    assert_eq!(stream.streaminfo.total_samples, 22_050);
    let decoded = decode_flac_bytes(&encoded);
    let expected = i32_to_f32(&samples, 24);
    let diff = max_abs_diff(&decoded, &expected);
    println!("flac 5.1 24-bit encode round-trip max abs diff: {diff}");
    assert!(
        diff == 0.0,
        "5.1 24-bit encode round-trip differs by {diff}"
    );
}

#[test]
fn flac24_streaming_encoder_round_trips() {
    let samples = sine_i32(9000, 2, 44_100, 7_000_000.0);
    let mut encoder = FlacEncoder::with_bits_per_sample(2, 44_100, 24).unwrap();
    let header = encoder.streaminfo_bytes();
    let streaminfo = parse_streaminfo(&header[8..42]).unwrap();
    assert_eq!(streaminfo.bits_per_sample, 24);
    let mut streamed = header;
    for chunk in samples.chunks(4096 * 2) {
        streamed.extend_from_slice(&encoder.encode_i32(chunk).unwrap());
    }
    assert_eq!(encoder.sample_number(), 9000);
    let decoded = decode_flac_bytes(&streamed);
    let expected = i32_to_f32(&samples, 24);
    let diff = max_abs_diff(&decoded, &expected);
    println!("flac 24-bit streaming encode round-trip max abs diff: {diff}");
    assert!(diff == 0.0, "24-bit streaming round-trip differs by {diff}");
}

#[test]
// The sine amplitude (30_000) fits i16, so the narrowing cast
// is exact.
#[allow(clippy::cast_possible_truncation)]
fn flac16_via_wide_path_matches_legacy_encoder() {
    // The 16-bit entry points are wrappers around the wide path
    // now, so the wide path at 16 bits must produce byte-identical
    // output to the legacy one-shot encoder.
    let samples_i16: Vec<i16> = sine_i32(9000, 2, 44_100, 30_000.0)
        .iter()
        .map(|&sample| sample as i16)
        .collect();
    let samples_i32: Vec<i32> = samples_i16
        .iter()
        .map(|&sample| i32::from(sample))
        .collect();
    let legacy = encode_flac_stream(&samples_i16, 2, 44_100).unwrap();
    let wide = encode_flac_stream_wide(&samples_i32, 2, 44_100, 16).unwrap();
    assert_eq!(wide, legacy);
    let frame_legacy = encode_flac_frame_wide(&samples_i32[..4096], 2, 44_100, 0, 16).unwrap();
    let frame_wide =
        codec_whatttttttttttt_core::encode_flac_frame(&samples_i16[..4096], 2, 44_100, 0).unwrap();
    assert_eq!(frame_legacy, frame_wide);
}

#[test]
fn flac_wide_rejects_bad_width_and_out_of_range_samples() {
    let samples = sine_i32(16, 1, 8000, 100.0);
    let err = encode_flac_frame_wide(&samples, 1, 8000, 0, 12).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
    let err = encode_flac_frame_wide(&samples, 1, 8000, 0, 32).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
    // 2^23 does not fit in 24 bits.
    let out_of_range = [8_388_608];
    let err = encode_flac_frame_wide(&out_of_range, 1, 8000, 0, 24).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
    // 2^19 does not fit in 20 bits.
    let out_of_range = [524_288];
    let err = encode_flac_frame_wide(&out_of_range, 1, 8000, 0, 20).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
    let err = FlacEncoder::with_bits_per_sample(2, 44_100, 32).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
    let err = encode_flac_stream_wide(&samples, 1, 8000, 12).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
}
