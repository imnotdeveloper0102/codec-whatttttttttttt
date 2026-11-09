//! AV1 keyframe reconstruction pipeline (AV1 spec v1.0.0).
//!
//! Entry points are [`reconstruct_av1_keyframe`] (already-parsed
//! headers plus the frame OBU payload) and
//! [`reconstruct_av1_frame_obu`] (a raw frame OBU payload, parsed
//! here through the existing frame parser). The pipeline owns
//! subset gating, tile dispatch, and frame assembly; tile mode
//! decode lives in `av1_tile`, intra prediction and the inverse
//! transform in `av1_intra`.
//!
//! Status, 2026-11-09 — honest: the pipeline is wired end to end,
//! but it cannot return pixels yet. Tile mode decode walks the
//! whole tile and then returns `Unsupported` naming the missing
//! coefficient CDF tables (`COEFFICIENT_GAP`, spec section 9.3),
//! so today both entry points return that error after validating
//! everything else. The per-block reconstruction loop (predict
//! each transform block, add the dequantized residual) is the
//! next step once coefficient decode exists; it is not written
//! here as untestable speculative code.
//!
//! Post-reconstruction stages are NOT applied in this round and
//! are not gated on, because they are separable from the
//! reconstruction itself: the loop filter (spec section 8.8),
//! CDEF (section 8.7.4), and loop restoration (section 8.9).
//! The fixtures used for testing have the loop filter enabled
//! (`loop_filter_level` [2, 3, 8, 7]) with CDEF and restoration
//! off, so once pixels land, frames from streams with a nonzero
//! loop filter will differ from ffmpeg until the loop filter
//! lands too. Film grain is different: it is rejected by the
//! subset check, because grain synthesis changes every pixel
//! and is not separable in the same way.

use crate::av1_frame::{
    parse_frame_obu, FrameHeader as Av1FrameHeader, TileGroup, INTER_FRAME, INTRA_ONLY_FRAME,
    KEY_FRAME, SWITCH_FRAME,
};
use crate::av1_sequence::SequenceHeader;
use crate::av1_tile::Av1TileDecoder;
use crate::error::{Error, Result};
use crate::frame::{ColourRange, ColourSpace, PixelFormat, VideoFrame};
use crate::frame_pool::{frame_bytes, plane_bytes};
use crate::Av1ReferenceState;

/// Reconstructs one AV1 keyframe (spec sections 8.4, 8.6, 8.7)
/// from its already-parsed headers.
///
/// `frame_payload` is the frame OBU payload: tile offsets in
/// `tile_group` are relative to it (see `parse_tile_group_obu`).
/// `timestamp` is in microseconds and is copied onto the
/// returned `VideoFrame`.
///
/// Only keyframes and intra-only frames are accepted (profile 0,
/// 8-bit, 4:2:0, no film grain, no superres, no quantizer
/// matrix, no segmentation, no delta quantization, no intra
/// block copy). Inter frames return `Unsupported`: inter
/// prediction is a much larger piece of work and is not part of
/// this round.
///
/// # Errors
///
/// Returns `Unsupported` for anything outside the subset above,
/// `InvalidData` for a tile range outside `frame_payload` or a
/// frame with a zero side, `Overflow` when the frame would exceed
/// the 64 MiB frame cap, and today `Unsupported`
/// (`COEFFICIENT_GAP`) from tile decode, because coefficient
/// decode needs the default coefficient CDF tables (spec section
/// 9.3) which are not ported yet.
pub fn reconstruct_av1_keyframe(
    seq: &SequenceHeader,
    header: &Av1FrameHeader,
    tile_group: &TileGroup,
    frame_payload: &[u8],
    timestamp: u64,
) -> Result<VideoFrame> {
    validate_keyframe_subset(seq, header)?;
    let frame = allocate_frame(seq, header, timestamp)?;
    for tile in &tile_group.tiles {
        let tile_end = tile
            .offset
            .checked_add(tile.size)
            .ok_or(Error::Overflow("av1 tile range overflows usize"))?;
        let tile_data = frame_payload
            .get(tile.offset..tile_end)
            .ok_or(Error::InvalidData {
                offset: tile.offset,
                detail: "av1 tile range is outside the frame payload",
            })?;
        let mut decoder = Av1TileDecoder::new(seq, header, tile.index, tile_data)?;
        decoder.decode_tile()?;
        // TODO(coeff): once `decode_tile` returns the decoded
        // coefficients, reconstruct `decoder.blocks()` into
        // `frame.planes` (intra prediction per transform block
        // from `av1_intra`, then dequantized residual added on
        // top). Until then `decode_tile` returns
        // `Unsupported(COEFFICIENT_GAP)` above, so this point is
        // unreachable today.
    }
    Ok(frame)
}

/// Parses one frame OBU payload (spec section 5.10) and
/// reconstructs the keyframe it holds. `state` is the caller's
/// reference state, updated by the frame header parse exactly
/// as the standalone parser does.
///
/// # Errors
///
/// Returns whatever `parse_frame_obu` returns for a malformed
/// frame OBU, plus the errors of [`reconstruct_av1_keyframe`].
pub fn reconstruct_av1_frame_obu(
    frame_obu_payload: &[u8],
    seq: &SequenceHeader,
    state: &mut Av1ReferenceState,
    timestamp: u64,
) -> Result<VideoFrame> {
    let (header, tile_group) = parse_frame_obu(frame_obu_payload, seq, state, 0, 0)?;
    reconstruct_av1_keyframe(seq, &header, &tile_group, frame_obu_payload, timestamp)
}

/// Rejects anything outside the reconstruction subset. The
/// sequence check covers profile, bit depth, monochrome,
/// subsampling, film grain presence, and the 8192-pixel limit;
/// the frame checks cover the rest.
fn validate_keyframe_subset(seq: &SequenceHeader, header: &Av1FrameHeader) -> Result<()> {
    seq.validate_subset()?;
    match header.frame_type {
        KEY_FRAME | INTRA_ONLY_FRAME => {}
        INTER_FRAME | SWITCH_FRAME => {
            return Err(Error::Unsupported(
                "av1 inter prediction is not implemented in this round (keyframe intra only)",
            ));
        }
        _ => {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "av1 frame type is out of range",
            });
        }
    }
    if header.upscaled_width != header.frame_width {
        return Err(Error::Unsupported(
            "av1 superres is not implemented in this round",
        ));
    }
    if header.using_qmatrix {
        return Err(Error::Unsupported(
            "av1 quantizer matrix is not implemented in this round",
        ));
    }
    if header.segmentation_enabled {
        return Err(Error::Unsupported(
            "av1 segmentation is not implemented in this round",
        ));
    }
    if header.delta_q_present {
        return Err(Error::Unsupported(
            "av1 delta quantization is not implemented in this round",
        ));
    }
    if header.allow_intrabc {
        return Err(Error::Unsupported(
            "av1 intra block copy is not implemented in this round",
        ));
    }
    if header.grain.is_some() {
        return Err(Error::Unsupported("av1 film grain is not supported"));
    }
    if header.frame_width == 0 || header.frame_height == 0 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "av1 frame has a zero side",
        });
    }
    Ok(())
}

/// Allocates the zeroed YUV 4:2:0 output frame. `frame_bytes`
/// is the 64 MiB cap check; `plane_bytes` gives the exact plane
/// sizes (chroma planes are ceil-divided by two per side).
fn allocate_frame(
    seq: &SequenceHeader,
    header: &Av1FrameHeader,
    timestamp: u64,
) -> Result<VideoFrame> {
    let _ = frame_bytes(
        header.frame_width,
        header.frame_height,
        PixelFormat::Yuv420,
        8,
    )?;
    let sizes = plane_bytes(
        header.frame_width,
        header.frame_height,
        PixelFormat::Yuv420,
        8,
    )?;
    let planes: Vec<Vec<u8>> = sizes.iter().map(|size| vec![0u8; *size]).collect();
    let width = usize::try_from(header.frame_width)
        .map_err(|_| Error::Overflow("av1 frame width exceeds usize"))?;
    let chroma_width = usize::try_from(header.frame_width.div_ceil(2))
        .map_err(|_| Error::Overflow("av1 frame width exceeds usize"))?;
    Ok(VideoFrame {
        width: header.frame_width,
        height: header.frame_height,
        format: PixelFormat::Yuv420,
        planes,
        strides: vec![width, chroma_width, chroma_width],
        timestamp,
        colour_space: colour_space_for(seq)?,
        colour_range: colour_range_for(seq),
    })
}

/// Maps the sequence header's matrix coefficients (spec section
/// 5.5.2) onto the frame's colour space: 1 is BT.709, 6 is
/// BT.601, 2 is unspecified and takes the BT.601 default. 0 is
/// the identity matrix, which is not YUV at all, and anything
/// else (for example BT.2020) has no `ColourSpace` variant, so
/// both are rejected rather than mislabelled.
fn colour_space_for(seq: &SequenceHeader) -> Result<ColourSpace> {
    match seq.color.matrix_coefficients {
        1 => Ok(ColourSpace::Bt709),
        2 | 6 => Ok(ColourSpace::Bt601),
        _ => Err(Error::Unsupported(
            "av1 matrix coefficients are not supported for reconstruction",
        )),
    }
}

/// The sequence header's colour range flag (spec section 5.5.2):
/// set means full range, clear means limited (studio) range.
fn colour_range_for(seq: &SequenceHeader) -> ColourRange {
    if seq.color.color_range {
        ColourRange::Full
    } else {
        ColourRange::Limited
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::av1_obu::{ObuReader, ObuType};
    use crate::av1_sequence::parse_sequence_header;

    /// Parses the sequence header of frame 0 of the 64x64
    /// keyframe-only fixture (IVF: 32-byte header, then 12-byte
    /// frame headers). The fixture's matrix coefficients are 2
    /// (unspecified) and its colour range is limited.
    fn fixture_sequence_header() -> SequenceHeader {
        let ivf = include_bytes!("../tests/data/av1rec_64x64.ivf");
        let frame_len = u32::from_le_bytes(ivf[32..36].try_into().unwrap()) as usize;
        let frame = &ivf[44..44 + frame_len];
        let mut reader = ObuReader::new(frame);
        while let Some(obu) = reader.next_obu().unwrap() {
            if obu.header.obu_type == ObuType::SequenceHeader {
                return parse_sequence_header(obu.payload).unwrap();
            }
        }
        panic!("fixture has no sequence header OBU");
    }

    #[test]
    fn colour_space_maps_bt709_bt601_and_unspecified() {
        // The sequence header's matrix coefficients decide the
        // frame's colour space; the identity matrix and BT.2020
        // have no mapping and are rejected, not mislabelled.
        let mut seq = fixture_sequence_header();
        assert_eq!(seq.color.matrix_coefficients, 2);
        assert_eq!(colour_space_for(&seq).unwrap(), ColourSpace::Bt601);
        seq.color.matrix_coefficients = 1;
        assert_eq!(colour_space_for(&seq).unwrap(), ColourSpace::Bt709);
        seq.color.matrix_coefficients = 6;
        assert_eq!(colour_space_for(&seq).unwrap(), ColourSpace::Bt601);
        seq.color.matrix_coefficients = 0;
        assert!(matches!(colour_space_for(&seq), Err(Error::Unsupported(_))));
        seq.color.matrix_coefficients = 9;
        assert!(matches!(colour_space_for(&seq), Err(Error::Unsupported(_))));
    }

    #[test]
    fn colour_range_follows_the_sequence_header_flag() {
        let mut seq = fixture_sequence_header();
        assert!(!seq.color.color_range);
        assert_eq!(colour_range_for(&seq), ColourRange::Limited);
        seq.color.color_range = true;
        assert_eq!(colour_range_for(&seq), ColourRange::Full);
    }

    #[test]
    fn allocate_frame_sizes_planes_and_strides_for_odd_dimensions() {
        // Odd dimensions round the chroma planes up: a 5x3 frame
        // has a 15-byte luma plane and two 6-byte chroma planes.
        let seq = fixture_sequence_header();
        let header = Av1FrameHeader {
            frame_width: 5,
            frame_height: 3,
            ..Default::default()
        };
        let frame = allocate_frame(&seq, &header, 42).unwrap();
        assert_eq!(frame.width, 5);
        assert_eq!(frame.height, 3);
        assert_eq!(frame.format, PixelFormat::Yuv420);
        assert_eq!(frame.timestamp, 42);
        assert_eq!(
            frame.planes.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![15, 6, 6]
        );
        assert_eq!(frame.strides, vec![5, 3, 3]);
    }

    #[test]
    fn allocate_frame_rejects_a_zero_side() {
        let seq = fixture_sequence_header();
        let header = Av1FrameHeader {
            frame_width: 0,
            frame_height: 64,
            ..Default::default()
        };
        assert!(matches!(
            allocate_frame(&seq, &header, 0),
            Err(Error::InvalidArgument(_))
        ));
    }
}
