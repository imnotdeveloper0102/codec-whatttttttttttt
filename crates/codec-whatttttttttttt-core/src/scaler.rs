//! Scaling (resizing) of decoded video frames.
//!
//! Three real resampling filters, all implemented from their
//! kernels rather than approximated by one another: bilinear
//! (2-tap tent), bicubic Catmull-Rom (4-tap cubic) and Lanczos-3
//! (6-tap windowed sinc, `a = 3`). Each output sample is a
//! weighted sum of input samples around its centre-mapped
//! position, with weights renormalised so a constant plane stays
//! constant, and every tap index is clamped to the plane bounds,
//! so scaling never reads outside a plane.

use crate::error::{Error, Result};
use crate::frame::{PixelFormat, VideoFrame};
use crate::limits::{MAX_HEIGHT, MAX_WIDTH};

/// Resampling filter used by `scale_frame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleFilter {
    /// Two-tap tent filter: smooth, cheapest, softest.
    Bilinear,
    /// Four-tap Catmull-Rom cubic: sharper than bilinear, with
    /// mild overshoot at hard edges.
    Bicubic,
    /// Six-tap Lanczos with `a = 3`: sharpest of the three, with
    /// ringing at hard edges.
    Lanczos3,
}

impl ScaleFilter {
    /// Half-width of the filter support in input samples.
    fn radius(self) -> f32 {
        match self {
            Self::Bilinear => 1.0,
            Self::Bicubic => 2.0,
            Self::Lanczos3 => 3.0,
        }
    }

    /// Filter weight at distance `x` in input samples.
    fn weight(self, x: f32) -> f32 {
        let ax = x.abs();
        match self {
            Self::Bilinear => {
                if ax >= 1.0 {
                    0.0
                } else {
                    1.0 - ax
                }
            }
            Self::Bicubic => catmull_rom(ax),
            Self::Lanczos3 => lanczos(ax),
        }
    }
}

/// Catmull-Rom cubic kernel at distance `x >= 0`. The piecewise
/// form is the standard one: `1.5x^3 - 2.5x^2 + 1` inside one
/// sample, `-0.5x^3 + 2.5x^2 - 4x + 2` between one and two.
fn catmull_rom(x: f32) -> f32 {
    if x >= 2.0 {
        0.0
    } else if x < 1.0 {
        1.5 * x * x * x - 2.5 * x * x + 1.0
    } else {
        -0.5 * x * x * x + 2.5 * x * x - 4.0 * x + 2.0
    }
}

/// Normalised sinc, `sin(pi x) / (pi x)`, with the removable
/// singularity at zero handled explicitly.
fn sinc(x: f32) -> f32 {
    if x == 0.0 {
        1.0
    } else {
        let px = std::f32::consts::PI * x;
        px.sin() / px
    }
}

/// Lanczos-3 kernel at distance `x >= 0`: `sinc(x) * sinc(x/3)`
/// inside three samples, zero outside.
fn lanczos(x: f32) -> f32 {
    if x >= 3.0 {
        0.0
    } else {
        sinc(x) * sinc(x / 3.0)
    }
}

/// One-dimensional tap list for one output position: (input
/// index, weight) pairs with indices already clamped into the
/// input range and weights renormalised to sum to one. The
/// centre of output sample `dst` maps to `(dst + 0.5) * scale -
/// 0.5` in input coordinates, so a 1:1 scale reproduces the
/// input exactly. When downscaling (`scale > 1`) the kernel is
/// widened by the scale factor, so one output sample averages
/// the input samples it covers instead of point-sampling
/// between them; without that, downscaling aliases.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    reason = "tap indices are within the frame, far below i32 range; f32 coordinates lose nothing at frame sizes"
)]
fn taps(filter: ScaleFilter, dst: u32, src_len: u32, scale: f32) -> Vec<(usize, f32)> {
    let centre = (dst as f32 + 0.5) * scale - 0.5;
    let widen = scale.max(1.0);
    let radius = filter.radius() * widen;
    let first = (centre - radius).ceil() as i32;
    let last = (centre + radius).floor() as i32;
    let mut taps_out: Vec<(usize, f32)> = Vec::new();
    let mut sum = 0.0f32;
    for index in first..=last {
        let weight = filter.weight((centre - index as f32) / widen);
        if weight == 0.0 {
            continue;
        }
        // Clamping at the edge replicates the border sample; the
        // renormalisation below keeps a constant signal constant.
        let clamped = index.clamp(0, src_len as i32 - 1);
        let clamped = usize::try_from(clamped).unwrap_or(0);
        sum += weight;
        taps_out.push((clamped, weight));
    }
    if sum != 0.0 {
        for (_, weight) in &mut taps_out {
            *weight /= sum;
        }
    }
    taps_out
}

/// Scales one single-channel plane. Separable: one horizontal
/// pass into a temporary buffer, then one vertical pass. Both
/// passes read only through the clamped tap lists.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "accumulators are clamped to 0..=255 before the u8 cast; dimensions are bounded by the library limit, so f32 ratios are exact enough"
)]
fn scale_plane(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    filter: ScaleFilter,
) -> Vec<u8> {
    let scale_x = src_w as f32 / dst_w as f32;
    let scale_y = src_h as f32 / dst_h as f32;
    // Tap lists depend only on the output coordinate, so they are
    // built once per column/row instead of once per pixel.
    let h_taps: Vec<Vec<(usize, f32)>> = (0..dst_w)
        .map(|x| taps(filter, x, src_w, scale_x))
        .collect();
    let v_taps: Vec<Vec<(usize, f32)>> = (0..dst_h)
        .map(|y| taps(filter, y, src_h, scale_y))
        .collect();
    let mut tmp = vec![0u8; dst_w as usize * src_h as usize];
    for y in 0..src_h as usize {
        for x in 0..dst_w as usize {
            let mut acc = 0.0f32;
            for (index, weight) in &h_taps[x] {
                acc += weight * f32::from(src[y * src_w as usize + index]);
            }
            tmp[y * dst_w as usize + x] = acc.clamp(0.0, 255.0).round() as u8;
        }
    }
    let mut dst = vec![0u8; dst_w as usize * dst_h as usize];
    for y in 0..dst_h as usize {
        for x in 0..dst_w as usize {
            let mut acc = 0.0f32;
            for (index, weight) in &v_taps[y] {
                acc += weight * f32::from(tmp[index * dst_w as usize + x]);
            }
            dst[y * dst_w as usize + x] = acc.clamp(0.0, 255.0).round() as u8;
        }
    }
    dst
}

/// Scales one packed plane with `channels` bytes per pixel,
/// resampling each channel independently with the same taps.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "accumulators are clamped to 0..=255 before the u8 cast; dimensions are bounded by the library limit, so f32 ratios are exact enough"
)]
fn scale_packed(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    channels: usize,
    filter: ScaleFilter,
) -> Vec<u8> {
    let scale_x = src_w as f32 / dst_w as f32;
    let scale_y = src_h as f32 / dst_h as f32;
    let mut tmp = vec![0u8; dst_w as usize * src_h as usize * channels];
    for y in 0..src_h as usize {
        for x in 0..dst_w as usize {
            for c in 0..channels {
                let mut acc = 0.0f32;
                for (index, weight) in taps(filter, x as u32, src_w, scale_x) {
                    acc += weight * f32::from(src[(y * src_w as usize + index) * channels + c]);
                }
                tmp[(y * dst_w as usize + x) * channels + c] = acc.clamp(0.0, 255.0).round() as u8;
            }
        }
    }
    let mut dst = vec![0u8; dst_w as usize * dst_h as usize * channels];
    for y in 0..dst_h as usize {
        for x in 0..dst_w as usize {
            for c in 0..channels {
                let mut acc = 0.0f32;
                for (index, weight) in taps(filter, y as u32, src_h, scale_y) {
                    acc += weight * f32::from(tmp[(index * dst_w as usize + x) * channels + c]);
                }
                dst[(y * dst_w as usize + x) * channels + c] = acc.clamp(0.0, 255.0).round() as u8;
            }
        }
    }
    dst
}

/// Trust boundary: a short plane must fail here, not index out
/// of bounds inside the resampling passes.
fn check_plane_lengths(frame: &VideoFrame) -> Result<()> {
    let plane_ok = match frame.format {
        PixelFormat::Yuv420 => {
            frame.planes.len() == 3
                && frame.planes[0].len() >= frame.width as usize * frame.height as usize
                && frame.planes[1].len()
                    >= frame.width.div_ceil(2) as usize * frame.height.div_ceil(2) as usize
                && frame.planes[2].len()
                    >= frame.width.div_ceil(2) as usize * frame.height.div_ceil(2) as usize
        }
        PixelFormat::Rgb24 | PixelFormat::Rgba32 => {
            frame.planes.len() == 1
                && frame.planes[0].len()
                    >= frame.width as usize * frame.height as usize * frame.format.bytes_per_pixel()
        }
    };
    if !plane_ok {
        return Err(Error::InvalidArgument(
            "frame planes are shorter than the declared shape",
        ));
    }
    Ok(())
}

/// Scales a video frame to `dst_width` x `dst_height` with the
/// given filter. YUV 4:2:0 planes are scaled at their own
/// resolutions (chroma at half width/height, rounded up); packed
/// formats are scaled per channel. The input frame is not
/// modified; colour space, range and timestamp are carried over.
///
/// # Errors
///
/// Returns `InvalidArgument` for a zero destination side, a
/// frame whose planes do not match its declared shape, or an
/// unsupported pixel format, and `Overflow` for a destination
/// side above the library limit.
pub fn scale_frame(
    frame: &VideoFrame,
    dst_width: u32,
    dst_height: u32,
    filter: ScaleFilter,
) -> Result<VideoFrame> {
    if dst_width == 0 || dst_height == 0 {
        return Err(Error::InvalidArgument("destination size has a zero side"));
    }
    if dst_width > MAX_WIDTH || dst_height > MAX_HEIGHT {
        return Err(Error::Overflow(
            "destination size exceeds the library limit",
        ));
    }
    if frame.width == 0 || frame.height == 0 {
        return Err(Error::InvalidArgument("source frame has a zero side"));
    }
    check_plane_lengths(frame)?;
    let (planes, strides) = match frame.format {
        PixelFormat::Yuv420 => {
            if frame.planes.len() != 3 {
                return Err(Error::InvalidArgument(
                    "yuv420 frame does not have three planes",
                ));
            }
            let src_chroma_w = frame.width.div_ceil(2);
            let src_chroma_h = frame.height.div_ceil(2);
            let dst_chroma_w = dst_width.div_ceil(2);
            let dst_chroma_h = dst_height.div_ceil(2);
            let y = scale_plane(
                &frame.planes[0],
                frame.width,
                frame.height,
                dst_width,
                dst_height,
                filter,
            );
            let u = scale_plane(
                &frame.planes[1],
                src_chroma_w,
                src_chroma_h,
                dst_chroma_w,
                dst_chroma_h,
                filter,
            );
            let v = scale_plane(
                &frame.planes[2],
                src_chroma_w,
                src_chroma_h,
                dst_chroma_w,
                dst_chroma_h,
                filter,
            );
            (
                vec![y, u, v],
                vec![
                    dst_width as usize,
                    dst_chroma_w as usize,
                    dst_chroma_w as usize,
                ],
            )
        }
        PixelFormat::Rgb24 | PixelFormat::Rgba32 => {
            if frame.planes.len() != 1 {
                return Err(Error::InvalidArgument(
                    "packed frame does not have one plane",
                ));
            }
            let channels = frame.format.bytes_per_pixel();
            let plane = scale_packed(
                &frame.planes[0],
                frame.width,
                frame.height,
                dst_width,
                dst_height,
                channels,
                filter,
            );
            (vec![plane], vec![dst_width as usize * channels])
        }
    };
    Ok(VideoFrame {
        width: dst_width,
        height: dst_height,
        format: frame.format,
        planes,
        strides,
        timestamp: frame.timestamp,
        colour_space: frame.colour_space,
        colour_range: frame.colour_range,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{ColourRange, ColourSpace};

    fn constant_frame(width: u32, height: u32, value: u8) -> VideoFrame {
        let size = width as usize * height as usize;
        VideoFrame {
            width,
            height,
            format: PixelFormat::Yuv420,
            planes: vec![
                vec![value; size],
                vec![value; size / 4],
                vec![value; size / 4],
            ],
            strides: vec![width as usize, width as usize / 2, width as usize / 2],
            timestamp: 0,
            colour_space: ColourSpace::default(),
            colour_range: ColourRange::default(),
        }
    }

    #[test]
    fn constant_frame_survives_every_filter() {
        for filter in [
            ScaleFilter::Bilinear,
            ScaleFilter::Bicubic,
            ScaleFilter::Lanczos3,
        ] {
            let frame = constant_frame(16, 16, 128);
            let scaled = scale_frame(&frame, 32, 32, filter).unwrap();
            assert!(scaled.planes[0].iter().all(|v| *v == 128));
            assert!(scaled.planes[1].iter().all(|v| *v == 128));
        }
    }

    #[test]
    fn identity_scale_reproduces_the_frame() {
        let frame = constant_frame(16, 16, 200);
        for filter in [
            ScaleFilter::Bilinear,
            ScaleFilter::Bicubic,
            ScaleFilter::Lanczos3,
        ] {
            let scaled = scale_frame(&frame, 16, 16, filter).unwrap();
            assert_eq!(scaled.planes, frame.planes);
        }
    }

    #[test]
    fn zero_destination_is_rejected() {
        let frame = constant_frame(16, 16, 128);
        assert!(matches!(
            scale_frame(&frame, 0, 16, ScaleFilter::Bilinear),
            Err(Error::InvalidArgument(_))
        ));
    }
}
