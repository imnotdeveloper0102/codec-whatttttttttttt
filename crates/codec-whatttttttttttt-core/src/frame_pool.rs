//! Reusable pool of decoded video frame buffers.
//!
//! A decoder that allocates a fresh `VideoFrame` per frame pays
//! the allocator on every frame. The pool keeps checked-in frames
//! and hands the same backing storage out again when a later
//! checkout asks for the same dimensions, format and bit depth,
//! so steady-state decoding allocates nothing.
//!
//! Conflict with the per-decode-call allocation limit, stated
//! plainly rather than worked around: `limits.rs` caps one decode
//! call at `MAX_ALLOCATION_BYTES` (64 MiB). A single 8K YUV 4:2:0
//! frame at 8 bit is 7680 * 4320 * 3 / 2 = 49,766,400 bytes
//! (~47.5 MiB) and fits; the same frame at 10 bit stores two
//! bytes per sample, so it is 99,532,800 bytes (~94.9 MiB) and
//! exceeds the 64 MiB cap. The pool enforces the cap on every
//! new allocation and returns `AllocationLimit` for a 10-bit 8K
//! frame instead of silently raising the limit; `limits.rs` was
//! deliberately not changed here. A caller that needs 10-bit 8K
//! must raise the library limit first, in `limits.rs`, as a
//! separate decision.

use crate::error::{Error, Result};
use crate::frame::{ColourRange, ColourSpace, PixelFormat, VideoFrame};
use crate::limits::MAX_ALLOCATION_BYTES;

/// Bytes per stored sample for a bit depth: 8 bit stores one byte
/// per sample, 10 and 12 bit store one `u16` per sample.
///
/// # Errors
///
/// Returns `InvalidArgument` for a bit depth other than 8, 10 or 12.
pub fn bytes_per_sample(bit_depth: u8) -> Result<usize> {
    match bit_depth {
        8 => Ok(1),
        10 | 12 => Ok(2),
        _ => Err(Error::InvalidArgument("bit depth is not 8, 10 or 12")),
    }
}

/// Plane byte sizes for a frame, in plane order. YUV 4:2:0
/// chroma planes are half width and half height, rounded up, so
/// odd dimensions still account for their chroma samples.
///
/// # Errors
///
/// Returns `InvalidArgument` for an unsupported bit depth or a
/// zero dimension, and `Overflow` when a plane size does not fit
/// in `usize`.
pub fn plane_bytes(
    width: u32,
    height: u32,
    format: PixelFormat,
    bit_depth: u8,
) -> Result<Vec<usize>> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidArgument("frame size has a zero side"));
    }
    let sample = bytes_per_sample(bit_depth)?;
    let pixels = |w: u32, h: u32| -> Result<usize> {
        usize::try_from(w)
            .and_then(|w| usize::try_from(h).map(|h| (w, h)))
            .map_err(|_| Error::Overflow("frame dimension exceeds usize"))
            .and_then(|(w, h)| {
                w.checked_mul(h)
                    .and_then(|p| p.checked_mul(sample))
                    .ok_or(Error::Overflow("plane size overflows usize"))
            })
    };
    match format {
        PixelFormat::Yuv420 => {
            let chroma_w = width.div_ceil(2);
            let chroma_h = height.div_ceil(2);
            Ok(vec![
                pixels(width, height)?,
                pixels(chroma_w, chroma_h)?,
                pixels(chroma_w, chroma_h)?,
            ])
        }
        PixelFormat::Rgb24 => Ok(vec![pixels(width, height)?
            .checked_mul(3)
            .ok_or(Error::Overflow("plane size overflows usize"))?]),
        PixelFormat::Rgba32 => Ok(vec![pixels(width, height)?
            .checked_mul(4)
            .ok_or(Error::Overflow("plane size overflows usize"))?]),
    }
}

/// Total bytes of one frame: the sum of its plane sizes.
/// YUV 4:2:0 at 8 bit is `width * height * 3 / 2` bytes; at
/// 10/12 bit it is twice that, because each sample is stored as
/// a `u16`.
///
/// # Errors
///
/// Propagates `plane_bytes` errors.
pub fn frame_bytes(width: u32, height: u32, format: PixelFormat, bit_depth: u8) -> Result<usize> {
    let mut total = 0usize;
    for plane in plane_bytes(width, height, format, bit_depth)? {
        total = total
            .checked_add(plane)
            .ok_or(Error::Overflow("frame size overflows usize"))?;
    }
    Ok(total)
}

/// Builds an empty `VideoFrame` of the given shape, with
/// zero-filled planes of the exact sizes from `plane_bytes`.
fn new_frame(width: u32, height: u32, format: PixelFormat, bit_depth: u8) -> Result<VideoFrame> {
    let sizes = plane_bytes(width, height, format, bit_depth)?;
    let planes: Vec<Vec<u8>> = sizes.iter().map(|size| vec![0u8; *size]).collect();
    let strides: Vec<usize> = match format {
        PixelFormat::Yuv420 => vec![
            usize::try_from(width).map_err(|_| Error::Overflow("stride exceeds usize"))?,
            usize::try_from(width.div_ceil(2))
                .map_err(|_| Error::Overflow("stride exceeds usize"))?,
            usize::try_from(width.div_ceil(2))
                .map_err(|_| Error::Overflow("stride exceeds usize"))?,
        ],
        PixelFormat::Rgb24 => vec![usize::try_from(width)
            .map_err(|_| Error::Overflow("stride exceeds usize"))?
            .checked_mul(3)
            .ok_or(Error::Overflow("stride exceeds usize"))?],
        PixelFormat::Rgba32 => vec![usize::try_from(width)
            .map_err(|_| Error::Overflow("stride exceeds usize"))?
            .checked_mul(4)
            .ok_or(Error::Overflow("stride exceeds usize"))?],
    };
    Ok(VideoFrame {
        width,
        height,
        format,
        planes,
        strides,
        timestamp: 0,
        colour_space: ColourSpace::default(),
        colour_range: ColourRange::default(),
    })
}

/// Byte size of a frame that already exists, from its planes.
fn owned_bytes(frame: &VideoFrame) -> usize {
    frame.planes.iter().map(Vec::len).sum()
}

/// Whether a pooled frame matches a checkout request: same
/// dimensions, format and plane sizes (plane sizes encode the
/// bit depth, since 10/12-bit planes are twice the 8-bit size).
fn matches(
    frame: &VideoFrame,
    width: u32,
    height: u32,
    format: PixelFormat,
    bit_depth: u8,
) -> bool {
    if frame.width != width || frame.height != height || frame.format != format {
        return false;
    }
    match plane_bytes(width, height, format, bit_depth) {
        Ok(sizes) => {
            frame.planes.len() == sizes.len()
                && frame
                    .planes
                    .iter()
                    .zip(sizes.iter())
                    .all(|(plane, size)| plane.len() == *size)
        }
        Err(_) => false,
    }
}

/// Pool statistics, for tests and for callers that tune the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FramePoolStats {
    /// Frames currently idle in the pool.
    pub idle_frames: usize,
    /// Bytes owned by the pool (idle plus checked-out frames).
    pub allocated_bytes: usize,
    /// Frames ever allocated fresh (not reused).
    pub allocations: u64,
    /// Checkouts served from an idle frame.
    pub reuses: u64,
}

/// A pool of reusable `VideoFrame` buffers.
///
/// The pool owns every frame it ever allocated until the pool is
/// dropped. `checkout` hands out an idle frame of the requested
/// shape or allocates a new one; `checkin` returns a frame to
/// the idle set. A frame the pool cannot keep (pool full, or
/// over the byte cap) is dropped on checkin instead of leaking.
#[derive(Debug)]
pub struct FramePool {
    idle: Vec<VideoFrame>,
    max_frames: usize,
    max_total_bytes: usize,
    allocated_bytes: usize,
    allocations: u64,
    reuses: u64,
}

impl FramePool {
    /// Creates a pool holding at most `max_frames` frames, with a
    /// total byte cap of `max_frames` times the per-decode-call
    /// allocation limit. A pool of zero frames never keeps
    /// anything; checkout still works but always allocates.
    #[must_use]
    pub fn new(max_frames: usize) -> Self {
        Self {
            idle: Vec::new(),
            max_frames,
            max_total_bytes: max_frames.saturating_mul(MAX_ALLOCATION_BYTES),
            allocated_bytes: 0,
            allocations: 0,
            reuses: 0,
        }
    }

    /// Creates a pool with an explicit total byte cap. The
    /// per-frame cap from `limits.rs` still applies to every new
    /// allocation; this cap only bounds the pool as a whole.
    #[must_use]
    pub fn with_total_bytes(max_frames: usize, max_total_bytes: usize) -> Self {
        Self {
            idle: Vec::new(),
            max_frames,
            max_total_bytes,
            allocated_bytes: 0,
            allocations: 0,
            reuses: 0,
        }
    }

    /// Checks out a frame of the requested shape. An idle frame
    /// of the same shape is reused (its old pixel data is left
    /// in place; the caller overwrites what it needs). Otherwise
    /// a fresh frame is allocated, subject to both caps.
    ///
    /// # Errors
    ///
    /// Returns `AllocationLimit` when one frame exceeds
    /// `MAX_ALLOCATION_BYTES` (for example 10-bit 8K) or when
    /// the pool's total byte cap would be exceeded, and
    /// propagates `plane_bytes` errors for bad arguments.
    pub fn checkout(
        &mut self,
        width: u32,
        height: u32,
        format: PixelFormat,
        bit_depth: u8,
    ) -> Result<VideoFrame> {
        if let Some(index) = self
            .idle
            .iter()
            .position(|frame| matches(frame, width, height, format, bit_depth))
        {
            self.reuses += 1;
            return Ok(self.idle.swap_remove(index));
        }
        let bytes = frame_bytes(width, height, format, bit_depth)?;
        if bytes > MAX_ALLOCATION_BYTES {
            return Err(Error::AllocationLimit {
                requested: bytes,
                limit: MAX_ALLOCATION_BYTES,
            });
        }
        let Some(total) = self.allocated_bytes.checked_add(bytes) else {
            return Err(Error::AllocationLimit {
                requested: bytes,
                limit: self.max_total_bytes,
            });
        };
        if total > self.max_total_bytes {
            return Err(Error::AllocationLimit {
                requested: bytes,
                limit: self.max_total_bytes,
            });
        }
        let frame = new_frame(width, height, format, bit_depth)?;
        self.allocated_bytes = total;
        self.allocations += 1;
        Ok(frame)
    }

    /// Returns a frame to the pool. The frame must be one this
    /// pool handed out: its bytes are already in the pool
    /// accounting and stay there while the frame is idle. A frame
    /// the pool cannot keep (pool full) is dropped and its bytes
    /// leave the pool accounting.
    pub fn checkin(&mut self, frame: VideoFrame) {
        if self.idle.len() >= self.max_frames {
            let bytes = owned_bytes(&frame);
            self.allocated_bytes = self.allocated_bytes.saturating_sub(bytes);
            return;
        }
        self.idle.push(frame);
    }

    /// Current pool statistics.
    #[must_use]
    pub fn stats(&self) -> FramePoolStats {
        FramePoolStats {
            idle_frames: self.idle.len(),
            allocated_bytes: self.allocated_bytes,
            allocations: self.allocations,
            reuses: self.reuses,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_bytes_8k_yuv420_8bit_is_49_766_400() {
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
    fn pool_rejects_10bit_8k_under_the_64mib_cap() {
        let mut pool = FramePool::new(2);
        let err = pool
            .checkout(7680, 4320, PixelFormat::Yuv420, 10)
            .unwrap_err();
        assert!(matches!(err, Error::AllocationLimit { .. }));
    }

    #[test]
    fn pool_reuses_a_checked_in_frame() {
        let mut pool = FramePool::new(2);
        let frame = pool.checkout(64, 64, PixelFormat::Yuv420, 8).unwrap();
        let ptr = frame.planes[0].as_ptr();
        pool.checkin(frame);
        let again = pool.checkout(64, 64, PixelFormat::Yuv420, 8).unwrap();
        assert_eq!(again.planes[0].as_ptr(), ptr);
        assert_eq!(pool.stats().reuses, 1);
        assert_eq!(pool.stats().allocations, 1);
    }
}
