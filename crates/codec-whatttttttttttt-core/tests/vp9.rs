//! Integration tests for VP9 bitstream parsing (Phase 8, VP9
//! Bitstream & Decoding Process Specification v0.6, sections
//! 6.1-6.4, 9.2, and Appendix B).
//!
//! The reference vectors are ffmpeg 8.1.2 (`libvpx-vp9`) encodes
//! of local CC0 sources (see `tests/data/README.md` for the exact
//! commands). The expected uncompressed-header values below are
//! the values ffmpeg's `trace_headers` bitstream filter reports
//! for the same files, so the parser is checked against an
//! independent decoder, not against itself. The trace does not
//! print compressed-header fields, so the compressed header is
//! verified by the bool decoder itself: a wrong bit count fails
//! the marker bit, runs out of bits, or fails the zero-padding
//! check in `exit_bool`. Tile sizes are verified against the raw
//! frame bytes: the non-last tile size field is big-endian
//! (`00 00 07 6c` = 1900 for the tiles vector frame 0,
//! `00 00 01 14` = 276 for frame 1), and the superframe index
//! sizes are little-endian.

use codec_whatttttttttttt_core::{
    parse_superframe, parse_vp9_frame, Codec, Decoder, Demuxer, Error, Packet,
    Vp9InterpolationFilter, Vp9ReferenceMode, Vp9ReferenceState,
};

/// Reads the IVF file's frames into (timestamp, payload) pairs.
/// IVF frame header is 12 bytes: size (u32 LE) + timestamp (u64
/// LE), after the 32-byte file header.
fn ivf_frames(name: &str) -> Vec<(u64, Vec<u8>)> {
    let data = std::fs::read(format!("tests/data/{name}")).unwrap();
    assert_eq!(&data[0..4], b"DKIF");
    assert_eq!(&data[8..12], b"VP90");
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

/// Video packets of a `WebM` file, in demux order.
fn webm_video_packets(name: &str) -> Vec<Packet> {
    let bytes = std::fs::read(format!("tests/data/{name}")).unwrap();
    let mut demuxer = Demuxer::from_bytes(&bytes).unwrap();
    let mut packets = Vec::new();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        if packet.codec == Codec::Vp9 {
            packets.push(packet);
        }
    }
    packets
}

/// Expected per-frame values for `vp9_64x64.ivf`, from ffmpeg
/// `trace_headers`: (size, `base_q_idx`, `loop_filter_level`,
/// `header_size_in_bytes`).
const FRAMES_64X64: [(usize, u32, u32, u32); 10] = [
    (1729, 33, 3, 63),
    (426, 74, 4, 14),
    (444, 86, 5, 5),
    (399, 78, 2, 3),
    (568, 69, 4, 3),
    (508, 60, 2, 3),
    (504, 55, 4, 3),
    (591, 50, 6, 10),
    (629, 46, 2, 5),
    (816, 44, 6, 11),
];

#[test]
fn key_frame_header_matches_ffmpeg_trace() {
    let frames = ivf_frames("vp9_64x64.ivf");
    let mut state = Vp9ReferenceState::default();
    let frame = parse_vp9_frame(&frames[0].1, &mut state).unwrap();
    let header = &frame.header;
    assert!(header.is_keyframe);
    assert!(header.frame_is_intra);
    assert!(header.show_frame);
    assert!(!header.error_resilient_mode);
    assert_eq!(header.profile, 0);
    assert_eq!(header.frame_width, 64);
    assert_eq!(header.frame_height, 64);
    assert_eq!(header.render_width, 64);
    assert_eq!(header.render_height, 64);
    assert_eq!(header.mi_cols, 8);
    assert_eq!(header.mi_rows, 8);
    assert_eq!(header.color.bit_depth, 8);
    // Spec section 7.2.2: color_space value 0 is UNKNOWN, not
    // BT_601 (1). The trace prints 0 for this frame; ffmpeg's
    // yuv420p(tv) display comes from elsewhere, not this field.
    assert_eq!(
        header.color.color_space,
        codec_whatttttttttttt_core::Vp9ColorSpace::Unknown
    );
    assert!(!header.color.color_range);
    assert!(header.color.subsampling_x);
    assert!(header.color.subsampling_y);
    assert_eq!(header.refresh_frame_flags, 0xFF);
    assert!(header.refresh_frame_context);
    assert!(header.frame_parallel_decoding_mode);
    assert_eq!(header.frame_context_idx, 0);
    assert_eq!(header.loop_filter_level, 3);
    assert_eq!(header.loop_filter_sharpness, 0);
    assert!(header.loop_filter_delta_enabled);
    assert_eq!(header.loop_filter_ref_deltas, [1, 0, -1, -1]);
    assert_eq!(header.loop_filter_mode_deltas, [0, 0]);
    assert_eq!(header.base_q_idx, 33);
    assert_eq!(header.delta_q_y_dc, 0);
    assert_eq!(header.delta_q_uv_dc, 0);
    assert_eq!(header.delta_q_uv_ac, 0);
    assert!(!header.lossless);
    assert!(!header.segmentation_enabled);
    assert_eq!(header.tile_cols_log2, 0);
    assert_eq!(header.tile_rows_log2, 0);
    assert_eq!(header.header_size_in_bytes, 63);
    // Uncompressed header is 144 bits = 18 bytes; compressed
    // header is 63 bytes, so the single tile starts at byte 81.
    assert_eq!(frame.header_bytes, 81);
    assert_eq!(frame.total_bytes, 1729);
    assert_eq!(frame.tiles.len(), 1);
    assert_eq!(frame.tiles[0].offset, 81);
    assert_eq!(frame.tiles[0].size, 1648);
    assert_eq!(frame.tiles[0].mi_col_start, 0);
    assert_eq!(frame.tiles[0].mi_col_end, 8);
    assert_eq!(frame.tiles[0].mi_row_start, 0);
    assert_eq!(frame.tiles[0].mi_row_end, 8);
    let compressed = header.compressed_header.expect("compressed header");
    assert_eq!(compressed.reference_mode, Vp9ReferenceMode::Single);
    assert!(compressed.prob_updates > 0);
    // The key frame refreshed all reference slots.
    assert_eq!(state.slots[0], Some((64, 64)));
    assert_eq!(state.slots[7], Some((64, 64)));
}

#[test]
fn all_frames_match_ffmpeg_trace() {
    let frames = ivf_frames("vp9_64x64.ivf");
    assert_eq!(frames.len(), 10);
    let mut state = Vp9ReferenceState::default();
    for (index, (size, base_q, loop_filter_level, header_size)) in FRAMES_64X64.iter().enumerate() {
        let payload = &frames[index].1;
        assert_eq!(payload.len(), *size, "frame {index} size");
        let frame = parse_vp9_frame(payload, &mut state).unwrap();
        let header = &frame.header;
        assert_eq!(header.is_keyframe, index == 0, "frame {index} key");
        assert!(header.show_frame, "frame {index} show");
        assert!(!header.error_resilient_mode, "frame {index} err_res");
        assert_eq!(header.frame_width, 64, "frame {index} width");
        assert_eq!(header.frame_height, 64, "frame {index} height");
        assert_eq!(header.base_q_idx, *base_q, "frame {index} base_q");
        assert_eq!(
            header.loop_filter_level, *loop_filter_level,
            "frame {index} loop filter"
        );
        assert_eq!(
            header.header_size_in_bytes, *header_size,
            "frame {index} header size"
        );
        assert_eq!(
            header.loop_filter_ref_deltas,
            [1, 0, -1, -1],
            "frame {index} loop filter deltas persist"
        );
        assert_eq!(header.loop_filter_mode_deltas, [0, 0]);
        assert_eq!(header.delta_q_y_dc, 0);
        assert_eq!(header.delta_q_uv_dc, 0);
        assert_eq!(header.delta_q_uv_ac, 0);
        assert!(!header.segmentation_enabled, "frame {index} segmentation");
        assert_eq!(header.tile_cols_log2, 0, "frame {index} tile cols");
        assert_eq!(header.tile_rows_log2, 0, "frame {index} tile rows");
        // One tile; its offset is the uncompressed header (18
        // bytes for the key frame, 10 for inter frames) plus the
        // compressed header, and it takes the rest of the frame.
        let uncompressed_bytes = if index == 0 { 18 } else { 10 };
        assert_eq!(
            frame.header_bytes,
            uncompressed_bytes + *header_size as usize,
            "frame {index} header bytes"
        );
        assert_eq!(frame.tiles.len(), 1, "frame {index} tile count");
        assert_eq!(frame.tiles[0].offset, frame.header_bytes);
        assert_eq!(frame.tiles[0].size, *size - frame.header_bytes);
        let compressed = header.compressed_header.expect("compressed header");
        assert_eq!(compressed.reference_mode, Vp9ReferenceMode::Single);
        assert!(compressed.prob_updates > 0, "frame {index} prob updates");
        if index > 0 {
            assert!(!header.frame_is_intra, "frame {index} intra");
            assert_eq!(header.refresh_frame_flags, 1, "frame {index} refresh");
            assert_eq!(header.ref_frame_idx, [0, 1, 2], "frame {index} refs");
            assert_eq!(
                header.ref_frame_sign_bias,
                [false, false, false],
                "frame {index} sign bias"
            );
            assert!(header.allow_high_precision_mv, "frame {index} hp mv");
            assert!(header.refresh_frame_context, "frame {index} refresh ctx");
            assert!(
                header.frame_parallel_decoding_mode,
                "frame {index} parallel"
            );
            assert_eq!(header.frame_context_idx, 0, "frame {index} ctx idx");
            assert_eq!(header.reset_frame_context, 0, "frame {index} reset ctx");
            // Frames 1-2 signal a raw interpolation filter of 1
            // (EIGHTTAP); frames 3-9 are switchable.
            if index <= 2 {
                assert_eq!(
                    header.interpolation_filter,
                    Vp9InterpolationFilter::EightTap,
                    "frame {index} interpolation"
                );
            } else {
                assert_eq!(
                    header.interpolation_filter,
                    Vp9InterpolationFilter::Switchable,
                    "frame {index} interpolation"
                );
            }
        }
    }
}

#[test]
fn multi_tile_frames_split_big_endian_sizes() {
    let packets = webm_video_packets("vp9_tiles.webm");
    assert_eq!(packets.len(), 10);
    // Frame 0: 512x64, 2 columns, 1 row. Tiles start at byte 155
    // (18-byte uncompressed header + 137-byte compressed header);
    // the size field is big-endian 1900, the second tile is the
    // remaining 1728 bytes.
    let mut state = Vp9ReferenceState::default();
    let frame0 = parse_vp9_frame(&packets[0].data, &mut state).unwrap();
    assert_eq!(frame0.header.frame_width, 512);
    assert_eq!(frame0.header.frame_height, 64);
    assert_eq!(frame0.header.tile_cols_log2, 1);
    assert_eq!(frame0.header.tile_rows_log2, 0);
    assert_eq!(frame0.header_bytes, 155);
    assert_eq!(frame0.tiles.len(), 2);
    assert_eq!(frame0.tiles[0].offset, 159);
    assert_eq!(frame0.tiles[0].size, 1900);
    assert_eq!(frame0.tiles[1].offset, 2059);
    assert_eq!(frame0.tiles[1].size, 1728);
    assert_eq!(frame0.tiles[0].mi_col_start, 0);
    assert_eq!(frame0.tiles[0].mi_col_end, 32);
    assert_eq!(frame0.tiles[1].mi_col_start, 32);
    assert_eq!(frame0.tiles[1].mi_col_end, 64);
    assert_eq!(frame0.tiles[0].mi_row_start, 0);
    assert_eq!(frame0.tiles[0].mi_row_end, 8);
    // Frame 1: tiles start at byte 30 (10-byte uncompressed
    // header + 20-byte compressed header); size field is
    // big-endian 276, second tile is 545 bytes.
    let frame1 = parse_vp9_frame(&packets[1].data, &mut state).unwrap();
    assert_eq!(frame1.header_bytes, 30);
    assert_eq!(frame1.tiles.len(), 2);
    assert_eq!(frame1.tiles[0].offset, 34);
    assert_eq!(frame1.tiles[0].size, 276);
    assert_eq!(frame1.tiles[1].offset, 310);
    assert_eq!(frame1.tiles[1].size, 545);
}

#[test]
fn superframe_splits_into_two_frames() {
    let frames = ivf_frames("vp9_superframe.ivf");
    assert_eq!(frames.len(), 1);
    let unit = &frames[0].1;
    let split = parse_superframe(unit).unwrap();
    assert_eq!(split, vec![(0, 1729), (1729, 426)]);
    // Both parts are the real vp9_64x64 frames 0 and 1.
    let reference = ivf_frames("vp9_64x64.ivf");
    assert_eq!(&unit[0..1729], &reference[0].1[..]);
    assert_eq!(&unit[1729..1729 + 426], &reference[1].1[..]);
    let mut state = Vp9ReferenceState::default();
    let first = parse_vp9_frame(&unit[split[0].0..split[0].0 + split[0].1], &mut state).unwrap();
    assert!(first.header.is_keyframe);
    let second = parse_vp9_frame(&unit[split[1].0..split[1].0 + split[1].1], &mut state).unwrap();
    assert!(!second.header.is_keyframe);
    assert_eq!(second.header.base_q_idx, 74);
}

#[test]
fn superframe_with_big_endian_sizes_is_invalid() {
    let frames = ivf_frames("vp9_superframe.ivf");
    let unit = &frames[0].1;
    // Same frames, but the index sizes written big-endian: the
    // little-endian reader sees 49414 and 43521, which overrun
    // the index.
    let mut big_endian = unit[..2155].to_vec();
    big_endian.extend_from_slice(&[0xC9, 0x06, 0xC1, 0x01, 0xAA, 0xC9]);
    let err = parse_superframe(&big_endian).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn truncated_tile_size_field_is_truncated() {
    let packets = webm_video_packets("vp9_tiles.webm");
    let mut state = Vp9ReferenceState::default();
    // Cut frame 0 two bytes into the first tile size field (tiles
    // start at byte 155).
    let err = parse_vp9_frame(&packets[0].data[..157], &mut state).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn truncated_tile_data_is_truncated() {
    let packets = webm_video_packets("vp9_tiles.webm");
    let mut state = Vp9ReferenceState::default();
    // Cut frame 0 inside the first tile (declared 1900 bytes,
    // only 100 present after the size field).
    let err = parse_vp9_frame(&packets[0].data[..159 + 100], &mut state).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn truncated_compressed_header_is_truncated() {
    let frames = ivf_frames("vp9_64x64.ivf");
    let mut state = Vp9ReferenceState::default();
    // Cut frame 0 inside the compressed header (18-byte
    // uncompressed header, 63-byte compressed header).
    let err = parse_vp9_frame(&frames[0].1[..40], &mut state).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn corrupt_frame_marker_is_invalid_data() {
    let frames = ivf_frames("vp9_64x64.ivf");
    let mut state = Vp9ReferenceState::default();
    let mut corrupt = frames[0].1.clone();
    corrupt[0] = 0x00;
    let err = parse_vp9_frame(&corrupt, &mut state).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn corrupt_sync_code_is_invalid_data() {
    let frames = ivf_frames("vp9_64x64.ivf");
    let mut state = Vp9ReferenceState::default();
    let mut corrupt = frames[0].1.clone();
    corrupt[1] ^= 0xFF;
    let err = parse_vp9_frame(&corrupt, &mut state).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn inter_frame_without_reference_state_is_invalid_data() {
    let frames = ivf_frames("vp9_64x64.ivf");
    // Parse frame 1 with a fresh state: its frame_size_with_refs
    // references slot 0, which is empty.
    let mut state = Vp9ReferenceState::default();
    let err = parse_vp9_frame(&frames[1].1, &mut state).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn corrupt_vp9_input_does_not_panic() {
    let frames = ivf_frames("vp9_64x64.ivf");
    let garbage = [0xDE, 0xAD, 0xBE, 0xEF, 0x12, 0x34, 0x56, 0x78];
    let mut state = Vp9ReferenceState::default();
    let _ = parse_vp9_frame(&garbage, &mut state);
    // All single-bit flips of the first 20 bytes of frame 0.
    for index in 0..20 {
        for bit in 0..8 {
            let mut candidate = frames[0].1.clone();
            candidate[index] ^= 1 << bit;
            let mut state = Vp9ReferenceState::default();
            let _ = parse_vp9_frame(&candidate, &mut state);
        }
    }
}

#[test]
fn decoder_probes_ivf_and_reports_vp9_unsupported_for_decode() {
    let frames = ivf_frames("vp9_64x64.ivf");
    let bytes = std::fs::read("tests/data/vp9_64x64.ivf").unwrap();
    assert_eq!(Decoder::probe(&bytes).unwrap(), Codec::Vp9);
    let mut state = Vp9ReferenceState::default();
    let frame = parse_vp9_frame(&frames[0].1, &mut state).unwrap();
    let mut decoder = Decoder::from_vp9_frame_header(&frame.header).expect("vp9 decoder");
    let packet = Packet {
        codec: Codec::Vp9,
        data: frames[0].1.clone(),
        timestamp: 0,
        is_keyframe: true,
    };
    let err = decoder.decode(&packet).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
}
