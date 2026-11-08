//! VP9 bitstream parsing (VP9 Bitstream & Decoding Process
//! Specification v0.6, 31 March 2016).
//!
//! Parser-only, like the AV1 subset in Phase 7: the uncompressed
//! header (section 6.2) and the compressed header (section 6.3)
//! are parsed in full, superframe indexes (Appendix B) are split,
//! and tiles (section 6.4) are framed by offset and size. Tile
//! data is never decoded, so no `VideoFrame` is produced and
//! `Decoder::decode` for `Codec::Vp9` returns `Unsupported`.
//!
//! Two endianness facts were verified empirically against ffmpeg
//! 8.1.2, because the spec text is ambiguous about them: a
//! non-last tile size is `f(32)`, big-endian (the little-endian
//! reading gives an absurd size, the big-endian reading gives the
//! exact tile split), while superframe index frame sizes are
//! little-endian (a synthetic superframe built from two real
//! frames splits into 2 frames with ffmpeg when the sizes are
//! little-endian, and is rejected as "Invalid frame size" when
//! big-endian).
//!
//! The compressed header is parsed structurally: the bool decoder
//! consumes exactly the bits the syntax requires, and the reported
//! fields are `tx_mode`, `reference_mode`, and the count of
//! probability updates consumed. The updated probability *values*
//! are not tracked, because the spec's default probability tables
//! are not ported and the values feed only tile decoding, which is
//! not implemented. The parse is verified by the bool-decoder
//! marker bit (must be 0), by the decoder never running out of
//! bits, and by the zero-padding check in `exit_bool`; the
//! uncompressed header values are additionally cross-checked
//! against ffmpeg's `trace_headers` output for every test vector
//! frame.

use crate::bitreader::BitReader;
use crate::error::{Error, Result};
use crate::limits::{MAX_HEIGHT, MAX_VIDEO_PACKET_BYTES, MAX_WIDTH};

/// Frame sync code, VP9 spec section 6.2.1: the bytes 0x49, 0x83,
/// 0x42, read as three `f(8)` values.
pub const VP9_FRAME_SYNC_CODE: [u8; 3] = [0x49, 0x83, 0x42];

/// Color space, VP9 spec section 7.2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Vp9ColorSpace {
    /// Unknown (signaled outside the bitstream).
    #[default]
    Unknown,
    /// Rec. ITU-R BT.601-7.
    Bt601,
    /// Rec. ITU-R BT.709-6.
    Bt709,
    /// SMPTE-170.
    Smpte170,
    /// SMPTE-240.
    Smpte240,
    /// Rec. ITU-R BT.2020-2.
    Bt2020,
    /// Reserved.
    Reserved,
    /// sRGB (IEC 61966-2-1).
    Rgb,
}

impl Vp9ColorSpace {
    /// Maps the 3-bit `color_space` value to the enum.
    #[must_use]
    pub fn from_u32(value: u32) -> Self {
        match value {
            1 => Self::Bt601,
            2 => Self::Bt709,
            3 => Self::Smpte170,
            4 => Self::Smpte240,
            5 => Self::Bt2020,
            6 => Self::Reserved,
            7 => Self::Rgb,
            _ => Self::Unknown,
        }
    }
}

/// Interpolation filter, VP9 spec sections 6.2.7 and 7.2.7.
/// The discriminants are the spec's `interpolation_filter` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Vp9InterpolationFilter {
    /// Eight-tap filter.
    #[default]
    EightTap,
    /// Eight-tap smooth filter.
    EightTapSmooth,
    /// Eight-tap sharp filter.
    EightTapSharp,
    /// Bilinear filter.
    Bilinear,
    /// Filter signaled per block.
    Switchable,
}

/// Transform mode, VP9 spec section 6.3.1. The discriminants are
/// the spec's `tx_mode` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vp9TxMode {
    /// Only 4x4 transforms.
    Only4x4,
    /// Up to 8x8 transforms.
    Allow8x8,
    /// Up to 16x16 transforms.
    Allow16x16,
    /// Up to 32x32 transforms.
    Allow32x32,
    /// Transform size signaled per block.
    Select,
}

/// Frame reference mode, VP9 spec section 6.3.12.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Vp9ReferenceMode {
    /// One reference frame.
    #[default]
    Single,
    /// Two reference frames.
    Compound,
    /// Reference mode signaled per block.
    Select,
}

/// Color configuration, VP9 spec section 6.2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vp9ColorConfig {
    /// Bit depth: 8, 10, or 12.
    pub bit_depth: u32,
    /// Color space.
    pub color_space: Vp9ColorSpace,
    /// True for full swing, false for studio swing.
    pub color_range: bool,
    /// True when chroma is subsampled horizontally.
    pub subsampling_x: bool,
    /// True when chroma is subsampled vertically.
    pub subsampling_y: bool,
}

impl Default for Vp9ColorConfig {
    fn default() -> Self {
        Self {
            bit_depth: 8,
            color_space: Vp9ColorSpace::Bt601,
            color_range: false,
            subsampling_x: true,
            subsampling_y: true,
        }
    }
}

/// Parsed compressed header, VP9 spec section 6.3. Only the
/// structural fields are reported; probability values are not
/// tracked (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vp9CompressedHeader {
    /// Transform mode for the frame.
    pub tx_mode: Vp9TxMode,
    /// Frame reference mode (single for intra frames).
    pub reference_mode: Vp9ReferenceMode,
    /// Number of `diff_update_prob` / `update_mv_prob` calls
    /// consumed while parsing the header.
    pub prob_updates: u32,
}

/// Parsed VP9 frame header: the uncompressed header (section 6.2)
/// plus the compressed header (section 6.3) when present.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the header mirrors the VP9 spec section 6.2 fields; grouping the flags would hide the spec mapping"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vp9FrameHeader {
    /// Bitstream profile (0..=3).
    pub profile: u32,
    /// True for a short show-existing frame.
    pub show_existing_frame: bool,
    /// Reference slot to show, valid when `show_existing_frame`.
    pub frame_to_show_map_idx: u32,
    /// True for a key frame (`frame_type` 0).
    pub is_keyframe: bool,
    /// True when the frame is shown after decoding.
    pub show_frame: bool,
    /// True when error-resilient mode is on.
    pub error_resilient_mode: bool,
    /// True for an intra-only inter frame.
    pub intra_only: bool,
    /// True for a key frame or an intra-only frame.
    pub frame_is_intra: bool,
    /// Frame context reset mode (0 when error-resilient).
    pub reset_frame_context: u32,
    /// Bitmask of reference slots refreshed by this frame.
    pub refresh_frame_flags: u32,
    /// Reference frame slot indexes (LAST, GOLDEN, ALTREF order).
    pub ref_frame_idx: [u32; 3],
    /// Reference sign biases (LAST, GOLDEN, ALTREF order).
    pub ref_frame_sign_bias: [bool; 3],
    /// Frame width in pixels.
    pub frame_width: u32,
    /// Frame height in pixels.
    pub frame_height: u32,
    /// Render width in pixels.
    pub render_width: u32,
    /// Render height in pixels.
    pub render_height: u32,
    /// Frame width in 8x8 mode-info blocks.
    pub mi_cols: u32,
    /// Frame height in 8x8 mode-info blocks.
    pub mi_rows: u32,
    /// Color configuration.
    pub color: Vp9ColorConfig,
    /// True when high-precision motion vectors are allowed.
    pub allow_high_precision_mv: bool,
    /// Interpolation filter for the frame.
    pub interpolation_filter: Vp9InterpolationFilter,
    /// True when the frame context is refreshed after this frame.
    pub refresh_frame_context: bool,
    /// True when frames may be decoded in parallel.
    pub frame_parallel_decoding_mode: bool,
    /// Frame context index (forced to 0 for intra or
    /// error-resilient frames, per section 6.2).
    pub frame_context_idx: u32,
    /// Loop filter level (0..=63).
    pub loop_filter_level: u32,
    /// Loop filter sharpness (0..=7).
    pub loop_filter_sharpness: u32,
    /// True when loop filter deltas are present.
    pub loop_filter_delta_enabled: bool,
    /// Effective loop filter reference deltas (carried across
    /// frames, reset by `setup_past_independence`).
    pub loop_filter_ref_deltas: [i32; 4],
    /// Effective loop filter mode deltas.
    pub loop_filter_mode_deltas: [i32; 2],
    /// Base quantization index.
    pub base_q_idx: u32,
    /// Y DC quantization delta.
    pub delta_q_y_dc: i32,
    /// UV DC quantization delta.
    pub delta_q_uv_dc: i32,
    /// UV AC quantization delta.
    pub delta_q_uv_ac: i32,
    /// True when the frame is lossless (with `base_q_idx` 0 and all
    /// quantization deltas 0).
    pub lossless: bool,
    /// True when segmentation is enabled.
    pub segmentation_enabled: bool,
    /// True when the segmentation map is updated.
    pub segmentation_update_map: bool,
    /// True when map updates are relative to the previous map.
    pub segmentation_temporal_update: bool,
    /// True when segment parameters are updated.
    pub segmentation_update_data: bool,
    /// True when segment parameters are absolute values.
    pub segmentation_abs_or_delta_update: bool,
    /// Segmentation tree probabilities (meaningful only when
    /// segmentation is enabled and the map is updated).
    pub segmentation_tree_probs: [u32; 7],
    /// Segmentation prediction probabilities.
    pub segmentation_pred_probs: [u32; 3],
    /// Segment feature values, [segment][feature].
    pub segmentation_features: [[i32; 4]; 8],
    /// Segment feature enable flags, [segment][feature].
    pub segmentation_feature_enabled: [[bool; 4]; 8],
    /// Base-2 log of the tile column count.
    pub tile_cols_log2: u32,
    /// Base-2 log of the tile row count.
    pub tile_rows_log2: u32,
    /// Compressed header size in bytes (`header_size_in_bytes`).
    pub header_size_in_bytes: u32,
    /// Parsed compressed header, `None` for show-existing frames.
    pub compressed_header: Option<Vp9CompressedHeader>,
}

impl Default for Vp9FrameHeader {
    fn default() -> Self {
        Self {
            profile: 0,
            show_existing_frame: false,
            frame_to_show_map_idx: 0,
            is_keyframe: false,
            show_frame: false,
            error_resilient_mode: false,
            intra_only: false,
            frame_is_intra: false,
            reset_frame_context: 0,
            refresh_frame_flags: 0,
            ref_frame_idx: [0; 3],
            ref_frame_sign_bias: [false; 3],
            frame_width: 0,
            frame_height: 0,
            render_width: 0,
            render_height: 0,
            mi_cols: 0,
            mi_rows: 0,
            color: Vp9ColorConfig::default(),
            allow_high_precision_mv: false,
            interpolation_filter: Vp9InterpolationFilter::EightTap,
            refresh_frame_context: false,
            frame_parallel_decoding_mode: false,
            frame_context_idx: 0,
            loop_filter_level: 0,
            loop_filter_sharpness: 0,
            loop_filter_delta_enabled: false,
            loop_filter_ref_deltas: [0; 4],
            loop_filter_mode_deltas: [0; 2],
            base_q_idx: 0,
            delta_q_y_dc: 0,
            delta_q_uv_dc: 0,
            delta_q_uv_ac: 0,
            lossless: false,
            segmentation_enabled: false,
            segmentation_update_map: false,
            segmentation_temporal_update: false,
            segmentation_update_data: false,
            segmentation_abs_or_delta_update: false,
            segmentation_tree_probs: [255; 7],
            segmentation_pred_probs: [255; 3],
            segmentation_features: [[0; 4]; 8],
            segmentation_feature_enabled: [[false; 4]; 8],
            tile_cols_log2: 0,
            tile_rows_log2: 0,
            header_size_in_bytes: 0,
            compressed_header: None,
        }
    }
}

impl Vp9FrameHeader {
    /// Validates the header against the Phase 8 decode subset:
    /// profile 0, 8-bit, YUV 4:2:0, no RGB. Parsing itself accepts
    /// the wider syntax and reports the values; this check runs
    /// when a decoder is constructed from the header.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a show-existing frame (it has
    /// no dimensions) and `Unsupported` for a header outside the
    /// subset.
    pub fn validate_subset(&self) -> Result<()> {
        if self.show_existing_frame {
            return Err(Error::InvalidArgument(
                "vp9 show-existing frame has no dimensions to validate",
            ));
        }
        if self.profile != 0 {
            return Err(Error::Unsupported(
                "vp9 profile 1, 2 and 3 are outside the phase 8 subset",
            ));
        }
        if self.color.bit_depth != 8 {
            return Err(Error::Unsupported(
                "vp9 bit depth above 8 is outside the phase 8 subset",
            ));
        }
        if !self.color.subsampling_x || !self.color.subsampling_y {
            return Err(Error::Unsupported(
                "vp9 chroma subsampling other than 4:2:0 is outside the phase 8 subset",
            ));
        }
        if self.color.color_space == Vp9ColorSpace::Rgb {
            return Err(Error::Unsupported(
                "vp9 rgb color space is outside the phase 8 subset",
            ));
        }
        Ok(())
    }
}

/// One tile of a VP9 frame (section 6.4): its grid position, its
/// mode-info block range, and its byte range within the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vp9Tile {
    /// Tile row (0-based).
    pub row: u32,
    /// Tile column (0-based).
    pub col: u32,
    /// First mode-info row of the tile.
    pub mi_row_start: u32,
    /// One past the last mode-info row of the tile.
    pub mi_row_end: u32,
    /// First mode-info column of the tile.
    pub mi_col_start: u32,
    /// One past the last mode-info column of the tile.
    pub mi_col_end: u32,
    /// Byte offset of the tile data within the frame.
    pub offset: usize,
    /// Tile data size in bytes (excluding the 4-byte size field).
    pub size: usize,
}

/// A parsed VP9 frame: header, tile framing, and byte layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vp9Frame {
    /// Frame header (uncompressed + compressed).
    pub header: Vp9FrameHeader,
    /// Tiles in raster order.
    pub tiles: Vec<Vp9Tile>,
    /// Bytes of uncompressed + compressed header.
    pub header_bytes: usize,
    /// Total frame size in bytes (== the input length).
    pub total_bytes: usize,
}

/// Decoder state carried across frames: reference frame sizes
/// (for `frame_size_with_refs`), loop filter deltas, and
/// segmentation parameters. A fresh state decodes a stream from
/// its key frame; a stream entered mid-way may fail with
/// `InvalidData` when it references an empty slot.
#[derive(Debug, Clone)]
pub struct Vp9ReferenceState {
    /// (width, height) of each of the 8 reference slots.
    pub slots: [Option<(u32, u32)>; 8],
    /// Loop filter reference deltas.
    pub loop_filter_ref_deltas: [i32; 4],
    /// Loop filter mode deltas.
    pub loop_filter_mode_deltas: [i32; 2],
    /// Segmentation tree probabilities.
    pub segmentation_tree_probs: [u32; 7],
    /// Segmentation prediction probabilities.
    pub segmentation_pred_probs: [u32; 3],
    /// Segment feature values, [segment][feature].
    pub segmentation_features: [[i32; 4]; 8],
    /// Segment feature enable flags, [segment][feature].
    pub segmentation_feature_enabled: [[bool; 4]; 8],
}

impl Default for Vp9ReferenceState {
    fn default() -> Self {
        Self {
            slots: [None; 8],
            loop_filter_ref_deltas: [0; 4],
            loop_filter_mode_deltas: [0; 2],
            segmentation_tree_probs: [255; 7],
            segmentation_pred_probs: [255; 3],
            segmentation_features: [[0; 4]; 8],
            segmentation_feature_enabled: [[false; 4]; 8],
        }
    }
}

impl Vp9ReferenceState {
    /// Refreshes the reference slots named by `flags` with the
    /// current frame size (section 8.10).
    pub fn refresh(&mut self, flags: u32, width: u32, height: u32) {
        for slot in 0..8 {
            if (flags >> slot) & 1 == 1 {
                self.slots[slot] = Some((width, height));
            }
        }
    }

    /// Resets the persistent header state, as
    /// `setup_past_independence` does (section 6.2): loop filter
    /// deltas to 0, segmentation probabilities to 255, segment
    /// features to 0 and disabled. Reference slots are not
    /// cleared here; the caller refreshes them per frame.
    fn reset_past_independence(&mut self) {
        self.loop_filter_ref_deltas = [0; 4];
        self.loop_filter_mode_deltas = [0; 2];
        self.segmentation_tree_probs = [255; 7];
        self.segmentation_pred_probs = [255; 3];
        self.segmentation_features = [[0; 4]; 8];
        self.segmentation_feature_enabled = [[false; 4]; 8];
    }
}

/// Bool decoder, VP9 spec section 9.2. The marker bit is read in
/// `new` with probability 128 and must be 0.
#[derive(Debug)]
pub struct Vp9BoolDecoder<'a> {
    data: &'a [u8],
    bit_pos: usize,
    value: u32,
    range: u32,
    /// Bits left after the first byte (8·sz − 8).
    max_bits: usize,
}

impl<'a> Vp9BoolDecoder<'a> {
    /// Initializes the decoder over `data` (section 9.2.1):
    /// `BoolValue` is the first byte, `BoolRange` is 255,
    /// `BoolMaxBits` is 8·sz − 8, then the marker bit is read.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for an empty segment and `InvalidData`
    /// when the marker bit is 1 or the segment is too short for
    /// the marker read.
    pub fn new(data: &'a [u8]) -> Result<Self> {
        if data.is_empty() {
            return Err(Error::Truncated { needed: 1, had: 0 });
        }
        let mut decoder = Self {
            data,
            bit_pos: 8,
            value: u32::from(data[0]),
            range: 255,
            max_bits: data.len() * 8 - 8,
        };
        if decoder.read_bool(128)? {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "vp9 bool decoder marker bit must be zero",
            });
        }
        Ok(decoder)
    }

    /// Reads one raw bit from the segment (`f(1)`).
    fn read_raw_bit(&mut self) -> Result<bool> {
        let byte = self.data.get(self.bit_pos / 8).ok_or(Error::Truncated {
            needed: self.bit_pos + 1,
            had: self.data.len() * 8,
        })?;
        let bit = (byte >> (7 - (self.bit_pos % 8))) & 1;
        self.bit_pos += 1;
        Ok(bit != 0)
    }

    /// Reads one bool with probability `prob` (section 9.2.2).
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a probability above 255 and
    /// `InvalidData` when the decoder runs out of bits (the spec
    /// requires this never happens for a conformant stream).
    pub fn read_bool(&mut self, prob: u32) -> Result<bool> {
        if prob > 255 {
            return Err(Error::InvalidArgument("vp9 bool probability exceeds 255"));
        }
        let split = 1 + (((self.range - 1) * prob) >> 8);
        let bit = if self.value < split {
            self.range = split;
            false
        } else {
            self.range -= split;
            self.value -= split;
            true
        };
        while self.range < 128 {
            if self.max_bits == 0 {
                return Err(Error::InvalidData {
                    offset: self.bit_pos / 8,
                    detail: "vp9 bool decoder ran out of bits",
                });
            }
            self.max_bits -= 1;
            let new_bit = self.read_raw_bit()?;
            self.range <<= 1;
            self.value = (self.value << 1) | u32::from(new_bit);
        }
        Ok(bit)
    }

    /// Reads an `n`-bit literal as `n` `read_bool(128)` calls
    /// (section 9.2.4).
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a width above 32 and
    /// propagates `read_bool` errors.
    pub fn read_literal(&mut self, n: u32) -> Result<u32> {
        if n > 32 {
            return Err(Error::InvalidArgument(
                "vp9 bool literal wider than 32 bits",
            ));
        }
        let mut value = 0u32;
        for _ in 0..n {
            value = (value << 1) | u32::from(self.read_bool(128)?);
        }
        Ok(value)
    }

    /// Exits the decoder (section 9.2.3): the remaining
    /// `BoolMaxBits` padding bits are read and must be zero.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when a padding bit is 1.
    pub fn exit(&mut self) -> Result<()> {
        while self.max_bits > 0 {
            self.max_bits -= 1;
            if self.read_raw_bit()? {
                return Err(Error::InvalidData {
                    offset: self.bit_pos / 8,
                    detail: "vp9 bool decoder padding bit must be zero",
                });
            }
        }
        Ok(())
    }

    /// Bytes consumed so far, rounded up. Equals the segment
    /// length after `exit`.
    #[must_use]
    pub fn bytes_consumed(&self) -> usize {
        self.bit_pos.div_ceil(8)
    }
}

/// Signed value, descriptor `s(n)` (section 4.9.2): magnitude
/// `f(n)` first, then a sign bit.
fn read_signed(reader: &mut BitReader<'_>, bits: u32) -> Result<i32> {
    let value = reader.read_bits(bits)?;
    // `bits` is at most 6 at every call site, so the magnitude
    // always fits in `i32`.
    let value =
        i32::try_from(value).map_err(|_| Error::Overflow("vp9 signed value exceeds i32 range"))?;
    let sign = reader.read_bit()?;
    Ok(if sign { -value } else { value })
}

/// Probability, section 6.2.12: a coded flag, then 8 bits, else 255.
fn read_prob(reader: &mut BitReader<'_>) -> Result<u32> {
    if reader.read_bit()? {
        reader.read_bits(8)
    } else {
        Ok(255)
    }
}

/// Quantization delta, section 6.2.10: a coded flag, then `s(4)`.
fn read_delta_q(reader: &mut BitReader<'_>) -> Result<i32> {
    if reader.read_bit()? {
        read_signed(reader, 4)
    } else {
        Ok(0)
    }
}

/// Frame sync code, section 6.2.1: three bytes, 0x49 0x83 0x42.
fn read_frame_sync(reader: &mut BitReader<'_>) -> Result<()> {
    for (index, expected) in VP9_FRAME_SYNC_CODE.iter().enumerate() {
        let byte = reader.read_u8()?;
        if byte != *expected {
            return Err(Error::InvalidData {
                offset: index,
                detail: "vp9 frame sync code mismatch",
            });
        }
    }
    Ok(())
}

/// Color config, section 6.2.2.
fn color_config(reader: &mut BitReader<'_>, profile: u32) -> Result<Vp9ColorConfig> {
    let bit_depth = if profile >= 2 {
        if reader.read_bit()? {
            12
        } else {
            10
        }
    } else {
        8
    };
    let color_space = Vp9ColorSpace::from_u32(reader.read_bits(3)?);
    // Section 7.2.2: RGB is not allowed when profile_low_bit is 0,
    // i.e. for profiles 0 and 2.
    if color_space == Vp9ColorSpace::Rgb && (profile == 0 || profile == 2) {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "vp9 rgb color space is not allowed for profile 0 or 2",
        });
    }
    if color_space == Vp9ColorSpace::Rgb {
        let (subsampling_x, subsampling_y) = if profile == 1 || profile == 3 {
            if reader.read_bit()? {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "vp9 reserved bit must be zero",
                });
            }
            (false, false)
        } else {
            (true, true)
        };
        return Ok(Vp9ColorConfig {
            bit_depth,
            color_space,
            color_range: true,
            subsampling_x,
            subsampling_y,
        });
    }
    let color_range = reader.read_bit()?;
    let (subsampling_x, subsampling_y) = if profile == 1 || profile == 3 {
        let x = reader.read_bit()?;
        let y = reader.read_bit()?;
        if reader.read_bit()? {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "vp9 reserved bit must be zero",
            });
        }
        (x, y)
    } else {
        (true, true)
    };
    Ok(Vp9ColorConfig {
        bit_depth,
        color_space,
        color_range,
        subsampling_x,
        subsampling_y,
    })
}

/// Frame size, section 6.2.3.
fn frame_size(reader: &mut BitReader<'_>) -> Result<(u32, u32)> {
    let width = reader.read_bits(16)? + 1;
    let height = reader.read_bits(16)? + 1;
    if width > MAX_WIDTH {
        return Err(Error::Overflow("vp9 frame width exceeds maximum"));
    }
    if height > MAX_HEIGHT {
        return Err(Error::Overflow("vp9 frame height exceeds maximum"));
    }
    Ok((width, height))
}

/// Render size, section 6.2.4.
fn render_size(reader: &mut BitReader<'_>, width: u32, height: u32) -> Result<(u32, u32)> {
    if reader.read_bit()? {
        let render_width = reader.read_bits(16)? + 1;
        let render_height = reader.read_bits(16)? + 1;
        if render_width > MAX_WIDTH {
            return Err(Error::Overflow("vp9 render width exceeds maximum"));
        }
        if render_height > MAX_HEIGHT {
            return Err(Error::Overflow("vp9 render height exceeds maximum"));
        }
        Ok((render_width, render_height))
    } else {
        Ok((width, height))
    }
}

/// Image size in mode-info and superblock units, section 6.2.6.
fn compute_image_size(width: u32, height: u32) -> (u32, u32, u32, u32) {
    let mi_cols = (width + 7) >> 3;
    let mi_rows = (height + 7) >> 3;
    let sb64_cols = (mi_cols + 7) >> 3;
    let sb64_rows = (mi_rows + 7) >> 3;
    (mi_cols, mi_rows, sb64_cols, sb64_rows)
}

/// Frame size with references, section 6.2.5. Returns frame size
/// and render size; `render_size` is part of this syntax, not a
/// separate call (see section 6.2.5).
fn frame_size_with_refs(
    reader: &mut BitReader<'_>,
    state: &Vp9ReferenceState,
    ref_frame_idx: [u32; 3],
) -> Result<(u32, u32, u32, u32)> {
    let mut found = false;
    let mut width = 0u32;
    let mut height = 0u32;
    for ref_idx in ref_frame_idx {
        if reader.read_bit()? {
            let slot = ref_idx as usize;
            let (ref_width, ref_height) = state.slots[slot].ok_or(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "vp9 reference frame slot is empty",
            })?;
            width = ref_width;
            height = ref_height;
            found = true;
            break;
        }
    }
    if !found {
        let (explicit_width, explicit_height) = frame_size(reader)?;
        width = explicit_width;
        height = explicit_height;
    }
    let (render_width, render_height) = render_size(reader, width, height)?;
    Ok((width, height, render_width, render_height))
}

/// Interpolation filter, section 6.2.7. The `literal_to_type`
/// table maps raw values 0..=3 to smooth, eight-tap, sharp,
/// bilinear.
fn read_interpolation_filter(reader: &mut BitReader<'_>) -> Result<Vp9InterpolationFilter> {
    if reader.read_bit()? {
        return Ok(Vp9InterpolationFilter::Switchable);
    }
    Ok(match reader.read_bits(2)? {
        0 => Vp9InterpolationFilter::EightTapSmooth,
        1 => Vp9InterpolationFilter::EightTap,
        2 => Vp9InterpolationFilter::EightTapSharp,
        _ => Vp9InterpolationFilter::Bilinear,
    })
}

/// Loop filter parameters, section 6.2.8. Deltas are applied to
/// the state and the effective values are reported by the caller
/// from the state.
fn loop_filter_params(
    reader: &mut BitReader<'_>,
    state: &mut Vp9ReferenceState,
) -> Result<(u32, u32, bool)> {
    let level = reader.read_bits(6)?;
    let sharpness = reader.read_bits(3)?;
    let delta_enabled = reader.read_bit()?;
    if delta_enabled && reader.read_bit()? {
        for index in 0..4 {
            if reader.read_bit()? {
                state.loop_filter_ref_deltas[index] = read_signed(reader, 6)?;
            }
        }
        for index in 0..2 {
            if reader.read_bit()? {
                state.loop_filter_mode_deltas[index] = read_signed(reader, 6)?;
            }
        }
    }
    Ok((level, sharpness, delta_enabled))
}

/// Quantization parameters, section 6.2.9. The local names
/// follow the spec section 6.2.9-6.2.10 field names.
#[allow(
    clippy::similar_names,
    reason = "names follow the VP9 spec sections 6.2.9-6.2.10"
)]
fn quantization_params(reader: &mut BitReader<'_>) -> Result<(u32, i32, i32, i32)> {
    let base_q_idx = reader.read_bits(8)?;
    let delta_q_y_dc = read_delta_q(reader)?;
    let delta_q_uv_dc = read_delta_q(reader)?;
    let delta_q_uv_ac = read_delta_q(reader)?;
    Ok((base_q_idx, delta_q_y_dc, delta_q_uv_dc, delta_q_uv_ac))
}

/// Segmentation parameters, section 6.2.11. Feature tables are
/// applied to the state; the caller reports the effective values
/// from the state. When segmentation is disabled the header
/// reports zeros.
///
/// Segment feature bit widths, section 6.2.11 (`feature_bits`):
/// quantization 8, loop filter 6, reference frame 2, skip 0.
const SEGMENT_FEATURE_BITS: [u32; 4] = [8, 6, 2, 0];
/// Whether each segment feature is signed, section 6.2.11
/// (`feature_signed`): quantization and loop filter are signed.
const SEGMENT_FEATURE_SIGNED: [bool; 4] = [true, true, false, false];

fn segmentation_params(
    reader: &mut BitReader<'_>,
    state: &mut Vp9ReferenceState,
    header: &mut Vp9FrameHeader,
) -> Result<()> {
    header.segmentation_enabled = reader.read_bit()?;
    if !header.segmentation_enabled {
        return Ok(());
    }
    header.segmentation_update_map = reader.read_bit()?;
    if header.segmentation_update_map {
        for index in 0..7 {
            state.segmentation_tree_probs[index] = read_prob(reader)?;
        }
        header.segmentation_temporal_update = reader.read_bit()?;
        for index in 0..3 {
            state.segmentation_pred_probs[index] = if header.segmentation_temporal_update {
                read_prob(reader)?
            } else {
                255
            };
        }
    }
    header.segmentation_update_data = reader.read_bit()?;
    if header.segmentation_update_data {
        header.segmentation_abs_or_delta_update = reader.read_bit()?;
        for segment in 0..8 {
            for feature in 0..4 {
                let enabled = reader.read_bit()?;
                state.segmentation_feature_enabled[segment][feature] = enabled;
                if enabled {
                    // The widest feature is 8 bits, so the value
                    // always fits in `i32`.
                    let mut value = i32::try_from(reader.read_bits(SEGMENT_FEATURE_BITS[feature])?)
                        .map_err(|_| Error::Overflow("vp9 segment feature exceeds i32 range"))?;
                    if SEGMENT_FEATURE_SIGNED[feature] && reader.read_bit()? {
                        value = -value;
                    }
                    state.segmentation_features[segment][feature] = value;
                } else {
                    state.segmentation_features[segment][feature] = 0;
                }
            }
        }
    }
    header.segmentation_tree_probs = state.segmentation_tree_probs;
    header.segmentation_pred_probs = state.segmentation_pred_probs;
    header.segmentation_features = state.segmentation_features;
    header.segmentation_feature_enabled = state.segmentation_feature_enabled;
    Ok(())
}

/// Tile info, section 6.2.13, with the min/max log2 tile-column
/// calculation of section 6.2.14.
fn tile_info(reader: &mut BitReader<'_>, sb64_cols: u32) -> Result<(u32, u32)> {
    let mut min_log2 = 0u32;
    while (64u32 << min_log2) < sb64_cols {
        min_log2 += 1;
    }
    let mut max_log2 = 1u32;
    while (sb64_cols >> max_log2) >= 4 {
        max_log2 += 1;
    }
    let max_log2 = max_log2 - 1;
    let mut tile_cols_log2 = min_log2;
    while tile_cols_log2 < max_log2 {
        if reader.read_bit()? {
            tile_cols_log2 += 1;
        } else {
            break;
        }
    }
    if tile_cols_log2 > 6 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "vp9 tile columns log2 exceeds 6",
        });
    }
    let mut tile_rows_log2 = reader.read_bits(1)?;
    if tile_rows_log2 == 1 {
        tile_rows_log2 += reader.read_bits(1)?;
    }
    Ok((tile_cols_log2, tile_rows_log2))
}

/// Parses the uncompressed header (section 6.2). Returns the
/// header and its length in bytes, including trailing zero bits.
/// The reference state is updated (reference slots are refreshed
/// by the caller in `parse_vp9_frame`, after the size is known;
/// loop filter and segmentation state is updated here).
///
/// # Errors
///
/// Returns `Truncated` for a cut-short header, `InvalidData` for
/// a bad frame marker, sync code, reserved bit, empty reference
/// slot, or non-zero trailing bit, and `Overflow` for a frame or
/// render size above the library limit.
#[allow(
    clippy::too_many_lines,
    reason = "the uncompressed header is one spec section (6.2); splitting it would hide the field order"
)]
#[allow(
    clippy::similar_names,
    reason = "names follow the VP9 spec sections 6.2.9-6.2.10"
)]
pub fn parse_vp9_frame_header(
    data: &[u8],
    state: &mut Vp9ReferenceState,
) -> Result<(Vp9FrameHeader, usize)> {
    let mut reader = BitReader::new(data);
    let mut header = Vp9FrameHeader::default();
    if reader.read_bits(2)? != 2 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "vp9 frame marker must be 2",
        });
    }
    let profile_low = reader.read_bits(1)?;
    let profile_high = reader.read_bits(1)?;
    header.profile = (profile_high << 1) | profile_low;
    if header.profile == 3 && reader.read_bit()? {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "vp9 profile 3 reserved bit must be zero",
        });
    }
    header.show_existing_frame = reader.read_bit()?;
    if header.show_existing_frame {
        header.frame_to_show_map_idx = reader.read_bits(3)?;
        header.header_size_in_bytes = 0;
        check_trailing_bits(&mut reader)?;
        return Ok((header, reader.bit_position() / 8));
    }
    header.is_keyframe = !reader.read_bit()?;
    header.show_frame = reader.read_bit()?;
    header.error_resilient_mode = reader.read_bit()?;
    if header.is_keyframe {
        read_frame_sync(&mut reader)?;
        header.color = color_config(&mut reader, header.profile)?;
        let (width, height) = frame_size(&mut reader)?;
        header.frame_width = width;
        header.frame_height = height;
        let (render_width, render_height) = render_size(&mut reader, width, height)?;
        header.render_width = render_width;
        header.render_height = render_height;
        header.refresh_frame_flags = 0xFF;
        header.frame_is_intra = true;
    } else {
        header.intra_only = if header.show_frame {
            false
        } else {
            reader.read_bit()?
        };
        header.frame_is_intra = header.intra_only;
        if !header.error_resilient_mode {
            header.reset_frame_context = reader.read_bits(2)?;
        }
        if header.intra_only {
            read_frame_sync(&mut reader)?;
            if header.profile > 0 {
                header.color = color_config(&mut reader, header.profile)?;
            } else {
                header.color = Vp9ColorConfig {
                    bit_depth: 8,
                    color_space: Vp9ColorSpace::Bt601,
                    color_range: false,
                    subsampling_x: true,
                    subsampling_y: true,
                };
            }
            header.refresh_frame_flags = reader.read_bits(8)?;
            let (width, height) = frame_size(&mut reader)?;
            header.frame_width = width;
            header.frame_height = height;
            let (render_width, render_height) = render_size(&mut reader, width, height)?;
            header.render_width = render_width;
            header.render_height = render_height;
        } else {
            header.refresh_frame_flags = reader.read_bits(8)?;
            for index in 0..3 {
                header.ref_frame_idx[index] = reader.read_bits(3)?;
                header.ref_frame_sign_bias[index] = reader.read_bit()?;
            }
            let (width, height, render_width, render_height) =
                frame_size_with_refs(&mut reader, state, header.ref_frame_idx)?;
            header.frame_width = width;
            header.frame_height = height;
            header.render_width = render_width;
            header.render_height = render_height;
            header.allow_high_precision_mv = reader.read_bit()?;
            header.interpolation_filter = read_interpolation_filter(&mut reader)?;
        }
    }
    if header.error_resilient_mode {
        header.refresh_frame_context = false;
        header.frame_parallel_decoding_mode = true;
    } else {
        header.refresh_frame_context = reader.read_bit()?;
        header.frame_parallel_decoding_mode = reader.read_bit()?;
    }
    header.frame_context_idx = reader.read_bits(2)?;
    if header.frame_is_intra || header.error_resilient_mode {
        state.reset_past_independence();
        header.frame_context_idx = 0;
    }
    let (level, sharpness, delta_enabled) = loop_filter_params(&mut reader, state)?;
    header.loop_filter_level = level;
    header.loop_filter_sharpness = sharpness;
    header.loop_filter_delta_enabled = delta_enabled;
    header.loop_filter_ref_deltas = state.loop_filter_ref_deltas;
    header.loop_filter_mode_deltas = state.loop_filter_mode_deltas;
    let (base_q_idx, delta_q_y_dc, delta_q_uv_dc, delta_q_uv_ac) =
        quantization_params(&mut reader)?;
    header.base_q_idx = base_q_idx;
    header.delta_q_y_dc = delta_q_y_dc;
    header.delta_q_uv_dc = delta_q_uv_dc;
    header.delta_q_uv_ac = delta_q_uv_ac;
    header.lossless =
        base_q_idx == 0 && delta_q_y_dc == 0 && delta_q_uv_dc == 0 && delta_q_uv_ac == 0;
    segmentation_params(&mut reader, state, &mut header)?;
    let (mi_cols, mi_rows, sb64_cols, _) =
        compute_image_size(header.frame_width, header.frame_height);
    header.mi_cols = mi_cols;
    header.mi_rows = mi_rows;
    let (tile_cols_log2, tile_rows_log2) = tile_info(&mut reader, sb64_cols)?;
    header.tile_cols_log2 = tile_cols_log2;
    header.tile_rows_log2 = tile_rows_log2;
    header.header_size_in_bytes = reader.read_bits(16)?;
    check_trailing_bits(&mut reader)?;
    Ok((header, reader.bit_position() / 8))
}

/// Trailing bits, section 6.1.1: zero bits to the byte boundary.
fn check_trailing_bits(reader: &mut BitReader<'_>) -> Result<()> {
    while reader.bit_position() % 8 != 0 {
        if reader.read_bit()? {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "vp9 trailing bit must be zero",
            });
        }
    }
    Ok(())
}

/// One `diff_update_prob` call (section 6.3.3): a `B(252)` flag,
/// then a subexponential delta when set. The delta value is
/// computed for bit consumption but not stored (see module docs).
fn diff_update_prob(decoder: &mut Vp9BoolDecoder<'_>, updates: &mut u32) -> Result<()> {
    *updates += 1;
    if decoder.read_bool(252)? {
        let _ = decode_term_subexp(decoder)?;
    }
    Ok(())
}

/// Subexponential delta, section 6.3.4. The final step is a
/// uniform value over 64..=254 (191 values): read 7 bits `v`;
/// only when `v >= 65` is one more bit read, giving
/// `(v << 1) - 1 + bit`. When `v < 65` the result is `v + 64`
/// and no further bit is consumed. This follows libvpx
/// `vp9_dsubexp.c` (`decode_uniform` + `decode_term_subexp`),
/// the reference implementation; reading the extra bit
/// unconditionally desynchronises the parse on every frame whose
/// updates reach this branch with `v < 65`.
fn decode_term_subexp(decoder: &mut Vp9BoolDecoder<'_>) -> Result<u32> {
    if decoder.read_literal(1)? == 0 {
        return decoder.read_literal(4);
    }
    if decoder.read_literal(1)? == 0 {
        return Ok(decoder.read_literal(4)? + 16);
    }
    if decoder.read_literal(1)? == 0 {
        return Ok(decoder.read_literal(5)? + 32);
    }
    let value = decoder.read_literal(7)?;
    if value < 65 {
        Ok(value + 64)
    } else {
        let bit = decoder.read_literal(1)?;
        Ok((value << 1) - 1 + bit)
    }
}

/// One `update_mv_prob` call (section 6.3.17): a `B(252)` flag,
/// then a 7-bit probability when set. The value is not stored
/// (see module docs).
fn update_mv_prob(decoder: &mut Vp9BoolDecoder<'_>, updates: &mut u32) -> Result<()> {
    *updates += 1;
    if decoder.read_bool(252)? {
        let _ = decoder.read_literal(7)?;
    }
    Ok(())
}

/// TX mode, section 6.3.1.
fn read_tx_mode(decoder: &mut Vp9BoolDecoder<'_>, lossless: bool) -> Result<Vp9TxMode> {
    if lossless {
        return Ok(Vp9TxMode::Only4x4);
    }
    let value = decoder.read_literal(2)?;
    let value = if value == 3 {
        value + decoder.read_literal(1)?
    } else {
        value
    };
    Ok(match value {
        0 => Vp9TxMode::Only4x4,
        1 => Vp9TxMode::Allow8x8,
        2 => Vp9TxMode::Allow16x16,
        3 => Vp9TxMode::Allow32x32,
        _ => Vp9TxMode::Select,
    })
}

/// TX mode probabilities, section 6.3.2. `TX_SIZES` is 4, so the
/// three loops run 1, 2, and 3 updates per context (2 contexts).
fn tx_mode_probs(decoder: &mut Vp9BoolDecoder<'_>, updates: &mut u32) -> Result<()> {
    for _ in 0..2 {
        diff_update_prob(decoder, updates)?;
        for _ in 0..2 {
            diff_update_prob(decoder, updates)?;
        }
        for _ in 0..3 {
            diff_update_prob(decoder, updates)?;
        }
    }
    Ok(())
}

/// Coefficient probabilities, section 6.3.7.
fn read_coef_probs(
    decoder: &mut Vp9BoolDecoder<'_>,
    tx_mode: Vp9TxMode,
    updates: &mut u32,
) -> Result<()> {
    let max_tx_size = match tx_mode {
        Vp9TxMode::Only4x4 => 0,
        Vp9TxMode::Allow8x8 => 1,
        Vp9TxMode::Allow16x16 => 2,
        Vp9TxMode::Allow32x32 | Vp9TxMode::Select => 3,
    };
    for _ in 0..=max_tx_size {
        if decoder.read_literal(1)? == 1 {
            for _ in 0..2 {
                for _ in 0..2 {
                    for band in 0..6 {
                        let max_l = if band == 0 { 3 } else { 6 };
                        for _ in 0..max_l {
                            for _ in 0..3 {
                                diff_update_prob(decoder, updates)?;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Frame reference mode, section 6.3.12.
fn frame_reference_mode(
    decoder: &mut Vp9BoolDecoder<'_>,
    ref_sign_bias: [bool; 3],
) -> Result<Vp9ReferenceMode> {
    // Compound prediction is allowed only when the sign biases
    // differ: GOLDEN or ALTREF against LAST.
    let compound_allowed =
        ref_sign_bias[1] != ref_sign_bias[0] || ref_sign_bias[2] != ref_sign_bias[0];
    if !compound_allowed {
        return Ok(Vp9ReferenceMode::Single);
    }
    if decoder.read_literal(1)? == 0 {
        return Ok(Vp9ReferenceMode::Single);
    }
    if decoder.read_literal(1)? == 0 {
        Ok(Vp9ReferenceMode::Compound)
    } else {
        Ok(Vp9ReferenceMode::Select)
    }
}

/// Frame reference mode probabilities, section 6.3.13.
fn frame_reference_mode_probs(
    decoder: &mut Vp9BoolDecoder<'_>,
    reference_mode: Vp9ReferenceMode,
    updates: &mut u32,
) -> Result<()> {
    if reference_mode == Vp9ReferenceMode::Select {
        for _ in 0..5 {
            diff_update_prob(decoder, updates)?;
        }
    }
    if reference_mode != Vp9ReferenceMode::Compound {
        for _ in 0..5 {
            diff_update_prob(decoder, updates)?;
            diff_update_prob(decoder, updates)?;
        }
    }
    if reference_mode != Vp9ReferenceMode::Single {
        for _ in 0..5 {
            diff_update_prob(decoder, updates)?;
        }
    }
    Ok(())
}

/// Motion vector probabilities, section 6.3.16. `MV_JOINTS` is 4,
/// `MV_CLASSES` is 11, `MV_OFFSET_BITS` is 10, `CLASS0_SIZE` is
/// 2, `MV_FR_SIZE` is 4.
fn mv_probs(
    decoder: &mut Vp9BoolDecoder<'_>,
    allow_high_precision_mv: bool,
    updates: &mut u32,
) -> Result<()> {
    for _ in 0..3 {
        update_mv_prob(decoder, updates)?;
    }
    for _ in 0..2 {
        update_mv_prob(decoder, updates)?;
        for _ in 0..10 {
            update_mv_prob(decoder, updates)?;
        }
        update_mv_prob(decoder, updates)?;
        for _ in 0..10 {
            update_mv_prob(decoder, updates)?;
        }
    }
    for _ in 0..2 {
        for _ in 0..2 {
            for _ in 0..3 {
                update_mv_prob(decoder, updates)?;
            }
        }
        for _ in 0..3 {
            update_mv_prob(decoder, updates)?;
        }
    }
    if allow_high_precision_mv {
        for _ in 0..2 {
            update_mv_prob(decoder, updates)?;
            update_mv_prob(decoder, updates)?;
        }
    }
    Ok(())
}

/// Parses the compressed header (section 6.3).
///
/// # Errors
///
/// Propagates bool-decoder errors (`InvalidData` when the marker
/// is 1 or the decoder runs out of bits).
#[allow(
    clippy::too_many_arguments,
    reason = "the compressed header depends on six uncompressed-header fields; bundling them would hide the spec mapping"
)]
fn parse_compressed_header(
    decoder: &mut Vp9BoolDecoder<'_>,
    lossless: bool,
    frame_is_intra: bool,
    interpolation_filter: Vp9InterpolationFilter,
    allow_high_precision_mv: bool,
    ref_sign_bias: [bool; 3],
) -> Result<Vp9CompressedHeader> {
    let mut updates = 0u32;
    let tx_mode = read_tx_mode(decoder, lossless)?;
    if tx_mode == Vp9TxMode::Select {
        tx_mode_probs(decoder, &mut updates)?;
    }
    read_coef_probs(decoder, tx_mode, &mut updates)?;
    for _ in 0..3 {
        diff_update_prob(decoder, &mut updates)?;
    }
    let mut reference_mode = Vp9ReferenceMode::Single;
    if !frame_is_intra {
        for _ in 0..7 {
            for _ in 0..3 {
                diff_update_prob(decoder, &mut updates)?;
            }
        }
        if interpolation_filter == Vp9InterpolationFilter::Switchable {
            for _ in 0..4 {
                for _ in 0..2 {
                    diff_update_prob(decoder, &mut updates)?;
                }
            }
        }
        for _ in 0..4 {
            diff_update_prob(decoder, &mut updates)?;
        }
        reference_mode = frame_reference_mode(decoder, ref_sign_bias)?;
        frame_reference_mode_probs(decoder, reference_mode, &mut updates)?;
        for _ in 0..4 {
            for _ in 0..9 {
                diff_update_prob(decoder, &mut updates)?;
            }
        }
        for _ in 0..16 {
            for _ in 0..3 {
                diff_update_prob(decoder, &mut updates)?;
            }
        }
        mv_probs(decoder, allow_high_precision_mv, &mut updates)?;
    }
    Ok(Vp9CompressedHeader {
        tx_mode,
        reference_mode,
        prob_updates: updates,
    })
}

/// Tile offset, section 6.4.1.
fn get_tile_offset(tile_num: u32, mis: u32, tile_log2: u32) -> u32 {
    let sbs = (mis + 7) >> 3;
    let offset = ((tile_num * sbs) >> tile_log2) << 3;
    offset.min(mis)
}

/// Frames the tiles of one frame (section 6.4). A non-last tile
/// size is `f(32)`, big-endian; the last tile takes the rest.
///
/// # Errors
///
/// Returns `Truncated` when a tile size field or a tile overruns
/// the frame, and `InvalidData` when a tile is empty (a tile
/// always carries at least its bool-decoder bytes).
fn frame_tiles(
    data: &[u8],
    tiles_start: usize,
    tile_cols_log2: u32,
    tile_rows_log2: u32,
    mi_cols: u32,
    mi_rows: u32,
) -> Result<Vec<Vp9Tile>> {
    let tile_cols = 1u32 << tile_cols_log2;
    let tile_rows = 1u32 << tile_rows_log2;
    let mut tiles = Vec::with_capacity(tile_cols as usize * tile_rows as usize);
    let mut offset = tiles_start;
    for row in 0..tile_rows {
        for col in 0..tile_cols {
            let last = row == tile_rows - 1 && col == tile_cols - 1;
            let (data_offset, size) = if last {
                (offset, data.len() - offset)
            } else {
                let size_bytes = data.get(offset..offset + 4).ok_or(Error::Truncated {
                    needed: offset + 4,
                    had: data.len(),
                })?;
                let size = u32::from_be_bytes([
                    size_bytes[0],
                    size_bytes[1],
                    size_bytes[2],
                    size_bytes[3],
                ]) as usize;
                if size > data.len() - offset - 4 {
                    return Err(Error::Truncated {
                        needed: offset + 4 + size,
                        had: data.len(),
                    });
                }
                (offset + 4, size)
            };
            if size == 0 {
                return Err(Error::InvalidData {
                    offset,
                    detail: "vp9 tile is empty",
                });
            }
            tiles.push(Vp9Tile {
                row,
                col,
                mi_row_start: get_tile_offset(row, mi_rows, tile_rows_log2),
                mi_row_end: get_tile_offset(row + 1, mi_rows, tile_rows_log2),
                mi_col_start: get_tile_offset(col, mi_cols, tile_cols_log2),
                mi_col_end: get_tile_offset(col + 1, mi_cols, tile_cols_log2),
                offset: data_offset,
                size,
            });
            offset = data_offset + size;
        }
    }
    Ok(tiles)
}

/// Parses one VP9 frame (section 6.1): uncompressed header,
/// compressed header, and tile framing. The reference state is
/// updated (slots refreshed with this frame's size) after the
/// header is parsed.
///
/// # Errors
///
/// Returns `Truncated` for a cut-short frame or tile,
/// `InvalidData` for a corrupt header or bool-decoder segment,
/// and `Overflow` for a frame size above the library limit.
pub fn parse_vp9_frame(data: &[u8], state: &mut Vp9ReferenceState) -> Result<Vp9Frame> {
    if data.is_empty() {
        return Err(Error::Truncated { needed: 1, had: 0 });
    }
    if data.len() > MAX_VIDEO_PACKET_BYTES {
        return Err(Error::AllocationLimit {
            requested: data.len(),
            limit: MAX_VIDEO_PACKET_BYTES,
        });
    }
    let (mut header, uncompressed_bytes) = parse_vp9_frame_header(data, state)?;
    if header.show_existing_frame {
        return Ok(Vp9Frame {
            header,
            tiles: Vec::new(),
            header_bytes: uncompressed_bytes,
            total_bytes: data.len(),
        });
    }
    let header_size = header.header_size_in_bytes as usize;
    let compressed_start = uncompressed_bytes;
    if compressed_start + header_size > data.len() {
        return Err(Error::Truncated {
            needed: compressed_start + header_size,
            had: data.len(),
        });
    }
    let mut bool_decoder =
        Vp9BoolDecoder::new(&data[compressed_start..compressed_start + header_size])?;
    let compressed = parse_compressed_header(
        &mut bool_decoder,
        header.lossless,
        header.frame_is_intra,
        header.interpolation_filter,
        header.allow_high_precision_mv,
        header.ref_frame_sign_bias,
    )?;
    bool_decoder.exit()?;
    if bool_decoder.bytes_consumed() != header_size {
        return Err(Error::InvalidData {
            offset: compressed_start,
            detail: "vp9 compressed header does not end at header_size_in_bytes",
        });
    }
    header.compressed_header = Some(compressed);
    let tiles_start = compressed_start + header_size;
    let tiles = frame_tiles(
        data,
        tiles_start,
        header.tile_cols_log2,
        header.tile_rows_log2,
        header.mi_cols,
        header.mi_rows,
    )?;
    state.refresh(
        header.refresh_frame_flags,
        header.frame_width,
        header.frame_height,
    );
    Ok(Vp9Frame {
        header,
        tiles,
        header_bytes: tiles_start,
        total_bytes: data.len(),
    })
}

/// Splits a chunk into frames using the superframe index
/// (Appendix B). Returns `(offset, size)` pairs in frame order.
/// A chunk without a valid index is a single frame.
///
/// The marker is `0b110` in the top 3 bits of the last byte;
/// `bytes_per_framesize_minus_1` (2 bits) and
/// `frames_in_superframe_minus_1` (3 bits) follow. The index is
/// `2 + frames * size_bytes` bytes long and its first byte must
/// equal its last byte. Frame sizes are little-endian.
///
/// # Errors
///
/// Returns `Truncated` for an empty chunk, and `InvalidData` when
/// the index is present but a frame size is zero, overruns the
/// index, or the sizes do not exactly fill the chunk.
pub fn parse_superframe(data: &[u8]) -> Result<Vec<(usize, usize)>> {
    if data.is_empty() {
        return Err(Error::Truncated { needed: 1, had: 0 });
    }
    let last = data[data.len() - 1];
    if last & 0xE0 != 0xC0 {
        return Ok(vec![(0, data.len())]);
    }
    let size_bytes = (((last >> 3) & 0x3) + 1) as usize;
    let frames = ((last & 0x7) + 1) as usize;
    let index_size = 2 + frames * size_bytes;
    if data.len() < index_size {
        return Ok(vec![(0, data.len())]);
    }
    let index_start = data.len() - index_size;
    if data[index_start] != last {
        return Ok(vec![(0, data.len())]);
    }
    let mut offset = 0usize;
    let mut out = Vec::with_capacity(frames);
    for frame in 0..frames {
        let mut size = 0usize;
        for byte in 0..size_bytes {
            size |= usize::from(data[index_start + 1 + frame * size_bytes + byte]) << (8 * byte);
        }
        if size == 0 {
            return Err(Error::InvalidData {
                offset: index_start,
                detail: "vp9 superframe frame size is zero",
            });
        }
        if offset + size > index_start {
            return Err(Error::InvalidData {
                offset: index_start,
                detail: "vp9 superframe frame size overruns the index",
            });
        }
        out.push((offset, size));
        offset += size;
    }
    if offset != index_start {
        return Err(Error::InvalidData {
            offset: index_start,
            detail: "vp9 superframe frame sizes do not fill the chunk",
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_frame_is_truncated() {
        let mut state = Vp9ReferenceState::default();
        assert!(matches!(
            parse_vp9_frame(&[], &mut state),
            Err(Error::Truncated { needed: 1, had: 0 })
        ));
    }

    #[test]
    fn bad_frame_marker_is_invalid() {
        let mut state = Vp9ReferenceState::default();
        assert!(matches!(
            parse_vp9_frame_header(&[0x00], &mut state),
            Err(Error::InvalidData { .. })
        ));
    }

    #[test]
    fn cut_short_key_frame_is_truncated() {
        // 0x80: marker 2, profile 0, show_existing 0, key frame,
        // show_frame 0, then no bits left for error_resilient_mode.
        let mut state = Vp9ReferenceState::default();
        assert!(matches!(
            parse_vp9_frame_header(&[0x80], &mut state),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn parses_show_existing_frame() {
        // Bits: marker 2 ("10"), profile 0, show_existing 1,
        // map_idx 5 ("101") -> 0b10001101 = 0x8D.
        let mut state = Vp9ReferenceState::default();
        let (header, bytes) = parse_vp9_frame_header(&[0x8D], &mut state).expect("header");
        assert!(header.show_existing_frame);
        assert_eq!(header.frame_to_show_map_idx, 5);
        assert_eq!(header.header_size_in_bytes, 0);
        assert_eq!(bytes, 1);
    }

    #[test]
    fn bool_decoder_empty_is_truncated() {
        assert!(matches!(
            Vp9BoolDecoder::new(&[]),
            Err(Error::Truncated { needed: 1, had: 0 })
        ));
    }

    #[test]
    fn bool_decoder_marker_one_is_invalid() {
        // First byte 0x80: value 128, marker read with prob 128:
        // split is 128, value 128 is not below split, so the
        // marker bit is 1.
        assert!(matches!(
            Vp9BoolDecoder::new(&[0x80]),
            Err(Error::InvalidData { .. })
        ));
    }

    #[test]
    fn bool_decoder_reads_zero_literal() {
        // 0x00 0x00 0x00: value 0, so the marker bit and every
        // literal bit read 0; the extra bytes feed renormalization.
        let mut decoder = Vp9BoolDecoder::new(&[0x00, 0x00, 0x00]).expect("decoder");
        assert_eq!(decoder.read_literal(4).expect("literal"), 0);
    }

    #[test]
    fn superframe_without_index_is_single_frame() {
        let frames = parse_superframe(&[1, 2, 3, 4]).expect("frames");
        assert_eq!(frames, vec![(0, 4)]);
    }

    #[test]
    fn superframe_splits_little_endian_index() {
        // Two frames of sizes 2 and 3. The index is 6 bytes:
        // marker byte, two little-endian sizes, marker byte.
        // Marker: 0b110 | (2-1)<<3 | (2-1) = 0xC9.
        let data = [
            0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xC9, 0x02, 0x00, 0x03, 0x00, 0xC9,
        ];
        let frames = parse_superframe(&data).expect("frames");
        assert_eq!(frames, vec![(0, 2), (2, 3)]);
    }

    #[test]
    fn superframe_mismatched_first_byte_is_single_frame() {
        // Index first byte differs from the last byte: not an
        // index per Appendix B.4, so the chunk is one frame.
        let data = [0xAA, 0xBB, 0x02, 0x00, 0x03, 0x00, 0x00, 0xC9];
        let frames = parse_superframe(&data).expect("frames");
        assert_eq!(frames, vec![(0, 8)]);
    }

    #[test]
    fn superframe_sizes_not_filling_is_invalid() {
        // Sizes 2 and 2, but only 3 bytes of frame data before
        // the index; the sizes sum to 4, not 3.
        let data = [0xAA, 0xBB, 0xCC, 0xC9, 0x02, 0x00, 0x02, 0x00, 0xC9];
        assert!(matches!(
            parse_superframe(&data),
            Err(Error::InvalidData { .. })
        ));
    }

    #[test]
    fn validate_subset_rejects_profile_one() {
        let header = Vp9FrameHeader {
            profile: 1,
            ..Vp9FrameHeader::default()
        };
        assert!(matches!(
            header.validate_subset(),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn validate_subset_rejects_non_420() {
        let color = Vp9ColorConfig {
            subsampling_x: false,
            ..Vp9ColorConfig::default()
        };
        let header = Vp9FrameHeader {
            color,
            ..Vp9FrameHeader::default()
        };
        assert!(matches!(
            header.validate_subset(),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn validate_subset_accepts_profile_zero_420() {
        let header = Vp9FrameHeader::default();
        assert!(header.validate_subset().is_ok());
    }

    #[test]
    fn color_space_mapping_matches_spec() {
        assert_eq!(Vp9ColorSpace::from_u32(0), Vp9ColorSpace::Unknown);
        assert_eq!(Vp9ColorSpace::from_u32(1), Vp9ColorSpace::Bt601);
        assert_eq!(Vp9ColorSpace::from_u32(5), Vp9ColorSpace::Bt2020);
        assert_eq!(Vp9ColorSpace::from_u32(7), Vp9ColorSpace::Rgb);
    }
}
