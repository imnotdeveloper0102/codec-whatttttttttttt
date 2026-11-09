//! Integration tests for the license gate (H.264/H.265/AAC).
//!
//! The fixtures below are test data only: the license ids are
//! invented strings that no real license uses, and they never
//! leave the test process.

use codec_whatttttttttttt_core::{
    Decoder, DecoderConfig, Encoder, EncoderConfig, Error, Frame, HardwareDecoder, LicenseContext,
    LicenseType, Packet, StubHardwareDecoder, VideoFrame,
};
#[cfg(feature = "licensed-avc-hevc")]
use codec_whatttttttttttt_core::{HardwareEncoder, StubHardwareEncoder};

/// A well-formed, unexpired license invented for tests.
fn test_license_valid_fixture() -> LicenseContext {
    LicenseContext::new(
        "test licensee".to_string(),
        LicenseType::Commercial,
        "TEST-12345678".to_string(),
        4_102_444_800,
        "avc".to_string(),
    )
    .unwrap()
}

/// An expired license invented for tests (epoch second 1).
fn test_license_expired_fixture() -> LicenseContext {
    LicenseContext::new(
        "test licensee".to_string(),
        LicenseType::Commercial,
        "TEST-12345678".to_string(),
        1,
        "avc".to_string(),
    )
    .unwrap()
}

fn avc_packet() -> Packet {
    Packet {
        codec: codec_whatttttttttttt_core::Codec::Avc,
        data: vec![0u8; 4],
        timestamp: 100,
        is_keyframe: true,
    }
}

#[test]
fn license_required_error_names_the_codec() {
    let err = Error::LicenseRequired { codec: "avc" };
    assert!(err.to_string().contains("avc"));
    assert!(err.to_string().contains("license required"));
}

#[test]
#[cfg(feature = "licensed-avc-hevc")]
fn software_decoder_refuses_avc_without_license() {
    let mut decoder = Decoder::new(DecoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Avc,
        ..DecoderConfig::default()
    })
    .unwrap();
    let err = decoder.decode(&avc_packet()).unwrap_err();
    assert!(matches!(err, Error::LicenseRequired { codec: "avc" }));
}

#[test]
#[cfg(not(feature = "licensed-avc-hevc"))]
fn software_decoder_refuses_avc_as_unsupported_when_feature_off() {
    let mut decoder = Decoder::new(DecoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Avc,
        ..DecoderConfig::default()
    })
    .unwrap();
    let err = decoder.decode(&avc_packet()).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
}

#[test]
#[cfg(feature = "licensed-avc-hevc")]
fn software_decoder_refuses_hevc_without_license() {
    let mut decoder = Decoder::new(DecoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Hevc,
        ..DecoderConfig::default()
    })
    .unwrap();
    let packet = Packet {
        codec: codec_whatttttttttttt_core::Codec::Hevc,
        data: vec![0u8; 4],
        timestamp: 0,
        is_keyframe: true,
    };
    let err = decoder.decode(&packet).unwrap_err();
    assert!(matches!(err, Error::LicenseRequired { codec: "hevc" }));
}

#[test]
#[cfg(feature = "licensed-avc-hevc")]
fn software_encoder_refuses_avc_without_license() {
    let mut encoder = Encoder::new(EncoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Avc,
        ..EncoderConfig::default()
    })
    .unwrap();
    let frame = Frame::Video(VideoFrame {
        width: 0,
        height: 0,
        format: codec_whatttttttttttt_core::PixelFormat::Yuv420,
        planes: Vec::new(),
        strides: Vec::new(),
        timestamp: 0,
        colour_space: codec_whatttttttttttt_core::ColourSpace::default(),
        colour_range: codec_whatttttttttttt_core::ColourRange::default(),
    });
    let err = encoder.encode(&frame).unwrap_err();
    assert!(matches!(err, Error::LicenseRequired { codec: "avc" }));
}

#[test]
#[cfg(not(feature = "licensed-avc-hevc"))]
fn software_encoder_refuses_avc_as_unsupported_when_feature_off() {
    let mut encoder = Encoder::new(EncoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Avc,
        ..EncoderConfig::default()
    })
    .unwrap();
    let frame = Frame::Video(VideoFrame {
        width: 0,
        height: 0,
        format: codec_whatttttttttttt_core::PixelFormat::Yuv420,
        planes: Vec::new(),
        strides: Vec::new(),
        timestamp: 0,
        colour_space: codec_whatttttttttttt_core::ColourSpace::default(),
        colour_range: codec_whatttttttttttt_core::ColourRange::default(),
    });
    let err = encoder.encode(&frame).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
}

#[test]
#[cfg(feature = "licensed-aac")]
fn aac_encoder_construction_requires_a_license() {
    // The gate is checked at construction, like
    // `Decoder::from_aac_config`: no license, no encoder.
    // `Encoder` has no `Debug`, so `unwrap_err` is unavailable;
    // the let-else extracts the error instead.
    let Err(err) = Encoder::new(EncoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Aac,
        ..EncoderConfig::default()
    }) else {
        panic!("aac encoder construction must fail without a license");
    };
    assert!(matches!(err, Error::LicenseRequired { codec: "aac" }));
}

#[test]
#[cfg(feature = "licensed-aac")]
fn aac_encoder_construction_opens_with_a_valid_license() {
    let encoder = Encoder::new(EncoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Aac,
        license: Some(test_license_valid_fixture()),
        ..EncoderConfig::default()
    });
    assert!(encoder.is_ok());
}

#[test]
#[cfg(not(feature = "licensed-aac"))]
fn aac_encoder_construction_is_unsupported_when_feature_off() {
    // `Encoder` has no `Debug`, so `unwrap_err` is unavailable;
    // the let-else extracts the error instead.
    let Err(err) = Encoder::new(EncoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Aac,
        license: Some(test_license_valid_fixture()),
        ..EncoderConfig::default()
    }) else {
        panic!("aac encoder construction must fail without the feature");
    };
    assert!(matches!(err, Error::Unsupported(_)));
}

#[test]
fn expired_test_license_fixture_is_license_required() {
    let mut license = test_license_expired_fixture();
    let err = license.validate("avc").unwrap_err();
    assert!(matches!(err, Error::LicenseRequired { codec: "avc" }));
}

#[test]
fn malformed_test_license_fixture_is_invalid_argument() {
    let err = LicenseContext::new(
        "test licensee".to_string(),
        LicenseType::Evaluation,
        "bad id!".to_string(),
        4_102_444_800,
        "avc".to_string(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
    let err = LicenseContext::new(
        String::new(),
        LicenseType::Internal,
        "TEST-12345678".to_string(),
        4_102_444_800,
        "avc".to_string(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
}

#[test]
fn audit_log_masks_license_id_and_records_denied_check() {
    let mut license = test_license_expired_fixture();
    let _ = license.validate("avc").unwrap_err();
    assert_eq!(license.audit_log().len(), 1);
    let record = &license.audit_log()[0];
    assert!(!record.granted);
    assert_eq!(record.license_id_masked, "TEST***");
    assert!(!record.license_id_masked.contains("12345678"));
    assert_eq!(record.codec, "avc");
}

#[test]
fn valid_test_license_fixture_records_granted_check() {
    let mut license = test_license_valid_fixture();
    license.validate("hevc").unwrap();
    assert_eq!(license.audit_log().len(), 1);
    assert!(license.audit_log()[0].granted);
    assert_eq!(license.audit_log()[0].license_id_masked, "TEST***");
}

#[test]
fn hardware_decoder_stub_without_open_is_license_required() {
    let mut stub = StubHardwareDecoder::new(codec_whatttttttttttt_core::Codec::Avc);
    let err = stub.decode(&avc_packet()).unwrap_err();
    assert!(matches!(err, Error::LicenseRequired { codec: "avc" }));
}

#[test]
#[cfg(feature = "licensed-avc-hevc")]
fn valid_test_license_fixture_opens_hardware_decoder_stub() {
    let mut stub = StubHardwareDecoder::new(codec_whatttttttttttt_core::Codec::Avc);
    stub.open(&mut test_license_valid_fixture()).unwrap();
    assert!(stub.is_open());
    let frame = stub.decode(&avc_packet()).unwrap();
    assert_eq!(frame.timestamp(), 100);
}

#[test]
#[cfg(feature = "licensed-avc-hevc")]
fn expired_test_license_fixture_does_not_open_hardware_decoder_stub() {
    let mut stub = StubHardwareDecoder::new(codec_whatttttttttttt_core::Codec::Avc);
    let err = stub.open(&mut test_license_expired_fixture()).unwrap_err();
    assert!(matches!(err, Error::LicenseRequired { codec: "avc" }));
    assert!(!stub.is_open());
}

#[test]
#[cfg(not(feature = "licensed-avc-hevc"))]
fn hardware_decoder_stub_open_is_unsupported_when_feature_off() {
    let mut stub = StubHardwareDecoder::new(codec_whatttttttttttt_core::Codec::Avc);
    let err = stub.open(&mut test_license_valid_fixture()).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
    assert!(!stub.is_open());
}

#[test]
#[cfg(feature = "licensed-avc-hevc")]
fn valid_test_license_fixture_opens_hardware_encoder_stub() {
    let mut stub = StubHardwareEncoder::new(codec_whatttttttttttt_core::Codec::Hevc);
    stub.open(&mut test_license_valid_fixture()).unwrap();
    assert!(stub.is_open());
    let frame = Frame::Video(VideoFrame {
        width: 0,
        height: 0,
        format: codec_whatttttttttttt_core::PixelFormat::Yuv420,
        planes: Vec::new(),
        strides: Vec::new(),
        timestamp: 55,
        colour_space: codec_whatttttttttttt_core::ColourSpace::default(),
        colour_range: codec_whatttttttttttt_core::ColourRange::default(),
    });
    let packet = stub.encode(&frame).unwrap();
    assert_eq!(packet.codec, codec_whatttttttttttt_core::Codec::Hevc);
    assert_eq!(packet.timestamp, 55);
}
