//! Integration tests for the bitrate API (`bitrate.rs`).
//!
//! Every valid ladder rung and sample rate must pass; every invalid
//! value must return `InvalidArgument` whose message names the codec
//! and states the allowed set (the error type holds a `&'static
//! str`, so the offending value itself cannot be interpolated —
//! see `bitrate.rs`).

use codec_whatttttttttttt_core::{
    validate_bitrate, vorbis_quality, AudioFrame, BitrateCodec, BitrateMode, Encoder,
    EncoderConfig, Error, Frame,
};

fn all_codecs() -> [BitrateCodec; 4] {
    [
        BitrateCodec::Mp3,
        BitrateCodec::Aac,
        BitrateCodec::Opus,
        BitrateCodec::Vorbis,
    ]
}

fn assert_invalid_with_codec_and_limit(
    codec: BitrateCodec,
    mode: BitrateMode,
    sample_rate: u32,
    limit_fragment: &str,
) {
    let err = validate_bitrate(codec, mode, sample_rate).unwrap_err();
    assert!(
        matches!(err, Error::InvalidArgument(_)),
        "{codec:?} {mode:?} at {sample_rate} Hz: expected InvalidArgument, got {err}"
    );
    let message = err.to_string();
    assert!(
        message.contains(codec.name()),
        "error message must name the codec: {message}"
    );
    assert!(
        message.contains(limit_fragment),
        "error message must state the allowed set ({limit_fragment}): {message}"
    );
}

#[test]
fn cbr_ladder_accepted_for_every_codec() {
    // 64/96/128/320 kbps at a rate every codec supports (48 kHz).
    for codec in all_codecs() {
        for kbps in [64, 96, 128, 320] {
            validate_bitrate(codec, BitrateMode::Cbr(kbps), 48000)
                .unwrap_or_else(|err| panic!("{codec:?} CBR {kbps} kbps at 48 kHz: {err}"));
        }
    }
}

#[test]
fn abr_ladder_accepted_for_every_codec() {
    for codec in all_codecs() {
        for kbps in [64, 96, 128, 320] {
            validate_bitrate(codec, BitrateMode::Abr(kbps), 48000)
                .unwrap_or_else(|err| panic!("{codec:?} ABR {kbps} kbps at 48 kHz: {err}"));
        }
    }
}

#[test]
fn vbr_quality_range_accepted_for_every_codec() {
    for codec in all_codecs() {
        for quality in 0..=9 {
            validate_bitrate(codec, BitrateMode::Vbr(quality), 48000)
                .unwrap_or_else(|err| panic!("{codec:?} VBR quality {quality} at 48 kHz: {err}"));
        }
    }
}

#[test]
fn mp3_accepts_mpeg_sample_rates_and_rejects_others() {
    for rate in [8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000] {
        validate_bitrate(BitrateCodec::Mp3, BitrateMode::Cbr(128), rate)
            .unwrap_or_else(|err| panic!("mp3 at {rate} Hz: {err}"));
    }
    for rate in [96000, 192_000, 44000] {
        assert_invalid_with_codec_and_limit(
            BitrateCodec::Mp3,
            BitrateMode::Cbr(128),
            rate,
            "8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000",
        );
    }
    // Zero is a different failure: "must be positive", not the list.
    assert_invalid_with_codec_and_limit(
        BitrateCodec::Mp3,
        BitrateMode::Cbr(128),
        0,
        "must be positive",
    );
}

#[test]
fn aac_accepts_rates_up_to_96000_and_rejects_192000() {
    for rate in [8000, 44100, 48000, 64000, 88200, 96000] {
        validate_bitrate(BitrateCodec::Aac, BitrateMode::Cbr(128), rate)
            .unwrap_or_else(|err| panic!("aac at {rate} Hz: {err}"));
    }
    assert_invalid_with_codec_and_limit(BitrateCodec::Aac, BitrateMode::Cbr(128), 192_000, "96000");
}

#[test]
fn opus_accepts_only_its_five_input_rates() {
    for rate in [8000, 12000, 16000, 24000, 48000] {
        validate_bitrate(BitrateCodec::Opus, BitrateMode::Cbr(128), rate)
            .unwrap_or_else(|err| panic!("opus at {rate} Hz: {err}"));
    }
    // 44100 Hz is the classic mistake: Opus has no 44.1 kHz mode.
    assert_invalid_with_codec_and_limit(
        BitrateCodec::Opus,
        BitrateMode::Cbr(128),
        44100,
        "8000, 12000, 16000, 24000, 48000",
    );
    assert_invalid_with_codec_and_limit(
        BitrateCodec::Opus,
        BitrateMode::Cbr(128),
        96000,
        "8000, 12000, 16000, 24000, 48000",
    );
}

#[test]
fn vorbis_accepts_rates_up_to_192000() {
    for rate in [8000, 44100, 48000, 96000, 176_400, 192_000] {
        validate_bitrate(BitrateCodec::Vorbis, BitrateMode::Vbr(2), rate)
            .unwrap_or_else(|err| panic!("vorbis at {rate} Hz: {err}"));
    }
    assert_invalid_with_codec_and_limit(
        BitrateCodec::Vorbis,
        BitrateMode::Vbr(2),
        22050 * 10,
        "192000",
    );
}

#[test]
fn off_ladder_bitrates_rejected_for_every_codec() {
    for codec in all_codecs() {
        for kbps in [0, 32, 48, 100, 160, 192, 256, 512] {
            assert_invalid_with_codec_and_limit(
                codec,
                BitrateMode::Cbr(kbps),
                48000,
                "64, 96, 128, 320",
            );
            assert_invalid_with_codec_and_limit(
                codec,
                BitrateMode::Abr(kbps),
                48000,
                "64, 96, 128, 320",
            );
        }
    }
}

#[test]
fn vbr_quality_above_9_rejected_for_every_codec() {
    for codec in all_codecs() {
        for quality in [10, 100, 255] {
            assert_invalid_with_codec_and_limit(codec, BitrateMode::Vbr(quality), 48000, "0..=9");
        }
    }
}

#[test]
fn vorbis_quality_mapping_is_documented_and_inverted() {
    // Unified 0 (best) -> Vorbis 10.0; unified 9 (worst) -> 1.0.
    assert_eq!(vorbis_quality(0), Some(10.0));
    assert_eq!(vorbis_quality(9), Some(1.0));
    assert_eq!(vorbis_quality(10), None);
}

#[test]
fn bitrate_bps_is_none_for_vbr() {
    assert_eq!(BitrateMode::Cbr(320).bitrate_bps(), Some(320_000));
    assert_eq!(BitrateMode::Abr(96).bitrate_bps(), Some(96_000));
    assert_eq!(BitrateMode::Vbr(3).bitrate_bps(), None);
}

#[test]
fn encoder_accepts_valid_bitrate_mode_for_opus_and_vorbis() {
    // AAC is not in this list: its construction needs the
    // `licensed-aac` feature plus a valid license (see the
    // license tests), so a bare construction here would fail
    // at the gate, not at bitrate validation.
    for codec in [
        codec_whatttttttttttt_core::Codec::Opus,
        codec_whatttttttttttt_core::Codec::Vorbis,
    ] {
        let config = EncoderConfig {
            codec,
            sample_rate: 48000,
            channels: 2,
            bitrate_mode: Some(BitrateMode::Cbr(128)),
            ..EncoderConfig::default()
        };
        Encoder::new(config).unwrap_or_else(|err| panic!("{codec:?} encoder: {err}"));
    }
}

#[test]
fn encoder_rejects_bitrate_mode_with_wrong_sample_rate() {
    let config = EncoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Opus,
        sample_rate: 44100,
        channels: 2,
        bitrate_mode: Some(BitrateMode::Cbr(128)),
        ..EncoderConfig::default()
    };
    let err = Encoder::new(config);
    assert!(matches!(err, Err(Error::InvalidArgument(_))));
    if let Err(err) = err {
        assert!(err.to_string().contains("opus"));
    }
}

#[test]
fn encoder_rejects_bitrate_mode_for_lossless_codec() {
    let config = EncoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Flac,
        sample_rate: 48000,
        channels: 2,
        bitrate_mode: Some(BitrateMode::Cbr(128)),
        ..EncoderConfig::default()
    };
    assert!(matches!(
        Encoder::new(config),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn encoder_rejects_bitrate_mode_disagreeing_with_bitrate() {
    let config = EncoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Opus,
        sample_rate: 48000,
        channels: 2,
        bitrate: Some(96_000),
        bitrate_mode: Some(BitrateMode::Cbr(128)),
        ..EncoderConfig::default()
    };
    assert!(matches!(
        Encoder::new(config),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn encoder_rejects_fixed_bitrate_with_vbr_mode() {
    let config = EncoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Vorbis,
        sample_rate: 48000,
        channels: 2,
        bitrate: Some(128_000),
        bitrate_mode: Some(BitrateMode::Vbr(2)),
        ..EncoderConfig::default()
    };
    assert!(matches!(
        Encoder::new(config),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn encoder_accepts_matching_bitrate_and_bitrate_mode() {
    let config = EncoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Opus,
        sample_rate: 48000,
        channels: 2,
        bitrate: Some(128_000),
        bitrate_mode: Some(BitrateMode::Cbr(128)),
        ..EncoderConfig::default()
    };
    Encoder::new(config).unwrap();
}

#[test]
fn opus_encode_still_unsupported_even_with_valid_bitrate() {
    // The bitrate API is ready; the Opus encoder is not. A valid
    // mode must not make `encode` pretend to work.
    let mut encoder = Encoder::new(EncoderConfig {
        codec: codec_whatttttttttttt_core::Codec::Opus,
        sample_rate: 48000,
        channels: 1,
        bitrate_mode: Some(BitrateMode::Cbr(128)),
        ..EncoderConfig::default()
    })
    .unwrap();
    let frame = Frame::Audio(AudioFrame {
        sample_rate: 48000,
        channels: 1,
        samples: vec![0.0; 480],
        timestamp: 0,
    });
    let err = encoder.encode(&frame).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
}
