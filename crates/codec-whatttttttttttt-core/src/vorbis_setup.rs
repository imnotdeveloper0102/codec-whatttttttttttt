//! Vorbis setup headers (Vorbis I specification sections 3, 4.2,
//! 5, 6.2.1, 7.2.2, 8.6.1).
//!
//! This module parses the three Vorbis headers (identification,
//! comment, setup) and the components the setup header configures:
//! codebooks (Huffman + VQ lookup), floors (type 0 header only,
//! type 1 full), residues (types 0/1/2), mappings and modes.
//! Audio packet decode lives in `vorbis_packet.rs`.
//!
//! Library limits that are stricter than the specification (to bound
//! memory on hostile input) are marked as such: a codebook may have
//! at most 16384 entries, and a VQ lookup table at most 1048576
//! values. Real libvorbis codebooks are a few hundred entries at
//! most, so these caps are far above anything a compliant encoder
//! produces.

use crate::error::{Error, Result};
use crate::huffman::HuffmanTable;
use crate::limits;
use crate::lsb_bitreader::{ilog, LsbBitReader};

/// Largest codebook entry count accepted (library limit, not a
/// specification limit; see the module docs).
const MAX_CODEBOOK_ENTRIES: u32 = 16384;

/// Largest VQ lookup table accepted (library limit).
const MAX_LOOKUP_VALUES: usize = 1_048_576;

/// Largest floor 1 X list (Vorbis I spec section 7.2.2).
const MAX_FLOOR1_VALUES: usize = 65;

/// Unpacks a Vorbis packed float32 (Vorbis I spec section 9.2.2):
/// mantissa is bits 0..20, sign is bit 31, exponent is bits 21..30,
/// value is `mantissa * 2^(exponent - 788)`. Computed through `f64`
/// so the exponent scaling is exact, mirroring the reference
/// `ldexp` implementation, then cast to `f32`. The exponent field
/// is 10 bits (at most 1023), so the `i32` conversion is exact;
/// the `f64` to `f32` cast is inherent to the `f32` result type.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
#[must_use]
pub fn float32_unpack(packed: u32) -> f32 {
    let mut mantissa = f64::from(packed & 0x1F_FFFF);
    let exponent = i32::try_from((packed & 0x7FE0_0000) >> 21).unwrap_or(0);
    if packed & 0x8000_0000 != 0 {
        mantissa = -mantissa;
    }
    (mantissa * 2f64.powi(exponent - 788)) as f32
}

/// `lookup1_values(entries, dimensions)` (Vorbis I spec section
/// 9.2.3): the greatest integer whose `dimensions`-th power is at
/// most `entries`. Computed by binary search with overflow-safe
/// multiplication.
#[must_use]
pub fn lookup1_values(entries: u32, dimensions: u32) -> u32 {
    if entries == 0 || dimensions == 0 {
        return 0;
    }
    if dimensions == 1 {
        return entries;
    }
    // The root is at most entries, and at least 1.
    let mut low: u64 = 1;
    let mut high: u64 = u64::from(entries);
    while low < high {
        let mid = (low + high).div_ceil(2);
        // Compute mid^dimensions, bailing out above entries.
        let mut power: u64 = 1;
        let mut overflow = false;
        for _ in 0..dimensions {
            match power.checked_mul(mid) {
                Some(value) if value <= u64::from(entries) => power = value,
                _ => {
                    overflow = true;
                    break;
                }
            }
        }
        if overflow {
            high = mid - 1;
        } else {
            low = mid;
        }
    }
    u32::try_from(low).unwrap_or(u32::MAX)
}

/// A Vorbis codebook (Vorbis I spec section 3.2.1): a Huffman table
/// for entropy decoding plus, for lookup types 1 and 2, a VQ value
/// table used to unpack vectors.
#[derive(Debug, Clone)]
pub struct Codebook {
    /// Number of scalars in each VQ vector.
    pub dimensions: u16,
    /// Number of entries (possible codewords).
    pub entries: u32,
    /// Lookup type: 0 (no VQ mapping), 1 (lattice), 2 (explicit).
    pub lookup_type: u8,
    /// `sequence_p` flag for VQ lookup types 1 and 2.
    pub sequence_p: bool,
    /// Precomputed base VQ values: `multiplicand * delta + minimum`
    /// for each lookup value (section 3.3). The `sequence_p`
    /// accumulation is applied at lookup time, not here.
    pub lookup_values: Vec<f32>,
    /// Huffman table, or `None` for a single-entry codebook (the
    /// errata 20150226 special case).
    table: Option<HuffmanTable>,
    /// The single used entry, for a single-entry codebook.
    single_entry: Option<u32>,
}

impl Codebook {
    /// True when the codebook has a VQ value mapping (lookup type
    /// 1 or 2). A codebook without one cannot be used in a VQ
    /// context (section 3.3).
    #[must_use]
    pub fn has_lookup(&self) -> bool {
        self.lookup_type != 0
    }

    /// Decodes one scalar (an entry index) from `reader` in scalar
    /// context. For a single-entry codebook this always returns the
    /// single entry and sinks one bit, tolerating a 0 or 1 bit
    /// (errata 20150226).
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when the input ends, and `InvalidData`
    /// when the bits match no code.
    pub fn decode_scalar(&self, reader: &mut LsbBitReader<'_>) -> Result<u32> {
        if let Some(entry) = self.single_entry {
            // Sink one bit; the value is ignored (0 and 1 both
            // return the single entry).
            let _ = reader.read_bit()?;
            return Ok(entry);
        }
        match &self.table {
            Some(table) => table.decode(reader),
            None => Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "codebook has neither a Huffman table nor a single entry",
            }),
        }
    }

    /// Decodes one VQ vector into `out`, which must have exactly
    /// `dimensions` elements (section 3.3).
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when `out` has the wrong length,
    /// `InvalidData` when the codebook has no value mapping or a
    /// lookup value is out of range, and `Truncated` when the input
    /// ends.
    pub fn decode_vector(&self, reader: &mut LsbBitReader<'_>, out: &mut [f32]) -> Result<()> {
        if out.len() != usize::from(self.dimensions) {
            return Err(Error::InvalidArgument(
                "VQ output vector length must equal codebook dimensions",
            ));
        }
        if self.lookup_type == 0 {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "codebook with lookup type 0 used in a VQ context",
            });
        }
        if self.lookup_values.is_empty() {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "codebook lookup table is empty",
            });
        }
        let entry = self.decode_scalar(reader)?;
        let mut last: f32 = 0.0;
        match self.lookup_type {
            1 => {
                // Lattice VQ (section 3.3): the entry is decomposed
                // in base `lookup_values.len()`, one digit per
                // dimension.
                let base = self.lookup_values.len() as u64;
                let mut index_divisor: u64 = 1;
                for value in out.iter_mut() {
                    let offset = if index_divisor > u64::from(entry) {
                        0
                    } else {
                        (u64::from(entry) / index_divisor) % base
                    };
                    let base_value = self.lookup_values[usize::try_from(offset).unwrap_or(0)];
                    *value = base_value + last;
                    if self.sequence_p {
                        last = *value;
                    }
                    index_divisor = index_divisor.saturating_mul(base);
                }
            }
            2 => {
                // Explicit VQ (section 3.3): the entry selects a
                // contiguous run of `dimensions` values.
                let start = (entry as usize)
                    .checked_mul(usize::from(self.dimensions))
                    .ok_or(Error::Overflow("VQ lookup offset exceeds address space"))?;
                for (i, value) in out.iter_mut().enumerate() {
                    let base_value =
                        *self
                            .lookup_values
                            .get(start + i)
                            .ok_or(Error::InvalidData {
                                offset: reader.bit_position() / 8,
                                detail: "VQ lookup value out of range",
                            })?;
                    *value = base_value + last;
                    if self.sequence_p {
                        last = *value;
                    }
                }
            }
            _ => {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "codebook lookup type above 2 is reserved",
                });
            }
        }
        Ok(())
    }
}

/// Parses the lookup table of one codebook (section 3.2.1). Returns
/// the lookup type, the lookup values, and the `sequence_p` flag.
/// The lookup type is read as 4 bits, so the `u8` conversion is
/// exact; the multiplicands are integers converted to `f32` because
/// the lookup table is `f32` by definition, and that conversion can
/// round values above 2^24, which is inherent to the format.
#[allow(clippy::cast_precision_loss)]
fn parse_codebook_lookup(
    reader: &mut LsbBitReader<'_>,
    entries: u32,
    dimensions: u16,
) -> Result<(u8, Vec<f32>, bool)> {
    let lookup_type = u8::try_from(reader.read_bits(4)?).map_err(|_| Error::InvalidData {
        offset: reader.bit_position() / 8,
        detail: "codebook lookup type exceeds u8 range",
    })?;
    if lookup_type > 2 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "codebook lookup type above 2 is reserved",
        });
    }
    let mut lookup_values: Vec<f32> = Vec::new();
    let mut sequence_p = false;
    if lookup_type != 0 {
        let minimum = float32_unpack(reader.read_bits(32)?);
        let delta = float32_unpack(reader.read_bits(32)?);
        let value_bits = reader.read_bits(4)? + 1;
        sequence_p = reader.read_bit()?;
        let count = if lookup_type == 1 {
            lookup1_values(entries, u32::from(dimensions)) as usize
        } else {
            (entries as usize)
                .checked_mul(usize::from(dimensions))
                .ok_or(Error::Overflow(
                    "codebook lookup value count exceeds address space",
                ))?
        };
        if count == 0 || count > MAX_LOOKUP_VALUES {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "codebook lookup value count is zero or above the library limit",
            });
        }
        // The multiplicands must actually be present in the packet;
        // checking first bounds the allocation by the input size.
        if count > reader.remaining_bits() / value_bits as usize {
            return Err(Error::Truncated {
                needed: count * value_bits as usize,
                had: reader.remaining_bits(),
            });
        }
        lookup_values.reserve(count);
        for _ in 0..count {
            let multiplicand = reader.read_bits(value_bits)?;
            lookup_values.push(multiplicand as f32 * delta + minimum);
        }
    }
    Ok((lookup_type, lookup_values, sequence_p))
}

/// Parses the codeword lengths of one codebook (section 3.2.1):
/// ordered runs of equal length, or unordered lengths with an
/// optional sparse flag. Returns one length per entry (0 = unused).
fn parse_codebook_lengths(reader: &mut LsbBitReader<'_>, entries: u32) -> Result<Vec<u8>> {
    let ordered = reader.read_bit()?;
    let mut lengths = vec![0u8; entries as usize];
    if ordered {
        // Ordered: runs of equal length, in ascending length order.
        let mut current_entry: usize = 0;
        let mut current_length = reader.read_bits(5)? + 1;
        while current_entry < entries as usize {
            let current_entry_u32 =
                u32::try_from(current_entry).map_err(|_| Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "codebook entry index exceeds u32 range",
                })?;
            let run = reader.read_bits(ilog(entries - current_entry_u32))?;
            if run == 0 {
                // A zero-length run makes no progress; the reference
                // would loop forever on this input.
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "ordered codebook has a zero-length run",
                });
            }
            let run = run as usize;
            if current_entry + run > entries as usize {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "ordered codebook run exceeds entry count",
                });
            }
            if current_length > limits::MAX_HUFFMAN_CODE_LENGTH {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "ordered codebook length exceeds maximum",
                });
            }
            for length in &mut lengths[current_entry..current_entry + run] {
                *length = u8::try_from(current_length).map_err(|_| Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "ordered codebook length exceeds u8 range",
                })?;
            }
            current_entry += run;
            current_length += 1;
        }
    } else {
        // Unordered: a sparse flag first, then one length per entry
        // (or a used flag plus a length when sparse).
        let sparse = reader.read_bit()?;
        for length in &mut lengths {
            if sparse {
                if reader.read_bit()? {
                    *length =
                        u8::try_from(reader.read_bits(5)? + 1).map_err(|_| Error::InvalidData {
                            offset: reader.bit_position() / 8,
                            detail: "codebook length exceeds u8 range",
                        })?;
                } else {
                    *length = 0;
                }
            } else {
                *length =
                    u8::try_from(reader.read_bits(5)? + 1).map_err(|_| Error::InvalidData {
                        offset: reader.bit_position() / 8,
                        detail: "codebook length exceeds u8 range",
                    })?;
            }
        }
    }
    Ok(lengths)
}

/// Parses one codebook from the setup header (section 3.2.1).
///
/// # Errors
///
/// Returns `Truncated` when the header ends mid-codebook, and
/// `InvalidData` when the sync pattern, an entry count, a codeword
/// length, the Huffman tree, or the lookup type is invalid.
fn parse_codebook(reader: &mut LsbBitReader<'_>) -> Result<Codebook> {
    // Sync pattern 0x564342, read as bytes 0x42, 0x43, 0x56.
    if reader.read_bits(24)? != 0x56_43_42 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "codebook sync pattern mismatch",
        });
    }
    let dimensions = u16::try_from(reader.read_bits(16)?).map_err(|_| Error::InvalidData {
        offset: reader.bit_position() / 8,
        detail: "codebook dimensions exceed u16 range",
    })?;
    let entries = reader.read_bits(24)?;
    if dimensions == 0 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "codebook dimensions must not be zero",
        });
    }
    if entries == 0 || entries > MAX_CODEBOOK_ENTRIES {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "codebook entry count is zero or above the library limit",
        });
    }
    let lengths = parse_codebook_lengths(reader, entries)?;
    // Lookup table.
    let (lookup_type, lookup_values, sequence_p) =
        parse_codebook_lookup(reader, entries, dimensions)?;
    // Huffman table, with the single-entry errata special case.
    let used: Vec<(usize, u8)> = lengths
        .iter()
        .enumerate()
        .filter(|(_, &length)| length > 0)
        .map(|(index, &length)| (index, length))
        .collect();
    let (table, single_entry) = if used.len() == 1 {
        let (index, length) = used[0];
        if length != 1 {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "single-entry codebook length must be 1 (errata 20150226)",
            });
        }
        (
            None,
            Some(u32::try_from(index).map_err(|_| Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "single-entry codebook index exceeds u32 range",
            })?),
        )
    } else {
        (Some(HuffmanTable::from_code_lengths(&lengths)?), None)
    };
    Ok(Codebook {
        dimensions,
        entries,
        lookup_type,
        sequence_p,
        lookup_values,
        table,
        single_entry,
    })
}

/// The floor 1 inverse dB table (Vorbis I spec section 10.1):
/// `table[i] = 10^(7 * (i - 255) / 256)`, computed once rather than
/// embedded as 256 constants. Spot values from the spec:
/// `table[0]` = `1.064_986_3e-07`, `table[254]` = `0.938_979_8`,
/// `table[255]` = `1.0`.
/// The table values are `f32` by definition (section 10.1), so
/// the `f64` to `f32` cast is inherent; the index is 0..=255, so
/// the `f64` conversion is exact.
#[allow(clippy::cast_possible_truncation)]
#[must_use]
pub fn floor1_inverse_db_table() -> &'static [f32; 256] {
    static TABLE: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0.0f32; 256];
        for (i, value) in table.iter_mut().enumerate() {
            let exponent = 7.0f64 * (f64::from(u16::try_from(i).unwrap_or(0)) - 255.0) / 256.0;
            *value = 10f64.powf(exponent) as f32;
        }
        table
    })
}

/// `low_neighbor(v, x)` (section 9.2.4): the position `n < x` of the
/// greatest vector element below `v[x]`. `v[0]` is always a valid
/// answer because `v[0] = 0` and all other X values are positive
/// and unique.
fn low_neighbor(values: &[u32], x: usize) -> usize {
    let mut best = 0;
    for n in 1..x {
        if values[n] < values[x] && values[n] > values[best] {
            best = n;
        }
    }
    best
}

/// `high_neighbor(v, x)` (section 9.2.5): the position `n < x` of
/// the smallest vector element above `v[x]`. `v[1] = 2^rangebits`
/// is the maximum X value, so it is always a valid answer.
fn high_neighbor(values: &[u32], x: usize) -> usize {
    let mut best = 1;
    for n in 0..x {
        if n != 1 && values[n] > values[x] && values[n] < values[best] {
            best = n;
        }
    }
    best
}

/// `render_point(x0, y0, x1, y1, x)` (section 9.2.6): the Y value at
/// point `x` on the line through `(x0, y0)` and `(x1, y1)`,
/// computed in integers. Division is exact here (the numerator is
/// non-negative and the denominator positive), so no rounding
/// question arises.
fn render_point(x0: i64, y0: i64, x1: i64, y1: i64, x: i64) -> i64 {
    let dy = y1 - y0;
    let adx = x1 - x0;
    if adx == 0 {
        return y0;
    }
    let ady = dy.abs();
    let off = ady * (x - x0) / adx;
    if dy < 0 {
        y0 - off
    } else {
        y0 + off
    }
}

/// `render_line(x0, y0, x1, y1, v)` (section 9.2.7): Bresenham-like
/// Writes `value` into `v` at index `x`, skipping out-of-range
/// indices. The floor vector is length `n`, and a segment may run
/// past it when the largest X value exceeds `n`; the spec then
/// truncates the floor to `n` elements, which is what skipping
/// achieves. A negative `x` converts to nothing via `try_from`.
fn write_floor_point(v: &mut [i32], x: i64, value: i64) {
    if let Ok(idx) = usize::try_from(x) {
        if idx < v.len() {
            v[idx] = i32::try_from(value).unwrap_or(i32::MAX);
        }
    }
}

/// integer line drawing into `v[x0..x1]`. Integer division rounds
/// toward zero, as Rust's `/` does for signed integers. Writes
/// outside `v` are skipped: the floor vector is length `n`, and a
/// segment may run past it when the largest X value exceeds `n`;
/// the spec then truncates the floor to `n` elements, which is
/// what skipping achieves.
fn render_line(x0: i64, y0: i64, x1: i64, y1: i64, v: &mut [i32]) {
    if x1 <= x0 {
        write_floor_point(v, x0, y0);
        return;
    }
    let dy = y1 - y0;
    let adx = x1 - x0;
    let ady = dy.abs();
    let base = dy / adx;
    let sy = if dy < 0 { base - 1 } else { base + 1 };
    let ady = ady - base.abs() * adx;
    let mut x = x0;
    let mut y = y0;
    let mut err: i64 = 0;
    write_floor_point(v, x, y);
    while x + 1 < x1 {
        x += 1;
        err += ady;
        if err >= adx {
            err -= adx;
            y += sy;
        } else {
            y += base;
        }
        write_floor_point(v, x, y);
    }
}

/// Floor type 0 configuration (Vorbis I spec section 6.2.1).
/// Packet decode is implemented (sections 6.2.2 and 6.2.3); no
/// encoder since libvorbis beta 4 emits floor 0, so real floor 0
/// streams are rare, but the format is not deprecated.
#[derive(Debug, Clone)]
pub struct Floor0 {
    /// LSP order.
    pub order: u8,
    /// Sample rate the Bark map was built for.
    pub rate: u16,
    /// Bark map size.
    pub bark_map_size: u16,
    /// Amplitude bits.
    pub amplitude_bits: u8,
    /// Amplitude offset.
    pub amplitude_offset: u8,
    /// Codebook indices used for the LSP coefficients.
    pub book_list: Vec<u8>,
}

/// Bark scale (Vorbis I spec section 6.2.3, post-errata 20150227):
/// `13.1*atan(0.00074*x) + 2.24*atan(0.0000000185*x^2) + 0.0001*x`.
/// The errata corrects a misplaced parenthesis in the earlier
/// typesetting, which had the `0.0001*x` term inside the second
/// `atan`.
fn bark_scale(x: f64) -> f64 {
    13.1 * (0.00074 * x).atan() + 2.24 * (0.000_000_018_5 * x * x).atan() + 0.0001 * x
}

impl Floor0 {
    /// Decodes one floor 0 curve of length `n` (sections 6.2.2
    /// and 6.2.3). Returns `Ok(None)` when the floor is unused:
    /// amplitude zero, or end-of-packet during decode, which is
    /// nominal per section 6.2.2.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when the book number is out of range,
    /// a referenced codebook is missing, the setup has a zero rate
    /// or Bark map size (the curve computation divides by both),
    /// or degenerate LSP coefficients make the curve denominator
    /// zero; `Truncated` only for read errors that are not
    /// end-of-packet (there are none in this path: end-of-packet
    /// maps to `Ok(None)`).
    pub fn decode(
        &self,
        reader: &mut LsbBitReader<'_>,
        codebooks: &[Codebook],
        n: usize,
    ) -> Result<Option<Vec<f32>>> {
        match self.decode_inner(reader, codebooks, n) {
            Ok(result) => Ok(result),
            Err(Error::Truncated { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn decode_inner(
        &self,
        reader: &mut LsbBitReader<'_>,
        codebooks: &[Codebook],
        n: usize,
    ) -> Result<Option<Vec<f32>>> {
        if self.rate == 0 || self.bark_map_size == 0 {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "floor 0 rate or bark map size is zero",
            });
        }
        let amplitude = reader.read_bits(u32::from(self.amplitude_bits))?;
        if amplitude == 0 {
            return Ok(None);
        }
        let book_count = self.book_list.len();
        let booknumber = reader.read_bits(ilog(u32::try_from(book_count).unwrap_or(0)))? as usize;
        if booknumber >= book_count {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "floor 0 book number exceeds book list",
            });
        }
        let book = codebooks
            .get(usize::from(self.book_list[booknumber]))
            .ok_or(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "floor 0 book exceeds codebook count",
            })?;
        // Section 6.2.2 steps 6-11: read VQ vectors from the one
        // selected book until at least `order` scalars are held,
        // adding the running `last` scalar to each vector before
        // concatenation and then setting `last` to the vector's
        // last scalar. `last` carries across vectors: the loop
        // returns to the vector-read step, not to the
        // initialization (resetting it each time would make the
        // accumulation a no-op); libvorbis `floor0_inverse1`
        // accumulates the same way. Over-reading past `order`
        // is allowed; the extra scalars are discarded.
        let mut coefficients: Vec<f32> = Vec::new();
        let mut last: f32 = 0.0;
        while coefficients.len() < usize::from(self.order) {
            let mut temp = vec![0.0f32; usize::from(book.dimensions)];
            book.decode_vector(reader, &mut temp)?;
            for value in &mut temp {
                *value += last;
            }
            if let Some(&last_value) = temp.last() {
                last = last_value;
            }
            coefficients.extend_from_slice(&temp);
        }
        coefficients.truncate(usize::from(self.order));
        Ok(Some(self.curve(amplitude, &coefficients, n)?))
    }

    /// Curve computation (section 6.2.3): the Bark-scale map,
    /// then the LSP filter response `p`/`q` at each mapped
    /// angle, converted from the log (dB) scale to linear
    /// amplitude. Consecutive output bins with the same map
    /// value share one computed value (step 8's
    /// `iteration_condition` chaining).
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when degenerate LSP coefficients
    /// make the curve value non-finite (see the note below).
    // The two casts are bounded: the map value is floored and
    // clamped to `[0, bark_map_size - 1]` (at most 65535) before
    // the `f64` to integer conversion, and the curve value is
    // checked finite before the `f64` to `f32` conversion, whose
    // rounding is the output format's own precision.
    #[allow(clippy::cast_possible_truncation)]
    fn curve(&self, amplitude: u32, coefficients: &[f32], n: usize) -> Result<Vec<f32>> {
        let order = usize::from(self.order);
        let bark_map_size = f64::from(self.bark_map_size);
        let bark_max = bark_scale(f64::from(self.rate) * 0.5);
        // `map[i]` for `i` in 0..n; the map is non-decreasing
        // because the Bark scale is increasing in frequency.
        // `bark_max` is positive (rate > 0, checked in
        // `decode_inner`), so the division is defined. Map
        // values fit in `u32`: they are clamped to
        // `bark_map_size - 1` (a `u16`).
        let n_u32 = u32::try_from(n).unwrap_or(0);
        let map: Vec<u32> = (0..n)
            .map(|i| {
                let i_u32 = u32::try_from(i).unwrap_or(0);
                let frequency = f64::from(self.rate) * f64::from(i_u32) / (2.0 * f64::from(n_u32));
                let value = (bark_scale(frequency) * bark_map_size / bark_max).floor() as i64;
                u32::try_from(value.clamp(0, i64::from(self.bark_map_size) - 1)).unwrap_or(0)
            })
            .collect();
        // `cos` of each LSP coefficient (an angle in radians).
        let cos_coefficients: Vec<f64> = coefficients
            .iter()
            .map(|&value| f64::from(value).cos())
            .collect();
        // `(2^amplitude_bits - 1)` as an `f64`. `amplitude_bits`
        // is at most 32 here (`decode_inner` reads the amplitude
        // with `read_bits`, which rejects wider reads) and at
        // least 1 (a zero amplitude returns early), so the power
        // is exact.
        let maxval = 2f64.powi(i32::from(self.amplitude_bits)) - 1.0;
        let amplitude_offset = f64::from(self.amplitude_offset);
        let mut output = vec![0.0f32; n];
        let mut i = 0usize;
        while i < n {
            let omega = std::f64::consts::PI * f64::from(map[i]) / bark_map_size;
            let cos_omega = omega.cos();
            let (p, q) = if order % 2 == 1 {
                let mut p = 1.0 - cos_omega * cos_omega;
                for j in 0..(order - 1) / 2 {
                    let difference = cos_coefficients[2 * j + 1] - cos_omega;
                    p *= 4.0 * difference * difference;
                }
                let mut q = 0.25;
                for j in 0..order.div_ceil(2) {
                    let difference = cos_coefficients[2 * j] - cos_omega;
                    q *= 4.0 * difference * difference;
                }
                (p, q)
            } else {
                let mut p = (1.0 - cos_omega) / 2.0;
                let mut q = (1.0 + cos_omega) / 2.0;
                for j in 0..order / 2 {
                    let difference = cos_coefficients[2 * j + 1] - cos_omega;
                    p *= 4.0 * difference * difference;
                    let difference = cos_coefficients[2 * j] - cos_omega;
                    q *= 4.0 * difference * difference;
                }
                (p, q)
            };
            // A zero denominator needs every factor of both `p`
            // and `q` to vanish at once, which valid LSP
            // coefficients (distinct angles in (0, pi)) never do;
            // it is degenerate or corrupt input. Reject it
            // rather than letting a non-finite floor curve into
            // the audio pipeline.
            let linear = (0.115_129_25
                * (f64::from(amplitude) * amplitude_offset / (maxval * (p + q).sqrt())
                    - amplitude_offset))
                .exp();
            if !linear.is_finite() {
                return Err(Error::InvalidData {
                    offset: 0,
                    detail: "floor 0 curve value is not finite",
                });
            }
            let value = linear as f32;
            let condition = map[i];
            while i < n && map[i] == condition {
                output[i] = value;
                i += 1;
            }
        }
        Ok(output)
    }
}

/// Floor type 1 configuration (Vorbis I spec section 7.2.2).
#[derive(Debug, Clone)]
pub struct Floor1 {
    /// Number of partitions.
    pub partitions: u8,
    /// Partition class per partition.
    pub partition_class_list: Vec<u8>,
    /// Y-value count per class.
    pub class_dimensions: Vec<u8>,
    /// Subclass bit count per class.
    pub class_subclasses: Vec<u8>,
    /// Masterbook per class, when the class has subclasses.
    pub class_masterbooks: Vec<Option<u8>>,
    /// Subclass books per class: `subclass_books[class][subclass]`
    /// is a codebook index, or -1 when the subclass uses no book.
    pub subclass_books: Vec<Vec<i32>>,
    /// Amplitude multiplier, 1..=4.
    pub multiplier: u8,
    /// Bits per X value.
    pub rangebits: u8,
    /// X coordinates in list order: `[0, 2^rangebits, ...]`.
    pub x_list: Vec<u32>,
}

impl Floor1 {
    /// Decodes one floor 1 curve of length `n` (sections 7.2.3 and
    /// 7.2.4). Returns `Ok(None)` when the floor is unused, including
    /// when the packet ends mid-curve: end-of-packet during curve
    /// decode is nominal (section 7.2.3).
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when a referenced codebook is missing
    /// or a decoded value is out of range, and `Truncated` only for
    /// errors that are not end-of-packet (there are none in this
    /// path: end-of-packet maps to `Ok(None)`).
    pub fn decode(
        &self,
        reader: &mut LsbBitReader<'_>,
        codebooks: &[Codebook],
        n: usize,
    ) -> Result<Option<Vec<f32>>> {
        match self.decode_inner(reader, codebooks, n) {
            Ok(result) => Ok(result),
            Err(Error::Truncated { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn decode_inner(
        &self,
        reader: &mut LsbBitReader<'_>,
        codebooks: &[Codebook],
        n: usize,
    ) -> Result<Option<Vec<f32>>> {
        if !reader.read_bit()? {
            return Ok(None);
        }
        let range = [256i64, 128, 86, 64][(self.multiplier - 1) as usize];
        let y_bits = ilog(u32::try_from(range - 1).unwrap_or(0));
        let mut y = vec![0i64; self.x_list.len()];
        y[0] = i64::from(reader.read_bits(y_bits)?);
        y[1] = i64::from(reader.read_bits(y_bits)?);
        let mut offset = 2usize;
        for &class in &self.partition_class_list {
            let class = class as usize;
            let cdim = self.class_dimensions[class] as usize;
            let cbits = self.class_subclasses[class] as usize;
            let csub = (1usize << cbits) - 1;
            let mut cval: u32 = 0;
            if cbits > 0 {
                let master = self.class_masterbooks[class].ok_or(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "floor 1 class has subclasses but no masterbook",
                })? as usize;
                let book = codebooks.get(master).ok_or(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "floor 1 masterbook exceeds codebook count",
                })?;
                cval = book.decode_scalar(reader)?;
            }
            for j in 0..cdim {
                let book_index = self.subclass_books[class][(cval as usize) & csub];
                if book_index >= 0 {
                    let book = codebooks
                        .get(usize::try_from(book_index).unwrap_or(usize::MAX))
                        .ok_or(Error::InvalidData {
                            offset: reader.bit_position() / 8,
                            detail: "floor 1 subclass book exceeds codebook count",
                        })?;
                    y[offset + j] = i64::from(book.decode_scalar(reader)?);
                } else {
                    y[offset + j] = 0;
                }
                cval >>= cbits;
            }
            offset += cdim;
        }
        Ok(Some(self.curve(&y, n)))
    }

    /// Curve computation (section 7.2.4): step 1 unwraps the
    /// difference values into final Y amplitudes, step 2 sorts by X
    /// and renders the piecewise lines, then the inverse dB table
    /// converts the result to linear amplitude.
    fn curve(&self, y: &[i64], n: usize) -> Vec<f32> {
        let range = [256i64, 128, 86, 64][(self.multiplier - 1) as usize];
        let values = self.x_list.len();
        let mut final_y = vec![0i64; values];
        let mut step2_flag = vec![false; values];
        step2_flag[0] = true;
        step2_flag[1] = true;
        final_y[0] = y[0];
        final_y[1] = y[1];
        for i in 2..values {
            let low = low_neighbor(&self.x_list, i);
            let high = high_neighbor(&self.x_list, i);
            let predicted = render_point(
                i64::from(self.x_list[low]),
                final_y[low],
                i64::from(self.x_list[high]),
                final_y[high],
                i64::from(self.x_list[i]),
            );
            let val = y[i];
            let highroom = range - predicted;
            let lowroom = predicted;
            let room = 2 * highroom.min(lowroom);
            if val != 0 {
                step2_flag[low] = true;
                step2_flag[high] = true;
                step2_flag[i] = true;
                if val >= room {
                    if highroom > lowroom {
                        final_y[i] = val - lowroom + predicted;
                    } else {
                        final_y[i] = predicted - val + highroom - 1;
                    }
                } else if val % 2 == 1 {
                    final_y[i] = predicted - (val + 1) / 2;
                } else {
                    final_y[i] = predicted + val / 2;
                }
            } else {
                step2_flag[i] = false;
                final_y[i] = predicted;
            }
        }
        // The spec's additional note in section 7.2.4 suggests
        // clamping final Y values to [0, range); valid setups cannot
        // produce out-of-range values, so this only guards corrupt
        // setups.
        for value in &mut final_y {
            *value = (*value).clamp(0, range - 1);
        }
        // Step 2: sort by X, then render one pass.
        let mut order: Vec<usize> = (0..values).collect();
        order.sort_by_key(|&i| self.x_list[i]);
        let mut floor = vec![0i32; n];
        let mut hx: i64 = 0;
        let mut lx: i64 = 0;
        let mut ly: i64 = final_y[order[0]] * i64::from(self.multiplier);
        for &i in order.iter().skip(1) {
            if step2_flag[i] {
                let hy = final_y[i] * i64::from(self.multiplier);
                hx = i64::from(self.x_list[i]);
                render_line(lx, ly, hx, hy, &mut floor);
                lx = hx;
                ly = hy;
            }
        }
        if hx < i64::try_from(n).unwrap_or(0) {
            render_line(hx, ly, i64::try_from(n).unwrap_or(0), ly, &mut floor);
        }
        let table = floor1_inverse_db_table();
        floor
            .iter()
            .map(|&value| table[usize::try_from(value.clamp(0, 255)).unwrap_or(0)])
            .collect()
    }
}

/// A floor configuration: type 0 (section 6) or type 1.
#[derive(Debug, Clone)]
pub enum Floor {
    /// Floor type 0 (section 6): Bark-scale LSP curve.
    Type0(Floor0),
    /// Floor type 1 (section 7).
    Type1(Floor1),
}

/// A residue configuration (Vorbis I spec section 8.6.1). The
/// header is identical for residue types 0, 1 and 2; only the
/// partition decode differs.
#[derive(Debug, Clone)]
pub struct Residue {
    /// Residue type: 0, 1 or 2.
    pub format: u8,
    /// First vector element coded.
    pub begin: u32,
    /// One past the last vector element coded.
    pub end: u32,
    /// Partition size in vector elements.
    pub partition_size: u32,
    /// Number of partition classifications.
    pub classifications: u32,
    /// Codebook used for classification codewords.
    pub classbook: u8,
    /// Cascade bitmap per classification (8 pass bits).
    pub cascade: Vec<u8>,
    /// VQ codebook per classification and pass, or `None` when the
    /// pass is unused for that classification.
    pub books: Vec<[Option<u8>; 8]>,
}

impl Residue {
    /// Decodes the residue vectors for one submap bundle (sections
    /// 8.6.2-8.6.5). Returns one vector of length `blocksize / 2`
    /// per channel. End-of-packet during decode is nominal: the
    /// vectors decoded so far are returned (section 8.6.2).
    #[must_use]
    pub fn decode(
        &self,
        reader: &mut LsbBitReader<'_>,
        codebooks: &[Codebook],
        channel_count: usize,
        blocksize: usize,
        do_not_decode: &[bool],
    ) -> Vec<Vec<f32>> {
        let half = blocksize / 2;
        if self.format == 2 {
            // Format 2 decodes a single interleaved vector and then
            // deinterleaves it (section 8.6.5). When every vector is
            // marked do-not-decode, no decode occurs at all.
            if do_not_decode.iter().all(|&flag| flag) {
                return vec![vec![0.0; half]; channel_count];
            }
            let interleaved =
                self.decode_vectors(reader, codebooks, 1, half * channel_count, &[false]);
            let mut out = vec![vec![0.0f32; half]; channel_count];
            for i in 0..half {
                for j in 0..channel_count {
                    out[j][i] = interleaved[0][i * channel_count + j];
                }
            }
            return out;
        }
        self.decode_vectors(reader, codebooks, channel_count, half, do_not_decode)
    }

    /// The shared decode infrastructure of section 8.6.2, over
    /// `vec_count` vectors of length `actual_size`.
    fn decode_vectors(
        &self,
        reader: &mut LsbBitReader<'_>,
        codebooks: &[Codebook],
        vec_count: usize,
        actual_size: usize,
        do_not_decode: &[bool],
    ) -> Vec<Vec<f32>> {
        let mut vectors = vec![vec![0.0f32; actual_size]; vec_count];
        // Section 8.6.2 limits begin/end to the actual vector size.
        // The spec text says "maximum of", which would not limit
        // anything; the limit is the minimum, i.e. the values are
        // clamped down to the actual size.
        let limit_begin = (self.begin as usize).min(actual_size);
        let limit_end = (self.end as usize).min(actual_size);
        if limit_end <= limit_begin {
            return vectors;
        }
        let n_to_read = limit_end - limit_begin;
        let partitions_to_read = n_to_read / self.partition_size as usize;
        if partitions_to_read == 0 {
            return vectors;
        }
        let Some(classbook) = codebooks.get(self.classbook as usize) else {
            return vectors;
        };
        let classwords_per_codeword = classbook.dimensions as usize;
        if classwords_per_codeword == 0 {
            return vectors;
        }
        let mut classifications = vec![vec![0u32; partitions_to_read]; vec_count];
        for pass in 0..8usize {
            let mut partition_count = 0usize;
            while partition_count < partitions_to_read {
                if pass == 0 {
                    // Decode one classification codeword per vector
                    // and split it into per-partition classes,
                    // least significant class last (descending i).
                    for j in 0..vec_count {
                        if do_not_decode[j] {
                            continue;
                        }
                        let Ok(mut temp) = classbook.decode_scalar(reader) else {
                            return vectors;
                        };
                        for i in (0..classwords_per_codeword).rev() {
                            if partition_count + i < partitions_to_read {
                                classifications[j][partition_count + i] =
                                    temp % self.classifications;
                            }
                            temp /= self.classifications;
                        }
                    }
                }
                for _ in 0..classwords_per_codeword {
                    if partition_count >= partitions_to_read {
                        break;
                    }
                    for j in 0..vec_count {
                        if do_not_decode[j] {
                            continue;
                        }
                        let vqclass = classifications[j][partition_count] as usize;
                        let vqbook = self.books.get(vqclass).and_then(|books| books[pass]);
                        if let Some(book_index) = vqbook {
                            let Some(book) = codebooks.get(book_index as usize) else {
                                return vectors;
                            };
                            let offset =
                                limit_begin + partition_count * self.partition_size as usize;
                            if self
                                .decode_partition(reader, book, &mut vectors[j], offset)
                                .is_err()
                            {
                                return vectors;
                            }
                        }
                    }
                    partition_count += 1;
                }
            }
        }
        vectors
    }

    /// Decodes one partition into `vector` at `offset` (sections
    /// 8.6.3 for format 0, 8.6.4 for formats 1 and 2).
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when the partition would run past the
    /// vector, and propagates read errors from the codebook.
    fn decode_partition(
        &self,
        reader: &mut LsbBitReader<'_>,
        book: &Codebook,
        vector: &mut [f32],
        offset: usize,
    ) -> Result<()> {
        let n = self.partition_size as usize;
        let dims = book.dimensions as usize;
        if dims == 0 || offset + n > vector.len() {
            return Err(Error::InvalidData {
                offset,
                detail: "residue partition extends past vector end",
            });
        }
        let mut entry = vec![0.0f32; dims];
        if self.format == 0 {
            // Format 0: interleaved sub-vectors. `step` is integral
            // because divisibility was validated at setup.
            let step = n / dims;
            for i in 0..step {
                book.decode_vector(reader, &mut entry)?;
                for j in 0..dims {
                    vector[offset + i + j * step] += entry[j];
                }
            }
        } else {
            // Formats 1 and 2: sequential fill, clamped to the
            // partition size.
            let mut i = 0usize;
            while i < n {
                book.decode_vector(reader, &mut entry)?;
                for &entry_value in &entry {
                    if i >= n {
                        break;
                    }
                    vector[offset + i] += entry_value;
                    i += 1;
                }
            }
        }
        Ok(())
    }
}

/// A channel mapping (Vorbis I spec section 4.2.4, mapping type 0).
#[derive(Debug, Clone)]
pub struct Mapping {
    /// Number of submaps.
    pub submaps: u8,
    /// Coupling steps as (magnitude channel, angle channel).
    pub coupling_steps: Vec<(u8, u8)>,
    /// Submap index per channel.
    pub mux: Vec<u8>,
    /// Floor index per submap.
    pub submap_floor: Vec<u8>,
    /// Residue index per submap.
    pub submap_residue: Vec<u8>,
}

/// A mode (Vorbis I spec section 4.2.4).
#[derive(Debug, Clone)]
pub struct Mode {
    /// True for a long block (`blocksize_1`), false for a short
    /// block (`blocksize_0`).
    pub blockflag: bool,
    /// Mapping index for this mode.
    pub mapping: u8,
}

/// The Vorbis identification header (section 4.2.2).
#[derive(Debug, Clone)]
pub struct VorbisIdentification {
    /// Audio channels.
    pub channels: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Maximum bitrate hint (0 when unset).
    pub bitrate_maximum: i32,
    /// Nominal bitrate hint (0 when unset).
    pub bitrate_nominal: i32,
    /// Minimum bitrate hint (0 when unset).
    pub bitrate_minimum: i32,
    /// Short block size in samples.
    pub blocksize_0: usize,
    /// Long block size in samples.
    pub blocksize_1: usize,
}

/// The Vorbis comment header (section 5).
#[derive(Debug, Clone, Default)]
pub struct VorbisComment {
    /// Vendor string as raw bytes.
    pub vendor: Vec<u8>,
    /// User comments as raw bytes, usually `KEY=value`.
    pub comments: Vec<Vec<u8>>,
}

/// The full Vorbis setup: all three headers parsed.
#[derive(Debug, Clone)]
pub struct VorbisSetup {
    /// Identification header.
    pub identification: VorbisIdentification,
    /// Comment header.
    pub comment: VorbisComment,
    /// Codebooks in setup order.
    pub codebooks: Vec<Codebook>,
    /// Floors in setup order.
    pub floors: Vec<Floor>,
    /// Residues in setup order.
    pub residues: Vec<Residue>,
    /// Mappings in setup order.
    pub mappings: Vec<Mapping>,
    /// Modes in setup order.
    pub modes: Vec<Mode>,
}

impl VorbisSetup {
    /// Parses all three Vorbis headers.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when a header ends early (except the
    /// comment header, where truncation is non-fatal per section
    /// 4.2), and `InvalidData` when a header field is invalid.
    pub fn from_headers(
        identification_data: &[u8],
        comment_data: &[u8],
        setup_data: &[u8],
    ) -> Result<Self> {
        let identification = parse_vorbis_identification(identification_data)?;
        let comment = parse_vorbis_comment(comment_data)?;
        let (codebooks, floors, residues, mappings, modes) =
            parse_setup(setup_data, &identification)?;
        Ok(Self {
            identification,
            comment,
            codebooks,
            floors,
            residues,
            mappings,
            modes,
        })
    }
}

/// Checks the common header prefix (section 4.2.1): one packet-type
/// byte followed by the six bytes `vorbis`.
///
/// # Errors
///
/// Returns `Truncated` when the packet is shorter than 7 bytes, and
/// `InvalidData` when the type or magic is wrong.
fn check_common_header(data: &[u8], packet_type: u8) -> Result<()> {
    if data.len() < 7 {
        return Err(Error::Truncated {
            needed: 7,
            had: data.len(),
        });
    }
    if data[0] != packet_type || &data[1..7] != b"vorbis" {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "Vorbis header magic mismatch",
        });
    }
    Ok(())
}

/// Parses the identification header (section 4.2.2).
///
/// # Errors
///
/// Returns `Truncated` when the header ends early, and
/// `InvalidData` when the version, channel count, sample rate,
/// blocksizes, or framing bit is invalid.
pub fn parse_vorbis_identification(data: &[u8]) -> Result<VorbisIdentification> {
    check_common_header(data, 1)?;
    let mut reader = LsbBitReader::new(&data[7..]);
    let version = reader.read_bits(32)?;
    if version != 0 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "Vorbis version must be 0",
        });
    }
    let channels = reader.read_bits(8)?;
    if channels == 0 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "Vorbis channel count must not be zero",
        });
    }
    let sample_rate = reader.read_bits(32)?;
    if sample_rate == 0 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "Vorbis sample rate must not be zero",
        });
    }
    let bitrate_maximum = reader.read_i32()?;
    let bitrate_nominal = reader.read_i32()?;
    let bitrate_minimum = reader.read_i32()?;
    let blocksize_0_exp = reader.read_bits(4)?;
    let blocksize_1_exp = reader.read_bits(4)?;
    let blocksize_0 = 1usize << blocksize_0_exp;
    let blocksize_1 = 1usize << blocksize_1_exp;
    if !(64..=8192).contains(&blocksize_0)
        || !(64..=8192).contains(&blocksize_1)
        || blocksize_0 > blocksize_1
    {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "Vorbis blocksize out of range or blocksize_0 exceeds blocksize_1",
        });
    }
    if !reader.read_bit()? {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "Vorbis identification framing bit must be 1",
        });
    }
    Ok(VorbisIdentification {
        channels,
        sample_rate,
        bitrate_maximum,
        bitrate_nominal,
        bitrate_minimum,
        blocksize_0,
        blocksize_1,
    })
}

/// Parses the comment header (section 5). End-of-packet while
/// reading it is non-fatal (section 4.2): whatever was parsed
/// before the packet ran out is kept.
///
/// # Errors
///
/// Returns `InvalidData` when the magic is wrong or the framing
/// bit, when present, is unset.
pub fn parse_vorbis_comment(data: &[u8]) -> Result<VorbisComment> {
    check_common_header(data, 3)?;
    let mut reader = LsbBitReader::new(&data[7..]);
    let mut vendor = Vec::new();
    let mut comments = Vec::new();
    let vendor_len = match reader.read_bits(32) {
        Ok(value) => value as usize,
        Err(Error::Truncated { .. }) => return Ok(VorbisComment { vendor, comments }),
        Err(error) => return Err(error),
    };
    // Bound the allocation by the bytes actually present.
    if vendor_len > reader.remaining_bits() / 8 {
        return Ok(VorbisComment { vendor, comments });
    }
    vendor = vec![0u8; vendor_len];
    reader.read_bytes(&mut vendor)?;
    let comment_count = match reader.read_bits(32) {
        Ok(value) => value,
        Err(Error::Truncated { .. }) => return Ok(VorbisComment { vendor, comments }),
        Err(error) => return Err(error),
    };
    for _ in 0..comment_count {
        let len = match reader.read_bits(32) {
            Ok(value) => value as usize,
            Err(Error::Truncated { .. }) => break,
            Err(error) => return Err(error),
        };
        if len > reader.remaining_bits() / 8 {
            break;
        }
        let mut comment = vec![0u8; len];
        reader.read_bytes(&mut comment)?;
        comments.push(comment);
    }
    match reader.read_bit() {
        // A set framing bit, and a missing framing bit (the
        // non-fatal end-of-packet case), are both accepted.
        Ok(true) | Err(Error::Truncated { .. }) => {}
        Ok(false) => {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "Vorbis comment framing bit must be 1",
            });
        }
        Err(error) => return Err(error),
    }
    Ok(VorbisComment { vendor, comments })
}

/// Parses a floor type 0 header (section 6.2.1).
///
/// # Errors
///
/// Returns `Truncated` when the header ends early, and
/// `InvalidData` when a book index exceeds the codebook count.
fn parse_floor0(reader: &mut LsbBitReader<'_>, codebook_count: usize) -> Result<Floor0> {
    let order = reader.read_u8(8)?;
    let rate = reader.read_u16(16)?;
    let bark_map_size = reader.read_u16(16)?;
    let amplitude_bits = reader.read_u8(6)?;
    let amplitude_offset = reader.read_u8(8)?;
    let book_count = reader.read_bits(4)? as usize + 1;
    let mut book_list = Vec::with_capacity(book_count);
    for _ in 0..book_count {
        let book = reader.read_u8(8)?;
        if book as usize >= codebook_count {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "floor 0 book exceeds highest codebook",
            });
        }
        book_list.push(book);
    }
    Ok(Floor0 {
        order,
        rate,
        bark_map_size,
        amplitude_bits,
        amplitude_offset,
        book_list,
    })
}

/// Parses a floor type 1 header (section 7.2.2).
///
/// # Errors
///
/// Returns `Truncated` when the header ends early, and
/// `InvalidData` when a book index exceeds the codebook count, the
/// X list exceeds 65 elements, or an X value repeats.
fn parse_floor1(reader: &mut LsbBitReader<'_>, codebook_count: usize) -> Result<Floor1> {
    let partitions = reader.read_u8(5)?;
    let mut partition_class_list = Vec::with_capacity(partitions as usize);
    let mut maximum_class: i32 = -1;
    for _ in 0..partitions {
        let class = reader.read_u8(4)?;
        maximum_class = maximum_class.max(i32::from(class));
        partition_class_list.push(class);
    }
    let class_count = usize::try_from((maximum_class + 1).max(0)).unwrap_or(0);
    let mut class_dimensions = vec![0u8; class_count];
    let mut class_subclasses = vec![0u8; class_count];
    let mut class_masterbooks: Vec<Option<u8>> = vec![None; class_count];
    let mut subclass_books: Vec<Vec<i32>> = vec![Vec::new(); class_count];
    for i in 0..class_count {
        let dimensions = reader.read_u8(3)? + 1;
        let subclasses = reader.read_u8(2)?;
        class_dimensions[i] = dimensions;
        class_subclasses[i] = subclasses;
        if subclasses != 0 {
            let master = reader.read_u8(8)?;
            if master as usize >= codebook_count {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "floor 1 masterbook exceeds highest codebook",
                });
            }
            class_masterbooks[i] = Some(master);
        }
        let book_count = 1usize << subclasses;
        let mut books = Vec::with_capacity(book_count);
        for _ in 0..book_count {
            let book = i32::from(reader.read_u8(8)?) - 1;
            if book >= 0 && usize::try_from(book).unwrap_or(usize::MAX) >= codebook_count {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "floor 1 subclass book exceeds highest codebook",
                });
            }
            books.push(book);
        }
        subclass_books[i] = books;
    }
    let multiplier = reader.read_u8(2)? + 1;
    let rangebits = reader.read_u8(4)?;
    let mut x_list = vec![0u32, 1u32 << rangebits];
    for &class in &partition_class_list {
        let cdim = class_dimensions[class as usize] as usize;
        for _ in 0..cdim {
            x_list.push(reader.read_bits(u32::from(rangebits))?);
        }
    }
    if x_list.len() > MAX_FLOOR1_VALUES {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "floor 1 X list exceeds 65 elements",
        });
    }
    let mut sorted = x_list.clone();
    sorted.sort_unstable();
    for pair in sorted.windows(2) {
        if pair[0] == pair[1] {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "floor 1 X list has duplicate values",
            });
        }
    }
    Ok(Floor1 {
        partitions,
        partition_class_list,
        class_dimensions,
        class_subclasses,
        class_masterbooks,
        subclass_books,
        multiplier,
        rangebits,
        x_list,
    })
}

/// Parses one residue header (section 8.6.1).
///
/// # Errors
///
/// Returns `Truncated` when the header ends early, and
/// `InvalidData` when a book index exceeds the codebook count, a
/// residue book has no value mapping, the classifications exceed
/// the classbook entries, or (for format 0) the partition size is
/// not a multiple of a used book's dimensions.
fn parse_residue(
    reader: &mut LsbBitReader<'_>,
    format: u8,
    codebooks: &[Codebook],
) -> Result<Residue> {
    let begin = reader.read_bits(24)?;
    let end = reader.read_bits(24)?;
    let partition_size = reader.read_bits(24)? + 1;
    let classifications = reader.read_bits(6)? + 1;
    let classbook = reader.read_u8(8)?;
    if classbook as usize >= codebooks.len() {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "residue classbook exceeds highest codebook",
        });
    }
    let classbook_entry_count = u64::from(codebooks[classbook as usize].entries);
    let classbook_dimensions = u32::from(codebooks[classbook as usize].dimensions);
    // classifications^dimensions must not exceed the classbook
    // entries (section 8.6.1). Bail out as soon as the running
    // product exceeds the entry count.
    if classifications > 1 {
        let mut power: u64 = 1;
        for _ in 0..classbook_dimensions {
            power *= u64::from(classifications);
            if power > classbook_entry_count {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "residue classifications exceed classbook entries",
                });
            }
        }
    }
    let mut cascade = Vec::with_capacity(classifications as usize);
    for _ in 0..classifications {
        let low_bits = reader.read_u8(3)?;
        let bitflag = reader.read_bit()?;
        let high_bits = if bitflag { reader.read_u8(5)? } else { 0 };
        cascade.push(high_bits * 8 + low_bits);
    }
    let mut books: Vec<[Option<u8>; 8]> = vec![[None; 8]; classifications as usize];
    for (i, &cascade_value) in cascade.iter().enumerate() {
        for (j, book_slot) in books[i].iter_mut().enumerate() {
            if cascade_value & (1 << j) != 0 {
                let book = reader.read_u8(8)?;
                if usize::from(book) >= codebooks.len() {
                    return Err(Error::InvalidData {
                        offset: reader.bit_position() / 8,
                        detail: "residue book exceeds highest codebook",
                    });
                }
                if !codebooks[book as usize].has_lookup() {
                    return Err(Error::InvalidData {
                        offset: reader.bit_position() / 8,
                        detail: "residue book has no value mapping",
                    });
                }
                *book_slot = Some(book);
            }
        }
    }
    if format == 0 {
        for class_books in &books {
            for &book in class_books.iter().flatten() {
                let dims = u32::from(codebooks[book as usize].dimensions);
                if dims == 0 || partition_size % dims != 0 {
                    return Err(Error::InvalidData {
                        offset: reader.bit_position() / 8,
                        detail: "residue format 0 partition size is not a multiple of codebook dimensions",
                    });
                }
            }
        }
    }
    Ok(Residue {
        format,
        begin,
        end,
        partition_size,
        classifications,
        classbook,
        cascade,
        books,
    })
}

/// Parses one mapping (section 4.2.4, mapping type 0).
///
/// # Errors
///
/// Returns `Truncated` when the header ends early, and
/// `InvalidData` when a coupling step, the reserved field, a mux
/// value, or a submap floor/residue index is invalid.
fn parse_mapping(
    reader: &mut LsbBitReader<'_>,
    channels: u32,
    floor_count: usize,
    residue_count: usize,
) -> Result<Mapping> {
    let submaps = if reader.read_bit()? {
        reader.read_u8(4)? + 1
    } else {
        1
    };
    let mut coupling_steps = Vec::new();
    if reader.read_bit()? {
        let steps = reader.read_bits(8)? as usize + 1;
        let channel_bits = ilog(channels - 1);
        for _ in 0..steps {
            let magnitude = reader.read_u8(channel_bits)?;
            let angle = reader.read_u8(channel_bits)?;
            if magnitude == angle
                || u32::from(magnitude) >= channels
                || u32::from(angle) >= channels
            {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "mapping coupling step is invalid",
                });
            }
            coupling_steps.push((magnitude, angle));
        }
    }
    if reader.read_bits(2)? != 0 {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "mapping reserved field must be zero",
        });
    }
    let mut mux = vec![0u8; channels as usize];
    if submaps > 1 {
        for channel_mux in &mut mux {
            let value = reader.read_u8(4)?;
            if value >= submaps {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "mapping mux exceeds submap count",
                });
            }
            *channel_mux = value;
        }
    }
    let mut submap_floor = Vec::with_capacity(submaps as usize);
    let mut submap_residue = Vec::with_capacity(submaps as usize);
    for _ in 0..submaps {
        // The unused time configuration placeholder (8 bits).
        let _ = reader.read_bits(8)?;
        let floor = reader.read_u8(8)?;
        if floor as usize >= floor_count {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "mapping submap floor exceeds floor count",
            });
        }
        let residue = reader.read_u8(8)?;
        if residue as usize >= residue_count {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "mapping submap residue exceeds residue count",
            });
        }
        submap_floor.push(floor);
        submap_residue.push(residue);
    }
    Ok(Mapping {
        submaps,
        coupling_steps,
        mux,
        submap_floor,
        submap_residue,
    })
}

/// Parses the setup header body (section 4.2.4) after the common
/// The five sections of a parsed setup header.
type SetupParts = (
    Vec<Codebook>,
    Vec<Floor>,
    Vec<Residue>,
    Vec<Mapping>,
    Vec<Mode>,
);

/// header prefix.
fn parse_setup(data: &[u8], identification: &VorbisIdentification) -> Result<SetupParts> {
    check_common_header(data, 5)?;
    let mut reader = LsbBitReader::new(&data[7..]);
    let codebook_count = reader.read_bits(8)? as usize + 1;
    let mut codebooks = Vec::with_capacity(codebook_count);
    for _ in 0..codebook_count {
        codebooks.push(parse_codebook(&mut reader)?);
    }
    let time_count = reader.read_bits(6)? as usize + 1;
    for _ in 0..time_count {
        if reader.read_bits(16)? != 0 {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "Vorbis time placeholder must be zero",
            });
        }
    }
    let floor_count = reader.read_bits(6)? as usize + 1;
    let mut floors = Vec::with_capacity(floor_count);
    for _ in 0..floor_count {
        let floor_type = reader.read_bits(16)?;
        match floor_type {
            0 => floors.push(Floor::Type0(parse_floor0(&mut reader, codebooks.len())?)),
            1 => floors.push(Floor::Type1(parse_floor1(&mut reader, codebooks.len())?)),
            _ => {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "Vorbis floor type above 1 is undecodable",
                });
            }
        }
    }
    let residue_count = reader.read_bits(6)? as usize + 1;
    let mut residues = Vec::with_capacity(residue_count);
    for _ in 0..residue_count {
        let residue_type = reader.read_bits(16)?;
        match residue_type {
            0..=2 => residues.push(parse_residue(
                &mut reader,
                u8::try_from(residue_type).unwrap_or(0),
                &codebooks,
            )?),
            _ => {
                return Err(Error::InvalidData {
                    offset: reader.bit_position() / 8,
                    detail: "Vorbis residue type above 2 is undecodable",
                });
            }
        }
    }
    let mapping_count = reader.read_bits(6)? as usize + 1;
    let mut mappings = Vec::with_capacity(mapping_count);
    for _ in 0..mapping_count {
        let mapping_type = reader.read_bits(16)?;
        if mapping_type != 0 {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "Vorbis mapping type must be zero",
            });
        }
        mappings.push(parse_mapping(
            &mut reader,
            identification.channels,
            floors.len(),
            residues.len(),
        )?);
    }
    let mode_count = reader.read_bits(6)? as usize + 1;
    let mut modes = Vec::with_capacity(mode_count);
    for _ in 0..mode_count {
        let blockflag = reader.read_bit()?;
        if reader.read_bits(16)? != 0 {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "Vorbis mode window type must be zero",
            });
        }
        if reader.read_bits(16)? != 0 {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "Vorbis mode transform type must be zero",
            });
        }
        let mapping = reader.read_u8(8)?;
        if mapping as usize >= mappings.len() {
            return Err(Error::InvalidData {
                offset: reader.bit_position() / 8,
                detail: "Vorbis mode mapping exceeds mapping count",
            });
        }
        modes.push(Mode { blockflag, mapping });
    }
    if !reader.read_bit()? {
        return Err(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "Vorbis setup framing bit must be 1",
        });
    }
    Ok((codebooks, floors, residues, mappings, modes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs bits LSB-first into bytes, mirroring `LsbBitReader`.
    /// Test helper only.
    struct LsbWriter {
        bytes: Vec<u8>,
        bit_pos: usize,
    }

    impl LsbWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                bit_pos: 0,
            }
        }

        fn write_bits(&mut self, value: u32, n: u32) {
            for i in 0..n {
                let bit = (value >> i) & 1;
                if self.bit_pos % 8 == 0 {
                    self.bytes.push(0);
                }
                if bit == 1 {
                    let last = self.bytes.len() - 1;
                    self.bytes[last] |= 1 << (self.bit_pos % 8);
                }
                self.bit_pos += 1;
            }
        }

        fn write_bytes(&mut self, bytes: &[u8]) {
            for &byte in bytes {
                self.write_bits(u32::from(byte), 8);
            }
        }

        /// Writes a Huffman codeword: the bits are written in
        /// stream order, most significant bit first, unlike the
        /// numeric fields above which are LSB-first.
        fn write_codeword(&mut self, code: u32, len: u32) {
            for i in (0..len).rev() {
                self.write_bits((code >> i) & 1, 1);
            }
        }

        fn finish(self) -> Vec<u8> {
            self.bytes
        }
    }

    /// Builds a codebook bitstream: sync, dimensions, entries,
    /// unordered non-sparse lengths, lookup type 0.
    fn codebook_bytes(dimensions: u16, lengths: &[u8]) -> Vec<u8> {
        let mut writer = LsbWriter::new();
        writer.write_bytes(&[0x42, 0x43, 0x56]);
        writer.write_bits(u32::from(dimensions), 16);
        writer.write_bits(u32::try_from(lengths.len()).unwrap_or(0), 24);
        writer.write_bits(0, 1); // unordered
        writer.write_bits(0, 1); // not sparse
        for &length in lengths {
            writer.write_bits(u32::from(length - 1), 5);
        }
        writer.write_bits(0, 4); // lookup type 0
        writer.finish()
    }

    #[test]
    fn float32_unpack_matches_known_values() {
        assert_eq!(float32_unpack(0), 0.0);
        // mantissa 0x100000, exponent 768: 2^20 * 2^-20 = 1.0.
        assert_eq!(float32_unpack(0x6010_0000), 1.0);
        assert_eq!(float32_unpack(0xE010_0000), -1.0);
        // mantissa 0x100000, exponent 769: 2.0.
        assert_eq!(float32_unpack(0x6030_0000), 2.0);
    }

    #[test]
    fn lookup1_values_matches_definition() {
        assert_eq!(lookup1_values(8, 3), 2);
        assert_eq!(lookup1_values(9, 2), 3);
        assert_eq!(lookup1_values(100, 1), 100);
        assert_eq!(lookup1_values(0, 2), 0);
        assert_eq!(lookup1_values(1, 5), 1);
    }

    #[test]
    fn floor1_inverse_db_table_spot_values() {
        let table = floor1_inverse_db_table();
        assert_eq!(table.len(), 256);
        assert!((table[0] - 1.064_986_3e-07).abs() < 1e-13);
        assert!((table[254] - 0.938_979_8).abs() < 1e-7);
        assert_eq!(table[255], 1.0);
    }

    #[test]
    fn codebook_parse_and_scalar_decode() {
        // Lengths [1, 2, 3, 3]: canonical codes 0, 10, 110, 111.
        let bytes = codebook_bytes(2, &[1, 2, 3, 3]);
        let mut reader = LsbBitReader::new(&bytes);
        let book = parse_codebook(&mut reader).unwrap();
        assert_eq!(book.dimensions, 2);
        assert_eq!(book.entries, 4);
        assert_eq!(book.lookup_type, 0);
        assert!(!book.has_lookup());
        // Decode the symbols 0, 1, 2, 3 in order: bits 0 10 110 111.
        let mut packet = LsbWriter::new();
        packet.write_codeword(0, 1);
        packet.write_codeword(0b10, 2);
        packet.write_codeword(0b110, 3);
        packet.write_codeword(0b111, 3);
        let packet = packet.finish();
        let mut reader = LsbBitReader::new(&packet);
        assert_eq!(book.decode_scalar(&mut reader).unwrap(), 0);
        assert_eq!(book.decode_scalar(&mut reader).unwrap(), 1);
        assert_eq!(book.decode_scalar(&mut reader).unwrap(), 2);
        assert_eq!(book.decode_scalar(&mut reader).unwrap(), 3);
    }

    #[test]
    fn codebook_bad_sync_is_rejected() {
        let mut bytes = codebook_bytes(2, &[1, 2, 3, 3]);
        bytes[0] = 0x00;
        let mut reader = LsbBitReader::new(&bytes);
        let err = parse_codebook(&mut reader).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn codebook_lookup_type_1_vector_decode() {
        // dimensions 2, entries 4, lookup type 1, minimum 0.0,
        // delta 1.0, value_bits 2, sequence_p 0, multiplicands [1, 2].
        // lookup1_values(4, 2) = 2.
        let mut writer = LsbWriter::new();
        writer.write_bytes(&[0x42, 0x43, 0x56]);
        writer.write_bits(2, 16);
        writer.write_bits(4, 24);
        writer.write_bits(0, 1); // unordered
        writer.write_bits(0, 1); // not sparse
        for &length in &[1u8, 2, 3, 3] {
            writer.write_bits(u32::from(length - 1), 5);
        }
        writer.write_bits(1, 4); // lookup type 1
        writer.write_bits(0, 32); // minimum 0.0
        writer.write_bits(0x6010_0000, 32); // delta 1.0
        writer.write_bits(1, 4); // value_bits 2
        writer.write_bits(0, 1); // sequence_p 0
        writer.write_bits(1, 2); // multiplicand 0
        writer.write_bits(2, 2); // multiplicand 1
        let bytes = writer.finish();
        let mut reader = LsbBitReader::new(&bytes);
        let book = parse_codebook(&mut reader).unwrap();
        assert_eq!(book.lookup_values.len(), 2);
        // Entry 1: i=0: (1/1)%2=1 -> 2.0; i=1: (1/2)%2=0 -> 1.0.
        // Entry 1 has code 10.
        let mut packet = LsbWriter::new();
        packet.write_codeword(0b10, 2);
        let packet = packet.finish();
        let mut reader = LsbBitReader::new(&packet);
        let mut out = [0.0f32; 2];
        book.decode_vector(&mut reader, &mut out).unwrap();
        assert_eq!(out, [2.0, 1.0]);
    }

    #[test]
    fn codebook_single_entry_sinks_one_bit() {
        // One entry, length 1. Both a 0 and a 1 bit decode to it.
        let bytes = codebook_bytes(1, &[1]);
        let mut reader = LsbBitReader::new(&bytes);
        let book = parse_codebook(&mut reader).unwrap();
        assert_eq!(book.single_entry, Some(0));
        let mut reader = LsbBitReader::new(&[0b0000_0000]);
        assert_eq!(book.decode_scalar(&mut reader).unwrap(), 0);
        let mut reader = LsbBitReader::new(&[0b0000_0001]);
        assert_eq!(book.decode_scalar(&mut reader).unwrap(), 0);
    }

    #[test]
    fn codebook_single_entry_wrong_length_is_rejected() {
        let bytes = codebook_bytes(1, &[2]);
        let mut reader = LsbBitReader::new(&bytes);
        let err = parse_codebook(&mut reader).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn identification_header_parses() {
        let mut writer = LsbWriter::new();
        writer.write_bytes(b"\x01vorbis");
        writer.write_bits(0, 32); // version
        writer.write_bits(2, 8); // channels
        writer.write_bits(44100, 32); // sample rate
        writer.write_bits(0, 32); // bitrate max
        writer.write_bits(128_000, 32); // bitrate nominal
        writer.write_bits(0, 32); // bitrate min
        writer.write_bits(8, 4); // blocksize_0 exponent: 256
        writer.write_bits(11, 4); // blocksize_1 exponent: 2048
        writer.write_bits(1, 1); // framing
        let bytes = writer.finish();
        let ident = parse_vorbis_identification(&bytes).unwrap();
        assert_eq!(ident.channels, 2);
        assert_eq!(ident.sample_rate, 44100);
        assert_eq!(ident.blocksize_0, 256);
        assert_eq!(ident.blocksize_1, 2048);
    }

    #[test]
    fn identification_bad_version_is_rejected() {
        let mut writer = LsbWriter::new();
        writer.write_bytes(b"\x01vorbis");
        writer.write_bits(1, 32); // version 1, not 0
        let bytes = writer.finish();
        let err = parse_vorbis_identification(&bytes).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn comment_header_parses_and_truncation_is_non_fatal() {
        let mut writer = LsbWriter::new();
        writer.write_bytes(b"\x03vorbis");
        writer.write_bits(4, 32);
        writer.write_bytes(b"test");
        writer.write_bits(1, 32);
        writer.write_bits(9, 32);
        writer.write_bytes(b"ENCODER=x");
        writer.write_bits(1, 1); // framing
        let bytes = writer.finish();
        let comment = parse_vorbis_comment(&bytes).unwrap();
        assert_eq!(comment.vendor, b"test");
        assert_eq!(comment.comments, vec![b"ENCODER=x".to_vec()]);
        // Truncated after the vendor: non-fatal, keeps the vendor.
        let truncated = &bytes[..7 + 4 + 4];
        let comment = parse_vorbis_comment(truncated).unwrap();
        assert_eq!(comment.vendor, b"test");
        assert_eq!(comment.comments.len(), 0);
    }

    #[test]
    fn floor1_curve_matches_hand_computed_values() {
        // Floor with partitions=1, class 0 (dimensions 2,
        // subclasses 0, subclass book 0), multiplier 1,
        // rangebits 4, X = [0, 16, 8, 12]. Book 0 decodes scalars
        // 0/1 from single bits (lengths [1, 1]).
        let book_bytes = codebook_bytes(1, &[1, 1]);
        let mut reader = LsbBitReader::new(&book_bytes);
        let book = parse_codebook(&mut reader).unwrap();
        let floor = Floor1 {
            partitions: 1,
            partition_class_list: vec![0],
            class_dimensions: vec![2],
            class_subclasses: vec![0],
            class_masterbooks: vec![None],
            subclass_books: vec![vec![0]],
            multiplier: 1,
            rangebits: 4,
            x_list: vec![0, 16, 8, 12],
        };
        // Packet: nonzero=1, Y[0]=100, Y[1]=50 (8 bits each),
        // Y[2]=0 (bit 0), Y[3]=1 (bit 1).
        let mut packet = LsbWriter::new();
        packet.write_bits(1, 1);
        packet.write_bits(100, 8);
        packet.write_bits(50, 8);
        packet.write_bits(0, 1);
        packet.write_bits(1, 1);
        let packet = packet.finish();
        let mut reader = LsbBitReader::new(&packet);
        let codebooks = [book];
        let curve = floor.decode(&mut reader, &codebooks, 16).unwrap().unwrap();
        assert_eq!(curve.len(), 16);
        let table = floor1_inverse_db_table();
        // Hand-computed integer floor before the dB lookup. The
        // curve renders in segments split at the step-2 points, so
        // the first segment runs (0, 100) -> (8, 75), not straight
        // to (12, 62): [100, 97, 94, 91, 88, 85, 82, 79, 75, 72, 69,
        // 66, 62, 59, 56, 53].
        let expected = [
            100, 97, 94, 91, 88, 85, 82, 79, 75, 72, 69, 66, 62, 59, 56, 53,
        ];
        for (i, &value) in expected.iter().enumerate() {
            assert_eq!(
                curve[i],
                table[usize::try_from(value).unwrap_or(0)],
                "index {i}"
            );
        }
    }

    #[test]
    fn floor1_unused_flag_returns_none() {
        let floor = Floor1 {
            partitions: 0,
            partition_class_list: vec![],
            class_dimensions: vec![],
            class_subclasses: vec![],
            class_masterbooks: vec![],
            subclass_books: vec![],
            multiplier: 1,
            rangebits: 4,
            x_list: vec![0, 16],
        };
        let mut reader = LsbBitReader::new(&[0b0000_0000]);
        let result = floor.decode(&mut reader, &[], 16).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn residue_header_parses() {
        // Book 0: dimensions 1, entries 4, lookup 0 (classbook).
        let book0 = codebook_bytes(1, &[2, 2, 2, 2]);
        let mut reader = LsbBitReader::new(&book0);
        let book0 = parse_codebook(&mut reader).unwrap();
        // Book 1: dimensions 2, entries 4, lookup type 1.
        let mut writer = LsbWriter::new();
        writer.write_bytes(&[0x42, 0x43, 0x56]);
        writer.write_bits(2, 16);
        writer.write_bits(4, 24);
        writer.write_bits(0, 1);
        writer.write_bits(0, 1);
        for &length in &[2u8, 2, 2, 2] {
            writer.write_bits(u32::from(length - 1), 5);
        }
        writer.write_bits(1, 4);
        writer.write_bits(0, 32);
        writer.write_bits(0x6010_0000, 32);
        writer.write_bits(1, 4);
        writer.write_bits(0, 1);
        writer.write_bits(0, 2);
        writer.write_bits(0, 2);
        let book1_bytes = writer.finish();
        let mut reader = LsbBitReader::new(&book1_bytes);
        let book1 = parse_codebook(&mut reader).unwrap();
        let codebooks = [book0, book1];
        // Residue header: begin 0, end 128, partition 8,
        // classifications 2, classbook 0, cascade [1, 0],
        // books[0][0] = 1.
        let mut writer = LsbWriter::new();
        writer.write_bits(0, 24);
        writer.write_bits(128, 24);
        writer.write_bits(7, 24); // partition size 8
        writer.write_bits(1, 6); // classifications 2
        writer.write_bits(0, 8); // classbook 0
        writer.write_bits(1, 3); // class 0 low bits
        writer.write_bits(0, 1); // class 0 bitflag
        writer.write_bits(0, 3); // class 1 low bits
        writer.write_bits(0, 1); // class 1 bitflag
        writer.write_bits(1, 8); // books[0][0] = 1
        let bytes = writer.finish();
        let mut reader = LsbBitReader::new(&bytes);
        let residue = parse_residue(&mut reader, 1, &codebooks).unwrap();
        assert_eq!(residue.begin, 0);
        assert_eq!(residue.end, 128);
        assert_eq!(residue.partition_size, 8);
        assert_eq!(residue.classifications, 2);
        assert_eq!(residue.classbook, 0);
        assert_eq!(residue.cascade, vec![1, 0]);
        assert_eq!(residue.books[0][0], Some(1));
        assert_eq!(residue.books[1], [None; 8]);
    }

    /// Builds a lookup-type-2 codebook with dimensions 2 and two
    /// entries: entry 0 is [1.0, 1.0], entry 1 is [2.0, 2.0]
    /// (minimum 0.0, delta 1.0, multiplicands [1, 1, 2, 2]).
    /// Codeword lengths are [1, 1], so entry 0 is the code `0`
    /// and entry 1 is the code `1`.
    fn floor0_book() -> Codebook {
        let mut writer = LsbWriter::new();
        writer.write_bytes(&[0x42, 0x43, 0x56]);
        writer.write_bits(2, 16);
        writer.write_bits(2, 24);
        writer.write_bits(0, 1); // unordered
        writer.write_bits(0, 1); // not sparse
        writer.write_bits(0, 5); // length 1
        writer.write_bits(0, 5); // length 1
        writer.write_bits(2, 4); // lookup type 2
        writer.write_bits(0, 32); // minimum 0.0
        writer.write_bits(0x6010_0000, 32); // delta 1.0
        writer.write_bits(2, 4); // value_bits 3
        writer.write_bits(0, 1); // sequence_p 0
        for &multiplicand in &[1u32, 1, 2, 2] {
            writer.write_bits(multiplicand, 3);
        }
        let bytes = writer.finish();
        let mut reader = LsbBitReader::new(&bytes);
        parse_codebook(&mut reader).unwrap()
    }

    fn floor0_test_setup(order: u8) -> Floor0 {
        Floor0 {
            order,
            rate: 8000,
            bark_map_size: 64,
            amplitude_bits: 8,
            amplitude_offset: 10,
            book_list: vec![0],
        }
    }

    #[test]
    fn floor0_decode_even_order_matches_hand_computed_curve() {
        // Order 2, amplitude 255, one VQ vector (entry 0):
        // coefficients [1.0, 1.0]. Expected curve for n = 8 was
        // computed independently from spec section 6.2.3 (see the
        // subagent report for the reference calculation); bin 0
        // is the fully hand-checked value: map[0] = 0, so
        // omega = 0, p = 0, q = 4*(cos(1)-1)^2, and the value is
        // exp(0.11512925*(10/sqrt(q) - 10)) = 1.106205.
        let book = floor0_book();
        let floor0 = floor0_test_setup(2);
        let mut packet = LsbWriter::new();
        packet.write_bits(255, 8); // amplitude
        packet.write_codeword(0, 1); // VQ entry 0
        let packet = packet.finish();
        let mut reader = LsbBitReader::new(&packet);
        let curve = floor0.decode(&mut reader, &[book], 8).unwrap().unwrap();
        let expected = [
            1.106_205f32,
            25.390_16,
            1.020_756,
            0.573_195_2,
            0.501_666_9,
            0.477_334_6,
            0.466_346_2,
            0.460_738_4,
        ];
        assert_eq!(curve.len(), 8);
        for (index, (&got, &want)) in curve.iter().zip(expected.iter()).enumerate() {
            assert!((got - want).abs() < 1e-5, "bin {index}: {got} != {want}");
        }
    }

    #[test]
    fn floor0_decode_odd_order_accumulates_last_across_vectors() {
        // Order 3 needs two VQ vectors (dimensions 2): the first
        // gives [1.0, 1.0] with last 0, the second gives [1.0,
        // 1.0] plus the running last 1.0, so [2.0, 2.0]; the
        // coefficients are [1.0, 1.0, 2.0] after truncation. If
        // `last` were reset per vector the coefficients would be
        // [1.0, 1.0, 1.0] and bin 0 would read 4.819799
        // instead of 0.7656405 (both computed independently
        // from spec section 6.2.3).
        let book = floor0_book();
        let floor0 = floor0_test_setup(3);
        let mut packet = LsbWriter::new();
        packet.write_bits(255, 8); // amplitude
        packet.write_codeword(0, 1); // VQ entry 0
        packet.write_codeword(0, 1); // VQ entry 0 again
        let packet = packet.finish();
        let mut reader = LsbBitReader::new(&packet);
        let curve = floor0.decode(&mut reader, &[book], 8).unwrap().unwrap();
        let expected = [
            0.765_640_5f32,
            8.855_375,
            0.915_988_7,
            0.610_539_1,
            0.578_239_8,
            0.580_579_9,
            0.589_264_4,
            0.597_49,
        ];
        assert_eq!(curve.len(), 8);
        for (index, (&got, &want)) in curve.iter().zip(expected.iter()).enumerate() {
            assert!((got - want).abs() < 1e-5, "bin {index}: {got} != {want}");
        }
    }

    #[test]
    fn floor0_decode_zero_amplitude_is_unused() {
        let book = floor0_book();
        let floor0 = floor0_test_setup(2);
        let mut packet = LsbWriter::new();
        packet.write_bits(0, 8); // amplitude 0
        let packet = packet.finish();
        let mut reader = LsbBitReader::new(&packet);
        assert!(floor0.decode(&mut reader, &[book], 8).unwrap().is_none());
    }

    #[test]
    fn floor0_decode_end_of_packet_is_unused() {
        // Amplitude is present, but the packet ends before the
        // VQ vector: nominal end-of-packet, so unused.
        let book = floor0_book();
        let floor0 = floor0_test_setup(2);
        let mut packet = LsbWriter::new();
        packet.write_bits(255, 8); // amplitude
        let packet = packet.finish();
        let mut reader = LsbBitReader::new(&packet);
        assert!(floor0.decode(&mut reader, &[book], 8).unwrap().is_none());
    }

    #[test]
    fn floor0_decode_out_of_range_book_number_is_invalid_data() {
        // Two books in the list, but the packet selects book 2:
        // the book number is read with ilog(2) = 2 bits.
        let book = floor0_book();
        let floor0 = Floor0 {
            book_list: vec![0, 0],
            ..floor0_test_setup(2)
        };
        let mut packet = LsbWriter::new();
        packet.write_bits(255, 8); // amplitude
        packet.write_bits(2, 2); // book number 2, out of range
        let packet = packet.finish();
        let mut reader = LsbBitReader::new(&packet);
        let err = floor0.decode(&mut reader, &[book], 8).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn floor0_decode_degenerate_coefficients_are_invalid_data() {
        // Coefficients [0.0, 0.0] (entry 0 of a book whose values
        // are all zero) make p + q vanish at omega = 0, so the
        // curve value is undefined; the decode is rejected
        // rather than returning a non-finite curve.
        let mut writer = LsbWriter::new();
        writer.write_bytes(&[0x42, 0x43, 0x56]);
        writer.write_bits(2, 16);
        writer.write_bits(1, 24);
        writer.write_bits(0, 1); // unordered
        writer.write_bits(0, 1); // not sparse
        writer.write_bits(0, 5); // length 1
        writer.write_bits(2, 4); // lookup type 2
        writer.write_bits(0, 32); // minimum 0.0
        writer.write_bits(0x6010_0000, 32); // delta 1.0
        writer.write_bits(0, 4); // value_bits 1
        writer.write_bits(0, 1); // sequence_p 0
        writer.write_bits(0, 1); // multiplicand 0
        writer.write_bits(0, 1); // multiplicand 0
        let bytes = writer.finish();
        let mut reader = LsbBitReader::new(&bytes);
        let book = parse_codebook(&mut reader).unwrap();
        let floor0 = floor0_test_setup(2);
        let mut packet = LsbWriter::new();
        packet.write_bits(255, 8); // amplitude
        packet.write_codeword(0, 1); // the single entry
        let packet = packet.finish();
        let mut reader = LsbBitReader::new(&packet);
        let err = floor0.decode(&mut reader, &[book], 8).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }
}
