//! Integration tests for WAV parsing, decoding, and round-trip encoding.

use std::io::Cursor;

use codec_whatttttttttttt_core::{
    parse_wav_header, Decoder, Encoder, EncoderConfig, Error, PcmFormat, WavDemuxer, WavMuxer,
};

fn valid_stereo() -> Vec<u8> {
    include_bytes!("data/valid_i16_stereo_44100.wav").to_vec()
}

#[test]
fn parses_valid_stereo_wav_header() {
    let header = parse_wav_header(&valid_stereo()).unwrap();
    assert_eq!(header.format, PcmFormat::I16);
    assert_eq!(header.channels, 2);
    assert_eq!(header.sample_rate, 44100);
    assert_eq!(header.frame_count(), 4410);
}

#[test]
fn decodes_valid_wav_to_expected_samples() {
    let demuxer = WavDemuxer::new(Cursor::new(valid_stereo())).unwrap();
    let samples = demuxer.samples().unwrap();
    assert_eq!(samples.len(), 4410 * 2);
    // First sample is sin(0) == 0; second channel is the same sine.
    assert!(samples[0].abs() < 1e-4);
    assert!(samples.iter().all(|s| *s >= -1.0 && *s <= 1.0));
}

#[test]
fn decodes_truncated_header_returns_truncated() {
    let bytes = include_bytes!("data/truncated_header.wav");
    let err = parse_wav_header(bytes).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn decodes_truncated_frame_returns_error() {
    let bytes = include_bytes!("data/truncated_data.wav");
    let err = parse_wav_header(bytes).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn declared_size_larger_than_file_returns_truncated() {
    let bytes = include_bytes!("data/declared_too_large.wav");
    let err = parse_wav_header(bytes).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn bad_magic_returns_invalid_data() {
    let bytes = include_bytes!("data/bad_magic.wav");
    let err = parse_wav_header(bytes).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn round_trip_pcm_i16_preserves_samples() {
    let input = valid_stereo();
    let mut demuxer = WavDemuxer::new(Cursor::new(input)).unwrap();
    let header = *demuxer.header();
    let mut decoder = Decoder::from_wav_header(&header).unwrap();
    let mut encoder = Encoder::new(EncoderConfig {
        sample_rate: header.sample_rate,
        channels: header.channels,
        pcm_format: header.format,
        ..EncoderConfig::default()
    })
    .unwrap();
    let mut out = Vec::new();
    let mut muxer =
        WavMuxer::new(&mut out, header.format, header.channels, header.sample_rate).unwrap();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        let frame = decoder.decode(&packet).unwrap();
        let packet_out = encoder.encode(&frame).unwrap();
        muxer.write_packet(&packet_out).unwrap();
    }
    muxer.finish().unwrap();
    let written_header = parse_wav_header(&out).unwrap();
    assert_eq!(written_header.frame_count(), header.frame_count());
    assert_eq!(out, valid_stereo());
}

#[test]
fn round_trip_pcm_f32_preserves_samples_exactly() {
    let input = include_bytes!("data/valid_f32_mono_44100.wav").to_vec();
    let mut demuxer = WavDemuxer::new(Cursor::new(input)).unwrap();
    let header = *demuxer.header();
    assert_eq!(header.format, PcmFormat::F32);
    let mut decoder = Decoder::from_wav_header(&header).unwrap();
    let mut encoder = Encoder::new(EncoderConfig {
        sample_rate: header.sample_rate,
        channels: header.channels,
        pcm_format: header.format,
        ..EncoderConfig::default()
    })
    .unwrap();
    let mut out = Vec::new();
    let mut muxer =
        WavMuxer::new(&mut out, header.format, header.channels, header.sample_rate).unwrap();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        let frame = decoder.decode(&packet).unwrap();
        let packet_out = encoder.encode(&frame).unwrap();
        muxer.write_packet(&packet_out).unwrap();
    }
    muxer.finish().unwrap();
    let written_header = parse_wav_header(&out).unwrap();
    assert_eq!(written_header.frame_count(), header.frame_count());
    let original = include_bytes!("data/valid_f32_mono_44100.wav").to_vec();
    assert_eq!(out, original);
}

#[test]
fn seek_to_frame_returns_audio_from_there() {
    let mut demuxer = WavDemuxer::new(Cursor::new(valid_stereo())).unwrap();
    demuxer.seek_to_frame(2205).unwrap();
    let packet = demuxer.next_packet().unwrap().unwrap();
    assert_eq!(packet.data.len(), 2205 * 2 * 2);
}
