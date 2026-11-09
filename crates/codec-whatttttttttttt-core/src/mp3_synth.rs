//! Layer III frequency-to-time conversion (ISO/IEC 11172-3 sections
//! 2.4.3.4.7-2.4.3.4.11 and Annex D).
//!
//! After Huffman decoding and stereo processing, each granule holds
//! 576 frequency lines per channel (32 subbands of 18 lines). The
//! stages here, in order, are:
//!
//! * `reorder_short`: short-block lines arrive grouped by
//!   scalefactor band and window; they are reordered to subband
//!   order, interleaved by window (section 2.4.3.4.7).
//! * `antialias`: the butterfly between neighbouring subbands
//!   (section 2.4.3.4.8).
//! * `imdct_granule`: the inverse MDCT per subband, through the
//!   shared `mdct_inverse` (the MP3 IMDCT has the same cosine
//!   matrix as the Vorbis one in `mdct.rs`, with spectrum length
//!   18 for long blocks and 6 for short windows), windowed with
//!   the long/start/stop/short windows of section 2.4.3.4.10 and
//!   overlap-added with the previous granule (section 2.4.3.4.11).
//!   Long blocks produce 36 samples per subband, of which the
//!   first 18 are output and the last 18 are kept as overlap.
//!   Short blocks are three 12-sample windows placed at offsets
//!   6, 12 and 18 of the 36-sample block, so the block's first
//!   and last 6 samples are zero (the window-switching diagram).
//! * `change_sign`: odd subbands have their odd lines negated
//!   before synthesis (section 2.4.3.4.10).
//! * `synth_granule`: the 32-band polyphase synthesis filterbank
//!   (Annex D): a 32-point DCT-II per time slot, then the windowed
//!   polyphase sum that produces 32 PCM samples per slot. The
//!   DCT-II and the synthesis sums follow the scalar reference
//!   implementation; the window coefficients are
//!   `MP3_POLYPHASE_WIN` and the DCT secants `MP3_DCT_SEC`.

use crate::error::{Error, Result};
use crate::mdct::mdct_inverse;
use crate::mp3_tables::{MP3_ANTIALIAS, MP3_DCT_SEC, MP3_POLYPHASE_WIN};

/// Number of frequency lines per granule and channel.
pub(crate) const MP3_GRANULE_LINES: usize = 576;
/// Subbands per granule.
const SUBBANDS: usize = 32;
/// Lines per subband.
const SUBBAND_LINES: usize = 18;
/// Polyphase state floats (15 history slots of 64).
pub(crate) const MP3_QMF_STATE_LEN: usize = 15 * 64;
/// Scratch floats for one synthesis granule: 18 time slots plus 15
/// history slots, of 64.
const SYNTH_SCRATCH_LEN: usize = (18 + 15) * 64;

/// Long-block window (ISO/IEC 11172-3 section 2.4.3.4.10):
/// `sin(pi/36 * (n + 1/2))`.
#[allow(clippy::cast_precision_loss)]
fn long_window() -> [f32; 36] {
    let mut window = [0.0f32; 36];
    for (n, value) in window.iter_mut().enumerate() {
        *value = (std::f32::consts::PI / 36.0 * (n as f32 + 0.5)).sin();
    }
    window
}

/// Start window (block type 1): long rising half, flat 1 for six
/// samples, falling short window, then six zeros.
#[allow(clippy::cast_precision_loss)]
fn start_window() -> [f32; 36] {
    let mut window = [0.0f32; 36];
    for (n, value) in window[..18].iter_mut().enumerate() {
        *value = (std::f32::consts::PI / 36.0 * (n as f32 + 0.5)).sin();
    }
    window[18..24].fill(1.0);
    for (n, value) in window.iter_mut().enumerate().skip(24).take(6) {
        *value = (std::f32::consts::PI / 12.0 * (29.5 - n as f32)).sin();
    }
    window
}

/// Stop window (block type 3): six zeros, rising short window,
/// flat 1 for six samples, then the long falling half.
#[allow(clippy::cast_precision_loss)]
fn stop_window() -> [f32; 36] {
    let mut window = [0.0f32; 36];
    for (n, value) in window.iter_mut().enumerate().skip(6).take(6) {
        *value = (std::f32::consts::PI / 12.0 * (n as f32 - 5.5)).sin();
    }
    window[12..18].fill(1.0);
    for (n, value) in window.iter_mut().enumerate().skip(18) {
        *value = (std::f32::consts::PI / 36.0 * (n as f32 + 0.5)).sin();
    }
    window
}

/// Short window: `sin(pi/12 * (n + 1/2))`, 12 samples.
#[allow(clippy::cast_precision_loss)]
fn short_window() -> [f32; 12] {
    let mut window = [0.0f32; 12];
    for (n, value) in window.iter_mut().enumerate() {
        *value = (std::f32::consts::PI / 12.0 * (n as f32 + 0.5)).sin();
    }
    window
}

/// Reorders the short-block region of a granule (section 2.4.3.4.7).
/// `start` is the first line of the short region
/// (`n_long_bands * 18`); `sfbtab` is the scalefactor-band table
/// already offset past the long bands. Lines arrive as
/// (band, window, line) and leave as (line, window) within each
/// subband, which is what the short IMDCT below expects.
///
/// # Errors
///
/// Returns `InvalidData` when the band table runs out or a band
/// width would read past the region.
pub(crate) fn reorder_short(
    grbuf: &mut [f32; MP3_GRANULE_LINES],
    start: usize,
    sfbtab: &[u8],
) -> Result<()> {
    let mut scratch = [0.0f32; MP3_GRANULE_LINES];
    let mut src_idx = 0usize;
    let mut dst_idx = 0usize;
    let mut sfb_idx = 0usize;
    loop {
        let len = *sfbtab.get(sfb_idx).ok_or(Error::InvalidData {
            offset: 0,
            detail: "mp3 reorder scalefactor band index out of range",
        })? as usize;
        if len == 0 {
            break;
        }
        sfb_idx += 3;
        for i in 0..len {
            for window in 0..3 {
                let value =
                    *grbuf
                        .get(start + src_idx + window * len + i)
                        .ok_or(Error::InvalidData {
                            offset: 0,
                            detail: "mp3 reorder reads past the granule",
                        })?;
                *scratch.get_mut(dst_idx).ok_or(Error::InvalidData {
                    offset: 0,
                    detail: "mp3 reorder writes past the granule",
                })? = value;
                dst_idx += 1;
            }
        }
        src_idx += 3 * len;
    }
    grbuf[start..start + dst_idx].copy_from_slice(&scratch[..dst_idx]);
    Ok(())
}

/// Antialias butterfly (section 2.4.3.4.8) over the first `nbands`
/// subband boundaries. A non-positive count does nothing (pure
/// short blocks have no long boundaries to filter).
#[allow(clippy::cast_sign_loss)]
pub(crate) fn antialias(grbuf: &mut [f32; MP3_GRANULE_LINES], nbands: i32) {
    let cs = MP3_ANTIALIAS[0];
    let ca = MP3_ANTIALIAS[1];
    let mut band = 0i32;
    while band < nbands {
        // Boundary between subband `band` and `band + 1`: the last
        // eight lines of the lower band pair with the first eight
        // of the upper band.
        let base = (band as usize + 1) * SUBBAND_LINES;
        for i in 0..8 {
            let upper = grbuf[base + i];
            let lower = grbuf[base - 1 - i];
            grbuf[base + i] = upper * cs[i] - lower * ca[i];
            grbuf[base - 1 - i] = upper * ca[i] + lower * cs[i];
        }
        band += 1;
    }
}

/// Inverse MDCT plus overlap-add for one granule (sections
/// 2.4.3.4.10-2.4.3.4.11). `block_type` is 0 (long), 1 (start),
/// 2 (short) or 3 (stop); `n_long_bands` is the count of leading
/// subbands that stay long in a mixed block (0 otherwise).
/// `overlap` holds the previous granule's second half, 18 samples
/// per subband, and is replaced by this granule's.
///
/// # Errors
///
/// Returns the `mdct_inverse` errors for inconsistent lengths;
/// the lengths here are fixed by construction, so this only fires
/// on internal misuse.
pub(crate) fn imdct_granule(
    grbuf: &mut [f32; MP3_GRANULE_LINES],
    overlap: &mut [f32; MP3_GRANULE_LINES],
    block_type: u8,
    n_long_bands: usize,
) -> Result<()> {
    let long = long_window();
    let start = start_window();
    let stop = stop_window();
    let short = short_window();
    // Long subbands: 0..n_long_bands for mixed blocks, all of them
    // for long/start/stop blocks. Mixed blocks have block type 2
    // (short) with long leading subbands; the long subbands use
    // the plain long window, while start/stop blocks use their own
    // window on every subband.
    let long_count = if block_type == 2 {
        n_long_bands
    } else {
        SUBBANDS
    };
    let long_window_for = |band: usize| -> &[f32; 36] {
        if block_type == 2 && band < n_long_bands {
            &long
        } else {
            match block_type {
                1 => &start,
                3 => &stop,
                _ => &long,
            }
        }
    };
    for band in 0..SUBBANDS {
        let base = band * SUBBAND_LINES;
        if band < long_count || block_type != 2 {
            let window = long_window_for(band);
            let mut timed = [0.0f32; 36];
            mdct_inverse(&grbuf[base..base + SUBBAND_LINES], &mut timed, window)?;
            for i in 0..SUBBAND_LINES {
                grbuf[base + i] = timed[i] + overlap[base + i];
                overlap[base + i] = timed[SUBBAND_LINES + i];
            }
        } else {
            // Short subband: three 6-line windows, interleaved by
            // window in the spectrum (`spec[3*k + w]`), each
            // producing 12 windowed samples placed at offsets
            // 6, 12 and 18 of the 36-sample block.
            let mut timed = [0.0f32; 36];
            for w in 0..3 {
                let mut spec = [0.0f32; 6];
                for k in 0..6 {
                    spec[k] = grbuf[base + 3 * k + w];
                }
                let mut windowed = [0.0f32; 12];
                mdct_inverse(&spec, &mut windowed, &short)?;
                for i in 0..12 {
                    timed[6 + 6 * w + i] += windowed[i];
                }
            }
            for i in 0..SUBBAND_LINES {
                grbuf[base + i] = timed[i] + overlap[base + i];
                overlap[base + i] = timed[SUBBAND_LINES + i];
            }
        }
    }
    Ok(())
}

/// Negates the odd lines of the odd subbands (section 2.4.3.4.10).
pub(crate) fn change_sign(grbuf: &mut [f32; MP3_GRANULE_LINES]) {
    for band in (1..SUBBANDS).step_by(2) {
        let base = band * SUBBAND_LINES;
        for i in (1..SUBBAND_LINES).step_by(2) {
            grbuf[base + i] = -grbuf[base + i];
        }
    }
}

/// Scales a polyphase sum to an f32 sample in [-1, 1]. The
/// reference synthesis works in units where 32768 is full scale.
fn scale_pcm(value: f32) -> f32 {
    (value / 32768.0).clamp(-1.0, 1.0)
}

/// One polyphase pair: two output samples, 16 slots apart, from the
/// `z` history at absolute `z_base` in the synthesis scratch.
/// The integer coefficients are the Annex D window in scaled form.
#[allow(clippy::cast_precision_loss)]
fn synth_pair(pcm: &mut [f32], at: usize, nch: usize, z: &[f32], z_base: usize) {
    let mut a = (z[z_base + 14 * 64] - z[z_base]) * 29.0;
    a += (z[z_base + 64] + z[z_base + 13 * 64]) * 213.0;
    a += (z[z_base + 12 * 64] - z[z_base + 2 * 64]) * 459.0;
    a += (z[z_base + 3 * 64] + z[z_base + 11 * 64]) * 2037.0;
    a += (z[z_base + 10 * 64] - z[z_base + 4 * 64]) * 5153.0;
    a += (z[z_base + 5 * 64] + z[z_base + 9 * 64]) * 6574.0;
    a += (z[z_base + 8 * 64] - z[z_base + 6 * 64]) * 37489.0;
    a += z[z_base + 7 * 64] * 75038.0;
    pcm[at] = scale_pcm(a);
    let z_base = z_base + 2;
    let mut a = z[z_base + 14 * 64] * 104.0;
    a += z[z_base + 12 * 64] * 1567.0;
    a += z[z_base + 10 * 64] * 9727.0;
    a += z[z_base + 8 * 64] * 64019.0;
    a += z[z_base + 6 * 64] * -9975.0;
    a += z[z_base + 4 * 64] * -45.0;
    a += z[z_base + 2 * 64] * 146.0;
    a += z[z_base] * -5.0;
    pcm[at + 16 * nch] = scale_pcm(a);
}

/// The 32-point DCT-II of the polyphase filterbank (Annex D),
/// in place on one channel's 576 subband samples. `n` is the
/// number of time slots (18 for Layer III).
#[allow(clippy::excessive_precision)]
fn dct_ii(grbuf: &mut [f32; MP3_GRANULE_LINES], n: usize) {
    for k in 0..n {
        // `t[row][i]`: the four partial sums for each of the
        // eight butterfly stages.
        let mut t = [[0.0f32; 8]; 4];
        for i in 0..8 {
            let x0 = grbuf[k + i * SUBBAND_LINES];
            let x1 = grbuf[k + (15 - i) * SUBBAND_LINES];
            let x2 = grbuf[k + (16 + i) * SUBBAND_LINES];
            let x3 = grbuf[k + (31 - i) * SUBBAND_LINES];
            let sum0 = x0 + x3;
            let sum1 = x1 + x2;
            let diff2 = (x1 - x2) * MP3_DCT_SEC[3 * i];
            let diff3 = (x0 - x3) * MP3_DCT_SEC[3 * i + 1];
            t[0][i] = sum0 + sum1;
            t[1][i] = (sum0 - sum1) * MP3_DCT_SEC[3 * i + 2];
            t[2][i] = diff3 + diff2;
            t[3][i] = (diff3 - diff2) * MP3_DCT_SEC[3 * i + 2];
        }
        for t_row in &mut t {
            let mut x = *t_row;
            let mut xt = x[0] - x[7];
            x[0] += x[7];
            x[7] = x[1] - x[6];
            x[1] += x[6];
            x[6] = x[2] - x[5];
            x[2] += x[5];
            x[5] = x[3] - x[4];
            x[3] += x[4];
            x[4] = x[0] - x[3];
            x[0] += x[3];
            x[3] = x[1] - x[2];
            x[1] += x[2];
            let out0 = x[0] + x[1];
            let out4 = (x[0] - x[1]) * 0.707_106_77;
            x[5] += x[6];
            x[6] = (x[6] + x[7]) * 0.707_106_77;
            x[7] += xt;
            x[3] = (x[3] + x[4]) * 0.707_106_77;
            x[5] -= x[7] * 0.198_912_367;
            x[7] += x[5] * 0.382_683_432;
            x[5] -= x[7] * 0.198_912_367;
            x[0] = xt - x[6];
            xt += x[6];
            let out1 = (xt + x[7]) * 0.509_795_61;
            let out2 = (x[4] + x[3]) * 0.541_196_11;
            let out3 = (x[0] - x[5]) * 0.601_344_88;
            let out5 = (x[0] + x[5]) * 0.899_976_19;
            let out6 = (x[4] - x[3]) * 1.306_563_02;
            let out7 = (xt - x[7]) * 2.562_915_56;
            *t_row = [out0, out1, out2, out3, out4, out5, out6, out7];
        }
        for i in 0..7 {
            let base = k + i * 4 * SUBBAND_LINES;
            grbuf[base] = t[0][i];
            grbuf[base + SUBBAND_LINES] = t[2][i] + t[3][i] + t[3][i + 1];
            grbuf[base + 2 * SUBBAND_LINES] = t[1][i] + t[1][i + 1];
            grbuf[base + 3 * SUBBAND_LINES] = t[2][i + 1] + t[3][i] + t[3][i + 1];
        }
        let base = k + 7 * 4 * SUBBAND_LINES;
        grbuf[base] = t[0][7];
        grbuf[base + SUBBAND_LINES] = t[2][7] + t[3][7];
        grbuf[base + 2 * SUBBAND_LINES] = t[1][7];
        grbuf[base + 3 * SUBBAND_LINES] = t[3][7];
    }
}

/// Polyphase synthesis for one granule (Annex D). `grbuf` holds
/// both channels' DCT-II inputs, left first (`1152` floats);
/// `pcm` receives `576 * nch` interleaved f32 samples.
/// `qmf_state` is the filterbank history, updated in place.
/// `nbands` is the time-slot count (18 for Layer III).
#[allow(
    clippy::similar_names,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
pub(crate) fn synth_granule(
    qmf_state: &mut [f32; MP3_QMF_STATE_LEN],
    grbuf: &[f32; MP3_GRANULE_LINES * 2],
    nbands: usize,
    nch: usize,
    pcm: &mut [f32],
) {
    let mut dct = *grbuf;
    for ch in 0..nch {
        let mut channel = [0.0f32; MP3_GRANULE_LINES];
        channel.copy_from_slice(&dct[ch * MP3_GRANULE_LINES..(ch + 1) * MP3_GRANULE_LINES]);
        dct_ii(&mut channel, nbands);
        dct[ch * MP3_GRANULE_LINES..(ch + 1) * MP3_GRANULE_LINES].copy_from_slice(&channel);
    }
    let mut lins = [0.0f32; SYNTH_SCRATCH_LEN];
    lins[..MP3_QMF_STATE_LEN].copy_from_slice(qmf_state);
    for slot in (0..nbands).step_by(2) {
        let lins_base = slot * 64;
        let zlin_base = lins_base + 15 * 64;
        let gr_base = slot;
        let xr_base = if nch == 1 {
            gr_base
        } else {
            MP3_GRANULE_LINES + gr_base
        };
        // Subband `k` at this slot (`xl`) and at the next slot
        // (`xl1`): the synthesis reads both time slots at once.
        let xl = |k: usize| dct[gr_base + k * SUBBAND_LINES];
        let xr = |k: usize| dct[xr_base + k * SUBBAND_LINES];
        let xl1 = |k: usize| dct[gr_base + 1 + k * SUBBAND_LINES];
        let xr1 = |k: usize| dct[xr_base + 1 + k * SUBBAND_LINES];
        let pcm_base = 32 * nch * slot;
        // The window pointer restarts at the table start for every
        // slot pair (it is a local of each synthesis call).
        let mut win_idx = 0usize;
        // The two centre pairs come first, before the loop below.
        lins[zlin_base + 4 * 15] = xl(16);
        lins[zlin_base + 4 * 15 + 1] = xr(16);
        lins[zlin_base + 4 * 15 + 2] = xl(0);
        lins[zlin_base + 4 * 15 + 3] = xr(0);
        lins[zlin_base + 4 * 31] = xl1(16);
        lins[zlin_base + 4 * 31 + 1] = xr1(16);
        lins[zlin_base + 4 * 31 + 2] = xl1(0);
        lins[zlin_base + 4 * 31 + 3] = xr1(0);
        // The centre pairs are written through `zlin` (lins + 960),
        // but the pair sums read from `lins` itself: the reference
        // calls them with `lins + 4*15`, not `zlin + 4*15`.
        synth_pair(
            pcm,
            pcm_base + (nch - 1),
            nch,
            &lins,
            lins_base + 4 * 15 + 1,
        );
        synth_pair(
            pcm,
            pcm_base + 32 * nch + (nch - 1),
            nch,
            &lins,
            lins_base + 4 * 15 + 64 + 1,
        );
        synth_pair(pcm, pcm_base, nch, &lins, lins_base + 4 * 15);
        synth_pair(
            pcm,
            pcm_base + 32 * nch,
            nch,
            &lins,
            lins_base + 4 * 15 + 64,
        );
        for i in (0..=14).rev() {
            // `zlin` offsets below are relative to `zlin_base`;
            // the last two land before it (negative offsets), so
            // the index is formed in `i32` and is still inside the
            // scratch (the base is at least 960).
            let zoff = |off: i32| -> usize { (zlin_base as i32 + off) as usize };
            lins[zoff(4 * i as i32)] = xl(31 - i);
            lins[zoff(4 * i as i32 + 1)] = xr(31 - i);
            lins[zoff(4 * i as i32 + 2)] = xl1(31 - i);
            lins[zoff(4 * i as i32 + 3)] = xr1(31 - i);
            lins[zoff(4 * (i as i32 + 16))] = xl1(1 + i);
            lins[zoff(4 * (i as i32 + 16) + 1)] = xr1(1 + i);
            lins[zoff(4 * (i as i32 - 16) + 2)] = xl(1 + i);
            lins[zoff(4 * (i as i32 - 16) + 3)] = xr(1 + i);
            // Eight window stages: the first sets `diffs`/`sums`,
            // the rest accumulate; stages 1, 3, 5, 7 subtract where
            // the others add (the polyphase sign pattern).
            let mut diffs = [0.0f32; 4];
            let mut sums = [0.0f32; 4];
            for (stage, k) in [0usize, 1, 2, 3, 4, 5, 6, 7].iter().enumerate() {
                let w0 = MP3_POLYPHASE_WIN[win_idx];
                let w1 = MP3_POLYPHASE_WIN[win_idx + 1];
                win_idx += 2;
                let vz_base = zlin_base as i32 + 4 * i as i32 - (*k as i32) * 64;
                let vy_base = zlin_base as i32 + 4 * i as i32 - (15 - *k as i32) * 64;
                for j in 0..4 {
                    let vz = lins[(vz_base + j as i32) as usize];
                    let vy = lins[(vy_base + j as i32) as usize];
                    if stage == 0 {
                        sums[j] = vz * w1 + vy * w0;
                        diffs[j] = vz * w0 - vy * w1;
                    } else if stage % 2 == 1 {
                        // S2 stages (1, 3, 5, 7): `diffs` subtracts.
                        sums[j] += vz * w1 + vy * w0;
                        diffs[j] += vy * w1 - vz * w0;
                    } else {
                        sums[j] += vz * w1 + vy * w0;
                        diffs[j] += vz * w0 - vy * w1;
                    }
                }
            }
            pcm[pcm_base + (nch - 1) + (15 - i) * nch] = scale_pcm(diffs[1]);
            pcm[pcm_base + (nch - 1) + (17 + i) * nch] = scale_pcm(sums[1]);
            pcm[pcm_base + (15 - i) * nch] = scale_pcm(diffs[0]);
            pcm[pcm_base + (17 + i) * nch] = scale_pcm(sums[0]);
            pcm[pcm_base + (nch - 1) + (47 - i) * nch] = scale_pcm(diffs[3]);
            pcm[pcm_base + (nch - 1) + (49 + i) * nch] = scale_pcm(sums[3]);
            pcm[pcm_base + (47 - i) * nch] = scale_pcm(diffs[2]);
            pcm[pcm_base + (49 + i) * nch] = scale_pcm(sums[2]);
        }
    }
    // The filterbank history for the next granule is the tail of
    // the scratch, past this granule's slots. (The reference keeps
    // only the even entries for mono, relying on the right-channel
    // outputs being overwritten; copying the whole state is
    // equivalent and keeps the history exact for both channels.)
    qmf_state.copy_from_slice(&lins[nbands * 64..nbands * 64 + MP3_QMF_STATE_LEN]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_window_is_a_sine_arch() {
        let window = long_window();
        assert!((window[0] - 0.0436).abs() < 1e-3);
        assert!((window[17] - 1.0).abs() < 1e-3);
        assert!((window[35] - 0.0436).abs() < 1e-3);
    }

    #[test]
    fn start_and_stop_windows_meet_in_the_middle() {
        let start = start_window();
        let stop = stop_window();
        assert_eq!(start[30], 0.0);
        assert_eq!(stop[0], 0.0);
        assert_eq!(start[20], 1.0);
        assert_eq!(stop[14], 1.0);
        assert!((start[24] - 0.9914).abs() < 1e-3);
        assert!((stop[11] - 0.9914).abs() < 1e-3);
    }

    #[test]
    fn antialias_with_zero_bands_is_a_no_op() {
        let mut grbuf = [1.0f32; MP3_GRANULE_LINES];
        antialias(&mut grbuf, 0);
        assert!(grbuf.iter().all(|&v| (v - 1.0).abs() < f32::EPSILON));
    }

    #[test]
    fn change_sign_negates_odd_lines_of_odd_subbands() {
        let mut grbuf = [1.0f32; MP3_GRANULE_LINES];
        change_sign(&mut grbuf);
        assert_eq!(grbuf[0], 1.0);
        assert_eq!(grbuf[18 + 1], -1.0);
        assert_eq!(grbuf[18], 1.0);
        assert_eq!(grbuf[36 + 1], 1.0);
    }
}
