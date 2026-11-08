//! AV1 Open Bitstream Unit (OBU) parsing (AV1 spec section 5.3).
//!
//! An OBU stream is a sequence of OBUs; each OBU has a one-byte
//! header, an optional LEB128 size, and a payload. This module only
//! parses the framing: the payload of a sequence header, frame
//! header, or tile group is interpreted by `av1_sequence` and
//! `av1_frame`. Metadata OBUs are parsed and skipped; the frame
//! header OBU and the tile group OBU are the payload of a frame
//! OBU, which this module does not combine.

use crate::bitreader::BitReader;
use crate::error::{Error, Result};

/// OBU type (AV1 spec section 5.3.2, Table 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObuType {
    /// Reserved (0).
    Reserved,
    /// Sequence header (1).
    SequenceHeader,
    /// Temporal delimiter (2).
    TemporalDelimiter,
    /// Frame header (3).
    FrameHeader,
    /// Tile group (4).
    TileGroup,
    /// Metadata (5).
    Metadata,
    /// Frame (6).
    Frame,
    /// Redundant frame header (7).
    RedundantFrameHeader,
    /// Tile list (8).
    TileList,
    /// Padding (15).
    Padding,
}

impl ObuType {
    /// The 4-bit type value on the wire (AV1 spec section 5.3.2).
    #[must_use]
    pub fn value(self) -> u8 {
        match self {
            Self::Reserved => 0,
            Self::SequenceHeader => 1,
            Self::TemporalDelimiter => 2,
            Self::FrameHeader => 3,
            Self::TileGroup => 4,
            Self::Metadata => 5,
            Self::Frame => 6,
            Self::RedundantFrameHeader => 7,
            Self::TileList => 8,
            Self::Padding => 15,
        }
    }

    /// Converts the raw 4-bit type value into an `ObuType`.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` for the reserved types 9..=14.
    pub fn from_value(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Reserved),
            1 => Ok(Self::SequenceHeader),
            2 => Ok(Self::TemporalDelimiter),
            3 => Ok(Self::FrameHeader),
            4 => Ok(Self::TileGroup),
            5 => Ok(Self::Metadata),
            6 => Ok(Self::Frame),
            7 => Ok(Self::RedundantFrameHeader),
            8 => Ok(Self::TileList),
            15 => Ok(Self::Padding),
            _ => Err(Error::Unsupported("unsupported av1 obu type")),
        }
    }
}

/// Header of one OBU (AV1 spec section 5.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObuHeader {
    /// OBU type.
    pub obu_type: ObuType,
    /// True when the OBU has an extension byte.
    pub has_extension: bool,
    /// True when the OBU has a size field.
    pub has_size_field: bool,
    /// Temporal id, when the extension byte is present.
    pub temporal_id: u8,
    /// Spatial id, when the extension byte is present.
    pub spatial_id: u8,
    /// Payload size in bytes, when `has_size_field` is set.
    pub payload_size: Option<usize>,
}

/// Parses one OBU header and its size field from `data` at `offset`.
/// Returns the header and the offset of the first payload byte.
///
/// # Errors
///
/// Returns `InvalidData` when the forbidden bit is set, `Truncated`
/// when the buffer ends inside the header or size field, and
/// `Unsupported` for a reserved OBU type.
pub fn parse_obu_header(data: &[u8], offset: usize) -> Result<(ObuHeader, usize)> {
    if data.len() < offset + 1 {
        return Err(Error::Truncated {
            needed: offset + 1,
            had: data.len(),
        });
    }
    let byte = data[offset];
    let forbidden = (byte >> 7) & 1;
    if forbidden != 0 {
        return Err(Error::InvalidData {
            offset,
            detail: "av1 obu forbidden bit is set",
        });
    }
    let obu_type = ObuType::from_value((byte >> 3) & 0x0F)?;
    let has_extension = (byte >> 2) & 1 != 0;
    let has_size_field = (byte >> 1) & 1 != 0;
    let reserved = byte & 1;
    if reserved != 0 {
        return Err(Error::InvalidData {
            offset,
            detail: "av1 obu reserved bit is set",
        });
    }
    let mut position = offset + 1;
    let (temporal_id, spatial_id) = if has_extension {
        if data.len() < position + 1 {
            return Err(Error::Truncated {
                needed: position + 1,
                had: data.len(),
            });
        }
        let extension = data[position];
        position += 1;
        ((extension >> 5) & 0x07, (extension >> 3) & 0x03)
    } else {
        (0, 0)
    };
    let payload_size = if has_size_field {
        let mut reader = BitReader::new(&data[position..]);
        let size = reader.read_leb128(8)?;
        // LEB128 consumes whole bytes, so the bit position is a
        // multiple of 8 here.
        let consumed = reader.bit_position() / 8;
        position += consumed;
        Some(
            usize::try_from(size)
                .map_err(|_| Error::Overflow("av1 obu size exceeds address space"))?,
        )
    } else {
        None
    };
    Ok((
        ObuHeader {
            obu_type,
            has_extension,
            has_size_field,
            temporal_id,
            spatial_id,
            payload_size,
        },
        position,
    ))
}

/// One parsed OBU: its header and its payload slice.
#[derive(Debug, Clone, Copy)]
pub struct Obu<'a> {
    /// OBU header.
    pub header: ObuHeader,
    /// Payload bytes.
    pub payload: &'a [u8],
}

/// Parses one OBU at `offset`. When the OBU has no size field, the
/// payload runs to the end of `data`; when it has one, the payload
/// is exactly that many bytes.
///
/// # Errors
///
/// Returns `Truncated` when the size field promises more bytes than
/// the buffer holds, `InvalidData` for a forbidden or reserved bit,
/// and `Unsupported` for a reserved OBU type.
pub fn parse_obu(data: &[u8], offset: usize) -> Result<(Obu<'_>, usize)> {
    let (header, payload_start) = parse_obu_header(data, offset)?;
    let (payload, next) = match header.payload_size {
        Some(size) => {
            let end = payload_start
                .checked_add(size)
                .ok_or(Error::Overflow("av1 obu size overflows"))?;
            if end > data.len() {
                return Err(Error::Truncated {
                    needed: end,
                    had: data.len(),
                });
            }
            (&data[payload_start..end], end)
        }
        None => (&data[payload_start..], data.len()),
    };
    Ok((Obu { header, payload }, next))
}

/// Iterator over the OBUs of one buffer. A buffer with no size
/// fields yields a single OBU; an empty buffer yields none.
#[derive(Debug, Clone)]
pub struct ObuReader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> ObuReader<'a> {
    /// Creates a reader over `data`.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    /// Returns the next OBU, or `None` at the end of the buffer.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a cut-short OBU, `InvalidData` for a
    /// forbidden or reserved bit, and `Unsupported` for a reserved
    /// OBU type.
    pub fn next_obu(&mut self) -> Result<Option<Obu<'a>>> {
        if self.position >= self.data.len() {
            return Ok(None);
        }
        let (obu, next) = parse_obu(self.data, self.position)?;
        self.position = next;
        Ok(Some(obu))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obu_header_sequence_header_with_size() {
        // type 1, has_size_field set, size 10.
        let data = [0x0A, 0x0A, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let (header, payload_start) = parse_obu_header(&data, 0).unwrap();
        assert_eq!(header.obu_type, ObuType::SequenceHeader);
        assert!(!header.has_extension);
        assert!(header.has_size_field);
        assert_eq!(header.payload_size, Some(10));
        assert_eq!(payload_start, 2);
    }

    #[test]
    fn obu_header_temporal_delimiter_no_size() {
        // type 2, no extension, no size field: header byte 0x10.
        // (The temporal delimiter in the test vectors is 0x12 0x00,
        // i.e. with a zero size field; see obu_reader test below.)
        let data = [0x10];
        let (header, payload_start) = parse_obu_header(&data, 0).unwrap();
        assert_eq!(header.obu_type, ObuType::TemporalDelimiter);
        assert!(!header.has_size_field);
        assert_eq!(header.payload_size, None);
        assert_eq!(payload_start, 1);
    }

    #[test]
    fn obu_header_with_extension() {
        // type 6 (frame), extension set, size set; extension byte
        // temporal 5, spatial 1.
        let data = [0x36, 0xA8, 0x01, 0xFF];
        let (header, payload_start) = parse_obu_header(&data, 0).unwrap();
        assert_eq!(header.obu_type, ObuType::Frame);
        assert!(header.has_extension);
        assert_eq!(header.temporal_id, 5);
        assert_eq!(header.spatial_id, 1);
        assert_eq!(header.payload_size, Some(1));
        assert_eq!(payload_start, 3);
    }

    #[test]
    fn obu_forbidden_bit_is_invalid_data() {
        let data = [0x8A, 0x0A];
        let err = parse_obu_header(&data, 0).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn obu_reserved_bit_is_invalid_data() {
        // type 1, has_size_field set, reserved bit set.
        let data = [0x0B, 0x0A];
        let err = parse_obu_header(&data, 0).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn obu_reserved_type_is_unsupported() {
        // type 9.
        let data = [0x4A, 0x00];
        let err = parse_obu_header(&data, 0).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn obu_truncated_size_is_truncated() {
        let data = [0x0A, 0x64];
        let err = parse_obu(&data, 0).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn obu_reader_iterates_stream() {
        let mut data = Vec::new();
        // Temporal delimiter with a zero size field, as libaom
        // writes it (header 0x12, LEB128 size 0).
        data.extend([0x12, 0x00]);
        data.extend([0x0A, 0x02, 0xAA, 0xBB]); // sequence header, 2 bytes
        let mut reader = ObuReader::new(&data);
        let first = reader.next_obu().unwrap().unwrap();
        assert_eq!(first.header.obu_type, ObuType::TemporalDelimiter);
        assert_eq!(first.payload.len(), 0);
        let second = reader.next_obu().unwrap().unwrap();
        assert_eq!(second.header.obu_type, ObuType::SequenceHeader);
        assert_eq!(second.payload, &[0xAA, 0xBB]);
        assert!(reader.next_obu().unwrap().is_none());
    }

    #[test]
    fn obu_reader_empty_is_none() {
        let mut reader = ObuReader::new(&[]);
        assert!(reader.next_obu().unwrap().is_none());
    }
}
