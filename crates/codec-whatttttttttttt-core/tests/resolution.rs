//! Resolution, scaling and frame-pool behaviour tests.
//!
//! Decoder tests stay at header level on purpose: AV1 and VP9
//! decoding in this crate is parser-only, so no test here claims
//! a frame was decoded. Resolution names below (240p, 720p, ...)
//! are only test names; every check is computed from
//! width/height.

use codec_whatttttttttttt_core::{
    av1_level, frame_bytes, minimum_av1_level, parse_sequence_header, scale_frame, suggest_av1,
    suggest_vp9, validate_av1_frame, validate_av1_sequence_header, validate_vp9_frame,
    validate_vp9_frame_header, vp9_profile_limits, ColourRange, ColourSpace, Error, FramePool,
    FrameSize, PixelFormat, ScaleFilter, VideoFrame, Vp9FrameHeader,
};

/// Sequence header payload of `av1_64x64.ivf` frame 0 (level 0,
/// 64x64), shared with the `av1_sequence` unit tests.
const SEQ_64X64: [u8; 10] = [0x00, 0x00, 0x00, 0x02, 0xAF, 0xFF, 0x9B, 0x5F, 0x20, 0x08];

fn gradient_frame(width: u32, height: u32) -> VideoFrame {
    let w = width as usize;
    let h = height as usize;
    let mut y = Vec::with_capacity(w * h);
    for _row in 0..height {
        for col in 0..width {
            y.push(u8::try_from((col * 255) / width.max(1)).unwrap());
        }
    }
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    VideoFrame {
        width,
        height,
        format: PixelFormat::Yuv420,
        planes: vec![y, vec![128u8; cw * ch], vec![128u8; cw * ch]],
        strides: vec![w, cw, cw],
        timestamp: 0,
        colour_space: ColourSpace::default(),
        colour_range: ColourRange::default(),
    }
}

fn checkerboard_frame(width: u32, height: u32, cell: u32) -> VideoFrame {
    let w = width as usize;
    let h = height as usize;
    let mut y = Vec::with_capacity(w * h);
    for row in 0..height {
        for col in 0..width {
            let on = ((row / cell) + (col / cell)) % 2 == 0;
            y.push(if on { 255 } else { 0 });
        }
    }
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    VideoFrame {
        width,
        height,
        format: PixelFormat::Yuv420,
        planes: vec![y, vec![128u8; cw * ch], vec![128u8; cw * ch]],
        strides: vec![w, cw, cw],
        timestamp: 0,
        colour_space: ColourSpace::default(),
        colour_range: ColourRange::default(),
    }
}

fn step_frame(width: u32, height: u32) -> VideoFrame {
    let w = width as usize;
    let h = height as usize;
    let mut y = Vec::with_capacity(w * h);
    for _ in 0..h {
        for col in 0..w {
            y.push(if col < w / 2 { 0 } else { 255 });
        }
    }
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    VideoFrame {
        width,
        height,
        format: PixelFormat::Yuv420,
        planes: vec![y, vec![128u8; cw * ch], vec![128u8; cw * ch]],
        strides: vec![w, cw, cw],
        timestamp: 0,
        colour_space: ColourSpace::default(),
        colour_range: ColourRange::default(),
    }
}

/// Largest absolute difference between horizontally adjacent
/// luma samples: a proxy for edge sharpness.
fn max_horizontal_step(frame: &VideoFrame) -> u32 {
    let plane = &frame.planes[0];
    let w = frame.width as usize;
    let mut max = 0u32;
    for row in 0..frame.height as usize {
        for col in 0..w - 1 {
            let diff = u32::from(plane[row * w + col].abs_diff(plane[row * w + col + 1]));
            max = max.max(diff);
        }
    }
    max
}

/// Standard deviation of the luma plane: a proxy for preserved
/// contrast after downscaling a checkerboard.
#[allow(
    clippy::cast_precision_loss,
    reason = "test planes hold at most a few hundred thousand samples, exactly representable in f64"
)]
fn luma_stddev(frame: &VideoFrame) -> f64 {
    let plane = &frame.planes[0];
    let n = plane.len() as f64;
    let mean = plane.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
    let variance = plane
        .iter()
        .map(|v| {
            let d = f64::from(*v) - mean;
            d * d
        })
        .sum::<f64>()
        / n;
    variance.sqrt()
}

#[test]
fn av1_level_2_0_accepts_240p_and_rejects_720p() {
    validate_av1_frame(0, 426, 240, Some(30)).unwrap();
    assert!(matches!(
        validate_av1_frame(0, 1280, 720, Some(30)),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn av1_level_3_1_accepts_720p_and_rejects_1080p() {
    validate_av1_frame(5, 1280, 720, Some(30)).unwrap();
    assert!(matches!(
        validate_av1_frame(5, 1920, 1080, Some(30)),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn av1_level_4_0_accepts_1080p() {
    validate_av1_frame(8, 1920, 1080, Some(30)).unwrap();
    validate_av1_frame(8, 1920, 1080, Some(60)).unwrap_err();
}

#[test]
fn av1_level_5_1_accepts_4k_and_level_6_1_accepts_8k() {
    validate_av1_frame(13, 3840, 2160, Some(60)).unwrap();
    assert!(matches!(
        validate_av1_frame(13, 7680, 4320, Some(30)),
        Err(Error::InvalidArgument(_))
    ));
    validate_av1_frame(17, 7680, 4320, Some(60)).unwrap();
}

#[test]
fn av1_undefined_level_idx_is_rejected() {
    assert!(matches!(av1_level(2), Err(Error::InvalidArgument(_))));
    assert!(matches!(av1_level(6), Err(Error::InvalidArgument(_))));
    assert!(matches!(
        validate_av1_frame(2, 426, 240, None),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn av1_minimum_level_is_computed_from_dimensions() {
    assert_eq!(minimum_av1_level(426, 240).unwrap().seq_level_idx, 0);
    assert_eq!(minimum_av1_level(640, 360).unwrap().seq_level_idx, 1);
    assert_eq!(minimum_av1_level(1280, 720).unwrap().seq_level_idx, 5);
    assert_eq!(minimum_av1_level(1920, 1080).unwrap().seq_level_idx, 8);
    assert_eq!(minimum_av1_level(3840, 2160).unwrap().seq_level_idx, 12);
    assert_eq!(minimum_av1_level(7680, 4320).unwrap().seq_level_idx, 16);
}

#[test]
fn av1_sequence_header_validates_against_its_declared_level() {
    let header = parse_sequence_header(&SEQ_64X64).unwrap();
    assert_eq!(header.level(), 0);
    validate_av1_sequence_header(&header).unwrap();
    let mut oversize = header.clone();
    oversize.max_frame_width_minus_1 = 1919;
    oversize.max_frame_height_minus_1 = 1079;
    assert!(matches!(
        validate_av1_sequence_header(&oversize),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn vp9_profiles_have_no_level_size_distinction() {
    for profile in 0..=3u32 {
        validate_vp9_frame(profile, 426, 240).unwrap();
        validate_vp9_frame(profile, 1280, 720).unwrap();
        validate_vp9_frame(profile, 1920, 1080).unwrap();
        validate_vp9_frame(profile, 3840, 2160).unwrap();
        validate_vp9_frame(profile, 7680, 4320).unwrap();
    }
    assert!(matches!(
        validate_vp9_frame(4, 640, 360),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        validate_vp9_frame(0, 8193, 4320),
        Err(Error::Overflow(_))
    ));
}

#[test]
fn vp9_frame_header_validates_profile_and_size() {
    let mut header = Vp9FrameHeader {
        profile: 0,
        frame_width: 1920,
        frame_height: 1080,
        ..Default::default()
    };
    validate_vp9_frame_header(&header).unwrap();
    header.profile = 2;
    validate_vp9_frame_header(&header).unwrap();
    header.frame_width = 8193;
    assert!(matches!(
        validate_vp9_frame_header(&header),
        Err(Error::Overflow(_))
    ));
    let limits = vp9_profile_limits(0).unwrap();
    assert_eq!(limits.bit_depths, &[8]);
    assert!(limits.allows_420);
    assert!(!limits.allows_422_444);
}

#[test]
fn frame_size_is_computed_from_dimensions_not_labels() {
    assert_eq!(FrameSize::new(426, 240).unwrap().pixel_count(), 102_240);
    assert_eq!(
        FrameSize::new(7680, 4320).unwrap().pixel_count(),
        33_177_600
    );
    assert_eq!(
        frame_bytes(7680, 4320, PixelFormat::Yuv420, 8).unwrap(),
        49_766_400
    );
    assert_eq!(
        frame_bytes(7680, 4320, PixelFormat::Yuv420, 10).unwrap(),
        99_532_800
    );
}

#[test]
fn scaler_upscale_lanczos_is_sharper_than_bilinear_on_a_step() {
    let frame = step_frame(64, 64);
    let bilinear = scale_frame(&frame, 128, 128, ScaleFilter::Bilinear).unwrap();
    let lanczos = scale_frame(&frame, 128, 128, ScaleFilter::Lanczos3).unwrap();
    let bicubic = scale_frame(&frame, 128, 128, ScaleFilter::Bicubic).unwrap();
    assert_eq!((bilinear.width, bilinear.height), (128, 128));
    assert_eq!((lanczos.width, lanczos.height), (128, 128));
    assert!(max_horizontal_step(&lanczos) > max_horizontal_step(&bilinear));
    assert!(max_horizontal_step(&bicubic) > max_horizontal_step(&bilinear));
}

#[test]
fn scaler_downscale_lanczos_preserves_more_checkerboard_contrast() {
    let frame = checkerboard_frame(128, 128, 8);
    let bilinear = scale_frame(&frame, 32, 32, ScaleFilter::Bilinear).unwrap();
    let lanczos = scale_frame(&frame, 32, 32, ScaleFilter::Lanczos3).unwrap();
    assert!(luma_stddev(&lanczos) > luma_stddev(&bilinear));
}

#[test]
fn scaler_gradient_upscale_stays_monotonic_for_bilinear() {
    let frame = gradient_frame(32, 32);
    let scaled = scale_frame(&frame, 64, 64, ScaleFilter::Bilinear).unwrap();
    let plane = &scaled.planes[0];
    for row in 0..64usize {
        for col in 0..63usize {
            assert!(plane[row * 64 + col] <= plane[row * 64 + col + 1]);
        }
    }
}

#[test]
fn scaler_handles_1x1_and_rejects_zero_destination() {
    let frame = gradient_frame(1, 1);
    let scaled = scale_frame(&frame, 4, 4, ScaleFilter::Lanczos3).unwrap();
    assert!(scaled.planes[0].iter().all(|v| *v == frame.planes[0][0]));
    assert!(matches!(
        scale_frame(&frame, 0, 4, ScaleFilter::Bilinear),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn frame_pool_reuses_the_same_buffer() {
    let mut pool = FramePool::new(2);
    let frame = pool.checkout(1920, 1080, PixelFormat::Yuv420, 8).unwrap();
    assert_eq!(frame.planes[0].len(), 1920 * 1080);
    let ptr = frame.planes[0].as_ptr();
    let stats = pool.stats();
    assert_eq!(stats.allocations, 1);
    assert_eq!(stats.reuses, 0);
    pool.checkin(frame);
    assert_eq!(pool.stats().idle_frames, 1);
    let again = pool.checkout(1920, 1080, PixelFormat::Yuv420, 8).unwrap();
    assert_eq!(again.planes[0].as_ptr(), ptr);
    assert_eq!(pool.stats().reuses, 1);
    assert_eq!(pool.stats().allocations, 1);
}

#[test]
fn frame_pool_8k_8bit_fits_but_10bit_hits_the_allocation_limit() {
    let mut pool = FramePool::new(2);
    let frame = pool.checkout(7680, 4320, PixelFormat::Yuv420, 8).unwrap();
    assert_eq!(frame.planes[0].len(), 7680 * 4320);
    pool.checkin(frame);
    assert!(matches!(
        pool.checkout(7680, 4320, PixelFormat::Yuv420, 10),
        Err(Error::AllocationLimit { .. })
    ));
}

#[test]
fn frame_pool_drops_frames_when_full() {
    let mut pool = FramePool::new(1);
    let first = pool.checkout(64, 64, PixelFormat::Yuv420, 8).unwrap();
    let second = pool.checkout(64, 64, PixelFormat::Yuv420, 8).unwrap();
    pool.checkin(first);
    pool.checkin(second);
    assert_eq!(pool.stats().idle_frames, 1);
}

#[test]
fn bitrate_and_crf_suggestions_rise_with_resolution() {
    for (w, h) in [
        (426u32, 240u32),
        (1280, 720),
        (1920, 1080),
        (3840, 2160),
        (7680, 4320),
    ] {
        let av1 = suggest_av1(w, h, 30).unwrap();
        let vp9 = suggest_vp9(w, h, 30).unwrap();
        assert!(av1.bitrate_kbps_min < av1.bitrate_kbps_max);
        assert!(vp9.bitrate_kbps_min < vp9.bitrate_kbps_max);
        assert!(vp9.bitrate_kbps_min >= av1.bitrate_kbps_min);
    }
    let sd = suggest_av1(426, 240, 30).unwrap();
    let hd = suggest_av1(1920, 1080, 30).unwrap();
    let uhd = suggest_av1(7680, 4320, 30).unwrap();
    assert!(hd.bitrate_kbps_min > sd.bitrate_kbps_max);
    assert!(uhd.bitrate_kbps_min > hd.bitrate_kbps_max);
    assert!(uhd.crf <= hd.crf);
    assert!(hd.crf <= sd.crf);
    assert_eq!(
        suggest_av1(1920, 1080, 60).unwrap().bitrate_kbps_min,
        hd.bitrate_kbps_min * 2
    );
}
