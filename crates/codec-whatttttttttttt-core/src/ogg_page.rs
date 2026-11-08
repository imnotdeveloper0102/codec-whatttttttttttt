//! Ogg page parsing and writing (RFC 3533).
//!
//! An Ogg page is a 27-byte header, a segment table of up to 255
//! entries, and the page data. Segment sizes sum to the data length;
//! a segment of 255 bytes means the packet continues in the next
//! segment, a shorter segment ends the packet. Pages are protected by
//! a CRC-32 with polynomial 0x04C11DB7, initial value 0, no reflection
//! and no final XOR, computed over the whole page with the CRC field
//! zeroed. A page whose CRC does not match is `InvalidData`, never
//! skipped silently.
//!
//! The largest possible page is 27 header + 255 segment-table bytes +
//! 255 segments of 255 bytes = 65307 bytes.

use crate::error::{Error, Result};

/// Ogg capture pattern at the start of every page.
pub const OGG_CAPTURE: [u8; 4] = *b"OggS";

/// Bytes in an Ogg page header before the segment table.
pub const OGG_PAGE_HEADER_BYTES: usize = 27;

/// Largest number of segments in one page.
pub const OGG_MAX_SEGMENTS: usize = 255;

/// Largest number of bytes in one page.
pub const OGG_MAX_PAGE_BYTES: usize = OGG_PAGE_HEADER_BYTES + OGG_MAX_SEGMENTS + 255 * 255;

/// Header type flag: this page continues a packet from the previous page.
pub const OGG_HEADER_CONTINUED: u8 = 0x01;
/// Header type flag: first page of a logical stream.
pub const OGG_HEADER_BOS: u8 = 0x02;
/// Header type flag: last page of a logical stream.
pub const OGG_HEADER_EOS: u8 = 0x04;

/// CRC-32 used by Ogg: polynomial 0x04C11DB7, MSB-first, no
/// reflection, initial value 0. Computed incrementally so callers can
/// skip the CRC field without copying the page.
#[must_use]
pub fn ogg_crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC-32 of `data` as a single chunk.
#[must_use]
pub fn ogg_crc32(data: &[u8]) -> u32 {
    ogg_crc32_update(0, data)
}

/// One parsed Ogg page (RFC 3533 section 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OggPage {
    /// Header type flags (`OGG_HEADER_*`).
    pub header_type: u8,
    /// Granule position: the sample number of the last sample in the
    /// last completed packet on this page, or -1 when no packet
    /// completes on this page.
    pub granule_position: i64,
    /// Serial number of the logical stream this page belongs to.
    pub serial: u32,
    /// Page sequence number within its logical stream.
    pub sequence: u32,
    /// Segment sizes, in order. Each entry is 0..=255; 255 continues.
    pub segments: Vec<u8>,
    /// Page payload: the concatenation of all segments.
    pub data: Vec<u8>,
}

impl OggPage {
    /// True when this page starts a logical stream.
    #[must_use]
    pub fn is_bos(&self) -> bool {
        self.header_type & OGG_HEADER_BOS != 0
    }

    /// True when this page ends a logical stream.
    #[must_use]
    pub fn is_eos(&self) -> bool {
        self.header_type & OGG_HEADER_EOS != 0
    }

    /// True when this page continues a packet from the previous page.
    #[must_use]
    pub fn is_continued(&self) -> bool {
        self.header_type & OGG_HEADER_CONTINUED != 0
    }

    /// Total encoded length of the page, header included.
    #[must_use]
    pub fn total_len(&self) -> usize {
        OGG_PAGE_HEADER_BYTES + self.segments.len() + self.data.len()
    }
}

/// Parses one Ogg page at the start of `data`. Returns the page and
/// the number of bytes it occupies.
///
/// # Errors
///
/// Returns `Truncated` for a cut-short header, segment table, or page
/// data, and `InvalidData` for a bad capture pattern, a version other
/// than 0, or a CRC mismatch.
pub fn parse_ogg_page(data: &[u8]) -> Result<(OggPage, usize)> {
    if data.len() < OGG_PAGE_HEADER_BYTES {
        return Err(Error::Truncated {
            needed: OGG_PAGE_HEADER_BYTES,
            had: data.len(),
        });
    }
    if data[0..4] != OGG_CAPTURE {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "missing OggS capture pattern",
        });
    }
    if data[4] != 0 {
        return Err(Error::InvalidData {
            offset: 4,
            detail: "ogg version must be 0",
        });
    }
    let header_type = data[5];
    let granule_position = i64::from_le_bytes([
        data[6], data[7], data[8], data[9], data[10], data[11], data[12], data[13],
    ]);
    let serial = u32::from_le_bytes([data[14], data[15], data[16], data[17]]);
    let sequence = u32::from_le_bytes([data[18], data[19], data[20], data[21]]);
    let stored_crc = u32::from_le_bytes([data[22], data[23], data[24], data[25]]);
    let segment_count = usize::from(data[26]);
    let table_end = OGG_PAGE_HEADER_BYTES + segment_count;
    let table = data
        .get(OGG_PAGE_HEADER_BYTES..table_end)
        .ok_or(Error::Truncated {
            needed: table_end,
            had: data.len(),
        })?;
    let data_len: usize = table.iter().map(|&size| usize::from(size)).sum();
    let page_end = table_end + data_len;
    let page_data = data.get(table_end..page_end).ok_or(Error::Truncated {
        needed: page_end,
        had: data.len(),
    })?;
    // The CRC covers the whole page with the CRC field zeroed, so it is
    // computed in two parts around bytes 22..=25.
    let mut crc = ogg_crc32_update(0, &data[..22]);
    crc = ogg_crc32_update(crc, &[0, 0, 0, 0]);
    crc = ogg_crc32_update(crc, &data[26..page_end]);
    if crc != stored_crc {
        return Err(Error::InvalidData {
            offset: 22,
            detail: "ogg page crc mismatch",
        });
    }
    Ok((
        OggPage {
            header_type,
            granule_position,
            serial,
            sequence,
            segments: table.to_vec(),
            data: page_data.to_vec(),
        },
        page_end,
    ))
}

/// Serializes a page, computing its CRC.
///
/// # Errors
///
/// Returns `InvalidData` when the segment table has more than 255
/// entries or when `page.data.len()` does not equal the sum of
/// `page.segments`.
pub fn write_ogg_page(page: &OggPage) -> Result<Vec<u8>> {
    if page.segments.len() > OGG_MAX_SEGMENTS {
        return Err(Error::InvalidData {
            offset: 26,
            detail: "ogg segment count exceeds 255",
        });
    }
    let data_len: usize = page.segments.iter().map(|&size| usize::from(size)).sum();
    if data_len != page.data.len() {
        return Err(Error::InvalidData {
            offset: 26,
            detail: "ogg segment table does not match page data length",
        });
    }
    let mut out = Vec::with_capacity(page.total_len());
    out.extend_from_slice(&OGG_CAPTURE);
    out.push(0);
    out.push(page.header_type);
    out.extend_from_slice(&page.granule_position.to_le_bytes());
    out.extend_from_slice(&page.serial.to_le_bytes());
    out.extend_from_slice(&page.sequence.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.push(
        u8::try_from(page.segments.len()).map_err(|_| Error::InvalidData {
            offset: 26,
            detail: "ogg segment count exceeds 255",
        })?,
    );
    out.extend_from_slice(&page.segments);
    out.extend_from_slice(&page.data);
    let crc = ogg_crc32(&out);
    out[22..26].copy_from_slice(&crc.to_le_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_reference_implementation() {
        // Value produced by an independent Python implementation of
        // the same MSB-first algorithm. The independent check against
        // real files is tests/ogg.rs, which parses the ffmpeg-produced
        // Ogg file and verifies its stored CRCs.
        assert_eq!(ogg_crc32(b"123456789"), 0x89A1_897F);
    }

    fn minimal_page() -> OggPage {
        OggPage {
            header_type: OGG_HEADER_BOS,
            granule_position: 0,
            serial: 1,
            sequence: 0,
            segments: vec![3],
            data: b"abc".to_vec(),
        }
    }

    #[test]
    fn write_then_parse_round_trips() {
        let page = minimal_page();
        let bytes = write_ogg_page(&page).unwrap();
        let (parsed, consumed) = parse_ogg_page(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(parsed, page);
    }

    #[test]
    fn write_rejects_too_many_segments() {
        let mut page = minimal_page();
        page.segments = vec![0; OGG_MAX_SEGMENTS + 1];
        page.data = Vec::new();
        let err = write_ogg_page(&page).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn write_rejects_segment_data_mismatch() {
        let mut page = minimal_page();
        page.segments = vec![4];
        page.data = b"abc".to_vec();
        let err = write_ogg_page(&page).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn bad_capture_returns_invalid_data() {
        let mut bytes = write_ogg_page(&minimal_page()).unwrap();
        bytes[0] = b'X';
        let err = parse_ogg_page(&bytes).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn bad_version_returns_invalid_data() {
        let mut bytes = write_ogg_page(&minimal_page()).unwrap();
        bytes[4] = 1;
        // Fix the CRC so the version check, not the CRC check, fires.
        let crc = ogg_crc32_update(0, &bytes[..22]);
        let crc = ogg_crc32_update(crc, &[0, 0, 0, 0]);
        let crc = ogg_crc32_update(crc, &bytes[26..]);
        bytes[22..26].copy_from_slice(&crc.to_le_bytes());
        let err = parse_ogg_page(&bytes).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn corrupt_data_returns_invalid_data_for_crc() {
        let mut bytes = write_ogg_page(&minimal_page()).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        let err = parse_ogg_page(&bytes).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn truncated_page_returns_truncated() {
        let bytes = write_ogg_page(&minimal_page()).unwrap();
        let err = parse_ogg_page(&bytes[..10]).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn truncated_segment_table_returns_truncated() {
        let bytes = write_ogg_page(&minimal_page()).unwrap();
        // Header claims one segment but the table is cut off.
        let err = parse_ogg_page(&bytes[..27]).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }
}
