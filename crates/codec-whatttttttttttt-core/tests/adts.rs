//! Integration tests for ADTS demuxing and muxing
//! (ISO/IEC 13818-7).
//!
//! The vectors are real files produced with ffmpeg 8.1.2 from a
//! local CC0 sine source (see the subagent report for the exact
//! commands). The CRC vector was built locally from the ffmpeg
//! frame with a Python-computed CRC-16 (polynomial `0x8005`,
//! init `0xFFFF`).

use codec_whatttttttttttt_core::{
    parse_adts_header, verify_adts_frame, write_adts_frame, AdtsDemuxer, Error,
};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/data/{name}")).unwrap()
}

fn drain(name: &str) -> Vec<codec_whatttttttttttt_core::AdtsFrame> {
    let mut demuxer = AdtsDemuxer::from_bytes(&fixture(name)).unwrap();
    let mut frames = Vec::new();
    while let Some(frame) = demuxer.next_frame().unwrap() {
        frames.push(frame);
    }
    frames
}

#[test]
fn adts_sine_has_45_lc_frames_at_44100_mono() {
    let frames = drain("sine_aac.adts");
    assert_eq!(frames.len(), 45);
    let first = &frames[0].header;
    assert_eq!(first.profile, 1);
    assert_eq!(first.sample_rate, 44_100);
    assert_eq!(first.sampling_frequency_index, 4);
    assert_eq!(first.channels, 1);
    assert!(first.protection_absent);
    assert_eq!(first.frame_length, 160);
    assert_eq!(frames[0].payload.len(), 160 - 7);
    assert_eq!(frames[0].offset, 0);
}

#[test]
fn adts_crc_frame_verifies() {
    let frames = drain("sine_aac_crc.adts");
    assert_eq!(frames.len(), 1);
    let header = &frames[0].header;
    assert!(!header.protection_absent);
    assert!(header.crc.is_some());
    let bytes = fixture("sine_aac_crc.adts");
    assert_eq!(verify_adts_frame(&bytes).unwrap().crc, header.crc);
}

#[test]
fn adts_write_read_round_trip_with_and_without_crc() {
    let payload: Vec<u8> = (0..100u8).map(|i| i % 251).collect();
    for with_crc in [false, true] {
        let bytes = write_adts_frame(&payload, 1, 44_100, 1, with_crc).unwrap();
        let (header, header_len) = parse_adts_header(&bytes).unwrap();
        assert_eq!(header.profile, 1);
        assert_eq!(header.sample_rate, 44_100);
        assert_eq!(header.channels, 1);
        assert_eq!(header.protection_absent, !with_crc);
        assert_eq!(header.frame_length, bytes.len());
        assert_eq!(header_len, if with_crc { 9 } else { 7 });
        let mut demuxer = AdtsDemuxer::from_bytes(&bytes).unwrap();
        let frame = demuxer.next_frame().unwrap().unwrap();
        assert_eq!(frame.payload, payload);
        assert!(demuxer.next_frame().unwrap().is_none());
    }
}

#[test]
fn adts_corrupted_crc_is_invalid_data() {
    let mut bytes = fixture("sine_aac_crc.adts");
    // Flip a payload byte; the CRC over header + payload no
    // longer matches the stored CRC.
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    let err = verify_adts_frame(&bytes).unwrap_err();
    assert!(
        matches!(err, Error::InvalidData { .. }),
        "unexpected error: {err:?}"
    );
}

#[test]
fn adts_cut_short_tail_is_truncated() {
    let bytes = fixture("sine_aac.adts");
    let cut = &bytes[..100];
    let mut demuxer = AdtsDemuxer::from_bytes(cut).unwrap();
    let err = demuxer.next_frame().unwrap_err();
    assert!(
        matches!(err, Error::Truncated { .. }),
        "unexpected error: {err:?}"
    );
}
