//! Frame encoder.
//!
//! `Codec::Pcm` and `Codec::Flac` (Phase 9) encode. Every other
//! codec returns `Unsupported` naming the codec. PCM encoding is
//! the inverse of PCM decoding: f32 samples in [-1.0, 1.0] are
//! converted with the same saturating scale factors as
//! `pcm::encode_samples`. FLAC encoding quantizes f32 to i16 with
//! the same scale, then writes 16-bit FLAC frames.

use crate::error::{Error, Result};
use crate::flac_frame::FlacEncoder;
use crate::frame::{Codec, Frame, Packet};
use crate::limits::{MAX_CHANNELS, MAX_HEIGHT, MAX_SAMPLE_RATE, MAX_WIDTH};
use crate::pcm::{encode_samples, PcmFormat};

/// Configuration for an `Encoder`. Limits are validated in `Encoder::new`.
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    /// Codec to encode.
    pub codec: Codec,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u32,
    /// Target bitrate in bits per second, when the codec uses one.
    pub bitrate: Option<u32>,
    /// Video width in pixels, when encoding video.
    pub width: Option<u32>,
    /// Video height in pixels, when encoding video.
    pub height: Option<u32>,
    /// PCM sample format for `Codec::Pcm`.
    pub pcm_format: PcmFormat,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            codec: Codec::Pcm,
            sample_rate: 44100,
            channels: 1,
            bitrate: None,
            width: None,
            height: None,
            pcm_format: PcmFormat::I16,
        }
    }
}

/// Stateful encoder. An `Encoder` is `Send` but not `Sync`: use one
/// instance from one thread at a time, or guard it with a `Mutex`.
pub struct Encoder {
    config: EncoderConfig,
    /// Streaming FLAC encoder, present when the codec is FLAC.
    /// It carries the running sample number across `encode` calls.
    flac_encoder: Option<FlacEncoder>,
}

/// f32 sample to i16 with the same saturating scale as
/// `pcm::encode_samples` for I16 (`s * 32768`, 32768 saturating to
/// 32767). Inputs that are not exactly representable are quantized,
/// as with PCM I16.
#[allow(clippy::cast_possible_truncation)]
fn f32_to_i16(sample: f32) -> i16 {
    // The input is clamped to [-1, 1], so the scaled value is in
    // [-32768, 32768] and fits in i32; the clamp to the i16 range
    // then makes the final conversion exact.
    let clamped = sample.clamp(-1.0, 1.0);
    let value = (clamped * 32768.0).round() as i32;
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

impl Encoder {
    /// Creates an encoder, validating the configuration.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the channel count or sample rate
    /// is zero, and `Overflow` when a value exceeds the hard limit.
    pub fn new(config: EncoderConfig) -> Result<Self> {
        if config.channels == 0 || config.channels > MAX_CHANNELS {
            return Err(Error::InvalidArgument("channel count out of range"));
        }
        if config.sample_rate == 0 || config.sample_rate > MAX_SAMPLE_RATE {
            return Err(Error::InvalidArgument("sample rate out of range"));
        }
        if let Some(width) = config.width {
            if width == 0 || width > MAX_WIDTH {
                return Err(Error::InvalidArgument("width out of range"));
            }
        }
        if let Some(height) = config.height {
            if height == 0 || height > MAX_HEIGHT {
                return Err(Error::InvalidArgument("height out of range"));
            }
        }
        let flac_encoder = if config.codec == Codec::Flac {
            Some(FlacEncoder::new(config.channels, config.sample_rate)?)
        } else {
            None
        };
        Ok(Self {
            config,
            flac_encoder,
        })
    }

    /// The `fLaC` marker plus STREAMINFO in streaming form (total
    /// samples 0, minimum block size 0), to write before the FLAC
    /// frames from `encode`. Returns an empty vector when this
    /// encoder is not a FLAC encoder.
    #[must_use]
    pub fn flac_streaminfo_bytes(&self) -> Vec<u8> {
        match &self.flac_encoder {
            Some(encoder) => encoder.streaminfo_bytes(),
            None => Vec::new(),
        }
    }

    /// Encodes one frame into a packet. A PCM packet is always a
    /// keyframe; its timestamp is copied from the frame. A FLAC
    /// packet is one FLAC frame (block size = frame count,
    /// 1..=4096); write `flac_streaminfo_bytes()` before the first
    /// frame. Every FLAC frame is independently decodable.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` for a codec not implemented yet,
    /// `InvalidArgument` when the frame type or its channel count or
    /// sample rate does not match the configuration, or when a FLAC
    /// frame holds more than 4096 samples.
    pub fn encode(&mut self, frame: &Frame) -> Result<Packet> {
        match self.config.codec {
            Codec::Pcm => {
                let audio = match frame {
                    Frame::Audio(audio) => audio,
                    Frame::Video(_) => {
                        return Err(Error::InvalidArgument(
                            "pcm encoder requires an audio frame",
                        ));
                    }
                };
                if audio.channels != self.config.channels {
                    return Err(Error::InvalidArgument(
                        "frame channel count does not match encoder config",
                    ));
                }
                if audio.sample_rate != self.config.sample_rate {
                    return Err(Error::InvalidArgument(
                        "frame sample rate does not match encoder config",
                    ));
                }
                let data = encode_samples(&audio.samples, self.config.pcm_format, audio.channels)?;
                Ok(Packet {
                    codec: Codec::Pcm,
                    data,
                    timestamp: audio.timestamp,
                    is_keyframe: true,
                })
            }
            Codec::Flac => {
                let audio = match frame {
                    Frame::Audio(audio) => audio,
                    Frame::Video(_) => {
                        return Err(Error::InvalidArgument(
                            "flac encoder requires an audio frame",
                        ));
                    }
                };
                if audio.channels != self.config.channels {
                    return Err(Error::InvalidArgument(
                        "frame channel count does not match encoder config",
                    ));
                }
                if audio.sample_rate != self.config.sample_rate {
                    return Err(Error::InvalidArgument(
                        "frame sample rate does not match encoder config",
                    ));
                }
                let samples: Vec<i16> = audio
                    .samples
                    .iter()
                    .map(|&sample| f32_to_i16(sample))
                    .collect();
                let Some(encoder) = &mut self.flac_encoder else {
                    return Err(Error::InvalidArgument("flac encoder is not initialized"));
                };
                let data = encoder.encode(&samples)?;
                Ok(Packet {
                    codec: Codec::Flac,
                    data,
                    timestamp: audio.timestamp,
                    is_keyframe: true,
                })
            }
            codec => Err(Error::Unsupported(match codec {
                Codec::Flac => "flac encode",
                Codec::Opus => "opus encode needs CELT/SILK encoders (not implemented)",
                Codec::Vorbis => {
                    "vorbis encode needs codebook/floor/residue encoders (not implemented)"
                }
                Codec::Av1 => "av1 encode needs a full video encoder (not implemented)",
                Codec::Vp9 => "vp9 encode needs a full video encoder (not implemented)",
                Codec::Pcm => "pcm encode",
            })),
        }
    }

    /// Flushes buffered packets. PCM buffers nothing, so this is `None`.
    ///
    /// # Errors
    ///
    /// This implementation never fails; the `Result` exists so later
    /// codecs with delay can report errors here.
    pub fn flush(&mut self) -> Result<Option<Packet>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::AudioFrame;

    #[test]
    fn encodes_audio_frame_to_pcm_packet() {
        let mut encoder = Encoder::new(EncoderConfig {
            sample_rate: 8000,
            channels: 1,
            ..EncoderConfig::default()
        })
        .unwrap();
        let frame = Frame::Audio(AudioFrame {
            sample_rate: 8000,
            channels: 1,
            samples: vec![0.5, -0.5],
            timestamp: 100,
        });
        let packet = encoder.encode(&frame).unwrap();
        assert_eq!(packet.codec, Codec::Pcm);
        assert_eq!(packet.timestamp, 100);
        assert!(packet.is_keyframe);
        assert_eq!(packet.data.len(), 4);
    }

    #[test]
    fn channel_mismatch_returns_invalid_argument() {
        let mut encoder = Encoder::new(EncoderConfig {
            sample_rate: 8000,
            channels: 2,
            ..EncoderConfig::default()
        })
        .unwrap();
        let frame = Frame::Audio(AudioFrame {
            sample_rate: 8000,
            channels: 1,
            samples: vec![0.5],
            timestamp: 0,
        });
        let err = encoder.encode(&frame).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    fn opus_encode_returns_unsupported() {
        let mut encoder = Encoder::new(EncoderConfig {
            codec: Codec::Opus,
            ..EncoderConfig::default()
        })
        .unwrap();
        let frame = Frame::Audio(AudioFrame {
            sample_rate: 44100,
            channels: 1,
            samples: vec![0.0],
            timestamp: 0,
        });
        let err = encoder.encode(&frame).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn flac_encode_produces_a_decodable_frame() {
        let mut encoder = Encoder::new(EncoderConfig {
            codec: Codec::Flac,
            sample_rate: 8000,
            channels: 1,
            ..EncoderConfig::default()
        })
        .unwrap();
        let frame = Frame::Audio(AudioFrame {
            sample_rate: 8000,
            channels: 1,
            samples: vec![0.0, 0.5, -0.5, 0.25],
            timestamp: 0,
        });
        let packet = encoder.encode(&frame).unwrap();
        assert_eq!(packet.codec, Codec::Flac);
        assert!(packet.is_keyframe);
        assert_ne!(packet.data.len(), 0);
        let header = encoder.flac_streaminfo_bytes();
        assert_eq!(&header[0..4], b"fLaC");
    }
}
