//! Layer III Huffman decoding and dequantization (ISO/IEC 11172-3
//! Annex B, Table B.7, and section 2.4.3.4.1).
//!
//! The 32 Huffman tables are stored as flattened decoding trees in
//! `mp3_tables.rs` (`MP3_HUFFMAN_TABS`, indexed by
//! `MP3_HUFFMAN_TABINDEX`): a tree node is one `i16`. A non-negative
//! entry is a leaf: its high byte is the code length in bits and its
//! low two nibbles are the decoded `(x, y)` pair, low nibble first.
//! A negative entry is an internal node: the walk peeks
//! `leaf & 7` more bits and continues at
//! `peeked - (leaf >> 3)` (the shift is arithmetic, so the subtrahend
//! is positive). This is the flattened-tree encoding of the Annex B
//! tables, transcribed from the CC0 minimp3 reference; the codewords
//! themselves are the spec's, only the storage is a tree instead of
//! a per-symbol table. The count-1 tables (`tab32`/`tab33`) use a
//! two-level lookup: the first 4 bits index the table, an entry
//! without bit 3 set is an internal node whose low two bits say how
//! many following bits extend the index, and a leaf's low three
//! bits are the code length while bits 4..=7 say which of the four
//! values are nonzero.
//!
//! Dequantization is `x^(4/3)` from the `MP3_POW43` table
//! (section 2.4.3.4.1), with linbits extending magnitudes of 15.
//! A sign bit follows every nonzero value; the sign is the next bit
//! in the stream, read before it is consumed.

use crate::error::{Error, Result};
use crate::mp3_tables::{
    MP3_COUNT1_TAB32, MP3_COUNT1_TAB33, MP3_HUFFMAN_TABINDEX, MP3_HUFFMAN_TABS, MP3_LINBITS,
    MP3_POW43,
};

/// Largest code length a Layer III Huffman code may have
/// (ISO/IEC 11172-3 Annex B: no code exceeds 32 bits, and the
/// flattened trees here resolve in far fewer steps).
const MAX_HUFFMAN_WALK_STEPS: usize = 40;

/// MSB-first bit cache over a bounded slice. Bits are read
/// most-significant first, as in the rest of this library's
/// `BitReader`; the cache only exists because Huffman decoding
/// peeks ahead without consuming (the tree walk reads up to 5 bits,
/// decides, then consumes the code length).
#[derive(Debug)]
pub(crate) struct Mp3BitCache<'a> {
    data: &'a [u8],
    /// Next byte to load into the cache.
    byte_pos: usize,
    /// Valid bits, held at the top of the word.
    cache: u64,
    /// Number of valid bits in `cache`.
    cached: u32,
    /// Total bits consumed from the start of `data`.
    bit_pos: usize,
}

impl<'a> Mp3BitCache<'a> {
    /// Creates a cache over `data`, positioned at bit 0.
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            cache: 0,
            cached: 0,
            bit_pos: 0,
        }
    }

    /// Total bits consumed so far.
    #[must_use]
    pub(crate) fn bit_position(&self) -> usize {
        self.bit_pos
    }

    /// Total bits in the underlying slice.
    #[must_use]
    pub(crate) fn total_bits(&self) -> usize {
        self.data.len() * 8
    }

    fn fill(&mut self) {
        while self.cached <= 56 && self.byte_pos < self.data.len() {
            let byte = u64::from(self.data[self.byte_pos]);
            self.byte_pos += 1;
            // The valid bits occupy the top `cached` bits, so the
            // next byte lands just below them.
            self.cache |= byte << (64 - self.cached - 8);
            self.cached += 8;
        }
    }

    /// Peeks `n` bits (0..=32) without consuming them.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when fewer than `n` bits remain.
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn peek(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        if n > 32 {
            return Err(Error::InvalidData {
                offset: self.bit_pos / 8,
                detail: "mp3 bit peek wider than 32 bits",
            });
        }
        self.fill();
        if self.cached < n {
            return Err(Error::Truncated {
                needed: n as usize,
                had: self.cached as usize,
            });
        }
        Ok((self.cache >> (64 - n)) as u32)
    }

    /// Consumes `n` bits previously peeked.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when fewer than `n` bits are cached;
    /// callers peek first, so this only fires on internal misuse.
    pub(crate) fn flush(&mut self, n: usize) -> Result<()> {
        let n = u32::try_from(n).map_err(|_| Error::InvalidData {
            offset: self.bit_pos / 8,
            detail: "mp3 bit flush wider than u32",
        })?;
        if self.cached < n {
            return Err(Error::Truncated {
                needed: n as usize,
                had: self.cached as usize,
            });
        }
        self.cache <<= n;
        self.cached -= n;
        self.bit_pos += n as usize;
        Ok(())
    }

    /// Reads `n` bits (0..=32) as a `u32`, MSB-first.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when fewer than `n` bits remain.
    pub(crate) fn read_bits(&mut self, n: u32) -> Result<u32> {
        let value = self.peek(n)?;
        self.flush(n as usize)?;
        Ok(value)
    }

    /// Reads one bit.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when no bits remain.
    pub(crate) fn read_bit(&mut self) -> Result<bool> {
        Ok(self.read_bits(1)? != 0)
    }

    /// Moves to the absolute bit position `pos`, discarding the
    /// cache. Used to skip ancillary bits at the end of a granule:
    /// the next granule starts exactly at its `part_23_length`
    /// boundary, not where the Huffman walk stopped. The walk
    /// overshoots that boundary by one code (it only notices it
    /// has passed the limit after flushing the code), so the
    /// target may be behind the current position; any position
    /// inside the slice is a valid reposition.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when `pos` is past the end of the data.
    pub(crate) fn seek_bit(&mut self, pos: usize) -> Result<()> {
        if pos > self.total_bits() {
            return Err(Error::Truncated {
                needed: pos,
                had: self.total_bits(),
            });
        }
        self.byte_pos = pos / 8;
        self.cache = 0;
        self.cached = 0;
        self.bit_pos = pos;
        // The position may sit inside a byte; drop the leading bits
        // of that byte so the next peek starts exactly at `pos`.
        // `flush` advances `bit_pos`, so it is restored afterwards.
        let skip = pos % 8;
        if skip > 0 {
            self.fill();
            self.flush(skip)?;
            self.bit_pos = pos;
        }
        Ok(())
    }
}

/// `x^(4/3)` for a non-negative magnitude (ISO/IEC 11172-3 section
/// 2.4.3.4.1). Small values come straight from `MP3_POW43`; large
/// ones use the table with a linear fractional correction, which is
/// the reference decoder's approximation and is accurate far below
/// the conformance threshold.
#[allow(clippy::cast_precision_loss)]
// The casts below are safe by construction: `x` is non-negative
// on entry, and the corrected index is bounded by the table size
// (see the range argument in the module docs).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn pow_43(x: i32) -> Result<f32> {
    if x < 0 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "mp3 dequantize magnitude is negative",
        });
    }
    if x < 129 {
        let entry = MP3_POW43.get(16 + x as usize).ok_or(Error::InvalidData {
            offset: 0,
            detail: "mp3 pow43 table index out of range",
        })?;
        return Ok(*entry);
    }
    let mut value = x;
    let mut mult: f32 = 256.0;
    if value < 1024 {
        mult = 16.0;
        value <<= 3;
    }
    // `sign` is 0 or 64: the low six bits of `2 * value`, masked.
    let sign = (2 * value) & 64;
    let frac = (value & 63) as f32 - sign as f32;
    let frac = frac / ((value & !63) + sign) as f32;
    let index = 16 + ((value + sign) >> 6) as usize;
    let base = MP3_POW43.get(index).ok_or(Error::InvalidData {
        offset: 0,
        detail: "mp3 pow43 table index out of range",
    })?;
    Ok(base * (1.0 + frac * (4.0 / 3.0 + frac * (2.0 / 9.0))) * mult)
}

/// Decodes one Huffman pair `(x, y)` from `cache` using the
/// flattened tree at `codebook_base`, returning the two magnitudes
/// (before sign bits) and consuming the code bits. `linbits` is the
/// table's extra-bit count (0 when the table has none).
// The casts below are safe by construction: the tree walk only
// produces non-negative leaves and bit widths of at most 7, and
// magnitudes are bounded by the linbits just read.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
fn decode_pair(
    cache: &mut Mp3BitCache<'_>,
    codebook_base: usize,
    linbits: usize,
    scale: f32,
    out: &mut [f32],
    at: usize,
) -> Result<()> {
    let mut width = 5usize;
    let mut steps = 0usize;
    let mut leaf = {
        let peeked = cache.peek(width as u32)? as usize;
        *MP3_HUFFMAN_TABS
            .get(codebook_base + peeked)
            .ok_or(Error::InvalidData {
                offset: cache.bit_position() / 8,
                detail: "mp3 huffman code index out of range",
            })?
    };
    while leaf < 0 {
        steps += 1;
        if steps > MAX_HUFFMAN_WALK_STEPS {
            return Err(Error::InvalidData {
                offset: cache.bit_position() / 8,
                detail: "mp3 huffman code does not resolve",
            });
        }
        cache.flush(width)?;
        width = (leaf & 7) as usize;
        // `leaf >> 3` is negative (arithmetic shift), so subtracting
        // it adds the internal node's subtree offset.
        let index = cache.peek(width as u32)? as i32 - (i32::from(leaf) >> 3);
        let index = usize::try_from(index).map_err(|_| Error::InvalidData {
            offset: cache.bit_position() / 8,
            detail: "mp3 huffman code index is negative",
        })?;
        leaf = *MP3_HUFFMAN_TABS
            .get(codebook_base + index)
            .ok_or(Error::InvalidData {
                offset: cache.bit_position() / 8,
                detail: "mp3 huffman code index out of range",
            })?;
    }
    // A leaf's high byte is the code length; the low nibbles are
    // the `(x, y)` magnitudes, low nibble first.
    cache.flush((leaf >> 8) as usize)?;
    let mut value = leaf;
    for slot in 0..2 {
        let mut magnitude = i32::from(value & 0x0F);
        value >>= 4;
        if linbits > 0 && magnitude == 15 {
            magnitude += cache.read_bits(linbits as u32)? as i32;
            let negative = cache.peek(1)? != 0;
            let decoded = pow_43(magnitude)? * if negative { -1.0 } else { 1.0 };
            *out.get_mut(at + slot).ok_or(Error::InvalidData {
                offset: cache.bit_position() / 8,
                detail: "mp3 huffman output overruns the granule",
            })? = scale * decoded;
        } else {
            // The sign is the next bit in the stream, read before
            // it is consumed; the table's first half holds the
            // negatives, so the sign selects the half by index.
            let decoded = if magnitude == 0 {
                0.0
            } else {
                let negative = cache.peek(1)? != 0;
                let index = 16 + magnitude as usize - if negative { 16 } else { 0 };
                let entry = MP3_POW43.get(index).ok_or(Error::InvalidData {
                    offset: cache.bit_position() / 8,
                    detail: "mp3 pow43 table index out of range",
                })?;
                *entry
            };
            *out.get_mut(at + slot).ok_or(Error::InvalidData {
                offset: cache.bit_position() / 8,
                detail: "mp3 huffman output overruns the granule",
            })? = scale * decoded;
        }
        if magnitude != 0 {
            cache.flush(1)?;
        }
    }
    Ok(())
}

/// Decodes the Huffman section of one granule channel into `dst`
/// (576 frequency lines, zero-filled by the caller).
///
/// `sfbtab` is the scalefactor-band width table for this granule
/// (long, short, or mixed, chosen by the side info);
/// `table_select` / `region_count` are the granule's three regions;
/// `scf` holds the dequantizer scalefactors, one per band, from
/// `decode_scalefactors`; `granule_limit_bits` is the absolute bit
/// position where this granule channel's `part_23_length` ends.
/// On success the cache is positioned exactly at that limit, so the
/// caller can start the next channel there.
///
/// # Errors
///
/// Returns `Truncated` when the bitstream ends mid-code,
/// `InvalidData` when a code, table index, or band index is out of
/// range, or when the decoded values would overrun the granule.
// The casts below are safe by construction: `big_values` is
// validated to at most 288 by the side-info parser, band widths
// come from the scalefactor tables (at most 192), and count-1
// leaves are table bytes.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::too_many_lines
)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_huffman(
    dst: &mut [f32; 576],
    cache: &mut Mp3BitCache<'_>,
    sfbtab: &[u8],
    table_select: [u8; 3],
    region_count: [u8; 3],
    big_values: usize,
    count1_table: bool,
    scf: &[f32],
    granule_limit_bits: usize,
) -> Result<()> {
    let mut big_val_cnt = big_values as i32;
    let mut region = 0usize;
    let mut sfb_idx = 0usize;
    let mut scf_idx = 0usize;
    let mut at = 0usize;
    // Current scalefactor: set per band in the big-values loop and
    // carried into the count-1 region, whose first pairs share the
    // last big-values band until a reload fires.
    let mut one = 0.0f32;
    while big_val_cnt > 0 {
        let table_num = *table_select.get(region).ok_or(Error::InvalidData {
            offset: cache.bit_position() / 8,
            detail: "mp3 huffman region index out of range",
        })? as usize;
        let mut sfb_cnt = i32::from(*region_count.get(region).ok_or(Error::InvalidData {
            offset: cache.bit_position() / 8,
            detail: "mp3 huffman region index out of range",
        })?);
        region += 1;
        let codebook_base = *MP3_HUFFMAN_TABINDEX
            .get(table_num)
            .ok_or(Error::InvalidData {
                offset: cache.bit_position() / 8,
                detail: "mp3 huffman table index out of range",
            })? as usize;
        let linbits = *MP3_LINBITS.get(table_num).ok_or(Error::InvalidData {
            offset: cache.bit_position() / 8,
            detail: "mp3 huffman table index out of range",
        })? as usize;
        loop {
            let width = *sfbtab.get(sfb_idx).ok_or(Error::InvalidData {
                offset: cache.bit_position() / 8,
                detail: "mp3 scalefactor band index out of range",
            })? as usize;
            sfb_idx += 1;
            let pairs_in_band = width / 2;
            if pairs_in_band == 0 {
                return Err(Error::InvalidData {
                    offset: cache.bit_position() / 8,
                    detail: "mp3 huffman band has zero width while big values remain",
                });
            }
            let mut pairs_to_decode = big_val_cnt.min(pairs_in_band as i32);
            one = *scf.get(scf_idx).ok_or(Error::InvalidData {
                offset: cache.bit_position() / 8,
                detail: "mp3 scalefactor index out of range",
            })?;
            scf_idx += 1;
            while pairs_to_decode > 0 {
                decode_pair(cache, codebook_base, linbits, one, dst, at)?;
                at += 2;
                pairs_to_decode -= 1;
            }
            big_val_cnt -= pairs_in_band as i32;
            sfb_cnt -= 1;
            if !(big_val_cnt > 0 && sfb_cnt >= 0) {
                break;
            }
        }
    }

    // Count-1 region: quads of values that are each -1, 0, or +1.
    let count1 = if count1_table {
        MP3_COUNT1_TAB33
    } else {
        MP3_COUNT1_TAB32
    };
    // Pairs left in the current band before the next scalefactor
    // reload; the big-values loop overshoots the band boundary, so
    // this starts one past the overshoot.
    let mut np = 1 - big_val_cnt;
    loop {
        let first = i32::from(
            *count1
                .get(cache.peek(4)? as usize)
                .ok_or(Error::InvalidData {
                    offset: cache.bit_position() / 8,
                    detail: "mp3 count1 code index out of range",
                })?,
        );
        let mut leaf = first;
        if leaf & 8 == 0 {
            let extra = (leaf & 3) as u32;
            let combined = cache.peek(4 + extra)?;
            let next_bits = if extra == 0 {
                0
            } else {
                combined & ((1u32 << extra) - 1)
            };
            let index = (leaf >> 3) as usize + next_bits as usize;
            leaf = i32::from(*count1.get(index).ok_or(Error::InvalidData {
                offset: cache.bit_position() / 8,
                detail: "mp3 count1 code index out of range",
            })?);
        }
        cache.flush((leaf & 7) as usize)?;
        if cache.bit_position() > granule_limit_bits {
            break;
        }
        // One quad is two pairs; a scalefactor reload happens
        // before each pair, and the pair uses the reloaded factor.
        for pair_start in [0usize, 2usize] {
            np -= 1;
            if np == 0 {
                let width = *sfbtab.get(sfb_idx).ok_or(Error::InvalidData {
                    offset: cache.bit_position() / 8,
                    detail: "mp3 scalefactor band index out of range",
                })? as usize;
                sfb_idx += 1;
                np = (width / 2) as i32;
                if np == 0 {
                    cache.seek_bit(granule_limit_bits)?;
                    return Ok(());
                }
                one = *scf.get(scf_idx).ok_or(Error::InvalidData {
                    offset: cache.bit_position() / 8,
                    detail: "mp3 scalefactor index out of range",
                })?;
                scf_idx += 1;
            }
            for s in pair_start..pair_start + 2 {
                if leaf & (128 >> s) != 0 {
                    let negative = cache.peek(1)? != 0;
                    *dst.get_mut(at + s).ok_or(Error::InvalidData {
                        offset: cache.bit_position() / 8,
                        detail: "mp3 huffman output overruns the granule",
                    })? = if negative { -one } else { one };
                    cache.flush(1)?;
                }
            }
        }
        at += 4;
    }
    cache.seek_bit(granule_limit_bits)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pow43_matches_known_powers() {
        // 2^(4/3) = 2.519842, 8^(4/3) = 16, 27^(4/3) = 81.
        assert!((pow_43(2).unwrap() - 2.519_842).abs() < 1e-5);
        assert!((pow_43(8).unwrap() - 16.0).abs() < 1e-5);
        assert!((pow_43(27).unwrap() - 81.0).abs() < 1e-5);
        assert_eq!(pow_43(0).unwrap(), 0.0);
        assert!((pow_43(1000).unwrap() - 10000.0).abs() < 1.0);
    }

    #[test]
    fn bit_cache_peeks_without_consuming() {
        let mut cache = Mp3BitCache::new(&[0b1010_0110, 0b1111_0000]);
        assert_eq!(cache.peek(4).unwrap(), 0b1010);
        assert_eq!(cache.peek(4).unwrap(), 0b1010);
        assert_eq!(cache.read_bits(4).unwrap(), 0b1010);
        assert_eq!(cache.peek(4).unwrap(), 0b0110);
        assert_eq!(cache.bit_position(), 4);
    }

    #[test]
    fn bit_cache_seek_skips_to_granule_limit() {
        let mut cache = Mp3BitCache::new(&[0xFF, 0xFF, 0xFF]);
        assert_eq!(cache.read_bits(3).unwrap(), 0b111);
        cache.seek_bit(20).unwrap();
        assert_eq!(cache.bit_position(), 20);
        assert_eq!(cache.read_bits(4).unwrap(), 0b1111);
    }

    #[test]
    fn bit_cache_truncates_past_end() {
        let mut cache = Mp3BitCache::new(&[0xFF]);
        assert_eq!(cache.read_bits(8).unwrap(), 0xFF);
        assert!(matches!(cache.read_bits(1), Err(Error::Truncated { .. })));
    }
}
