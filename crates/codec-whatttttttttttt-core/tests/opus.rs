//! Integration tests for Opus (RFC 6716, RFC 7845).
//!
//! The reference vectors are real Opus files encoded with ffmpeg
//! 8.1.2 (libopus) from the local CC0 WAV vectors; see
//! `tests/data/README.md` for the exact commands. Packing codes the
//! real files do not exercise (code 1, 2, 3) use hand-built packets
//! whose bytes follow RFC 6716 section 3.2.
//!
//! CELT sample reconstruction is NOT implemented in this phase
//! (RFC 6716 Appendix A tables), so the end-to-end decode test
//! asserts the honest `Unsupported` error, not fabricated PCM.
//! Silence CELT frames do decode, and are used to verify pre-skip.

use std::io::Cursor;

use codec_whatttttttttttt_core::{
    decode_celt_frame_header, parse_opus_head, parse_opus_packet, parse_opus_tags, Codec, Decoder,
    Error, Frame, OggDemuxer, Packet, RangeDecoder, RangeEncoder,
};

fn toc(config: u8, stereo: bool, code: u8) -> u8 {
    (config << 3) | (u8::from(stereo) << 2) | code
}

fn stereo_bytes() -> Vec<u8> {
    include_bytes!("data/sine_stereo_44100.opus").to_vec()
}

fn mono_bytes() -> Vec<u8> {
    include_bytes!("data/sine_mono_8000.opus").to_vec()
}

/// The first two packets of an Ogg Opus file: `OpusHead`, `OpusTags`.
fn first_two_packets(bytes: Vec<u8>) -> (Vec<u8>, Vec<u8>, OggDemuxer) {
    let mut demuxer = OggDemuxer::new(Cursor::new(bytes)).unwrap();
    assert_eq!(demuxer.codec(), Codec::Opus);
    let head = demuxer.next_packet().unwrap().unwrap();
    let tags = demuxer.next_packet().unwrap().unwrap();
    (head.data, tags.data, demuxer)
}

#[test]
fn opus_head_parses_from_real_stereo_file() {
    let (head, _, _) = first_two_packets(stereo_bytes());
    let parsed = parse_opus_head(&head).unwrap();
    assert_eq!(parsed.channels, 2);
    assert_eq!(parsed.pre_skip, 312);
    assert_eq!(parsed.input_sample_rate, 48000);
    assert_eq!(parsed.output_gain_db(), 0.0);
    assert_eq!(parsed.channel_mapping_family, 0);
}

#[test]
fn opus_head_parses_from_real_mono_file() {
    let (head, _, _) = first_two_packets(mono_bytes());
    let parsed = parse_opus_head(&head).unwrap();
    assert_eq!(parsed.channels, 1);
    assert_eq!(parsed.pre_skip, 312);
    assert_eq!(parsed.input_sample_rate, 8000);
    assert_eq!(parsed.channel_mapping_family, 0);
}

#[test]
fn opus_tags_parse_from_real_file() {
    let (_, tags, _) = first_two_packets(stereo_bytes());
    let parsed = parse_opus_tags(&tags).unwrap();
    assert_eq!(parsed.vendor, b"Lavf62.12.102");
    assert_eq!(parsed.comments.len(), 1);
    assert_eq!(parsed.comments[0], b"encoder=Lavc62.28.102 libopus");
}

#[test]
fn real_file_uses_code_0_celt_only_packets() {
    let (_, _, mut demuxer) = first_two_packets(stereo_bytes());
    let mut audio_packets = 0;
    while let Some(packet) = demuxer.next_packet().unwrap() {
        let parsed = parse_opus_packet(&packet.data).unwrap();
        // TOC 0xfc: config 31 (CELT-only, fullband, 20 ms),
        // stereo, code 0.
        assert_eq!(parsed.config.number, 31);
        assert_eq!(parsed.channels, 2);
        assert_eq!(parsed.frame_count(), 1);
        assert_eq!(parsed.total_duration_samples_48k(), 960);
        audio_packets += 1;
    }
    assert_eq!(audio_packets, 6);
}

#[test]
fn packing_code_1_two_equal_frames() {
    // N = 7 bytes total, payload 6, each frame 3 bytes.
    let packet = [toc(19, false, 1), 1, 2, 3, 4, 5, 6];
    let parsed = parse_opus_packet(&packet).unwrap();
    assert_eq!(parsed.frame_count(), 2);
    assert_eq!(parsed.frames[0], &[1, 2, 3]);
    assert_eq!(parsed.frames[1], &[4, 5, 6]);
}

#[test]
fn packing_code_2_two_different_frames() {
    // First frame length 2 (one length byte), then 2 + 3 bytes.
    let packet = [toc(19, false, 2), 2, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
    let parsed = parse_opus_packet(&packet).unwrap();
    assert_eq!(parsed.frame_count(), 2);
    assert_eq!(parsed.frames[0], &[0xAA, 0xBB]);
    assert_eq!(parsed.frames[1], &[0xCC, 0xDD, 0xEE]);
}

#[test]
fn packing_code_3_vbr_three_frames() {
    // Count byte: M=3, VBR set. Lengths 2 and 3 coded, last frame
    // takes the rest (4 bytes).
    let packet = [toc(19, false, 3), 0x83, 2, 3, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    let parsed = parse_opus_packet(&packet).unwrap();
    assert_eq!(parsed.frame_count(), 3);
    assert_eq!(parsed.frames[0], &[1, 2]);
    assert_eq!(parsed.frames[1], &[3, 4, 5]);
    assert_eq!(parsed.frames[2], &[6, 7, 8, 9]);
}

#[test]
fn packing_code_3_cbr_with_padding() {
    // Count byte: M=2, CBR, padding set. Padding-length byte 2
    // means 2 padding bytes; R = 4, so each frame is 2 bytes.
    let packet = [toc(19, false, 3), 0x42, 2, 0xAA, 0xBB, 0xCC, 0xDD, 0, 0];
    let parsed = parse_opus_packet(&packet).unwrap();
    assert_eq!(parsed.frame_count(), 2);
    assert_eq!(parsed.frames[0], &[0xAA, 0xBB]);
    assert_eq!(parsed.frames[1], &[0xCC, 0xDD]);
}

#[test]
fn empty_packet_returns_truncated() {
    let err = parse_opus_packet(&[]).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn truncated_code_3_returns_truncated() {
    // Code 3 needs a count byte after the TOC.
    let packet = [toc(19, false, 3)];
    let err = parse_opus_packet(&packet).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn code_3_zero_frames_returns_invalid_data() {
    let packet = [toc(19, false, 3), 0x00];
    let err = parse_opus_packet(&packet).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn code_1_odd_payload_returns_invalid_data() {
    // N = 6 bytes total, payload 5 (odd) violates [R3].
    let packet = [toc(19, false, 1), 1, 2, 3, 4, 5];
    let err = parse_opus_packet(&packet).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn celt_header_decodes_on_real_frames_then_reports_unsupported() {
    // Every audio frame of the real files is a non-silence CELT
    // frame. The header symbols (silence, post-filter, transient,
    // intra) must decode without error, so the function reaches its
    // Unsupported return for the unimplemented sample
    // reconstruction; an earlier failure would surface as
    // InvalidData or Truncated instead.
    for bytes in [stereo_bytes(), mono_bytes()] {
        let (_, _, mut demuxer) = first_two_packets(bytes);
        let mut frames = 0;
        while let Some(packet) = demuxer.next_packet().unwrap() {
            let parsed = parse_opus_packet(&packet.data).unwrap();
            for frame in &parsed.frames {
                let err = decode_celt_frame_header(frame).unwrap_err();
                match err {
                    Error::Unsupported(detail) => {
                        assert!(detail.contains("CELT sample reconstruction"), "{detail}");
                    }
                    other => panic!("expected Unsupported, got {other:?}"),
                }
                frames += 1;
            }
        }
        assert_eq!(frames, 6);
    }
}

#[test]
fn decoder_reports_unsupported_for_celt_samples_on_real_file() {
    let (head, _, mut demuxer) = first_two_packets(stereo_bytes());
    let parsed_head = parse_opus_head(&head).unwrap();
    let mut decoder = Decoder::from_opus_head(&parsed_head).unwrap();
    let packet = demuxer.next_packet().unwrap().unwrap();
    let err = decoder.decode(&packet).unwrap_err();
    match err {
        Error::Unsupported(detail) => {
            // `decode_celt_frame_header` propagates its own
            // Unsupported for a non-silence frame.
            assert!(detail.contains("CELT sample reconstruction"), "{detail}");
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn decoder_rejects_silk_mode_as_unsupported() {
    // Config 0 is SILK-only, narrowband, 10 ms, mono.
    let head_bytes = {
        let mut v = b"OpusHead".to_vec();
        v.extend_from_slice(&[1, 1, 0, 0, 0x80, 0xBB, 0, 0, 0, 0, 0]);
        v
    };
    let head = parse_opus_head(&head_bytes).unwrap();
    let mut decoder = Decoder::from_opus_head(&head).unwrap();
    let packet = Packet {
        codec: Codec::Opus,
        data: vec![toc(0, false, 0), 0xAA, 0xBB],
        timestamp: 480,
        is_keyframe: true,
    };
    let err = decoder.decode(&packet).unwrap_err();
    match err {
        Error::Unsupported(detail) => {
            assert!(detail.contains("silk"), "{detail}");
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

/// Encodes a CELT silence frame with the test range encoder.
fn silence_celt_frame() -> Vec<u8> {
    let mut encoder = RangeEncoder::new();
    encoder.encode_bit_logp(15, true).unwrap();
    encoder.done()
}

#[test]
fn silence_frame_decodes_and_pre_skip_is_subtracted() {
    // TOC 0xfc: config 31 (CELT-only, fullband, 20 ms = 960
    // samples at 48 kHz), stereo, code 0. The OpusHead pre-skip
    // is 312 samples, so the first decoded frame keeps 960 - 312
    // = 648 frames of silence.
    let (head, _, _) = first_two_packets(stereo_bytes());
    let parsed_head = parse_opus_head(&head).unwrap();
    let mut decoder = Decoder::from_opus_head(&parsed_head).unwrap();
    let mut packet_data = vec![toc(31, true, 0)];
    packet_data.extend_from_slice(&silence_celt_frame());
    let packet = Packet {
        codec: Codec::Opus,
        data: packet_data,
        timestamp: 960,
        is_keyframe: true,
    };
    match decoder.decode(&packet).unwrap() {
        Frame::Audio(audio) => {
            assert_eq!(audio.sample_rate, 48000);
            assert_eq!(audio.channels, 2);
            assert_eq!(audio.frame_count(), 648);
            assert!(audio.samples.iter().all(|&s| s == 0.0));
            assert_eq!(audio.timestamp, 0);
        }
        Frame::Video(_) => panic!("expected an audio frame"),
    }
    // The pre-skip is spent; a second silence frame keeps all 960.
    let mut packet_data = vec![toc(31, true, 0)];
    packet_data.extend_from_slice(&silence_celt_frame());
    let packet = Packet {
        codec: Codec::Opus,
        data: packet_data,
        timestamp: 1920,
        is_keyframe: true,
    };
    match decoder.decode(&packet).unwrap() {
        Frame::Audio(audio) => {
            assert_eq!(audio.frame_count(), 960);
            assert_eq!(audio.timestamp, 20_000);
        }
        Frame::Video(_) => panic!("expected an audio frame"),
    }
}

#[test]
fn plc_returns_none_before_first_packet_then_silence() {
    let (head, _, _) = first_two_packets(stereo_bytes());
    let parsed_head = parse_opus_head(&head).unwrap();
    let mut decoder = Decoder::from_opus_head(&parsed_head).unwrap();
    assert_eq!(decoder.conceal_loss().unwrap(), None);

    // Decode one silence frame to set the last frame duration.
    let mut packet_data = vec![toc(31, true, 0)];
    packet_data.extend_from_slice(&silence_celt_frame());
    let packet = Packet {
        codec: Codec::Opus,
        data: packet_data,
        timestamp: 960,
        is_keyframe: true,
    };
    decoder.decode(&packet).unwrap();

    // Two consecutive losses get consecutive timestamps and do
    // not panic.
    for expected_timestamp in [20_000u64, 40_000] {
        match decoder.conceal_loss().unwrap() {
            Some(Frame::Audio(audio)) => {
                assert_eq!(audio.frame_count(), 960);
                assert!(audio.samples.iter().all(|&s| s == 0.0));
                assert_eq!(audio.timestamp, expected_timestamp);
            }
            other => panic!("expected a silence audio frame, got {other:?}"),
        }
    }
}

#[test]
fn range_decoder_round_trip() {
    // The test encoder/decoder pair must invert each other for
    // symbols, bits, uints, and raw bits (RFC 6716 sections 4.1,
    // 5.1).
    let mut encoder = RangeEncoder::new();
    encoder.encode_symbol(&[10, 20, 30, 40], 2).unwrap();
    encoder.encode_bit_logp(4, true).unwrap();
    encoder.encode_uint(300, 1000).unwrap();
    encoder.encode_bits(0xA5, 8).unwrap();
    let bytes = encoder.done();

    let mut decoder = RangeDecoder::new(&bytes);
    assert_eq!(decoder.decode_symbol(&[10, 20, 30, 40]).unwrap(), 2);
    assert!(decoder.decode_bit_logp(4).unwrap());
    assert_eq!(decoder.decode_uint(1000).unwrap(), 300);
    assert_eq!(decoder.decode_bits(8).unwrap(), 0xA5);
}

#[test]
fn opus_head_family1_stream_count_sum_overflow_returns_error() {
    // Fuzz crash (2026-11-08): stream_count 235 + coupled_count 43
    // overflows u8 in the mapping-entry check. The header is also
    // inconsistent per RFC 7845 section 5.1.1: the decoded channel
    // count (M + N = 278) MUST be no larger than 255. The output
    // channel count (2) need not equal M + N.
    let head: [u8; 23] = [
        0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, // "OpusHead"
        0x01, // version
        0x02, // channels
        0x38, 0x01, // pre-skip
        0x80, 0xbb, 0x00, 0x01, // input sample rate
        0x00, 0x00, // output gain
        0x01, // channel mapping family 1
        0xeb, // stream_count 235
        0x2b, // coupled_count 43
        0x00, 0xff, // mapping
    ];
    assert!(parse_opus_head(&head).is_err());
}
