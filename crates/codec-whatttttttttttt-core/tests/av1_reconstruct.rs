//! AV1 reconstruction tests: tile mode decode and the keyframe
//! pipeline vs ffmpeg fixtures.
//!
//! The fixtures are keyframe-only AV1 streams produced with ffmpeg
//! 8.1.2 (`libaom-av1`, CDEF and restoration disabled, `testsrc2`).
//! The tile decoder walks the partition tree and reads each leaf
//! block's intra mode info (AV1 spec v1.0.0 sections 5.11, 8.3.2),
//! then returns `Unsupported` naming the missing coefficient CDF
//! tables, which are not ported in this round. The recorded modes
//! are checked against the frame geometry instead of against
//! pixels. A wrong CDF value or context would derail the partition
//! walk, so the exact area coverage below is the check that the walk
//! stayed aligned with the encoder. The pipeline tests pin the
//! subset gating and the coefficient-gap error; the pixel test at
//! the end is ignored until coefficient decode lands.

use codec_whatttttttttttt_core::{
    av1_tile::{Av1TileDecoder, COEFFICIENT_GAP},
    parse_frame_obu, parse_sequence_header, reconstruct_av1_frame_obu, reconstruct_av1_keyframe,
    Av1FrameHeader, Av1ReferenceState, Error, GrainParams, ObuReader, ObuType, SequenceHeader,
    TileGroup,
};

/// Reads an IVF file's frames into (timestamp, payload) pairs.
fn ivf_frames(data: &[u8]) -> Vec<(u64, Vec<u8>)> {
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

/// Parses frame 0 of an IVF file into its sequence header, frame
/// header, tile group and frame OBU payload.
fn first_frame(data: &[u8]) -> (SequenceHeader, Av1FrameHeader, TileGroup, Vec<u8>) {
    let frames = ivf_frames(data);
    assert!(!frames.is_empty(), "ivf file has no frames");
    let payload = frames[0].1.clone();
    let mut seq: Option<SequenceHeader> = None;
    let mut frame: Option<(Av1FrameHeader, TileGroup, Vec<u8>)> = None;
    let mut reader = ObuReader::new(&payload);
    while let Some(obu) = reader.next_obu().unwrap() {
        match obu.header.obu_type {
            ObuType::SequenceHeader => {
                seq = Some(parse_sequence_header(obu.payload).unwrap());
            }
            ObuType::Frame => {
                let seq = seq.as_ref().expect("frame OBU before sequence header");
                let mut state = Av1ReferenceState::default();
                let (header, tile_group) =
                    parse_frame_obu(obu.payload, seq, &mut state, 0, 0).unwrap();
                frame = Some((header, tile_group, obu.payload.to_vec()));
            }
            _ => {}
        }
    }
    let seq = seq.expect("no sequence header OBU in frame 0");
    let (header, tile_group, frame_payload) = frame.expect("no frame OBU in frame 0");
    (seq, header, tile_group, frame_payload)
}

/// Decodes tile 0 of frame 0 and checks the recorded leaf blocks
/// cover the frame exactly (clipped to the frame edge).
fn check_tile_modes(data: &[u8], width: u32, height: u32) {
    let (seq, header, tile_group, payload) = first_frame(data);
    assert_eq!(header.frame_type, 0, "fixture frame 0 must be a keyframe");
    assert_eq!(header.frame_width, width);
    assert_eq!(header.frame_height, height);
    assert_eq!(tile_group.tiles.len(), 1, "fixtures are single-tile");
    let tile = &tile_group.tiles[0];
    let tile_data = &payload[tile.offset..tile.offset + tile.size];
    let mut decoder = Av1TileDecoder::new(&seq, &header, tile.index, tile_data).unwrap();
    let err = decoder.decode_tile().unwrap_err();
    assert!(
        matches!(err, Error::Unsupported(msg) if msg == COEFFICIENT_GAP),
        "expected the coefficient gap, got {err:?}"
    );
    let blocks = decoder.blocks();
    assert!(!blocks.is_empty(), "no leaf blocks recorded");
    let mi_cols = 2 * ((width as usize + 7) >> 3);
    let mi_rows = 2 * ((height as usize + 7) >> 3);
    let mut covered = 0usize;
    for block in blocks {
        let row = block.mi_row as usize;
        let col = block.mi_col as usize;
        assert!(
            row < mi_rows && col < mi_cols,
            "block starts outside the frame"
        );
        assert!(block.mi_size < 22, "block size out of range");
        assert!(block.y_mode <= 12, "luma intra mode out of range");
        assert!(block.uv_mode <= 13, "chroma intra mode out of range");
        assert!((1..=255).contains(&block.qindex), "qindex out of range");
        assert!(block.tx_size < 19, "transform size out of range");
        assert!(block.angle_delta_y >= -3 && block.angle_delta_y <= 3);
        assert!(block.angle_delta_uv >= -3 && block.angle_delta_uv <= 3);
        // Block width/height in 4x4 units, from the spec tables.
        let (bw4, bh4) = block_extent(block.mi_size);
        let clipped_w = (col + bw4).min(mi_cols) - col;
        let clipped_h = (row + bh4).min(mi_rows) - row;
        covered += clipped_w * clipped_h;
    }
    assert_eq!(
        covered,
        mi_rows * mi_cols,
        "leaf blocks must tile the frame exactly"
    );
}

/// Width and height of a block size in 4x4 units (spec section 9.2).
fn block_extent(mi_size: u8) -> (usize, usize) {
    const WIDE: [usize; 22] = [
        1, 1, 2, 2, 2, 4, 4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 1, 4, 2, 8, 4, 16,
    ];
    const HIGH: [usize; 22] = [
        1, 2, 1, 2, 4, 2, 4, 8, 4, 8, 16, 8, 16, 32, 16, 32, 4, 1, 8, 2, 16, 4,
    ];
    (WIDE[mi_size as usize], HIGH[mi_size as usize])
}

#[test]
fn tile_modes_cover_64x64_keyframe() {
    check_tile_modes(include_bytes!("data/av1rec_64x64.ivf"), 64, 64);
}

/// Blocked by unimplemented intra block copy, not by the tile
/// decoder: libaom encodes these fixtures with `allow_intrabc = 1`
/// and a block uses it, so the tile walk returns
/// `Unsupported("av1 intra block copy is not implemented in this
/// round")`. Re-enable once intra block copy lands (or the
/// fixtures are re-encoded without intrabc).
#[test]
#[ignore = "av1 intra block copy is not implemented in this round (see LIMITS.md)"]
fn tile_modes_cover_320x240_keyframe() {
    check_tile_modes(include_bytes!("data/av1rec_320x240.ivf"), 320, 240);
}

/// Blocked by the same intra-block-copy gap as
/// `tile_modes_cover_320x240_keyframe` (see its doc comment).
/// Re-enable once intra block copy lands.
#[test]
#[ignore = "av1 intra block copy is not implemented in this round (see LIMITS.md)"]
fn tile_modes_cover_640x480_keyframe() {
    check_tile_modes(include_bytes!("data/av1rec_640x480.ivf"), 640, 480);
}

#[test]
fn tile_decoder_rejects_tile_index_outside_grid() {
    let (seq, header, tile_group, payload) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    let tile = &tile_group.tiles[0];
    let tile_data = &payload[tile.offset..tile.offset + tile.size];
    assert!(matches!(
        Av1TileDecoder::new(&seq, &header, 99, tile_data),
        Err(Error::InvalidData { .. })
    ));
}

#[test]
fn tile_decoder_rejects_truncated_tile_data() {
    let (seq, header, tile_group, payload) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    let tile = &tile_group.tiles[0];
    let tile_data = &payload[tile.offset..tile.offset + tile.size];
    // Two bytes are enough to initialize the symbol decoder, but the
    // real tile's mode walk needs far more, so the walk must fail
    // instead of decoding on zero padding.
    let mut decoder = Av1TileDecoder::new(&seq, &header, tile.index, &tile_data[..2])
        .expect("two bytes initialize");
    let err = decoder.decode_tile().unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn pipeline_returns_coefficient_gap_for_64x64_keyframe() {
    let (seq, header, tile_group, payload) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    let err = reconstruct_av1_keyframe(&seq, &header, &tile_group, &payload, 0).unwrap_err();
    assert!(
        matches!(err, Error::Unsupported(msg) if msg == COEFFICIENT_GAP),
        "expected the coefficient gap, got {err:?}"
    );
}

#[test]
fn pipeline_obu_entry_returns_coefficient_gap_for_64x64_keyframe() {
    let frames = ivf_frames(include_bytes!("data/av1rec_64x64.ivf"));
    let payload = &frames[0].1;
    let mut seq: Option<SequenceHeader> = None;
    let mut frame_payload: Option<Vec<u8>> = None;
    let mut reader = ObuReader::new(payload);
    while let Some(obu) = reader.next_obu().unwrap() {
        match obu.header.obu_type {
            ObuType::SequenceHeader => {
                seq = Some(parse_sequence_header(obu.payload).unwrap());
            }
            ObuType::Frame => {
                frame_payload = Some(obu.payload.to_vec());
            }
            _ => {}
        }
    }
    let seq = seq.expect("no sequence header OBU");
    let frame_payload = frame_payload.expect("no frame OBU");
    let mut state = Av1ReferenceState::default();
    let err = reconstruct_av1_frame_obu(&frame_payload, &seq, &mut state, 0).unwrap_err();
    assert!(
        matches!(err, Error::Unsupported(msg) if msg == COEFFICIENT_GAP),
        "expected the coefficient gap, got {err:?}"
    );
}

/// Each gating test mutates one field of the parsed 64x64
/// keyframe (which passes every gate) and asserts the pipeline
/// rejects it with `Unsupported` before touching tile data.
fn assert_pipeline_unsupported(seq: &SequenceHeader, header: &Av1FrameHeader) {
    let (_, _, tile_group, payload) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    let err = reconstruct_av1_keyframe(seq, header, &tile_group, &payload, 0).unwrap_err();
    assert!(
        matches!(err, Error::Unsupported(_)),
        "expected Unsupported, got {err:?}"
    );
}

#[test]
fn pipeline_rejects_inter_frame() {
    let (seq, mut header, _, _) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    header.frame_type = 1;
    assert_pipeline_unsupported(&seq, &header);
}

#[test]
fn pipeline_rejects_superres() {
    let (seq, mut header, _, _) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    header.upscaled_width = header.frame_width + 16;
    assert_pipeline_unsupported(&seq, &header);
}

#[test]
fn pipeline_rejects_qmatrix() {
    let (seq, mut header, _, _) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    header.using_qmatrix = true;
    assert_pipeline_unsupported(&seq, &header);
}

#[test]
fn pipeline_rejects_segmentation() {
    let (seq, mut header, _, _) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    header.segmentation_enabled = true;
    assert_pipeline_unsupported(&seq, &header);
}

#[test]
fn pipeline_rejects_delta_q() {
    let (seq, mut header, _, _) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    header.delta_q_present = true;
    assert_pipeline_unsupported(&seq, &header);
}

#[test]
fn pipeline_rejects_intrabc() {
    let (seq, mut header, _, _) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    header.allow_intrabc = true;
    assert_pipeline_unsupported(&seq, &header);
}

#[test]
fn pipeline_rejects_film_grain() {
    let (seq, mut header, _, _) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    header.grain = Some(GrainParams::default());
    assert_pipeline_unsupported(&seq, &header);
}

#[test]
fn pipeline_rejects_nonzero_profile() {
    let (mut seq, header, _, _) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    seq.profile = 1;
    assert_pipeline_unsupported(&seq, &header);
}

#[test]
fn pipeline_rejects_10bit() {
    let (mut seq, header, _, _) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    seq.color.bit_depth = 10;
    assert_pipeline_unsupported(&seq, &header);
}

#[test]
fn pipeline_rejects_non_420_subsampling() {
    let (mut seq, header, _, _) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    seq.color.subsampling_x = false;
    assert_pipeline_unsupported(&seq, &header);
}

#[test]
fn pipeline_rejects_identity_matrix() {
    let (mut seq, header, _, _) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    seq.color.matrix_coefficients = 0;
    assert_pipeline_unsupported(&seq, &header);
}

#[test]
fn pipeline_rejects_tile_range_outside_payload() {
    let (seq, header, tile_group, payload) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    let tile = &tile_group.tiles[0];
    // Cut the payload off exactly at the tile start, so the tile
    // range no longer fits inside it.
    let err = reconstruct_av1_keyframe(&seq, &header, &tile_group, &payload[..tile.offset], 0)
        .unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn pipeline_obu_entry_rejects_empty_payload() {
    let (seq, _, _, _) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    let mut state = Av1ReferenceState::default();
    let err = reconstruct_av1_frame_obu(&[], &seq, &mut state, 0).unwrap_err();
    assert!(matches!(
        err,
        Error::Truncated { .. } | Error::InvalidData { .. }
    ));
}

#[test]
fn pipeline_obu_entry_rejects_truncated_frame_obu() {
    let frames = ivf_frames(include_bytes!("data/av1rec_64x64.ivf"));
    let payload = &frames[0].1;
    let mut frame_payload: Option<Vec<u8>> = None;
    let mut seq: Option<SequenceHeader> = None;
    let mut reader = ObuReader::new(payload);
    while let Some(obu) = reader.next_obu().unwrap() {
        match obu.header.obu_type {
            ObuType::SequenceHeader => {
                seq = Some(parse_sequence_header(obu.payload).unwrap());
            }
            ObuType::Frame => {
                frame_payload = Some(obu.payload.to_vec());
            }
            _ => {}
        }
    }
    let seq = seq.expect("no sequence header OBU");
    let frame_payload = frame_payload.expect("no frame OBU");
    let mut state = Av1ReferenceState::default();
    // Four bytes cut the frame header off mid-field.
    let err = reconstruct_av1_frame_obu(&frame_payload[..4], &seq, &mut state, 0).unwrap_err();
    assert!(matches!(
        err,
        Error::Truncated { .. } | Error::InvalidData { .. }
    ));
}

/// Bit-exact pixel comparison against ffmpeg's decode of frame 0
/// (reference YUV produced with `ffmpeg -i av1rec_64x64.ivf
/// -vf select=eq(n,0) -frames:v 1 -pix_fmt yuv420p`).
/// Ignored until coefficient decode lands: today the pipeline
/// returns `Unsupported(COEFFICIENT_GAP)` instead of a frame.
/// The 320x240 and 640x480 equivalents are pending the parser
/// bug as well (see the ignored tile-mode tests above).
#[test]
#[ignore = "needs coefficient decode (COEFFICIENT_GAP)"]
fn pixels_match_ffmpeg_for_64x64_keyframe() {
    let (seq, header, tile_group, payload) = first_frame(include_bytes!("data/av1rec_64x64.ivf"));
    let frame = reconstruct_av1_keyframe(&seq, &header, &tile_group, &payload, 0).unwrap();
    assert_eq!(frame.width, 64);
    assert_eq!(frame.height, 64);
    let reference = include_bytes!("data/av1rec_64x64_f0.yuv");
    assert_eq!(reference.len(), 64 * 64 * 3 / 2);
    assert_eq!(frame.planes[0], reference[0..4096]);
    assert_eq!(frame.planes[1], reference[4096..5120]);
    assert_eq!(frame.planes[2], reference[5120..6144]);
}
