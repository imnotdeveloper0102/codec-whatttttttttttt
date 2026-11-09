//! Modified discrete cosine transform (MDCT), shared by Vorbis and Opus
//! (CELT mode).
//!
//! Vorbis I spec section 4.3.7 defers the MDCT definition to the Sporer
//! paper; this module implements the Vorbis MDCT, with spectrum length
//! `N` and time length `2N`:
//!
//! ```text
//! forward:  X[k] = (2/N) * sum_{n=0}^{2N-1} x[n] * cos((pi/N) * (n + 1/2 + N/2) * (k + 1/2))
//! inverse:  y[n] = sum_{k=0}^{N-1} X[k] * cos((pi/N) * (n + 1/2 + N/2) * (k + 1/2))
//! ```
//!
//! The angle uses `N` as the spectrum length (half the block length):
//! writing the block length as `2N` makes the forward `X[k] =
//! sum x[n] * cos((2*pi/(2N)) * (n + 1/2 + (2N)/4) * (k + 1/2))`,
//! the standard form. The scale split matches libvorbis (`mdct.c`):
//! `mdct_forward` multiplies its output by `4/n` (here `2/N`, with
//! `n = 2N`), while `mdct_backward` applies no scale at all. That
//! split is not arbitrary: the cosine matrix satisfies
//! `C * C^T = N * I`, so the spectrum-domain round trip is
//! `forward(inverse(X)) = 2 * X`, while `C^T * C` has diagonal
//! `N/2`, so the forward scale `2/N` times that diagonal is exactly
//! 1 and the overlap-add of consecutive windowed blocks reproduces
//! the input. A single block is therefore not an identity in either
//! direction (the aliasing terms cancel only in the overlap-add),
//! but the overlap-add identity is exact, which the unit tests pin.
//! This matters because the spectrum stored in a Vorbis file is in
//! libvorbis's scaled convention: a decoder inverse that carried
//! the `2/N` scale would shrink every decoded block by `2/N`
//! (128x too small for a 512-sample block).
//! The inverse applies the caller-supplied window; window selection
//! and overlap-add live in the codec modules, not here, so the MDCT
//! stays testable against known vectors without codec state.
//!
//! The cosine matrix is evaluated through a table of `cos(pi*j/(4N))`
//! for `j` in `0..8N`: the angle `(pi/N)*(n + 1/2 + N/2)*(k + 1/2)`
//! equals `pi*m/(4N)` with `m = (2n + 1 + N)*(2k + 1)`, an integer, so
//! reducing `m` modulo `8N` in integer arithmetic is exact and avoids
//! the precision loss of forming the large angle in `f32`. The
//! implementation is a direct O(N^2) sum; an FFT-based MDCT is a later
//! optimisation, not a correctness requirement for this phase.
//!
//! CELT (RFC 6716 section 4.3.7) uses the same cosine matrix:
//! its inverse MDCT (`clt_mdct_backward`) equals the direct cosine
//! sum at scale 1.0, the same scale `mdct_inverse` computes, so no
//! pre-scale is applied or needed on the CELT path (verified against
//! the reference build in `celt_reconstruct.rs`); do not change the
//! scale here, which is the Vorbis convention.

use crate::error::{Error, Result};

/// Largest spectrum length accepted (half of the largest Vorbis
/// blocksize, 8192). Bounds the cosine table at `8 * 4096` entries.
const MAX_SPECTRUM_LEN: usize = 4096;

/// Cosine table for one MDCT size: `table[j] = cos(pi*j/(4N))`.
fn cosine_table(spectrum_len: usize) -> Vec<f32> {
    let n = spectrum_len;
    let mut table = Vec::with_capacity(8 * n);
    // `spectrum_len` is at most MAX_SPECTRUM_LEN (4096), so `j` and
    // `4 * n` fit in `u16` and the `f32` conversion is exact.
    let n16 = u16::try_from(n).unwrap_or(0);
    for j in 0..8 * n16 {
        let angle = std::f32::consts::PI * f32::from(j) / (4.0 * f32::from(n16));
        table.push(angle.cos());
    }
    table
}

/// Cosine of the MDCT angle for spectrum index `k` and time index `n`,
/// via the table. `spectrum_len` is `N`.
fn mdct_cos(table: &[f32], spectrum_len: usize, k: usize, n: usize) -> f32 {
    let modulus = 8 * spectrum_len;
    // m = (2n + 1 + N) * (2k + 1); all factors fit in usize for the
    // accepted sizes, but the product is taken in u64 so a future
    // larger limit cannot overflow silently.
    let m = (2 * u64::try_from(n).unwrap_or(0) + 1 + u64::try_from(spectrum_len).unwrap_or(0))
        * (2 * u64::try_from(k).unwrap_or(0) + 1);
    // The remainder is below `modulus` (at most 8 * 4096), so the
    // conversion back to `usize` cannot fail.
    let index = usize::try_from(m % u64::try_from(modulus).unwrap_or(0)).unwrap_or(0);
    table[index]
}

/// Forward MDCT: `input` holds `2N` time samples, `output` receives `N`
/// spectrum coefficients. The output is scaled by `2/N` (libvorbis
/// `mdct_forward` multiplies by `4/n`, with `n = 2N` the block
/// length); the inverse applies no scale, so the pair matches the
/// spectrum convention stored in Vorbis files.
///
/// # Errors
///
/// Returns `InvalidArgument` when the lengths are inconsistent or the
/// spectrum length exceeds the limit, and `InvalidData` when a length
/// is zero.
pub fn mdct_forward(input: &[f32], output: &mut [f32]) -> Result<()> {
    let spectrum_len = output.len();
    if spectrum_len == 0 || spectrum_len > MAX_SPECTRUM_LEN {
        return Err(Error::InvalidArgument(
            "mdct spectrum length must be between 1 and 4096",
        ));
    }
    if input.len() != 2 * spectrum_len {
        return Err(Error::InvalidArgument(
            "mdct forward input must be twice the output length",
        ));
    }
    let table = cosine_table(spectrum_len);
    // `spectrum_len` is at most MAX_SPECTRUM_LEN (4096), so the
    // `f32` conversion is exact.
    let scale = 2.0f32 / f32::from(u16::try_from(spectrum_len).unwrap_or(0));
    for (k, out) in output.iter_mut().enumerate() {
        let mut sum = 0.0f32;
        for (n, &sample) in input.iter().enumerate() {
            sum += sample * mdct_cos(&table, spectrum_len, k, n);
        }
        *out = sum * scale;
    }
    Ok(())
}

/// Inverse MDCT: `input` holds `N` spectrum coefficients, `output`
/// receives `2N` time samples, each multiplied by the corresponding
/// `window` element. No scale is applied here: libvorbis
/// `mdct_backward` applies none, and the forward's `2/N` scale is
/// what makes the overlap-add of consecutive windowed blocks
/// reproduce the input (see the module docs). A single block is not
/// an identity in either direction, and the overlap-add of
/// consecutive windowed blocks is what reproduces the input.
///
/// # Errors
///
/// Returns `InvalidArgument` when the lengths are inconsistent or the
/// spectrum length exceeds the limit, and `InvalidData` when a length
/// is zero.
pub fn mdct_inverse(input: &[f32], output: &mut [f32], window: &[f32]) -> Result<()> {
    let spectrum_len = input.len();
    if spectrum_len == 0 || spectrum_len > MAX_SPECTRUM_LEN {
        return Err(Error::InvalidArgument(
            "mdct spectrum length must be between 1 and 4096",
        ));
    }
    if output.len() != 2 * spectrum_len || window.len() != 2 * spectrum_len {
        return Err(Error::InvalidArgument(
            "mdct inverse output and window must be twice the input length",
        ));
    }
    let table = cosine_table(spectrum_len);
    for (n, out) in output.iter_mut().enumerate() {
        let mut sum = 0.0f32;
        for (k, &coeff) in input.iter().enumerate() {
            sum += coeff * mdct_cos(&table, spectrum_len, k, n);
        }
        *out = sum * window[n];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn forward_of_inverse_is_twice_spectrum() {
        // The MDCT is a lapped transform: a single block is not an
        // identity in either direction. With the libvorbis scale
        // split (forward carries `2/N`, inverse carries none) and
        // `C * C^T = N * I`, the exact spectrum-domain round trip
        // is forward(inverse(X)) = 2 * X. The time-domain identity
        // only appears after the overlap-add of consecutive blocks;
        // see the Vorbis packet decoder tests for that property.
        let n = 64usize;
        let mut spectrum = vec![0.0f32; n];
        for (i, value) in spectrum.iter_mut().enumerate() {
            // The index is below 64, so the `u16` conversion is exact
            // and the value below 11 is exact in `f32`.
            let v = u16::try_from(i).unwrap_or(0);
            *value = f32::from((v * 7 + 3) % 11) * 0.37 - 1.0;
        }
        let window = vec![1.0f32; 2 * n];
        let mut back = vec![0.0f32; 2 * n];
        mdct_inverse(&spectrum, &mut back, &window).unwrap();
        let mut spectrum2 = vec![0.0f32; n];
        mdct_forward(&back, &mut spectrum2).unwrap();
        for k in 0..n {
            let expected = 2.0 * spectrum[k];
            assert!(
                approx_eq(spectrum2[k], expected, 1e-3),
                "k={k}: {} != {expected}",
                spectrum2[k]
            );
        }
    }

    #[test]
    fn inverse_of_impulse_spectrum_matches_direct_sum() {
        // A single unit spectrum coefficient k=0: the inverse
        // applies no scale (libvorbis `mdct_backward`), so the
        // output is exactly cos of the MDCT angle per sample.
        // This pins the absolute scale of the inverse: any hidden
        // `2/N` factor here would shrink every decoded Vorbis
        // block by `2/N`.
        let n = 8usize;
        let mut spectrum = vec![0.0f32; n];
        // `n` is 8, so the `f32` conversion is exact.
        let nf = f32::from(u16::try_from(n).unwrap_or(0));
        spectrum[0] = 1.0;
        let window = vec![1.0f32; 2 * n];
        let mut time = vec![0.0f32; 2 * n];
        mdct_inverse(&spectrum, &mut time, &window).unwrap();
        for (i, &sample) in time.iter().enumerate() {
            let i_f = f32::from(u16::try_from(i).unwrap_or(0));
            let angle = std::f32::consts::PI / nf * (i_f + 0.5 + nf / 2.0) * 0.5;
            assert!(
                approx_eq(sample, angle.cos(), 1e-5),
                "sample {i}: {sample} != {}",
                angle.cos()
            );
        }
    }

    #[test]
    fn mismatched_lengths_return_invalid_argument() {
        let mut out = vec![0.0f32; 4];
        assert!(matches!(
            mdct_forward(&[0.0; 6], &mut out),
            Err(Error::InvalidArgument(_))
        ));
        let mut time = vec![0.0f32; 8];
        let window = [1.0f32; 8];
        assert!(matches!(
            mdct_inverse(&[0.0; 4], &mut time, &window[..7]),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn zero_spectrum_length_is_rejected() {
        let mut out: Vec<f32> = Vec::new();
        assert!(matches!(
            mdct_forward(&[], &mut out),
            Err(Error::InvalidArgument(_))
        ));
    }
}
