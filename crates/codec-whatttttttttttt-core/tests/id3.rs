//! Integration tests for ID3v1/v2 reading and writing
//! (`ID3v2` spec).
//!
//! The vector is a real file produced with ffmpeg 8.1.2 from a
//! local CC0 sine source with ID3v2.3 tags (see the subagent
//! report for the exact command).

use codec_whatttttttttttt_core::{parse_id3, parse_id3v1, write_id3v1, write_id3v2, Id3Tag};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/data/{name}")).unwrap()
}

#[test]
fn id3_tagged_mp3_fields() {
    let tag = parse_id3(&fixture("tagged.mp3")).unwrap();
    assert_eq!(tag.title.as_deref(), Some("ID3 Title"));
    assert_eq!(tag.artist.as_deref(), Some("ID3 Artist"));
    assert_eq!(tag.album.as_deref(), Some("ID3 Album"));
    assert_eq!(tag.track, Some(3));
    assert_eq!(tag.year.as_deref(), Some("2026"));
    assert_eq!(tag.genre.as_deref(), Some("Rock"));
}

#[test]
fn id3v2_write_read_round_trip() {
    let tag = Id3Tag {
        title: Some("Round Trip".to_string()),
        artist: Some("Tester".to_string()),
        album: Some("Album".to_string()),
        track: Some(3),
        total_tracks: Some(12),
        year: Some("2026".to_string()),
        genre: Some("Rock".to_string()),
        ..Id3Tag::default()
    };
    let bytes = write_id3v2(&tag).unwrap();
    let (back, _) = codec_whatttttttttttt_core::parse_id3v2(&bytes).unwrap();
    assert_eq!(back.title, tag.title);
    assert_eq!(back.artist, tag.artist);
    assert_eq!(back.album, tag.album);
    assert_eq!(back.track, Some(3));
    assert_eq!(back.total_tracks, Some(12));
    assert_eq!(back.year, tag.year);
    assert_eq!(back.genre, tag.genre);
}

#[test]
fn id3v1_write_read_round_trip() {
    let tag = Id3Tag {
        title: Some("V1 Title".to_string()),
        artist: Some("V1 Artist".to_string()),
        album: Some("V1 Album".to_string()),
        track: Some(3),
        year: Some("2026".to_string()),
        genre: Some("Rock".to_string()),
        ..Id3Tag::default()
    };
    let bytes = write_id3v1(&tag);
    assert_eq!(bytes.len(), 128);
    let back = parse_id3v1(&bytes).unwrap();
    assert_eq!(back.title, tag.title);
    assert_eq!(back.artist, tag.artist);
    assert_eq!(back.album, tag.album);
    assert_eq!(back.track, Some(3));
    assert_eq!(back.year, tag.year);
    assert_eq!(back.genre, tag.genre);
}

#[test]
fn id3_missing_tag_is_invalid_data() {
    let err = parse_id3(b"not a tagged file at all").unwrap_err();
    assert!(matches!(
        err,
        codec_whatttttttttttt_core::Error::InvalidData { .. }
    ));
}
