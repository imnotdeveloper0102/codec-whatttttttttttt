//! EBML variable-length integers and element headers (RFC 8794,
//! Matroska specification).
//!
//! Every value read here is bounded by the parent element: an element
//! size that would run past its parent is `InvalidData`, not a longer
//! read. A header cut short by the end of the buffer is `Truncated`.
//! Traversal is iterative (a cursor over a byte range), never
//! recursive, so a crafted nesting depth cannot exhaust the stack;
//! callers enforce `MAX_NESTING_DEPTH` when they descend.

use crate::error::{Error, Result};

/// Largest EBML variable-length integer width, in bytes (RFC 8794
/// section 4.1: a vint is 1 to 8 octets).
pub const MAX_VINT_BYTES: usize = 8;

/// Largest EBML element ID width, in bytes (RFC 8794 section 4.2:
/// an ID is 1 to 4 octets).
pub const MAX_ID_BYTES: usize = 4;

/// Reads an unsigned EBML variable-length integer at `offset`.
/// Returns the value and the number of bytes consumed. The width is
/// the count of leading zero bits in the first octet plus one; the
/// marker bit itself is not part of the value (RFC 8794 section 4.1).
///
/// # Errors
///
/// Returns `Truncated` when the buffer ends inside the integer, and
/// `InvalidData` when the first octet is zero (a width above 8).
pub fn read_vint(data: &[u8], offset: usize) -> Result<(u64, usize)> {
    let first = *data.get(offset).ok_or(Error::Truncated {
        needed: offset + 1,
        had: data.len(),
    })?;
    if first == 0 {
        return Err(Error::InvalidData {
            offset,
            detail: "ebml vint starts with a zero octet",
        });
    }
    let width = first.leading_zeros() as usize + 1;
    if width > MAX_VINT_BYTES {
        return Err(Error::InvalidData {
            offset,
            detail: "ebml vint wider than 8 octets",
        });
    }
    if data.len() < offset + width {
        return Err(Error::Truncated {
            needed: offset + width,
            had: data.len(),
        });
    }
    // The marker bit is the highest set bit of the first octet;
    // everything below it, plus the following octets, is the value.
    let marker = 0x80u8 >> (width - 1);
    let mut value = u64::from(first & (marker - 1));
    for &byte in &data[offset + 1..offset + width] {
        value = (value << 8) | u64::from(byte);
    }
    Ok((value, width))
}

/// Reads an EBML element size at `offset`. Returns `None` for the
/// unknown-size value (all value bits set, RFC 8794 section 4.1.2),
/// which means the element runs to the end of its parent.
///
/// # Errors
///
/// The same errors as `read_vint`.
pub fn read_vint_unknown(data: &[u8], offset: usize) -> Result<(Option<u64>, usize)> {
    let (value, width) = read_vint(data, offset)?;
    // All value bits set: 7 bits per octet.
    let all_ones = if width >= 8 {
        // 7*8 = 56 value bits; shifting by 56 is safe in u64.
        (1u64 << 56) - 1
    } else {
        (1u64 << (7 * width)) - 1
    };
    if value == all_ones {
        Ok((None, width))
    } else {
        Ok((Some(value), width))
    }
}

/// Reads a signed EBML variable-length integer at `offset` (RFC 8794
/// section 4.1.3): the unsigned value minus `(2^(7*width-1) - 1)`.
/// Used by EBML lacing, where a lace size is stored as a difference
/// from the previous lace.
///
/// # Errors
///
/// The same errors as `read_vint`.
pub fn read_signed_vint(data: &[u8], offset: usize) -> Result<(i64, usize)> {
    let (value, width) = read_vint(data, offset)?;
    let bias = (1i64 << (7 * width - 1)) - 1;
    let signed = i64::try_from(value)
        .map_err(|_| Error::Overflow("ebml signed vint value exceeds i64 range"))?;
    Ok((signed - bias, width))
}

/// Reads an EBML element ID at `offset`. Unlike a vint, the marker
/// bits are part of the ID (RFC 8794 section 4.2), so the ID is the
/// raw big-endian value of its octets. Returns the ID and the number
/// of bytes consumed.
///
/// # Errors
///
/// Returns `Truncated` when the buffer ends inside the ID, and
/// `InvalidData` when the first octet is zero (a width above 4).
pub fn read_element_id(data: &[u8], offset: usize) -> Result<(u32, usize)> {
    let first = *data.get(offset).ok_or(Error::Truncated {
        needed: offset + 1,
        had: data.len(),
    })?;
    if first == 0 {
        return Err(Error::InvalidData {
            offset,
            detail: "ebml element id starts with a zero octet",
        });
    }
    let width = first.leading_zeros() as usize + 1;
    if width > MAX_ID_BYTES {
        return Err(Error::InvalidData {
            offset,
            detail: "ebml element id wider than 4 octets",
        });
    }
    if data.len() < offset + width {
        return Err(Error::Truncated {
            needed: offset + width,
            had: data.len(),
        });
    }
    let mut id: u32 = 0;
    for &byte in &data[offset..offset + width] {
        id = (id << 8) | u32::from(byte);
    }
    Ok((id, width))
}

/// Header of one EBML element: its ID, its size (`None` for the
/// unknown-size value, meaning it runs to the end of its parent),
/// and the byte ranges of the header and the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EbmlElementHeader {
    /// Element ID (marker bits included).
    pub id: u32,
    /// Payload size in bytes, or `None` for unknown size.
    pub size: Option<u64>,
    /// Offset of the first header byte.
    pub offset: usize,
    /// Offset of the first payload byte.
    pub data_start: usize,
    /// Offset one past the last payload byte (the parent end for
    /// unknown size).
    pub data_end: usize,
}

impl EbmlElementHeader {
    /// Payload length in bytes. Unknown size is reported against the
    /// parent end, which is where `data_end` already points.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        self.data_end.saturating_sub(self.data_start)
    }

    /// Total element length in bytes, header included.
    #[must_use]
    pub fn total_len(&self) -> usize {
        self.data_end.saturating_sub(self.offset)
    }
}

/// Parses one element header at `offset`, bounded by `parent_end`
/// (the offset one past the last byte of the parent element). The
/// payload must end at or before `parent_end`; a size that would run
/// past the parent is `InvalidData` (RFC 8794: a size larger than the
/// remaining parent is malformed, not a longer element).
///
/// # Errors
///
/// Returns `Truncated` when the buffer ends inside the header, and
/// `InvalidData` when the ID is malformed or the size overruns the
/// parent.
pub fn parse_element_header(
    data: &[u8],
    offset: usize,
    parent_end: usize,
) -> Result<EbmlElementHeader> {
    if offset >= parent_end {
        return Err(Error::Truncated {
            needed: offset + 1,
            had: data.len(),
        });
    }
    let (id, id_len) = read_element_id(data, offset)?;
    let (size, size_len) = read_vint_unknown(data, offset + id_len)?;
    let data_start = offset + id_len + size_len;
    if data_start > parent_end {
        return Err(Error::Truncated {
            needed: data_start,
            had: data.len(),
        });
    }
    let data_end = match size {
        Some(size) => {
            let end = data_start.saturating_add(usize::try_from(size).unwrap_or(usize::MAX));
            if end > parent_end {
                return Err(Error::InvalidData {
                    offset,
                    detail: "ebml element size exceeds parent element",
                });
            }
            end
        }
        None => parent_end,
    };
    Ok(EbmlElementHeader {
        id,
        size,
        offset,
        data_start,
        data_end,
    })
}

/// Reads a big-endian unsigned integer of `len` bytes (0..=8) at
/// `offset`. A zero length reads as 0, which is how Matroska encodes
/// a missing value.
///
/// # Errors
///
/// Returns `Truncated` when the buffer ends inside the integer, and
/// `InvalidData` when `len` is above 8.
pub fn read_uint(data: &[u8], offset: usize, len: usize) -> Result<u64> {
    if len > 8 {
        return Err(Error::InvalidData {
            offset,
            detail: "ebml unsigned integer longer than 8 octets",
        });
    }
    if data.len() < offset + len {
        return Err(Error::Truncated {
            needed: offset + len,
            had: data.len(),
        });
    }
    let mut value: u64 = 0;
    for &byte in &data[offset..offset + len] {
        value = (value << 8) | u64::from(byte);
    }
    Ok(value)
}

/// Reads a big-endian signed integer of `len` bytes (1..=8) at
/// `offset`, sign-extended to `i64`.
///
/// # Errors
///
/// Returns `Truncated` when the buffer ends inside the integer, and
/// `InvalidData` when `len` is zero or above 8.
pub fn read_sint(data: &[u8], offset: usize, len: usize) -> Result<i64> {
    if len == 0 || len > 8 {
        return Err(Error::InvalidData {
            offset,
            detail: "ebml signed integer length must be 1..=8 octets",
        });
    }
    if data.len() < offset + len {
        return Err(Error::Truncated {
            needed: offset + len,
            had: data.len(),
        });
    }
    // Sign-extend from `len` bytes: pad a big-endian 8-byte buffer
    // with the sign bit, then read it as `i64`. The shift trick
    // `(unsigned << shift) as i64 >> shift` does not work in a
    // wider integer type, where the shifted value stays positive.
    let mut buf = [if data[offset] & 0x80 != 0 { 0xFF } else { 0x00 }; 8];
    buf[8 - len..].copy_from_slice(&data[offset..offset + len]);
    Ok(i64::from_be_bytes(buf))
}

/// Reads a big-endian IEEE 754 float of 4 (`f32`) or 8 (`f64`) bytes
/// at `offset`, returned as `f64`.
///
/// # Errors
///
/// Returns `Truncated` when the buffer ends inside the float, and
/// `InvalidData` for any other length.
pub fn read_float(data: &[u8], offset: usize, len: usize) -> Result<f64> {
    match len {
        4 => {
            if data.len() < offset + 4 {
                return Err(Error::Truncated {
                    needed: offset + 4,
                    had: data.len(),
                });
            }
            Ok(f64::from(f32::from_be_bytes(
                data[offset..offset + 4].try_into().unwrap_or([0; 4]),
            )))
        }
        8 => {
            if data.len() < offset + 8 {
                return Err(Error::Truncated {
                    needed: offset + 8,
                    had: data.len(),
                });
            }
            Ok(f64::from_be_bytes(
                data[offset..offset + 8].try_into().unwrap_or([0; 8]),
            ))
        }
        _ => Err(Error::InvalidData {
            offset,
            detail: "ebml float must be 4 or 8 octets",
        }),
    }
}

/// Reads a UTF-8 string of `len` bytes at `offset`. Matroska strings
/// are NUL-terminated in some elements; a trailing NUL is stripped.
///
/// # Errors
///
/// Returns `Truncated` when the buffer ends inside the string, and
/// `InvalidData` when the bytes are not valid UTF-8.
pub fn read_string(data: &[u8], offset: usize, len: usize) -> Result<String> {
    if data.len() < offset + len {
        return Err(Error::Truncated {
            needed: offset + len,
            had: data.len(),
        });
    }
    let mut bytes = &data[offset..offset + len];
    // A trailing NUL is padding, not part of the string.
    while bytes.last() == Some(&0) {
        bytes = &bytes[..bytes.len() - 1];
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| Error::InvalidData {
        offset,
        detail: "ebml string is not valid utf-8",
    })
}

/// Iterative cursor over the children of one EBML master element.
/// `next_header` yields each child header in order; unknown elements
/// are returned like any other, so the caller can skip them by size.
/// The cursor never recurses: descending into a child means creating
/// a new cursor over the child's payload range.
#[derive(Debug, Clone)]
pub struct EbmlCursor<'a> {
    data: &'a [u8],
    position: usize,
    end: usize,
}

impl<'a> EbmlCursor<'a> {
    /// Creates a cursor over `data[start..end]`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the range is out of bounds.
    pub fn new(data: &'a [u8], start: usize, end: usize) -> Result<Self> {
        if start > end || end > data.len() {
            return Err(Error::InvalidArgument("ebml cursor range out of bounds"));
        }
        Ok(Self {
            data,
            position: start,
            end,
        })
    }

    /// Current offset.
    #[must_use]
    pub fn position(&self) -> usize {
        self.position
    }

    /// End of the cursor range.
    #[must_use]
    pub fn end(&self) -> usize {
        self.end
    }

    /// True when no child elements remain.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.position >= self.end
    }

    /// Returns the next child header, or `None` at the end of the
    /// range. A header that would overrun the range is an error, not
    /// a silent stop, so a corrupt size is caught here.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a cut-short header and `InvalidData`
    /// for a malformed ID or a size overrunning the parent.
    pub fn next_header(&mut self) -> Result<Option<EbmlElementHeader>> {
        if self.position >= self.end {
            return Ok(None);
        }
        let header = parse_element_header(self.data, self.position, self.end)?;
        if header.total_len() == 0 {
            return Err(Error::InvalidData {
                offset: self.position,
                detail: "ebml element consumed zero bytes",
            });
        }
        self.position = header.data_end;
        Ok(Some(header))
    }

    /// Skips the rest of the range. Used after a caller has read the
    /// children it needs and wants to ignore the rest.
    pub fn skip_rest(&mut self) {
        self.position = self.end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vint_decodes_known_values() {
        // RFC 8794 section 4.1 examples: 0x40 0x00 is 0, 0x82 is 2.
        assert_eq!(read_vint(&[0x40, 0x00], 0).unwrap(), (0, 2));
        assert_eq!(read_vint(&[0x82], 0).unwrap(), (2, 1));
        assert_eq!(read_vint(&[0x40, 0x7F], 0).unwrap(), (127, 2));
        assert_eq!(read_vint(&[0x20, 0x10, 0x00], 0).unwrap(), (0x1000, 3));
    }

    #[test]
    fn vint_unknown_size_detected() {
        // All value bits set: 0xFF (1 octet), 0x7FFF (2 octets).
        assert_eq!(read_vint_unknown(&[0xFF], 0).unwrap(), (None, 1));
        assert_eq!(read_vint_unknown(&[0x7F, 0xFF], 0).unwrap(), (None, 2));
        assert_eq!(read_vint_unknown(&[0x82], 0).unwrap(), (Some(2), 1));
    }

    #[test]
    fn vint_leading_zero_is_invalid_data() {
        let err = read_vint(&[0x00], 0).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn vint_truncated_returns_truncated() {
        let err = read_vint(&[0x40], 0).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn signed_vint_applies_bias() {
        // Width 1: bias 63. 0x80 is the unsigned 0 -> -63.
        assert_eq!(read_signed_vint(&[0x80], 0).unwrap(), (-63, 1));
        // Width 2: bias 8191. 0x40 0x00 is the unsigned 0 -> -8191.
        assert_eq!(read_signed_vint(&[0x40, 0x00], 0).unwrap(), (-8191, 2));
        // 0xBF is unsigned 63 -> 0.
        assert_eq!(read_signed_vint(&[0xBF], 0).unwrap(), (0, 1));
        // 0xC0 is unsigned 64 -> 1.
        assert_eq!(read_signed_vint(&[0xC0], 0).unwrap(), (1, 1));
    }

    #[test]
    fn element_id_keeps_marker_bits() {
        assert_eq!(
            read_element_id(&[0x1A, 0x45, 0xDF, 0xA3], 0).unwrap(),
            (0x1A45_DFA3, 4)
        );
        assert_eq!(read_element_id(&[0xAE], 0).unwrap(), (0xAE, 1));
        assert_eq!(read_element_id(&[0x42, 0x86], 0).unwrap(), (0x4286, 2));
    }

    #[test]
    fn element_id_leading_zero_is_invalid_data() {
        let err = read_element_id(&[0x00], 0).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn element_header_size_over_parent_is_invalid_data() {
        // EBML element (0x1A45_DFA3) declaring 100 bytes inside a
        // 10-byte parent.
        let data = [0x1A, 0x45, 0xDF, 0xA3, 0xC4, 0x00, 0x00, 0x00];
        let err = parse_element_header(&data, 0, 10).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn element_header_truncated_returns_truncated() {
        let data = [0x1A, 0x45, 0xDF, 0xA3];
        let err = parse_element_header(&data, 0, 4).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn element_header_unknown_size_runs_to_parent_end() {
        // Segment with unknown size (0xFF) inside a 20-byte parent.
        let data = [0x18, 0x53, 0x80, 0x67, 0xFF, 0x00, 0x00, 0x00];
        let header = parse_element_header(&data, 0, 20).unwrap();
        assert_eq!(header.size, None);
        assert_eq!(header.data_start, 5);
        assert_eq!(header.data_end, 20);
    }

    #[test]
    fn cursor_yields_children_in_order() {
        // Two 1-byte-ID elements: 0xAE size 1, 0xD7 size 1.
        let data = [0xAE, 0x81, 0x00, 0xD7, 0x81, 0x00];
        let mut cursor = EbmlCursor::new(&data, 0, data.len()).unwrap();
        let first = cursor.next_header().unwrap().unwrap();
        assert_eq!(first.id, 0xAE);
        assert_eq!(first.payload_len(), 1);
        let second = cursor.next_header().unwrap().unwrap();
        assert_eq!(second.id, 0xD7);
        assert!(cursor.next_header().unwrap().is_none());
    }

    #[test]
    fn cursor_size_overrun_is_invalid_data() {
        let data = [0xAE, 0x85, 0x00];
        let mut cursor = EbmlCursor::new(&data, 0, data.len()).unwrap();
        let err = cursor.next_header().unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn read_uint_and_sint() {
        assert_eq!(read_uint(&[0x01, 0x02], 0, 2).unwrap(), 0x0102);
        assert_eq!(read_uint(&[], 0, 0).unwrap(), 0);
        assert_eq!(read_sint(&[0xFF], 0, 1).unwrap(), -1);
        assert_eq!(read_sint(&[0xFF, 0xFF], 0, 2).unwrap(), -1);
        assert_eq!(read_sint(&[0x80, 0x00], 0, 2).unwrap(), -32768);
    }

    #[test]
    fn read_float_f32_and_f64() {
        let f = read_float(&[0x3F, 0x80, 0x00, 0x00], 0, 4).unwrap();
        assert!((f - 1.0).abs() < f64::EPSILON);
        let f = read_float(&[0x40, 0x09, 0x21, 0xFB, 0x54, 0x44, 0x2D, 0x18], 0, 8).unwrap();
        assert!((f - std::f64::consts::PI).abs() < 1e-15);
    }

    #[test]
    fn read_string_strips_trailing_nul() {
        assert_eq!(read_string(b"webm\0", 0, 5).unwrap(), "webm");
        let err = read_string(&[0xFF, 0xFE], 0, 2).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }
}
