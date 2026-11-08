//! Integration tests for AV1 OBU, sequence header, and frame
//! header parsing (Phase 7, AV1 spec v1.0.0 sections 5.3, 5.5, 5.9).
//!
//! The reference vector is `av1_64x64.ivf`, produced with ffmpeg
//! 8.1.2 (`libaom-av1`, 64x64, 10 fps, 1 s, `testsrc2`). The
//! expected header values below are the values ffmpeg's
//! `trace_headers` bitstream filter reports for the same file, so
//! the parser is checked against an independent decoder, not
//! against itself. Where the trace prints a raw bit and the spec
//! infers a different final value, the test asserts the spec value
//! and says so in a comment.

use codec_whatttttttttttt_core::{
    check_trailing_bits, parse_av1_frame_header, parse_frame_obu, parse_obu, parse_obu_header,
    parse_sequence_header, Av1ReferenceState, Codec, Decoder, Error, ObuReader, ObuType, Packet,
};

/// Sequence header payload of `av1_64x64.ivf` frame 0 (the 10-byte
/// payload of the sequence header OBU, without its OBU header).
const SEQ_64X64: [u8; 10] = [0x00, 0x00, 0x00, 0x02, 0xAF, 0xFF, 0x9B, 0x5F, 0x20, 0x08];

/// The same sequence header as a complete OBU (header 0x0A, LEB128
/// size 0x0A, then the payload above).
const SEQ_64X64_OBU: [u8; 12] = [
    0x0A, 0x0A, 0x00, 0x00, 0x00, 0x02, 0xAF, 0xFF, 0x9B, 0x5F, 0x20, 0x08,
];

/// Reads the IVF file's frames into (timestamp, payload) pairs.
fn ivf_frames() -> Vec<(u64, Vec<u8>)> {
    let data = include_bytes!("data/av1_64x64.ivf");
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

#[test]
fn obu_header_parses() {
    // Sequence header OBU: 0x0A header, 0x0A LEB128 size (10).
    let obu = parse_obu(&SEQ_64X64_OBU, 0).unwrap().0;
    assert_eq!(obu.header.obu_type, ObuType::SequenceHeader);
    assert!(obu.header.has_size_field);
    assert_eq!(obu.payload, SEQ_64X64);
    // Temporal delimiter OBU: 0x12 header, 0x00 size (0).
    let td = parse_obu(&[0x12, 0x00], 0).unwrap().0;
    assert_eq!(td.header.obu_type, ObuType::TemporalDelimiter);
    assert_eq!(td.payload.len(), 0);
}

#[test]
fn obu_header_forbidden_bit_is_invalid_data() {
    let err = parse_obu_header(&[0x8A, 0x0A], 0).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn obu_header_reserved_type_is_unsupported() {
    // Type 9 is reserved.
    let err = parse_obu(&[0x4A, 0x00], 0).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
}

#[test]
fn obu_truncated_is_truncated() {
    // Header says 10 payload bytes, only 2 present.
    let err = parse_obu(&[0x0A, 0x0A, 0x00, 0x00], 0).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
    // Size field itself cut short.
    let err = parse_obu(&[0x0A], 0).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn obu_reader_walks_a_temporal_unit() {
    let frames = ivf_frames();
    // Frame 0: temporal delimiter, sequence header, frame.
    let mut reader = ObuReader::new(&frames[0].1);
    let first = reader.next_obu().unwrap().unwrap();
    assert_eq!(first.header.obu_type, ObuType::TemporalDelimiter);
    let second = reader.next_obu().unwrap().unwrap();
    assert_eq!(second.header.obu_type, ObuType::SequenceHeader);
    let third = reader.next_obu().unwrap().unwrap();
    assert_eq!(third.header.obu_type, ObuType::Frame);
    assert_eq!(third.payload.len(), 762);
    assert!(reader.next_obu().unwrap().is_none());
    // Frame 1 holds four frame OBUs (a temporal unit can contain
    // several frames; only the last is shown).
    let mut reader = ObuReader::new(&frames[1].1);
    let mut frame_obus = 0;
    while let Some(obu) = reader.next_obu().unwrap() {
        if obu.header.obu_type == ObuType::Frame {
            frame_obus += 1;
        }
    }
    assert_eq!(frame_obus, 4);
}

#[test]
fn sequence_header_matches_ffmpeg_trace() {
    let seq = parse_sequence_header(&SEQ_64X64).unwrap();
    assert_eq!(seq.profile, 0);
    assert!(!seq.still_picture);
    assert!(!seq.reduced_still_picture_header);
    assert_eq!(seq.operating_points.len(), 1);
    assert_eq!(seq.operating_points[0].idc, 0);
    assert_eq!(seq.operating_points[0].level, 0);
    assert_eq!(seq.operating_points[0].tier, 0);
    assert_eq!(seq.frame_width_bits_minus_1, 5);
    assert_eq!(seq.frame_height_bits_minus_1, 5);
    assert_eq!(seq.max_frame_width_minus_1 + 1, 64);
    assert_eq!(seq.max_frame_height_minus_1 + 1, 64);
    assert!(!seq.frame_id_numbers_present);
    assert!(!seq.use_128x128_superblock);
    assert!(seq.enable_filter_intra);
    assert!(seq.enable_intra_edge_filter);
    assert!(!seq.enable_interintra_compound);
    assert!(seq.enable_masked_compound);
    assert!(seq.enable_warped_motion);
    assert!(!seq.enable_dual_filter);
    assert!(seq.enable_order_hint);
    assert!(!seq.enable_jnt_comp);
    assert!(seq.enable_ref_frame_mvs);
    // SELECT (2): the choice is made per frame.
    assert_eq!(seq.seq_force_screen_content_tools, 2);
    assert_eq!(seq.seq_force_integer_mv, 2);
    assert_eq!(seq.order_hint_bits, 7);
    assert!(!seq.enable_superres);
    assert!(seq.enable_cdef);
    assert!(!seq.enable_restoration);
    assert_eq!(seq.color.bit_depth, 8);
    assert!(!seq.color.mono_chrome);
    assert_eq!(seq.color.color_primaries, 2);
    assert_eq!(seq.color.transfer_characteristics, 2);
    assert_eq!(seq.color.matrix_coefficients, 2);
    assert!(!seq.color.color_range);
    assert!(seq.color.subsampling_x);
    assert!(seq.color.subsampling_y);
    assert_eq!(seq.color.chroma_sample_position, 0);
    assert!(!seq.color.separate_uv_delta_q);
    assert!(!seq.film_grain_params_present);
    assert!(seq.timing_info.is_none());
}

#[test]
fn sequence_header_truncated_is_truncated() {
    let err = parse_sequence_header(&SEQ_64X64[..5]).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn sequence_header_bad_trailing_bit_is_invalid_data() {
    let mut bad = SEQ_64X64;
    // The header fields end at bit 76 of 80; bit 76 is the
    // trailing one bit (0x08 in the last byte), followed by three
    // zero bits. Clear it.
    bad[9] &= 0xF7;
    let err = parse_sequence_header(&bad).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn key_frame_header_matches_ffmpeg_trace() {
    let seq = parse_sequence_header(&SEQ_64X64).unwrap();
    let frames = ivf_frames();
    let mut state = Av1ReferenceState::default();
    let mut reader = ObuReader::new(&frames[0].1);
    let _ = reader.next_obu().unwrap().unwrap();
    let _ = reader.next_obu().unwrap().unwrap();
    let frame_obu = reader.next_obu().unwrap().unwrap();
    let (header, tile_group) = parse_frame_obu(frame_obu.payload, &seq, &mut state, 0, 0).unwrap();
    assert_eq!(header.frame_type, 0);
    assert!(header.show_frame);
    // Spec section 5.9.2 forces error_resilient_mode to 1 for a
    // shown key frame without reading a bit.
    assert!(header.error_resilient_mode);
    assert!(!header.disable_cdf_update);
    assert!(header.allow_screen_content_tools);
    // Spec section 5.9.2 forces force_integer_mv to 1 for intra
    // frames; the trace prints the raw bit (0) read before that.
    assert!(header.force_integer_mv);
    assert!(!header.frame_size_override_flag);
    assert_eq!(header.order_hint, 0);
    assert_eq!(header.primary_ref_frame, 7);
    assert_eq!(header.refresh_frame_flags, 0xFF);
    assert_eq!(header.frame_width, 64);
    assert_eq!(header.frame_height, 64);
    // Spec section 5.9.2 forces allow_warped_motion to 0 for
    // intra frames.
    assert!(!header.allow_warped_motion);
    assert_eq!(header.base_q_idx, 86);
    assert_eq!(header.loop_filter_level, [4, 4, 6, 3]);
    assert_eq!(header.cdef_damping, 4);
    assert_eq!(header.tx_mode, 2);
    assert!(!header.reduced_tx_set);
    // The header is 80 bits (10 bytes); the single tile takes the
    // rest of the 762-byte frame OBU payload.
    assert_eq!(tile_group.tiles.len(), 1);
    assert_eq!(tile_group.tiles[0].offset, 10);
    assert_eq!(tile_group.tiles[0].size, 752);
}

#[test]
fn first_inter_frame_header_matches_ffmpeg_trace() {
    let seq = parse_sequence_header(&SEQ_64X64).unwrap();
    let frames = ivf_frames();
    let mut state = Av1ReferenceState::default();
    let mut reader = ObuReader::new(&frames[0].1);
    let _ = reader.next_obu().unwrap().unwrap();
    let _ = reader.next_obu().unwrap().unwrap();
    let frame_obu = reader.next_obu().unwrap().unwrap();
    let _ = parse_frame_obu(frame_obu.payload, &seq, &mut state, 0, 0).unwrap();
    let mut reader = ObuReader::new(&frames[1].1);
    let _ = reader.next_obu().unwrap().unwrap();
    let frame_obu = reader.next_obu().unwrap().unwrap();
    let (header, tile_group) = parse_frame_obu(frame_obu.payload, &seq, &mut state, 0, 0).unwrap();
    assert_eq!(header.frame_type, 1);
    assert!(!header.show_frame);
    assert!(header.showable_frame);
    assert!(!header.error_resilient_mode);
    assert_eq!(header.primary_ref_frame, 7);
    assert_eq!(header.refresh_frame_flags, 2);
    assert_eq!(header.ref_frame_idx, [0, 0, 0, 0, 0, 0, 0]);
    assert!(header.allow_high_precision_mv);
    assert!(!header.is_filter_switchable);
    assert_eq!(header.interpolation_filter, 0);
    assert!(header.is_motion_mode_switchable);
    assert!(header.use_ref_frame_mvs);
    assert_eq!(header.base_q_idx, 98);
    assert_eq!(header.loop_filter_level, [4, 4, 10, 7]);
    assert_eq!(header.tx_mode, 2);
    assert!(!header.reference_select);
    assert!(header.allow_warped_motion);
    assert!(!header.reduced_tx_set);
    assert_eq!(header.gm_types, [0; 7]);
    // The inter frame header ends at bit 129; byte alignment pads
    // 7 zero bits to byte 17.
    assert_eq!(tile_group.tiles.len(), 1);
    assert_eq!(tile_group.tiles[0].offset, 17);
    assert_eq!(tile_group.tiles[0].size, 608 - 17);
}

#[test]
fn show_existing_frame_is_parsed() {
    let seq = parse_sequence_header(&SEQ_64X64).unwrap();
    let frames = ivf_frames();
    let mut state = Av1ReferenceState::default();
    // Parse frames 0 and 1 so the reference slots are populated.
    for frame in frames.iter().take(2) {
        let mut reader = ObuReader::new(&frame.1);
        while let Some(obu) = reader.next_obu().unwrap() {
            if obu.header.obu_type == ObuType::Frame {
                let _ = parse_frame_obu(obu.payload, &seq, &mut state, 0, 0).unwrap();
            }
        }
    }
    // Frame 2 is a temporal delimiter plus a 3-byte frame header
    // OBU holding a show_existing_frame header (payload 0xb8:
    // show_existing = 1, map index = 3, then trailing bits).
    let mut reader = ObuReader::new(&frames[2].1);
    let _ = reader.next_obu().unwrap().unwrap();
    let frame_header_obu = reader.next_obu().unwrap().unwrap();
    assert_eq!(frame_header_obu.header.obu_type, ObuType::FrameHeader);
    assert_eq!(frame_header_obu.payload, &[0xB8]);
    let (header, bits) =
        parse_av1_frame_header(frame_header_obu.payload, &seq, &mut state, 0, 0).unwrap();
    assert!(header.show_existing_frame);
    assert_eq!(header.frame_to_show_map_idx, 3);
    assert_eq!(header.frame_type, state.ref_frames[3].frame_type);
    assert_eq!(bits, 4);
    check_trailing_bits(frame_header_obu.payload, bits).unwrap();
}

#[test]
fn frame_header_truncated_payload_is_truncated() {
    let seq = parse_sequence_header(&SEQ_64X64).unwrap();
    let frames = ivf_frames();
    let mut reader = ObuReader::new(&frames[0].1);
    let _ = reader.next_obu().unwrap().unwrap();
    let _ = reader.next_obu().unwrap().unwrap();
    let frame_obu = reader.next_obu().unwrap().unwrap();
    let mut state = Av1ReferenceState::default();
    // Truncate inside the header (5 bytes; the key frame header
    // needs 10). Truncating inside the tile data would not fail:
    // the single tile takes the rest of the OBU.
    let err = parse_frame_obu(&frame_obu.payload[..5], &seq, &mut state, 0, 0).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn corrupt_av1_input_does_not_panic() {
    // Random bytes through the OBU reader and the sequence header
    // parser must return an error, never panic.
    let garbage = [0xDE, 0xAD, 0xBE, 0xEF, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC];
    let mut reader = ObuReader::new(&garbage);
    let _ = reader.next_obu();
    let _ = parse_sequence_header(&garbage);
    let mut flipped = SEQ_64X64;
    for byte in &mut flipped {
        *byte ^= 0xFF;
    }
    let _ = parse_sequence_header(&flipped);
    // All single-bit flips of the sequence header payload.
    for index in 0..SEQ_64X64.len() {
        for bit in 0..8 {
            let mut candidate = SEQ_64X64;
            candidate[index] ^= 1 << bit;
            let _ = parse_sequence_header(&candidate);
        }
    }
}

#[test]
fn decoder_probes_ivf_and_reports_av1_unsupported_for_decode() {
    let frames = ivf_frames();
    let bytes = include_bytes!("data/av1_64x64.ivf");
    assert_eq!(Decoder::probe(bytes).unwrap(), Codec::Av1);
    let seq = parse_sequence_header(&SEQ_64X64).unwrap();
    let mut decoder = Decoder::from_av1_sequence_header(&seq).unwrap();
    let packet = Packet {
        codec: Codec::Av1,
        data: frames[0].1.clone(),
        timestamp: 0,
        is_keyframe: true,
    };
    let err = decoder.decode(&packet).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
}
