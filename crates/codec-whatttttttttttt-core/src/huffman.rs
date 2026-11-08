//! Vorbis Huffman tables.
//!
//! FLAC residual coding uses Rice codes, not Huffman codes, so this
//! module is not on the FLAC decode path; it exists because Vorbis
//! codebooks (Phase 5) are Huffman-coded. Building the table once here
//! keeps a Kraft-sum bug from being repeated in two codecs.
//!
//! Construction follows the Vorbis codeword assignment (Vorbis I spec
//! section 3.2.1; libvorbis `_make_words` in sharedbook.c): codewords
//! are assigned in entry order via a marker array, NOT by sorting
//! symbols by (code length, symbol value) as generic canonical
//! Huffman codes do. For codebooks whose lengths happen to be sorted
//! by entry the two agree, but for unordered codebooks they differ,
//! and the bitstream only matches the Vorbis assignment. A table is
//! complete when its Kraft sum is exactly 1; over-subscribed tables
//! are always rejected, and incomplete tables are rejected unless the
//! caller explicitly allows them, because a code that matches no
//! symbol is corrupt input, not a reason to guess.
//!
//! Decode reads bits one at a time with the first bit as the most
//! significant bit of the codeword, and binary-searches the codewords
//! of each length. The Vorbis assignment does not make the codewords
//! of one length consecutive, so an offset from a first code cannot
//! index them; the per-length sorted lists are the lookup.

use crate::bitreader::BitSource;
use crate::error::{Error, Result};
use crate::limits::MAX_HUFFMAN_CODE_LENGTH;

/// A Vorbis Huffman table.
///
/// Symbols are `u32` values (Vorbis codebook entries); the table maps a
/// bit pattern read MSB-first to a symbol. The table is immutable after
/// construction and is `Send + Sync`.
#[derive(Debug, Clone)]
pub struct HuffmanTable {
    /// `(codeword, length, symbol)` triples, sorted by (length,
    /// codeword). `length_starts[len]` is the start index of the
    /// length-`len` run in this list.
    codes: Vec<(u32, u8, u32)>,
    /// `length_starts[len]` is the start index in `codes` for
    /// codewords of that length.
    length_starts: [u32; (MAX_HUFFMAN_CODE_LENGTH + 1) as usize],
    /// `counts[len]` is the number of codes with that length.
    counts: [u32; (MAX_HUFFMAN_CODE_LENGTH + 1) as usize],
    /// Longest code length in the table.
    max_length: u32,
}

/// Computes Vorbis Huffman codewords from per-symbol code lengths.
///
/// Follows libvorbis `_make_words` (sharedbook.c) and Vorbis I spec
/// section 3.2.1: codewords are assigned in entry order through a
/// `marker[33]` array, not by sorting symbols by (length, symbol).
/// Returns the raw codeword for each used entry, in entry order;
/// the caller pairs them with their lengths and symbols. The raw
/// codewords are what the decoder's MSB-first accumulation builds,
/// so no bit reversal is applied here.
fn make_words(lengths: &[u8]) -> Vec<u32> {
    let mut marker = [0u32; (MAX_HUFFMAN_CODE_LENGTH + 1) as usize];
    let mut words = Vec::new();
    for &length in lengths {
        let length = length as usize;
        if length == 0 {
            continue;
        }
        let mut entry = marker[length];
        words.push(entry);
        // Walk the markers from this length down, incrementing even
        // entries and jumping at the first odd one.
        for j in (1..=length).rev() {
            if marker[j] & 1 == 1 {
                if j == 1 {
                    marker[1] = marker[1].wrapping_add(1);
                } else {
                    marker[j] = marker[j - 1].wrapping_mul(2);
                }
                break;
            }
            marker[j] = marker[j].wrapping_add(1);
        }
        // Prune the markers for longer lengths that point at the
        // subtree just taken.
        for j in (length + 1)..=(MAX_HUFFMAN_CODE_LENGTH as usize) {
            if (marker[j] >> 1) == entry {
                entry = marker[j];
                marker[j] = marker[j - 1].wrapping_mul(2);
            } else {
                break;
            }
        }
    }
    words
}

impl HuffmanTable {
    /// Builds a complete canonical table from per-symbol code lengths.
    ///
    /// `lengths[symbol]` is the code length for that symbol, or 0 when
    /// the symbol is unused. At least one symbol must be used.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when a length exceeds the limit, when the
    /// table is empty, over-subscribed, or incomplete.
    pub fn from_code_lengths(lengths: &[u8]) -> Result<Self> {
        Self::build(lengths, false)
    }

    /// Builds a Vorbis table that may be incomplete (Kraft sum below
    /// 1). Only callers whose spec permits incomplete tables may use
    /// this; over-subscribed tables are still rejected.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when a length exceeds the limit, when the
    /// table is empty, or when it is over-subscribed.
    pub fn from_code_lengths_incomplete(lengths: &[u8]) -> Result<Self> {
        Self::build(lengths, true)
    }

    fn build(lengths: &[u8], allow_incomplete: bool) -> Result<Self> {
        let max_length = MAX_HUFFMAN_CODE_LENGTH as usize;
        let mut counts = [0u32; (MAX_HUFFMAN_CODE_LENGTH + 1) as usize];
        for &length in lengths {
            if length as usize > max_length {
                return Err(Error::InvalidData {
                    offset: 0,
                    detail: "huffman code length exceeds maximum",
                });
            }
            if length > 0 {
                counts[length as usize] += 1;
            }
        }
        // Kraft sum in fixed point: a complete table sums to 2^32.
        let mut kraft: u64 = 0;
        for (length, &count) in counts.iter().enumerate().skip(1) {
            kraft += u64::from(count) << (32 - length);
        }
        if kraft == 0 {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "huffman table has no used symbols",
            });
        }
        if kraft > (1u64 << 32) {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "huffman table is over-subscribed",
            });
        }
        if kraft < (1u64 << 32) && !allow_incomplete {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "huffman table is incomplete",
            });
        }
        // Vorbis codewords: entry-order assignment via _make_words.
        // The codewords of one length are not necessarily consecutive,
        // so the decode lookup is a per-length sorted list, built
        // below, not a first-code offset.
        let words = make_words(lengths);
        let mut entries: Vec<(u32, u8, u32)> = Vec::with_capacity(words.len());
        let mut word_index = 0usize;
        for (symbol, &length) in lengths.iter().enumerate() {
            if length > 0 {
                let code = words[word_index];
                word_index += 1;
                let symbol = u32::try_from(symbol).map_err(|_| Error::InvalidData {
                    offset: 0,
                    detail: "huffman symbol index exceeds u32 range",
                })?;
                entries.push((code, length, symbol));
            }
        }
        // Sort by (length, codeword) so each length's run is sorted
        // for binary search.
        entries.sort_unstable_by_key(|a| (a.1, a.0));
        let mut length_starts = [0u32; (MAX_HUFFMAN_CODE_LENGTH + 1) as usize];
        let mut running: u32 = 0;
        for length in 1..=(MAX_HUFFMAN_CODE_LENGTH as usize) {
            length_starts[length] = running;
            running += counts[length];
        }
        let max_used = entries
            .iter()
            .map(|&(_, length, _)| length as usize)
            .max()
            .unwrap_or(0);
        Ok(Self {
            codes: entries,
            length_starts,
            counts,
            max_length: u32::try_from(max_used)
                .map_err(|_| Error::Overflow("huffman code length exceeds u32 range"))?,
        })
    }

    /// Longest code length in the table.
    #[must_use]
    pub fn max_length(&self) -> u32 {
        self.max_length
    }

    /// Number of symbols in the table.
    #[must_use]
    pub fn symbol_count(&self) -> usize {
        self.codes.len()
    }

    /// Vorbis code and length for `symbol`, or `None` when the symbol
    /// is unused. Used by tests and by encoders in later phases.
    #[must_use]
    pub fn code_for(&self, symbol: u32) -> Option<(u32, u32)> {
        let &(code, length, _) = self.codes.iter().find(|&&(_, _, s)| s == symbol)?;
        Some((code, u32::from(length)))
    }

    /// Decodes one symbol from `reader`, reading bits one at a time
    /// with the first bit as the most significant bit of the codeword.
    /// `reader` may be MSB-first (`BitReader`, FLAC) or LSB-first
    /// (`LsbBitReader`, Vorbis): the codeword bit order is the same
    /// either way, only the byte/bit packing differs.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when the input ends mid-code, and
    /// `InvalidData` when the bits match no code in the table.
    pub fn decode(&self, reader: &mut impl BitSource) -> Result<u32> {
        // `u64` accumulation: a 32-bit code shifted left once would
        // overflow a `u32` on the last iteration.
        let mut code: u64 = 0;
        for length in 1..=self.max_length as usize {
            let bit = u64::from(reader.read_bit()?);
            code = (code << 1) | bit;
            let count = self.counts[length];
            if count == 0 {
                continue;
            }
            // Binary-search this length's codewords for the bits read
            // so far. The Vorbis assignment leaves gaps between the
            // codewords of one length, so no first-code offset can
            // index them; the sorted run is the lookup.
            let start = self.length_starts[length] as usize;
            let end = start + count as usize;
            let run = &self.codes[start..end];
            let target = u32::try_from(code)
                .map_err(|_| Error::Overflow("huffman code exceeds u32 range"))?;
            if let Ok(position) = run.binary_search_by_key(&target, |&(code, _, _)| code) {
                return Ok(run[position].2);
            }
            // No codeword of this length matches yet; the bits so far
            // may be the prefix of a longer codeword, so keep reading
            // instead of rejecting them here.
        }
        Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "huffman code does not match any symbol",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitreader::BitReader;

    // Lengths [2, 1, 3, 3] for symbols 0..=3 give the Vorbis codes
    // (entry-order assignment, not canonical): symbol 1 -> 1,
    // symbol 0 -> 00, symbol 2 -> 010, symbol 3 -> 011.
    fn example_table() -> HuffmanTable {
        HuffmanTable::from_code_lengths(&[2, 1, 3, 3]).unwrap()
    }

    fn encode(table: &HuffmanTable, symbols: &[u32]) -> Vec<u8> {
        let mut bits: Vec<bool> = Vec::new();
        for &symbol in symbols {
            let (code, length) = table.code_for(symbol).unwrap();
            for i in (0..length).rev() {
                bits.push((code >> i) & 1 == 1);
            }
        }
        let mut bytes = vec![0u8; bits.len().div_ceil(8)];
        for (i, bit) in bits.iter().enumerate() {
            if *bit {
                bytes[i / 8] |= 1 << (7 - (i % 8));
            }
        }
        bytes
    }

    #[test]
    fn vorbis_codes_match_expected_assignment() {
        let table = example_table();
        assert_eq!(table.code_for(1), Some((0b1, 1)));
        assert_eq!(table.code_for(0), Some((0b00, 2)));
        assert_eq!(table.code_for(2), Some((0b010, 3)));
        assert_eq!(table.code_for(3), Some((0b011, 3)));
        assert_eq!(table.code_for(4), None);
    }

    #[test]
    fn decodes_hand_built_bitstream() {
        let table = example_table();
        // Symbols 1, 0, 3, 2 -> bits 1 00 011 010.
        let bytes = encode(&table, &[1, 0, 3, 2]);
        let mut reader = BitReader::new(&bytes);
        assert_eq!(table.decode(&mut reader).unwrap(), 1);
        assert_eq!(table.decode(&mut reader).unwrap(), 0);
        assert_eq!(table.decode(&mut reader).unwrap(), 3);
        assert_eq!(table.decode(&mut reader).unwrap(), 2);
    }

    #[test]
    fn vorbis_assignment_differs_from_canonical() {
        // Lengths [2, 1, 2]: the canonical assignment would give
        // symbol 0 -> 10, symbol 1 -> 0, symbol 2 -> 11. The Vorbis
        // entry-order assignment gives symbol 0 -> 00, symbol 1 -> 1,
        // symbol 2 -> 01. This is the regression test for the Vorbis
        // PCM bug: decoding real Vorbis packets with the canonical
        // assignment reads the wrong codebook entries.
        let table = HuffmanTable::from_code_lengths(&[2, 1, 2]).unwrap();
        assert_eq!(table.code_for(0), Some((0b00, 2)));
        assert_eq!(table.code_for(1), Some((0b1, 1)));
        assert_eq!(table.code_for(2), Some((0b01, 2)));
        let mut reader = BitReader::new(&[0b0010_1000]);
        assert_eq!(table.decode(&mut reader).unwrap(), 0);
        assert_eq!(table.decode(&mut reader).unwrap(), 1);
        assert_eq!(table.decode(&mut reader).unwrap(), 2);
    }

    #[test]
    fn book5_codewords_match_libvorbis() {
        // Codebook 5 of the mono Vorbis test file (the classbook):
        // lengthlist dumped from the file with libvorbis itself.
        // The first classword of mono audio packet 4 is entry 88,
        // whose bits are 11101 (code 29, length 5); the canonical
        // assignment would give entry 88 the code 10010 instead and
        // read entry 30 (code 11101100) from those bits.
        let lengths: [u8; 100] = [
            4, 7, 13, 9, 15, 9, 16, 8, 10, 13, 7, 5, 8, 6, 9, 7, 10, 7, 10, 11, 11, 6, 7, 8, 8, 9,
            9, 9, 12, 16, 8, 5, 8, 6, 8, 6, 9, 7, 10, 12, 11, 7, 7, 7, 6, 7, 7, 7, 11, 15, 7, 5, 8,
            6, 7, 5, 7, 6, 9, 13, 13, 9, 9, 8, 6, 6, 5, 5, 9, 14, 8, 6, 8, 6, 6, 4, 5, 3, 5, 13, 9,
            9, 11, 8, 10, 7, 8, 4, 5, 12, 11, 16, 17, 15, 17, 12, 13, 8, 8, 15,
        ];
        let table = HuffmanTable::from_code_lengths(&lengths).unwrap();
        assert_eq!(table.code_for(88), Some((29, 5)));
        assert_eq!(table.code_for(30), Some((51, 8)));
        assert_eq!(table.code_for(77), Some((6, 3)));
        assert_eq!(table.code_for(0), Some((0, 4)));
        assert_eq!(table.code_for(31), Some((7, 5)));
        // The bits 11101 at the start of the classword section decode
        // to entry 88.
        let mut reader = BitReader::new(&[0b1110_1000]);
        assert_eq!(table.decode(&mut reader).unwrap(), 88);
    }

    #[test]
    fn single_symbol_table_is_incomplete_by_nature() {
        // One code of length 1 covers only half the code space, so a
        // single-symbol table is incomplete and needs the explicit
        // incomplete constructor.
        let table = HuffmanTable::from_code_lengths_incomplete(&[1]).unwrap();
        let mut reader = BitReader::new(&[0b0]);
        assert_eq!(table.decode(&mut reader).unwrap(), 0);
    }

    #[test]
    fn over_subscribed_table_is_rejected() {
        // Two symbols of length 1 already fill the code space.
        let err = HuffmanTable::from_code_lengths(&[1, 1, 1]).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn incomplete_table_is_rejected_by_default() {
        // One symbol of length 2 leaves half the code space unused.
        let err = HuffmanTable::from_code_lengths(&[2]).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn incomplete_table_is_accepted_when_allowed() {
        let table = HuffmanTable::from_code_lengths_incomplete(&[2]).unwrap();
        let mut reader = BitReader::new(&[0b00]);
        assert_eq!(table.decode(&mut reader).unwrap(), 0);
    }

    #[test]
    fn code_length_above_limit_is_rejected() {
        let err = HuffmanTable::from_code_lengths(&[33]).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn empty_table_is_rejected() {
        let err = HuffmanTable::from_code_lengths(&[0, 0]).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn truncated_code_returns_truncated() {
        let table = example_table();
        let mut reader = BitReader::new(&[]);
        let err = table.decode(&mut reader).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn code_above_length_range_can_still_match_longer_code() {
        // Lengths [1, 2, 2]: codes are symbol 0 -> 0, symbol 1 -> 10,
        // symbol 2 -> 11. Decoding 11 must not be rejected after the
        // first bit, even though 1 is above the length-1 code range.
        let table = HuffmanTable::from_code_lengths(&[1, 2, 2]).unwrap();
        let mut reader = BitReader::new(&[0b1100_0000]);
        assert_eq!(table.decode(&mut reader).unwrap(), 2);
    }

    #[test]
    fn invalid_code_returns_invalid_data() {
        // Incomplete table with symbols of lengths [2, 1]: codes 00
        // and 1. The bits 01 match no code (a complete table has no
        // such bit pattern, so the incomplete constructor is required
        // here).
        let table = HuffmanTable::from_code_lengths_incomplete(&[2, 1]).unwrap();
        let mut reader = BitReader::new(&[0b0100_0000]);
        let err = table.decode(&mut reader).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }
}
