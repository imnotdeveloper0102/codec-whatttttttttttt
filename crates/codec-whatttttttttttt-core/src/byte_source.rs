//! Bounded reading from a byte source.
//!
//! Parsers must never allocate from a size declared by untrusted input.
//! `BoundedReader` reads in fixed 64 KiB chunks and refuses to accumulate
//! more than the caller-supplied maximum, returning `AllocationLimit`
//! instead of growing without bound.

use std::io::Read;

use crate::error::{Error, Result};
use crate::limits::{MAX_WAV_FILE_BYTES, READ_CHUNK_BYTES};

/// A `Read` adapter that caps total buffered bytes at `max_bytes`.
pub struct BoundedReader<R: Read> {
    inner: R,
    max_bytes: usize,
    total_read: usize,
}

impl<R: Read> std::fmt::Debug for BoundedReader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundedReader")
            .field("max_bytes", &self.max_bytes)
            .field("total_read", &self.total_read)
            .finish_non_exhaustive()
    }
}

impl<R: Read> BoundedReader<R> {
    /// Creates a reader that refuses to read more than `max_bytes` in total.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when `max_bytes` is zero.
    pub fn new(inner: R, max_bytes: usize) -> Result<Self> {
        if max_bytes == 0 {
            return Err(Error::InvalidArgument(
                "bounded reader max_bytes must be positive",
            ));
        }
        Ok(Self {
            inner,
            max_bytes,
            total_read: 0,
        })
    }

    /// Reads all remaining bytes, in `READ_CHUNK_BYTES` chunks.
    ///
    /// # Errors
    ///
    /// Returns `AllocationLimit` when the source holds more than
    /// `max_bytes`, and `Io` on an underlying read failure.
    pub fn read_all_bounded(&mut self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        // Heap-allocated: a 64 KiB stack buffer trips clippy::large_stack_arrays.
        let mut chunk = vec![0u8; READ_CHUNK_BYTES];
        loop {
            let n = self.inner.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            self.total_read += n;
            if self.total_read > self.max_bytes {
                return Err(Error::AllocationLimit {
                    requested: self.total_read,
                    limit: self.max_bytes,
                });
            }
            out.extend_from_slice(&chunk[..n]);
        }
        Ok(out)
    }
}

/// Reads at most `MAX_WAV_FILE_BYTES` from `reader` into memory.
/// Used by the Phase 1 WAV demuxer; streaming demux in a later phase
/// removes the whole-file bound.
///
/// # Errors
///
/// Returns `AllocationLimit` when the source exceeds the bound, and
/// `Io` on an underlying read failure.
pub fn read_wav_bytes<R: Read>(reader: R) -> Result<Vec<u8>> {
    BoundedReader::new(reader, MAX_WAV_FILE_BYTES)?.read_all_bounded()
}

/// Reads at most `MAX_FLAC_FILE_BYTES` from `reader` into memory.
/// Used by the Phase 2 FLAC demuxer.
///
/// # Errors
///
/// Returns `AllocationLimit` when the source exceeds the bound, and
/// `Io` on an underlying read failure.
pub fn read_flac_bytes<R: Read>(reader: R) -> Result<Vec<u8>> {
    BoundedReader::new(reader, crate::limits::MAX_FLAC_FILE_BYTES)?.read_all_bounded()
}

/// Reads at most `MAX_OGG_FILE_BYTES` from `reader` into memory.
/// Used by the Phase 3 Ogg demuxer.
///
/// # Errors
///
/// Returns `AllocationLimit` when the source exceeds the bound, and
/// `Io` on an underlying read failure.
pub fn read_ogg_bytes<R: Read>(reader: R) -> Result<Vec<u8>> {
    BoundedReader::new(reader, crate::limits::MAX_OGG_FILE_BYTES)?.read_all_bounded()
}

/// Reads at most `MAX_WEBM_FILE_BYTES` from `reader` into memory.
/// Used by the Phase 6 Matroska demuxer.
///
/// # Errors
///
/// Returns `AllocationLimit` when the source exceeds the bound, and
/// `Io` on an underlying read failure.
pub fn read_webm_bytes<R: Read>(reader: R) -> Result<Vec<u8>> {
    BoundedReader::new(reader, crate::limits::MAX_WEBM_FILE_BYTES)?.read_all_bounded()
}

/// Reads at most `MAX_MP4_FILE_BYTES` from `reader` into memory.
/// Used by the Phase 6 MP4 demuxer.
///
/// # Errors
///
/// Returns `AllocationLimit` when the source exceeds the bound, and
/// `Io` on an underlying read failure.
pub fn read_mp4_bytes<R: Read>(reader: R) -> Result<Vec<u8>> {
    BoundedReader::new(reader, crate::limits::MAX_MP4_FILE_BYTES)?.read_all_bounded()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_short_input_fully() {
        let mut reader = BoundedReader::new(Cursor::new(b"hello"), 16).unwrap();
        assert_eq!(reader.read_all_bounded().unwrap(), b"hello");
    }

    #[test]
    fn refuses_input_over_max() {
        let mut reader = BoundedReader::new(Cursor::new(vec![0u8; 100]), 16).unwrap();
        let err = reader.read_all_bounded().unwrap_err();
        assert!(matches!(err, Error::AllocationLimit { .. }));
    }

    #[test]
    fn zero_max_is_invalid_argument() {
        let err = BoundedReader::new(Cursor::new(b""), 0).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }
}
