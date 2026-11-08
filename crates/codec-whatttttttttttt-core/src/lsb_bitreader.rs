//! LSB-first bit reader for Vorbis (Vorbis I spec section 2.1.4).
//!
//! Vorbis packs integers least-significant-bit first into the least
//! significant unused bit of the destination byte, bytes in order. This
//! is the opposite order from `BitReader`, which reads MSB-first for
//! FLAC, Opus and the video codecs, so Vorbis gets its own reader
//! rather than a flag on the shared one: a wrong bit order silently
//! decodes garbage, and separate types make the order visible at the
//! call site.
//!
//! Reading past the end of a packet is reported as `Truncated` here;
//! the Vorbis packet decoder decides, per spec section, whether that
//! is nominal (floor curve decode, residue decode) or an error
//! (headers, mode and window decode).

use crate::error::{Error, Result};

/// Reads bits LSB-first from a borrowed slice. No I/O is performed here.
#[derive(Debug, Clone)]
pub struct LsbBitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> LsbBitReader<'a> {
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

    /// Current bit position, for error offsets.
    #[must_use]
    pub fn bit_position(&self) -> usize {
        self.bit_pos
    }

    /// True when no bits remain.
    #[must_use]
    pub fn at_end(&self) -> bool {
        self.remaining_bits() == 0
    }

    /// Reads `n` bits (0..=32) as a `u32`, LSB-first: the first bit
    /// read becomes bit 0 of the result.
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
        for i in 0..n {
            let byte = self.data[self.bit_pos / 8];
            let bit = (byte >> (self.bit_pos % 8)) & 1;
            value |= u32::from(bit) << i;
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

    /// Reads `n` bits (0..=8) as a `u8`. `read_bits(n)` returns a
    /// value below 2^n, so the conversion is exact when `n <= 8`.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when fewer than `n` bits remain, and
    /// `InvalidData` when `n > 8`.
    pub fn read_u8(&mut self, n: u32) -> Result<u8> {
        if n > 8 {
            return Err(Error::InvalidData {
                offset: self.bit_pos / 8,
                detail: "u8 read wider than 8 bits",
            });
        }
        u8::try_from(self.read_bits(n)?).map_err(|_| Error::InvalidData {
            offset: self.bit_pos / 8,
            detail: "u8 value exceeds range",
        })
    }

    /// Reads `n` bits (0..=16) as a `u16`. `read_bits(n)` returns a
    /// value below 2^n, so the conversion is exact when `n <= 16`.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when fewer than `n` bits remain, and
    /// `InvalidData` when `n > 16`.
    pub fn read_u16(&mut self, n: u32) -> Result<u16> {
        if n > 16 {
            return Err(Error::InvalidData {
                offset: self.bit_pos / 8,
                detail: "u16 read wider than 16 bits",
            });
        }
        u16::try_from(self.read_bits(n)?).map_err(|_| Error::InvalidData {
            offset: self.bit_pos / 8,
            detail: "u16 value exceeds range",
        })
    }

    /// Reads 32 bits and reinterprets them as a signed `i32`. The
    /// Vorbis bitrate fields are signed, so the bit pattern is
    /// reinterpreted rather than converted.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when fewer than 32 bits remain.
    pub fn read_i32(&mut self) -> Result<i32> {
        Ok(i32::from_ne_bytes(self.read_bits(32)?.to_ne_bytes()))
    }

    /// Reads `buf.len()` bytes, each as 8 LSB-first bits. Vorbis
    /// comment strings are octet-aligned in practice, but reading them
    /// bit by bit keeps this correct even when a preceding field left
    /// the reader unaligned.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when fewer than `buf.len()` bytes remain.
    pub fn read_bytes(&mut self, buf: &mut [u8]) -> Result<()> {
        for byte in buf.iter_mut() {
            *byte = u8::try_from(self.read_bits(8)?).map_err(|_| Error::InvalidData {
                offset: self.bit_pos / 8,
                detail: "byte value exceeds u8 range",
            })?;
        }
        Ok(())
    }
}

impl crate::bitreader::BitSource for LsbBitReader<'_> {
    fn read_bit(&mut self) -> Result<bool> {
        self.read_bit()
    }

    fn bit_position(&self) -> usize {
        self.bit_position()
    }
}

/// Position of the highest set bit (1-based); `ilog(0) = 0`,
/// `ilog(1) = 1`, `ilog(3) = 2`. Vorbis I spec section 9.2.1. Used by
/// Vorbis setup and packet decode, and by the Opus range decoder
/// (RFC 6716 section 4.1.5) for the same definition.
#[must_use]
pub fn ilog(value: u32) -> u32 {
    if value == 0 {
        0
    } else {
        32 - value.leading_zeros()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bits_lsb_first() {
        // 0b1010_0110: first bit read is bit 0 (0), then 1, 1, 0...
        let mut reader = LsbBitReader::new(&[0b1010_0110]);
        assert_eq!(reader.read_bits(4).unwrap(), 0b0110);
        assert_eq!(reader.read_bits(4).unwrap(), 0b1010);
        assert_eq!(reader.remaining_bits(), 0);
    }

    #[test]
    fn multi_byte_value_is_little_endian() {
        // The 32-bit value 0x04030201 is stored as bytes 01 02 03 04.
        let mut reader = LsbBitReader::new(&[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(reader.read_bits(32).unwrap(), 0x0403_0201);
    }

    #[test]
    fn read_past_end_returns_truncated() {
        let mut reader = LsbBitReader::new(&[0xFF]);
        assert_eq!(reader.read_bits(8).unwrap(), 0xFF);
        let err = reader.read_bit().unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn read_wider_than_32_bits_returns_invalid_data() {
        let mut reader = LsbBitReader::new(&[0xFF; 8]);
        let err = reader.read_bits(33).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn ilog_matches_spec_examples() {
        assert_eq!(ilog(0), 0);
        assert_eq!(ilog(1), 1);
        assert_eq!(ilog(2), 2);
        assert_eq!(ilog(3), 2);
        assert_eq!(ilog(4), 3);
        assert_eq!(ilog(7), 3);
    }
}
