// Casts in this module are bounded by construction: every
// value cast to a narrower integer comes from `read_bits(n)`
// (at most n bits wide), a validated range, or a table entry
// whose size is fixed by the standard, and every float cast
// is an intentional conversion of a bit-reinterpreted integer
// or a spectrum value. The pedantic cast lints therefore do
// not find real truncation risks here; the bounds are checked
// where the values are read, not where they are cast.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_precision_loss)]

//! AAC-LC frame headers and raw-data-block decoding.
//!
//! References: ISO/IEC 13818-7 (MPEG-2 AAC) for the ADTS
//! header (section 6.2.1, Table 6.3) and the sampling-frequency
//! table (Table 6.4), and ISO/IEC 14496-3 (MPEG-4 Audio) for
//! the `AudioSpecificConfig` syntax (section 1.6.2.1) and the
//! raw-data-block syntax (section 4.5.2.3). Only AAC-LC (audio
//! object type 2) is decoded by this library; HE-AAC (SBR,
//! object type 5), AAC-LD (23) and USAC (39) are rejected with
//! `Unsupported` at config time, so a caller can tell "wrong
//! AAC flavour" apart from "corrupt input".
//!
//! The spectral Huffman tables live in `aac_huffman.rs` and
//! the scalefactor-band offsets, windows and TNS tables in
//! `aac_tables.rs`; this file holds the headers plus the
//! raw-data-block decoder that uses them.

use crate::aac_huffman::{decode_scale_factor, decode_spectral, huffman_is_quad};
use crate::aac_tables::{
    KBD_LONG_1024, KBD_LONG_960, KBD_SHORT_120, KBD_SHORT_128, MAX_TNS_SFB_LC, NUM_SWB_1024,
    NUM_SWB_128, NUM_SWB_960, SINE_LONG_1024, SINE_LONG_960, SINE_SHORT_120, SINE_SHORT_128,
    SWB_OFFSET_1024_WINDOW, SWB_OFFSET_128_WINDOW, TNS_COEF_0_3, TNS_COEF_0_4, TNS_COEF_1_3,
    TNS_COEF_1_4,
};
use crate::adts_frame::parse_adts_header as parse_adts_header_raw;
use crate::bitreader::BitReader;
use crate::error::{Error, Result};
use crate::mdct::mdct_inverse;

/// Sampling rates indexed by `sampling_frequency_index`
/// (ISO/IEC 13818-7 Table 6.4). Indices 13 and 14 are
/// reserved in the table; index 15 is the escape to an
/// explicit 24-bit rate.
pub const AAC_SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8000,
    7350,
];

/// Channel counts indexed by `channelConfiguration`
/// (ISO/IEC 14496-3 Table 1.19). Configuration 0 means the
/// count comes from a program config element, which this
/// library does not read.
pub const AAC_CHANNEL_COUNTS: [u32; 8] = [0, 1, 2, 3, 4, 5, 6, 8];

/// AAC stream configuration, from an ADTS header or an
/// `AudioSpecificConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacConfig {
    /// Audio object type: 2 for AAC-LC (the only type decoded).
    pub audio_object_type: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u32,
    /// Samples per frame: 1024, or 960 when the
    /// `frameLengthFlag` is set.
    pub frame_length: u32,
}

/// Looks up a sampling-frequency index (ISO/IEC 13818-7
/// Table 6.4).
///
/// # Errors
///
/// Returns `InvalidData` for the reserved indices 13 and 14.
pub fn sample_rate_for_index(index: u32) -> Result<u32> {
    AAC_SAMPLE_RATES
        .get(index as usize)
        .copied()
        .ok_or(Error::InvalidData {
            offset: 0,
            detail: "aac sampling frequency index is reserved (13 or 14)",
        })
}

/// Parses an `AudioSpecificConfig` (ISO/IEC 14496-3 section
/// 1.6.2.1), as carried by an MP4 `esds` `DecoderSpecificInfo`
/// or by an `mp4a` sample entry.
///
/// # Errors
///
/// Returns `Truncated` when the config ends mid-field,
/// `InvalidData` for a reserved sampling-frequency index or
/// a channel configuration of 0 (which needs a program
/// config element), and `Unsupported` for any audio object
/// type other than AAC-LC (2).
pub fn parse_audio_specific_config(data: &[u8]) -> Result<AacConfig> {
    if data.is_empty() {
        return Err(Error::Truncated { needed: 1, had: 0 });
    }
    let mut reader = BitReader::new(data);
    let audio_object_type = reader.read_bits(5)?;
    if audio_object_type != 2 {
        return Err(Error::Unsupported(
            "aac audio object type is not AAC-LC (only object type 2 is decoded)",
        ));
    }
    let frequency_index = reader.read_bits(4)?;
    let sample_rate = if frequency_index == 15 {
        reader.read_bits(24)?
    } else {
        sample_rate_for_index(frequency_index)?
    };
    let channel_config = reader.read_bits(4)?;
    let channels = AAC_CHANNEL_COUNTS
        .get(channel_config as usize)
        .copied()
        .unwrap_or(0);
    if channels == 0 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "aac channel configuration 0 needs a program config element",
        });
    }
    // GASpecificConfig (ISO/IEC 14496-3 section 1.6.2.1):
    // frameLengthFlag, dependsOnCoreCoder, extensionFlag.
    // A set extension flag carries extensionAudioObjectType
    // (5 bits); SBR there would make this HE-AAC, which is
    // rejected above by object type but can also arrive as an
    // extension, so it is rejected here too.
    let frame_length_flag = reader.read_bit()?;
    let _depends_on_core_coder = reader.read_bit()?;
    let extension_flag = reader.read_bit()?;
    if extension_flag {
        let extension_object_type = reader.read_bits(5)?;
        if extension_object_type == 5 {
            return Err(Error::Unsupported(
                "aac SBR extension (HE-AAC) is not decoded",
            ));
        }
    }
    Ok(AacConfig {
        audio_object_type,
        sample_rate,
        channels,
        frame_length: if frame_length_flag { 960 } else { 1024 },
    })
}

/// Parses one ADTS frame header (ISO/IEC 13818-7 section
/// 6.2.1) and returns the stream configuration plus the total
/// frame length in bytes (header plus payload, including the
/// 2-byte CRC when `protection_absent` is 0).
///
/// This is a thin wrapper over `adts_frame::parse_adts_header`,
/// which owns the single bit-level ADTS header parser in the
/// crate (it also owns CRC-16, the streaming demuxer and the
/// writer). The wrapper only adds what the AAC decoder needs:
/// the profile must be LC (anything else is `Unsupported`,
/// not corrupt input), the channel configuration is mapped to
/// a channel count, and the frame must fit in the supplied
/// bytes (the shared parser only bounds the frame against the
/// allocation limit, so without this check `split_adts_frames`
/// could slice past the end of the input).
///
/// Behaviour differences from the old duplicated parser, all
/// deliberate: a reserved profile (3) is now `InvalidData`
/// from the shared parser instead of `Unsupported` (it is a
/// corrupt header, not an unsupported flavour); error detail
/// strings and offsets for bad fields now come from the shared
/// parser (same `Error` variants); and a frame longer than
/// `MAX_AUDIO_PACKET_BYTES` is now `AllocationLimit`, matching
/// the rest of the library.
///
/// # Errors
///
/// Returns `Truncated` when fewer than 7 header bytes are
/// present or the frame is longer than the supplied bytes,
/// `InvalidData` for a bad syncword, a nonzero layer, a
/// reserved sampling-frequency index, a channel configuration
/// of 0, or a frame length shorter than the header,
/// `Unsupported` for a non-LC profile, and `AllocationLimit`
/// when the frame exceeds the audio packet limit.
pub fn parse_adts_header(data: &[u8]) -> Result<(AacConfig, usize)> {
    let (header, _header_len) = parse_adts_header_raw(data)?;
    if header.profile != 1 {
        return Err(Error::Unsupported(
            "adts profile is not AAC-LC (only profile 1, object type 2, is decoded)",
        ));
    }
    if header.frame_length > data.len() {
        return Err(Error::Truncated {
            needed: header.frame_length,
            had: data.len(),
        });
    }
    // The shared parser already rejects channel configuration
    // 0, so the table lookup below always finds a nonzero
    // count; the fallback is unreachable by construction.
    let channels = AAC_CHANNEL_COUNTS
        .get(usize::from(header.channels))
        .copied()
        .unwrap_or(0);
    Ok((
        AacConfig {
            audio_object_type: 2,
            sample_rate: header.sample_rate,
            channels,
            frame_length: 1024,
        },
        header.frame_length,
    ))
}

/// Splits a whole ADTS stream into frames, returning the
/// stream configuration (from the first header) and one byte
/// slice per frame (header included).
///
/// # Errors
///
/// Returns the same errors as `parse_adts_header`, and
/// `InvalidData` when a later header disagrees with the first
/// on sample rate or channels.
pub fn split_adts_frames(data: &[u8]) -> Result<(AacConfig, Vec<&[u8]>)> {
    let mut frames: Vec<&[u8]> = Vec::new();
    let mut offset = 0usize;
    let mut config: Option<AacConfig> = None;
    while offset < data.len() {
        let (frame_config, frame_len) = parse_adts_header(&data[offset..])?;
        match config {
            None => config = Some(frame_config),
            Some(first) => {
                if first.sample_rate != frame_config.sample_rate
                    || first.channels != frame_config.channels
                {
                    return Err(Error::InvalidData {
                        offset,
                        detail: "adts stream changes sample rate or channels mid-stream",
                    });
                }
            }
        }
        frames.push(&data[offset..offset + frame_len]);
        offset += frame_len;
    }
    match config {
        Some(config) => Ok((config, frames)),
        None => Err(Error::Truncated { needed: 7, had: 0 }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adts_header_parses_44100_stereo_lc() {
        // ADTS header for AAC-LC, 44.1 kHz, stereo, frame
        // length 100: syncword FFF, ID 0, layer 0,
        // protection_absent 1, profile 1 (LC), sf index 4,
        // channel config 2, frame length 100.
        let header: [u8; 7] = [0xFF, 0xF1, 0x50, 0x80, 0x0C, 0x9F, 0xFC];
        let mut data = header.to_vec();
        data.resize(100, 0);
        let (config, len) = parse_adts_header(&data).unwrap();
        assert_eq!(len, 100);
        assert_eq!(config.audio_object_type, 2);
        assert_eq!(config.sample_rate, 44_100);
        assert_eq!(config.channels, 2);
        assert_eq!(config.frame_length, 1024);
    }

    #[test]
    fn adts_bad_syncword_is_invalid_data() {
        let data = [0x00u8; 16];
        let err = parse_adts_header(&data).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn adts_short_input_is_truncated() {
        let err = parse_adts_header(&[0xFF, 0xF1]).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn audio_specific_config_parses_lc_stereo_44100() {
        // ASC: object type 2 (00010), sf index 4 (0100),
        // channel config 2 (0010), frameLengthFlag 0,
        // dependsOnCoreCoder 0, extensionFlag 0.
        // (The bytes 0x12 0x10 carry exactly those bits;
        // 0x11 0x90 would be sf index 3, 48000 Hz.)
        let asc: [u8; 2] = [0x12, 0x10];
        let config = parse_audio_specific_config(&asc).unwrap();
        assert_eq!(config.audio_object_type, 2);
        assert_eq!(config.sample_rate, 44_100);
        assert_eq!(config.channels, 2);
        assert_eq!(config.frame_length, 1024);
    }

    #[test]
    fn audio_specific_config_rejects_sbr_object_type() {
        // Object type 5 (SBR): 00101, sf index 4, channel 2.
        let asc: [u8; 2] = [0x2B, 0x90];
        let err = parse_audio_specific_config(&asc).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn sample_rate_table_rejects_reserved_indices() {
        assert!(matches!(
            sample_rate_for_index(13),
            Err(Error::InvalidData { .. })
        ));
        assert_eq!(sample_rate_for_index(4).unwrap(), 44_100);
    }
}

// =====================================================================
// Raw-data-block decoding (ISO/IEC 14496-3 section 4.5.2.3).
//
// The walk mirrors the reference decoder's structure (ICS info,
// section data, scale factors, pulse, TNS, spectral data, then
// dequantize, MS/IS stereo, TNS filter and the filter bank),
// but every line below is original code written from the
// standard's syntax tables.
// =====================================================================

/// Window sequences (ISO/IEC 14496-3 Table 4.6.7).
const WINDOW_ONLY_LONG: u8 = 0;
const WINDOW_LONG_START: u8 = 1;
const WINDOW_EIGHT_SHORT: u8 = 2;
const WINDOW_LONG_STOP: u8 = 3;

/// Element ids in a raw data block (ISO/IEC 14496-3 Table 4.6.5).
const ID_SCE: u8 = 0;
const ID_CPE: u8 = 1;
const ID_CCE: u8 = 2;
const ID_LFE: u8 = 3;
const ID_DSE: u8 = 4;
const ID_PCE: u8 = 5;
const ID_FIL: u8 = 6;
const ID_END: u8 = 7;

/// Section codebooks: zero, noise (PNS), intensity stereo
/// (ISO/IEC 14496-3 Table 4.6.6).
const ZERO_HCB: u8 = 0;
const NOISE_HCB: u8 = 13;
const INTENSITY_HCB: u8 = 14;
const INTENSITY_HCB2: u8 = 15;

/// Extension types in a fill element (ISO/IEC 14496-3
/// Table 4.6.9). Only the two SBR types are refused; the rest
/// are skipped, since the float path applies no dynamic range
/// control.
const EXT_SBR_DATA: u8 = 2;
const EXT_SBR_DATA_CRC: u8 = 3;

/// A decoded AAC frame: interleaved `f32` samples, one frame
/// of `frame_length` samples per channel.
#[derive(Debug, Clone)]
pub struct AacDecodedFrame {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u32,
    /// Interleaved samples, `frame_length * channels` long.
    pub samples: Vec<f32>,
}

/// Stateful AAC-LC decoder for one stream: holds the overlap
/// buffers and previous window shapes the filter bank needs
/// across frames (ISO/IEC 14496-3 section 4.6.3.1).
#[derive(Debug)]
pub struct AacDecoder {
    config: AacConfig,
    sf_index: u8,
    overlap: Vec<Vec<f32>>,
    window_shape_prev: Vec<u8>,
    /// Blocks decoded so far whose window sequence was
    /// `EIGHT_SHORT`; a diagnostic counter, so a test can
    /// prove short windows were exercised.
    short_window_blocks: u64,
    /// PNS noise generator state, carried across frames.
    pns_rng: PnsRng,
}

impl AacDecoder {
    /// Creates a decoder for a stream described by `config`.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` for non-LC object types, for
    /// 7350 Hz (no scalefactor-band table exists for it) and
    /// for frame lengths other than 1024 or 960, and
    /// `InvalidData` for a sample rate with no table entry or
    /// an out-of-range channel count.
    pub fn new(config: &AacConfig) -> Result<Self> {
        if config.audio_object_type != 2 {
            return Err(Error::Unsupported(
                "only AAC-LC (audio object type 2) is decoded",
            ));
        }
        if config.channels == 0 || config.channels > 8 {
            return Err(Error::InvalidArgument(
                "aac channel count must be between 1 and 8",
            ));
        }
        if config.frame_length != 1024 && config.frame_length != 960 {
            return Err(Error::Unsupported("aac frame length must be 1024 or 960"));
        }
        let Some(sf_index) = AAC_SAMPLE_RATES
            .iter()
            .position(|&rate| rate == config.sample_rate)
        else {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "aac sample rate has no scalefactor-band table",
            });
        };
        if sf_index == 12 {
            // 7350 Hz: the standard's tables stop at 8000 Hz.
            return Err(Error::Unsupported(
                "aac sample rate 7350 Hz is not supported",
            ));
        }
        let channels = config.channels as usize;
        let frame_length = config.frame_length as usize;
        Ok(Self {
            config: *config,
            sf_index: sf_index as u8,
            overlap: vec![vec![0.0; frame_length]; channels],
            window_shape_prev: vec![0; channels],
            short_window_blocks: 0,
            pns_rng: PnsRng::new(),
        })
    }

    /// The stream configuration this decoder was created with.
    #[must_use]
    pub fn config(&self) -> AacConfig {
        self.config
    }

    /// How many decoded blocks used the `EIGHT_SHORT` window
    /// sequence (a diagnostic counter for tests).
    #[must_use]
    pub fn short_window_blocks(&self) -> u64 {
        self.short_window_blocks
    }

    /// Decodes one raw data block (one MP4 sample, or one ADTS
    /// frame payload with the ADTS header stripped) into PCM.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a short block, `InvalidData`
    /// for a malformed block, and `Unsupported` for features
    /// this decoder does not implement (SBR, LTP, PNS,
    /// gain control, dependent coupling).
    // One coherent element loop: parse every element, then
    // run PNS, MS/IS, coupling, TNS and the filter bank in
    // order. Splitting it would scatter the element dispatch
    // without making any part clearer.
    #[allow(clippy::too_many_lines)]
    pub fn decode_raw_data_block(&mut self, data: &[u8]) -> Result<AacDecodedFrame> {
        if data.is_empty() {
            return Err(Error::Truncated { needed: 8, had: 0 });
        }
        let ctx = BlockContext {
            sf_index: self.sf_index,
            frame_length: self.config.frame_length,
        };
        let mut reader = BitReader::new(data);
        let mut channels: Vec<DecodedChannel> = Vec::new();
        let mut couplings: Vec<Coupling> = Vec::new();
        let mut first_element = true;
        loop {
            if reader.remaining_bits() < 3 {
                return Err(Error::Truncated {
                    needed: 3,
                    had: reader.remaining_bits(),
                });
            }
            let id = reader.read_bits(3)? as u8;
            match id {
                ID_END => break,
                ID_SCE => {
                    let tag = reader.read_bits(4)? as u8;
                    let stream = parse_individual_channel_stream(&mut reader, &ctx, false, None)?;
                    if stream.is_used {
                        return Err(Error::InvalidData {
                            offset: reader.bit_position() / 8,
                            detail: "intensity stereo in a single channel element",
                        });
                    }
                    channels.push(DecodedChannel {
                        stream,
                        tag,
                        is_cpe: false,
                        is_left: false,
                    });
                }
                ID_CPE => {
                    let tag = reader.read_bits(4)? as u8;
                    let common_window = reader.read_bit()?;
                    let (left, right) = parse_channel_pair(&mut reader, &ctx, common_window)?;
                    channels.push(DecodedChannel {
                        stream: left,
                        tag,
                        is_cpe: true,
                        is_left: true,
                    });
                    channels.push(DecodedChannel {
                        stream: right,
                        tag,
                        is_cpe: true,
                        is_left: false,
                    });
                }
                ID_LFE => {
                    let tag = reader.read_bits(4)? as u8;
                    let stream = parse_individual_channel_stream(&mut reader, &ctx, false, None)?;
                    if stream.is_used {
                        return Err(Error::InvalidData {
                            offset: reader.bit_position() / 8,
                            detail: "intensity stereo in a low-frequency element",
                        });
                    }
                    channels.push(DecodedChannel {
                        stream,
                        tag,
                        is_cpe: false,
                        is_left: false,
                    });
                }
                ID_CCE => {
                    let coupling = parse_coupling_element(&mut reader, &ctx)?;
                    couplings.push(coupling);
                }
                ID_DSE => parse_data_stream_element(&mut reader)?,
                ID_PCE => {
                    if !first_element {
                        return Err(Error::InvalidData {
                            offset: reader.bit_position() / 8,
                            detail: "program config element must be the first element",
                        });
                    }
                    parse_program_config_element(&mut reader)?;
                }
                ID_FIL => parse_fill_element(&mut reader)?,
                _ => {
                    return Err(Error::InvalidData {
                        offset: reader.bit_position() / 8,
                        detail: "unknown raw data block element id",
                    });
                }
            }
            first_element = false;
        }

        if channels.len() != self.config.channels as usize {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "raw data block channel count does not match the config",
            });
        }

        // PNS fills noise bands with shaped pseudo-random
        // noise (ISO/IEC 14496-3 section 4.6.3.6) before
        // MS/IS stereo, which skips those bands.
        let mut index = 0;
        while index < channels.len() {
            if channels[index].is_cpe {
                let (left, right) = channels.split_at_mut(index + 1);
                let (left_stream, right_stream) = (&mut left[index].stream, &mut right[0].stream);
                apply_pns_pair(left_stream, right_stream, &mut self.pns_rng);
                index += 2;
            } else {
                apply_pns_single(&mut channels[index].stream, &mut self.pns_rng);
                index += 1;
            }
        }

        // MS and intensity stereo act on the dequantized spectra
        // of a channel pair (ISO/IEC 14496-3 section 4.6.3.2).
        let mut index = 0;
        while index < channels.len() {
            if channels[index].is_cpe {
                let (left, right) = channels.split_at_mut(index + 1);
                let (left_stream, right_stream) = (&mut left[index].stream, &mut right[0].stream);
                apply_ms_stereo(left_stream, right_stream);
                apply_intensity_stereo(left_stream, right_stream);
                index += 2;
            } else {
                index += 1;
            }
        }

        // Coupling feeds the target spectra before their TNS
        // filters run, so the coupled energy is filtered with
        // the target channel.
        for coupling in &couplings {
            apply_coupling(&mut channels, coupling);
        }

        // TNS, then the filter bank with its overlap-add state.
        let frame_length = self.config.frame_length as usize;
        let channel_count = channels.len();
        let mut output = vec![0.0f32; frame_length * channel_count];
        for (channel_index, channel) in channels.iter_mut().enumerate() {
            if channel.stream.ics.window_sequence == WINDOW_EIGHT_SHORT {
                self.short_window_blocks += 1;
            }
            apply_tns(&mut channel.stream, self.sf_index);
            let window_shape_prev = self.window_shape_prev[channel_index];
            let time = filter_bank(
                &channel.stream,
                &mut self.overlap[channel_index],
                window_shape_prev,
                self.config.frame_length,
            )?;
            self.window_shape_prev[channel_index] = channel.stream.ics.window_shape;
            for (sample_index, sample) in time.iter().enumerate() {
                output[sample_index * channel_count + channel_index] = *sample;
            }
        }

        Ok(AacDecodedFrame {
            sample_rate: self.config.sample_rate,
            channels: self.config.channels,
            samples: output,
        })
    }
}

/// Per-block constants shared by the element parsers.
struct BlockContext {
    sf_index: u8,
    frame_length: u32,
}

/// One decoded output channel, with the element that produced
/// it (a coupling target is matched by tag and pair side).
struct DecodedChannel {
    stream: ChannelStream,
    tag: u8,
    is_cpe: bool,
    is_left: bool,
}

/// A coupling channel element (ISO/IEC 14496-3 section
/// 4.5.2.3.3): its own spectrum plus one gain list per
/// coupled target. The gain lists are stored in target
/// order, so list `i` belongs to target `i`.
struct Coupling {
    spec: Vec<f32>,
    ics: IcsInfo,
    sfb_cb: [[u8; 64]; 8],
    targets: Vec<CoupledTarget>,
    gains: Vec<GainList>,
}

/// One coupled target: an SCE/LFE by tag, or one or both
/// sides of a CPE by tag.
struct CoupledTarget {
    is_cpe: bool,
    tag: u8,
    left: bool,
    right: bool,
}

/// Gains for one coupled target: either a single common gain
/// for the whole spectrum, or one gain per scalefactor band
/// of the coupling channel (bands with a zero codebook were
/// never read, so their gain stays 0).
struct GainList {
    common: Option<f32>,
    per_band: [[f32; 64]; 8],
}

/// Parses a channel pair element (ISO/IEC 14496-3 section
/// 4.5.2.3.3). With a common window the two channels share
/// one ICS info block and the MS mask is read once; without
/// one each channel has its own.
fn parse_channel_pair(
    reader: &mut BitReader,
    ctx: &BlockContext,
    common_window: bool,
) -> Result<(ChannelStream, ChannelStream)> {
    if common_window {
        let ics = parse_ics_info(reader, ctx)?;
        let ms_mask_present = reader.read_bits(2)? as u8;
        if ms_mask_present == 3 {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "ms mask present value 3 is reserved",
            });
        }
        let mut ms_used = [[false; 64]; 8];
        if ms_mask_present == 1 {
            for row in ms_used.iter_mut().take(ics.num_window_groups as usize) {
                for cell in row.iter_mut().take(ics.max_sfb as usize) {
                    *cell = reader.read_bit()?;
                }
            }
        }
        let mut left = parse_individual_channel_stream(reader, ctx, true, Some(&ics))?;
        left.ms_mask_present = ms_mask_present;
        left.ms_used = ms_used;
        let right = parse_individual_channel_stream(reader, ctx, true, Some(&ics))?;
        Ok((left, right))
    } else {
        let left = parse_individual_channel_stream(reader, ctx, false, None)?;
        let right = parse_individual_channel_stream(reader, ctx, false, None)?;
        Ok((left, right))
    }
}

/// Parses a coupling channel element. The coupling channel's
/// own spectrum is dequantized here; the gain lists are read
/// in target order (the spec numbers them from 1, with list
/// 0 being the coupling channel itself, which carries no
/// bits). Dependent coupling (`cc_domain == 1`) is refused:
/// its gains are coded per window group of the target, not
/// per band of the coupling channel, and this decoder does
/// not implement that form.
fn parse_coupling_element(reader: &mut BitReader, ctx: &BlockContext) -> Result<Coupling> {
    let _tag = reader.read_bits(4)?;
    let ind_sw_cce_flag = reader.read_bit()?;
    let num_coupled_elements = reader.read_bits(3)? as usize;
    let mut targets = Vec::new();
    let mut num_gain_element_lists = 0usize;
    for _ in 0..=num_coupled_elements {
        num_gain_element_lists += 1;
        let target_is_cpe = reader.read_bit()?;
        let target_tag = reader.read_bits(4)? as u8;
        let (left, right) = if target_is_cpe {
            let left = reader.read_bit()?;
            let right = reader.read_bit()?;
            if left && right {
                num_gain_element_lists += 1;
            }
            (left, right)
        } else {
            (true, false)
        };
        targets.push(CoupledTarget {
            is_cpe: target_is_cpe,
            tag: target_tag,
            left,
            right,
        });
    }
    let cc_domain = reader.read_bit()?;
    if cc_domain {
        return Err(Error::Unsupported(
            "dependent channel coupling (cc_domain == 1) is not supported",
        ));
    }
    let sign = reader.read_bit()?;
    let _gain_element_scale = reader.read_bits(2)?;
    let stream = parse_individual_channel_stream(reader, ctx, false, None)?;
    if stream.is_used {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "intensity stereo in a coupling channel element",
        });
    }
    // The gain lists follow the targets in order; a CPE
    // target with both sides set consumes two consecutive
    // lists, left first.
    let mut gains = Vec::new();
    for _ in 1..num_gain_element_lists {
        let common_gain_element = if ind_sw_cce_flag {
            true
        } else {
            reader.read_bit()?
        };
        if common_gain_element {
            let delta = decode_scale_factor(reader)?;
            let gain = gain_from_delta(delta, sign);
            gains.push(GainList {
                common: Some(gain),
                per_band: [[0.0; 64]; 8],
            });
        } else {
            let mut per_band = [[0.0f32; 64]; 8];
            let mut gain_sf: i32 = 0;
            for (cb_row, band_row) in stream
                .sfb_cb
                .iter()
                .zip(per_band.iter_mut())
                .take(stream.ics.num_window_groups as usize)
            {
                for (cb, band) in cb_row
                    .iter()
                    .zip(band_row.iter_mut())
                    .take(stream.ics.max_sfb as usize)
                {
                    if *cb != ZERO_HCB {
                        gain_sf += decode_scale_factor(reader)?;
                        *band = gain_from_delta(gain_sf, sign);
                    }
                }
            }
            gains.push(GainList {
                common: None,
                per_band,
            });
        }
    }
    Ok(Coupling {
        spec: stream.spec.clone(),
        ics: stream.ics,
        sfb_cb: stream.sfb_cb,
        targets,
        gains,
    })
}

/// Converts a coupling gain code value to a linear gain. The
/// value is DPCM-coded from 0 (like a scalefactor) and the
/// sign bit negates the whole gain; the 2-bit
/// `gain_element_scale` is an encoder resolution choice that
/// this decoder treats as 0, which is the value encoders
/// use in practice. This is an approximation, documented
/// here because no conformance fixture exercises it.
fn gain_from_delta(gain_sf: i32, sign: bool) -> f32 {
    let gain = 2f32.powf(gain_sf as f32 * 0.25);
    if sign {
        -gain
    } else {
        gain
    }
}

/// Applies one coupling element to its targets: each target
/// spectrum gains the coupling spectrum scaled by its gain
/// list. A common gain scales the whole spectrum; a per-band
/// list scales each band of the coupling channel, using the
/// coupling channel's own band offsets.
fn apply_coupling(channels: &mut [DecodedChannel], coupling: &Coupling) {
    // A per-band gain list becomes a per-sample gain array in
    // the coupling channel's band coordinates.
    let frame_length = coupling.spec.len();
    let mut per_sample_gain: Option<Vec<f32>> = None;
    for (list_index, target) in coupling.targets.iter().enumerate() {
        let Some(gain_list) = coupling.gains.get(list_index) else {
            continue;
        };
        let gain_array = if let Some(common) = gain_list.common {
            vec![common; frame_length]
        } else {
            let gains = per_sample_gain.get_or_insert_with(|| {
                let mut array = vec![0.0f32; frame_length];
                for group in 0..coupling.ics.num_window_groups as usize {
                    for sfb in 0..coupling.ics.max_sfb as usize {
                        if coupling.sfb_cb[group][sfb] == ZERO_HCB {
                            continue;
                        }
                        let start = coupling.ics.sect_sfb_offset[group][sfb] as usize;
                        let end = coupling.ics.sect_sfb_offset[group][sfb + 1] as usize;
                        for sample in array.iter_mut().take(end.min(frame_length)).skip(start) {
                            *sample = gain_list.per_band[group][sfb];
                        }
                    }
                }
                array
            });
            gains.clone()
        };
        for channel in channels.iter_mut() {
            let is_target = if target.is_cpe {
                channel.is_cpe
                    && channel.tag == target.tag
                    && ((target.left && channel.is_left) || (target.right && !channel.is_left))
            } else {
                !channel.is_cpe && channel.tag == target.tag
            };
            if !is_target {
                continue;
            }
            for (index, sample) in channel.stream.spec.iter_mut().enumerate() {
                *sample += gain_array[index] * coupling.spec[index];
            }
        }
    }
}

/// Skips a data stream element (ISO/IEC 14496-3 section
/// 4.5.2.3.3): a tag, a byte-alignment flag, a byte count
/// (255 means one more count byte follows), then that many
/// bytes of opaque data.
fn parse_data_stream_element(reader: &mut BitReader) -> Result<()> {
    let _tag = reader.read_bits(4)?;
    let byte_aligned = reader.read_bit()?;
    let mut count = reader.read_bits(8)? as usize;
    if count == 255 {
        count += reader.read_bits(8)? as usize;
    }
    if byte_aligned {
        align_to_byte(reader)?;
    }
    skip_bits(reader, count * 8)
}

/// Parses a program config element (ISO/IEC 14496-3 section
/// 4.5.2.3.3) and discards it: the channel layout comes
/// from the `AudioSpecificConfig`, not from a PCE in the
/// stream, so only the bit consumption matters here.
fn parse_program_config_element(reader: &mut BitReader) -> Result<()> {
    let _tag = reader.read_bits(4)?;
    let _profile = reader.read_bits(2)?;
    let _sf_index = reader.read_bits(4)?;
    let num_front = reader.read_bits(4)? as usize;
    let num_side = reader.read_bits(4)? as usize;
    let num_back = reader.read_bits(4)? as usize;
    let num_lfe = reader.read_bits(2)? as usize;
    let num_assoc = reader.read_bits(3)? as usize;
    let num_valid_cc = reader.read_bits(4)? as usize;
    if reader.read_bit()? {
        let _mono_mixdown = reader.read_bits(4)?;
    }
    if reader.read_bit()? {
        let _stereo_mixdown = reader.read_bits(4)?;
    }
    if reader.read_bit()? {
        let _matrix_mixdown_idx = reader.read_bits(2)?;
        let _pseudo_surround = reader.read_bit()?;
    }
    for _ in 0..(num_front + num_side + num_back) {
        let _is_cpe = reader.read_bit()?;
        let _tag_select = reader.read_bits(4)?;
    }
    for _ in 0..num_lfe {
        let _tag_select = reader.read_bits(4)?;
    }
    for _ in 0..num_assoc {
        let _tag_select = reader.read_bits(4)?;
    }
    for _ in 0..num_valid_cc {
        let _is_ind_sw = reader.read_bit()?;
        let _tag_select = reader.read_bits(4)?;
    }
    align_to_byte(reader)?;
    let comment_bytes = reader.read_bits(8)? as usize;
    skip_bits(reader, comment_bytes * 8)
}

/// Skips a fill element (ISO/IEC 14496-3 section 4.5.2.3.3).
/// The payload is `count` bytes starting with a 4-bit
/// extension type; SBR extensions are refused because this
/// decoder is AAC-LC only, everything else is skipped.
fn parse_fill_element(reader: &mut BitReader) -> Result<()> {
    let mut count = reader.read_bits(4)? as usize;
    if count == 15 {
        count += reader.read_bits(8)? as usize - 1;
    }
    if count == 0 {
        return Ok(());
    }
    let extension_type = reader.read_bits(4)? as u8;
    if extension_type == EXT_SBR_DATA || extension_type == EXT_SBR_DATA_CRC {
        return Err(Error::Unsupported("SBR extension (HE-AAC) is not decoded"));
    }
    skip_bits(reader, count * 8 - 4)
}

/// Moves the reader to the next byte boundary, discarding
/// the bits in between.
fn align_to_byte(reader: &mut BitReader) -> Result<()> {
    let remainder = reader.bit_position() % 8;
    if remainder != 0 {
        skip_bits(reader, 8 - remainder)?;
    }
    Ok(())
}

/// Discards `count` bits, in bounded chunks so a hostile
/// count cannot loop on a single huge read.
fn skip_bits(reader: &mut BitReader, count: usize) -> Result<()> {
    let mut remaining = count;
    while remaining > 0 {
        let step = remaining.min(32);
        reader.read_bits(step as u32)?;
        remaining -= step;
    }
    Ok(())
}

// =====================================================================
// Individual channel stream (ISO/IEC 14496-3 section 4.5.2.3.2).
// =====================================================================

/// ICS info: window sequence and shape, the scalefactor-band
/// layout for this block, and the section-to-sample offsets.
#[derive(Debug, Clone, Copy)]
struct IcsInfo {
    window_sequence: u8,
    window_shape: u8,
    max_sfb: u8,
    num_windows: u8,
    num_window_groups: u8,
    window_group_length: [u8; 8],
    num_swb: u8,
    swb_offset: [u16; 52],
    swb_offset_max: u16,
    sect_sfb_offset: [[u16; 52]; 8],
}

impl Default for IcsInfo {
    fn default() -> Self {
        Self {
            window_sequence: 0,
            window_shape: 0,
            max_sfb: 0,
            num_windows: 0,
            num_window_groups: 0,
            window_group_length: [0; 8],
            num_swb: 0,
            swb_offset: [0; 52],
            swb_offset_max: 0,
            sect_sfb_offset: [[0; 52]; 8],
        }
    }
}

/// One TNS filter (ISO/IEC 14496-3 section 4.6.3.4): the
/// band it covers and its coefficient indices.
#[derive(Debug, Clone, Copy, Default)]
struct TnsFilter {
    length: u8,
    order: u8,
    direction: bool,
    coef_compress: bool,
    coef: [u8; 20],
}

/// A fully parsed channel stream: the quantized spectrum and
/// its dequantized form, plus the side info the stereo and
/// TNS stages need.
struct ChannelStream {
    ics: IcsInfo,
    global_gain: u8,
    sfb_cb: [[u8; 64]; 8],
    scale_factors: [[i16; 64]; 8],
    num_sec: [u8; 8],
    sect_cb: [[u8; 128]; 8],
    sect_start: [[u8; 128]; 8],
    sect_end: [[u8; 128]; 8],
    pulse_present: bool,
    pulse_start_sfb: u8,
    pulse_number: u8,
    pulse_offsets: [u8; 4],
    pulse_amps: [u8; 4],
    tns_present: bool,
    tns_n_filt: [u8; 8],
    tns_coef_res: [u8; 8],
    tns_filters: [[TnsFilter; 3]; 8],
    quant: Vec<i16>,
    spec: Vec<f32>,
    is_used: bool,
    ms_mask_present: u8,
    ms_used: [[bool; 64]; 8],
}

impl ChannelStream {
    fn new(frame_length: u32) -> Self {
        Self {
            ics: IcsInfo::default(),
            global_gain: 0,
            sfb_cb: [[0; 64]; 8],
            scale_factors: [[0; 64]; 8],
            num_sec: [0; 8],
            sect_cb: [[0; 128]; 8],
            sect_start: [[0; 128]; 8],
            sect_end: [[0; 128]; 8],
            pulse_present: false,
            pulse_start_sfb: 0,
            pulse_number: 0,
            pulse_offsets: [0; 4],
            pulse_amps: [0; 4],
            tns_present: false,
            tns_n_filt: [0; 8],
            tns_coef_res: [0; 8],
            tns_filters: [[TnsFilter::default(); 3]; 8],
            quant: vec![0; frame_length as usize],
            spec: vec![0.0; frame_length as usize],
            is_used: false,
            ms_mask_present: 0,
            ms_used: [[false; 64]; 8],
        }
    }
}

/// Parses one individual channel stream: global gain, ICS
/// info (unless the pair shares one), sections, scale
/// factors, pulse data, TNS data, gain control, then the
/// spectral data, which is dequantized before returning.
/// Gain control (the SSR object) is refused: AAC-LC never
/// sets it.
fn parse_individual_channel_stream(
    reader: &mut BitReader,
    ctx: &BlockContext,
    common_window: bool,
    shared_ics: Option<&IcsInfo>,
) -> Result<ChannelStream> {
    let mut stream = ChannelStream::new(ctx.frame_length);
    stream.global_gain = reader.read_bits(8)? as u8;
    stream.ics = if common_window {
        // The pair parsed the shared ICS info before the
        // first channel; this unwrap-free lookup is safe
        // because the caller always passes it here.
        match shared_ics {
            Some(ics) => *ics,
            None => {
                return Err(Error::InvalidArgument(
                    "common window channel stream without a shared ics",
                ));
            }
        }
    } else {
        parse_ics_info(reader, ctx)?
    };
    parse_section_data(reader, &mut stream)?;
    parse_scale_factor_data(reader, &mut stream)?;
    stream.pulse_present = reader.read_bit()?;
    if stream.pulse_present {
        if stream.ics.window_sequence == WINDOW_EIGHT_SHORT {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "pulse data in a short-window channel",
            });
        }
        parse_pulse_data(reader, &mut stream)?;
    }
    stream.tns_present = reader.read_bit()?;
    if stream.tns_present {
        parse_tns_data(reader, &mut stream)?;
    }
    let gain_control_present = reader.read_bit()?;
    if gain_control_present {
        return Err(Error::Unsupported(
            "gain control data (SSR) is not supported in AAC-LC",
        ));
    }
    parse_spectral_data(reader, &mut stream)?;
    apply_pulse(&mut stream)?;
    dequantize(&mut stream);
    Ok(stream)
}

/// Parses ICS info (ISO/IEC 14496-3 section 4.5.2.3.2): the
/// reserved bit, window sequence and shape, max scalefactor
/// band, window grouping, and the predictor flag. Long-term
/// prediction is an AAC-LTP feature, so a set flag is
/// refused rather than mis-parsed as the following data.
fn parse_ics_info(reader: &mut BitReader, ctx: &BlockContext) -> Result<IcsInfo> {
    let mut ics = IcsInfo::default();
    if reader.read_bit()? {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "ics reserved bit is set",
        });
    }
    ics.window_sequence = reader.read_bits(2)? as u8;
    ics.window_shape = reader.read_bits(1)? as u8;
    let grouping = if ics.window_sequence == WINDOW_EIGHT_SHORT {
        ics.max_sfb = reader.read_bits(4)? as u8;
        reader.read_bits(7)?
    } else {
        ics.max_sfb = reader.read_bits(6)? as u8;
        0
    };
    window_grouping(&mut ics, grouping, ctx);
    if ics.max_sfb > ics.num_swb {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "max_sfb exceeds the scalefactor band count",
        });
    }
    if ics.window_sequence != WINDOW_EIGHT_SHORT {
        let predictor_present = reader.read_bit()?;
        if predictor_present {
            return Err(Error::Unsupported(
                "long-term prediction (LTP) is not supported in AAC-LC",
            ));
        }
    }
    Ok(ics)
}

/// Builds the window grouping and scalefactor-band offsets
/// for an ICS info block (ISO/IEC 14496-3 section
/// 4.5.2.3.2). A 960-sample frame uses the 960-band count
/// with the 1024 offsets truncated at the frame length,
/// which is how the reference decoder handles it.
fn window_grouping(ics: &mut IcsInfo, grouping: u32, ctx: &BlockContext) {
    let sf_index = ctx.sf_index as usize;
    let frame_length = ctx.frame_length;
    if ics.window_sequence == WINDOW_EIGHT_SHORT {
        ics.num_windows = 8;
        ics.num_swb = NUM_SWB_128[sf_index];
        let table = SWB_OFFSET_128_WINDOW[sf_index];
        ics.swb_offset[..ics.num_swb as usize].copy_from_slice(&table[..ics.num_swb as usize]);
        ics.swb_offset[ics.num_swb as usize] = (frame_length / 8) as u16;
        ics.swb_offset_max = (frame_length / 8) as u16;
        // The grouping bits run from the second window
        // onwards: a clear bit starts a new group.
        ics.num_window_groups = 0;
        let mut window = 0;
        while window < 8 {
            if window == 0 || (grouping >> (6 - (window - 1))) & 1 == 0 {
                ics.window_group_length[ics.num_window_groups as usize] = 1;
                ics.num_window_groups += 1;
            } else {
                let last = (ics.num_window_groups - 1) as usize;
                ics.window_group_length[last] += 1;
            }
            window += 1;
        }
        for group in 0..ics.num_window_groups as usize {
            let mut offset: u16 = 0;
            for sfb in 0..ics.num_swb as usize {
                let width = if sfb + 1 == ics.num_swb as usize {
                    (frame_length / 8) as u16 - table[sfb]
                } else {
                    table[sfb + 1] - table[sfb]
                };
                let width = width * u16::from(ics.window_group_length[group]);
                ics.sect_sfb_offset[group][sfb] = offset;
                offset += width;
            }
            ics.sect_sfb_offset[group][ics.num_swb as usize] = offset;
        }
    } else {
        ics.num_windows = 1;
        ics.num_window_groups = 1;
        ics.window_group_length[0] = 1;
        ics.num_swb = if frame_length == 1024 {
            NUM_SWB_1024[sf_index]
        } else {
            NUM_SWB_960[sf_index]
        };
        let table = SWB_OFFSET_1024_WINDOW[sf_index];
        ics.swb_offset[..ics.num_swb as usize].copy_from_slice(&table[..ics.num_swb as usize]);
        ics.swb_offset[ics.num_swb as usize] = frame_length as u16;
        ics.swb_offset_max = frame_length as u16;
        ics.sect_sfb_offset[0][..=ics.num_swb as usize]
            .copy_from_slice(&ics.swb_offset[..=ics.num_swb as usize]);
    }
}

/// Parses section data (ISO/IEC 14496-3 section 4.5.2.3.2):
/// per window group, a run of (codebook, length) pairs that
/// must cover exactly `max_sfb` bands. Codebook 12 is
/// reserved; codebook 13 is perceptual noise substitution,
/// which this decoder refuses instead of filling the band
/// with shaped noise.
fn parse_section_data(reader: &mut BitReader, stream: &mut ChannelStream) -> Result<()> {
    let is_short = stream.ics.window_sequence == WINDOW_EIGHT_SHORT;
    let sect_bits = if is_short { 3 } else { 5 };
    let sect_esc_val = (1u32 << sect_bits) - 1;
    let max_sfb = stream.ics.max_sfb as usize;
    for group in 0..stream.ics.num_window_groups as usize {
        let mut sfb = 0usize;
        let mut section = 0usize;
        while sfb < max_sfb {
            if section >= 128 {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "too many sections in a window group",
                });
            }
            let sect_cb = reader.read_bits(4)? as u8;
            if sect_cb == 12 {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "section codebook 12 is reserved",
                });
            }
            if sect_cb == INTENSITY_HCB || sect_cb == INTENSITY_HCB2 {
                stream.is_used = true;
            }
            let mut sect_len = 0usize;
            loop {
                let increment = reader.read_bits(sect_bits)? as usize;
                sect_len += increment;
                if increment as u32 != sect_esc_val {
                    break;
                }
                if sect_len > max_sfb {
                    return Err(Error::InvalidData {
                        offset: reader.bit_position() / 8,
                        detail: "section length escapes past max_sfb",
                    });
                }
            }
            if sect_len > max_sfb - sfb {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "section length exceeds the remaining bands",
                });
            }
            stream.sect_cb[group][section] = sect_cb;
            stream.sect_start[group][section] = sfb as u8;
            stream.sect_end[group][section] = (sfb + sect_len) as u8;
            for band in sfb..(sfb + sect_len) {
                stream.sfb_cb[group][band] = sect_cb;
            }
            sfb += sect_len;
            section += 1;
        }
        stream.num_sec[group] = section as u8;
    }
    Ok(())
}

/// Parses scale factor data (ISO/IEC 14496-3 section
/// 4.5.2.3.2): the scalefactors are DPCM-coded from the
/// global gain, intensity positions accumulate from 0, and
/// a zero codebook band carries no bits and a factor of 0.
/// A scalefactor outside 0..=255 is corrupt input.
fn parse_scale_factor_data(reader: &mut BitReader, stream: &mut ChannelStream) -> Result<()> {
    let max_sfb = stream.ics.max_sfb as usize;
    let mut scale_factor: i32 = i32::from(stream.global_gain);
    let mut is_position: i32 = 0;
    // Noise energy starts at global_gain - 90; the first
    // noise band carries a 9-bit PCM value (minus 256),
    // later noise bands carry Huffman deltas.
    let mut noise_energy: i32 = i32::from(stream.global_gain) - 90;
    let mut noise_pcm_flag = true;
    for group in 0..stream.ics.num_window_groups as usize {
        for sfb in 0..max_sfb {
            let codebook = stream.sfb_cb[group][sfb];
            let value = match codebook {
                ZERO_HCB => 0,
                INTENSITY_HCB | INTENSITY_HCB2 => {
                    is_position += decode_scale_factor(reader)?;
                    if !(-255..=255).contains(&is_position) {
                        return Err(Error::InvalidData {
                            offset: reader.bit_position() / 8,
                            detail: "intensity position out of range",
                        });
                    }
                    is_position
                }
                NOISE_HCB => {
                    if noise_pcm_flag {
                        noise_pcm_flag = false;
                        noise_energy += reader.read_bits(9)? as i32 - 256;
                    } else {
                        noise_energy += decode_scale_factor(reader)?;
                    }
                    if !(-1000..=1000).contains(&noise_energy) {
                        return Err(Error::InvalidData {
                            offset: reader.bit_position() / 8,
                            detail: "noise energy out of range",
                        });
                    }
                    noise_energy
                }
                _ => {
                    scale_factor += decode_scale_factor(reader)?;
                    if !(0..=255).contains(&scale_factor) {
                        return Err(Error::InvalidData {
                            offset: reader.bit_position() / 8,
                            detail: "scalefactor out of range",
                        });
                    }
                    scale_factor
                }
            };
            stream.scale_factors[group][sfb] = value as i16;
        }
    }
    Ok(())
}

/// Parses pulse data (ISO/IEC 14496-3 section 4.5.2.3.2):
/// the number of pulses, the starting band, and per pulse
/// an offset and an amplitude.
fn parse_pulse_data(reader: &mut BitReader, stream: &mut ChannelStream) -> Result<()> {
    stream.pulse_number = reader.read_bits(2)? as u8;
    stream.pulse_start_sfb = reader.read_bits(6)? as u8;
    if stream.pulse_start_sfb >= stream.ics.num_swb {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "pulse start band exceeds the band count",
        });
    }
    for index in 0..=(stream.pulse_number as usize) {
        stream.pulse_offsets[index] = reader.read_bits(5)? as u8;
        stream.pulse_amps[index] = reader.read_bits(4)? as u8;
    }
    Ok(())
}

/// Parses TNS data (ISO/IEC 14496-3 section 4.6.3.4): per
/// window, a filter count and resolution, then per filter a
/// length, an order, a direction, a compression flag and the
/// coefficient indices. The order is capped at 20, the
/// largest order the TNS tables support.
fn parse_tns_data(reader: &mut BitReader, stream: &mut ChannelStream) -> Result<()> {
    let is_short = stream.ics.window_sequence == WINDOW_EIGHT_SHORT;
    let n_filt_bits = if is_short { 1 } else { 2 };
    let length_bits = if is_short { 4 } else { 6 };
    let order_bits = if is_short { 3 } else { 5 };
    for window in 0..stream.ics.num_windows as usize {
        let n_filt = reader.read_bits(n_filt_bits)? as u8;
        stream.tns_n_filt[window] = n_filt;
        if n_filt == 0 {
            continue;
        }
        let coef_res = u8::from(reader.read_bit()?);
        stream.tns_coef_res[window] = coef_res;
        for filter_index in 0..n_filt as usize {
            let length = reader.read_bits(length_bits)? as u8;
            let order = reader.read_bits(order_bits)? as u8;
            if order > 20 {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "tns filter order exceeds the supported maximum",
                });
            }
            let mut filter = TnsFilter {
                length,
                order,
                direction: false,
                coef_compress: false,
                coef: [0; 20],
            };
            if order > 0 {
                filter.direction = reader.read_bit()?;
                filter.coef_compress = reader.read_bit()?;
                let coef_bits = (u32::from(coef_res) + 3) - u32::from(filter.coef_compress);
                for index in 0..order as usize {
                    filter.coef[index] = reader.read_bits(coef_bits)? as u8;
                }
            }
            stream.tns_filters[window][filter_index] = filter;
        }
    }
    Ok(())
}

/// Parses spectral data (ISO/IEC 14496-3 section 4.5.2.3.2):
/// one Huffman codeword per 4 (quad books 1-4) or 2 (pair
/// books 5-11) quantized values, placed into the spectrum
/// through the section offsets. Zero, noise and intensity
/// sections carry no values here.
fn parse_spectral_data(reader: &mut BitReader, stream: &mut ChannelStream) -> Result<()> {
    let frame_length = stream.quant.len();
    let nshort = frame_length / 8;
    let mut group_base = 0usize;
    for group in 0..stream.ics.num_window_groups as usize {
        let mut position = group_base;
        for section in 0..stream.num_sec[group] as usize {
            let codebook = stream.sect_cb[group][section];
            let start = stream.ics.sect_sfb_offset[group]
                [stream.sect_start[group][section] as usize] as usize;
            let end = stream.ics.sect_sfb_offset[group][stream.sect_end[group][section] as usize]
                as usize;
            if codebook == ZERO_HCB
                || codebook == NOISE_HCB
                || codebook == INTENSITY_HCB
                || codebook == INTENSITY_HCB2
            {
                position += end - start;
                continue;
            }
            let increment = if huffman_is_quad(codebook) { 4 } else { 2 };
            let mut sample = start;
            while sample < end {
                if position + increment > frame_length {
                    return Err(Error::InvalidData {
                        offset: reader.bit_position() / 8,
                        detail: "spectral data runs past the frame length",
                    });
                }
                let values = decode_spectral(reader, codebook)?;
                stream.quant[position..position + increment].copy_from_slice(&values[..increment]);
                position += increment;
                sample += increment;
            }
        }
        group_base += usize::from(stream.ics.window_group_length[group]) * nshort;
    }
    Ok(())
}

/// Applies pulse data to the quantized spectrum: each pulse
/// adds its amplitude to one quantized value, in the
/// direction of that value's sign (ISO/IEC 14496-3 section
/// 4.6.3.3).
fn apply_pulse(stream: &mut ChannelStream) -> Result<()> {
    if !stream.pulse_present {
        return Ok(());
    }
    let frame_length = stream.quant.len();
    let mut position = usize::min(
        stream.ics.swb_offset[stream.pulse_start_sfb as usize] as usize,
        stream.ics.swb_offset_max as usize,
    );
    for index in 0..=(stream.pulse_number as usize) {
        position += stream.pulse_offsets[index] as usize;
        if position >= frame_length {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "pulse position runs past the frame length",
            });
        }
        let amp = i16::from(stream.pulse_amps[index]);
        if stream.quant[position] > 0 {
            stream.quant[position] += amp;
        } else {
            stream.quant[position] -= amp;
        }
    }
    Ok(())
}

/// Dequantizes the spectrum (ISO/IEC 14496-3 section
/// 4.6.3.1): each quantized value becomes
/// `sign(q) * |q|^(4/3) * 2^(sf/4)`, with intensity bands
/// using a scalefactor of 0 because their right channel is
/// rebuilt from the left one later.
fn dequantize(stream: &mut ChannelStream) {
    let win_inc = stream.ics.swb_offset[stream.ics.num_swb as usize] as usize;
    let mut quant_index = 0usize;
    let mut group_index = 0usize;
    for group in 0..stream.ics.num_window_groups as usize {
        let mut band_offset = 0usize;
        let mut group_increase = 0usize;
        // This loop must run to `num_swb`, not `max_sfb`:
        // the reference decoder's `quant_to_spec` loops over
        // every scalefactor band, and the bands above
        // `max_sfb` carry zero quantized values but still
        // advance `group_index` by a full window. Stopping
        // at `max_sfb` leaves `group_index` short, so every
        // later window group is written too low in the
        // spectrum and the last windows read as silence.
        for sfb in 0..stream.ics.num_swb as usize {
            let width = (stream.ics.swb_offset[sfb + 1] - stream.ics.swb_offset[sfb]) as usize;
            let codebook = stream.sfb_cb[group][sfb];
            let scale_factor = if codebook == INTENSITY_HCB || codebook == INTENSITY_HCB2 {
                0
            } else {
                stream.scale_factors[group][sfb]
            };
            // The standard's dequantization scale is
            // 2^(sf/4 - 25): the -25 comes from the
            // scalefactor table's origin (the reference
            // decoder's `pow2sf_tab` starts at 2^-25, and
            // its fixed-point path subtracts 100 from the
            // scalefactor for the same reason). The remaining
            // 2^-15 of the overall level lives in the IMDCT
            // (see `imdct`), not here, so that PNS bands —
            // which bypass dequantization — stay consistent
            // with coded bands.
            let scale = 2f32.powf(f32::from(scale_factor) * 0.25 - 25.0);
            let mut window_offset = group_index + band_offset;
            for _window in 0..stream.ics.window_group_length[group] as usize {
                let mut bin = 0usize;
                while bin < width {
                    for step in 0..4 {
                        let target = window_offset + bin + step;
                        let value = stream.quant[quant_index + step];
                        stream.spec[target] = iquant(value) * scale;
                    }
                    quant_index += 4;
                    group_increase += 4;
                    bin += 4;
                }
                window_offset += win_inc;
            }
            band_offset += width;
        }
        group_index += group_increase;
    }
}

/// Inverse quantization of one value: `sign(q) * |q|^(4/3)`.
fn iquant(value: i16) -> f32 {
    let magnitude = (i32::from(value).unsigned_abs() as f32).powf(4.0 / 3.0);
    if value < 0 {
        -magnitude
    } else {
        magnitude
    }
}

// =====================================================================
// Stereo reconstruction, TNS and the filter bank.
// =====================================================================

/// MS stereo decoding (ISO/IEC 14496-3 section 4.6.3.2):
/// bands marked mid/side (or all bands when the mask is 2)
/// are converted back with `L = M + S`, `R = M - S`, with no
/// scaling. Intensity and noise bands are left alone.
fn apply_ms_stereo(left: &mut ChannelStream, right: &mut ChannelStream) {
    if left.ms_mask_present == 0 {
        return;
    }
    let frame_length = left.spec.len();
    let nshort = frame_length / 8;
    let mut group = 0usize;
    for g in 0..left.ics.num_window_groups as usize {
        for sfb in 0..left.ics.max_sfb as usize {
            if !left.ms_used[g][sfb] && left.ms_mask_present != 2 {
                continue;
            }
            let codebook = left.sfb_cb[g][sfb];
            if codebook == INTENSITY_HCB || codebook == INTENSITY_HCB2 || codebook == NOISE_HCB {
                continue;
            }
            let start = left.ics.swb_offset[sfb] as usize;
            let end = usize::min(
                left.ics.swb_offset[sfb + 1] as usize,
                left.ics.swb_offset_max as usize,
            );
            for window in 0..left.ics.window_group_length[g] as usize {
                for index in start..end {
                    let position = (group + window) * nshort + index;
                    let mid = left.spec[position];
                    let side = right.spec[position];
                    left.spec[position] = mid + side;
                    right.spec[position] = mid - side;
                }
            }
        }
        group += left.ics.window_group_length[g] as usize;
    }
}

/// Intensity stereo decoding (ISO/IEC 14496-3 section
/// 4.6.3.3): the right channel of an intensity band is
/// rebuilt from the left channel, scaled by
/// `0.5^(0.25 * is_position)` with the position clamped to
/// -120..=120, and negated when the intensity sign disagrees
/// with the MS mask for that band.
fn apply_intensity_stereo(left: &mut ChannelStream, right: &mut ChannelStream) {
    if !right.is_used {
        return;
    }
    let frame_length = right.spec.len();
    let nshort = frame_length / 8;
    let mut group = 0usize;
    for g in 0..right.ics.num_window_groups as usize {
        for window in 0..right.ics.window_group_length[g] as usize {
            for sfb in 0..right.ics.max_sfb as usize {
                let codebook = right.sfb_cb[g][sfb];
                if codebook != INTENSITY_HCB && codebook != INTENSITY_HCB2 {
                    continue;
                }
                let is_intensity: i32 = if codebook == INTENSITY_HCB2 { 1 } else { -1 };
                let invert: i32 = if left.ms_mask_present == 1 {
                    1 - 2 * i32::from(left.ms_used[g][sfb])
                } else {
                    1
                };
                let position = right.scale_factors[g][sfb].clamp(-120, 120);
                let scale = 0.5f32.powf(0.25 * f32::from(position));
                let start = right.ics.swb_offset[sfb] as usize;
                let end = usize::min(
                    right.ics.swb_offset[sfb + 1] as usize,
                    left.ics.swb_offset_max as usize,
                );
                for index in start..end {
                    let at = (group + window) * nshort + index;
                    let value = left.spec[at] * scale;
                    right.spec[at] = if is_intensity == invert {
                        value
                    } else {
                        -value
                    };
                }
            }
        }
        // `group` is the absolute index of the group's first
        // window, so it advances by the group's length (the
        // MS decoder above does the same); advancing it per
        // window would double-count `window` above.
        group += right.ics.window_group_length[g] as usize;
    }
}

/// The PNS pseudo-random generator state (ISO/IEC 14496-3
/// section 4.6.3.6). This is the linear congruential
/// generator the reference decoder (ffmpeg's
/// `aacdec_proc_template.c`, `lcg_random`) uses, not the
/// two-state generator of the FAAD2 reference: the state is
/// a single 32-bit word, advanced by `* 1664525 +
/// 1013904223` (mod 2^32), seeded once with `0x1f2e3d4c`
/// when the decoder is created, and shared by every channel
/// and frame — it is never reset between frames. The
/// sequence itself only shapes the noise, since each
/// generated vector is normalized to its band energy, but
/// matching the reference generator sample-for-sample is
/// what makes PNS bands bit-comparable with the reference
/// decoder's output.
#[derive(Debug, Clone, Copy)]
struct PnsRng {
    state: u32,
}

impl PnsRng {
    fn new() -> Self {
        Self { state: 0x1f2e_3d4c }
    }

    /// One step of the linear congruential generator.
    /// Returns the new state as raw bits; callers interpret
    /// them as a signed 32-bit integer, exactly as the
    /// reference decoder assigns the generator's signed
    /// result to a float.
    fn next(&mut self) -> u32 {
        self.state = self
            .state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        self.state
    }
}

/// Fills one noise band with pseudo-random values, then
/// normalizes the vector so its energy equals
/// `2^(sf / 2)` for the band's noise energy `sf` (clamped
/// to -120..=120). Because of the normalization, the band
/// energy is exact no matter which values the generator
/// produced; only the per-sample phases depend on it.
fn gen_rand_vector(
    spec: &mut [f32],
    start: usize,
    end: usize,
    scale_factor: i16,
    rng: &mut PnsRng,
) {
    let size = end - start;
    if size == 0 {
        return;
    }
    let mut energy: f32 = 0.0;
    for index in 0..size {
        let value = rng.next() as i32 as f32;
        spec[start + index] = value;
        energy += value * value;
    }
    if energy > 0.0 {
        let scale =
            (1.0 / energy.sqrt()) * 2f32.powf(0.25 * f32::from(scale_factor.clamp(-120, 120)));
        for index in 0..size {
            spec[start + index] *= scale;
        }
    }
}

/// Fills the noise bands of one channel. The order matches
/// the reference decoder's `decode_spectrum_and_dequant`:
/// for each window group, for each scalefactor band, then
/// for each window in the group — a noise band's values are
/// generated window by window, and each window's vector is
/// normalized on its own. The generator state advances in
/// that order, so a different nesting would assign the
/// reference sequence to the wrong bands.
fn pns_fill_channel(stream: &mut ChannelStream, rng: &mut PnsRng) {
    let frame_length = stream.spec.len();
    let nshort = frame_length / 8;
    let mut window_base = 0usize;
    for group in 0..stream.ics.num_window_groups as usize {
        for sfb in 0..stream.ics.max_sfb as usize {
            if stream.sfb_cb[group][sfb] != NOISE_HCB {
                continue;
            }
            for win in 0..stream.ics.window_group_length[group] as usize {
                let base = (window_base + win) * nshort;
                let begin = usize::min(base + stream.ics.swb_offset[sfb] as usize, frame_length);
                let end = usize::min(base + stream.ics.swb_offset[sfb + 1] as usize, frame_length);
                gen_rand_vector(
                    &mut stream.spec,
                    begin,
                    end,
                    stream.scale_factors[group][sfb],
                    rng,
                );
            }
        }
        window_base += usize::from(stream.ics.window_group_length[group]);
    }
}

/// Perceptual noise substitution for a single channel
/// (ISO/IEC 14496-3 section 4.6.3.6): every band coded
/// with codebook 13 is filled with shaped noise instead of
/// decoded values.
fn apply_pns_single(stream: &mut ChannelStream, rng: &mut PnsRng) {
    pns_fill_channel(stream, rng);
}

/// Perceptual noise substitution for a channel pair. The
/// reference decoder generates each channel's noise
/// independently from the shared generator state — the left
/// channel's bands first, then the right channel's, with
/// the state advancing throughout — and does not re-use the
/// left channel's sequence for the right one, so this
/// decoder does the same. (The standard suggests correlated
/// noise when both channels code a band as noise under an
/// MS mask, but the reference decoder's output — the oracle
/// for the conformance tests — is the uncorrelated form.)
fn apply_pns_pair(left: &mut ChannelStream, right: &mut ChannelStream, rng: &mut PnsRng) {
    pns_fill_channel(left, rng);
    pns_fill_channel(right, rng);
}

/// TNS decoding (ISO/IEC 14496-3 section 4.6.3.4): the
/// transmitted reflection coefficients are converted to
/// LPC coefficients by Levinson recursion, then each
/// filter runs as an autoregressive filter over its band,
/// forwards or backwards according to its direction bit.
fn apply_tns(stream: &mut ChannelStream, sf_index: u8) {
    if !stream.tns_present {
        return;
    }
    let frame_length = stream.spec.len();
    let nshort = frame_length / 8;
    let is_short = stream.ics.window_sequence == WINDOW_EIGHT_SHORT;
    let (max_long, max_short) = MAX_TNS_SFB_LC[sf_index as usize];
    let max_tns_sfb = if is_short { max_short } else { max_long };
    for window in 0..stream.ics.num_windows as usize {
        // `bottom` restarts at the top scalefactor band for
        // every window (the reference decoder sets it inside
        // its window loop); leaving it running across windows
        // makes the filter ranges of windows 1..7 wrong,
        // which corrupts every EIGHT_SHORT frame. It must be
        // reset before the `n_filt == 0` skip, so a filterless
        // window still resets it for the next window.
        let mut bottom = stream.ics.num_swb;
        let n_filt = stream.tns_n_filt[window];
        if n_filt == 0 {
            continue;
        }
        let coef_res = stream.tns_coef_res[window];
        for filter_index in 0..n_filt as usize {
            let filter = stream.tns_filters[window][filter_index];
            let top = bottom;
            bottom = top.saturating_sub(filter.length);
            let order = usize::from(filter.order);
            if order == 0 {
                continue;
            }
            let start_sfb = usize::from(bottom.min(max_tns_sfb).min(stream.ics.max_sfb));
            let end_sfb = usize::from(top.min(max_tns_sfb).min(stream.ics.max_sfb));
            let start = usize::min(
                stream.ics.swb_offset[start_sfb] as usize,
                stream.ics.swb_offset_max as usize,
            );
            let end = usize::min(
                stream.ics.swb_offset[end_sfb] as usize,
                stream.ics.swb_offset_max as usize,
            );
            if end <= start {
                continue;
            }
            let lpc = tns_decode_coef(filter, coef_res);
            tns_ar_filter(
                &mut stream.spec,
                window * nshort,
                start,
                end,
                filter.direction,
                &lpc,
            );
        }
    }
}

/// Converts TNS reflection coefficients to LPC
/// coefficients. The table is chosen by the resolution bit
/// and the compression flag (ISO/IEC 14496-3 Table 4.6.8),
/// then Levinson recursion builds the predictor.
fn tns_decode_coef(filter: TnsFilter, coef_res: u8) -> [f32; 21] {
    let table_index = 2 * usize::from(filter.coef_compress) + usize::from(coef_res != 0);
    let table = match table_index {
        0 => TNS_COEF_0_3,
        1 => TNS_COEF_0_4,
        2 => TNS_COEF_1_3,
        _ => TNS_COEF_1_4,
    };
    let order = filter.order as usize;
    let mut lpc = [0.0f32; 21];
    let mut tmp = [0.0f32; 20];
    for index in 0..order {
        tmp[index] = table[filter.coef[index] as usize];
    }
    lpc[0] = 1.0;
    for m in 1..=order {
        lpc[m] = tmp[m - 1];
        for i in 1..m {
            lpc[i] += tmp[m - 1] * lpc[m - i];
        }
    }
    lpc
}

/// One TNS autoregressive filter pass over `spec` between
/// `start` and `end` (absolute spectrum indices within one
/// window starting at `window_base`). A forward filter
/// walks up from `start`, a backward filter walks down
/// from `end - 1`; each output replaces its input, with a
/// ring buffer of the previous outputs.
fn tns_ar_filter(
    spec: &mut [f32],
    window_base: usize,
    start: usize,
    end: usize,
    backward: bool,
    lpc: &[f32; 21],
) {
    let order = lpc.len() - 1;
    let size = end - start;
    let mut state = [0.0f32; 40];
    let mut state_index = 0usize;
    if backward {
        let mut index = end;
        for _ in 0..size {
            index -= 1;
            let at = window_base + index;
            let mut value = spec[at];
            for j in 0..order {
                value -= lpc[j + 1] * state[state_index + j];
            }
            spec[at] = value;
            state[state_index] = value;
            state[state_index + order] = value;
            state_index = if state_index == 0 {
                order - 1
            } else {
                state_index - 1
            };
        }
    } else {
        for index in start..end {
            let at = window_base + index;
            let mut value = spec[at];
            for j in 0..order {
                value -= lpc[j + 1] * state[state_index + j];
            }
            spec[at] = value;
            state[state_index] = value;
            state[state_index + order] = value;
            state_index = if state_index == 0 {
                order - 1
            } else {
                state_index - 1
            };
        }
    }
}

/// Picks the half window for a frame: sine or KBD, long or
/// short, 1024 or 960 samples (ISO/IEC 14496-3 section
/// 4.6.3.1). The `frame_length` is validated in
/// `AacDecoder::new`, so the fallback arm is unreachable
/// in practice.
fn window_table(frame_length: u32, short: bool, shape: u8) -> &'static [f32] {
    match (frame_length, short, shape) {
        (1024, false, 0) => SINE_LONG_1024,
        (1024, false, _) => KBD_LONG_1024,
        (960, false, 0) => SINE_LONG_960,
        (960, false, _) => KBD_LONG_960,
        (1024, true, 0) => SINE_SHORT_128,
        (1024, true, _) => KBD_SHORT_128,
        (960, true, 0) => SINE_SHORT_120,
        (960, true, _) => KBD_SHORT_120,
        _ => SINE_LONG_1024,
    }
}

/// The filter bank (ISO/IEC 14496-3 section 4.6.3.1): one
/// inverse MDCT per window, windowed and overlap-added
/// against the previous frame's tail. The four window
/// sequences differ only in how the two halves are mixed.
/// The IMDCT itself comes from `mdct.rs`; it applies no
/// scale, so the caller multiplies by `2/N` (one over the
/// spectrum length) as the standard requires. Returns the
/// `frame_length` time samples; the overlap tail is written
/// back into `overlap`.
// The four window sequences share one overlap-add body;
// splitting the arms into separate functions would
// duplicate the overlap bookkeeping.
#[allow(clippy::too_many_lines)]
fn filter_bank(
    stream: &ChannelStream,
    overlap: &mut [f32],
    window_shape_prev: u8,
    frame_length: u32,
) -> Result<Vec<f32>> {
    let nlong = frame_length as usize;
    let nshort = nlong / 8;
    let trans = nshort / 2;
    let nflat_ls = (nlong - nshort) / 2;
    let sequence = stream.ics.window_sequence;
    let window_long = window_table(frame_length, false, stream.ics.window_shape);
    let window_long_prev = window_table(frame_length, false, window_shape_prev);
    let window_short = window_table(frame_length, true, stream.ics.window_shape);
    let window_short_prev = window_table(frame_length, true, window_shape_prev);
    let mut output = vec![0.0f32; nlong];
    let mut new_overlap = vec![0.0f32; nlong];

    match sequence {
        WINDOW_ONLY_LONG => {
            let transf = imdct(&stream.spec[..nlong])?;
            for index in 0..nlong {
                output[index] = overlap[index] + transf[index] * window_long_prev[index];
                new_overlap[index] = transf[nlong + index] * window_long[nlong - 1 - index];
            }
        }
        WINDOW_LONG_START => {
            let transf = imdct(&stream.spec[..nlong])?;
            for index in 0..nlong {
                output[index] = overlap[index] + transf[index] * window_long_prev[index];
            }
            new_overlap[..nflat_ls].copy_from_slice(&transf[nlong..nlong + nflat_ls]);
            for index in 0..nshort {
                new_overlap[nflat_ls + index] =
                    transf[nlong + nflat_ls + index] * window_short[nshort - 1 - index];
            }
        }
        WINDOW_EIGHT_SHORT => {
            let mut transf = vec![0.0f32; 2 * nlong];
            for window in 0..8 {
                let block = imdct(&stream.spec[window * nshort..(window + 1) * nshort])?;
                transf[2 * nshort * window..2 * nshort * window + 2 * nshort]
                    .copy_from_slice(&block);
            }
            // The first flat region passes the previous
            // overlap through untouched.
            output[..nflat_ls].copy_from_slice(&overlap[..nflat_ls]);
            // Each short window contributes its first half
            // windowed by the short window and its second
            // half windowed reversed; consecutive windows
            // overlap by one short half. The fifth window's
            // second half straddles the frame end: its first
            // `trans` samples finish this frame, the rest go
            // into the overlap for the next frame.
            for index in 0..nshort {
                output[nflat_ls + index] =
                    overlap[nflat_ls + index] + transf[index] * window_short_prev[index];
                output[nflat_ls + nshort + index] = overlap[nflat_ls + nshort + index]
                    + transf[nshort + index] * window_short[nshort - 1 - index]
                    + transf[2 * nshort + index] * window_short[index];
                output[nflat_ls + 2 * nshort + index] = overlap[nflat_ls + 2 * nshort + index]
                    + transf[3 * nshort + index] * window_short[nshort - 1 - index]
                    + transf[4 * nshort + index] * window_short[index];
                output[nflat_ls + 3 * nshort + index] = overlap[nflat_ls + 3 * nshort + index]
                    + transf[5 * nshort + index] * window_short[nshort - 1 - index]
                    + transf[6 * nshort + index] * window_short[index];
                if index < trans {
                    output[nflat_ls + 4 * nshort + index] = overlap[nflat_ls + 4 * nshort + index]
                        + transf[7 * nshort + index] * window_short[nshort - 1 - index]
                        + transf[8 * nshort + index] * window_short[index];
                }
            }
            for index in 0..nshort {
                if index >= trans {
                    new_overlap[nflat_ls + 4 * nshort + index - nlong] = transf[7 * nshort + index]
                        * window_short[nshort - 1 - index]
                        + transf[8 * nshort + index] * window_short[index];
                }
                new_overlap[nflat_ls + 5 * nshort + index - nlong] = transf[9 * nshort + index]
                    * window_short[nshort - 1 - index]
                    + transf[10 * nshort + index] * window_short[index];
                new_overlap[nflat_ls + 6 * nshort + index - nlong] = transf[11 * nshort + index]
                    * window_short[nshort - 1 - index]
                    + transf[12 * nshort + index] * window_short[index];
                new_overlap[nflat_ls + 7 * nshort + index - nlong] = transf[13 * nshort + index]
                    * window_short[nshort - 1 - index]
                    + transf[14 * nshort + index] * window_short[index];
                new_overlap[nflat_ls + 8 * nshort + index - nlong] =
                    transf[15 * nshort + index] * window_short[nshort - 1 - index];
            }
            for index in 0..nflat_ls {
                new_overlap[nflat_ls + nshort + index] = 0.0;
            }
        }
        WINDOW_LONG_STOP => {
            let transf = imdct(&stream.spec[..nlong])?;
            output[..nflat_ls].copy_from_slice(&overlap[..nflat_ls]);
            for index in 0..nshort {
                output[nflat_ls + index] =
                    overlap[nflat_ls + index] + transf[nflat_ls + index] * window_short_prev[index];
            }
            for index in 0..nflat_ls {
                output[nflat_ls + nshort + index] =
                    overlap[nflat_ls + nshort + index] + transf[nflat_ls + nshort + index];
            }
            for index in 0..nlong {
                new_overlap[index] = transf[nlong + index] * window_long[nlong - 1 - index];
            }
        }
        _ => {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "unknown aac window sequence",
            });
        }
    }
    overlap.copy_from_slice(&new_overlap);
    Ok(output)
}

/// One inverse MDCT with the AAC scale: `mdct_inverse`
/// writes `2N` time samples from `N` spectrum values with
/// no scale, and the standard scales the result by `2/N`
/// (one over the spectrum length here) — plus a further
/// 2^-15. That extra factor is the PCM normalization the
/// reference decoder's float IMDCT carries relative to the
/// plain kernel: its sincos table is scaled by sqrt(N) and
/// applied twice, and its IFFT normalization leaves the
/// float IMDCT 2^15 below the plain `2/N` sum. Without it
/// the decoded output is exactly 32768 times too loud
/// (measured on the mono fixture: `max_abs` 33358 before the
/// correction, 3e-6 after). Keeping the factor here, rather
/// than in dequantization, keeps PNS bands — which bypass
/// dequantization and are scaled by 2^(sf/4) on their own —
/// at the same level as coded bands.
fn imdct(spectrum: &[f32]) -> Result<Vec<f32>> {
    let mut output = vec![0.0f32; 2 * spectrum.len()];
    let ones = vec![1.0f32; 2 * spectrum.len()];
    mdct_inverse(spectrum, &mut output, &ones)?;
    let scale = 1.0 / (spectrum.len() as f32 * 32768.0);
    for sample in &mut output {
        *sample *= scale;
    }
    Ok(output)
}
