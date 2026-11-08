//! Integration tests for the common `Demuxer` (Phase 6): probing,
//! track listing, packet order, and keyframe seeking across both
//! containers, checked against the real ffmpeg-produced vectors.

use codec_whatttttttttttt_core::{probe_container, Codec, ContainerKind, Demuxer, Error};

fn file_bytes(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/data/{name}")).unwrap()
}

fn first_video_keyframe_after_seek(name: &str, timestamp_us: u64) -> Option<u64> {
    let mut demuxer = Demuxer::from_bytes(&file_bytes(name)).unwrap();
    demuxer.seek(timestamp_us).unwrap();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        if (packet.codec == Codec::Av1 || packet.codec == Codec::Vp9) && packet.is_keyframe {
            return Some(packet.timestamp);
        }
    }
    None
}

#[test]
fn probe_container_identifies_both() {
    assert_eq!(
        probe_container(&file_bytes("av1_opus.webm")).unwrap(),
        ContainerKind::Webm
    );
    assert_eq!(
        probe_container(&file_bytes("av1_opus.mp4")).unwrap(),
        ContainerKind::Mp4
    );
    let err = probe_container(b"RIFFxxxx").unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
    let err = probe_container(&[]).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn demuxer_reports_container_kind() {
    let webm = Demuxer::from_bytes(&file_bytes("av1_opus.webm")).unwrap();
    assert_eq!(webm.container_kind(), ContainerKind::Webm);
    let mp4 = Demuxer::from_bytes(&file_bytes("av1_opus.mp4")).unwrap();
    assert_eq!(mp4.container_kind(), ContainerKind::Mp4);
}

#[test]
fn webm_and_mp4_agree_on_packets() {
    // The same AV1+Opus content muxed into both containers must
    // demux to the same packet sequence (codec, timestamp, size).
    // MP4 audio timestamps are on a 20 ms grid; WebM audio
    // timestamps are the raw container values, 1 ms later after
    // the first packet.
    let mut webm = Demuxer::from_bytes(&file_bytes("av1_opus.webm")).unwrap();
    let mut mp4 = Demuxer::from_bytes(&file_bytes("av1_opus.mp4")).unwrap();
    let mut webm_packets = Vec::new();
    while let Some(packet) = webm.next_packet().unwrap() {
        webm_packets.push((packet.codec, packet.timestamp, packet.data.len()));
    }
    let mut mp4_packets = Vec::new();
    while let Some(packet) = mp4.next_packet().unwrap() {
        mp4_packets.push((packet.codec, packet.timestamp, packet.data.len()));
    }
    assert_eq!(webm_packets.len(), mp4_packets.len());
    for (index, ((w_codec, w_ts, w_size), (m_codec, m_ts, m_size))) in
        webm_packets.iter().zip(mp4_packets.iter()).enumerate()
    {
        assert_eq!(w_codec, m_codec, "packet {index} codec");
        assert_eq!(w_size, m_size, "packet {index} size");
        if *w_codec == Codec::Av1 {
            assert_eq!(w_ts, m_ts, "video packet {index} timestamp");
        }
    }
    // Audio timestamps differ by the container's Opus pre-skip
    // handling: WebM starts at 0, MP4 starts at 0 too here, but
    // WebM audio packets land 1 ms after the MP4 grid.
    let webm_audio: Vec<u64> = webm_packets
        .iter()
        .filter(|(codec, _, _)| *codec == Codec::Opus)
        .map(|(_, ts, _)| *ts)
        .collect();
    let mp4_audio: Vec<u64> = mp4_packets
        .iter()
        .filter(|(codec, _, _)| *codec == Codec::Opus)
        .map(|(_, ts, _)| *ts)
        .collect();
    assert_eq!(webm_audio, vec![0, 21_000, 41_000, 61_000, 81_000, 101_000]);
    assert_eq!(mp4_audio, vec![0, 20_000, 40_000, 60_000, 80_000, 100_000]);
}

#[test]
fn seek_webm_to_keyframe() {
    // Keyframes at 0, 300, 600, 900 ms. Seeking to 500 ms lands
    // in the cluster starting at 181 ms, and the first video
    // keyframe after the seek is at 300 ms.
    assert_eq!(
        first_video_keyframe_after_seek("vp9_keyframes.webm", 500_000),
        Some(300_000)
    );
    // Seeking to 0 stays at the first keyframe.
    assert_eq!(
        first_video_keyframe_after_seek("vp9_keyframes.webm", 0),
        Some(0)
    );
    // Seeking past the last keyframe finds it at 900 ms.
    assert_eq!(
        first_video_keyframe_after_seek("vp9_keyframes.webm", 950_000),
        Some(900_000)
    );
}

#[test]
fn seek_mp4_to_keyframe() {
    // Sync samples at 0, 100, 300, 400, 600, 700, 900 ms (libaom
    // emits an extra sync sample after each forced keyframe).
    // Seeking to 500 ms takes the greatest sync sample at or
    // before it, 400 ms.
    assert_eq!(
        first_video_keyframe_after_seek("av1_keyframes.mp4", 500_000),
        Some(400_000)
    );
    assert_eq!(
        first_video_keyframe_after_seek("av1_keyframes.mp4", 0),
        Some(0)
    );
    assert_eq!(
        first_video_keyframe_after_seek("av1_keyframes.mp4", 950_000),
        Some(900_000)
    );
}

#[test]
fn seek_single_keyframe_file_goes_to_zero() {
    // `av1_opus.webm` and `av1_opus.mp4` have only one video
    // keyframe (at 0), so any seek lands there.
    assert_eq!(
        first_video_keyframe_after_seek("av1_opus.webm", 800_000),
        Some(0)
    );
    assert_eq!(
        first_video_keyframe_after_seek("av1_opus.mp4", 800_000),
        Some(0)
    );
}

#[test]
fn demuxer_rejects_unknown_container_without_panicking() {
    let err = Demuxer::from_bytes(b"RIFFxxxx").unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
    let err = Demuxer::from_bytes(&[]).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}
