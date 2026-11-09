// Casts in this module are bounded by construction: motion-vector
// components are clamped to the frame plus a border before use,
// subpel filter taps are small signed constants, and block indices
// are range-checked. The pedantic cast lints therefore do not find
// real truncation risks here; the bounds are checked where the
// values are produced, not where they are cast.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]

//! VP9 prediction: intra prediction, inter prediction with motion
//! compensation, motion-vector decoding and prediction, and the
//! mode-probability contexts of spec section 9.3.2.
//!
//! VP9 Bitstream & Decoding Process Specification v0.6, sections
//! 6.4.23 (plane block sizes), 6.5 (motion-vector prediction),
//! 8.5.1 (intra prediction), 8.5.2 (inter prediction), and 9.3.2
//! (probability selection for mode syntax).
//!
//! The per-frame mode maps (`Vp9ModeMaps`) hold the block-level
//! state the tile decoder fills and the predictors and loop filter
//! read: skip flags, transform sizes, block sizes, modes, segment
//! ids, reference frames, interpolation filters, motion vectors,
//! and the above/left context arrays.

use crate::error::{Error, Result};
use crate::vp9_bitstream::Vp9BoolDecoder;
use crate::vp9_token::{
    clip1, clip3, read_tree, round2, Vp9Counts, Vp9FrameContext, BILINEAR, BLOCK_64X64, BLOCK_8X8,
    COUNTER_TO_CONTEXT, D117_PRED, D135_PRED, D153_PRED, D207_PRED, D45_PRED, D63_PRED, DC_PRED,
    EIGHTTAP, H_PRED, INTERP_EXTEND, INTRA_FRAME, LAST_FRAME, MAX_MV_REF_CANDIDATES, MAX_TXSIZE,
    MI_SIZE, MI_WIDTH_LOG2, MODE_2_COUNTER, MV_BORDER, MV_CLASS_TREE, MV_FR_TREE, MV_JOINT_HNZVNZ,
    MV_JOINT_HNZVZ, MV_JOINT_HZVNZ, MV_JOINT_TREE, MV_JOINT_ZERO, MV_REF_BLOCKS, NUM_8X8_HIGH,
    NUM_8X8_WIDE, REF_SCALE_SHIFT, SS_SIZE_420, SUBPEL_BITS, SUBPEL_MASK, TM_PRED, TX_4X4, V_PRED,
};

/// Number of reference frames (INTRA, LAST, GOLDEN, ALTREF).
pub(crate) const REF_FRAMES: usize = 4;
/// No reference frame.
pub(crate) const NONE_FRAME: i8 = -1;

/// Subpel interpolation filters, spec section 8.5.2.4.
/// Indexed [filter][fractional position 0..15][tap 0..7].
/// Filter 0 = EIGHTTAP, 1 = `EIGHTTAP_SMOOTH`, 2 = `EIGHTTAP_SHARP`,
/// 3 = BILINEAR.
pub(crate) const SUBPEL_FILTERS: [[[i16; 8]; 16]; 4] = [
    [
        [0, 0, 0, 128, 0, 0, 0, 0],
        [0, 1, -5, 126, 8, -3, 1, 0],
        [-1, 3, -10, 122, 18, -6, 2, 0],
        [-1, 4, -13, 118, 27, -9, 3, -1],
        [-1, 4, -16, 112, 37, -11, 4, -1],
        [-1, 5, -18, 105, 48, -14, 4, -1],
        [-1, 5, -19, 97, 58, -16, 5, -1],
        [-1, 6, -19, 88, 68, -18, 5, -1],
        [-1, 6, -19, 78, 78, -19, 6, -1],
        [-1, 5, -18, 68, 88, -19, 6, -1],
        [-1, 5, -16, 58, 97, -19, 5, -1],
        [-1, 4, -14, 48, 105, -18, 5, -1],
        [-1, 4, -11, 37, 112, -16, 4, -1],
        [-1, 3, -9, 27, 118, -13, 4, -1],
        [0, 2, -6, 18, 122, -10, 3, -1],
        [0, 1, -3, 8, 126, -5, 1, 0],
    ],
    [
        [0, 0, 0, 128, 0, 0, 0, 0],
        [-3, -1, 32, 64, 38, 1, -3, 0],
        [-2, -2, 29, 63, 41, 2, -3, 0],
        [-2, -2, 26, 63, 43, 4, -4, 0],
        [-2, -3, 24, 62, 46, 5, -4, 0],
        [-2, -3, 21, 60, 49, 7, -4, 0],
        [-1, -4, 18, 59, 51, 9, -4, 0],
        [-1, -4, 16, 57, 53, 12, -4, -1],
        [-1, -4, 14, 55, 55, 14, -4, -1],
        [-1, -4, 12, 53, 57, 16, -4, -1],
        [0, -4, 9, 51, 59, 18, -4, -1],
        [0, -4, 7, 49, 60, 21, -3, -2],
        [0, -4, 5, 46, 62, 24, -3, -2],
        [0, -4, 4, 43, 63, 26, -2, -2],
        [0, -3, 2, 41, 63, 29, -2, -2],
        [0, -3, 1, 38, 64, 32, -1, -3],
    ],
    [
        [0, 0, 0, 128, 0, 0, 0, 0],
        [-1, 3, -7, 127, 8, -3, 1, 0],
        [-2, 5, -13, 125, 17, -6, 3, -1],
        [-3, 7, -17, 121, 27, -10, 5, -2],
        [-4, 9, -20, 115, 37, -13, 6, -2],
        [-4, 10, -23, 108, 48, -16, 8, -3],
        [-4, 10, -24, 100, 59, -19, 9, -3],
        [-4, 11, -24, 90, 70, -21, 10, -4],
        [-4, 11, -23, 80, 80, -23, 11, -4],
        [-4, 10, -21, 70, 90, -24, 11, -4],
        [-3, 9, -19, 59, 100, -24, 10, -4],
        [-3, 8, -16, 48, 108, -23, 10, -4],
        [-2, 6, -13, 37, 115, -20, 9, -4],
        [-2, 5, -10, 27, 121, -17, 7, -3],
        [-1, 3, -6, 17, 125, -13, 5, -2],
        [0, 1, -3, 8, 127, -7, 3, -1],
    ],
    [
        [0, 0, 0, 128, 0, 0, 0, 0],
        [0, 0, 0, 120, 8, 0, 0, 0],
        [0, 0, 0, 112, 16, 0, 0, 0],
        [0, 0, 0, 104, 24, 0, 0, 0],
        [0, 0, 0, 96, 32, 0, 0, 0],
        [0, 0, 0, 88, 40, 0, 0, 0],
        [0, 0, 0, 80, 48, 0, 0, 0],
        [0, 0, 0, 72, 56, 0, 0, 0],
        [0, 0, 0, 64, 64, 0, 0, 0],
        [0, 0, 0, 56, 72, 0, 0, 0],
        [0, 0, 0, 48, 80, 0, 0, 0],
        [0, 0, 0, 40, 88, 0, 0, 0],
        [0, 0, 0, 32, 96, 0, 0, 0],
        [0, 0, 0, 24, 104, 0, 0, 0],
        [0, 0, 0, 16, 112, 0, 0, 0],
        [0, 0, 0, 8, 120, 0, 0, 0],
    ],
];

/// A decoded frame's sample planes, 8-bit 4:2:0 (profile 0).
/// `width`/`height` are the padded dimensions (the MI grid:
/// `mi_cols * 8` by `mi_rows * 8` for luma), because the spec
/// reconstructs the full padded frame and crops at output
/// (section 8.9). `vis_width`/`vis_height` are the visible frame
/// dimensions used for inter-prediction scaling and output.
/// Chroma planes are half the luma size in each dimension.
pub(crate) struct Vp9FrameBuffer {
    pub width: usize,
    pub height: usize,
    pub vis_width: usize,
    pub vis_height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Vp9FrameBuffer {
    /// Allocates a zeroed frame buffer for a frame of `mi_cols`
    /// by `mi_rows` 8x8 blocks with visible size
    /// `vis_width` by `vis_height`. Returns `Overflow` when the
    /// size would overflow.
    pub(crate) fn new(
        mi_cols: usize,
        mi_rows: usize,
        vis_width: usize,
        vis_height: usize,
    ) -> Result<Self> {
        let width = mi_cols
            .checked_mul(8)
            .ok_or(Error::Overflow("vp9 frame width overflow"))?;
        let height = mi_rows
            .checked_mul(8)
            .ok_or(Error::Overflow("vp9 frame height overflow"))?;
        let y_len = width
            .checked_mul(height)
            .ok_or(Error::Overflow("vp9 luma plane size overflow"))?;
        let uv_w = width / 2;
        let uv_h = height / 2;
        let uv_len = uv_w
            .checked_mul(uv_h)
            .ok_or(Error::Overflow("vp9 chroma plane size overflow"))?;
        Ok(Self {
            width,
            height,
            vis_width,
            vis_height,
            y: vec![0u8; y_len],
            u: vec![0u8; uv_len],
            v: vec![0u8; uv_len],
        })
    }

    /// Luma stride (samples per row).
    pub(crate) fn y_stride(&self) -> usize {
        self.width
    }

    /// Chroma stride (samples per row).
    pub(crate) fn uv_stride(&self) -> usize {
        self.width.div_ceil(2)
    }

    /// Chroma plane height.
    pub(crate) fn uv_height(&self) -> usize {
        self.height.div_ceil(2)
    }

    /// Mutable plane slice: 0 = Y, 1 = U, 2 = V.
    pub(crate) fn plane_mut(&mut self, plane: usize) -> &mut [u8] {
        match plane {
            0 => &mut self.y,
            1 => &mut self.u,
            _ => &mut self.v,
        }
    }

    /// Plane slice: 0 = Y, 1 = U, 2 = V.
    pub(crate) fn plane(&self, plane: usize) -> &[u8] {
        match plane {
            0 => &self.y,
            1 => &self.u,
            _ => &self.v,
        }
    }

    /// Plane stride: luma width, chroma half width.
    pub(crate) fn plane_stride(&self, plane: usize) -> usize {
        if plane == 0 {
            self.y_stride()
        } else {
            self.uv_stride()
        }
    }

    /// Plane width in samples.
    pub(crate) fn plane_width(&self, plane: usize) -> usize {
        if plane == 0 {
            self.width
        } else {
            self.uv_stride()
        }
    }

    /// Plane height in samples.
    pub(crate) fn plane_height(&self, plane: usize) -> usize {
        if plane == 0 {
            self.height
        } else {
            self.uv_height()
        }
    }
}

/// Per-frame mode maps: the block-level state the tile decoder
/// fills and the predictors and loop filter read. All 2D arrays
/// are indexed `[mi_row][mi_col]` in 8x8-block units.
pub(crate) struct Vp9ModeMaps {
    pub mi_rows: usize,
    pub mi_cols: usize,
    /// Skip flag per 8x8 block, spec section 6.4.5.
    pub skips: Vec<Vec<u8>>,
    /// Transform size per 8x8 block.
    pub tx_sizes: Vec<Vec<u8>>,
    /// Block size per 8x8 block.
    pub mi_sizes: Vec<Vec<u8>>,
    /// Luma prediction mode per 8x8 block.
    pub y_modes: Vec<Vec<u8>>,
    /// Segment id per 8x8 block.
    pub segment_ids: Vec<Vec<u8>>,
    /// Reference frame per 8x8 block and ref list [0]/[1].
    pub ref_frames: Vec<Vec<[i8; 2]>>,
    /// Interpolation filter per 8x8 block.
    pub interp_filters: Vec<Vec<u8>>,
    /// Motion vector per 8x8 block, ref list, [row, col].
    pub mvs: Vec<Vec<[[i32; 2]; 2]>>,
    /// Sub-block motion vectors per 8x8 block, ref list, sub-block.
    pub sub_mvs: Vec<Vec<[[[i32; 2]; 4]; 2]>>,
    /// Sub-block intra modes per 8x8 block (4 entries).
    pub sub_modes: Vec<Vec<[u8; 4]>>,
    /// Above segment-id prediction context, per mi column.
    pub above_seg_pred: Vec<u8>,
    /// Left segment-id prediction context, per mi row.
    pub left_seg_pred: Vec<u8>,
    /// Above partition context, per mi column.
    pub above_partition: Vec<u8>,
    /// Left partition context, per mi row.
    pub left_partition: Vec<u8>,
    /// Above nonzero context per plane, per 4x4 column.
    pub above_nonzero: [Vec<u8>; 3],
    /// Left nonzero context per plane, per 4x4 row.
    pub left_nonzero: [Vec<u8>; 3],
    /// Inter-mode context per reference frame, spec section 6.5.
    pub mode_context: [u8; REF_FRAMES],
    /// Previous frame's segment ids (for `use_prev_frame_mvs`).
    pub prev_segment_ids: Vec<Vec<u8>>,
    /// Previous frame's motion vectors.
    pub prev_mvs: Vec<Vec<[[i32; 2]; 2]>>,
    /// Previous frame's reference frames.
    pub prev_ref_frames: Vec<Vec<[i8; 2]>>,
}

impl Vp9ModeMaps {
    /// Allocates zeroed maps for a frame of `mi_rows` by `mi_cols`
    /// 8x8 blocks.
    pub(crate) fn new(mi_rows: usize, mi_cols: usize) -> Self {
        Self {
            mi_rows,
            mi_cols,
            skips: vec![vec![0u8; mi_cols]; mi_rows],
            tx_sizes: vec![vec![0u8; mi_cols]; mi_rows],
            mi_sizes: vec![vec![0u8; mi_cols]; mi_rows],
            y_modes: vec![vec![0u8; mi_cols]; mi_rows],
            segment_ids: vec![vec![0u8; mi_cols]; mi_rows],
            ref_frames: vec![vec![[INTRA_FRAME, NONE_FRAME]; mi_cols]; mi_rows],
            interp_filters: vec![vec![EIGHTTAP; mi_cols]; mi_rows],
            mvs: vec![vec![[[0i32; 2]; 2]; mi_cols]; mi_rows],
            sub_mvs: vec![vec![[[[0i32; 2]; 4]; 2]; mi_cols]; mi_rows],
            sub_modes: vec![vec![[0u8; 4]; mi_cols]; mi_rows],
            above_seg_pred: vec![0u8; mi_cols],
            left_seg_pred: vec![0u8; mi_rows],
            above_partition: vec![0u8; mi_cols],
            left_partition: vec![0u8; mi_rows],
            above_nonzero: [
                vec![0u8; 2 * mi_cols],
                vec![0u8; 2 * mi_cols],
                vec![0u8; 2 * mi_cols],
            ],
            left_nonzero: [
                vec![0u8; 2 * mi_rows],
                vec![0u8; 2 * mi_rows],
                vec![0u8; 2 * mi_rows],
            ],
            mode_context: [0u8; REF_FRAMES],
            prev_segment_ids: vec![vec![0u8; mi_cols]; mi_rows],
            prev_mvs: vec![vec![[[0i32; 2]; 2]; mi_cols]; mi_rows],
            prev_ref_frames: vec![vec![[INTRA_FRAME, NONE_FRAME]; mi_cols]; mi_rows],
        }
    }

    /// Clears the above contexts at the start of a frame,
    /// spec section 6.4.1.
    pub(crate) fn clear_above_context(&mut self) {
        self.above_partition.fill(0);
        self.above_seg_pred.fill(0);
        for plane in 0..3 {
            self.above_nonzero[plane].fill(0);
        }
    }

    /// Clears the left contexts at the start of a superblock row,
    /// spec section 6.4.2.
    pub(crate) fn clear_left_context(&mut self) {
        self.left_partition.fill(0);
        self.left_seg_pred.fill(0);
        for plane in 0..3 {
            self.left_nonzero[plane].fill(0);
        }
    }
}

/// Block size of a plane, spec section 6.4.23. Only 4:2:0 is
/// supported (profile 0), so `plane > 0` always uses the
/// `SS_SIZE_420` table.
pub(crate) fn get_plane_block_size(subsize: u8, plane: usize) -> u8 {
    if plane == 0 {
        subsize
    } else {
        SS_SIZE_420[subsize as usize]
    }
}

/// Transform size for a chroma plane, spec section 6.4.24.
pub(crate) fn get_uv_tx_size(mi_size: u8, tx_size: u8) -> u8 {
    if mi_size < BLOCK_8X8 {
        TX_4X4
    } else {
        let plane_size = get_plane_block_size(mi_size, 1);
        tx_size.min(MAX_TXSIZE[plane_size as usize])
    }
}

// ---------------------------------------------------------------------------
// Probability selection, spec section 9.3.2.
// ---------------------------------------------------------------------------

/// The above/left reference frames of a block, spec section 6.4.
/// `avail_u` is `MiRow > 0`; `avail_l` is `MiCol > MiColStart`
/// (tile-relative, not `MiCol > 0`).
pub(crate) struct Vp9Neighbors {
    pub avail_u: bool,
    pub avail_l: bool,
    /// Above reference frames [0]/[1], or INTRA/NONE if unavailable.
    pub above_ref: [i8; 2],
    /// Left reference frames [0]/[1], or INTRA/NONE if unavailable.
    pub left_ref: [i8; 2],
}

impl Vp9Neighbors {
    /// Reads the neighbors of the block at (`mi_row`, `mi_col`).
    pub(crate) fn at(
        maps: &Vp9ModeMaps,
        mi_row: usize,
        mi_col: usize,
        mi_col_start: usize,
    ) -> Self {
        let avail_u = mi_row > 0;
        let avail_l = mi_col > mi_col_start;
        let above_ref = if avail_u {
            maps.ref_frames[mi_row - 1][mi_col]
        } else {
            [INTRA_FRAME, NONE_FRAME]
        };
        let left_ref = if avail_l {
            maps.ref_frames[mi_row][mi_col - 1]
        } else {
            [INTRA_FRAME, NONE_FRAME]
        };
        Self {
            avail_u,
            avail_l,
            above_ref,
            left_ref,
        }
    }

    /// Whether the above block is intra.
    pub(crate) fn above_intra(&self) -> bool {
        self.above_ref[0] <= INTRA_FRAME
    }

    /// Whether the left block is intra.
    pub(crate) fn left_intra(&self) -> bool {
        self.left_ref[0] <= INTRA_FRAME
    }

    /// Whether the above block uses a single reference.
    pub(crate) fn above_single(&self) -> bool {
        self.above_ref[1] <= NONE_FRAME
    }

    /// Whether the left block uses a single reference.
    pub(crate) fn left_single(&self) -> bool {
        self.left_ref[1] <= NONE_FRAME
    }
}

/// Partition context, spec section 9.3.2.
pub(crate) fn partition_context(
    maps: &Vp9ModeMaps,
    mi_row: usize,
    mi_col: usize,
    bsize: u8,
    has_rows: bool,
    has_cols: bool,
) -> usize {
    let bsl = MI_WIDTH_LOG2[bsize as usize] as usize;
    let boffset = MI_WIDTH_LOG2[BLOCK_64X64 as usize] as usize - bsl;
    let num8x8 = NUM_8X8_WIDE[bsize as usize] as usize;
    let mut above = 0u8;
    let mut left = 0u8;
    for i in 0..num8x8 {
        // A block at the frame edge can run past the grid when
        // the frame size is not a multiple of 8; out-of-range
        // context entries read as 0 (spec section 9.3.2).
        if let Some(&v) = maps.above_partition.get(mi_col + i) {
            above |= v;
        }
        if let Some(&v) = maps.left_partition.get(mi_row + i) {
            left |= v;
        }
    }
    above = u8::from(above & (1u8 << boffset) > 0);
    left = u8::from(left & (1u8 << boffset) > 0);
    let _ = (has_rows, has_cols);
    bsl * 4 + (left as usize) * 2 + above as usize
}

/// Above/left intra modes for the `default_intra_mode` context,
/// spec section 9.3.2. `idx`/`idy` are the sub-block coordinates
/// within the current block (0 for a full block); `sub_modes` is
/// the current block's already-decoded sub-modes.
// The nine arguments are the spec's neighbor inputs (mode maps,
// block position/size, availability, sub-block position and the
// sub-mode grid); grouping them into a struct would churn the
// single caller for no decode benefit, so the count is kept.
#[allow(clippy::too_many_arguments)]
pub(crate) fn intra_mode_neighbors(
    maps: &Vp9ModeMaps,
    mi_row: usize,
    mi_col: usize,
    mi_size: u8,
    avail_u: bool,
    avail_l: bool,
    idx: usize,
    idy: usize,
    sub_modes: [u8; 4],
) -> (u8, u8) {
    if mi_size >= BLOCK_8X8 {
        let above = if avail_u {
            maps.sub_modes[mi_row - 1][mi_col][2]
        } else {
            DC_PRED
        };
        let left = if avail_l {
            maps.sub_modes[mi_row][mi_col - 1][1]
        } else {
            DC_PRED
        };
        (above, left)
    } else {
        let above = if idy > 0 {
            sub_modes[idx]
        } else if avail_u {
            maps.sub_modes[mi_row - 1][mi_col][2 + idx]
        } else {
            DC_PRED
        };
        let left = if idx > 0 {
            sub_modes[idy * 2]
        } else if avail_l {
            maps.sub_modes[mi_row][mi_col - 1][1 + idy * 2]
        } else {
            DC_PRED
        };
        (above, left)
    }
}

/// Skip context, spec section 9.3.2.
pub(crate) fn skip_context(
    maps: &Vp9ModeMaps,
    mi_row: usize,
    mi_col: usize,
    n: &Vp9Neighbors,
) -> usize {
    let mut ctx = 0usize;
    if n.avail_u {
        ctx += maps.skips[mi_row - 1][mi_col] as usize;
    }
    if n.avail_l {
        ctx += maps.skips[mi_row][mi_col - 1] as usize;
    }
    ctx
}

/// Segment-id prediction context, spec section 9.3.2.
pub(crate) fn seg_pred_context(maps: &Vp9ModeMaps, mi_row: usize, mi_col: usize) -> usize {
    (maps.left_seg_pred[mi_row] + maps.above_seg_pred[mi_col]) as usize
}

/// Is-inter context, spec section 9.3.2.
pub(crate) fn is_inter_context(n: &Vp9Neighbors) -> usize {
    if n.avail_u && n.avail_l {
        if n.left_intra() && n.above_intra() {
            3
        } else {
            usize::from(n.left_intra() || n.above_intra())
        }
    } else if n.avail_u || n.avail_l {
        2 * usize::from(if n.avail_u {
            n.above_intra()
        } else {
            n.left_intra()
        })
    } else {
        0
    }
}

/// Compound-mode context, spec section 9.3.2.
pub(crate) fn comp_mode_context(n: &Vp9Neighbors, comp_fixed_ref: i8) -> usize {
    if n.avail_u && n.avail_l {
        if n.above_single() && n.left_single() {
            usize::from((n.above_ref[0] == comp_fixed_ref) ^ (n.left_ref[0] == comp_fixed_ref))
        } else if n.above_single() {
            2 + usize::from(n.above_ref[0] == comp_fixed_ref || n.above_intra())
        } else if n.left_single() {
            2 + usize::from(n.left_ref[0] == comp_fixed_ref || n.left_intra())
        } else {
            4
        }
    } else if n.avail_u {
        if n.above_single() {
            usize::from(n.above_ref[0] == comp_fixed_ref)
        } else {
            3
        }
    } else if n.avail_l {
        if n.left_single() {
            usize::from(n.left_ref[0] == comp_fixed_ref)
        } else {
            3
        }
    } else {
        1
    }
}

/// Compound-reference context, spec section 9.3.2.
/// `sign_bias` is the header's `ref_frame_sign_bias`
/// (LAST/GOLDEN/ALTREF order); `comp_var_ref` is `[var0, var1]`.
pub(crate) fn comp_ref_context(
    n: &Vp9Neighbors,
    comp_fixed_ref: i8,
    comp_var_ref: [i8; 2],
    sign_bias: [bool; 3],
) -> usize {
    let fix_ref_idx = usize::from(sign_bias[(comp_fixed_ref - 1) as usize]);
    let var_ref_idx = 1 - fix_ref_idx;
    if n.avail_u && n.avail_l {
        if n.above_intra() && n.left_intra() {
            2
        } else if n.left_intra() {
            if n.above_single() {
                1 + 2 * usize::from(n.above_ref[0] != comp_var_ref[1])
            } else {
                1 + 2 * usize::from(n.above_ref[var_ref_idx] != comp_var_ref[1])
            }
        } else if n.above_intra() {
            if n.left_single() {
                1 + 2 * usize::from(n.left_ref[0] != comp_var_ref[1])
            } else {
                1 + 2 * usize::from(n.left_ref[var_ref_idx] != comp_var_ref[1])
            }
        } else {
            let above_var_ref = if n.above_single() {
                n.above_ref[0]
            } else {
                n.above_ref[var_ref_idx]
            };
            let left_var_ref = if n.left_single() {
                n.left_ref[0]
            } else {
                n.left_ref[var_ref_idx]
            };
            if above_var_ref == left_var_ref && comp_var_ref[1] == above_var_ref {
                0
            } else if n.left_single() && n.above_single() {
                if (above_var_ref == comp_fixed_ref && left_var_ref == comp_var_ref[0])
                    || (left_var_ref == comp_fixed_ref && above_var_ref == comp_var_ref[0])
                {
                    4
                } else if above_var_ref == left_var_ref {
                    3
                } else {
                    1
                }
            } else if n.left_single() || n.above_single() {
                let nonsingle_var_ref = if n.left_single() {
                    above_var_ref
                } else {
                    left_var_ref
                };
                let single_var_ref = if n.above_single() {
                    above_var_ref
                } else {
                    left_var_ref
                };
                if nonsingle_var_ref == comp_var_ref[1] && single_var_ref != comp_var_ref[1] {
                    1
                } else if single_var_ref == comp_var_ref[1] && nonsingle_var_ref != comp_var_ref[1]
                {
                    2
                } else {
                    4
                }
            } else if above_var_ref == left_var_ref {
                4
            } else {
                2
            }
        }
    } else if n.avail_u {
        if n.above_intra() {
            2
        } else if n.above_single() {
            3 * usize::from(n.above_ref[0] != comp_var_ref[1])
        } else {
            4 * usize::from(n.above_ref[var_ref_idx] != comp_var_ref[1])
        }
    } else if n.avail_l {
        if n.left_intra() {
            2
        } else if n.left_single() {
            3 * usize::from(n.left_ref[0] != comp_var_ref[1])
        } else {
            4 * usize::from(n.left_ref[var_ref_idx] != comp_var_ref[1])
        }
    } else {
        2
    }
}

/// Single-reference p1 context, spec section 9.3.2.
pub(crate) fn single_ref_p1_context(n: &Vp9Neighbors) -> usize {
    // LAST_FRAME is 1; the ref values are compared directly.
    if n.avail_u && n.avail_l {
        if n.above_intra() && n.left_intra() {
            2
        } else if n.left_intra() {
            if n.above_single() {
                4 * usize::from(n.above_ref[0] == 1)
            } else {
                1 + usize::from(n.above_ref[0] == 1 || n.above_ref[1] == 1)
            }
        } else if n.above_intra() {
            if n.left_single() {
                4 * usize::from(n.left_ref[0] == 1)
            } else {
                1 + usize::from(n.left_ref[0] == 1 || n.left_ref[1] == 1)
            }
        } else if n.above_single() && n.left_single() {
            2 * usize::from(n.above_ref[0] == 1) + 2 * usize::from(n.left_ref[0] == 1)
        } else if !n.above_single() && !n.left_single() {
            1 + usize::from(
                n.above_ref[0] == 1
                    || n.above_ref[1] == 1
                    || n.left_ref[0] == 1
                    || n.left_ref[1] == 1,
            )
        } else {
            let rfs = if n.above_single() {
                n.above_ref[0]
            } else {
                n.left_ref[0]
            };
            let crf1 = if n.above_single() {
                n.left_ref[0]
            } else {
                n.above_ref[0]
            };
            let crf2 = if n.above_single() {
                n.left_ref[1]
            } else {
                n.above_ref[1]
            };
            if rfs == 1 {
                3 + usize::from(crf1 == 1 || crf2 == 1)
            } else {
                usize::from(crf1 == 1 || crf2 == 1)
            }
        }
    } else if n.avail_u {
        if n.above_intra() {
            2
        } else if n.above_single() {
            4 * usize::from(n.above_ref[0] == 1)
        } else {
            1 + usize::from(n.above_ref[0] == 1 || n.above_ref[1] == 1)
        }
    } else if n.avail_l {
        if n.left_intra() {
            2
        } else if n.left_single() {
            4 * usize::from(n.left_ref[0] == 1)
        } else {
            1 + usize::from(n.left_ref[0] == 1 || n.left_ref[1] == 1)
        }
    } else {
        2
    }
}

/// Single-reference p2 context, spec section 9.3.2.
pub(crate) fn single_ref_p2_context(n: &Vp9Neighbors) -> usize {
    // LAST_FRAME = 1, GOLDEN_FRAME = 2, ALTREF_FRAME = 3.
    if n.avail_u && n.avail_l {
        if n.above_intra() && n.left_intra() {
            2
        } else if n.left_intra() {
            if n.above_single() {
                if n.above_ref[0] == 1 {
                    3
                } else {
                    4 * usize::from(n.above_ref[0] == 2)
                }
            } else {
                1 + 2 * usize::from(n.above_ref[0] == 2 || n.above_ref[1] == 2)
            }
        } else if n.above_intra() {
            if n.left_single() {
                if n.left_ref[0] == 1 {
                    3
                } else {
                    4 * usize::from(n.left_ref[0] == 2)
                }
            } else {
                1 + 2 * usize::from(n.left_ref[0] == 2 || n.left_ref[1] == 2)
            }
        } else if n.above_single() && n.left_single() {
            if n.above_ref[0] == 1 && n.left_ref[0] == 1 {
                3
            } else if n.above_ref[0] == 1 {
                4 * usize::from(n.left_ref[0] == 2)
            } else if n.left_ref[0] == 1 {
                4 * usize::from(n.above_ref[0] == 2)
            } else {
                2 * usize::from(n.above_ref[0] == 2) + 2 * usize::from(n.left_ref[0] == 2)
            }
        } else if !n.above_single() && !n.left_single() {
            if n.above_ref[0] == n.left_ref[0] && n.above_ref[1] == n.left_ref[1] {
                3 * usize::from(n.above_ref[0] == 2 || n.above_ref[1] == 2)
            } else {
                2
            }
        } else {
            let rfs = if n.above_single() {
                n.above_ref[0]
            } else {
                n.left_ref[0]
            };
            let crf1 = if n.above_single() {
                n.left_ref[0]
            } else {
                n.above_ref[0]
            };
            let crf2 = if n.above_single() {
                n.left_ref[1]
            } else {
                n.above_ref[1]
            };
            if rfs == 2 {
                3 + usize::from(crf1 == 2 || crf2 == 2)
            } else if rfs == 3 {
                usize::from(crf1 == 2 || crf2 == 2)
            } else {
                1 + 2 * usize::from(crf1 == 2 || crf2 == 2)
            }
        }
    } else if n.avail_u {
        if n.above_intra() || (n.above_ref[0] == 1 && n.above_single()) {
            2
        } else if n.above_single() {
            4 * usize::from(n.above_ref[0] == 2)
        } else {
            3 * usize::from(n.above_ref[0] == 2 || n.above_ref[1] == 2)
        }
    } else if n.avail_l {
        if n.left_intra() || (n.left_ref[0] == 1 && n.left_single()) {
            2
        } else if n.left_single() {
            4 * usize::from(n.left_ref[0] == 2)
        } else {
            3 * usize::from(n.left_ref[0] == 2 || n.left_ref[1] == 2)
        }
    } else {
        2
    }
}

/// Transform-size context, spec section 9.3.2.
pub(crate) fn tx_size_context(
    maps: &Vp9ModeMaps,
    mi_row: usize,
    mi_col: usize,
    n: &Vp9Neighbors,
    max_tx_size: u8,
) -> usize {
    let mut above = max_tx_size;
    let mut left = max_tx_size;
    if n.avail_u && maps.skips[mi_row - 1][mi_col] == 0 {
        above = maps.tx_sizes[mi_row - 1][mi_col];
    }
    if n.avail_l && maps.skips[mi_row][mi_col - 1] == 0 {
        left = maps.tx_sizes[mi_row][mi_col - 1];
    }
    if !n.avail_l {
        left = above;
    }
    if !n.avail_u {
        above = left;
    }
    usize::from(above + left > max_tx_size)
}

/// Interpolation-filter context, spec section 9.3.2.
pub(crate) fn interp_filter_context(
    maps: &Vp9ModeMaps,
    mi_row: usize,
    mi_col: usize,
    n: &Vp9Neighbors,
) -> usize {
    let left_interp = if n.avail_l && n.left_ref[0] > INTRA_FRAME {
        maps.interp_filters[mi_row][mi_col - 1] as usize
    } else {
        3
    };
    let above_interp = if n.avail_u && n.above_ref[0] > INTRA_FRAME {
        maps.interp_filters[mi_row - 1][mi_col] as usize
    } else {
        3
    };
    if left_interp == above_interp {
        left_interp
    } else if left_interp == 3 && above_interp != 3 {
        above_interp
    } else if left_interp != 3 && above_interp == 3 {
        left_interp
    } else {
        3
    }
}

// ---------------------------------------------------------------------------
// Motion-vector prediction, spec section 6.5.
// ---------------------------------------------------------------------------

/// Motion-vector candidates for one reference frame, spec 6.5.1.
pub(crate) struct Vp9MvCandidates {
    /// `RefListMv`[0..2] after clamping.
    pub list: [[i32; 2]; 2],
    /// Inter-mode context for this reference frame.
    pub mode_context: u8,
}

/// Whether a motion vector is small enough to use high precision,
/// spec section 6.5.13.
pub(crate) fn use_mv_hp(mv: [i32; 2]) -> bool {
    (mv[0].abs() >> 3) < 8 && (mv[1].abs() >> 3) < 8
}

/// Clamps a motion-vector row to the frame plus a border,
/// spec section 6.5.4.
fn clamp_mv_row(mvec: i32, border: i32, mi_row: usize, mi_size: u8, mi_rows: usize) -> i32 {
    let bh = i32::from(NUM_8X8_HIGH[mi_size as usize]);
    let mb_to_top = -((mi_row as i32 * 8) * 8);
    let mb_to_bottom = ((mi_rows as i32 - bh - mi_row as i32) * 8) * 8;
    clip3(
        i64::from(mvec),
        i64::from(mb_to_top - border),
        i64::from(mb_to_bottom + border),
    ) as i32
}

/// Clamps a motion-vector column to the frame plus a border,
/// spec section 6.5.5.
fn clamp_mv_col(mvec: i32, border: i32, mi_col: usize, mi_size: u8, mi_cols: usize) -> i32 {
    let bw = i32::from(NUM_8X8_WIDE[mi_size as usize]);
    let mb_to_left = -((mi_col as i32 * 8) * 8);
    let mb_to_right = ((mi_cols as i32 - bw - mi_col as i32) * 8) * 8;
    clip3(
        i64::from(mvec),
        i64::from(mb_to_left - border),
        i64::from(mb_to_right + border),
    ) as i32
}

/// Motion-vector reference search state, spec section 6.5.1.
/// Carries the candidate lists while `find_mv_refs` runs.
struct MvRefSearch<'a> {
    maps: &'a Vp9ModeMaps,
    mi_row: usize,
    mi_col: usize,
    mi_size: u8,
    ref_frame: i8,
    sign_bias: [bool; 3],
    mi_col_start: usize,
    mi_col_end: usize,
    ref_list_mv: [[i32; 2]; 2],
    ref_mv_count: usize,
    candidate_mv: [[i32; 2]; 2],
    candidate_frame: [i8; 2],
}

impl MvRefSearch<'_> {
    /// Whether a candidate position is accessible, spec 6.5.2.
    /// Moving across the top and bottom tile edges is allowed,
    /// but moving across the left and right tile edges is not.
    fn is_inside(&self, r: i32, c: i32) -> bool {
        r >= 0
            && (r as usize) < self.maps.mi_rows
            && c >= self.mi_col_start as i32
            && (c as usize) < self.mi_col_end
    }

    /// Clamps one candidate, spec section 6.5.3.
    fn clamp_mv_ref(&mut self, i: usize) {
        self.ref_list_mv[i][0] = clamp_mv_row(
            self.ref_list_mv[i][0],
            MV_BORDER,
            self.mi_row,
            self.mi_size,
            self.maps.mi_rows,
        );
        self.ref_list_mv[i][1] = clamp_mv_col(
            self.ref_list_mv[i][1],
            MV_BORDER,
            self.mi_col,
            self.mi_size,
            self.maps.mi_cols,
        );
    }

    /// Adds a candidate motion vector, spec section 6.5.6.
    fn add_mv_ref_list(&mut self, ref_list: usize) {
        if self.ref_mv_count >= MAX_MV_REF_CANDIDATES {
            return;
        }
        if self.ref_mv_count > 0 && self.candidate_mv[ref_list] == self.ref_list_mv[0] {
            return;
        }
        self.ref_list_mv[self.ref_mv_count] = self.candidate_mv[ref_list];
        self.ref_mv_count += 1;
    }

    /// Reads a candidate's motion vector and reference frame,
    /// spec section 6.5.10.
    fn get_block_mv(&mut self, r: usize, c: usize, ref_list: usize, use_prev: bool) {
        if use_prev {
            self.candidate_mv[ref_list] = self.maps.prev_mvs[r][c][ref_list];
            self.candidate_frame[ref_list] = self.maps.prev_ref_frames[r][c][ref_list];
        } else {
            self.candidate_mv[ref_list] = self.maps.mvs[r][c][ref_list];
            self.candidate_frame[ref_list] = self.maps.ref_frames[r][c][ref_list];
        }
    }

    /// Reads a candidate's sub-block motion vector,
    /// spec section 6.5.11.
    fn get_sub_block_mv(&mut self, r: usize, c: usize, ref_list: usize, delta_col: i8, block: i32) {
        // idx_n_column_to_subblock[block][deltaCol == 0].
        const IDX_N_COLUMN_TO_SUBBLOCK: [[usize; 2]; 4] = [[1, 2], [1, 3], [3, 2], [3, 3]];
        let idx = if block >= 0 {
            IDX_N_COLUMN_TO_SUBBLOCK[block as usize][usize::from(delta_col == 0)]
        } else {
            3
        };
        self.candidate_mv[ref_list] = self.maps.sub_mvs[r][c][ref_list][idx];
    }

    /// Adds a candidate when its reference matches,
    /// spec section 6.5.7.
    fn if_same_ref_frame_add_mv(&mut self, r: usize, c: usize, use_prev: bool) {
        for j in 0..2 {
            self.get_block_mv(r, c, j, use_prev);
            if self.candidate_frame[j] == self.ref_frame {
                self.add_mv_ref_list(j);
                return;
            }
        }
    }

    /// Scales a candidate motion vector by the sign bias,
    /// spec section 6.5.9.
    fn scale_mv(&mut self, ref_list: usize) {
        let cand_frame = self.candidate_frame[ref_list];
        // sign_bias is indexed by ref frame (LAST=1..ALTREF=3);
        // INTRA (0) is never a candidate reference here.
        if self.sign_bias[(cand_frame - 1) as usize]
            != self.sign_bias[(self.ref_frame - 1) as usize]
        {
            self.candidate_mv[ref_list][0] *= -1;
            self.candidate_mv[ref_list][1] *= -1;
        }
    }

    /// Adds a candidate whose reference differs, spec 6.5.8.
    fn if_diff_ref_frame_add_mv(&mut self, r: usize, c: usize, use_prev: bool) {
        for j in 0..2 {
            self.get_block_mv(r, c, j, use_prev);
        }
        let mvs_same = self.candidate_mv[0] == self.candidate_mv[1];
        if self.candidate_frame[0] > INTRA_FRAME && self.candidate_frame[0] != self.ref_frame {
            self.scale_mv(0);
            self.add_mv_ref_list(0);
        }
        if self.candidate_frame[1] > INTRA_FRAME
            && self.candidate_frame[1] != self.ref_frame
            && !mvs_same
        {
            self.scale_mv(1);
            self.add_mv_ref_list(1);
        }
    }

    /// Runs the search, spec section 6.5.1. Returns the
    /// inter-mode context for this reference frame.
    fn run(&mut self, block: i32, use_prev: bool) -> u8 {
        let mut different_ref_found = false;
        let mut context_counter = 0u8;
        let search = MV_REF_BLOCKS[self.mi_size as usize];
        for offset in search.iter().take(2) {
            let candidate_r = self.mi_row as i32 + i32::from(offset[0]);
            let candidate_c = self.mi_col as i32 + i32::from(offset[1]);
            if self.is_inside(candidate_r, candidate_c) {
                different_ref_found = true;
                let r = candidate_r as usize;
                let c = candidate_c as usize;
                context_counter += MODE_2_COUNTER[self.maps.y_modes[r][c] as usize];
                for j in 0..2 {
                    if self.maps.ref_frames[r][c][j] == self.ref_frame {
                        self.get_sub_block_mv(r, c, j, offset[1], block);
                        self.add_mv_ref_list(j);
                        break;
                    }
                }
            }
        }
        for offset in search.iter().skip(2) {
            let candidate_r = self.mi_row as i32 + i32::from(offset[0]);
            let candidate_c = self.mi_col as i32 + i32::from(offset[1]);
            if self.is_inside(candidate_r, candidate_c) {
                different_ref_found = true;
                self.if_same_ref_frame_add_mv(candidate_r as usize, candidate_c as usize, false);
            }
        }
        if use_prev {
            self.if_same_ref_frame_add_mv(self.mi_row, self.mi_col, true);
        }
        if different_ref_found {
            for offset in &search {
                let candidate_r = self.mi_row as i32 + i32::from(offset[0]);
                let candidate_c = self.mi_col as i32 + i32::from(offset[1]);
                if self.is_inside(candidate_r, candidate_c) {
                    self.if_diff_ref_frame_add_mv(
                        candidate_r as usize,
                        candidate_c as usize,
                        false,
                    );
                }
            }
        }
        if use_prev {
            self.if_diff_ref_frame_add_mv(self.mi_row, self.mi_col, true);
        }
        for i in 0..2 {
            self.clamp_mv_ref(i);
        }
        COUNTER_TO_CONTEXT[context_counter as usize]
    }
}

/// Finds the motion-vector candidates for `ref_frame` at the block
/// (`mi_row`, `mi_col`) of size `mi_size`, spec section 6.5.1.
/// `block` is the sub-block index (-1 for a whole block);
/// `use_prev` is `UsePrevFrameMvs`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn find_mv_refs(
    maps: &Vp9ModeMaps,
    mi_row: usize,
    mi_col: usize,
    mi_size: u8,
    ref_frame: i8,
    block: i32,
    use_prev: bool,
    sign_bias: [bool; 3],
    mi_col_start: usize,
    mi_col_end: usize,
) -> Vp9MvCandidates {
    let mut search = MvRefSearch {
        maps,
        mi_row,
        mi_col,
        mi_size,
        ref_frame,
        sign_bias,
        mi_col_start,
        mi_col_end,
        ref_list_mv: [[0i32; 2]; 2],
        ref_mv_count: 0,
        candidate_mv: [[0i32; 2]; 2],
        candidate_frame: [INTRA_FRAME; 2],
    };
    let mode_context = search.run(block, use_prev);
    Vp9MvCandidates {
        list: search.ref_list_mv,
        mode_context,
    }
}

/// Computes Nearest/Near/Best MVs from the candidate list,
/// spec section 6.5.12. Returns (nearest, near, best).
pub(crate) fn find_best_ref_mvs(
    candidates: [[i32; 2]; 2],
    allow_hp: bool,
    mi_row: usize,
    mi_col: usize,
    mi_size: u8,
    mi_rows: usize,
    mi_cols: usize,
) -> ([i32; 2], [i32; 2], [i32; 2]) {
    let mut list = candidates;
    // (BORDERINPIXELS - INTERP_EXTEND) << 3 = (16 - 3) << 3 = 104.
    for delta in &mut list {
        let mut delta_row = delta[0];
        let mut delta_col = delta[1];
        if !allow_hp || !use_mv_hp(*delta) {
            if delta_row & 1 != 0 {
                delta_row += if delta_row > 0 { -1 } else { 1 };
            }
            if delta_col & 1 != 0 {
                delta_col += if delta_col > 0 { -1 } else { 1 };
            }
        }
        delta[0] = clamp_mv_row(delta_row, 104, mi_row, mi_size, mi_rows);
        delta[1] = clamp_mv_col(delta_col, 104, mi_col, mi_size, mi_cols);
    }
    (list[0], list[1], list[0])
}

/// Appends sub-8x8 motion vectors, spec section 6.5.14.
/// Returns (nearest, near) for the ref list. `block_mvs` is the
/// current block's sub-block MVs for this ref list.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_sub8x8_mvs(
    maps: &Vp9ModeMaps,
    mi_row: usize,
    mi_col: usize,
    mi_size: u8,
    block: i32,
    ref_frame: i8,
    block_mvs: [[i32; 2]; 4],
    use_prev: bool,
    sign_bias: [bool; 3],
    mi_col_start: usize,
    mi_col_end: usize,
) -> ([i32; 2], [i32; 2]) {
    let candidates = find_mv_refs(
        maps,
        mi_row,
        mi_col,
        mi_size,
        ref_frame,
        block,
        use_prev,
        sign_bias,
        mi_col_start,
        mi_col_end,
    );
    let mut sub8x8: [[i32; 2]; 2] = [[0i32; 2]; 2];
    let mut dst = 0usize;
    if block == 0 {
        for i in 0..2 {
            sub8x8[dst] = candidates.list[i];
            dst += 1;
        }
    } else if block <= 2 {
        sub8x8[dst] = block_mvs[0];
        dst += 1;
    } else {
        sub8x8[dst] = block_mvs[2];
        dst += 1;
    }
    let mut idx = 1i32;
    while idx >= 0 && dst < 2 {
        if block_mvs[idx as usize] != sub8x8[0] {
            sub8x8[dst] = block_mvs[idx as usize];
            dst += 1;
        }
        idx -= 1;
    }
    for n in 0..2 {
        if dst >= 2 {
            break;
        }
        if candidates.list[n] != sub8x8[0] {
            sub8x8[dst] = candidates.list[n];
            dst += 1;
        }
    }
    if dst < 2 {
        sub8x8[dst] = [0i32; 2];
    }
    (sub8x8[0], sub8x8[1])
}

// ---------------------------------------------------------------------------
// Motion-vector decoding, spec section 6.5.2 and 7.4.17.
// ---------------------------------------------------------------------------

/// Reads one motion-vector component, spec section 6.5.2.
/// `comp` is 0 (row) or 1 (col). `use_hp` selects whether the
/// third fractional bit is read (1 when not read).
fn read_mv_component(
    dec: &mut Vp9BoolDecoder,
    ctx: &Vp9FrameContext,
    counts: &mut Vp9Counts,
    comp: usize,
    use_hp: bool,
) -> Result<i32> {
    // mv_sign: 0 = positive, 1 = negative.
    let sign = dec.read_bool(u32::from(ctx.mv_sign_prob[comp]))?;
    counts.mv_sign[comp][usize::from(sign)] += 1;
    // mv_class: 0..10.
    let class = read_tree(dec, &MV_CLASS_TREE, &ctx.mv_class_probs[comp])?;
    counts.mv_class[comp][class as usize] += 1;
    let class = class as usize;
    let mag: i32 = if class == 0 {
        // mv_class0_bit, mv_class0_fr, mv_class0_hp.
        let bit = dec.read_bool(u32::from(ctx.mv_class0_bit_prob[comp]))?;
        counts.mv_class0_bit[comp][usize::from(bit)] += 1;
        let bit = usize::from(bit);
        let fr = read_tree(dec, &MV_FR_TREE, &ctx.mv_class0_fr_probs[comp][bit])?;
        counts.mv_class0_fr[comp][bit][fr as usize] += 1;
        let hp = if use_hp {
            let hp = dec.read_bool(u32::from(ctx.mv_class0_hp_prob[comp]))?;
            counts.mv_class0_hp[comp][usize::from(hp)] += 1;
            i32::from(hp)
        } else {
            1
        };
        ((bit as i32) << 3) | (i32::from(fr) << 1) | hp
    } else {
        // mv_bit for each bit of the integer part, then mv_fr, mv_hp.
        let mut d = 0i32;
        for i in 0..class {
            let bit = dec.read_bool(u32::from(ctx.mv_bits_prob[comp][i]))?;
            counts.mv_bits[comp][i][usize::from(bit)] += 1;
            d |= i32::from(bit) << i;
        }
        let fr = read_tree(dec, &MV_FR_TREE, &ctx.mv_fr_probs[comp])?;
        counts.mv_fr[comp][fr as usize] += 1;
        let hp = if use_hp {
            let hp = dec.read_bool(u32::from(ctx.mv_hp_prob[comp]))?;
            counts.mv_hp[comp][usize::from(hp)] += 1;
            i32::from(hp)
        } else {
            1
        };
        // CLASS0_SIZE = 2.
        (2 << (class + 2)) + ((d << 3) | (i32::from(fr) << 1) | hp)
    };
    let mag = mag + 1;
    Ok(if sign { -mag } else { mag })
}

/// Reads a motion vector, spec section 6.5.2.
/// `best` is `BestMv`[ref]; `allow_hp` is
/// `allow_high_precision_mv`.
pub(crate) fn read_mv(
    dec: &mut Vp9BoolDecoder,
    ctx: &Vp9FrameContext,
    counts: &mut Vp9Counts,
    best: [i32; 2],
    allow_hp: bool,
) -> Result<[i32; 2]> {
    let use_hp = allow_hp && use_mv_hp(best);
    // mv_joint: 0 = ZERO, 1 = HNZVZ (col only), 2 = HZVNZ (row
    // only), 3 = HNZVNZ (both).
    let joint = read_tree(dec, &MV_JOINT_TREE, &ctx.mv_joint_probs)?;
    counts.mv_joint[joint as usize] += 1;
    // Joint ZERO means neither component is present.
    if joint == MV_JOINT_ZERO as i8 {
        return Ok(best);
    }
    let mut diff = [0i32; 2];
    // The joint constants are u8 spec values; the tree decoder returns
    // i8, and 0..=3 fits in i8, so the casts are lossless.
    if joint == MV_JOINT_HZVNZ as i8 || joint == MV_JOINT_HNZVNZ as i8 {
        diff[0] = read_mv_component(dec, ctx, counts, 0, use_hp)?;
    }
    if joint == MV_JOINT_HNZVZ as i8 || joint == MV_JOINT_HNZVNZ as i8 {
        diff[1] = read_mv_component(dec, ctx, counts, 1, use_hp)?;
    }
    Ok([best[0] + diff[0], best[1] + diff[1]])
}

// ---------------------------------------------------------------------------
// Intra prediction, spec section 8.5.1.
// ---------------------------------------------------------------------------

/// Predicts a transform block with an intra mode, spec 8.5.1.
/// `plane_data` is the plane being reconstructed (luma or chroma);
/// the prediction is written into the `size` by `size` block at
/// (`x`, `y`). `max_x`/`max_y` are the plane's last valid
/// coordinates (padded frame size - 1). `mode` is the intra mode
/// (already resolved: `uv_mode` for chroma, `y_mode`/`sub_modes`
/// for luma). `have_left`/`have_above` say whether the neighboring
/// samples exist; `not_on_right` says the block is not on the
/// right edge of the frame (needed for the 4x4 above-row
/// extension).
// The function is one `match` over the ten intra modes plus the
// DC fallback, exactly as the spec lays the prediction out
// (section 8.5.1); splitting it per mode would scatter the shared
// `above`/`left` setup and risk decode changes, and the eleven
// arguments are the spec's prediction inputs, so both counts stay.
#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_intra(
    plane_data: &mut [u8],
    stride: usize,
    max_x: usize,
    max_y: usize,
    x: usize,
    y: usize,
    have_left: bool,
    have_above: bool,
    not_on_right: bool,
    tx_size: u8,
    mode: u8,
) {
    let log2_size = tx_size as usize + 2;
    let size = 1usize << log2_size;
    // above_row[-1..2*size-1] stored with a +1 offset; the
    // default for a missing above row is 127, for a missing left
    // column 129 (spec section 8.5.1).
    let mut above_row = [127i64; 65];
    let mut left_col = [129i64; 32];
    if have_above {
        for (i, slot) in above_row[1..=size].iter_mut().enumerate() {
            *slot = i64::from(plane_data[(y - 1) * stride + (x + i).min(max_x)]);
        }
        // The extension copies the last above sample; read it
        // before taking the mutable slice below.
        let above_last = above_row[1 + size - 1];
        for (i, slot) in above_row[(1 + size)..=(2 * size)].iter_mut().enumerate() {
            let i = i + size;
            *slot = if not_on_right && tx_size == TX_4X4 {
                i64::from(plane_data[(y - 1) * stride + (x + i).min(max_x)])
            } else {
                above_last
            };
        }
        above_row[0] = if have_left {
            i64::from(plane_data[(y - 1) * stride + (x - 1).min(max_x)])
        } else {
            129
        };
    } else {
        // above_row stays 127 (including index -1).
        let above_last = above_row[1 + size - 1];
        for slot in &mut above_row[(1 + size)..=(2 * size)] {
            *slot = above_last;
        }
    }
    if have_left {
        for (i, slot) in left_col[..size].iter_mut().enumerate() {
            *slot = i64::from(plane_data[(y + i).min(max_y) * stride + (x - 1)]);
        }
    }
    // Helper to index above_row with the spec's -1..2*size-1 range.
    let above = |i: i64| -> i64 { above_row[(i + 1) as usize] };
    let mut pred = [[0i64; 32]; 32];
    match mode {
        V_PRED => {
            // V_PRED
            for row in pred.iter_mut().take(size) {
                for (j, slot) in row.iter_mut().enumerate().take(size) {
                    *slot = above(j as i64);
                }
            }
        }
        H_PRED => {
            // H_PRED
            for (i, row) in pred.iter_mut().enumerate().take(size) {
                for slot in row.iter_mut().take(size) {
                    *slot = left_col[i];
                }
            }
        }
        D207_PRED => {
            // D207_PRED
            for slot in pred[size - 1].iter_mut().take(size) {
                *slot = left_col[size - 1];
            }
            for (i, row) in pred.iter_mut().enumerate().take(size - 1) {
                row[0] = round2(left_col[i] + left_col[i + 1], 1);
            }
            for (i, row) in pred.iter_mut().enumerate().take(size.saturating_sub(2)) {
                row[1] = round2(left_col[i] + 2 * left_col[i + 1] + left_col[i + 2], 2);
            }
            if size >= 2 {
                pred[size - 2][1] = round2(left_col[size - 2] + 3 * left_col[size - 1], 2);
            }
            // Runs bottom-up: each row copies the row below shifted
            // by two columns, so the split keeps the source row
            // borrowed while the destination row is written.
            for i in (0..size - 1).rev() {
                let (head, tail) = pred.split_at_mut(i + 1);
                let row = &mut head[i];
                let next = &tail[0];
                for (j, slot) in row.iter_mut().enumerate().take(size).skip(2) {
                    *slot = next[j - 2];
                }
            }
        }
        D45_PRED => {
            // D45_PRED
            for (i, row) in pred.iter_mut().enumerate().take(size) {
                for (j, slot) in row.iter_mut().enumerate().take(size) {
                    let s = i + j;
                    *slot = if s + 2 < 2 * size {
                        round2(
                            above(s as i64) + 2 * above(s as i64 + 1) + above(s as i64 + 2),
                            2,
                        )
                    } else {
                        above(2 * size as i64 - 1)
                    };
                }
            }
        }
        D63_PRED => {
            // D63_PRED
            for (i, row) in pred.iter_mut().enumerate().take(size) {
                for (j, slot) in row.iter_mut().enumerate().take(size) {
                    let k = i / 2 + j;
                    *slot = if i & 1 == 1 {
                        round2(
                            above(k as i64) + 2 * above(k as i64 + 1) + above(k as i64 + 2),
                            2,
                        )
                    } else {
                        round2(above(k as i64) + above(k as i64 + 1), 1)
                    };
                }
            }
        }
        D117_PRED => {
            // D117_PRED
            for (j, slot) in pred[0].iter_mut().enumerate().take(size) {
                *slot = round2(above(j as i64 - 1) + above(j as i64), 1);
            }
            pred[1][0] = round2(left_col[0] + 2 * above(-1) + above(0), 2);
            for (j, slot) in pred[1].iter_mut().enumerate().take(size).skip(1) {
                *slot = round2(
                    above(j as i64 - 2) + 2 * above(j as i64 - 1) + above(j as i64),
                    2,
                );
            }
            if size >= 3 {
                pred[2][0] = round2(above(-1) + 2 * left_col[0] + left_col[1], 2);
            }
            for (i, row) in pred.iter_mut().enumerate().take(size).skip(3) {
                row[0] = round2(left_col[i - 3] + 2 * left_col[i - 2] + left_col[i - 1], 2);
            }
            // Each row copies the row two above shifted by one
            // column; the split keeps the source row borrowed
            // while the destination row is written.
            for i in 2..size {
                let (head, tail) = pred.split_at_mut(i);
                let row = &mut tail[0];
                let src = &head[i - 2];
                for (j, slot) in row.iter_mut().enumerate().take(size).skip(1) {
                    *slot = src[j - 1];
                }
            }
        }
        D135_PRED => {
            // D135_PRED
            pred[0][0] = round2(left_col[0] + 2 * above(-1) + above(0), 2);
            for (j, slot) in pred[0].iter_mut().enumerate().take(size).skip(1) {
                *slot = round2(
                    above(j as i64 - 2) + 2 * above(j as i64 - 1) + above(j as i64),
                    2,
                );
            }
            if size >= 2 {
                pred[1][0] = round2(above(-1) + 2 * left_col[0] + left_col[1], 2);
            }
            for (i, row) in pred.iter_mut().enumerate().take(size).skip(2) {
                row[0] = round2(left_col[i - 2] + 2 * left_col[i - 1] + left_col[i], 2);
            }
            // Each row copies the row above shifted by one column;
            // the split keeps the source row borrowed while the
            // destination row is written.
            for i in 1..size {
                let (head, tail) = pred.split_at_mut(i);
                let row = &mut tail[0];
                let src = &head[i - 1];
                for (j, slot) in row.iter_mut().enumerate().take(size).skip(1) {
                    *slot = src[j - 1];
                }
            }
        }
        D153_PRED => {
            // D153_PRED
            pred[0][0] = round2(left_col[0] + above(-1), 1);
            for (i, row) in pred.iter_mut().enumerate().take(size).skip(1) {
                row[0] = round2(left_col[i - 1] + left_col[i], 1);
            }
            pred[0][1] = round2(left_col[0] + 2 * above(-1) + above(0), 2);
            if size >= 2 {
                pred[1][1] = round2(above(-1) + 2 * left_col[0] + left_col[1], 2);
            }
            for (i, row) in pred.iter_mut().enumerate().take(size).skip(2) {
                row[1] = round2(left_col[i - 2] + 2 * left_col[i - 1] + left_col[i], 2);
            }
            for (j, slot) in pred[0].iter_mut().enumerate().take(size).skip(2) {
                *slot = round2(
                    above(j as i64 - 3) + 2 * above(j as i64 - 2) + above(j as i64 - 1),
                    2,
                );
            }
            // Each row copies the row above shifted by two columns;
            // the split keeps the source row borrowed while the
            // destination row is written.
            for i in 1..size {
                let (head, tail) = pred.split_at_mut(i);
                let row = &mut tail[0];
                let src = &head[i - 1];
                for (j, slot) in row.iter_mut().enumerate().take(size).skip(2) {
                    *slot = src[j - 2];
                }
            }
        }
        TM_PRED => {
            // TM_PRED
            for (i, row) in pred.iter_mut().enumerate().take(size) {
                for (j, slot) in row.iter_mut().enumerate().take(size) {
                    *slot = i64::from(clip1(above(j as i64) + left_col[i] - above(-1)));
                }
            }
        }
        _ => {
            // DC_PRED (mode 0 and any fallback)
            let dc = if have_left && have_above {
                let mut sum = 0i64;
                for (k, &value) in left_col.iter().enumerate().take(size) {
                    sum += value + above(k as i64);
                }
                (sum + size as i64) >> (log2_size + 1)
            } else if have_left {
                let mut sum = 0i64;
                for &value in left_col.iter().take(size) {
                    sum += value;
                }
                (sum + (1i64 << (log2_size - 1))) >> log2_size
            } else if have_above {
                let mut sum = 0i64;
                for k in 0..size {
                    sum += above(k as i64);
                }
                (sum + (1i64 << (log2_size - 1))) >> log2_size
            } else {
                128
            };
            for row in pred.iter_mut().take(size) {
                for slot in row.iter_mut().take(size) {
                    *slot = dc;
                }
            }
        }
    }
    // Write the prediction into the plane. A TX block at the
    // frame edge can extend past the padded plane (the MI grid is
    // rounded up to a superblock, so a block starting inside the
    // frame can overhang it); samples past the plane are never
    // output (section 8.9 crops to the visible size) and never
    // read as prediction references (reads are clamped to
    // max_x/max_y), so they are dropped here.
    for (i, row) in pred.iter().enumerate().take(size) {
        for (j, value) in row.iter().enumerate().take(size) {
            if y + i <= max_y && x + j <= max_x {
                plane_data[(y + i) * stride + x + j] = clip1(*value);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Inter prediction, spec section 8.5.2.
// ---------------------------------------------------------------------------

/// Number of subpel positions (spec section 4): `1 << SUBPEL_BITS`.
pub(crate) const SUBPEL_SHIFTS: i32 = 16;

/// Rounds a motion-vector component to the nearest integer after
/// halving, spec section 8.5.2.1. Rust `/` truncates toward zero,
/// which is the division the spec uses here.
fn round_mv_comp_q2(value: i32) -> i32 {
    (if value < 0 { value - 1 } else { value + 1 }) / 2
}

/// Rounds a motion-vector component to the nearest integer after
/// quartering, spec section 8.5.2.1.
fn round_mv_comp_q4(value: i32) -> i32 {
    (if value < 0 { value - 2 } else { value + 2 }) / 4
}

/// Selects the motion vector for a block, spec section 8.5.2.1.
/// `block_mvs` is `BlockMvs[refList]` (the four sub-block MVs of
/// the 8x8 region); `block_idx` is the 4x4 sub-block index within
/// it. For chroma of a sub-8x8 block the vector is the average of
/// the luma sub-block vectors it covers.
fn select_mv(
    plane: usize,
    mi_size: u8,
    block_mvs: &[[i32; 2]; 4],
    block_idx: usize,
    subsampling_x: usize,
    subsampling_y: usize,
) -> [i32; 2] {
    // A luma block, a block at least 8x8, and a chroma block at the
    // top-left sub-block all use the sub-block vector as-is.
    if plane == 0 || mi_size >= BLOCK_8X8 || (subsampling_x == 0 && subsampling_y == 0) {
        block_mvs[block_idx]
    } else if subsampling_x == 0 && subsampling_y == 1 {
        [
            round_mv_comp_q2(block_mvs[block_idx][0] + block_mvs[block_idx + 2][0]),
            round_mv_comp_q2(block_mvs[block_idx][1] + block_mvs[block_idx + 2][1]),
        ]
    } else if subsampling_x == 1 && subsampling_y == 0 {
        [
            round_mv_comp_q2(block_mvs[block_idx][0] + block_mvs[block_idx + 1][0]),
            round_mv_comp_q2(block_mvs[block_idx][1] + block_mvs[block_idx + 1][1]),
        ]
    } else {
        [
            round_mv_comp_q4(block_mvs[0][0] + block_mvs[1][0] + block_mvs[2][0] + block_mvs[3][0]),
            round_mv_comp_q4(block_mvs[0][1] + block_mvs[1][1] + block_mvs[2][1] + block_mvs[3][1]),
        ]
    }
}

/// Clamps a motion vector to the frame plus the interpolation
/// border, spec section 8.5.2.2. The vector is converted to
/// 1/16-pel units first (`2 * mv`, then shifted for chroma
/// subsampling). Clamping is applied before scaling.
#[allow(clippy::too_many_arguments)]
fn clamp_mv_for_inter(
    mv: [i32; 2],
    plane: usize,
    mi_size: u8,
    mi_row: usize,
    mi_col: usize,
    mi_rows: usize,
    mi_cols: usize,
    subsampling_x: usize,
    subsampling_y: usize,
) -> [i32; 2] {
    let sx = if plane == 0 { 0 } else { subsampling_x as i32 };
    let sy = if plane == 0 { 0 } else { subsampling_y as i32 };
    let bh = i32::from(NUM_8X8_HIGH[mi_size as usize]);
    let bw = i32::from(NUM_8X8_WIDE[mi_size as usize]);
    let mi_size = MI_SIZE as i32;
    let mb_to_top = -((mi_row as i32 * mi_size) * 16) >> sy;
    let mb_to_bottom = (((mi_rows as i32 - bh - mi_row as i32) * mi_size) * 16) >> sy;
    let mb_to_left = -((mi_col as i32 * mi_size) * 16) >> sx;
    let mb_to_right = (((mi_cols as i32 - bw - mi_col as i32) * mi_size) * 16) >> sx;
    let spel_left = (INTERP_EXTEND + ((bw * mi_size) >> sx)) << SUBPEL_BITS;
    let spel_right = spel_left - SUBPEL_SHIFTS;
    let spel_top = (INTERP_EXTEND + ((bh * mi_size) >> sy)) << SUBPEL_BITS;
    let spel_bottom = spel_top - SUBPEL_SHIFTS;
    [
        clip3(
            i64::from((2 * mv[0]) >> sy),
            i64::from(mb_to_top - spel_top),
            i64::from(mb_to_bottom + spel_bottom),
        ) as i32,
        clip3(
            i64::from((2 * mv[1]) >> sx),
            i64::from(mb_to_left - spel_left),
            i64::from(mb_to_right + spel_right),
        ) as i32,
    ]
}

/// Scales a clamped motion vector to the reference frame's size,
/// spec section 8.5.2.3. Returns (`start_x`, `start_y`,
/// `step_x`, `step_y`) in 1/16-pel units. `ref_buf` is the
/// reference frame; the current frame's visible size is passed
/// separately. A reference frame whose size violates the
/// conformance limits is an `InvalidData` error.
#[allow(clippy::too_many_arguments)]
fn scale_mv_for_inter(
    plane: usize,
    x: usize,
    y: usize,
    clamped_mv: [i32; 2],
    ref_buf: &Vp9FrameBuffer,
    curr_vis_width: usize,
    curr_vis_height: usize,
    subsampling_x: usize,
    subsampling_y: usize,
) -> Result<(i64, i64, i64, i64)> {
    let ref_width = ref_buf.vis_width as i64;
    let ref_height = ref_buf.vis_height as i64;
    let curr_width = curr_vis_width as i64;
    let curr_height = curr_vis_height as i64;
    if 2 * curr_width < ref_width
        || 2 * curr_height < ref_height
        || curr_width > 16 * ref_width
        || curr_height > 16 * ref_height
    {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "vp9 reference frame size out of conformance range",
        });
    }
    let x_scale = (ref_width << REF_SCALE_SHIFT) / curr_width;
    let y_scale = (ref_height << REF_SCALE_SHIFT) / curr_height;
    let base_x = (x as i64 * x_scale) >> REF_SCALE_SHIFT;
    let base_y = (y as i64 * y_scale) >> REF_SCALE_SHIFT;
    // Even for chroma blocks the fractional part is based on the
    // luma block location.
    let luma_x = if plane > 0 {
        (x << subsampling_x) as i64
    } else {
        x as i64
    };
    let luma_y = if plane > 0 {
        (y << subsampling_y) as i64
    } else {
        y as i64
    };
    let frac_x = ((16 * luma_x * x_scale) >> REF_SCALE_SHIFT) & i64::from(SUBPEL_MASK);
    let frac_y = ((16 * luma_y * y_scale) >> REF_SCALE_SHIFT) & i64::from(SUBPEL_MASK);
    let dx = ((i64::from(clamped_mv[1]) * x_scale) >> REF_SCALE_SHIFT) + frac_x;
    let dy = ((i64::from(clamped_mv[0]) * y_scale) >> REF_SCALE_SHIFT) + frac_y;
    let step_x = (16 * x_scale) >> REF_SCALE_SHIFT;
    let step_y = (16 * y_scale) >> REF_SCALE_SHIFT;
    let start_x = (base_x << SUBPEL_BITS) + dx;
    let start_y = (base_y << SUBPEL_BITS) + dy;
    Ok((start_x, start_y, step_x, step_y))
}

/// Predicts one block from one reference frame with subpel
/// interpolation, spec section 8.5.2.4. Returns the predicted
/// samples in raster order (`w` by `h`). The reference
/// coordinates are clamped to the reference frame's visible
/// size, so no sample outside the reference frame is read.
#[allow(clippy::too_many_arguments)]
fn predict_block_inter(
    ref_buf: &Vp9FrameBuffer,
    plane: usize,
    start_x: i64,
    start_y: i64,
    step_x: i64,
    step_y: i64,
    w: usize,
    h: usize,
    interp_filter: u8,
    subsampling_x: usize,
    subsampling_y: usize,
) -> Vec<i64> {
    let plane_sub_x = if plane == 0 { 0 } else { subsampling_x };
    let plane_sub_y = if plane == 0 { 0 } else { subsampling_y };
    let last_x = ((ref_buf.vis_width + plane_sub_x) >> plane_sub_x) as i64 - 1;
    let last_y = ((ref_buf.vis_height + plane_sub_y) >> plane_sub_y) as i64 - 1;
    let intermediate_height = (((h as i64 - 1) * step_y + 15) >> 4) + 8;
    let filters = &SUBPEL_FILTERS[interp_filter as usize];
    let ref_plane = ref_buf.plane(plane);
    let ref_stride = ref_buf.plane_stride(plane);
    // Horizontal pass: each intermediate row is the reference
    // filtered horizontally. The row index does not depend on the
    // column, so it is computed once per row.
    let mut intermediate = vec![0i64; intermediate_height as usize * w];
    for r in 0..intermediate_height as usize {
        let row = clip3((start_y >> 4) + r as i64 - 3, 0, last_y) as usize;
        for c in 0..w {
            let p = start_x + step_x * c as i64;
            let frac = (p & 15) as usize;
            let base_col = (p >> 4) - 3;
            let mut s = 0i64;
            for (t, tap) in filters[frac].iter().enumerate() {
                let col = clip3(base_col + t as i64, 0, last_x) as usize;
                s += i64::from(*tap) * i64::from(ref_plane[row * ref_stride + col]);
            }
            intermediate[r * w + c] = i64::from(clip1(round2(s, 7)));
        }
    }
    // Vertical pass: the intermediate rows are filtered
    // vertically. The intermediate height guarantees
    // `(p >> 4) + t` stays in range for `t < 8`.
    let mut pred = vec![0i64; w * h];
    for r in 0..h {
        for c in 0..w {
            let p = (start_y & 15) + step_y * r as i64;
            let frac = (p & 15) as usize;
            let base_row = p >> 4;
            let mut s = 0i64;
            for (t, tap) in filters[frac].iter().enumerate() {
                s += i64::from(*tap) * intermediate[(base_row + t as i64) as usize * w + c];
            }
            pred[r * w + c] = i64::from(clip1(round2(s, 7)));
        }
    }
    pred
}

/// Predicts a block from its reference frame(s) with motion
/// compensation, spec section 8.5.2. `curr` is the frame being
/// reconstructed (written); `ref_buffers` is the reference frame
/// store indexed by slot (0..8), and `ref_frame_idx` maps a
/// reference frame (LAST/GOLDEN/ALTREF) to its slot.
/// `block_mvs` is `BlockMvs` for the block, indexed
/// `[refList][sub-block]`; `ref_frame` is the block's reference
/// frames `[refList]`. `x`/`y`/`w`/`h` are in the current plane's
/// sample coordinates. For a compound prediction the two
/// predictions are averaged with `Round2(..., 1)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_inter(
    curr: &mut Vp9FrameBuffer,
    plane: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    block_idx: usize,
    mi_size: u8,
    mi_row: usize,
    mi_col: usize,
    ref_frame: [i8; 2],
    ref_frame_idx: [usize; 3],
    block_mvs: &[[[i32; 2]; 4]; 2],
    interp_filter: u8,
    ref_buffers: &[Option<Vp9FrameBuffer>; 8],
    subsampling_x: usize,
    subsampling_y: usize,
) -> Result<()> {
    if plane > 2 || block_idx >= 4 || interp_filter > BILINEAR {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "vp9 inter prediction called with an out-of-range argument",
        });
    }
    let mi_rows = curr.height / MI_SIZE;
    let mi_cols = curr.width / MI_SIZE;
    // A block may start inside the frame but overhang it (the MI
    // grid is rounded up to a superblock); the write below drops
    // the overhanging samples. A block starting entirely past the
    // frame is a caller bug.
    if x >= curr.plane_width(plane) || y >= curr.plane_height(plane) {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "vp9 inter prediction block starts past the frame",
        });
    }
    let curr_vis_width = curr.vis_width;
    let curr_vis_height = curr.vis_height;
    let is_compound = ref_frame[1] > NONE_FRAME;
    let num_refs = if is_compound { 2 } else { 1 };
    let mut preds: [Vec<i64>; 2] = [Vec::new(), Vec::new()];
    for ref_list in 0..num_refs {
        if ref_frame[ref_list] <= INTRA_FRAME {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "vp9 inter prediction has an intra reference frame",
            });
        }
        let ref_idx = ref_frame_idx[(ref_frame[ref_list] - LAST_FRAME) as usize];
        let ref_buf = ref_buffers[ref_idx].as_ref().ok_or(Error::InvalidData {
            offset: 0,
            detail: "vp9 inter prediction references an empty reference slot",
        })?;
        let mv = select_mv(
            plane,
            mi_size,
            &block_mvs[ref_list],
            block_idx,
            subsampling_x,
            subsampling_y,
        );
        let clamped = clamp_mv_for_inter(
            mv,
            plane,
            mi_size,
            mi_row,
            mi_col,
            mi_rows,
            mi_cols,
            subsampling_x,
            subsampling_y,
        );
        let (start_x, start_y, step_x, step_y) = scale_mv_for_inter(
            plane,
            x,
            y,
            clamped,
            ref_buf,
            curr_vis_width,
            curr_vis_height,
            subsampling_x,
            subsampling_y,
        )?;
        preds[ref_list] = predict_block_inter(
            ref_buf,
            plane,
            start_x,
            start_y,
            step_x,
            step_y,
            w,
            h,
            interp_filter,
            subsampling_x,
            subsampling_y,
        );
    }
    let stride = curr.plane_stride(plane);
    let plane_w = curr.plane_width(plane);
    let plane_h = curr.plane_height(plane);
    let plane_data = curr.plane_mut(plane);
    // A block at the frame edge can overhang the padded plane;
    // the overhanging samples are dropped (see predict_intra).
    for r in 0..h {
        for c in 0..w {
            if y + r >= plane_h || x + c >= plane_w {
                continue;
            }
            let value = if is_compound {
                clip1(round2(preds[0][r * w + c] + preds[1][r * w + c], 1))
            } else {
                clip1(preds[0][r * w + c])
            };
            plane_data[(y + r) * stride + x + c] = value;
        }
    }
    Ok(())
}
