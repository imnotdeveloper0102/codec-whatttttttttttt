//! MP3 frame parsing and Layer III decoding (ISO/IEC 11172-3).
//!
//! A frame is a 4-byte header, an optional 16-bit CRC, side
//! information (granule headers), and main data (scalefactors and
//! Huffman-coded spectral data). Layer III is decoded in full:
//! frame header, side info, the bit reservoir, scalefactors,
//! Huffman decode (`mp3_huffman.rs`), dequantization, stereo
//! processing (mid/side and intensity), reorder, antialias, IMDCT
//! and polyphase synthesis (`mp3_synth.rs`).
//!
//! Layers I and II parse their headers (so a caller can tell the
//! layer apart from a corrupt stream) but decode returns
//! `Unsupported`: only Layer III is implemented in this module.
//! Free-format frames (bitrate index 0) take their length from the
//! packet, since the header carries no bitrate to compute it from.
//!
//! The decoder is stateful: the bit reservoir (up to 511 bytes of
//! a previous frame's unused main data), the IMDCT overlap, the
//! polyphase history, and the intensity-stereo positions all carry
//! across frames, so frames must be fed in order from the start of
//! the stream. A frame whose `main_data_begin` points further back
//! than the reservoir holds is `InvalidData`, not silence: the
//! stream started mid-way and its first granules are undecodable.

use crate::error::{Error, Result};
use crate::frame::AudioFrame;
use crate::limits::MAX_AUDIO_PACKET_BYTES;
use crate::mp3_huffman::{decode_huffman, Mp3BitCache};
use crate::mp3_synth::{
    antialias, change_sign, imdct_granule, reorder_short, synth_granule, MP3_GRANULE_LINES,
    MP3_QMF_STATE_LEN,
};
use crate::mp3_tables::{
    MP3_EXPFRAC, MP3_PAN, MP3_PRETAB, MP3_SCFC_DECODE, MP3_SCF_LONG, MP3_SCF_MIXED, MP3_SCF_MOD,
    MP3_SCF_PARTITIONS, MP3_SCF_SHORT,
};

/// MPEG audio version, from the header's version id
/// (ISO/IEC 11172-3 section 2.4.1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVersion {
    /// MPEG-1 (ISO/IEC 11172-3).
    Mpeg1,
    /// MPEG-2 (ISO/IEC 13818-3).
    Mpeg2,
    /// MPEG-2.5 (a later extension, same syntax as MPEG-2).
    Mpeg25,
}

/// MPEG audio layer, from the header's layer bits
/// (ISO/IEC 11172-3 section 2.4.1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp3Layer {
    /// Layer I.
    Layer1,
    /// Layer II.
    Layer2,
    /// Layer III.
    Layer3,
}

/// Channel mode, from the header (ISO/IEC 11172-3 section 2.4.1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    /// Stereo.
    Stereo,
    /// Joint stereo (mid/side and/or intensity).
    JointStereo,
    /// Dual channel (two independent mono channels).
    DualChannel,
    /// Mono.
    Mono,
}

/// Bitrate table (kbps) indexed by [version row][layer][index],
/// ISO/IEC 11172-3 Table B.1 and ISO/IEC 13818-3 Table B.1.
/// Index 0 is free format, 15 is bad.
const BITRATE_KBPS: [[[u32; 15]; 3]; 2] = [
    // MPEG-1: Layer I, II, III.
    [
        [
            0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
        ],
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
        ],
        [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ],
    ],
    // MPEG-2 / MPEG-2.5: Layer I, II, III.
    [
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
        ],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    ],
];

/// Sample rates (Hz) indexed by [version][rate index],
/// ISO/IEC 11172-3 Table B.2 and ISO/IEC 13818-3 Table B.2.
const SAMPLE_RATES: [[u32; 3]; 3] = [
    [44100, 48000, 32000], // MPEG-1
    [22050, 24000, 16000], // MPEG-2
    [11025, 12000, 8000],  // MPEG-2.5
];

/// Scalefactor-band table row for a (version, rate index) pair
/// (ISO/IEC 11172-3 Table B.8): rows 0-1 are MPEG-2.5, 2-4
/// MPEG-2, 5-7 MPEG-1.
fn sfb_table_row(version: MpegVersion, rate_index: usize) -> usize {
    match (version, rate_index) {
        (MpegVersion::Mpeg25, 0 | 1) => 0,
        (MpegVersion::Mpeg25, _) => 1,
        (MpegVersion::Mpeg2, 0) => 2,
        (MpegVersion::Mpeg2, 1) => 3,
        (MpegVersion::Mpeg2, _) => 4,
        (MpegVersion::Mpeg1, 0) => 5,
        (MpegVersion::Mpeg1, 1) => 6,
        (MpegVersion::Mpeg1, _) => 7,
    }
}

/// One MP3 frame header (ISO/IEC 11172-3 section 2.4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mp3FrameHeader {
    /// MPEG version.
    pub version: MpegVersion,
    /// Layer.
    pub layer: Mp3Layer,
    /// True when the 16-bit CRC follows the header.
    pub has_crc: bool,
    /// Bitrate in kbps; 0 is free format.
    pub bitrate_kbps: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Padding bit set.
    pub padding: bool,
    /// Channel mode.
    pub channel_mode: ChannelMode,
    /// Mode extension bits (joint stereo): bit 1 is mid/side,
    /// bit 0 is intensity stereo.
    pub mode_extension: u8,
    /// Frame length in bytes, including header and padding;
    /// 0 for free format (the packet length is the frame length).
    pub frame_length: usize,
    /// Samples per frame per channel (384, 576, or 1152).
    pub samples_per_frame: usize,
}

impl Mp3FrameHeader {
    /// Parses and validates a 4-byte header.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for fewer than 4 bytes and
    /// `InvalidData` for a bad sync, a reserved version or layer,
    /// a bad bitrate index, or a reserved sample-rate index.
    #[allow(clippy::too_many_lines)]
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 4 {
            return Err(Error::Truncated {
                needed: 4,
                had: data.len(),
            });
        }
        // The frame sync is 11 bits (0xFFE); MPEG-2.5 frames carry
        // only 11 sync bits, so the 12th bit is not required.
        if data[0] != 0xFF || data[1] & 0xE0 != 0xE0 {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "mp3 frame sync not found",
            });
        }
        let version = match (data[1] >> 3) & 0x3 {
            0 => MpegVersion::Mpeg25,
            1 => {
                return Err(Error::InvalidData {
                    offset: 1,
                    detail: "mp3 reserved mpeg version",
                });
            }
            2 => MpegVersion::Mpeg2,
            _ => MpegVersion::Mpeg1,
        };
        let layer = match (data[1] >> 1) & 0x3 {
            0 => {
                return Err(Error::InvalidData {
                    offset: 1,
                    detail: "mp3 reserved layer",
                });
            }
            1 => Mp3Layer::Layer3,
            2 => Mp3Layer::Layer2,
            _ => Mp3Layer::Layer1,
        };
        let bitrate_index = (data[2] >> 4) & 0xF;
        if bitrate_index == 15 {
            return Err(Error::InvalidData {
                offset: 2,
                detail: "mp3 bad bitrate index",
            });
        }
        let rate_index = ((data[2] >> 2) & 0x3) as usize;
        if rate_index == 3 {
            return Err(Error::InvalidData {
                offset: 2,
                detail: "mp3 reserved sample rate index",
            });
        }
        let version_row = match version {
            MpegVersion::Mpeg1 => 0,
            _ => 1,
        };
        let layer_row = match layer {
            Mp3Layer::Layer1 => 0,
            Mp3Layer::Layer2 => 1,
            Mp3Layer::Layer3 => 2,
        };
        let bitrate_kbps = BITRATE_KBPS[version_row][layer_row][bitrate_index as usize];
        let sample_rate = SAMPLE_RATES[match version {
            MpegVersion::Mpeg1 => 0,
            MpegVersion::Mpeg2 => 1,
            MpegVersion::Mpeg25 => 2,
        }][rate_index];
        let samples_per_frame = match layer {
            Mp3Layer::Layer1 => 384,
            Mp3Layer::Layer2 => 1152,
            Mp3Layer::Layer3 => match version {
                MpegVersion::Mpeg1 => 1152,
                _ => 576,
            },
        };
        let padding = data[2] & 0x2 != 0;
        let frame_length = if bitrate_kbps == 0 {
            0
        } else {
            // Layer I frames are slot-aligned to 4 bytes; the
            // others are `samples/8 * bitrate / sample_rate`
            // bytes plus one padding byte when set.
            let length = match layer {
                Mp3Layer::Layer1 => {
                    (12 * u64::from(bitrate_kbps) * 1000 / u64::from(sample_rate)
                        + u64::from(padding))
                        * 4
                }
                _ => {
                    samples_per_frame as u64 * u64::from(bitrate_kbps) * 1000
                        / (u64::from(sample_rate) * 8)
                        + u64::from(padding)
                }
            };
            usize::try_from(length).map_err(|_| Error::Overflow("mp3 frame length"))?
        };
        Ok(Self {
            version,
            layer,
            has_crc: data[1] & 0x1 == 0,
            bitrate_kbps,
            sample_rate,
            padding,
            channel_mode: match (data[3] >> 6) & 0x3 {
                0 => ChannelMode::Stereo,
                1 => ChannelMode::JointStereo,
                2 => ChannelMode::DualChannel,
                _ => ChannelMode::Mono,
            },
            mode_extension: (data[3] >> 4) & 0x3,
            frame_length,
            samples_per_frame,
        })
    }

    /// Number of channels.
    #[must_use]
    pub fn channels(&self) -> usize {
        match self.channel_mode {
            ChannelMode::Mono => 1,
            _ => 2,
        }
    }

    /// True when the mode extension selects mid/side stereo.
    #[must_use]
    pub fn is_ms_stereo(&self) -> bool {
        self.mode_extension & 0x2 != 0
    }

    /// True when the mode extension selects intensity stereo.
    #[must_use]
    pub fn is_intensity_stereo(&self) -> bool {
        self.mode_extension & 0x1 != 0
    }

    /// Granules per frame: 2 for MPEG-1 Layer III, 1 otherwise.
    #[must_use]
    pub fn granule_count(&self) -> usize {
        match (self.version, self.layer) {
            (MpegVersion::Mpeg1, Mp3Layer::Layer3) => 2,
            _ => 1,
        }
    }

    /// Side-information length in bytes (ISO/IEC 11172-3
    /// section 2.4.2 and ISO/IEC 13818-3 section 2.4.2).
    #[must_use]
    pub fn side_info_len(&self) -> usize {
        match self.version {
            MpegVersion::Mpeg1 => {
                if self.channels() == 1 {
                    17
                } else {
                    32
                }
            }
            _ => {
                if self.channels() == 1 {
                    9
                } else {
                    17
                }
            }
        }
    }
}

/// Side information for one granule channel (ISO/IEC 11172-3
/// section 2.4.2).
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct GranuleInfo {
    /// Bits of main data for this granule channel.
    pub part_23_length: u16,
    /// Pairs of big values (at most 288).
    pub big_values: u16,
    /// Global gain (quantizer step).
    pub global_gain: u8,
    /// Scalefactor compression code.
    pub scalefac_compress: u16,
    /// Window type: 0 long, 1 start, 2 short, 3 stop.
    pub block_type: u8,
    /// Mixed long/short block.
    pub mixed_block_flag: bool,
    /// Huffman table per region.
    pub table_select: [u8; 3],
    /// Scalefactor bands per region (255 means "the rest").
    pub region_count: [u8; 3],
    /// Subblock gain per short window.
    pub subblock_gain: [u8; 3],
    /// Preflag (MPEG-1) or derived (MPEG-2/2.5).
    pub preflag: bool,
    /// Scalefactor scale (0 halves, 1 doubles the step).
    pub scalefac_scale: bool,
    /// Count-1 table selector.
    pub count1_table: bool,
    /// Scalefactor reuse flags for granule 1 (0 for granule 0).
    pub scfsi: u8,
    /// Long scalefactor bands.
    pub n_long_sfb: usize,
    /// Short scalefactor bands.
    pub n_short_sfb: usize,
    /// Scalefactor-band width table for this granule.
    pub sfbtab: &'static [u8],
}

/// Parsed side information for a frame.
#[derive(Debug)]
pub(crate) struct SideInfo {
    /// Bytes of main data borrowed from previous frames.
    pub main_data_begin: usize,
    /// One entry per granule channel, granule-major.
    pub granules: Vec<GranuleInfo>,
}

/// Parses side information from `data` (exactly the side-info
/// bytes, after the header and CRC).
///
/// # Errors
///
/// Returns `Truncated` when the side info is short, and
/// `InvalidData` for a `big_values` above 288, a zero block type
/// with window switching, or a `part_23_length` sum that does not
/// fit the main data plus the reservoir.
#[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
fn parse_side_info(
    header: &Mp3FrameHeader,
    data: &[u8],
    main_data_bits: usize,
) -> Result<SideInfo> {
    let side_len = header.side_info_len();
    if data.len() < side_len {
        return Err(Error::Truncated {
            needed: side_len,
            had: data.len(),
        });
    }
    let mut cache = Mp3BitCache::new(&data[..side_len]);
    let channels = header.channels();
    let granule_count = header.granule_count();
    let is_mpeg1 = header.version == MpegVersion::Mpeg1;
    let rate_index = match header.sample_rate {
        44100 | 22050 | 11025 => 0,
        48000 | 24000 | 12000 => 1,
        _ => 2,
    };
    let row = sfb_table_row(header.version, rate_index);
    // The combined private-bits plus scalefactor-reuse field.
    // Parsed explicitly here (the reference decoder keeps it in a
    // shift register, which conflates the private bits into the
    // second channel's granule-0 flags; the field is private data
    // and must not affect scalefactor reuse).
    let mut scfsi_per_channel = [0u8; 2];
    let main_data_begin;
    if is_mpeg1 {
        main_data_begin = cache.read_bits(9)? as usize;
        let field_bits = 7 + 2 * channels;
        let field = cache.read_bits(field_bits as u32)? as usize;
        if channels == 1 {
            scfsi_per_channel[0] = (field & 0xF) as u8;
        } else {
            scfsi_per_channel[0] = ((field >> 4) & 0xF) as u8;
            scfsi_per_channel[1] = (field & 0xF) as u8;
        }
    } else {
        let field_bits = 8 + channels;
        let field = cache.read_bits(field_bits as u32)? as usize;
        main_data_begin = field >> channels;
    }
    let mut granules: Vec<GranuleInfo> = Vec::with_capacity(granule_count * channels);
    let mut part_23_sum: usize = 0;
    for _ in 0..granule_count * channels {
        let part_23_length = cache.read_bits(12)? as u16;
        part_23_sum += usize::from(part_23_length);
        let big_values = cache.read_bits(9)? as u16;
        if big_values > 288 {
            return Err(Error::InvalidData {
                offset: cache.bit_position() / 8,
                detail: "mp3 big_values exceeds 288",
            });
        }
        let global_gain = cache.read_bits(8)? as u8;
        let scalefac_compress = cache.read_bits(if is_mpeg1 { 4 } else { 9 })? as u16;
        let mut info = GranuleInfo {
            part_23_length,
            big_values,
            global_gain,
            scalefac_compress,
            block_type: 0,
            mixed_block_flag: false,
            table_select: [0; 3],
            region_count: [0, 0, 255],
            subblock_gain: [0; 3],
            preflag: false,
            scalefac_scale: false,
            count1_table: false,
            scfsi: 0,
            n_long_sfb: 22,
            n_short_sfb: 0,
            sfbtab: &MP3_SCF_LONG[row],
        };
        if cache.read_bit()? {
            let block_type = cache.read_bits(2)? as u8;
            if block_type == 0 {
                return Err(Error::InvalidData {
                    offset: cache.bit_position() / 8,
                    detail: "mp3 window switching with long block type",
                });
            }
            info.block_type = block_type;
            info.mixed_block_flag = cache.read_bit()?;
            info.region_count[0] = 7;
            info.region_count[1] = 255;
            if block_type == 2 {
                if info.mixed_block_flag {
                    info.sfbtab = &MP3_SCF_MIXED[row];
                    info.n_long_sfb = if is_mpeg1 { 8 } else { 6 };
                    info.n_short_sfb = 30;
                } else {
                    info.region_count[0] = 8;
                    info.sfbtab = &MP3_SCF_SHORT[row];
                    info.n_long_sfb = 0;
                    info.n_short_sfb = 39;
                }
            }
            let tables = cache.read_bits(10)? << 5;
            info.subblock_gain[0] = cache.read_bits(3)? as u8;
            info.subblock_gain[1] = cache.read_bits(3)? as u8;
            info.subblock_gain[2] = cache.read_bits(3)? as u8;
            info.table_select[0] = (tables >> 10) as u8;
            info.table_select[1] = ((tables >> 5) & 31) as u8;
            info.table_select[2] = (tables & 31) as u8;
        } else {
            let tables = cache.read_bits(15)?;
            info.region_count[0] = cache.read_bits(4)? as u8;
            info.region_count[1] = cache.read_bits(3)? as u8;
            info.table_select[0] = (tables >> 10) as u8;
            info.table_select[1] = ((tables >> 5) & 31) as u8;
            info.table_select[2] = (tables & 31) as u8;
        }
        info.preflag = if is_mpeg1 {
            cache.read_bit()?
        } else {
            scalefac_compress >= 500
        };
        info.scalefac_scale = cache.read_bit()?;
        info.count1_table = cache.read_bit()?;
        granules.push(info);
    }
    // Scalefactor reuse: granule 0 never reuses, granule 1 reuses
    // its channel's flags unless granule 0 of that channel was a
    // short block, whose scalefactors cannot be reused.
    if granule_count == 2 {
        for channel in 0..channels {
            let first_is_short = granules[channel].block_type == 2;
            let scfsi = if first_is_short {
                0
            } else {
                scfsi_per_channel[channel]
            };
            granules[channels + channel].scfsi = scfsi;
        }
    }
    if part_23_sum > main_data_bits + main_data_begin * 8 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "mp3 part_23_length sum exceeds main data plus reservoir",
        });
    }
    Ok(SideInfo {
        main_data_begin,
        granules,
    })
}

/// `y * 2^(-exp_q2 / 4)`, the scalefactor gain helper
/// (ISO/IEC 11172-3 section 2.4.2.7). The fractional quarters use
/// the `MP3_EXPFRAC` table; a negative exponent (unreachable from
/// the callers here, whose exponents are non-negative by
/// construction) falls back to a direct power so the function
/// never shifts by a negative amount.
#[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
fn ldexp_q2(mut y: f32, mut exp_q2: i32) -> f32 {
    if exp_q2 < 0 {
        return y * 2f32.powf(-exp_q2 as f32 / 4.0);
    }
    loop {
        let e = exp_q2.min(30 * 4);
        y *= MP3_EXPFRAC[(e & 3) as usize] * ((1u32 << 30) >> (e >> 2)) as f32;
        exp_q2 -= e;
        if exp_q2 <= 0 {
            break;
        }
    }
    y
}

/// Reads one granule channel's scalefactors from the main-data
/// bitstream into float multipliers (ISO/IEC 11172-3 section
/// 2.4.2.7). `ist_pos` is the channel's persistent intensity
/// positions, updated in place; the returned array holds one
/// multiplier per scalefactor band.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::too_many_lines
)]
fn decode_scalefactors(
    header: &Mp3FrameHeader,
    cache: &mut Mp3BitCache<'_>,
    gr: &GranuleInfo,
    ist_pos: &mut [u8; 40],
    channel: usize,
) -> Result<[f32; 40]> {
    let mut scf_size = [0u8; 4];
    // Partition counts, offset for MPEG-2/2.5 mixed-radix decoding.
    // Row 0 is long blocks, 1 mixed, 2 short.
    let partition_row = usize::from(gr.n_short_sfb > 0) + usize::from(gr.n_long_sfb == 0);
    let mut partition_offset = 0usize;
    let scfsi: i32;
    if header.version == MpegVersion::Mpeg1 {
        let part = MP3_SCFC_DECODE[gr.scalefac_compress as usize];
        scf_size[0] = part >> 2;
        scf_size[1] = part >> 2;
        scf_size[2] = part & 3;
        scf_size[3] = part & 3;
        scfsi = i32::from(gr.scfsi);
    } else {
        let mut sfc = i32::from(gr.scalefac_compress);
        let ist = header.is_intensity_stereo() && channel == 1;
        if ist {
            sfc >>= 1;
        }
        let mut k = if ist { 12 } else { 0 };
        while sfc >= 0 {
            let mut modprod: i32 = 1;
            for i in (0..4).rev() {
                let modulus = i32::from(MP3_SCF_MOD[k + i]);
                scf_size[i] = (sfc / modprod % modulus) as u8;
                modprod *= modulus;
            }
            sfc -= modprod;
            k += 4;
        }
        partition_offset = k;
        scfsi = -16;
    }
    // Four partitions of scalefactors, each with its own width;
    // a set reuse flag copies the previous granule's values.
    let mut iscf = [0u8; 40];
    let mut pos = 0usize;
    let mut scfsi_walk = scfsi;
    for (i, &bits) in scf_size.iter().enumerate() {
        let count = *MP3_SCF_PARTITIONS
            .get(partition_row)
            .and_then(|row| row.get(partition_offset + i))
            .ok_or(Error::InvalidData {
                offset: cache.bit_position() / 8,
                detail: "mp3 scalefactor partition index out of range",
            })? as usize;
        if count == 0 {
            break;
        }
        if scfsi_walk & 8 != 0 {
            iscf[pos..pos + count].copy_from_slice(&ist_pos[pos..pos + count]);
        } else if bits == 0 {
            iscf[pos..pos + count].fill(0);
            ist_pos[pos..pos + count].fill(0);
        } else {
            let max_scf = if scfsi_walk < 0 {
                (1i32 << bits) - 1
            } else {
                -1
            };
            for k in 0..count {
                let value = cache.read_bits(u32::from(bits))? as i32;
                ist_pos[pos + k] = if value == max_scf { 255 } else { value as u8 };
                iscf[pos + k] = value as u8;
            }
        }
        pos += count;
        scfsi_walk *= 2;
    }
    if pos + 3 > iscf.len() {
        return Err(Error::InvalidData {
            offset: cache.bit_position() / 8,
            detail: "mp3 scalefactor partitions overrun the band table",
        });
    }
    iscf[pos..pos + 3].fill(0);
    let scf_shift = u32::from(gr.scalefac_scale) + 1;
    if gr.n_short_sfb > 0 {
        let shift = 3 - scf_shift;
        let mut i = 0usize;
        while i < gr.n_short_sfb {
            let base = gr.n_long_sfb + i;
            iscf[base] = iscf[base].wrapping_add(gr.subblock_gain[0] << shift);
            iscf[base + 1] = iscf[base + 1].wrapping_add(gr.subblock_gain[1] << shift);
            iscf[base + 2] = iscf[base + 2].wrapping_add(gr.subblock_gain[2] << shift);
            i += 3;
        }
    } else if gr.preflag {
        for i in 0..10 {
            iscf[11 + i] = iscf[11 + i].wrapping_add(MP3_PRETAB[i]);
        }
    }
    let gain_exp = i32::from(gr.global_gain) - 4 - 210 - if header.is_ms_stereo() { 2 } else { 0 };
    let gain = ldexp_q2(2048.0, 44 - gain_exp);
    let mut scf = [0.0f32; 40];
    for (i, value) in scf
        .iter_mut()
        .enumerate()
        .take(gr.n_long_sfb + gr.n_short_sfb)
    {
        *value = ldexp_q2(gain, (i32::from(iscf[i])) << scf_shift);
    }
    Ok(scf)
}

/// Intensity stereo for one granule (ISO/IEC 11172-3 section
/// 2.4.3.4.9). `grbuf` holds both channels' 576 lines, left
/// first. `ist_pos` is the right channel's persistent positions,
/// updated for the top bands. Bands above the highest band with
/// right-channel energy are intensity-coded; the rest are
/// mid/side when that mode is set.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
fn intensity_stereo(
    grbuf: &mut [f32; MP3_GRANULE_LINES * 2],
    ist_pos: &mut [u8; 40],
    gr: &[GranuleInfo],
    header: &Mp3FrameHeader,
) {
    let sfbtab = gr[0].sfbtab;
    let n_sfb = gr[0].n_long_sfb + gr[0].n_short_sfb;
    let mut max_band = [-1i32; 3];
    let mut right_idx = MP3_GRANULE_LINES;
    for i in 0..n_sfb {
        let width = sfbtab.get(i).map_or(0, |&w| w as usize);
        let mut k = 0usize;
        while k + 1 < width {
            if grbuf[right_idx + k] != 0.0 || grbuf[right_idx + k + 1] != 0.0 {
                max_band[i % 3] = i as i32;
                break;
            }
            k += 2;
        }
        right_idx += width;
    }
    if gr[0].n_long_sfb > 0 {
        let top = max_band[0].max(max_band[1]).max(max_band[2]);
        max_band = [top; 3];
    }
    let max_blocks = if gr[0].n_short_sfb > 0 { 3 } else { 1 };
    for (i, &band) in max_band.iter().enumerate().take(max_blocks) {
        let itop = n_sfb - max_blocks + i;
        let prev = itop as i32 - max_blocks as i32;
        if prev >= 0 {
            let default_pos: u8 = if header.version == MpegVersion::Mpeg1 {
                3
            } else {
                0
            };
            ist_pos[itop] = if band >= prev {
                default_pos
            } else {
                ist_pos[prev as usize]
            };
        }
    }
    let max_pos: usize = if header.version == MpegVersion::Mpeg1 {
        7
    } else {
        64
    };
    let mpeg2_sh = u32::from(gr.get(1).map_or(0, |g| g.scalefac_compress & 1));
    let mut left_idx = 0usize;
    for i in 0..n_sfb {
        let width = sfbtab.get(i).map_or(0, |&w| w as usize);
        let ipos = ist_pos[i] as usize;
        if (i as i32) > max_band[i % 3] && ipos < max_pos {
            let (kl, kr) = if header.version == MpegVersion::Mpeg1 {
                (MP3_PAN[2 * ipos], MP3_PAN[2 * ipos + 1])
            } else {
                let mut kl = 1.0f32;
                let mut kr = ldexp_q2(1.0, (((ipos + 1) >> 1) << mpeg2_sh) as i32);
                if ipos & 1 != 0 {
                    kl = kr;
                    kr = 1.0;
                }
                (kl, kr)
            };
            let scale = if header.is_ms_stereo() {
                std::f32::consts::SQRT_2
            } else {
                1.0
            };
            for j in 0..width {
                let value = grbuf[left_idx + j];
                grbuf[MP3_GRANULE_LINES + left_idx + j] = value * kr * scale;
                grbuf[left_idx + j] = value * kl * scale;
            }
        } else if header.is_ms_stereo() {
            for j in 0..width {
                let left = grbuf[left_idx + j];
                let right = grbuf[MP3_GRANULE_LINES + left_idx + j];
                grbuf[left_idx + j] = left + right;
                grbuf[MP3_GRANULE_LINES + left_idx + j] = left - right;
            }
        }
        left_idx += width;
    }
}

/// Mid/side stereo over a whole granule (ISO/IEC 11172-3 section
/// 2.4.3.4.9): left becomes L+R, right becomes L-R.
fn midside_stereo(grbuf: &mut [f32; MP3_GRANULE_LINES * 2]) {
    for i in 0..MP3_GRANULE_LINES {
        let left = grbuf[i];
        let right = grbuf[MP3_GRANULE_LINES + i];
        grbuf[i] = left + right;
        grbuf[MP3_GRANULE_LINES + i] = left - right;
    }
}

/// Decodes one granule into `pcm_out` (576 samples per channel).
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
#[allow(clippy::too_many_arguments)]
fn decode_granule(
    header: &Mp3FrameHeader,
    cache: &mut Mp3BitCache<'_>,
    gr_infos: &[GranuleInfo],
    ist_pos: &mut [[u8; 40]; 2],
    overlap: &mut [[f32; MP3_GRANULE_LINES]; 2],
    qmf_state: &mut [f32; MP3_QMF_STATE_LEN],
    pcm_out: &mut Vec<f32>,
) -> Result<()> {
    let channels = header.channels();
    let mut grbuf = [0.0f32; MP3_GRANULE_LINES * 2];
    for ch in 0..channels {
        let gr = &gr_infos[ch];
        let limit = cache.bit_position() + usize::from(gr.part_23_length);
        let scf = decode_scalefactors(header, cache, gr, &mut ist_pos[ch], ch)?;
        let mut channel = [0.0f32; MP3_GRANULE_LINES];
        decode_huffman(
            &mut channel,
            cache,
            gr.sfbtab,
            gr.table_select,
            gr.region_count,
            gr.big_values as usize,
            gr.count1_table,
            &scf,
            limit,
        )?;
        grbuf[ch * MP3_GRANULE_LINES..(ch + 1) * MP3_GRANULE_LINES].copy_from_slice(&channel);
    }
    if header.is_intensity_stereo() {
        intensity_stereo(&mut grbuf, &mut ist_pos[1], gr_infos, header);
    } else if header.is_ms_stereo() {
        midside_stereo(&mut grbuf);
    }
    // Sample-rate row 2 of the "my sample rate" numbering is
    // 8000 Hz (MPEG-2.5), where mixed blocks keep four long bands.
    let my_sr_is_8000 = matches!(header.version, MpegVersion::Mpeg25) && header.sample_rate == 8000;
    for ch in 0..channels {
        let gr = &gr_infos[ch];
        let n_long_bands = if gr.mixed_block_flag {
            if my_sr_is_8000 {
                4
            } else {
                2
            }
        } else {
            0
        };
        let mut channel = [0.0f32; MP3_GRANULE_LINES];
        channel.copy_from_slice(&grbuf[ch * MP3_GRANULE_LINES..(ch + 1) * MP3_GRANULE_LINES]);
        let mut aa_bands = 31i32;
        if gr.n_short_sfb > 0 {
            aa_bands = n_long_bands as i32 - 1;
            reorder_short(&mut channel, n_long_bands * 18, &gr.sfbtab[gr.n_long_sfb..])?;
        }
        antialias(&mut channel, aa_bands);
        imdct_granule(&mut channel, &mut overlap[ch], gr.block_type, n_long_bands)?;
        change_sign(&mut channel);
        grbuf[ch * MP3_GRANULE_LINES..(ch + 1) * MP3_GRANULE_LINES].copy_from_slice(&channel);
    }
    let mut pcm = vec![0.0f32; MP3_GRANULE_LINES * channels];
    synth_granule(qmf_state, &grbuf, 18, channels, &mut pcm);
    pcm_out.extend_from_slice(&pcm);
    Ok(())
}

/// Largest bit-reservoir in bytes (ISO/IEC 11172-3 section 2.4.2:
/// `main_data_begin` is 9 bits, so at most 511 bytes back).
const MAX_RESERVOIR_BYTES: usize = 511;

/// Stateful Layer III decoder: the bit reservoir, IMDCT overlap,
/// polyphase history, and intensity positions carry across frames.
#[derive(Debug)]
pub struct Mp3Decoder {
    reservoir: [u8; MAX_RESERVOIR_BYTES],
    reservoir_len: usize,
    overlap: [[f32; MP3_GRANULE_LINES]; 2],
    qmf_state: [f32; MP3_QMF_STATE_LEN],
    ist_pos: [[u8; 40]; 2],
}

impl Default for Mp3Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Mp3Decoder {
    /// Creates a decoder with an empty reservoir and zero history.
    #[must_use]
    pub fn new() -> Self {
        Self {
            reservoir: [0; MAX_RESERVOIR_BYTES],
            reservoir_len: 0,
            overlap: [[0.0; MP3_GRANULE_LINES]; 2],
            qmf_state: [0.0; MP3_QMF_STATE_LEN],
            ist_pos: [[0; 40]; 2],
        }
    }

    /// Decodes exactly one frame from `data` (the whole frame:
    /// header, CRC if present, side info, main data).
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a short packet, `InvalidData` for
    /// a bad header, side info, or a `main_data_begin` beyond the
    /// reservoir, and `Unsupported` for Layers I and II or for
    /// free-format frames whose length cannot be derived (the
    /// packet length is used instead, so this only fires when the
    /// packet itself is empty of main data).
    #[allow(clippy::cast_possible_truncation)]
    pub fn decode_frame(&mut self, data: &[u8]) -> Result<AudioFrame> {
        if data.len() > MAX_AUDIO_PACKET_BYTES {
            return Err(Error::AllocationLimit {
                requested: data.len(),
                limit: MAX_AUDIO_PACKET_BYTES,
            });
        }
        let header = Mp3FrameHeader::parse(data)?;
        if header.layer != Mp3Layer::Layer3 {
            return Err(Error::Unsupported(
                "mp3 layer I/II decode is not implemented (layer III only)",
            ));
        }
        let frame_length = if header.frame_length == 0 {
            data.len()
        } else {
            header.frame_length
        };
        if data.len() < frame_length {
            return Err(Error::Truncated {
                needed: frame_length,
                had: data.len(),
            });
        }
        if data.len() > frame_length {
            return Err(Error::InvalidData {
                offset: frame_length,
                detail: "mp3 packet holds more than one frame",
            });
        }
        let crc_len = if header.has_crc { 2 } else { 0 };
        let side_len = header.side_info_len();
        if frame_length < 4 + crc_len + side_len {
            return Err(Error::Truncated {
                needed: 4 + crc_len + side_len,
                had: frame_length,
            });
        }
        let main_data_start = 4 + crc_len + side_len;
        let main_data_bits = (frame_length - main_data_start) * 8;
        let side_info =
            parse_side_info(&header, &data[4 + crc_len..main_data_start], main_data_bits)?;
        if side_info.main_data_begin > self.reservoir_len {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "mp3 main_data_begin exceeds the bit reservoir",
            });
        }
        let mut main_data: Vec<u8> =
            Vec::with_capacity(side_info.main_data_begin + frame_length - main_data_start);
        main_data.extend_from_slice(
            &self.reservoir[self.reservoir_len - side_info.main_data_begin..self.reservoir_len],
        );
        main_data.extend_from_slice(&data[main_data_start..frame_length]);
        let mut cache = Mp3BitCache::new(&main_data);
        let channels = header.channels();
        let granule_count = header.granule_count();
        let mut samples: Vec<f32> =
            Vec::with_capacity(granule_count * MP3_GRANULE_LINES * channels);
        for granule in 0..granule_count {
            let infos = &side_info.granules[granule * channels..(granule + 1) * channels];
            decode_granule(
                &header,
                &mut cache,
                infos,
                &mut self.ist_pos,
                &mut self.overlap,
                &mut self.qmf_state,
                &mut samples,
            )?;
        }
        // Save the unused tail of this frame's main data as the
        // reservoir for the next frame, keeping at most 511 bytes.
        let mut pos_bytes = cache.bit_position().div_ceil(8);
        let mut remains = main_data.len().saturating_sub(pos_bytes);
        if remains > MAX_RESERVOIR_BYTES {
            pos_bytes += remains - MAX_RESERVOIR_BYTES;
            remains = MAX_RESERVOIR_BYTES;
        }
        self.reservoir[..remains].copy_from_slice(&main_data[pos_bytes..pos_bytes + remains]);
        self.reservoir_len = remains;
        Ok(AudioFrame {
            sample_rate: header.sample_rate,
            channels: channels as u32,
            samples,
            timestamp: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_lame_header() {
        // First frame of a 128 kbps 44.1 kHz stereo Layer III
        // stream: FF FB 90 00 (sync, MPEG-1, Layer III, no CRC,
        // 128 kbps, 44.1 kHz, no padding, stereo).
        let header = Mp3FrameHeader::parse(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        assert_eq!(header.version, MpegVersion::Mpeg1);
        assert_eq!(header.layer, Mp3Layer::Layer3);
        assert!(!header.has_crc);
        assert_eq!(header.bitrate_kbps, 128);
        assert_eq!(header.sample_rate, 44100);
        assert_eq!(header.channels(), 2);
        assert_eq!(header.frame_length, 417);
        assert_eq!(header.samples_per_frame, 1152);
        assert_eq!(header.side_info_len(), 32);
        assert_eq!(header.granule_count(), 2);
    }

    #[test]
    fn parses_mpeg2_mono_header() {
        // FF F3 80 C0: MPEG-2, Layer III, 64 kbps, 22050 Hz, mono.
        let header = Mp3FrameHeader::parse(&[0xFF, 0xF3, 0x80, 0xC0]).unwrap();
        assert_eq!(header.version, MpegVersion::Mpeg2);
        assert_eq!(header.sample_rate, 22050);
        assert_eq!(header.bitrate_kbps, 64);
        assert_eq!(header.channels(), 1);
        assert_eq!(header.frame_length, 208);
        assert_eq!(header.samples_per_frame, 576);
        assert_eq!(header.side_info_len(), 9);
    }

    #[test]
    fn bad_sync_is_invalid_data() {
        let err = Mp3FrameHeader::parse(&[0x00, 0xFB, 0x90, 0x00]).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn reserved_version_and_layer_are_invalid_data() {
        // Version id 1 is reserved.
        let err = Mp3FrameHeader::parse(&[0xFF, 0xEB, 0x90, 0x00]).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
        // Layer id 0 is reserved.
        let err = Mp3FrameHeader::parse(&[0xFF, 0xF9, 0x90, 0x00]).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn bad_bitrate_and_rate_indexes_are_invalid_data() {
        let err = Mp3FrameHeader::parse(&[0xFF, 0xFB, 0xF0, 0x00]).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
        let err = Mp3FrameHeader::parse(&[0xFF, 0xFB, 0x9C, 0x00]).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn short_header_is_truncated() {
        let err = Mp3FrameHeader::parse(&[0xFF, 0xFB]).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn ldexp_q2_matches_powers_of_two() {
        // ldexp_q2(y, e) = y * 2^(-e/4).
        assert!((ldexp_q2(1.0, 0) - 1.0).abs() < 1e-6);
        assert!((ldexp_q2(1.0, 4) - 0.5).abs() < 1e-6);
        assert!((ldexp_q2(1.0, 8) - 0.25).abs() < 1e-6);
        assert!((ldexp_q2(2.0, -4) - 4.0).abs() < 1e-6);
    }
}
