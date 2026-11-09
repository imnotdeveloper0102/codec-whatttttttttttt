//! AV1 intra reconstruction: dequantization, inverse transform, and
//! intra prediction (AV1 spec v1.0.0 sections 8.4, 8.6, 8.7).
//!
//! This module covers the pixel math for keyframe intra blocks only:
//! profile 0, 8-bit, 4:2:0. It takes decoded coefficients and decoded
//! mode info as inputs; it does not decode the bitstream (that is
//! `av1_tile`) and it does not apply the loop filter, CDEF, or loop
//! restoration, which are separate post-reconstruction stages. The
//! inverse transform is written here from the spec's butterfly
//! definition rather than reusing `mdct`, whose windowed MDCT is a
//! different transform.
//!
//! The dequantization tables, `Cos128_Lookup`, `Dr_Intra_Derivative`,
//! `Intra_Filter_Taps`, `Sm_Weights`, and `Intra_Edge_Kernel` are
//! transcribed from the spec (sections 8.6, 8.7, 9.2); the unit tests
//! below pin the spot values the spec text itself gives and check the
//! tables are monotonic where the spec's construction implies it, so a
//! transcription slip is caught here rather than in a pixel.

// The identifiers and casts in this module follow the spec's
// pseudocode one-for-one (single-letter names, `as` casts between
// the spec's integer types, long spec-shaped functions), so the
// pedantic lints for those are allowed here, as in
// `celt_reconstruct`. `dead_code` is temporary: nothing live calls
// this module yet; the reconstruction loop in `av1_reconstruct`
// will consume it once coefficient decode lands, and this allow
// goes away then.
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::too_many_arguments)]
#![allow(dead_code)]

use crate::av1_tile::{
    D67_PRED, DC_PRED, PAETH_PRED, SMOOTH_H_PRED, SMOOTH_PRED, SMOOTH_V_PRED, TX_16X32, TX_32X16,
    TX_32X32, TX_32X64, TX_4X4, TX_64X32, TX_64X64, TX_HEIGHT, TX_HEIGHT_LOG2, TX_WIDTH,
    TX_WIDTH_LOG2, V_PRED,
};
use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Transform size and transform type constants (spec section 3, 7.13)
// ---------------------------------------------------------------------------

/// Transform size: 4x16.
pub(crate) const TX_4X16: u8 = 13;
/// Transform size: 16x4.
pub(crate) const TX_16X4: u8 = 14;
/// Transform size: 8x32.
pub(crate) const TX_8X32: u8 = 15;
/// Transform size: 32x8.
pub(crate) const TX_32X8: u8 = 16;
/// Transform size: 16x64.
pub(crate) const TX_16X64: u8 = 17;
/// Transform size: 64x16.
pub(crate) const TX_64X16: u8 = 18;

/// Transform type: DCT in both directions.
pub(crate) const TX_TYPE_DCT_DCT: u8 = 0;
/// Transform type: ADST vertically, DCT horizontally.
pub(crate) const TX_TYPE_ADST_DCT: u8 = 1;
/// Transform type: DCT vertically, ADST horizontally.
pub(crate) const TX_TYPE_DCT_ADST: u8 = 2;
/// Transform type: ADST in both directions.
pub(crate) const TX_TYPE_ADST_ADST: u8 = 3;
/// Transform type: flipped ADST vertically, DCT horizontally.
pub(crate) const TX_TYPE_FLIPADST_DCT: u8 = 4;
/// Transform type: DCT vertically, flipped ADST horizontally.
pub(crate) const TX_TYPE_DCT_FLIPADST: u8 = 5;
/// Transform type: flipped ADST in both directions.
pub(crate) const TX_TYPE_FLIPADST_FLIPADST: u8 = 6;
/// Transform type: ADST vertically, flipped ADST horizontally.
pub(crate) const TX_TYPE_ADST_FLIPADST: u8 = 7;
/// Transform type: flipped ADST vertically, ADST horizontally.
pub(crate) const TX_TYPE_FLIPADST_ADST: u8 = 8;
/// Transform type: identity in both directions.
pub(crate) const TX_TYPE_IDTX: u8 = 9;
/// Transform type: DCT vertically only.
pub(crate) const TX_TYPE_V_DCT: u8 = 10;
/// Transform type: DCT horizontally only.
pub(crate) const TX_TYPE_H_DCT: u8 = 11;
/// Transform type: ADST vertically only.
pub(crate) const TX_TYPE_V_ADST: u8 = 12;
/// Transform type: ADST horizontally only.
pub(crate) const TX_TYPE_H_ADST: u8 = 13;
/// Transform type: flipped ADST vertically only.
pub(crate) const TX_TYPE_V_FLIPADST: u8 = 14;
/// Transform type: flipped ADST horizontally only.
pub(crate) const TX_TYPE_H_FLIPADST: u8 = 15;

/// Angle step in degrees per unit of angle delta (spec section 3).
pub(crate) const ANGLE_STEP: i32 = 3;
/// Scale bits for the filter-intra taps (spec section 3,
/// `INTRA_FILTER_SCALE_BITS`).
pub(crate) const INTRA_FILTER_SCALE_BITS: u32 = 4;

// ---------------------------------------------------------------------------
// Dequantization tables (spec section 8.6)
// ---------------------------------------------------------------------------

/// DC quantizer steps for 8-bit (`Dc_Qlookup[0]`, spec section 8.6).
pub(crate) const DC_QLOOKUP_8BIT: [u16; 256] = [
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

/// AC quantizer steps for 8-bit (`Ac_Qlookup[0]`, spec section 8.6).
pub(crate) const AC_QLOOKUP_8BIT: [u16; 256] = [
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
// Inverse transform tables (spec sections 8.7, 9.2)
// ---------------------------------------------------------------------------

/// `Cos128_Lookup` (spec section 8.7): 4096 * cos(angle * pi / 128),
/// rounded, for angle 0..=64.
pub(crate) const COS128_LOOKUP: [i32; 65] = [
    4096, 4095, 4091, 4085, 4076, 4065, 4052, 4036, 4017, 3996, 3973, 3948, 3920, 3889, 3857, 3822,
    3784, 3745, 3703, 3659, 3612, 3564, 3513, 3461, 3406, 3349, 3290, 3229, 3166, 3102, 3035, 2967,
    2896, 2824, 2751, 2675, 2598, 2520, 2440, 2359, 2276, 2191, 2106, 2019, 1931, 1842, 1751, 1660,
    1567, 1474, 1380, 1285, 1189, 1092, 995, 897, 799, 700, 601, 501, 401, 301, 201, 101, 0,
];

/// Row shift per transform size (spec section 8.7, `Transform_Row_Shift`).
pub(crate) const TRANSFORM_ROW_SHIFT: [u32; 19] =
    [0, 1, 2, 2, 2, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2];

// ---------------------------------------------------------------------------
// Intra prediction tables (spec sections 8.4, 9.2)
// ---------------------------------------------------------------------------

/// Prediction angle in degrees per intra mode (`Mode_To_Angle`,
/// spec section 9.2), indexed by the intra mode enum.
pub(crate) const MODE_TO_ANGLE: [i32; 13] = [0, 90, 180, 45, 135, 113, 157, 203, 67, 0, 0, 0, 0];

/// Directional prediction step per angle (`Dr_Intra_Derivative`,
/// spec section 9.2). Only the angles reachable from a mode angle
/// plus an angle delta are non-zero.
pub(crate) const DR_INTRA_DERIVATIVE: [i32; 90] = [
    0, 0, 0, 1023, 0, 0, 547, 0, 0, 372, 0, 0, 0, 0, 273, 0, 0, 215, 0, 0, 178, 0, 0, 151, 0, 0,
    132, 0, 0, 116, 0, 0, 102, 0, 0, 0, 90, 0, 0, 80, 0, 0, 71, 0, 0, 64, 0, 0, 57, 0, 0, 51, 0, 0,
    45, 0, 0, 0, 40, 0, 0, 35, 0, 0, 31, 0, 0, 27, 0, 0, 23, 0, 0, 19, 0, 0, 15, 0, 0, 0, 0, 11, 0,
    0, 7, 0, 0, 3, 0, 0,
];

/// Filter-intra taps (`Intra_Filter_Taps`, spec section 9.2):
/// [`filter_intra_mode`][4x2 sub-block position][neighbour sample].
pub(crate) const INTRA_FILTER_TAPS: [[[i32; 7]; 8]; 5] = [
    [
        [-6, 10, 0, 0, 0, 12, 0],
        [-5, 2, 10, 0, 0, 9, 0],
        [-3, 1, 1, 10, 0, 7, 0],
        [-3, 1, 1, 2, 10, 5, 0],
        [-4, 6, 0, 0, 0, 2, 12],
        [-3, 2, 6, 0, 0, 2, 9],
        [-3, 2, 2, 6, 0, 2, 7],
        [-3, 1, 2, 2, 6, 3, 5],
    ],
    [
        [-10, 16, 0, 0, 0, 10, 0],
        [-6, 0, 16, 0, 0, 6, 0],
        [-4, 0, 0, 16, 0, 4, 0],
        [-2, 0, 0, 0, 16, 2, 0],
        [-10, 16, 0, 0, 0, 0, 10],
        [-6, 0, 16, 0, 0, 0, 6],
        [-4, 0, 0, 16, 0, 0, 4],
        [-2, 0, 0, 0, 16, 0, 2],
    ],
    [
        [-8, 8, 0, 0, 0, 16, 0],
        [-8, 0, 8, 0, 0, 16, 0],
        [-8, 0, 0, 8, 0, 16, 0],
        [-8, 0, 0, 0, 8, 16, 0],
        [-4, 4, 0, 0, 0, 0, 16],
        [-4, 0, 4, 0, 0, 0, 16],
        [-4, 0, 0, 4, 0, 0, 16],
        [-4, 0, 0, 0, 4, 0, 16],
    ],
    [
        [-2, 8, 0, 0, 0, 10, 0],
        [-1, 3, 8, 0, 0, 6, 0],
        [-1, 2, 3, 8, 0, 4, 0],
        [0, 1, 2, 3, 8, 2, 0],
        [-1, 4, 0, 0, 0, 3, 10],
        [-1, 3, 4, 0, 0, 4, 6],
        [-1, 2, 3, 4, 0, 4, 4],
        [-1, 2, 2, 3, 4, 3, 3],
    ],
    [
        [-12, 14, 0, 0, 0, 14, 0],
        [-10, 0, 14, 0, 0, 12, 0],
        [-9, 0, 0, 14, 0, 11, 0],
        [-8, 0, 0, 0, 14, 10, 0],
        [-10, 12, 0, 0, 0, 0, 14],
        [-9, 1, 12, 0, 0, 0, 12],
        [-8, 0, 0, 12, 0, 1, 11],
        [-7, 0, 0, 1, 12, 1, 9],
    ],
];

/// Smooth prediction weights for a 4-wide/tall region (`Sm_Weights_Tx_4x4`,
/// spec section 9.2).
pub(crate) const SM_WEIGHTS_4: [i32; 4] = [255, 149, 85, 64];
/// Smooth prediction weights for an 8-wide/tall region (`Sm_Weights_Tx_8x8`,
/// spec section 9.2).
pub(crate) const SM_WEIGHTS_8: [i32; 8] = [255, 197, 146, 105, 73, 50, 37, 32];
/// Smooth prediction weights for a 16-wide/tall region (`Sm_Weights_Tx_16x16`,
/// spec section 9.2).
pub(crate) const SM_WEIGHTS_16: [i32; 16] = [
    255, 225, 196, 170, 145, 123, 102, 84, 68, 54, 43, 33, 26, 20, 17, 16,
];
/// Smooth prediction weights for a 32-wide/tall region (`Sm_Weights_Tx_32x32`,
/// spec section 9.2).
pub(crate) const SM_WEIGHTS_32: [i32; 32] = [
    255, 240, 225, 210, 196, 182, 169, 157, 145, 133, 122, 111, 101, 92, 83, 74, 66, 59, 52, 45,
    39, 34, 29, 25, 21, 17, 14, 12, 10, 9, 8, 8,
];
/// Smooth prediction weights for a 64-wide/tall region (`Sm_Weights_Tx_64x64`,
/// spec section 9.2).
pub(crate) const SM_WEIGHTS_64: [i32; 64] = [
    255, 248, 240, 233, 225, 218, 210, 203, 196, 189, 182, 176, 169, 163, 156, 150, 144, 138, 133,
    127, 121, 116, 111, 106, 101, 96, 91, 86, 82, 77, 73, 69, 65, 61, 57, 54, 50, 47, 44, 41, 38,
    35, 32, 29, 27, 25, 22, 20, 18, 16, 15, 13, 12, 10, 9, 8, 7, 6, 6, 5, 5, 4, 4, 4,
];

/// Edge filter kernels (`Intra_Edge_Kernel`, spec section 8.4):
/// [strength - 1][tap].
pub(crate) const INTRA_EDGE_KERNEL: [[i32; 5]; 3] =
    [[0, 4, 8, 4, 0], [0, 5, 6, 5, 0], [2, 4, 4, 4, 2]];

// ---------------------------------------------------------------------------
// Rounding and clipping helpers (spec section 3)
// ---------------------------------------------------------------------------

/// `Round2(x, n)`: `(x + (1 << (n - 1))) >> n` with an arithmetic shift,
/// or `x` when `n == 0` (spec section 3).
pub(crate) fn round2(x: i64, n: u32) -> i32 {
    if n == 0 {
        return x.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    }
    ((x + (1i64 << (n - 1))) >> n).clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// `Round2Signed(x, n)`: sign-symmetric rounding. The spec section 3
/// text for this function was not shown in the fetched copy; the
/// sign-symmetric form is the only form consistent with its uses
/// (filter intra, CFL), and it is validated here only by unit tests,
/// not yet end to end against ffmpeg.
pub(crate) fn round2_signed(x: i64, n: u32) -> i32 {
    if x >= 0 {
        round2(x, n)
    } else {
        -round2(-x, n)
    }
}

/// Clip a sample to the 8-bit range (spec section 3, `Clip1`).
pub(crate) fn clip1_8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Clip a value to an inclusive range (spec section 3, `Clip3`).
pub(crate) fn clip3(v: i64, lo: i64, hi: i64) -> i32 {
    v.clamp(lo, hi) as i32
}

// ---------------------------------------------------------------------------
// Dequantization (spec section 8.6)
// ---------------------------------------------------------------------------

/// DC quantizer step for an 8-bit qindex (spec section 8.6, `dc_q`).
pub(crate) fn dc_q(qindex: i32) -> u16 {
    DC_QLOOKUP_8BIT[qindex.clamp(0, 255) as usize]
}

/// AC quantizer step for an 8-bit qindex (spec section 8.6, `ac_q`).
pub(crate) fn ac_q(qindex: i32) -> u16 {
    AC_QLOOKUP_8BIT[qindex.clamp(0, 255) as usize]
}

/// Dequantization denominator for a transform size (spec section 8.6):
/// 2 for 16x32/32x16/32x32/16x64/64x16, 4 for 32x64/64x32/64x64, else 1.
pub(crate) fn dq_denom(tx_size: u8) -> i32 {
    match tx_size {
        TX_16X32 | TX_32X16 | TX_32X32 | TX_16X64 | TX_64X16 => 2,
        TX_32X64 | TX_64X32 | TX_64X64 => 4,
        _ => 1,
    }
}

/// Dequantize one coefficient (spec section 8.6, `reconstruct`): the
/// masked absolute value is divided by the denominator, then the sign
/// is reapplied and the result clipped to 16 bits.
pub(crate) fn dequantize_coeff(quant: i32, q: u16, tx_size: u8) -> i32 {
    let dq = quant as i64 * q as i64;
    let sign = if dq < 0 { -1i64 } else { 1i64 };
    let dq2 = sign * ((dq.abs() & 0xFF_FFFF) / dq_denom(tx_size) as i64);
    dq2.clamp(-32768, 32767) as i32
}

/// True when the transform type flips the residual vertically
/// (spec section 8.6, `reconstruct`).
pub(crate) fn flip_ud(plane_tx_type: u8) -> bool {
    matches!(
        plane_tx_type,
        TX_TYPE_FLIPADST_DCT
            | TX_TYPE_FLIPADST_ADST
            | TX_TYPE_V_FLIPADST
            | TX_TYPE_FLIPADST_FLIPADST
    )
}

/// True when the transform type flips the residual horizontally
/// (spec section 8.6, `reconstruct`).
pub(crate) fn flip_lr(plane_tx_type: u8) -> bool {
    matches!(
        plane_tx_type,
        TX_TYPE_DCT_FLIPADST
            | TX_TYPE_ADST_FLIPADST
            | TX_TYPE_H_FLIPADST
            | TX_TYPE_FLIPADST_FLIPADST
    )
}

/// Dequantize a transform block and add its residual to the plane
/// (spec section 8.6, `reconstruct`).
///
/// `plane` is the full frame plane with row stride `stride`; the block
/// covers rows `y..y+h` and columns `x..x+w`, where `w`/`h` are the
/// transform width/height. `quant` holds `tw * th` coefficients in
/// raster order (`tw = min(32, w)`, `th = min(32, h)`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn reconstruct_tx_block(
    plane: &mut [u8],
    stride: usize,
    frame_w: usize,
    frame_h: usize,
    x: usize,
    y: usize,
    quant: &[i32],
    tx_size: u8,
    plane_tx_type: u8,
    dc_q: u16,
    ac_q: u16,
    lossless: bool,
) -> Result<()> {
    if tx_size >= 19 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 transform size is out of range",
        });
    }
    let w = TX_WIDTH[tx_size as usize] as usize;
    let h = TX_HEIGHT[tx_size as usize] as usize;
    if x.checked_add(w).map_or(true, |e| e > frame_w)
        || y.checked_add(h).map_or(true, |e| e > frame_h)
    {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 transform block is outside the frame",
        });
    }
    let tw = w.min(32);
    let th = h.min(32);
    if quant.len() < tw * th {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 transform block has fewer coefficients than its size",
        });
    }
    if lossless && tx_size != TX_4X4 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 lossless inverse transform needs a 4x4 transform",
        });
    }
    let mut dequant = vec![0i32; tw * th];
    for i in 0..th {
        for j in 0..tw {
            let q = if i == 0 && j == 0 { dc_q } else { ac_q };
            dequant[i * tw + j] = dequantize_coeff(quant[i * tw + j], q, tx_size);
        }
    }
    let residual = inverse_transform_2d(&dequant, tx_size, plane_tx_type, lossless)?;
    let flip_up = flip_ud(plane_tx_type);
    let flip_left = flip_lr(plane_tx_type);
    for i in 0..h {
        for j in 0..w {
            let xx = if flip_left { w - j - 1 } else { j };
            let yy = if flip_up { h - i - 1 } else { i };
            let idx = (y + yy) * stride + (x + xx);
            let v = plane[idx] as i32 + residual[i * w + j];
            plane[idx] = clip1_8(v);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Inverse transform (spec section 8.7)
// ---------------------------------------------------------------------------

/// `cos128(angle)`: 4096 * cos(angle * pi / 128), via the lookup table
/// with the spec's symmetry branches (spec section 8.7).
pub(crate) fn cos128(angle: i32) -> i32 {
    let angle2 = angle & 255;
    if angle2 <= 64 {
        COS128_LOOKUP[angle2 as usize]
    } else if angle2 <= 128 {
        -COS128_LOOKUP[(128 - angle2) as usize]
    } else if angle2 <= 192 {
        -COS128_LOOKUP[(angle2 - 128) as usize]
    } else {
        COS128_LOOKUP[(256 - angle2) as usize]
    }
}

/// `sin128(angle) = cos128(angle - 64)` (spec section 8.7).
pub(crate) fn sin128(angle: i32) -> i32 {
    cos128(angle - 64)
}

/// Bit-reversal of `x` over `num_bits` bits (spec section 8.7, `brev`).
pub(crate) fn brev(num_bits: u32, x: usize) -> usize {
    let mut t = 0usize;
    for i in 0..num_bits {
        let bit = (x >> i) & 1;
        t += bit << (num_bits - 1 - i);
    }
    t
}

/// Butterfly `B(a, b, angle, flip, r)` (spec section 8.7). The spec's
/// special case for angle 32 + 64k is algebraically identical to the
/// general case in exact integer arithmetic, so only the general case
/// is implemented. When `flip` is set, the two outputs are exchanged
/// after the rotation.
pub(crate) fn butterfly_b(t: &mut [i32], a: usize, b: usize, angle: i32, flip: bool, _r: u32) {
    let c = cos128(angle) as i64;
    let s = sin128(angle) as i64;
    let ta = t[a] as i64;
    let tb = t[b] as i64;
    let x = ta * c - tb * s;
    let y = ta * s + tb * c;
    t[a] = round2(x, 12);
    t[b] = round2(y, 12);
    if flip {
        t.swap(a, b);
    }
}

/// Hadamard step `H(a, b, flip, r)` (spec section 8.7): the outputs are
/// clipped to `r` bits; when `flip` is set the pair is swapped first.
pub(crate) fn hadamard_h(t: &mut [i32], a: usize, b: usize, flip: bool, r: u32) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let x = t[a] as i64;
    let y = t[b] as i64;
    let lo = -(1i64 << (r - 1));
    let hi = (1i64 << (r - 1)) - 1;
    t[a] = (x + y).clamp(lo, hi) as i32;
    t[b] = (x - y).clamp(lo, hi) as i32;
}

/// Inverse DCT of length `1 << n` (spec section 8.7). The steps are
/// numbered as in the spec; the `..` ranges in the spec are inclusive.
pub(crate) fn inverse_dct(t: &mut [i32], n: u32, r: u32) -> Result<()> {
    if !(2..=6).contains(&n) || t.len() != 1usize << n {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 inverse DCT length is out of range",
        });
    }
    // 1. Permutation.
    let copy = t.to_vec();
    for i in 0..t.len() {
        t[i] = copy[brev(n, i)];
    }
    // 2. n == 6.
    if n == 6 {
        for i in 0..16 {
            butterfly_b(t, 32 + i, 63 - i, 63 - 4 * brev(4, i) as i32, false, r);
        }
    }
    // 3. n >= 5.
    if n >= 5 {
        for i in 0..8 {
            butterfly_b(
                t,
                16 + i,
                31 - i,
                6 + ((brev(3, 7 - i) as i32) << 3),
                false,
                r,
            );
        }
    }
    // 4. n == 6.
    if n == 6 {
        for i in 0..16 {
            hadamard_h(t, 32 + i * 2, 33 + i * 2, i & 1 == 1, r);
        }
    }
    // 5. n >= 4.
    if n >= 4 {
        for i in 0..4 {
            butterfly_b(
                t,
                8 + i,
                15 - i,
                12 + ((brev(2, 3 - i) as i32) << 4),
                false,
                r,
            );
        }
    }
    // 6. n >= 5.
    if n >= 5 {
        for i in 0..8 {
            hadamard_h(t, 16 + 2 * i, 17 + 2 * i, i & 1 == 1, r);
        }
    }
    // 7. n == 6.
    if n == 6 {
        for i in 0..4 {
            for j in 0..2 {
                butterfly_b(
                    t,
                    62 - i * 4 - j,
                    33 + i * 4 + j,
                    60 - 16 * brev(2, i) as i32 + 64 * j as i32,
                    true,
                    r,
                );
            }
        }
    }
    // 8. n >= 3.
    if n >= 3 {
        for i in 0..2 {
            butterfly_b(t, 4 + i, 7 - i, 56 - 32 * i as i32, false, r);
        }
    }
    // 9. n >= 4.
    if n >= 4 {
        for i in 0..4 {
            hadamard_h(t, 8 + 2 * i, 9 + 2 * i, i & 1 == 1, r);
        }
    }
    // 10. n >= 5.
    if n >= 5 {
        for i in 0..2 {
            for j in 0..2 {
                butterfly_b(
                    t,
                    30 - 4 * i - j,
                    17 + 4 * i + j,
                    24 + ((j as i32) << 6) + ((1 - i as i32) << 5),
                    true,
                    r,
                );
            }
        }
    }
    // 11. n == 6.
    if n == 6 {
        for i in 0..8 {
            for j in 0..2 {
                hadamard_h(t, 32 + i * 4 + j, 35 + i * 4 - j, i & 1 == 1, r);
            }
        }
    }
    // 12.
    for i in 0..2 {
        butterfly_b(t, 2 * i, 2 * i + 1, 32 + 16 * i as i32, i == 0, r);
    }
    // 13. n >= 3.
    if n >= 3 {
        for i in 0..2 {
            hadamard_h(t, 4 + 2 * i, 5 + 2 * i, i == 1, r);
        }
    }
    // 14. n >= 4.
    if n >= 4 {
        for i in 0..2 {
            butterfly_b(t, 14 - i, 9 + i, 48 + 64 * i as i32, true, r);
        }
    }
    // 15. n >= 5.
    if n >= 5 {
        for i in 0..4 {
            for j in 0..2 {
                hadamard_h(t, 16 + 4 * i + j, 19 + 4 * i - j, i & 1 == 1, r);
            }
        }
    }
    // 16. n == 6.
    if n == 6 {
        for i in 0..2 {
            for j in 0..4 {
                butterfly_b(
                    t,
                    61 - i * 8 - j,
                    34 + i * 8 + j,
                    56 - i as i32 * 32 + (j as i32 >> 1) * 64,
                    true,
                    r,
                );
            }
        }
    }
    // 17.
    for i in 0..2 {
        hadamard_h(t, i, 3 - i, false, r);
    }
    // 18. n >= 3.
    if n >= 3 {
        butterfly_b(t, 6, 5, 32, true, r);
    }
    // 19. n >= 4.
    if n >= 4 {
        for i in 0..2 {
            for j in 0..2 {
                hadamard_h(t, 8 + 4 * i + j, 11 + 4 * i - j, i == 1, r);
            }
        }
    }
    // 20. n >= 5.
    if n >= 5 {
        for i in 0..4 {
            butterfly_b(t, 29 - i, 18 + i, 48 + (i as i32 >> 1) * 64, true, r);
        }
    }
    // 21. n == 6.
    if n == 6 {
        for i in 0..4 {
            for j in 0..4 {
                hadamard_h(t, 32 + 8 * i + j, 39 + 8 * i - j, i & 1 == 1, r);
            }
        }
    }
    // 22. n >= 3.
    if n >= 3 {
        for i in 0..4 {
            hadamard_h(t, i, 7 - i, false, r);
        }
    }
    // 23. n >= 4.
    if n >= 4 {
        for i in 0..2 {
            butterfly_b(t, 13 - i, 10 + i, 32, true, r);
        }
    }
    // 24. n >= 5.
    if n >= 5 {
        for i in 0..2 {
            for j in 0..4 {
                hadamard_h(t, 16 + i * 8 + j, 23 + i * 8 - j, i == 1, r);
            }
        }
    }
    // 25. n == 6.
    if n == 6 {
        for i in 0..8 {
            butterfly_b(t, 59 - i, 36 + i, if i < 4 { 48 } else { 112 }, true, r);
        }
    }
    // 26. n >= 4.
    if n >= 4 {
        for i in 0..8 {
            hadamard_h(t, i, 15 - i, false, r);
        }
    }
    // 27. n >= 5.
    if n >= 5 {
        for i in 0..4 {
            butterfly_b(t, 27 - i, 20 + i, 32, true, r);
        }
    }
    // 28. n == 6.
    if n == 6 {
        for i in 0..8 {
            hadamard_h(t, 32 + i, 47 - i, false, r);
            hadamard_h(t, 48 + i, 63 - i, true, r);
        }
    }
    // 29. n >= 5.
    if n >= 5 {
        for i in 0..16 {
            hadamard_h(t, i, 31 - i, false, r);
        }
    }
    // 30. n == 6.
    if n == 6 {
        for i in 0..8 {
            butterfly_b(t, 55 - i, 40 + i, 32, true, r);
        }
    }
    // 31. n == 6.
    if n == 6 {
        for i in 0..32 {
            hadamard_h(t, i, 63 - i, false, r);
        }
    }
    Ok(())
}

/// Input permutation for the inverse ADST (spec section 8.7).
pub(crate) fn adst_input_perm(t: &mut [i32], n: u32) {
    let n0 = 1usize << n;
    let copy = t.to_vec();
    for (i, v) in t.iter_mut().enumerate().take(n0) {
        let idx = if i & 1 == 1 { i - 1 } else { n0 - i - 1 };
        *v = copy[idx];
    }
}

/// Output permutation for the inverse ADST (spec section 8.7).
pub(crate) fn adst_output_perm(t: &mut [i32], n: u32) {
    let n0 = 1usize << n;
    let copy = t.to_vec();
    for (i, v) in t.iter_mut().enumerate().take(n0) {
        let a = (i >> 3) & 1;
        let b = ((i >> 2) & 1) ^ ((i >> 3) & 1);
        let c = ((i >> 1) & 1) ^ ((i >> 2) & 1);
        let d = (i & 1) ^ ((i >> 1) & 1);
        let idx = ((d << 3) | (c << 2) | (b << 1) | a) >> (4 - n);
        *v = if i & 1 == 1 { -copy[idx] } else { copy[idx] };
    }
}

/// Inverse ADST of length 4 (spec section 8.7, `adst4`).
pub(crate) fn inverse_adst4(t: &mut [i32]) {
    const SINPI_1_9: i64 = 1321;
    const SINPI_2_9: i64 = 2482;
    const SINPI_3_9: i64 = 3344;
    const SINPI_4_9: i64 = 3803;
    let mut s = [0i64; 7];
    s[0] = SINPI_1_9 * t[0] as i64;
    s[1] = SINPI_2_9 * t[0] as i64;
    s[2] = SINPI_3_9 * t[1] as i64;
    s[3] = SINPI_4_9 * t[2] as i64;
    s[4] = SINPI_1_9 * t[2] as i64;
    s[5] = SINPI_2_9 * t[3] as i64;
    s[6] = SINPI_4_9 * t[3] as i64;
    let a7 = t[0] as i64 - t[2] as i64;
    let b7 = a7 + t[3] as i64;
    s[0] += s[3];
    s[1] -= s[4];
    s[3] = s[2];
    s[2] = SINPI_3_9 * b7;
    s[0] += s[5];
    s[1] -= s[6];
    let mut x = [0i64; 4];
    x[0] = s[0] + s[3];
    x[1] = s[1] + s[3];
    x[2] = s[2];
    x[3] = s[0] + s[1];
    x[3] -= s[3];
    for i in 0..4 {
        t[i] = round2(x[i], 12);
    }
}

/// Inverse ADST of length 8 (spec section 8.7, `adst8`).
///
/// NOTE — open discrepancy, 2026-11-09: this function and
/// `inverse_adst16` are implemented verbatim from the spec
/// (sections 7.13.2.4/7.13.2.5/7.13.2.7/7.13.2.8; every step,
/// permutation, and butterfly angle re-checked against the spec
/// HTML twice, and the (0,0) entry hand-traced). The resulting
/// matrix is a DST-IV-family matrix: column 0 of the 8-point
/// transform is [401, 1189, 1930, 2598, 3165, 3612, 3919, 4076],
/// i.e. 4096*sin((2k+1)*pi/32). The textbook DST-VII inverse
/// instead predicts [516, 1015, 1479, 1893, 2242, 2515, 2703,
/// 2798] (4096*sqrt(8/17)*sin((k+1)*pi/17)). ADST4 matches the
/// DST-VII model exactly, so the family is DST-VII at length 4
/// but the length-8/16 factorization provably cannot produce the
/// DST-VII matrix: its (0,0) entry is 401, a cos128 value, while
/// the DST-VII model needs 516, which no integer cos128 angle
/// yields. The spec (v1.0.0 with Errata 1) states it was
/// validated against libaom, so the factorization is kept as
/// written and the DST-VII model test is ignored, not deleted.
/// The arbiter is a pixel comparison against ffmpeg/dav1d once
/// coefficient decode exists; until then, ADST 8x8/16x16 blocks
/// must be treated as unverified.
pub(crate) fn inverse_adst8(t: &mut [i32], r: u32) {
    adst_input_perm(t, 3);
    for i in 0..4 {
        butterfly_b(t, 2 * i, 2 * i + 1, 60 - 16 * i as i32, true, r);
    }
    for i in 0..4 {
        hadamard_h(t, i, 4 + i, false, r);
    }
    for i in 0..2 {
        butterfly_b(t, 4 + 3 * i, 5 + i, 48 - 32 * i as i32, true, r);
    }
    for i in 0..2 {
        for j in 0..2 {
            hadamard_h(t, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..2 {
        butterfly_b(t, 2 + 4 * i, 3 + 4 * i, 32, true, r);
    }
    adst_output_perm(t, 3);
}

/// Inverse ADST of length 16 (spec section 8.7, `adst16`).
pub(crate) fn inverse_adst16(t: &mut [i32], r: u32) {
    adst_input_perm(t, 4);
    for i in 0..8 {
        butterfly_b(t, 2 * i, 2 * i + 1, 62 - 8 * i as i32, true, r);
    }
    for i in 0..8 {
        hadamard_h(t, i, 8 + i, false, r);
    }
    for i in 0..2 {
        butterfly_b(t, 8 + 2 * i, 9 + 2 * i, 56 - 32 * i as i32, true, r);
        butterfly_b(t, 13 + 2 * i, 12 + 2 * i, 8 + 32 * i as i32, true, r);
    }
    for i in 0..4 {
        for j in 0..2 {
            hadamard_h(t, 8 * j + i, 4 + 8 * j + i, false, r);
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            butterfly_b(
                t,
                4 + 8 * j + 3 * i,
                5 + 8 * j + i,
                48 - 32 * i as i32,
                true,
                r,
            );
        }
    }
    for i in 0..2 {
        for j in 0..4 {
            hadamard_h(t, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..4 {
        butterfly_b(t, 2 + 4 * i, 3 + 4 * i, 32, true, r);
    }
    adst_output_perm(t, 4);
}

/// Inverse ADST of length `1 << n` for `n` in 2..=4 (spec section 8.7).
/// Longer transforms never use ADST in a conformant stream (the
/// transform sets for 32x32 and above are DCT-only), so other lengths
/// are an error rather than a silent no-op.
pub(crate) fn inverse_adst(t: &mut [i32], n: u32, r: u32) -> Result<()> {
    match n {
        2 => inverse_adst4(t),
        3 => inverse_adst8(t, r),
        4 => inverse_adst16(t, r),
        _ => {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 inverse ADST length is out of range",
            });
        }
    }
    Ok(())
}

/// Walsh-Hadamard transform of length 4 (spec section 8.7, `wht`).
pub(crate) fn inverse_wht(t: &mut [i32], shift: u32) {
    let a = t[0] >> shift;
    let c = t[1] >> shift;
    let d = t[2] >> shift;
    let b = t[3] >> shift;
    let a = a + c;
    let d = d - b;
    let e = (a - d) >> 1;
    let b = e - b;
    let c = e - c;
    let a = a - b;
    let d = d + c;
    t[0] = a;
    t[1] = b;
    t[2] = c;
    t[3] = d;
}

/// Inverse identity transform of length `1 << n` for `n` in 2..=5
/// (spec section 8.7).
pub(crate) fn inverse_identity(t: &mut [i32], n: u32) -> Result<()> {
    match n {
        2 => {
            for v in t.iter_mut() {
                *v = round2(*v as i64 * 5793, 12);
            }
        }
        3 => {
            for v in t.iter_mut() {
                *v *= 2;
            }
        }
        4 => {
            for v in t.iter_mut() {
                *v = round2(*v as i64 * 11586, 12);
            }
        }
        5 => {
            for v in t.iter_mut() {
                *v *= 4;
            }
        }
        _ => {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 inverse identity length is out of range",
            });
        }
    }
    Ok(())
}

/// 2D inverse transform (spec section 8.7, `inverse transform`).
/// `dequant` holds `tw * th` dequantized coefficients in raster order
/// (`tw = min(32, w)`, `th = min(32, h)`); the result holds `w * h`
/// residuals in raster order.
pub(crate) fn inverse_transform_2d(
    dequant: &[i32],
    tx_size: u8,
    plane_tx_type: u8,
    lossless: bool,
) -> Result<Vec<i32>> {
    const COL_SHIFT: u32 = 4;
    // 8-bit: rowClampRange = BitDepth + 8 = 16, colClampRange =
    // max(BitDepth + 6, 16) = 16.
    const CLAMP_RANGE: u32 = 16;
    if tx_size >= 19 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 transform size is out of range",
        });
    }
    let log2_w = TX_WIDTH_LOG2[tx_size as usize] as u32;
    let log2_h = TX_HEIGHT_LOG2[tx_size as usize] as u32;
    let w = 1usize << log2_w;
    let h = 1usize << log2_h;
    let tw = w.min(32);
    let th = h.min(32);
    if dequant.len() < tw * th {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 transform block has fewer coefficients than its size",
        });
    }
    if lossless && (w != 4 || h != 4) {
        // The Walsh-Hadamard transform is length 4, so a lossless
        // transform block is always 4x4 (spec section 8.7).
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 lossless inverse transform needs a 4x4 transform",
        });
    }
    let row_shift = if lossless {
        0
    } else {
        TRANSFORM_ROW_SHIFT[tx_size as usize]
    };
    let mut residual = vec![0i32; w * h];
    // Row transforms.
    for i in 0..h {
        let mut row = vec![0i32; w];
        for j in 0..w {
            if i < th && j < tw {
                row[j] = dequant[i * tw + j];
            }
        }
        if (log2_w as i32 - log2_h as i32).abs() == 1 {
            for v in &mut row {
                *v = round2(*v as i64 * 2896, 12);
            }
        }
        if lossless {
            inverse_wht(&mut row, 2);
        } else if matches!(
            plane_tx_type,
            TX_TYPE_DCT_DCT | TX_TYPE_ADST_DCT | TX_TYPE_FLIPADST_DCT | TX_TYPE_H_DCT
        ) {
            inverse_dct(&mut row, log2_w, CLAMP_RANGE)?;
        } else if matches!(
            plane_tx_type,
            TX_TYPE_DCT_ADST
                | TX_TYPE_ADST_ADST
                | TX_TYPE_DCT_FLIPADST
                | TX_TYPE_FLIPADST_FLIPADST
                | TX_TYPE_ADST_FLIPADST
                | TX_TYPE_FLIPADST_ADST
                | TX_TYPE_H_ADST
                | TX_TYPE_H_FLIPADST
        ) {
            inverse_adst(&mut row, log2_w, CLAMP_RANGE)?;
        } else if plane_tx_type == TX_TYPE_IDTX {
            inverse_identity(&mut row, log2_w)?;
        } else {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 transform type is out of range",
            });
        }
        for j in 0..w {
            residual[i * w + j] = round2(row[j] as i64, row_shift);
        }
    }
    // Clip between the row and column passes.
    let lo = -(1i32 << (CLAMP_RANGE - 1));
    let hi = (1i32 << (CLAMP_RANGE - 1)) - 1;
    for v in &mut residual {
        *v = (*v).clamp(lo, hi);
    }
    // Column transforms.
    for j in 0..w {
        let mut col = vec![0i32; h];
        for i in 0..h {
            col[i] = residual[i * w + j];
        }
        if lossless {
            inverse_wht(&mut col, 0);
        } else if matches!(
            plane_tx_type,
            TX_TYPE_DCT_DCT | TX_TYPE_DCT_ADST | TX_TYPE_DCT_FLIPADST | TX_TYPE_V_DCT
        ) {
            inverse_dct(&mut col, log2_h, CLAMP_RANGE)?;
        } else if matches!(
            plane_tx_type,
            TX_TYPE_ADST_DCT
                | TX_TYPE_ADST_ADST
                | TX_TYPE_FLIPADST_DCT
                | TX_TYPE_FLIPADST_FLIPADST
                | TX_TYPE_ADST_FLIPADST
                | TX_TYPE_FLIPADST_ADST
                | TX_TYPE_V_ADST
                | TX_TYPE_V_FLIPADST
        ) {
            inverse_adst(&mut col, log2_h, CLAMP_RANGE)?;
        } else if plane_tx_type == TX_TYPE_IDTX {
            inverse_identity(&mut col, log2_h)?;
        } else {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 transform type is out of range",
            });
        }
        for i in 0..h {
            residual[i * w + j] = round2(col[i] as i64, COL_SHIFT);
        }
    }
    Ok(residual)
}

// ---------------------------------------------------------------------------
// Intra prediction (spec section 8.4)
// ---------------------------------------------------------------------------

/// Whether a mode is directional (spec section 8.4, `is_directional_mode`).
pub(crate) fn is_directional_mode(mode: u8) -> bool {
    (V_PRED..=D67_PRED).contains(&mode)
}

/// Smooth weights for a length (spec section 8.4, `smWeights`): the
/// table for `log2(len)`, with the 4x4 table used for smaller lengths.
pub(crate) fn sm_weights(log2_len: u32) -> &'static [i32] {
    match log2_len {
        0..=2 => &SM_WEIGHTS_4,
        3 => &SM_WEIGHTS_8,
        4 => &SM_WEIGHTS_16,
        5 => &SM_WEIGHTS_32,
        _ => &SM_WEIGHTS_64,
    }
}

/// Inputs to one intra prediction block (spec section 8.4, `predict_intra`).
///
/// `plane` is the full frame plane being written; the block covers
/// rows `y..y+h` and columns `x..x+w`. Neighbours are read from the
/// already-reconstructed samples of the same plane, so the caller
/// must invoke this in raster order within a prediction block.
/// `tile_x0/tile_y0/tile_x1/tile_y1` bound the tile in plane samples;
/// prediction never reads across a tile boundary.
#[allow(clippy::too_many_arguments)]
pub(crate) struct IntraPred<'a> {
    pub plane: &'a mut [u8],
    pub stride: usize,
    pub frame_w: usize,
    pub frame_h: usize,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub mode: u8,
    pub angle_delta: i32,
    pub use_filter_intra: bool,
    pub filter_intra_mode: u8,
    pub filter_type: u8,
    pub enable_intra_edge_filter: bool,
    pub tile_x0: usize,
    pub tile_y0: usize,
    pub tile_x1: usize,
    pub tile_y1: usize,
}

/// Run intra prediction for one transform block and write the
/// prediction into the plane (spec section 8.4).
///
/// `mode == UV_CFL_PRED` is rejected here: CFL prediction needs the
/// luma plane as well and is applied by `apply_cfl` after this
/// function has written the DC prediction.
pub(crate) fn predict_intra_block(p: IntraPred<'_>) -> Result<()> {
    let IntraPred {
        plane,
        stride,
        frame_w,
        frame_h,
        x,
        y,
        w,
        h,
        mode,
        angle_delta,
        use_filter_intra,
        filter_intra_mode,
        filter_type,
        enable_intra_edge_filter,
        tile_x0,
        tile_y0,
        tile_x1,
        tile_y1,
    } = p;
    if !matches!(w, 4 | 8 | 16 | 32 | 64) || !matches!(h, 4 | 8 | 16 | 32 | 64) {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 intra prediction block size is not a power of two",
        });
    }
    if mode > PAETH_PRED {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 intra prediction mode is out of range",
        });
    }
    if !(-3..=3).contains(&angle_delta) {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 intra angle delta is out of range",
        });
    }
    if use_filter_intra && filter_intra_mode >= 5 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 filter intra mode is out of range",
        });
    }
    if filter_type > 1 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 intra filter type is out of range",
        });
    }
    if x.checked_add(w).map_or(true, |e| e > frame_w)
        || y.checked_add(h).map_or(true, |e| e > frame_h)
        || x < tile_x0
        || y < tile_y0
        || x + w > tile_x1
        || y + h > tile_y1
    {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 intra prediction block is outside the frame or tile",
        });
    }
    let have_above = y > 0;
    let have_left = x > 0;
    let have_above_right = y > 0 && x + w < frame_w && (y - 1 < tile_y0 || x + 2 * w <= tile_x1);
    let have_below_left = x > 0 && y + h < frame_h && (x - 1 < tile_x0 || y + 2 * h <= tile_y1);
    let max_x = frame_w - 1;
    let max_y = frame_h - 1;
    // Neighbour arrays are indexed from -2 so the upsampled range
    // (-2 .. 2*(w+h)-2) is addressable; index 2 is position -1.
    let len = 2 * (w + h) + 2;
    let mut above_row = vec![0i32; len];
    let mut left_col = vec![0i32; len];
    let at = |arr: &[i32], i: i32| -> i32 { arr[(i + 2) as usize] };
    if have_above {
        let above_limit = max_x.min(x + (if have_above_right { 2 * w } else { w }) - 1);
        for i in 0..(w + h) as i32 {
            above_row[(i + 2) as usize] =
                plane[(y - 1) * stride + above_limit.min(x + i as usize)] as i32;
        }
    } else if have_left {
        for i in 0..(w + h) as i32 {
            above_row[(i + 2) as usize] = plane[y * stride + (x - 1)] as i32;
        }
    } else {
        for i in 0..(w + h) as i32 {
            above_row[(i + 2) as usize] = 127;
        }
    }
    if have_left {
        let left_limit = max_y.min(y + (if have_below_left { 2 * h } else { h }) - 1);
        for i in 0..(w + h) as i32 {
            left_col[(i + 2) as usize] =
                plane[left_limit.min(y + i as usize) * stride + (x - 1)] as i32;
        }
    } else if have_above {
        for i in 0..(w + h) as i32 {
            left_col[(i + 2) as usize] = plane[(y - 1) * stride + x] as i32;
        }
    } else {
        for i in 0..(w + h) as i32 {
            left_col[(i + 2) as usize] = 129;
        }
    }
    // Position -1 (the corner).
    let corner = if have_above && have_left {
        plane[(y - 1) * stride + (x - 1)] as i32
    } else if have_above {
        plane[(y - 1) * stride + x] as i32
    } else if have_left {
        plane[y * stride + (x - 1)] as i32
    } else {
        128
    };
    above_row[1] = corner;
    left_col[1] = corner;

    let mut pred = vec![0i32; w * h];
    if use_filter_intra {
        predict_recursive(&mut pred, w, h, &above_row, &left_col, filter_intra_mode);
    } else if is_directional_mode(mode) {
        predict_directional(
            &mut pred,
            w,
            h,
            &mut above_row,
            &mut left_col,
            mode,
            angle_delta,
            filter_type,
            enable_intra_edge_filter,
            max_x,
            max_y,
            x,
            y,
        )?;
    } else if mode == DC_PRED {
        predict_dc(
            &mut pred, w, h, &above_row, &left_col, have_above, have_left,
        );
    } else if matches!(mode, SMOOTH_PRED | SMOOTH_V_PRED | SMOOTH_H_PRED) {
        predict_smooth(&mut pred, w, h, &above_row, &left_col, mode);
    } else {
        // PAETH_PRED.
        for i in 0..h {
            for j in 0..w {
                let base = at(&above_row, j as i32) + at(&left_col, i as i32) - at(&above_row, -1);
                let p_left = (base - at(&left_col, i as i32)).abs();
                let p_top = (base - at(&above_row, j as i32)).abs();
                let p_top_left = (base - at(&above_row, -1)).abs();
                pred[i * w + j] = if p_left <= p_top && p_left <= p_top_left {
                    at(&left_col, i as i32)
                } else if p_top <= p_top_left {
                    at(&above_row, j as i32)
                } else {
                    at(&above_row, -1)
                };
            }
        }
    }
    for i in 0..h {
        for j in 0..w {
            plane[(y + i) * stride + (x + j)] = clip1_8(pred[i * w + j]);
        }
    }
    Ok(())
}

/// Filter-intra (recursive) prediction (spec section 8.4).
fn predict_recursive(
    pred: &mut [i32],
    w: usize,
    h: usize,
    above_row: &[i32],
    left_col: &[i32],
    filter_intra_mode: u8,
) {
    let at = |arr: &[i32], i: i32| -> i32 { arr[(i + 2) as usize] };
    let w4 = w >> 2;
    let h2 = h >> 1;
    for i2 in 0..h2 {
        for j4 in 0..w4 {
            let mut p = [0i32; 7];
            for i in 0..7 {
                if i < 5 {
                    if i2 == 0 {
                        p[i] = at(above_row, ((j4 << 2) + i) as i32 - 1);
                    } else if j4 == 0 && i == 0 {
                        p[i] = at(left_col, ((i2 << 1) - 1) as i32);
                    } else {
                        p[i] = pred[((i2 << 1) - 1) * w + (j4 << 2) + i - 1];
                    }
                } else if j4 == 0 {
                    p[i] = at(left_col, ((i2 << 1) + i - 5) as i32);
                } else {
                    p[i] = pred[((i2 << 1) + i - 5) * w + (j4 << 2) - 1];
                }
            }
            for i1 in 0..2 {
                for j1 in 0..4 {
                    let mut pr = 0i64;
                    for i in 0..7 {
                        pr += INTRA_FILTER_TAPS[filter_intra_mode as usize][(i1 << 2) + j1][i]
                            as i64
                            * p[i] as i64;
                    }
                    pred[((i2 << 1) + i1) * w + (j4 << 2) + j1] =
                        round2_signed(pr, INTRA_FILTER_SCALE_BITS).clamp(0, 255);
                }
            }
        }
    }
}

/// DC prediction (spec section 8.4). The both-neighbours average is a
/// plain integer division, not a clip; the one-neighbour averages are
/// clipped to 8 bits.
fn predict_dc(
    pred: &mut [i32],
    w: usize,
    h: usize,
    above_row: &[i32],
    left_col: &[i32],
    have_above: bool,
    have_left: bool,
) {
    let at = |arr: &[i32], i: i32| -> i32 { arr[(i + 2) as usize] };
    let avg = if have_above && have_left {
        let mut sum = 0i32;
        for i in 0..h {
            sum += at(left_col, i as i32);
        }
        for j in 0..w {
            sum += at(above_row, j as i32);
        }
        sum += ((w + h) >> 1) as i32;
        sum / (w + h) as i32
    } else if have_left {
        let mut sum = 0i32;
        for i in 0..h {
            sum += at(left_col, i as i32);
        }
        clip1_8((sum + (h >> 1) as i32) >> (h as u32).ilog2()) as i32
    } else if have_above {
        let mut sum = 0i32;
        for j in 0..w {
            sum += at(above_row, j as i32);
        }
        clip1_8((sum + (w >> 1) as i32) >> (w as u32).ilog2()) as i32
    } else {
        128
    };
    for v in pred.iter_mut() {
        *v = avg;
    }
}

/// Smooth prediction (spec section 8.4).
fn predict_smooth(
    pred: &mut [i32],
    w: usize,
    h: usize,
    above_row: &[i32],
    left_col: &[i32],
    mode: u8,
) {
    let at = |arr: &[i32], i: i32| -> i32 { arr[(i + 2) as usize] };
    let weights_x = sm_weights((w as u32).ilog2());
    let weights_y = sm_weights((h as u32).ilog2());
    for i in 0..h {
        for j in 0..w {
            let v = match mode {
                SMOOTH_PRED => {
                    weights_y[i] * at(above_row, j as i32)
                        + (256 - weights_y[i]) * at(left_col, (h - 1) as i32)
                        + weights_x[j] * at(left_col, i as i32)
                        + (256 - weights_x[j]) * at(above_row, (w - 1) as i32)
                }
                SMOOTH_V_PRED => {
                    weights_y[i] * at(above_row, j as i32)
                        + (256 - weights_y[i]) * at(left_col, (h - 1) as i32)
                }
                _ => {
                    // SMOOTH_H_PRED.
                    weights_x[j] * at(left_col, i as i32)
                        + (256 - weights_x[j]) * at(above_row, (w - 1) as i32)
                }
            };
            let shift = if mode == SMOOTH_PRED { 9 } else { 8 };
            pred[i * w + j] = round2(v as i64, shift);
        }
    }
}

/// Edge filter strength for a directional prediction (spec section 8.4,
/// `edge_filter_strength`).
fn edge_filter_strength(w: usize, h: usize, filter_type: u8, d: i32) -> i32 {
    let d = d.unsigned_abs() as i32;
    let blk_wh = (w + h) as i32;
    // The spec lists the <=12 and <=16 rows for filter type 0
    // separately, but both use the same threshold (40 -> 1), so
    // they are one branch here.
    if filter_type == 0 {
        if blk_wh <= 8 {
            i32::from(d >= 56)
        } else if blk_wh <= 16 {
            i32::from(d >= 40)
        } else if blk_wh <= 24 {
            if d >= 32 {
                3
            } else if d >= 16 {
                2
            } else {
                i32::from(d >= 8)
            }
        } else if blk_wh <= 32 {
            if d >= 32 {
                3
            } else if d >= 4 {
                2
            } else {
                1
            }
        } else {
            3
        }
    } else if blk_wh <= 8 {
        if d >= 64 {
            2
        } else {
            i32::from(d >= 40)
        }
    } else if blk_wh <= 16 {
        if d >= 48 {
            2
        } else {
            i32::from(d >= 20)
        }
    } else if blk_wh <= 24 {
        i32::from(d >= 4) * 3
    } else {
        3
    }
}

/// Edge upsample selection for a directional prediction (spec section 8.4,
/// `edge_upsample_selection`).
fn edge_upsample_selection(w: usize, h: usize, filter_type: u8, d: i32) -> bool {
    let d = d.unsigned_abs() as i32;
    if d <= 0 || d >= 40 {
        return false;
    }
    let blk_wh = (w + h) as i32;
    if filter_type == 0 {
        blk_wh <= 16
    } else {
        blk_wh <= 8
    }
}

/// Edge upsample in place (spec section 8.4, `edge_upsample`).
/// `buf` is indexed from -2; `num_px` samples at positions
/// -1..num_px-1 are the input, and positions -2..2*num_px-2 are the
/// output.
fn edge_upsample(buf: &mut [i32], num_px: usize) {
    // dup[0] is position -1; dup[k] is position k-2 for k = 1..num_px+1;
    // dup[num_px+2] repeats position num_px-1 (spec section 8.4).
    let mut dup = vec![0i32; num_px + 3];
    dup[0] = buf[1];
    dup[1..num_px + 2].copy_from_slice(&buf[1..num_px + 2]);
    dup[num_px + 2] = buf[num_px + 1];
    buf[0] = dup[0];
    for i in 0..num_px {
        let s = -dup[i] + 9 * dup[i + 1] + 9 * dup[i + 2] - dup[i + 3];
        let s = clip1_8(round2(s as i64, 4));
        buf[2 * i + 1] = s as i32;
        buf[2 * i + 2] = dup[i + 2];
    }
}

/// Edge filter in place (spec section 8.4, `edge_filter`).
/// `buf` is indexed from -1; `sz` samples at positions -1..sz-1 are
/// filtered with the kernel for `strength`.
fn edge_filter(buf: &mut [i32], sz: usize, strength: i32) {
    if strength == 0 {
        return;
    }
    let mut edge = vec![0i32; sz];
    edge.copy_from_slice(&buf[1..=sz]);
    for i in 1..sz {
        let mut s = 0i64;
        let kernel = &INTRA_EDGE_KERNEL[(strength - 1) as usize];
        for (j, &tap) in kernel.iter().enumerate() {
            let idx = (i as i32 - 2 + j as i32).clamp(0, sz as i32 - 1) as usize;
            s += tap as i64 * edge[idx] as i64;
        }
        buf[i + 1] = ((s + 8) >> 4) as i32;
    }
}

/// Directional prediction (spec section 8.4).
#[allow(clippy::too_many_arguments)]
fn predict_directional(
    pred: &mut [i32],
    w: usize,
    h: usize,
    above_row: &mut [i32],
    left_col: &mut [i32],
    mode: u8,
    angle_delta: i32,
    filter_type: u8,
    enable_intra_edge_filter: bool,
    max_x: usize,
    max_y: usize,
    x: usize,
    y: usize,
) -> Result<()> {
    // Index 2 is position -1; index 1 is position -2.
    let at = |arr: &[i32], i: i32| -> i32 { arr[(i + 2) as usize] };
    let p_angle = MODE_TO_ANGLE[mode as usize] + angle_delta * ANGLE_STEP;
    let mut upsample_above = false;
    let mut upsample_left = false;
    if enable_intra_edge_filter && p_angle != 90 && p_angle != 180 {
        if p_angle > 90 && p_angle < 180 && w + h >= 24 {
            // Filter the corner (spec section 8.4, `filter_corner`).
            let s = at(left_col, 0) * 5 + at(above_row, -1) * 6 + at(above_row, 0) * 5;
            let corner = round2(s as i64, 4);
            above_row[1] = corner;
            left_col[1] = corner;
        }
        // Above edge.
        let strength = edge_filter_strength(w, h, filter_type, p_angle - 90);
        let num_px = w.min(max_x - x + 1) + if p_angle < 90 { h } else { 0 } + 1;
        edge_filter(&mut above_row[1..], num_px, strength);
        // Left edge.
        let strength = edge_filter_strength(w, h, filter_type, p_angle - 180);
        let num_px = h.min(max_y - y + 1) + if p_angle > 180 { w } else { 0 } + 1;
        edge_filter(&mut left_col[1..], num_px, strength);
        upsample_above = edge_upsample_selection(w, h, filter_type, p_angle - 90);
        let num_px = w + if p_angle < 90 { h } else { 0 };
        if upsample_above {
            edge_upsample(&mut above_row[1..], num_px);
        }
        upsample_left = edge_upsample_selection(w, h, filter_type, p_angle - 180);
        let num_px = h + if p_angle > 180 { w } else { 0 };
        if upsample_left {
            edge_upsample(&mut left_col[1..], num_px);
        }
    }
    let dx = if p_angle < 90 {
        DR_INTRA_DERIVATIVE[p_angle as usize]
    } else if p_angle > 90 && p_angle < 180 {
        DR_INTRA_DERIVATIVE[(180 - p_angle) as usize]
    } else {
        0
    };
    let dy = if p_angle > 90 && p_angle < 180 {
        DR_INTRA_DERIVATIVE[(p_angle - 90) as usize]
    } else if p_angle > 180 {
        DR_INTRA_DERIVATIVE[(270 - p_angle) as usize]
    } else {
        0
    };
    // Bounds for a directional read: the farthest base index the
    // branches below can request.
    let max_base_above = ((w + h - 1) as i32) << upsample_above as i32;
    let max_base_left = ((w + h - 1) as i32) << upsample_left as i32;
    if p_angle < 90 {
        for i in 0..h {
            for j in 0..w {
                let idx = ((i + 1) as i32) * dx;
                let base =
                    (idx >> (6 - upsample_above as i32)) + ((j as i32) << upsample_above as i32);
                let shift = ((idx << upsample_above as i32) >> 1) & 0x1F;
                pred[i * w + j] = if base < max_base_above {
                    if base < -1 || base + 1 > max_base_above + 1 {
                        return Err(Error::InvalidData {
                            offset: 0,
                            detail: "av1 directional prediction reads outside the above row",
                        });
                    }
                    round2(
                        at(above_row, base) as i64 * (32 - shift) as i64
                            + at(above_row, base + 1) as i64 * shift as i64,
                        5,
                    )
                } else {
                    at(above_row, max_base_above)
                };
            }
        }
    } else if p_angle > 90 && p_angle < 180 {
        for i in 0..h {
            for j in 0..w {
                let idx = ((j as i32) << 6) - ((i + 1) as i32) * dx;
                let base = idx >> (6 - upsample_above as i32);
                if base >= -(1 << upsample_above as i32) {
                    let shift = ((idx << upsample_above as i32) >> 1) & 0x1F;
                    if base < -1 || base + 1 > max_base_above + 1 {
                        return Err(Error::InvalidData {
                            offset: 0,
                            detail: "av1 directional prediction reads outside the above row",
                        });
                    }
                    pred[i * w + j] = round2(
                        at(above_row, base) as i64 * (32 - shift) as i64
                            + at(above_row, base + 1) as i64 * shift as i64,
                        5,
                    );
                } else {
                    let idx = ((i as i32) << 6) - ((j + 1) as i32) * dy;
                    let base = idx >> (6 - upsample_left as i32);
                    let shift = ((idx << upsample_left as i32) >> 1) & 0x1F;
                    if base < -1 || base + 1 > max_base_left + 1 {
                        return Err(Error::InvalidData {
                            offset: 0,
                            detail: "av1 directional prediction reads outside the left column",
                        });
                    }
                    pred[i * w + j] = round2(
                        at(left_col, base) as i64 * (32 - shift) as i64
                            + at(left_col, base + 1) as i64 * shift as i64,
                        5,
                    );
                }
            }
        }
    } else if p_angle > 180 {
        for i in 0..h {
            for j in 0..w {
                let idx = ((j + 1) as i32) * dy;
                let base =
                    (idx >> (6 - upsample_left as i32)) + ((i as i32) << upsample_left as i32);
                let shift = ((idx << upsample_left as i32) >> 1) & 0x1F;
                if base < -1 || base + 1 > max_base_left + 1 {
                    return Err(Error::InvalidData {
                        offset: 0,
                        detail: "av1 directional prediction reads outside the left column",
                    });
                }
                pred[i * w + j] = round2(
                    at(left_col, base) as i64 * (32 - shift) as i64
                        + at(left_col, base + 1) as i64 * shift as i64,
                    5,
                );
            }
        }
    } else if p_angle == 90 {
        for i in 0..h {
            for j in 0..w {
                pred[i * w + j] = at(above_row, j as i32);
            }
        }
    } else {
        // p_angle == 180.
        for i in 0..h {
            for j in 0..w {
                pred[i * w + j] = at(left_col, i as i32);
            }
        }
    }
    Ok(())
}

/// Whether the neighbour mode at a block is a smooth mode (spec section
/// 8.4, `is_smooth`): for chroma the caller passes the UV mode.
pub(crate) fn is_smooth_mode(mode: u8) -> bool {
    matches!(mode, SMOOTH_PRED | SMOOTH_V_PRED | SMOOTH_H_PRED)
}

/// The filter type for a block from its above and left neighbour
/// modes (spec section 8.4, `get_filter_type`): 1 when either
/// neighbour uses a smooth mode.
pub(crate) fn filter_type_from_neighbours(above_mode: Option<u8>, left_mode: Option<u8>) -> u8 {
    let above_smooth = above_mode.is_some_and(is_smooth_mode);
    let left_smooth = left_mode.is_some_and(is_smooth_mode);
    u8::from(above_smooth || left_smooth)
}

/// Palette prediction (spec section 8.4, `predict_palette`): each
/// sample is the palette colour at its colour-map index.
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_palette_block(
    plane: &mut [u8],
    stride: usize,
    frame_w: usize,
    frame_h: usize,
    x: usize,
    y: usize,
    colors: &[u16; 8],
    palette_size: u8,
    map: &[u8],
    map_w: usize,
    map_h: usize,
) -> Result<()> {
    if palette_size == 0 || palette_size > 8 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 palette size is out of range",
        });
    }
    if x.checked_add(map_w).map_or(true, |e| e > frame_w)
        || y.checked_add(map_h).map_or(true, |e| e > frame_h)
        || map.len() < map_w * map_h
    {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 palette block is outside the frame",
        });
    }
    for i in 0..map_h {
        for j in 0..map_w {
            let idx = map[i * map_w + j];
            if idx >= palette_size {
                return Err(Error::InvalidData {
                    offset: 0,
                    detail: "av1 palette colour map index is out of range",
                });
            }
            plane[(y + i) * stride + (x + j)] = clip1_8(colors[idx as usize] as i32);
        }
    }
    Ok(())
}

/// Chroma-from-luma prediction (spec section 8.4, `predict_cfl`).
/// The DC prediction for the chroma block must already be written
/// into `plane`; this function adds the scaled luma deviation.
///
/// `luma_plane` is the fully reconstructed luma plane; the chroma
/// block covers rows `y..y+tx_h` and columns `x..x+tx_w` in chroma
/// samples, and `sub_x`/`sub_y` are the chroma subsampling shifts
/// (1, 1 for 4:2:0).
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_cfl(
    plane: &mut [u8],
    stride: usize,
    frame_w: usize,
    frame_h: usize,
    x: usize,
    y: usize,
    tx_w: usize,
    tx_h: usize,
    alpha: i32,
    luma_plane: &[u8],
    luma_stride: usize,
    luma_w: usize,
    luma_h: usize,
    sub_x: u32,
    sub_y: u32,
) -> Result<()> {
    if x.checked_add(tx_w).map_or(true, |e| e > frame_w)
        || y.checked_add(tx_h).map_or(true, |e| e > frame_h)
    {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 CFL block is outside the frame",
        });
    }
    let mut l = vec![0i64; tx_w * tx_h];
    let mut luma_avg = 0i64;
    for i in 0..tx_h {
        for j in 0..tx_w {
            let luma_y = ((y + i) << sub_y).min(luma_h - (1 << sub_y));
            let luma_x = ((x + j) << sub_x).min(luma_w - (1 << sub_x));
            let mut t = 0i64;
            for dy in 0..(1usize << sub_y) {
                for dx in 0..(1usize << sub_x) {
                    t += luma_plane[(luma_y + dy) * luma_stride + (luma_x + dx)] as i64;
                }
            }
            let v = t << (3 - sub_x - sub_y);
            l[i * tx_w + j] = v;
            luma_avg += v;
        }
    }
    luma_avg = round2(luma_avg, (tx_w as u32).ilog2() + (tx_h as u32).ilog2()) as i64;
    for i in 0..tx_h {
        for j in 0..tx_w {
            let idx = (y + i) * stride + (x + j);
            let scaled = round2_signed(alpha as i64 * (l[i * tx_w + j] - luma_avg), 6);
            plane[idx] = clip1_8(plane[idx] as i32 + scaled);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
//
// The transform tests compare against an f64 model of the orthonormal
// DCT/ADST kernels with a small tolerance; they are not bit-exact
// against ffmpeg. The hand-computed cases pin the exact fixed-point
// values the spec's butterfly steps produce.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::av1_tile::{D45_PRED, D67_PRED, H_PRED, TX_4X8, TX_8X8, UV_CFL_PRED};

    // -- Dequantization -----------------------------------------------------

    #[test]
    fn dc_and_ac_qlookup_spot_values_match_spec() {
        assert_eq!(dc_q(0), 4);
        assert_eq!(dc_q(112), 111);
        assert_eq!(dc_q(255), 1336);
        assert_eq!(ac_q(0), 4);
        assert_eq!(ac_q(112), 136);
        assert_eq!(ac_q(255), 1828);
    }

    #[test]
    fn qlookup_tables_are_non_decreasing_and_clamped() {
        for w in DC_QLOOKUP_8BIT.windows(2) {
            assert!(w[0] <= w[1]);
        }
        for w in AC_QLOOKUP_8BIT.windows(2) {
            assert!(w[0] <= w[1]);
        }
        assert_eq!(dc_q(-5), dc_q(0));
        assert_eq!(dc_q(300), dc_q(255));
        assert_eq!(ac_q(-1), ac_q(0));
    }

    #[test]
    fn dequantize_coeff_applies_denominator_sign_and_clip() {
        assert_eq!(dequantize_coeff(1, 111, TX_4X4), 111);
        assert_eq!(dequantize_coeff(-2, 111, TX_4X4), -222);
        assert_eq!(dequantize_coeff(1, 136, TX_32X32), 68);
        assert_eq!(dequantize_coeff(1, 136, TX_64X64), 34);
        // The clip applies to the signed value (spec section 8.6), so a
        // large negative coefficient clips to -32768, not -32767.
        assert_eq!(dequantize_coeff(32767, 1828, TX_4X4), 32767);
        assert_eq!(dequantize_coeff(-32767, 1828, TX_4X4), -32768);
        assert_eq!(dequantize_coeff(0, 111, TX_4X4), 0);
    }

    #[test]
    fn flip_flags_match_spec_table() {
        assert!(flip_ud(TX_TYPE_FLIPADST_DCT));
        assert!(!flip_ud(TX_TYPE_DCT_DCT));
        assert!(flip_ud(TX_TYPE_V_FLIPADST));
        assert!(!flip_ud(TX_TYPE_ADST_DCT));
        assert!(flip_lr(TX_TYPE_DCT_FLIPADST));
        assert!(flip_lr(TX_TYPE_H_FLIPADST));
        assert!(flip_lr(TX_TYPE_FLIPADST_FLIPADST));
        assert!(flip_ud(TX_TYPE_FLIPADST_FLIPADST));
        assert!(!flip_lr(TX_TYPE_DCT_DCT));
    }

    // -- Rounding -----------------------------------------------------------

    #[test]
    fn round2_matches_spec_examples() {
        assert_eq!(round2(4096, 12), 1);
        assert_eq!(round2(8192, 12), 2);
        assert_eq!(round2(-1, 12), 0);
        assert_eq!(round2(-4096, 12), -1);
        assert_eq!(round2(2048, 12), 1);
        assert_eq!(round2(2047, 12), 0);
        assert_eq!(round2(123, 0), 123);
    }

    #[test]
    fn round2_signed_is_sign_symmetric() {
        assert_eq!(round2_signed(-17, 4), -1);
        assert_eq!(round2_signed(-16, 4), -1);
        assert_eq!(round2_signed(17, 4), 1);
        assert_eq!(round2_signed(-24, 4), -2);
        assert_eq!(round2_signed(0, 4), 0);
        assert_eq!(round2_signed(24, 4), 2);
    }

    #[test]
    fn cos128_and_sin128_spot_values() {
        assert_eq!(cos128(0), 4096);
        assert_eq!(cos128(32), 2896);
        assert_eq!(cos128(64), 0);
        assert_eq!(cos128(96), -2896);
        assert_eq!(cos128(128), -4096);
        assert_eq!(sin128(0), 0);
        assert_eq!(sin128(32), 2896);
        assert_eq!(sin128(64), 4096);
    }

    #[test]
    fn brev_reverses_bits() {
        assert_eq!(brev(2, 1), 2);
        assert_eq!(brev(3, 1), 4);
        assert_eq!(brev(4, 5), 10);
        assert_eq!(brev(6, 1), 32);
    }

    // -- Inverse transform --------------------------------------------------

    fn dct_reference(input: &[i32]) -> Vec<f64> {
        let n = input.len();
        let mut out = vec![0f64; n];
        for (k, out_k) in out.iter_mut().enumerate() {
            let mut s = 0f64;
            for (idx, &v) in input.iter().enumerate() {
                let m = if idx == 0 {
                    std::f64::consts::FRAC_1_SQRT_2
                } else {
                    ((std::f64::consts::PI * (2 * k + 1) as f64 * idx as f64) / (2 * n) as f64)
                        .cos()
                };
                s += v as f64 * m;
            }
            *out_k = s;
        }
        out
    }

    fn adst_reference(input: &[i32]) -> Vec<f64> {
        let n = input.len();
        let c = (8f64 / (2 * n + 1) as f64).sqrt();
        let mut out = vec![0f64; n];
        for (k, out_k) in out.iter_mut().enumerate() {
            let mut s = 0f64;
            for (idx, &v) in input.iter().enumerate() {
                let m = c
                    * ((std::f64::consts::PI * (2 * idx + 1) as f64 * (k + 1) as f64)
                        / (2 * n + 1) as f64)
                        .sin();
                s += v as f64 * m;
            }
            *out_k = s;
        }
        out
    }

    fn assert_close(actual: &[i32], expected: &[f64], tol: i32) {
        assert_eq!(actual.len(), expected.len());
        for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
            assert!(
                (*a as f64 - *e).abs() <= tol as f64,
                "index {i}: got {a}, expected {e:.2} (tol {tol})"
            );
        }
    }

    #[test]
    fn inverse_dct_dc_response_is_flat_for_all_lengths() {
        for n in 2..=6u32 {
            let len = 1usize << n;
            let mut t = vec![0i32; len];
            t[0] = 4096;
            inverse_dct(&mut t, n, 16).unwrap();
            for v in &t {
                assert_eq!(*v, 2896, "dct length {len}");
            }
        }
    }

    #[test]
    fn inverse_dct_matches_float_reference() {
        // The patterns stay inside the 16-bit clamp range of the
        // spec's Hadamard steps: a pattern whose true DCT exceeds
        // +-32767 saturates by design (spec section 8.7), so it
        // cannot be compared against an unclipped float model.
        let patterns: Vec<Vec<i32>> = vec![
            vec![0, 4096, 0, 0],
            vec![1000, -2000, 3000, -500],
            vec![4096, 0, 0, 0, 0, 0, 0, 0],
            vec![500, 1500, -2500, 3500, -500, 2500, -1500, 500],
            vec![1000; 16],
            vec![4096, -4096, 2048, -2048, 1024, -1024, 512, -512],
            vec![500; 32],
            vec![-250; 64],
        ];
        for input in patterns {
            let n = (input.len() as u32).ilog2();
            let expected = dct_reference(&input);
            let mut t = input.clone();
            inverse_dct(&mut t, n, 16).unwrap();
            let tol = if input.len() >= 32 { 16 } else { 4 };
            assert_close(&t, &expected, tol);
        }
    }

    #[test]
    fn inverse_dct_rejects_bad_length() {
        let mut t = vec![0i32; 4];
        assert!(inverse_dct(&mut t, 1, 16).is_err());
        assert!(inverse_dct(&mut t, 7, 16).is_err());
    }

    #[test]
    fn inverse_adst4_matches_hand_traced_columns() {
        let cases = [
            ([4096, 0, 0, 0], [1321, 2482, 3344, 3803]),
            ([0, 4096, 0, 0], [3344, 3344, 0, -3344]),
            ([0, 0, 4096, 0], [3803, -1321, -3344, 2482]),
            ([0, 0, 0, 4096], [2482, -3803, 3344, -1321]),
        ];
        for (input, expected) in cases {
            let mut t = input;
            inverse_adst(&mut t, 2, 16).unwrap();
            assert_eq!(t, expected, "input {input:?}");
        }
    }

    #[test]
    #[ignore = "ADST8/16 vs DST-VII model is an open discrepancy; see inverse_adst8 note"]
    fn inverse_adst_matches_float_reference() {
        // OPEN DISCREPANCY (see the module note on `inverse_adst8`):
        // the spec's ADST8/ADST16 factorization, implemented here
        // verbatim from spec section 7.13.2.7/7.13.2.8, produces a
        // DST-IV-family matrix, while the DST-VII model below predicts
        // different values (measured max error 2600 for ADST8, 6186
        // for ADST16). ADST4 matches the DST-VII model exactly. Until
        // this is resolved against ffmpeg/dav1d pixel output (which
        // needs coefficient decode first), this test is ignored rather
        // than deleted, so the gap stays visible.
        let patterns: Vec<Vec<i32>> = vec![
            vec![1000, -2000, 3000, -500, 1500, -2500, 3500, -500],
            vec![4096, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            vec![500; 16],
        ];
        for input in patterns {
            let n = (input.len() as u32).ilog2();
            let expected = adst_reference(&input);
            let mut t = input.clone();
            inverse_adst(&mut t, n, 16).unwrap();
            assert_close(&t, &expected, 4);
        }
    }

    #[test]
    fn inverse_adst8_matches_spec_factorization() {
        // Regression test pinning the spec factorization's output
        // (see the note on `inverse_adst8`): the first column of
        // the 8-point transform, 4096*sin((2k+1)*pi/32).
        let mut t = [4096, 0, 0, 0, 0, 0, 0, 0];
        inverse_adst(&mut t, 3, 16).unwrap();
        assert_eq!(t, [401, 1189, 1930, 2598, 3165, 3612, 3919, 4076]);
    }

    #[test]
    fn inverse_adst_rejects_lengths_above_16() {
        let mut t = vec![0i32; 32];
        assert!(inverse_adst(&mut t, 5, 16).is_err());
    }

    #[test]
    fn inverse_identity_scales_by_sqrt_n_over_2() {
        let mut t = [4096, 0, 0, 0];
        inverse_identity(&mut t, 2).unwrap();
        assert_eq!(t, [5793, 0, 0, 0]);
        let mut t = [100, 0, 0, 0, 0, 0, 0, 0];
        inverse_identity(&mut t, 3).unwrap();
        assert_eq!(t[0], 200);
        let mut t = [4096; 16];
        inverse_identity(&mut t, 4).unwrap();
        assert_eq!(t[0], 11586);
        let mut t = [100; 32];
        inverse_identity(&mut t, 5).unwrap();
        assert_eq!(t[0], 400);
    }

    #[test]
    fn inverse_wht_spot_values() {
        let mut t = [4096, 0, 0, 0];
        inverse_wht(&mut t, 2);
        assert_eq!(t, [512, 512, 512, 512]);
        let mut t = [4096, 0, 0, 0];
        inverse_wht(&mut t, 0);
        assert_eq!(t, [2048, 2048, 2048, 2048]);
    }

    #[test]
    fn inverse_transform_2d_dc_4x4_is_flat_128() {
        let mut quant = vec![0i32; 16];
        quant[0] = 4096;
        let residual = inverse_transform_2d(&quant, TX_4X4, TX_TYPE_DCT_DCT, false).unwrap();
        assert_eq!(residual, vec![128; 16]);
    }

    #[test]
    fn inverse_transform_2d_dc_4x8_scales_by_pre_scale() {
        // Row DCT4([2896, 0, 0, 0]) = [2048; 4] after the 2896
        // pre-scale; column DCT8([2048, 0, ...]) = [1448; 8]
        // because the AV1 DCT's DC gain is 1/sqrt(2) at every
        // length, not 1/sqrt(N); Round2(1448, 4) = 91.
        let mut quant = vec![0i32; 32];
        quant[0] = 4096;
        let residual = inverse_transform_2d(&quant, TX_4X8, TX_TYPE_DCT_DCT, false).unwrap();
        assert_eq!(residual, vec![91; 32]);
    }

    #[test]
    fn reconstruct_tx_block_flipadst_dc_flips_vertically() {
        // Column ADST4([2896, 0, 0, 0]) = [934, 1755, 2364, 2689];
        // Round2(..., 4) = [58, 110, 148, 168] down each column
        // (the row DCT spreads the DC across all columns, so every
        // column is the same), flipped vertically.
        let mut plane = vec![0u8; 64];
        let mut quant = vec![0i32; 16];
        quant[0] = 4096;
        reconstruct_tx_block(
            &mut plane,
            8,
            8,
            8,
            0,
            0,
            &quant,
            TX_4X4,
            TX_TYPE_FLIPADST_DCT,
            1,
            1,
            false,
        )
        .unwrap();
        assert_eq!(&plane[0..4], &[168, 168, 168, 168]);
        assert_eq!(&plane[8..12], &[148, 148, 148, 148]);
        assert_eq!(&plane[16..20], &[110, 110, 110, 110]);
        assert_eq!(&plane[24..28], &[58, 58, 58, 58]);
    }

    #[test]
    fn reconstruct_tx_block_rejects_out_of_frame_and_bad_size() {
        let mut plane = vec![0u8; 64];
        let quant = vec![0i32; 16];
        assert!(reconstruct_tx_block(
            &mut plane,
            8,
            8,
            8,
            5,
            0,
            &quant,
            TX_4X4,
            TX_TYPE_DCT_DCT,
            1,
            1,
            false
        )
        .is_err());
        assert!(reconstruct_tx_block(
            &mut plane,
            8,
            8,
            8,
            0,
            0,
            &quant,
            19,
            TX_TYPE_DCT_DCT,
            1,
            1,
            false
        )
        .is_err());
        assert!(reconstruct_tx_block(
            &mut plane,
            8,
            8,
            8,
            0,
            0,
            &quant,
            TX_8X8,
            TX_TYPE_DCT_DCT,
            1,
            1,
            true
        )
        .is_err());
    }

    // -- Intra prediction ---------------------------------------------------

    fn make_plane(w: usize, h: usize) -> Vec<u8> {
        vec![0u8; w * h]
    }

    fn pred_args(
        plane: &mut [u8],
        stride: usize,
        frame_w: usize,
        frame_h: usize,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        mode: u8,
    ) -> IntraPred<'_> {
        IntraPred {
            plane,
            stride,
            frame_w,
            frame_h,
            x,
            y,
            w,
            h,
            mode,
            angle_delta: 0,
            use_filter_intra: false,
            filter_intra_mode: 0,
            filter_type: 0,
            enable_intra_edge_filter: false,
            tile_x0: 0,
            tile_y0: 0,
            tile_x1: frame_w,
            tile_y1: frame_h,
        }
    }

    fn set_neighbours_4x4(plane: &mut [u8], stride: usize, above: [u8; 4], left: [u8; 4]) {
        for j in 0..4 {
            plane[3 * stride + 4 + j] = above[j];
        }
        for i in 0..4 {
            plane[(4 + i) * stride + 3] = left[i];
        }
    }

    #[test]
    fn dc_prediction_averages_both_neighbours() {
        let mut plane = make_plane(8, 8);
        set_neighbours_4x4(&mut plane, 8, [100, 110, 120, 130], [50, 60, 70, 80]);
        predict_intra_block(pred_args(&mut plane, 8, 8, 8, 4, 4, 4, 4, DC_PRED)).unwrap();
        for i in 0..4 {
            for j in 0..4 {
                assert_eq!(plane[(4 + i) * 8 + 4 + j], 90);
            }
        }
    }

    #[test]
    fn dc_prediction_left_only_and_no_neighbours() {
        let mut plane = make_plane(8, 8);
        for i in 0..4 {
            plane[i * 8 + 3] = [50, 60, 70, 80][i];
        }
        predict_intra_block(pred_args(&mut plane, 8, 8, 8, 4, 0, 4, 4, DC_PRED)).unwrap();
        for i in 0..4 {
            for j in 0..4 {
                assert_eq!(plane[i * 8 + 4 + j], 65);
            }
        }
        let mut plane = make_plane(8, 8);
        predict_intra_block(pred_args(&mut plane, 8, 8, 8, 0, 0, 4, 4, DC_PRED)).unwrap();
        for i in 0..4 {
            for j in 0..4 {
                assert_eq!(plane[i * 8 + j], 128);
            }
        }
    }

    #[test]
    fn vertical_and_horizontal_prediction_copy_neighbours() {
        let mut plane = make_plane(8, 8);
        set_neighbours_4x4(&mut plane, 8, [100, 110, 120, 130], [50, 60, 70, 80]);
        predict_intra_block(pred_args(&mut plane, 8, 8, 8, 4, 4, 4, 4, V_PRED)).unwrap();
        for i in 0..4 {
            assert_eq!(
                &plane[(4 + i) * 8 + 4..(4 + i) * 8 + 8],
                &[100, 110, 120, 130]
            );
        }
        let mut plane = make_plane(8, 8);
        set_neighbours_4x4(&mut plane, 8, [100, 110, 120, 130], [50, 60, 70, 80]);
        predict_intra_block(pred_args(&mut plane, 8, 8, 8, 4, 4, 4, 4, H_PRED)).unwrap();
        for i in 0..4 {
            for j in 0..4 {
                assert_eq!(plane[(4 + i) * 8 + 4 + j], [50, 60, 70, 80][i]);
            }
        }
    }

    #[test]
    fn paeth_prediction_picks_closest_neighbour() {
        // Corner wins: base = 100 + 50 - 75 = 75, pTopLeft = 0.
        let mut plane = make_plane(8, 8);
        set_neighbours_4x4(&mut plane, 8, [100, 100, 100, 100], [50, 50, 50, 50]);
        plane[3 * 8 + 3] = 75;
        predict_intra_block(pred_args(&mut plane, 8, 8, 8, 4, 4, 4, 4, PAETH_PRED)).unwrap();
        for i in 0..4 {
            for j in 0..4 {
                assert_eq!(plane[(4 + i) * 8 + 4 + j], 75);
            }
        }
        // Above wins: base = 200 + 100 - 100 = 200, pTop = 0.
        let mut plane = make_plane(8, 8);
        set_neighbours_4x4(&mut plane, 8, [200, 200, 200, 200], [100, 100, 100, 100]);
        plane[3 * 8 + 3] = 100;
        predict_intra_block(pred_args(&mut plane, 8, 8, 8, 4, 4, 4, 4, PAETH_PRED)).unwrap();
        for i in 0..4 {
            for j in 0..4 {
                assert_eq!(plane[(4 + i) * 8 + 4 + j], 200);
            }
        }
    }

    #[test]
    fn smooth_v_prediction_spot_values() {
        let mut plane = make_plane(8, 8);
        set_neighbours_4x4(&mut plane, 8, [100, 110, 120, 130], [50, 60, 70, 80]);
        predict_intra_block(pred_args(&mut plane, 8, 8, 8, 4, 4, 4, 4, SMOOTH_V_PRED)).unwrap();
        assert_eq!(plane[4 * 8 + 4], 100);
        assert_eq!(plane[7 * 8 + 4], 85);
        assert_eq!(plane[4 * 8 + 7], 130);
        assert_eq!(plane[7 * 8 + 7], 93);
    }

    #[test]
    fn edge_filter_strength_spot_values() {
        // Spec section 8.4 table, both filter types, at and below
        // each threshold; the sign of `d` is ignored.
        assert_eq!(edge_filter_strength(4, 4, 0, 56), 1);
        assert_eq!(edge_filter_strength(4, 4, 0, 55), 0);
        assert_eq!(edge_filter_strength(4, 4, 0, -56), 1);
        assert_eq!(edge_filter_strength(8, 8, 0, 40), 1);
        assert_eq!(edge_filter_strength(8, 8, 0, 39), 0);
        assert_eq!(edge_filter_strength(12, 12, 0, 32), 3);
        assert_eq!(edge_filter_strength(12, 12, 0, 16), 2);
        assert_eq!(edge_filter_strength(12, 12, 0, 8), 1);
        assert_eq!(edge_filter_strength(12, 12, 0, 7), 0);
        assert_eq!(edge_filter_strength(16, 16, 0, 32), 3);
        assert_eq!(edge_filter_strength(16, 16, 0, 4), 2);
        assert_eq!(edge_filter_strength(16, 16, 0, 3), 1);
        assert_eq!(edge_filter_strength(32, 32, 0, 0), 3);
        assert_eq!(edge_filter_strength(4, 4, 1, 64), 2);
        assert_eq!(edge_filter_strength(4, 4, 1, 40), 1);
        assert_eq!(edge_filter_strength(4, 4, 1, 39), 0);
        assert_eq!(edge_filter_strength(8, 8, 1, 48), 2);
        assert_eq!(edge_filter_strength(8, 8, 1, 20), 1);
        assert_eq!(edge_filter_strength(8, 8, 1, 19), 0);
        assert_eq!(edge_filter_strength(12, 12, 1, 4), 3);
        assert_eq!(edge_filter_strength(12, 12, 1, 3), 0);
        assert_eq!(edge_filter_strength(32, 32, 1, 0), 3);
    }

    #[test]
    fn d45_prediction_interpolates_along_diagonal() {
        let mut plane = make_plane(16, 16);
        for j in 0..8 {
            plane[3 * 16 + 4 + j] = [10, 20, 30, 40, 50, 60, 70, 80][j];
        }
        predict_intra_block(pred_args(&mut plane, 16, 16, 16, 4, 4, 4, 4, D45_PRED)).unwrap();
        assert_eq!(plane[4 * 16 + 4], 20);
        assert_eq!(plane[4 * 16 + 7], 50);
        assert_eq!(plane[7 * 16 + 4], 50);
        assert_eq!(plane[7 * 16 + 7], 80);
    }

    #[test]
    fn filter_intra_mode_1_spot_values() {
        let mut plane = make_plane(8, 8);
        set_neighbours_4x4(&mut plane, 8, [10, 20, 30, 40], [5, 15, 25, 35]);
        plane[3 * 8 + 3] = 0;
        let mut args = pred_args(&mut plane, 8, 8, 8, 4, 4, 4, 4, DC_PRED);
        args.use_filter_intra = true;
        args.filter_intra_mode = 1;
        predict_intra_block(args).unwrap();
        assert_eq!(&plane[4 * 8 + 4..4 * 8 + 8], &[13, 22, 31, 41]);
        assert_eq!(&plane[5 * 8 + 4..5 * 8 + 8], &[19, 26, 34, 42]);
        assert_eq!(plane[6 * 8 + 4], 25);
    }

    #[test]
    fn palette_prediction_looks_up_colours() {
        let mut plane = make_plane(8, 8);
        let colors = [10, 20, 30, 40, 0, 0, 0, 0];
        let map = [0, 1, 2, 3, 3, 2, 1, 0, 0, 0, 1, 1, 2, 2, 3, 3];
        predict_palette_block(&mut plane, 8, 8, 8, 0, 0, &colors, 4, &map, 4, 4).unwrap();
        assert_eq!(&plane[0..4], &[10, 20, 30, 40]);
        assert_eq!(&plane[8..12], &[40, 30, 20, 10]);
        assert_eq!(&plane[16..20], &[10, 10, 20, 20]);
        assert_eq!(&plane[24..28], &[30, 30, 40, 40]);
        let bad_map = [0, 1, 2, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(
            predict_palette_block(&mut plane, 8, 8, 8, 0, 0, &colors, 4, &bad_map, 4, 4).is_err()
        );
    }

    #[test]
    fn cfl_prediction_adds_scaled_luma_deviation() {
        let mut plane = vec![100u8; 64];
        let mut luma = vec![0u8; 64];
        for y in 0..8 {
            for x in 0..8 {
                luma[y * 8 + x] = (100 + x) as u8;
            }
        }
        apply_cfl(&mut plane, 8, 8, 8, 0, 0, 4, 4, 8, &luma, 8, 8, 8, 1, 1).unwrap();
        for i in 0..4 {
            assert_eq!(&plane[i * 8..i * 8 + 4], &[97, 99, 101, 103]);
        }
    }

    #[test]
    fn filter_type_comes_from_smooth_neighbours() {
        assert_eq!(filter_type_from_neighbours(Some(SMOOTH_PRED), None), 1);
        assert_eq!(filter_type_from_neighbours(Some(DC_PRED), Some(V_PRED)), 0);
        assert_eq!(filter_type_from_neighbours(None, None), 0);
        assert_eq!(filter_type_from_neighbours(None, Some(SMOOTH_H_PRED)), 1);
        assert!(is_directional_mode(D45_PRED));
        assert!(is_directional_mode(D67_PRED));
        assert!(!is_directional_mode(DC_PRED));
        assert!(!is_directional_mode(SMOOTH_PRED));
    }

    #[test]
    fn prediction_rejects_bad_inputs() {
        let mut plane = make_plane(64, 64);
        // Block outside the frame.
        assert!(
            predict_intra_block(pred_args(&mut plane, 64, 64, 64, 60, 60, 8, 8, DC_PRED)).is_err()
        );
        // CFL mode is handled by apply_cfl, not here.
        assert!(
            predict_intra_block(pred_args(&mut plane, 64, 64, 64, 0, 0, 4, 4, UV_CFL_PRED))
                .is_err()
        );
        // Non power-of-two size.
        assert!(
            predict_intra_block(pred_args(&mut plane, 64, 64, 64, 0, 0, 2, 4, DC_PRED)).is_err()
        );
        // Angle delta out of range.
        let mut args = pred_args(&mut plane, 64, 64, 64, 0, 0, 4, 4, D45_PRED);
        args.angle_delta = 4;
        assert!(predict_intra_block(args).is_err());
    }
}
