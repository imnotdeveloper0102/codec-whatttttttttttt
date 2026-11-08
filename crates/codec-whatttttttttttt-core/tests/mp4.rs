//! Integration tests for MP4 demuxing (Phase 6, ISO BMFF).
//!
//! The vectors are real files produced with ffmpeg 8.1.2 (see
//! `tests/data/README.md` for the exact commands). Packet counts,
//! codecs, timestamps, and sizes below are the values ffprobe
//! reports for those files, checked against the demuxer output.

use codec_whatttttttttttt_core::{parse_box_header, Codec, Demuxer, Error, TrackKind};

fn mp4_bytes(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/data/{name}")).unwrap()
}

fn all_packets(name: &str) -> Vec<(Codec, u64, bool, usize)> {
    let mut demuxer = Demuxer::from_bytes(&mp4_bytes(name)).unwrap();
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
fn box_size_smaller_than_header_is_invalid_data() {
    // Declared size 4, smaller than the 8-byte box header.
    let data = [0x00, 0x00, 0x00, 0x04, b'f', b't', b'y', b'p'];
    let err = parse_box_header(&data, 0, data.len()).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn box_overrunning_the_file_is_truncated() {
    // Declared size 24, only 12 bytes present.
    let data = [
        0x00, 0x00, 0x00, 0x18, b'f', b't', b'y', b'p', 0x69, 0x73, 0x6F, 0x6D,
    ];
    let err = parse_box_header(&data, 0, data.len()).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn box_size_zero_means_to_parent_end() {
    let data = [
        0x00, 0x00, 0x00, 0x00, b'f', b'r', b'e', b'e', 0x01, 0x02, 0x03, 0x04,
    ];
    let header = parse_box_header(&data, 0, data.len()).unwrap();
    assert_eq!(header.size, 12);
    assert_eq!(header.data_start, 8);
}

#[test]
fn box_largesize_parses() {
    let mut data = vec![0x00, 0x00, 0x00, 0x01, b'm', b'd', b'a', b't'];
    data.extend_from_slice(&24u64.to_be_bytes());
    data.extend(vec![0u8; 16]);
    let header = parse_box_header(&data, 0, data.len()).unwrap();
    assert_eq!(header.size, 24);
    assert_eq!(header.data_start, 16);
}

#[test]
fn av1_opus_mp4_tracks_and_duration() {
    let demuxer = Demuxer::from_bytes(&mp4_bytes("av1_opus.mp4")).unwrap();
    assert_eq!(demuxer.duration_us(), Some(1_000_000));
    assert_eq!(demuxer.packet_count(), 16);
    let tracks = demuxer.tracks();
    assert_eq!(tracks.len(), 2);
    let audio = &tracks[0];
    assert_eq!(audio.kind, TrackKind::Audio);
    assert_eq!(audio.codec, Codec::Opus);
    assert_eq!(audio.sample_rate, Some(48_000));
    assert_eq!(audio.channels, Some(2));
    // The edit list carries the 312-sample Opus pre-skip; the
    // track duration is 106.5 ms of media time.
    assert_eq!(audio.duration_us, Some(106_500));
    assert_eq!(audio.codec_private.len(), 11);
    let video = &tracks[1];
    assert_eq!(video.kind, TrackKind::Video);
    assert_eq!(video.codec, Codec::Av1);
    assert_eq!(video.width, Some(64));
    assert_eq!(video.height, Some(64));
    assert_eq!(video.duration_us, Some(1_000_000));
    assert_eq!(video.codec_private.len(), 16);
}

#[test]
fn av1_opus_mp4_packets_in_order() {
    let packets = all_packets("av1_opus.mp4");
    assert_eq!(packets.len(), 16);
    let audio: Vec<(u64, usize)> = packets
        .iter()
        .filter(|(codec, _, _, _)| *codec == Codec::Opus)
        .map(|(_, ts, _, size)| (*ts, *size))
        .collect();
    assert_eq!(
        audio,
        vec![
            (0, 224),
            (20_000, 168),
            (40_000, 169),
            (60_000, 159),
            (80_000, 159),
            (100_000, 230),
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
}

#[test]
fn vp9_opus_mp4_packets() {
    let demuxer = Demuxer::from_bytes(&mp4_bytes("vp9_opus.mp4")).unwrap();
    assert_eq!(demuxer.packet_count(), 16);
    let packets = all_packets("vp9_opus.mp4");
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
fn av1_keyframes_mp4_sync_samples() {
    let demuxer = Demuxer::from_bytes(&mp4_bytes("av1_keyframes.mp4")).unwrap();
    assert_eq!(demuxer.packet_count(), 61);
    let packets = all_packets("av1_keyframes.mp4");
    // libaom emits an extra sync sample after each forced
    // keyframe, so the keyframes are at 0, 100, 300, 400, 600,
    // 700, and 900 ms.
    let video_keys: Vec<u64> = packets
        .iter()
        .filter(|(codec, _, key, _)| *codec == Codec::Av1 && *key)
        .map(|(_, ts, _, _)| *ts)
        .collect();
    assert_eq!(
        video_keys,
        vec![0, 100_000, 300_000, 400_000, 600_000, 700_000, 900_000]
    );
}

#[test]
fn corrupt_mp4_returns_error_without_panicking() {
    // Truncated mid-moov (mdat comes first, moov is at the end).
    let bytes = mp4_bytes("av1_opus.mp4");
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

#[test]
fn fuzz_stco_huge_count_returns_error() {
    // Fuzz crash (2026-11-08): a `stco` box declaring ~268M chunk
    // offsets made the demuxer pre-allocate ~2 GiB and OOM. The
    // demuxer must return an error, not allocate from a count the
    // box cannot hold. Fixture: fuzz artifact, kept whole —
    // `cargo fuzz tmin` could not reduce it (see tests/data/README.md).
    use codec_whatttttttttttt_core::Mp4Demuxer;
    let bytes = include_bytes!("data/mp4_stco_huge_count.mp4");
    // The count check rejects the box as inconsistent before any
    // allocation; before the fix this returned Truncated after
    // pre-allocating ~2 GiB.
    let err = Mp4Demuxer::parse(bytes).unwrap_err();
    assert!(
        matches!(err, Error::InvalidData { .. }),
        "expected InvalidData, got {err:?}"
    );
}
