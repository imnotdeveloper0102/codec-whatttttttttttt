//! Hardware decode/encode path for licensed codecs.
//!
//! The software decoder and encoder never touch AVC or HEVC:
//! those codecs may only run on platform hardware
//! (`MediaCodec`, `VideoToolbox`, Media Foundation), which is the
//! caller's responsibility to license. AAC is licensed too, but
//! unlike AVC/HEVC it has a software decode path: AAC-LC decoding
//! in `Decoder` is allowed only behind the `licensed-aac`
//! feature with a license validated at construction (see
//! `Decoder::from_aac_config`); without a license the software
//! path returns the gate error from `licensed_software_path_error`.
//! This module defines the traits for the hardware path and a
//! stub implementation that proves the gating without calling any
//! real hardware API.
//!
//! Error rule, kept the same everywhere in the crate: when the
//! compile-time feature for the codec is off, the path does not
//! exist in the build at all, so the error is `Unsupported`.
//! When the feature is on but no valid license was presented,
//! the path exists and is closed, so the error is
//! `LicenseRequired`. The two cases must stay distinguishable:
//! rebuilding with the feature and presenting a license are
//! different fixes.
//!
//! `Codec::Aac` has a hardware path behind the `licensed-aac`
//! feature, the same shape as AVC/HEVC. Nothing in this module
//! calls a real hardware API: the stubs return a minimal valid
//! frame or packet and are marked as such wherever they appear.

use crate::error::{Error, Result};
use crate::frame::{
    AudioFrame, Codec, ColourRange, ColourSpace, Frame, Packet, PixelFormat, VideoFrame,
};
use crate::license::LicenseContext;

/// Error for a licensed codec asked of the software path
/// without a license. Feature off means the licensed path was
/// not compiled in (`Unsupported`); feature on means the caller
/// reached the software path without a license
/// (`LicenseRequired`). AAC-LC software decode exists, but only
/// for a decoder built with a validated license (see
/// `Decoder::from_aac_config`); this error is what an
/// unlicensed decoder returns. AVC/HEVC have no software path
/// at all. AAC is gated on `licensed-aac`, AVC/HEVC on
/// `licensed-avc-hevc`; any other codec has no licensed path and
/// is always `Unsupported`.
#[must_use]
pub fn licensed_software_path_error(codec: Codec) -> Error {
    match codec {
        Codec::Aac => {
            #[cfg(feature = "licensed-aac")]
            {
                Error::LicenseRequired {
                    codec: codec.name(),
                }
            }
            #[cfg(not(feature = "licensed-aac"))]
            {
                Error::Unsupported(
                    "aac software decode and encode are not available; \
                    hardware decode requires the licensed-aac feature and a valid license",
                )
            }
        }
        Codec::Avc | Codec::Hevc => {
            #[cfg(feature = "licensed-avc-hevc")]
            {
                Error::LicenseRequired {
                    codec: codec.name(),
                }
            }
            #[cfg(not(feature = "licensed-avc-hevc"))]
            {
                Error::Unsupported(
                    "avc/hevc software decode and encode are not available; \
                    hardware decode requires the licensed-avc-hevc feature and a valid license",
                )
            }
        }
        _ => Error::Unsupported("software decode and encode are not available for this codec"),
    }
}

/// Whether the compile-time gate for the codec's hardware path is
/// open. AVC, HEVC, and AAC have paths; every other codec reports
/// closed, so opening a stub for one is `Unsupported`.
#[must_use]
pub fn licensed_feature_enabled(codec: Codec) -> bool {
    match codec {
        Codec::Avc | Codec::Hevc => {
            #[cfg(feature = "licensed-avc-hevc")]
            {
                true
            }
            #[cfg(not(feature = "licensed-avc-hevc"))]
            {
                false
            }
        }
        Codec::Aac => {
            #[cfg(feature = "licensed-aac")]
            {
                true
            }
            #[cfg(not(feature = "licensed-aac"))]
            {
                false
            }
        }
        _ => false,
    }
}

/// Checks the passthrough gate for one packet of `codec`: open
/// codecs pass, licensed codecs pass only when their feature is
/// compiled in and a license was presented. The license itself
/// was validated when it was set (see the demuxers'
/// `set_license_context`); this check only asks whether one is
/// present, so it can run per packet without growing the audit
/// log.
///
/// # Errors
///
/// Returns `Unsupported` when the codec's feature is off and
/// `LicenseRequired` when no license was presented.
pub fn passthrough_allowed(codec: Codec, license_present: bool) -> Result<()> {
    if !matches!(codec, Codec::Avc | Codec::Hevc | Codec::Aac) {
        return Ok(());
    }
    if !licensed_feature_enabled(codec) {
        return Err(Error::Unsupported(
            "passthrough for this codec is not enabled in this build; \
            rebuild with the licensed-avc-hevc or licensed-aac feature",
        ));
    }
    if !license_present {
        return Err(Error::LicenseRequired {
            codec: codec.name(),
        });
    }
    Ok(())
}

/// Hardware decoder for one licensed codec. The decoder is opened
/// with a license before any packet is decoded; `decode` before
/// `open` is `LicenseRequired`.
pub trait HardwareDecoder {
    /// Codec this decoder handles.
    fn codec(&self) -> Codec;
    /// Opens the hardware path after the license checks out.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the codec has no hardware path
    /// or its feature is off, and `LicenseRequired` (or
    /// `InvalidArgument` for a malformed license) when the
    /// license check fails.
    fn open(&mut self, license: &mut LicenseContext) -> Result<()>;
    /// Decodes one packet into a frame.
    ///
    /// # Errors
    ///
    /// Returns `LicenseRequired` when the decoder was not opened.
    fn decode(&mut self, packet: &Packet) -> Result<Frame>;
}

/// Hardware encoder for one licensed codec. Same opening rule as
/// `HardwareDecoder`: no license, no encoding.
pub trait HardwareEncoder {
    /// Codec this encoder handles.
    fn codec(&self) -> Codec;
    /// Opens the hardware path after the license checks out.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the codec has no hardware path
    /// or its feature is off, and `LicenseRequired` (or
    /// `InvalidArgument` for a malformed license) when the
    /// license check fails.
    fn open(&mut self, license: &mut LicenseContext) -> Result<()>;
    /// Encodes one frame into a packet.
    ///
    /// # Errors
    ///
    /// Returns `LicenseRequired` when the encoder was not opened.
    fn encode(&mut self, frame: &Frame) -> Result<Packet>;
}

/// Stub hardware decoder: proves the license gate without calling
/// any real hardware API. A successful `decode` returns a minimal
/// valid video frame (0x0, no planes, the packet's timestamp) for
/// AVC/HEVC, or a minimal valid audio frame (no samples) for AAC,
/// not decoded samples or pixels.
pub struct StubHardwareDecoder {
    codec: Codec,
    opened: bool,
}

impl StubHardwareDecoder {
    /// Builds a closed stub for the codec.
    #[must_use]
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            opened: false,
        }
    }

    /// True after a successful `open`.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.opened
    }
}

impl HardwareDecoder for StubHardwareDecoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn open(&mut self, license: &mut LicenseContext) -> Result<()> {
        if !licensed_feature_enabled(self.codec) {
            return Err(Error::Unsupported(
                "hardware decode for this codec is not enabled in this build",
            ));
        }
        license.validate(self.codec.name())?;
        self.opened = true;
        Ok(())
    }

    fn decode(&mut self, packet: &Packet) -> Result<Frame> {
        if !self.opened {
            return Err(Error::LicenseRequired {
                codec: self.codec.name(),
            });
        }
        if packet.codec != self.codec {
            return Err(Error::InvalidArgument(
                "packet codec does not match hardware decoder codec",
            ));
        }
        // AAC is audio: the stub returns a minimal valid audio
        // frame (no samples, the packet's timestamp), mirroring
        // the 0x0 video frame for AVC/HEVC.
        if self.codec == Codec::Aac {
            return Ok(Frame::Audio(AudioFrame {
                sample_rate: 0,
                channels: 0,
                samples: Vec::new(),
                timestamp: packet.timestamp,
            }));
        }
        Ok(Frame::Video(VideoFrame {
            width: 0,
            height: 0,
            format: PixelFormat::Yuv420,
            planes: Vec::new(),
            strides: Vec::new(),
            timestamp: packet.timestamp,
            colour_space: ColourSpace::default(),
            colour_range: ColourRange::default(),
        }))
    }
}

/// Stub hardware encoder: proves the license gate without calling
/// any real hardware API. A successful `encode` returns a packet
/// with no payload, not encoded bytes.
pub struct StubHardwareEncoder {
    codec: Codec,
    opened: bool,
}

impl StubHardwareEncoder {
    /// Builds a closed stub for the codec.
    #[must_use]
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            opened: false,
        }
    }

    /// True after a successful `open`.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.opened
    }
}

impl HardwareEncoder for StubHardwareEncoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn open(&mut self, license: &mut LicenseContext) -> Result<()> {
        if !licensed_feature_enabled(self.codec) {
            return Err(Error::Unsupported(
                "hardware encode for this codec is not enabled in this build",
            ));
        }
        license.validate(self.codec.name())?;
        self.opened = true;
        Ok(())
    }

    fn encode(&mut self, frame: &Frame) -> Result<Packet> {
        if !self.opened {
            return Err(Error::LicenseRequired {
                codec: self.codec.name(),
            });
        }
        Ok(Packet {
            codec: self.codec,
            data: Vec::new(),
            timestamp: frame.timestamp(),
            is_keyframe: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::license::LicenseType;

    fn valid_fixture() -> LicenseContext {
        LicenseContext::new(
            "test licensee".to_string(),
            LicenseType::Commercial,
            "TEST-12345678".to_string(),
            4_102_444_800,
            "avc".to_string(),
        )
        .unwrap()
    }

    #[test]
    fn decode_before_open_is_license_required() {
        let mut stub = StubHardwareDecoder::new(Codec::Avc);
        let packet = Packet {
            codec: Codec::Avc,
            data: Vec::new(),
            timestamp: 7,
            is_keyframe: true,
        };
        let err = stub.decode(&packet).unwrap_err();
        assert!(matches!(err, Error::LicenseRequired { codec: "avc" }));
    }

    #[test]
    fn non_licensed_codec_stub_never_opens() {
        let mut stub = StubHardwareDecoder::new(Codec::Pcm);
        let err = stub.open(&mut valid_fixture()).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }
}
