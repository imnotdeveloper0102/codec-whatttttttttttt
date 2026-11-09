//! Integration tests for Vorbis comment packets (Vorbis I
//! section 5).
//!
//! The FLAC vector is a real file produced with ffmpeg 8.1.2
//! from a local CC0 sine source (see `tests/data/README.md`).

use codec_whatttttttttttt_core::{
    parse_flac_stream, parse_vorbis_comment_packet, write_vorbis_comment_packet, VorbisComment,
};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/data/{name}")).unwrap()
}

#[test]
fn vorbis_comment_round_trip_from_flac_metadata() {
    let stream = parse_flac_stream(&fixture("sine_stereo_44100.flac")).unwrap();
    let comment = VorbisComment::from_metadata(&stream.metadata);
    let bytes = comment.write().unwrap();
    let back = parse_vorbis_comment_packet(&bytes).unwrap();
    assert_eq!(back, comment);
}

#[test]
fn vorbis_comment_write_read_round_trip() {
    let mut comment = VorbisComment::new(b"test vendor");
    comment.set("TITLE", b"Round Trip").unwrap();
    comment.set("artist", b"Tester").unwrap();
    let bytes = write_vorbis_comment_packet(&comment).unwrap();
    let back = parse_vorbis_comment_packet(&bytes).unwrap();
    assert_eq!(back.vendor, b"test vendor");
    // Keys are stored uppercased and matched case-insensitively.
    assert_eq!(back.get_str("title"), Some("Round Trip"));
    assert_eq!(back.get_str("ARTIST"), Some("Tester"));
    assert_eq!(back, comment);
}

#[test]
fn vorbis_comment_missing_framing_bit_is_rejected() {
    let mut comment = VorbisComment::new(b"vendor");
    comment.set("TITLE", b"x").unwrap();
    let mut bytes = comment.write().unwrap();
    // The last byte is the framing bit; clearing it must fail.
    let last = bytes.len() - 1;
    bytes[last] = 0;
    let err = parse_vorbis_comment_packet(&bytes).unwrap_err();
    assert!(matches!(
        err,
        codec_whatttttttttttt_core::Error::InvalidData { .. }
    ));
}
