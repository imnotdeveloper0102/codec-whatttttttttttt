//! Integration tests for WebM/Matroska demuxing (Phase 6).
//!
//! The vectors are real files produced with ffmpeg 8.1.2 (see
//! `tests/data/README.md` for the exact commands). Packet counts,
//! codecs, timestamps, and sizes below are the values ffprobe
//! reports for those files, checked against the demuxer output.

use codec_whatttttttttttt_core::{Codec, Demuxer, Error, TrackKind};

fn webm_bytes(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/data/{name}")).unwrap()
}

fn all_packets(name: &str) -> Vec<(Codec, u64, bool, usize)> {
    let mut demuxer = Demuxer::from_bytes(&webm_bytes(name)).unwrap();
    let mut packets = Vec::new();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        packets.push((
            packet.codec,
            packet.timestamp,
            packet.is_keyframe,
            packet.data.len(),
        ));
    }
    packets
}

#[test]
fn av1_opus_webm_tracks_and_duration() {
    let demuxer = Demuxer::from_bytes(&webm_bytes("av1_opus.webm")).unwrap();
    assert_eq!(demuxer.duration_us(), Some(1_000_000));
    assert_eq!(demuxer.packet_count(), 16);
    let tracks = demuxer.tracks();
    assert_eq!(tracks.len(), 2);
    let audio = &tracks[0];
    assert_eq!(audio.kind, TrackKind::Audio);
    assert_eq!(audio.codec_id, "A_OPUS");
    assert_eq!(audio.codec, Codec::Opus);
    assert_eq!(audio.sample_rate, Some(48_000));
    assert_eq!(audio.channels, Some(2));
    assert_eq!(audio.codec_private.len(), 19);
    let video = &tracks[1];
    assert_eq!(video.kind, TrackKind::Video);
    assert_eq!(video.codec_id, "V_AV1");
    assert_eq!(video.codec, Codec::Av1);
    assert_eq!(video.width, Some(64));
    assert_eq!(video.height, Some(64));
    assert_eq!(video.default_duration_us, Some(100_000));
    assert_eq!(video.codec_private.len(), 16);
}

#[test]
fn av1_opus_webm_packets_in_order() {
    let packets = all_packets("av1_opus.webm");
    assert_eq!(packets.len(), 16);
    // Audio packets carry the raw container timestamps (0, 21, 41,
    // 61, 81, 101 ms); ffprobe shows them 7 ms earlier because it
    // applies the Opus pre-skip, which the demuxer does not.
    let audio: Vec<(u64, usize)> = packets
        .iter()
        .filter(|(codec, _, _, _)| *codec == Codec::Opus)
        .map(|(_, ts, _, size)| (*ts, *size))
        .collect();
    assert_eq!(
        audio,
        vec![
            (0, 224),
            (21_000, 168),
            (41_000, 169),
            (61_000, 159),
            (81_000, 159),
            (101_000, 230),
        ]
    );
    let video: Vec<(u64, bool, usize)> = packets
        .iter()
        .filter(|(codec, _, _, _)| *codec == Codec::Av1)
        .map(|(_, ts, key, size)| (*ts, *key, *size))
        .collect();
    assert_eq!(
        video,
        vec![
            (0, true, 777),
            (100_000, false, 1437),
            (200_000, false, 3),
            (300_000, false, 195),
            (400_000, false, 3),
            (500_000, false, 439),
            (600_000, false, 3),
            (700_000, false, 183),
            (800_000, false, 3),
            (900_000, false, 344),
        ]
    );
    // The first video packet is the keyframe; the 3-byte packets
    // are show_existing frames.
    assert!(packets[1].2);
}

#[test]
fn vp9_vorbis_webm_packets() {
    let demuxer = Demuxer::from_bytes(&webm_bytes("vp9_vorbis.webm")).unwrap();
    assert_eq!(demuxer.packet_count(), 17);
    let tracks = demuxer.tracks();
    assert_eq!(tracks[0].codec, Codec::Vorbis);
    assert_eq!(tracks[0].sample_rate, Some(44_100));
    assert_eq!(tracks[1].codec, Codec::Vp9);
    let packets = all_packets("vp9_vorbis.webm");
    let audio: Vec<u64> = packets
        .iter()
        .filter(|(codec, _, _, _)| *codec == Codec::Vorbis)
        .map(|(_, ts, _, _)| *ts)
        .collect();
    assert_eq!(audio, vec![0, 3_000, 6_000, 19_000, 42_000, 65_000, 89_000]);
    let video: Vec<(u64, usize)> = packets
        .iter()
        .filter(|(codec, _, _, _)| *codec == Codec::Vp9)
        .map(|(_, ts, _, size)| (*ts, *size))
        .collect();
    assert_eq!(
        video,
        vec![
            (0, 1729),
            (100_000, 426),
            (200_000, 444),
            (300_000, 399),
            (400_000, 568),
            (500_000, 508),
            (600_000, 504),
            (700_000, 591),
            (800_000, 629),
            (900_000, 816),
        ]
    );
}

#[test]
fn vp9_keyframes_webm_packet_count() {
    let demuxer = Demuxer::from_bytes(&webm_bytes("vp9_keyframes.webm")).unwrap();
    assert_eq!(demuxer.packet_count(), 61);
    let packets = all_packets("vp9_keyframes.webm");
    let video_keys: Vec<u64> = packets
        .iter()
        .filter(|(codec, _, key, _)| *codec == Codec::Vp9 && *key)
        .map(|(_, ts, _, _)| *ts)
        .collect();
    assert_eq!(video_keys, vec![0, 300_000, 600_000, 900_000]);
}

#[test]
fn corrupt_webm_returns_error_without_panicking() {
    // Truncated mid-cluster.
    let bytes = webm_bytes("av1_opus.webm");
    let err = Demuxer::from_bytes(&bytes[..100]).unwrap_err();
    assert!(matches!(
        err,
        Error::Truncated { .. } | Error::InvalidData { .. }
    ));
    // Garbage bytes.
    let err = Demuxer::from_bytes(&[0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0, 0, 0, 0]).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
    // Empty input.
    let err = Demuxer::from_bytes(&[]).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
    // Random bit flips in the first kilobyte must not panic either.
    let mut flipped = bytes.clone();
    for index in (0..1000).step_by(37) {
        flipped[index] ^= 0xFF;
    }
    let _ = Demuxer::from_bytes(&flipped);
}
