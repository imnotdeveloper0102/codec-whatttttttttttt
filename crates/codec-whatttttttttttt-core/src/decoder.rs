//! Frame decoder.
//!
//! Phase 1 decodes `Codec::Pcm`; Phase 2 adds `Codec::Flac`.
//! Phase 4 adds `Codec::Opus` (CELT silence frames as silence;
//! SILK returns `Unsupported` naming the unimplemented part)
//! and Phase 5 adds `Codec::Vorbis`. Phase 7 adds `Codec::Av1`
//! as parser-only; the current round wires tile decode and
//! intra reconstruction, which today returns `Unsupported`
//! naming the missing coefficient CDF tables, so no pixels
//! exist yet. Phase 8 adds `Codec::Vp9` the same way; the
//! current round wires reconstruction, which decodes keyframes
//! bit-exactly and returns `Unsupported` for inter frames
//! until motion compensation is pixel-correct. The current
//! round adds `Codec::Alac` (`Decoder::from_alac_config`, no
//! license needed) and `Codec::Aac` (`Decoder::from_aac_config`,
//! licensed-aac feature plus a validated `LicenseContext`).
//! Every other
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

use crate::aac_frame::{AacConfig, AacDecoder};
use crate::alac_frame::{decode_alac_audio_frame, AlacConfig};
use crate::av1_frame::Av1ReferenceState;
use crate::av1_obu::{ObuReader, ObuType};
use crate::av1_reconstruct::reconstruct_av1_frame_obu;
use crate::av1_sequence::{parse_sequence_header, SequenceHeader};
use crate::celt::{conceal_loss as celt_conceal_loss, decode_celt_frame_header};
use crate::celt_reconstruct::{celt_end_band, CeltDecoder};
use crate::dsd_decimator::DsdDecimator;
use crate::dsd_stream::DsdHeader;
use crate::error::{Error, Result};
use crate::flac_frame::{decode_flac_frame, StreamInfo};
use crate::frame::{AudioFrame, Codec, Frame, Metadata, Packet, VideoFrame};
use crate::license::LicenseContext;
use crate::limits::{
    MAX_AUDIO_PACKET_BYTES, MAX_CHANNELS, MAX_HEIGHT, MAX_SAMPLE_RATE, MAX_VIDEO_PACKET_BYTES,
    MAX_WIDTH,
};
use crate::mp3_frame::Mp3Decoder;
use crate::opus_packet::{parse_opus_packet, OpusHead, OpusMode};
use crate::pcm::{decode_samples, PcmFormat};
use crate::vorbis_packet::VorbisPacketDecoder;
use crate::vorbis_setup::VorbisSetup;
use crate::vp9_bitstream::{parse_superframe, parse_vp9_frame, Vp9FrameHeader, Vp9ReferenceState};
use crate::vp9_reconstruct::{reconstruct_vp9_frame, Vp9ReconstructionState};
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
    /// DSD output rate in Hz, optional when `codec` is `Dsd`.
    /// When `None`, `from_dsd_header` uses the DSD rate / 16
    /// (176.4 kHz for DSD64). Must be a 44.1 kHz-family rate
    /// (44.1/88.2/176.4/352.8/705.6 kHz) or a 48 kHz-family
    /// rate (48/96/192 kHz); the decimator validates it.
    pub dsd_output_rate: Option<u32>,
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
            dsd_output_rate: None,
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
    /// VP9 reconstruction state, carried across frames (saved
    /// probability contexts, reference frame store, previous
    /// mode maps). Owned by the VP9 pipeline in `vp9_reconstruct`.
    vp9_recon_state: Vp9ReconstructionState,
    /// Stateful CELT decoder, built in `from_opus_head` once
    /// the channel count is known; carried across packets.
    celt: Option<CeltDecoder>,
    /// DSD header, when the decoder was built from one.
    dsd_header: Option<DsdHeader>,
    /// DSD decimator, holding the filter state across packets.
    dsd_decimator: Option<DsdDecimator>,
    /// ALAC magic cookie, when the decoder was built from one.
    alac_config: Option<AlacConfig>,
    /// AAC configuration, when the decoder was built from one.
    aac_config: Option<AacConfig>,
    /// Stateful AAC spectral decoder (overlap buffers and
    /// previous window shapes), built with the config above.
    aac: Option<AacDecoder>,
    /// True when an AAC decoder was built with a validated
    /// license (see `from_aac_config`); `decode` refuses AAC
    /// packets otherwise.
    aac_licensed: bool,
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
    /// MP3 Layer III state (bit reservoir, IMDCT overlap,
    /// polyphase history), created lazily on the first frame:
    /// MP3 is self-describing, so no setup is needed.
    mp3: Option<Mp3Decoder>,
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
            vp9_recon_state: Vp9ReconstructionState::new(),
            celt: None,
            opus_last_frame_duration_samples: 0,
            opus_last_granule: 0,
            vorbis_last_granule: 0,
            vorbis: None,
            mp3: None,
            dsd_header: None,
            dsd_decimator: None,
            alac_config: None,
            aac_config: None,
            aac: None,
            aac_licensed: false,
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
        decoder.celt = Some(CeltDecoder::new(usize::from(head.channels))?);
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

    /// Creates an AV1 decoder from a parsed sequence header. The
    /// header is validated against the subset (profile 0, 8-bit,
    /// YUV 4:2:0, no film grain); `decode` runs the
    /// reconstruction pipeline, which today returns
    /// `Unsupported` naming the missing coefficient CDF tables
    /// (no pixels yet) and gates inter frames the same way.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as `Decoder::new`, and
    /// `Unsupported` or `Overflow` when the sequence header is
    /// outside the subset.
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

    /// Creates a VP9 decoder from a parsed frame header. The
    /// header is validated against the subset (profile 0, 8-bit,
    /// YUV 4:2:0, no RGB); `decode` runs reconstruction, which
    /// decodes keyframes bit-exactly and returns `Unsupported`
    /// for inter frames until motion compensation is
    /// pixel-correct.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as `Decoder::new`, and
    /// `Unsupported` or `InvalidArgument` when the frame header is
    /// outside the subset.
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

    /// Creates a DSD decoder from a parsed DSF/DFF header. The
    /// default output rate is the DSD rate / 16 (176.4 kHz for
    /// DSD64, 352.8 kHz for DSD128); use
    /// `with_dsd_output_rate` to pick another rate from the
    /// 44.1 kHz or 48 kHz family before decoding. The decimator
    /// holds filter state across packets, so one decoder decodes
    /// one stream; `flush` returns the filter tail.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as `Decoder::new`, and
    /// `InvalidArgument` when the channel count exceeds the
    /// decoder limit.
    pub fn from_dsd_header(header: &DsdHeader) -> Result<Self> {
        let mut decoder = Self::new(DecoderConfig {
            codec: Codec::Dsd,
            ..DecoderConfig::default()
        })?;
        if header.channels > decoder.config.max_channels {
            return Err(Error::InvalidArgument(
                "dsd channel count exceeds decoder limit",
            ));
        }
        let target_rate = header.sample_rate / 16;
        let decimator = DsdDecimator::new(
            header.sample_rate,
            header.channels,
            target_rate,
            header.layout(),
        )?;
        decoder.dsd_header = Some(header.clone());
        decoder.dsd_decimator = Some(decimator);
        Ok(decoder)
    }

    /// Changes the DSD output rate. Only valid before the first
    /// `decode` call on a DSD decoder: it rebuilds the
    /// decimator, discarding its filter state.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when this is not a DSD decoder
    /// or the rate is outside the supported families.
    pub fn with_dsd_output_rate(mut self, target_rate: u32) -> Result<Self> {
        let header = self.dsd_header.clone().ok_or(Error::InvalidArgument(
            "dsd output rate requires a decoder built from a dsd header",
        ))?;
        let decimator = DsdDecimator::new(
            header.sample_rate,
            header.channels,
            target_rate,
            header.layout(),
        )?;
        self.config.dsd_output_rate = Some(target_rate);
        self.dsd_decimator = Some(decimator);
        Ok(self)
    }

    /// Creates an ALAC decoder from a parsed magic cookie
    /// (`AlacConfig`, from `parse_alac_config`). ALAC is open
    /// (Apple reference implementation, Apache-2.0), so no
    /// license is needed. One decoder decodes one stream; ALAC
    /// frames carry no cross-frame state.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as `Decoder::new`, and
    /// `InvalidArgument` when the channel count exceeds the
    /// decoder limit.
    pub fn from_alac_config(config: &AlacConfig) -> Result<Self> {
        let mut decoder = Self::new(DecoderConfig {
            codec: Codec::Alac,
            ..DecoderConfig::default()
        })?;
        if config.channels > decoder.config.max_channels {
            return Err(Error::InvalidArgument(
                "alac channel count exceeds decoder limit",
            ));
        }
        decoder.alac_config = Some(*config);
        Ok(decoder)
    }

    /// Creates an AAC-LC decoder from a parsed configuration
    /// (`AacConfig`, from `parse_adts_header` or
    /// `parse_audio_specific_config`). AAC is licensed: the
    /// `licensed-aac` feature must be compiled in and a valid
    /// `LicenseContext` must be presented. The license is
    /// validated (and audited) here, at construction; `decode`
    /// then runs without re-validating, and a decoder built
    /// without a license refuses every packet.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the `licensed-aac` feature is
    /// off, `LicenseRequired` when no license is presented or
    /// it has expired, `InvalidArgument` for a malformed
    /// license or a channel count above the decoder limit, and
    /// the same validation errors as `Decoder::new`.
    pub fn from_aac_config(
        config: &AacConfig,
        license: Option<&mut LicenseContext>,
    ) -> Result<Self> {
        if !crate::hw::licensed_feature_enabled(Codec::Aac) {
            return Err(Error::Unsupported(
                "aac decode requires the licensed-aac feature; rebuild with --features licensed-aac",
            ));
        }
        let Some(license) = license else {
            return Err(Error::LicenseRequired { codec: "aac" });
        };
        license.validate("aac")?;
        let mut decoder = Self::new(DecoderConfig {
            codec: Codec::Aac,
            ..DecoderConfig::default()
        })?;
        if config.channels > decoder.config.max_channels {
            return Err(Error::InvalidArgument(
                "aac channel count exceeds decoder limit",
            ));
        }
        if config.sample_rate > decoder.config.max_sample_rate {
            return Err(Error::Overflow("aac sample rate exceeds maximum"));
        }
        decoder.aac_config = Some(*config);
        // Validates the config for the spectral decoder too:
        // 7350 Hz, an unknown rate and a bad frame length fail
        // here, before any packet is decoded.
        decoder.aac = Some(AacDecoder::new(config)?);
        decoder.aac_licensed = true;
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
        if &data[0..4] == b"DSD " {
            return Ok(Codec::Dsd);
        }
        if &data[0..4] == b"FRM8" {
            if data.len() >= 12 && &data[8..12] == b"DSD " {
                return Ok(Codec::Dsd);
            }
            return Err(Error::Unsupported(
                "dff container: use DsdDemuxer to identify the stream",
            ));
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
    /// Opus; AV1 coefficient decode, whose CDF tables are still
    /// missing; VP9 inter frames, whose motion compensation is
    /// not yet pixel-correct), `AllocationLimit` when the packet
    /// exceeds `max_frame_bytes`, `InvalidArgument` when a PCM
    /// packet arrives without a configured format, a FLAC packet
    /// arrives without a STREAMINFO block, an Opus packet arrives
    /// without an `OpusHead`, or a Vorbis packet arrives without
    /// a setup, and `InvalidData` for frame-unaligned PCM bytes,
    /// a corrupt FLAC frame, a malformed Opus packet, or a corrupt
    /// Vorbis packet.
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
            Codec::Mp3 => self.decode_mp3(packet),
            Codec::Dsd => self.decode_dsd(packet),
            Codec::Alac => self.decode_alac(packet),
            Codec::Aac => {
                // AAC-LC software decode exists, but only behind
                // the license gate: a decoder built by
                // `from_aac_config` with a validated license sets
                // `aac_licensed`; any other construction (including
                // `Decoder::new` with codec Aac) reaches here
                // unlicensed, so the answer is the gate error:
                // `Unsupported` when the feature is off,
                // `LicenseRequired` when it is on.
                if !self.aac_licensed {
                    return Err(crate::hw::licensed_software_path_error(Codec::Aac));
                }
                self.decode_aac(packet)
            }
            Codec::Av1 => self.decode_av1(packet),
            Codec::Vp9 => self.decode_vp9(packet),
            // AVC/HEVC are never decoded in software: the gate
            // lives in `hw`, and this decoder holds no license,
            // so the answer is always the gate error.
            Codec::Avc | Codec::Hevc => {
                Err(crate::hw::licensed_software_path_error(self.config.codec))
            }
        }
    }

    /// Decodes one AV1 temporal unit: one packet holds the OBUs of a
    /// single frame (temporal delimiter, optional sequence header,
    /// frame OBU). Keyframe intra only for now; the pipeline in
    /// `av1_reconstruct` gates everything else with `Unsupported`
    /// — today the coefficient CDF tables are missing, so a valid
    /// keyframe returns `Unsupported` naming that gap and no
    /// pixels exist yet. A packet showing an existing frame is
    /// not covered: the pipeline expects a real frame OBU with
    /// tile data.
    fn decode_av1(&mut self, packet: &Packet) -> Result<Frame> {
        if packet.data.len() > self.config.max_frame_bytes {
            return Err(Error::AllocationLimit {
                requested: packet.data.len(),
                limit: self.config.max_frame_bytes,
            });
        }
        let mut frame_payload: Option<Vec<u8>> = None;
        let mut reader = ObuReader::new(&packet.data);
        while let Some(obu) = reader.next_obu()? {
            match obu.header.obu_type {
                ObuType::SequenceHeader => {
                    self.av1_sequence = Some(parse_sequence_header(obu.payload)?);
                }
                ObuType::Frame => {
                    frame_payload = Some(obu.payload.to_vec());
                }
                _ => {}
            }
        }
        let frame_payload = frame_payload.ok_or(Error::InvalidData {
            offset: 0,
            detail: "av1 packet has no frame OBU",
        })?;
        // Clone: `reconstruct_av1_frame_obu` needs `&mut self.av1_state`
        // while the sequence header is still borrowed.
        let seq = self.av1_sequence.clone().ok_or(Error::InvalidArgument(
            "av1 sequence header has not been seen yet: construct the decoder from a sequence header",
        ))?;
        let video =
            reconstruct_av1_frame_obu(&frame_payload, &seq, &mut self.av1_state, packet.timestamp)?;
        Ok(Frame::Video(video))
    }

    /// Decodes one VP9 packet. A packet may hold a superframe;
    /// each sub-frame is decoded in order and the last decoded
    /// frame is returned. Keyframes (and show-existing frames)
    /// decode bit-exactly; inter frames return `Unsupported`
    /// until motion compensation is pixel-correct, so the
    /// decoder never returns wrong pixels for them.
    fn decode_vp9(&mut self, packet: &Packet) -> Result<Frame> {
        if packet.data.len() > self.config.max_frame_bytes {
            return Err(Error::AllocationLimit {
                requested: packet.data.len(),
                limit: self.config.max_frame_bytes,
            });
        }
        let mut shown: Option<VideoFrame> = None;
        for (offset, size) in parse_superframe(&packet.data)? {
            let sub = &packet.data[offset..offset + size];
            let parsed = parse_vp9_frame(sub, &mut self.vp9_state)?;
            if !parsed.header.frame_is_intra && !parsed.header.show_existing_frame {
                return Err(Error::Unsupported(
                    "vp9 inter-frame reconstruction is not yet pixel-correct (keyframes decode; inter frames return Unsupported)",
                ));
            }
            let mut frame = reconstruct_vp9_frame(&parsed, sub, &mut self.vp9_recon_state)?;
            frame.timestamp = packet.timestamp;
            shown = Some(frame);
        }
        match shown {
            Some(frame) => Ok(Frame::Video(frame)),
            None => Err(Error::InvalidData {
                offset: 0,
                detail: "vp9 packet holds no frame",
            }),
        }
    }

    /// Decodes one ALAC frame. One packet holds exactly one
    /// ALAC frame, as produced by an MP4 demuxer; the output
    /// sample count comes from the frame's partial-frame count
    /// (or the cookie's frame length) and is always <= 16384.
    fn decode_alac(&mut self, packet: &Packet) -> Result<Frame> {
        if packet.data.len() > self.config.max_frame_bytes {
            return Err(Error::AllocationLimit {
                requested: packet.data.len(),
                limit: self.config.max_frame_bytes,
            });
        }
        let config = self.alac_config.ok_or(Error::InvalidArgument(
            "alac config not set: construct the decoder from an alac config",
        ))?;
        decode_alac_audio_frame(&packet.data, &config, packet.timestamp)
    }

    /// Decodes one MP3 Layer III frame. One packet holds exactly
    /// one frame; the decoder state (bit reservoir, IMDCT
    /// overlap, polyphase history) is created on the first frame
    /// and carried across packets, so frames must be fed in
    /// order from the start of the stream. Layers I and II
    /// return `Unsupported` from the frame decoder.
    fn decode_mp3(&mut self, packet: &Packet) -> Result<Frame> {
        if packet.data.len() > self.config.max_frame_bytes {
            return Err(Error::AllocationLimit {
                requested: packet.data.len(),
                limit: self.config.max_frame_bytes,
            });
        }
        let decoder = self.mp3.get_or_insert_with(Mp3Decoder::new);
        let mut audio = decoder.decode_frame(&packet.data)?;
        audio.timestamp = packet.timestamp;
        Ok(Frame::Audio(audio))
    }

    /// Decodes one AAC raw data block (one MP4 sample, or one
    /// ADTS frame payload with the header stripped). The gate
    /// in `decode` is what makes this reachable only with a
    /// validated license; this method only runs the spectral
    /// decoder built by `from_aac_config`.
    fn decode_aac(&mut self, packet: &Packet) -> Result<Frame> {
        if packet.data.len() > self.config.max_frame_bytes {
            return Err(Error::AllocationLimit {
                requested: packet.data.len(),
                limit: self.config.max_frame_bytes,
            });
        }
        let decoder = self.aac.as_mut().ok_or(Error::InvalidArgument(
            "aac config not set: construct the decoder from an aac config",
        ))?;
        let frame = decoder.decode_raw_data_block(&packet.data)?;
        Ok(Frame::Audio(AudioFrame {
            sample_rate: frame.sample_rate,
            channels: frame.channels,
            samples: frame.samples,
            timestamp: packet.timestamp,
        }))
    }

    /// Decodes one DSD packet. The packet holds raw DSD bytes
    /// in the header's layout (DSF block groups or DFF
    /// byte-interleaved); the decimator carries the filter state
    /// across packets, so early packets may produce an empty
    /// frame while the filter fills, and the tail only appears
    /// on `flush`.
    fn decode_dsd(&mut self, packet: &Packet) -> Result<Frame> {
        if packet.data.len() > self.config.max_frame_bytes {
            return Err(Error::AllocationLimit {
                requested: packet.data.len(),
                limit: self.config.max_frame_bytes,
            });
        }
        let (channels, target_rate) = {
            let header = self.dsd_header.as_ref().ok_or(Error::InvalidArgument(
                "dsd header not set: construct the decoder from a dsd header",
            ))?;
            let decimator = self.dsd_decimator.as_ref().ok_or(Error::InvalidArgument(
                "dsd header not set: construct the decoder from a dsd header",
            ))?;
            (header.channels, decimator.target_rate())
        };
        let decimator = self.dsd_decimator.as_mut().ok_or(Error::InvalidArgument(
            "dsd header not set: construct the decoder from a dsd header",
        ))?;
        let samples = decimator.feed(&packet.data)?;
        Ok(Frame::Audio(AudioFrame {
            sample_rate: target_rate,
            channels,
            samples,
            timestamp: packet.timestamp,
        }))
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
                        let celt = self.celt.as_mut().ok_or(Error::InvalidArgument(
                            "opus celt decoder not initialised: construct the decoder from an opus head",
                        ))?;
                        let decoded = celt.decode_frame(
                            frame,
                            celt_end_band(opus_packet.config.bandwidth),
                            frame_duration,
                        )?;
                        samples.extend_from_slice(&decoded);
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
        if self.config.codec == Codec::Dsd {
            let Some(decimator) = self.dsd_decimator.as_mut() else {
                return Ok(None);
            };
            let samples = decimator.flush()?;
            if samples.is_empty() {
                return Ok(None);
            }
            let (channels, target_rate) = {
                let header = self.dsd_header.as_ref().ok_or(Error::InvalidArgument(
                    "dsd header not set: construct the decoder from a dsd header",
                ))?;
                (header.channels, decimator.target_rate())
            };
            // The tail starts one frame-duration before the end
            // of the stream; its timestamp is derived from the
            // sample count, not from a packet timestamp.
            let tail_frames = samples.len() / channels.max(1) as usize;
            let timestamp_us = self.dsd_header.as_ref().map_or(0, |header| {
                // DSF declares the sample count; DFF does
                // not, so it is derived from the data size.
                let total_samples = if header.sample_count > 0 {
                    header.sample_count
                } else {
                    header.data_size / u64::from(header.channels) * 8
                };
                total_samples.saturating_sub(tail_frames as u64) * 1_000_000
                    / u64::from(header.sample_rate)
            });
            return Ok(Some(Frame::Audio(AudioFrame {
                sample_rate: target_rate,
                channels,
                samples,
                timestamp: timestamp_us,
            })));
        }
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
    fn av1_decoder_rejects_reserved_obu_packet_as_invalid_data() {
        use crate::av1_sequence::parse_sequence_header;
        let seq_bytes = [0x00u8, 0x00, 0x00, 0x02, 0xAF, 0xFF, 0x9B, 0x5F, 0x20, 0x08];
        let seq = parse_sequence_header(&seq_bytes).unwrap();
        let mut decoder = Decoder::from_av1_sequence_header(&seq).unwrap();
        // Ten zero bytes: the first OBU has the reserved type 0,
        // which the decoder skips, and no frame OBU follows, so
        // the packet fails as "no frame OBU" rather than reaching
        // reconstruction.
        let packet = Packet {
            codec: Codec::Av1,
            data: vec![0u8; 10],
            timestamp: 0,
            is_keyframe: true,
        };
        let err = decoder.decode(&packet).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
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
    fn vp9_decoder_rejects_garbage_packet_as_invalid_data() {
        let mut decoder = Decoder::from_vp9_frame_header(&Vp9FrameHeader::default()).unwrap();
        // Ten zero bytes: the frame marker is 0, not 2, so the
        // header parse fails before reconstruction is reached.
        let packet = Packet {
            codec: Codec::Vp9,
            data: vec![0u8; 10],
            timestamp: 0,
            is_keyframe: true,
        };
        let err = decoder.decode(&packet).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
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
