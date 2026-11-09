// Casts in this module are bounded by construction: tree indices
// are range-checked before use, probabilities are clamped to
// 1..=255 before narrowing, and the remaining casts move between
// integer types whose values are known small (block sizes, scan
// positions, transform indices). The pedantic cast lints therefore
// do not find real truncation risks here; the bounds are checked
// where the values are produced, not where they are cast.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]

//! VP9 entropy decoding, probability tables, coefficient decoding,
//! dequantization, and inverse transforms.
//!
//! VP9 Bitstream & Decoding Process Specification v0.6, sections
//! 6.3 (compressed header), 8.3-8.4 (probability adaptation), 8.6
//! (dequantization), 8.7 (inverse transform), 9.2-9.3 (bool decoder,
//! trees, probability selection), and 10 (fixed tables).
//!
//! The fixed probability tables are transcribed from libvpx
//! `vp9/common/vp9_entropymode.c`, `vp9/common/vp9_entropy.c`, and
//! `vp9/common/vp9_quant_common.c` (`WebM` project, BSD-style license),
//! and from Mesa `vl/vl_probs_table.h` (default coefficient
//! probabilities), cross-checked against spec section 10. The Pareto
//! table and the scan tables come from spec sections 10.3 and 10.1.
//!
//! Only the 8-bit, 4:2:0, profile 0 subset is implemented (see
//! `Vp9FrameHeader::validate_subset`); bit depth and subsampling are
//! therefore fixed to 8 and 1 here.

use crate::error::{Error, Result};
use crate::vp9_bitstream::{
    Vp9BoolDecoder, Vp9FrameHeader, Vp9InterpolationFilter, Vp9ReferenceMode, Vp9TxMode,
};

// ---------------------------------------------------------------------------
// Modes, sizes, partitions (spec section 7)
// ---------------------------------------------------------------------------

/// Intra prediction modes, spec section 7.4.
pub(crate) const DC_PRED: u8 = 0;
pub(crate) const V_PRED: u8 = 1;
pub(crate) const H_PRED: u8 = 2;
pub(crate) const D45_PRED: u8 = 3;
pub(crate) const D135_PRED: u8 = 4;
pub(crate) const D117_PRED: u8 = 5;
pub(crate) const D153_PRED: u8 = 6;
pub(crate) const D207_PRED: u8 = 7;
pub(crate) const D63_PRED: u8 = 8;
pub(crate) const TM_PRED: u8 = 9;

/// Inter prediction modes. The spec numbers them NEARESTMV = 10,
/// NEARMV = 11, ZEROMV = 12, NEWMV = 13: only that order makes the
/// spec section 9.3.1 inter-mode tree valid (its leaves are the
/// offsets from NEARESTMV, so ZEROMV must be NEARESTMV + 2).
pub(crate) const NEARESTMV: u8 = 10;
pub(crate) const NEARMV: u8 = 11;
pub(crate) const ZEROMV: u8 = 12;
pub(crate) const NEWMV: u8 = 13;
pub(crate) const MB_MODE_COUNT: usize = 14;

/// Reference frames, spec section 7.4.
pub(crate) const INTRA_FRAME: i8 = 0;
pub(crate) const LAST_FRAME: i8 = 1;
pub(crate) const GOLDEN_FRAME: i8 = 2;
pub(crate) const ALTREF_FRAME: i8 = 3;

/// Block sizes, spec section 7.4.
// The block-size constants are the complete spec table; the
// decoder reads only the sizes its profiles need today, so the
// unused entries are kept for table completeness, not deleted.
#[allow(dead_code)]
pub(crate) const BLOCK_4X4: u8 = 0;
#[allow(dead_code)]
pub(crate) const BLOCK_4X8: u8 = 1;
#[allow(dead_code)]
pub(crate) const BLOCK_8X4: u8 = 2;
pub(crate) const BLOCK_8X8: u8 = 3;
#[allow(dead_code)]
pub(crate) const BLOCK_8X16: u8 = 4;
#[allow(dead_code)]
pub(crate) const BLOCK_16X8: u8 = 5;
pub(crate) const BLOCK_16X16: u8 = 6;
#[allow(dead_code)]
pub(crate) const BLOCK_16X32: u8 = 7;
#[allow(dead_code)]
pub(crate) const BLOCK_32X16: u8 = 8;
#[allow(dead_code)]
pub(crate) const BLOCK_32X32: u8 = 9;
#[allow(dead_code)]
pub(crate) const BLOCK_32X64: u8 = 10;
#[allow(dead_code)]
pub(crate) const BLOCK_64X32: u8 = 11;
pub(crate) const BLOCK_64X64: u8 = 12;
pub(crate) const BLOCK_SIZES: usize = 13;

/// Partition types, spec section 7.4.
pub(crate) const PARTITION_NONE: u8 = 0;
pub(crate) const PARTITION_HORZ: u8 = 1;
pub(crate) const PARTITION_VERT: u8 = 2;
pub(crate) const PARTITION_SPLIT: u8 = 3;

/// Transform sizes, spec section 7.4.
pub(crate) const TX_4X4: u8 = 0;
pub(crate) const TX_8X8: u8 = 1;
pub(crate) const TX_16X16: u8 = 2;
pub(crate) const TX_32X32: u8 = 3;

/// Transform modes, spec section 7.4.
pub(crate) const TX_MODE_ONLY_4X4: u8 = 0;
pub(crate) const TX_MODE_ALLOW_8X8: u8 = 1;
pub(crate) const TX_MODE_ALLOW_16X16: u8 = 2;
pub(crate) const TX_MODE_ALLOW_32X32: u8 = 3;
pub(crate) const TX_MODE_SELECT: u8 = 4;

/// Interpolation filters, spec section 7.4.
pub(crate) const EIGHTTAP: u8 = 0;
pub(crate) const EIGHTTAP_SMOOTH: u8 = 1;
pub(crate) const EIGHTTAP_SHARP: u8 = 2;
pub(crate) const BILINEAR: u8 = 3;
pub(crate) const SWITCHABLE: u8 = 4;

/// Motion vector joints, spec section 7.4.
pub(crate) const MV_JOINT_ZERO: u8 = 0;
pub(crate) const MV_JOINT_HNZVZ: u8 = 1;
pub(crate) const MV_JOINT_HZVNZ: u8 = 2;
pub(crate) const MV_JOINT_HNZVNZ: u8 = 3;

/// Coefficient tokens, spec section 7.4.
// Part of the token table above; kept for completeness.
#[allow(dead_code)]
pub(crate) const ZERO_TOKEN: u8 = 0;
pub(crate) const ONE_TOKEN: u8 = 1;
pub(crate) const TWO_TOKEN: u8 = 2;
pub(crate) const THREE_TOKEN: u8 = 3;
pub(crate) const FOUR_TOKEN: u8 = 4;
pub(crate) const DCT_VAL_CAT1: u8 = 5;
pub(crate) const DCT_VAL_CAT2: u8 = 6;
pub(crate) const DCT_VAL_CAT3: u8 = 7;
pub(crate) const DCT_VAL_CAT4: u8 = 8;
pub(crate) const DCT_VAL_CAT5: u8 = 9;
pub(crate) const DCT_VAL_CAT6: u8 = 10;

/// Transform types, spec section 7.4.
pub(crate) const DCT_DCT: u8 = 0;
pub(crate) const ADST_DCT: u8 = 1;
pub(crate) const DCT_ADST: u8 = 2;
pub(crate) const ADST_ADST: u8 = 3;

/// Segment features, spec section 7.4.
pub(crate) const SEG_LVL_ALT_Q: usize = 0;
pub(crate) const SEG_LVL_ALT_L: usize = 1;
pub(crate) const SEG_LVL_REF_FRAME: usize = 2;
pub(crate) const SEG_LVL_SKIP: usize = 3;

/// Mode-info block size in pixels (spec section 7.4).
pub(crate) const MI_SIZE: usize = 8;
/// Sub-pixel bits for motion vectors (spec section 7.4).
pub(crate) const SUBPEL_BITS: u32 = 4;
pub(crate) const SUBPEL_MASK: i32 = 15;
/// Reference scaling shift (spec section 8.5.2.3).
pub(crate) const REF_SCALE_SHIFT: u32 = 14;
/// Interpolation border (spec section 8.5.2.4).
pub(crate) const INTERP_EXTEND: i32 = 3;
/// Motion vector border for `clamp_mv_ref` (spec section 6.5).
pub(crate) const MV_BORDER: i32 = 16 << 3;
/// High-precision motion vector threshold (spec section 6.5).
// Spec constant kept with the MV table; not read by the
// current decoder subset.
#[allow(dead_code)]
pub(crate) const COMPANDED_MVREF_THRESH: i32 = 8;
/// Maximum loop filter level (spec section 8.8).
pub(crate) const MAX_LOOP_FILTER: i32 = 63;
/// Maximum number of motion vector reference candidates.
pub(crate) const MAX_MV_REF_CANDIDATES: usize = 2;
/// Neighbour count searched by `find_mv_refs` (spec section 6.5).
pub(crate) const MVREF_NEIGHBOURS: usize = 8;
/// Count saturation for probability adaptation (spec section 8.4).
pub(crate) const COUNT_SAT: u32 = 20;
/// Maximum update factor for probability adaptation (spec 8.4).
pub(crate) const MAX_UPDATE_FACTOR: u32 = 128;
/// Count saturation for coefficient adaptation (spec section 8.4).
pub(crate) const COEF_COUNT_SAT: u32 = 24;

// ---------------------------------------------------------------------------
// Conversion tables (spec section 10.2)
// ---------------------------------------------------------------------------

pub(crate) const B_WIDTH_LOG2: [u8; BLOCK_SIZES] = [0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4];
pub(crate) const B_HEIGHT_LOG2: [u8; BLOCK_SIZES] = [0, 1, 0, 1, 2, 1, 2, 3, 2, 3, 4, 3, 4];
pub(crate) const NUM_4X4_WIDE: [u8; BLOCK_SIZES] = [1, 1, 2, 2, 2, 4, 4, 4, 8, 8, 8, 16, 16];
pub(crate) const NUM_4X4_HIGH: [u8; BLOCK_SIZES] = [1, 2, 1, 2, 4, 2, 4, 8, 4, 8, 16, 8, 16];
pub(crate) const MI_WIDTH_LOG2: [u8; BLOCK_SIZES] = [0, 0, 0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3];
pub(crate) const NUM_8X8_WIDE: [u8; BLOCK_SIZES] = [1, 1, 1, 1, 1, 2, 2, 2, 4, 4, 4, 8, 8];
// Block-size lookup table kept in full with its siblings.
#[allow(dead_code)]
pub(crate) const MI_HEIGHT_LOG2: [u8; BLOCK_SIZES] = [0, 0, 0, 0, 1, 0, 1, 2, 1, 2, 3, 2, 3];
pub(crate) const NUM_8X8_HIGH: [u8; BLOCK_SIZES] = [1, 1, 1, 1, 2, 1, 2, 4, 2, 4, 8, 4, 8];
pub(crate) const SIZE_GROUP: [u8; BLOCK_SIZES] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 3];
pub(crate) const MAX_TXSIZE: [u8; BLOCK_SIZES] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 3];
pub(crate) const TX_MODE_TO_BIGGEST: [u8; 5] = [TX_4X4, TX_8X8, TX_16X16, TX_32X32, TX_32X32];

/// Sub-block size for each partition type, spec section 10.2.
/// `-1` is `BLOCK_INVALID`: the partition is not allowed there.
pub(crate) const SUBSIZE: [[i8; BLOCK_SIZES]; 4] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
    [-1, -1, -1, 2, -1, -1, 5, -1, -1, 8, -1, -1, 11],
    [-1, -1, -1, 1, -1, -1, 4, -1, -1, 7, -1, -1, 10],
    [-1, -1, -1, 0, -1, -1, 3, -1, -1, 6, -1, -1, 9],
];

/// Plane block size for 4:2:0 (`ss_size_lookup[subsize][1][1]`,
/// spec section 6.4.23). Only 4:2:0 is in the subset, so only the
/// (1, 1) column is kept.
pub(crate) const SS_SIZE_420: [u8; BLOCK_SIZES] = [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

/// Coefficient band for 4x4 transforms, spec section 10.2.
pub(crate) const COEFBAND_4X4: [u8; 16] = [0, 1, 1, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 5, 5, 5];

/// Coefficient band for 8x8 and larger transforms, spec section
/// 10.2: the first 22 entries are listed, everything after is 5.
const COEFBAND_8X8PLUS_HEAD: [u8; 22] = [
    0, 1, 1, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 5,
];

pub(crate) fn coefband_8x8plus(c: usize) -> u8 {
    if c < COEFBAND_8X8PLUS_HEAD.len() {
        COEFBAND_8X8PLUS_HEAD[c]
    } else {
        5
    }
}

/// Energy class of each token, spec section 10.2.
pub(crate) const ENERGY_CLASS: [u8; 11] = [0, 1, 2, 3, 3, 4, 4, 5, 5, 5, 5];

/// Transform type for each intra mode (and `DCT_DCT` for inter modes),
/// spec section 10.2.
pub(crate) const MODE2TXFM: [u8; MB_MODE_COUNT] = [
    DCT_DCT, ADST_DCT, DCT_ADST, DCT_DCT, ADST_ADST, ADST_DCT, DCT_ADST, DCT_ADST, ADST_DCT,
    ADST_ADST, DCT_DCT, DCT_DCT, DCT_DCT, DCT_DCT,
];

/// Motion vector reference search order, spec section 6.5.
pub(crate) const MV_REF_BLOCKS: [[[i8; 2]; MVREF_NEIGHBOURS]; BLOCK_SIZES] = [
    [
        [-1, 0],
        [0, -1],
        [-1, -1],
        [-2, 0],
        [0, -2],
        [-2, -1],
        [-1, -2],
        [-2, -2],
    ],
    [
        [-1, 0],
        [0, -1],
        [-1, -1],
        [-2, 0],
        [0, -2],
        [-2, -1],
        [-1, -2],
        [-2, -2],
    ],
    [
        [-1, 0],
        [0, -1],
        [-1, -1],
        [-2, 0],
        [0, -2],
        [-2, -1],
        [-1, -2],
        [-2, -2],
    ],
    [
        [-1, 0],
        [0, -1],
        [-1, -1],
        [-2, 0],
        [0, -2],
        [-2, -1],
        [-1, -2],
        [-2, -2],
    ],
    [
        [0, -1],
        [-1, 0],
        [1, -1],
        [-1, -1],
        [0, -2],
        [-2, 0],
        [-2, -1],
        [-1, -2],
    ],
    [
        [-1, 0],
        [0, -1],
        [-1, 1],
        [-1, -1],
        [-2, 0],
        [0, -2],
        [-1, -2],
        [-2, -1],
    ],
    [
        [-1, 0],
        [0, -1],
        [-1, 1],
        [1, -1],
        [-1, -1],
        [-3, 0],
        [0, -3],
        [-3, -3],
    ],
    [
        [0, -1],
        [-1, 0],
        [2, -1],
        [-1, -1],
        [-1, 1],
        [0, -3],
        [-3, 0],
        [-3, -3],
    ],
    [
        [-1, 0],
        [0, -1],
        [-1, 2],
        [-1, -1],
        [1, -1],
        [-3, 0],
        [0, -3],
        [-3, -3],
    ],
    [
        [-1, 1],
        [1, -1],
        [-1, 2],
        [2, -1],
        [-1, -1],
        [-3, 0],
        [0, -3],
        [-3, -3],
    ],
    [
        [0, -1],
        [-1, 0],
        [4, -1],
        [-1, 2],
        [-1, -1],
        [0, -3],
        [-3, 0],
        [2, -1],
    ],
    [
        [-1, 0],
        [0, -1],
        [-1, 4],
        [2, -1],
        [-1, -1],
        [-3, 0],
        [0, -3],
        [-1, 2],
    ],
    [
        [-1, 3],
        [3, -1],
        [-1, 4],
        [4, -1],
        [-1, -1],
        [-1, 0],
        [0, -1],
        [-1, 6],
    ],
];

/// Mode counter for the inter-mode context, spec section 6.5.
/// Indexed by prediction mode: intra modes count 9, NEARESTMV and
/// NEARMV count 0 (both use a predicted motion vector), ZEROMV
/// counts 3, NEWMV counts 1. (The two "predicted" modes share
/// counter 0, which is what makes counter sum 0 = `BOTH_PREDICTED`.)
pub(crate) const MODE_2_COUNTER: [u8; MB_MODE_COUNT] = [9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 0, 0, 3, 1];

/// Inter-mode context for each counter sum, spec section 6.5.
/// 0 = `BOTH_ZERO`, 1 = `ZERO_PLUS_PREDICTED`, 2 = `BOTH_PREDICTED`,
/// 3 = `NEW_PLUS_NON_INTRA`, 4 = `BOTH_NEW`, 5 = `INTRA_PLUS_NON_INTRA`,
/// 6 = `BOTH_INTRA`, 9 = `INVALID_CASE` (unreachable sums).
pub(crate) const COUNTER_TO_CONTEXT: [u8; 19] =
    [2, 3, 4, 1, 3, 9, 0, 9, 9, 5, 5, 9, 5, 9, 9, 9, 9, 9, 6];

// ---------------------------------------------------------------------------
// Trees (spec section 9.3.1)
// ---------------------------------------------------------------------------

pub(crate) const PARTITION_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
pub(crate) const COLS_PARTITION_TREE: [i8; 2] = [-1, -3];
pub(crate) const ROWS_PARTITION_TREE: [i8; 2] = [-2, -3];
pub(crate) const INTRA_MODE_TREE: [i8; 18] = [
    0, 2, -9, 4, -1, 6, 8, 12, -2, 10, -4, -5, -3, 14, -8, 16, -6, -7,
];
pub(crate) const SEGMENT_TREE: [i8; 14] = [2, 4, 6, 8, 10, 12, 0, -1, -2, -3, -4, -5, -6, -7];
pub(crate) const BINARY_TREE: [i8; 2] = [0, -1];
pub(crate) const TX_SIZE_32_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
pub(crate) const TX_SIZE_16_TREE: [i8; 4] = [0, 2, -1, -2];
pub(crate) const TX_SIZE_8_TREE: [i8; 2] = [0, -1];
/// Leaves are offsets from NEARESTMV: bit 0 gives ZEROMV (+2),
/// then NEARESTMV (+0), NEARMV (+1), NEWMV (+3).
pub(crate) const INTER_MODE_TREE: [i8; 6] = [-2, 2, 0, 4, -1, -3];
pub(crate) const INTERP_FILTER_TREE: [i8; 4] = [0, 2, -1, -2];
pub(crate) const MV_JOINT_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
pub(crate) const MV_CLASS_TREE: [i8; 20] = [
    0, 2, -1, 4, 6, 8, -2, -3, 10, 12, -4, -5, -6, 14, 16, 18, -7, -8, -9, -10,
];
pub(crate) const MV_FR_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
/// Small token tree used by coefficient probability adaptation,
/// spec section 8.4.2: `{0, 0, -ZERO_TOKEN, 4, -ONE_TOKEN, -TWO_TOKEN}`.
pub(crate) const SMALL_TOKEN_TREE: [i8; 6] = [0, 0, 0, 4, -1, -2];

// ---------------------------------------------------------------------------
// Coefficient value tables (spec sections 6.4.24, 9.3.1, 10.2)
// ---------------------------------------------------------------------------

/// Extra bits for each token: (category, bit count, base value).
/// From spec section 6.4.24 and libvpx `vp9_entropy.c`.
// Coefficient table kept in full; the decoder reads it through
// the token path that does not reference this table directly.
#[allow(dead_code)]
pub(crate) const EXTRA_BITS: [[u8; 3]; 11] = [
    [0, 0, 0],
    [0, 0, 1],
    [0, 0, 2],
    [0, 0, 3],
    [0, 0, 4],
    [1, 1, 5],
    [2, 2, 7],
    [3, 3, 11],
    [4, 4, 19],
    [5, 5, 35],
    [6, 14, 67],
];

/// Category probabilities, libvpx `vp9_entropy.c`
/// (`vp9_cat1_prob` .. `vp9_cat6_prob`).
pub(crate) const CAT_PROBS: [&[u8]; 7] = [
    &[],
    &[159],
    &[165, 145],
    &[173, 148, 140],
    &[176, 155, 140, 135],
    &[180, 157, 141, 134, 130],
    &[
        254, 254, 254, 252, 249, 243, 230, 196, 177, 153, 140, 133, 130, 129,
    ],
];

/// Rounding shift with the spec's arithmetic-shift semantics:
/// `Round2(x, n) = (x + 2^(n-1)) >> n`. Rust's `>>` on `i64` is an
/// arithmetic shift, matching libvpx's `ROUND_POWER_OF_TWO`.
pub(crate) fn round2(x: i64, n: u32) -> i64 {
    (x + (1i64 << (n - 1))) >> n
}

/// Clamp to `[low, high]`, spec `Clip3`.
pub(crate) fn clip3(value: i64, low: i64, high: i64) -> i64 {
    value.clamp(low, high)
}

/// Clamp an 8-bit sample to `[0, 255]`, spec `Clip1`.
pub(crate) fn clip1(value: i64) -> u8 {
    value.clamp(0, 255) as u8
}

/// Inverse probability remap table, spec section 6.3.5.
/// 255 entries; indices 0..254. The fixed-size type enforces the
/// length at compile time.
pub(crate) const INV_MAP_TABLE: [u8; 255] = [
    7, 20, 33, 46, 59, 72, 85, 98, 111, 124, 137, 150, 163, 176, 189, 202, 215, 228, 241, 254, 1,
    2, 3, 4, 5, 6, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 21, 22, 23, 24, 25, 26, 27, 28,
    29, 30, 31, 32, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 47, 48, 49, 50, 51, 52, 53, 54,
    55, 56, 57, 58, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 73, 74, 75, 76, 77, 78, 79, 80,
    81, 82, 83, 84, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 99, 100, 101, 102, 103, 104,
    105, 106, 107, 108, 109, 110, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 123, 125,
    126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136, 138, 139, 140, 141, 142, 143, 144, 145,
    146, 147, 148, 149, 151, 152, 153, 154, 155, 156, 157, 158, 159, 160, 161, 162, 164, 165, 166,
    167, 168, 169, 170, 171, 172, 173, 174, 175, 177, 178, 179, 180, 181, 182, 183, 184, 185, 186,
    187, 188, 190, 191, 192, 193, 194, 195, 196, 197, 198, 199, 200, 201, 203, 204, 205, 206, 207,
    208, 209, 210, 211, 212, 213, 214, 216, 217, 218, 219, 220, 221, 222, 223, 224, 225, 226, 227,
    229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 242, 243, 244, 245, 246, 247, 248,
    249, 250, 251, 252, 253, 253,
];

/// Pareto table, spec section 10.3. Each row gives the 8 probabilities
/// for the token branch, indexed by node 2..9 (ZERO=0, ONE=1, TWO=2,
/// THREE=3, FOUR=4, CAT1=5, CAT2=6, CAT3=7, CAT4=8, CAT5=9, CAT6=10;
/// the node values used here are 2..9, i.e. pareto(node) with node
/// 2 = TWO). Columns are non-decreasing down each column.
pub(crate) const PARETO_TABLE: [[u8; 8]; 128] = [
    [3, 86, 128, 6, 86, 23, 88, 29],
    [9, 86, 129, 17, 88, 61, 94, 76],
    [15, 87, 129, 28, 89, 93, 100, 110],
    [20, 88, 130, 38, 91, 118, 106, 136],
    [26, 89, 131, 48, 92, 139, 111, 156],
    [31, 90, 131, 58, 94, 156, 117, 171],
    [37, 90, 132, 66, 95, 171, 122, 184],
    [42, 91, 132, 75, 97, 183, 127, 194],
    [47, 92, 133, 83, 98, 193, 132, 202],
    [52, 93, 133, 90, 100, 201, 137, 208],
    [57, 94, 134, 98, 101, 208, 142, 214],
    [62, 94, 135, 105, 103, 214, 146, 218],
    [66, 95, 135, 111, 104, 219, 151, 222],
    [71, 96, 136, 117, 106, 224, 155, 225],
    [76, 97, 136, 123, 107, 227, 159, 228],
    [80, 98, 137, 129, 109, 231, 162, 231],
    [84, 98, 138, 134, 110, 234, 166, 233],
    [89, 99, 138, 140, 112, 236, 170, 235],
    [93, 100, 139, 145, 113, 238, 173, 236],
    [97, 101, 140, 149, 115, 240, 176, 238],
    [101, 102, 140, 154, 116, 242, 179, 239],
    [105, 103, 141, 158, 118, 243, 182, 240],
    [109, 104, 141, 162, 119, 244, 185, 241],
    [113, 104, 142, 166, 120, 245, 187, 242],
    [116, 105, 143, 170, 122, 246, 190, 243],
    [120, 106, 143, 173, 123, 247, 192, 244],
    [123, 107, 144, 177, 125, 248, 195, 244],
    [127, 108, 145, 180, 126, 249, 197, 245],
    [130, 109, 145, 183, 128, 249, 199, 245],
    [134, 110, 146, 186, 129, 250, 201, 246],
    [137, 111, 147, 189, 131, 251, 203, 246],
    [140, 112, 147, 192, 132, 251, 205, 247],
    [143, 113, 148, 194, 133, 251, 207, 247],
    [146, 114, 149, 197, 135, 252, 208, 248],
    [149, 115, 149, 199, 136, 252, 210, 248],
    [152, 115, 150, 201, 138, 252, 211, 248],
    [155, 116, 151, 204, 139, 253, 213, 249],
    [158, 117, 151, 206, 140, 253, 214, 249],
    [161, 118, 152, 208, 142, 253, 216, 249],
    [163, 119, 153, 210, 143, 253, 217, 249],
    [166, 120, 153, 212, 144, 254, 218, 250],
    [168, 121, 154, 213, 146, 254, 220, 250],
    [171, 122, 155, 215, 147, 254, 221, 250],
    [173, 123, 155, 217, 148, 254, 222, 250],
    [176, 124, 156, 218, 150, 254, 223, 250],
    [178, 125, 157, 220, 151, 254, 224, 251],
    [180, 126, 157, 221, 152, 254, 225, 251],
    [183, 127, 158, 222, 153, 254, 226, 251],
    [185, 128, 159, 224, 155, 255, 227, 251],
    [187, 129, 160, 225, 156, 255, 228, 251],
    [189, 131, 160, 226, 157, 255, 228, 251],
    [191, 132, 161, 227, 159, 255, 229, 251],
    [193, 133, 162, 228, 160, 255, 230, 252],
    [195, 134, 163, 230, 161, 255, 231, 252],
    [197, 135, 163, 231, 162, 255, 231, 252],
    [199, 136, 164, 232, 163, 255, 232, 252],
    [201, 137, 165, 233, 165, 255, 233, 252],
    [202, 138, 166, 233, 166, 255, 233, 252],
    [204, 139, 166, 234, 167, 255, 234, 252],
    [206, 140, 167, 235, 168, 255, 235, 252],
    [207, 141, 168, 236, 169, 255, 235, 252],
    [209, 142, 169, 237, 171, 255, 236, 252],
    [210, 144, 169, 237, 172, 255, 236, 252],
    [212, 145, 170, 238, 173, 255, 237, 252],
    [214, 146, 171, 239, 174, 255, 237, 253],
    [215, 147, 172, 240, 175, 255, 238, 253],
    [216, 148, 173, 240, 176, 255, 238, 253],
    [218, 149, 173, 241, 177, 255, 239, 253],
    [219, 150, 174, 241, 179, 255, 239, 253],
    [220, 152, 175, 242, 180, 255, 240, 253],
    [222, 153, 176, 242, 181, 255, 240, 253],
    [223, 154, 177, 243, 182, 255, 240, 253],
    [224, 155, 178, 244, 183, 255, 241, 253],
    [225, 156, 178, 244, 184, 255, 241, 253],
    [226, 158, 179, 244, 185, 255, 242, 253],
    [228, 159, 180, 245, 186, 255, 242, 253],
    [229, 160, 181, 245, 187, 255, 242, 253],
    [230, 161, 182, 246, 188, 255, 243, 253],
    [231, 163, 183, 246, 189, 255, 243, 253],
    [232, 164, 184, 247, 190, 255, 243, 253],
    [233, 165, 185, 247, 191, 255, 244, 253],
    [234, 166, 185, 247, 192, 255, 244, 253],
    [235, 168, 186, 248, 193, 255, 244, 253],
    [236, 169, 187, 248, 194, 255, 244, 253],
    [236, 170, 188, 248, 195, 255, 245, 253],
    [237, 171, 189, 249, 196, 255, 245, 254],
    [238, 173, 190, 249, 197, 255, 245, 254],
    [239, 174, 191, 249, 198, 255, 245, 254],
    [240, 175, 192, 249, 199, 255, 246, 254],
    [240, 177, 193, 250, 200, 255, 246, 254],
    [241, 178, 194, 250, 201, 255, 246, 254],
    [242, 179, 195, 250, 202, 255, 246, 254],
    [242, 181, 196, 250, 203, 255, 247, 254],
    [243, 182, 197, 251, 204, 255, 247, 254],
    [244, 184, 198, 251, 205, 255, 247, 254],
    [244, 185, 199, 251, 206, 255, 247, 254],
    [245, 186, 200, 251, 207, 255, 247, 254],
    [246, 188, 201, 252, 207, 255, 248, 254],
    [246, 189, 202, 252, 208, 255, 248, 254],
    [247, 191, 203, 252, 209, 255, 248, 254],
    [247, 192, 204, 252, 210, 255, 248, 254],
    [248, 194, 205, 252, 211, 255, 248, 254],
    [248, 195, 206, 252, 212, 255, 249, 254],
    [249, 197, 207, 253, 213, 255, 249, 254],
    [249, 198, 208, 253, 214, 255, 249, 254],
    [250, 200, 210, 253, 215, 255, 249, 254],
    [250, 201, 211, 253, 215, 255, 249, 254],
    [250, 203, 212, 253, 216, 255, 249, 254],
    [251, 204, 213, 253, 217, 255, 250, 254],
    [251, 206, 214, 254, 218, 255, 250, 254],
    [252, 207, 216, 254, 219, 255, 250, 254],
    [252, 209, 217, 254, 220, 255, 250, 254],
    [252, 211, 218, 254, 221, 255, 250, 254],
    [253, 213, 219, 254, 222, 255, 250, 254],
    [253, 214, 221, 254, 223, 255, 250, 254],
    [253, 216, 222, 254, 224, 255, 251, 254],
    [253, 218, 224, 254, 225, 255, 251, 254],
    [254, 220, 225, 254, 225, 255, 251, 254],
    [254, 222, 227, 255, 226, 255, 251, 254],
    [254, 224, 228, 255, 227, 255, 251, 254],
    [254, 226, 230, 255, 228, 255, 251, 254],
    [255, 228, 231, 255, 230, 255, 251, 254],
    [255, 230, 233, 255, 231, 255, 252, 254],
    [255, 232, 235, 255, 232, 255, 252, 254],
    [255, 235, 237, 255, 233, 255, 252, 254],
    [255, 238, 240, 255, 235, 255, 252, 255],
    [255, 241, 243, 255, 236, 255, 252, 254],
    [255, 246, 247, 255, 239, 255, 253, 255],
];

/// `pareto(node, prob)` from spec section 9.3.2. The node is the
/// tree node 2..9 (token TWO..CAT6); prob is the pivot probability
/// `coef_probs[...][Min(2, 1 + node)]`.
pub(crate) fn pareto(node: usize, prob: u8) -> u8 {
    if node < 2 {
        return prob;
    }
    let x = ((prob as usize) - 1) / 2;
    if prob % 2 == 1 {
        PARETO_TABLE[x][node - 2]
    } else {
        ((u16::from(PARETO_TABLE[x][node - 2]) + u16::from(PARETO_TABLE[x + 1][node - 2])) >> 1)
            as u8
    }
}

// ---------------------------------------------------------------------------
// Default probability tables (spec section 10.4, libvpx vp9_entropymode.c)
// ---------------------------------------------------------------------------

/// Keyframe partition probabilities, indexed [ctx][node].
/// Used for keyframes / intra-only frames (libvpx `kf_partition_probs`).
pub(crate) const KF_PARTITION_PROBS: [[u8; 3]; 16] = [
    [158, 97, 94],
    [93, 24, 99],
    [85, 119, 44],
    [62, 59, 67],
    [149, 53, 53],
    [94, 20, 48],
    [83, 53, 24],
    [52, 18, 18],
    [150, 40, 39],
    [78, 12, 26],
    [67, 33, 11],
    [24, 7, 5],
    [174, 35, 49],
    [68, 11, 27],
    [57, 15, 9],
    [12, 3, 3],
];

/// Default (inter-frame) partition probabilities, indexed [ctx][node].
pub(crate) const DEFAULT_PARTITION_PROBS: [[u8; 3]; 16] = [
    [199, 122, 141],
    [147, 63, 159],
    [148, 133, 118],
    [121, 104, 114],
    [174, 73, 87],
    [92, 41, 83],
    [82, 99, 50],
    [53, 39, 39],
    [177, 58, 59],
    [68, 26, 63],
    [52, 79, 25],
    [17, 14, 12],
    [222, 34, 30],
    [72, 16, 44],
    [58, 32, 12],
    [10, 7, 6],
];

/// Default luma intra-mode probabilities, indexed [size group][node].
pub(crate) const DEFAULT_IF_Y_PROBS: [[u8; 9]; 4] = [
    [65, 32, 18, 144, 162, 194, 41, 51, 98],
    [132, 68, 18, 165, 217, 196, 45, 40, 78],
    [173, 80, 19, 176, 240, 193, 64, 35, 46],
    [221, 135, 38, 194, 248, 121, 96, 85, 29],
];

/// Default chroma intra-mode probabilities, indexed [y mode][node].
pub(crate) const DEFAULT_IF_UV_PROBS: [[u8; 9]; 10] = [
    [120, 7, 76, 176, 208, 126, 28, 54, 103],
    [48, 12, 154, 155, 139, 90, 34, 117, 119],
    [67, 6, 25, 204, 243, 158, 13, 21, 96],
    [97, 5, 44, 131, 176, 139, 48, 68, 97],
    [83, 5, 42, 156, 111, 152, 26, 49, 152],
    [80, 5, 58, 178, 74, 83, 33, 62, 145],
    [86, 5, 32, 154, 192, 168, 14, 22, 163],
    [85, 5, 32, 156, 216, 148, 19, 29, 73],
    [77, 7, 64, 116, 132, 122, 37, 126, 120],
    [101, 21, 107, 181, 192, 103, 19, 67, 125],
];

/// Keyframe chroma intra-mode probabilities, indexed [y mode][node].
pub(crate) const KF_UV_MODE_PROB: [[u8; 9]; 10] = [
    [144, 11, 54, 157, 195, 130, 46, 58, 108],
    [118, 15, 123, 148, 131, 101, 44, 93, 131],
    [113, 12, 23, 188, 226, 142, 26, 32, 125],
    [120, 11, 50, 123, 163, 135, 64, 77, 103],
    [113, 9, 36, 155, 111, 157, 32, 44, 161],
    [116, 9, 55, 176, 76, 96, 37, 61, 149],
    [115, 9, 28, 141, 161, 167, 21, 25, 193],
    [120, 12, 32, 145, 195, 142, 32, 38, 86],
    [116, 12, 64, 120, 140, 125, 49, 115, 121],
    [102, 19, 66, 162, 182, 122, 35, 59, 128],
];

/// Default inter-mode probabilities, indexed [context][node].
pub(crate) const DEFAULT_INTER_MODE_PROBS: [[u8; 3]; 7] = [
    [2, 173, 34],
    [7, 145, 85],
    [7, 166, 63],
    [7, 94, 66],
    [8, 64, 46],
    [17, 81, 31],
    [25, 29, 30],
];

pub(crate) const DEFAULT_SKIP_PROBS: [u8; 3] = [192, 128, 64];
pub(crate) const DEFAULT_INTRA_INTER_P: [u8; 4] = [9, 102, 187, 225];
pub(crate) const DEFAULT_COMP_INTER_P: [u8; 5] = [239, 183, 119, 96, 41];
pub(crate) const DEFAULT_COMP_REF_P: [u8; 5] = [50, 126, 123, 221, 226];
pub(crate) const DEFAULT_SINGLE_REF_P: [[u8; 2]; 5] =
    [[33, 16], [77, 74], [142, 142], [172, 170], [238, 247]];
pub(crate) const DEFAULT_SWITCHABLE_INTERP_PROB: [[u8; 2]; 4] =
    [[235, 162], [36, 255], [34, 3], [149, 144]];

/// Default transform-size probabilities. Indexed [max tx size][ctx]
/// and then by tree node: 32x32 has 3 nodes, 16x16 has 2, 8x8 has 1.
pub(crate) const DEFAULT_TX_PROBS_32X32: [[u8; 3]; 2] = [[3, 136, 37], [5, 52, 13]];
pub(crate) const DEFAULT_TX_PROBS_16X16: [[u8; 2]; 2] = [[20, 152], [15, 101]];
pub(crate) const DEFAULT_TX_PROBS_8X8: [[u8; 1]; 2] = [[100], [66]];

/// Default segmentation tree probabilities (all 255) and
/// segmentation prediction probabilities (all 255).
// Default probabilities kept with the other default tables;
// the decoder builds its working context from the frame header.
#[allow(dead_code)]
pub(crate) const DEFAULT_SEG_TREE_PROBS: [u8; 7] = [255; 7];
#[allow(dead_code)]
pub(crate) const DEFAULT_SEG_PRED_PROBS: [u8; 3] = [255; 3];

/// Default loop filter deltas: reference deltas for
/// INTRA/LAST/GOLDEN/ALTREF, and the two mode deltas.
// Default loop-filter deltas kept with the other default tables.
#[allow(dead_code)]
pub(crate) const DEFAULT_LF_REF_DELTAS: [i32; 4] = [1, 0, -1, -1];
#[allow(dead_code)]
pub(crate) const DEFAULT_LF_MODE_DELTAS: [i32; 2] = [0, 0];

/// Default motion-vector joint probabilities, libvpx
/// `default_nmv_joints`.
pub(crate) const DEFAULT_NMV_JOINTS: [u8; 3] = [32, 64, 96];

/// Default motion-vector component probabilities, libvpx
/// `default_nmv_components`. Flat layout per component
/// (vertical = 0, horizontal = 1), 33 entries:
/// sign, class[10], class0, bits[10], `class0_fp`[6], fp[3],
/// `class0_hp`, hp.
pub(crate) const DEFAULT_NMV_COMPONENTS: [[u8; 33]; 2] = [
    [
        128, 224, 144, 192, 168, 192, 176, 192, 198, 198, 245, 216, 136, 140, 148, 160, 176, 192,
        224, 234, 234, 240, 128, 128, 64, 96, 112, 64, 64, 96, 64, 160, 128,
    ],
    [
        128, 216, 128, 176, 160, 176, 176, 192, 198, 198, 208, 208, 136, 140, 148, 160, 176, 192,
        224, 234, 234, 240, 128, 128, 64, 96, 112, 64, 64, 96, 64, 160, 128,
    ],
];

/// Keyframe luma intra-mode probabilities, spec section 10.4
/// (`kf_y_mode_probs`), indexed [above mode][left mode][node].
/// Spot check: `[0][0] == [137, 30, 42, 148, 151, 207, 70, 52, 91]`.
pub(crate) const KF_Y_MODE_PROB: [[[u8; 9]; 10]; 10] = [
    [
        [137, 30, 42, 148, 151, 207, 70, 52, 91],
        [92, 45, 102, 136, 116, 180, 74, 90, 100],
        [73, 32, 19, 187, 222, 215, 46, 34, 100],
        [91, 30, 32, 116, 121, 186, 93, 86, 94],
        [72, 35, 36, 149, 68, 206, 68, 63, 105],
        [73, 31, 28, 138, 57, 124, 55, 122, 151],
        [67, 23, 21, 140, 126, 197, 40, 37, 171],
        [86, 27, 28, 128, 154, 212, 45, 43, 53],
        [74, 32, 27, 107, 86, 160, 63, 134, 102],
        [59, 67, 44, 140, 161, 202, 78, 67, 119],
    ],
    [
        [63, 36, 126, 146, 123, 158, 60, 90, 96],
        [43, 46, 168, 134, 107, 128, 69, 142, 92],
        [44, 29, 68, 159, 201, 177, 50, 57, 77],
        [58, 38, 76, 114, 97, 172, 78, 133, 92],
        [46, 41, 76, 140, 63, 184, 69, 112, 57],
        [38, 32, 85, 140, 46, 112, 54, 151, 133],
        [39, 27, 61, 131, 110, 175, 44, 75, 136],
        [52, 30, 74, 113, 130, 175, 51, 64, 58],
        [47, 35, 80, 100, 74, 143, 64, 163, 74],
        [36, 61, 116, 114, 128, 162, 80, 125, 82],
    ],
    [
        [82, 26, 26, 171, 208, 204, 44, 32, 105],
        [55, 44, 68, 166, 179, 192, 57, 57, 108],
        [42, 26, 11, 199, 241, 228, 23, 15, 85],
        [68, 42, 19, 131, 160, 199, 55, 52, 83],
        [58, 50, 25, 139, 115, 232, 39, 52, 118],
        [50, 35, 33, 153, 104, 162, 64, 59, 131],
        [44, 24, 16, 150, 177, 202, 33, 19, 156],
        [55, 27, 12, 153, 203, 218, 26, 27, 49],
        [53, 49, 21, 110, 116, 168, 59, 80, 76],
        [38, 72, 19, 168, 203, 212, 50, 50, 107],
    ],
    [
        [103, 26, 36, 129, 132, 201, 83, 80, 93],
        [59, 38, 83, 112, 103, 162, 98, 136, 90],
        [62, 30, 23, 158, 200, 207, 59, 57, 50],
        [67, 30, 29, 84, 86, 191, 102, 91, 59],
        [60, 32, 33, 112, 71, 220, 64, 89, 104],
        [53, 26, 34, 130, 56, 149, 84, 120, 103],
        [53, 21, 23, 133, 109, 210, 56, 77, 172],
        [77, 19, 29, 112, 142, 228, 55, 66, 36],
        [61, 29, 29, 93, 97, 165, 83, 175, 162],
        [47, 47, 43, 114, 137, 181, 100, 99, 95],
    ],
    [
        [69, 23, 29, 128, 83, 199, 46, 44, 101],
        [53, 40, 55, 139, 69, 183, 61, 80, 110],
        [40, 29, 19, 161, 180, 207, 43, 24, 91],
        [60, 34, 19, 105, 61, 198, 53, 64, 89],
        [52, 31, 22, 158, 40, 209, 58, 62, 89],
        [44, 31, 29, 147, 46, 158, 56, 102, 198],
        [35, 19, 12, 135, 87, 209, 41, 45, 167],
        [55, 25, 21, 118, 95, 215, 38, 39, 66],
        [51, 38, 25, 113, 58, 164, 70, 93, 97],
        [47, 54, 34, 146, 108, 203, 72, 103, 151],
    ],
    [
        [64, 19, 37, 156, 66, 138, 49, 95, 133],
        [46, 27, 80, 150, 55, 124, 55, 121, 135],
        [36, 23, 27, 165, 149, 166, 54, 64, 118],
        [53, 21, 36, 131, 63, 163, 60, 109, 81],
        [40, 26, 35, 154, 40, 185, 51, 97, 123],
        [35, 19, 34, 179, 19, 97, 48, 129, 124],
        [36, 20, 26, 136, 62, 164, 33, 77, 154],
        [45, 18, 32, 130, 90, 157, 40, 79, 91],
        [45, 26, 28, 129, 45, 129, 49, 147, 123],
        [38, 44, 51, 136, 74, 162, 57, 97, 121],
    ],
    [
        [75, 17, 22, 136, 138, 185, 32, 34, 166],
        [56, 39, 58, 133, 117, 173, 48, 53, 187],
        [35, 21, 12, 161, 212, 207, 20, 23, 145],
        [56, 29, 19, 117, 109, 181, 55, 68, 112],
        [47, 29, 17, 153, 64, 220, 59, 51, 114],
        [46, 16, 24, 136, 76, 147, 41, 64, 172],
        [34, 17, 11, 108, 152, 187, 13, 15, 209],
        [51, 24, 14, 115, 133, 209, 32, 26, 104],
        [55, 30, 18, 122, 79, 179, 44, 88, 116],
        [37, 49, 25, 129, 168, 164, 41, 54, 148],
    ],
    [
        [82, 22, 32, 127, 143, 213, 39, 41, 70],
        [62, 44, 61, 123, 105, 189, 48, 57, 64],
        [47, 25, 17, 175, 222, 220, 24, 30, 86],
        [68, 36, 17, 106, 102, 206, 59, 74, 74],
        [57, 39, 23, 151, 68, 216, 55, 63, 58],
        [49, 30, 35, 141, 70, 168, 82, 40, 115],
        [51, 25, 15, 136, 129, 202, 38, 35, 139],
        [68, 26, 16, 111, 141, 215, 29, 28, 28],
        [59, 39, 19, 114, 75, 180, 77, 104, 42],
        [40, 61, 26, 126, 152, 206, 61, 59, 93],
    ],
    [
        [78, 23, 39, 111, 117, 170, 74, 124, 94],
        [48, 34, 86, 101, 92, 146, 78, 179, 134],
        [47, 22, 24, 138, 187, 178, 68, 69, 59],
        [56, 25, 33, 105, 112, 187, 95, 177, 129],
        [48, 31, 27, 114, 63, 183, 82, 116, 56],
        [43, 28, 37, 121, 63, 123, 61, 192, 169],
        [42, 17, 24, 109, 97, 177, 56, 76, 122],
        [58, 18, 28, 105, 139, 182, 70, 92, 63],
        [46, 23, 32, 74, 86, 150, 67, 183, 88],
        [36, 38, 48, 92, 122, 165, 88, 137, 91],
    ],
    [
        [65, 70, 60, 155, 159, 199, 61, 60, 81],
        [44, 78, 115, 132, 119, 173, 71, 112, 93],
        [39, 38, 21, 184, 227, 206, 42, 32, 64],
        [58, 47, 36, 124, 137, 193, 80, 82, 78],
        [49, 50, 35, 144, 95, 205, 63, 78, 59],
        [41, 53, 52, 148, 71, 142, 65, 128, 51],
        [40, 36, 28, 143, 143, 202, 40, 55, 137],
        [52, 34, 29, 129, 183, 227, 42, 35, 43],
        [42, 44, 44, 104, 105, 164, 64, 130, 80],
        [43, 81, 53, 140, 169, 204, 68, 84, 72],
    ],
];

// ---------------------------------------------------------------------------
// Default coefficient probabilities (Mesa `vl_probs_table.h`,
// transcribed from libvpx `default_coef_probs_*`).
// Layout: [plane Y=0/UV=1][ref Intra=0/Inter=1][band 0..5][ctx][node].
// Band 0 only has 3 contexts in the source (it is only used at scan
// position 0, where the context is the above/left sum, 0..2); the
// unused contexts 3..5 are padded with zeros here.
// ---------------------------------------------------------------------------

pub(crate) const DEFAULT_COEF_PROBS_4X4: [[[[[u8; 3]; 6]; 6]; 2]; 2] = [
    [
        [
            [
                [195, 29, 183],
                [84, 49, 136],
                [8, 42, 71],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [31, 107, 169],
                [35, 99, 159],
                [17, 82, 140],
                [8, 66, 114],
                [2, 44, 76],
                [1, 19, 32],
            ],
            [
                [40, 132, 201],
                [29, 114, 187],
                [13, 91, 157],
                [7, 75, 127],
                [3, 58, 95],
                [1, 28, 47],
            ],
            [
                [69, 142, 221],
                [42, 122, 201],
                [15, 91, 159],
                [6, 67, 121],
                [1, 42, 77],
                [1, 17, 31],
            ],
            [
                [102, 148, 228],
                [67, 117, 204],
                [17, 82, 154],
                [6, 59, 114],
                [2, 39, 75],
                [1, 15, 29],
            ],
            [
                [156, 57, 233],
                [119, 57, 212],
                [58, 48, 163],
                [29, 40, 124],
                [12, 30, 81],
                [3, 12, 31],
            ],
        ],
        [
            [
                [191, 107, 226],
                [124, 117, 204],
                [25, 99, 155],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [29, 148, 210],
                [37, 126, 194],
                [8, 93, 157],
                [2, 68, 118],
                [1, 39, 69],
                [1, 17, 33],
            ],
            [
                [41, 151, 213],
                [27, 123, 193],
                [3, 82, 144],
                [1, 58, 105],
                [1, 32, 60],
                [1, 13, 26],
            ],
            [
                [59, 159, 220],
                [23, 126, 198],
                [4, 88, 151],
                [1, 66, 114],
                [1, 38, 71],
                [1, 18, 34],
            ],
            [
                [114, 136, 232],
                [51, 114, 207],
                [11, 83, 155],
                [3, 56, 105],
                [1, 33, 65],
                [1, 17, 34],
            ],
            [
                [149, 65, 234],
                [121, 57, 215],
                [61, 49, 166],
                [28, 36, 114],
                [12, 25, 76],
                [3, 16, 42],
            ],
        ],
    ],
    [
        [
            [
                [214, 49, 220],
                [132, 63, 188],
                [42, 65, 137],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [85, 137, 221],
                [104, 131, 216],
                [49, 111, 192],
                [21, 87, 155],
                [2, 49, 87],
                [1, 16, 28],
            ],
            [
                [89, 163, 230],
                [90, 137, 220],
                [29, 100, 183],
                [10, 70, 135],
                [2, 42, 81],
                [1, 17, 33],
            ],
            [
                [108, 167, 237],
                [55, 133, 222],
                [15, 97, 179],
                [4, 72, 135],
                [1, 45, 85],
                [1, 19, 38],
            ],
            [
                [124, 146, 240],
                [66, 124, 224],
                [17, 88, 175],
                [4, 58, 122],
                [1, 36, 75],
                [1, 18, 37],
            ],
            [
                [141, 79, 241],
                [126, 70, 227],
                [66, 58, 182],
                [30, 44, 136],
                [12, 34, 96],
                [2, 20, 47],
            ],
        ],
        [
            [
                [229, 99, 249],
                [143, 111, 235],
                [46, 109, 192],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [82, 158, 236],
                [94, 146, 224],
                [25, 117, 191],
                [9, 87, 149],
                [3, 56, 99],
                [1, 33, 57],
            ],
            [
                [83, 167, 237],
                [68, 145, 222],
                [10, 103, 177],
                [2, 72, 131],
                [1, 41, 79],
                [1, 20, 39],
            ],
            [
                [99, 167, 239],
                [47, 141, 224],
                [10, 104, 178],
                [2, 73, 133],
                [1, 44, 85],
                [1, 22, 47],
            ],
            [
                [127, 145, 243],
                [71, 129, 228],
                [17, 93, 177],
                [3, 61, 124],
                [1, 41, 84],
                [1, 21, 52],
            ],
            [
                [157, 78, 244],
                [140, 72, 231],
                [69, 58, 184],
                [31, 44, 137],
                [14, 38, 105],
                [8, 23, 61],
            ],
        ],
    ],
];

pub(crate) const DEFAULT_COEF_PROBS_8X8: [[[[[u8; 3]; 6]; 6]; 2]; 2] = [
    [
        [
            [
                [125, 34, 187],
                [52, 41, 133],
                [6, 31, 56],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [37, 109, 153],
                [51, 102, 147],
                [23, 87, 128],
                [8, 67, 101],
                [1, 41, 63],
                [1, 19, 29],
            ],
            [
                [31, 154, 185],
                [17, 127, 175],
                [6, 96, 145],
                [2, 73, 114],
                [1, 51, 82],
                [1, 28, 45],
            ],
            [
                [23, 163, 200],
                [10, 131, 185],
                [2, 93, 148],
                [1, 67, 111],
                [1, 41, 69],
                [1, 14, 24],
            ],
            [
                [29, 176, 217],
                [12, 145, 201],
                [3, 101, 156],
                [1, 69, 111],
                [1, 39, 63],
                [1, 14, 23],
            ],
            [
                [57, 192, 233],
                [25, 154, 215],
                [6, 109, 167],
                [3, 78, 118],
                [1, 48, 69],
                [1, 21, 29],
            ],
        ],
        [
            [
                [202, 105, 245],
                [108, 106, 216],
                [18, 90, 144],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [33, 172, 219],
                [64, 149, 206],
                [14, 117, 177],
                [5, 90, 141],
                [2, 61, 95],
                [1, 37, 57],
            ],
            [
                [33, 179, 220],
                [11, 140, 198],
                [1, 89, 148],
                [1, 60, 104],
                [1, 33, 57],
                [1, 12, 21],
            ],
            [
                [30, 181, 221],
                [8, 141, 198],
                [1, 87, 145],
                [1, 58, 100],
                [1, 31, 55],
                [1, 12, 20],
            ],
            [
                [32, 186, 224],
                [7, 142, 198],
                [1, 86, 143],
                [1, 58, 100],
                [1, 31, 55],
                [1, 12, 22],
            ],
            [
                [57, 192, 227],
                [20, 143, 204],
                [3, 96, 154],
                [1, 68, 112],
                [1, 42, 69],
                [1, 19, 32],
            ],
        ],
    ],
    [
        [
            [
                [212, 35, 215],
                [113, 47, 169],
                [29, 48, 105],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [74, 129, 203],
                [106, 120, 203],
                [49, 107, 178],
                [19, 84, 144],
                [4, 50, 84],
                [1, 15, 25],
            ],
            [
                [71, 172, 217],
                [44, 141, 209],
                [15, 102, 173],
                [6, 76, 133],
                [2, 51, 89],
                [1, 24, 42],
            ],
            [
                [64, 185, 231],
                [31, 148, 216],
                [8, 103, 175],
                [3, 74, 131],
                [1, 46, 81],
                [1, 18, 30],
            ],
            [
                [65, 196, 235],
                [25, 157, 221],
                [5, 105, 174],
                [1, 67, 120],
                [1, 38, 69],
                [1, 15, 30],
            ],
            [
                [65, 204, 238],
                [30, 156, 224],
                [7, 107, 177],
                [2, 70, 124],
                [1, 42, 73],
                [1, 18, 34],
            ],
        ],
        [
            [
                [225, 86, 251],
                [144, 104, 235],
                [42, 99, 181],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [85, 175, 239],
                [112, 165, 229],
                [29, 136, 200],
                [12, 103, 162],
                [6, 77, 123],
                [2, 53, 84],
            ],
            [
                [75, 183, 239],
                [30, 155, 221],
                [3, 106, 171],
                [1, 74, 128],
                [1, 44, 76],
                [1, 17, 28],
            ],
            [
                [73, 185, 240],
                [27, 159, 222],
                [2, 107, 172],
                [1, 75, 127],
                [1, 42, 73],
                [1, 17, 29],
            ],
            [
                [62, 190, 238],
                [21, 159, 222],
                [2, 107, 172],
                [1, 72, 122],
                [1, 40, 71],
                [1, 18, 32],
            ],
            [
                [61, 199, 240],
                [27, 161, 226],
                [4, 113, 180],
                [1, 76, 129],
                [1, 46, 80],
                [1, 23, 41],
            ],
        ],
    ],
];

pub(crate) const DEFAULT_COEF_PROBS_16X16: [[[[[u8; 3]; 6]; 6]; 2]; 2] = [
    [
        [
            [
                [7, 27, 153],
                [5, 30, 95],
                [1, 16, 30],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [50, 75, 127],
                [57, 75, 124],
                [27, 67, 108],
                [10, 54, 86],
                [1, 33, 52],
                [1, 12, 18],
            ],
            [
                [43, 125, 151],
                [26, 108, 148],
                [7, 83, 122],
                [2, 59, 89],
                [1, 38, 60],
                [1, 17, 27],
            ],
            [
                [23, 144, 163],
                [13, 112, 154],
                [2, 75, 117],
                [1, 50, 81],
                [1, 31, 51],
                [1, 14, 23],
            ],
            [
                [18, 162, 185],
                [6, 123, 171],
                [1, 78, 125],
                [1, 51, 86],
                [1, 31, 54],
                [1, 14, 23],
            ],
            [
                [15, 199, 227],
                [3, 150, 204],
                [1, 91, 146],
                [1, 55, 95],
                [1, 30, 53],
                [1, 11, 20],
            ],
        ],
        [
            [
                [19, 55, 240],
                [19, 59, 196],
                [3, 52, 105],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [41, 166, 207],
                [104, 153, 199],
                [31, 123, 181],
                [14, 101, 152],
                [5, 72, 106],
                [1, 36, 52],
            ],
            [
                [35, 176, 211],
                [12, 131, 190],
                [2, 88, 144],
                [1, 60, 101],
                [1, 36, 60],
                [1, 16, 28],
            ],
            [
                [28, 183, 213],
                [8, 134, 191],
                [1, 86, 142],
                [1, 56, 96],
                [1, 30, 53],
                [1, 12, 20],
            ],
            [
                [20, 190, 215],
                [4, 135, 192],
                [1, 84, 139],
                [1, 53, 91],
                [1, 28, 49],
                [1, 11, 20],
            ],
            [
                [13, 196, 216],
                [2, 137, 192],
                [1, 86, 143],
                [1, 57, 99],
                [1, 32, 56],
                [1, 13, 24],
            ],
        ],
    ],
    [
        [
            [
                [211, 29, 217],
                [96, 47, 156],
                [22, 43, 87],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [78, 120, 193],
                [111, 116, 186],
                [46, 102, 164],
                [15, 80, 128],
                [2, 49, 76],
                [1, 18, 28],
            ],
            [
                [71, 161, 203],
                [42, 132, 192],
                [10, 98, 150],
                [3, 69, 109],
                [1, 44, 70],
                [1, 18, 29],
            ],
            [
                [57, 186, 211],
                [30, 140, 196],
                [4, 93, 146],
                [1, 62, 102],
                [1, 38, 65],
                [1, 16, 27],
            ],
            [
                [47, 199, 217],
                [14, 145, 196],
                [1, 88, 142],
                [1, 57, 98],
                [1, 36, 62],
                [1, 15, 26],
            ],
            [
                [26, 219, 229],
                [5, 155, 207],
                [1, 94, 151],
                [1, 60, 104],
                [1, 36, 62],
                [1, 16, 28],
            ],
        ],
        [
            [
                [233, 29, 248],
                [146, 47, 220],
                [43, 52, 140],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [100, 163, 232],
                [179, 161, 222],
                [63, 142, 204],
                [37, 113, 174],
                [26, 89, 137],
                [18, 68, 97],
            ],
            [
                [85, 181, 230],
                [32, 146, 209],
                [7, 100, 164],
                [3, 71, 121],
                [1, 45, 77],
                [1, 18, 30],
            ],
            [
                [65, 187, 230],
                [20, 148, 207],
                [2, 97, 159],
                [1, 68, 116],
                [1, 40, 70],
                [1, 14, 29],
            ],
            [
                [40, 194, 227],
                [8, 147, 204],
                [1, 94, 155],
                [1, 65, 112],
                [1, 39, 66],
                [1, 14, 26],
            ],
            [
                [16, 208, 228],
                [3, 151, 207],
                [1, 98, 160],
                [1, 67, 117],
                [1, 41, 74],
                [1, 17, 31],
            ],
        ],
    ],
];

pub(crate) const DEFAULT_COEF_PROBS_32X32: [[[[[u8; 3]; 6]; 6]; 2]; 2] = [
    [
        [
            [
                [17, 38, 140],
                [7, 34, 80],
                [1, 17, 29],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [37, 75, 128],
                [41, 76, 128],
                [26, 66, 116],
                [12, 52, 94],
                [2, 32, 55],
                [1, 10, 16],
            ],
            [
                [50, 127, 154],
                [37, 109, 152],
                [16, 82, 121],
                [5, 59, 85],
                [1, 35, 54],
                [1, 13, 20],
            ],
            [
                [40, 142, 167],
                [17, 110, 157],
                [2, 71, 112],
                [1, 44, 72],
                [1, 27, 45],
                [1, 11, 17],
            ],
            [
                [30, 175, 188],
                [9, 124, 169],
                [1, 74, 116],
                [1, 48, 78],
                [1, 30, 49],
                [1, 11, 18],
            ],
            [
                [10, 222, 223],
                [2, 150, 194],
                [1, 83, 128],
                [1, 48, 79],
                [1, 27, 45],
                [1, 11, 17],
            ],
        ],
        [
            [
                [36, 41, 235],
                [29, 36, 193],
                [10, 27, 111],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [85, 165, 222],
                [177, 162, 215],
                [110, 135, 195],
                [57, 113, 168],
                [23, 83, 120],
                [10, 49, 61],
            ],
            [
                [85, 190, 223],
                [36, 139, 200],
                [5, 90, 146],
                [1, 60, 103],
                [1, 38, 65],
                [1, 18, 30],
            ],
            [
                [72, 202, 223],
                [23, 141, 199],
                [2, 86, 140],
                [1, 56, 97],
                [1, 36, 61],
                [1, 16, 27],
            ],
            [
                [55, 218, 225],
                [13, 145, 200],
                [1, 86, 141],
                [1, 57, 99],
                [1, 35, 61],
                [1, 13, 22],
            ],
            [
                [15, 235, 212],
                [1, 132, 184],
                [1, 84, 139],
                [1, 57, 97],
                [1, 34, 56],
                [1, 14, 23],
            ],
        ],
    ],
    [
        [
            [
                [181, 21, 201],
                [61, 37, 123],
                [10, 38, 71],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [47, 106, 172],
                [95, 104, 173],
                [42, 93, 159],
                [18, 77, 131],
                [4, 50, 81],
                [1, 17, 23],
            ],
            [
                [62, 147, 199],
                [44, 130, 189],
                [28, 102, 154],
                [18, 75, 115],
                [2, 44, 65],
                [1, 12, 19],
            ],
            [
                [55, 153, 210],
                [24, 130, 194],
                [3, 93, 146],
                [1, 61, 97],
                [1, 31, 50],
                [1, 10, 16],
            ],
            [
                [49, 186, 223],
                [17, 148, 204],
                [1, 96, 142],
                [1, 53, 83],
                [1, 26, 44],
                [1, 11, 17],
            ],
            [
                [13, 217, 212],
                [2, 136, 180],
                [1, 78, 124],
                [1, 50, 83],
                [1, 29, 49],
                [1, 14, 23],
            ],
        ],
        [
            [
                [197, 13, 247],
                [82, 17, 222],
                [25, 17, 162],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
            [
                [126, 186, 247],
                [234, 191, 243],
                [176, 177, 234],
                [104, 158, 220],
                [66, 128, 186],
                [55, 90, 137],
            ],
            [
                [111, 197, 242],
                [46, 158, 219],
                [9, 104, 171],
                [2, 65, 125],
                [1, 44, 80],
                [1, 17, 91],
            ],
            [
                [104, 208, 245],
                [39, 168, 224],
                [3, 109, 162],
                [1, 79, 124],
                [1, 50, 102],
                [1, 43, 102],
            ],
            [
                [84, 220, 246],
                [31, 177, 231],
                [2, 115, 180],
                [1, 79, 134],
                [1, 55, 77],
                [1, 60, 79],
            ],
            [
                [43, 243, 240],
                [8, 180, 217],
                [1, 115, 166],
                [1, 84, 121],
                [1, 51, 67],
                [1, 16, 6],
            ],
        ],
    ],
];

/// Scan tables, spec section 10.1. Each is a permutation of
/// `0..N-1` (checked by unit test).
pub(crate) const DEFAULT_SCAN_4X4: [u16; 16] =
    [0, 4, 1, 5, 8, 2, 12, 9, 3, 6, 13, 10, 7, 14, 11, 15];
pub(crate) const COL_SCAN_4X4: [u16; 16] = [0, 4, 8, 1, 12, 5, 9, 2, 13, 6, 10, 3, 7, 14, 11, 15];
pub(crate) const ROW_SCAN_4X4: [u16; 16] = [0, 1, 4, 2, 5, 3, 6, 8, 9, 7, 12, 10, 13, 11, 14, 15];

pub(crate) const DEFAULT_SCAN_8X8: [u16; 64] = [
    0, 8, 1, 16, 9, 2, 17, 24, 10, 3, 18, 25, 32, 11, 4, 26, 33, 19, 40, 12, 34, 27, 5, 41, 20, 48,
    13, 35, 42, 28, 21, 6, 49, 56, 36, 43, 29, 7, 14, 50, 57, 44, 22, 37, 15, 51, 58, 30, 45, 23,
    52, 59, 38, 31, 60, 53, 46, 39, 61, 54, 47, 62, 55, 63,
];
pub(crate) const COL_SCAN_8X8: [u16; 64] = [
    0, 8, 16, 1, 24, 9, 32, 17, 2, 40, 25, 10, 33, 18, 48, 3, 26, 41, 11, 56, 19, 34, 4, 49, 27,
    42, 12, 35, 20, 57, 50, 28, 5, 43, 13, 36, 58, 51, 21, 44, 6, 29, 59, 37, 14, 52, 22, 7, 45,
    60, 30, 15, 38, 53, 23, 46, 31, 61, 39, 54, 47, 62, 55, 63,
];
pub(crate) const ROW_SCAN_8X8: [u16; 64] = [
    0, 1, 2, 8, 9, 3, 16, 10, 4, 17, 11, 24, 5, 18, 25, 12, 19, 26, 32, 6, 13, 20, 33, 27, 7, 34,
    40, 21, 28, 41, 14, 35, 48, 42, 29, 36, 49, 22, 43, 15, 56, 37, 50, 44, 30, 57, 23, 51, 58, 45,
    38, 52, 31, 59, 53, 46, 60, 39, 61, 47, 54, 55, 62, 63,
];

pub(crate) const DEFAULT_SCAN_16X16: [u16; 256] = [
    0, 16, 1, 32, 17, 2, 48, 33, 18, 3, 64, 34, 49, 19, 65, 80, 50, 4, 35, 66, 20, 81, 96, 51, 5,
    36, 82, 97, 67, 112, 21, 52, 98, 37, 83, 113, 6, 68, 128, 53, 22, 99, 114, 84, 7, 129, 38, 69,
    100, 115, 144, 130, 85, 54, 23, 8, 145, 39, 70, 116, 101, 131, 160, 146, 55, 86, 24, 71, 132,
    117, 161, 40, 9, 102, 147, 176, 162, 87, 56, 25, 133, 118, 177, 148, 72, 103, 41, 163, 10, 192,
    178, 88, 57, 134, 149, 119, 26, 164, 73, 104, 193, 42, 179, 208, 11, 135, 89, 165, 120, 150,
    58, 194, 180, 27, 74, 209, 105, 151, 136, 43, 90, 224, 166, 195, 181, 121, 210, 59, 12, 152,
    106, 167, 196, 75, 137, 225, 211, 240, 182, 122, 91, 28, 197, 13, 226, 168, 183, 153, 44, 212,
    138, 107, 241, 60, 29, 123, 198, 184, 227, 169, 242, 76, 213, 154, 45, 92, 14, 199, 139, 61,
    228, 214, 170, 185, 243, 108, 77, 155, 30, 15, 200, 229, 124, 215, 244, 93, 46, 186, 171, 201,
    109, 140, 230, 62, 216, 245, 31, 125, 78, 156, 231, 47, 187, 202, 217, 94, 246, 141, 63, 232,
    172, 110, 247, 157, 79, 218, 203, 126, 233, 188, 248, 95, 173, 142, 219, 111, 249, 234, 158,
    127, 189, 204, 250, 235, 143, 174, 220, 205, 159, 251, 190, 221, 175, 236, 237, 191, 206, 252,
    222, 253, 207, 238, 223, 254, 239, 255,
];
pub(crate) const COL_SCAN_16X16: [u16; 256] = [
    0, 16, 32, 48, 1, 64, 17, 80, 33, 96, 49, 2, 65, 112, 18, 81, 34, 128, 50, 97, 3, 66, 144, 19,
    113, 35, 82, 160, 98, 51, 129, 4, 67, 176, 20, 114, 145, 83, 36, 99, 130, 52, 192, 5, 161, 68,
    115, 21, 146, 84, 208, 177, 37, 131, 100, 53, 162, 224, 69, 6, 116, 193, 147, 85, 22, 240, 132,
    38, 178, 101, 163, 54, 209, 117, 70, 7, 148, 194, 86, 179, 225, 23, 133, 39, 164, 8, 102, 210,
    241, 55, 195, 118, 149, 71, 180, 24, 87, 226, 134, 165, 211, 40, 103, 56, 72, 150, 196, 242,
    119, 9, 181, 227, 88, 166, 25, 135, 41, 104, 212, 57, 151, 197, 120, 73, 243, 182, 136, 167,
    213, 89, 10, 228, 105, 152, 198, 26, 42, 121, 183, 244, 168, 58, 137, 229, 74, 214, 90, 153,
    199, 184, 11, 106, 245, 27, 122, 230, 169, 43, 215, 59, 200, 138, 185, 246, 75, 12, 91, 154,
    216, 231, 107, 28, 44, 201, 123, 170, 60, 247, 232, 76, 139, 13, 92, 217, 186, 248, 155, 108,
    29, 124, 45, 202, 233, 171, 61, 14, 77, 140, 15, 249, 93, 30, 187, 156, 218, 46, 109, 125, 62,
    172, 78, 203, 31, 141, 234, 94, 47, 188, 63, 157, 110, 250, 219, 79, 126, 204, 173, 142, 95,
    189, 111, 235, 158, 220, 251, 127, 174, 143, 205, 236, 159, 190, 221, 252, 175, 206, 237, 191,
    253, 222, 238, 207, 254, 223, 239, 255,
];
pub(crate) const ROW_SCAN_16X16: [u16; 256] = [
    0, 1, 2, 16, 3, 17, 4, 18, 32, 5, 33, 19, 6, 34, 48, 20, 49, 7, 35, 21, 50, 64, 8, 36, 65, 22,
    51, 37, 80, 9, 66, 52, 23, 38, 81, 67, 10, 53, 24, 82, 68, 96, 39, 11, 54, 83, 97, 69, 25, 98,
    84, 40, 112, 55, 12, 70, 99, 113, 85, 26, 41, 56, 114, 100, 13, 71, 128, 86, 27, 115, 101, 129,
    42, 57, 72, 116, 14, 87, 130, 102, 144, 73, 131, 117, 28, 58, 15, 88, 43, 145, 103, 132, 146,
    118, 74, 160, 89, 133, 104, 29, 59, 147, 119, 44, 161, 148, 90, 105, 134, 162, 120, 176, 75,
    135, 149, 30, 60, 163, 177, 45, 121, 91, 106, 164, 178, 150, 192, 136, 165, 179, 31, 151, 193,
    76, 122, 61, 137, 194, 107, 152, 180, 208, 46, 166, 167, 195, 92, 181, 138, 209, 123, 153, 224,
    196, 77, 168, 210, 182, 240, 108, 197, 62, 154, 225, 183, 169, 211, 47, 139, 93, 184, 226, 212,
    241, 198, 170, 124, 155, 199, 78, 213, 185, 109, 227, 200, 63, 228, 242, 140, 214, 171, 186,
    156, 229, 243, 125, 94, 201, 244, 215, 216, 230, 141, 187, 202, 79, 172, 110, 157, 245, 217,
    231, 95, 246, 232, 126, 203, 247, 233, 173, 218, 142, 111, 158, 188, 248, 127, 234, 219, 249,
    189, 204, 143, 174, 159, 250, 235, 205, 220, 175, 190, 251, 221, 191, 206, 236, 207, 237, 252,
    222, 253, 223, 238, 239, 254, 255,
];

pub(crate) const DEFAULT_SCAN_32X32: [u16; 1024] = [
    0, 32, 1, 64, 33, 2, 96, 65, 34, 128, 3, 97, 66, 160, 129, 35, 98, 4, 67, 130, 161, 192, 36,
    99, 224, 5, 162, 193, 68, 131, 37, 100, 225, 194, 256, 163, 69, 132, 6, 226, 257, 288, 195,
    101, 164, 38, 258, 7, 227, 289, 133, 320, 70, 196, 165, 290, 259, 228, 39, 321, 102, 352, 8,
    197, 71, 134, 322, 291, 260, 353, 384, 229, 166, 103, 40, 354, 323, 292, 135, 385, 198, 261,
    72, 9, 416, 167, 386, 355, 230, 324, 104, 293, 41, 417, 199, 136, 262, 387, 448, 325, 356, 10,
    73, 418, 231, 168, 449, 294, 388, 105, 419, 263, 42, 200, 357, 450, 137, 480, 74, 326, 232, 11,
    389, 169, 295, 420, 106, 451, 481, 358, 264, 327, 201, 43, 138, 512, 482, 390, 296, 233, 170,
    421, 75, 452, 359, 12, 513, 265, 483, 328, 107, 202, 514, 544, 422, 391, 453, 139, 44, 234,
    484, 297, 360, 171, 76, 515, 545, 266, 329, 454, 13, 423, 203, 108, 546, 485, 576, 298, 235,
    140, 361, 330, 172, 547, 45, 455, 267, 577, 486, 77, 204, 362, 608, 14, 299, 578, 109, 236,
    487, 609, 331, 141, 579, 46, 15, 173, 610, 363, 78, 205, 16, 110, 237, 611, 142, 47, 174, 79,
    206, 17, 111, 238, 48, 143, 80, 175, 112, 207, 49, 18, 239, 81, 113, 19, 50, 82, 114, 51, 83,
    115, 640, 516, 392, 268, 144, 20, 672, 641, 548, 517, 424, 393, 300, 269, 176, 145, 52, 21,
    704, 673, 642, 580, 549, 518, 456, 425, 394, 332, 301, 270, 208, 177, 146, 84, 53, 22, 736,
    705, 674, 643, 612, 581, 550, 519, 488, 457, 426, 395, 364, 333, 302, 271, 240, 209, 178, 147,
    116, 85, 54, 23, 737, 706, 675, 613, 582, 551, 489, 458, 427, 365, 334, 303, 241, 210, 179,
    117, 86, 55, 738, 707, 614, 583, 490, 459, 366, 335, 242, 211, 118, 87, 739, 615, 491, 367,
    243, 119, 768, 644, 520, 396, 272, 148, 24, 800, 769, 676, 645, 552, 521, 428, 397, 304, 273,
    180, 149, 56, 25, 832, 801, 770, 708, 677, 646, 584, 553, 522, 460, 429, 398, 336, 305, 274,
    212, 181, 150, 88, 57, 26, 864, 833, 802, 771, 740, 709, 678, 647, 616, 585, 554, 523, 492,
    461, 430, 399, 368, 337, 306, 275, 244, 213, 182, 151, 120, 89, 58, 27, 865, 834, 803, 741,
    710, 679, 617, 586, 555, 493, 462, 431, 369, 338, 307, 245, 214, 183, 121, 90, 59, 866, 835,
    742, 711, 618, 587, 494, 463, 370, 339, 246, 215, 122, 91, 867, 743, 619, 495, 371, 247, 123,
    896, 772, 648, 524, 400, 276, 152, 28, 928, 897, 804, 773, 680, 649, 556, 525, 432, 401, 308,
    277, 184, 153, 60, 29, 960, 929, 898, 836, 805, 774, 712, 681, 650, 588, 557, 526, 464, 433,
    402, 340, 309, 278, 216, 185, 154, 92, 61, 30, 992, 961, 930, 899, 868, 837, 806, 775, 744,
    713, 682, 651, 620, 589, 558, 527, 496, 465, 434, 403, 372, 341, 310, 279, 248, 217, 186, 155,
    124, 93, 62, 31, 993, 962, 931, 869, 838, 807, 745, 714, 683, 621, 590, 559, 497, 466, 435,
    373, 342, 311, 249, 218, 187, 125, 94, 63, 994, 963, 870, 839, 746, 715, 622, 591, 498, 467,
    374, 343, 250, 219, 126, 95, 995, 871, 747, 623, 499, 375, 251, 127, 900, 776, 652, 528, 404,
    280, 156, 932, 901, 808, 777, 684, 653, 560, 529, 436, 405, 312, 281, 188, 157, 964, 933, 902,
    840, 809, 778, 716, 685, 654, 592, 561, 530, 468, 437, 406, 344, 313, 282, 220, 189, 158, 996,
    965, 934, 903, 872, 841, 810, 779, 748, 717, 686, 655, 624, 593, 562, 531, 500, 469, 438, 407,
    376, 345, 314, 283, 252, 221, 190, 159, 997, 966, 935, 873, 842, 811, 749, 718, 687, 625, 594,
    563, 501, 470, 439, 377, 346, 315, 253, 222, 191, 998, 967, 874, 843, 750, 719, 626, 595, 502,
    471, 378, 347, 254, 223, 999, 875, 751, 627, 503, 379, 255, 904, 780, 656, 532, 408, 284, 936,
    905, 812, 781, 688, 657, 564, 533, 440, 409, 316, 285, 968, 937, 906, 844, 813, 782, 720, 689,
    658, 596, 565, 534, 472, 441, 410, 348, 317, 286, 1000, 969, 938, 907, 876, 845, 814, 783, 752,
    721, 690, 659, 628, 597, 566, 535, 504, 473, 442, 411, 380, 349, 318, 287, 1001, 970, 939, 877,
    846, 815, 753, 722, 691, 629, 598, 567, 505, 474, 443, 381, 350, 319, 1002, 971, 878, 847, 754,
    723, 630, 599, 506, 475, 382, 351, 1003, 879, 755, 631, 507, 383, 908, 784, 660, 536, 412, 940,
    909, 816, 785, 692, 661, 568, 537, 444, 413, 972, 941, 910, 848, 817, 786, 724, 693, 662, 600,
    569, 538, 476, 445, 414, 1004, 973, 942, 911, 880, 849, 818, 787, 756, 725, 694, 663, 632, 601,
    570, 539, 508, 477, 446, 415, 1005, 974, 943, 881, 850, 819, 757, 726, 695, 633, 602, 571, 509,
    478, 447, 1006, 975, 882, 851, 758, 727, 634, 603, 510, 479, 1007, 883, 759, 635, 511, 912,
    788, 664, 540, 944, 913, 820, 789, 696, 665, 572, 541, 976, 945, 914, 852, 821, 790, 728, 697,
    666, 604, 573, 542, 1008, 977, 946, 915, 884, 853, 822, 791, 760, 729, 698, 667, 636, 605, 574,
    543, 1009, 978, 947, 885, 854, 823, 761, 730, 699, 637, 606, 575, 1010, 979, 886, 855, 762,
    731, 638, 607, 1011, 887, 763, 639, 916, 792, 668, 948, 917, 824, 793, 700, 669, 980, 949, 918,
    856, 825, 794, 732, 701, 670, 1012, 981, 950, 919, 888, 857, 826, 795, 764, 733, 702, 671,
    1013, 982, 951, 889, 858, 827, 765, 734, 703, 1014, 983, 890, 859, 766, 735, 1015, 891, 767,
    920, 796, 952, 921, 828, 797, 984, 953, 922, 860, 829, 798, 1016, 985, 954, 923, 892, 861, 830,
    799, 1017, 986, 955, 893, 862, 831, 1018, 987, 894, 863, 1019, 895, 924, 956, 925, 988, 957,
    926, 1020, 989, 958, 927, 1021, 990, 959, 1022, 991, 1023,
];

/// Quantization lookups, libvpx `vp9_quant_common.c`
/// (`dc_qlookup`, `ac_qlookup`, 8-bit).
pub(crate) const DC_QLOOKUP: [i32; 256] = [
    4, 8, 8, 9, 10, 11, 12, 12, 13, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 23, 24, 25, 26, 26, 27,
    28, 29, 30, 31, 32, 32, 33, 34, 35, 36, 37, 38, 38, 39, 40, 41, 42, 43, 43, 44, 45, 46, 47, 48,
    48, 49, 50, 51, 52, 53, 53, 54, 55, 56, 57, 57, 58, 59, 60, 61, 62, 62, 63, 64, 65, 66, 66, 67,
    68, 69, 70, 70, 71, 72, 73, 74, 74, 75, 76, 77, 78, 78, 79, 80, 81, 81, 82, 83, 84, 85, 85, 87,
    88, 90, 92, 93, 95, 96, 98, 99, 101, 102, 104, 105, 107, 108, 110, 111, 113, 114, 116, 117,
    118, 120, 121, 123, 125, 127, 129, 131, 134, 136, 138, 140, 142, 144, 146, 148, 150, 152, 154,
    156, 158, 161, 164, 166, 169, 172, 174, 177, 180, 182, 185, 187, 190, 192, 195, 199, 202, 205,
    208, 211, 214, 217, 220, 223, 226, 230, 233, 237, 240, 243, 247, 250, 253, 257, 261, 265, 269,
    272, 276, 280, 284, 288, 292, 296, 300, 304, 309, 313, 317, 322, 326, 330, 335, 340, 344, 349,
    354, 359, 364, 369, 374, 379, 384, 389, 395, 400, 406, 411, 417, 423, 429, 435, 441, 447, 454,
    461, 467, 475, 482, 489, 497, 505, 513, 522, 530, 539, 549, 559, 569, 579, 590, 602, 614, 626,
    640, 654, 668, 684, 700, 717, 736, 755, 775, 796, 819, 843, 869, 896, 925, 955, 988, 1022,
    1058, 1098, 1139, 1184, 1232, 1282, 1336,
];
pub(crate) const AC_QLOOKUP: [i32; 256] = [
    4, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30,
    31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54,
    55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78,
    79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101,
    102, 104, 106, 108, 110, 112, 114, 116, 118, 120, 122, 124, 126, 128, 130, 132, 134, 136, 138,
    140, 142, 144, 146, 148, 150, 152, 155, 158, 161, 164, 167, 170, 173, 176, 179, 182, 185, 188,
    191, 194, 197, 200, 203, 207, 211, 215, 219, 223, 227, 231, 235, 239, 243, 247, 251, 255, 260,
    265, 270, 275, 280, 285, 290, 295, 300, 305, 311, 317, 323, 329, 335, 341, 347, 353, 359, 366,
    373, 380, 387, 394, 401, 408, 416, 424, 432, 440, 448, 456, 465, 474, 483, 492, 501, 510, 520,
    530, 540, 550, 560, 571, 582, 593, 604, 615, 627, 639, 651, 663, 676, 689, 702, 715, 729, 743,
    757, 771, 786, 801, 816, 832, 848, 864, 881, 898, 915, 933, 951, 969, 988, 1007, 1026, 1046,
    1066, 1087, 1108, 1129, 1151, 1173, 1196, 1219, 1243, 1267, 1292, 1317, 1343, 1369, 1396, 1423,
    1451, 1479, 1508, 1537, 1567, 1597, 1628, 1660, 1692, 1725, 1759, 1793, 1828,
];

// ---------------------------------------------------------------------------
// Frame probability context (spec sections 6.3, 8.3-8.4)
// ---------------------------------------------------------------------------

/// Reference mode values, spec section 6.3.12.
pub(crate) const REF_MODE_SINGLE: u8 = 0;
pub(crate) const REF_MODE_COMPOUND: u8 = 1;
pub(crate) const REF_MODE_SELECT: u8 = 2;

/// Coefficient probabilities for one transform size:
/// [plane Y/UV][reference intra/inter][band 0..5][context][node].
/// (Band 0 has only 3 live contexts; entries 3..5 are padding.)
pub(crate) type Vp9CoefProbs = [[[[[u8; 3]; 6]; 6]; 2]; 2];

/// Frame probability context: every probability the tile decoder
/// reads. A working context starts as a clone of a saved context
/// (spec section 6.1), the compressed header updates it in place,
/// tile decoding counts syntax elements, and probability adaptation
/// (section 8.4) merges the *saved* context with the counts back
/// into the working context.
#[derive(Clone)]
pub(crate) struct Vp9FrameContext {
    /// Transform-size probabilities, spec section 6.3.2:
    /// [max tx size context][node].
    pub tx_probs_8x8: [[u8; 1]; 2],
    pub tx_probs_16x16: [[u8; 2]; 2],
    pub tx_probs_32x32: [[u8; 3]; 2],
    /// Coefficient probabilities, spec section 6.3.7:
    /// [tx size][plane 0 = Y, 1 = UV][ref 0 = intra, 1 = inter]
    /// [band][context][node].
    pub coef_probs: [Vp9CoefProbs; 4],
    /// Skip probabilities, spec section 6.3.8: [context].
    pub skip_prob: [u8; 3],
    /// Inter-mode probabilities, spec section 6.3.9:
    /// [context][node].
    pub inter_mode_probs: [[u8; 3]; 7],
    /// Interpolation-filter probabilities, spec section 6.3.10:
    /// [context][node].
    pub interp_filter_probs: [[u8; 2]; 4],
    /// Is-inter probabilities, spec section 6.3.11: [context].
    pub is_inter_prob: [u8; 4],
    /// Compound-mode probabilities, spec section 6.3.13: [context].
    pub comp_mode_prob: [u8; 5],
    /// Compound-reference probabilities, spec section 6.3.13: [context].
    pub comp_ref_prob: [u8; 5],
    /// Single-reference probabilities, spec section 6.3.13:
    /// [context][p1 / p2].
    pub single_ref_prob: [[u8; 2]; 5],
    /// Luma intra-mode probabilities, spec section 6.3.14:
    /// [size group][node].
    pub y_mode_probs: [[u8; 9]; 4],
    /// Chroma intra-mode probabilities, spec section 6.3.14:
    /// [luma mode][node].
    pub uv_mode_probs: [[u8; 9]; 10],
    /// Partition probabilities, spec section 6.3.15:
    /// [context][node].
    pub partition_probs: [[u8; 3]; 16],
    /// Motion-vector joint probabilities, spec section 6.3.16: [node].
    pub mv_joint_probs: [u8; 3],
    /// Motion-vector sign probabilities, spec section 6.3.16:
    /// [component 0 = row, 1 = column].
    pub mv_sign_prob: [u8; 2],
    /// Motion-vector class probabilities: [component][class].
    pub mv_class_probs: [[u8; 10]; 2],
    /// Motion-vector class-0 bit probabilities: [component].
    pub mv_class0_bit_prob: [u8; 2],
    /// Motion-vector bit probabilities: [component][bit index].
    pub mv_bits_prob: [[u8; 10]; 2],
    /// Motion-vector class-0 fractional probabilities:
    /// [component][class-0 bit][node].
    pub mv_class0_fr_probs: [[[u8; 3]; 2]; 2],
    /// Motion-vector fractional probabilities: [component][node].
    pub mv_fr_probs: [[u8; 3]; 2],
    /// Motion-vector class-0 high-precision probabilities: [component].
    pub mv_class0_hp_prob: [u8; 2],
    /// Motion-vector high-precision probabilities: [component].
    pub mv_hp_prob: [u8; 2],
    /// Transform mode for the frame, spec section 6.3.1, as a
    /// `TX_MODE_*` value.
    pub tx_mode: u8,
    /// Frame reference mode, spec section 6.3.12: 0 = single,
    /// 1 = compound, 2 = select.
    pub reference_mode: u8,
    /// Fixed compound reference frame, spec section 6.3.18; valid
    /// when the reference mode is compound or select.
    pub comp_fixed_ref: i8,
    /// Variable compound reference frames, spec section 6.3.18.
    pub comp_var_ref: [i8; 2],
}

impl Vp9FrameContext {
    /// Default context: every probability at its spec section 10 /
    /// libvpx default. Keyframe-only tables (`kf_partition_probs`,
    /// `kf_y_mode_probs`, `kf_uv_mode_probs`) are *not* stored here;
    /// the tile decoder consults them directly for intra frames.
    pub(crate) fn new_default() -> Self {
        let nmv = |comp: usize, index: usize| DEFAULT_NMV_COMPONENTS[comp][index];
        Self {
            tx_probs_8x8: DEFAULT_TX_PROBS_8X8,
            tx_probs_16x16: DEFAULT_TX_PROBS_16X16,
            tx_probs_32x32: DEFAULT_TX_PROBS_32X32,
            coef_probs: [
                DEFAULT_COEF_PROBS_4X4,
                DEFAULT_COEF_PROBS_8X8,
                DEFAULT_COEF_PROBS_16X16,
                DEFAULT_COEF_PROBS_32X32,
            ],
            skip_prob: DEFAULT_SKIP_PROBS,
            inter_mode_probs: DEFAULT_INTER_MODE_PROBS,
            interp_filter_probs: DEFAULT_SWITCHABLE_INTERP_PROB,
            is_inter_prob: DEFAULT_INTRA_INTER_P,
            comp_mode_prob: DEFAULT_COMP_INTER_P,
            comp_ref_prob: DEFAULT_COMP_REF_P,
            single_ref_prob: DEFAULT_SINGLE_REF_P,
            y_mode_probs: DEFAULT_IF_Y_PROBS,
            uv_mode_probs: DEFAULT_IF_UV_PROBS,
            partition_probs: DEFAULT_PARTITION_PROBS,
            mv_joint_probs: DEFAULT_NMV_JOINTS,
            mv_sign_prob: [nmv(0, 0), nmv(1, 0)],
            mv_class_probs: [nmv_array(0, 1), nmv_array(1, 1)],
            mv_class0_bit_prob: [nmv(0, 11), nmv(1, 11)],
            mv_bits_prob: [nmv_array(0, 12), nmv_array(1, 12)],
            mv_class0_fr_probs: [
                [nmv_array3(0, 22), nmv_array3(0, 25)],
                [nmv_array3(1, 22), nmv_array3(1, 25)],
            ],
            mv_fr_probs: [nmv_array3(0, 28), nmv_array3(1, 28)],
            mv_class0_hp_prob: [nmv(0, 31), nmv(1, 31)],
            mv_hp_prob: [nmv(0, 32), nmv(1, 32)],
            tx_mode: TX_MODE_SELECT,
            reference_mode: REF_MODE_SINGLE,
            comp_fixed_ref: ALTREF_FRAME,
            comp_var_ref: [LAST_FRAME, GOLDEN_FRAME],
        }
    }
}

/// Copies 10 consecutive entries out of a flat NMV component table
/// (class or bit probabilities).
fn nmv_array(comp: usize, start: usize) -> [u8; 10] {
    let mut out = [0u8; 10];
    out.copy_from_slice(&DEFAULT_NMV_COMPONENTS[comp][start..start + 10]);
    out
}

/// Copies 3 consecutive entries out of a flat NMV component table
/// (fractional probabilities).
fn nmv_array3(comp: usize, start: usize) -> [u8; 3] {
    let mut out = [0u8; 3];
    out.copy_from_slice(&DEFAULT_NMV_COMPONENTS[comp][start..start + 3]);
    out
}

// ---------------------------------------------------------------------------
// Syntax-element counts (spec sections 8.3, 9.3.4)
// ---------------------------------------------------------------------------

/// Per-[tx size][plane][ref][band][context] coefficient counts.
/// The token tree is adapted from `n0` (ZERO tokens), `n1` (ONE
/// tokens) and `n2` (tokens of TWO or more); the more-coefficients
/// bit is adapted from `neob` (EOB reads) out of `eob_total` reads.
#[derive(Clone, Copy, Default)]
pub(crate) struct Vp9CoefCounts {
    pub eob_total: u32,
    pub neob: u32,
    pub n0: u32,
    pub n1: u32,
    pub n2: u32,
}

/// Syntax-element counts collected during tile decoding, spec
/// section 9.3.4. Indexed exactly as the spec's `counts_*` arrays;
/// `intra_mode` covers both `intra_mode` (context = size group)
/// and `sub_intra_mode` (context = 0).
#[derive(Clone, Default)]
pub(crate) struct Vp9Counts {
    /// [context][partition value].
    pub partition: [[u32; 4]; 16],
    /// [size-group context][mode].
    pub intra_mode: [[u32; 10]; 4],
    /// [luma mode][chroma mode].
    pub uv_mode: [[u32; 10]; 10],
    /// [context][skip].
    pub skip: [[u32; 2]; 3],
    /// [context][is_inter].
    pub is_inter: [[u32; 2]; 4],
    /// [context][compound mode].
    pub comp_mode: [[u32; 2]; 5],
    /// [context][compound reference].
    pub comp_ref: [[u32; 2]; 5],
    /// [context][p1 / p2][value].
    pub single_ref: [[[u32; 2]; 2]; 5],
    /// [component][sign].
    pub mv_sign: [[u32; 2]; 2],
    /// [component][class-0 bit].
    pub mv_class0_bit: [[u32; 2]; 2],
    /// [component][bit index][bit].
    pub mv_bits: [[[u32; 2]; 10]; 2],
    /// [max tx size - 1][context][tx size value].
    pub tx_size: [[[u32; 4]; 2]; 3],
    /// [context][inter mode - NEARESTMV].
    pub inter_mode: [[u32; 4]; 7],
    /// [context][switchable filter].
    pub interp_filter: [[u32; 3]; 4],
    /// [joint value].
    pub mv_joint: [u32; 4],
    /// [component][class].
    pub mv_class: [[u32; 11]; 2],
    /// [component][class-0 bit][fractional value].
    pub mv_class0_fr: [[[u32; 4]; 2]; 2],
    /// [component][high-precision bit].
    pub mv_class0_hp: [[u32; 2]; 2],
    /// [component][fractional value].
    pub mv_fr: [[u32; 4]; 2],
    /// [component][high-precision bit].
    pub mv_hp: [[u32; 2]; 2],
    /// [tx size][plane][ref][band][context].
    pub coef: [[[[[Vp9CoefCounts; 6]; 6]; 2]; 2]; 4],
}

// ---------------------------------------------------------------------------
// Probability adaptation (spec section 8.4)
// ---------------------------------------------------------------------------

/// Merges one probability with the observed counts, spec section
/// 8.4.1: the count-derived probability is blended into the
/// pre-existing probability with a factor capped by `count_sat`
/// and `max_update_factor`.
fn merge_prob(pre_prob: u8, ct0: u32, ct1: u32, count_sat: u32, max_update_factor: u32) -> u8 {
    let den = ct0 + ct1;
    // checked_div returns None exactly when den == 0, which is
    // the spec's "no counts" case (probability 128).
    let prob = (ct0 * 256 + (den >> 1)).checked_div(den).unwrap_or(128);
    let prob = prob.clamp(1, 255);
    let count = (ct0 + ct1).min(count_sat);
    let factor = max_update_factor * count / count_sat;
    let out = (u32::from(pre_prob) * (256 - factor) + prob * factor + 128) >> 8;
    out as u8
}

/// Merges a whole probability tree with the observed value counts,
/// spec section 8.4.1. `counts` is indexed by leaf value; the
/// branch counts are derived by walking the tree. The pre-existing
/// probabilities are read from `pre_probs` and the merged values
/// are written to `probs`; the two must be distinct arrays, because
/// adaptation merges the *saved* context, not the working context
/// (which already carries the compressed-header updates).
/// Returns the total count under this node.
fn merge_probs(
    tree: &[i8],
    i: usize,
    pre_probs: &[u8],
    probs: &mut [u8],
    counts: &[u32],
    count_sat: u32,
    max_update_factor: u32,
) -> u32 {
    let s = tree[i];
    let left_count = if s <= 0 {
        counts[(-i32::from(s)) as usize]
    } else {
        merge_probs(
            tree,
            s as usize,
            pre_probs,
            probs,
            counts,
            count_sat,
            max_update_factor,
        )
    };
    let r = tree[i + 1];
    let right_count = if r <= 0 {
        counts[(-i32::from(r)) as usize]
    } else {
        merge_probs(
            tree,
            r as usize,
            pre_probs,
            probs,
            counts,
            count_sat,
            max_update_factor,
        )
    };
    probs[i >> 1] = merge_prob(
        pre_probs[i >> 1],
        left_count,
        right_count,
        count_sat,
        max_update_factor,
    );
    left_count + right_count
}

/// Adapts one binary probability, spec section 8.4.2. The merge
/// base is the *saved* probability `pre_prob`.
fn adapt_prob(pre_prob: u8, prob: &mut u8, counts: [u32; 2]) {
    *prob = merge_prob(pre_prob, counts[0], counts[1], COUNT_SAT, MAX_UPDATE_FACTOR);
}

/// Adapts a whole probability tree, spec section 8.4.2.
fn adapt_probs(tree: &[i8], pre_probs: &[u8], probs: &mut [u8], counts: &[u32]) {
    merge_probs(
        tree,
        0,
        pre_probs,
        probs,
        counts,
        COUNT_SAT,
        MAX_UPDATE_FACTOR,
    );
}

impl Vp9FrameContext {
    /// Adapts the coefficient probabilities, spec section 8.4.1.
    /// The merge base is the *saved* context (`pre`), and the
    /// result is written into `self` (the working context); this
    /// is why header prob updates are overwritten by adaptation
    /// for every adapted probability. `frame_is_intra` selects the
    /// update factor: 112 for an intra frame, 128 when the previous
    /// frame was a key frame, 112 otherwise.
    pub(crate) fn adapt_coef_probs(
        &mut self,
        pre: &Vp9FrameContext,
        counts: &Vp9Counts,
        frame_is_intra: bool,
        last_frame_was_key: bool,
    ) {
        let update_factor = if frame_is_intra {
            112
        } else if last_frame_was_key {
            128
        } else {
            112
        };
        for t in 0..4 {
            for plane in 0..2 {
                for reference in 0..2 {
                    for band in 0..6 {
                        let max_ctx = if band == 0 { 3 } else { 6 };
                        for ctx in 0..max_ctx {
                            let c = counts.coef[t][plane][reference][band][ctx];
                            let token_counts = [c.n0, c.n1, c.n2];
                            merge_probs(
                                &SMALL_TOKEN_TREE,
                                2,
                                &pre.coef_probs[t][plane][reference][band][ctx],
                                &mut self.coef_probs[t][plane][reference][band][ctx],
                                &token_counts,
                                COEF_COUNT_SAT,
                                update_factor,
                            );
                            let more_counts = [c.neob, c.eob_total - c.neob];
                            merge_probs(
                                &BINARY_TREE,
                                0,
                                &pre.coef_probs[t][plane][reference][band][ctx],
                                &mut self.coef_probs[t][plane][reference][band][ctx],
                                &more_counts,
                                COEF_COUNT_SAT,
                                update_factor,
                            );
                        }
                    }
                }
            }
        }
    }

    /// Adapts the non-coefficient probabilities, spec section 8.4.2.
    /// The merge base is the *saved* context (`pre`). Only called
    /// for non-intra-only frames; the caller gates on that.
    // Long because it walks every probability array in spec order;
    // splitting it would obscure the one-to-one mapping to 8.4.2.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn adapt_noncoef_probs(
        &mut self,
        pre: &Vp9FrameContext,
        counts: &Vp9Counts,
        header: &Vp9FrameHeader,
    ) {
        for ctx in 0..16 {
            adapt_probs(
                &PARTITION_TREE,
                &pre.partition_probs[ctx],
                &mut self.partition_probs[ctx],
                &counts.partition[ctx],
            );
        }
        for ctx in 0..4 {
            adapt_probs(
                &INTRA_MODE_TREE,
                &pre.y_mode_probs[ctx],
                &mut self.y_mode_probs[ctx],
                &counts.intra_mode[ctx],
            );
        }
        for mode in 0..10 {
            adapt_probs(
                &INTRA_MODE_TREE,
                &pre.uv_mode_probs[mode],
                &mut self.uv_mode_probs[mode],
                &counts.uv_mode[mode],
            );
        }
        for ctx in 0..3 {
            adapt_prob(
                pre.skip_prob[ctx],
                &mut self.skip_prob[ctx],
                counts.skip[ctx],
            );
        }
        for ctx in 0..4 {
            adapt_prob(
                pre.is_inter_prob[ctx],
                &mut self.is_inter_prob[ctx],
                counts.is_inter[ctx],
            );
        }
        for ctx in 0..5 {
            adapt_prob(
                pre.comp_mode_prob[ctx],
                &mut self.comp_mode_prob[ctx],
                counts.comp_mode[ctx],
            );
            adapt_prob(
                pre.comp_ref_prob[ctx],
                &mut self.comp_ref_prob[ctx],
                counts.comp_ref[ctx],
            );
            adapt_prob(
                pre.single_ref_prob[ctx][0],
                &mut self.single_ref_prob[ctx][0],
                counts.single_ref[ctx][0],
            );
            adapt_prob(
                pre.single_ref_prob[ctx][1],
                &mut self.single_ref_prob[ctx][1],
                counts.single_ref[ctx][1],
            );
        }
        for ctx in 0..7 {
            adapt_probs(
                &INTER_MODE_TREE,
                &pre.inter_mode_probs[ctx],
                &mut self.inter_mode_probs[ctx],
                &counts.inter_mode[ctx],
            );
        }
        if header.interpolation_filter == Vp9InterpolationFilter::Switchable {
            for ctx in 0..4 {
                adapt_probs(
                    &INTERP_FILTER_TREE,
                    &pre.interp_filter_probs[ctx],
                    &mut self.interp_filter_probs[ctx],
                    &counts.interp_filter[ctx],
                );
            }
        }
        if self.tx_mode == TX_MODE_SELECT {
            // The tx-size trees are indexed by max tx size: the 8x8
            // tree for max 1, the 16x16 tree for max 2, the 32x32
            // tree for max 3.
            for ctx in 0..2 {
                merge_probs(
                    &TX_SIZE_8_TREE,
                    0,
                    &pre.tx_probs_8x8[ctx],
                    &mut self.tx_probs_8x8[ctx],
                    &counts.tx_size[0][ctx],
                    COUNT_SAT,
                    MAX_UPDATE_FACTOR,
                );
                merge_probs(
                    &TX_SIZE_16_TREE,
                    0,
                    &pre.tx_probs_16x16[ctx],
                    &mut self.tx_probs_16x16[ctx],
                    &counts.tx_size[1][ctx],
                    COUNT_SAT,
                    MAX_UPDATE_FACTOR,
                );
                merge_probs(
                    &TX_SIZE_32_TREE,
                    0,
                    &pre.tx_probs_32x32[ctx],
                    &mut self.tx_probs_32x32[ctx],
                    &counts.tx_size[2][ctx],
                    COUNT_SAT,
                    MAX_UPDATE_FACTOR,
                );
            }
        }
        adapt_probs(
            &MV_JOINT_TREE,
            &pre.mv_joint_probs,
            &mut self.mv_joint_probs,
            &counts.mv_joint,
        );
        for comp in 0..2 {
            adapt_prob(
                pre.mv_sign_prob[comp],
                &mut self.mv_sign_prob[comp],
                counts.mv_sign[comp],
            );
            adapt_probs(
                &MV_CLASS_TREE,
                &pre.mv_class_probs[comp],
                &mut self.mv_class_probs[comp],
                &counts.mv_class[comp],
            );
            adapt_prob(
                pre.mv_class0_bit_prob[comp],
                &mut self.mv_class0_bit_prob[comp],
                counts.mv_class0_bit[comp],
            );
            for bit in 0..10 {
                adapt_prob(
                    pre.mv_bits_prob[comp][bit],
                    &mut self.mv_bits_prob[comp][bit],
                    counts.mv_bits[comp][bit],
                );
            }
            for class0_bit in 0..2 {
                adapt_probs(
                    &MV_FR_TREE,
                    &pre.mv_class0_fr_probs[comp][class0_bit],
                    &mut self.mv_class0_fr_probs[comp][class0_bit],
                    &counts.mv_class0_fr[comp][class0_bit],
                );
            }
            adapt_probs(
                &MV_FR_TREE,
                &pre.mv_fr_probs[comp],
                &mut self.mv_fr_probs[comp],
                &counts.mv_fr[comp],
            );
            if header.allow_high_precision_mv {
                adapt_prob(
                    pre.mv_class0_hp_prob[comp],
                    &mut self.mv_class0_hp_prob[comp],
                    counts.mv_class0_hp[comp],
                );
                adapt_prob(
                    pre.mv_hp_prob[comp],
                    &mut self.mv_hp_prob[comp],
                    counts.mv_hp[comp],
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Compressed-header re-parse with value tracking (spec section 6.3)
// ---------------------------------------------------------------------------

/// Inverse recentering, spec section 6.3.5.
fn inv_recenter_nonneg(v: u32, m: u32) -> u32 {
    if v > 2 * m {
        v
    } else if v % 2 == 1 {
        m - ((v + 1) >> 1)
    } else {
        m + (v >> 1)
    }
}

/// Inverse probability remap, spec section 6.3.5.
fn inv_remap_prob(delta_prob: u32, prob: u8) -> u8 {
    let mut m = u32::from(prob);
    let v = u32::from(INV_MAP_TABLE[delta_prob as usize]);
    m -= 1;
    if (m << 1) <= 255 {
        m = 1 + inv_recenter_nonneg(v, m);
    } else {
        m = 255 - inv_recenter_nonneg(v, 255 - 1 - m);
    }
    m as u8
}

/// Truncated exponential-Golomb decode, spec section 6.3.4 (the
/// parser / libvpx form; the `v < 65` branch takes no extra bit).
fn decode_term_subexp(dec: &mut Vp9BoolDecoder) -> Result<u32> {
    if !dec.read_bool(128)? {
        return dec.read_literal(4);
    }
    if !dec.read_bool(128)? {
        return Ok(dec.read_literal(4)? + 16);
    }
    if !dec.read_bool(128)? {
        return Ok(dec.read_literal(5)? + 32);
    }
    let v = dec.read_literal(7)?;
    if v < 65 {
        return Ok(v + 64);
    }
    let bit = dec.read_literal(1)?;
    Ok((v << 1) - 1 + bit)
}

/// Probability update, spec section 6.3.3.
fn diff_update_prob(dec: &mut Vp9BoolDecoder, prob: u8) -> Result<u8> {
    if dec.read_bool(252)? {
        let delta = decode_term_subexp(dec)?;
        Ok(inv_remap_prob(delta, prob))
    } else {
        Ok(prob)
    }
}

/// Motion-vector probability update, spec section 6.3.17.
fn update_mv_prob(dec: &mut Vp9BoolDecoder, prob: u8) -> Result<u8> {
    if dec.read_bool(252)? {
        let mv_prob = dec.read_literal(7)?;
        Ok(((mv_prob << 1) | 1) as u8)
    } else {
        Ok(prob)
    }
}

fn tx_mode_value(tx_mode: Vp9TxMode) -> u8 {
    match tx_mode {
        Vp9TxMode::Only4x4 => TX_MODE_ONLY_4X4,
        Vp9TxMode::Allow8x8 => TX_MODE_ALLOW_8X8,
        Vp9TxMode::Allow16x16 => TX_MODE_ALLOW_16X16,
        Vp9TxMode::Allow32x32 => TX_MODE_ALLOW_32X32,
        Vp9TxMode::Select => TX_MODE_SELECT,
    }
}

fn reference_mode_value(mode: Vp9ReferenceMode) -> u8 {
    match mode {
        Vp9ReferenceMode::Single => REF_MODE_SINGLE,
        Vp9ReferenceMode::Compound => REF_MODE_COMPOUND,
        Vp9ReferenceMode::Select => REF_MODE_SELECT,
    }
}

/// Compound reference setup, spec section 6.3.18. `sign_bias` is
/// in header order [LAST, GOLDEN, ALTREF].
pub(crate) fn setup_compound_reference_mode(sign_bias: [bool; 3]) -> (i8, [i8; 2]) {
    if sign_bias[0] == sign_bias[1] {
        (ALTREF_FRAME, [LAST_FRAME, GOLDEN_FRAME])
    } else if sign_bias[0] == sign_bias[2] {
        (GOLDEN_FRAME, [LAST_FRAME, ALTREF_FRAME])
    } else {
        (LAST_FRAME, [GOLDEN_FRAME, ALTREF_FRAME])
    }
}

/// Re-parses the compressed header (spec section 6.3) with value
/// tracking, starting from the saved context `base`. The parser
/// discards the updated probability *values*, so reconstruction
/// re-reads them here. `packet` is the full frame packet;
/// `header_bytes` is the frame's total header size (the
/// `header_bytes` field of `Vp9Frame`); the compressed header
/// occupies `header_bytes - header.header_size_in_bytes ..
/// header_bytes`.
///
/// # Errors
/// Returns `InvalidData` when the re-parse disagrees with the
/// parser (tx mode or reference mode mismatch, or the bool decoder
/// does not end exactly at `header_size_in_bytes`), and propagates
/// bool-decoder errors.
// Long because it re-reads the compressed header in the spec's
// section 6.3 order; splitting it would obscure the bit order.
// Each `read_literal(1)` below consumes one bit, so the two
// same-looking conditions in `frame_reference_mode` are sequential
// reads, not a repeated condition.
#[allow(clippy::too_many_lines, clippy::same_functions_in_if_condition)]
pub(crate) fn parse_compressed_header_probs(
    header: &Vp9FrameHeader,
    packet: &[u8],
    header_bytes: usize,
    base: &Vp9FrameContext,
) -> Result<Vp9FrameContext> {
    let compressed = header
        .compressed_header
        .as_ref()
        .ok_or(Error::InvalidData {
            offset: 0,
            detail: "vp9 frame has no compressed header",
        })?;
    let header_size = header.header_size_in_bytes as usize;
    let start = header_bytes
        .checked_sub(header_size)
        .ok_or(Error::InvalidData {
            offset: 0,
            detail: "vp9 compressed header size exceeds header bytes",
        })?;
    let bytes = packet.get(start..header_bytes).ok_or(Error::InvalidData {
        offset: start,
        detail: "vp9 compressed header extends past the packet",
    })?;
    let mut ctx = base.clone();
    let mut dec = Vp9BoolDecoder::new(bytes)?;

    // Section 6.3.1: read_tx_mode.
    let tx_mode = if header.lossless {
        TX_MODE_ONLY_4X4
    } else {
        let mut value = dec.read_literal(2)?;
        if value == 3 {
            value = 3 + dec.read_literal(1)?;
        }
        value as u8
    };
    if tx_mode != tx_mode_value(compressed.tx_mode) {
        return Err(Error::InvalidData {
            offset: start,
            detail: "vp9 re-parsed tx mode disagrees with the parser",
        });
    }
    ctx.tx_mode = tx_mode;

    // Section 6.3.2: tx_mode_probs (spec order, confirmed by libvpx).
    if tx_mode == TX_MODE_SELECT {
        for i in 0..2 {
            ctx.tx_probs_8x8[i][0] = diff_update_prob(&mut dec, ctx.tx_probs_8x8[i][0])?;
        }
        for i in 0..2 {
            for j in 0..2 {
                ctx.tx_probs_16x16[i][j] = diff_update_prob(&mut dec, ctx.tx_probs_16x16[i][j])?;
            }
        }
        for i in 0..2 {
            for j in 0..3 {
                ctx.tx_probs_32x32[i][j] = diff_update_prob(&mut dec, ctx.tx_probs_32x32[i][j])?;
            }
        }
    }

    // Section 6.3.7: read_coef_probs.
    let max_tx_size = TX_MODE_TO_BIGGEST[tx_mode as usize];
    for tx_sz in 0..=max_tx_size {
        if dec.read_literal(1)? == 1 {
            for i in 0..2 {
                for j in 0..2 {
                    for k in 0..6 {
                        let max_l = if k == 0 { 3 } else { 6 };
                        for l in 0..max_l {
                            for m in 0..3 {
                                let idx = tx_sz as usize;
                                ctx.coef_probs[idx][i][j][k][l][m] =
                                    diff_update_prob(&mut dec, ctx.coef_probs[idx][i][j][k][l][m])?;
                            }
                        }
                    }
                }
            }
        }
    }

    // Section 6.3.8: read_skip_prob.
    for i in 0..3 {
        ctx.skip_prob[i] = diff_update_prob(&mut dec, ctx.skip_prob[i])?;
    }

    // Sections 6.3.9-6.3.16 are only present for inter frames
    // (spec section 6.3; the parser gates them the same way).
    // For an intra frame the compressed header ends here.
    if !header.frame_is_intra {
        // Section 6.3.9: read_inter_mode_probs.
        for i in 0..7 {
            for j in 0..3 {
                ctx.inter_mode_probs[i][j] =
                    diff_update_prob(&mut dec, ctx.inter_mode_probs[i][j])?;
            }
        }

        // Section 6.3.10: read_interp_filter_probs.
        if header.interpolation_filter == Vp9InterpolationFilter::Switchable {
            for j in 0..4 {
                for i in 0..2 {
                    ctx.interp_filter_probs[j][i] =
                        diff_update_prob(&mut dec, ctx.interp_filter_probs[j][i])?;
                }
            }
        }

        // Section 6.3.11: read_is_inter_probs.
        for i in 0..4 {
            ctx.is_inter_prob[i] = diff_update_prob(&mut dec, ctx.is_inter_prob[i])?;
        }

        // Section 6.3.12: frame_reference_mode.
        let bias = header.ref_frame_sign_bias;
        let compound_allowed = bias[1] != bias[0] || bias[2] != bias[0];
        let reference_mode = if compound_allowed {
            if dec.read_literal(1)? == 0 {
                REF_MODE_SINGLE
            } else if dec.read_literal(1)? == 0 {
                REF_MODE_COMPOUND
            } else {
                REF_MODE_SELECT
            }
        } else {
            REF_MODE_SINGLE
        };
        if reference_mode != reference_mode_value(compressed.reference_mode) {
            return Err(Error::InvalidData {
                offset: start,
                detail: "vp9 re-parsed reference mode disagrees with the parser",
            });
        }
        ctx.reference_mode = reference_mode;
        if reference_mode == REF_MODE_COMPOUND || reference_mode == REF_MODE_SELECT {
            // Section 6.3.12 only calls the setup for SELECT, but
            // section 6.4.17 (read_ref_frames) needs CompFixedRef /
            // CompVarRef for COMPOUND too (comp_mode = reference_mode
            // there), so the setup runs for both; it depends only on
            // the sign biases, which are fixed for the frame.
            let (fixed, var) = setup_compound_reference_mode(bias);
            ctx.comp_fixed_ref = fixed;
            ctx.comp_var_ref = var;
        }

        // Section 6.3.13: frame_reference_mode_probs.
        if reference_mode == REF_MODE_SELECT {
            for i in 0..5 {
                ctx.comp_mode_prob[i] = diff_update_prob(&mut dec, ctx.comp_mode_prob[i])?;
            }
        }
        if reference_mode != REF_MODE_COMPOUND {
            for i in 0..5 {
                ctx.single_ref_prob[i][0] = diff_update_prob(&mut dec, ctx.single_ref_prob[i][0])?;
                ctx.single_ref_prob[i][1] = diff_update_prob(&mut dec, ctx.single_ref_prob[i][1])?;
            }
        }
        if reference_mode != REF_MODE_SINGLE {
            for i in 0..5 {
                ctx.comp_ref_prob[i] = diff_update_prob(&mut dec, ctx.comp_ref_prob[i])?;
            }
        }

        // Section 6.3.14: read_y_mode_probs.
        for i in 0..4 {
            for j in 0..9 {
                ctx.y_mode_probs[i][j] = diff_update_prob(&mut dec, ctx.y_mode_probs[i][j])?;
            }
        }

        // Section 6.3.15: read_partition_probs.
        for i in 0..16 {
            for j in 0..3 {
                ctx.partition_probs[i][j] = diff_update_prob(&mut dec, ctx.partition_probs[i][j])?;
            }
        }

        // Section 6.3.16: mv_probs (spec order, confirmed by libvpx
        // `read_mv_probs`).
        for j in 0..3 {
            ctx.mv_joint_probs[j] = update_mv_prob(&mut dec, ctx.mv_joint_probs[j])?;
        }
        for i in 0..2 {
            ctx.mv_sign_prob[i] = update_mv_prob(&mut dec, ctx.mv_sign_prob[i])?;
            for j in 0..10 {
                ctx.mv_class_probs[i][j] = update_mv_prob(&mut dec, ctx.mv_class_probs[i][j])?;
            }
            ctx.mv_class0_bit_prob[i] = update_mv_prob(&mut dec, ctx.mv_class0_bit_prob[i])?;
            for j in 0..10 {
                ctx.mv_bits_prob[i][j] = update_mv_prob(&mut dec, ctx.mv_bits_prob[i][j])?;
            }
        }
        for i in 0..2 {
            for j in 0..2 {
                for k in 0..3 {
                    ctx.mv_class0_fr_probs[i][j][k] =
                        update_mv_prob(&mut dec, ctx.mv_class0_fr_probs[i][j][k])?;
                }
            }
            for k in 0..3 {
                ctx.mv_fr_probs[i][k] = update_mv_prob(&mut dec, ctx.mv_fr_probs[i][k])?;
            }
        }
        if header.allow_high_precision_mv {
            for i in 0..2 {
                ctx.mv_class0_hp_prob[i] = update_mv_prob(&mut dec, ctx.mv_class0_hp_prob[i])?;
                ctx.mv_hp_prob[i] = update_mv_prob(&mut dec, ctx.mv_hp_prob[i])?;
            }
        }
    }

    // The bool decoder must end exactly at the end of the
    // compressed header; any mismatch means the re-parse order or
    // counts are wrong.
    dec.exit()?;
    if dec.bytes_consumed() != header_size {
        return Err(Error::InvalidData {
            offset: start,
            detail: "vp9 compressed header re-parse did not end at the header size",
        });
    }
    Ok(ctx)
}

// ---------------------------------------------------------------------------
// Tree decode, token / coefficient decode (spec sections 6.4.24-6.4.26, 9.3)
// ---------------------------------------------------------------------------

/// Tree decode, spec section 9.3.3: walk the tree with
/// `read_bool(P(n >> 1))` until a leaf (non-positive value) is
/// reached; the leaf value is `-n`.
pub(crate) fn read_tree(dec: &mut Vp9BoolDecoder, tree: &[i8], probs: &[u8]) -> Result<i8> {
    let mut node: i32 = 0;
    loop {
        let bit = dec.read_bool(u32::from(probs[(node >> 1) as usize]))?;
        node = i32::from(tree[(node + i32::from(bit)) as usize]);
        if node <= 0 {
            return Ok((-node) as i8);
        }
    }
}

/// Scan table for a transform block, spec section 6.4.25.
pub(crate) fn get_scan(tx_size: u8, tx_type: u8) -> &'static [u16] {
    match tx_size {
        TX_4X4 => match tx_type {
            ADST_DCT => &ROW_SCAN_4X4,
            DCT_ADST => &COL_SCAN_4X4,
            _ => &DEFAULT_SCAN_4X4,
        },
        TX_8X8 => match tx_type {
            ADST_DCT => &ROW_SCAN_8X8,
            DCT_ADST => &COL_SCAN_8X8,
            _ => &DEFAULT_SCAN_8X8,
        },
        TX_16X16 => match tx_type {
            ADST_DCT => &ROW_SCAN_16X16,
            DCT_ADST => &COL_SCAN_16X16,
            _ => &DEFAULT_SCAN_16X16,
        },
        _ => &DEFAULT_SCAN_32X32,
    }
}

/// Coefficient band for scan position `c`, spec section 6.4.24.
fn coef_band(c: usize, tx_size: u8) -> usize {
    if tx_size == TX_4X4 {
        COEFBAND_4X4[c] as usize
    } else {
        coefband_8x8plus(c) as usize
    }
}

/// The two neighbor positions whose token caches feed the
/// coefficient context, spec section 9.3.2. `pos` is the raster
/// position of the coefficient within the transform block; it is
/// never 0 here (scan position `c` is greater than 0).
fn coef_neighbors(pos: usize, tx_size: u8, tx_type: u8) -> [usize; 2] {
    let n = 4usize << tx_size;
    let i = pos / n;
    let j = pos % n;
    if i > 0 && j > 0 {
        let a = (i - 1) * n + j;
        let a2 = i * n + j - 1;
        match tx_type {
            DCT_ADST => [a, a],
            ADST_DCT => [a2, a2],
            _ => [a, a2],
        }
    } else if i > 0 {
        let a = (i - 1) * n + j;
        [a, a]
    } else {
        let a2 = i * n + j - 1;
        [a2, a2]
    }
}

/// Reads a category coefficient value: `num_extra` bits, each with
/// its own probability, most significant first (spec section 6.4.26).
fn read_coeff(dec: &mut Vp9BoolDecoder, cat_probs: &[u8], num_extra: usize) -> Result<i32> {
    let mut coef = 0i32;
    for (e, prob) in cat_probs.iter().take(num_extra).enumerate() {
        let bit = dec.read_bool(u32::from(*prob))?;
        coef += i32::from(bit) << (num_extra - 1 - e);
    }
    Ok(coef)
}

/// Decodes the coefficients of one transform block, spec sections
/// 6.4.24-6.4.26 and 9.3. Follows libvpx `decode_coefs`: the
/// more-coefficients bit (prob[0]) gates each position, the zero
/// bit (prob[1]) selects a ZERO token without re-reading
/// more-coefficients, the one bit (prob[2]) selects a ONE token,
/// and the Pareto branch selects TWO..CAT6. The sign bit is read
/// after the value. `tokens` is written in raster order; the caller
/// reads it back in raster order for dequantization.
///
/// `above_nonzero` / `left_nonzero` are the plane's nonzero
/// context arrays indexed by 4x4 block; `start_x4` / `start_y4`
/// are the transform block's top-left corner in 4x4 units in the
/// plane's coordinates. Counts are recorded per
/// [tx size][plane][ref][band][context].
///
/// Returns whether the block has any nonzero coefficient.
// The negated `read_bool` form mirrors the spec's bit-0-first tree
// walk (bit 0 takes the left branch); inverting the branches would
// obscure the mapping to section 9.3.3.
// `tx_size` and `tx_type` are the spec's two distinct concepts
// (transform size vs transform type); the names are kept.
#[allow(clippy::too_many_arguments, clippy::if_not_else, clippy::similar_names)]
pub(crate) fn decode_block_tokens(
    dec: &mut Vp9BoolDecoder,
    ctx: &Vp9FrameContext,
    counts: &mut Vp9Counts,
    tokens: &mut [i32; 1024],
    plane: usize,
    tx_size: u8,
    tx_type: u8,
    is_inter: bool,
    start_x4: usize,
    start_y4: usize,
    above_nonzero: &[u8],
    left_nonzero: &[u8],
) -> Result<bool> {
    let tx = tx_size as usize;
    let seg_eob = 16usize << (tx_size << 1);
    let scan = get_scan(tx_size, tx_type);
    let numpts = 1usize << tx_size;
    let plane_idx = usize::from(plane != 0);
    let ref_idx = usize::from(is_inter);
    let mut token_cache = [0u8; 1024];
    let mut check_eob = true;
    let mut c = 0usize;
    while c < seg_eob {
        let pos = scan[c] as usize;
        let band = coef_band(c, tx_size);
        // Coefficient context, spec section 9.3.2.
        let coef_ctx = if c == 0 {
            let mut above = 0u8;
            let mut left = 0u8;
            for i in 0..numpts {
                if let Some(&v) = above_nonzero.get(start_x4 + i) {
                    above |= v;
                }
                if let Some(&v) = left_nonzero.get(start_y4 + i) {
                    left |= v;
                }
            }
            (above + left) as usize
        } else {
            let nb = coef_neighbors(pos, tx_size, tx_type);
            ((1 + u32::from(token_cache[nb[0]]) + u32::from(token_cache[nb[1]])) >> 1) as usize
        };
        if check_eob {
            let prob = ctx.coef_probs[tx][plane_idx][ref_idx][band][coef_ctx][0];
            counts.coef[tx][plane_idx][ref_idx][band][coef_ctx].eob_total += 1;
            if !dec.read_bool(u32::from(prob))? {
                counts.coef[tx][plane_idx][ref_idx][band][coef_ctx].neob += 1;
                break;
            }
        }
        let probs = &ctx.coef_probs[tx][plane_idx][ref_idx][band][coef_ctx];
        if !dec.read_bool(u32::from(probs[1]))? {
            // ZERO token: no value, no sign bit, and the next
            // position is decoded without re-reading more_coefs.
            tokens[pos] = 0;
            token_cache[pos] = 0;
            counts.coef[tx][plane_idx][ref_idx][band][coef_ctx].n0 += 1;
            check_eob = false;
            c += 1;
            continue;
        }
        // Non-zero token.
        let (token, value) = if !dec.read_bool(u32::from(probs[2]))? {
            counts.coef[tx][plane_idx][ref_idx][band][coef_ctx].n1 += 1;
            (ONE_TOKEN, 1i32)
        } else {
            counts.coef[tx][plane_idx][ref_idx][band][coef_ctx].n2 += 1;
            // Pareto branch: p[k] is the probability of token
            // (k + 2); the pivot is prob[2].
            let p = [
                pareto(2, probs[2]),
                pareto(3, probs[2]),
                pareto(4, probs[2]),
                pareto(5, probs[2]),
                pareto(6, probs[2]),
                pareto(7, probs[2]),
                pareto(8, probs[2]),
                pareto(9, probs[2]),
            ];
            if !dec.read_bool(u32::from(p[0]))? {
                if !dec.read_bool(u32::from(p[1]))? {
                    (TWO_TOKEN, 2)
                } else {
                    let bit = dec.read_bool(u32::from(p[2]))?;
                    (
                        if bit { FOUR_TOKEN } else { THREE_TOKEN },
                        3 + i32::from(bit),
                    )
                }
            } else if !dec.read_bool(u32::from(p[3]))? {
                if !dec.read_bool(u32::from(p[4]))? {
                    (DCT_VAL_CAT1, 5 + read_coeff(dec, CAT_PROBS[1], 1)?)
                } else {
                    (DCT_VAL_CAT2, 7 + read_coeff(dec, CAT_PROBS[2], 2)?)
                }
            } else if !dec.read_bool(u32::from(p[5]))? {
                if !dec.read_bool(u32::from(p[6]))? {
                    (DCT_VAL_CAT3, 11 + read_coeff(dec, CAT_PROBS[3], 3)?)
                } else {
                    (DCT_VAL_CAT4, 19 + read_coeff(dec, CAT_PROBS[4], 4)?)
                }
            } else if !dec.read_bool(u32::from(p[7]))? {
                (DCT_VAL_CAT5, 35 + read_coeff(dec, CAT_PROBS[5], 5)?)
            } else {
                (DCT_VAL_CAT6, 67 + read_coeff(dec, CAT_PROBS[6], 14)?)
            }
        };
        let sign = dec.read_bool(128)?;
        tokens[pos] = if sign { -value } else { value };
        token_cache[pos] = ENERGY_CLASS[token as usize];
        check_eob = true;
        c += 1;
    }
    for cc in c..seg_eob {
        tokens[scan[cc] as usize] = 0;
    }
    Ok(c > 0)
}

// ---------------------------------------------------------------------------
// Dequantization (spec section 8.6.1)
// ---------------------------------------------------------------------------

/// Whether a segmentation feature is active for a block, spec
/// section 6.4.9.
pub(crate) fn seg_feature_active(
    header: &Vp9FrameHeader,
    segment_id: usize,
    feature: usize,
) -> bool {
    header.segmentation_enabled && header.segmentation_feature_enabled[segment_id][feature]
}

/// Quantizer index for the current block, spec section 8.6.1.
pub(crate) fn get_qindex(header: &Vp9FrameHeader, segment_id: usize) -> i32 {
    if seg_feature_active(header, segment_id, SEG_LVL_ALT_Q) {
        let mut data = header.segmentation_features[segment_id][SEG_LVL_ALT_Q];
        if !header.segmentation_abs_or_delta_update {
            data += header.base_q_idx as i32;
        }
        clip3(i64::from(data), 0, 255) as i32
    } else {
        header.base_q_idx as i32
    }
}

/// DC quantizer lookup, spec section 8.6.1 (8-bit table).
fn dc_q(b: i32) -> i32 {
    DC_QLOOKUP[clip3(i64::from(b), 0, 255) as usize]
}

/// AC quantizer lookup, spec section 8.6.1 (8-bit table).
fn ac_q(b: i32) -> i32 {
    AC_QLOOKUP[clip3(i64::from(b), 0, 255) as usize]
}

/// DC quantizer value for a plane, spec section 8.6.1.
pub(crate) fn get_dc_quant(header: &Vp9FrameHeader, segment_id: usize, plane: usize) -> i32 {
    let qindex = get_qindex(header, segment_id);
    if plane == 0 {
        dc_q(qindex + header.delta_q_y_dc)
    } else {
        dc_q(qindex + header.delta_q_uv_dc)
    }
}

/// AC quantizer value for a plane, spec section 8.6.1.
pub(crate) fn get_ac_quant(header: &Vp9FrameHeader, segment_id: usize, plane: usize) -> i32 {
    let qindex = get_qindex(header, segment_id);
    if plane == 0 {
        ac_q(qindex)
    } else {
        ac_q(qindex + header.delta_q_uv_ac)
    }
}

// ---------------------------------------------------------------------------
// Inverse transforms (spec section 8.7)
// ---------------------------------------------------------------------------

/// Bit reversal of the low `num_bits` bits, spec section 8.7.1.1.
fn brev(num_bits: u32, x: u32) -> u32 {
    x.reverse_bits() >> (32 - num_bits)
}

/// Cosine lookup, spec section 8.7.1.1: `cos64_lookup[i]` is
/// `round(16384 * cos(i * pi / 64))`.
const COS64_LOOKUP: [i64; 33] = [
    16384, 16364, 16305, 16207, 16069, 15893, 15679, 15426, 15137, 14811, 14449, 14053, 13623,
    13160, 12665, 12140, 11585, 11003, 10394, 9760, 9102, 8423, 7723, 7005, 6270, 5520, 4756, 3981,
    3196, 2404, 1606, 804, 0,
];

fn cos64(angle: i64) -> i64 {
    let a = angle & 127;
    if a <= 32 {
        COS64_LOOKUP[a as usize]
    } else if a <= 64 {
        -COS64_LOOKUP[(64 - a) as usize]
    } else if a <= 96 {
        -COS64_LOOKUP[(a - 64) as usize]
    } else {
        COS64_LOOKUP[(128 - a) as usize]
    }
}

fn sin64(angle: i64) -> i64 {
    cos64(angle - 32)
}

/// The B butterfly, spec section 8.7.1.1. The spec's two
/// multiplication form (for angles 16 + 32k) is algebraically
/// identical to the general four-multiplication form (the same
/// integer products), so only the general form is implemented.
/// `swap` is the spec's second argument: after the butterfly,
/// `T[a]` and `T[b]` are exchanged.
// Single-letter names are the spec's (T, S, a, b).
#[allow(clippy::many_single_char_names)]
fn butterfly_b(t: &mut [i64], a: usize, b: usize, angle: i64, swap: bool) {
    let x = t[a] * cos64(angle) - t[b] * sin64(angle);
    let y = t[a] * sin64(angle) + t[b] * cos64(angle);
    t[a] = round2(x, 14);
    t[b] = round2(y, 14);
    if swap {
        t.swap(a, b);
    }
}

/// The H butterfly, spec section 8.7.1.1. `H(a, b, 1)` is defined
/// as `H(b, a, 0)`, which is *not* a plain swap of the `H(a, b, 0)`
/// result: it negates the difference.
// Single-letter names are the spec's (T, S, a, b).
#[allow(clippy::many_single_char_names)]
fn butterfly_h(t: &mut [i64], a: usize, b: usize, swap: bool) {
    let x = t[a];
    let y = t[b];
    if swap {
        t[b] = y + x;
        t[a] = y - x;
    } else {
        t[a] = x + y;
        t[b] = x - y;
    }
}

/// The SB butterfly, spec section 8.7.1.1: like B, but the
/// products are written to the higher-precision array `s` without
/// rounding, and the optional swap exchanges `S[a]` and `S[b]`.
// Single-letter names are the spec's (T, S, a, b).
#[allow(clippy::many_single_char_names)]
fn butterfly_sb(t: &[i64], s: &mut [i64], a: usize, b: usize, angle: i64, swap: bool) {
    let x = t[a] * cos64(angle) - t[b] * sin64(angle);
    let y = t[a] * sin64(angle) + t[b] * cos64(angle);
    s[a] = x;
    s[b] = y;
    if swap {
        s.swap(a, b);
    }
}

/// The SH butterfly, spec section 8.7.1.1: reads the
/// higher-precision array `s` and writes rounded values to `t`.
fn butterfly_sh(s: &[i64], t: &mut [i64], a: usize, b: usize) {
    t[a] = round2(s[a] + s[b], 14);
    t[b] = round2(s[a] - s[b], 14);
}

/// DCT input permutation, spec section 8.7.1.2.
fn dct_perm(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let copy: Vec<i64> = t[..n0].to_vec();
    for i in 0..n0 {
        t[i] = copy[brev(n, i as u32) as usize];
    }
}

/// Inverse DCT process, spec section 8.7.1.3. `t` holds `1 << n`
/// values. Step 1 recurses on the first half (`t[..n1]`) with
/// `n - 1`: the sub-process's indices are all below `2^(n-1)`,
/// so it can only see the first half.
fn dct_process(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let n1 = 1usize << (n - 1);
    let n2 = 1usize << (n - 2);
    let n3 = if n >= 3 { 1usize << (n - 3) } else { 0 };
    // Step 1.
    if n == 2 {
        butterfly_b(t, 0, 1, 16, true);
    } else {
        dct_process(&mut t[..n1], n - 1);
    }
    // Step 2.
    for i in 0..n2 {
        let angle = 32 - i64::from(brev(5, (n1 + i) as u32));
        butterfly_b(t, n1 + i, n0 - 1 - i, angle, false);
    }
    // Step 3.
    if n >= 3 {
        for i in 0..n3 {
            for j in 0..2usize {
                butterfly_h(t, n1 + 4 * i + 2 * j, n1 + 1 + 4 * i + 2 * j, j == 1);
            }
        }
    }
    // Step 4 (n == 5 only).
    if n == 5 {
        for i in 0..2usize {
            for j in 0..2usize {
                let a =
                    (n0 as i64 - i64::from(n) + 3 - n2 as i64 * j as i64 - 4 * i as i64) as usize;
                let b =
                    (n1 as i64 + i64::from(n) - 4 + n2 as i64 * j as i64 + 4 * i as i64) as usize;
                let angle = 28 - 16 * i as i64 + 56 * j as i64;
                butterfly_b(t, a, b, angle, true);
            }
        }
        for i in 0..2usize {
            for j in 0..4usize {
                let a = n1 + n3 * j + i;
                let b = (n1 as i64 + n2 as i64 - 5 + n3 as i64 * j as i64 - i as i64) as usize;
                butterfly_h(t, a, b, j & 1 == 1);
            }
        }
    }
    // Step 5 (n >= 4).
    if n >= 4 {
        let i_max = usize::from(n == 5);
        for i in 0..=i_max {
            for j in 0..2usize {
                let a = (n0 as i64 - i64::from(n) + 2 - i as i64 - n2 as i64 * j as i64) as usize;
                let b = (n1 as i64 + i64::from(n) - 3 + i as i64 + n2 as i64 * j as i64) as usize;
                let angle = 24 + 48 * j as i64;
                butterfly_b(t, a, b, angle, true);
            }
        }
        for i in 0..=(2 * n as usize - 7) {
            for j in 0..2usize {
                let a = n1 + n2 * j + i;
                let b = (n1 as i64 + n2 as i64 - 1 + n2 as i64 * j as i64 - i as i64) as usize;
                butterfly_h(t, a, b, j & 1 == 1);
            }
        }
    }
    // Step 6 (n >= 3).
    if n >= 3 {
        for i in 0..n3 {
            let a = n0 - n3 - 1 - i;
            let b = n1 + n3 + i;
            butterfly_b(t, a, b, 16, true);
        }
    }
    // Step 7.
    for i in 0..n1 {
        butterfly_h(t, i, n0 - 1 - i, false);
    }
}

/// ADST input permutation, spec section 8.7.1.4.
fn adst_input_perm(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let n1 = 1usize << (n - 1);
    let copy: Vec<i64> = t[..n0].to_vec();
    for i in 0..n1 {
        t[2 * i] = copy[n0 - 1 - 2 * i];
        t[2 * i + 1] = copy[2 * i];
    }
}

/// ADST output permutation, spec section 8.7.1.5.
fn adst_output_perm(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let copy: Vec<i64> = t[..n0].to_vec();
    if n == 4 {
        for a in 0..2 {
            for b in 0..2 {
                for c in 0..2 {
                    for d in 0..2 {
                        t[8 * a + 4 * b + 2 * c + d] =
                            copy[8 * (d ^ c) + 4 * (c ^ b) + 2 * (b ^ a) + a];
                    }
                }
            }
        }
    } else {
        for a in 0..2 {
            for b in 0..2 {
                for c in 0..2 {
                    t[4 * a + 2 * b + c] = copy[4 * (c ^ b) + 2 * (b ^ a) + a];
                }
            }
        }
    }
}

/// Inverse ADST4, spec section 8.7.1.6.
fn adst4(t: &mut [i64]) {
    const SINPI_1_9: i64 = 5283;
    const SINPI_2_9: i64 = 9929;
    const SINPI_3_9: i64 = 13377;
    const SINPI_4_9: i64 = 15212;
    let s0 = SINPI_1_9 * t[0];
    let s1 = SINPI_2_9 * t[0];
    let s2 = SINPI_3_9 * t[1];
    let s3 = SINPI_4_9 * t[2];
    let s4 = SINPI_1_9 * t[2];
    let s5 = SINPI_2_9 * t[3];
    let s6 = SINPI_4_9 * t[3];
    let v = t[0] - t[2] + t[3];
    let s7 = SINPI_3_9 * v;
    let x0 = s0 + s3 + s5;
    let x1 = s1 - s4 - s6;
    let x2 = s7;
    let x3 = s2;
    let s0 = x0 + x3;
    let s1 = x1 + x3;
    let s2 = x2;
    let s3 = x0 + x1 - x3;
    t[0] = round2(s0, 14);
    t[1] = round2(s1, 14);
    t[2] = round2(s2, 14);
    t[3] = round2(s3, 14);
}

/// Inverse ADST8, spec section 8.7.1.7.
fn adst8(t: &mut [i64]) {
    let mut s = [0i64; 8];
    adst_input_perm(t, 3);
    for i in 0..4 {
        butterfly_sb(t, &mut s, 2 * i, 1 + 2 * i, 30 - 8 * i as i64, true);
    }
    for i in 0..4 {
        butterfly_sh(&s, t, i, 4 + i);
    }
    for i in 0..2 {
        butterfly_sb(t, &mut s, 4 + 3 * i, 5 + i, 24 - 16 * i as i64, true);
    }
    for i in 0..2 {
        butterfly_sh(&s, t, 4 + i, 6 + i);
    }
    for i in 0..2 {
        butterfly_h(t, i, 2 + i, false);
    }
    for i in 0..2 {
        butterfly_b(t, 2 + 4 * i, 3 + 4 * i, 16, true);
    }
    adst_output_perm(t, 3);
    for i in 0..4 {
        t[1 + 2 * i] = -t[1 + 2 * i];
    }
}

/// Inverse ADST16, spec section 8.7.1.8.
fn adst16(t: &mut [i64]) {
    let mut s = [0i64; 16];
    adst_input_perm(t, 4);
    for i in 0..8 {
        butterfly_sb(t, &mut s, 2 * i, 1 + 2 * i, 31 - 4 * i as i64, true);
    }
    for i in 0..8 {
        butterfly_sh(&s, t, i, 8 + i);
    }
    for i in 0..4 {
        butterfly_sb(t, &mut s, 8 + 2 * i, 9 + 2 * i, 28 - 16 * i as i64, true);
    }
    for i in 0..4 {
        butterfly_sh(&s, t, 8 + i, 12 + i);
    }
    for i in 0..4 {
        butterfly_h(t, i, 4 + i, false);
    }
    for i in 0..2 {
        for j in 0..2 {
            butterfly_sb(
                t,
                &mut s,
                4 + 8 * i + 3 * j,
                5 + 8 * i + j,
                24 - 16 * j as i64,
                true,
            );
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            butterfly_sh(&s, t, 4 + 8 * j + i, 6 + 8 * j + i);
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            butterfly_h(t, 8 * j + i, 2 + 8 * j + i, false);
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            butterfly_b(
                t,
                2 + 4 * j + 8 * i,
                3 + 4 * j + 8 * i,
                48 + 64 * ((i ^ j) as i64),
                false,
            );
        }
    }
    adst_output_perm(t, 4);
    for i in 0..2 {
        for j in 0..2 {
            let idx = 1 + 12 * j + 2 * i;
            t[idx] = -t[idx];
        }
    }
}

/// Inverse ADST dispatch, spec section 8.7.1.9.
fn adst_process(t: &mut [i64], n: u32) {
    match n {
        2 => adst4(t),
        3 => adst8(t),
        _ => adst16(t),
    }
}

/// Inverse Walsh-Hadamard transform, spec section 8.7.1.10.
/// Only the first 4 elements of `t` are read and written.
// Single-letter names are the spec's (T, S, a, b).
#[allow(clippy::many_single_char_names)]
fn wht(t: &mut [i64], shift: u32) {
    let mut a = t[0] >> shift;
    let mut c = t[1] >> shift;
    let mut d = t[2] >> shift;
    let mut b = t[3] >> shift;
    a += c;
    d -= b;
    let e = (a - d) >> 1;
    b = e - b;
    c = e - c;
    a -= b;
    d += c;
    t[0] = a;
    t[1] = b;
    t[2] = c;
    t[3] = d;
}

/// 2D inverse transform, spec section 8.7.2. `dequant` holds the
/// dequantized coefficients in the top-left `n0 x n0` block
/// (`n0 = 1 << n`, `n` = 2..5); the transformed samples are written
/// back in place. `lossless` requires `n == 2`: the WHT is a 4x4
/// transform, and lossless frames always use 4x4 transforms
/// (the caller guarantees this via `read_tx_size`).
pub(crate) fn inverse_transform_2d(
    dequant: &mut [[i64; 32]; 32],
    n: u32,
    tx_type: u8,
    lossless: bool,
) {
    let n0 = 1usize << n;
    let mut t = [0i64; 32];
    // Row transforms.
    for row in dequant.iter_mut().take(n0) {
        t[..n0].copy_from_slice(&row[..n0]);
        if lossless {
            wht(&mut t, 2);
        } else if tx_type == DCT_DCT || tx_type == ADST_DCT {
            dct_perm(&mut t, n);
            dct_process(&mut t, n);
        } else {
            adst_process(&mut t, n);
        }
        row[..n0].copy_from_slice(&t[..n0]);
    }
    // Column transforms.
    for j in 0..n0 {
        for (i, row) in dequant.iter().take(n0).enumerate() {
            t[i] = row[j];
        }
        if lossless {
            wht(&mut t, 0);
        } else if tx_type == DCT_DCT || tx_type == DCT_ADST {
            dct_perm(&mut t, n);
            dct_process(&mut t, n);
        } else {
            adst_process(&mut t, n);
        }
        for (i, row) in dequant.iter_mut().take(n0).enumerate() {
            row[j] = if lossless {
                t[i]
            } else {
                round2(t[i], std::cmp::min(6, n + 2))
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_permutation(table: &[u16]) -> bool {
        let mut seen = vec![false; table.len()];
        for &v in table {
            let v = v as usize;
            if v >= table.len() || seen[v] {
                return false;
            }
            seen[v] = true;
        }
        true
    }

    #[test]
    fn scan_tables_are_permutations() {
        assert!(is_permutation(&DEFAULT_SCAN_4X4));
        assert!(is_permutation(&COL_SCAN_4X4));
        assert!(is_permutation(&ROW_SCAN_4X4));
        assert!(is_permutation(&DEFAULT_SCAN_8X8));
        assert!(is_permutation(&COL_SCAN_8X8));
        assert!(is_permutation(&ROW_SCAN_8X8));
        assert!(is_permutation(&DEFAULT_SCAN_16X16));
        assert!(is_permutation(&COL_SCAN_16X16));
        assert!(is_permutation(&ROW_SCAN_16X16));
        assert!(is_permutation(&DEFAULT_SCAN_32X32));
    }

    #[test]
    fn pareto_spot_rows_match_spec_and_libvpx() {
        assert_eq!(PARETO_TABLE[0], [3, 86, 128, 6, 86, 23, 88, 29]);
        assert_eq!(PARETO_TABLE[1], [9, 86, 129, 17, 88, 61, 94, 76]);
        assert_eq!(PARETO_TABLE[64], [214, 146, 171, 239, 174, 255, 237, 253]);
        assert_eq!(PARETO_TABLE[125], [255, 238, 240, 255, 235, 255, 252, 255]);
        assert_eq!(PARETO_TABLE[126], [255, 241, 243, 255, 236, 255, 252, 254]);
        assert_eq!(PARETO_TABLE[127], [255, 246, 247, 255, 239, 255, 253, 255]);
        // Columns 0..6 are non-decreasing; column 7 has a genuine
        // dip (255 -> 254 -> 255 at rows 125..127) present in both
        // the spec and libvpx, so it is not asserted here.
        for (col, _) in PARETO_TABLE[0].iter().enumerate().take(7) {
            for row in 1..128 {
                assert!(
                    PARETO_TABLE[row][col] >= PARETO_TABLE[row - 1][col],
                    "pareto column {col} decreases at row {row}"
                );
            }
        }
    }

    #[test]
    fn inv_map_table_structure() {
        assert_eq!(INV_MAP_TABLE.len(), 255);
        for (i, &v) in INV_MAP_TABLE.iter().take(20).enumerate() {
            assert_eq!(v, (7 + 13 * i) as u8, "inv_map entry {i}");
        }
        assert!(!INV_MAP_TABLE.contains(&0));
        assert_eq!(
            INV_MAP_TABLE
                .iter()
                .fold(0, |n, &v| n + usize::from(v == 253)),
            2
        );
    }

    #[test]
    fn kf_y_mode_prob_dc_dc_spot_value() {
        assert_eq!(
            KF_Y_MODE_PROB[0][0],
            [137, 30, 42, 148, 151, 207, 70, 52, 91]
        );
    }

    #[test]
    fn quant_lookups_are_monotone_256_long() {
        assert_eq!(DC_QLOOKUP.len(), 256);
        assert_eq!(AC_QLOOKUP.len(), 256);
        assert_eq!(DC_QLOOKUP[0], 4);
        assert_eq!(DC_QLOOKUP[255], 1336);
        assert_eq!(AC_QLOOKUP[0], 4);
        assert_eq!(AC_QLOOKUP[255], 1828);
        for w in DC_QLOOKUP.windows(2) {
            assert!(w[1] >= w[0]);
        }
        for w in AC_QLOOKUP.windows(2) {
            assert!(w[1] >= w[0]);
        }
    }

    #[test]
    fn coef_prob_tables_have_432_entries_each() {
        let count = |t: &[[[[[u8; 3]; 6]; 6]; 2]; 2]| -> usize {
            t.iter()
                .flat_map(|p| p.iter())
                .flat_map(|r| r.iter())
                .flat_map(|b| b.iter())
                .flat_map(|c| c.iter())
                .count()
        };
        assert_eq!(count(&DEFAULT_COEF_PROBS_4X4), 432);
        assert_eq!(count(&DEFAULT_COEF_PROBS_8X8), 432);
        assert_eq!(count(&DEFAULT_COEF_PROBS_16X16), 432);
        assert_eq!(count(&DEFAULT_COEF_PROBS_32X32), 432);
    }

    #[test]
    fn merge_prob_with_no_counts_keeps_probability() {
        assert_eq!(merge_prob(128, 0, 0, COUNT_SAT, MAX_UPDATE_FACTOR), 128);
        assert_eq!(merge_prob(200, 0, 0, COUNT_SAT, MAX_UPDATE_FACTOR), 200);
    }

    #[test]
    fn merge_prob_moves_toward_observed_side() {
        // All counts on the zero side pull the probability up.
        let moved = merge_prob(128, 20, 0, COUNT_SAT, MAX_UPDATE_FACTOR);
        assert!(moved > 128, "moved = {moved}");
        // All counts on the one side pull it down.
        let moved = merge_prob(128, 0, 20, COUNT_SAT, MAX_UPDATE_FACTOR);
        assert!(moved < 128, "moved = {moved}");
    }

    #[test]
    fn inverse_dct_4x4_dc_only_is_constant() {
        // Hand-computed from spec section 8.7: a DC-only 4x4 block
        // with value 64 inverse-transforms to a constant block of 2s.
        let mut dequant = [[0i64; 32]; 32];
        dequant[0][0] = 64;
        inverse_transform_2d(&mut dequant, 2, DCT_DCT, false);
        for (i, row) in dequant.iter().enumerate().take(4) {
            for (j, &value) in row.iter().enumerate().take(4) {
                assert_eq!(value, 2, "dequant[{i}][{j}]");
            }
        }
    }

    #[test]
    fn inverse_wht_4x4_dc_only_is_constant() {
        // Hand-computed from spec section 8.7.1.10: a DC-only 4x4
        // lossless block with value 64 inverse-transforms to a
        // constant block of 4s.
        let mut dequant = [[0i64; 32]; 32];
        dequant[0][0] = 64;
        inverse_transform_2d(&mut dequant, 2, DCT_DCT, true);
        for (i, row) in dequant.iter().enumerate().take(4) {
            for (j, &value) in row.iter().enumerate().take(4) {
                assert_eq!(value, 4, "dequant[{i}][{j}]");
            }
        }
    }

    #[test]
    fn inverse_dct_8x8_dc_only_is_constant() {
        // The 8x8 inverse DCT exercises the n == 3 recursion; a
        // DC-only block must still transform to a constant block.
        let mut dequant = [[0i64; 32]; 32];
        dequant[0][0] = 4096;
        inverse_transform_2d(&mut dequant, 3, DCT_DCT, false);
        let first = dequant[0][0];
        assert!(first != 0, "8x8 DC-only transform must be nonzero");
        for (i, row) in dequant.iter().take(8).enumerate() {
            for (j, v) in row.iter().take(8).enumerate() {
                assert_eq!(*v, first, "dequant[{i}][{j}]");
            }
        }
    }

    #[test]
    fn inverse_dct_16x16_dc_only_is_constant() {
        // The 16x16 inverse DCT exercises the n == 4 recursion.
        let mut dequant = [[0i64; 32]; 32];
        dequant[0][0] = 4096;
        inverse_transform_2d(&mut dequant, 4, DCT_DCT, false);
        let first = dequant[0][0];
        assert!(first != 0, "16x16 DC-only transform must be nonzero");
        for (i, row) in dequant.iter().take(16).enumerate() {
            for (j, v) in row.iter().take(16).enumerate() {
                assert_eq!(*v, first, "dequant[{i}][{j}]");
            }
        }
    }

    #[test]
    fn inverse_dct_32x32_dc_only_is_constant() {
        // The 32x32 inverse DCT exercises the n == 5 recursion
        // (steps 4 and 5 of the spec's DCT); a DC-only block must
        // still transform to a constant block.
        let mut dequant = [[0i64; 32]; 32];
        dequant[0][0] = 4096;
        inverse_transform_2d(&mut dequant, 5, DCT_DCT, false);
        let first = dequant[0][0];
        assert!(first != 0, "32x32 DC-only transform must be nonzero");
        for (i, row) in dequant.iter().take(32).enumerate() {
            for (j, v) in row.iter().take(32).enumerate() {
                assert_eq!(*v, first, "dequant[{i}][{j}]");
            }
        }
    }

    #[test]
    fn inverse_adst_4x4_dc_only_is_constant_along_rows() {
        // Spec section 8.7.2: for TxType ADST_DCT the row
        // transform is DCT and the column transform is ADST,
        // so a DC-only block is constant along each row but
        // varies down each column. Hand-computed: the row DCT
        // of [4096, 0, 0, 0] is a constant 2897, and the
        // column ADST of [2897, 0, 0, 0] is [934, 1756, 2365,
        // 2690], which after Round2(., 4) is [58, 110, 148, 168].
        let mut dequant = [[0i64; 32]; 32];
        dequant[0][0] = 4096;
        inverse_transform_2d(&mut dequant, 2, ADST_DCT, false);
        let expected = [58i64, 110, 148, 168];
        for (i, row) in dequant.iter().take(4).enumerate() {
            for (j, v) in row.iter().take(4).enumerate() {
                assert_eq!(*v, expected[i], "dequant[{i}][{j}]");
            }
        }
    }
}
