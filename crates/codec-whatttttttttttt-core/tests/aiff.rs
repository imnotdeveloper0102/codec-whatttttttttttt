//! Integration tests for AIFF/AIFF-C demuxing and muxing
//! (Apple AIFF spec / EA IFF 85).
//!
//! The vectors are real files produced with ffmpeg 8.1.2 from a
//! local CC0 sine source (see the subagent report for the exact
//! commands).

use codec_whatttttttttttt_core::{
    parse_aiff, write_aiff, write_extended_float, AiffDemuxer, AiffForm, AiffHeader, Error,
};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/data/{name}")).unwrap()
}

#[test]
fn aiff_sine_header_and_samples() {
    let bytes = fixture("sine_aiff.aiff");
    let stream = parse_aiff(&bytes).unwrap();
    assert_eq!(stream.header.form, AiffForm::Aiff);
    assert_eq!(stream.header.channels, 1);
    assert_eq!(stream.header.frames, 44_100);
    assert_eq!(stream.header.sample_size, 16);
    assert_eq!(stream.header.sample_rate, 44_100.0);
    assert_eq!(stream.data_len, 88_200);
    let mut demuxer = AiffDemuxer::from_bytes(&bytes).unwrap();
    let mut total = 0usize;
    let mut first_timestamp = None;
    while let Some(packet) = demuxer.next_packet().unwrap() {
        if first_timestamp.is_none() {
            first_timestamp = Some(packet.timestamp);
        }
        total += packet.data.len();
    }
    assert_eq!(first_timestamp, Some(0));
    assert_eq!(total, 88_200);
}

#[test]
fn aifc_sowt_is_little_endian() {
    let bytes = fixture("sine_aifc.aifc");
    let stream = parse_aiff(&bytes).unwrap();
    assert_eq!(stream.header.form, AiffForm::Aifc);
    assert_eq!(stream.header.compression, Some(*b"sowt"));
    assert!(stream.header.is_little_endian());
    assert_eq!(stream.header.channels, 1);
    assert_eq!(stream.header.sample_rate, 44_100.0);
}

#[test]
fn aiff_mark_and_comment_chunks_parse() {
    // Hand-built AIFF: COMM + MARK + COMT + SSND.
    let mut file: Vec<u8> = Vec::new();
    file.extend_from_slice(b"FORM");
    file.extend_from_slice(&[0, 0, 0, 0]); // size, patched below
    file.extend_from_slice(b"AIFF");
    let chunk = |file: &mut Vec<u8>, id: &[u8; 4], body: &[u8]| {
        file.extend_from_slice(id);
        // Test-built chunks are a few bytes; the length always fits u32.
        let body_len = u32::try_from(body.len()).expect("test chunk body fits u32");
        file.extend_from_slice(&body_len.to_be_bytes());
        file.extend_from_slice(body);
        if body.len() % 2 == 1 {
            file.push(0);
        }
    };
    let mut comm: Vec<u8> = Vec::new();
    comm.extend_from_slice(&1u16.to_be_bytes()); // channels
    comm.extend_from_slice(&1u32.to_be_bytes()); // frames
    comm.extend_from_slice(&16u16.to_be_bytes()); // sample size
    comm.extend_from_slice(&write_extended_float(44_100.0).unwrap());
    chunk(&mut file, b"COMM", &comm);
    let mut mark: Vec<u8> = Vec::new();
    mark.extend_from_slice(&1u16.to_be_bytes()); // one marker
    mark.extend_from_slice(&1u16.to_be_bytes()); // id
    mark.extend_from_slice(&0u32.to_be_bytes()); // position
    mark.push(1); // Pascal name length
    mark.push(b'A');
    chunk(&mut file, b"MARK", &mark);
    let mut comment: Vec<u8> = Vec::new();
    comment.extend_from_slice(&1u16.to_be_bytes()); // one comment
    comment.extend_from_slice(&0u32.to_be_bytes()); // timestamp
    comment.extend_from_slice(&1u16.to_be_bytes()); // marker id
    comment.extend_from_slice(&2u16.to_be_bytes()); // text length
    comment.extend_from_slice(b"hi");
    chunk(&mut file, b"COMT", &comment);
    let mut ssnd: Vec<u8> = Vec::new();
    ssnd.extend_from_slice(&0u32.to_be_bytes()); // offset
    ssnd.extend_from_slice(&0u32.to_be_bytes()); // block size
    ssnd.extend_from_slice(&[0x12, 0x34]); // one 16-bit sample
    chunk(&mut file, b"SSND", &ssnd);
    let form_size = u32::try_from(file.len() - 8).unwrap();
    file[4..8].copy_from_slice(&form_size.to_be_bytes());

    let stream = parse_aiff(&file).unwrap();
    assert_eq!(stream.markers.len(), 1);
    assert_eq!(stream.markers[0].id, 1);
    assert_eq!(stream.markers[0].position, 0);
    assert_eq!(stream.markers[0].name, "A");
    assert_eq!(stream.comments.len(), 1);
    assert_eq!(stream.comments[0].marker_id, 1);
    assert_eq!(stream.comments[0].text, "hi");
    assert_eq!(stream.data_len, 2);
}

#[test]
fn aiff_write_read_round_trip() {
    let header = AiffHeader {
        form: AiffForm::Aiff,
        channels: 1,
        frames: 2,
        sample_size: 16,
        sample_rate: 44_100.0,
        compression: None,
        compression_name: String::new(),
    };
    let samples: Vec<u8> = vec![0x12, 0x34, 0x56, 0x78];
    let bytes = write_aiff(&header, &samples).unwrap();
    let stream = parse_aiff(&bytes).unwrap();
    assert_eq!(stream.header.channels, 1);
    assert_eq!(stream.header.frames, 2);
    assert_eq!(stream.data_len, 4);
    let mut demuxer = AiffDemuxer::from_bytes(&bytes).unwrap();
    let packet = demuxer.next_packet().unwrap().unwrap();
    assert_eq!(packet.data, samples);
    assert!(demuxer.next_packet().unwrap().is_none());
}

#[test]
fn aiff_cut_short_is_truncated() {
    let bytes = fixture("sine_aiff.aiff");
    let cut = &bytes[..100];
    let err = parse_aiff(cut).unwrap_err();
    assert!(
        matches!(err, Error::Truncated { .. }),
        "unexpected error: {err:?}"
    );
}
