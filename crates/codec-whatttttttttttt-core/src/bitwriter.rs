//! Bit-level writer over an owned byte buffer.
//!
//! The counterpart of `bitreader::BitReader`: writes bits MSB-first,
//! which is the order FLAC subframes use. Bits are accumulated in
//! the last byte of the buffer and flushed when full; `finish`
//! returns the bytes with the last byte zero-padded, matching the
//! FLAC rule that a frame ends on a byte boundary (RFC 9639
//! section 9.2: subframe data is followed by zero padding to the
//! byte boundary before the CRC-16).

use crate::error::{Error, Result};

/// Writes bits MSB-first into an owned `Vec<u8>`.
///
/// Invariant: `used_bits` is the number of bits used in the last
/// byte, 0..=8. It is 0 only for a fresh writer (no bytes yet);
/// after any write it is 1..=8, and 8 means the last byte is full
/// and the next bit starts a fresh byte.
#[derive(Debug, Clone, Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    used_bits: u32,
}

impl BitWriter {
    /// Creates an empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of bits written so far.
    #[must_use]
    pub fn bit_position(&self) -> usize {
        if self.bytes.is_empty() {
            0
        } else if self.used_bits == 8 {
            self.bytes.len() * 8
        } else {
            (self.bytes.len() - 1) * 8 + usize::try_from(self.used_bits).unwrap_or(0)
        }
    }

    /// Writes `n` bits (0..=32) of `value`, MSB-first. Only the low
    /// `n` bits of `value` are written.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when `n > 32`.
    pub fn write_bits(&mut self, value: u32, n: u32) -> Result<()> {
        if n > 32 {
            return Err(Error::InvalidArgument("bit write wider than 32 bits"));
        }
        for i in (0..n).rev() {
            self.write_bit((value >> i) & 1 != 0);
        }
        Ok(())
    }

    /// Writes one bit.
    pub fn write_bit(&mut self, bit: bool) {
        if self.used_bits == 8 || self.bytes.is_empty() {
            self.bytes.push(0);
            self.used_bits = 0;
        }
        if bit {
            let last = self.bytes.len() - 1;
            self.bytes[last] |= 1 << (7 - self.used_bits);
        }
        self.used_bits += 1;
    }

    /// Writes `value` as a signed two's-complement number in `bits`
    /// bits (1..=32). Callers must range-check first: a value
    /// outside the range is truncated to its low bits (the FLAC
    /// encoder checks each value against its subframe's range).
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when `bits` is 0 or above 32.
    pub fn write_signed(&mut self, value: i64, bits: u32) -> Result<()> {
        if bits == 0 || bits > 32 {
            return Err(Error::InvalidArgument(
                "signed bit write must be 1..=32 bits",
            ));
        }
        let mask: u64 = if bits == 32 {
            u64::from(u32::MAX)
        } else {
            (1u64 << bits) - 1
        };
        // Two's-complement bit pattern: `to_ne_bytes` reinterprets
        // the signed value's bits as unsigned without a sign-losing
        // cast, then the mask keeps only the low `bits` bits.
        let unsigned = u64::from_ne_bytes(value.to_ne_bytes()) & mask;
        self.write_bits(u32::try_from(unsigned).unwrap_or(0), bits)
    }

    /// Writes a whole byte. Only valid when byte-aligned (fresh
    /// writer, or the last byte is full).
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the writer is not byte-aligned.
    pub fn write_u8(&mut self, value: u8) -> Result<()> {
        if self.used_bits != 0 && self.used_bits != 8 {
            return Err(Error::InvalidArgument("byte write requires byte alignment"));
        }
        self.bytes.push(value);
        self.used_bits = 8;
        Ok(())
    }

    /// Appends already-encoded bytes. Only valid when byte-aligned.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the writer is not byte-aligned.
    pub fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        if self.used_bits != 0 && self.used_bits != 8 {
            return Err(Error::InvalidArgument("byte write requires byte alignment"));
        }
        self.bytes.extend_from_slice(data);
        if !data.is_empty() {
            self.used_bits = 8;
        }
        Ok(())
    }

    /// Returns the bytes written, zero-padding the last byte when
    /// it is partial. A writer that is already byte-aligned returns
    /// its bytes unchanged.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }

    /// Number of bytes written so far, rounded up to a whole byte.
    #[must_use]
    pub fn len_bytes(&self) -> usize {
        self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_bits_msb_first() {
        let mut writer = BitWriter::new();
        writer.write_bits(0b101, 3).unwrap();
        writer.write_bits(0b11, 2).unwrap();
        writer.write_bits(0b000, 3).unwrap();
        assert_eq!(writer.finish(), vec![0b1011_1000]);
    }

    #[test]
    fn writes_signed_negative_as_twos_complement() {
        let mut writer = BitWriter::new();
        writer.write_signed(-1, 8).unwrap();
        writer.write_signed(-2, 4).unwrap();
        writer.write_signed(5, 4).unwrap();
        assert_eq!(writer.finish(), vec![0xFF, 0xE5]);
    }

    #[test]
    fn finish_pads_final_byte_with_zeros() {
        let mut writer = BitWriter::new();
        writer.write_bits(1, 1).unwrap();
        assert_eq!(writer.finish(), vec![0b1000_0000]);
    }

    #[test]
    fn write_wider_than_32_bits_is_rejected() {
        let mut writer = BitWriter::new();
        assert!(matches!(
            writer.write_bits(0, 33),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn byte_write_requires_alignment() {
        let mut writer = BitWriter::new();
        writer.write_bits(1, 1).unwrap();
        assert!(matches!(
            writer.write_u8(0xFF),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn bit_position_counts_partial_byte() {
        let mut writer = BitWriter::new();
        assert_eq!(writer.bit_position(), 0);
        writer.write_bits(0b101, 3).unwrap();
        assert_eq!(writer.bit_position(), 3);
        writer.write_bits(0xFF, 8).unwrap();
        assert_eq!(writer.bit_position(), 11);
    }
}
