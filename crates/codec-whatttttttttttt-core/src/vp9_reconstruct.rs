// Casts in this module are bounded by construction: block and
// sample indices are range-checked against the MI grid and the
// plane bounds before use, and probability values are 8-bit.
// The pedantic cast lints therefore do not find real truncation
// risks here.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]

//! VP9 frame reconstruction pipeline, spec sections 6.4 and 8.
//!
//! VP9 Bitstream & Decoding Process Specification v0.6, sections
//! 6.1 (frame flow), 6.4 (tile decoding), 8.1 (frame decoding),
//! 8.5 (prediction), 8.6 (reconstruction), 8.8 (loop filter),
//! 8.9 (output) and 8.10 (reference update).
//!
//! The parser (`vp9_bitstream`) supplies the frame header and
//! the tile framing; this module decodes the tiles, reconstructs
//! the frame, applies the loop filter, and manages the reference
//! frames. The compressed-header probabilities are re-parsed here
//! (see `vp9_token::parse_compressed_header_probs`) because the
//! parser counts probability updates without keeping the values.
//!
//! Only profile 0, 8-bit, 4:2:0 is reconstructed; anything else
//! is rejected by `Vp9FrameHeader::validate_subset`.

use crate::error::{Error, Result};
use crate::frame::{ColourRange, ColourSpace, PixelFormat, VideoFrame};
use crate::vp9_bitstream::{
    Vp9BoolDecoder, Vp9Frame, Vp9FrameHeader, Vp9InterpolationFilter, Vp9Tile,
};
use crate::vp9_loopfilter::{apply_loop_filter, Vp9LoopFilterLevels};
use crate::vp9_predict::{
    append_sub8x8_mvs, comp_mode_context, comp_ref_context, find_best_ref_mvs, find_mv_refs,
    get_plane_block_size, get_uv_tx_size, interp_filter_context, intra_mode_neighbors,
    is_inter_context, partition_context, predict_inter, predict_intra, read_mv, seg_pred_context,
    single_ref_p1_context, single_ref_p2_context, skip_context, tx_size_context, Vp9FrameBuffer,
    Vp9ModeMaps, Vp9Neighbors, NONE_FRAME,
};
use crate::vp9_token::{
    clip1, decode_block_tokens, get_ac_quant, get_dc_quant, inverse_transform_2d,
    parse_compressed_header_probs, read_tree, seg_feature_active, Vp9Counts, Vp9FrameContext,
    ALTREF_FRAME, BILINEAR, BLOCK_64X64, BLOCK_8X8, B_HEIGHT_LOG2, B_WIDTH_LOG2,
    COLS_PARTITION_TREE, DCT_DCT, EIGHTTAP, EIGHTTAP_SHARP, EIGHTTAP_SMOOTH, GOLDEN_FRAME,
    INTERP_FILTER_TREE, INTER_MODE_TREE, INTRA_FRAME, INTRA_MODE_TREE, KF_PARTITION_PROBS,
    KF_UV_MODE_PROB, KF_Y_MODE_PROB, LAST_FRAME, MAX_TXSIZE, MI_SIZE, MODE2TXFM, NEARESTMV, NEARMV,
    NEWMV, NUM_4X4_HIGH, NUM_4X4_WIDE, NUM_8X8_HIGH, NUM_8X8_WIDE, PARTITION_HORZ, PARTITION_NONE,
    PARTITION_SPLIT, PARTITION_TREE, PARTITION_VERT, REF_MODE_COMPOUND, REF_MODE_SELECT,
    ROWS_PARTITION_TREE, SEGMENT_TREE, SEG_LVL_REF_FRAME, SEG_LVL_SKIP, SIZE_GROUP, SUBSIZE,
    SWITCHABLE, TX_16X16, TX_32X32, TX_8X8, TX_MODE_SELECT, TX_MODE_TO_BIGGEST, TX_SIZE_16_TREE,
    TX_SIZE_32_TREE, TX_SIZE_8_TREE, ZEROMV,
};

/// Reconstruction state carried across frames: the four saved
/// frame contexts, the eight reference frame slots, the previous
/// frame's mode maps (for `use_prev_frame_mvs`), and the
/// previous frame's dimensions and flags (also for
/// `use_prev_frame_mvs`, spec section 6.1).
pub struct Vp9ReconstructionState {
    /// Saved frame contexts, spec section 6.1.
    saved_contexts: [Vp9FrameContext; 4],
    /// Reference frame store, spec section 8.10, indexed by slot.
    frame_store: [Option<Vp9FrameBuffer>; 8],
    /// Previous frame's mode maps, or `None` before the first frame.
    prev_maps: Option<Vp9ModeMaps>,
    /// Previous frame's visible width, for `use_prev_frame_mvs`.
    last_width: usize,
    /// Previous frame's visible height.
    last_height: usize,
    /// Previous frame's `intra_only` flag.
    last_intra_only: bool,
    /// Previous frame's `show_frame` flag.
    last_show_frame: bool,
    /// Whether the previous frame was a key frame.
    last_is_keyframe: bool,
}

impl Vp9ReconstructionState {
    /// Creates fresh state. The saved contexts start at the spec
    /// defaults; the first coded frame must be a key frame.
    #[must_use]
    pub fn new() -> Self {
        Self {
            saved_contexts: [
                Vp9FrameContext::new_default(),
                Vp9FrameContext::new_default(),
                Vp9FrameContext::new_default(),
                Vp9FrameContext::new_default(),
            ],
            frame_store: [None, None, None, None, None, None, None, None],
            prev_maps: None,
            last_width: 0,
            last_height: 0,
            last_intra_only: false,
            last_show_frame: false,
            last_is_keyframe: true,
        }
    }
}

impl Default for Vp9ReconstructionState {
    fn default() -> Self {
        Self::new()
    }
}

/// Reconstructs one VP9 frame.
///
/// `frame` is a parsed frame (`parse_vp9_frame`); `tile_data` is
/// the full frame packet bytes (tile slices are taken from it via
/// `tile.offset` / `tile.size`); `refs` is the reconstruction
/// state carried across frames (reference frames, saved
/// probabilities, previous-frame mode maps).
///
/// Returns a YUV420 8-bit `VideoFrame` cropped to the frame's
/// visible size. A `show_existing_frame` packet returns the
/// stored frame instead of decoding. An inter frame whose
/// reference slot is empty, or a corrupt/truncated tile, returns
/// a typed error.
///
/// # Errors
///
/// Returns an error when the frame references an empty reference
/// slot, when a tile is corrupt or truncated, or when the frame
/// header fails subset validation.
pub fn reconstruct_vp9_frame(
    frame: &Vp9Frame,
    tile_data: &[u8],
    refs: &mut Vp9ReconstructionState,
) -> Result<VideoFrame> {
    let header = &frame.header;
    // A show_existing packet carries no dimensions or colour
    // info, so it is handled before subset validation.
    if header.show_existing_frame {
        return show_existing_frame(header, refs);
    }
    header.validate_subset()?;
    decode_frame(frame, tile_data, refs)
}

/// Returns a previously decoded frame, spec section 8.9.
fn show_existing_frame(
    header: &Vp9FrameHeader,
    refs: &Vp9ReconstructionState,
) -> Result<VideoFrame> {
    let idx = header.frame_to_show_map_idx as usize;
    let stored = refs
        .frame_store
        .get(idx)
        .and_then(|slot| slot.as_ref())
        .ok_or(Error::InvalidData {
            offset: 0,
            detail: "vp9 show_existing_frame references an empty slot",
        })?;
    Ok(build_video_frame(stored, header))
}

/// Decodes a normal (non-show-existing) frame: context reset and
/// load, compressed-header re-parse, tile decoding, probability
/// adaptation, loop filter, output, and reference update.
/// Spec sections 6.1, 8.1, 8.8, 8.9 and 8.10.
// The function is the per-frame pipeline; splitting it would
// scatter the frame state (context, maps, reference store) across
// helpers for no decode benefit, so the length stays.
#[allow(clippy::too_many_lines)]
fn decode_frame(
    frame: &Vp9Frame,
    tile_data: &[u8],
    refs: &mut Vp9ReconstructionState,
) -> Result<VideoFrame> {
    let header = &frame.header;
    let mi_cols = header.mi_cols as usize;
    let mi_rows = header.mi_rows as usize;
    let vis_width = header.frame_width as usize;
    let vis_height = header.frame_height as usize;
    // Frame-context reset, spec section 6.1.2: a key frame, an
    // error-resilient frame, or reset == 3 resets all four saved
    // contexts; reset == 2 resets the indexed context.
    if header.frame_is_intra || header.error_resilient_mode || header.reset_frame_context == 3 {
        for saved in &mut refs.saved_contexts {
            *saved = Vp9FrameContext::new_default();
        }
    } else if header.reset_frame_context == 2 {
        refs.saved_contexts[header.frame_context_idx as usize] = Vp9FrameContext::new_default();
    }
    // Load the working context from the saved slot.
    let saved_idx = header.frame_context_idx as usize;
    let base = refs.saved_contexts[saved_idx].clone();
    // Re-parse the compressed header with value tracking; the
    // parser counts updates but does not keep the values.
    let ctx = parse_compressed_header_probs(header, tile_data, frame.header_bytes, &base)?;
    // Whether the previous frame's motion vectors may be used,
    // spec section 6.1.
    let use_prev_mvs = !header.error_resilient_mode
        && vis_width == refs.last_width
        && vis_height == refs.last_height
        && !refs.last_intra_only
        && refs.last_show_frame
        && !refs.last_is_keyframe;
    let frame_buffer = Vp9FrameBuffer::new(mi_cols, mi_rows, vis_width, vis_height)?;
    let mut maps = Vp9ModeMaps::new(mi_rows, mi_cols);
    // Carry the previous frame's mode maps for MV prediction.
    // `prev_mvs` / `prev_ref_frames` are the previous frame's
    // current vectors and references (spec section 8.10);
    // `prev_segment_ids` is the previous frame's already-updated
    // segment-id map (spec section 8.1).
    if let Some(prev) = refs.prev_maps.take() {
        if prev.mi_rows == mi_rows && prev.mi_cols == mi_cols {
            maps.prev_mvs = prev.mvs;
            maps.prev_ref_frames = prev.ref_frames;
            maps.prev_segment_ids = prev.prev_segment_ids;
        }
    }
    let mut decoder = Vp9TileDecoder {
        header,
        ctx,
        counts: Vp9Counts::default(),
        maps,
        frame: frame_buffer,
        use_prev_mvs,
        ref_frame_idx: [
            header.ref_frame_idx[0] as usize,
            header.ref_frame_idx[1] as usize,
            header.ref_frame_idx[2] as usize,
        ],
        mi_row: 0,
        mi_col: 0,
        mi_size: 0,
        mi_row_start: 0,
        mi_col_start: 0,
        mi_row_end: 0,
        mi_col_end: 0,
        is_inter: false,
        skip: false,
        ref_frame: [INTRA_FRAME, NONE_FRAME],
        y_mode: 0,
        uv_mode: 0,
        sub_modes: [0; 4],
        segment_id: 0,
        tx_size: 0,
        interp_filter: EIGHTTAP,
        block_mvs: [[[0; 2]; 4]; 2],
        tokens: [0; 1024],
    };
    // Loop-filter levels are computed before tile decoding,
    // spec section 6.1; the filter itself runs after.
    let lf_levels = Vp9LoopFilterLevels::init(header);
    decoder.decode_tiles(frame, tile_data, &refs.frame_store)?;
    // Probability adaptation, spec section 8.4: the saved
    // context (not the working context) is the merge base.
    if !header.error_resilient_mode && !header.frame_parallel_decoding_mode {
        decoder.ctx.adapt_coef_probs(
            &base,
            &decoder.counts,
            header.frame_is_intra,
            refs.last_is_keyframe,
        );
        decoder
            .ctx
            .adapt_noncoef_probs(&base, &decoder.counts, header);
    }
    // Save the working context back when requested.
    if header.refresh_frame_context {
        refs.saved_contexts[saved_idx] = decoder.ctx.clone();
    }
    // Loop filter, spec section 8.8, after the whole frame is
    // reconstructed.
    if header.loop_filter_level != 0 {
        apply_loop_filter(
            &mut decoder.frame,
            &decoder.maps,
            &lf_levels,
            header.loop_filter_sharpness,
            1,
            1,
        );
    }
    let output = build_video_frame(&decoder.frame, header);
    // Reference update, spec section 8.10.
    for slot in 0..8 {
        if (header.refresh_frame_flags >> slot) & 1 == 1 {
            refs.frame_store[slot] = Some(clone_frame_buffer(&decoder.frame));
        }
    }
    // Previous-frame mode maps, spec sections 8.1 and 8.10.
    // Segment ids are only carried forward when segmentation is
    // enabled and the map is updated; otherwise the previous
    // frame's segment ids stay in `prev_segment_ids`.
    let mut maps = decoder.maps;
    if header.segmentation_enabled && header.segmentation_update_map {
        maps.prev_segment_ids.clone_from(&maps.segment_ids);
    }
    refs.prev_maps = Some(maps);
    refs.last_width = vis_width;
    refs.last_height = vis_height;
    refs.last_intra_only = header.intra_only;
    refs.last_show_frame = header.show_frame;
    refs.last_is_keyframe = header.is_keyframe;
    Ok(output)
}

/// Copies a frame buffer (the reference store owns its frames).
fn clone_frame_buffer(frame: &Vp9FrameBuffer) -> Vp9FrameBuffer {
    Vp9FrameBuffer {
        width: frame.width,
        height: frame.height,
        vis_width: frame.vis_width,
        vis_height: frame.vis_height,
        y: frame.y.clone(),
        u: frame.u.clone(),
        v: frame.v.clone(),
    }
}

/// Builds the output `VideoFrame`, spec section 8.9: the padded
/// frame is cropped to the visible size. Chroma is 4:2:0, so its
/// visible size is the ceiling of half the luma size.
fn build_video_frame(buffer: &Vp9FrameBuffer, header: &Vp9FrameHeader) -> VideoFrame {
    let vis_width = buffer.vis_width;
    let vis_height = buffer.vis_height;
    let uv_width = vis_width.div_ceil(2);
    let uv_height = vis_height.div_ceil(2);
    let mut y_plane = Vec::with_capacity(vis_width * vis_height);
    for row in 0..vis_height {
        y_plane.extend_from_slice(&buffer.y[row * buffer.width..row * buffer.width + vis_width]);
    }
    let mut u_plane = Vec::with_capacity(uv_width * uv_height);
    let mut v_plane = Vec::with_capacity(uv_width * uv_height);
    for row in 0..uv_height {
        let stride = buffer.uv_stride();
        u_plane.extend_from_slice(&buffer.u[row * stride..row * stride + uv_width]);
        v_plane.extend_from_slice(&buffer.v[row * stride..row * stride + uv_width]);
    }
    let colour_space = match header.color.color_space {
        crate::vp9_bitstream::Vp9ColorSpace::Bt709 => ColourSpace::Bt709,
        _ => ColourSpace::Bt601,
    };
    let colour_range = if header.color.color_range {
        ColourRange::Full
    } else {
        ColourRange::Limited
    };
    VideoFrame {
        width: vis_width as u32,
        height: vis_height as u32,
        format: PixelFormat::Yuv420,
        planes: vec![y_plane, u_plane, v_plane],
        strides: vec![vis_width, uv_width, uv_width],
        timestamp: 0,
        colour_space,
        colour_range,
    }
}

/// Interpolation filter value for the frame header, spec
/// section 6.2. `Switchable` means the filter is signaled per
/// block; the other values are the filter for every block.
fn header_interp_filter(header: &Vp9FrameHeader) -> u8 {
    match header.interpolation_filter {
        Vp9InterpolationFilter::EightTap => EIGHTTAP,
        Vp9InterpolationFilter::EightTapSmooth => EIGHTTAP_SMOOTH,
        Vp9InterpolationFilter::EightTapSharp => EIGHTTAP_SHARP,
        Vp9InterpolationFilter::Bilinear => BILINEAR,
        Vp9InterpolationFilter::Switchable => SWITCHABLE,
    }
}

// ---------------------------------------------------------------------------
// Tile decoding, spec section 6.4.
// ---------------------------------------------------------------------------

/// Per-frame tile decoder: the working frame context, the
/// probability counts, the mode maps, the frame being
/// reconstructed, and the current block's mode info. One
/// `Vp9TileDecoder` decodes one frame; the bool decoder is
/// per-tile and is threaded through as a parameter.
struct Vp9TileDecoder<'a> {
    /// Frame header (uncompressed + compressed).
    header: &'a Vp9FrameHeader,
    /// Working frame context (loaded from the saved slot and
    /// updated by the compressed header).
    ctx: Vp9FrameContext,
    /// Probability counts for adaptation, spec section 8.3.
    counts: Vp9Counts,
    /// Per-block mode maps for this frame.
    maps: Vp9ModeMaps,
    /// Frame being reconstructed (padded to the MI grid).
    frame: Vp9FrameBuffer,
    /// Whether the previous frame's MVs may be used, spec 6.1.
    use_prev_mvs: bool,
    /// Reference-frame slot index per ref list, spec 6.2.
    ref_frame_idx: [usize; 3],
    /// Current block's MI row.
    mi_row: usize,
    /// Current block's MI column.
    mi_col: usize,
    /// Current block's size index.
    mi_size: u8,
    /// Current tile's first MI row.
    mi_row_start: usize,
    /// Current tile's first MI column.
    mi_col_start: usize,
    /// Current tile's one-past-last MI row.
    mi_row_end: usize,
    /// Current tile's one-past-last MI column.
    mi_col_end: usize,
    /// Current block is inter.
    is_inter: bool,
    /// Current block is skipped (no residual).
    skip: bool,
    /// Current block's reference frames [0]/[1].
    ref_frame: [i8; 2],
    /// Current block's luma mode.
    y_mode: u8,
    /// Current block's chroma mode.
    uv_mode: u8,
    /// Current block's 4x4 sub-block luma modes.
    sub_modes: [u8; 4],
    /// Current block's segment id.
    segment_id: u8,
    /// Current block's transform size.
    tx_size: u8,
    /// Current block's interpolation filter.
    interp_filter: u8,
    /// Current block's sub-block MVs [ref list][sub-block].
    block_mvs: [[[i32; 2]; 4]; 2],
    /// Coefficient buffer for one transform block, raster order.
    tokens: [i32; 1024],
}

impl Vp9TileDecoder<'_> {
    /// Decodes all tiles of the frame, spec section 6.4.1. The
    /// above context is cleared once per frame, not per tile.
    fn decode_tiles(
        &mut self,
        frame: &Vp9Frame,
        tile_data: &[u8],
        ref_buffers: &[Option<Vp9FrameBuffer>; 8],
    ) -> Result<()> {
        self.maps.clear_above_context();
        for tile in &frame.tiles {
            self.decode_tile(tile, tile_data, ref_buffers)?;
        }
        Ok(())
    }

    /// Decodes one tile, spec section 6.4.2: a fresh bool
    /// decoder over the tile bytes, then the superblock raster.
    /// The left context is cleared at the start of each
    /// superblock row.
    fn decode_tile(
        &mut self,
        tile: &Vp9Tile,
        tile_data: &[u8],
        ref_buffers: &[Option<Vp9FrameBuffer>; 8],
    ) -> Result<()> {
        self.mi_row_start = tile.mi_row_start as usize;
        self.mi_col_start = tile.mi_col_start as usize;
        self.mi_row_end = tile.mi_row_end as usize;
        self.mi_col_end = tile.mi_col_end as usize;
        let end = tile
            .offset
            .checked_add(tile.size)
            .ok_or(Error::Overflow("vp9 tile offset overflow"))?;
        let tile_bytes = tile_data.get(tile.offset..end).ok_or(Error::InvalidData {
            offset: tile.offset,
            detail: "vp9 tile extends past the frame packet",
        })?;
        let mut dec = Vp9BoolDecoder::new(tile_bytes)?;
        let mut r = self.mi_row_start;
        while r < self.mi_row_end {
            self.maps.clear_left_context();
            let mut c = self.mi_col_start;
            while c < self.mi_col_end {
                self.decode_partition(&mut dec, r, c, BLOCK_64X64, ref_buffers)?;
                c += 8;
            }
            r += 8;
        }
        // The tile's bool stream must end cleanly (padding zero).
        dec.exit()?;
        Ok(())
    }

    /// Decodes a partition node, spec section 6.4.3. An 8x8
    /// node is decoded by a single `decode_block` call even when
    /// its subsize is below 8x8: the block covers the whole 8x8
    /// MI unit and its sub-block modes live in the 2x2 grid
    /// (libvpx `decode_partition`, `!hbs` branch).
    fn decode_partition(
        &mut self,
        dec: &mut Vp9BoolDecoder,
        r: usize,
        c: usize,
        bsize: u8,
        ref_buffers: &[Option<Vp9FrameBuffer>; 8],
    ) -> Result<()> {
        let mi_rows = self.header.mi_rows as usize;
        let mi_cols = self.header.mi_cols as usize;
        if r >= mi_rows || c >= mi_cols {
            return Ok(());
        }
        let num8x8 = NUM_8X8_WIDE[bsize as usize] as usize;
        let half = num8x8 >> 1;
        let has_rows = r + half < mi_rows;
        let has_cols = c + half < mi_cols;
        let partition = self.read_partition(dec, r, c, bsize, has_rows, has_cols)?;
        let subsize = SUBSIZE[partition as usize][bsize as usize];
        if subsize < 0 {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "vp9 partition is not valid for this block size",
            });
        }
        let subsize = subsize as u8;
        if subsize < BLOCK_8X8 || partition == PARTITION_NONE {
            self.decode_block(dec, r, c, subsize, ref_buffers)?;
        } else if partition == PARTITION_HORZ {
            self.decode_block(dec, r, c, subsize, ref_buffers)?;
            if has_rows {
                self.decode_block(dec, r + half, c, subsize, ref_buffers)?;
            }
        } else if partition == PARTITION_VERT {
            self.decode_block(dec, r, c, subsize, ref_buffers)?;
            if has_cols {
                self.decode_block(dec, r, c + half, subsize, ref_buffers)?;
            }
        } else {
            self.decode_partition(dec, r, c, subsize, ref_buffers)?;
            self.decode_partition(dec, r, c + half, subsize, ref_buffers)?;
            self.decode_partition(dec, r + half, c, subsize, ref_buffers)?;
            self.decode_partition(dec, r + half, c + half, subsize, ref_buffers)?;
        }
        // Partition-context update, spec section 6.4.3. A block
        // at the frame edge can run past the grid; out-of-range
        // entries are left alone.
        if bsize == BLOCK_8X8 || partition != PARTITION_SPLIT {
            for i in 0..num8x8 {
                if let Some(v) = self.maps.above_partition.get_mut(c + i) {
                    *v = 15 >> B_WIDTH_LOG2[subsize as usize];
                }
                if let Some(v) = self.maps.left_partition.get_mut(r + i) {
                    *v = 15 >> B_HEIGHT_LOG2[subsize as usize];
                }
            }
        }
        Ok(())
    }

    /// Reads the partition of a node, spec section 6.4.3 and
    /// 9.3.2. With neither rows nor columns available the
    /// partition is SPLIT without reading bits; that forced
    /// SPLIT is counted, matching the tree-read cases (if the
    /// frame-parallel-off fixture disagrees, this is the first
    /// thing to revisit).
    fn read_partition(
        &mut self,
        dec: &mut Vp9BoolDecoder,
        r: usize,
        c: usize,
        bsize: u8,
        has_rows: bool,
        has_cols: bool,
    ) -> Result<u8> {
        let ctx = partition_context(&self.maps, r, c, bsize, has_rows, has_cols);
        let probs: [u8; 3] = if self.header.frame_is_intra {
            KF_PARTITION_PROBS[ctx]
        } else {
            self.ctx.partition_probs[ctx]
        };
        let partition = if has_rows && has_cols {
            read_tree(dec, &PARTITION_TREE, &probs)?
        } else if has_cols {
            read_tree(dec, &COLS_PARTITION_TREE, &[probs[1]])?
        } else if has_rows {
            read_tree(dec, &ROWS_PARTITION_TREE, &[probs[2]])?
        } else {
            PARTITION_SPLIT as i8
        };
        let partition = partition as u8;
        self.counts.partition[ctx][partition as usize] += 1;
        Ok(partition)
    }

    /// Decodes one block, spec section 6.4.4: mode info,
    /// residual, then the mode-map fill. A block at the frame
    /// edge can run past the grid; out-of-range map entries
    /// are skipped.
    fn decode_block(
        &mut self,
        dec: &mut Vp9BoolDecoder,
        r: usize,
        c: usize,
        subsize: u8,
        ref_buffers: &[Option<Vp9FrameBuffer>; 8],
    ) -> Result<()> {
        self.mi_row = r;
        self.mi_col = c;
        self.mi_size = subsize;
        self.mode_info(dec)?;
        let any_nonzero = self.residual(dec, ref_buffers)?;
        // An inter block of 8x8 or larger with no coefficients
        // at all is skipped (spec section 6.4.4, libvpx
        // `decode_block`).
        if self.is_inter && subsize >= BLOCK_8X8 && !any_nonzero {
            self.skip = true;
        }
        let mi_rows = self.header.mi_rows as usize;
        let mi_cols = self.header.mi_cols as usize;
        for y in 0..NUM_8X8_HIGH[subsize as usize] as usize {
            for x in 0..NUM_8X8_WIDE[subsize as usize] as usize {
                let row = r + y;
                let col = c + x;
                if row >= mi_rows || col >= mi_cols {
                    continue;
                }
                self.maps.skips[row][col] = u8::from(self.skip);
                self.maps.tx_sizes[row][col] = self.tx_size;
                self.maps.mi_sizes[row][col] = self.mi_size;
                self.maps.y_modes[row][col] = self.y_mode;
                self.maps.segment_ids[row][col] = self.segment_id;
                self.maps.ref_frames[row][col] = self.ref_frame;
                if self.is_inter {
                    self.maps.interp_filters[row][col] = self.interp_filter;
                    // The whole-block MV is the bottom-right
                    // sub-block's MV, spec section 6.4.4.
                    self.maps.mvs[row][col][0] = self.block_mvs[0][3];
                    self.maps.mvs[row][col][1] = self.block_mvs[1][3];
                    self.maps.sub_mvs[row][col] = self.block_mvs;
                } else {
                    self.maps.sub_modes[row][col] = self.sub_modes;
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Mode info, spec sections 6.4.5 to 6.4.20.
// ---------------------------------------------------------------------------

impl Vp9TileDecoder<'_> {
    /// Reads the current block's mode info, spec section 6.4.5.
    fn mode_info(&mut self, dec: &mut Vp9BoolDecoder) -> Result<()> {
        if self.header.frame_is_intra {
            self.intra_frame_mode_info(dec)
        } else {
            self.inter_frame_mode_info(dec)
        }
    }

    /// Intra-frame mode info, spec section 6.4.6. The default
    /// intra modes use the keyframe tables (not the frame
    /// context) and are not counted, spec section 9.3.4.
    fn intra_frame_mode_info(&mut self, dec: &mut Vp9BoolDecoder) -> Result<()> {
        self.intra_segment_id(dec)?;
        self.read_skip(dec)?;
        self.read_tx_size(dec, true)?;
        self.ref_frame = [INTRA_FRAME, NONE_FRAME];
        self.is_inter = false;
        if self.mi_size >= BLOCK_8X8 {
            let mode = self.read_default_intra_mode(dec, 0, 0)?;
            self.y_mode = mode;
            self.sub_modes = [mode; 4];
        } else {
            let num_4x4_wide = NUM_4X4_WIDE[self.mi_size as usize] as usize;
            let num_4x4_high = NUM_4X4_HIGH[self.mi_size as usize] as usize;
            let mut mode = 0u8;
            for idy in (0..2).step_by(num_4x4_high) {
                for idx in (0..2).step_by(num_4x4_wide) {
                    mode = self.read_default_intra_mode(dec, idx, idy)?;
                    for y2 in 0..num_4x4_high {
                        for x2 in 0..num_4x4_wide {
                            self.sub_modes[(idy + y2) * 2 + idx + x2] = mode;
                        }
                    }
                }
            }
            self.y_mode = mode;
        }
        self.uv_mode = self.read_default_uv_mode(dec)?;
        Ok(())
    }

    /// Default intra mode from the keyframe table, spec sections
    /// 6.4.6 and 9.3.2. `idx`/`idy` are the sub-block
    /// coordinates within the current block.
    fn read_default_intra_mode(
        &mut self,
        dec: &mut Vp9BoolDecoder,
        idx: usize,
        idy: usize,
    ) -> Result<u8> {
        let avail_u = self.mi_row > 0;
        let avail_l = self.mi_col > self.mi_col_start;
        let (above, left) = intra_mode_neighbors(
            &self.maps,
            self.mi_row,
            self.mi_col,
            self.mi_size,
            avail_u,
            avail_l,
            idx,
            idy,
            self.sub_modes,
        );
        let mode = read_tree(
            dec,
            &INTRA_MODE_TREE,
            &KF_Y_MODE_PROB[above as usize][left as usize],
        )?;
        Ok(mode as u8)
    }

    /// Default chroma mode from the keyframe table, spec section
    /// 6.4.6. Not counted.
    fn read_default_uv_mode(&mut self, dec: &mut Vp9BoolDecoder) -> Result<u8> {
        let mode = read_tree(
            dec,
            &INTRA_MODE_TREE,
            &KF_UV_MODE_PROB[self.y_mode as usize],
        )?;
        Ok(mode as u8)
    }

    /// Intra segment id, spec section 6.4.7. Not counted.
    fn intra_segment_id(&mut self, dec: &mut Vp9BoolDecoder) -> Result<()> {
        if self.header.segmentation_enabled && self.header.segmentation_update_map {
            let probs = seg_tree_probs_u8(self.header);
            let id = read_tree(dec, &SEGMENT_TREE, &probs)?;
            self.segment_id = id as u8;
        } else {
            self.segment_id = 0;
        }
        Ok(())
    }

    /// Skip flag, spec section 6.4.8.
    fn read_skip(&mut self, dec: &mut Vp9BoolDecoder) -> Result<()> {
        if seg_feature_active(self.header, self.segment_id as usize, SEG_LVL_SKIP) {
            self.skip = true;
            return Ok(());
        }
        let n = Vp9Neighbors::at(&self.maps, self.mi_row, self.mi_col, self.mi_col_start);
        let ctx = skip_context(&self.maps, self.mi_row, self.mi_col, &n);
        let skip = dec.read_bool(u32::from(self.ctx.skip_prob[ctx]))?;
        self.counts.skip[ctx][usize::from(skip)] += 1;
        self.skip = skip;
        Ok(())
    }

    /// Transform size, spec section 6.4.10.
    fn read_tx_size(&mut self, dec: &mut Vp9BoolDecoder, allow_select: bool) -> Result<()> {
        let max_tx_size = MAX_TXSIZE[self.mi_size as usize];
        if allow_select && self.ctx.tx_mode == TX_MODE_SELECT && self.mi_size >= BLOCK_8X8 {
            let n = Vp9Neighbors::at(&self.maps, self.mi_row, self.mi_col, self.mi_col_start);
            let ctx = tx_size_context(&self.maps, self.mi_row, self.mi_col, &n, max_tx_size);
            let tx_size = match max_tx_size {
                TX_8X8 => read_tree(dec, &TX_SIZE_8_TREE, &self.ctx.tx_probs_8x8[ctx])?,
                TX_16X16 => read_tree(dec, &TX_SIZE_16_TREE, &self.ctx.tx_probs_16x16[ctx])?,
                _ => read_tree(dec, &TX_SIZE_32_TREE, &self.ctx.tx_probs_32x32[ctx])?,
            };
            let tx_size = tx_size as u8;
            self.counts.tx_size[(max_tx_size - 1) as usize][ctx][tx_size as usize] += 1;
            self.tx_size = tx_size;
        } else {
            self.tx_size = max_tx_size.min(TX_MODE_TO_BIGGEST[self.ctx.tx_mode as usize]);
        }
        Ok(())
    }

    /// Inter-frame mode info, spec section 6.4.11.
    fn inter_frame_mode_info(&mut self, dec: &mut Vp9BoolDecoder) -> Result<()> {
        self.inter_segment_id(dec)?;
        self.read_skip(dec)?;
        self.read_is_inter(dec)?;
        let allow_select = !self.skip || !self.is_inter;
        self.read_tx_size(dec, allow_select)?;
        if self.is_inter {
            self.inter_block_mode_info(dec)
        } else {
            self.intra_block_mode_info(dec)
        }
    }

    /// Inter segment id, spec section 6.4.12.
    fn inter_segment_id(&mut self, dec: &mut Vp9BoolDecoder) -> Result<()> {
        if !self.header.segmentation_enabled {
            self.segment_id = 0;
            return Ok(());
        }
        let predicted = self.get_segment_id();
        if self.header.segmentation_update_map {
            if self.header.segmentation_temporal_update {
                let ctx = seg_pred_context(&self.maps, self.mi_row, self.mi_col);
                let prob = self.header.segmentation_pred_probs[ctx];
                let seg_id_predicted = dec.read_bool(prob)?;
                if seg_id_predicted {
                    self.segment_id = predicted;
                } else {
                    let probs = seg_tree_probs_u8(self.header);
                    let id = read_tree(dec, &SEGMENT_TREE, &probs)?;
                    self.segment_id = id as u8;
                }
                for i in 0..NUM_8X8_WIDE[self.mi_size as usize] as usize {
                    if let Some(v) = self.maps.above_seg_pred.get_mut(self.mi_col + i) {
                        *v = u8::from(seg_id_predicted);
                    }
                }
                for i in 0..NUM_8X8_HIGH[self.mi_size as usize] as usize {
                    if let Some(v) = self.maps.left_seg_pred.get_mut(self.mi_row + i) {
                        *v = u8::from(seg_id_predicted);
                    }
                }
            } else {
                let probs = seg_tree_probs_u8(self.header);
                let id = read_tree(dec, &SEGMENT_TREE, &probs)?;
                self.segment_id = id as u8;
            }
        } else {
            self.segment_id = predicted;
        }
        Ok(())
    }

    /// Predicted segment id: the smallest value in the
    /// on-screen region covered by the block, spec 6.4.14.
    fn get_segment_id(&self) -> u8 {
        let bw = NUM_8X8_WIDE[self.mi_size as usize] as usize;
        let bh = NUM_8X8_HIGH[self.mi_size as usize] as usize;
        let mi_cols = self.header.mi_cols as usize;
        let mi_rows = self.header.mi_rows as usize;
        let xmis = (mi_cols - self.mi_col).min(bw);
        let ymis = (mi_rows - self.mi_row).min(bh);
        let mut seg = 7u8;
        for y in 0..ymis {
            for x in 0..xmis {
                if let Some(&v) = self
                    .maps
                    .prev_segment_ids
                    .get(self.mi_row + y)
                    .and_then(|row| row.get(self.mi_col + x))
                {
                    seg = seg.min(v);
                }
            }
        }
        seg
    }

    /// Is-inter flag, spec section 6.4.13.
    fn read_is_inter(&mut self, dec: &mut Vp9BoolDecoder) -> Result<()> {
        if seg_feature_active(self.header, self.segment_id as usize, SEG_LVL_REF_FRAME) {
            self.is_inter = self.header.segmentation_features[self.segment_id as usize]
                [SEG_LVL_REF_FRAME]
                != i32::from(INTRA_FRAME);
            return Ok(());
        }
        let n = Vp9Neighbors::at(&self.maps, self.mi_row, self.mi_col, self.mi_col_start);
        let ctx = is_inter_context(&n);
        let is_inter = dec.read_bool(u32::from(self.ctx.is_inter_prob[ctx]))?;
        self.counts.is_inter[ctx][usize::from(is_inter)] += 1;
        self.is_inter = is_inter;
        Ok(())
    }

    /// Intra-block mode info inside an inter frame, spec section
    /// 6.4.15. Intra modes use the frame-context tables here
    /// (not the keyframe tables) and are counted.
    fn intra_block_mode_info(&mut self, dec: &mut Vp9BoolDecoder) -> Result<()> {
        self.ref_frame = [INTRA_FRAME, NONE_FRAME];
        if self.mi_size >= BLOCK_8X8 {
            let mode = self.read_intra_mode(dec)?;
            self.y_mode = mode;
            self.sub_modes = [mode; 4];
        } else {
            let num_4x4_wide = NUM_4X4_WIDE[self.mi_size as usize] as usize;
            let num_4x4_high = NUM_4X4_HIGH[self.mi_size as usize] as usize;
            let mut mode = 0u8;
            for idy in (0..2).step_by(num_4x4_high) {
                for idx in (0..2).step_by(num_4x4_wide) {
                    mode = self.read_sub_intra_mode(dec)?;
                    for y2 in 0..num_4x4_high {
                        for x2 in 0..num_4x4_wide {
                            self.sub_modes[(idy + y2) * 2 + idx + x2] = mode;
                        }
                    }
                }
            }
            self.y_mode = mode;
        }
        self.uv_mode = self.read_uv_mode(dec)?;
        Ok(())
    }

    /// Intra mode for a whole block, spec sections 6.4.15 and
    /// 9.3.2: frame-context table, context = size group.
    fn read_intra_mode(&mut self, dec: &mut Vp9BoolDecoder) -> Result<u8> {
        let ctx = SIZE_GROUP[self.mi_size as usize] as usize;
        let mode = read_tree(dec, &INTRA_MODE_TREE, &self.ctx.y_mode_probs[ctx])?;
        self.counts.intra_mode[ctx][mode as usize] += 1;
        Ok(mode as u8)
    }

    /// Intra mode for a 4x4 sub-block, spec sections 6.4.15 and
    /// 9.3.2: frame-context table, context 0.
    fn read_sub_intra_mode(&mut self, dec: &mut Vp9BoolDecoder) -> Result<u8> {
        let mode = read_tree(dec, &INTRA_MODE_TREE, &self.ctx.y_mode_probs[0])?;
        self.counts.intra_mode[0][mode as usize] += 1;
        Ok(mode as u8)
    }

    /// Chroma mode, spec sections 6.4.15 and 9.3.2:
    /// frame-context table, context = luma mode.
    fn read_uv_mode(&mut self, dec: &mut Vp9BoolDecoder) -> Result<u8> {
        let mode = read_tree(
            dec,
            &INTRA_MODE_TREE,
            &self.ctx.uv_mode_probs[self.y_mode as usize],
        )?;
        self.counts.uv_mode[self.y_mode as usize][mode as usize] += 1;
        Ok(mode as u8)
    }

    /// Inter-block mode info, spec section 6.4.16.
    // The function is the inter-mode state machine (reference
    // frames, skip/inter flags, mode, interpolation filter, MV
    // assignment for both the 8x8 and sub-8x8 paths); splitting it
    // would scatter the block state across helpers for no decode
    // benefit, so the length stays.
    #[allow(clippy::too_many_lines)]
    fn inter_block_mode_info(&mut self, dec: &mut Vp9BoolDecoder) -> Result<()> {
        self.read_ref_frames(dec)?;
        let mut nearest_mv = [[0i32; 2]; 2];
        let mut near_mv = [[0i32; 2]; 2];
        let mut best_mv = [[0i32; 2]; 2];
        for j in 0..2 {
            if self.ref_frame[j] > INTRA_FRAME {
                let candidates = find_mv_refs(
                    &self.maps,
                    self.mi_row,
                    self.mi_col,
                    self.mi_size,
                    self.ref_frame[j],
                    -1,
                    self.use_prev_mvs,
                    self.header.ref_frame_sign_bias,
                    self.mi_col_start,
                    self.mi_col_end,
                );
                self.maps.mode_context[self.ref_frame[j] as usize] = candidates.mode_context;
                let (nearest, near, best) = find_best_ref_mvs(
                    candidates.list,
                    self.header.allow_high_precision_mv,
                    self.mi_row,
                    self.mi_col,
                    self.mi_size,
                    self.header.mi_rows as usize,
                    self.header.mi_cols as usize,
                );
                nearest_mv[j] = nearest;
                near_mv[j] = near;
                best_mv[j] = best;
            }
        }
        let is_compound = self.ref_frame[1] > INTRA_FRAME;
        if seg_feature_active(self.header, self.segment_id as usize, SEG_LVL_SKIP) {
            self.y_mode = ZEROMV;
        } else if self.mi_size >= BLOCK_8X8 {
            let ctx = self.maps.mode_context[self.ref_frame[0] as usize] as usize;
            let inter_mode = read_tree(dec, &INTER_MODE_TREE, &self.ctx.inter_mode_probs[ctx])?;
            self.counts.inter_mode[ctx][inter_mode as usize] += 1;
            self.y_mode = NEARESTMV + inter_mode as u8;
        }
        if self.header.interpolation_filter == Vp9InterpolationFilter::Switchable {
            let n = Vp9Neighbors::at(&self.maps, self.mi_row, self.mi_col, self.mi_col_start);
            let ctx = interp_filter_context(&self.maps, self.mi_row, self.mi_col, &n);
            let filter = read_tree(dec, &INTERP_FILTER_TREE, &self.ctx.interp_filter_probs[ctx])?;
            self.counts.interp_filter[ctx][filter as usize] += 1;
            self.interp_filter = filter as u8;
        } else {
            self.interp_filter = header_interp_filter(self.header);
        }
        if self.mi_size < BLOCK_8X8 {
            let num_4x4_wide = NUM_4X4_WIDE[self.mi_size as usize] as usize;
            let num_4x4_high = NUM_4X4_HIGH[self.mi_size as usize] as usize;
            for idy in (0..2).step_by(num_4x4_high) {
                for idx in (0..2).step_by(num_4x4_wide) {
                    let ctx = self.maps.mode_context[self.ref_frame[0] as usize] as usize;
                    let inter_mode =
                        read_tree(dec, &INTER_MODE_TREE, &self.ctx.inter_mode_probs[ctx])?;
                    self.counts.inter_mode[ctx][inter_mode as usize] += 1;
                    self.y_mode = NEARESTMV + inter_mode as u8;
                    if self.y_mode == NEARESTMV || self.y_mode == NEARMV {
                        for j in 0..=usize::from(is_compound) {
                            let (nearest, near) = append_sub8x8_mvs(
                                &self.maps,
                                self.mi_row,
                                self.mi_col,
                                self.mi_size,
                                (idy * 2 + idx) as i32,
                                self.ref_frame[j],
                                self.block_mvs[j],
                                self.use_prev_mvs,
                                self.header.ref_frame_sign_bias,
                                self.mi_col_start,
                                self.mi_col_end,
                            );
                            nearest_mv[j] = nearest;
                            near_mv[j] = near;
                        }
                    }
                    let mv = self.assign_mv(dec, is_compound, nearest_mv, near_mv, best_mv)?;
                    for y2 in 0..num_4x4_high {
                        for x2 in 0..num_4x4_wide {
                            let block = (idy + y2) * 2 + idx + x2;
                            for (ref_list, block_mvs) in self
                                .block_mvs
                                .iter_mut()
                                .enumerate()
                                .take(1 + usize::from(is_compound))
                            {
                                block_mvs[block] = mv[ref_list];
                            }
                        }
                    }
                }
            }
        } else {
            let mv = self.assign_mv(dec, is_compound, nearest_mv, near_mv, best_mv)?;
            for (ref_list, block_mvs) in self
                .block_mvs
                .iter_mut()
                .enumerate()
                .take(1 + usize::from(is_compound))
            {
                for slot in block_mvs.iter_mut() {
                    *slot = mv[ref_list];
                }
            }
        }
        Ok(())
    }

    /// Assigns the block's motion vectors, spec section 6.4.18.
    /// Returns the MV per ref list.
    fn assign_mv(
        &mut self,
        dec: &mut Vp9BoolDecoder,
        is_compound: bool,
        nearest_mv: [[i32; 2]; 2],
        near_mv: [[i32; 2]; 2],
        best_mv: [[i32; 2]; 2],
    ) -> Result<[[i32; 2]; 2]> {
        let mut mv = [[0i32; 2]; 2];
        for i in 0..=usize::from(is_compound) {
            mv[i] = if self.y_mode == NEWMV {
                read_mv(
                    dec,
                    &self.ctx,
                    &mut self.counts,
                    best_mv[i],
                    self.header.allow_high_precision_mv,
                )?
            } else if self.y_mode == NEARESTMV {
                nearest_mv[i]
            } else if self.y_mode == NEARMV {
                near_mv[i]
            } else {
                [0, 0]
            };
        }
        Ok(mv)
    }

    /// Reference frames, spec section 6.4.17.
    fn read_ref_frames(&mut self, dec: &mut Vp9BoolDecoder) -> Result<()> {
        if seg_feature_active(self.header, self.segment_id as usize, SEG_LVL_REF_FRAME) {
            self.ref_frame[0] = self.header.segmentation_features[self.segment_id as usize]
                [SEG_LVL_REF_FRAME] as i8;
            self.ref_frame[1] = NONE_FRAME;
            return Ok(());
        }
        let n = Vp9Neighbors::at(&self.maps, self.mi_row, self.mi_col, self.mi_col_start);
        let comp_mode = if self.ctx.reference_mode == REF_MODE_SELECT {
            let ctx = comp_mode_context(&n, self.ctx.comp_fixed_ref);
            let bit = dec.read_bool(u32::from(self.ctx.comp_mode_prob[ctx]))?;
            self.counts.comp_mode[ctx][usize::from(bit)] += 1;
            u8::from(bit)
        } else {
            self.ctx.reference_mode
        };
        if comp_mode == REF_MODE_COMPOUND {
            let idx = usize::from(
                self.header.ref_frame_sign_bias[(self.ctx.comp_fixed_ref - 1) as usize],
            );
            let ctx = comp_ref_context(
                &n,
                self.ctx.comp_fixed_ref,
                self.ctx.comp_var_ref,
                self.header.ref_frame_sign_bias,
            );
            let comp_ref = dec.read_bool(u32::from(self.ctx.comp_ref_prob[ctx]))?;
            self.counts.comp_ref[ctx][usize::from(comp_ref)] += 1;
            self.ref_frame[idx] = self.ctx.comp_fixed_ref;
            self.ref_frame[1 - idx] = self.ctx.comp_var_ref[usize::from(comp_ref)];
        } else {
            let ctx1 = single_ref_p1_context(&n);
            let p1 = dec.read_bool(u32::from(self.ctx.single_ref_prob[ctx1][0]))?;
            self.counts.single_ref[ctx1][0][usize::from(p1)] += 1;
            if p1 {
                let ctx2 = single_ref_p2_context(&n);
                let p2 = dec.read_bool(u32::from(self.ctx.single_ref_prob[ctx2][1]))?;
                self.counts.single_ref[ctx2][1][usize::from(p2)] += 1;
                self.ref_frame[0] = if p2 { ALTREF_FRAME } else { GOLDEN_FRAME };
            } else {
                self.ref_frame[0] = LAST_FRAME;
            }
            self.ref_frame[1] = NONE_FRAME;
        }
        Ok(())
    }
}

/// Segmentation tree probabilities as bytes. The header stores
/// them as `u32`; the tree decoder reads `u8` probabilities and
/// the values are always in 0..=255.
fn seg_tree_probs_u8(header: &Vp9FrameHeader) -> [u8; 7] {
    let mut probs = [0u8; 7];
    for (i, p) in header.segmentation_tree_probs.iter().enumerate() {
        probs[i] = *p as u8;
    }
    probs
}

// ---------------------------------------------------------------------------
// Residual and reconstruction, spec sections 6.4.21 and 8.6.
// ---------------------------------------------------------------------------

impl Vp9TileDecoder<'_> {
    /// Residual of the current block, spec section 6.4.21.
    /// Returns whether any transform block had a nonzero
    /// coefficient (the `EobTotal > 0` of the spec).
    ///
    /// A block below 8x8 is decoded as part of its 8x8 MI unit:
    /// `decode_block` is called once per 8x8 unit and the
    /// residual covers the whole unit (libvpx `decode_partition`
    /// `!hbs` branch). The per-4x4-sub-block data (sub-modes,
    /// sub-block MVs) is indexed by the 2x2-grid position
    /// `y * num_4x4_wide + x` — the libvpx `bmi[(row << 1) + col]`
    /// convention. That is also the index `get_scan` uses for
    /// the transform type (spec section 6.4.25): the earlier
    /// reading of `blockIdx` as a per-row counter would select
    /// the wrong sub-mode for the off-diagonal transform
    /// blocks of a sub-8x8 unit.
    // The function is the per-block residual pipeline (inter
    // prediction, then per-plane coefficient decode and TX
    // reconstruction); splitting it would scatter the block
    // state across helpers for no decode benefit.
    #[allow(clippy::too_many_lines)]
    fn residual(
        &mut self,
        dec: &mut Vp9BoolDecoder,
        ref_buffers: &[Option<Vp9FrameBuffer>; 8],
    ) -> Result<bool> {
        let mi_row = self.mi_row;
        let mi_col = self.mi_col;
        let mi_size = self.mi_size;
        let is_inter = self.is_inter;
        let skip = self.skip;
        let tx_size = self.tx_size;
        let y_mode = self.y_mode;
        let uv_mode = self.uv_mode;
        let sub_modes = self.sub_modes;
        let ref_frame = self.ref_frame;
        let interp_filter = self.interp_filter;
        let lossless = self.header.lossless;
        let avail_u = mi_row > 0;
        let avail_l = mi_col > self.mi_col_start;
        let mi_cols = self.header.mi_cols as usize;
        let mi_rows = self.header.mi_rows as usize;
        // The plane size is clamped to 8x8 for sub-8x8 blocks so
        // the chroma size lookup stays valid (spec 6.4.21/6.4.23);
        // for luma that is the whole 8x8 MI unit, as above.
        let bsize = if mi_size < BLOCK_8X8 {
            BLOCK_8X8
        } else {
            mi_size
        };
        let mut any_nonzero = false;
        for plane in 0..3 {
            let tx_sz = if plane == 0 {
                tx_size
            } else {
                get_uv_tx_size(mi_size, tx_size)
            };
            let step = 1usize << tx_sz;
            let plane_sz = get_plane_block_size(bsize, plane);
            let num_4x4_wide = NUM_4X4_WIDE[plane_sz as usize] as usize;
            let num_4x4_high = NUM_4X4_HIGH[plane_sz as usize] as usize;
            let sub_x = usize::from(plane != 0);
            let sub_y = usize::from(plane != 0);
            let base_x = (mi_col * MI_SIZE) >> sub_x;
            let base_y = (mi_row * MI_SIZE) >> sub_y;
            // Inter prediction for the whole plane, before the
            // coefficients, spec section 6.4.21.
            if is_inter {
                if mi_size < BLOCK_8X8 {
                    for y in 0..num_4x4_high {
                        for x in 0..num_4x4_wide {
                            let block_idx = y * num_4x4_wide + x;
                            predict_inter(
                                &mut self.frame,
                                plane,
                                base_x + 4 * x,
                                base_y + 4 * y,
                                4,
                                4,
                                block_idx,
                                mi_size,
                                mi_row,
                                mi_col,
                                ref_frame,
                                self.ref_frame_idx,
                                &self.block_mvs,
                                interp_filter,
                                ref_buffers,
                                sub_x,
                                sub_y,
                            )?;
                        }
                    }
                } else {
                    predict_inter(
                        &mut self.frame,
                        plane,
                        base_x,
                        base_y,
                        num_4x4_wide * 4,
                        num_4x4_high * 4,
                        0,
                        mi_size,
                        mi_row,
                        mi_col,
                        ref_frame,
                        self.ref_frame_idx,
                        &self.block_mvs,
                        interp_filter,
                        ref_buffers,
                        sub_x,
                        sub_y,
                    )?;
                }
            }
            let max_x = (mi_cols * MI_SIZE) >> sub_x;
            let max_y = (mi_rows * MI_SIZE) >> sub_y;
            for y in (0..num_4x4_high).step_by(step) {
                for x in (0..num_4x4_wide).step_by(step) {
                    let start_x = base_x + 4 * x;
                    let start_y = base_y + 4 * y;
                    let mut nonzero = false;
                    if start_x < max_x && start_y < max_y {
                        // Mode and transform type for this
                        // transform block, spec section 6.4.25.
                        let block_idx = y * num_4x4_wide + x;
                        let (mode, tx_type) = if plane == 0 && mi_size < BLOCK_8X8 {
                            let m = sub_modes[block_idx];
                            let t = if lossless || is_inter || tx_sz == TX_32X32 {
                                DCT_DCT
                            } else {
                                MODE2TXFM[m as usize]
                            };
                            (m, t)
                        } else if plane == 0 {
                            let t = if lossless || is_inter || tx_sz == TX_32X32 {
                                DCT_DCT
                            } else {
                                MODE2TXFM[y_mode as usize]
                            };
                            (y_mode, t)
                        } else {
                            (uv_mode, DCT_DCT)
                        };
                        if !is_inter {
                            let stride = self.frame.plane_stride(plane);
                            let plane_max_x = self.frame.plane_width(plane) - 1;
                            let plane_max_y = self.frame.plane_height(plane) - 1;
                            let have_left = avail_l || x > 0;
                            let have_above = avail_u || y > 0;
                            let not_on_right = x + step < num_4x4_wide;
                            let plane_data = self.frame.plane_mut(plane);
                            predict_intra(
                                plane_data,
                                stride,
                                plane_max_x,
                                plane_max_y,
                                start_x,
                                start_y,
                                have_left,
                                have_above,
                                not_on_right,
                                tx_sz,
                                mode,
                            );
                        }
                        if !skip {
                            let above = &self.maps.above_nonzero[plane];
                            let left = &self.maps.left_nonzero[plane];
                            nonzero = decode_block_tokens(
                                dec,
                                &self.ctx,
                                &mut self.counts,
                                &mut self.tokens,
                                plane,
                                tx_sz,
                                tx_type,
                                is_inter,
                                start_x >> 2,
                                start_y >> 2,
                                above,
                                left,
                            )?;
                            if nonzero {
                                any_nonzero = true;
                                self.reconstruct_tx_block(plane, start_x, start_y, tx_sz, tx_type);
                            }
                        }
                    }
                    for i in 0..step {
                        let ax = (start_x >> 2) + i;
                        let ay = (start_y >> 2) + i;
                        if let Some(v) = self.maps.above_nonzero[plane].get_mut(ax) {
                            *v = u8::from(nonzero);
                        }
                        if let Some(v) = self.maps.left_nonzero[plane].get_mut(ay) {
                            *v = u8::from(nonzero);
                        }
                    }
                }
            }
        }
        Ok(any_nonzero)
    }

    /// Dequantizes one transform block and adds the inverse
    /// transform to the prediction, spec section 8.6. `tokens`
    /// holds the coefficients in raster order (written by
    /// `decode_block_tokens` through the scan table).
    fn reconstruct_tx_block(
        &mut self,
        plane: usize,
        start_x: usize,
        start_y: usize,
        tx_sz: u8,
        tx_type: u8,
    ) {
        let n = u32::from(tx_sz) + 2;
        let n0 = 1usize << n;
        let dq_denom = if tx_sz == TX_32X32 { 2 } else { 1 };
        let dc_quant = get_dc_quant(self.header, self.segment_id as usize, plane);
        let ac_quant = get_ac_quant(self.header, self.segment_id as usize, plane);
        let mut dequant = [[0i64; 32]; 32];
        for (i, dequant_row) in dequant.iter_mut().enumerate().take(n0) {
            for (j, slot) in dequant_row.iter_mut().enumerate().take(n0) {
                let token = self.tokens[i * n0 + j];
                let q = if i == 0 && j == 0 { dc_quant } else { ac_quant };
                // Integer division truncates toward zero, matching
                // the spec's `/` (section 4.1).
                *slot = (i64::from(token) * i64::from(q)) / i64::from(dq_denom);
            }
        }
        inverse_transform_2d(&mut dequant, n, tx_type, self.header.lossless);
        let stride = self.frame.plane_stride(plane);
        let plane_data = self.frame.plane_mut(plane);
        for (i, dequant_row) in dequant.iter().enumerate().take(n0) {
            for (j, &value) in dequant_row.iter().enumerate().take(n0) {
                let idx = (start_y + i) * stride + (start_x + j);
                // A transform block at the frame edge can extend
                // past the padded plane; those samples are
                // outside the output and are not written.
                if let Some(sample) = plane_data.get_mut(idx) {
                    *sample = clip1(i64::from(*sample) + value);
                }
            }
        }
    }
}
