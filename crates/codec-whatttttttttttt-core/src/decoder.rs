//! Frame decoder.
//!
//! Phase 1 decodes `Codec::Pcm`; Phase 2 adds `Codec::Flac`.
//! Phase 4 adds `Codec::Opus` (CELT silence frames as silence;
//! CELT sample reconstruction and SILK return `Unsupported`
//! naming the unimplemented part) and Phase 5 adds
//! `Codec::Vorbis`. Phase 7 adds `Codec::Av1` as parser-only:
//! `Decoder::from_av1_sequence_header` validates the subset and
//! `decode` returns `Unsupported` for reconstruction until tile
//! decoding exists with passing tests. Phase 8 adds `Codec::Vp9`
//! the same way: `Decoder::from_vp9_frame_header` validates the
//! subset (profile 0, 8-bit, YUV 4:2:0, no RGB) and `decode`
//! returns `Unsupported` until tile decoding exists. Every other
//! codec returns `Unsupported` naming the codec, so a caller can
//! tell "not built yet" apart from "corrupt input". PCM decoding
//! needs a format: it comes from a WAV header via
//! `Decoder::from_wav_header`, or from `DecoderConfig` fields set
//! by the caller. FLAC decoding needs a STREAMINFO block: it comes
//! from `Decoder::from_flac_streaminfo`.
//! Opus decoding needs an `OpusHead`: it comes from
//! `Decoder::from_opus_head`. Vorbis decoding needs all three
//! headers: they come from `Decoder::from_vorbis_setup`.
//! The design document (section 4.1) lists only limit fields in
//! `DecoderConfig`; the three `pcm_*` fields below are the Phase 1
//! extension that makes PCM decoding possible, recorded in
//! `docs/DESIGN.md` section 12.

use crate::av1_frame::Av1ReferenceState;
use crate::av1_sequence::SequenceHeader;
use crate::celt::{conceal_loss as celt_conceal_loss, decode_celt_frame_header};
use crate::error::{Error, Result};
use crate::flac_frame::{decode_flac_frame, StreamInfo};
use crate::frame::{AudioFrame, Codec, Frame, Metadata, Packet};
use crate::limits::{
    MAX_AUDIO_PACKET_BYTES, MAX_CHANNELS, MAX_HEIGHT, MAX_SAMPLE_RATE, MAX_VIDEO_PACKET_BYTES,
    MAX_WIDTH,
};
use crate::opus_packet::{parse_opus_packet, OpusHead, OpusMode};
use crate::pcm::{decode_samples, PcmFormat};
use crate::vorbis_packet::VorbisPacketDecoder;
use crate::vorbis_setup::VorbisSetup;
use crate::vp9_bitstream::{Vp9FrameHeader, Vp9ReferenceState};
use crate::wav_header::WavHeader;

/// Opus is always decoded at this internal sample rate (RFC 6716
/// section 2). The output gain and pre-skip are in 48 kHz samples.
const OPUS_INTERNAL_SAMPLE_RATE: u32 = 48_000;

/// Configuration for a `Decoder`. Limits are validated in `Decoder::new`.
#[derive(Debug, Clone)]
pub struct DecoderConfig {
    /// Codec to decode.
    pub codec: Codec,
    /// Largest audio packet accepted, in bytes.
    pub max_frame_bytes: usize,
    /// Largest channel count accepted.
    pub max_channels: u32,
    /// Largest sample rate accepted, in Hz.
    pub max_sample_rate: u32,
    /// Largest video width accepted, in pixels.
    pub max_width: u32,
    /// Largest video height accepted, in pixels.
    pub max_height: u32,
    /// PCM sample format, required when `codec` is `Pcm`.
    pub pcm_format: Option<PcmFormat>,
    /// PCM channel count, required when `codec` is `Pcm`.
    pub pcm_channels: Option<u32>,
    /// PCM sample rate in Hz, required when `codec` is `Pcm`.
    pub pcm_sample_rate: Option<u32>,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self {
            codec: Codec::Pcm,
            max_frame_bytes: MAX_AUDIO_PACKET_BYTES,
            max_channels: MAX_CHANNELS,
            max_sample_rate: MAX_SAMPLE_RATE,
            max_width: MAX_WIDTH,
            max_height: MAX_HEIGHT,
            pcm_format: None,
            pcm_channels: None,
            pcm_sample_rate: None,
        }
    }
}

/// Stateful decoder. A `Decoder` is `Send` but not `Sync`: use one
/// instance from one thread at a time, or guard it with a `Mutex`.
pub struct Decoder {
    config: DecoderConfig,
    metadata: Metadata,
    streaminfo: Option<StreamInfo>,
    opus_head: Option<OpusHead>,
    /// Opus pre-skip samples still to drop from the stream start.
    opus_pre_skip_remaining: usize,
    /// AV1 sequence header, when the decoder was built from one.
    av1_sequence: Option<SequenceHeader>,
    /// AV1 reference state, carried across frames.
    av1_state: Av1ReferenceState,
    /// VP9 frame header, when the decoder was built from one.
    vp9_header: Option<Vp9FrameHeader>,
    /// VP9 reference state, carried across frames.
    vp9_state: Vp9ReferenceState,
    /// Duration of one frame of the last decoded Opus packet, in
    /// 48 kHz samples; 0 before the first packet.
    opus_last_frame_duration_samples: u32,
    /// Granule position (48 kHz samples) at the end of the last
    /// decoded Opus packet; 0 before the first packet.
    opus_last_granule: u64,
    /// Granule position (samples) at the end of the last decoded
    /// Vorbis packet; 0 before the first packet.
    vorbis_last_granule: u64,
    vorbis: Option<VorbisPacketDecoder>,
}

impl Decoder {
    /// Creates a decoder, validating the configuration limits.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when a limit is zero or inconsistent,
    /// and `Overflow` when a limit exceeds the hard limit in `limits.rs`.
    pub fn new(config: DecoderConfig) -> Result<Self> {
        if config.max_frame_bytes == 0 || config.max_frame_bytes > MAX_VIDEO_PACKET_BYTES {
            return Err(Error::InvalidArgument(
                "max_frame_bytes must be positive and within the video packet limit",
            ));
        }
        if config.max_channels == 0 || config.max_channels > MAX_CHANNELS {
            return Err(Error::Overflow("max_channels exceeds maximum"));
        }
        if config.max_sample_rate == 0 || config.max_sample_rate > MAX_SAMPLE_RATE {
            return Err(Error::Overflow("max_sample_rate exceeds maximum"));
        }
        if config.max_width == 0 || config.max_width > MAX_WIDTH {
            return Err(Error::Overflow("max_width exceeds maximum"));
        }
        if config.max_height == 0 || config.max_height > MAX_HEIGHT {
            return Err(Error::Overflow("max_height exceeds maximum"));
        }
        if let Some(channels) = config.pcm_channels {
            if channels == 0 || channels > config.max_channels {
                return Err(Error::InvalidArgument("pcm_channels out of range"));
            }
        }
        if let Some(rate) = config.pcm_sample_rate {
            if rate == 0 || rate > config.max_sample_rate {
                return Err(Error::InvalidArgument("pcm_sample_rate out of range"));
            }
        }
        Ok(Self {
            config,
            metadata: Metadata::new(),
            streaminfo: None,
            opus_head: None,
            opus_pre_skip_remaining: 0,
            av1_sequence: None,
            av1_state: Av1ReferenceState::default(),
            vp9_header: None,
            vp9_state: Vp9ReferenceState::default(),
            opus_last_frame_duration_samples: 0,
            opus_last_granule: 0,
            vorbis_last_granule: 0,
            vorbis: None,
        })
    }

    /// Creates a PCM decoder from a parsed WAV header.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as `Decoder::new`.
    pub fn from_wav_header(header: &WavHeader) -> Result<Self> {
        Self::new(DecoderConfig {
            codec: Codec::Pcm,
            pcm_format: Some(header.format),
            pcm_channels: Some(header.channels),
            pcm_sample_rate: Some(header.sample_rate),
            ..DecoderConfig::default()
        })
    }

    /// Creates a FLAC decoder from a parsed STREAMINFO block.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as `Decoder::new`.
    pub fn from_flac_streaminfo(streaminfo: &StreamInfo) -> Result<Self> {
        let mut decoder = Self::new(DecoderConfig {
            codec: Codec::Flac,
            ..DecoderConfig::default()
        })?;
        decoder.streaminfo = Some(*streaminfo);
        Ok(decoder)
    }

    /// Creates an Opus decoder from a parsed `OpusHead` (RFC 7845
    /// section 5.1). Phase 4 supports channel mapping family 0
    /// (mono/stereo) only; families 1 and 255 return `Unsupported`.
    /// The decoder outputs at the 48 kHz internal rate; use
    /// `resample` to convert to the input sample rate.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as `Decoder::new`, and
    /// `Unsupported` for a channel mapping family above 0.
    pub fn from_opus_head(head: &OpusHead) -> Result<Self> {
        if head.channel_mapping_family != 0 {
            return Err(Error::Unsupported(
                "opus channel mapping family 1 and 255 are not decoded in phase 4",
            ));
        }
        let mut decoder = Self::new(DecoderConfig {
            codec: Codec::Opus,
            ..DecoderConfig::default()
        })?;
        if u32::from(head.channels) > decoder.config.max_channels {
            return Err(Error::InvalidArgument(
                "opus channel count exceeds decoder limit",
            ));
        }
        decoder.opus_head = Some(head.clone());
        decoder.opus_pre_skip_remaining = usize::from(head.pre_skip);
        Ok(decoder)
    }

    /// Creates a Vorbis decoder from a parsed setup (all three
    /// headers). The first packet primes the decoder and produces
    /// no output; see `decode`.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as `Decoder::new`.
    pub fn from_vorbis_setup(setup: VorbisSetup) -> Result<Self> {
        let mut decoder = Self::new(DecoderConfig {
            codec: Codec::Vorbis,
            ..DecoderConfig::default()
        })?;
        if setup.identification.channels > decoder.config.max_channels {
            return Err(Error::InvalidArgument(
                "vorbis channel count exceeds decoder limit",
            ));
        }
        if setup.identification.sample_rate > decoder.config.max_sample_rate {
            return Err(Error::InvalidArgument(
                "vorbis sample rate exceeds decoder limit",
            ));
        }
        decoder.vorbis = Some(VorbisPacketDecoder::new(setup));
        Ok(decoder)
    }

    /// Creates an AV1 decoder from a parsed sequence header. Phase
    /// 7 is parser-only: the header is validated against the subset
    /// (profile 0, 8-bit, YUV 4:2:0, no film grain) and `decode`
    /// returns `Unsupported` for reconstruction until tile decoding
    /// exists with passing tests.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as `Decoder::new`, and
    /// `Unsupported` or `Overflow` when the sequence header is
    /// outside the Phase 7 subset.
    pub fn from_av1_sequence_header(header: &SequenceHeader) -> Result<Self> {
        header.validate_subset()?;
        let mut decoder = Self::new(DecoderConfig {
            codec: Codec::Av1,
            max_frame_bytes: MAX_VIDEO_PACKET_BYTES,
            ..DecoderConfig::default()
        })?;
        decoder.av1_sequence = Some(header.clone());
        Ok(decoder)
    }

    /// Creates a VP9 decoder from a parsed frame header. Phase 8
    /// is parser-only: the header is validated against the subset
    /// (profile 0, 8-bit, YUV 4:2:0, no RGB) and `decode` returns
    /// `Unsupported` for reconstruction until tile decoding exists
    /// with passing tests.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as `Decoder::new`, and
    /// `Unsupported` or `InvalidArgument` when the frame header is
    /// outside the Phase 8 subset.
    pub fn from_vp9_frame_header(header: &Vp9FrameHeader) -> Result<Self> {
        header.validate_subset()?;
        let mut decoder = Self::new(DecoderConfig {
            codec: Codec::Vp9,
            max_frame_bytes: MAX_VIDEO_PACKET_BYTES,
            ..DecoderConfig::default()
        })?;
        decoder.vp9_header = Some(header.clone());
        Ok(decoder)
    }

    /// Detects the codec from container magic bytes. Ogg cannot be
    /// identified from its magic alone — the codec lives in the first
    /// packet — so `OggS` returns `Unsupported` and the caller must
    /// use `OggDemuxer` instead. WebM/Matroska (EBML magic) and MP4
    /// (`ftyp`) are containers too: they return `Unsupported` and
    /// the caller must use `Demuxer`. IVF (`DKIF`) identifies AV1
    /// and VP9 from its fourcc.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when fewer than 4 bytes are given, and
    /// `Unsupported` when the magic matches no known container, or
    /// when the container needs a demuxer.
    pub fn probe(data: &[u8]) -> Result<Codec> {
        if data.len() < 4 {
            return Err(Error::Truncated {
                needed: 4,
                had: data.len(),
            });
        }
        if &data[0..4] == b"RIFF" {
            return Ok(Codec::Pcm);
        }
        if &data[0..4] == b"fLaC" {
            return Ok(Codec::Flac);
        }
        if &data[0..4] == b"OggS" {
            return Err(Error::Unsupported(
                "ogg container: use OggDemuxer to identify the codec",
            ));
        }
        if &data[0..4] == b"\x1A\x45\xDF\xA3" {
            return Err(Error::Unsupported(
                "webm/matroska container: use Demuxer to identify the codec",
            ));
        }
        if data.len() >= 8 && &data[4..8] == b"ftyp" {
            return Err(Error::Unsupported(
                "mp4 container: use Demuxer to identify the codec",
            ));
        }
        if &data[0..4] == b"DKIF" {
            if data.len() < 12 {
                return Err(Error::Truncated {
                    needed: 12,
                    had: data.len(),
                });
            }
            if &data[8..12] == b"AV01" {
                return Ok(Codec::Av1);
            }
            if &data[8..12] == b"VP90" {
                return Ok(Codec::Vp9);
            }
            return Err(Error::Unsupported("unknown ivf fourcc"));
        }
        Err(Error::Unsupported("unknown container signature"))
    }

    /// Decodes one packet into a frame. The decoder holds no frame
    /// history for PCM or FLAC, so every call is independent. A FLAC
    /// packet must hold exactly one frame, as produced by
    /// `FlacDemuxer` or `OggDemuxer`. An Opus packet may hold several
    /// frames; they are concatenated into one frame, and the
    /// pre-skip is dropped from the stream start. A Vorbis packet
    /// produces one overlap-add output, except the first packet,
    /// which primes the decoder and returns an empty frame.
    ///
    /// Packet timestamps are granule positions in samples (the Ogg
    /// convention, documented in `ogg_stream.rs`); the decoder
    /// converts them to microseconds at the codec sample rate.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` for a codec not implemented yet (SILK
    /// Opus, CELT sample reconstruction, Vorbis floor type 0),
    /// `AllocationLimit` when the packet exceeds `max_frame_bytes`,
    /// `InvalidArgument` when a PCM packet arrives without a
    /// configured format, a FLAC packet arrives without a STREAMINFO
    /// block, an Opus packet arrives without an `OpusHead`, or a
    /// Vorbis packet arrives without a setup, and `InvalidData` for
    /// frame-unaligned PCM bytes, a corrupt FLAC frame, a malformed
    /// Opus packet, or a corrupt Vorbis packet.
    pub fn decode(&mut self, packet: &Packet) -> Result<Frame> {
        if packet.codec != self.config.codec {
            return Err(Error::InvalidArgument(
                "packet codec does not match decoder codec",
            ));
        }
        match self.config.codec {
            Codec::Pcm => {
                if packet.data.len() > self.config.max_frame_bytes {
                    return Err(Error::AllocationLimit {
                        requested: packet.data.len(),
                        limit: self.config.max_frame_bytes,
                    });
                }
                let format = self.config.pcm_format.ok_or(Error::InvalidArgument(
                    "pcm format not set: construct the decoder from a wav header",
                ))?;
                let channels = self.config.pcm_channels.ok_or(Error::InvalidArgument(
                    "pcm channels not set: construct the decoder from a wav header",
                ))?;
                let sample_rate = self.config.pcm_sample_rate.ok_or(Error::InvalidArgument(
                    "pcm sample rate not set: construct the decoder from a wav header",
                ))?;
                let samples = decode_samples(&packet.data, format, channels)?;
                Ok(Frame::Audio(AudioFrame {
                    sample_rate,
                    channels,
                    samples,
                    timestamp: packet.timestamp,
                }))
            }
            Codec::Flac => {
                if packet.data.len() > self.config.max_frame_bytes {
                    return Err(Error::AllocationLimit {
                        requested: packet.data.len(),
                        limit: self.config.max_frame_bytes,
                    });
                }
                let streaminfo = self.streaminfo.ok_or(Error::InvalidArgument(
                    "flac streaminfo not set: construct the decoder from a streaminfo block",
                ))?;
                let (mut audio, _) = decode_flac_frame(&packet.data, &streaminfo)?;
                audio.timestamp = packet.timestamp;
                Ok(Frame::Audio(audio))
            }
            Codec::Opus => self.decode_opus(packet),
            Codec::Vorbis => self.decode_vorbis(packet),
            Codec::Av1 => Err(Error::Unsupported(
                "av1 reconstruction is not implemented in phase 7 (parser-only; use parse_frame_obu for the header and tile framing)",
            )),
            Codec::Vp9 => Err(Error::Unsupported(
                "vp9 reconstruction is not implemented in phase 8 (parser-only; use parse_vp9_frame for the header and tile framing)",
            )),
        }
    }

    /// Decodes one Opus packet (RFC 6716, RFC 7845). Phase 4 decodes
    /// CELT silence frames as silence and returns `Unsupported`
    /// naming the unimplemented part for everything else (CELT
    /// non-silence sample reconstruction, SILK/Hybrid modes).
    fn decode_opus(&mut self, packet: &Packet) -> Result<Frame> {
        if packet.data.len() > self.config.max_frame_bytes {
            return Err(Error::AllocationLimit {
                requested: packet.data.len(),
                limit: self.config.max_frame_bytes,
            });
        }
        let (channels, output_gain_db) = {
            let head = self.opus_head.as_ref().ok_or(Error::InvalidArgument(
                "opus head not set: construct the decoder from an opus head",
            ))?;
            (u32::from(head.channels), head.output_gain_db())
        };
        let opus_packet = parse_opus_packet(&packet.data)?;
        let packet_channels = opus_packet.channels as usize;
        if packet_channels != channels as usize {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "opus packet channel count does not match opus head",
            });
        }
        let frame_duration = opus_packet.config.frame_duration_samples_48k as usize;
        let mut samples: Vec<f32> = Vec::new();
        for frame in &opus_packet.frames {
            if frame.is_empty() {
                // A zero-length frame is a lost frame (RFC 7845
                // section 3 allows zero-byte frames in any code);
                // conceal it with silence of one frame duration.
                samples.extend_from_slice(&celt_conceal_loss(frame_duration, packet_channels));
                continue;
            }
            match opus_packet.config.mode {
                OpusMode::Silk | OpusMode::Hybrid => {
                    return Err(Error::Unsupported(
                        "opus silk decode (silk/hybrid mode not implemented in phase 4)",
                    ));
                }
                OpusMode::Celt => {
                    let header = decode_celt_frame_header(frame)?;
                    if header.silence {
                        samples
                            .extend_from_slice(&celt_conceal_loss(frame_duration, packet_channels));
                    } else {
                        return Err(Error::Unsupported(
                            "opus celt sample decode (celt sample reconstruction not implemented in phase 4; frame header decoded)",
                        ));
                    }
                }
            }
        }
        // Pre-skip: drop the leading samples of the stream, in
        // 48 kHz samples per channel, across packets until it runs
        // out.
        let total_frames = samples.len() / packet_channels.max(1);
        let drop_frames = self.opus_pre_skip_remaining.min(total_frames);
        if drop_frames > 0 {
            samples.drain(..drop_frames * packet_channels);
            self.opus_pre_skip_remaining -= drop_frames;
        }
        // Output gain (Q7.8 dB) from the OpusHead, applied to the
        // decoded samples. Silence is unaffected.
        if output_gain_db != 0.0 {
            let gain = 10f32.powf(output_gain_db / 20.0);
            for sample in &mut samples {
                *sample *= gain;
            }
        }
        self.opus_last_frame_duration_samples = opus_packet.config.frame_duration_samples_48k;
        self.opus_last_granule = packet.timestamp;
        // The packet timestamp is the granule position (48 kHz
        // samples) at the end of the packet; the frame starts one
        // packet duration earlier.
        let start = packet
            .timestamp
            .saturating_sub(u64::from(opus_packet.total_duration_samples_48k()));
        let timestamp_us = start * 1_000_000 / u64::from(OPUS_INTERNAL_SAMPLE_RATE);
        Ok(Frame::Audio(AudioFrame {
            sample_rate: OPUS_INTERNAL_SAMPLE_RATE,
            channels,
            samples,
            timestamp: timestamp_us,
        }))
    }

    /// Decodes one Vorbis packet. The first packet primes the
    /// decoder and returns an empty frame (section 4.3.8).
    fn decode_vorbis(&mut self, packet: &Packet) -> Result<Frame> {
        if packet.data.len() > self.config.max_frame_bytes {
            return Err(Error::AllocationLimit {
                requested: packet.data.len(),
                limit: self.config.max_frame_bytes,
            });
        }
        let vorbis = self.vorbis.as_mut().ok_or(Error::InvalidArgument(
            "vorbis setup not set: construct the decoder from a vorbis setup",
        ))?;
        let decoded = vorbis.decode_packet(&packet.data)?;
        self.vorbis_last_granule = packet.timestamp;
        let (channels, sample_rate) = {
            let setup = vorbis.setup();
            (
                setup.identification.channels,
                setup.identification.sample_rate,
            )
        };
        match decoded {
            None => Ok(Frame::Audio(AudioFrame {
                sample_rate,
                channels,
                samples: Vec::new(),
                timestamp: 0,
            })),
            Some(decoded_packet) => {
                // The output starts at the previous block's
                // midpoint: granule - current blocksize +
                // (prev - cur) / 4. The arithmetic is in `i128` so
                // the granule (a `u64`) and the possibly-negative
                // `(previous - blocksize) / 4` cannot wrap.
                let granule = i128::from(packet.timestamp);
                let blocksize = i128::try_from(decoded_packet.blocksize).unwrap_or(0);
                let previous = i128::try_from(decoded_packet.previous_blocksize).unwrap_or(0);
                let start = (granule - blocksize + (previous - blocksize) / 4).max(0);
                // `start` is non-negative and at most the granule,
                // so it fits in `u64`.
                let timestamp_us =
                    u64::try_from((start * 1_000_000 / i128::from(sample_rate)).max(0))
                        .unwrap_or(u64::MAX);
                Ok(Frame::Audio(AudioFrame {
                    sample_rate,
                    channels,
                    samples: decoded_packet.samples,
                    timestamp: timestamp_us,
                }))
            }
        }
    }

    /// Returns a concealment frame for a lost packet (packet loss
    /// concealment, PLC), or `None` when no concealment is
    /// available. This is an explicit API: the caller decides a
    /// packet is lost (for example, a gap in Ogg sequence numbers)
    /// and calls this instead of `decode`.
    ///
    /// Opus returns a silence frame of the last frame's duration
    /// (RFC 6716 section 4.4 allows silence as a simple PLC; the
    /// reference CELT PLC repeats the previous period and the SILK
    /// PLC extrapolates LPC, neither of which is implemented). It
    /// returns `None` before the first packet, when no frame
    /// duration is known. Vorbis has no PLC: a lost packet is
    /// skipped, and the next packet's overlap-add uses the previous
    /// block's tail, so this returns `None` for Vorbis and every
    /// other codec.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the Opus decoder was not
    /// constructed from an `OpusHead`.
    pub fn conceal_loss(&mut self) -> Result<Option<Frame>> {
        match self.config.codec {
            Codec::Opus => {
                if self.opus_last_frame_duration_samples == 0 {
                    return Ok(None);
                }
                let channels = {
                    let head = self.opus_head.as_ref().ok_or(Error::InvalidArgument(
                        "opus head not set: construct the decoder from an opus head",
                    ))?;
                    u32::from(head.channels)
                };
                let samples = celt_conceal_loss(
                    self.opus_last_frame_duration_samples as usize,
                    channels as usize,
                );
                let timestamp_us =
                    self.opus_last_granule * 1_000_000 / u64::from(OPUS_INTERNAL_SAMPLE_RATE);
                // Advance the stream position past the concealed
                // frame so consecutive losses get consecutive
                // timestamps.
                self.opus_last_granule += u64::from(self.opus_last_frame_duration_samples);
                Ok(Some(Frame::Audio(AudioFrame {
                    sample_rate: OPUS_INTERNAL_SAMPLE_RATE,
                    channels,
                    samples,
                    timestamp: timestamp_us,
                })))
            }
            _ => Ok(None),
        }
    }

    /// Flushes buffered frames. PCM and FLAC buffer nothing, so
    /// this is `None` for them. Opus buffers nothing either. Vorbis
    /// buffers the second half of the last block; `flush` returns
    /// it once, then `None`.
    ///
    /// # Errors
    ///
    /// This implementation never fails; the `Result` exists so later
    /// codecs with delay can report errors here.
    pub fn flush(&mut self) -> Result<Option<Frame>> {
        if self.config.codec != Codec::Vorbis {
            return Ok(None);
        }
        let Some(vorbis) = self.vorbis.as_mut() else {
            return Ok(None);
        };
        let Some(tail) = vorbis.flush() else {
            return Ok(None);
        };
        // The tail starts half a block before the last granule.
        let tail_frames = tail.samples.len() / tail.channels.max(1) as usize;
        let start = self.vorbis_last_granule.saturating_sub(tail_frames as u64);
        let timestamp_us = start * 1_000_000 / u64::from(tail.sample_rate);
        Ok(Some(Frame::Audio(AudioFrame {
            sample_rate: tail.sample_rate,
            channels: tail.channels,
            samples: tail.samples,
            timestamp: timestamp_us,
        })))
    }

    /// Metadata read at open time. Container metadata blocks are
    /// parsed by the demuxer and are not copied here; use
    /// `FlacDemuxer::metadata` for FLAC Vorbis comments.
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// AV1 reference state, carried across frames. Exposed so a
    /// caller parsing frame headers with `parse_frame_obu` can share
    /// the decoder's state instead of keeping its own.
    #[must_use]
    pub fn av1_state_mut(&mut self) -> &mut Av1ReferenceState {
        &mut self.av1_state
    }

    /// VP9 reference state, carried across frames. Exposed so a
    /// caller parsing frame headers with `parse_vp9_frame` can
    /// share the decoder's state instead of keeping its own.
    #[must_use]
    pub fn vp9_state_mut(&mut self) -> &mut Vp9ReferenceState {
        &mut self.vp9_state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcm::encode_samples;

    fn pcm_packet(samples: &[f32]) -> Packet {
        Packet {
            codec: Codec::Pcm,
            data: encode_samples(samples, PcmFormat::I16, 1).unwrap(),
            timestamp: 0,
            is_keyframe: true,
        }
    }

    #[test]
    fn decodes_pcm_packet_to_audio_frame() {
        let mut decoder = Decoder::new(DecoderConfig {
            pcm_format: Some(PcmFormat::I16),
            pcm_channels: Some(1),
            pcm_sample_rate: Some(8000),
            ..DecoderConfig::default()
        })
        .unwrap();
        let frame = decoder.decode(&pcm_packet(&[0.5, -0.5])).unwrap();
        match frame {
            Frame::Audio(audio) => {
                assert_eq!(audio.sample_rate, 8000);
                assert_eq!(audio.channels, 1);
                assert_eq!(audio.samples.len(), 2);
            }
            Frame::Video(_) => panic!("expected an audio frame"),
        }
    }

    #[test]
    fn flac_decode_without_streaminfo_returns_invalid_argument() {
        let mut decoder = Decoder::new(DecoderConfig {
            codec: Codec::Flac,
            ..DecoderConfig::default()
        })
        .unwrap();
        let packet = Packet {
            codec: Codec::Flac,
            data: b"fLaC".to_vec(),
            timestamp: 0,
            is_keyframe: true,
        };
        let err = decoder.decode(&packet).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    fn probe_ogg_returns_unsupported() {
        let err = Decoder::probe(b"OggSxxxx").unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn packet_codec_mismatch_returns_invalid_argument() {
        let mut decoder = Decoder::new(DecoderConfig {
            pcm_format: Some(PcmFormat::I16),
            pcm_channels: Some(1),
            pcm_sample_rate: Some(8000),
            ..DecoderConfig::default()
        })
        .unwrap();
        let packet = Packet {
            codec: Codec::Flac,
            data: Vec::new(),
            timestamp: 0,
            is_keyframe: true,
        };
        let err = decoder.decode(&packet).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    fn probe_detects_wav_and_flac_magic() {
        assert_eq!(Decoder::probe(b"RIFFxxxx").unwrap(), Codec::Pcm);
        assert_eq!(Decoder::probe(b"fLaCxxxx").unwrap(), Codec::Flac);
        assert!(matches!(
            Decoder::probe(b"XXXX"),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn probe_detects_ebml_ftyp_and_ivf() {
        assert!(matches!(
            Decoder::probe(b"\x1A\x45\xDF\xA3xxxx"),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            Decoder::probe(b"xxxxftyp"),
            Err(Error::Unsupported(_))
        ));
        let mut ivf_av1 = b"DKIF".to_vec();
        ivf_av1.extend([0u8; 4]);
        ivf_av1.extend(b"AV01");
        assert_eq!(Decoder::probe(&ivf_av1).unwrap(), Codec::Av1);
        let mut ivf_vp9 = b"DKIF".to_vec();
        ivf_vp9.extend([0u8; 4]);
        ivf_vp9.extend(b"VP90");
        assert_eq!(Decoder::probe(&ivf_vp9).unwrap(), Codec::Vp9);
        assert!(matches!(
            Decoder::probe(b"DKIFxxxx"),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn av1_decoder_rejects_reconstruction_as_unsupported() {
        use crate::av1_sequence::parse_sequence_header;
        let seq_bytes = [0x00u8, 0x00, 0x00, 0x02, 0xAF, 0xFF, 0x9B, 0x5F, 0x20, 0x08];
        let seq = parse_sequence_header(&seq_bytes).unwrap();
        let mut decoder = Decoder::from_av1_sequence_header(&seq).unwrap();
        let packet = Packet {
            codec: Codec::Av1,
            data: vec![0u8; 10],
            timestamp: 0,
            is_keyframe: true,
        };
        let err = decoder.decode(&packet).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn av1_decoder_rejects_out_of_subset_sequence() {
        use crate::av1_sequence::parse_sequence_header;
        let seq_bytes = [0x00u8, 0x00, 0x00, 0x02, 0xAF, 0xFF, 0x9B, 0x5F, 0x20, 0x08];
        let mut seq = parse_sequence_header(&seq_bytes).unwrap();
        seq.profile = 1;
        assert!(matches!(
            Decoder::from_av1_sequence_header(&seq),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn vp9_decoder_rejects_reconstruction_as_unsupported() {
        let mut decoder = Decoder::from_vp9_frame_header(&Vp9FrameHeader::default()).unwrap();
        let packet = Packet {
            codec: Codec::Vp9,
            data: vec![0u8; 10],
            timestamp: 0,
            is_keyframe: true,
        };
        let err = decoder.decode(&packet).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn vp9_decoder_rejects_out_of_subset_header() {
        let header = Vp9FrameHeader {
            profile: 1,
            ..Vp9FrameHeader::default()
        };
        assert!(matches!(
            Decoder::from_vp9_frame_header(&header),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn probe_short_input_returns_truncated() {
        let err = Decoder::probe(b"RI").unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn oversized_packet_returns_allocation_limit() {
        let mut decoder = Decoder::new(DecoderConfig {
            max_frame_bytes: 4,
            pcm_format: Some(PcmFormat::I16),
            pcm_channels: Some(1),
            pcm_sample_rate: Some(8000),
            ..DecoderConfig::default()
        })
        .unwrap();
        let packet = Packet {
            codec: Codec::Pcm,
            data: vec![0u8; 8],
            timestamp: 0,
            is_keyframe: true,
        };
        let err = decoder.decode(&packet).unwrap_err();
        assert!(matches!(err, Error::AllocationLimit { .. }));
    }
}
