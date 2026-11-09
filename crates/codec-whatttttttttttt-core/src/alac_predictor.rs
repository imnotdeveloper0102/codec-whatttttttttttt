//! ALAC adaptive-Golomb (Rice) decode and dynamic predictor.
//!
//! Ported from Apple's ALAC reference implementation
//! (`macosforge/alac`, Apache License 2.0): `codec/ag_dec.c`
//! (`dyn_decomp`, `dyn_get`, `dyn_get_32bit`), `codec/aglib.h`
//! (the `QBSHIFT`/`QB`/`MAX_PREFIX_*` constants) and
//! `codec/dp_dec.c` (`unpc_block`). The code here is a fresh Rust
//! implementation of the same algorithms, not a copy of the C
//! source: arithmetic that can overflow a 32-bit word in C is done
//! in `i64`/`u64` or with wrapping operations here, and every
//! bitstream read is bounds-checked against the packet length
//! (Apple's `ag_dec.c` reads 32-bit words past the end of the
//! buffer and relies on caller padding; this module pads with
//! zeroes explicitly and still refuses to start a sample past
//! the real end of the packet).
//!
//! The Rice decoder is adaptive: the Golomb parameter `k` is
//! derived from a running mean `mb` (see `aglib.h`: `k` is
//! `lg3a(mb >> 9)`, capped by `kb`), and long runs of zero
//! residuals are coded as a single count (`zmode`). The predictor
//! (`unpc_block`) reconstructs samples from residuals with an
//! adaptive FIR filter whose coefficients are corrected by the
//! sign of each residual (`dp_dec.c`); the order-4 and order-8
//! special cases in the C source are algebraically identical to
//! the general case (the same sum and the same coefficient
//! updates, only unrolled), so only the general case is
//! implemented here.

// Casts in this module are bounded by construction: values
// cast to a narrower integer come from bit reads of a known
// width, Rice values already range-checked against the frame
// size, or the predictor's `i64` intermediates (the reference
// decoder's C code does the same arithmetic in 32-bit words
// with wrapping). The pedantic cast lints do not find real
// truncation risks here; the bounds are checked where the
// values are read, not where they are cast.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_precision_loss)]

use crate::error::{Error, Result};

/// `QB` shift: the mean `mb` is tracked in Q9 fixed point
/// (`aglib.h`: `QBSHIFT 9`, `QB (1 << QBSHIFT)`).
const QB_SHIFT: u32 = 9;
/// `QB`, the fixed-point one (512).
const QB: u32 = 1 << QB_SHIFT;
/// Mean-tracking multiplier shift (`aglib.h`: `MMULSHIFT 2`).
const MMUL_SHIFT: u32 = 2;
/// Mean denominator shift (`aglib.h`: `MDENSHIFT (QBSHIFT -
/// MMULSHIFT - 1)` = 6).
const MDEN_SHIFT: u32 = QB_SHIFT - MMUL_SHIFT - 1;
/// Mean offset (`aglib.h`: `MOFF (1 << (MDENSHIFT - 2))` = 16).
const MOFF: u32 = 1 << (MDEN_SHIFT - 2);
/// Leading-zero offset for the zero-run parameter
/// (`aglib.h`: `BITOFF 24`).
const BITOFF: i32 = 24;
/// Largest unary prefix before the escape path
/// (`aglib.h`: `MAX_PREFIX_16` / `MAX_PREFIX_32` = 9).
const MAX_PREFIX: u32 = 9;
/// Bits read on the 16-bit escape path
/// (`aglib.h`: `MAX_DATATYPE_BITS_16` = 16).
const MAX_DATATYPE_BITS_16: u32 = 16;
/// Mean clamp (`aglib.h`: `N_MAX_MEAN_CLAMP` /
/// `N_MEAN_CLAMP_VAL` = 0xffff).
const MAX_MEAN_CLAMP: u32 = 0xffff;

/// MSB-first bit cursor over one ALAC packet.
///
/// The packet is stored with eight zero bytes appended so the
/// Rice decoder's 32-bit lookahead never reads out of bounds;
/// `limit_bits` is the length of the real packet, and ordinary
/// reads refuse to cross it. Rice reads use `read_bits_padded`,
/// which may read into the zero padding (matching the reference
/// decoder, whose caller pads the input the same way), while
/// `dyn_decompress` still refuses to *start* a sample past the
/// real end.
#[derive(Debug, Clone)]
pub struct AlacBitReader<'a> {
    data: &'a [u8],
    /// Length of the real packet, in bits.
    limit_bits: usize,
    /// Current position, in bits from the start of the packet.
    bit_pos: usize,
}

impl<'a> AlacBitReader<'a> {
    /// Creates a cursor over `padded` (packet bytes plus zero
    /// padding) with `original_len` real packet bytes.
    #[must_use]
    pub fn new(padded: &'a [u8], original_len: usize) -> Self {
        Self {
            data: padded,
            limit_bits: original_len * 8,
            bit_pos: 0,
        }
    }

    /// Current bit position.
    #[must_use]
    pub fn bit_position(&self) -> usize {
        self.bit_pos
    }

    /// Real packet length, in bits.
    #[must_use]
    pub fn limit_bits(&self) -> usize {
        self.limit_bits
    }

    /// Moves the cursor to an absolute bit position. The caller
    /// is responsible for the position being meaningful (used to
    /// rewind one bit in the Rice decoder, where the reference
    /// implementation decrements its bit position directly).
    pub fn set_position(&mut self, bit_pos: usize) {
        self.bit_pos = bit_pos;
    }

    /// Skips `n` bits, refusing to cross the real packet end.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when fewer than `n` bits remain.
    pub fn advance(&mut self, n: usize) -> Result<()> {
        if self.bit_pos + n > self.limit_bits {
            return Err(Error::Truncated {
                needed: n,
                had: self.limit_bits.saturating_sub(self.bit_pos),
            });
        }
        self.bit_pos += n;
        Ok(())
    }

    /// Aligns the cursor to the next byte boundary.
    pub fn align_to_byte(&mut self) {
        let remainder = self.bit_pos % 8;
        if remainder != 0 {
            self.bit_pos += 8 - remainder;
        }
    }

    fn read_bit_from(&mut self, limit: usize) -> Result<bool> {
        if self.bit_pos >= limit {
            return Err(Error::Truncated { needed: 1, had: 0 });
        }
        let byte = self.data.get(self.bit_pos / 8).copied().unwrap_or(0);
        let bit = (byte >> (7 - (self.bit_pos % 8))) & 1;
        self.bit_pos += 1;
        Ok(bit == 1)
    }

    fn read_bits_from(&mut self, n: u32, limit: usize) -> Result<u32> {
        if n > 32 {
            return Err(Error::InvalidData {
                offset: self.bit_pos / 8,
                detail: "alac bit read wider than 32 bits",
            });
        }
        if n as usize > limit.saturating_sub(self.bit_pos) {
            return Err(Error::Truncated {
                needed: n as usize,
                had: limit.saturating_sub(self.bit_pos),
            });
        }
        let mut value: u32 = 0;
        for _ in 0..n {
            let byte = self.data.get(self.bit_pos / 8).copied().unwrap_or(0);
            let bit = (byte >> (7 - (self.bit_pos % 8))) & 1;
            value = (value << 1) | u32::from(bit);
            self.bit_pos += 1;
        }
        Ok(value)
    }

    /// Reads `n` bits MSB-first, refusing to cross the real
    /// packet end.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when fewer than `n` bits remain, and
    /// `InvalidData` when `n > 32`.
    pub fn read_bits(&mut self, n: u32) -> Result<u32> {
        self.read_bits_from(n, self.limit_bits)
    }

    /// Reads one bit, refusing to cross the real packet end.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when no bits remain.
    pub fn read_bit(&mut self) -> Result<bool> {
        self.read_bit_from(self.limit_bits)
    }

    /// Reads `n` bits MSB-first, allowing the read to extend
    /// into the zero padding past the real packet end. Only the
    /// Rice decoder uses this: its 32-bit lookahead may cross
    /// the end by a few bits, and the padding reads as zero,
    /// exactly as in the reference decoder.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when fewer than `n` bits remain even
    /// in the padded buffer, and `InvalidData` when `n > 32`.
    pub fn read_bits_padded(&mut self, n: u32) -> Result<u32> {
        self.read_bits_from(n, self.data.len() * 8)
    }

    /// Reads one bit, allowing the read to extend into the zero
    /// padding past the real packet end.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when no bits remain even in the
    /// padded buffer.
    pub fn read_bit_padded(&mut self) -> Result<bool> {
        self.read_bit_from(self.data.len() * 8)
    }

    /// Reads `n` bits as a signed value (sign-extended).
    ///
    /// # Errors
    ///
    /// Returns the same errors as `read_bits`.
    pub fn read_signed(&mut self, n: u32) -> Result<i32> {
        let raw = self.read_bits(n)?;
        if n == 0 {
            return Ok(0);
        }
        let shift = 32 - n;
        Ok((raw << shift) as i32 >> shift)
    }
}

/// Adaptive-Golomb parameters (`AGParamRec` in `aglib.h`).
/// Only the fields the decoder uses are kept: `fw`, `sw` and
/// `maxrun` are write-only in the reference decoder.
#[derive(Debug, Clone, Copy)]
pub struct AgParams {
    /// Initial mean (`mb0`), from the cookie's `mb` field.
    mb: u32,
    /// History multiplier (`pb`), scaled per channel by
    /// `pb_factor`: `(pb * pb_factor) / 4`.
    pb: u32,
    /// Rice parameter cap (`kb`), from the cookie.
    kb: u32,
    /// `(1 << kb) - 1`, the mask for the zero-run parameter.
    wb: u32,
}

impl AgParams {
    /// Builds parameters as `set_ag_params` does in `ag_dec.c`:
    /// `wb = (1 << kb) - 1`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when `kb` is too large for the
    /// `wb` mask to fit in a `u32`.
    pub fn new(mb: u32, pb: u32, kb: u32) -> Result<Self> {
        if kb >= 32 {
            return Err(Error::InvalidArgument(
                "alac rice parameter kb must be below 32",
            ));
        }
        Ok(Self {
            mb,
            pb,
            kb,
            wb: (1u32 << kb) - 1,
        })
    }
}

/// Counts leading zero bits of a `u32` (`lead` in `ag_dec.c`).
fn lead(value: u32) -> u32 {
    value.leading_zeros()
}

/// `lg3a` in `ag_dec.c`: `floor(log2(x + 3))`, via the
/// leading-zero count.
fn lg3a(x: u32) -> u32 {
    31 - lead(x.wrapping_add(3))
}

/// Counts consecutive 1 bits from the cursor, stopping at the
/// first 0 bit or after `MAX_PREFIX` ones. Returns the count
/// and whether a terminating 0 was consumed. The reference
/// implementation counts leading ones in a loaded 32-bit word;
/// reading bit by bit gives the same count, and the escape
/// threshold (`MAX_PREFIX`) bounds the lookahead.
fn count_ones(reader: &mut AlacBitReader<'_>) -> Result<(u32, bool)> {
    let mut count: u32 = 0;
    while count < MAX_PREFIX {
        if reader.read_bit_padded()? {
            count += 1;
        } else {
            return Ok((count, true));
        }
    }
    Ok((count, false))
}

/// `dyn_get` in `ag_dec.c`: decodes one zero-run count. The
/// unary prefix is a run of 1 bits terminated by a 0; a prefix
/// of `MAX_PREFIX` ones escapes to a raw 16-bit value. In the
/// normal path the remainder `v` is `k` bits; when `v < 2` the
/// last remainder bit is not consumed (the reference
/// implementation decrements its bit position), so the cursor
/// is rewound by one bit here.
fn dyn_get(reader: &mut AlacBitReader<'_>, m: u32, k: u32) -> Result<u32> {
    let (pre, _terminated) = count_ones(reader)?;
    if pre >= MAX_PREFIX {
        return reader.read_bits_padded(MAX_DATATYPE_BITS_16);
    }
    let v = reader.read_bits_padded(k)?;
    let mut result = i64::from(pre) * i64::from(m) + i64::from(v) - 1;
    if v < 2 {
        result -= i64::from(v) - 1;
        reader.set_position(reader.bit_position().saturating_sub(1));
    }
    u32::try_from(result).map_err(|_| Error::InvalidData {
        offset: reader.bit_position() / 8,
        detail: "alac rice value out of range",
    })
}

/// `dyn_get_32bit` in `ag_dec.c`: decodes one Rice value. The
/// unary prefix is a run of 1 bits terminated by a 0; a prefix
/// of `MAX_PREFIX` ones escapes to a raw `maxbits`-bit value.
/// When `k == 1` the value is the prefix itself. Otherwise the
/// remainder `v` is `k` bits, of which the last is only
/// consumed when `v >= 2`.
fn dyn_get_32bit(reader: &mut AlacBitReader<'_>, m: u32, k: u32, maxbits: u32) -> Result<u32> {
    let (pre, _terminated) = count_ones(reader)?;
    if pre >= MAX_PREFIX {
        return reader.read_bits_padded(maxbits);
    }
    if k == 1 {
        return Ok(pre);
    }
    let v = reader.read_bits_padded(k)?;
    reader.set_position(reader.bit_position().saturating_sub(1));
    let mut result = u64::from(pre) * u64::from(m);
    if v >= 2 {
        result += u64::from(v) - 1;
        reader.set_position(reader.bit_position() + 1);
    }
    u32::try_from(result).map_err(|_| Error::InvalidData {
        offset: reader.bit_position() / 8,
        detail: "alac rice value out of range",
    })
}

/// Decodes `num_samples` adaptive-Golomb values from the
/// cursor (`dyn_decomp` in `ag_dec.c`). The least significant
/// bit of each decoded value is its sign bit: odd values are
/// negative. A zero run (`zmode`) follows a sample whose
/// running mean dropped low enough, and is coded as a single
/// count.
///
/// # Errors
///
/// Returns `Truncated` when the cursor runs out of real packet
/// bits before a sample starts, and `InvalidData` when a zero
/// run would overrun the requested sample count.
pub fn dyn_decompress(
    reader: &mut AlacBitReader<'_>,
    params: &AgParams,
    num_samples: usize,
    max_size: u32,
) -> Result<Vec<i32>> {
    let mut out: Vec<i32> = Vec::with_capacity(num_samples);
    let mut mb = params.mb;
    let mut zmode: u32 = 0;
    let mut c: usize = 0;
    while c < num_samples {
        if reader.bit_position() >= reader.limit_bits() {
            return Err(Error::Truncated { needed: 1, had: 0 });
        }
        let m = mb >> QB_SHIFT;
        let k = lg3a(m).min(params.kb);
        let m = (1u32 << k) - 1;
        let n = dyn_get_32bit(reader, m, k, max_size)?;
        let ndecode = n.wrapping_add(zmode);
        let multiplier: i32 = if ndecode & 1 == 1 { -1 } else { 1 };
        let del = (((u64::from(ndecode) + 1) >> 1) as i64) * i64::from(multiplier);
        out.push(del as i32);
        c += 1;
        mb = params
            .pb
            .wrapping_mul(n.wrapping_add(zmode))
            .wrapping_add(mb)
            .wrapping_sub((params.pb.wrapping_mul(mb)) >> QB_SHIFT);
        if n > MAX_MEAN_CLAMP {
            mb = MAX_MEAN_CLAMP;
        }
        zmode = 0;
        if ((mb << MMUL_SHIFT) < QB) && c < num_samples {
            zmode = 1;
            let k = lead(mb) as i32 - BITOFF + ((mb + MOFF) >> MDEN_SHIFT) as i32;
            let k = u32::try_from(k).map_err(|_| Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "alac zero-run rice parameter out of range",
            })?;
            let mz = ((1u32 << k) - 1) & params.wb;
            let n = dyn_get(reader, mz, k)?;
            if c + n as usize > num_samples {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "alac zero run exceeds frame sample count",
                });
            }
            out.resize(out.len() + n as usize, 0);
            c += n as usize;
            if n >= 65_535 {
                zmode = 0;
            }
            mb = 0;
        }
    }
    Ok(out)
}

/// Sign-extends `value` to `chan_bits` bits (`(value <<
/// chanshift) >> chanshift` in `dp_dec.c`, where
/// `chanshift = 32 - chanbits`).
fn sign_extend(value: i32, chan_bits: u32) -> i32 {
    let shift = 32 - chan_bits;
    value.wrapping_shl(shift).wrapping_shr(shift)
}

/// Reconstructs samples from Rice residuals (`unpc_block` in
/// `dp_dec.c`).
///
/// `num_active == 0` copies the residuals through;
/// `num_active == 31` is the running-sum mode used when the
/// predictor mode is nonzero (the caller first runs this mode
/// in place, then runs the general mode over the result);
/// otherwise an adaptive FIR filter with `coefs.len()`
/// coefficients and denominator shift `den_shift` is applied,
/// correcting the coefficients by the sign of each residual.
/// The order-4 and order-8 fast paths in the C source compute
/// the same sum and the same updates as the general path, so
/// they are not duplicated here.
///
/// # Errors
///
/// Returns `InvalidArgument` when `chan_bits` is outside
/// 1..=32, or when the general path is asked for coefficients
/// it was not given.
#[allow(
    clippy::cast_possible_wrap,
    reason = "deliberate wrapping casts match the C reference, whose values stay in range for valid streams"
)]
pub fn unpc_block(
    residuals: &[i32],
    coefs: &[i16],
    num_active: usize,
    chan_bits: u32,
    den_shift: u32,
) -> Result<Vec<i32>> {
    if chan_bits == 0 || chan_bits > 32 {
        return Err(Error::InvalidArgument(
            "alac channel bit width must be between 1 and 32",
        ));
    }
    let num = residuals.len();
    let mut out: Vec<i32> = vec![0; num];
    if num == 0 {
        return Ok(out);
    }
    out[0] = residuals[0];
    if num_active == 0 {
        if num > 1 {
            out[1..].copy_from_slice(&residuals[1..]);
        }
        return Ok(out);
    }
    if num_active == 31 {
        let mut prev = out[0];
        for j in 1..num {
            let del = residuals[j].wrapping_add(prev);
            prev = sign_extend(del, chan_bits);
            out[j] = prev;
        }
        return Ok(out);
    }
    if coefs.len() < num_active {
        return Err(Error::InvalidArgument(
            "alac predictor coefficient count below num_active",
        ));
    }
    let mut coefs: Vec<i64> = coefs[..num_active].iter().map(|&c| i64::from(c)).collect();
    let denhalf: i64 = if den_shift == 0 {
        0
    } else {
        1i64 << (den_shift - 1)
    };
    let first = num_active.min(num - 1);
    for j in 1..=first {
        let del = i64::from(residuals[j]) + i64::from(out[j - 1]);
        out[j] = sign_extend(del as i32, chan_bits);
    }
    let lim = num_active + 1;
    for j in lim..num {
        let top = i64::from(out[j - lim]);
        let mut sum: i64 = 0;
        for k in 0..num_active {
            sum += coefs[k] * (i64::from(out[j - 1 - k]) - top);
        }
        let del = i64::from(residuals[j]);
        let mut del0 = del;
        let sg = del.signum();
        let reconstructed = del + top + ((sum + denhalf) >> den_shift);
        out[j] = sign_extend(reconstructed as i32, chan_bits);
        if sg > 0 {
            for k in (0..num_active).rev() {
                let dd = top - i64::from(out[j - 1 - k]);
                let sgn = dd.signum();
                coefs[k] -= sgn;
                del0 -= (num_active - k) as i64 * ((sgn * dd) >> den_shift);
                if del0 <= 0 {
                    break;
                }
            }
        } else if sg < 0 {
            for k in (0..num_active).rev() {
                let dd = top - i64::from(out[j - 1 - k]);
                let sgn = dd.signum();
                coefs[k] += sgn;
                del0 -= (num_active - k) as i64 * (((-sgn) * dd) >> den_shift);
                if del0 >= 0 {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// Channel decorrelation for a stereo pair (`unmix16` in
/// `matrix_dec.c`): `mixres == 0` interleaves the channels
/// unchanged; otherwise the generalized mid/side inverse is
/// `l = u + v - ((mixres * v) >> mixbits)`, `r = l - v`.
/// The 16-bit results are the low 16 bits of the computed
/// values, as in the C cast to `int16_t`.
#[must_use]
pub fn unmix_stereo_16(u: &[i32], v: &[i32], mix_bits: u32, mix_res: i32) -> Vec<i32> {
    let mut out: Vec<i32> = Vec::with_capacity(u.len() * 2);
    if mix_res != 0 {
        for (index, &u_val) in u.iter().enumerate() {
            let v_val = i64::from(v.get(index).copied().unwrap_or(0));
            let l = i64::from(u_val) + v_val - ((i64::from(mix_res) * v_val) >> mix_bits);
            let r = l - v_val;
            out.push(i32::from(l as i16));
            out.push(i32::from(r as i16));
        }
    } else {
        for (index, &u_val) in u.iter().enumerate() {
            out.push(i32::from(u_val as i16));
            out.push(i32::from(v.get(index).copied().unwrap_or(0) as i16));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_reader_reads_msb_first_and_rewinds() {
        let padded = [0b1010_0110u8, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut reader = AlacBitReader::new(&padded, 1);
        assert_eq!(reader.read_bits(4).unwrap(), 0b1010);
        reader.set_position(0);
        assert_eq!(reader.read_bits(8).unwrap(), 0b1010_0110);
        assert!(reader.read_bits(1).is_err());
    }

    #[test]
    fn sign_extend_keeps_negative_values() {
        assert_eq!(sign_extend(0x7FFF, 16), 0x7FFF);
        assert_eq!(sign_extend(0xFFFF, 16), -1);
        assert_eq!(sign_extend(0x100, 9), -256);
    }

    #[test]
    fn unpc_block_order_zero_copies_residuals() {
        let residuals = [10, -3, 7, 0];
        let out = unpc_block(&residuals, &[], 0, 16, 5).unwrap();
        assert_eq!(out, residuals);
    }

    #[test]
    fn unpc_block_mode31_is_running_sum() {
        let residuals = [5, 2, -1, 3];
        let out = unpc_block(&residuals, &[], 31, 16, 0).unwrap();
        assert_eq!(out, [5, 7, 6, 9]);
    }

    #[test]
    fn unpc_block_first_samples_are_cumulative() {
        // With two active coefficients the first two samples
        // after the seed are plain cumulative sums.
        let residuals = [100, 10, -5, 0, 0];
        let out = unpc_block(&residuals, &[0, 0], 2, 16, 5).unwrap();
        assert_eq!(out[0], 100);
        assert_eq!(out[1], 110);
        assert_eq!(out[2], 105);
    }
}
