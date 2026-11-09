// Casts in this module are bounded by construction: filter
// levels are clipped to 0..63, sample values are 8-bit, and all
// plane coordinates are clamped to the plane bounds before use.
// The pedantic cast lints therefore do not find real truncation
// risks here.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]

//! VP9 loop filter, spec section 8.8.
//!
//! VP9 Bitstream & Decoding Process Specification v0.6, section
//! 8.8 (frame init, superblock loop, filter size, adaptive
//! strength, sample filtering).
//!
//! The filter runs once per frame after the whole frame is
//! reconstructed (spec section 8.1): intra prediction of later
//! blocks uses unfiltered samples, so filtering must not start
//! until reconstruction is complete. The results are part of the
//! decoded frame and are used to predict subsequent frames.
//!
//! Edge samples: the spec indexes samples outside the frame
//! (up to 8 samples past an edge). libvpx reads those from the
//! frame border, which replicates the edge sample, so this
//! implementation clamps coordinates to the plane bounds. The
//! padded plane (the MI grid) is the frame here, so the clamp is
//! to the padded bounds.

use crate::vp9_bitstream::Vp9FrameHeader;
use crate::vp9_predict::{get_uv_tx_size, Vp9FrameBuffer, Vp9ModeMaps};
use crate::vp9_token::{
    clip1, clip3, round2, seg_feature_active, BLOCK_16X16, INTRA_FRAME, MAX_LOOP_FILTER, NEARESTMV,
    NEARMV, NEWMV, NUM_8X8_HIGH, NUM_8X8_WIDE, SEG_LVL_ALT_L, TX_16X16, TX_4X4, TX_8X8,
};

/// Loop-filter strength lookup table, spec section 8.8.1.
/// Indexed `[segment_id][ref_frame][mode_type]`: 8 segments, 4
/// reference frames (INTRA, LAST, GOLDEN, ALTREF), 2 mode types
/// (0 = intra or ZEROMV, 1 = NEARESTMV, NEARMV or NEWMV).
pub(crate) struct Vp9LoopFilterLevels {
    pub lvl: [[[i32; 2]; 4]; 8],
}

impl Vp9LoopFilterLevels {
    /// Builds the lookup table for a frame, spec section 8.8.1.
    /// `header` carries the level, sharpness, delta enable flag,
    /// delta values, and segmentation features. When deltas are
    /// disabled every entry is the (possibly segment-adjusted)
    /// base level; when enabled, the intra level and the inter
    /// levels carry the reference and mode deltas.
    pub(crate) fn init(header: &Vp9FrameHeader) -> Self {
        let n_shift = header.loop_filter_level >> 5;
        let mut lvl = [[[0i32; 2]; 4]; 8];
        for (seg, lvl_slot) in lvl.iter_mut().enumerate() {
            let mut lvl_seg = header.loop_filter_level as i32;
            if seg_feature_active(header, seg, SEG_LVL_ALT_L) {
                let data = header.segmentation_features[seg][SEG_LVL_ALT_L];
                if header.segmentation_abs_or_delta_update {
                    lvl_seg = data;
                } else {
                    lvl_seg = data + header.loop_filter_level as i32;
                }
                lvl_seg = clip3(i64::from(lvl_seg), 0, i64::from(MAX_LOOP_FILTER)) as i32;
            }
            if header.loop_filter_delta_enabled {
                let intra_lvl =
                    lvl_seg + (header.loop_filter_ref_deltas[INTRA_FRAME as usize] << n_shift);
                lvl_slot[INTRA_FRAME as usize][0] =
                    clip3(i64::from(intra_lvl), 0, i64::from(MAX_LOOP_FILTER)) as i32;
                // Mode type 1 is never used for an intra
                // reference; keep the base level there.
                lvl_slot[INTRA_FRAME as usize][1] = lvl_seg;
                for (r, lvl_ref) in lvl_slot.iter_mut().enumerate().skip(1) {
                    for (m, lvl_entry) in lvl_ref.iter_mut().enumerate() {
                        let inter_lvl = lvl_seg
                            + (header.loop_filter_ref_deltas[r] << n_shift)
                            + (header.loop_filter_mode_deltas[m] << n_shift);
                        *lvl_entry =
                            clip3(i64::from(inter_lvl), 0, i64::from(MAX_LOOP_FILTER)) as i32;
                    }
                }
            } else {
                for lvl_ref in lvl_slot.iter_mut() {
                    for lvl_entry in lvl_ref.iter_mut() {
                        *lvl_entry = lvl_seg;
                    }
                }
            }
        }
        Self { lvl }
    }
}

/// Applies the loop filter to a reconstructed frame, spec
/// section 8.8. `frame` is modified in place. `maps` holds the
/// mode info the filter reads (block sizes, transform sizes,
/// skip flags, segment ids, reference frames, modes).
/// `sharpness` is `loop_filter_sharpness` from the header.
/// The caller skips this when `loop_filter_level` is 0.
pub(crate) fn apply_loop_filter(
    frame: &mut Vp9FrameBuffer,
    maps: &Vp9ModeMaps,
    levels: &Vp9LoopFilterLevels,
    sharpness: u32,
    subsampling_x: usize,
    subsampling_y: usize,
) {
    let mut row = 0;
    while row < maps.mi_rows {
        let mut col = 0;
        while col < maps.mi_cols {
            for plane in 0..3 {
                for pass in 0..2 {
                    superblock_loop_filter(
                        frame,
                        maps,
                        levels,
                        sharpness,
                        plane,
                        pass,
                        row,
                        col,
                        subsampling_x,
                        subsampling_y,
                    );
                }
            }
            col += 8;
        }
        row += 8;
    }
}

/// Filters one superblock, spec section 8.8.2. `row`/`col` are
/// the superblock's top-left corner in 8x8-block units. Pass 0
/// filters vertical edges (`dx` = 1), pass 1 horizontal edges
/// (`dy` = 1).
#[allow(clippy::too_many_arguments)]
fn superblock_loop_filter(
    frame: &mut Vp9FrameBuffer,
    maps: &Vp9ModeMaps,
    levels: &Vp9LoopFilterLevels,
    sharpness: u32,
    plane: usize,
    pass: usize,
    row: usize,
    col: usize,
    subsampling_x: usize,
    subsampling_y: usize,
) {
    let (sub_x, sub_y) = if plane == 0 {
        (0, 0)
    } else {
        (subsampling_x, subsampling_y)
    };
    let (dx, dy, sub, edge_len) = if pass == 0 {
        (1i64, 0i64, sub_x, 64 >> sub_y)
    } else {
        (0i64, 1i64, sub_y, 64 >> sub_x)
    };
    let num_edges = 16 >> sub;
    for edge in 0..num_edges {
        for i in 0..edge_len {
            let (x, y) = if pass == 0 {
                (col * 8 + edge * (4 << sub_x), row * 8 + (i << sub_y))
            } else {
                (col * 8 + (i << sub_x), row * 8 + edge * (4 << sub_y))
            };
            // The spec reads mode info even for off-screen edges,
            // and the edge can sit past the last block row or
            // column of a partial superblock. Clamping is safe:
            // those edges fail the on-screen check, so
            // `apply_filter` is 0 and the values are unused.
            let loop_col = (((x >> 3) >> sub_x) << sub_x).min(maps.mi_cols - 1);
            let loop_row = (((y >> 3) >> sub_y) << sub_y).min(maps.mi_rows - 1);
            let mi_size = maps.mi_sizes[loop_row][loop_col];
            let tx_size = maps.tx_sizes[loop_row][loop_col];
            let tx_sz = if plane > 0 {
                get_uv_tx_size(mi_size, tx_size)
            } else {
                tx_size
            };
            let sb_size = if sub == 0 {
                mi_size
            } else {
                mi_size.max(BLOCK_16X16)
            };
            let skip = maps.skips[loop_row][loop_col];
            let is_intra = maps.ref_frames[loop_row][loop_col][0] <= INTRA_FRAME;
            let is_block_edge = if pass == 0 {
                x % (8 * NUM_8X8_WIDE[sb_size as usize] as usize) == 0
            } else {
                y % (8 * NUM_8X8_HIGH[sb_size as usize] as usize) == 0
            };
            let is_tx_edge = if pass == 1
                && sub_x == 1
                && maps.mi_cols % 2 == 1
                && edge % 2 == 1
                && x + 8 >= maps.mi_cols * 8
            {
                false
            } else {
                edge % (1usize << tx_sz) == 0
            };
            let is_32_edge = edge % 8 == 0;
            let on_screen = x < maps.mi_cols * 8
                && y < maps.mi_rows * 8
                && !(pass == 0 && x == 0)
                && !(pass == 1 && y == 0);
            let apply_filter = on_screen
                && (is_block_edge || (is_tx_edge && is_intra) || (is_tx_edge && skip == 0));
            let filter_size = filter_size(
                tx_sz,
                is_32_edge,
                pass,
                x,
                y,
                sub_x,
                sub_y,
                maps.mi_cols,
                maps.mi_rows,
            );
            let (lvl, limit, blimit, thresh) =
                adaptive_strength(maps, levels, sharpness, loop_row, loop_col);
            if apply_filter && lvl > 0 {
                sample_filter(
                    frame,
                    plane,
                    x >> sub_x,
                    y >> sub_y,
                    limit,
                    blimit,
                    thresh,
                    dx,
                    dy,
                    filter_size,
                );
            }
        }
    }
}

/// Maximum filter size for an edge, spec section 8.8.3.
#[allow(clippy::too_many_arguments)]
fn filter_size(
    tx_sz: u8,
    is_32_edge: bool,
    pass: usize,
    x: usize,
    y: usize,
    sub_x: usize,
    sub_y: usize,
    mi_cols: usize,
    mi_rows: usize,
) -> u8 {
    let base_size = if tx_sz == TX_4X4 && is_32_edge {
        TX_8X8
    } else {
        tx_sz.min(TX_16X16)
    };
    // Both edge cases return the same size, so they share one
    // condition; the two disjuncts keep their original order and
    // short-circuiting.
    if (pass == 0 && sub_x == 1 && base_size == TX_16X16 && (x >> 3) == mi_cols - 1)
        || (pass == 1 && sub_y == 1 && base_size == TX_16X16 && (y >> 3) == mi_rows - 1)
    {
        TX_8X8
    } else {
        base_size
    }
}

/// Filter strength for an edge, spec section 8.8.4. Returns
/// (`lvl`, `limit`, `blimit`, `thresh`).
fn adaptive_strength(
    maps: &Vp9ModeMaps,
    levels: &Vp9LoopFilterLevels,
    sharpness: u32,
    loop_row: usize,
    loop_col: usize,
) -> (i32, i32, i32, i32) {
    let segment = maps.segment_ids[loop_row][loop_col] as usize;
    let reff = maps.ref_frames[loop_row][loop_col][0].clamp(0, 3) as usize;
    let mode = maps.y_modes[loop_row][loop_col];
    let mode_type = usize::from(mode == NEARESTMV || mode == NEARMV || mode == NEWMV);
    let lvl = levels.lvl[segment][reff][mode_type];
    let shift = if sharpness > 4 {
        2
    } else {
        i32::from(sharpness > 0)
    };
    let limit = if sharpness > 0 {
        clip3(i64::from(lvl >> shift), 1, 9 - i64::from(sharpness)) as i32
    } else {
        (lvl >> shift).max(1)
    };
    let blimit = 2 * (lvl + 2) + limit;
    let thresh = lvl >> 4;
    (lvl, limit, blimit, thresh)
}

/// Reads a sample, clamping the coordinates to the plane bounds
/// (spec section 8.8.5 indexes samples outside the frame; the
/// frame border in libvpx replicates the edge sample).
fn sample_at(frame: &Vp9FrameBuffer, plane: usize, x: i64, y: i64) -> i64 {
    let width = frame.plane_width(plane) as i64;
    let height = frame.plane_height(plane) as i64;
    let cx = x.clamp(0, width - 1) as usize;
    let cy = y.clamp(0, height - 1) as usize;
    let stride = frame.plane_stride(plane);
    i64::from(frame.plane(plane)[cy * stride + cx])
}

/// Writes a sample, silently skipping writes outside the plane.
/// The spec can address just past the frame edge (the narrow
/// filter's second sample); libvpx writes those into the frame
/// border, which is not part of the output.
fn set_sample(frame: &mut Vp9FrameBuffer, plane: usize, x: i64, y: i64, value: i64) {
    let width = frame.plane_width(plane) as i64;
    let height = frame.plane_height(plane) as i64;
    if x < 0 || x >= width || y < 0 || y >= height {
        return;
    }
    let stride = frame.plane_stride(plane);
    frame.plane_mut(plane)[y as usize * stride + x as usize] = clip1(value);
}

/// Filters one edge, spec section 8.8.5. `x`/`y` are in the
/// plane's coordinates; `dx`/`dy` point across the edge.
#[allow(clippy::too_many_arguments)]
fn sample_filter(
    frame: &mut Vp9FrameBuffer,
    plane: usize,
    x: usize,
    y: usize,
    limit: i32,
    blimit: i32,
    thresh: i32,
    dx: i64,
    dy: i64,
    filter_size: u8,
) {
    let (hev_mask, filter_mask, flat_mask, flat_mask2) = filter_mask(
        frame,
        plane,
        x,
        y,
        limit,
        blimit,
        thresh,
        dx,
        dy,
        filter_size,
    );
    if !filter_mask {
        return;
    }
    if filter_size == TX_4X4 || !flat_mask {
        narrow_filter(frame, plane, x, y, dx, dy, hev_mask);
    } else if filter_size == TX_8X8 || !flat_mask2 {
        wide_filter(frame, plane, x, y, dx, dy, 3);
    } else {
        wide_filter(frame, plane, x, y, dx, dy, 4);
    }
}

/// Filter masks for one edge, spec section 8.8.5.1. Returns
/// (`hev_mask`, `filter_mask`, `flat_mask`, `flat_mask2`).
/// `BitDepth` is 8, so the `<< (BitDepth - 8)` shifts are 0 and
/// the thresholds are used as written.
#[allow(clippy::too_many_arguments)]
fn filter_mask(
    frame: &Vp9FrameBuffer,
    plane: usize,
    edge_x: usize,
    edge_y: usize,
    limit: i32,
    blimit: i32,
    thresh: i32,
    dx: i64,
    dy: i64,
    filter_size: u8,
) -> (bool, bool, bool, bool) {
    let px = edge_x as i64;
    let py = edge_y as i64;
    // q[k] is k samples past the edge (q0 is on the edge);
    // p[k] is k+1 samples before it (p0 is just before it).
    let after = |k: i64| sample_at(frame, plane, px + dx * k, py + dy * k);
    let before = |k: i64| sample_at(frame, plane, px - dx * (k + 1), py - dy * (k + 1));
    let q0 = after(0);
    let q1 = after(1);
    let q2 = after(2);
    let q3 = after(3);
    let p0 = before(0);
    let p1 = before(1);
    let p2 = before(2);
    let p3 = before(3);
    let hev_mask = (p1 - p0).abs() > i64::from(thresh) || (q1 - q0).abs() > i64::from(thresh);
    let limit_bd = i64::from(limit);
    let blimit_bd = i64::from(blimit);
    let mut mask = false;
    mask |= (p3 - p2).abs() > limit_bd;
    mask |= (p2 - p1).abs() > limit_bd;
    mask |= (p1 - p0).abs() > limit_bd;
    mask |= (q1 - q0).abs() > limit_bd;
    mask |= (q2 - q1).abs() > limit_bd;
    mask |= (q3 - q2).abs() > limit_bd;
    mask |= (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 > blimit_bd;
    let filter_mask = !mask;
    let mut flat_mask = true;
    if filter_size >= TX_8X8 {
        let mut not_flat = false;
        not_flat |= (p1 - p0).abs() > 1;
        not_flat |= (q1 - q0).abs() > 1;
        not_flat |= (p2 - p0).abs() > 1;
        not_flat |= (q2 - q0).abs() > 1;
        not_flat |= (p3 - p0).abs() > 1;
        not_flat |= (q3 - q0).abs() > 1;
        flat_mask = !not_flat;
    }
    let mut flat_mask2 = true;
    if filter_size >= TX_16X16 {
        let q4 = after(4);
        let q5 = after(5);
        let q6 = after(6);
        let q7 = after(7);
        let p4 = before(4);
        let p5 = before(5);
        let p6 = before(6);
        let p7 = before(7);
        let mut not_flat = false;
        not_flat |= (p7 - p0).abs() > 1;
        not_flat |= (q7 - q0).abs() > 1;
        not_flat |= (p6 - p0).abs() > 1;
        not_flat |= (q6 - q0).abs() > 1;
        not_flat |= (p5 - p0).abs() > 1;
        not_flat |= (q5 - q0).abs() > 1;
        not_flat |= (p4 - p0).abs() > 1;
        not_flat |= (q4 - q0).abs() > 1;
        flat_mask2 = !not_flat;
    }
    (hev_mask, filter_mask, flat_mask, flat_mask2)
}

/// Narrow filter, spec section 8.8.5.2. Modifies one sample on
/// each side of the edge, or two when there is no high edge
/// variance.
fn narrow_filter(
    frame: &mut Vp9FrameBuffer,
    plane: usize,
    x: usize,
    y: usize,
    dx: i64,
    dy: i64,
    hev_mask: bool,
) {
    let x = x as i64;
    let y = y as i64;
    let q0 = sample_at(frame, plane, x, y);
    let q1 = sample_at(frame, plane, x + dx, y + dy);
    let p0 = sample_at(frame, plane, x - dx, y - dy);
    let p1 = sample_at(frame, plane, x - 2 * dx, y - 2 * dy);
    let ps1 = p1 - 128;
    let ps0 = p0 - 128;
    let qs0 = q0 - 128;
    let qs1 = q1 - 128;
    let filter4_clamp = |v: i64| clip3(v, -128, 127);
    let mut filter = if hev_mask {
        filter4_clamp(ps1 - qs1)
    } else {
        0
    };
    filter = filter4_clamp(filter + 3 * (qs0 - ps0));
    let filter1 = filter4_clamp(filter + 4) >> 3;
    let filter2 = filter4_clamp(filter + 3) >> 3;
    let oq0 = filter4_clamp(qs0 - filter1) + 128;
    let op0 = filter4_clamp(ps0 + filter2) + 128;
    set_sample(frame, plane, x, y, oq0);
    set_sample(frame, plane, x - dx, y - dy, op0);
    if !hev_mask {
        let filter = round2(filter1, 1);
        let oq1 = filter4_clamp(qs1 - filter) + 128;
        let op1 = filter4_clamp(ps1 + filter) + 128;
        set_sample(frame, plane, x + dx, y + dy, oq1);
        set_sample(frame, plane, x - 2 * dx, y - 2 * dy, op1);
    }
}

/// Wide filter, spec section 8.8.5.3. Averages `2 * half + 2`
/// samples around the edge, where `half = (1 << (log2_size - 1)) -
/// 1`. All filtered values are computed before any are written,
/// so the filter reads the pre-filter samples.
fn wide_filter(
    frame: &mut Vp9FrameBuffer,
    plane: usize,
    edge_x: usize,
    edge_y: usize,
    dx: i64,
    dy: i64,
    log2_size: u32,
) {
    let half = (1i64 << (log2_size - 1)) - 1;
    let px = edge_x as i64;
    let py = edge_y as i64;
    // filtered[idx] for idx in -half..half, stored at index idx + half.
    let mut filtered = [0i64; 16];
    for idx in -half..half {
        let mut total = sample_at(frame, plane, px + dx * idx, py + dy * idx);
        for off in -half..=half {
            let clamped = clip3(idx + off, -(half + 1), half);
            total += sample_at(frame, plane, px + dx * clamped, py + dy * clamped);
        }
        filtered[(idx + half) as usize] = round2(total, log2_size);
    }
    for idx in -half..half {
        set_sample(
            frame,
            plane,
            px + dx * idx,
            py + dy * idx,
            filtered[(idx + half) as usize],
        );
    }
}
