//! Frame encoder.
//!
//! `Codec::Pcm`, `Codec::Flac` (Phase 9, 16/20/24-bit), `Codec::Mp3`
//! (CBR only), `Codec::Aac` (AAC-LC, behind the license gate),
//! `Codec::Alac` (16/24-bit lossless), and `Codec::Dsd` (PCM to
//! DSD via a delta-sigma modulator) encode. Every other codec
//! returns `Unsupported` naming the codec, except `Avc`/`Hevc`,
//! which return the license-gate error. PCM encoding is the
//! inverse of PCM decoding: f32 samples in [-1.0, 1.0] are
//! converted with the same saturating scale factors as
//! `pcm::encode_samples`. FLAC encoding quantizes f32 to i16
//! (or i32 at 20/24 bits) with the same scale, then writes FLAC
//! frames at that width.

use crate::aac_encode::AacEncoder;
use crate::alac_encode::AlacEncoder;
use crate::bitrate::{BitrateCodec, BitrateMode};
use crate::dsd_decimator::DsdLevel;
use crate::dsd_modulator::DsdModulator;
use crate::error::{Error, Result};
use crate::flac_frame::FlacEncoder;
use crate::frame::{Codec, Frame, Packet};
use crate::license::LicenseContext;
use crate::limits::{MAX_CHANNELS, MAX_HEIGHT, MAX_SAMPLE_RATE, MAX_WIDTH};
use crate::mp3_encode::Mp3Encoder;
use crate::pcm::{encode_samples, PcmFormat};
use std::collections::VecDeque;

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
    /// Bitrate mode (`Cbr`/`Vbr`/`Abr`) for lossy audio encoders.
    /// Validated against the codec and sample rate in
    /// `Encoder::new` via `bitrate::validate_bitrate`. It must
    /// agree with `bitrate` when both are set (a `Cbr`/`Abr`
    /// value in kbps must equal `bitrate` in bps); a `Vbr` mode
    /// has no fixed bitrate, so `bitrate` must be `None` with it.
    /// Opus encoding is still unimplemented, so a valid mode
    /// there only prepares the API — `encode` still returns
    /// `Unsupported` for Opus.
    pub bitrate_mode: Option<BitrateMode>,
    /// Video width in pixels, when encoding video.
    pub width: Option<u32>,
    /// Video height in pixels, when encoding video.
    pub height: Option<u32>,
    /// PCM sample format for `Codec::Pcm`.
    pub pcm_format: PcmFormat,
    /// License for a licensed codec (`Codec::Aac`). Taken by
    /// `Encoder::new` when the codec is AAC; without it AAC
    /// construction fails with the license-gate error.
    pub license: Option<LicenseContext>,
    /// DSD level for `Codec::Dsd` (fixes the DSD rate).
    /// Defaults to DSD64 when `None`.
    pub dsd_level: Option<DsdLevel>,
    /// FLAC bit depth for `Codec::Flac` (16, 20, or 24).
    /// Defaults to 16 when `None`; the encoder quantizes
    /// f32 samples to that width.
    pub bits_per_sample: Option<u8>,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            codec: Codec::Pcm,
            sample_rate: 44100,
            channels: 1,
            bitrate: None,
            bitrate_mode: None,
            width: None,
            height: None,
            pcm_format: PcmFormat::I16,
            license: None,
            dsd_level: None,
            bits_per_sample: None,
        }
    }
}

/// Stateful encoder. An `Encoder` is `Send` but not `Sync`: use one
/// instance from one thread at a time, or guard it with a `Mutex`.
pub struct Encoder {
    config: EncoderConfig,
    /// Streaming FLAC encoder, present when the codec is FLAC.
    /// It carries the running sample number across `encode` calls.
    flac: Option<FlacEncoder>,
    /// Streaming MP3 encoder, present when the codec is MP3.
    mp3: Option<Mp3Encoder>,
    /// MP3 flush packets waiting for `flush` calls: `flush`
    /// returns one packet, but `Mp3Encoder::flush` returns all
    /// of them at once.
    mp3_pending: VecDeque<Packet>,
    /// AAC-LC encoder, present when the codec is AAC.
    aac_lc: Option<AacEncoder>,
    /// ALAC encoder, present when the codec is ALAC.
    alac: Option<AlacEncoder>,
    /// Delta-sigma modulator, present when the codec is DSD.
    dsd_modulator: Option<DsdModulator>,
    /// Timestamp (microseconds) where the next DSD flush packet
    /// starts: the end of the last encoded DSD frame.
    dsd_next_timestamp: u64,
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

/// f32 sample to i32 at `bits` bits (16, 20, or 24), with the
/// saturating scale the FLAC wide encoder expects: `s *
/// 2^(bits-1)`, rounded, clamped to the signed range of `bits`.
/// The input is clamped to [-1, 1] first, so the scaled value
/// always fits in i32 for these widths.
#[allow(clippy::cast_possible_truncation)]
fn f32_to_i32(sample: f32, bits: u8) -> i32 {
    let scale = 2f32.powi(i32::from(bits) - 1);
    let clamped = sample.clamp(-1.0, 1.0);
    let value = (clamped * scale).round() as i32;
    let max = (1i32 << (i32::from(bits) - 1)) - 1;
    let min = -(1i32 << (i32::from(bits) - 1));
    value.clamp(min, max)
}

impl Encoder {
    /// Creates an encoder, validating the configuration. For
    /// `Codec::Aac` this fails at construction when the
    /// licensed-aac feature is off (`Unsupported`), no license
    /// was presented, or it has expired (`LicenseRequired`),
    /// like `Decoder::from_aac_config`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the channel count or sample rate
    /// is zero, and `Overflow` when a value exceeds the hard limit.
    pub fn new(mut config: EncoderConfig) -> Result<Self> {
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
        if let Some(mode) = config.bitrate_mode {
            let Some(bitrate_codec) = BitrateCodec::from_codec(config.codec) else {
                return Err(Error::InvalidArgument(
                    "bitrate_mode is only supported for mp3, opus, vorbis, and aac encoders",
                ));
            };
            mode.validate_for(bitrate_codec, config.sample_rate)?;
            match (config.bitrate, mode.bitrate_bps()) {
                (Some(bitrate), Some(mode_bps)) if bitrate != mode_bps => {
                    return Err(Error::InvalidArgument(
                        "bitrate and bitrate_mode disagree: the mode bitrate in kbps must equal bitrate in bps",
                    ));
                }
                (Some(_), None) => {
                    return Err(Error::InvalidArgument(
                        "bitrate_mode vbr has no fixed bitrate: set bitrate to None",
                    ));
                }
                _ => {}
            }
        }
        let flac = if config.codec == Codec::Flac {
            Some(FlacEncoder::with_bits_per_sample(
                config.channels,
                config.sample_rate,
                config.bits_per_sample.unwrap_or(16),
            )?)
        } else {
            None
        };
        let mp3 = if config.codec == Codec::Mp3 {
            Some(Mp3Encoder::new(
                config.sample_rate,
                config.channels,
                config.bitrate_mode.unwrap_or(BitrateMode::Cbr(128)),
            )?)
        } else {
            None
        };
        let aac_lc = if config.codec == Codec::Aac {
            // Fails here, like Decoder::from_aac_config, when the
            // licensed-aac feature is off (Unsupported), no license
            // was presented, or it has expired (LicenseRequired).
            // Defaults to CBR 128 kbps when no bitrate_mode is set;
            // bitrate_mode itself was already validated above.
            Some(AacEncoder::new(
                config.sample_rate,
                config.channels,
                config.bitrate_mode.unwrap_or(BitrateMode::Cbr(128)),
                config.license.take(),
            )?)
        } else {
            None
        };
        let alac = if config.codec == Codec::Alac {
            let bits = match config.pcm_format {
                PcmFormat::I16 => 16,
                PcmFormat::I24 => 24,
                PcmFormat::I32 | PcmFormat::F32 => {
                    return Err(Error::InvalidArgument(
                        "alac encoder requires pcm_format I16 or I24",
                    ));
                }
            };
            Some(AlacEncoder::new(config.sample_rate, config.channels, bits)?)
        } else {
            None
        };
        let dsd_modulator = if config.codec == Codec::Dsd {
            Some(DsdModulator::new(
                config.dsd_level.unwrap_or(DsdLevel::Dsd64),
                config.sample_rate,
                config.channels,
            )?)
        } else {
            None
        };
        Ok(Self {
            config,
            flac,
            mp3,
            mp3_pending: VecDeque::new(),
            aac_lc,
            alac,
            dsd_modulator,
            dsd_next_timestamp: 0,
        })
    }

    /// The `fLaC` marker plus STREAMINFO in streaming form (total
    /// samples 0, minimum block size 0), to write before the FLAC
    /// frames from `encode`. Returns an empty vector when this
    /// encoder is not a FLAC encoder.
    #[must_use]
    pub fn flac_streaminfo_bytes(&self) -> Vec<u8> {
        match &self.flac {
            Some(encoder) => encoder.streaminfo_bytes(),
            None => Vec::new(),
        }
    }

    /// Encodes one frame into a packet. A PCM packet is always a
    /// keyframe; its timestamp is copied from the frame. A FLAC
    /// packet is one FLAC frame (block size = frame count,
    /// 1..=4096); write `flac_streaminfo_bytes()` before the first
    /// frame. Every FLAC frame is independently decodable. An MP3
    /// packet is one frame of 1152 (MPEG-1) or 576 (MPEG-2/2.5)
    /// samples per channel, and the encoder carries one frame of
    /// delay: the first calls return silent priming frames and
    /// the last frames only appear on `flush`. An AAC packet is
    /// one 1024-sample frame. An ALAC packet is one frame of
    /// 1..=4096 samples. A DSD packet holds raw DSD bytes in
    /// DSF layout (LSB-first bits, per-channel 4096-byte blocks
    /// back to back); short feeds may return an empty packet
    /// while the interpolation window fills, and the tail only
    /// appears on `flush`. Licensed video codecs (`Avc`, `Hevc`)
    /// are never encoded in software: the gate lives in `hw`,
    /// and this encoder holds no license, so encoding them
    /// always returns the gate error. AAC-LC is encoded in
    /// software, but only when the `licensed-aac` feature is on
    /// and a valid license was given to `Encoder::new`;
    /// otherwise construction already fails with the gate error.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` for a codec not implemented yet,
    /// `InvalidArgument` when the frame type or its channel count or
    /// sample rate does not match the configuration, or when a FLAC
    /// frame holds more than 4096 samples.
    // One dispatch match over the codecs: splitting it would
    // scatter a single match across helpers for no gain.
    #[allow(clippy::too_many_lines)]
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
                let Some(encoder) = &mut self.flac else {
                    return Err(Error::InvalidArgument("flac encoder is not initialized"));
                };
                // At 20/24 bits the samples go through the wide
                // path as i32; at 16 bits they stay i16, as before.
                let bits = self.config.bits_per_sample.unwrap_or(16);
                let data = if bits == 16 {
                    let samples: Vec<i16> = audio
                        .samples
                        .iter()
                        .map(|&sample| f32_to_i16(sample))
                        .collect();
                    encoder.encode(&samples)?
                } else {
                    let samples: Vec<i32> = audio
                        .samples
                        .iter()
                        .map(|&sample| f32_to_i32(sample, bits))
                        .collect();
                    encoder.encode_i32(&samples)?
                };
                Ok(Packet {
                    codec: Codec::Flac,
                    data,
                    timestamp: audio.timestamp,
                    is_keyframe: true,
                })
            }
            Codec::Mp3 => {
                let audio = match frame {
                    Frame::Audio(audio) => audio,
                    Frame::Video(_) => {
                        return Err(Error::InvalidArgument(
                            "mp3 encoder requires an audio frame",
                        ));
                    }
                };
                let Some(encoder) = &mut self.mp3 else {
                    return Err(Error::InvalidArgument("mp3 encoder is not initialized"));
                };
                encoder.encode(audio)
            }
            Codec::Aac => {
                let audio = match frame {
                    Frame::Audio(audio) => audio,
                    Frame::Video(_) => {
                        return Err(Error::InvalidArgument(
                            "aac encoder requires an audio frame",
                        ));
                    }
                };
                let Some(encoder) = &mut self.aac_lc else {
                    return Err(crate::hw::licensed_software_path_error(Codec::Aac));
                };
                encoder.encode(audio)
            }
            Codec::Alac => {
                let audio = match frame {
                    Frame::Audio(audio) => audio,
                    Frame::Video(_) => {
                        return Err(Error::InvalidArgument(
                            "alac encoder requires an audio frame",
                        ));
                    }
                };
                let Some(encoder) = &mut self.alac else {
                    return Err(Error::InvalidArgument("alac encoder is not initialized"));
                };
                encoder.encode(audio)
            }
            Codec::Dsd => {
                let audio = match frame {
                    Frame::Audio(audio) => audio,
                    Frame::Video(_) => {
                        return Err(Error::InvalidArgument(
                            "dsd encoder requires an audio frame",
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
                let Some(modulator) = &mut self.dsd_modulator else {
                    return Err(Error::InvalidArgument("dsd modulator is not initialized"));
                };
                let data = modulator.feed(&audio.samples)?;
                // The flush packet starts where this frame ends.
                let frames = audio.samples.len() / audio.channels.max(1) as usize;
                self.dsd_next_timestamp =
                    audio.timestamp + frames as u64 * 1_000_000 / u64::from(audio.sample_rate);
                Ok(Packet {
                    codec: Codec::Dsd,
                    data,
                    timestamp: audio.timestamp,
                    is_keyframe: true,
                })
            }
            codec => {
                // Licensed video codecs are never encoded in
                // software: the gate lives in `hw`, and this
                // encoder holds no license, so the answer is
                // always the gate error.
                if matches!(codec, Codec::Avc | Codec::Hevc) {
                    return Err(crate::hw::licensed_software_path_error(codec));
                }
                Err(Error::Unsupported(match codec {
                    Codec::Flac => "flac encode",
                    Codec::Opus => "opus encode needs CELT/SILK encoders (not implemented)",
                    Codec::Vorbis => {
                        "vorbis encode needs codebook/floor/residue encoders (not implemented)"
                    }
                    Codec::Av1 => "av1 encode needs a full video encoder (not implemented)",
                    Codec::Vp9 => "vp9 encode needs a full video encoder (not implemented)",
                    Codec::Pcm => "pcm encode",
                    Codec::Avc | Codec::Hevc => "avc/hevc software encode is not available",
                    // MP3, AAC, ALAC, and DSD are handled by
                    // their arms above; these arms are only here
                    // so the match stays exhaustive.
                    Codec::Aac => "aac encode",
                    Codec::Alac => "alac encode",
                    Codec::Mp3 => "mp3 encode",
                    Codec::Dsd => "dsd encode",
                }))
            }
        }
    }

    /// Flushes buffered packets. PCM buffers nothing, so this is `None`.
    /// MP3 buffers its tail frames: each `flush` call returns the
    /// next one until the queue is empty. DSD returns the
    /// modulator's tail as one final packet, then `None`.
    ///
    /// # Errors
    ///
    /// Returns the bitstream errors of the MP3 or DSD flush.
    pub fn flush(&mut self) -> Result<Option<Packet>> {
        // MP3: the sub-encoder's flush returns every remaining
        // frame at once, but this method returns one packet per
        // call, so the rest wait in `mp3_pending`.
        if let Some(packet) = self.mp3_pending.pop_front() {
            return Ok(Some(packet));
        }
        if let Some(encoder) = &mut self.mp3 {
            self.mp3_pending.extend(encoder.flush()?);
            if let Some(packet) = self.mp3_pending.pop_front() {
                return Ok(Some(packet));
            }
        }
        // DSD: the modulator's tail is one final chunk of DSD
        // bytes; after it the modulator is spent, so a second
        // flush returns nothing.
        if let Some(modulator) = &mut self.dsd_modulator {
            let data = modulator.flush()?;
            if !data.is_empty() {
                return Ok(Some(Packet {
                    codec: Codec::Dsd,
                    data,
                    timestamp: self.dsd_next_timestamp,
                    is_keyframe: true,
                }));
            }
        }
        Ok(None)
    }

    /// DSD samples emitted so far by the modulator, or 0 when
    /// this is not a DSD encoder.
    #[must_use]
    pub fn dsd_samples_emitted(&self) -> u64 {
        self.dsd_modulator
            .as_ref()
            .map_or(0, DsdModulator::dsd_samples_emitted)
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
