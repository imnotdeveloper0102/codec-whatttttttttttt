//! AV1 tile decode: partition walk and intra mode info (AV1 spec v1.0.0).
//!
//! This module decodes one tile's mode information using the symbol
//! decoder from [`crate::av1_frame`] (spec section 8.2) and the default
//! CDF tables from spec section 9.3. It walks the partition tree
//! (section 5.11.3), reads each leaf block's intra mode info
//! (section 5.11.7), palette mode info (section 5.11.25) and palette
//! tokens (section 5.11.26), and reads the block transform size
//! (section 5.11.21).
//!
//! The decode stops at the first transform block's coefficients:
//! coefficient entropy decode needs the default coefficient CDF tables
//! (spec section 9.3: `Coeff_Base`, `Coeff_Br`, `Eob_Pt_*`,
//! `Eob_Extra`, `Coeff_Base_Eob`, `Dc_Sign`, `Txb_Skip` - roughly
//! fifteen thousand values), which are not ported in this round.
//! [`Av1TileDecoder::decode_tile`] therefore walks the whole tile's
//! mode info first and then returns `Error::Unsupported` naming that
//! gap; the recorded modes stay available through
//! [`Av1TileDecoder::blocks`].
//!
//! Only intra frames are decoded here. Inter mode info, motion vectors
//! and inter prediction are out of scope for this round.

// The casts in this module convert between the spec's integer types
// (block sizes, CDF indices, palette values) on values that are
// bounded by construction, so the pedantic cast lints are allowed
// here, as in `celt_reconstruct`.
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_precision_loss)]

use crate::av1_frame::{
    Av1SymbolDecoder, FrameHeader as Av1FrameHeader, MAX_SEGMENTS, SEG_LVL_ALT_Q,
};
use crate::av1_sequence::SequenceHeader;
use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Block sizes, intra modes, partitions, transform sizes (spec section 3)
// ---------------------------------------------------------------------------

/// Block size: 4x4.
pub(crate) const BLOCK_4X4: u8 = 0;
/// Block size: 4x8.
// Reserved for the coefficient and inter rounds.
#[allow(dead_code)]
pub(crate) const BLOCK_4X8: u8 = 1;
/// Block size: 8x4.
// Reserved for the coefficient and inter rounds.
#[allow(dead_code)]
pub(crate) const BLOCK_8X4: u8 = 2;
/// Block size: 8x8.
pub(crate) const BLOCK_8X8: u8 = 3;
/// Block size: 16x16.
// Reserved for the coefficient and inter rounds.
#[allow(dead_code)]
pub(crate) const BLOCK_16X16: u8 = 6;
/// Block size: 32x32.
// Reserved for the coefficient and inter rounds.
#[allow(dead_code)]
pub(crate) const BLOCK_32X32: u8 = 9;
/// Block size: 64x64.
pub(crate) const BLOCK_64X64: u8 = 12;
/// Block size: 128x128.
pub(crate) const BLOCK_128X128: u8 = 15;
/// Block size: invalid (table sentinel).
// Reserved for the coefficient and inter rounds.
#[allow(dead_code)]
pub(crate) const BLOCK_INVALID: u8 = 22;
/// Number of block sizes in the spec tables.
pub(crate) const BLOCK_SIZES: usize = 22;

/// Intra mode: DC prediction.
pub(crate) const DC_PRED: u8 = 0;
/// Intra mode: vertical prediction.
pub(crate) const V_PRED: u8 = 1;
/// Intra mode: horizontal prediction.
// Reserved for the coefficient round (mode comparisons).
#[allow(dead_code)]
pub(crate) const H_PRED: u8 = 2;
/// Intra mode: 45-degree prediction.
// Reserved for the coefficient round (mode comparisons).
#[allow(dead_code)]
pub(crate) const D45_PRED: u8 = 3;
/// Intra mode: 135-degree prediction.
// Reserved for the coefficient round (mode comparisons).
#[allow(dead_code)]
pub(crate) const D135_PRED: u8 = 4;
/// Intra mode: 113-degree prediction.
// Reserved for the coefficient round (mode comparisons).
#[allow(dead_code)]
pub(crate) const D113_PRED: u8 = 5;
/// Intra mode: 157-degree prediction.
// Reserved for the coefficient round (mode comparisons).
#[allow(dead_code)]
pub(crate) const D157_PRED: u8 = 6;
/// Intra mode: 203-degree prediction.
// Reserved for the coefficient round (mode comparisons).
#[allow(dead_code)]
pub(crate) const D203_PRED: u8 = 7;
/// Intra mode: 67-degree prediction.
pub(crate) const D67_PRED: u8 = 8;
/// Intra mode: smooth prediction.
// Reserved for the coefficient round (mode comparisons).
#[allow(dead_code)]
pub(crate) const SMOOTH_PRED: u8 = 9;
/// Intra mode: smooth vertical prediction.
// Reserved for the coefficient round (mode comparisons).
#[allow(dead_code)]
pub(crate) const SMOOTH_V_PRED: u8 = 10;
/// Intra mode: smooth horizontal prediction.
// Reserved for the coefficient round (mode comparisons).
#[allow(dead_code)]
pub(crate) const SMOOTH_H_PRED: u8 = 11;
/// Intra mode: Paeth prediction.
// Reserved for the coefficient round (mode comparisons).
#[allow(dead_code)]
pub(crate) const PAETH_PRED: u8 = 12;
/// Intra mode: chroma-from-luma prediction.
pub(crate) const UV_CFL_PRED: u8 = 13;
/// Number of intra modes.
pub(crate) const INTRA_MODES: usize = 13;

/// Partition: no split.
pub(crate) const PARTITION_NONE: u8 = 0;
/// Partition: horizontal split.
pub(crate) const PARTITION_HORZ: u8 = 1;
/// Partition: vertical split.
pub(crate) const PARTITION_VERT: u8 = 2;
/// Partition: four-way split.
pub(crate) const PARTITION_SPLIT: u8 = 3;
/// Partition: horizontal A.
pub(crate) const PARTITION_HORZ_A: u8 = 4;
/// Partition: horizontal B.
pub(crate) const PARTITION_HORZ_B: u8 = 5;
/// Partition: vertical A.
pub(crate) const PARTITION_VERT_A: u8 = 6;
/// Partition: vertical B.
pub(crate) const PARTITION_VERT_B: u8 = 7;
/// Partition: four horizontal strips.
pub(crate) const PARTITION_HORZ_4: u8 = 8;
/// Partition: four vertical strips.
pub(crate) const PARTITION_VERT_4: u8 = 9;

/// Transform size: 4x4.
pub(crate) const TX_4X4: u8 = 0;
/// Transform size: 8x8.
pub(crate) const TX_8X8: u8 = 1;
/// Transform size: 16x16.
pub(crate) const TX_16X16: u8 = 2;
/// Transform size: 32x32.
pub(crate) const TX_32X32: u8 = 3;
/// Transform size: 64x64.
pub(crate) const TX_64X64: u8 = 4;
/// Transform size: 4x8.
pub(crate) const TX_4X8: u8 = 5;
/// Transform size: 8x4.
pub(crate) const TX_8X4: u8 = 6;
/// Transform size: 8x16.
pub(crate) const TX_8X16: u8 = 7;
/// Transform size: 16x8.
pub(crate) const TX_16X8: u8 = 8;
/// Transform size: 16x32.
pub(crate) const TX_16X32: u8 = 9;
/// Transform size: 32x16.
pub(crate) const TX_32X16: u8 = 10;
/// Transform size: 32x64.
pub(crate) const TX_32X64: u8 = 11;
/// Transform size: 64x32.
pub(crate) const TX_64X32: u8 = 12;
/// Number of transform sizes (spec section 3, `TX_SIZES_ALL`).
pub(crate) const TX_SIZES_ALL: usize = 19;

/// Largest angle delta (spec section 3, `MAX_ANGLE_DELTA`).
pub(crate) const MAX_ANGLE_DELTA: i32 = 3;
/// Small delta-q value that triggers the extended read (spec section 3).
pub(crate) const DELTA_Q_SMALL: u32 = 3;
/// Small delta-lf value that triggers the extended read (spec section 3).
pub(crate) const DELTA_LF_SMALL: u32 = 3;

/// Base-2 log of the block width in 4-sample units (spec section 9.2).
pub(crate) const MI_WIDTH_LOG2: [u8; BLOCK_SIZES] = [
    0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 5, 0, 2, 1, 3, 2, 4,
];
/// Base-2 log of the block height in 4-sample units (spec section 9.2).
pub(crate) const MI_HEIGHT_LOG2: [u8; BLOCK_SIZES] = [
    0, 1, 0, 1, 2, 1, 2, 3, 2, 3, 4, 3, 4, 5, 4, 5, 2, 0, 3, 1, 4, 2,
];
/// Block width in 4-sample units (spec section 9.2).
pub(crate) const NUM_4X4_BLOCKS_WIDE: [u8; BLOCK_SIZES] = [
    1, 1, 2, 2, 2, 4, 4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 1, 4, 2, 8, 4, 16,
];
/// Block height in 4-sample units (spec section 9.2).
pub(crate) const NUM_4X4_BLOCKS_HIGH: [u8; BLOCK_SIZES] = [
    1, 2, 1, 2, 4, 2, 4, 8, 4, 8, 16, 8, 16, 32, 16, 32, 4, 1, 8, 2, 16, 4,
];
/// Block width in samples (spec section 9.2: 4 * `NUM_4X4_BLOCKS_WIDE`).
// Reserved for the coefficient round (block-size lookups).
#[allow(dead_code)]
pub(crate) const BLOCK_WIDTH: [u16; BLOCK_SIZES] = [
    4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 32, 64, 64, 64, 128, 128, 4, 16, 8, 32, 16, 64,
];
/// Block height in samples (spec section 9.2: 4 * `NUM_4X4_BLOCKS_HIGH`).
// Reserved for the coefficient round (block-size lookups).
#[allow(dead_code)]
pub(crate) const BLOCK_HEIGHT: [u16; BLOCK_SIZES] = [
    4, 8, 4, 8, 16, 8, 16, 32, 16, 32, 64, 32, 64, 128, 64, 128, 16, 4, 32, 8, 64, 16,
];
/// Size group used as the context for intra syntax elements (spec 9.2).
// Reserved for the coefficient round (mode-context lookups).
#[allow(dead_code)]
pub(crate) const SIZE_GROUP: [u8; BLOCK_SIZES] = [
    0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 0, 0, 1, 1, 2, 2,
];
/// Largest transform size for a block size (spec section 9.2).
pub(crate) const MAX_TX_SIZE_RECT: [u8; BLOCK_SIZES] = [
    TX_4X4, TX_4X8, TX_8X4, TX_8X8, TX_8X16, TX_16X8, TX_16X16, TX_16X32, TX_32X16, TX_32X32,
    TX_32X64, TX_64X32, TX_64X64, TX_64X64, TX_64X64, TX_64X64, 13, 14, 15, 16, 17, 18,
];
/// Maximum transform depth for a block size (spec section 5.11.21).
pub(crate) const MAX_TX_DEPTH: [u8; BLOCK_SIZES] = [
    0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4, 4, 4, 4, 2, 2, 3, 3, 4, 4,
];
/// Sub-block size for each partition type (spec section 9.2).
pub(crate) const PARTITION_SUBSIZE: [[u8; BLOCK_SIZES]; 10] = [
    [
        0, 22, 22, 3, 22, 22, 6, 22, 22, 9, 22, 22, 12, 22, 22, 15, 22, 22, 22, 22, 22, 22,
    ],
    [
        22, 22, 22, 2, 22, 22, 5, 22, 22, 8, 22, 22, 11, 22, 22, 14, 22, 22, 22, 22, 22, 22,
    ],
    [
        22, 22, 22, 1, 22, 22, 4, 22, 22, 7, 22, 22, 10, 22, 22, 13, 22, 22, 22, 22, 22, 22,
    ],
    [
        22, 22, 22, 0, 22, 22, 3, 22, 22, 6, 22, 22, 9, 22, 22, 12, 22, 22, 22, 22, 22, 22,
    ],
    [
        22, 22, 22, 2, 22, 22, 5, 22, 22, 8, 22, 22, 11, 22, 22, 14, 22, 22, 22, 22, 22, 22,
    ],
    [
        22, 22, 22, 2, 22, 22, 5, 22, 22, 8, 22, 22, 11, 22, 22, 14, 22, 22, 22, 22, 22, 22,
    ],
    [
        22, 22, 22, 1, 22, 22, 4, 22, 22, 7, 22, 22, 10, 22, 22, 13, 22, 22, 22, 22, 22, 22,
    ],
    [
        22, 22, 22, 1, 22, 22, 4, 22, 22, 7, 22, 22, 10, 22, 22, 13, 22, 22, 22, 22, 22, 22,
    ],
    [
        22, 22, 22, 22, 22, 22, 17, 22, 22, 19, 22, 22, 21, 22, 22, 22, 22, 22, 22, 22, 22, 22,
    ],
    [
        22, 22, 22, 22, 22, 22, 16, 22, 22, 18, 22, 22, 20, 22, 22, 22, 22, 22, 22, 22, 22, 22,
    ],
];
/// Transform size after one split (spec section 9.2).
pub(crate) const SPLIT_TX_SIZE: [u8; TX_SIZES_ALL] =
    [0, 0, 1, 2, 3, 0, 0, 1, 1, 2, 2, 3, 3, 5, 6, 7, 8, 9, 10];
/// Transform width in samples (spec section 9.2).
pub(crate) const TX_WIDTH: [u8; TX_SIZES_ALL] = [
    4, 8, 16, 32, 64, 4, 8, 8, 16, 16, 32, 32, 64, 4, 16, 8, 32, 16, 64,
];
/// Transform height in samples (spec section 9.2).
pub(crate) const TX_HEIGHT: [u8; TX_SIZES_ALL] = [
    4, 8, 16, 32, 64, 8, 4, 16, 8, 32, 16, 64, 32, 16, 4, 32, 8, 64, 16,
];
/// Base-2 log of the transform width (spec section 9.2).
// Reserved for the coefficient round (transform-size lookups).
#[allow(dead_code)]
pub(crate) const TX_WIDTH_LOG2: [u8; TX_SIZES_ALL] =
    [2, 3, 4, 5, 6, 2, 3, 3, 4, 4, 5, 5, 6, 2, 4, 3, 5, 4, 6];
/// Base-2 log of the transform height (spec section 9.2).
// Reserved for the coefficient round (transform-size lookups).
#[allow(dead_code)]
pub(crate) const TX_HEIGHT_LOG2: [u8; TX_SIZES_ALL] =
    [2, 3, 4, 5, 6, 3, 2, 4, 3, 5, 4, 6, 5, 4, 2, 5, 3, 6, 4];
/// Square transform size with side `min(w, h)` (spec section 9.2).
// Reserved for the coefficient round (transform-size lookups).
#[allow(dead_code)]
pub(crate) const TX_SIZE_SQR: [u8; TX_SIZES_ALL] =
    [0, 1, 2, 3, 4, 0, 0, 1, 1, 2, 2, 3, 3, 0, 0, 1, 1, 2, 2];
/// Square transform size with side `max(w, h)` (spec section 9.2).
// Reserved for the coefficient round (transform-size lookups).
#[allow(dead_code)]
pub(crate) const TX_SIZE_SQR_UP: [u8; TX_SIZES_ALL] =
    [0, 1, 2, 3, 4, 1, 1, 2, 2, 3, 3, 4, 4, 2, 2, 3, 3, 4, 4];
/// Residual block size for a plane (spec section 5.11.36, `Subsampled_Size`).
pub(crate) const SUBSAMPLED_SIZE: [[[u8; 2]; 2]; BLOCK_SIZES] = [
    [[0, 0], [0, 0]],
    [[1, 0], [22, 0]],
    [[2, 22], [0, 0]],
    [[3, 2], [1, 0]],
    [[4, 3], [22, 1]],
    [[5, 22], [3, 2]],
    [[6, 5], [4, 3]],
    [[7, 6], [22, 4]],
    [[8, 22], [6, 5]],
    [[9, 8], [7, 6]],
    [[10, 9], [22, 7]],
    [[11, 22], [9, 8]],
    [[12, 11], [10, 9]],
    [[13, 12], [22, 10]],
    [[14, 22], [12, 11]],
    [[15, 13], [14, 12]],
    [[16, 1], [22, 1]],
    [[17, 22], [2, 2]],
    [[18, 4], [22, 16]],
    [[19, 22], [5, 17]],
    [[20, 7], [22, 18]],
    [[21, 22], [8, 19]],
];
/// Intra mode context mapping for the `intra_frame_y_mode` CDF (spec 8.3.2).
pub(crate) const INTRA_MODE_CONTEXT: [u8; INTRA_MODES] = [0, 1, 2, 3, 4, 4, 4, 4, 3, 0, 1, 2, 0];
/// Palette colour context from the neighbourhood hash (spec section 9.2).
pub(crate) const PALETTE_COLOR_CONTEXT: [i8; 9] = [-1, -1, 0, -1, -1, 4, 3, 2, 1];
/// Palette colour hash multipliers for the three neighbours (spec 9.2).
pub(crate) const PALETTE_COLOR_HASH_MULTIPLIERS: [u8; 3] = [1, 2, 2];

/// Residual block size of `block_size` for `plane` (spec section 5.11.36).
pub(crate) fn get_plane_residual_size(block_size: u8, plane: u8, sub_x: u8, sub_y: u8) -> u8 {
    let (sx, sy) = if plane == 0 {
        (0usize, 0usize)
    } else {
        (usize::from(sub_x), usize::from(sub_y))
    };
    SUBSAMPLED_SIZE[usize::from(block_size)][sx][sy]
}

/// Transform size with width `w` and height `h` (spec section 5.11.36).
// Reserved for the coefficient round.
#[allow(dead_code)]
pub(crate) fn find_tx_size(w: u8, h: u8) -> u8 {
    for tx in 0..TX_SIZES_ALL {
        if TX_WIDTH[tx] == w && TX_HEIGHT[tx] == h {
            return u8::try_from(tx).unwrap_or(0);
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Default CDF tables (AV1 spec section 9.3). Every row is a cumulative
// distribution ending in 32768, 0; the trailing 0 is the adaptation
// counter that `read_symbol` increments. Rows are transcribed verbatim
// from the spec; see the unit tests below for the shape checks.
// ---------------------------------------------------------------------------

/// Default CDF for `intra_frame_y_mode` (spec section 9.3).
pub(crate) const DEFAULT_INTRA_FRAME_Y_MODE_CDF: [[[u16; 14]; 5]; 5] = [
    [
        [
            15588, 17027, 19338, 20218, 20682, 21110, 21825, 23244, 24189, 28165, 29093, 30466,
            32768, 0,
        ],
        [
            12016, 18066, 19516, 20303, 20719, 21444, 21888, 23032, 24434, 28658, 30172, 31409,
            32768, 0,
        ],
        [
            10052, 10771, 22296, 22788, 23055, 23239, 24133, 25620, 26160, 29336, 29929, 31567,
            32768, 0,
        ],
        [
            14091, 15406, 16442, 18808, 19136, 19546, 19998, 22096, 24746, 29585, 30958, 32462,
            32768, 0,
        ],
        [
            12122, 13265, 15603, 16501, 18609, 20033, 22391, 25583, 26437, 30261, 31073, 32475,
            32768, 0,
        ],
    ],
    [
        [
            10023, 19585, 20848, 21440, 21832, 22760, 23089, 24023, 25381, 29014, 30482, 31436,
            32768, 0,
        ],
        [
            5983, 24099, 24560, 24886, 25066, 25795, 25913, 26423, 27610, 29905, 31276, 31794,
            32768, 0,
        ],
        [
            7444, 12781, 20177, 20728, 21077, 21607, 22170, 23405, 24469, 27915, 29090, 30492,
            32768, 0,
        ],
        [
            8537, 14689, 15432, 17087, 17408, 18172, 18408, 19825, 24649, 29153, 31096, 32210,
            32768, 0,
        ],
        [
            7543, 14231, 15496, 16195, 17905, 20717, 21984, 24516, 26001, 29675, 30981, 31994,
            32768, 0,
        ],
    ],
    [
        [
            12613, 13591, 21383, 22004, 22312, 22577, 23401, 25055, 25729, 29538, 30305, 32077,
            32768, 0,
        ],
        [
            9687, 13470, 18506, 19230, 19604, 20147, 20695, 22062, 23219, 27743, 29211, 30907,
            32768, 0,
        ],
        [
            6183, 6505, 26024, 26252, 26366, 26434, 27082, 28354, 28555, 30467, 30794, 32086,
            32768, 0,
        ],
        [
            10718, 11734, 14954, 17224, 17565, 17924, 18561, 21523, 23878, 28975, 30287, 32252,
            32768, 0,
        ],
        [
            9194, 9858, 16501, 17263, 18424, 19171, 21563, 25961, 26561, 30072, 30737, 32463,
            32768, 0,
        ],
    ],
    [
        [
            12602, 14399, 15488, 18381, 18778, 19315, 19724, 21419, 25060, 29696, 30917, 32409,
            32768, 0,
        ],
        [
            8203, 13821, 14524, 17105, 17439, 18131, 18404, 19468, 25225, 29485, 31158, 32342,
            32768, 0,
        ],
        [
            8451, 9731, 15004, 17643, 18012, 18425, 19070, 21538, 24605, 29118, 30078, 32018,
            32768, 0,
        ],
        [
            7714, 9048, 9516, 16667, 16817, 16994, 17153, 18767, 26743, 30389, 31536, 32528, 32768,
            0,
        ],
        [
            8843, 10280, 11496, 15317, 16652, 17943, 19108, 22718, 25769, 29953, 30983, 32485,
            32768, 0,
        ],
    ],
    [
        [
            12578, 13671, 15979, 16834, 19075, 20913, 22989, 25449, 26219, 30214, 31150, 32477,
            32768, 0,
        ],
        [
            9563, 13626, 15080, 15892, 17756, 20863, 22207, 24236, 25380, 29653, 31143, 32277,
            32768, 0,
        ],
        [
            8356, 8901, 17616, 18256, 19350, 20106, 22598, 25947, 26466, 29900, 30523, 32261,
            32768, 0,
        ],
        [
            10835, 11815, 13124, 16042, 17018, 18039, 18947, 22753, 24615, 29489, 30883, 32482,
            32768, 0,
        ],
        [
            7618, 8288, 9859, 10509, 15386, 18657, 22903, 28776, 29180, 31355, 31802, 32593, 32768,
            0,
        ],
    ],
];

/// Default CDF for `y_mode` (spec section 9.3).
pub(crate) const DEFAULT_Y_MODE_CDF: [[u16; 14]; 4] = [
    [
        22801, 23489, 24293, 24756, 25601, 26123, 26606, 27418, 27945, 29228, 29685, 30349, 32768,
        0,
    ],
    [
        18673, 19845, 22631, 23318, 23950, 24649, 25527, 27364, 28152, 29701, 29984, 30852, 32768,
        0,
    ],
    [
        19770, 20979, 23396, 23939, 24241, 24654, 25136, 27073, 27830, 29360, 29730, 30659, 32768,
        0,
    ],
    [
        20155, 21301, 22838, 23178, 23261, 23533, 23703, 24804, 25352, 26575, 27016, 28049, 32768,
        0,
    ],
];

/// Default CDF for `uv_mode` when CFL is not allowed (spec section 9.3).
pub(crate) const DEFAULT_UV_MODE_CFL_NOT_ALLOWED_CDF: [[u16; 14]; 13] = [
    [
        22631, 24152, 25378, 25661, 25986, 26520, 27055, 27923, 28244, 30059, 30941, 31961, 32768,
        0,
    ],
    [
        9513, 26881, 26973, 27046, 27118, 27664, 27739, 27824, 28359, 29505, 29800, 31796, 32768, 0,
    ],
    [
        9845, 9915, 28663, 28704, 28757, 28780, 29198, 29822, 29854, 30764, 31777, 32029, 32768, 0,
    ],
    [
        13639, 13897, 14171, 25331, 25606, 25727, 25953, 27148, 28577, 30612, 31355, 32493, 32768,
        0,
    ],
    [
        9764, 9835, 9930, 9954, 25386, 27053, 27958, 28148, 28243, 31101, 31744, 32363, 32768, 0,
    ],
    [
        11825, 13589, 13677, 13720, 15048, 29213, 29301, 29458, 29711, 31161, 31441, 32550, 32768,
        0,
    ],
    [
        14175, 14399, 16608, 16821, 17718, 17775, 28551, 30200, 30245, 31837, 32342, 32667, 32768,
        0,
    ],
    [
        12885, 13038, 14978, 15590, 15673, 15748, 16176, 29128, 29267, 30643, 31961, 32461, 32768,
        0,
    ],
    [
        12026, 13661, 13874, 15305, 15490, 15726, 15995, 16273, 28443, 30388, 30767, 32416, 32768,
        0,
    ],
    [
        19052, 19840, 20579, 20916, 21150, 21467, 21885, 22719, 23174, 28861, 30379, 32175, 32768,
        0,
    ],
    [
        18627, 19649, 20974, 21219, 21492, 21816, 22199, 23119, 23527, 27053, 31397, 32148, 32768,
        0,
    ],
    [
        17026, 19004, 19997, 20339, 20586, 21103, 21349, 21907, 22482, 25896, 26541, 31819, 32768,
        0,
    ],
    [
        12124, 13759, 14959, 14992, 15007, 15051, 15078, 15166, 15255, 15753, 16039, 16606, 32768,
        0,
    ],
];

/// Default CDF for `uv_mode` when CFL is allowed (spec section 9.3).
pub(crate) const DEFAULT_UV_MODE_CFL_ALLOWED_CDF: [[u16; 15]; 13] = [
    [
        10407, 11208, 12900, 13181, 13823, 14175, 14899, 15656, 15986, 20086, 20995, 22455, 24212,
        32768, 0,
    ],
    [
        4532, 19780, 20057, 20215, 20428, 21071, 21199, 21451, 22099, 24228, 24693, 27032, 29472,
        32768, 0,
    ],
    [
        5273, 5379, 20177, 20270, 20385, 20439, 20949, 21695, 21774, 23138, 24256, 24703, 26679,
        32768, 0,
    ],
    [
        6740, 7167, 7662, 14152, 14536, 14785, 15034, 16741, 18371, 21520, 22206, 23389, 24182,
        32768, 0,
    ],
    [
        4987, 5368, 5928, 6068, 19114, 20315, 21857, 22253, 22411, 24911, 25380, 26027, 26376,
        32768, 0,
    ],
    [
        5370, 6889, 7247, 7393, 9498, 21114, 21402, 21753, 21981, 24780, 25386, 26517, 27176,
        32768, 0,
    ],
    [
        4816, 4961, 7204, 7326, 8765, 8930, 20169, 20682, 20803, 23188, 23763, 24455, 24940, 32768,
        0,
    ],
    [
        6608, 6740, 8529, 9049, 9257, 9356, 9735, 18827, 19059, 22336, 23204, 23964, 24793, 32768,
        0,
    ],
    [
        5998, 7419, 7781, 8933, 9255, 9549, 9753, 10417, 18898, 22494, 23139, 24764, 25989, 32768,
        0,
    ],
    [
        10660, 11298, 12550, 12957, 13322, 13624, 14040, 15004, 15534, 20714, 21789, 23443, 24861,
        32768, 0,
    ],
    [
        10522, 11530, 12552, 12963, 13378, 13779, 14245, 15235, 15902, 20102, 22696, 23774, 25838,
        32768, 0,
    ],
    [
        10099, 10691, 12639, 13049, 13386, 13665, 14125, 15163, 15636, 19676, 20474, 23519, 25208,
        32768, 0,
    ],
    [
        3144, 5087, 7382, 7504, 7593, 7690, 7801, 8064, 8232, 9248, 9875, 10521, 29048, 32768, 0,
    ],
];

/// Default CDF for `angle_delta` (spec section 9.3).
pub(crate) const DEFAULT_ANGLE_DELTA_CDF: [[u16; 8]; 8] = [
    [2180, 5032, 7567, 22776, 26989, 30217, 32768, 0],
    [2301, 5608, 8801, 23487, 26974, 30330, 32768, 0],
    [3780, 11018, 13699, 19354, 23083, 31286, 32768, 0],
    [4581, 11226, 15147, 17138, 21834, 28397, 32768, 0],
    [1737, 10927, 14509, 19588, 22745, 28823, 32768, 0],
    [2664, 10176, 12485, 17650, 21600, 30495, 32768, 0],
    [2240, 11096, 15453, 20341, 22561, 28917, 32768, 0],
    [3605, 10428, 12459, 17676, 21244, 30655, 32768, 0],
];

/// Default CDF for `use_intrabc` (spec section 9.3).
pub(crate) const DEFAULT_INTRABC_CDF: [u16; 3] = [30531, 32768, 0];

/// Default partition CDFs by block width log2 (spec section 9.3).
pub(crate) const DEFAULT_PARTITION_W8_CDF: [[u16; 5]; 4] = [
    [19132, 25510, 30392, 32768, 0],
    [13928, 19855, 28540, 32768, 0],
    [12522, 23679, 28629, 32768, 0],
    [9896, 18783, 25853, 32768, 0],
];
/// Default partition CDF for 16-wide blocks (spec section 9.3).
pub(crate) const DEFAULT_PARTITION_W16_CDF: [[u16; 11]; 4] = [
    [
        15597, 20929, 24571, 26706, 27664, 28821, 29601, 30571, 31902, 32768, 0,
    ],
    [
        7925, 11043, 16785, 22470, 23971, 25043, 26651, 28701, 29834, 32768, 0,
    ],
    [
        5414, 13269, 15111, 20488, 22360, 24500, 25537, 26336, 32117, 32768, 0,
    ],
    [
        2662, 6362, 8614, 20860, 23053, 24778, 26436, 27829, 31171, 32768, 0,
    ],
];
/// Default partition CDF for 32-wide blocks (spec section 9.3).
pub(crate) const DEFAULT_PARTITION_W32_CDF: [[u16; 11]; 4] = [
    [
        18462, 20920, 23124, 27647, 28227, 29049, 29519, 30178, 31544, 32768, 0,
    ],
    [
        7689, 9060, 12056, 24992, 25660, 26182, 26951, 28041, 29052, 32768, 0,
    ],
    [
        6015, 9009, 10062, 24544, 25409, 26545, 27071, 27526, 32047, 32768, 0,
    ],
    [
        1394, 2208, 2796, 28614, 29061, 29466, 29840, 30185, 31899, 32768, 0,
    ],
];
/// Default partition CDF for 64-wide blocks (spec section 9.3).
pub(crate) const DEFAULT_PARTITION_W64_CDF: [[u16; 11]; 4] = [
    [
        20137, 21547, 23078, 29566, 29837, 30261, 30524, 30892, 31724, 32768, 0,
    ],
    [
        6732, 7490, 9497, 27944, 28250, 28515, 28969, 29630, 30104, 32768, 0,
    ],
    [
        5945, 7663, 8348, 28683, 29117, 29749, 30064, 30298, 32238, 32768, 0,
    ],
    [
        870, 1212, 1487, 31198, 31394, 31574, 31743, 31881, 32332, 32768, 0,
    ],
];
/// Default partition CDF for 128-wide blocks (spec section 9.3).
pub(crate) const DEFAULT_PARTITION_W128_CDF: [[u16; 9]; 4] = [
    [27899, 28219, 28529, 32484, 32539, 32619, 32639, 32768, 0],
    [6607, 6990, 8268, 32060, 32219, 32338, 32371, 32768, 0],
    [5429, 6676, 7122, 32027, 32227, 32531, 32582, 32768, 0],
    [711, 966, 1172, 32448, 32538, 32617, 32664, 32768, 0],
];

/// Default CDF for `tx_depth` with 8x8 maximum (spec section 9.3).
pub(crate) const DEFAULT_TX_8X8_CDF: [[u16; 3]; 3] =
    [[19968, 32768, 0], [19968, 32768, 0], [24320, 32768, 0]];
/// Default CDF for `tx_depth` with 16x16 maximum (spec section 9.3).
pub(crate) const DEFAULT_TX_16X16_CDF: [[u16; 4]; 3] = [
    [12272, 30172, 32768, 0],
    [12272, 30172, 32768, 0],
    [18677, 30848, 32768, 0],
];
/// Default CDF for `tx_depth` with 32x32 maximum (spec section 9.3).
pub(crate) const DEFAULT_TX_32X32_CDF: [[u16; 4]; 3] = [
    [12986, 15180, 32768, 0],
    [12986, 15180, 32768, 0],
    [24302, 25602, 32768, 0],
];
/// Default CDF for `tx_depth` with 64x64 maximum (spec section 9.3).
pub(crate) const DEFAULT_TX_64X64_CDF: [[u16; 4]; 3] = [
    [5782, 11475, 32768, 0],
    [5782, 11475, 32768, 0],
    [16803, 22759, 32768, 0],
];

/// Default CDF for `txfm_split` (spec section 9.3).
pub(crate) const DEFAULT_TXFM_SPLIT_CDF: [[u16; 3]; 21] = [
    [28581, 32768, 0],
    [23846, 32768, 0],
    [20847, 32768, 0],
    [24315, 32768, 0],
    [18196, 32768, 0],
    [12133, 32768, 0],
    [18791, 32768, 0],
    [10887, 32768, 0],
    [11005, 32768, 0],
    [27179, 32768, 0],
    [20004, 32768, 0],
    [11281, 32768, 0],
    [26549, 32768, 0],
    [19308, 32768, 0],
    [14224, 32768, 0],
    [28015, 32768, 0],
    [21546, 32768, 0],
    [14400, 32768, 0],
    [28165, 32768, 0],
    [22401, 32768, 0],
    [16088, 32768, 0],
];

/// Default CDF for `filter_intra_mode` (spec section 9.3).
pub(crate) const DEFAULT_FILTER_INTRA_MODE_CDF: [u16; 6] = [8949, 12776, 17211, 29558, 32768, 0];
/// Default CDF for `use_filter_intra` by block size (spec section 9.3).
pub(crate) const DEFAULT_FILTER_INTRA_CDF: [[u16; 3]; 22] = [
    [4621, 32768, 0],
    [6743, 32768, 0],
    [5893, 32768, 0],
    [7866, 32768, 0],
    [12551, 32768, 0],
    [9394, 32768, 0],
    [12408, 32768, 0],
    [14301, 32768, 0],
    [12756, 32768, 0],
    [22343, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [12770, 32768, 0],
    [10368, 32768, 0],
    [20229, 32768, 0],
    [18101, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
];

/// Default CDF for `segment_id` (spec section 9.3).
pub(crate) const DEFAULT_SEGMENT_ID_CDF: [[u16; 9]; 3] = [
    [5622, 7893, 16093, 18233, 27809, 28373, 32533, 32768, 0],
    [14274, 18230, 22557, 24935, 29980, 30851, 32344, 32768, 0],
    [27527, 28487, 28723, 28890, 32397, 32647, 32679, 32768, 0],
];
/// Default CDF for `seg_id_predicted` (spec section 9.3; `128 * 128` = 16384).
pub(crate) const DEFAULT_SEGMENT_ID_PREDICTED_CDF: [[u16; 3]; 3] =
    [[16384, 32768, 0], [16384, 32768, 0], [16384, 32768, 0]];
/// Default CDF for `skip_mode` (spec section 9.3).
pub(crate) const DEFAULT_SKIP_MODE_CDF: [[u16; 3]; 3] =
    [[32621, 32768, 0], [20708, 32768, 0], [8127, 32768, 0]];
/// Default CDF for `skip` (spec section 9.3).
pub(crate) const DEFAULT_SKIP_CDF: [[u16; 3]; 3] =
    [[31671, 32768, 0], [16515, 32768, 0], [4576, 32768, 0]];
/// Default CDF for `delta_q_abs` (spec section 9.3).
pub(crate) const DEFAULT_DELTA_Q_CDF: [u16; 5] = [28160, 32120, 32677, 32768, 0];
/// Default CDF for `delta_lf_abs` (spec section 9.3).
pub(crate) const DEFAULT_DELTA_LF_CDF: [u16; 5] = [28160, 32120, 32677, 32768, 0];

/// Default CDF for `intra_tx_type` set 1 (spec section 9.3).
pub(crate) const DEFAULT_INTRA_TX_TYPE_SET1_CDF: [[[u16; 8]; 13]; 2] = [
    [
        [1535, 8035, 9461, 12751, 23467, 27825, 32768, 0],
        [564, 3335, 9709, 10870, 18143, 28094, 32768, 0],
        [672, 3247, 3676, 11982, 19415, 23127, 32768, 0],
        [5279, 13885, 15487, 18044, 23527, 30252, 32768, 0],
        [4423, 6074, 7985, 10416, 25693, 29298, 32768, 0],
        [1486, 4241, 9460, 10662, 16456, 27694, 32768, 0],
        [439, 2838, 3522, 6737, 18058, 23754, 32768, 0],
        [1190, 4233, 4855, 11670, 20281, 24377, 32768, 0],
        [1045, 4312, 8647, 10159, 18644, 29335, 32768, 0],
        [202, 3734, 4747, 7298, 17127, 24016, 32768, 0],
        [447, 4312, 6819, 8884, 16010, 23858, 32768, 0],
        [277, 4369, 5255, 8905, 16465, 22271, 32768, 0],
        [3409, 5436, 10599, 15599, 19687, 24040, 32768, 0],
    ],
    [
        [1870, 13742, 14530, 16498, 23770, 27698, 32768, 0],
        [326, 8796, 14632, 15079, 19272, 27486, 32768, 0],
        [484, 7576, 7712, 14443, 19159, 22591, 32768, 0],
        [1126, 15340, 15895, 17023, 20896, 30279, 32768, 0],
        [655, 4854, 5249, 5913, 22099, 27138, 32768, 0],
        [1299, 6458, 8885, 9290, 14851, 25497, 32768, 0],
        [311, 5295, 5552, 6885, 16107, 22672, 32768, 0],
        [883, 8059, 8270, 11258, 17289, 21549, 32768, 0],
        [741, 7580, 9318, 10345, 16688, 29046, 32768, 0],
        [110, 7406, 7915, 9195, 16041, 23329, 32768, 0],
        [363, 7974, 9357, 10673, 15629, 24474, 32768, 0],
        [153, 7647, 8112, 9936, 15307, 19996, 32768, 0],
        [3511, 6332, 11165, 15335, 19323, 23594, 32768, 0],
    ],
];

/// Default CDF for `intra_tx_type` set 2 (spec section 9.3).
pub(crate) const DEFAULT_INTRA_TX_TYPE_SET2_CDF: [[[u16; 6]; 13]; 3] = [
    [
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
    ],
    [
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
        [6554, 13107, 19661, 26214, 32768, 0],
    ],
    [
        [1127, 12814, 22772, 27483, 32768, 0],
        [145, 6761, 11980, 26667, 32768, 0],
        [362, 5887, 11678, 16725, 32768, 0],
        [385, 15213, 18587, 30693, 32768, 0],
        [25, 2914, 23134, 27903, 32768, 0],
        [60, 4470, 11749, 23991, 32768, 0],
        [37, 3332, 14511, 21448, 32768, 0],
        [157, 6320, 13036, 17439, 32768, 0],
        [119, 6719, 12906, 29396, 32768, 0],
        [47, 5537, 12576, 21499, 32768, 0],
        [269, 6076, 11258, 23115, 32768, 0],
        [83, 5615, 12001, 17228, 32768, 0],
        [1968, 5556, 12023, 18547, 32768, 0],
    ],
];

/// Default CDF for `palette_size_y_minus_2` (spec section 9.3).
pub(crate) const DEFAULT_PALETTE_Y_SIZE_CDF: [[u16; 8]; 7] = [
    [7952, 13000, 18149, 21478, 25527, 29241, 32768, 0],
    [7139, 11421, 16195, 19544, 23666, 28073, 32768, 0],
    [7788, 12741, 17325, 20500, 24315, 28530, 32768, 0],
    [8271, 14064, 18246, 21564, 25071, 28533, 32768, 0],
    [12725, 19180, 21863, 24839, 27535, 30120, 32768, 0],
    [9711, 14888, 16923, 21052, 25661, 27875, 32768, 0],
    [14940, 20797, 21678, 24186, 27033, 28999, 32768, 0],
];
/// Default CDF for `palette_size_uv_minus_2` (spec section 9.3).
pub(crate) const DEFAULT_PALETTE_UV_SIZE_CDF: [[u16; 8]; 7] = [
    [8713, 19979, 27128, 29609, 31331, 32272, 32768, 0],
    [5839, 15573, 23581, 26947, 29848, 31700, 32768, 0],
    [4426, 11260, 17999, 21483, 25863, 29430, 32768, 0],
    [3228, 9464, 14993, 18089, 22523, 27420, 32768, 0],
    [3768, 8886, 13091, 17852, 22495, 27207, 32768, 0],
    [2464, 8451, 12861, 21632, 25525, 28555, 32768, 0],
    [1269, 5435, 10433, 18963, 21700, 25865, 32768, 0],
];
/// Default CDFs for `palette_color_idx_y` by palette size 2..8 (spec 9.3).
/// Default CDFs for `palette_color_idx_y` by palette size 2..8 (spec section 9.3).
pub(crate) const DEFAULT_PALETTE_SIZE_2_Y_COLOR_CDF: [[u16; 3]; 5] = [
    [28710, 32768, 0],
    [16384, 32768, 0],
    [10553, 32768, 0],
    [27036, 32768, 0],
    [31603, 32768, 0],
];
pub(crate) const DEFAULT_PALETTE_SIZE_3_Y_COLOR_CDF: [[u16; 4]; 5] = [
    [27877, 30490, 32768, 0],
    [11532, 25697, 32768, 0],
    [6544, 30234, 32768, 0],
    [23018, 28072, 32768, 0],
    [31915, 32385, 32768, 0],
];
pub(crate) const DEFAULT_PALETTE_SIZE_4_Y_COLOR_CDF: [[u16; 5]; 5] = [
    [25572, 28046, 30045, 32768, 0],
    [9478, 21590, 27256, 32768, 0],
    [7248, 26837, 29824, 32768, 0],
    [19167, 24486, 28349, 32768, 0],
    [31400, 31825, 32250, 32768, 0],
];
pub(crate) const DEFAULT_PALETTE_SIZE_5_Y_COLOR_CDF: [[u16; 6]; 5] = [
    [24779, 26955, 28576, 30282, 32768, 0],
    [8669, 20364, 24073, 28093, 32768, 0],
    [4255, 27565, 29377, 31067, 32768, 0],
    [19864, 23674, 26716, 29530, 32768, 0],
    [31646, 31893, 32147, 32426, 32768, 0],
];
pub(crate) const DEFAULT_PALETTE_SIZE_6_Y_COLOR_CDF: [[u16; 7]; 5] = [
    [23132, 25407, 26970, 28435, 30073, 32768, 0],
    [7443, 17242, 20717, 24762, 27982, 32768, 0],
    [6300, 24862, 26944, 28784, 30671, 32768, 0],
    [18916, 22895, 25267, 27435, 29652, 32768, 0],
    [31270, 31550, 31808, 32059, 32353, 32768, 0],
];
pub(crate) const DEFAULT_PALETTE_SIZE_7_Y_COLOR_CDF: [[u16; 8]; 5] = [
    [23105, 25199, 26464, 27684, 28931, 30318, 32768, 0],
    [6950, 15447, 18952, 22681, 25567, 28563, 32768, 0],
    [7560, 23474, 25490, 27203, 28921, 30708, 32768, 0],
    [18544, 22373, 24457, 26195, 28119, 30045, 32768, 0],
    [31198, 31451, 31670, 31882, 32123, 32391, 32768, 0],
];
pub(crate) const DEFAULT_PALETTE_SIZE_8_Y_COLOR_CDF: [[u16; 9]; 5] = [
    [21689, 23883, 25163, 26352, 27506, 28827, 30195, 32768, 0],
    [6892, 15385, 17840, 21606, 24287, 26753, 29204, 32768, 0],
    [5651, 23182, 25042, 26518, 27982, 29392, 30900, 32768, 0],
    [19349, 22578, 24418, 25994, 27524, 29031, 30448, 32768, 0],
    [31028, 31270, 31504, 31705, 31927, 32153, 32392, 32768, 0],
];
/// Default CDFs for `palette_color_idx_uv` by palette size 2..8 (spec 9.3).
/// Default CDFs for `palette_color_idx_uv` by palette size 2..8 (spec section 9.3).
pub(crate) const DEFAULT_PALETTE_SIZE_2_UV_COLOR_CDF: [[u16; 3]; 5] = [
    [29089, 32768, 0],
    [16384, 32768, 0],
    [8713, 32768, 0],
    [29257, 32768, 0],
    [31610, 32768, 0],
];
pub(crate) const DEFAULT_PALETTE_SIZE_3_UV_COLOR_CDF: [[u16; 4]; 5] = [
    [25257, 29145, 32768, 0],
    [12287, 27293, 32768, 0],
    [7033, 27960, 32768, 0],
    [20145, 25405, 32768, 0],
    [30608, 31639, 32768, 0],
];
pub(crate) const DEFAULT_PALETTE_SIZE_4_UV_COLOR_CDF: [[u16; 5]; 5] = [
    [24210, 27175, 29903, 32768, 0],
    [9888, 22386, 27214, 32768, 0],
    [5901, 26053, 29293, 32768, 0],
    [18318, 22152, 28333, 32768, 0],
    [30459, 31136, 31926, 32768, 0],
];
pub(crate) const DEFAULT_PALETTE_SIZE_5_UV_COLOR_CDF: [[u16; 6]; 5] = [
    [22980, 25479, 27781, 29986, 32768, 0],
    [8413, 21408, 24859, 28874, 32768, 0],
    [2257, 29449, 30594, 31598, 32768, 0],
    [19189, 21202, 25915, 28620, 32768, 0],
    [31844, 32044, 32281, 32518, 32768, 0],
];
pub(crate) const DEFAULT_PALETTE_SIZE_6_UV_COLOR_CDF: [[u16; 7]; 5] = [
    [22217, 24567, 26637, 28683, 30548, 32768, 0],
    [7307, 16406, 19636, 24632, 28424, 32768, 0],
    [4441, 25064, 26879, 28942, 30919, 32768, 0],
    [17210, 20528, 23319, 26750, 29582, 32768, 0],
    [30674, 30953, 31396, 31735, 32207, 32768, 0],
];
pub(crate) const DEFAULT_PALETTE_SIZE_7_UV_COLOR_CDF: [[u16; 8]; 5] = [
    [21239, 23168, 25044, 26962, 28705, 30506, 32768, 0],
    [6545, 15012, 18004, 21817, 25503, 28701, 32768, 0],
    [3448, 26295, 27437, 28704, 30126, 31442, 32768, 0],
    [15889, 18323, 21704, 24698, 26976, 29690, 32768, 0],
    [30988, 31204, 31479, 31734, 31983, 32325, 32768, 0],
];
pub(crate) const DEFAULT_PALETTE_SIZE_8_UV_COLOR_CDF: [[u16; 9]; 5] = [
    [21442, 23288, 24758, 26246, 27649, 28980, 30563, 32768, 0],
    [5863, 14933, 17552, 20668, 23683, 26411, 29273, 32768, 0],
    [3415, 25810, 26877, 27990, 29223, 30394, 31618, 32768, 0],
    [17965, 20084, 22232, 23974, 26274, 28402, 30390, 32768, 0],
    [31190, 31329, 31516, 31679, 31825, 32026, 32322, 32768, 0],
];
/// Default CDF for `has_palette_y` (spec section 9.3).
pub(crate) const DEFAULT_PALETTE_Y_MODE_CDF: [[[u16; 3]; 3]; 7] = [
    [[31676, 32768, 0], [3419, 32768, 0], [1261, 32768, 0]],
    [[31912, 32768, 0], [2859, 32768, 0], [980, 32768, 0]],
    [[31823, 32768, 0], [3400, 32768, 0], [781, 32768, 0]],
    [[32030, 32768, 0], [3561, 32768, 0], [904, 32768, 0]],
    [[32309, 32768, 0], [7337, 32768, 0], [1462, 32768, 0]],
    [[32265, 32768, 0], [4015, 32768, 0], [1521, 32768, 0]],
    [[32450, 32768, 0], [7946, 32768, 0], [129, 32768, 0]],
];
/// Default CDF for `has_palette_uv` (spec section 9.3).
pub(crate) const DEFAULT_PALETTE_UV_MODE_CDF: [[u16; 3]; 2] =
    [[32461, 32768, 0], [21488, 32768, 0]];
/// Default CDF for `cfl_alpha_signs` (spec section 9.3).
pub(crate) const DEFAULT_CFL_SIGN_CDF: [u16; 9] =
    [1418, 2123, 13340, 18405, 26972, 28343, 32294, 32768, 0];
/// Default CDF for `cfl_alpha_u` / `cfl_alpha_v` (spec section 9.3).
pub(crate) const DEFAULT_CFL_ALPHA_CDF: [[u16; 17]; 6] = [
    [
        7637, 20719, 31401, 32481, 32657, 32688, 32692, 32696, 32700, 32704, 32708, 32712, 32716,
        32720, 32724, 32768, 0,
    ],
    [
        14365, 23603, 28135, 31168, 32167, 32395, 32487, 32573, 32620, 32647, 32668, 32672, 32676,
        32680, 32684, 32768, 0,
    ],
    [
        11532, 22380, 28445, 31360, 32349, 32523, 32584, 32649, 32673, 32677, 32681, 32685, 32689,
        32693, 32697, 32768, 0,
    ],
    [
        26990, 31402, 32282, 32571, 32692, 32696, 32700, 32704, 32708, 32712, 32716, 32720, 32724,
        32728, 32732, 32768, 0,
    ],
    [
        17248, 26058, 28904, 30608, 31305, 31877, 32126, 32321, 32394, 32464, 32516, 32560, 32576,
        32593, 32622, 32768, 0,
    ],
    [
        14738, 21678, 25779, 27901, 29024, 30302, 30980, 31843, 32144, 32413, 32520, 32594, 32622,
        32656, 32660, 32768, 0,
    ],
];

// ---------------------------------------------------------------------------
// Mutable per-tile CDF context
// ---------------------------------------------------------------------------

/// Owned, mutable copies of the default CDF tables for one tile.
///
/// The symbol decoder adapts each CDF row in place as symbols are
/// read (spec section 8.2.3), so the tile works on its own copy; the
/// defaults above are never mutated. A fresh tile starts from the
/// defaults (spec section 8.3.1); the frame-end CDF update from the
/// context-update tile is out of scope for the mode milestone.
#[derive(Clone)]
pub(crate) struct Av1TileCdf {
    pub(crate) intra_frame_y_mode: [[[u16; 14]; 5]; 5],
    // Reserved for intra blocks inside inter frames.
    #[allow(dead_code)]
    pub(crate) y_mode: [[u16; 14]; 4],
    pub(crate) uv_mode_cfl_not_allowed: [[u16; 14]; 13],
    pub(crate) uv_mode_cfl_allowed: [[u16; 15]; 13],
    pub(crate) angle_delta: [[u16; 8]; 8],
    pub(crate) intrabc: [u16; 3],
    pub(crate) partition_w8: [[u16; 5]; 4],
    pub(crate) partition_w16: [[u16; 11]; 4],
    pub(crate) partition_w32: [[u16; 11]; 4],
    pub(crate) partition_w64: [[u16; 11]; 4],
    pub(crate) partition_w128: [[u16; 9]; 4],
    pub(crate) tx_8x8: [[u16; 3]; 3],
    pub(crate) tx_16x16: [[u16; 4]; 3],
    pub(crate) tx_32x32: [[u16; 4]; 3],
    pub(crate) tx_64x64: [[u16; 4]; 3],
    // Reserved for the coefficient round.
    #[allow(dead_code)]
    pub(crate) txfm_split: [[u16; 3]; 21],
    pub(crate) filter_intra_mode: [u16; 6],
    pub(crate) filter_intra: [[u16; 3]; 22],
    pub(crate) segment_id: [[u16; 9]; 3],
    // Reserved for the inter-frame segment decode.
    #[allow(dead_code)]
    pub(crate) segment_id_predicted: [[u16; 3]; 3],
    // Reserved for the inter-frame skip-mode decode.
    #[allow(dead_code)]
    pub(crate) skip_mode: [[u16; 3]; 3],
    pub(crate) skip: [[u16; 3]; 3],
    pub(crate) delta_q: [u16; 5],
    pub(crate) delta_lf: [u16; 5],
    // Reserved for the coefficient round (transform-type selection).
    #[allow(dead_code)]
    pub(crate) intra_tx_type_set1: [[[u16; 8]; 13]; 2],
    // Reserved for the coefficient round (transform-type selection).
    #[allow(dead_code)]
    pub(crate) intra_tx_type_set2: [[[u16; 6]; 13]; 3],
    pub(crate) palette_y_size: [[u16; 8]; 7],
    pub(crate) palette_uv_size: [[u16; 8]; 7],
    pub(crate) palette_y_color: [[Vec<u16>; 5]; 7],
    pub(crate) palette_uv_color: [[Vec<u16>; 5]; 7],
    pub(crate) palette_y_mode: [[[u16; 3]; 3]; 7],
    pub(crate) palette_uv_mode: [[u16; 3]; 2],
    pub(crate) cfl_sign: [u16; 9],
    pub(crate) cfl_alpha: [[u16; 17]; 6],
}

impl Av1TileCdf {
    /// Fresh CDF context from the spec defaults (spec section 8.3.1).
    pub(crate) fn new() -> Self {
        Self {
            intra_frame_y_mode: DEFAULT_INTRA_FRAME_Y_MODE_CDF,
            y_mode: DEFAULT_Y_MODE_CDF,
            uv_mode_cfl_not_allowed: DEFAULT_UV_MODE_CFL_NOT_ALLOWED_CDF,
            uv_mode_cfl_allowed: DEFAULT_UV_MODE_CFL_ALLOWED_CDF,
            angle_delta: DEFAULT_ANGLE_DELTA_CDF,
            intrabc: DEFAULT_INTRABC_CDF,
            partition_w8: DEFAULT_PARTITION_W8_CDF,
            partition_w16: DEFAULT_PARTITION_W16_CDF,
            partition_w32: DEFAULT_PARTITION_W32_CDF,
            partition_w64: DEFAULT_PARTITION_W64_CDF,
            partition_w128: DEFAULT_PARTITION_W128_CDF,
            tx_8x8: DEFAULT_TX_8X8_CDF,
            tx_16x16: DEFAULT_TX_16X16_CDF,
            tx_32x32: DEFAULT_TX_32X32_CDF,
            tx_64x64: DEFAULT_TX_64X64_CDF,
            txfm_split: DEFAULT_TXFM_SPLIT_CDF,
            filter_intra_mode: DEFAULT_FILTER_INTRA_MODE_CDF,
            filter_intra: DEFAULT_FILTER_INTRA_CDF,
            segment_id: DEFAULT_SEGMENT_ID_CDF,
            segment_id_predicted: DEFAULT_SEGMENT_ID_PREDICTED_CDF,
            skip_mode: DEFAULT_SKIP_MODE_CDF,
            skip: DEFAULT_SKIP_CDF,
            delta_q: DEFAULT_DELTA_Q_CDF,
            delta_lf: DEFAULT_DELTA_LF_CDF,
            intra_tx_type_set1: DEFAULT_INTRA_TX_TYPE_SET1_CDF,
            intra_tx_type_set2: DEFAULT_INTRA_TX_TYPE_SET2_CDF,
            palette_y_size: DEFAULT_PALETTE_Y_SIZE_CDF,
            palette_uv_size: DEFAULT_PALETTE_UV_SIZE_CDF,
            palette_y_color: [
                palette_rows(DEFAULT_PALETTE_SIZE_2_Y_COLOR_CDF),
                palette_rows(DEFAULT_PALETTE_SIZE_3_Y_COLOR_CDF),
                palette_rows(DEFAULT_PALETTE_SIZE_4_Y_COLOR_CDF),
                palette_rows(DEFAULT_PALETTE_SIZE_5_Y_COLOR_CDF),
                palette_rows(DEFAULT_PALETTE_SIZE_6_Y_COLOR_CDF),
                palette_rows(DEFAULT_PALETTE_SIZE_7_Y_COLOR_CDF),
                palette_rows(DEFAULT_PALETTE_SIZE_8_Y_COLOR_CDF),
            ],
            palette_uv_color: [
                palette_rows(DEFAULT_PALETTE_SIZE_2_UV_COLOR_CDF),
                palette_rows(DEFAULT_PALETTE_SIZE_3_UV_COLOR_CDF),
                palette_rows(DEFAULT_PALETTE_SIZE_4_UV_COLOR_CDF),
                palette_rows(DEFAULT_PALETTE_SIZE_5_UV_COLOR_CDF),
                palette_rows(DEFAULT_PALETTE_SIZE_6_UV_COLOR_CDF),
                palette_rows(DEFAULT_PALETTE_SIZE_7_UV_COLOR_CDF),
                palette_rows(DEFAULT_PALETTE_SIZE_8_UV_COLOR_CDF),
            ],
            palette_y_mode: DEFAULT_PALETTE_Y_MODE_CDF,
            palette_uv_mode: DEFAULT_PALETTE_UV_MODE_CDF,
            cfl_sign: DEFAULT_CFL_SIGN_CDF,
            cfl_alpha: DEFAULT_CFL_ALPHA_CDF,
        }
    }
}

// ---------------------------------------------------------------------------
// Decoded mode info for one leaf block
// ---------------------------------------------------------------------------

/// Mode information decoded for one leaf block, in decode order.
///
/// This is the handoff to the reconstruction stage: everything the
/// intra predictor, dequantizer and inverse transform need, except
/// the coefficients themselves (not decoded in this round).
#[derive(Debug, Clone)]
pub struct Av1BlockMode {
    /// Block row in 4x4 units.
    pub mi_row: u32,
    /// Block column in 4x4 units.
    pub mi_col: u32,
    /// Block size (spec section 3).
    pub mi_size: u8,
    /// Luma intra mode (0..12).
    pub y_mode: u8,
    /// Chroma intra mode (0..13).
    pub uv_mode: u8,
    /// Luma angle delta (-3..3).
    pub angle_delta_y: i32,
    /// Chroma angle delta (-3..3).
    pub angle_delta_uv: i32,
    /// True when the block has no residual.
    pub skip: bool,
    /// Segment id (0 when segmentation is off).
    pub segment_id: u8,
    /// Quantizer index after any delta-q read.
    pub qindex: u8,
    /// True when filter-intra prediction applies.
    pub use_filter_intra: bool,
    /// Filter-intra mode (0..4), valid when `use_filter_intra`.
    pub filter_intra_mode: u8,
    /// Luma palette size (0 when no palette).
    pub palette_size_y: u8,
    /// Chroma palette size (0 when no palette).
    pub palette_size_uv: u8,
    /// Luma palette colours (first `palette_size_y` entries valid).
    pub palette_colors_y: [u16; 8],
    /// U palette colours (first `palette_size_uv` entries valid).
    pub palette_colors_u: [u16; 8],
    /// V palette colours (first `palette_size_uv` entries valid).
    pub palette_colors_v: [u16; 8],
    /// Luma palette index per sample, row-major, `block_w * block_h`.
    pub color_map_y: Vec<u8>,
    /// Chroma palette index per sample, row-major, chroma dims.
    pub color_map_uv: Vec<u8>,
    /// Chroma palette map width in samples (after the <4 adjustment).
    pub color_map_uv_width: u8,
    /// Chroma palette map height in samples (after the <4 adjustment).
    pub color_map_uv_height: u8,
    /// Chroma-from-luma alpha for U (0 when not CFL).
    pub cfl_alpha_u: i32,
    /// Chroma-from-luma alpha for V (0 when not CFL).
    pub cfl_alpha_v: i32,
    /// Transform size of the block.
    pub tx_size: u8,
    /// True when the block has chroma.
    pub has_chroma: bool,
}

/// The coefficient CDF tables are not ported in this round, so no
/// transform block can be decoded yet.
pub const COEFFICIENT_GAP: &str = "av1 coefficient decode needs the default coefficient CDF tables (spec section 9.3: Coeff_Base, Coeff_Br, Eob_Pt_16/32/64/128/256/512/1024, Eob_Extra, Coeff_Base_Eob, Dc_Sign, Txb_Skip)";

// ---------------------------------------------------------------------------
// Tile decoder
// ---------------------------------------------------------------------------

/// Decodes one tile's partition tree and intra mode info.
///
/// Grids are indexed in 4x4 units over the whole frame (spec section
/// 5.11: `MiRows` / `MiCols` are in 4x4 units), so neighbour contexts
/// read naturally across tile-internal block boundaries. Only the
/// current tile's region is written; neighbour reads outside the tile
/// use `is_inside`, which reports them unavailable.
#[allow(clippy::struct_excessive_bools)]
pub struct Av1TileDecoder<'a> {
    seq: &'a SequenceHeader,
    header: &'a Av1FrameHeader,
    /// Frame height in 4x4 units.
    mi_rows: usize,
    /// Frame width in 4x4 units.
    mi_cols: usize,
    /// First tile row in 4x4 units.
    tile_mi_row_start: usize,
    /// One past the last tile row in 4x4 units.
    tile_mi_row_end: usize,
    /// First tile column in 4x4 units.
    tile_mi_col_start: usize,
    /// One past the last tile column in 4x4 units.
    tile_mi_col_end: usize,
    sym: Av1SymbolDecoder<'a>,
    cdf: Av1TileCdf,
    mi_sizes: Vec<u8>,
    y_modes: Vec<u8>,
    uv_modes: Vec<u8>,
    skips: Vec<bool>,
    skip_modes: Vec<bool>,
    is_inters: Vec<bool>,
    inter_tx_sizes: Vec<u8>,
    segment_ids: Vec<u8>,
    palette_sizes_y: Vec<u8>,
    palette_sizes_uv: Vec<u8>,
    /// Luma palette colours per 4x4 position (for `get_palette_cache`).
    palette_colors_y: Vec<[u16; 8]>,
    /// U palette colours per 4x4 position (for `get_palette_cache`).
    palette_colors_u: Vec<[u16; 8]>,
    above_seg_pred: Vec<u8>,
    left_seg_pred: Vec<u8>,
    above_level: Vec<Vec<u16>>,
    left_level: Vec<Vec<u16>>,
    above_dc: Vec<Vec<u8>>,
    left_dc: Vec<Vec<u8>>,
    /// Current quantizer index, updated by `read_delta_qindex`.
    current_qindex: u8,
    /// Loop-filter deltas for the current block.
    delta_lf: [i32; 4],
    /// Largest segment with an enabled feature, else 0 (spec 5.9.12).
    last_active_seg_id: usize,
    /// True while delta-q reads apply to the current superblock.
    read_deltas: bool,
    /// Current leaf block row in 4x4 units.
    cur_row: usize,
    /// Current leaf block column in 4x4 units.
    cur_col: usize,
    /// Current leaf block size.
    cur_size: usize,
    /// True when the block above is inside this tile.
    cur_avail_u: bool,
    /// True when the block to the left is inside this tile.
    cur_avail_l: bool,
    /// True when the current leaf block has chroma.
    cur_has_chroma: bool,
    /// Decoded leaf blocks, in decode order.
    blocks: Vec<Av1BlockMode>,
}

impl<'a> Av1TileDecoder<'a> {
    /// Creates a tile decoder for `tile` of the frame described by
    /// `header`, reading symbols from `tile_data`.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when the tile data is too short to
    /// initialize the symbol decoder, and `InvalidData` when the tile
    /// index is outside the frame's tile grid.
    pub fn new(
        seq: &'a SequenceHeader,
        header: &'a Av1FrameHeader,
        tile_index: u32,
        tile_data: &'a [u8],
    ) -> Result<Self> {
        let mi_cols = 2 * ((usize::try_from(header.frame_width).unwrap_or(0) + 7) >> 3);
        let mi_rows = 2 * ((usize::try_from(header.frame_height).unwrap_or(0) + 7) >> 3);
        if mi_cols == 0 || mi_rows == 0 {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 tile decode needs a non-empty frame",
            });
        }
        let tile_cols = usize::try_from(header.tile_cols).unwrap_or(0);
        let tile_rows = usize::try_from(header.tile_rows).unwrap_or(0);
        if tile_cols == 0 || tile_rows == 0 {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 tile decode needs a non-empty tile grid",
            });
        }
        let tile_index = usize::try_from(tile_index).unwrap_or(usize::MAX);
        if tile_index >= tile_cols * tile_rows {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 tile index is outside the tile grid",
            });
        }
        let tile_col = tile_index % tile_cols;
        let tile_row = tile_index / tile_cols;
        let col_start =
            usize::try_from(header.mi_col_starts.get(tile_col).copied().unwrap_or(0)).unwrap_or(0);
        let col_end = usize::try_from(header.mi_col_starts.get(tile_col + 1).copied().unwrap_or(0))
            .unwrap_or(mi_cols);
        let row_start =
            usize::try_from(header.mi_row_starts.get(tile_row).copied().unwrap_or(0)).unwrap_or(0);
        let row_end = usize::try_from(header.mi_row_starts.get(tile_row + 1).copied().unwrap_or(0))
            .unwrap_or(mi_rows);
        if col_start >= col_end || row_start >= row_end || col_end > mi_cols || row_end > mi_rows {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 tile bounds are outside the frame",
            });
        }
        let sym = Av1SymbolDecoder::new(tile_data, tile_data.len(), header.disable_cdf_update)?;
        let planes = 3usize;
        let mut last_active_seg_id = 0usize;
        for segment in 0..MAX_SEGMENTS {
            if header.seg_feature_enabled[segment]
                .iter()
                .any(|enabled| *enabled)
            {
                last_active_seg_id = segment;
            }
        }
        Ok(Self {
            seq,
            header,
            mi_rows,
            mi_cols,
            tile_mi_row_start: row_start,
            tile_mi_row_end: row_end,
            tile_mi_col_start: col_start,
            tile_mi_col_end: col_end,
            sym,
            cdf: Av1TileCdf::new(),
            mi_sizes: vec![0; mi_rows * mi_cols],
            y_modes: vec![DC_PRED; mi_rows * mi_cols],
            uv_modes: vec![DC_PRED; mi_rows * mi_cols],
            skips: vec![false; mi_rows * mi_cols],
            skip_modes: vec![false; mi_rows * mi_cols],
            is_inters: vec![false; mi_rows * mi_cols],
            inter_tx_sizes: vec![TX_4X4; mi_rows * mi_cols],
            segment_ids: vec![0; mi_rows * mi_cols],
            palette_sizes_y: vec![0; mi_rows * mi_cols],
            palette_sizes_uv: vec![0; mi_rows * mi_cols],
            palette_colors_y: vec![[0; 8]; mi_rows * mi_cols],
            palette_colors_u: vec![[0; 8]; mi_rows * mi_cols],
            above_seg_pred: vec![0; mi_cols],
            left_seg_pred: vec![0; mi_rows],
            above_level: vec![vec![0; mi_cols]; planes],
            left_level: vec![vec![0; mi_rows]; planes],
            above_dc: vec![vec![0; mi_cols]; planes],
            left_dc: vec![vec![0; mi_rows]; planes],
            current_qindex: header.base_q_idx,
            delta_lf: [0; 4],
            last_active_seg_id,
            read_deltas: false,
            cur_row: 0,
            cur_col: 0,
            cur_size: usize::from(BLOCK_4X4),
            cur_avail_u: false,
            cur_avail_l: false,
            cur_has_chroma: false,
            blocks: Vec::new(),
        })
    }

    /// Leaf blocks decoded so far, in decode order.
    #[must_use]
    pub fn blocks(&self) -> &[Av1BlockMode] {
        &self.blocks
    }

    fn grid_index(&self, row: usize, col: usize) -> usize {
        row * self.mi_cols + col
    }

    /// True when the 4x4 position is inside this tile (spec 5.11.3).
    fn is_inside(&self, row: usize, col: usize) -> bool {
        row >= self.tile_mi_row_start
            && row < self.tile_mi_row_end
            && col >= self.tile_mi_col_start
            && col < self.tile_mi_col_end
    }

    /// Reads one symbol with the given CDF row (spec section 8.2.3).
    ///
    /// This is an associated function, not a method, so callers can
    /// pass disjoint borrows of the symbol decoder and the CDF row.
    fn symbol(sym: &mut Av1SymbolDecoder<'_>, cdf: &mut [u16]) -> Result<u32> {
        sym.read_symbol(cdf)
    }

    /// Reads `n` literal bits, most significant first (spec 5.11.1).
    fn literal(&mut self, n: u32) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..n {
            value = (value << 1) | u32::from(self.sym.read_bool()?);
        }
        Ok(value)
    }

    /// Reads a non-symmetric value (spec section 5.3.4).
    fn read_ns(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Err(Error::InvalidArgument("av1 ns(0) is undefined"));
        }
        let w = floor_log2(n) + 1;
        let m = (1u32 << w) - n;
        let value = self.literal(w - 1)?;
        if value < m {
            Ok(value)
        } else {
            let extra = self.literal(1)?;
            Ok(((value << 1) - m) + extra)
        }
    }

    /// Zeroes the above contexts at the start of a tile (spec 5.11.2).
    fn clear_above_context(&mut self) {
        for plane in 0..3 {
            self.above_level[plane].fill(0);
            self.above_dc[plane].fill(0);
        }
        self.above_seg_pred.fill(0);
    }

    /// Zeroes the left contexts at the start of a tile row (spec 5.11.2).
    fn clear_left_context(&mut self) {
        for plane in 0..3 {
            self.left_level[plane].fill(0);
            self.left_dc[plane].fill(0);
        }
        self.left_seg_pred.fill(0);
    }

    /// Zeroes the level and DC contexts over a skipped block (spec 5.11.4).
    fn reset_block_context(
        &mut self,
        row: usize,
        col: usize,
        bw4: usize,
        bh4: usize,
        has_chroma: bool,
    ) {
        let planes = if has_chroma { 3 } else { 1 };
        for plane in 0..planes {
            let sub_x = if plane == 0 {
                0
            } else {
                usize::from(self.seq.color.subsampling_x)
            };
            let sub_y = if plane == 0 {
                0
            } else {
                usize::from(self.seq.color.subsampling_y)
            };
            for i in (col >> sub_x)..((col + bw4) >> sub_x) {
                if i < self.mi_cols {
                    self.above_level[plane][i] = 0;
                    self.above_dc[plane][i] = 0;
                }
            }
            for i in (row >> sub_y)..((row + bh4) >> sub_y) {
                if i < self.mi_rows {
                    self.left_level[plane][i] = 0;
                    self.left_dc[plane][i] = 0;
                }
            }
        }
    }
}

// Segment feature level for skip (spec section 5.9.12).
const SEG_LVL_SKIP: usize = 6;
/// Largest loop-filter delta magnitude (spec section 3).
const MAX_LOOP_FILTER: i32 = 63;
/// TX mode: select per block (spec section 5.9.2).
const TX_MODE_SELECT: u8 = 2;
/// Number of loop-filter delta slots (spec section 5.9.2).
const FRAME_LF_COUNT: usize = 4;
/// Number of palette colours (spec section 3).
const PALETTE_COLORS: usize = 8;
/// Neighbours considered by the palette colour context (spec 5.11.27).
const PALETTE_NUM_NEIGHBORS: usize = 3;

impl Av1TileDecoder<'_> {
    /// Decodes the whole tile (spec section 5.11.2).
    ///
    /// # Errors
    ///
    /// Returns the coefficient gap (`Unsupported`) after the whole
    /// tile's mode info is recorded; see the module docs.
    pub fn decode_tile(&mut self) -> Result<()> {
        self.clear_above_context();
        self.delta_lf = [0; FRAME_LF_COUNT];
        let sb_size: usize = if self.seq.use_128x128_superblock {
            128
        } else {
            64
        };
        let sb_size4 = sb_size / 4;
        let sb_block = if sb_size == 128 {
            BLOCK_128X128
        } else {
            BLOCK_64X64
        };
        let mut sb_row = self.tile_mi_row_start;
        while sb_row < self.tile_mi_row_end {
            self.clear_left_context();
            let mut sb_col = self.tile_mi_col_start;
            while sb_col < self.tile_mi_col_end {
                self.read_deltas = self.header.delta_q_present;
                self.read_lr()?;
                self.decode_partition(sb_row, sb_col, usize::from(sb_block))?;
                sb_col += sb_size4;
            }
            sb_row += sb_size4;
        }
        // The symbol decoder pads with zeros past the tile end, so a
        // truncated tile would otherwise walk "successfully" on
        // padding. The mode walk alone must not over-consume the tile
        // by more than the trailing-bits tolerance (spec 8.2.4).
        if self.sym.symbol_max_bits() < -14 {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 tile data ends during the mode walk",
            });
        }
        // The mode walk is complete, but no transform block's
        // coefficients could be decoded; report that gap now rather
        // than pretending the tile decoded.
        Err(Error::Unsupported(COEFFICIENT_GAP))
    }

    /// Reads loop-restoration unit info for one superblock (spec 5.11.2).
    ///
    /// Restoration is not decoded in this round; a stream that uses it
    /// fails loudly here instead of silently mis-reading the tile.
    fn read_lr(&mut self) -> Result<()> {
        if self.header.uses_lr {
            return Err(Error::Unsupported(
                "av1 loop restoration is not implemented in this round",
            ));
        }
        Ok(())
    }

    /// Walks one partition node (spec section 5.11.3).
    // The ten-way partition dispatch mirrors the spec's
    // `decode_partition` one-for-one; splitting it would hide
    // that shape.
    #[allow(clippy::too_many_lines)]
    fn decode_partition(&mut self, row: usize, col: usize, b_size: usize) -> Result<()> {
        if row >= self.mi_rows || col >= self.mi_cols {
            return Ok(());
        }
        if b_size >= BLOCK_SIZES {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 partition block size is out of range",
            });
        }
        let avail_u = row > 0 && self.is_inside(row - 1, col);
        let avail_l = col > 0 && self.is_inside(row, col - 1);
        let num4x4 = usize::from(NUM_4X4_BLOCKS_WIDE[b_size]);
        let half = num4x4 >> 1;
        let quarter = half >> 1;
        let has_rows = row + half < self.mi_rows;
        let has_cols = col + half < self.mi_cols;
        let partition = if b_size < usize::from(BLOCK_8X8) {
            PARTITION_NONE
        } else if has_rows && has_cols {
            self.read_partition(b_size, row, col, avail_u, avail_l)?
        } else if has_cols {
            if self.read_split_or_horz(b_size, row, col, avail_u, avail_l)? {
                PARTITION_SPLIT
            } else {
                PARTITION_HORZ
            }
        } else if has_rows {
            if self.read_split_or_vert(b_size, row, col, avail_u, avail_l)? {
                PARTITION_SPLIT
            } else {
                PARTITION_VERT
            }
        } else {
            PARTITION_SPLIT
        };
        let sub_size = usize::from(PARTITION_SUBSIZE[usize::from(partition)][b_size]);
        let split_size = usize::from(PARTITION_SUBSIZE[usize::from(PARTITION_SPLIT)][b_size]);
        if sub_size >= BLOCK_SIZES || split_size >= BLOCK_SIZES {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 partition subsize is invalid for the block size",
            });
        }
        match partition {
            PARTITION_NONE => self.decode_block(row, col, sub_size),
            PARTITION_HORZ => {
                self.decode_block(row, col, sub_size)?;
                if has_rows {
                    self.decode_block(row + half, col, sub_size)
                } else {
                    Ok(())
                }
            }
            PARTITION_VERT => {
                self.decode_block(row, col, sub_size)?;
                if has_cols {
                    self.decode_block(row, col + half, sub_size)
                } else {
                    Ok(())
                }
            }
            PARTITION_SPLIT => {
                self.decode_partition(row, col, sub_size)?;
                self.decode_partition(row, col + half, sub_size)?;
                self.decode_partition(row + half, col, sub_size)?;
                self.decode_partition(row + half, col + half, sub_size)
            }
            PARTITION_HORZ_A => {
                self.decode_block(row, col, split_size)?;
                self.decode_block(row, col + half, split_size)?;
                self.decode_block(row + half, col, sub_size)
            }
            PARTITION_HORZ_B => {
                self.decode_block(row, col, sub_size)?;
                self.decode_block(row + half, col, split_size)?;
                self.decode_block(row + half, col + half, split_size)
            }
            PARTITION_VERT_A => {
                self.decode_block(row, col, split_size)?;
                self.decode_block(row + half, col, split_size)?;
                self.decode_block(row, col + half, sub_size)
            }
            PARTITION_VERT_B => {
                self.decode_block(row, col, sub_size)?;
                self.decode_block(row, col + half, split_size)?;
                self.decode_block(row + half, col + half, split_size)
            }
            PARTITION_HORZ_4 => {
                for i in 0..3 {
                    self.decode_block(row + quarter * i, col, sub_size)?;
                }
                if row + quarter * 3 < self.mi_rows {
                    self.decode_block(row + quarter * 3, col, sub_size)
                } else {
                    Ok(())
                }
            }
            PARTITION_VERT_4 => {
                for i in 0..3 {
                    self.decode_block(row, col + quarter * i, sub_size)?;
                }
                if col + quarter * 3 < self.mi_cols {
                    self.decode_block(row, col + quarter * 3, sub_size)
                } else {
                    Ok(())
                }
            }
            _ => Err(Error::InvalidData {
                offset: 0,
                detail: "av1 partition value is out of range",
            }),
        }
    }

    /// Reads a partition symbol (spec sections 5.11.3 and 8.3.2).
    fn read_partition(
        &mut self,
        b_size: usize,
        row: usize,
        col: usize,
        avail_u: bool,
        avail_l: bool,
    ) -> Result<u8> {
        let bsl = usize::from(MI_WIDTH_LOG2[b_size]);
        let ctx = self.partition_ctx(b_size, row, col, avail_u, avail_l);
        let sym = match bsl {
            1 => Self::symbol(&mut self.sym, &mut self.cdf.partition_w8[ctx])?,
            2 => Self::symbol(&mut self.sym, &mut self.cdf.partition_w16[ctx])?,
            3 => Self::symbol(&mut self.sym, &mut self.cdf.partition_w32[ctx])?,
            4 => Self::symbol(&mut self.sym, &mut self.cdf.partition_w64[ctx])?,
            _ => Self::symbol(&mut self.sym, &mut self.cdf.partition_w128[ctx])?,
        };
        Ok(sym as u8)
    }

    /// Partition context from the neighbours' sizes (spec 8.3.2).
    fn partition_ctx(
        &self,
        b_size: usize,
        row: usize,
        col: usize,
        avail_u: bool,
        avail_l: bool,
    ) -> usize {
        let bsl = usize::from(MI_WIDTH_LOG2[b_size]);
        let above = avail_u
            && usize::from(
                MI_WIDTH_LOG2[usize::from(self.mi_sizes[self.grid_index(row - 1, col)])],
            ) < bsl;
        let left = avail_l
            && usize::from(
                MI_HEIGHT_LOG2[usize::from(self.mi_sizes[self.grid_index(row, col - 1)])],
            ) < bsl;
        usize::from(left) * 2 + usize::from(above)
    }

    /// One row of the partition CDF for the given block-size log2.
    fn partition_row(&self, bsl: usize, ctx: usize) -> &[u16] {
        match bsl {
            1 => &self.cdf.partition_w8[ctx],
            2 => &self.cdf.partition_w16[ctx],
            3 => &self.cdf.partition_w32[ctx],
            4 => &self.cdf.partition_w64[ctx],
            _ => &self.cdf.partition_w128[ctx],
        }
    }

    /// Reads a split-or-horizontal symbol; true means SPLIT (spec 8.3.2).
    ///
    /// The CDF is gathered from the partition CDF row, so adaptation
    /// applies to the temporary copy only.
    fn read_split_or_horz(
        &mut self,
        b_size: usize,
        row: usize,
        col: usize,
        avail_u: bool,
        avail_l: bool,
    ) -> Result<bool> {
        let bsl = usize::from(MI_WIDTH_LOG2[b_size]);
        if bsl < 2 {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 split-or-horz is undefined for 8x8 blocks",
            });
        }
        let ctx = self.partition_ctx(b_size, row, col, avail_u, avail_l);
        let row_cdf = self.partition_row(bsl, ctx);
        let mut psum = cdf_prob(row_cdf, usize::from(PARTITION_VERT))
            + cdf_prob(row_cdf, usize::from(PARTITION_SPLIT))
            + cdf_prob(row_cdf, usize::from(PARTITION_HORZ_A))
            + cdf_prob(row_cdf, usize::from(PARTITION_VERT_A))
            + cdf_prob(row_cdf, usize::from(PARTITION_VERT_B));
        if b_size != usize::from(BLOCK_128X128) {
            psum += cdf_prob(row_cdf, usize::from(PARTITION_VERT_4));
        }
        let mut gathered = [(1u16 << 15).saturating_sub(psum), 1 << 15, 0];
        let sym = Self::symbol(&mut self.sym, &mut gathered)?;
        Ok(sym == 1)
    }

    /// Reads a split-or-vertical symbol; true means SPLIT (spec 8.3.2).
    fn read_split_or_vert(
        &mut self,
        b_size: usize,
        row: usize,
        col: usize,
        avail_u: bool,
        avail_l: bool,
    ) -> Result<bool> {
        let bsl = usize::from(MI_WIDTH_LOG2[b_size]);
        if bsl < 2 {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 split-or-vert is undefined for 8x8 blocks",
            });
        }
        let ctx = self.partition_ctx(b_size, row, col, avail_u, avail_l);
        let row_cdf = self.partition_row(bsl, ctx);
        let mut psum = cdf_prob(row_cdf, usize::from(PARTITION_HORZ))
            + cdf_prob(row_cdf, usize::from(PARTITION_SPLIT))
            + cdf_prob(row_cdf, usize::from(PARTITION_HORZ_A))
            + cdf_prob(row_cdf, usize::from(PARTITION_HORZ_B))
            + cdf_prob(row_cdf, usize::from(PARTITION_VERT_A));
        if b_size != usize::from(BLOCK_128X128) {
            psum += cdf_prob(row_cdf, usize::from(PARTITION_HORZ_4));
        }
        let mut gathered = [(1u16 << 15).saturating_sub(psum), 1 << 15, 0];
        let sym = Self::symbol(&mut self.sym, &mut gathered)?;
        Ok(sym == 1)
    }
}

impl Av1TileDecoder<'_> {
    /// Decodes one leaf block's mode info (spec section 5.11.4).
    ///
    /// The mode info is recorded; the block's coefficients are not
    /// decoded yet (see the module docs), so the caller reports the
    /// coefficient gap after the whole tile is walked.
    fn decode_block(&mut self, row: usize, col: usize, sub_size: usize) -> Result<()> {
        if sub_size >= BLOCK_SIZES || row >= self.mi_rows || col >= self.mi_cols {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 leaf block is outside the frame or has an invalid size",
            });
        }
        self.cur_row = row;
        self.cur_col = col;
        self.cur_size = sub_size;
        let bw4 = usize::from(NUM_4X4_BLOCKS_WIDE[sub_size]);
        let bh4 = usize::from(NUM_4X4_BLOCKS_HIGH[sub_size]);
        let sub_x = usize::from(self.seq.color.subsampling_x);
        let sub_y = usize::from(self.seq.color.subsampling_y);
        let mut has_chroma = true;
        if bh4 == 1 && sub_y == 1 && row % 2 == 0 {
            has_chroma = false;
        }
        if bw4 == 1 && sub_x == 1 && col % 2 == 0 {
            has_chroma = false;
        }
        self.cur_has_chroma = has_chroma;
        self.cur_avail_u = row > 0 && self.is_inside(row - 1, col);
        self.cur_avail_l = col > 0 && self.is_inside(row, col - 1);
        let mut mode = self.intra_frame_mode_info(has_chroma)?;
        self.palette_tokens(&mut mode)?;
        self.read_block_tx_size(&mut mode)?;
        if mode.skip {
            self.reset_block_context(row, col, bw4, bh4, has_chroma);
        }
        self.fill_block_grids(&mode);
        self.blocks.push(mode);
        Ok(())
    }

    /// Fills the neighbour-context grids over one leaf block.
    ///
    /// The spec fills the mode grids before the residual and the
    /// remaining grids after it; the residual never runs here, so all
    /// of them are filled now with identical values. Edge blocks are
    /// clipped to the frame.
    fn fill_block_grids(&mut self, mode: &Av1BlockMode) {
        let row = mode.mi_row as usize;
        let col = mode.mi_col as usize;
        let bw4 = usize::from(NUM_4X4_BLOCKS_WIDE[usize::from(mode.mi_size)]);
        let bh4 = usize::from(NUM_4X4_BLOCKS_HIGH[usize::from(mode.mi_size)]);
        for r in row..(row + bh4).min(self.mi_rows) {
            for c in col..(col + bw4).min(self.mi_cols) {
                let idx = self.grid_index(r, c);
                self.mi_sizes[idx] = mode.mi_size;
                self.y_modes[idx] = mode.y_mode;
                self.uv_modes[idx] = mode.uv_mode;
                self.skips[idx] = mode.skip;
                self.skip_modes[idx] = false;
                self.is_inters[idx] = false;
                self.inter_tx_sizes[idx] = mode.tx_size;
                self.segment_ids[idx] = mode.segment_id;
                self.palette_sizes_y[idx] = mode.palette_size_y;
                self.palette_sizes_uv[idx] = mode.palette_size_uv;
                self.palette_colors_y[idx] = mode.palette_colors_y;
                self.palette_colors_u[idx] = mode.palette_colors_u;
            }
        }
    }

    /// Reads one leaf block's intra mode info (spec section 5.11.7).
    fn intra_frame_mode_info(&mut self, has_chroma: bool) -> Result<Av1BlockMode> {
        let mut mode = Av1BlockMode {
            mi_row: self.cur_row as u32,
            mi_col: self.cur_col as u32,
            mi_size: self.cur_size as u8,
            y_mode: DC_PRED,
            uv_mode: DC_PRED,
            angle_delta_y: 0,
            angle_delta_uv: 0,
            skip: false,
            segment_id: 0,
            qindex: self.current_qindex,
            use_filter_intra: false,
            filter_intra_mode: 0,
            palette_size_y: 0,
            palette_size_uv: 0,
            palette_colors_y: [0; 8],
            palette_colors_u: [0; 8],
            palette_colors_v: [0; 8],
            color_map_y: Vec::new(),
            color_map_uv: Vec::new(),
            color_map_uv_width: 0,
            color_map_uv_height: 0,
            cfl_alpha_u: 0,
            cfl_alpha_v: 0,
            tx_size: TX_4X4,
            has_chroma,
        };
        if self.header.seg_id_pre_skip {
            self.intra_segment_id(&mut mode)?;
        }
        // skip_mode is always 0 for intra frames: read_skip_mode is an
        // inter-only syntax (spec section 5.11.7).
        self.read_skip(&mut mode)?;
        if !self.header.seg_id_pre_skip {
            self.intra_segment_id(&mut mode)?;
        }
        self.read_cdef(&mode)?;
        self.read_delta_qindex(&mut mode)?;
        self.read_delta_lf(mode.skip)?;
        self.read_deltas = false;
        if self.header.allow_intrabc {
            let use_intrabc = Self::symbol(&mut self.sym, &mut self.cdf.intrabc)?;
            if use_intrabc != 0 {
                return Err(Error::Unsupported(
                    "av1 intra block copy is not implemented in this round",
                ));
            }
        }
        // Intra frames read the luma mode with the neighbour-context
        // CDF (spec section 5.11.7); the size-group CDF is only for
        // intra blocks inside inter frames.
        mode.y_mode = self.read_intra_frame_y_mode()? as u8;
        mode.angle_delta_y = self.intra_angle_info(mode.y_mode)?;
        if has_chroma {
            let lossless = self.segment_lossless(mode.segment_id);
            mode.uv_mode = self.read_uv_mode(mode.y_mode, lossless)? as u8;
            if mode.uv_mode == UV_CFL_PRED {
                self.read_cfl_alphas(&mut mode)?;
            }
            mode.angle_delta_uv = self.intra_angle_info(mode.uv_mode)?;
        }
        let bw4 = usize::from(NUM_4X4_BLOCKS_WIDE[usize::from(mode.mi_size)]);
        let bh4 = usize::from(NUM_4X4_BLOCKS_HIGH[usize::from(mode.mi_size)]);
        if usize::from(mode.mi_size) >= usize::from(BLOCK_8X8)
            && bw4 * 4 <= 64
            && bh4 * 4 <= 64
            && self.header.allow_screen_content_tools
        {
            self.palette_mode_info(&mut mode)?;
        }
        self.filter_intra_mode_info(&mut mode)?;
        Ok(mode)
    }

    /// Reads the segment id for an intra frame (spec section 5.11.8).
    fn intra_segment_id(&mut self, mode: &mut Av1BlockMode) -> Result<()> {
        if self.header.segmentation_enabled {
            self.read_segment_id(mode)?;
        } else {
            mode.segment_id = 0;
        }
        Ok(())
    }

    /// Reads a segment id from the stream (spec section 5.11.9).
    fn read_segment_id(&mut self, mode: &mut Av1BlockMode) -> Result<()> {
        let row = self.cur_row;
        let col = self.cur_col;
        let prev_ul: i32 = if self.cur_avail_u && self.cur_avail_l {
            i32::from(self.segment_ids[self.grid_index(row - 1, col - 1)])
        } else {
            -1
        };
        let prev_u: i32 = if self.cur_avail_u {
            i32::from(self.segment_ids[self.grid_index(row - 1, col)])
        } else {
            -1
        };
        let prev_left: i32 = if self.cur_avail_l {
            i32::from(self.segment_ids[self.grid_index(row, col - 1)])
        } else {
            -1
        };
        // Spec section 5.11.9: pred is the left segment when the
        // upper one is unavailable (0 when both are), the upper
        // segment when the left one is unavailable, and otherwise
        // the upper one when the upper-left agrees with it, else
        // the left one. The first two cases both yield the upper
        // segment, so they are one condition here.
        let pred = if prev_u == -1 {
            if prev_left == -1 {
                0
            } else {
                prev_left
            }
        } else if prev_left == -1 || prev_ul == prev_u {
            prev_u
        } else {
            prev_left
        };
        if mode.skip {
            mode.segment_id = pred as u8;
            return Ok(());
        }
        // Spec section 8.3.2: context 2 when all three agree, 1
        // when any pair agrees, 0 otherwise. The upper-left
        // neighbour being unavailable forces 0 even when the
        // other two agree, so the pair check also requires it.
        let ctx = if prev_ul >= 0 && prev_ul == prev_u && prev_ul == prev_left {
            2
        } else {
            usize::from(
                prev_ul >= 0 && (prev_ul == prev_u || prev_ul == prev_left || prev_u == prev_left),
            )
        };
        let diff = Self::symbol(&mut self.sym, &mut self.cdf.segment_id[ctx])?;
        let segment_id = neg_deinterleave(diff as i32, pred, self.last_active_seg_id as i32 + 1);
        if !(0..MAX_SEGMENTS as i32).contains(&segment_id) {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 segment id is out of range",
            });
        }
        mode.segment_id = segment_id as u8;
        Ok(())
    }

    /// Reads the skip flag (spec section 5.11.11).
    fn read_skip(&mut self, mode: &mut Av1BlockMode) -> Result<()> {
        if self.header.seg_id_pre_skip && self.seg_feature_active(mode.segment_id, SEG_LVL_SKIP) {
            mode.skip = true;
        } else {
            let ctx = self.skip_ctx();
            mode.skip = Self::symbol(&mut self.sym, &mut self.cdf.skip[ctx])? != 0;
        }
        Ok(())
    }

    /// Skip context from the neighbours' skip flags (spec 8.3.2).
    fn skip_ctx(&self) -> usize {
        let row = self.cur_row;
        let col = self.cur_col;
        let above = self.cur_avail_u && self.skips[self.grid_index(row - 1, col)];
        let left = self.cur_avail_l && self.skips[self.grid_index(row, col - 1)];
        usize::from(above) + usize::from(left)
    }

    /// Reads the CDEF index (spec section 5.11.12).
    ///
    /// A CDEF index is read once per 64x64 region and feeds only the
    /// loop filter, which is not applied in this round. A stream that
    /// would read one fails loudly here instead of consuming the
    /// wrong bits.
    fn read_cdef(&mut self, mode: &Av1BlockMode) -> Result<()> {
        if mode.skip
            || self.header.coded_lossless
            || !self.seq.enable_cdef
            || self.header.allow_intrabc
        {
            return Ok(());
        }
        Err(Error::Unsupported(
            "av1 cdef index decode is not implemented in this round",
        ))
    }

    /// Reads a quantizer index delta (spec section 5.11.13).
    fn read_delta_qindex(&mut self, mode: &mut Av1BlockMode) -> Result<()> {
        let sb_block = if self.seq.use_128x128_superblock {
            BLOCK_128X128
        } else {
            BLOCK_64X64
        };
        if mode.mi_size == sb_block && mode.skip {
            return Ok(());
        }
        if self.read_deltas {
            let mut delta_q_abs = Self::symbol(&mut self.sym, &mut self.cdf.delta_q)?;
            if delta_q_abs == DELTA_Q_SMALL {
                let rem_bits = self.literal(3)? + 1;
                let abs_bits = self.literal(rem_bits)?;
                delta_q_abs = abs_bits + (1 << rem_bits) + 1;
            }
            if delta_q_abs != 0 {
                let sign = self.literal(1)?;
                let reduced = if sign == 0 {
                    delta_q_abs as i32
                } else {
                    -(delta_q_abs as i32)
                };
                let updated = i32::from(self.current_qindex) + (reduced << self.header.delta_q_res);
                self.current_qindex = updated.clamp(1, 255) as u8;
            }
        }
        mode.qindex = self.current_qindex;
        Ok(())
    }

    /// Reads loop-filter deltas (spec section 5.11.14).
    fn read_delta_lf(&mut self, skip: bool) -> Result<()> {
        let sb_block = if self.seq.use_128x128_superblock {
            BLOCK_128X128
        } else {
            BLOCK_64X64
        };
        if self.cur_size == usize::from(sb_block) && skip {
            return Ok(());
        }
        if !(self.read_deltas && self.header.delta_lf_present) {
            return Ok(());
        }
        let num_planes = if self.seq.color.mono_chrome { 1 } else { 3 };
        let mut frame_lf_count = 1usize;
        if self.header.delta_lf_multi {
            frame_lf_count = if num_planes > 1 {
                FRAME_LF_COUNT
            } else {
                FRAME_LF_COUNT - 2
            };
        }
        for i in 0..frame_lf_count {
            let mut delta_lf_abs = Self::symbol(&mut self.sym, &mut self.cdf.delta_lf)?;
            if delta_lf_abs == DELTA_LF_SMALL {
                let rem_bits = self.literal(3)? + 1;
                let abs_bits = self.literal(rem_bits)?;
                delta_lf_abs = abs_bits + (1 << rem_bits) + 1;
            }
            if delta_lf_abs != 0 {
                let sign = self.literal(1)?;
                let reduced = if sign == 0 {
                    delta_lf_abs as i32
                } else {
                    -(delta_lf_abs as i32)
                };
                let updated = self.delta_lf[i] + (reduced << self.header.delta_lf_res);
                self.delta_lf[i] = updated.clamp(-MAX_LOOP_FILTER, MAX_LOOP_FILTER);
            }
        }
        Ok(())
    }
}

impl Av1TileDecoder<'_> {
    /// Reads the luma intra mode for an intra frame (spec 8.3.2).
    fn read_intra_frame_y_mode(&mut self) -> Result<u32> {
        let row = self.cur_row;
        let col = self.cur_col;
        let above = if self.cur_avail_u {
            usize::from(
                INTRA_MODE_CONTEXT[usize::from(self.y_modes[self.grid_index(row - 1, col)])],
            )
        } else {
            usize::from(INTRA_MODE_CONTEXT[usize::from(DC_PRED)])
        };
        let left = if self.cur_avail_l {
            usize::from(
                INTRA_MODE_CONTEXT[usize::from(self.y_modes[self.grid_index(row, col - 1)])],
            )
        } else {
            usize::from(INTRA_MODE_CONTEXT[usize::from(DC_PRED)])
        };
        Self::symbol(&mut self.sym, &mut self.cdf.intra_frame_y_mode[above][left])
    }

    /// Reads the chroma intra mode (spec 8.3.2).
    fn read_uv_mode(&mut self, y_mode: u8, lossless: bool) -> Result<u32> {
        let size = self.cur_size;
        let bw = usize::from(NUM_4X4_BLOCKS_WIDE[size]) * 4;
        let bh = usize::from(NUM_4X4_BLOCKS_HIGH[size]) * 4;
        let residual = get_plane_residual_size(
            size as u8,
            1,
            u8::from(self.seq.color.subsampling_x),
            u8::from(self.seq.color.subsampling_y),
        );
        let cfl_allowed = (lossless && residual == BLOCK_4X4) || (!lossless && bw.max(bh) <= 32);
        let y = usize::from(y_mode);
        if cfl_allowed {
            Self::symbol(&mut self.sym, &mut self.cdf.uv_mode_cfl_allowed[y])
        } else {
            Self::symbol(&mut self.sym, &mut self.cdf.uv_mode_cfl_not_allowed[y])
        }
    }

    /// Reads an angle delta for a directional mode (spec 5.11.16).
    fn intra_angle_info(&mut self, mode: u8) -> Result<i32> {
        if self.cur_size >= usize::from(BLOCK_8X8) && is_directional_mode(mode) {
            let idx = usize::from(mode - V_PRED);
            let delta = Self::symbol(&mut self.sym, &mut self.cdf.angle_delta[idx])?;
            Ok(delta as i32 - MAX_ANGLE_DELTA)
        } else {
            Ok(0)
        }
    }

    /// Reads chroma-from-luma alphas (spec sections 5.11.17, 8.3.2).
    fn read_cfl_alphas(&mut self, mode: &mut Av1BlockMode) -> Result<()> {
        let signs = Self::symbol(&mut self.sym, &mut self.cdf.cfl_sign)?;
        let sign_u = (signs + 1) / 3;
        let sign_v = (signs + 1) % 3;
        if sign_u != 0 {
            let ctx = (signs - 2) as usize;
            let alpha = Self::symbol(&mut self.sym, &mut self.cdf.cfl_alpha[ctx])?;
            let mut value = 1 + alpha as i32;
            if sign_u == 1 {
                value = -value;
            }
            mode.cfl_alpha_u = value;
        }
        if sign_v != 0 {
            let ctx = ((sign_v - 1) * 3 + sign_u) as usize;
            let alpha = Self::symbol(&mut self.sym, &mut self.cdf.cfl_alpha[ctx])?;
            let mut value = 1 + alpha as i32;
            if sign_v == 1 {
                value = -value;
            }
            mode.cfl_alpha_v = value;
        }
        Ok(())
    }

    /// Reads palette mode info (spec section 5.11.25).
    fn palette_mode_info(&mut self, mode: &mut Av1BlockMode) -> Result<()> {
        let size = usize::from(mode.mi_size);
        let bsize_ctx = usize::from(MI_WIDTH_LOG2[size]) + usize::from(MI_HEIGHT_LOG2[size]) - 2;
        if mode.y_mode == DC_PRED {
            let ctx = self.palette_ctx(0);
            let has_palette =
                Self::symbol(&mut self.sym, &mut self.cdf.palette_y_mode[bsize_ctx][ctx])?;
            if has_palette != 0 {
                let size_minus_2 =
                    Self::symbol(&mut self.sym, &mut self.cdf.palette_y_size[bsize_ctx])?;
                mode.palette_size_y = (size_minus_2 + 2) as u8;
                self.read_palette_colors_y(mode)?;
            }
        }
        if mode.has_chroma && mode.uv_mode == DC_PRED {
            // The UV palette context is whether the luma palette is
            // present (spec section 8.3.2). This context could not be
            // re-verified against the spec text in this session (the
            // spec browser was unavailable); the tile smoke test is
            // the check that the walk stays aligned.
            let ctx = usize::from(mode.palette_size_y > 0);
            let has_palette = Self::symbol(&mut self.sym, &mut self.cdf.palette_uv_mode[ctx])?;
            if has_palette != 0 {
                let size_minus_2 =
                    Self::symbol(&mut self.sym, &mut self.cdf.palette_uv_size[bsize_ctx])?;
                mode.palette_size_uv = (size_minus_2 + 2) as u8;
                self.read_palette_colors_uv(mode)?;
            }
        }
        Ok(())
    }

    /// Palette-size context from the neighbours (spec 8.3.2).
    fn palette_ctx(&self, plane: usize) -> usize {
        let row = self.cur_row;
        let col = self.cur_col;
        let sizes = if plane == 0 {
            &self.palette_sizes_y
        } else {
            &self.palette_sizes_uv
        };
        let above = self.cur_avail_u && sizes[self.grid_index(row - 1, col)] > 0;
        let left = self.cur_avail_l && sizes[self.grid_index(row, col - 1)] > 0;
        usize::from(above) + usize::from(left)
    }

    /// Reads the luma palette colours (spec section 5.11.25).
    fn read_palette_colors_y(&mut self, mode: &mut Av1BlockMode) -> Result<()> {
        let size = usize::from(mode.palette_size_y);
        let cache = self.get_palette_cache(0);
        let bit_depth = u32::from(self.seq.color.bit_depth);
        let mut idx = 0usize;
        for &color in &cache {
            if idx >= size {
                break;
            }
            if self.literal(1)? != 0 {
                mode.palette_colors_y[idx] = color;
                idx += 1;
            }
        }
        if idx < size {
            mode.palette_colors_y[idx] = self.literal(bit_depth)? as u16;
            idx += 1;
        }
        let mut palette_bits = 0u32;
        if idx < size {
            let min_bits = bit_depth - 3;
            let extra = self.literal(2)?;
            palette_bits = min_bits + extra;
        }
        while idx < size {
            let delta = self.literal(palette_bits)?;
            let prev = u32::from(mode.palette_colors_y[idx - 1]);
            let value = prev + delta + 1;
            mode.palette_colors_y[idx] = value.min((1u32 << bit_depth) - 1) as u16;
            let range = (1u32 << bit_depth) - u32::from(mode.palette_colors_y[idx]) - 1;
            palette_bits = palette_bits.min(ceil_log2(range));
            idx += 1;
        }
        mode.palette_colors_y[..size].sort_unstable();
        Ok(())
    }

    /// Reads the chroma palette colours (spec section 5.11.25).
    fn read_palette_colors_uv(&mut self, mode: &mut Av1BlockMode) -> Result<()> {
        let size = usize::from(mode.palette_size_uv);
        let cache = self.get_palette_cache(1);
        let bit_depth = u32::from(self.seq.color.bit_depth);
        let mut idx = 0usize;
        for &color in &cache {
            if idx >= size {
                break;
            }
            if self.literal(1)? != 0 {
                mode.palette_colors_u[idx] = color;
                idx += 1;
            }
        }
        if idx < size {
            mode.palette_colors_u[idx] = self.literal(bit_depth)? as u16;
            idx += 1;
        }
        let mut palette_bits = 0u32;
        if idx < size {
            let min_bits = bit_depth - 3;
            let extra = self.literal(2)?;
            palette_bits = min_bits + extra;
        }
        while idx < size {
            let delta = self.literal(palette_bits)?;
            let prev = u32::from(mode.palette_colors_u[idx - 1]);
            let value = prev + delta;
            mode.palette_colors_u[idx] = value.min((1u32 << bit_depth) - 1) as u16;
            let range = (1u32 << bit_depth) - u32::from(mode.palette_colors_u[idx]);
            palette_bits = palette_bits.min(ceil_log2(range));
            idx += 1;
        }
        mode.palette_colors_u[..size].sort_unstable();
        if self.literal(1)? != 0 {
            let min_bits = bit_depth.saturating_sub(4);
            let extra = self.literal(2)?;
            let bits = min_bits + extra;
            let max_val = 1u32 << bit_depth;
            mode.palette_colors_v[0] = self.literal(bit_depth)? as u16;
            for idx in 1..size {
                let mut delta = self.literal(bits)? as i32;
                if delta != 0 {
                    let sign = self.literal(1)?;
                    if sign != 0 {
                        delta = -delta;
                    }
                }
                let prev = i32::from(mode.palette_colors_v[idx - 1]);
                let mut val = prev + delta;
                if val < 0 {
                    val += max_val as i32;
                }
                if val >= max_val as i32 {
                    val -= max_val as i32;
                }
                mode.palette_colors_v[idx] = val as u16;
            }
        } else {
            for idx in 0..size {
                mode.palette_colors_v[idx] = self.literal(bit_depth)? as u16;
            }
        }
        Ok(())
    }

    /// Merges the above and left palettes into a sorted cache (spec 5.11.25).
    fn get_palette_cache(&self, plane: usize) -> Vec<u16> {
        let row = self.cur_row;
        let col = self.cur_col;
        let (sizes, colors) = if plane == 0 {
            (&self.palette_sizes_y, &self.palette_colors_y)
        } else {
            (&self.palette_sizes_uv, &self.palette_colors_u)
        };
        let mut above_n = 0usize;
        if row > 0 && row * 4 % 64 != 0 {
            above_n = usize::from(sizes[self.grid_index(row - 1, col)]).min(8);
        }
        let mut left_n = 0usize;
        if self.cur_avail_l {
            left_n = usize::from(sizes[self.grid_index(row, col - 1)]).min(8);
        }
        let above_colors: &[u16] = if above_n > 0 {
            &colors[self.grid_index(row - 1, col)][..above_n]
        } else {
            &[]
        };
        let left_colors: &[u16] = if left_n > 0 {
            &colors[self.grid_index(row, col - 1)][..left_n]
        } else {
            &[]
        };
        let mut cache: Vec<u16> = Vec::new();
        let mut above_idx = 0usize;
        let mut left_idx = 0usize;
        while above_idx < above_n && left_idx < left_n {
            let above_c = above_colors[above_idx];
            let left_c = left_colors[left_idx];
            if left_c < above_c {
                if cache.last() != Some(&left_c) {
                    cache.push(left_c);
                }
                left_idx += 1;
            } else {
                if cache.last() != Some(&above_c) {
                    cache.push(above_c);
                }
                above_idx += 1;
                if left_c == above_c {
                    left_idx += 1;
                }
            }
        }
        while above_idx < above_n {
            let value = above_colors[above_idx];
            above_idx += 1;
            if cache.last() != Some(&value) {
                cache.push(value);
            }
        }
        while left_idx < left_n {
            let value = left_colors[left_idx];
            left_idx += 1;
            if cache.last() != Some(&value) {
                cache.push(value);
            }
        }
        cache
    }

    /// Reads the palette index maps (spec section 5.11.26).
    fn palette_tokens(&mut self, mode: &mut Av1BlockMode) -> Result<()> {
        let size = usize::from(mode.mi_size);
        let block_w = usize::from(NUM_4X4_BLOCKS_WIDE[size]) * 4;
        let block_h = usize::from(NUM_4X4_BLOCKS_HIGH[size]) * 4;
        let onscreen_w = block_w.min((self.mi_cols - self.cur_col) * 4);
        let onscreen_h = block_h.min((self.mi_rows - self.cur_row) * 4);
        if mode.palette_size_y > 0 {
            mode.color_map_y = self.read_palette_map(
                usize::from(mode.palette_size_y),
                block_w,
                block_h,
                onscreen_w,
                onscreen_h,
                true,
            )?;
        }
        if mode.palette_size_uv > 0 {
            let sub_x = usize::from(self.seq.color.subsampling_x);
            let sub_y = usize::from(self.seq.color.subsampling_y);
            let mut uv_w = block_w >> sub_x;
            let mut uv_h = block_h >> sub_y;
            let mut on_w = onscreen_w >> sub_x;
            let mut on_h = onscreen_h >> sub_y;
            if uv_w < 4 {
                uv_w += 2;
                on_w += 2;
            }
            if uv_h < 4 {
                uv_h += 2;
                on_h += 2;
            }
            mode.color_map_uv_width = uv_w as u8;
            mode.color_map_uv_height = uv_h as u8;
            mode.color_map_uv = self.read_palette_map(
                usize::from(mode.palette_size_uv),
                uv_w,
                uv_h,
                on_w,
                on_h,
                false,
            )?;
        }
        Ok(())
    }

    /// Reads one palette index map in diagonal order (spec 5.11.26).
    fn read_palette_map(
        &mut self,
        palette_size: usize,
        block_w: usize,
        block_h: usize,
        onscreen_w: usize,
        onscreen_h: usize,
        is_luma: bool,
    ) -> Result<Vec<u8>> {
        let mut map = vec![0u8; block_w * block_h];
        map[0] = self.read_ns(palette_size as u32)? as u8;
        for i in 1..onscreen_h + onscreen_w - 1 {
            let j_start = i.min(onscreen_w - 1);
            let j_end = (i + 1).saturating_sub(onscreen_h);
            let mut j = j_start;
            loop {
                let (ctx, order) = get_palette_color_context(&map, i - j, j, palette_size, block_w);
                let idx = if is_luma {
                    Self::symbol(
                        &mut self.sym,
                        &mut self.cdf.palette_y_color[palette_size - 2][ctx],
                    )?
                } else {
                    Self::symbol(
                        &mut self.sym,
                        &mut self.cdf.palette_uv_color[palette_size - 2][ctx],
                    )?
                };
                map[(i - j) * block_w + j] = order.get(idx as usize).copied().unwrap_or(0);
                if j == j_end {
                    break;
                }
                j -= 1;
            }
        }
        for i in 0..onscreen_h {
            for j in onscreen_w..block_w {
                map[i * block_w + j] = map[i * block_w + onscreen_w - 1];
            }
        }
        for i in onscreen_h..block_h {
            for j in 0..block_w {
                map[i * block_w + j] = map[(onscreen_h - 1) * block_w + j];
            }
        }
        Ok(map)
    }

    /// Reads filter-intra info (spec section 5.11.24).
    fn filter_intra_mode_info(&mut self, mode: &mut Av1BlockMode) -> Result<()> {
        let bw = usize::from(NUM_4X4_BLOCKS_WIDE[usize::from(mode.mi_size)]) * 4;
        let bh = usize::from(NUM_4X4_BLOCKS_HIGH[usize::from(mode.mi_size)]) * 4;
        if self.seq.enable_filter_intra
            && mode.y_mode == DC_PRED
            && mode.palette_size_y == 0
            && bw.max(bh) <= 32
        {
            let use_filter = Self::symbol(
                &mut self.sym,
                &mut self.cdf.filter_intra[usize::from(mode.mi_size)],
            )?;
            if use_filter != 0 {
                mode.use_filter_intra = true;
                mode.filter_intra_mode =
                    Self::symbol(&mut self.sym, &mut self.cdf.filter_intra_mode)? as u8;
            }
        }
        Ok(())
    }

    /// Reads the block transform size (spec section 5.11.21).
    fn read_block_tx_size(&mut self, mode: &mut Av1BlockMode) -> Result<()> {
        // Intra frames never take the variable-TX-size branch:
        // is_inter is 0, so !skip || !is_inter is always true.
        let tx_size = self.read_tx_size(mode, true)?;
        mode.tx_size = tx_size;
        let row = mode.mi_row as usize;
        let col = mode.mi_col as usize;
        let bw4 = usize::from(NUM_4X4_BLOCKS_WIDE[usize::from(mode.mi_size)]);
        let bh4 = usize::from(NUM_4X4_BLOCKS_HIGH[usize::from(mode.mi_size)]);
        for r in row..(row + bh4).min(self.mi_rows) {
            for c in col..(col + bw4).min(self.mi_cols) {
                let idx = self.grid_index(r, c);
                self.inter_tx_sizes[idx] = tx_size;
            }
        }
        Ok(())
    }

    /// Reads one transform size (spec section 5.11.20).
    fn read_tx_size(&mut self, mode: &Av1BlockMode, allow_select: bool) -> Result<u8> {
        if self.segment_lossless(mode.segment_id) {
            return Ok(TX_4X4);
        }
        let size = usize::from(mode.mi_size);
        let mut tx_size = MAX_TX_SIZE_RECT[size];
        if size > usize::from(BLOCK_4X4) && allow_select && self.header.tx_mode == TX_MODE_SELECT {
            let max_tx_depth = MAX_TX_DEPTH[size];
            let ctx = self.tx_depth_ctx();
            let depth = match max_tx_depth {
                4 => Self::symbol(&mut self.sym, &mut self.cdf.tx_64x64[ctx])?,
                3 => Self::symbol(&mut self.sym, &mut self.cdf.tx_32x32[ctx])?,
                2 => Self::symbol(&mut self.sym, &mut self.cdf.tx_16x16[ctx])?,
                _ => Self::symbol(&mut self.sym, &mut self.cdf.tx_8x8[ctx])?,
            };
            for _ in 0..depth {
                tx_size = SPLIT_TX_SIZE[usize::from(tx_size)];
            }
        }
        Ok(tx_size)
    }

    /// TX-depth context from the neighbours' transform sizes (spec 8.3.2).
    fn tx_depth_ctx(&self) -> usize {
        let max_rect = usize::from(MAX_TX_SIZE_RECT[self.cur_size]);
        let max_w = usize::from(TX_WIDTH[max_rect]);
        let max_h = usize::from(TX_HEIGHT[max_rect]);
        let above_w = if self.cur_avail_u {
            self.get_above_tx_width(self.cur_row, self.cur_col)
        } else {
            0
        };
        let left_h = if self.cur_avail_l {
            self.get_left_tx_height(self.cur_row, self.cur_col)
        } else {
            0
        };
        usize::from(above_w >= max_w) + usize::from(left_h >= max_h)
    }

    /// Transform width above a position (spec section 8.3.2).
    fn get_above_tx_width(&self, row: usize, col: usize) -> usize {
        if row == 0 {
            return 64;
        }
        if row == self.cur_row {
            if !self.cur_avail_u {
                return 64;
            }
            let above = self.grid_index(row - 1, col);
            if self.skips[above] && self.is_inters[above] {
                return usize::from(NUM_4X4_BLOCKS_WIDE[usize::from(self.mi_sizes[above])]) * 4;
            }
        }
        let above = self.grid_index(row - 1, col);
        usize::from(TX_WIDTH[usize::from(self.inter_tx_sizes[above])])
    }

    /// Transform height left of a position (spec section 8.3.2).
    fn get_left_tx_height(&self, row: usize, col: usize) -> usize {
        if col == 0 {
            return 64;
        }
        if col == self.cur_col {
            if !self.cur_avail_l {
                return 64;
            }
            let left = self.grid_index(row, col - 1);
            if self.skips[left] && self.is_inters[left] {
                return usize::from(NUM_4X4_BLOCKS_HIGH[usize::from(self.mi_sizes[left])]) * 4;
            }
        }
        let left = self.grid_index(row, col - 1);
        usize::from(TX_HEIGHT[usize::from(self.inter_tx_sizes[left])])
    }

    /// True when a segment feature is active (spec section 5.11.15).
    fn seg_feature_active(&self, segment_id: u8, feature: usize) -> bool {
        self.header.segmentation_enabled
            && self.header.seg_feature_enabled[usize::from(segment_id)][feature]
    }

    /// Quantizer index for one segment (spec section 7.11.3).
    fn qindex_for_segment(&self, segment_id: u8) -> i32 {
        let seg = usize::from(segment_id);
        if self.header.seg_feature_enabled[seg][SEG_LVL_ALT_Q] {
            (i32::from(self.header.base_q_idx) + self.header.seg_feature_data[seg][SEG_LVL_ALT_Q])
                .clamp(0, 255)
        } else {
            i32::from(self.header.base_q_idx)
        }
    }

    /// True when a segment is lossless (spec section 5.9.2).
    fn segment_lossless(&self, segment_id: u8) -> bool {
        self.qindex_for_segment(segment_id) == 0
            && self.header.delta_q_y_dc == 0
            && self.header.delta_q_u_ac == 0
            && self.header.delta_q_u_dc == 0
            && self.header.delta_q_v_ac == 0
            && self.header.delta_q_v_dc == 0
    }
}

/// Turns one palette colour CDF table into owned per-context rows.
fn palette_rows<const N: usize>(table: [[u16; N]; 5]) -> [Vec<u16>; 5] {
    std::array::from_fn(|i| table[i].to_vec())
}

/// Floor of the base-2 logarithm; 0 for an input of 0 (spec section 3).
fn floor_log2(mut x: u32) -> u32 {
    let mut s: u32 = 0;
    while x != 0 {
        x >>= 1;
        s += 1;
    }
    s.saturating_sub(1)
}

/// Ceiling of the base-2 logarithm; 0 for inputs below 2.
fn ceil_log2(value: u32) -> u32 {
    if value <= 1 {
        0
    } else {
        32 - (value - 1).leading_zeros()
    }
}

/// Probability of one symbol from a CDF row (spec section 8.2.3).
fn cdf_prob(row: &[u16], index: usize) -> u16 {
    let hi = row.get(index).copied().unwrap_or(0);
    let lo = if index == 0 {
        0
    } else {
        row.get(index - 1).copied().unwrap_or(0)
    };
    hi.saturating_sub(lo)
}

/// De-interleaves a segment id difference (spec section 5.11.9).
fn neg_deinterleave(diff: i32, reference: i32, max: i32) -> i32 {
    if reference == 0 {
        return diff;
    }
    if reference >= max - 1 {
        return max - diff - 1;
    }
    if 2 * reference < max {
        if diff <= 2 * reference {
            if diff & 1 != 0 {
                reference + ((diff + 1) >> 1)
            } else {
                reference - (diff >> 1)
            }
        } else {
            diff
        }
    } else if diff <= 2 * (max - reference - 1) {
        if diff & 1 != 0 {
            reference + ((diff + 1) >> 1)
        } else {
            reference - (diff >> 1)
        }
    } else {
        max - (diff + 1)
    }
}

/// True for the directional intra modes (spec section 5.11.16).
fn is_directional_mode(mode: u8) -> bool {
    (V_PRED..=D67_PRED).contains(&mode)
}

/// Palette colour context and ordering for one sample (spec 5.11.27).
///
/// Returns the CDF context and the colour order (best-scoring first).
/// The hash uses the three best scores after the selection sort, which
/// is why the scores are swapped alongside the order.
fn get_palette_color_context(
    map: &[u8],
    row: usize,
    col: usize,
    palette_size: usize,
    width: usize,
) -> (usize, [u8; PALETTE_COLORS]) {
    let mut scores = [0u8; PALETTE_COLORS];
    let mut order = [0u8, 1, 2, 3, 4, 5, 6, 7];
    if col > 0 {
        let neighbor = map[row * width + col - 1] as usize;
        scores[neighbor] += 2;
    }
    if row > 0 && col > 0 {
        let neighbor = map[(row - 1) * width + col - 1] as usize;
        scores[neighbor] += 1;
    }
    if row > 0 {
        let neighbor = map[(row - 1) * width + col] as usize;
        scores[neighbor] += 2;
    }
    for i in 0..PALETTE_NUM_NEIGHBORS {
        let mut max_score = scores[i];
        let mut max_idx = i;
        // The palette can be smaller than the neighbour count, so
        // this range is empty when i + 1 is past the palette; a
        // slice of the same bounds would panic there, so the
        // range loop stays.
        #[allow(clippy::needless_range_loop)]
        for j in i + 1..palette_size {
            if scores[j] > max_score {
                max_score = scores[j];
                max_idx = j;
            }
        }
        if max_idx != i {
            scores.swap(i, max_idx);
            order.swap(i, max_idx);
        }
    }
    let mut hash = 0usize;
    for i in 0..PALETTE_NUM_NEIGHBORS {
        hash += usize::from(scores[i]) * usize::from(PALETTE_COLOR_HASH_MULTIPLIERS[i]);
    }
    let ctx = PALETTE_COLOR_CONTEXT[hash];
    let ctx = if ctx < 0 { 0 } else { ctx as usize };
    (ctx, order)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid AV1 CDF row (spec section 8.2): at least three
    /// entries, strictly increasing probabilities, second-to-last
    /// entry 32768 (1.0 in 15-bit fixed point), last entry 0.
    /// A row that fails this check is either mistranscribed from
    /// spec section 9.3 or not a CDF at all.
    fn assert_cdf_row(row: &[u16]) {
        assert!(row.len() >= 3, "cdf row too short: {row:?}");
        for w in row[..row.len() - 1].windows(2) {
            assert!(w[0] < w[1], "cdf row not strictly increasing: {row:?}");
        }
        assert_eq!(row[row.len() - 2], 32768, "cdf row must end 32768: {row:?}");
        assert_eq!(row[row.len() - 1], 0, "cdf row must end 0: {row:?}");
    }

    fn assert_cdf_rows<const N: usize>(rows: &[[u16; N]]) {
        for row in rows {
            assert_cdf_row(row);
        }
    }

    #[test]
    fn default_mode_cdfs_are_valid() {
        assert_cdf_rows(&DEFAULT_INTRA_FRAME_Y_MODE_CDF.concat());
        assert_eq!(DEFAULT_INTRA_FRAME_Y_MODE_CDF.len(), 5);
        assert_cdf_rows(&DEFAULT_Y_MODE_CDF);
        assert_cdf_rows(&DEFAULT_UV_MODE_CFL_NOT_ALLOWED_CDF);
        assert_cdf_rows(&DEFAULT_UV_MODE_CFL_ALLOWED_CDF);
        assert_cdf_rows(&DEFAULT_ANGLE_DELTA_CDF);
        assert_cdf_row(&DEFAULT_INTRABC_CDF);
        assert_cdf_rows(&DEFAULT_PARTITION_W8_CDF);
        assert_cdf_rows(&DEFAULT_PARTITION_W16_CDF);
        assert_cdf_rows(&DEFAULT_PARTITION_W32_CDF);
        assert_cdf_rows(&DEFAULT_PARTITION_W64_CDF);
        assert_cdf_rows(&DEFAULT_PARTITION_W128_CDF);
        assert_cdf_rows(&DEFAULT_TX_8X8_CDF);
        assert_cdf_rows(&DEFAULT_TX_16X16_CDF);
        assert_cdf_rows(&DEFAULT_TX_32X32_CDF);
        assert_cdf_rows(&DEFAULT_TX_64X64_CDF);
        assert_eq!(DEFAULT_TXFM_SPLIT_CDF.len(), 21);
        assert_cdf_rows(&DEFAULT_TXFM_SPLIT_CDF);
        assert_cdf_row(&DEFAULT_FILTER_INTRA_MODE_CDF);
        assert_cdf_rows(&DEFAULT_FILTER_INTRA_CDF);
        assert_cdf_rows(&DEFAULT_SEGMENT_ID_CDF);
        assert_cdf_rows(&DEFAULT_SEGMENT_ID_PREDICTED_CDF);
        assert_cdf_rows(&DEFAULT_SKIP_MODE_CDF);
        assert_cdf_rows(&DEFAULT_SKIP_CDF);
        assert_cdf_row(&DEFAULT_DELTA_Q_CDF);
        assert_cdf_row(&DEFAULT_DELTA_LF_CDF);
        assert_cdf_rows(&DEFAULT_INTRA_TX_TYPE_SET1_CDF.concat());
        assert_cdf_rows(&DEFAULT_INTRA_TX_TYPE_SET2_CDF.concat());
        assert_cdf_rows(&DEFAULT_PALETTE_Y_SIZE_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_UV_SIZE_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_2_Y_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_3_Y_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_4_Y_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_5_Y_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_6_Y_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_7_Y_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_8_Y_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_2_UV_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_3_UV_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_4_UV_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_5_UV_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_6_UV_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_7_UV_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_SIZE_8_UV_COLOR_CDF);
        assert_cdf_rows(&DEFAULT_PALETTE_Y_MODE_CDF.concat());
        assert_cdf_rows(&DEFAULT_PALETTE_UV_MODE_CDF);
        assert_cdf_row(&DEFAULT_CFL_SIGN_CDF);
        assert_cdf_rows(&DEFAULT_CFL_ALPHA_CDF);
    }
}
