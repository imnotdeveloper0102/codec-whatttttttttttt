//! Integration tests for MPEG-TS demuxing and muxing
//! (ISO/IEC 13818-1).
//!
//! The vector is a real file produced with ffmpeg 8.1.2 from
//! local CC0 sine/testsrc sources (see the subagent report for
//! the exact command). Packet counts below are the values
//! ffprobe reports for the file, checked against the demuxer
//! output.

#[cfg(all(feature = "licensed-avc-hevc", feature = "licensed-aac"))]
use codec_whatttttttttttt_core::TsMuxer;
use codec_whatttttttttttt_core::{Codec, Error, LicenseContext, LicenseType, TsDemuxer};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/data/{name}")).unwrap()
}

fn valid_license() -> LicenseContext {
    LicenseContext::new(
        "test licensee".to_string(),
        LicenseType::Commercial,
        "TEST-12345678".to_string(),
        4_102_444_800,
        "avc".to_string(),
    )
    .unwrap()
}

#[cfg(all(feature = "licensed-avc-hevc", feature = "licensed-aac"))]
fn drain_with_license(bytes: &[u8]) -> Vec<codec_whatttttttttttt_core::Packet> {
    let mut demuxer = TsDemuxer::parse(bytes).unwrap();
    demuxer.set_license_context(Some(valid_license())).unwrap();
    let mut packets = Vec::new();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        packets.push(packet);
    }
    packets
}

#[test]
fn ts_avc_aac_has_two_streams_and_13_pes_packets() {
    let bytes = fixture("avc_aac.ts");
    assert_eq!(bytes.len(), 12_220);
    assert_eq!(bytes.len() % 188, 0);
    let demuxer = TsDemuxer::parse(&bytes).unwrap();
    // One packet per PES: 10 video PES + 3 audio PES. The audio
    // PES packets each hold several ADTS frames (ffmpeg packs
    // ~15 AAC frames per PES), so this is 13 packets, not the 45
    // ADTS frames ffprobe counts.
    assert_eq!(demuxer.packet_count(), 13);
    let streams = demuxer.streams();
    assert_eq!(streams.len(), 2);
    assert!(streams.iter().any(|s| s.codec == Some(Codec::Avc)));
    assert!(streams.iter().any(|s| s.codec == Some(Codec::Aac)));
    let stats = demuxer.stats();
    assert_eq!(stats.packets, 65);
    assert_eq!(stats.continuity_errors, 0);
    assert_eq!(stats.dropped_duplicates, 0);
    // PCR is present on the video stream.
    let video = streams
        .iter()
        .find(|s| s.codec == Some(Codec::Avc))
        .unwrap();
    assert!(video.last_pcr_us.is_some());
}

#[test]
fn ts_next_packet_without_license_is_a_gate_error() {
    let bytes = fixture("avc_aac.ts");
    let mut demuxer = TsDemuxer::parse(&bytes).unwrap();
    let err = demuxer.next_packet().unwrap_err();
    assert!(
        matches!(err, Error::LicenseRequired { .. } | Error::Unsupported(_)),
        "unexpected error: {err:?}"
    );
}

#[test]
#[cfg(all(feature = "licensed-avc-hevc", feature = "licensed-aac"))]
fn ts_avc_aac_drains_with_license() {
    let packets = drain_with_license(&fixture("avc_aac.ts"));
    assert_eq!(packets.iter().filter(|p| p.codec == Codec::Avc).count(), 10);
    assert_eq!(packets.iter().filter(|p| p.codec == Codec::Aac).count(), 3);
    assert_eq!(packets.len(), 13);
    // The first video packet is a keyframe (random access).
    let first_video = packets.iter().find(|p| p.codec == Codec::Avc).unwrap();
    assert!(first_video.is_keyframe);
    // Audio packets are always keyframes.
    assert!(packets
        .iter()
        .filter(|p| p.codec == Codec::Aac)
        .all(|p| p.is_keyframe));
    // Each audio PES holds ADTS frames as-is.
    let first_audio = packets.iter().find(|p| p.codec == Codec::Aac).unwrap();
    let (header, _) = codec_whatttttttttttt_core::parse_adts_header(&first_audio.data).unwrap();
    assert_eq!(header.profile, 1);
    assert_eq!(header.sample_rate, 44_100);
    assert_eq!(header.channels, 1);
}

#[test]
#[cfg(all(feature = "licensed-avc-hevc", feature = "licensed-aac"))]
fn ts_mux_round_trip_preserves_payloads() {
    let video_payload: Vec<u8> = (0u8..=250).cycle().take(300).collect();
    let audio_payload: Vec<u8> = (0u8..=250).cycle().take(160).collect();
    let mut muxer = TsMuxer::new();
    muxer.add_stream(0x101, Codec::Avc).unwrap();
    muxer.add_stream(0x102, Codec::Aac).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&muxer.packet(0x101, &video_payload, Some(0), true).unwrap());
    bytes.extend_from_slice(&muxer.packet(0x102, &audio_payload, Some(0), false).unwrap());
    assert_eq!(bytes.len() % 188, 0);
    let packets = drain_with_license(&bytes);
    assert_eq!(packets.len(), 2);
    assert_eq!(packets[0].codec, Codec::Avc);
    assert_eq!(packets[0].data, video_payload);
    assert_eq!(packets[1].codec, Codec::Aac);
    assert_eq!(packets[1].data, audio_payload);
}

#[test]
#[cfg(not(all(feature = "licensed-avc-hevc", feature = "licensed-aac")))]
fn ts_set_license_without_features_is_unsupported() {
    let bytes = fixture("avc_aac.ts");
    let mut demuxer = TsDemuxer::parse(&bytes).unwrap();
    let err = demuxer
        .set_license_context(Some(valid_license()))
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
    let err = demuxer.next_packet().unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
}

#[test]
fn ts_cut_short_final_packet_is_truncated() {
    let mut bytes = fixture("avc_aac.ts");
    bytes.pop();
    let err = TsDemuxer::parse(&bytes).unwrap_err();
    assert!(
        matches!(err, Error::Truncated { .. }),
        "unexpected error: {err:?}"
    );
}

#[test]
fn ts_duplicated_packet_is_dropped_and_counted() {
    let bytes = fixture("avc_aac.ts");
    // Duplicate one 188-byte packet in the middle of the file.
    let mut dup = Vec::with_capacity(bytes.len() + 188);
    dup.extend_from_slice(&bytes[..1880]);
    dup.extend_from_slice(&bytes[1880..1880 + 188]);
    dup.extend_from_slice(&bytes[1880..]);
    let demuxer = TsDemuxer::parse(&dup).unwrap();
    assert_eq!(demuxer.stats().packets, 66);
    assert_eq!(demuxer.stats().dropped_duplicates, 1);
    assert_eq!(demuxer.packet_count(), 13);
}
