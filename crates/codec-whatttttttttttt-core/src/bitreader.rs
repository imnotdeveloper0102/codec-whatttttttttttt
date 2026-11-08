//! Bit-level reader over a bounded slice.
//!
//! All reads are checked against the remaining bits. This is the single
//! trust boundary for every bitstream parser in the library (FLAC, Vorbis,
//! Opus, VP9, AV1, EBML), so a bound-check bug here would affect every
//! codec; the module therefore has its own property tests.

use crate::error::{Error, Result};

/// One-bit-at-a-time source shared by the MSB-first `BitReader` and
/// the LSB-first `LsbBitReader`. Huffman codewords are read bit by bit
/// with the first bit as the most significant bit of the codeword in
/// both cases (Vorbis I spec section 3.2.1), so `HuffmanTable::decode`
/// is generic over this trait instead of being duplicated per reader.
pub trait BitSource {
    /// Reads one bit.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when no bits remain.
    fn read_bit(&mut self) -> Result<bool>;

    /// Current bit position, for error offsets.
    fn bit_position(&self) -> usize;
}

impl BitSource for BitReader<'_> {
    fn read_bit(&mut self) -> Result<bool> {
        self.read_bit()
    }

    fn bit_position(&self) -> usize {
        self.bit_position()
    }
}

/// Reads bits MSB-first from a borrowed slice. No I/O is performed here.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    /// Creates a reader over `data`. Reading never outlives the slice.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    /// Number of bits not yet consumed.
    #[must_use]
    pub fn remaining_bits(&self) -> usize {
        self.data.len() * 8 - self.bit_pos
    }

    /// Number of bytes not yet consumed, rounded up to a whole byte.
    #[must_use]
    pub fn remaining_bytes(&self) -> usize {
        self.remaining_bits().div_ceil(8)
    }

    /// Current bit position, for error offsets.
    #[must_use]
    pub fn bit_position(&self) -> usize {
        self.bit_pos
    }

    /// Reads `n` bits (0..=32) as a `u32`, MSB-first.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when fewer than `n` bits remain, and
    /// `InvalidData` when `n > 32`.
    pub fn read_bits(&mut self, n: u32) -> Result<u32> {
        if n > 32 {
            return Err(Error::InvalidData {
                offset: self.bit_pos / 8,
                detail: "bit read wider than 32 bits",
            });
        }
        let n = n as usize;
        if self.remaining_bits() < n {
            return Err(Error::Truncated {
                needed: n,
                had: self.remaining_bits(),
            });
        }
        let mut value: u32 = 0;
        for _ in 0..n {
            let byte = self.data[self.bit_pos / 8];
            let bit = (byte >> (7 - (self.bit_pos % 8))) & 1;
            value = (value << 1) | u32::from(bit);
            self.bit_pos += 1;
        }
        Ok(value)
    }

    /// Reads one bit.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when no bits remain.
    pub fn read_bit(&mut self) -> Result<bool> {
        Ok(self.read_bits(1)? != 0)
    }

    /// Reads one byte, aligned to the next byte boundary only if the
    /// reader is already byte-aligned; otherwise reads the next 8 bits.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when fewer than 8 bits remain.
    pub fn read_u8(&mut self) -> Result<u8> {
        Ok(u8::try_from(self.read_bits(8)?).unwrap_or(0))
    }

    /// Reads an unsigned LEB128 value, consuming at most `max_bytes` bytes.
    /// The continuation bit of the last byte must be clear, and the value
    /// must fit in a `u64` without overflowing the shift.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when the input ends mid-value, and
    /// `InvalidData` when the value needs more than `max_bytes` bytes or
    /// would overflow `u64`.
    pub fn read_leb128(&mut self, max_bytes: usize) -> Result<u64> {
        if max_bytes == 0 || max_bytes > 10 {
            return Err(Error::InvalidArgument(
                "leb128 max_bytes must be between 1 and 10",
            ));
        }
        let mut value: u64 = 0;
        for i in 0..max_bytes {
            let byte = self.read_u8()?;
            let low = u64::from(byte & 0x7F);
            let shift = i * 7;
            // The 10th byte may only contribute one bit to a u64.
            if shift == 63 && low > 1 {
                return Err(Error::InvalidData {
                    offset: self.bit_pos / 8,
                    detail: "leb128 value overflows u64",
                });
            }
            value |= low << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(Error::InvalidData {
            offset: self.bit_pos / 8,
            detail: "leb128 value exceeds max_bytes",
        })
    }

    /// Skips to the next byte boundary. A no-op when already aligned.
    pub fn align_to_byte(&mut self) {
        let remainder = self.bit_pos % 8;
        if remainder != 0 {
            self.bit_pos += 8 - remainder;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bits_msb_first() {
        let mut reader = BitReader::new(&[0b1010_0110]);
        assert_eq!(reader.read_bits(4).unwrap(), 0b1010);
        assert_eq!(reader.read_bits(4).unwrap(), 0b0110);
        assert_eq!(reader.remaining_bits(), 0);
    }

    #[test]
    fn read_past_end_returns_truncated() {
        let mut reader = BitReader::new(&[0xFF]);
        assert_eq!(reader.read_bits(8).unwrap(), 0xFF);
        let err = reader.read_bit().unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn read_wider_than_32_bits_returns_invalid_data() {
        let mut reader = BitReader::new(&[0xFF; 8]);
        let err = reader.read_bits(33).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn leb128_decodes_known_values() {
        let mut reader = BitReader::new(&[0xE5, 0x8E, 0x26]);
        assert_eq!(reader.read_leb128(10).unwrap(), 624_485);
        let mut reader = BitReader::new(&[0x00]);
        assert_eq!(reader.read_leb128(10).unwrap(), 0);
    }

    #[test]
    fn leb128_unterminated_within_max_returns_invalid_data() {
        let mut reader = BitReader::new(&[0x80, 0x80]);
        let err = reader.read_leb128(2).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn align_to_byte_skips_partial_byte() {
        let mut reader = BitReader::new(&[0b1111_0000, 0xAB]);
        assert_eq!(reader.read_bits(3).unwrap(), 0b111);
        reader.align_to_byte();
        assert_eq!(reader.read_u8().unwrap(), 0xAB);
    }
}
