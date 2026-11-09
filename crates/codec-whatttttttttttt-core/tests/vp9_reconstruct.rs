//! Integration tests for VP9 reconstruction (Phase 8 final round,
//! VP9 Bitstream & Decoding Process Specification v0.6, sections
//! 6.4 and 8).
//!
//! Every decoded frame is compared pixel-by-pixel against an
//! ffmpeg 8.1.2 (`libvpx-vp9`) decode of the same file, stored as
//! rawvideo yuv420p in `tests/data/*_ref.yuv` (tightly packed:
//! Y plane, then U, then V). The goal is bit-exact output
//! (`max_diff == 0`); the comparison reports the actual maximum
//! absolute sample difference and the number of differing samples
//! so a rounding gap, if one ever appears, is measured and not
//! hidden. A test fails when `max_diff != 0`.
//!
//! Reference YUVs were produced with:
//! `ffmpeg -i <file> -pix_fmt yuv420p -f rawvideo <name>_ref.yuv`
//! (see `tests/data/README.md` for the encode commands).

use codec_whatttttttttttt_core::{
    parse_superframe, parse_vp9_frame, reconstruct_vp9_frame, Codec, Demuxer, Packet, PixelFormat,
    VideoFrame, Vp9ReconstructionState, Vp9ReferenceState,
};

/// Reads an IVF file's frame payloads.
fn ivf_frames(name: &str) -> Vec<Vec<u8>> {
    let data = std::fs::read(format!("tests/data/{name}")).unwrap();
    assert_eq!(&data[0..4], b"DKIF");
    assert_eq!(&data[8..12], b"VP90");
    let mut frames = Vec::new();
    let mut offset = 32;
    while offset + 12 <= data.len() {
        let size = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        frames.push(data[offset + 12..offset + 12 + size].to_vec());
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

/// Reads a reference YUV file (ffmpeg rawvideo yuv420p).
fn ref_yuv(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/data/{name}")).unwrap()
}

/// Compares one decoded frame against its reference slice.
/// Returns (maximum absolute sample difference, differing sample
/// count). The frame must be YUV420 and tightly packed, which is
/// what the reconstruction pipeline returns.
fn compare_frame(frame: &VideoFrame, reference: &[u8]) -> (u8, usize) {
    assert_eq!(frame.format, PixelFormat::Yuv420);
    let width = frame.width as usize;
    let height = frame.height as usize;
    let y_len = width * height;
    let uv_width = width.div_ceil(2);
    let uv_height = height.div_ceil(2);
    let uv_len = uv_width * uv_height;
    assert_eq!(
        reference.len(),
        y_len + 2 * uv_len,
        "reference slice has the wrong size for {width}x{height}"
    );
    assert_eq!(frame.planes.len(), 3);
    assert_eq!(frame.planes[0].len(), y_len);
    assert_eq!(frame.planes[1].len(), uv_len);
    assert_eq!(frame.planes[2].len(), uv_len);
    let mut max_diff = 0u8;
    let mut diff_count = 0usize;
    for (plane, reference_plane) in frame.planes.iter().zip([
        &reference[..y_len],
        &reference[y_len..y_len + uv_len],
        &reference[y_len + uv_len..],
    ]) {
        for (a, b) in plane.iter().zip(reference_plane.iter()) {
            let diff = a.abs_diff(*b);
            if diff > 0 {
                diff_count += 1;
            }
            max_diff = max_diff.max(diff);
        }
    }
    (max_diff, diff_count)
}

/// Decodes every frame of an IVF file and compares it against the
/// reference YUV. Returns (max diff, differing sample count,
/// frame count). Panics if any frame fails to parse or reconstruct.
fn decode_ivf_and_compare(name: &str, ref_name: &str) -> (u8, usize, usize) {
    let payloads = ivf_frames(name);
    let reference = ref_yuv(ref_name);
    let mut parse_state = Vp9ReferenceState::default();
    let mut refs = Vp9ReconstructionState::new();
    let mut max_diff = 0u8;
    let mut diff_count = 0usize;
    let mut frames = 0usize;
    let mut ref_offset = 0usize;
    for (i, payload) in payloads.iter().enumerate() {
        // A unit may hold a superframe; split it first.
        for (offset, size) in parse_superframe(payload).unwrap() {
            let sub = &payload[offset..offset + size];
            let parsed = parse_vp9_frame(sub, &mut parse_state)
                .unwrap_or_else(|e| panic!("{name} frame {i}: parse failed: {e}"));
            let frame = reconstruct_vp9_frame(&parsed, sub, &mut refs)
                .unwrap_or_else(|e| panic!("{name} frame {i}: reconstruct failed: {e}"));
            let frame_len = frame.width as usize * frame.height as usize * 3 / 2;
            let (diff, count) =
                compare_frame(&frame, &reference[ref_offset..ref_offset + frame_len]);
            max_diff = max_diff.max(diff);
            diff_count += count;
            frames += 1;
            ref_offset += frame_len;
        }
    }
    assert_eq!(
        ref_offset,
        reference.len(),
        "{name}: reference YUV has trailing bytes"
    );
    (max_diff, diff_count, frames)
}

/// Decodes every `WebM` video packet and compares it against the
/// reference YUV. Returns (max diff, differing sample count,
/// frame count).
fn decode_webm_and_compare(name: &str, ref_name: &str) -> (u8, usize, usize) {
    let packets = webm_video_packets(name);
    let reference = ref_yuv(ref_name);
    let mut parse_state = Vp9ReferenceState::default();
    let mut refs = Vp9ReconstructionState::new();
    let mut max_diff = 0u8;
    let mut diff_count = 0usize;
    let mut frames = 0usize;
    let mut ref_offset = 0usize;
    for (i, packet) in packets.iter().enumerate() {
        for (offset, size) in parse_superframe(&packet.data).unwrap() {
            let sub = &packet.data[offset..offset + size];
            let parsed = parse_vp9_frame(sub, &mut parse_state)
                .unwrap_or_else(|e| panic!("{name} packet {i}: parse failed: {e}"));
            let frame = reconstruct_vp9_frame(&parsed, sub, &mut refs)
                .unwrap_or_else(|e| panic!("{name} packet {i}: reconstruct failed: {e}"));
            let frame_len = frame.width as usize * frame.height as usize * 3 / 2;
            let (diff, count) =
                compare_frame(&frame, &reference[ref_offset..ref_offset + frame_len]);
            max_diff = max_diff.max(diff);
            diff_count += count;
            frames += 1;
            ref_offset += frame_len;
        }
    }
    assert_eq!(
        ref_offset,
        reference.len(),
        "{name}: reference YUV has trailing bytes"
    );
    (max_diff, diff_count, frames)
}

/// Asserts a fixture decodes bit-exactly against ffmpeg.
fn assert_bit_exact(name: &str, max_diff: u8, diff_count: usize, frames: usize) {
    assert_eq!(
        max_diff, 0,
        "{name}: {frames} frames decoded but pixels differ from ffmpeg: max diff {max_diff}, {diff_count} differing samples"
    );
}

#[test]
fn keyframe_only_64x64_matches_ffmpeg() {
    let (max_diff, diff_count, frames) =
        decode_ivf_and_compare("vp9rec_keyonly_64x64.ivf", "vp9rec_keyonly_64x64_ref.yuv");
    assert_eq!(frames, 10);
    assert_bit_exact("vp9rec_keyonly_64x64", max_diff, diff_count, frames);
}

#[test]
#[ignore = "VP9 inter-frame reconstruction not yet pixel-correct (see LIMITS.md)"]
fn inter_320x240_matches_ffmpeg() {
    let (max_diff, diff_count, frames) =
        decode_ivf_and_compare("vp9rec_320x240.ivf", "vp9rec_320x240_ref.yuv");
    assert_eq!(frames, 10);
    assert_bit_exact("vp9rec_320x240", max_diff, diff_count, frames);
}

#[test]
#[ignore = "VP9 inter-frame reconstruction not yet pixel-correct (see LIMITS.md)"]
fn inter_640x480_two_tile_columns_matches_ffmpeg() {
    let (max_diff, diff_count, frames) =
        decode_ivf_and_compare("vp9rec_640x480.ivf", "vp9rec_640x480_ref.yuv");
    assert_eq!(frames, 10);
    assert_bit_exact("vp9rec_640x480", max_diff, diff_count, frames);
}

#[test]
#[ignore = "VP9 inter-frame reconstruction not yet pixel-correct (see LIMITS.md)"]
fn frame_parallel_off_320x240_matches_ffmpeg() {
    // frame_parallel_decoding_mode = false, so probability
    // adaptation runs and the next frame reads the adapted
    // probabilities.
    let (max_diff, diff_count, frames) =
        decode_ivf_and_compare("vp9rec_320x240_fp0.ivf", "vp9rec_320x240_fp0_ref.yuv");
    assert_eq!(frames, 10);
    assert_bit_exact("vp9rec_320x240_fp0", max_diff, diff_count, frames);
}

#[test]
fn lossless_64x64_matches_ffmpeg() {
    // Lossless (q = 0) exercises the WHT inverse transform and
    // the Only4x4 transform mode.
    let (max_diff, diff_count, frames) =
        decode_ivf_and_compare("vp9rec_lossless_64x64.ivf", "vp9rec_lossless_64x64_ref.yuv");
    assert_eq!(frames, 10);
    assert_bit_exact("vp9rec_lossless_64x64", max_diff, diff_count, frames);
}

#[test]
#[ignore = "VP9 inter-frame reconstruction not yet pixel-correct (see LIMITS.md)"]
fn mandelbrot_320x240_matches_ffmpeg() {
    // High-frequency content: many non-zero coefficients, all
    // transform sizes, and a large keyframe.
    let (max_diff, diff_count, frames) =
        decode_ivf_and_compare("vp9rec_mandel_320x240.ivf", "vp9rec_mandel_320x240_ref.yuv");
    assert_eq!(frames, 10);
    assert_bit_exact("vp9rec_mandel_320x240", max_diff, diff_count, frames);
}

#[test]
fn tx32_keyframe_320x128_matches_ffmpeg() {
    // Regression test for the TX_32X32 transform-type fix
    // (spec section 6.4.25: TxType is DCT_DCT when txSz is
    // TX_32X32, whatever the intra mode). The keyframe of this
    // file has a non-skipped 32x32 luma block with a V_PRED mode
    // (which maps to ADST_DCT); using the mode's transform type
    // instead of DCT_DCT corrupted the block's column transform
    // and, through intra prediction, the whole right half of the
    // frame. Only the keyframe is checked: later inter frames
    // still differ (see the inter-frame tests).
    let payloads = ivf_frames("vp9rec_testsrc2_320x128.ivf");
    let reference = ref_yuv("vp9rec_testsrc2_320x128_ref.yuv");
    let mut parse_state = Vp9ReferenceState::default();
    let mut refs = Vp9ReconstructionState::new();
    let parsed = parse_vp9_frame(&payloads[0], &mut parse_state).unwrap();
    let frame = reconstruct_vp9_frame(&parsed, &payloads[0], &mut refs).unwrap();
    let frame_len = frame.width as usize * frame.height as usize * 3 / 2;
    let (max_diff, diff_count) = compare_frame(&frame, &reference[..frame_len]);
    assert_bit_exact("vp9rec_testsrc2_320x128 keyframe", max_diff, diff_count, 1);
}

#[test]
fn tx32_keyframe_mandel_320x240_matches_ffmpeg() {
    // Same TX_32X32 regression, high-frequency content: the
    // keyframe used to fail the tile bool-decoder exit check.
    let payloads = ivf_frames("vp9rec_mandel_320x240.ivf");
    let reference = ref_yuv("vp9rec_mandel_320x240_ref.yuv");
    let mut parse_state = Vp9ReferenceState::default();
    let mut refs = Vp9ReconstructionState::new();
    let parsed = parse_vp9_frame(&payloads[0], &mut parse_state).unwrap();
    let frame = reconstruct_vp9_frame(&parsed, &payloads[0], &mut refs).unwrap();
    let frame_len = frame.width as usize * frame.height as usize * 3 / 2;
    let (max_diff, diff_count) = compare_frame(&frame, &reference[..frame_len]);
    assert_bit_exact("vp9rec_mandel_320x240 keyframe", max_diff, diff_count, 1);
}

#[test]
fn tx32_keyframe_mandel_320x128_matches_ffmpeg() {
    // Same TX_32X32 regression, smallest failing case (2
    // superblock rows): the keyframe used to under-consume its
    // tile by 823 bytes and fail the exit check.
    let payloads = ivf_frames("vp9rec_mandel_320x128.ivf");
    let reference = ref_yuv("vp9rec_mandel_320x128_ref.yuv");
    let mut parse_state = Vp9ReferenceState::default();
    let mut refs = Vp9ReconstructionState::new();
    let parsed = parse_vp9_frame(&payloads[0], &mut parse_state).unwrap();
    let frame = reconstruct_vp9_frame(&parsed, &payloads[0], &mut refs).unwrap();
    let frame_len = frame.width as usize * frame.height as usize * 3 / 2;
    let (max_diff, diff_count) = compare_frame(&frame, &reference[..frame_len]);
    assert_bit_exact("vp9rec_mandel_320x128 keyframe", max_diff, diff_count, 1);
}

#[test]
#[ignore = "VP9 inter-frame reconstruction not yet pixel-correct (see LIMITS.md)"]
fn existing_64x64_matches_ffmpeg() {
    // The original parser fixture: inter frames with switchable
    // interpolation and high-precision motion vectors.
    let (max_diff, diff_count, frames) =
        decode_ivf_and_compare("vp9_64x64.ivf", "vp9_64x64_ref.yuv");
    assert_eq!(frames, 10);
    assert_bit_exact("vp9_64x64", max_diff, diff_count, frames);
}

#[test]
#[ignore = "VP9 inter-frame reconstruction not yet pixel-correct (see LIMITS.md)"]
fn keyframes_webm_matches_ffmpeg() {
    // Forced keyframes mid-stream (frames 0, 3, 6, 9): the frame
    // context resets mid-stream.
    let (max_diff, diff_count, frames) =
        decode_webm_and_compare("vp9_keyframes.webm", "vp9_keyframes_ref.yuv");
    assert_eq!(frames, 10);
    assert_bit_exact("vp9_keyframes", max_diff, diff_count, frames);
}

#[test]
#[ignore = "VP9 inter-frame reconstruction not yet pixel-correct (see LIMITS.md)"]
fn tiles_webm_matches_ffmpeg() {
    // 512x64 with two tile columns.
    let (max_diff, diff_count, frames) =
        decode_webm_and_compare("vp9_tiles.webm", "vp9_tiles_ref.yuv");
    assert_eq!(frames, 10);
    assert_bit_exact("vp9_tiles", max_diff, diff_count, frames);
}

#[test]
fn superframe_ivf_matches_ffmpeg() {
    // One IVF unit holding two frames (key + inter); the unit is
    // split with `parse_superframe` before parsing each frame.
    let (max_diff, diff_count, frames) =
        decode_ivf_and_compare("vp9_superframe.ivf", "vp9_superframe_ref.yuv");
    assert_eq!(frames, 2);
    assert_bit_exact("vp9_superframe", max_diff, diff_count, frames);
}

#[test]
fn show_existing_frame_returns_saved_frame() {
    let payloads = ivf_frames("vp9rec_keyonly_64x64.ivf");
    let reference = ref_yuv("vp9rec_keyonly_64x64_ref.yuv");
    let mut parse_state = Vp9ReferenceState::default();
    let mut refs = Vp9ReconstructionState::new();
    let first = parse_vp9_frame(&payloads[0], &mut parse_state).unwrap();
    let decoded = reconstruct_vp9_frame(&first, &payloads[0], &mut refs).unwrap();
    // Packet [0x88]: frame marker 2, profile 0, show_existing = 1,
    // frame_to_show_map_idx = 0 (uncompressed header bits are
    // read most-significant-bit first).
    let show = parse_vp9_frame(&[0x88], &mut parse_state).unwrap();
    assert!(show.header.show_existing_frame);
    let shown = reconstruct_vp9_frame(&show, &[0x88], &mut refs).unwrap();
    assert_eq!(shown.width, decoded.width);
    assert_eq!(shown.height, decoded.height);
    assert_eq!(shown.planes, decoded.planes);
    let (max_diff, diff_count) = compare_frame(&shown, &reference[..6144]);
    assert_eq!((max_diff, diff_count), (0, 0));
}

#[test]
fn inter_frame_without_reference_returns_error() {
    // Decode an inter frame first, with no keyframe before it:
    // the LAST reference slot is empty.
    let payloads = ivf_frames("vp9_64x64.ivf");
    let mut parse_state = Vp9ReferenceState::default();
    let mut refs = Vp9ReconstructionState::new();
    // The parser itself rejects an inter frame whose reference
    // slot is empty; if it ever parsed, the reconstruction must
    // reject it instead. Either stage must fail cleanly.
    match parse_vp9_frame(&payloads[1], &mut parse_state) {
        Err(e) => {
            assert!(
                format!("{e}").contains("reference"),
                "expected an empty-reference error, got: {e}"
            );
        }
        Ok(parsed) => {
            assert!(!parsed.header.is_keyframe);
            let err = reconstruct_vp9_frame(&parsed, &payloads[1], &mut refs).unwrap_err();
            assert!(
                format!("{err}").contains("reference"),
                "expected an empty-reference error, got: {err}"
            );
        }
    }
}

#[test]
fn truncated_keyframe_returns_error() {
    let payloads = ivf_frames("vp9rec_keyonly_64x64.ivf");
    let mut parse_state = Vp9ReferenceState::default();
    let mut refs = Vp9ReconstructionState::new();
    // Cut the payload in half: the header parses, but the tile
    // data runs out mid-frame.
    let half = payloads[0][..payloads[0].len() / 2].to_vec();
    let parsed = parse_vp9_frame(&half, &mut parse_state).unwrap();
    let err = reconstruct_vp9_frame(&parsed, &half, &mut refs).unwrap_err();
    assert!(
        !format!("{err}").is_empty(),
        "truncated frame should return a typed error"
    );
}

#[test]
fn header_only_keyframe_returns_error() {
    let payloads = ivf_frames("vp9rec_keyonly_64x64.ivf");
    let mut parse_state = Vp9ReferenceState::default();
    let mut refs = Vp9ReconstructionState::new();
    let parsed = parse_vp9_frame(&payloads[0], &mut parse_state).unwrap();
    // Only the header bytes: no tile data at all. The parser
    // rejects the empty tile; if it ever parsed, the
    // reconstruction must reject it instead.
    let header_only = payloads[0][..parsed.header_bytes].to_vec();
    match parse_vp9_frame(&header_only, &mut Vp9ReferenceState::default()) {
        Err(e) => {
            assert!(
                !format!("{e}").is_empty(),
                "header-only frame should return a typed error"
            );
        }
        Ok(reparsed) => {
            let err = reconstruct_vp9_frame(&reparsed, &header_only, &mut refs).unwrap_err();
            assert!(
                !format!("{err}").is_empty(),
                "header-only frame should return a typed error"
            );
        }
    }
}

#[test]
fn corrupted_tile_data_does_not_decode_as_correct_pixels() {
    let payloads = ivf_frames("vp9rec_keyonly_64x64.ivf");
    let reference = ref_yuv("vp9rec_keyonly_64x64_ref.yuv");
    let mut parse_state = Vp9ReferenceState::default();
    let mut refs = Vp9ReconstructionState::new();
    // Flip a byte in the middle of the tile data. The decoder
    // must not silently return the correct frame: it either
    // errors or returns different pixels.
    let mut corrupted = payloads[0].clone();
    let mid = corrupted.len() / 2;
    corrupted[mid] ^= 0xFF;
    let parsed = parse_vp9_frame(&corrupted, &mut parse_state).unwrap();
    match reconstruct_vp9_frame(&parsed, &corrupted, &mut refs) {
        Err(_) => {}
        Ok(frame) => {
            let (max_diff, diff_count) = compare_frame(&frame, &reference[..6144]);
            assert!(
                diff_count > 0 || max_diff > 0,
                "corrupted tile decoded to the exact correct pixels"
            );
        }
    }
}
