//! Integration tests for MP4 tag (`meta`/`ilst`/`udta`)
//! parsing (ISO/IEC 14496-12).
//!
//! The vectors are real files produced with ffmpeg 8.1.2 from
//! local CC0 sine/testsrc sources with metadata (see the
//! subagent report for the exact commands).

use codec_whatttttttttttt_core::{parse_mp4_tags, Demuxer, Error};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/data/{name}")).unwrap()
}

fn title_of(metadata: &codec_whatttttttttttt_core::Metadata) -> Option<&str> {
    metadata.get("TITLE").and_then(|entry| entry.as_str())
}

#[test]
fn mp4_tags_from_demuxer_metadata() {
    let demuxer = Demuxer::from_bytes(&fixture("avc_aac.mp4")).unwrap();
    let metadata = demuxer.metadata();
    assert_eq!(title_of(metadata), Some("Passthrough AVC"));
    assert_eq!(
        metadata.get("ARTIST").and_then(|e| e.as_str()),
        Some("Test Artist")
    );
    assert_eq!(
        metadata.get("ALBUM").and_then(|e| e.as_str()),
        Some("Test Album")
    );
}

#[test]
fn m4a_title_tag() {
    let tags = parse_mp4_tags(&fixture("sine_aac.m4a")).unwrap();
    assert_eq!(title_of(&tags), Some("M4A Title"));
}

#[test]
fn mp4_tags_on_non_mp4_is_truncated() {
    let err = parse_mp4_tags(&[]).unwrap_err();
    assert!(
        matches!(err, Error::Truncated { .. }),
        "unexpected error: {err:?}"
    );
}
