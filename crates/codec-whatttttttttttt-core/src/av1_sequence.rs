//! AV1 sequence header OBU parsing (AV1 spec sections 5.5.1 and
//! 5.5.2).
//!
//! The sequence header fixes the properties of the whole coded
//! video sequence: profile, level, maximum frame size, bit depth,
//! chroma subsampling, and which tools the frames may use. The
//! values a frame header needs later (order hint bits, superblock
//! size, screen-content tool forcing) are kept on the struct.

use crate::bitreader::BitReader;
use crate::error::{Error, Result};
use crate::limits::{MAX_HEIGHT, MAX_WIDTH};

/// Value of `seq_force_screen_content_tools` /
/// `seq_force_integer_mv` meaning "the frame header decides".
pub const SELECT_SCREEN_CONTENT_TOOLS: u8 = 2;
/// Value of `seq_force_integer_mv` meaning "the frame header decides".
pub const SELECT_INTEGER_MV: u8 = 2;

/// Timing information (AV1 spec section 5.5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimingInfo {
    /// Display tick units.
    pub num_units_in_display_tick: u32,
    /// Time scale.
    pub time_scale: u32,
    /// True when every picture has the same display interval.
    pub equal_picture_interval: bool,
    /// Ticks per picture minus one, when the interval is equal.
    pub num_ticks_per_picture_minus_1: Option<u32>,
}

/// Decoder model information (AV1 spec section 5.5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecoderModelInfo {
    /// Buffer delay length minus one.
    pub buffer_delay_length_minus_1: u8,
    /// Decoding tick units.
    pub num_units_in_decoding_tick: u32,
    /// Buffer removal time length minus one.
    pub buffer_removal_time_length_minus_1: u8,
    /// Frame presentation time length minus one.
    pub frame_presentation_time_length_minus_1: u8,
}

/// One operating point (AV1 spec section 5.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OperatingPoint {
    /// Operating point idc.
    pub idc: u16,
    /// Level index.
    pub level: u8,
    /// Tier (0 or 1; only read when the level is above 7).
    pub tier: u8,
    /// Initial display delay minus one, when present.
    pub initial_display_delay_minus_1: Option<u8>,
    /// True when the decoder model is present for this operating
    /// point (only read when decoder model info is present).
    pub decoder_model_present: bool,
}

/// Color configuration (AV1 spec section 5.5.2). The booleans mirror
/// the spec syntax one-to-one, so they stay as fields rather than a
/// bitfield.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorConfig {
    /// Bit depth (8, 10, or 12).
    pub bit_depth: u8,
    /// True for monochrome (one plane only).
    pub mono_chrome: bool,
    /// Color primaries (2 = unspecified).
    pub color_primaries: u8,
    /// Transfer characteristics (2 = unspecified).
    pub transfer_characteristics: u8,
    /// Matrix coefficients (2 = unspecified).
    pub matrix_coefficients: u8,
    /// True for full range.
    pub color_range: bool,
    /// True when chroma is subsampled horizontally.
    pub subsampling_x: bool,
    /// True when chroma is subsampled vertically.
    pub subsampling_y: bool,
    /// Chroma sample position (0 = unknown).
    pub chroma_sample_position: u8,
    /// True when U and V may have separate delta quantizers.
    pub separate_uv_delta_q: bool,
}

/// Parsed AV1 sequence header (AV1 spec section 5.5.1). The
/// booleans mirror the spec syntax one-to-one, so they stay as
/// fields rather than a bitfield.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceHeader {
    /// Sequence profile (0, 1, or 2).
    pub profile: u8,
    /// True for a still picture.
    pub still_picture: bool,
    /// True for a reduced still picture header.
    pub reduced_still_picture_header: bool,
    /// Operating points, in order.
    pub operating_points: Vec<OperatingPoint>,
    /// Frame width bit count minus one (the width is coded with
    /// this many bits plus one when the frame size is overridden).
    pub frame_width_bits_minus_1: u8,
    /// Frame height bit count minus one.
    pub frame_height_bits_minus_1: u8,
    /// Maximum frame width minus one.
    pub max_frame_width_minus_1: u32,
    /// Maximum frame height minus one.
    pub max_frame_height_minus_1: u32,
    /// True when frame id numbers are present.
    pub frame_id_numbers_present: bool,
    /// Delta frame id length minus two, when frame ids are present.
    pub delta_frame_id_length_minus_2: u8,
    /// Additional frame id length minus one, when frame ids are present.
    pub additional_frame_id_length_minus_1: u8,
    /// True for 128x128 superblocks (else 64x64).
    pub use_128x128_superblock: bool,
    /// True when filter intra is enabled.
    pub enable_filter_intra: bool,
    /// True when intra edge filtering is enabled.
    pub enable_intra_edge_filter: bool,
    /// True when interintra compound is enabled.
    pub enable_interintra_compound: bool,
    /// True when masked compound is enabled.
    pub enable_masked_compound: bool,
    /// True when warped motion is enabled.
    pub enable_warped_motion: bool,
    /// True when dual filtering is enabled.
    pub enable_dual_filter: bool,
    /// True when order hints are enabled.
    pub enable_order_hint: bool,
    /// True when joint compound is enabled.
    pub enable_jnt_comp: bool,
    /// True when reference frame motion vectors are enabled.
    pub enable_ref_frame_mvs: bool,
    /// Screen content tools forcing: 0 off, 1 on, 2 select per frame.
    pub seq_force_screen_content_tools: u8,
    /// Integer motion vector forcing: 0 off, 1 on, 2 select per frame.
    pub seq_force_integer_mv: u8,
    /// Order hint bits (0 when order hints are disabled).
    pub order_hint_bits: u8,
    /// True when super-resolution is enabled.
    pub enable_superres: bool,
    /// True when CDEF is enabled.
    pub enable_cdef: bool,
    /// True when loop restoration is enabled.
    pub enable_restoration: bool,
    /// Color configuration.
    pub color: ColorConfig,
    /// True when film grain parameters are present.
    pub film_grain_params_present: bool,
    /// Timing information, when present.
    pub timing_info: Option<TimingInfo>,
    /// Decoder model information, when present.
    pub decoder_model_info: Option<DecoderModelInfo>,
}

impl SequenceHeader {
    /// Maximum frame width in pixels.
    #[must_use]
    pub fn max_frame_width(&self) -> u32 {
        self.max_frame_width_minus_1 + 1
    }

    /// Maximum frame height in pixels.
    #[must_use]
    pub fn max_frame_height(&self) -> u32 {
        self.max_frame_height_minus_1 + 1
    }

    /// Level index of operating point 0.
    #[must_use]
    pub fn level(&self) -> u8 {
        self.operating_points.first().map_or(0, |op| op.level)
    }

    /// Checks the Phase 7 subset (see `docs/LIMITS.md`): profile 0,
    /// 8-bit, non-monochrome YUV 4:2:0, no film grain. Dimensions
    /// above 8192 are out of scope.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` for a profile, bit depth, chroma
    /// format, or film grain setting outside the subset, and
    /// `Overflow` for a frame larger than 8192 pixels on a side.
    pub fn validate_subset(&self) -> Result<()> {
        if self.max_frame_width() > MAX_WIDTH || self.max_frame_height() > MAX_HEIGHT {
            return Err(Error::Overflow(
                "av1 frame dimensions exceed the 8192 pixel limit",
            ));
        }
        if self.profile != 0 {
            return Err(Error::Unsupported("av1 profile is not 0"));
        }
        if self.color.bit_depth != 8 {
            return Err(Error::Unsupported("av1 bit depth is not 8"));
        }
        if self.color.mono_chrome {
            return Err(Error::Unsupported("av1 monochrome is not supported"));
        }
        if !(self.color.subsampling_x && self.color.subsampling_y) {
            return Err(Error::Unsupported("av1 chroma subsampling is not 4:2:0"));
        }
        if self.film_grain_params_present {
            return Err(Error::Unsupported("av1 film grain is not supported"));
        }
        Ok(())
    }
}

/// Reads an unsigned variable-length code (AV1 spec section 5.3.4):
/// a run of zero bits terminated by a one bit, then that many bits
/// as the suffix.
fn read_uvlc(reader: &mut BitReader<'_>) -> Result<u32> {
    let mut leading_zeros: u32 = 0;
    loop {
        if reader.read_bit()? {
            break;
        }
        leading_zeros += 1;
        if leading_zeros > 32 {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "av1 uvlc leading zero run is too long",
            });
        }
    }
    if leading_zeros == 0 {
        return Ok(0);
    }
    let suffix = reader.read_bits(leading_zeros)?;
    Ok(((1u32 << leading_zeros) - 1).wrapping_add(suffix))
}

/// Reads the trailing bits of a sequence header OBU (AV1 spec
/// section 5.3.6): a one bit, then zero bits to the byte boundary.
fn read_trailing_bits(reader: &mut BitReader<'_>) -> Result<()> {
    if !reader.read_bit()? {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "av1 trailing one bit is not set",
        });
    }
    while reader.bit_position() % 8 != 0 {
        if reader.read_bit()? {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "av1 trailing zero bit is set",
            });
        }
    }
    Ok(())
}

/// Parses a sequence header OBU payload. The single function
/// mirrors the spec syntax (AV1 spec section 5.5.1): timing info,
/// operating points, frame dimensions, frame id lengths, the
/// feature flags, then the color config. Splitting it would scatter
/// the sequence-level state across helpers.
///
/// # Errors
///
/// Returns `Truncated` when the payload ends mid-field, `InvalidData`
/// for a malformed trailing-bits section, and `Unsupported` for a
/// profile above 2.
#[allow(clippy::too_many_lines)]
pub fn parse_sequence_header(payload: &[u8]) -> Result<SequenceHeader> {
    let mut reader = BitReader::new(payload);
    let profile = u8::try_from(reader.read_bits(3)?).unwrap_or(0);
    if profile > 2 {
        return Err(Error::Unsupported("av1 profile is above 2"));
    }
    let still_picture = reader.read_bit()?;
    let reduced = reader.read_bit()?;
    let mut timing_info = None;
    let mut decoder_model_info = None;
    let mut operating_points: Vec<OperatingPoint> = Vec::new();
    if reduced {
        let level = u8::try_from(reader.read_bits(5)?).unwrap_or(0);
        operating_points.push(OperatingPoint {
            idc: 0,
            level,
            tier: 0,
            initial_display_delay_minus_1: None,
            decoder_model_present: false,
        });
    } else {
        let timing_info_present = reader.read_bit()?;
        if timing_info_present {
            let num_units_in_display_tick = reader.read_bits(32)?;
            let time_scale = reader.read_bits(32)?;
            let equal_picture_interval = reader.read_bit()?;
            let num_ticks_per_picture_minus_1 = if equal_picture_interval {
                Some(read_uvlc(&mut reader)?)
            } else {
                None
            };
            timing_info = Some(TimingInfo {
                num_units_in_display_tick,
                time_scale,
                equal_picture_interval,
                num_ticks_per_picture_minus_1,
            });
        }
        let decoder_model_info_present = if timing_info_present {
            reader.read_bit()?
        } else {
            false
        };
        if decoder_model_info_present {
            let buffer_delay_length_minus_1 = u8::try_from(reader.read_bits(5)?).unwrap_or(0);
            let num_units_in_decoding_tick = reader.read_bits(32)?;
            let buffer_removal_time_length_minus_1 =
                u8::try_from(reader.read_bits(5)?).unwrap_or(0);
            let frame_presentation_time_length_minus_1 =
                u8::try_from(reader.read_bits(5)?).unwrap_or(0);
            decoder_model_info = Some(DecoderModelInfo {
                buffer_delay_length_minus_1,
                num_units_in_decoding_tick,
                buffer_removal_time_length_minus_1,
                frame_presentation_time_length_minus_1,
            });
        }
        let initial_display_delay_present = reader.read_bit()?;
        let operating_points_cnt_minus_1 = reader.read_bits(5)?;
        for _ in 0..=operating_points_cnt_minus_1 {
            let idc = u16::try_from(reader.read_bits(12)?).unwrap_or(0);
            let level = u8::try_from(reader.read_bits(5)?).unwrap_or(0);
            let tier = if level > 7 {
                u8::try_from(reader.read_bits(1)?).unwrap_or(0)
            } else {
                0
            };
            let decoder_model_present = if decoder_model_info_present {
                let present = reader.read_bit()?;
                if present {
                    // operating_parameters_info: decoder and encoder
                    // buffer delays plus the low delay flag. The
                    // delay fields are buffer_delay_length_minus_1
                    // + 1 bits wide.
                    let width = u32::from(
                        decoder_model_info.map_or(0, |info| info.buffer_delay_length_minus_1),
                    ) + 1;
                    let _decoder_buffer_delay = reader.read_bits(width)?;
                    let _encoder_buffer_delay = reader.read_bits(width)?;
                    let _low_delay_mode = reader.read_bit()?;
                }
                present
            } else {
                false
            };
            let initial_display_delay_minus_1 = if initial_display_delay_present {
                let present = reader.read_bit()?;
                if present {
                    Some(u8::try_from(reader.read_bits(4)?).unwrap_or(0))
                } else {
                    None
                }
            } else {
                None
            };
            operating_points.push(OperatingPoint {
                idc,
                level,
                tier,
                initial_display_delay_minus_1,
                decoder_model_present,
            });
        }
    }
    let frame_width_bits_minus_1 = reader.read_bits(4)?;
    let frame_height_bits_minus_1 = reader.read_bits(4)?;
    let max_frame_width_minus_1 = reader.read_bits(frame_width_bits_minus_1 + 1)?;
    let max_frame_height_minus_1 = reader.read_bits(frame_height_bits_minus_1 + 1)?;
    let frame_id_numbers_present = if reduced { false } else { reader.read_bit()? };
    let (delta_frame_id_length_minus_2, additional_frame_id_length_minus_1) =
        if frame_id_numbers_present {
            (
                u8::try_from(reader.read_bits(4)?).unwrap_or(0),
                u8::try_from(reader.read_bits(3)?).unwrap_or(0),
            )
        } else {
            (0, 0)
        };
    let use_128x128_superblock = reader.read_bit()?;
    let enable_filter_intra = reader.read_bit()?;
    let enable_intra_edge_filter = reader.read_bit()?;
    let (
        enable_interintra_compound,
        enable_masked_compound,
        enable_warped_motion,
        enable_dual_filter,
        enable_order_hint,
        enable_jnt_comp,
        enable_ref_frame_mvs,
        seq_force_screen_content_tools,
        seq_force_integer_mv,
        order_hint_bits,
    ) = if reduced {
        (
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            SELECT_SCREEN_CONTENT_TOOLS,
            SELECT_INTEGER_MV,
            0,
        )
    } else {
        let enable_interintra_compound = reader.read_bit()?;
        let enable_masked_compound = reader.read_bit()?;
        let enable_warped_motion = reader.read_bit()?;
        let enable_dual_filter = reader.read_bit()?;
        let enable_order_hint = reader.read_bit()?;
        let (enable_jnt_comp, enable_ref_frame_mvs) = if enable_order_hint {
            (reader.read_bit()?, reader.read_bit()?)
        } else {
            (false, false)
        };
        let seq_choose_screen_content_tools = reader.read_bit()?;
        let seq_force_screen_content_tools = if seq_choose_screen_content_tools {
            SELECT_SCREEN_CONTENT_TOOLS
        } else {
            u8::try_from(reader.read_bits(1)?).unwrap_or(0)
        };
        let seq_force_integer_mv = if seq_force_screen_content_tools > 0 {
            let seq_choose_integer_mv = reader.read_bit()?;
            if seq_choose_integer_mv {
                SELECT_INTEGER_MV
            } else {
                u8::try_from(reader.read_bits(1)?).unwrap_or(0)
            }
        } else {
            SELECT_INTEGER_MV
        };
        let order_hint_bits = if enable_order_hint {
            u8::try_from(reader.read_bits(3)?).unwrap_or(0) + 1
        } else {
            0
        };
        (
            enable_interintra_compound,
            enable_masked_compound,
            enable_warped_motion,
            enable_dual_filter,
            enable_order_hint,
            enable_jnt_comp,
            enable_ref_frame_mvs,
            seq_force_screen_content_tools,
            seq_force_integer_mv,
            order_hint_bits,
        )
    };
    let enable_superres = reader.read_bit()?;
    let enable_cdef = reader.read_bit()?;
    let enable_restoration = reader.read_bit()?;
    let color = parse_color_config(&mut reader, profile)?;
    let film_grain_params_present = reader.read_bit()?;
    read_trailing_bits(&mut reader)?;
    Ok(SequenceHeader {
        profile,
        still_picture,
        reduced_still_picture_header: reduced,
        operating_points,
        frame_width_bits_minus_1: u8::try_from(frame_width_bits_minus_1).unwrap_or(0),
        frame_height_bits_minus_1: u8::try_from(frame_height_bits_minus_1).unwrap_or(0),
        max_frame_width_minus_1,
        max_frame_height_minus_1,
        frame_id_numbers_present,
        delta_frame_id_length_minus_2,
        additional_frame_id_length_minus_1,
        use_128x128_superblock,
        enable_filter_intra,
        enable_intra_edge_filter,
        enable_interintra_compound,
        enable_masked_compound,
        enable_warped_motion,
        enable_dual_filter,
        enable_order_hint,
        enable_jnt_comp,
        enable_ref_frame_mvs,
        seq_force_screen_content_tools,
        seq_force_integer_mv,
        order_hint_bits,
        enable_superres,
        enable_cdef,
        enable_restoration,
        color,
        film_grain_params_present,
        timing_info,
        decoder_model_info,
    })
}

/// Parses the color configuration (AV1 spec section 5.5.2). The
/// `color_description_present_flag` is read even for monochrome,
/// and the monochrome branch reads `color_range` and returns before
/// `separate_uv_delta_q`.
fn parse_color_config(reader: &mut BitReader<'_>, profile: u8) -> Result<ColorConfig> {
    let high_bitdepth = reader.read_bit()?;
    let (bit_depth, twelve_bit) = if profile == 2 && high_bitdepth {
        let twelve_bit = reader.read_bit()?;
        (if twelve_bit { 12 } else { 10 }, twelve_bit)
    } else if profile <= 2 {
        (if high_bitdepth { 10 } else { 8 }, false)
    } else {
        (8, false)
    };
    let _ = twelve_bit;
    let mono_chrome = if profile == 1 {
        false
    } else {
        reader.read_bit()?
    };
    let color_description_present = reader.read_bit()?;
    let (color_primaries, transfer_characteristics, matrix_coefficients) =
        if color_description_present {
            (
                u8::try_from(reader.read_bits(8)?).unwrap_or(0),
                u8::try_from(reader.read_bits(8)?).unwrap_or(0),
                u8::try_from(reader.read_bits(8)?).unwrap_or(0),
            )
        } else {
            (2, 2, 2)
        };
    if mono_chrome {
        let color_range = reader.read_bit()?;
        return Ok(ColorConfig {
            bit_depth,
            mono_chrome: true,
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            color_range,
            subsampling_x: true,
            subsampling_y: true,
            chroma_sample_position: 0,
            separate_uv_delta_q: false,
        });
    }
    if color_primaries == 1 && transfer_characteristics == 13 && matrix_coefficients == 0 {
        let separate_uv_delta_q = reader.read_bit()?;
        return Ok(ColorConfig {
            bit_depth,
            mono_chrome: false,
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            color_range: true,
            subsampling_x: false,
            subsampling_y: false,
            chroma_sample_position: 0,
            separate_uv_delta_q,
        });
    }
    let color_range = reader.read_bit()?;
    let (subsampling_x, subsampling_y, chroma_sample_position) = if profile == 0 {
        (true, true, u8::try_from(reader.read_bits(2)?).unwrap_or(0))
    } else if profile == 1 {
        (false, false, 0)
    } else if bit_depth == 12 {
        let subsampling_x = reader.read_bit()?;
        let subsampling_y = if subsampling_x {
            reader.read_bit()?
        } else {
            false
        };
        let chroma_sample_position = if subsampling_x && subsampling_y {
            u8::try_from(reader.read_bits(2)?).unwrap_or(0)
        } else {
            0
        };
        (subsampling_x, subsampling_y, chroma_sample_position)
    } else {
        (true, false, 0)
    };
    let separate_uv_delta_q = reader.read_bit()?;
    Ok(ColorConfig {
        bit_depth,
        mono_chrome: false,
        color_primaries,
        transfer_characteristics,
        matrix_coefficients,
        color_range,
        subsampling_x,
        subsampling_y,
        chroma_sample_position,
        separate_uv_delta_q,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sequence header payload of `av1_64x64.ivf` frame 0, from the
    /// OBU with header 0x0a and size 10.
    const SEQ_64X64: [u8; 10] = [0x00, 0x00, 0x00, 0x02, 0xAF, 0xFF, 0x9B, 0x5F, 0x20, 0x08];

    #[test]
    fn sequence_header_matches_libav1_trace() {
        let header = parse_sequence_header(&SEQ_64X64).unwrap();
        assert_eq!(header.profile, 0);
        assert!(!header.still_picture);
        assert!(!header.reduced_still_picture_header);
        assert_eq!(header.operating_points.len(), 1);
        assert_eq!(header.operating_points[0].idc, 0);
        assert_eq!(header.operating_points[0].level, 0);
        assert_eq!(header.max_frame_width(), 64);
        assert_eq!(header.max_frame_height(), 64);
        assert!(!header.frame_id_numbers_present);
        assert!(!header.use_128x128_superblock);
        assert!(header.enable_filter_intra);
        assert!(header.enable_intra_edge_filter);
        assert!(!header.enable_interintra_compound);
        assert!(header.enable_masked_compound);
        assert!(header.enable_warped_motion);
        assert!(!header.enable_dual_filter);
        assert!(header.enable_order_hint);
        assert!(!header.enable_jnt_comp);
        assert!(header.enable_ref_frame_mvs);
        assert_eq!(
            header.seq_force_screen_content_tools,
            SELECT_SCREEN_CONTENT_TOOLS
        );
        assert_eq!(header.seq_force_integer_mv, SELECT_INTEGER_MV);
        assert_eq!(header.order_hint_bits, 7);
        assert!(!header.enable_superres);
        assert!(header.enable_cdef);
        assert!(!header.enable_restoration);
        assert_eq!(header.color.bit_depth, 8);
        assert!(!header.color.mono_chrome);
        assert_eq!(header.color.color_primaries, 2);
        assert_eq!(header.color.transfer_characteristics, 2);
        assert_eq!(header.color.matrix_coefficients, 2);
        assert!(!header.color.color_range);
        assert!(header.color.subsampling_x);
        assert!(header.color.subsampling_y);
        assert_eq!(header.color.chroma_sample_position, 0);
        assert!(!header.color.separate_uv_delta_q);
        assert!(!header.film_grain_params_present);
        header.validate_subset().unwrap();
    }

    #[test]
    fn sequence_header_truncated_payload_is_truncated() {
        let err = parse_sequence_header(&SEQ_64X64[..4]).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn sequence_header_bad_trailing_bit_is_invalid_data() {
        // Flip the trailing one bit to zero: the last byte 0x08
        // (00001000) becomes 0x00, so the trailing bit read is 0.
        let mut payload = SEQ_64X64;
        payload[9] = 0x00;
        let err = parse_sequence_header(&payload).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn subset_rejects_profile_1() {
        let mut header = parse_sequence_header(&SEQ_64X64).unwrap();
        header.profile = 1;
        assert!(matches!(
            header.validate_subset().unwrap_err(),
            Error::Unsupported(_)
        ));
    }

    #[test]
    fn subset_rejects_10_bit() {
        let mut header = parse_sequence_header(&SEQ_64X64).unwrap();
        header.color.bit_depth = 10;
        assert!(matches!(
            header.validate_subset().unwrap_err(),
            Error::Unsupported(_)
        ));
    }

    #[test]
    fn subset_rejects_oversize_frame() {
        let mut header = parse_sequence_header(&SEQ_64X64).unwrap();
        header.max_frame_width_minus_1 = 8192;
        assert!(matches!(
            header.validate_subset().unwrap_err(),
            Error::Overflow(_)
        ));
    }
}
