//! AV1 frame header OBU parsing and tile group framing (AV1 spec
//! sections 5.9, 5.10, 5.11, and 8.2).
//!
//! Phase 7 is parser-only for the frame contents: the uncompressed
//! header and the tile group framing are parsed and validated, but
//! tile data is not decoded. `Decoder::decode` for AV1 returns
//! `Unsupported` until reconstruction exists with passing tests
//! against libaom output. The symbol decoder of section 8.2 is
//! implemented here as a standalone type so tile decoding can use
//! it in a later phase; it is unit-tested against hand-computed
//! values from the spec, not against real tile data.

use crate::av1_sequence::SequenceHeader;
use crate::bitreader::BitReader;
use crate::error::{Error, Result};

/// Key frame.
pub const KEY_FRAME: u8 = 0;
/// Inter frame.
pub const INTER_FRAME: u8 = 1;
/// Intra-only frame.
pub const INTRA_ONLY_FRAME: u8 = 2;
/// Switch frame.
pub const SWITCH_FRAME: u8 = 3;

/// Number of reference frame slots.
pub const NUM_REF_FRAMES: usize = 8;
/// Number of references per frame.
pub const REFS_PER_FRAME: usize = 7;
/// `primary_ref_frame` value meaning "no primary reference".
pub const PRIMARY_REF_NONE: u8 = 7;

/// Largest tile width in pixels (AV1 spec section 3).
pub const MAX_TILE_WIDTH: u32 = 4096;
/// Largest tile area in superblocks (AV1 spec section 3).
pub const MAX_TILE_AREA_SB: u32 = 4096;
/// Largest number of tile columns.
pub const MAX_TILE_COLS: u32 = 64;
/// Largest number of tile rows.
pub const MAX_TILE_ROWS: u32 = 64;

/// Super-resolution numerator (AV1 spec section 3).
pub const SUPERRES_NUM: u32 = 8;
/// Super-resolution denominator minimum.
pub const SUPERRES_DENOM_MIN: u32 = 8;
/// Super-resolution denominator bits.
pub const SUPERRES_DENOM_BITS: u32 = 3;

/// Number of segments.
pub const MAX_SEGMENTS: usize = 8;
/// Number of segmentation feature levels.
pub const SEG_LVL_MAX: usize = 8;
/// Segmentation feature level for the quantizer.
pub const SEG_LVL_ALT_Q: usize = 0;

/// Global motion type: identity.
pub const GM_IDENTITY: u8 = 0;
/// Global motion type: translation.
pub const GM_TRANSLATION: u8 = 1;
/// Global motion type: rotation and zoom.
pub const GM_ROTZOOM: u8 = 2;
/// Global motion type: affine.
pub const GM_AFFINE: u8 = 3;

/// Warped model precision bits (AV1 spec section 3).
const WARPEDMODEL_PREC_BITS: u32 = 16;
/// Global motion absolute alpha bits.
const GM_ABS_ALPHA_BITS: u32 = 12;
/// Global motion alpha precision bits.
const GM_ALPHA_PREC_BITS: u32 = 15;
/// Global motion absolute translation-only bits.
const GM_ABS_TRANS_ONLY_BITS: u32 = 9;
/// Global motion translation-only precision bits.
const GM_TRANS_ONLY_PREC_BITS: u32 = 3;
/// Global motion absolute translation bits.
const GM_ABS_TRANS_BITS: u32 = 12;
/// Global motion translation precision bits.
const GM_TRANS_PREC_BITS: u32 = 6;

/// Floor of log2, for a non-zero value.
fn floor_log2(value: u32) -> u32 {
    value.ilog2()
}

/// Smallest `k` such that `blk_size << k >= target` (AV1 spec
/// section 5.9.16).
fn tile_log2(blk_size: u32, target: u32) -> u32 {
    let mut k = 0;
    while (blk_size << k) < target {
        k += 1;
    }
    k
}

/// Relative distance between two order hints (AV1 spec section
/// 5.9.3). Returns 0 when order hints are disabled.
fn get_relative_dist(a: u32, b: u32, order_hint_bits: u8, enable_order_hint: bool) -> i32 {
    if !enable_order_hint {
        return 0;
    }
    // Order hints are at most 8 bits, so the difference and the
    // masked result both fit in an `i32`; `i64` keeps the
    // subtraction exact without a wrapping cast.
    let diff = i64::from(a) - i64::from(b);
    let m = 1i64 << (order_hint_bits - 1);
    i32::try_from((diff & (m - 1)) - (diff & m)).unwrap_or(0)
}

/// Reads a signed value: the least significant bit is the sign (AV1
/// spec section 5.3.3, `su(n)`).
fn read_su(reader: &mut BitReader<'_>, n: u32) -> Result<i32> {
    let value = reader.read_bits(n)?;
    if value & 1 == 1 {
        Ok(-(i32::try_from(value >> 1).unwrap_or(0)))
    } else {
        Ok(i32::try_from(value >> 1).unwrap_or(0))
    }
}

/// Reads a non-symmetric value (AV1 spec section 5.3.4, `ns(n)`).
fn read_ns(reader: &mut BitReader<'_>, n: u32) -> Result<u32> {
    if n == 0 {
        return Err(Error::InvalidArgument("av1 ns(0) is undefined"));
    }
    let w = floor_log2(n) + 1;
    let m = (1u32 << w) - n;
    let value = reader.read_bits(w - 1)?;
    if value < m {
        Ok(value)
    } else {
        let extra = reader.read_bits(1)?;
        Ok(((value << 1) - m) + extra)
    }
}

/// Reads a delta quantizer value (AV1 spec section 5.9.13).
fn read_delta_q(reader: &mut BitReader<'_>) -> Result<i32> {
    if reader.read_bit()? {
        read_su(reader, 1 + 6)
    } else {
        Ok(0)
    }
}

/// Validates the byte-alignment padding of a frame or tile group
/// header (AV1 spec section 5.3.5): the padding bits must all be
/// zero.
fn read_byte_alignment(reader: &mut BitReader<'_>) -> Result<()> {
    while reader.bit_position() % 8 != 0 {
        if reader.read_bit()? {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "av1 byte alignment bit is set",
            });
        }
    }
    Ok(())
}

/// Validates the trailing bits at the end of a frame header OBU
/// (AV1 spec section 5.3.4): a one bit, then zero bits to the end of
/// the OBU payload.
///
/// # Errors
///
/// Returns `Truncated` when the data ends before the trailing bits,
/// and `InvalidData` when the trailing one bit is not set or a
/// trailing zero bit is set.
pub fn check_trailing_bits(data: &[u8], bit_position: usize) -> Result<()> {
    let mut reader = BitReader::new(data);
    // Advance the reader to the given bit position by reading and
    // discarding bits; the position is always within the data.
    for _ in 0..bit_position {
        let _ = reader.read_bit()?;
    }
    if !reader.read_bit()? {
        return Err(Error::InvalidData {
            offset: bit_position / 8,
            detail: "av1 trailing one bit is not set",
        });
    }
    while reader.bit_position() < data.len() * 8 {
        if reader.read_bit()? {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "av1 trailing zero bit is set",
            });
        }
    }
    Ok(())
}

/// Film grain parameters (AV1 spec section 5.9.30). Stored per
/// reference frame slot so `load_grain_params` can copy them. The
/// booleans mirror the spec syntax one-to-one.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GrainParams {
    /// True when grain is applied to this frame.
    pub apply_grain: bool,
    /// Grain seed.
    pub grain_seed: u16,
    /// Number of luma points.
    pub num_y_points: u8,
    /// Luma point values.
    pub point_y_value: [u8; 14],
    /// Luma point scalings.
    pub point_y_scaling: [u8; 14],
    /// True when chroma scaling comes from luma.
    pub chroma_scaling_from_luma: bool,
    /// Number of Cb points.
    pub num_cb_points: u8,
    /// Cb point values.
    pub point_cb_value: [u8; 14],
    /// Cb point scalings.
    pub point_cb_scaling: [u8; 14],
    /// Number of Cr points.
    pub num_cr_points: u8,
    /// Cr point values.
    pub point_cr_value: [u8; 14],
    /// Cr point scalings.
    pub point_cr_scaling: [u8; 14],
    /// Grain scaling minus 8.
    pub grain_scaling_minus_8: u8,
    /// AR coefficient lag.
    pub ar_coeff_lag: u8,
    /// Luma AR coefficients plus 128.
    pub ar_coeffs_y_plus_128: [u8; 24],
    /// Cb AR coefficients plus 128.
    pub ar_coeffs_cb_plus_128: [u8; 25],
    /// Cr AR coefficients plus 128.
    pub ar_coeffs_cr_plus_128: [u8; 25],
    /// AR coefficient shift minus 6.
    pub ar_coeff_shift_minus_6: u8,
    /// Grain scale shift.
    pub grain_scale_shift: u8,
    /// Cb multiplier.
    pub cb_mult: u8,
    /// Cb luma multiplier.
    pub cb_luma_mult: u8,
    /// Cb offset.
    pub cb_offset: u8,
    /// Cr multiplier.
    pub cr_mult: u8,
    /// Cr luma multiplier.
    pub cr_luma_mult: u8,
    /// Cr offset.
    pub cr_offset: u8,
    /// True when grain overlaps block boundaries.
    pub overlap_flag: bool,
    /// True when grain is clipped to the restricted range.
    pub clip_to_restricted_range: bool,
}

/// One reference frame slot: the state a later frame header needs
/// from it (order hint, dimensions, grain parameters, and the global
/// motion parameters of the previous frame for `read_global_param`).
#[derive(Debug, Clone)]
pub struct RefFrameSlot {
    /// True when the slot holds a decoded frame.
    pub valid: bool,
    /// Order hint of the frame in the slot.
    pub order_hint: u32,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Upscaled width in pixels.
    pub upscaled_width: u32,
    /// Render width in pixels.
    pub render_width: u32,
    /// Render height in pixels.
    pub render_height: u32,
    /// Frame type of the frame in the slot.
    pub frame_type: u8,
    /// Frame id of the frame in the slot, when frame ids are present.
    pub frame_id: u32,
    /// Segmentation feature enabled flags of the frame in the slot.
    pub seg_feature_enabled: [[bool; SEG_LVL_MAX]; MAX_SEGMENTS],
    /// Segmentation feature data of the frame in the slot.
    pub seg_feature_data: [[i32; SEG_LVL_MAX]; MAX_SEGMENTS],
    /// Film grain parameters of the frame in the slot.
    pub grain: Option<GrainParams>,
}

impl Default for RefFrameSlot {
    fn default() -> Self {
        Self {
            valid: false,
            order_hint: 0,
            width: 0,
            height: 0,
            upscaled_width: 0,
            render_width: 0,
            render_height: 0,
            frame_type: KEY_FRAME,
            frame_id: 0,
            seg_feature_enabled: [[false; SEG_LVL_MAX]; MAX_SEGMENTS],
            seg_feature_data: [[0; SEG_LVL_MAX]; MAX_SEGMENTS],
            grain: None,
        }
    }
}

/// Decoder state carried across frames of one sequence: the
/// reference frame slots and the previous global motion parameters.
/// The header parser mutates the order hints and validity flags
/// exactly where the spec does (`ref_order_hint` updates,
/// `mark_ref_frames`, and the key-frame reset); the slot contents
/// are refreshed by `refresh` after a frame header is parsed, which
/// is where a parser-only implementation applies the refresh flags.
#[derive(Debug, Clone)]
pub struct Av1ReferenceState {
    /// Reference frame slots.
    pub ref_frames: [RefFrameSlot; NUM_REF_FRAMES],
    /// Previous global motion parameters, per reference
    /// (`LAST_FRAME..ALTREF_FRAME`, index 0..6).
    pub prev_gm_params: [[i32; 6]; REFS_PER_FRAME],
    /// Current frame id, when frame ids are present.
    pub current_frame_id: u32,
    /// Previous frame id, when frame ids are present.
    pub prev_frame_id: u32,
}

impl Default for Av1ReferenceState {
    fn default() -> Self {
        let mut prev_gm_params = [[0i32; 6]; REFS_PER_FRAME];
        for params in &mut prev_gm_params {
            for (index, value) in params.iter_mut().enumerate() {
                // Identity: the (2,2) element of each 2x3 matrix is
                // 1 << WARPEDMODEL_PREC_BITS.
                *value = if index % 3 == 2 {
                    1 << WARPEDMODEL_PREC_BITS
                } else {
                    0
                };
            }
        }
        Self {
            ref_frames: Default::default(),
            prev_gm_params,
            current_frame_id: 0,
            prev_frame_id: 0,
        }
    }
}

impl Av1ReferenceState {
    /// Applies the refresh flags of a parsed frame header: every
    /// slot whose flag is set takes the new frame's dimensions,
    /// order hint, type, and grain parameters.
    pub fn refresh(&mut self, header: &FrameHeader) {
        for (index, slot) in self.ref_frames.iter_mut().enumerate() {
            if header.refresh_frame_flags & (1 << index) != 0 {
                slot.valid = true;
                slot.order_hint = header.order_hint;
                slot.width = header.frame_width;
                slot.height = header.frame_height;
                slot.upscaled_width = header.upscaled_width;
                slot.render_width = header.render_width;
                slot.render_height = header.render_height;
                slot.frame_type = header.frame_type;
                slot.frame_id = header.current_frame_id;
                slot.seg_feature_enabled = header.seg_feature_enabled;
                slot.seg_feature_data = header.seg_feature_data;
                slot.grain.clone_from(&header.grain);
            }
        }
    }
}

/// Parsed AV1 frame header (AV1 spec section 5.9.2). This is the
/// uncompressed header only; tile data is described by `TileGroup`.
/// The booleans mirror the spec syntax one-to-one.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameHeader {
    /// True when this frame shows an existing reference frame.
    pub show_existing_frame: bool,
    /// Reference slot shown, when `show_existing_frame` is set.
    pub frame_to_show_map_idx: u8,
    /// Frame type: 0 key, 1 inter, 2 intra-only, 3 switch.
    pub frame_type: u8,
    /// True when the frame is shown.
    pub show_frame: bool,
    /// True when the frame may be shown later.
    pub showable_frame: bool,
    /// True when error resilient mode is on.
    pub error_resilient_mode: bool,
    /// True when CDF updates are disabled.
    pub disable_cdf_update: bool,
    /// True when screen content tools are allowed.
    pub allow_screen_content_tools: bool,
    /// True when motion vectors are forced to integers.
    pub force_integer_mv: bool,
    /// True when the frame size may differ from the sequence size.
    pub frame_size_override_flag: bool,
    /// Order hint of this frame.
    pub order_hint: u32,
    /// Primary reference frame, or 7 for none.
    pub primary_ref_frame: u8,
    /// Refresh flags for the reference slots.
    pub refresh_frame_flags: u8,
    /// Reference slot per reference (LAST..ALTREF), for inter frames.
    pub ref_frame_idx: [u8; REFS_PER_FRAME],
    /// Frame width in pixels.
    pub frame_width: u32,
    /// Frame height in pixels.
    pub frame_height: u32,
    /// Upscaled width in pixels (before super-resolution).
    pub upscaled_width: u32,
    /// Render width in pixels.
    pub render_width: u32,
    /// Render height in pixels.
    pub render_height: u32,
    /// True when intra block copy is allowed.
    pub allow_intrabc: bool,
    /// True when high precision motion vectors are allowed.
    pub allow_high_precision_mv: bool,
    /// True when the interpolation filter is switchable.
    pub is_filter_switchable: bool,
    /// Interpolation filter (0..3), when not switchable.
    pub interpolation_filter: u8,
    /// True when the motion mode is switchable.
    pub is_motion_mode_switchable: bool,
    /// True when reference frame motion vectors are used.
    pub use_ref_frame_mvs: bool,
    /// True when the frame-end CDF update is disabled.
    pub disable_frame_end_update_cdf: bool,
    /// True when tiles are uniformly spaced.
    pub uniform_tile_spacing: bool,
    /// Number of tile columns.
    pub tile_cols: u32,
    /// Number of tile rows.
    pub tile_rows: u32,
    /// Log2 of the tile columns.
    pub tile_cols_log2: u8,
    /// Log2 of the tile rows.
    pub tile_rows_log2: u8,
    /// Tile size field width in bytes (0 when there is one tile).
    pub tile_size_bytes: u8,
    /// Tile id used for the CDF update.
    pub context_update_tile_id: u32,
    /// Start of each tile column in 8x8 blocks.
    pub mi_col_starts: Vec<u32>,
    /// Start of each tile row in 8x8 blocks.
    pub mi_row_starts: Vec<u32>,
    /// Base quantizer index.
    pub base_q_idx: u8,
    /// Delta quantizer for Y DC.
    pub delta_q_y_dc: i32,
    /// Delta quantizer for U DC.
    pub delta_q_u_dc: i32,
    /// Delta quantizer for U AC.
    pub delta_q_u_ac: i32,
    /// Delta quantizer for V DC.
    pub delta_q_v_dc: i32,
    /// Delta quantizer for V AC.
    pub delta_q_v_ac: i32,
    /// True when a quantizer matrix is used.
    pub using_qmatrix: bool,
    /// Quantizer matrix level for Y.
    pub qm_y: u8,
    /// Quantizer matrix level for U.
    pub qm_u: u8,
    /// Quantizer matrix level for V.
    pub qm_v: u8,
    /// True when segmentation is enabled.
    pub segmentation_enabled: bool,
    /// Segmentation feature enabled flags, per segment and level.
    pub seg_feature_enabled: [[bool; SEG_LVL_MAX]; MAX_SEGMENTS],
    /// Segmentation feature data, per segment and level.
    pub seg_feature_data: [[i32; SEG_LVL_MAX]; MAX_SEGMENTS],
    /// True when a segment id may be skipped.
    pub seg_id_pre_skip: bool,
    /// True when delta quantizers are present.
    pub delta_q_present: bool,
    /// Delta quantizer resolution.
    pub delta_q_res: u8,
    /// True when delta loop filter values are present.
    pub delta_lf_present: bool,
    /// Delta loop filter resolution.
    pub delta_lf_res: u8,
    /// True when the delta loop filter applies to all planes.
    pub delta_lf_multi: bool,
    /// Loop filter levels (Y vertical, Y horizontal, U, V).
    pub loop_filter_level: [u8; 4],
    /// Loop filter sharpness.
    pub loop_filter_sharpness: u8,
    /// True when loop filter deltas are enabled.
    pub loop_filter_delta_enabled: bool,
    /// CDEF damping.
    pub cdef_damping: u8,
    /// CDEF bits (number of strength entries is 1 << bits).
    pub cdef_bits: u8,
    /// CDEF strengths per entry (Y primary, Y secondary, UV primary,
    /// UV secondary).
    pub cdef_strengths: Vec<[u8; 4]>,
    /// True when loop restoration is used.
    pub uses_lr: bool,
    /// Loop restoration type per plane.
    pub lr_types: [u8; 3],
    /// TX mode: 0 only 4x4, 1 largest, 2 select.
    pub tx_mode: u8,
    /// True when the reference mode may be selected per block.
    pub reference_select: bool,
    /// True when skip mode is present.
    pub skip_mode_present: bool,
    /// True when warped motion is allowed.
    pub allow_warped_motion: bool,
    /// True when the reduced TX set is used.
    pub reduced_tx_set: bool,
    /// Global motion type per reference (LAST..ALTREF).
    pub gm_types: [u8; REFS_PER_FRAME],
    /// Global motion parameters per reference.
    pub gm_params: [[i32; 6]; REFS_PER_FRAME],
    /// True when every segment is lossless.
    pub coded_lossless: bool,
    /// True when the frame is lossless and not super-resolved.
    pub all_lossless: bool,
    /// Film grain parameters of this frame.
    pub grain: Option<GrainParams>,
    /// Current frame id, when frame ids are present.
    pub current_frame_id: u32,
    /// Frame presentation time, when temporal point info is present.
    pub frame_presentation_time: Option<u32>,
}

/// One tile of a tile group: its index and its byte range in the
/// tile group OBU payload (after the tile group header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileInfo {
    /// Tile number (row-major: row * `TileCols` + col).
    pub index: u32,
    /// Offset of the tile data within the tile group payload.
    pub offset: usize,
    /// Size of the tile data in bytes.
    pub size: usize,
}

/// Parsed tile group header (AV1 spec section 5.11): the range of
/// tiles in this group and each tile's byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileGroup {
    /// First tile number in the group.
    pub tg_start: u32,
    /// Last tile number in the group.
    pub tg_end: u32,
    /// Tiles in the group, in order.
    pub tiles: Vec<TileInfo>,
}

/// Parses a frame header OBU payload (the uncompressed header) and
/// returns the header plus the number of bits consumed. The caller
/// validates any trailing bits (frame header OBU) or parses the
/// tile group from the remaining bytes (frame OBU). The temporal and
/// spatial ids come from the OBU extension header (0 when the OBU
/// has no extension) and gate the buffer removal time reads.
///
/// # Errors
///
/// Returns `Truncated` when the payload ends mid-field,
/// `InvalidData` for a malformed header, and `Unsupported` for a
/// feature outside the parser (for example a frame id length that
/// cannot be represented).
pub fn parse_frame_header(
    payload: &[u8],
    seq: &SequenceHeader,
    state: &mut Av1ReferenceState,
    temporal_id: u8,
    spatial_id: u8,
) -> Result<(FrameHeader, usize)> {
    let mut reader = BitReader::new(payload);
    let header = parse_uncompressed_header(&mut reader, seq, state, temporal_id, spatial_id)?;
    Ok((header, reader.bit_position()))
}

/// Parses a frame OBU payload (AV1 spec section 5.10): the frame
/// header, byte alignment, and the tile group. The temporal and
/// spatial ids come from the OBU extension header (0 when the OBU
/// has no extension).
///
/// # Errors
///
/// Returns `Truncated` for a cut-short header or tile group,
/// `InvalidData` for a set byte-alignment bit or a tile group that
/// overruns the payload, and `Unsupported` for a feature outside
/// the parser.
pub fn parse_frame_obu(
    payload: &[u8],
    seq: &SequenceHeader,
    state: &mut Av1ReferenceState,
    temporal_id: u8,
    spatial_id: u8,
) -> Result<(FrameHeader, TileGroup)> {
    let mut reader = BitReader::new(payload);
    let header = parse_uncompressed_header(&mut reader, seq, state, temporal_id, spatial_id)?;
    read_byte_alignment(&mut reader)?;
    let tile_data_start = reader.bit_position() / 8;
    let tile_data = &payload[tile_data_start..];
    let tile_group = parse_tile_group_obu(tile_data, &header, tile_data_start)?;
    Ok((header, tile_group))
}

/// Parses a standalone tile group OBU payload (AV1 spec section
/// 5.11) for a frame whose header was already parsed. Tile data is
/// framed, not decoded. `base_offset` is the offset of `data` within
/// the enclosing frame OBU payload (0 for a standalone tile group
/// OBU); tile offsets and error offsets are reported relative to
/// that payload, not to `data`.
///
/// # Errors
///
/// Returns `Truncated` when a tile size overruns the payload, and
/// `InvalidData` for a tile range outside the frame or leftover
/// bytes after the last tile.
pub fn parse_tile_group_obu(
    data: &[u8],
    header: &FrameHeader,
    base_offset: usize,
) -> Result<TileGroup> {
    let num_tiles = header.tile_cols * header.tile_rows;
    if num_tiles == 0 {
        return Err(Error::InvalidData {
            offset: base_offset,
            detail: "av1 tile group has no tiles",
        });
    }
    let total_len = base_offset + data.len();
    let mut reader = BitReader::new(data);
    let tile_start_and_end_present = if num_tiles > 1 {
        reader.read_bit()?
    } else {
        false
    };
    let (tg_start, tg_end) = if num_tiles == 1 || !tile_start_and_end_present {
        (0, num_tiles - 1)
    } else {
        let tile_bits = u32::from(header.tile_cols_log2) + u32::from(header.tile_rows_log2);
        (reader.read_bits(tile_bits)?, reader.read_bits(tile_bits)?)
    };
    if tg_start > tg_end || tg_end >= num_tiles {
        return Err(Error::InvalidData {
            offset: base_offset + reader.bit_position() / 8,
            detail: "av1 tile group range is outside the frame",
        });
    }
    read_byte_alignment(&mut reader)?;
    // The tile sizes are read from the byte-aligned position.
    // `position` is relative to `data` for indexing; tile offsets
    // reported in `TileInfo` are relative to the enclosing payload.
    let mut position = reader.bit_position() / 8;
    let mut tiles: Vec<TileInfo> = Vec::new();
    let mut remaining = data.len() - position;
    for tile_num in tg_start..=tg_end {
        let last_tile = tile_num == tg_end;
        let size = if last_tile {
            remaining
        } else {
            let size_bytes = usize::from(header.tile_size_bytes);
            if remaining < size_bytes {
                return Err(Error::Truncated {
                    needed: base_offset + position + size_bytes,
                    had: total_len,
                });
            }
            let mut size_minus_1: usize = 0;
            for index in 0..size_bytes {
                size_minus_1 |= usize::from(data[position + index]) << (8 * index);
            }
            position += size_bytes;
            remaining -= size_bytes;
            let size = size_minus_1 + 1;
            if size > remaining {
                return Err(Error::Truncated {
                    needed: base_offset + position + size,
                    had: total_len,
                });
            }
            size
        };
        tiles.push(TileInfo {
            index: tile_num,
            offset: base_offset + position,
            size,
        });
        position += size;
        remaining -= size;
    }
    if position != data.len() {
        return Err(Error::InvalidData {
            offset: base_offset + position,
            detail: "av1 tile group has leftover bytes after the last tile",
        });
    }
    Ok(TileGroup {
        tg_start,
        tg_end,
        tiles,
    })
}

/// Result of `frame_size` / `frame_size_with_refs`: the frame
/// dimensions and the upscaled width.
struct FrameSize {
    width: u32,
    height: u32,
    upscaled_width: u32,
}

/// Result of `tile_info`.
struct TileInfoResult {
    cols: u32,
    rows: u32,
    cols_log2: u8,
    rows_log2: u8,
    size_bytes: u8,
    context_update_tile_id: u32,
    uniform: bool,
    mi_col_starts: Vec<u32>,
    mi_row_starts: Vec<u32>,
}

/// Result of `quantization_params`.
struct QuantParams {
    base_q_idx: u8,
    delta_q_y_dc: i32,
    delta_q_u_dc: i32,
    delta_q_u_ac: i32,
    delta_q_v_dc: i32,
    delta_q_v_ac: i32,
    using_qmatrix: bool,
    qm_y: u8,
    qm_u: u8,
    qm_v: u8,
}

/// Result of `segmentation_params`.
struct SegmentationParams {
    enabled: bool,
    feature_enabled: [[bool; SEG_LVL_MAX]; MAX_SEGMENTS],
    feature_data: [[i32; SEG_LVL_MAX]; MAX_SEGMENTS],
    seg_id_pre_skip: bool,
}

/// Result of `loop_filter_params`.
struct LoopFilterParams {
    level: [u8; 4],
    sharpness: u8,
    delta_enabled: bool,
}

/// Result of `cdef_params`.
struct CdefParams {
    damping: u8,
    bits: u8,
    strengths: Vec<[u8; 4]>,
}

/// Result of `lr_params`.
struct LrParams {
    uses_lr: bool,
    types: [u8; 3],
}

/// Result of `global_motion_params`.
struct GlobalMotionParams {
    types: [u8; REFS_PER_FRAME],
    params: [[i32; 6]; REFS_PER_FRAME],
}

/// Parses the uncompressed header (AV1 spec section 5.9.2). The
/// temporal and spatial ids come from the OBU extension header (0
/// when the OBU has no extension) and gate the buffer removal time
/// reads.
#[allow(clippy::too_many_lines)]
fn parse_uncompressed_header(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    state: &mut Av1ReferenceState,
    temporal_id: u8,
    spatial_id: u8,
) -> Result<FrameHeader> {
    let mut header = FrameHeader::default();
    // All eight refresh flags set (one per reference slot). Written
    // as `u8::MAX` rather than `(1 << 8) - 1`: shifting a `u8` left
    // by 8 overflows.
    let all_frames: u8 = u8::MAX;
    let id_len: u32 = if seq.frame_id_numbers_present {
        u32::from(seq.additional_frame_id_length_minus_1)
            + u32::from(seq.delta_frame_id_length_minus_2)
            + 3
    } else {
        0
    };
    let decoder_model_present = seq.decoder_model_info.is_some();
    let equal_picture_interval = seq
        .timing_info
        .is_some_and(|info| info.equal_picture_interval);

    // The reduced still picture header fixes most of the frame
    // header without reading any bits.
    let frame_is_intra;
    if seq.reduced_still_picture_header {
        header.show_existing_frame = false;
        header.frame_type = KEY_FRAME;
        frame_is_intra = true;
        header.show_frame = true;
        header.showable_frame = false;
        header.error_resilient_mode = true;
    } else {
        header.show_existing_frame = reader.read_bit()?;
        if header.show_existing_frame {
            return parse_show_existing_frame(reader, seq, state, id_len, all_frames);
        }
        header.frame_type = u8::try_from(reader.read_bits(2)?).unwrap_or(0);
        frame_is_intra = header.frame_type == INTRA_ONLY_FRAME || header.frame_type == KEY_FRAME;
        header.show_frame = reader.read_bit()?;
        if header.show_frame && decoder_model_present && !equal_picture_interval {
            header.frame_presentation_time = Some(parse_temporal_point_info(reader, seq)?);
        }
        header.showable_frame = if header.show_frame {
            header.frame_type != KEY_FRAME
        } else {
            reader.read_bit()?
        };
        header.error_resilient_mode = if header.frame_type == SWITCH_FRAME
            || (header.frame_type == KEY_FRAME && header.show_frame)
        {
            true
        } else {
            reader.read_bit()?
        };
    }
    if header.frame_type == KEY_FRAME && header.show_frame {
        for slot in &mut state.ref_frames {
            slot.valid = false;
            slot.order_hint = 0;
        }
    }
    header.disable_cdf_update = reader.read_bit()?;
    header.allow_screen_content_tools =
        if seq.seq_force_screen_content_tools == crate::av1_sequence::SELECT_SCREEN_CONTENT_TOOLS {
            reader.read_bit()?
        } else {
            seq.seq_force_screen_content_tools != 0
        };
    header.force_integer_mv = if header.allow_screen_content_tools {
        if seq.seq_force_integer_mv == crate::av1_sequence::SELECT_INTEGER_MV {
            reader.read_bit()?
        } else {
            seq.seq_force_integer_mv != 0
        }
    } else {
        false
    };
    if frame_is_intra {
        header.force_integer_mv = true;
    }
    if seq.frame_id_numbers_present {
        state.prev_frame_id = state.current_frame_id;
        state.current_frame_id = reader.read_bits(id_len)?;
        mark_ref_frames(state, seq, id_len);
    } else {
        state.current_frame_id = 0;
    }
    header.current_frame_id = state.current_frame_id;
    header.frame_size_override_flag = if header.frame_type == SWITCH_FRAME {
        true
    } else if seq.reduced_still_picture_header {
        false
    } else {
        reader.read_bit()?
    };
    header.order_hint = reader.read_bits(u32::from(seq.order_hint_bits))?;
    header.primary_ref_frame = if frame_is_intra || header.error_resilient_mode {
        PRIMARY_REF_NONE
    } else {
        u8::try_from(reader.read_bits(3)?).unwrap_or(0)
    };
    if decoder_model_present {
        let buffer_removal_time_present = reader.read_bit()?;
        if buffer_removal_time_present {
            parse_buffer_removal_time(reader, seq, temporal_id, spatial_id)?;
        }
    }
    header.allow_high_precision_mv = false;
    header.use_ref_frame_mvs = false;
    header.allow_intrabc = false;
    if header.frame_type == SWITCH_FRAME || (header.frame_type == KEY_FRAME && header.show_frame) {
        header.refresh_frame_flags = all_frames;
    } else {
        header.refresh_frame_flags = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
    }
    if (!frame_is_intra || header.refresh_frame_flags != all_frames)
        && header.error_resilient_mode
        && seq.enable_order_hint
    {
        for index in 0..NUM_REF_FRAMES {
            let ref_order_hint = reader.read_bits(u32::from(seq.order_hint_bits))?;
            if ref_order_hint != state.ref_frames[index].order_hint {
                state.ref_frames[index].valid = false;
            }
            state.ref_frames[index].order_hint = ref_order_hint;
        }
    }
    if frame_is_intra {
        let size = parse_frame_size(reader, seq, header.frame_size_override_flag)?;
        header.frame_width = size.width;
        header.frame_height = size.height;
        header.upscaled_width = size.upscaled_width;
        let (render_width, render_height) =
            parse_render_size(reader, header.upscaled_width, header.frame_height)?;
        header.render_width = render_width;
        header.render_height = render_height;
        if header.allow_screen_content_tools && header.upscaled_width == header.frame_width {
            header.allow_intrabc = reader.read_bit()?;
        }
    } else {
        let mut frame_refs_short_signaling = false;
        let mut last_frame_idx = 0u8;
        let mut gold_frame_idx = 0u8;
        if seq.enable_order_hint {
            frame_refs_short_signaling = reader.read_bit()?;
            if frame_refs_short_signaling {
                last_frame_idx = u8::try_from(reader.read_bits(3)?).unwrap_or(0);
                gold_frame_idx = u8::try_from(reader.read_bits(3)?).unwrap_or(0);
            }
        }
        if frame_refs_short_signaling {
            header.ref_frame_idx = set_frame_refs(
                last_frame_idx,
                gold_frame_idx,
                header.order_hint,
                state,
                seq,
            );
        } else {
            for index in 0..REFS_PER_FRAME {
                header.ref_frame_idx[index] = u8::try_from(reader.read_bits(3)?).unwrap_or(0);
                if seq.frame_id_numbers_present {
                    let n = u32::from(seq.delta_frame_id_length_minus_2) + 2;
                    let delta_frame_id_minus_1 = reader.read_bits(n)?;
                    let _ = delta_frame_id_minus_1;
                }
            }
        }
        if header.frame_size_override_flag && !header.error_resilient_mode {
            let size = parse_frame_size_with_refs(reader, seq, header.ref_frame_idx, state)?;
            header.frame_width = size.0;
            header.frame_height = size.1;
            header.upscaled_width = size.2;
            header.render_width = size.3;
            header.render_height = size.4;
        } else {
            let size = parse_frame_size(reader, seq, header.frame_size_override_flag)?;
            header.frame_width = size.width;
            header.frame_height = size.height;
            header.upscaled_width = size.upscaled_width;
            let (render_width, render_height) =
                parse_render_size(reader, header.upscaled_width, header.frame_height)?;
            header.render_width = render_width;
            header.render_height = render_height;
        }
        if header.force_integer_mv {
            header.allow_high_precision_mv = false;
        } else {
            header.allow_high_precision_mv = reader.read_bit()?;
        }
        let (is_filter_switchable, interpolation_filter) = parse_interpolation_filter(reader)?;
        header.is_filter_switchable = is_filter_switchable;
        header.interpolation_filter = interpolation_filter;
        header.is_motion_mode_switchable = reader.read_bit()?;
        header.use_ref_frame_mvs = if header.error_resilient_mode || !seq.enable_ref_frame_mvs {
            false
        } else {
            reader.read_bit()?
        };
    }
    header.disable_frame_end_update_cdf =
        if seq.reduced_still_picture_header || header.disable_cdf_update {
            true
        } else {
            reader.read_bit()?
        };
    let tile_info = parse_tile_info(reader, seq, header.frame_width, header.frame_height)?;
    header.uniform_tile_spacing = tile_info.uniform;
    header.tile_cols = tile_info.cols;
    header.tile_rows = tile_info.rows;
    header.tile_cols_log2 = tile_info.cols_log2;
    header.tile_rows_log2 = tile_info.rows_log2;
    header.tile_size_bytes = tile_info.size_bytes;
    header.context_update_tile_id = tile_info.context_update_tile_id;
    header.mi_col_starts = tile_info.mi_col_starts;
    header.mi_row_starts = tile_info.mi_row_starts;
    let quant = parse_quantization_params(reader, seq)?;
    header.base_q_idx = quant.base_q_idx;
    header.delta_q_y_dc = quant.delta_q_y_dc;
    header.delta_q_u_dc = quant.delta_q_u_dc;
    header.delta_q_u_ac = quant.delta_q_u_ac;
    header.delta_q_v_dc = quant.delta_q_v_dc;
    header.delta_q_v_ac = quant.delta_q_v_ac;
    header.using_qmatrix = quant.using_qmatrix;
    header.qm_y = quant.qm_y;
    header.qm_u = quant.qm_u;
    header.qm_v = quant.qm_v;
    let segmentation = parse_segmentation_params(reader, header.primary_ref_frame, state)?;
    header.segmentation_enabled = segmentation.enabled;
    header.seg_feature_enabled = segmentation.feature_enabled;
    header.seg_feature_data = segmentation.feature_data;
    header.seg_id_pre_skip = segmentation.seg_id_pre_skip;
    let (delta_q_present, delta_q_res) = parse_delta_q_params(reader, header.base_q_idx)?;
    header.delta_q_present = delta_q_present;
    header.delta_q_res = delta_q_res;
    let (delta_lf_present, delta_lf_res, delta_lf_multi) =
        parse_delta_lf_params(reader, delta_q_present, header.allow_intrabc)?;
    header.delta_lf_present = delta_lf_present;
    header.delta_lf_res = delta_lf_res;
    header.delta_lf_multi = delta_lf_multi;
    // CodedLossless and AllLossless (AV1 spec section 5.9.2): a
    // segment is lossless when its quantizer index is 0 and every
    // delta quantizer is 0.
    let mut coded_lossless = true;
    for segment in 0..MAX_SEGMENTS {
        let qindex = get_qindex(&header, segment);
        let lossless = qindex == 0
            && header.delta_q_y_dc == 0
            && header.delta_q_u_ac == 0
            && header.delta_q_u_dc == 0
            && header.delta_q_v_ac == 0
            && header.delta_q_v_dc == 0;
        if !lossless {
            coded_lossless = false;
        }
    }
    header.coded_lossless = coded_lossless;
    header.all_lossless = coded_lossless && header.frame_width == header.upscaled_width;
    let loop_filter =
        parse_loop_filter_params(reader, seq, header.coded_lossless, header.allow_intrabc)?;
    header.loop_filter_level = loop_filter.level;
    header.loop_filter_sharpness = loop_filter.sharpness;
    header.loop_filter_delta_enabled = loop_filter.delta_enabled;
    let cdef = parse_cdef_params(reader, seq, header.coded_lossless, header.allow_intrabc)?;
    header.cdef_damping = cdef.damping;
    header.cdef_bits = cdef.bits;
    header.cdef_strengths = cdef.strengths;
    let lr = parse_lr_params(reader, seq, header.all_lossless, header.allow_intrabc)?;
    header.uses_lr = lr.uses_lr;
    header.lr_types = lr.types;
    header.tx_mode = parse_tx_mode(reader, header.coded_lossless)?;
    header.reference_select = parse_frame_reference_mode(reader, frame_is_intra)?;
    header.skip_mode_present = parse_skip_mode_params(
        reader,
        seq,
        state,
        frame_is_intra,
        header.reference_select,
        header.order_hint,
        header.ref_frame_idx,
    )?;
    header.allow_warped_motion =
        if frame_is_intra || header.error_resilient_mode || !seq.enable_warped_motion {
            false
        } else {
            reader.read_bit()?
        };
    header.reduced_tx_set = reader.read_bit()?;
    let global_motion = parse_global_motion_params(
        reader,
        state,
        frame_is_intra,
        header.allow_high_precision_mv,
    )?;
    header.gm_types = global_motion.types;
    header.gm_params = global_motion.params;
    header.grain = parse_film_grain_params(
        reader,
        seq,
        state,
        header.show_frame,
        header.showable_frame,
        header.frame_type,
    )?;
    // The refresh flags apply once the header is fully parsed; the
    // slot contents are updated here because this library is
    // parser-only and never decodes the frame afterwards.
    state.refresh(&header);
    Ok(header)
}

/// Parses the `show_existing_frame` path (AV1 spec section 5.9.2):
/// the frame shows a reference frame, so only the map index, an
/// optional temporal point, an optional display frame id, and the
/// film grain parameters are read.
fn parse_show_existing_frame(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    state: &mut Av1ReferenceState,
    id_len: u32,
    all_frames: u8,
) -> Result<FrameHeader> {
    let frame_to_show_map_idx = u8::try_from(reader.read_bits(3)?).unwrap_or(0);
    let index = usize::from(frame_to_show_map_idx);
    if index >= NUM_REF_FRAMES || !state.ref_frames[index].valid {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "av1 show_existing_frame refers to an invalid reference slot",
        });
    }
    let equal_picture_interval = seq
        .timing_info
        .is_some_and(|info| info.equal_picture_interval);
    let frame_presentation_time = if seq.decoder_model_info.is_some() && !equal_picture_interval {
        Some(parse_temporal_point_info(reader, seq)?)
    } else {
        None
    };
    if seq.frame_id_numbers_present {
        let _display_frame_id = reader.read_bits(id_len)?;
    }
    let frame_type = state.ref_frames[index].frame_type;
    let refresh_frame_flags = if frame_type == KEY_FRAME {
        all_frames
    } else {
        0
    };
    let grain = if seq.film_grain_params_present {
        state.ref_frames[index].grain.clone()
    } else {
        None
    };
    let slot = &state.ref_frames[index];
    Ok(FrameHeader {
        show_existing_frame: true,
        frame_to_show_map_idx,
        frame_type,
        show_frame: true,
        showable_frame: false,
        refresh_frame_flags,
        order_hint: slot.order_hint,
        frame_width: slot.width,
        frame_height: slot.height,
        upscaled_width: slot.upscaled_width,
        render_width: slot.render_width,
        render_height: slot.render_height,
        grain,
        frame_presentation_time,
        ..Default::default()
    })
}

/// Reads the temporal point info (AV1 spec section 5.9.31): the
/// frame presentation time.
fn parse_temporal_point_info(reader: &mut BitReader<'_>, seq: &SequenceHeader) -> Result<u32> {
    let n = seq.decoder_model_info.map_or(0, |info| {
        u32::from(info.frame_presentation_time_length_minus_1) + 1
    });
    reader.read_bits(n)
}

/// Reads the buffer removal times (AV1 spec section 5.9.2). Only
/// operating points whose decoder model is present and that are in
/// the temporal and spatial layers of this OBU carry a value.
fn parse_buffer_removal_time(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    temporal_id: u8,
    spatial_id: u8,
) -> Result<()> {
    let n = seq.decoder_model_info.map_or(0, |info| {
        u32::from(info.buffer_removal_time_length_minus_1) + 1
    });
    for op in &seq.operating_points {
        if !op.decoder_model_present {
            continue;
        }
        let op_pt_idc = op.idc;
        let in_temporal_layer = (op_pt_idc >> temporal_id) & 1;
        let in_spatial_layer = (op_pt_idc >> (spatial_id + 8)) & 1;
        if op_pt_idc == 0 || (in_temporal_layer == 1 && in_spatial_layer == 1) {
            let _buffer_removal_time = reader.read_bits(n)?;
        }
    }
    Ok(())
}

/// Marks reference frames invalid by frame id (AV1 spec section
/// 5.9.4). Only called when frame ids are present.
fn mark_ref_frames(state: &mut Av1ReferenceState, seq: &SequenceHeader, id_len: u32) {
    let diff_len = u32::from(seq.delta_frame_id_length_minus_2) + 2;
    let current = state.current_frame_id;
    let diff = 1u32 << diff_len;
    for slot in &mut state.ref_frames {
        if !slot.valid {
            continue;
        }
        let frame_id = slot.frame_id;
        if current > diff {
            if frame_id > current || frame_id < current.wrapping_sub(diff) {
                slot.valid = false;
            }
        } else if frame_id > current
            && frame_id < ((1u32 << id_len).wrapping_add(current).wrapping_sub(diff))
        {
            slot.valid = false;
        }
    }
}

/// Computes the reference indices for short signaling (AV1 spec
/// section 7.8).
fn set_frame_refs(
    last_frame_idx: u8,
    gold_frame_idx: u8,
    order_hint: u32,
    state: &Av1ReferenceState,
    seq: &SequenceHeader,
) -> [u8; REFS_PER_FRAME] {
    let mut ref_frame_idx = [u8::MAX; REFS_PER_FRAME];
    // LAST_FRAME is reference 0, GOLDEN_FRAME is reference 3.
    ref_frame_idx[0] = last_frame_idx;
    ref_frame_idx[3] = gold_frame_idx;
    let mut used = [false; NUM_REF_FRAMES];
    used[usize::from(last_frame_idx)] = true;
    used[usize::from(gold_frame_idx)] = true;
    let cur_frame_hint = 1i32 << (seq.order_hint_bits - 1);
    let mut shifted: [i32; NUM_REF_FRAMES] = [0; NUM_REF_FRAMES];
    for (index, slot) in state.ref_frames.iter().enumerate() {
        shifted[index] = cur_frame_hint
            + get_relative_dist(
                slot.order_hint,
                order_hint,
                seq.order_hint_bits,
                seq.enable_order_hint,
            );
    }
    // ALTREF_FRAME (reference 6): latest backward reference.
    if let Some(reference) = find_backward(&shifted, used, cur_frame_hint, true) {
        ref_frame_idx[6] = reference;
        used[usize::from(reference)] = true;
    }
    // BWDREF_FRAME (reference 4): earliest backward reference.
    if let Some(reference) = find_backward(&shifted, used, cur_frame_hint, false) {
        ref_frame_idx[4] = reference;
        used[usize::from(reference)] = true;
    }
    // ALTREF2_FRAME (reference 5): next earliest backward reference.
    if let Some(reference) = find_backward(&shifted, used, cur_frame_hint, false) {
        ref_frame_idx[5] = reference;
        used[usize::from(reference)] = true;
    }
    // Remaining references (LAST2, LAST3, BWDREF, ALTREF2, ALTREF)
    // take the latest forward references, in that order.
    for reference_slot in [1u8, 2, 4, 5, 6] {
        let index = usize::from(reference_slot);
        if ref_frame_idx[index] != u8::MAX {
            continue;
        }
        if let Some(reference) = find_forward(&shifted, used, cur_frame_hint) {
            ref_frame_idx[index] = reference;
            used[usize::from(reference)] = true;
        }
    }
    // Any reference still unset takes the slot with the smallest
    // output order.
    for slot in &mut ref_frame_idx {
        if *slot == u8::MAX {
            let mut best: Option<u8> = None;
            let mut best_hint = i32::MAX;
            for (index, hint) in shifted.iter().enumerate() {
                if !used[index] && *hint < best_hint {
                    best_hint = *hint;
                    best = Some(u8::try_from(index).unwrap_or(0));
                }
            }
            if let Some(reference) = best {
                *slot = reference;
                used[usize::from(reference)] = true;
            } else {
                *slot = 0;
            }
        }
    }
    ref_frame_idx
}

/// Finds the latest (or earliest) backward reference: an unused
/// slot whose shifted order hint is at or after the current frame.
/// `used` is passed by value: it is an 8-byte array.
fn find_backward(
    shifted: &[i32; NUM_REF_FRAMES],
    used: [bool; NUM_REF_FRAMES],
    cur_frame_hint: i32,
    latest: bool,
) -> Option<u8> {
    let mut best: Option<u8> = None;
    let mut best_hint = if latest { i32::MIN } else { i32::MAX };
    for (index, hint) in shifted.iter().enumerate() {
        if used[index] || *hint < cur_frame_hint {
            continue;
        }
        if latest {
            if best.is_none() || *hint >= best_hint {
                best_hint = *hint;
                best = Some(u8::try_from(index).unwrap_or(0));
            }
        } else if best.is_none() || *hint < best_hint {
            best_hint = *hint;
            best = Some(u8::try_from(index).unwrap_or(0));
        }
    }
    best
}

/// Finds the latest forward reference: an unused slot whose shifted
/// order hint is before the current frame. `used` is passed by
/// value: it is an 8-byte array.
fn find_forward(
    shifted: &[i32; NUM_REF_FRAMES],
    used: [bool; NUM_REF_FRAMES],
    cur_frame_hint: i32,
) -> Option<u8> {
    let mut best: Option<u8> = None;
    let mut best_hint = i32::MIN;
    for (index, hint) in shifted.iter().enumerate() {
        if used[index] || *hint >= cur_frame_hint {
            continue;
        }
        if best.is_none() || *hint >= best_hint {
            best_hint = *hint;
            best = Some(u8::try_from(index).unwrap_or(0));
        }
    }
    best
}

/// Quantizer index for one segment (AV1 spec section 7.11.3,
/// `get_qindex` with `ignoreDeltaQ` set).
fn get_qindex(header: &FrameHeader, segment: usize) -> i32 {
    if header.seg_feature_enabled[segment][SEG_LVL_ALT_Q] {
        let data = header.seg_feature_data[segment][SEG_LVL_ALT_Q];
        (i32::from(header.base_q_idx) + data).clamp(0, 255)
    } else {
        i32::from(header.base_q_idx)
    }
}

/// Frame size (AV1 spec section 5.9.5): the explicit size when the
/// override flag is set, else the sequence maximum; then
/// super-resolution and the image size.
fn parse_frame_size(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    override_flag: bool,
) -> Result<FrameSize> {
    let (width, height) = if override_flag {
        let n_width = u32::from(seq.frame_width_bits_minus_1) + 1;
        let n_height = u32::from(seq.frame_height_bits_minus_1) + 1;
        let width_minus_1 = reader.read_bits(n_width)?;
        let height_minus_1 = reader.read_bits(n_height)?;
        (width_minus_1 + 1, height_minus_1 + 1)
    } else {
        (
            seq.max_frame_width_minus_1 + 1,
            seq.max_frame_height_minus_1 + 1,
        )
    };
    parse_superres(reader, seq, width, height)
}

/// Frame size with references (AV1 spec section 5.9.7): the first
/// reference whose `found_ref` bit is set supplies the size and
/// render size; otherwise the plain frame size and render size are
/// used. Returns (width, height, upscaled width, render width,
/// render height).
fn parse_frame_size_with_refs(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    ref_frame_idx: [u8; REFS_PER_FRAME],
    state: &Av1ReferenceState,
) -> Result<(u32, u32, u32, u32, u32)> {
    for &slot_index in &ref_frame_idx {
        if reader.read_bit()? {
            let slot = &state.ref_frames[usize::from(slot_index)];
            let upscaled_width = slot.upscaled_width;
            let height = slot.height;
            let (width, upscaled_width) = parse_superres_with_width(reader, seq, upscaled_width)?;
            return Ok((
                width,
                height,
                upscaled_width,
                slot.render_width,
                slot.render_height,
            ));
        }
    }
    let size = parse_frame_size(reader, seq, true)?;
    let (render_width, render_height) =
        parse_render_size(reader, size.upscaled_width, size.height)?;
    Ok((
        size.width,
        size.height,
        size.upscaled_width,
        render_width,
        render_height,
    ))
}

/// Super-resolution parameters (AV1 spec section 5.9.8).
fn parse_superres(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    frame_width: u32,
    frame_height: u32,
) -> Result<FrameSize> {
    let (width, upscaled_width) = parse_superres_with_width(reader, seq, frame_width)?;
    Ok(FrameSize {
        width,
        height: frame_height,
        upscaled_width,
    })
}

/// Applies the super-resolution denominator to a width: the
/// upscaled width is the width before super-resolution, and the
/// returned width is the coded width after downscaling.
fn parse_superres_with_width(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    upscaled_width: u32,
) -> Result<(u32, u32)> {
    let use_superres = if seq.enable_superres {
        reader.read_bit()?
    } else {
        false
    };
    let denom = if use_superres {
        reader.read_bits(SUPERRES_DENOM_BITS)? + SUPERRES_DENOM_MIN
    } else {
        SUPERRES_NUM
    };
    let width = (upscaled_width * SUPERRES_NUM + (denom / 2)) / denom;
    Ok((width, upscaled_width))
}

/// Render size (AV1 spec section 5.9.6).
fn parse_render_size(
    reader: &mut BitReader<'_>,
    upscaled_width: u32,
    frame_height: u32,
) -> Result<(u32, u32)> {
    if reader.read_bit()? {
        let render_width = reader.read_bits(16)? + 1;
        let render_height = reader.read_bits(16)? + 1;
        Ok((render_width, render_height))
    } else {
        Ok((upscaled_width, frame_height))
    }
}

/// Interpolation filter (AV1 spec section 5.9.10). Returns
/// (`is_filter_switchable`, `interpolation_filter`).
fn parse_interpolation_filter(reader: &mut BitReader<'_>) -> Result<(bool, u8)> {
    if reader.read_bit()? {
        Ok((true, 0))
    } else {
        Ok((false, u8::try_from(reader.read_bits(2)?).unwrap_or(0)))
    }
}

/// Tile info (AV1 spec section 5.9.15). The single function
/// mirrors the spec syntax, which branches on uniform spacing and
/// then on the tile log2 counts; splitting it would scatter the
/// tile-size logic across helpers with no shared state.
#[allow(clippy::too_many_lines)]
fn parse_tile_info(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    frame_width: u32,
    frame_height: u32,
) -> Result<TileInfoResult> {
    let mi_cols = 2 * ((frame_width + 7) >> 3);
    let mi_rows = 2 * ((frame_height + 7) >> 3);
    let sb_cols = if seq.use_128x128_superblock {
        (mi_cols + 31) >> 5
    } else {
        (mi_cols + 15) >> 4
    };
    let sb_rows = if seq.use_128x128_superblock {
        (mi_rows + 31) >> 5
    } else {
        (mi_rows + 15) >> 4
    };
    let sb_shift = if seq.use_128x128_superblock { 5 } else { 4 };
    let sb_size = sb_shift + 2;
    let max_tile_width_sb = MAX_TILE_WIDTH >> sb_size;
    let max_tile_area_sb = MAX_TILE_AREA_SB >> (2 * sb_size);
    let min_log2_tile_cols = tile_log2(max_tile_width_sb, sb_cols);
    let max_log2_tile_cols = tile_log2(1, sb_cols.min(MAX_TILE_COLS));
    let max_log2_tile_rows = tile_log2(1, sb_rows.min(MAX_TILE_ROWS));
    let min_log2_tiles = min_log2_tile_cols.max(tile_log2(max_tile_area_sb, sb_rows * sb_cols));
    let uniform = reader.read_bit()?;
    let (cols, rows, cols_log2, rows_log2, mi_col_starts, mi_row_starts) = if uniform {
        let mut cols_log2 = min_log2_tile_cols;
        while cols_log2 < max_log2_tile_cols {
            if reader.read_bit()? {
                cols_log2 += 1;
            } else {
                break;
            }
        }
        let tile_width_sb = (sb_cols + (1 << cols_log2) - 1) >> cols_log2;
        let mut mi_col_starts: Vec<u32> = Vec::new();
        let mut start_sb = 0;
        while start_sb < sb_cols {
            mi_col_starts.push(start_sb << sb_shift);
            start_sb += tile_width_sb;
        }
        mi_col_starts.push(mi_cols);
        let cols = u32::try_from(mi_col_starts.len() - 1).unwrap_or(0);
        let min_log2_tile_rows = min_log2_tiles.saturating_sub(cols_log2);
        let mut rows_log2 = min_log2_tile_rows;
        while rows_log2 < max_log2_tile_rows {
            if reader.read_bit()? {
                rows_log2 += 1;
            } else {
                break;
            }
        }
        let tile_height_sb = (sb_rows + (1 << rows_log2) - 1) >> rows_log2;
        let mut mi_row_starts: Vec<u32> = Vec::new();
        let mut start_sb = 0;
        while start_sb < sb_rows {
            mi_row_starts.push(start_sb << sb_shift);
            start_sb += tile_height_sb;
        }
        mi_row_starts.push(mi_rows);
        let rows = u32::try_from(mi_row_starts.len() - 1).unwrap_or(0);
        (
            cols,
            rows,
            cols_log2,
            rows_log2,
            mi_col_starts,
            mi_row_starts,
        )
    } else {
        let mut widest_tile_sb = 0;
        let mut mi_col_starts: Vec<u32> = Vec::new();
        let mut start_sb = 0;
        while start_sb < sb_cols {
            mi_col_starts.push(start_sb << sb_shift);
            let max_width = (sb_cols - start_sb).min(max_tile_width_sb);
            let width_in_sbs_minus_1 = read_ns(reader, max_width)?;
            let size_sb = width_in_sbs_minus_1 + 1;
            widest_tile_sb = widest_tile_sb.max(size_sb);
            start_sb += size_sb;
        }
        mi_col_starts.push(mi_cols);
        let cols = u32::try_from(mi_col_starts.len() - 1).unwrap_or(0);
        let cols_log2 = tile_log2(1, cols);
        let max_tile_area_sb = if min_log2_tiles > 0 {
            (sb_rows * sb_cols) >> (min_log2_tiles + 1)
        } else {
            sb_rows * sb_cols
        };
        let max_tile_height_sb = (max_tile_area_sb / widest_tile_sb.max(1)).max(1);
        let mut mi_row_starts: Vec<u32> = Vec::new();
        let mut start_sb = 0;
        while start_sb < sb_rows {
            mi_row_starts.push(start_sb << sb_shift);
            let max_height = (sb_rows - start_sb).min(max_tile_height_sb);
            let height_in_sbs_minus_1 = read_ns(reader, max_height)?;
            let size_sb = height_in_sbs_minus_1 + 1;
            start_sb += size_sb;
        }
        mi_row_starts.push(mi_rows);
        let rows = u32::try_from(mi_row_starts.len() - 1).unwrap_or(0);
        let rows_log2 = tile_log2(1, rows);
        (
            cols,
            rows,
            cols_log2,
            rows_log2,
            mi_col_starts,
            mi_row_starts,
        )
    };
    let (size_bytes, context_update_tile_id) = if cols_log2 > 0 || rows_log2 > 0 {
        let context_update_tile_id = reader.read_bits(cols_log2 + rows_log2)?;
        let tile_size_bytes_minus_1 = reader.read_bits(2)?;
        (
            u8::try_from(tile_size_bytes_minus_1 + 1).unwrap_or(0),
            context_update_tile_id,
        )
    } else {
        (0, 0)
    };
    Ok(TileInfoResult {
        cols,
        rows,
        cols_log2: u8::try_from(cols_log2).unwrap_or(0),
        rows_log2: u8::try_from(rows_log2).unwrap_or(0),
        size_bytes,
        context_update_tile_id,
        uniform,
        mi_col_starts,
        mi_row_starts,
    })
}

/// Quantization parameters (AV1 spec section 5.9.12). The
/// `delta_q_u_dc` / `delta_q_v_dc` / `delta_q_u_ac` /
/// `delta_q_v_ac` names are the spec's own parameter names.
#[allow(clippy::similar_names)]
fn parse_quantization_params(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
) -> Result<QuantParams> {
    let base_q_idx = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
    let delta_q_y_dc = read_delta_q(reader)?;
    let num_planes = if seq.color.mono_chrome { 1 } else { 3 };
    let (delta_q_u_dc, delta_q_u_ac, delta_q_v_dc, delta_q_v_ac) = if num_planes > 1 {
        let diff_uv_delta = if seq.color.separate_uv_delta_q {
            reader.read_bit()?
        } else {
            false
        };
        let delta_q_u_dc = read_delta_q(reader)?;
        let delta_q_u_ac = read_delta_q(reader)?;
        if diff_uv_delta {
            (
                delta_q_u_dc,
                delta_q_u_ac,
                read_delta_q(reader)?,
                read_delta_q(reader)?,
            )
        } else {
            (delta_q_u_dc, delta_q_u_ac, delta_q_u_dc, delta_q_u_ac)
        }
    } else {
        (0, 0, 0, 0)
    };
    let using_qmatrix = reader.read_bit()?;
    let (qm_y, qm_u, qm_v) = if using_qmatrix {
        let qm_y = u8::try_from(reader.read_bits(4)?).unwrap_or(0);
        let qm_u = u8::try_from(reader.read_bits(4)?).unwrap_or(0);
        let qm_v = if seq.color.separate_uv_delta_q {
            u8::try_from(reader.read_bits(4)?).unwrap_or(0)
        } else {
            qm_u
        };
        (qm_y, qm_u, qm_v)
    } else {
        (0, 0, 0)
    };
    Ok(QuantParams {
        base_q_idx,
        delta_q_y_dc,
        delta_q_u_dc,
        delta_q_u_ac,
        delta_q_v_dc,
        delta_q_v_ac,
        using_qmatrix,
        qm_y,
        qm_u,
        qm_v,
    })
}

/// Segmentation feature tables (AV1 spec section 5.9.14).
const SEG_FEATURE_BITS: [u32; SEG_LVL_MAX] = [8, 6, 6, 6, 6, 3, 0, 0];
const SEG_FEATURE_SIGNED: [bool; SEG_LVL_MAX] = [true, true, true, true, true, false, false, false];
const SEG_FEATURE_MAX: [i32; SEG_LVL_MAX] = [255, 63, 63, 63, 63, 7, 0, 0];

/// Segmentation parameters (AV1 spec section 5.9.14). When the
/// update data flag is clear, the feature tables are copied from
/// the primary reference frame's slot instead of being read.
fn parse_segmentation_params(
    reader: &mut BitReader<'_>,
    primary_ref_frame: u8,
    state: &Av1ReferenceState,
) -> Result<SegmentationParams> {
    let mut result = SegmentationParams {
        enabled: false,
        feature_enabled: [[false; SEG_LVL_MAX]; MAX_SEGMENTS],
        feature_data: [[0; SEG_LVL_MAX]; MAX_SEGMENTS],
        seg_id_pre_skip: false,
    };
    result.enabled = reader.read_bit()?;
    if !result.enabled {
        return Ok(result);
    }
    let update_data = if primary_ref_frame == PRIMARY_REF_NONE {
        true
    } else {
        let update_map = reader.read_bit()?;
        if update_map {
            let _temporal_update = reader.read_bit()?;
        }
        reader.read_bit()?
    };
    if update_data {
        for segment in 0..MAX_SEGMENTS {
            for level in 0..SEG_LVL_MAX {
                let enabled = reader.read_bit()?;
                result.feature_enabled[segment][level] = enabled;
                if enabled {
                    let bits = SEG_FEATURE_BITS[level];
                    let limit = SEG_FEATURE_MAX[level];
                    let value = if SEG_FEATURE_SIGNED[level] {
                        read_su(reader, 1 + bits)?
                    } else {
                        i32::try_from(reader.read_bits(bits)?).unwrap_or(0)
                    };
                    result.feature_data[segment][level] = value.clamp(-limit, limit);
                }
            }
        }
    } else {
        // The feature tables are inherited from the primary
        // reference frame (load_previous_segment_ids).
        let slot = &state.ref_frames[usize::from(primary_ref_frame)];
        result.feature_enabled = slot.seg_feature_enabled;
        result.feature_data = slot.seg_feature_data;
    }
    // SegIdPreSkip is set when any active segment enables a feature
    // at or above the reference frame level.
    for segment in 0..MAX_SEGMENTS {
        for level in 0..SEG_LVL_MAX {
            if result.feature_enabled[segment][level] && level >= 5 {
                result.seg_id_pre_skip = true;
            }
        }
    }
    Ok(result)
}

/// Delta quantizer parameters (AV1 spec section 5.9.17).
fn parse_delta_q_params(reader: &mut BitReader<'_>, base_q_idx: u8) -> Result<(bool, u8)> {
    let mut present = false;
    let mut res = 0;
    if base_q_idx > 0 {
        present = reader.read_bit()?;
    }
    if present {
        res = u8::try_from(reader.read_bits(2)?).unwrap_or(0);
    }
    Ok((present, res))
}

/// Delta loop filter parameters (AV1 spec section 5.9.18).
fn parse_delta_lf_params(
    reader: &mut BitReader<'_>,
    delta_q_present: bool,
    allow_intrabc: bool,
) -> Result<(bool, u8, bool)> {
    let mut present = false;
    let mut res = 0;
    let mut multi = false;
    if delta_q_present && !allow_intrabc {
        present = reader.read_bit()?;
        if present {
            res = u8::try_from(reader.read_bits(2)?).unwrap_or(0);
            multi = reader.read_bit()?;
        }
    }
    Ok((present, res, multi))
}

/// Loop filter parameters (AV1 spec section 5.9.11).
fn parse_loop_filter_params(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    coded_lossless: bool,
    allow_intrabc: bool,
) -> Result<LoopFilterParams> {
    if coded_lossless || allow_intrabc {
        return Ok(LoopFilterParams {
            level: [0, 0, 0, 0],
            sharpness: 0,
            delta_enabled: false,
        });
    }
    let mut level = [0u8; 4];
    level[0] = u8::try_from(reader.read_bits(6)?).unwrap_or(0);
    level[1] = u8::try_from(reader.read_bits(6)?).unwrap_or(0);
    let num_planes = if seq.color.mono_chrome { 1 } else { 3 };
    if num_planes > 1 && (level[0] != 0 || level[1] != 0) {
        level[2] = u8::try_from(reader.read_bits(6)?).unwrap_or(0);
        level[3] = u8::try_from(reader.read_bits(6)?).unwrap_or(0);
    }
    let sharpness = u8::try_from(reader.read_bits(3)?).unwrap_or(0);
    let delta_enabled = reader.read_bit()?;
    if delta_enabled {
        let delta_update = reader.read_bit()?;
        if delta_update {
            for _ in 0..8 {
                if reader.read_bit()? {
                    let _ref_delta = read_su(reader, 1 + 6)?;
                }
            }
            for _ in 0..2 {
                if reader.read_bit()? {
                    let _mode_delta = read_su(reader, 1 + 6)?;
                }
            }
        }
    }
    Ok(LoopFilterParams {
        level,
        sharpness,
        delta_enabled,
    })
}

/// CDEF parameters (AV1 spec section 5.9.19).
fn parse_cdef_params(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    coded_lossless: bool,
    allow_intrabc: bool,
) -> Result<CdefParams> {
    if coded_lossless || allow_intrabc || !seq.enable_cdef {
        return Ok(CdefParams {
            damping: 3,
            bits: 0,
            strengths: vec![[0; 4]],
        });
    }
    let damping = u8::try_from(reader.read_bits(2)?).unwrap_or(0) + 3;
    let bits = u8::try_from(reader.read_bits(2)?).unwrap_or(0);
    let num_planes = if seq.color.mono_chrome { 1 } else { 3 };
    let mut strengths: Vec<[u8; 4]> = Vec::new();
    for _ in 0..(1usize << bits) {
        let y_pri = u8::try_from(reader.read_bits(4)?).unwrap_or(0);
        let mut y_sec = u8::try_from(reader.read_bits(2)?).unwrap_or(0);
        if y_sec == 3 {
            y_sec += 1;
        }
        let (uv_pri, uv_sec) = if num_planes > 1 {
            let uv_pri = u8::try_from(reader.read_bits(4)?).unwrap_or(0);
            let mut uv_sec = u8::try_from(reader.read_bits(2)?).unwrap_or(0);
            if uv_sec == 3 {
                uv_sec += 1;
            }
            (uv_pri, uv_sec)
        } else {
            (0, 0)
        };
        strengths.push([y_pri, y_sec, uv_pri, uv_sec]);
    }
    Ok(CdefParams {
        damping,
        bits,
        strengths,
    })
}

/// Loop restoration parameters (AV1 spec section 5.9.20).
fn parse_lr_params(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    all_lossless: bool,
    allow_intrabc: bool,
) -> Result<LrParams> {
    if all_lossless || allow_intrabc || !seq.enable_restoration {
        return Ok(LrParams {
            uses_lr: false,
            types: [0; 3],
        });
    }
    let num_planes = if seq.color.mono_chrome { 1 } else { 3 };
    let mut types = [0u8; 3];
    let mut uses_lr = false;
    let mut uses_chroma_lr = false;
    for (plane, slot) in types.iter_mut().enumerate().take(num_planes) {
        let lr_type = u8::try_from(reader.read_bits(2)?).unwrap_or(0);
        *slot = lr_type;
        if lr_type != 0 {
            uses_lr = true;
            if plane > 0 {
                uses_chroma_lr = true;
            }
        }
    }
    if uses_lr {
        if seq.use_128x128_superblock {
            let lr_unit_shift = reader.read_bit()?;
            let _ = lr_unit_shift;
        } else {
            let lr_unit_shift = reader.read_bit()?;
            if lr_unit_shift {
                let _lr_unit_extra_shift = reader.read_bit()?;
            }
        }
        if seq.color.subsampling_x && seq.color.subsampling_y && uses_chroma_lr {
            let _lr_uv_shift = reader.read_bit()?;
        }
    }
    Ok(LrParams { uses_lr, types })
}

/// TX mode (AV1 spec section 5.9.21). Returns 0 for only 4x4, 1
/// for largest, 2 for select.
fn parse_tx_mode(reader: &mut BitReader<'_>, coded_lossless: bool) -> Result<u8> {
    if coded_lossless {
        Ok(0)
    } else if reader.read_bit()? {
        Ok(2)
    } else {
        Ok(1)
    }
}

/// Frame reference mode (AV1 spec section 5.9.23). Returns true
/// when the reference mode may be selected per block.
fn parse_frame_reference_mode(reader: &mut BitReader<'_>, frame_is_intra: bool) -> Result<bool> {
    if frame_is_intra {
        Ok(false)
    } else {
        reader.read_bit()
    }
}

/// Skip mode parameters (AV1 spec section 5.9.22). Returns true
/// when skip mode is present in the frame.
fn parse_skip_mode_params(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    state: &Av1ReferenceState,
    frame_is_intra: bool,
    reference_select: bool,
    order_hint: u32,
    ref_frame_idx: [u8; REFS_PER_FRAME],
) -> Result<bool> {
    let mut skip_mode_allowed = false;
    if !frame_is_intra && reference_select && seq.enable_order_hint {
        let mut forward_idx: Option<usize> = None;
        let mut forward_hint = 0u32;
        let mut backward_idx: Option<usize> = None;
        let mut backward_hint = 0u32;
        for (index, &slot_index) in ref_frame_idx.iter().enumerate() {
            let ref_hint = state.ref_frames[usize::from(slot_index)].order_hint;
            let dist = get_relative_dist(
                ref_hint,
                order_hint,
                seq.order_hint_bits,
                seq.enable_order_hint,
            );
            if dist < 0 {
                if forward_idx.is_none()
                    || get_relative_dist(
                        ref_hint,
                        forward_hint,
                        seq.order_hint_bits,
                        seq.enable_order_hint,
                    ) > 0
                {
                    forward_idx = Some(index);
                    forward_hint = ref_hint;
                }
            } else if dist > 0
                && (backward_idx.is_none()
                    || get_relative_dist(
                        ref_hint,
                        backward_hint,
                        seq.order_hint_bits,
                        seq.enable_order_hint,
                    ) < 0)
            {
                backward_idx = Some(index);
                backward_hint = ref_hint;
            }
        }
        if forward_idx.is_none() {
            skip_mode_allowed = false;
        } else if backward_idx.is_some() {
            skip_mode_allowed = true;
        } else {
            let forward_hint_value = forward_hint;
            let mut second_forward_idx: Option<usize> = None;
            let mut second_forward_hint = 0u32;
            for (index, &slot_index) in ref_frame_idx.iter().enumerate() {
                let ref_hint = state.ref_frames[usize::from(slot_index)].order_hint;
                let dist = get_relative_dist(
                    ref_hint,
                    forward_hint_value,
                    seq.order_hint_bits,
                    seq.enable_order_hint,
                );
                if dist < 0
                    && (second_forward_idx.is_none()
                        || get_relative_dist(
                            ref_hint,
                            second_forward_hint,
                            seq.order_hint_bits,
                            seq.enable_order_hint,
                        ) > 0)
                {
                    second_forward_idx = Some(index);
                    second_forward_hint = ref_hint;
                }
            }
            skip_mode_allowed = second_forward_idx.is_some();
        }
    }
    if skip_mode_allowed {
        reader.read_bit()
    } else {
        Ok(false)
    }
}

/// Global motion parameters (AV1 spec section 5.9.24).
fn parse_global_motion_params(
    reader: &mut BitReader<'_>,
    state: &mut Av1ReferenceState,
    frame_is_intra: bool,
    allow_high_precision_mv: bool,
) -> Result<GlobalMotionParams> {
    let mut result = GlobalMotionParams {
        types: [GM_IDENTITY; REFS_PER_FRAME],
        params: [[0; 6]; REFS_PER_FRAME],
    };
    for params in &mut result.params {
        for (index, value) in params.iter_mut().enumerate() {
            *value = if index % 3 == 2 {
                1 << WARPEDMODEL_PREC_BITS
            } else {
                0
            };
        }
    }
    if frame_is_intra {
        return Ok(result);
    }
    for reference in 0..REFS_PER_FRAME {
        let is_global = reader.read_bit()?;
        let gm_type = if is_global {
            let is_rot_zoom = reader.read_bit()?;
            if is_rot_zoom {
                GM_ROTZOOM
            } else if reader.read_bit()? {
                GM_TRANSLATION
            } else {
                GM_AFFINE
            }
        } else {
            GM_IDENTITY
        };
        result.types[reference] = gm_type;
        if gm_type >= GM_ROTZOOM {
            read_global_param(
                reader,
                state,
                allow_high_precision_mv,
                gm_type,
                reference,
                2,
                &mut result.params,
            )?;
            read_global_param(
                reader,
                state,
                allow_high_precision_mv,
                gm_type,
                reference,
                3,
                &mut result.params,
            )?;
            if gm_type == GM_AFFINE {
                read_global_param(
                    reader,
                    state,
                    allow_high_precision_mv,
                    gm_type,
                    reference,
                    4,
                    &mut result.params,
                )?;
                read_global_param(
                    reader,
                    state,
                    allow_high_precision_mv,
                    gm_type,
                    reference,
                    5,
                    &mut result.params,
                )?;
            } else {
                result.params[reference][4] = -result.params[reference][3];
                result.params[reference][5] = result.params[reference][2];
            }
        }
        if gm_type >= GM_TRANSLATION {
            read_global_param(
                reader,
                state,
                allow_high_precision_mv,
                gm_type,
                reference,
                0,
                &mut result.params,
            )?;
            read_global_param(
                reader,
                state,
                allow_high_precision_mv,
                gm_type,
                reference,
                1,
                &mut result.params,
            )?;
        }
    }
    // The parsed parameters become the previous parameters for the
    // next frame.
    state.prev_gm_params = result.params;
    Ok(result)
}

/// Reads one global motion parameter (AV1 spec section 5.9.25).
#[allow(clippy::too_many_arguments)]
fn read_global_param(
    reader: &mut BitReader<'_>,
    state: &Av1ReferenceState,
    allow_high_precision_mv: bool,
    gm_type: u8,
    reference: usize,
    index: usize,
    params: &mut [[i32; 6]; REFS_PER_FRAME],
) -> Result<()> {
    let (abs_bits, prec_bits) = if index < 2 {
        if gm_type == GM_TRANSLATION {
            let adjust = u32::from(!allow_high_precision_mv);
            (
                GM_ABS_TRANS_ONLY_BITS - adjust,
                GM_TRANS_ONLY_PREC_BITS - adjust,
            )
        } else {
            (GM_ABS_TRANS_BITS, GM_TRANS_PREC_BITS)
        }
    } else {
        (GM_ABS_ALPHA_BITS, GM_ALPHA_PREC_BITS)
    };
    let prec_diff = WARPEDMODEL_PREC_BITS - prec_bits;
    let round = if index % 3 == 2 {
        1 << WARPEDMODEL_PREC_BITS
    } else {
        0
    };
    let sub = if index % 3 == 2 { 1 << prec_bits } else { 0 };
    let mx = 1i32 << abs_bits;
    let prev = state.prev_gm_params[reference][index];
    let r = (prev >> prec_diff) - sub;
    let decoded = decode_signed_subexp_with_ref(reader, -mx, mx + 1, r)?;
    params[reference][index] = (decoded << prec_diff) + round;
    Ok(())
}

/// Decodes a signed subexponential value with a reference (AV1 spec
/// section 5.9.26).
fn decode_signed_subexp_with_ref(
    reader: &mut BitReader<'_>,
    low: i32,
    high: i32,
    r: i32,
) -> Result<i32> {
    let value = decode_unsigned_subexp_with_ref(reader, high - low, r - low)?;
    Ok(value + low)
}

/// Decodes an unsigned subexponential value with a reference (AV1
/// spec section 5.9.27).
fn decode_unsigned_subexp_with_ref(reader: &mut BitReader<'_>, mx: i32, r: i32) -> Result<i32> {
    let value = decode_subexp(reader, mx)?;
    if (r << 1) <= mx {
        Ok(inverse_recenter(r, value))
    } else {
        Ok(mx - 1 - inverse_recenter(mx - 1 - r, value))
    }
}

/// Decodes a subexponential value (AV1 spec section 5.9.28).
fn decode_subexp(reader: &mut BitReader<'_>, num_syms: i32) -> Result<i32> {
    let mut i = 0i32;
    let mut mk = 0i32;
    let k = 3i32;
    loop {
        let b2 = if i == 0 { k } else { k + i - 1 };
        let a = 1i32 << b2;
        if num_syms <= mk + 3 * a {
            let subexp_final_bits = read_ns(reader, u32::try_from(num_syms - mk).unwrap_or(0))?;
            return Ok(i32::try_from(subexp_final_bits).unwrap_or(0) + mk);
        }
        if reader.read_bit()? {
            i += 1;
            mk += a;
        } else {
            let subexp_bits = reader.read_bits(u32::try_from(b2).unwrap_or(0))?;
            return Ok(i32::try_from(subexp_bits).unwrap_or(0) + mk);
        }
    }
}

/// Inverse recentering (AV1 spec section 5.9.29).
fn inverse_recenter(r: i32, v: i32) -> i32 {
    if v > 2 * r {
        v
    } else if v & 1 == 1 {
        r - ((v + 1) >> 1)
    } else {
        r + (v >> 1)
    }
}

/// Film grain parameters (AV1 spec section 5.9.30). Returns `None`
/// when no grain is applied to this frame.
fn parse_film_grain_params(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    state: &mut Av1ReferenceState,
    show_frame: bool,
    showable_frame: bool,
    frame_type: u8,
) -> Result<Option<GrainParams>> {
    if !seq.film_grain_params_present || (!show_frame && !showable_frame) {
        return Ok(None);
    }
    if !reader.read_bit()? {
        return Ok(None);
    }
    let mut grain = GrainParams {
        apply_grain: true,
        ..Default::default()
    };
    grain.grain_seed = u16::try_from(reader.read_bits(16)?).unwrap_or(0);
    let update_grain = if frame_type == INTER_FRAME {
        reader.read_bit()?
    } else {
        true
    };
    if !update_grain {
        let ref_idx = usize::try_from(reader.read_bits(3)?).unwrap_or(0);
        if ref_idx >= NUM_REF_FRAMES {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "av1 film grain reference index is out of range",
            });
        }
        let temp_seed = grain.grain_seed;
        grain = state.ref_frames[ref_idx].grain.clone().unwrap_or_default();
        grain.grain_seed = temp_seed;
        grain.apply_grain = true;
        return Ok(Some(grain));
    }
    grain.num_y_points = u8::try_from(reader.read_bits(4)?).unwrap_or(0);
    for index in 0..usize::from(grain.num_y_points) {
        grain.point_y_value[index] = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
        grain.point_y_scaling[index] = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
    }
    grain.chroma_scaling_from_luma = if seq.color.mono_chrome {
        false
    } else {
        reader.read_bit()?
    };
    let num_y_points = grain.num_y_points;
    if seq.color.mono_chrome
        || grain.chroma_scaling_from_luma
        || (seq.color.subsampling_x && seq.color.subsampling_y && num_y_points == 0)
    {
        grain.num_cb_points = 0;
        grain.num_cr_points = 0;
    } else {
        grain.num_cb_points = u8::try_from(reader.read_bits(4)?).unwrap_or(0);
        for index in 0..usize::from(grain.num_cb_points) {
            grain.point_cb_value[index] = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
            grain.point_cb_scaling[index] = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
        }
        grain.num_cr_points = u8::try_from(reader.read_bits(4)?).unwrap_or(0);
        for index in 0..usize::from(grain.num_cr_points) {
            grain.point_cr_value[index] = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
            grain.point_cr_scaling[index] = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
        }
    }
    grain.grain_scaling_minus_8 = u8::try_from(reader.read_bits(2)?).unwrap_or(0);
    grain.ar_coeff_lag = u8::try_from(reader.read_bits(2)?).unwrap_or(0);
    let num_pos_luma = 2 * usize::from(grain.ar_coeff_lag) * (usize::from(grain.ar_coeff_lag) + 1);
    let num_pos_chroma = if num_y_points != 0 {
        num_pos_luma + 1
    } else {
        num_pos_luma
    };
    if num_y_points != 0 {
        for index in 0..num_pos_luma {
            grain.ar_coeffs_y_plus_128[index] = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
        }
    }
    if grain.chroma_scaling_from_luma || grain.num_cb_points != 0 {
        for index in 0..num_pos_chroma {
            grain.ar_coeffs_cb_plus_128[index] = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
        }
    }
    if grain.chroma_scaling_from_luma || grain.num_cr_points != 0 {
        for index in 0..num_pos_chroma {
            grain.ar_coeffs_cr_plus_128[index] = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
        }
    }
    grain.ar_coeff_shift_minus_6 = u8::try_from(reader.read_bits(2)?).unwrap_or(0);
    grain.grain_scale_shift = u8::try_from(reader.read_bits(2)?).unwrap_or(0);
    if grain.num_cb_points != 0 {
        grain.cb_mult = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
        grain.cb_luma_mult = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
        grain.cb_offset = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
    }
    if grain.num_cr_points != 0 {
        grain.cr_mult = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
        grain.cr_luma_mult = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
        grain.cr_offset = u8::try_from(reader.read_bits(8)?).unwrap_or(0);
    }
    grain.overlap_flag = reader.read_bit()?;
    grain.clip_to_restricted_range = reader.read_bit()?;
    Ok(Some(grain))
}

/// AV1 symbol decoder (AV1 spec section 8.2). This is the arithmetic
/// decoder used inside tiles; it is separate from the Opus range
/// coder in `range_decoder.rs`, which is a different algorithm.
///
/// The decoder reads from its own bit position in the tile data.
/// `read_symbol` mutates the CDF it is given (the last entry counts
/// decodes, up to 32) unless `disable_cdf_update` is set.
#[derive(Debug)]
pub struct Av1SymbolDecoder<'a> {
    data: &'a [u8],
    bit_pos: usize,
    symbol_value: u32,
    symbol_range: u32,
    symbol_max_bits: i32,
    disable_cdf_update: bool,
}

/// Minimum probability assigned to each symbol (AV1 spec section 3).
const EC_MIN_PROB: u32 = 4;
/// Bits to reduce CDF precision during symbol decoding.
const EC_PROB_SHIFT: u32 = 6;

impl<'a> Av1SymbolDecoder<'a> {
    /// Initializes the symbol decoder for `sz` bytes of tile data
    /// (AV1 spec section 8.2.2). The tile data must start at a byte
    /// boundary.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when the tile data is shorter than the
    /// 15 bits the initialization reads.
    pub fn new(data: &'a [u8], sz: usize, disable_cdf_update: bool) -> Result<Self> {
        if data.len() < sz {
            return Err(Error::Truncated {
                needed: sz,
                had: data.len(),
            });
        }
        let mut decoder = Self {
            data,
            bit_pos: 0,
            symbol_value: 0,
            symbol_range: 1 << 15,
            symbol_max_bits: 0,
            disable_cdf_update,
        };
        let num_bits = (sz * 8).min(15);
        let buf = decoder.read_bits(num_bits)?;
        let padded_buf = buf << (15 - num_bits);
        decoder.symbol_value = ((1 << 15) - 1) ^ padded_buf;
        // Tile sizes are bounded by the OBU size (at most 16 MiB),
        // so the conversion cannot overflow in practice; the error
        // is returned rather than assumed.
        decoder.symbol_max_bits = i32::try_from(sz)
            .map_err(|_| Error::Overflow("av1 tile size exceeds i32"))?
            .checked_mul(8)
            .ok_or(Error::Overflow("av1 tile size exceeds i32"))?
            - 15;
        Ok(decoder)
    }

    /// Reads `n` bits (0..=32) MSB-first from the tile data.
    fn read_bits(&mut self, n: usize) -> Result<u32> {
        if n > 32 {
            return Err(Error::InvalidData {
                offset: self.bit_pos / 8,
                detail: "av1 symbol bit read wider than 32 bits",
            });
        }
        if self.bit_pos + n > self.data.len() * 8 {
            return Err(Error::Truncated {
                needed: self.bit_pos + n,
                had: self.data.len() * 8,
            });
        }
        let mut value: u32 = 0;
        for _ in 0..n {
            let byte = self.data[self.bit_pos / 8];
            let bit = (byte >> (7 - (self.bit_pos % 8))) & 1;
            value = (value << 1) | u32::from(bit);
            self.bit_pos += 1;
        }
        Ok(value)
    }

    /// Reads one bit at an absolute position, for the exit check.
    fn bit_at(&self, position: usize) -> bool {
        if position >= self.data.len() * 8 {
            return false;
        }
        let byte = self.data[position / 8];
        (byte >> (7 - (position % 8))) & 1 == 1
    }

    /// Current symbol value (for tests).
    #[must_use]
    pub fn symbol_value(&self) -> u32 {
        self.symbol_value
    }

    /// Current symbol range (for tests).
    #[must_use]
    pub fn symbol_range(&self) -> u32 {
        self.symbol_range
    }

    /// Remaining bits the decoder may read (for tests).
    #[must_use]
    pub fn symbol_max_bits(&self) -> i32 {
        self.symbol_max_bits
    }

    /// Decodes one symbol with the given CDF (AV1 spec section
    /// 8.2.6). The CDF has `N + 1` entries for `N` symbols; the
    /// last entry counts decodes. The CDF is adapted in place
    /// unless CDF updates are disabled.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when the tile data ends during
    /// renormalization, and `InvalidData` for a CDF with fewer than
    /// 3 entries or a final entry that is not `1 << 15`.
    pub fn read_symbol(&mut self, cdf: &mut [u16]) -> Result<u32> {
        if cdf.len() < 3 {
            return Err(Error::InvalidData {
                offset: self.bit_pos / 8,
                detail: "av1 symbol cdf has fewer than 3 entries",
            });
        }
        let n = cdf.len() - 1;
        if cdf[n - 1] != 1 << 15 {
            return Err(Error::InvalidData {
                offset: self.bit_pos / 8,
                detail: "av1 symbol cdf does not end at 1 << 15",
            });
        }
        let mut cur = self.symbol_range;
        let mut symbol: u32 = 0;
        loop {
            let prev = cur;
            let f = (1u32 << 15) - u32::from(cdf[usize::try_from(symbol).unwrap_or(0)]);
            cur = (((self.symbol_range >> 8) * (f >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT))
                + EC_MIN_PROB * (u32::try_from(n).unwrap_or(0) - symbol - 1);
            if self.symbol_value >= cur {
                self.symbol_range = prev - cur;
                self.symbol_value -= cur;
                break;
            }
            symbol += 1;
            if usize::try_from(symbol).unwrap_or(usize::MAX) >= n {
                return Err(Error::InvalidData {
                    offset: self.bit_pos / 8,
                    detail: "av1 symbol value is outside the cdf range",
                });
            }
        }
        // Renormalize.
        let bits = 15 - floor_log2(self.symbol_range);
        self.symbol_range <<= bits;
        let num_bits = usize::try_from(bits)
            .unwrap_or(0)
            .min(usize::try_from(self.symbol_max_bits.max(0)).unwrap_or(0));
        let new_data = self.read_bits(num_bits)?;
        let padded_data = new_data << (bits as usize - num_bits);
        self.symbol_value = padded_data ^ (((self.symbol_value + 1) << bits) - 1);
        self.symbol_max_bits -= i32::try_from(bits).unwrap_or(0);
        // Adapt the CDF.
        if !self.disable_cdf_update {
            let rate = 3
                + u32::from(cdf[n] > 15)
                + u32::from(cdf[n] > 31)
                + floor_log2(u32::try_from(n).unwrap_or(0)).min(2);
            let mut tmp: i32 = 0;
            for (index, cdf_entry) in cdf.iter_mut().enumerate().take(n - 1) {
                if u32::try_from(index).unwrap_or(0) == symbol {
                    tmp = 1 << 15;
                }
                let cdf_value = i32::from(*cdf_entry);
                if tmp < cdf_value {
                    *cdf_entry =
                        u16::try_from(cdf_value - ((cdf_value - tmp) >> rate)).unwrap_or(0);
                } else {
                    *cdf_entry =
                        u16::try_from(cdf_value + ((tmp - cdf_value) >> rate)).unwrap_or(0);
                }
            }
            if cdf[n] < 32 {
                cdf[n] += 1;
            }
        }
        Ok(symbol)
    }

    /// Decodes one pseudo-raw bit with equal probability (AV1 spec
    /// section 8.2.3). Used by `read_literal`.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when the tile data ends during the read.
    pub fn read_bool(&mut self) -> Result<bool> {
        let mut cdf = [1u16 << 14, 1u16 << 15, 0u16];
        Ok(self.read_symbol(&mut cdf)? != 0)
    }

    /// Exits the symbol decoder (AV1 spec section 8.2.4): validates
    /// the trailing bits of the tile data.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when the trailing one bit is not set,
    /// a trailing zero bit is set, or more than 14 padding bits were
    /// consumed.
    pub fn exit_symbol(&self) -> Result<()> {
        if self.symbol_max_bits < -14 {
            return Err(Error::InvalidData {
                offset: self.bit_pos / 8,
                detail: "av1 symbol decoder consumed too many padding bits",
            });
        }
        let trailing_bit_position = self.bit_pos
            - (15usize).min(usize::try_from((self.symbol_max_bits + 15).max(0)).unwrap_or(0));
        let padding_end_position =
            self.bit_pos + usize::try_from(self.symbol_max_bits.max(0)).unwrap_or(0);
        if !self.bit_at(trailing_bit_position) {
            return Err(Error::InvalidData {
                offset: trailing_bit_position / 8,
                detail: "av1 symbol trailing one bit is not set",
            });
        }
        for position in (trailing_bit_position + 1)..padding_end_position {
            if self.bit_at(position) {
                return Err(Error::InvalidData {
                    offset: position / 8,
                    detail: "av1 symbol trailing zero bit is set",
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::av1_obu::{ObuReader, ObuType};
    use crate::av1_sequence::parse_sequence_header;

    /// Sequence header payload of `av1_64x64.ivf` frame 0.
    const SEQ_64X64: [u8; 10] = [0x00, 0x00, 0x00, 0x02, 0xAF, 0xFF, 0x9B, 0x5F, 0x20, 0x08];

    /// Reads the IVF file's frames into (timestamp, payload) pairs.
    fn read_ivf_frames() -> Vec<(u64, Vec<u8>)> {
        // `include_bytes!` is relative to this source file and
        // works regardless of the test runner's working directory;
        // a runtime relative path would not.
        let data = include_bytes!("../tests/data/av1_64x64.ivf");
        assert_eq!(&data[0..4], b"DKIF");
        let mut frames = Vec::new();
        let mut offset = 32;
        while offset + 12 <= data.len() {
            let size = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            let timestamp = u64::from_le_bytes(data[offset + 4..offset + 12].try_into().unwrap());
            frames.push((timestamp, data[offset + 12..offset + 12 + size].to_vec()));
            offset += 12 + size;
        }
        frames
    }

    #[test]
    fn key_frame_header_matches_ffmpeg_trace() {
        let seq = parse_sequence_header(&SEQ_64X64).unwrap();
        let frames = read_ivf_frames();
        assert_eq!(frames.len(), 10);
        let mut state = Av1ReferenceState::default();
        // Frame 0: temporal delimiter, sequence header, frame OBU.
        let mut reader = ObuReader::new(&frames[0].1);
        let td = reader.next_obu().unwrap().unwrap();
        assert_eq!(td.header.obu_type, ObuType::TemporalDelimiter);
        let seq_obu = reader.next_obu().unwrap().unwrap();
        assert_eq!(seq_obu.header.obu_type, ObuType::SequenceHeader);
        assert_eq!(seq_obu.payload, SEQ_64X64);
        let frame_obu = reader.next_obu().unwrap().unwrap();
        assert_eq!(frame_obu.header.obu_type, ObuType::Frame);
        assert_eq!(frame_obu.payload.len(), 762);
        let (header, tile_group) =
            parse_frame_obu(frame_obu.payload, &seq, &mut state, 0, 0).unwrap();
        assert_eq!(header.frame_type, KEY_FRAME);
        assert!(header.show_frame);
        // Spec section 5.9.2 forces error_resilient_mode to 1 for
        // a shown key frame without reading a bit; the ffmpeg trace
        // confirms it by printing no such field for this frame.
        assert!(header.error_resilient_mode);
        assert!(!header.disable_cdf_update);
        assert!(header.allow_screen_content_tools);
        // Spec section 5.9.2 forces force_integer_mv to 1 for
        // intra frames. The ffmpeg trace prints the raw bit (0)
        // read before that rule, so the trace value and the
        // spec's final value differ here.
        assert!(header.force_integer_mv);
        assert!(!header.frame_size_override_flag);
        assert_eq!(header.order_hint, 0);
        assert_eq!(header.primary_ref_frame, PRIMARY_REF_NONE);
        assert_eq!(header.refresh_frame_flags, 0xFF);
        assert_eq!(header.frame_width, 64);
        assert_eq!(header.frame_height, 64);
        assert_eq!(header.upscaled_width, 64);
        assert_eq!(header.render_width, 64);
        assert_eq!(header.render_height, 64);
        assert!(!header.allow_intrabc);
        assert!(!header.disable_frame_end_update_cdf);
        assert!(header.uniform_tile_spacing);
        assert_eq!(header.tile_cols, 1);
        assert_eq!(header.tile_rows, 1);
        assert_eq!(header.tile_size_bytes, 0);
        assert_eq!(header.base_q_idx, 86);
        assert_eq!(header.delta_q_y_dc, 0);
        assert!(!header.using_qmatrix);
        assert!(!header.segmentation_enabled);
        assert!(!header.delta_q_present);
        assert!(!header.delta_lf_present);
        assert_eq!(header.loop_filter_level, [4, 4, 6, 3]);
        assert_eq!(header.loop_filter_sharpness, 0);
        assert!(header.loop_filter_delta_enabled);
        assert_eq!(header.cdef_damping, 4);
        assert_eq!(header.cdef_bits, 0);
        assert_eq!(header.tx_mode, 2);
        assert!(!header.reference_select);
        assert!(!header.skip_mode_present);
        // Spec section 5.9.2 forces allow_warped_motion to 0 for
        // intra frames; the ffmpeg trace prints no such field for
        // this frame.
        assert!(!header.allow_warped_motion);
        assert!(!header.reduced_tx_set);
        assert!(!header.coded_lossless);
        assert!(!header.all_lossless);
        assert_eq!(tile_group.tiles.len(), 1);
        // The frame header ends at bit 80 (byte 10), byte
        // alignment adds nothing, and the single-tile group header
        // is empty, so the tile spans the rest of the OBU.
        assert_eq!(tile_group.tiles[0].offset, 10);
        assert_eq!(tile_group.tiles[0].size, 752);
    }

    #[test]
    fn inter_frame_header_matches_ffmpeg_trace() {
        let seq = parse_sequence_header(&SEQ_64X64).unwrap();
        let frames = read_ivf_frames();
        let mut state = Av1ReferenceState::default();
        // Parse frame 0 first so the reference state is populated.
        let mut reader = ObuReader::new(&frames[0].1);
        let _ = reader.next_obu().unwrap().unwrap();
        let _ = reader.next_obu().unwrap().unwrap();
        let frame_obu = reader.next_obu().unwrap().unwrap();
        let _ = parse_frame_obu(frame_obu.payload, &seq, &mut state, 0, 0).unwrap();
        // Frame 1: temporal delimiter, then four frame OBUs. The
        // first frame OBU holds the inter frame header.
        let mut reader = ObuReader::new(&frames[1].1);
        let _ = reader.next_obu().unwrap().unwrap();
        let frame_obu = reader.next_obu().unwrap().unwrap();
        assert_eq!(frame_obu.header.obu_type, ObuType::Frame);
        let (header, tile_group) =
            parse_frame_obu(frame_obu.payload, &seq, &mut state, 0, 0).unwrap();
        assert_eq!(header.frame_type, INTER_FRAME);
        assert!(!header.show_frame);
        assert!(header.showable_frame);
        assert!(!header.error_resilient_mode);
        assert_eq!(header.primary_ref_frame, 7);
        assert_eq!(header.refresh_frame_flags, 2);
        assert_eq!(header.ref_frame_idx, [0, 0, 0, 0, 0, 0, 0]);
        assert!(header.allow_high_precision_mv);
        assert!(!header.is_filter_switchable);
        assert_eq!(header.interpolation_filter, 0);
        assert!(header.is_motion_mode_switchable);
        assert!(header.use_ref_frame_mvs);
        assert_eq!(header.base_q_idx, 98);
        assert_eq!(header.loop_filter_level, [4, 4, 10, 7]);
        assert_eq!(header.tx_mode, 2);
        assert!(!header.reference_select);
        assert!(header.allow_warped_motion);
        assert!(!header.reduced_tx_set);
        assert_eq!(header.gm_types, [GM_IDENTITY; REFS_PER_FRAME]);
        assert_eq!(tile_group.tiles.len(), 1);
        // The inter frame header ends at bit 129; byte alignment
        // pads 7 zero bits to byte 17, and the single-tile group
        // header is empty, so the tile spans the rest of the OBU.
        assert_eq!(tile_group.tiles[0].offset, 17);
        assert_eq!(tile_group.tiles[0].size, 608 - 17);
    }

    #[test]
    fn show_existing_frame_is_parsed() {
        let seq = parse_sequence_header(&SEQ_64X64).unwrap();
        let frames = read_ivf_frames();
        let mut state = Av1ReferenceState::default();
        // Parse frames 0 and 1 so the reference slots are populated.
        // IVF frame 1 holds four inter frames, not one.
        for frame in frames.iter().take(2) {
            let mut reader = ObuReader::new(&frame.1);
            while let Some(obu) = reader.next_obu().unwrap() {
                if obu.header.obu_type == ObuType::Frame {
                    let _ = parse_frame_obu(obu.payload, &seq, &mut state, 0, 0).unwrap();
                }
            }
        }
        // Frame 2 is a temporal delimiter plus a 3-byte frame header
        // OBU holding a show_existing_frame header (payload 0xb8:
        // show_existing = 1, map index = 3, then trailing bits).
        let mut reader = ObuReader::new(&frames[2].1);
        let _ = reader.next_obu().unwrap().unwrap();
        let frame_header_obu = reader.next_obu().unwrap().unwrap();
        assert_eq!(frame_header_obu.header.obu_type, ObuType::FrameHeader);
        assert_eq!(frame_header_obu.payload, &[0xB8]);
        let (header, bits) =
            parse_frame_header(frame_header_obu.payload, &seq, &mut state, 0, 0).unwrap();
        assert!(header.show_existing_frame);
        assert_eq!(header.frame_to_show_map_idx, 3);
        assert_eq!(header.frame_type, state.ref_frames[3].frame_type);
        assert_eq!(bits, 4);
        check_trailing_bits(frame_header_obu.payload, bits).unwrap();
    }

    #[test]
    fn frame_header_truncated_payload_is_truncated() {
        let seq = parse_sequence_header(&SEQ_64X64).unwrap();
        let frames = read_ivf_frames();
        let mut reader = ObuReader::new(&frames[0].1);
        let _ = reader.next_obu().unwrap().unwrap();
        let _ = reader.next_obu().unwrap().unwrap();
        let frame_obu = reader.next_obu().unwrap().unwrap();
        let mut state = Av1ReferenceState::default();
        // Truncate inside the header (5 bytes; the key frame header
        // needs 10). Truncating inside the tile data would not
        // fail: the single tile takes the rest of the OBU, so a
        // short tile is undetectable at this level.
        let err = parse_frame_obu(&frame_obu.payload[..5], &seq, &mut state, 0, 0).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn frame_header_corrupt_bits_do_not_panic() {
        let seq = parse_sequence_header(&SEQ_64X64).unwrap();
        let frames = read_ivf_frames();
        let mut reader = ObuReader::new(&frames[0].1);
        let _ = reader.next_obu().unwrap().unwrap();
        let _ = reader.next_obu().unwrap().unwrap();
        let frame_obu = reader.next_obu().unwrap().unwrap();
        // Flip every 7th bit of the frame OBU payload; the parse
        // must return an error or a header, never panic.
        let mut corrupted = frame_obu.payload.to_vec();
        for index in (0..corrupted.len()).step_by(7) {
            corrupted[index] ^= 0x40;
        }
        let mut state = Av1ReferenceState::default();
        let _ = parse_frame_obu(&corrupted, &seq, &mut state, 0, 0);
    }

    #[test]
    fn symbol_decoder_init_values() {
        let data = [0x80u8, 0x00];
        let decoder = Av1SymbolDecoder::new(&data, 2, false).unwrap();
        assert_eq!(decoder.symbol_value(), 16383);
        assert_eq!(decoder.symbol_range(), 32768);
        assert_eq!(decoder.symbol_max_bits(), 1);
    }

    #[test]
    fn symbol_decoder_reads_bool_symbol() {
        // Hand-computed from AV1 spec section 8.2.6: with the
        // 50/50 CDF [16384, 32768, 0] and input 0x80 0x00, the
        // decoded symbol is 1 and the CDF adapts to [15360, 32768, 1].
        let data = [0x80u8, 0x00];
        let mut decoder = Av1SymbolDecoder::new(&data, 2, false).unwrap();
        let mut cdf = [16384u16, 32768, 0];
        let symbol = decoder.read_symbol(&mut cdf).unwrap();
        assert_eq!(symbol, 1);
        assert_eq!(cdf, [15360, 32768, 1]);
        assert_eq!(decoder.symbol_value(), 32767);
        assert_eq!(decoder.symbol_range(), 32776);
        assert_eq!(decoder.symbol_max_bits(), 0);
    }

    #[test]
    fn symbol_decoder_read_bool_matches_symbol() {
        let data = [0x80u8, 0x00];
        let mut decoder = Av1SymbolDecoder::new(&data, 2, false).unwrap();
        assert!(decoder.read_bool().unwrap());
    }

    #[test]
    fn symbol_decoder_exit_validates_trailing_bits() {
        // Input 0xC0 0x00: after one 50/50 symbol the trailing one
        // bit is at position 1 and every later bit is zero, so the
        // exit check passes.
        let data = [0xC0u8, 0x00];
        let mut decoder = Av1SymbolDecoder::new(&data, 2, false).unwrap();
        let mut cdf = [16384u16, 32768, 0];
        let _ = decoder.read_symbol(&mut cdf).unwrap();
        decoder.exit_symbol().unwrap();
    }

    #[test]
    fn symbol_decoder_exit_rejects_missing_trailing_bit() {
        // Input 0x80 0x00: the trailing one bit is at position 1,
        // but that bit is zero, so the exit check fails.
        let data = [0x80u8, 0x00];
        let mut decoder = Av1SymbolDecoder::new(&data, 2, false).unwrap();
        let mut cdf = [16384u16, 32768, 0];
        let _ = decoder.read_symbol(&mut cdf).unwrap();
        let err = decoder.exit_symbol().unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn symbol_decoder_truncated_data_is_truncated() {
        let data = [0x80u8];
        let err = Av1SymbolDecoder::new(&data, 2, false).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }
}
