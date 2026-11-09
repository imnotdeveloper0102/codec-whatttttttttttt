//! Integration tests for AVC/HEVC/AAC passthrough behind the
//! license gate.
//!
//! The vectors are real files produced with ffmpeg 8.1.2 from
//! local CC0 sine/testsrc sources (see the subagent report for the
//! exact commands; `tests/data/README.md` is owned by the
//! aggregator worker). Packet counts below are the values ffprobe
//! reports for those files, checked against the demuxer output.
//!
//! Gate behaviour, with both licensed features on:
//! parsing succeeds without a license (tracks are listed,
//! `codec_private` is kept) and only `next_packet` refuses
//! licensed packets, with `LicenseRequired`. With the features
//! off, the same refusal is `Unsupported`. An expired license is
//! rejected eagerly by `set_license_context`.

use codec_whatttttttttttt_core::{
    Codec, Decoder, DecoderConfig, Demuxer, Encoder, EncoderConfig, Error, Frame, LicenseContext,
    LicenseType, Packet, TrackKind, VideoFrame,
};

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
fn expired_license() -> LicenseContext {
    LicenseContext::new(
        "test licensee".to_string(),
        LicenseType::Commercial,
        "TEST-12345678".to_string(),
        1,
        "avc".to_string(),
    )
    .unwrap()
}

/// The gate error for a licensed packet: `LicenseRequired` when
/// the features are on, `Unsupported` when they are off.
fn is_gate_error(err: &Error) -> bool {
    matches!(err, Error::LicenseRequired { .. } | Error::Unsupported(_))
}

#[cfg(all(feature = "licensed-avc-hevc", feature = "licensed-aac"))]
fn drain_with_license(name: &str) -> Vec<Packet> {
    let mut demuxer = Demuxer::from_bytes(&fixture(name)).unwrap();
    demuxer.set_license_context(Some(valid_license())).unwrap();
    let mut packets = Vec::new();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        packets.push(packet);
    }
    packets
}

#[test]
fn mp4_avc_aac_parses_without_license_and_lists_tracks() {
    let demuxer = Demuxer::from_bytes(&fixture("avc_aac.mp4")).unwrap();
    let tracks = demuxer.tracks();
    assert_eq!(tracks.len(), 2);
    let video = tracks.iter().find(|t| t.kind == TrackKind::Video).unwrap();
    assert_eq!(video.codec, Codec::Avc);
    assert_eq!(video.codec_id, "avc1");
    // avcC config record, version 1, present without a license.
    assert_ne!(video.codec_private.len(), 0);
    assert_eq!(video.codec_private[0], 1);
    assert_eq!(video.width, Some(64));
    assert_eq!(video.height, Some(64));
    let audio = tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap();
    assert_eq!(audio.codec, Codec::Aac);
    assert_eq!(audio.codec_id, "mp4a");
    // esds AudioSpecificConfig, present without a license.
    assert_ne!(audio.codec_private.len(), 0);
    assert_eq!(audio.sample_rate, Some(44_100));
    assert_eq!(audio.channels, Some(1));
}

#[test]
#[cfg(all(feature = "licensed-avc-hevc", feature = "licensed-aac"))]
fn mp4_next_packet_without_license_is_a_gate_error_and_does_not_advance() {
    let mut demuxer = Demuxer::from_bytes(&fixture("avc_aac.mp4")).unwrap();
    let err = demuxer.next_packet().unwrap_err();
    assert!(is_gate_error(&err), "unexpected error: {err:?}");
    // The position did not advance: after setting a license the
    // first packet is still the first video keyframe. Its
    // timestamp is the presentation time, which is nonzero here
    // because x264's B-frame reorder delay puts the IDR frame's
    // presentation after the first B-frames.
    demuxer.set_license_context(Some(valid_license())).unwrap();
    let packet = demuxer.next_packet().unwrap().unwrap();
    assert_eq!(packet.codec, Codec::Avc);
    assert!(packet.is_keyframe);
}

#[test]
#[cfg(all(feature = "licensed-avc-hevc", feature = "licensed-aac"))]
fn mp4_avc_aac_drains_with_license() {
    let packets = drain_with_license("avc_aac.mp4");
    assert_eq!(packets.iter().filter(|p| p.codec == Codec::Avc).count(), 10);
    assert_eq!(packets.iter().filter(|p| p.codec == Codec::Aac).count(), 45);
    assert_eq!(packets.len(), 55);
}

#[test]
#[cfg(all(feature = "licensed-avc-hevc", feature = "licensed-aac"))]
fn mp4_hevc_aac_drains_with_license() {
    let packets = drain_with_license("hevc_aac.mp4");
    assert_eq!(
        packets.iter().filter(|p| p.codec == Codec::Hevc).count(),
        10
    );
    assert_eq!(packets.iter().filter(|p| p.codec == Codec::Aac).count(), 45);
    assert_eq!(packets.len(), 55);
}

#[test]
#[cfg(all(feature = "licensed-avc-hevc", feature = "licensed-aac"))]
fn mkv_avc_aac_drains_with_license() {
    let packets = drain_with_license("avc_aac.mkv");
    assert_eq!(packets.iter().filter(|p| p.codec == Codec::Avc).count(), 10);
    assert_eq!(packets.iter().filter(|p| p.codec == Codec::Aac).count(), 45);
    assert_eq!(packets.len(), 55);
}

#[test]
#[cfg(all(feature = "licensed-avc-hevc", feature = "licensed-aac"))]
fn m4a_aac_drains_with_license() {
    // M4A is MP4 carrying AAC, not a codec of its own.
    let packets = drain_with_license("sine_aac.m4a");
    assert_eq!(packets.len(), 45);
    assert!(packets.iter().all(|p| p.codec == Codec::Aac));
}

#[test]
#[cfg(all(feature = "licensed-avc-hevc", feature = "licensed-aac"))]
fn expired_license_is_rejected_eagerly() {
    let mut demuxer = Demuxer::from_bytes(&fixture("avc_aac.mp4")).unwrap();
    let err = demuxer
        .set_license_context(Some(expired_license()))
        .unwrap_err();
    assert!(
        matches!(err, Error::LicenseRequired { codec: "avc" }),
        "unexpected error: {err:?}"
    );
    // The failed check left no license in place.
    let err = demuxer.next_packet().unwrap_err();
    assert!(
        matches!(err, Error::LicenseRequired { .. }),
        "unexpected error: {err:?}"
    );
}

#[test]
#[cfg(not(all(feature = "licensed-avc-hevc", feature = "licensed-aac")))]
fn set_license_without_features_is_unsupported() {
    let mut demuxer = Demuxer::from_bytes(&fixture("avc_aac.mp4")).unwrap();
    let err = demuxer
        .set_license_context(Some(valid_license()))
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
    let err = demuxer.next_packet().unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
}

#[test]
fn decoder_refuses_avc_hevc_aac() {
    for codec in [Codec::Avc, Codec::Hevc, Codec::Aac] {
        let mut decoder = Decoder::new(DecoderConfig {
            codec,
            ..DecoderConfig::default()
        })
        .unwrap();
        let packet = Packet {
            codec,
            data: vec![0; 8],
            timestamp: 0,
            is_keyframe: true,
        };
        let err = decoder.decode(&packet).unwrap_err();
        assert!(is_gate_error(&err), "{codec:?}: unexpected error: {err:?}");
    }
}

#[test]
fn encoder_refuses_avc_hevc_aac() {
    for codec in [Codec::Avc, Codec::Hevc] {
        let mut encoder = Encoder::new(EncoderConfig {
            codec,
            width: Some(64),
            height: Some(64),
            ..EncoderConfig::default()
        })
        .unwrap();
        let frame = Frame::Video(VideoFrame {
            width: 64,
            height: 64,
            format: codec_whatttttttttttt_core::PixelFormat::Yuv420,
            planes: vec![vec![0; 64 * 64], vec![0; 32 * 32], vec![0; 32 * 32]],
            strides: vec![64, 32, 32],
            timestamp: 0,
            colour_space: codec_whatttttttttttt_core::ColourSpace::Bt709,
            colour_range: codec_whatttttttttttt_core::ColourRange::Limited,
        });
        let err = encoder.encode(&frame).unwrap_err();
        assert!(is_gate_error(&err), "{codec:?}: unexpected error: {err:?}");
    }
    // AAC: the gate is checked at construction, so there is no
    // encoder to call encode() on; the construction error is
    // itself the gate error (Unsupported without the feature,
    // LicenseRequired with it but no license). `Encoder` has
    // no `Debug`, so `unwrap_err` is unavailable; the let-else
    // extracts the error instead.
    let Err(err) = Encoder::new(EncoderConfig {
        codec: Codec::Aac,
        ..EncoderConfig::default()
    }) else {
        panic!("aac encoder construction must fail without a license");
    };
    assert!(is_gate_error(&err), "aac: unexpected error: {err:?}");
}
