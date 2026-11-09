//! Minimal MP4 reader used by the AAC/ALAC integration tests.
//!
//! The crate's own MP4 demuxer rejects `mp4a`/`alac` sample
//! entries (that is the parallel passthrough worker's area), so
//! these tests read packets straight from the file: find the
//! codec config box (`alac` or `esds`), read the sample sizes
//! (`stsz`), chunk offsets (`stco`) and sample-to-chunk map
//! (`stsc`), and slice the samples out of the file bytes.
//! Nothing here is library code; it only exists so the tests can
//! feed real packets to `Decoder`.

// Dead code is expected here, not a bug: this module is a shared
// test helper included by several test targets, each using a
// different subset of its functions, and it is also compiled
// standalone as its own test target where nothing calls it at
// all. Linting each includer's subset separately would churn
// the helpers every time a test is added.
#![allow(dead_code)]

use std::fs;

/// One MP4 file, read whole, with its audio samples sliced out.
pub struct Mp4Samples {
    /// Whole file bytes, kept so packet slices stay alive.
    pub bytes: Vec<u8>,
    /// Byte ranges (start, end) of each audio sample, in order.
    pub sample_ranges: Vec<(usize, usize)>,
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes: [u8; 4] = data.get(offset..offset + 4)?.try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

/// Finds a box of the given type by recursive search, returning
/// the full box bytes (8-byte header included). The search
/// descends into container boxes only (`moov`, `trak`, `mdia`,
/// `minf`, `stbl`, `stsd` entries, `meta` with its 4-byte
/// version/flags skipped); `mdat` payload is never scanned.
#[must_use]
pub fn find_box(data: &[u8], wanted: [u8; 4]) -> Option<&[u8]> {
    find_box_in(data, 0, data.len(), wanted)
}

fn find_box_in(data: &[u8], start: usize, end: usize, wanted: [u8; 4]) -> Option<&[u8]> {
    let mut offset = start;
    while offset + 8 <= end {
        let size = read_u32(data, offset)? as usize;
        let ty: [u8; 4] = data.get(offset + 4..offset + 8)?.try_into().ok()?;
        if size < 8 || offset + size > end {
            return None;
        }
        if ty == wanted {
            return Some(&data[offset..offset + size]);
        }
        let body_start = offset + 8;
        let body_end = offset + size;
        match &ty {
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"udta" => {
                if let Some(found) = find_box_in(data, body_start, body_end, wanted) {
                    return Some(found);
                }
            }
            b"meta" => {
                if let Some(found) = find_box_in(data, body_start + 4, body_end, wanted) {
                    return Some(found);
                }
            }
            b"stsd" => {
                // version/flags (4) + entry count (4), then one
                // sample entry: 8-byte header + 28 bytes of
                // audio sample-entry fields, then child boxes.
                let entry = body_start + 8;
                if let Some(found) = find_box_in(data, entry + 8 + 28, body_end, wanted) {
                    return Some(found);
                }
            }
            _ => {}
        }
        offset += size;
    }
    None
}

/// Reads the sample table: `stsz` sizes, `stco` chunk offsets,
/// `stsc` sample-to-chunk entries. Returns the samples in order
/// as (start, end) byte ranges into the file.
#[must_use]
pub fn sample_ranges(data: &[u8]) -> Option<Vec<(usize, usize)>> {
    let stsz = find_box(data, *b"stsz")?;
    let sample_size = read_u32(stsz, 12)?;
    let count = read_u32(stsz, 16)? as usize;
    let mut sizes: Vec<u32> = Vec::with_capacity(count);
    if sample_size != 0 {
        sizes.resize(count, sample_size);
    } else {
        for i in 0..count {
            sizes.push(read_u32(stsz, 20 + 4 * i)?);
        }
    }
    let stco = find_box(data, *b"stco")?;
    let chunk_count = read_u32(stco, 12)? as usize;
    let mut chunk_offsets: Vec<u32> = Vec::with_capacity(chunk_count);
    for i in 0..chunk_count {
        chunk_offsets.push(read_u32(stco, 16 + 4 * i)?);
    }
    let stsc_box = find_box(data, *b"stsc")?;
    let entry_count = read_u32(stsc_box, 12)? as usize;
    let mut stsc_entries: Vec<(u32, u32)> = Vec::with_capacity(entry_count);
    for i in 0..entry_count {
        let first_chunk = read_u32(stsc_box, 16 + 12 * i)?;
        let samples_per_chunk = read_u32(stsc_box, 20 + 12 * i)?;
        stsc_entries.push((first_chunk, samples_per_chunk));
    }
    if stsc_entries.is_empty() || chunk_offsets.is_empty() {
        return None;
    }
    // Walk chunks in order; each chunk's entry is the last one
    // whose first_chunk is <= the chunk number.
    let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(count);
    let mut sample_index = 0usize;
    for (chunk_index, &chunk_offset) in chunk_offsets.iter().enumerate() {
        let chunk_no = u32::try_from(chunk_index + 1).ok()?;
        let mut samples_per_chunk = 0u32;
        for &(first_chunk, per_chunk) in &stsc_entries {
            if first_chunk <= chunk_no {
                samples_per_chunk = per_chunk;
            }
        }
        let mut offset = chunk_offset as usize;
        for _ in 0..samples_per_chunk {
            if sample_index >= count {
                break;
            }
            let size = sizes[sample_index] as usize;
            ranges.push((offset, offset + size));
            offset += size;
            sample_index += 1;
        }
    }
    if ranges.len() != count {
        return None;
    }
    Some(ranges)
}

/// Loads an MP4 file and slices its audio samples.
///
/// # Panics
///
/// Panics when the fixture file cannot be read or has no
/// readable sample table; fixtures are checked in, so either
/// means the checkout is broken, not the file.
#[must_use]
pub fn load_mp4(path: &str) -> Mp4Samples {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("cannot read fixture {path}: {err}"));
    let sample_ranges = sample_ranges(&bytes)
        .unwrap_or_else(|| panic!("cannot read sample table from fixture {path}"));
    Mp4Samples {
        bytes,
        sample_ranges,
    }
}

impl Mp4Samples {
    /// Bytes of sample `index`.
    ///
    /// # Panics
    ///
    /// Panics when `index` is out of range; callers iterate
    /// `0..len()`, so this only fires on a logic error.
    #[must_use]
    pub fn sample(&self, index: usize) -> &[u8] {
        let (start, end) = self.sample_ranges[index];
        &self.bytes[start..end]
    }

    /// Number of samples.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sample_ranges.len()
    }

    /// Whether the file has no audio samples.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sample_ranges.is_empty()
    }
}

/// Reads a little-endian i16 raw file into i32 samples.
///
/// # Panics
///
/// Panics when the reference file cannot be read or has a
/// length that is not a whole number of samples; references
/// are checked in, so either means the checkout is broken.
#[must_use]
pub fn read_s16le_raw(path: &str) -> Vec<i32> {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("cannot read reference {path}: {err}"));
    assert_eq!(bytes.len() % 2, 0, "reference {path} has an odd length");
    bytes
        .chunks_exact(2)
        .map(|chunk| i32::from(i16::from_le_bytes([chunk[0], chunk[1]])))
        .collect()
}

/// Reads a little-endian i32 raw file into i32 samples.
///
/// # Panics
///
/// Panics when the reference file cannot be read or has a
/// length that is not a whole number of samples; references
/// are checked in, so either means the checkout is broken.
#[must_use]
pub fn read_s32le_raw(path: &str) -> Vec<i32> {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("cannot read reference {path}: {err}"));
    assert_eq!(bytes.len() % 4, 0, "reference {path} has a bad length");
    bytes
        .chunks_exact(4)
        .map(|chunk| i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Reads a little-endian f32 raw file into f32 samples.
///
/// # Panics
///
/// Panics when the reference file cannot be read or has a
/// length that is not a whole number of samples; references
/// are checked in, so either means the checkout is broken.
#[must_use]
pub fn read_f32le_raw(path: &str) -> Vec<f32> {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("cannot read reference {path}: {err}"));
    assert_eq!(bytes.len() % 4, 0, "reference {path} has a bad length");
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}
