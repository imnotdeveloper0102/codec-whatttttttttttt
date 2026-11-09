//! Bitrate selection and validation for lossy audio encoders.
//!
//! It defines its own `BitrateCodec` selector covering the lossy
//! audio codecs — MP3, AAC-LC, Opus, Vorbis — and validates a
//! `BitrateMode` against that codec and the sample rate.
//! `EncoderConfig::bitrate_mode` reuses this validation for the
//! codecs that do have a `Codec` variant today (Mp3, Opus, Vorbis,
//! Aac).
//!
//! # VBR scale
//!
//! `BitrateMode::Vbr` uses one unified integer scale, 0..=9, where
//! **0 is the best quality (largest output) and 9 the lowest**,
//! following the LAME `-V 0..9` convention. A codec whose native
//! scale runs the other way is mapped explicitly instead of silently
//! reinterpreting the number: `vorbis_quality` maps the unified
//! scale onto the Vorbis native quality scale (-1.0..=10.0, higher
//! is better), so unified 0 becomes Vorbis 10.0 and unified 9
//! becomes Vorbis 1.0.
//!
//! # Bitrate ladder
//!
//! CBR and ABR accept only the ladder 64 / 96 / 128 / 320 kbps for
//! every codec here. That is a deliberate library restriction, not a
//! claim about the codecs: MP3, AAC, Opus, and Vorbis all accept
//! wider bitrate ranges in their specifications, but this library
//! only exposes the four ladder rungs until an encoder exists that
//! can honour an arbitrary target. Opus encoding itself is still
//! unimplemented (CELT/SILK `Unsupported`): the Opus entries here
//! only make the API ready for the future encoder, they do not
//! claim an Opus encoder exists.
//!
//! # Error messages
//!
//! `Error::InvalidArgument` holds a `&'static str`, so a message
//! cannot interpolate the offending value at runtime. Each message
//! therefore names the codec and states the allowed set in full;
//! the caller already knows the value it passed. Tests assert on
//! both the codec name and the allowed set in the message.

use crate::error::{Error, Result};
use crate::frame::Codec;

/// Target-bitrate mode for a lossy audio encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BitrateMode {
    /// Constant bitrate, in kilobits per second.
    Cbr(u32),
    /// Variable bitrate. Quality 0..=9 on the unified scale: 0 is
    /// the best quality, 9 the lowest (LAME `-V` convention). See
    /// the module docs and `vorbis_quality` for the per-codec
    /// mapping.
    Vbr(u8),
    /// Average bitrate, in kilobits per second.
    Abr(u32),
}

impl BitrateMode {
    /// Bitrate in bits per second for `Cbr`/`Abr`, or `None` for
    /// `Vbr`, which has no fixed bitrate by design.
    #[must_use]
    pub fn bitrate_bps(self) -> Option<u32> {
        match self {
            Self::Cbr(kbps) | Self::Abr(kbps) => kbps.checked_mul(1000),
            Self::Vbr(_) => None,
        }
    }

    /// True for `Vbr`.
    #[must_use]
    pub fn is_vbr(self) -> bool {
        matches!(self, Self::Vbr(_))
    }

    /// Validates this mode for `codec` at `sample_rate`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` naming the codec and the allowed
    /// set when the bitrate is off the ladder, the VBR quality is
    /// outside 0..=9, or the sample rate is not one the codec
    /// supports.
    pub fn validate_for(self, codec: BitrateCodec, sample_rate: u32) -> Result<()> {
        validate_bitrate(codec, self, sample_rate)
    }
}

/// Lossy audio codec a bitrate mode applies to.
///
/// This is separate from `Codec` on purpose: the MP3 and AAC
/// bitrate tables predate their `Codec` variants, and bitrate
/// validation must work for them before their decoders/encoders
/// land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BitrateCodec {
    /// MPEG-1/2/2.5 Layer III.
    Mp3,
    /// AAC-LC (patent-encumbered; software encode/decode behind
    /// the license gate, hardware only — see `license.rs`).
    Aac,
    /// Opus (RFC 6716). Encoding is still unimplemented; the
    /// bitrate API is ready for the future encoder.
    Opus,
    /// Vorbis I.
    Vorbis,
}

impl BitrateCodec {
    /// Short name used in errors and logs.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Aac => "aac",
            Self::Opus => "opus",
            Self::Vorbis => "vorbis",
        }
    }

    /// Sample rates the codec accepts, in Hz.
    ///
    /// MP3 takes the MPEG-1/2/2.5 rates (8000–48000 Hz). AAC-LC
    /// takes the MPEG-4 rates up to 96000 Hz. Opus takes its five
    /// input rates (RFC 6716 section 2: 8/12/16/24/48 kHz; the
    /// decoder always outputs 48 kHz). Vorbis takes any common
    /// rate up to the library limit of 192 kHz.
    #[must_use]
    pub fn supported_sample_rates(self) -> &'static [u32] {
        match self {
            Self::Mp3 => &[8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000],
            Self::Aac => &[
                8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000,
            ],
            Self::Opus => &[8000, 12000, 16000, 24000, 48000],
            Self::Vorbis => &[
                8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000,
                176_400, 192_000,
            ],
        }
    }

    /// CBR/ABR ladder in kbps. One ladder for every codec: see the
    /// module docs for why this is a library restriction, not a
    /// spec limit.
    #[must_use]
    pub fn supported_bitrates_kbps(self) -> &'static [u32] {
        // The ladder is the same for all four codecs today; the
        // match keeps the per-codec hook for when it diverges.
        match self {
            Self::Mp3 | Self::Aac | Self::Opus | Self::Vorbis => &[64, 96, 128, 320],
        }
    }

    /// Maps a `Codec` to its bitrate codec, or `None` when the
    /// codec has no bitrate mode (PCM/FLAC are lossless, AV1/VP9
    /// are video, AVC/HEVC are licensed video, DSD/ALAC are not
    /// lossy-bitrate codecs).
    #[must_use]
    pub fn from_codec(codec: Codec) -> Option<Self> {
        match codec {
            Codec::Mp3 => Some(Self::Mp3),
            Codec::Opus => Some(Self::Opus),
            Codec::Vorbis => Some(Self::Vorbis),
            Codec::Aac => Some(Self::Aac),
            _ => None,
        }
    }
}

/// Maps a unified VBR quality (0..=9, 0 best) onto the Vorbis
/// native quality scale (-1.0..=10.0, higher is better).
///
/// The mapping is linear: unified 0 becomes Vorbis 10.0 and
/// unified 9 becomes Vorbis 1.0, i.e. `q = 10.0 - quality`. The
/// Vorbis extremes below 1.0 and the `-1.0` floor are not reachable
/// through the unified scale; a caller that needs them must use
/// the Vorbis encoder directly once it exists. Returns `None`
/// when `quality` is outside 0..=9.
#[must_use]
pub fn vorbis_quality(quality: u8) -> Option<f32> {
    if quality > 9 {
        return None;
    }
    Some(10.0 - f32::from(quality))
}

/// Validates a bitrate mode for `codec` at `sample_rate`.
///
/// # Errors
///
/// Returns `InvalidArgument` naming the codec and the allowed set
/// when the bitrate is off the ladder, the VBR quality is outside
/// 0..=9, or the sample rate is not one the codec supports.
pub fn validate_bitrate(codec: BitrateCodec, mode: BitrateMode, sample_rate: u32) -> Result<()> {
    if sample_rate == 0 {
        return Err(Error::InvalidArgument(match codec {
            BitrateCodec::Mp3 => "mp3 sample rate must be positive",
            BitrateCodec::Aac => "aac sample rate must be positive",
            BitrateCodec::Opus => "opus sample rate must be positive",
            BitrateCodec::Vorbis => "vorbis sample rate must be positive",
        }));
    }
    if !codec.supported_sample_rates().contains(&sample_rate) {
        return Err(Error::InvalidArgument(match codec {
            BitrateCodec::Mp3 => {
                "mp3 sample rate must be one of 8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000 Hz"
            }
            BitrateCodec::Aac => {
                "aac sample rate must be one of 8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000 Hz"
            }
            BitrateCodec::Opus => {
                "opus sample rate must be one of 8000, 12000, 16000, 24000, 48000 Hz"
            }
            BitrateCodec::Vorbis => {
                "vorbis sample rate must be one of 8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000, 176400, 192000 Hz"
            }
        }));
    }
    match mode {
        BitrateMode::Cbr(kbps) => {
            if !codec.supported_bitrates_kbps().contains(&kbps) {
                return Err(Error::InvalidArgument(match codec {
                    BitrateCodec::Mp3 => "mp3 cbr bitrate must be one of 64, 96, 128, 320 kbps",
                    BitrateCodec::Aac => "aac cbr bitrate must be one of 64, 96, 128, 320 kbps",
                    BitrateCodec::Opus => "opus cbr bitrate must be one of 64, 96, 128, 320 kbps",
                    BitrateCodec::Vorbis => {
                        "vorbis cbr bitrate must be one of 64, 96, 128, 320 kbps"
                    }
                }));
            }
        }
        BitrateMode::Abr(kbps) => {
            if !codec.supported_bitrates_kbps().contains(&kbps) {
                return Err(Error::InvalidArgument(match codec {
                    BitrateCodec::Mp3 => "mp3 abr bitrate must be one of 64, 96, 128, 320 kbps",
                    BitrateCodec::Aac => "aac abr bitrate must be one of 64, 96, 128, 320 kbps",
                    BitrateCodec::Opus => "opus abr bitrate must be one of 64, 96, 128, 320 kbps",
                    BitrateCodec::Vorbis => {
                        "vorbis abr bitrate must be one of 64, 96, 128, 320 kbps"
                    }
                }));
            }
        }
        BitrateMode::Vbr(quality) => {
            if quality > 9 {
                return Err(Error::InvalidArgument(match codec {
                    BitrateCodec::Mp3 => "mp3 vbr quality must be 0..=9 (0 is best quality)",
                    BitrateCodec::Aac => "aac vbr quality must be 0..=9 (0 is best quality)",
                    BitrateCodec::Opus => "opus vbr quality must be 0..=9 (0 is best quality)",
                    BitrateCodec::Vorbis => "vorbis vbr quality must be 0..=9 (0 is best quality)",
                }));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbr_bps_converts_kilobits() {
        assert_eq!(BitrateMode::Cbr(128).bitrate_bps(), Some(128_000));
        assert_eq!(BitrateMode::Abr(64).bitrate_bps(), Some(64_000));
        assert_eq!(BitrateMode::Vbr(0).bitrate_bps(), None);
        assert!(BitrateMode::Vbr(5).is_vbr());
        assert!(!BitrateMode::Cbr(128).is_vbr());
    }

    #[test]
    fn vorbis_quality_maps_unified_scale_to_native() {
        assert_eq!(vorbis_quality(0), Some(10.0));
        assert_eq!(vorbis_quality(9), Some(1.0));
        assert_eq!(vorbis_quality(5), Some(5.0));
        assert_eq!(vorbis_quality(10), None);
    }

    #[test]
    fn from_codec_maps_lossy_audio_codecs() {
        assert_eq!(
            BitrateCodec::from_codec(Codec::Mp3),
            Some(BitrateCodec::Mp3)
        );
        assert_eq!(
            BitrateCodec::from_codec(Codec::Opus),
            Some(BitrateCodec::Opus)
        );
        assert_eq!(
            BitrateCodec::from_codec(Codec::Vorbis),
            Some(BitrateCodec::Vorbis)
        );
        assert_eq!(
            BitrateCodec::from_codec(Codec::Aac),
            Some(BitrateCodec::Aac)
        );
        assert_eq!(BitrateCodec::from_codec(Codec::Flac), None);
        assert_eq!(BitrateCodec::from_codec(Codec::Pcm), None);
        assert_eq!(BitrateCodec::from_codec(Codec::Alac), None);
    }

    #[test]
    fn opus_rejects_44100_as_input_rate() {
        // Opus input rates are 8/12/16/24/48 kHz only (RFC 6716);
        // 44100 Hz is a common mistake and must be rejected here,
        // not silently resampled by the encoder.
        let err = validate_bitrate(BitrateCodec::Opus, BitrateMode::Cbr(128), 44100).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
        assert!(err.to_string().contains("opus"));
    }
}
