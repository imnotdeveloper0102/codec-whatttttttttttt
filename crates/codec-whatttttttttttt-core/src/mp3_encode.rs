//! MP3 Layer III encoder, basic CBR (ISO/IEC 11172-3).
//!
//! This is a deliberately basic encoder, not a LAME-class one:
//! long blocks only (no window switching, so transients smear),
//! plain stereo (no mid/side or intensity stereo), a simple
//! per-band quantizer (no psychoacoustic masking model beyond
//! putting the quantization step under each scalefactor band's
//! own peak), and no bit reservoir (`main_data_begin` is always
//! 0, so every frame is independently decodable). Quality is
//! modest; the doc claim is only that the stream is valid Layer
//! III, decodable by this library's `Mp3Decoder` and by ffmpeg,
//! at the requested CBR.
//!
//! # How the spectrum is obtained
//!
//! A textbook encoder runs a 32-band PQMF analysis filterbank
//! followed by a windowed MDCT. That needs the 512-tap analysis
//! prototype window of Annex D, which this crate does not carry
//! (its synthesis window is stored in a factored form for the
//! decoder, and no coefficient may be invented here). Instead
//! this encoder inverts the decoder's own pipeline. One granule
//! of decoding is a fixed linear map from the 576 dequantized
//! spectrum lines to 576 PCM samples, plus an additive term from
//! the carried state (IMDCT overlap and polyphase history):
//! `pcm = A * spectrum + b(state)`. But a spectrum also drives
//! the next two granules through the overlap and the polyphase
//! filterbank memory, and that cross-granule effect is the
//! dominant one (measured Frobenius norms: ||A|| = 304,
//! ||B|| = 731, ||C|| = 192, where B maps a spectrum to the
//! next granule's PCM and C to the one after), so solving one
//! granule at a time ignores the largest terms and its spectrum
//! wrecks the following granules. The encoder therefore solves
//! a window of four granules jointly, with all three maps, by
//! conjugate gradient on the window normal equations
//! `(F^T F + lambda I) x = F^T (target - baseline)`, and
//! commits the window's first two granules (one frame for
//! MPEG-1, two for MPEG-2/2.5); the rest stay buffered as
//! lookahead. The three maps are built once by probing the
//! decoder's own antialias/IMDCT/change-sign/synthesis stages
//! (a unit spectrum at 1e-3, so the synthesis clamp never
//! engages, divided back out), so no coefficient is invented.
//! `A` is rank 574: PCM rows 0 and 16 of a granule come from
//! the filterbank history alone, so no spectrum moves them;
//! the ridge term only lifts those two exactly singular
//! directions. The decoder state is advanced with the spectrum
//! that was actually written (after quantization), so the
//! encoder's simulation stays exactly in step with a real
//! decoder. The pre-quantization error is the ridge bias
//! (measured: the interior granules of a sine reproduce at
//! 0.0000 RMS); all further loss comes from quantization, as
//! in any encoder. The lookahead costs a delay: the first
//! call(s) return silent priming frames (one frame for
//! MPEG-1, three for MPEG-2/2.5) and the last frames are only
//! returned by `flush`; the tail window is padded by repeating
//! the last granule (assume the signal continues), which keeps
//! the last frame from quantizing badly. The spectrum of a
//! granule is whatever the decoder needs to reproduce the PCM,
//! which for ordinary signals is close to the true MDCT
//! spectrum because the filterbank is near-perfect-reconstruction.
//!
//! # Huffman encode tables
//!
//! The encode codewords are not typed in by hand: they are
//! derived at runtime from the decode tables in
//! `mp3_tables.rs` (ISO/IEC 11172-3 Annex B, Table B.7) by
//! walking the same flattened trees the decoder walks. A
//! leaf's path bits are its codeword; a leaf reached after
//! `consumed` flushed bits with remaining length `rem` has
//! total length `consumed + rem`. The count-1 tables are
//! derived the same way from their two-level lookup. If the
//! derivation ever disagrees with the decoder (a missing or
//! conflicting codeword), encoding fails with a typed error
//! instead of emitting a bad stream.
//!
//! # Rate control
//!
//! Each granule channel gets an equal share of the frame's main
//! data bits. Quantization tries a ladder of (target peak
//! magnitude, band cutoff) pairs, coarsest last, and takes the
//! first whose `part_23_length` fits its share; the final
//! fallback is an all-zero spectrum, which always fits. The
//! frame is padded with zero ancillary bits to its exact CBR
//! length; the padding bit is never set, so the true average
//! bitrate is the floor of the nominal one (for example 417
//! bytes/frame at 128 kbps, 44.1 kHz, i.e. 127.96 kbps).
//!
//! # Scope
//!
//! CBR only: `BitrateMode::Vbr` and `BitrateMode::Abr` return
//! `Unsupported` from `Mp3Encoder::new`. MPEG-1 rates
//! (32000/44100/48000 Hz) take 64/96/128/320 kbps; MPEG-2/2.5
//! rates take 64/96/128 kbps only, because ISO/IEC 11172-3
//! Table B.1 caps MPEG-2/2.5 Layer III at 160 kbps, so 320
//! kbps there is rejected as `InvalidArgument`. One `encode`
//! call consumes exactly one frame (1152 samples per channel
//! for MPEG-1, 576 for MPEG-2/2.5) and returns exactly one
//! frame packet, but the packet is for an earlier frame (see
//! the lookahead above): the first call(s) return silent
//! priming frames and the last frames come from `flush`.

use std::collections::{HashMap, VecDeque};
use std::sync::OnceLock;

use crate::bitrate::{BitrateCodec, BitrateMode};
use crate::bitwriter::BitWriter;
use crate::error::{Error, Result};
use crate::frame::{AudioFrame, Codec, Packet};
use crate::mp3_frame::MpegVersion;
use crate::mp3_synth::{
    antialias, change_sign, imdct_granule, synth_granule, MP3_GRANULE_LINES, MP3_QMF_STATE_LEN,
};
use crate::mp3_tables::{
    MP3_COUNT1_TAB32, MP3_COUNT1_TAB33, MP3_HUFFMAN_TABINDEX, MP3_HUFFMAN_TABS, MP3_LINBITS,
    MP3_SCFC_DECODE, MP3_SCF_LONG,
};

/// Frequency lines per granule and channel (ISO/IEC 11172-3
/// section 2.4.3.4).
const LINES: usize = MP3_GRANULE_LINES;
/// Long-block scalefactor bands (ISO/IEC 11172-3 Table B.8:
/// 21 non-empty bands; the 22nd table entry is 0).
/// Long-block scalefactor bands (ISO/IEC 11172-3 Table B.8
/// has 22 widths per row). Only 21 scalefactors are
/// transmitted (partitions 6+5+5+5); band 21's index is
/// implicitly 0, so its step is fixed by the global gain.
const BANDS: usize = 22;
/// Scalefactors actually written to the bitstream (bands
/// 0..20); band 21 is never transmitted.
const TRANSMITTED_SCF: usize = 21;
/// Largest quantized magnitude any Huffman table can carry:
/// table 23/31 hold 15 plus 13 linbits (ISO/IEC 11172-3
/// Annex B, Table B.7).
const MAX_QUANTIZED: i32 = 15 + ((1 << 13) - 1);
/// Ancillary bits reserved at the end of a frame's main data:
/// the decoder's count-1 loop peeks a few bits past a granule
/// channel's `part_23_length`, so the last channel must not end
/// exactly at the end of the frame.
const ANCILLARY_RESERVE_BITS: usize = 16;

/// One Huffman codeword: the code bits (right-aligned) and
/// their count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HuffmanCode {
    code: u32,
    len: u32,
}

/// A big-values Huffman table (1..=31, except the unused 0, 4,
/// 14), derived from the decode tree: symbol `(x, y)` (both
/// 0..=15) maps to its codeword, `max_mag` is the largest
/// magnitude the table's symbols reach, and `linbits` is the
/// extra-bit count for magnitude 15 (ISO/IEC 11172-3 Annex B,
/// Table B.7).
#[derive(Debug)]
struct BigTable {
    codes: HashMap<(u8, u8), HuffmanCode>,
    max_mag: u8,
    linbits: u8,
}

/// A count-1 table (`tab32`/`tab33`, ISO/IEC 11172-3 Annex B,
/// Table B.7): quad pattern (four nonzero flags, `v` first)
/// maps to its codeword, or `None` when the pattern has no
/// code in this table.
#[derive(Debug)]
struct Count1Table {
    codes: [Option<HuffmanCode>; 16],
}

/// Walks one flattened Huffman decode tree and records every
/// leaf's codeword. `entry` is the tree node reached so far,
/// `prefix` holds the examined bits, `examined_len` is their
/// count, `consumed` is the bits already flushed before this
/// node's peek, and `peek_width` is the width of that peek.
/// A leaf's remaining length is its high byte, so its total
/// code length is `consumed + remaining`; the code itself is
/// the top that many bits of the examined prefix. An internal
/// node (negative entry) peeks `entry & 7` more bits and
/// continues at `peeked - (entry >> 3)` (see `mp3_huffman.rs`
/// for the tree encoding).
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
fn collect_big_codes(
    entry: i16,
    prefix: u32,
    examined_len: u32,
    consumed: u32,
    peek_width: u32,
    base: usize,
    out: &mut HashMap<(u8, u8), HuffmanCode>,
) {
    if entry >= 0 {
        let remaining = (entry >> 8) as u32;
        let total_len = consumed + remaining;
        if total_len > examined_len || total_len > 32 {
            return;
        }
        let code = if total_len == 0 {
            0
        } else {
            prefix >> (examined_len - total_len)
        };
        let symbol = ((entry & 0xF) as u8, ((entry >> 4) & 0xF) as u8);
        match out.get(&symbol) {
            Some(existing)
                if *existing
                    != (HuffmanCode {
                        code,
                        len: total_len,
                    }) =>
            {
                // Conflicting codewords for one symbol mean the
                // tree is not a prefix code; drop the symbol so
                // encoding fails loudly instead of emitting a
                // stream the decoder reads differently.
                out.remove(&symbol);
            }
            Some(_) => {}
            None => {
                out.insert(
                    symbol,
                    HuffmanCode {
                        code,
                        len: total_len,
                    },
                );
            }
        }
        return;
    }
    let width = (entry & 7) as u32;
    let offset = (-(entry >> 3)) as usize;
    let child_consumed = consumed + peek_width;
    let count = 1u32 << width;
    for next in 0..count {
        let Some(&child) = MP3_HUFFMAN_TABS.get(base + next as usize + offset) else {
            continue;
        };
        collect_big_codes(
            child,
            (prefix << width) | next,
            examined_len + width,
            child_consumed,
            width,
            base,
            out,
        );
    }
}

/// Derives all big-values encode tables from the decode trees.
/// Tables 0, 4 and 14 are unused (their index points at the
/// all-zero tree) and stay `None`.
fn build_big_tables() -> Vec<Option<BigTable>> {
    let mut tables: Vec<Option<BigTable>> = Vec::with_capacity(32);
    for table_num in 0..32usize {
        if table_num == 0 || table_num == 4 || table_num == 14 {
            tables.push(None);
            continue;
        }
        let base = match MP3_HUFFMAN_TABINDEX.get(table_num) {
            Some(&index) if index >= 0 => index.unsigned_abs() as usize,
            _ => {
                tables.push(None);
                continue;
            }
        };
        let mut codes: HashMap<(u8, u8), HuffmanCode> = HashMap::new();
        for peeked in 0..32u32 {
            let Some(&entry) = MP3_HUFFMAN_TABS.get(base + peeked as usize) else {
                continue;
            };
            collect_big_codes(entry, peeked, 5, 0, 5, base, &mut codes);
        }
        let max_mag = codes.keys().flat_map(|&(x, y)| [x, y]).max().unwrap_or(0);
        tables.push(Some(BigTable {
            codes,
            max_mag,
            linbits: MP3_LINBITS.get(table_num).copied().unwrap_or(0),
        }));
    }
    tables
}

/// Derives one count-1 encode table from its lookup table. The
/// first 4 bits index the table; an entry with bit 3 set is a
/// leaf (low 3 bits are the code length, high nibble the quad
/// pattern), otherwise the low 2 bits say how many following
/// bits extend the index into the second level (see
/// `mp3_huffman.rs`).
fn build_count1_table(tab: &[u8]) -> Count1Table {
    let mut codes: [Option<HuffmanCode>; 16] = [None; 16];
    for index in 0..16u32 {
        let Some(&entry) = tab.get(index as usize) else {
            continue;
        };
        if entry & 8 != 0 {
            let len = u32::from(entry & 7);
            if len == 0 || len > 4 {
                continue;
            }
            let pattern = (entry >> 4) as usize;
            codes[pattern] = Some(HuffmanCode {
                code: index >> (4 - len),
                len,
            });
        } else {
            let extra = u32::from(entry & 3);
            let base = (entry >> 3) as usize;
            for next in 0..(1u32 << extra) {
                let Some(&leaf) = tab.get(base + next as usize) else {
                    continue;
                };
                if leaf & 8 == 0 {
                    continue;
                }
                let len = u32::from(leaf & 7);
                let examined_len = 4 + extra;
                if len == 0 || len > examined_len {
                    continue;
                }
                let pattern = (leaf >> 4) as usize;
                let examined = (index << extra) | next;
                codes[pattern] = Some(HuffmanCode {
                    code: examined >> (examined_len - len),
                    len,
                });
            }
        }
    }
    Count1Table { codes }
}

/// All Huffman encode tables, derived once from the decode
/// tables.
struct HuffmanTables {
    big: Vec<Option<BigTable>>,
    count1: [Count1Table; 2],
}

fn huffman_tables() -> &'static HuffmanTables {
    static TABLES: OnceLock<HuffmanTables> = OnceLock::new();
    TABLES.get_or_init(|| HuffmanTables {
        big: build_big_tables(),
        count1: [
            build_count1_table(MP3_COUNT1_TAB32),
            build_count1_table(MP3_COUNT1_TAB33),
        ],
    })
}

/// Matrix-vector product for a row-major `n * n` matrix.
fn matvec(a: &[f64], x: &[f64], n: usize) -> Vec<f64> {
    let mut y = vec![0.0f64; n];
    for row in 0..n {
        let mut sum = 0.0f64;
        for col in 0..n {
            sum += a[row * n + col] * x[col];
        }
        y[row] = sum;
    }
    y
}

/// Transpose of a row-major `n * n` matrix.
fn transpose(a: &[f64], n: usize) -> Vec<f64> {
    let mut t = vec![0.0f64; n * n];
    for i in 0..n {
        for j in 0..n {
            t[j * n + i] = a[i * n + j];
        }
    }
    t
}

/// The decoder pipeline for one granule, as the decoder runs
/// it (antialias, IMDCT with overlap-add, change of sign,
/// polyphase synthesis), holding the carried state. Running it
/// with a zero spectrum yields the state's additive
/// contribution to the PCM; running it with the written
/// spectrum advances the state exactly as a decoder's would.
#[derive(Debug, Clone)]
struct DecoderSim {
    overlap: [[f32; LINES]; 2],
    qmf_state: [f32; MP3_QMF_STATE_LEN],
}

impl DecoderSim {
    fn new() -> Self {
        Self {
            overlap: [[0.0; LINES]; 2],
            qmf_state: [0.0; MP3_QMF_STATE_LEN],
        }
    }

    /// Runs one granule: `spectra[ch]` is that channel's 576
    /// dequantized lines. Returns the interleaved PCM
    /// (`576 * channels` samples).
    ///
    /// # Errors
    ///
    /// Returns the IMDCT errors; the lengths here are fixed by
    /// construction, so this only fires on internal misuse.
    fn run_granule(&mut self, spectra: &[[f32; LINES]; 2], channels: usize) -> Result<Vec<f32>> {
        let mut channel_bufs = [[0.0f32; LINES]; 2];
        for ch in 0..channels {
            channel_bufs[ch].copy_from_slice(&spectra[ch]);
            antialias(&mut channel_bufs[ch], 31);
            imdct_granule(&mut channel_bufs[ch], &mut self.overlap[ch], 0, 0)?;
            change_sign(&mut channel_bufs[ch]);
        }
        let mut grbuf = [0.0f32; LINES * 2];
        grbuf[..LINES].copy_from_slice(&channel_bufs[0]);
        if channels == 2 {
            grbuf[LINES..].copy_from_slice(&channel_bufs[1]);
        }
        let mut pcm = vec![0.0f32; LINES * channels];
        synth_granule(&mut self.qmf_state, &grbuf, 18, channels, &mut pcm);
        Ok(pcm)
    }
}

/// Solver for a window of granules, jointly. One granule
/// of decoding is a fixed linear map `pcm = A * spectrum +
/// b(state)`, but a spectrum also drives the next two
/// granules through the IMDCT overlap and the polyphase
/// filterbank memory, and that cross-granule effect is the
/// dominant one (measured Frobenius norms: ||A|| = 304,
/// ||B|| = 731, ||C|| = 192, where B maps a spectrum to the
/// next granule's PCM and C to the one after). Solving one
/// granule at a time ignores B and C, so its spectrum wrecks
/// the following granules; solving a window of four granules
/// jointly with all three maps reproduces the interior
/// granules exactly (measured linear RMS 0.0000). The maps
/// are built once by probing the decoder's own pipeline (a
/// unit spectrum at 1e-3, so the synthesis clamp never
/// engages, divided back out), which is why no coefficient
/// is invented here.
struct JointSolver {
    a: Vec<f64>,
    b: Vec<f64>,
    c: Vec<f64>,
    at: Vec<f64>,
    bt: Vec<f64>,
    ct: Vec<f64>,
}

impl JointSolver {
    /// Conjugate gradient on the window normal equations
    /// `(F^T F + lambda I) x = F^T r`, where `F` is the
    /// block-lower-triangular window operator `y_k = A x_k +
    /// B x_{k-1} + C x_{k-2}` and `r` is the residual
    /// `target - baseline` per granule. `targets` and
    /// `baselines` are one channel's per-granule spectra/PCM
    /// (both length `W`, 1..=4). CG from zero finds the
    /// minimum-norm solution of the well-conditioned part and
    /// converges in a handful of iterations (measured: 2).
    fn solve_channel(
        &self,
        targets: &[[f64; LINES]],
        baselines: &[[f64; LINES]],
    ) -> Vec<[f64; LINES]> {
        let w = targets.len();
        let n = LINES;
        let mut r = vec![0.0f64; w * n];
        for k in 0..w {
            for i in 0..n {
                r[k * n + i] = targets[k][i] - baselines[k][i];
            }
        }
        let forward = |x: &[f64]| -> Vec<f64> {
            let mut y = vec![0.0f64; w * n];
            for k in 0..w {
                let ax = matvec(&self.a, &x[k * n..(k + 1) * n], n);
                for i in 0..n {
                    y[k * n + i] += ax[i];
                }
                if k > 0 {
                    let bx = matvec(&self.b, &x[(k - 1) * n..k * n], n);
                    for i in 0..n {
                        y[k * n + i] += bx[i];
                    }
                }
                if k > 1 {
                    let cx = matvec(&self.c, &x[(k - 2) * n..(k - 1) * n], n);
                    for i in 0..n {
                        y[k * n + i] += cx[i];
                    }
                }
            }
            y
        };
        let adjoint = |y: &[f64]| -> Vec<f64> {
            let mut x = vec![0.0f64; w * n];
            for k in 0..w {
                let aty = matvec(&self.at, &y[k * n..(k + 1) * n], n);
                for i in 0..n {
                    x[k * n + i] += aty[i];
                }
                if k + 1 < w {
                    let bty = matvec(&self.bt, &y[(k + 1) * n..(k + 2) * n], n);
                    for i in 0..n {
                        x[k * n + i] += bty[i];
                    }
                }
                if k + 2 < w {
                    let cty = matvec(&self.ct, &y[(k + 2) * n..(k + 3) * n], n);
                    for i in 0..n {
                        x[k * n + i] += cty[i];
                    }
                }
            }
            x
        };
        // Absolute ridge: far above f64 round-off, far below
        // the signal; it only lifts the two exactly singular
        // directions (the uncontrollable PCM rows 0 and 16).
        let lambda = 1e-6f64;
        let apply_m = |p: &[f64]| -> Vec<f64> {
            let fp = forward(p);
            let mut mp = adjoint(&fp);
            for (i, v) in mp.iter_mut().enumerate() {
                *v += lambda * p[i];
            }
            mp
        };
        let bvec = adjoint(&r);
        let b_norm = bvec.iter().map(|v| v * v).sum::<f64>().sqrt();
        let mut x = vec![0.0f64; w * n];
        let mut rvec = bvec.clone();
        let mut pvec = rvec.clone();
        let mut rs: f64 = rvec.iter().map(|v| v * v).sum();
        for _ in 0..100usize {
            let mp = apply_m(&pvec);
            let pmp: f64 = pvec.iter().zip(mp.iter()).map(|(a, b)| a * b).sum();
            if pmp <= 0.0 {
                break;
            }
            let alpha = rs / pmp;
            for i in 0..w * n {
                x[i] += alpha * pvec[i];
                rvec[i] -= alpha * mp[i];
            }
            let rs_new: f64 = rvec.iter().map(|v| v * v).sum();
            if b_norm == 0.0 || rs_new.sqrt() / b_norm < 1e-8 {
                break;
            }
            let beta = rs_new / rs;
            for i in 0..w * n {
                pvec[i] = rvec[i] + beta * pvec[i];
            }
            rs = rs_new;
        }
        let mut out = Vec::with_capacity(w);
        for k in 0..w {
            let mut spec = [0.0f64; LINES];
            spec.copy_from_slice(&x[k * n..(k + 1) * n]);
            out.push(spec);
        }
        out
    }
}

/// Builds the joint window solver once per process by
/// probing the decoder's own pipeline: for each spectrum
/// line, one granule with a unit spectrum gives its column
/// of A, then two zero granules on the same simulation give
/// its columns of B and C. `None` when a probe fails.
fn joint_solver() -> Option<&'static JointSolver> {
    static JOINT: OnceLock<Option<JointSolver>> = OnceLock::new();
    JOINT
        .get_or_init(|| {
            // Probe amplitude 1e-3: the synthesis clamps its PCM
            // output to [-1, 1] (`scale_pcm`), so a unit probe
            // would measure the clamped map, not the linear one.
            // At 1e-3 the pipeline stays in its linear regime and
            // the true map is the measured one divided by 1e-3.
            const PROBE: f32 = 1e-3;
            let n = LINES;
            let mut a = vec![0.0f64; n * n];
            let mut b = vec![0.0f64; n * n];
            let mut c = vec![0.0f64; n * n];
            for col in 0..n {
                let mut spectrum = [0.0f32; LINES];
                spectrum[col] = PROBE;
                let mut sim = DecoderSim::new();
                let pcm = sim.run_granule(&[spectrum, [0.0f32; LINES]], 1).ok()?;
                for (row, &value) in pcm.iter().enumerate() {
                    a[row * n + col] = f64::from(value) / f64::from(PROBE);
                }
                let pcm2 = sim.run_granule(&[[0.0f32; LINES]; 2], 1).ok()?;
                for (row, &value) in pcm2.iter().enumerate() {
                    b[row * n + col] = f64::from(value) / f64::from(PROBE);
                }
                let pcm3 = sim.run_granule(&[[0.0f32; LINES]; 2], 1).ok()?;
                for (row, &value) in pcm3.iter().enumerate() {
                    c[row * n + col] = f64::from(value) / f64::from(PROBE);
                }
            }
            let at = transpose(&a, n);
            let bt = transpose(&b, n);
            let ct = transpose(&c, n);
            Some(JointSolver {
                a,
                b,
                c,
                at,
                bt,
                ct,
            })
        })
        .as_ref()
}

/// MPEG version for a sample rate (ISO/IEC 11172-3 Table B.2
/// and ISO/IEC 13818-3 Table B.2), or `None` when the rate is
/// not an MPEG Layer III rate.
fn version_for_rate(sample_rate: u32) -> Option<MpegVersion> {
    match sample_rate {
        32000 | 44100 | 48000 => Some(MpegVersion::Mpeg1),
        16000 | 22050 | 24000 => Some(MpegVersion::Mpeg2),
        8000 | 11025 | 12000 => Some(MpegVersion::Mpeg25),
        _ => None,
    }
}

/// Sample-rate index within its version (ISO/IEC 11172-3
/// section 2.4.1.3): 0 is 44100/22050/11025 Hz, 1 is
/// 48000/24000/12000 Hz, 2 is 32000/16000/8000 Hz.
fn rate_index(sample_rate: u32) -> u8 {
    match sample_rate {
        44100 | 22050 | 11025 => 0,
        48000 | 24000 | 12000 => 1,
        _ => 2,
    }
}

/// Scalefactor-band table row (ISO/IEC 11172-3 Table B.8):
/// rows 0-1 are MPEG-2.5, 2-4 MPEG-2, 5-7 MPEG-1.
fn sfb_table_row(version: MpegVersion, rate: u8) -> usize {
    match (version, rate) {
        (MpegVersion::Mpeg25, 0 | 1) => 0,
        (MpegVersion::Mpeg25, _) => 1,
        (MpegVersion::Mpeg2, 0) => 2,
        (MpegVersion::Mpeg2, 1) => 3,
        (MpegVersion::Mpeg2, _) => 4,
        (MpegVersion::Mpeg1, 0) => 5,
        (MpegVersion::Mpeg1, 1) => 6,
        (MpegVersion::Mpeg1, _) => 7,
    }
}

/// Bitrate index (ISO/IEC 11172-3 Table B.1): MPEG-1 Layer III
/// carries 64/96/128/320 kbps at indexes 5/7/9/14, MPEG-2/2.5
/// Layer III carries 64/96/128 kbps at 8/10/12 and no 320
/// kbps (its table tops out at 160 kbps).
fn bitrate_index(version: MpegVersion, kbps: u32) -> Option<u8> {
    match version {
        MpegVersion::Mpeg1 => match kbps {
            64 => Some(5),
            96 => Some(7),
            128 => Some(9),
            320 => Some(14),
            _ => None,
        },
        _ => match kbps {
            64 => Some(8),
            96 => Some(10),
            128 => Some(12),
            _ => None,
        },
    }
}

/// Scalefactor-band layout for one (version, rate) pair: the
/// width of each of the 22 long bands, each band's first line,
/// and each line's band.
#[derive(Debug, Clone, Copy)]
struct BandLayout {
    widths: [u8; BANDS],
    starts: [usize; BANDS],
    band_of_line: [usize; LINES],
}

impl BandLayout {
    fn new(version: MpegVersion, sample_rate: u32) -> Self {
        let row = sfb_table_row(version, rate_index(sample_rate));
        let widths_full = MP3_SCF_LONG[row];
        let mut widths = [0u8; BANDS];
        widths.copy_from_slice(&widths_full[..BANDS]);
        let mut starts = [0usize; BANDS];
        let mut band_of_line = [0usize; LINES];
        let mut line = 0usize;
        for (band, &width) in widths.iter().enumerate() {
            starts[band] = line;
            for _ in 0..width {
                if line < LINES {
                    band_of_line[line] = band;
                }
                line += 1;
            }
        }
        Self {
            widths,
            starts,
            band_of_line,
        }
    }
}

/// One granule channel, quantized: the integer spectrum, the
/// scalefactors the decoder will reconstruct from the side
/// info, and the dequantized spectrum (for advancing the
/// decoder simulation with what was actually written).
struct QuantizedChannel {
    q: [i32; LINES],
    /// Stored scalefactor index per band.
    iscf: [u8; BANDS],
    global_gain: u8,
    /// MPEG-1 `scalefac_compress` index, or the MPEG-2/2.5
    /// mixed-radix `scalefac_compress` value.
    scalefac_compress: u16,
    /// Dequantized spectrum (`|q|^(4/3) * scf`, with sign).
    dequantized: [f32; LINES],
}

/// Bits needed to store `value` (0 when the value is 0).
// `leading_zeros` of a u8 is 0..8 by construction, so the
// narrowing cast is exact.
#[allow(clippy::cast_possible_truncation)]
fn bits_needed(value: u8) -> u8 {
    if value == 0 {
        0
    } else {
        8 - value.leading_zeros() as u8
    }
}

/// Inverse of `MP3_SCFC_DECODE` (index by packed part,
/// 0..=19): parts 4, 8, 16, 17 have no entry and are never
/// reached, since the widening above lands on a valid pair.
const PART_TO_COMPRESS: [u8; 20] = [
    0, 1, 2, 3, 255, 5, 6, 7, 255, 8, 9, 10, 4, 11, 12, 13, 255, 255, 14, 15,
];

/// Quantizes one granule channel's spectrum. `qtarget` is the
/// target peak quantized magnitude per band and `cutoff_bands`
/// is the number of bands kept (higher bands are zeroed).
/// The global gain is set from the loudest band so no band
/// overflows the target: every other band's step is then at or
/// above its ideal, so its peak is at or below the target.
// One cohesive quantization pass (band steps, global gain,
// scalefactor compression, then the per-line quantizer);
// splitting it would obscure the band-step/global-gain flow.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]
fn quantize_channel(
    spectrum: &[f32; LINES],
    layout: &BandLayout,
    is_mpeg1: bool,
    qtarget: f64,
    cutoff_bands: usize,
) -> QuantizedChannel {
    let mut band_max = [0.0f64; BANDS];
    for (line, &value) in spectrum.iter().enumerate() {
        let band = layout.band_of_line[line];
        if band < cutoff_bands {
            band_max[band] = band_max[band].max(f64::from(value).abs());
        }
    }
    // The ideal step for a band puts its peak at `qtarget`:
    // `|q| = (|x| / scf)^(3/4)`, so `scf = peak /
    // qtarget^(4/3)`. Silent bands take the loudest band's
    // step (their step is irrelevant, since every `q` is 0).
    let qtarget_pow = qtarget.powf(4.0 / 3.0);
    let mut desired_scf = [0.0f64; BANDS];
    let mut scf_max = 0.0f64;
    for band in 0..BANDS {
        if band_max[band] > 0.0 {
            desired_scf[band] = band_max[band] / qtarget_pow;
            scf_max = scf_max.max(desired_scf[band]);
        }
    }
    let (global_gain, iscf, scf) = if scf_max <= 0.0 {
        (128u8, [0u8; BANDS], [1.0f64; BANDS])
    } else {
        // Decoder scalefactor (ISO/IEC 11172-3 section
        // 2.4.2.7, `scalefac_scale` 0): `scf =
        // 2^(global_gain/4 - 53.5 - iscf/2)`. The loudest band
        // gets `iscf` 0; the rest follow from their ideal step,
        // clamped to the stored range (0..=15 for bands 0..10,
        // 0..=7 for bands 11..20, the MPEG-1 width limits).
        let gain = (214.0 + 4.0 * scf_max.log2()).round() as i32;
        let gain = gain.clamp(0, 255) as u8;
        let mut iscf = [0u8; BANDS];
        let mut scf = [0.0f64; BANDS];
        for band in 0..BANDS {
            let ideal = if desired_scf[band] > 0.0 {
                desired_scf[band]
            } else {
                scf_max
            };
            let raw = f64::from(gain) / 2.0 - 107.0 - 2.0 * ideal.log2();
            let limit = if band < 11 { 15u8 } else { 7u8 };
            iscf[band] = (raw.round() as i32).clamp(0, i32::from(limit)) as u8;
            scf[band] = 2f64.powf(f64::from(gain) / 4.0 - 53.5 - f64::from(iscf[band]) / 2.0);
        }
        // Band 21 is never transmitted: its index is
        // implicitly 0, so its step is the global gain's step.
        iscf[BANDS - 1] = 0;
        scf[BANDS - 1] = 2f64.powf(f64::from(gain) / 4.0 - 53.5);
        (gain, iscf, scf)
    };
    // Scalefactor compression (ISO/IEC 11172-3 section
    // 2.4.2.7): MPEG-1 stores one width for bands 0..10 and
    // one for bands 11..20, packed as `(a << 2) | b` through
    // `MP3_SCFC_DECODE`; MPEG-2/2.5 stores four per-partition
    // widths as a mixed radix (`80*w0 + 16*w1 + 4*w2 + w3`,
    // ISO/IEC 13818-3 section 2.4.2.7).
    let scalefac_compress = if is_mpeg1 {
        let mut a = 0u8;
        let mut b = 0u8;
        for &step in &iscf[..11] {
            a = a.max(bits_needed(step));
        }
        for &step in &iscf[11..TRANSMITTED_SCF] {
            b = b.max(bits_needed(step));
        }
        // Not every `(a, b)` pair is in the table
        // (`(1, 0)`, `(2, 0)`, `(4, 0)`, `(4, 1)` are
        // missing); widening `b` by one reaches a valid pair
        // and only ever relaxes the fit.
        let part = (a << 2) | b;
        let mut part = part;
        if !MP3_SCFC_DECODE.contains(&part) {
            b = if a == 4 { 2 } else { 1 };
            part = (a << 2) | b;
        }
        u16::from(
            PART_TO_COMPRESS
                .get(part as usize)
                .copied()
                .unwrap_or(0)
                .min(15),
        )
    } else {
        let partitions = [(0usize, 6usize), (6, 11), (11, 16), (16, 21)];
        let limits = [4u8, 4, 3, 3];
        let mut widths = [0u8; 4];
        for (i, &(start, end)) in partitions.iter().enumerate() {
            for &step in &iscf[start..end] {
                widths[i] = widths[i].max(bits_needed(step));
            }
            widths[i] = widths[i].min(limits[i]);
        }
        u16::from(widths[0]) * 80
            + u16::from(widths[1]) * 16
            + u16::from(widths[2]) * 4
            + u16::from(widths[3])
    };
    let mut q = [0i32; LINES];
    let mut dequantized = [0.0f32; LINES];
    for (line, &value) in spectrum.iter().enumerate() {
        let band = layout.band_of_line[line];
        if band >= cutoff_bands || value == 0.0 {
            continue;
        }
        let magnitude = (f64::from(value).abs() / scf[band]).powf(0.75).round() as i32;
        let magnitude = magnitude.clamp(0, MAX_QUANTIZED);
        q[line] = if value < 0.0 { -magnitude } else { magnitude };
        let restored = f64::from(magnitude).powf(4.0 / 3.0) * scf[band];
        dequantized[line] = if value < 0.0 {
            -(restored as f32)
        } else {
            restored as f32
        };
    }
    QuantizedChannel {
        q,
        iscf,
        global_gain,
        scalefac_compress,
        dequantized,
    }
}

/// Side-information fields for one granule channel (ISO/IEC
/// 11172-3 section 2.4.2).
#[derive(Debug, Clone)]
struct ChannelParams {
    part_23_length: u16,
    big_values: u16,
    global_gain: u8,
    scalefac_compress: u16,
    table_select: [u8; 3],
    /// Stored region counts (bands minus one): region 0
    /// covers 14 bands, region 1 covers 7.
    region_count: [u8; 2],
    count1_table: bool,
}

/// Picks the cheapest Huffman table for a region's pairs:
/// the table must represent the largest magnitude present,
/// and the cost is the code length plus a sign bit per nonzero
/// value plus the linbits for magnitudes of exactly 15.
/// Returns table 1 (which only holds 0/1) for an empty region.
// The table number comes from enumerating 32 tables, so it
// fits in u8; the cast below is exact.
#[allow(clippy::cast_possible_truncation)]
fn choose_big_table(pairs: &[(i32, i32)], tables: &HuffmanTables) -> Result<u8> {
    if pairs.is_empty() {
        return Ok(1);
    }
    let max_mag = pairs
        .iter()
        .flat_map(|&(x, y)| [x.abs(), y.abs()])
        .max()
        .unwrap_or(0);
    let mut best: Option<(u32, u8)> = None;
    for (num, table) in tables.big.iter().enumerate().skip(1) {
        let Some(table) = table else { continue };
        let table_max: i32 = if table.linbits > 0 {
            15 + ((1i32 << table.linbits) - 1)
        } else {
            i32::from(table.max_mag)
        };
        if max_mag > table_max {
            continue;
        }
        let mut cost: u32 = 0;
        let mut usable = true;
        for &(x, y) in pairs {
            let Some(code) = table.codes.get(&(
                x.unsigned_abs().min(15) as u8,
                y.unsigned_abs().min(15) as u8,
            )) else {
                usable = false;
                break;
            };
            cost += code.len;
            for value in [x, y] {
                if value != 0 {
                    cost += 1;
                }
                if value.abs() >= 15 && table.linbits > 0 {
                    cost += u32::from(table.linbits);
                }
            }
        }
        if usable && best.map_or(true, |(best_cost, _)| cost < best_cost) {
            best = Some((cost, num as u8));
        }
    }
    best.map(|(_, num)| num).ok_or(Error::InvalidData {
        offset: 0,
        detail: "mp3 encode found no huffman table for the region magnitudes",
    })
}

/// Writes one big-values pair: the pair codeword, then per
/// value the linbits (when the magnitude is 15 in a linbits
/// table) and the sign bit (1 for negative, as the decoder
/// reads it).
fn write_big_pair(writer: &mut BitWriter, table: &BigTable, pair: (i32, i32)) -> Result<()> {
    let Some(code) = table.codes.get(&(
        pair.0.unsigned_abs().min(15) as u8,
        pair.1.unsigned_abs().min(15) as u8,
    )) else {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "mp3 encode pair has no huffman codeword",
        });
    };
    writer.write_bits(code.code, code.len)?;
    for value in [pair.0, pair.1] {
        let magnitude = value.unsigned_abs();
        if magnitude >= 15 && table.linbits > 0 {
            // The codeword covers 15; the excess rides in the
            // linbits, exactly as the decoder reads them.
            let extra = magnitude - 15;
            writer.write_bits(extra, u32::from(table.linbits))?;
        }
        if value != 0 {
            writer.write_bit(value < 0);
        }
    }
    Ok(())
}

/// Encodes one granule channel's scalefactors and Huffman data
/// into `writer` (scalefactors first, then the big-values
/// region, then the count-1 region), mirroring the walk the
/// decoder performs in `decode_huffman` so the two stay in
/// step band by band.
#[allow(clippy::too_many_lines)]
fn encode_channel_bits(
    quantized: &QuantizedChannel,
    layout: &BandLayout,
    is_mpeg1: bool,
    tables: &HuffmanTables,
    writer: &mut BitWriter,
) -> Result<ChannelParams> {
    let start_bits = writer.bit_position();
    // Scalefactors (ISO/IEC 11172-3 section 2.4.2.7), in band
    // order, with the widths the compression code promises.
    if is_mpeg1 {
        let part = MP3_SCFC_DECODE
            .get(quantized.scalefac_compress as usize)
            .copied()
            .unwrap_or(0);
        let width_a = u32::from(part >> 2);
        let width_b = u32::from(part & 3);
        for band in 0..TRANSMITTED_SCF {
            let width = if band < 11 { width_a } else { width_b };
            writer.write_bits(u32::from(quantized.iscf[band]), width)?;
        }
    } else {
        let sfc = i32::from(quantized.scalefac_compress);
        let mut widths = [0u8; 4];
        let moduli = [5i32, 5, 4, 4];
        let mut modprod = 1i32;
        for i in (0..4).rev() {
            // The digit is a mixed-radix digit, 0..modulus
            // (<= 5), so the narrowing cast is exact.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let digit = (sfc / modprod % moduli[i]) as u8;
            widths[i] = digit;
            modprod *= moduli[i];
        }
        let partitions = [(0usize, 6usize), (6, 11), (11, 16), (16, 21)];
        for (i, &(first, end)) in partitions.iter().enumerate() {
            for band in first..end {
                writer.write_bits(u32::from(quantized.iscf[band]), u32::from(widths[i]))?;
            }
        }
    }
    // Big values: pairs up to the last line with |q| > 1,
    // rounded up to an even pair count so the count-1 region
    // starts on a quad boundary (the decoder reads quads of
    // four lines and rejects a write past line 576).
    let mut big_values: usize = 0;
    for (line, &value) in quantized.q.iter().enumerate() {
        if value.abs() > 1 {
            big_values = (line / 2 + 1).max(big_values);
        }
    }
    if big_values > 0 && big_values % 2 != 0 {
        big_values += 1;
    }
    // Regions: 14 bands, then 7, then the rest (none). The
    // stored counts are bands minus one, matching the
    // decoder's `sfb_cnt` walk.
    let region_bands = [14usize, 7, 1];
    let mut remaining = big_values;
    let mut band_idx = 0usize;
    let mut table_select = [1u8; 3];
    // Collect each region's pairs first (table choice needs
    // the whole region), then emit them in band order.
    let mut region_pairs: [Vec<(i32, i32)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for (region, &bands_here) in region_bands.iter().enumerate() {
        for _ in 0..bands_here {
            if remaining == 0 {
                break;
            }
            let width = usize::from(layout.widths[band_idx]);
            let pairs_in_band = width / 2;
            let take = remaining.min(pairs_in_band);
            let first_line = layout.starts[band_idx];
            for p in 0..take {
                let at = first_line + 2 * p;
                region_pairs[region].push((quantized.q[at], quantized.q[at + 1]));
            }
            remaining = remaining.saturating_sub(pairs_in_band);
            band_idx += 1;
        }
        table_select[region] = choose_big_table(&region_pairs[region], tables)?;
    }
    for (region, pairs) in region_pairs.iter().enumerate() {
        let Some(table) = tables.big[table_select[region] as usize].as_ref() else {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "mp3 encode region table is missing",
            });
        };
        for &pair in pairs {
            write_big_pair(writer, table, pair)?;
        }
    }
    // Count-1 region: quads of four lines from the end of the
    // big-values region. Values here are -1, 0 or +1 by
    // construction. The cheaper of the two count-1 tables wins.
    let first_line = 2 * big_values;
    let mut quads: Vec<[i32; 4]> = Vec::new();
    let mut line = first_line;
    while line < LINES {
        quads.push([
            quantized.q[line],
            quantized.q[line + 1],
            quantized.q[line + 2],
            quantized.q[line + 3],
        ]);
        line += 4;
    }
    let mut count1_cost = [0u32; 2];
    let mut count1_usable = [true; 2];
    for (t, table) in tables.count1.iter().enumerate() {
        for quad in &quads {
            let pattern = quad
                .iter()
                .enumerate()
                .fold(0usize, |acc, (i, &v)| acc | if v != 0 { 8 >> i } else { 0 });
            match table.codes[pattern] {
                Some(code) => {
                    count1_cost[t] += code.len + pattern.count_ones();
                }
                None => {
                    count1_usable[t] = false;
                }
            }
        }
    }
    let count1_table = if count1_usable[1] && (!count1_usable[0] || count1_cost[1] < count1_cost[0])
    {
        true
    } else if !count1_usable[0] {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "mp3 encode quad has no count1 codeword",
        });
    } else {
        false
    };
    let table = &tables.count1[usize::from(count1_table)];
    for quad in &quads {
        let pattern = quad
            .iter()
            .enumerate()
            .fold(0usize, |acc, (i, &v)| acc | if v != 0 { 8 >> i } else { 0 });
        let Some(code) = table.codes[pattern] else {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "mp3 encode quad has no count1 codeword",
            });
        };
        writer.write_bits(code.code, code.len)?;
        for &value in quad {
            if value != 0 {
                writer.write_bit(value < 0);
            }
        }
    }
    let part_23_length = writer.bit_position() - start_bits;
    let part_23_length = u16::try_from(part_23_length)
        .map_err(|_| Error::Overflow("mp3 part_23_length exceeds 12 bits"))?;
    Ok(ChannelParams {
        part_23_length,
        big_values: u16::try_from(big_values)
            .map_err(|_| Error::Overflow("mp3 big_values exceeds 9 bits"))?,
        global_gain: quantized.global_gain,
        scalefac_compress: quantized.scalefac_compress,
        table_select,
        region_count: [13, 6],
        count1_table,
    })
}

/// Quantization attempts, coarsest last: (target peak
/// magnitude, bands kept). The final attempt zeroes the whole
/// spectrum, which always fits its bit share.
const QUANT_ATTEMPTS: [(f64, usize); 7] = [
    (12.0, 21),
    (8.0, 21),
    (5.0, 19),
    (3.0, 17),
    (2.0, 14),
    (1.0, 10),
    (0.0, 0),
];

/// Granules in one joint-solve window: the frame being
/// committed plus two granules of lookahead (two frames for
/// MPEG-1, four for MPEG-2/2.5).
const WINDOW_GRANULES: usize = 4;
/// Granules committed per window step (one frame for
/// MPEG-1, two frames for MPEG-2/2.5).
const COMMIT_GRANULES: usize = 2;

/// One buffered input frame: its granule targets, per
/// channel, in granule order.
#[derive(Debug)]
struct BufferedFrame {
    timestamp: u64,
    granules: Vec<[[f32; LINES]; 2]>,
}

/// Basic MP3 Layer III CBR encoder.
///
/// One `encode` call consumes exactly one frame (1152 samples
/// per channel for MPEG-1, 576 for MPEG-2/2.5) and returns
/// exactly one frame packet, but the packet is for an earlier
/// frame: the spectrum of a granule is solved jointly with
/// the two following granules (see `JointSolver`), so the
/// encoder holds one window of lookahead. The first call(s)
/// return silent priming frames instead -- one frame for
/// MPEG-1, three for MPEG-2/2.5 -- and the last frames are
/// only returned by `flush`, so call `flush` after the last
/// `encode`. The encoder is stateful: the decoder simulation
/// (IMDCT overlap and polyphase history) carries across
/// frames, so frames must be fed in order from the start of
/// the stream. It is `Send` but not `Sync`.
#[derive(Debug)]
pub struct Mp3Encoder {
    sample_rate: u32,
    channels: u32,
    kbps: u32,
    version: MpegVersion,
    frame_length: usize,
    side_len: usize,
    granules: usize,
    samples_per_frame: usize,
    layout: BandLayout,
    sim: DecoderSim,
    buffer: VecDeque<BufferedFrame>,
    ready: VecDeque<Packet>,
}

impl Mp3Encoder {
    /// Creates an encoder for CBR Layer III at `sample_rate`
    /// with `channels` (1 or 2).
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a bitrate off the
    /// 64/96/128/320 kbps ladder, a sample rate outside the
    /// MPEG rates, 320 kbps at an MPEG-2/2.5 rate (the spec
    /// caps those at 160 kbps), or a channel count other than
    /// 1 or 2; `Unsupported` for VBR/ABR modes and when the
    /// joint spectrum map fails to build.
    pub fn new(sample_rate: u32, channels: u32, mode: BitrateMode) -> Result<Self> {
        mode.validate_for(BitrateCodec::Mp3, sample_rate)?;
        let kbps = match mode {
            BitrateMode::Cbr(kbps) => kbps,
            BitrateMode::Vbr(_) => {
                return Err(Error::Unsupported(
                    "mp3 vbr encode is not implemented (cbr only)",
                ));
            }
            BitrateMode::Abr(_) => {
                return Err(Error::Unsupported(
                    "mp3 abr encode is not implemented (cbr only)",
                ));
            }
        };
        if channels != 1 && channels != 2 {
            return Err(Error::InvalidArgument(
                "mp3 encoder supports 1 or 2 channels",
            ));
        }
        let Some(version) = version_for_rate(sample_rate) else {
            return Err(Error::InvalidArgument(
                "mp3 sample rate must be one of 8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000 Hz",
            ));
        };
        if kbps == 320 && version != MpegVersion::Mpeg1 {
            return Err(Error::InvalidArgument(
                "mp3 320 kbps is not a valid mpeg-2/2.5 layer iii bitrate (maximum 160 kbps)",
            ));
        }
        if bitrate_index(version, kbps).is_none() {
            return Err(Error::InvalidArgument(
                "mp3 cbr bitrate must be one of 64, 96, 128, 320 kbps",
            ));
        }
        if joint_solver().is_none() {
            return Err(Error::Unsupported(
                "mp3 joint spectrum map could not be built",
            ));
        }
        let (granules, samples_per_frame) = match version {
            MpegVersion::Mpeg1 => (2, 1152),
            _ => (1, 576),
        };
        // Frame length (ISO/IEC 11172-3 section 2.4.1): the
        // padding bit is never set, so this is the floor of
        // the nominal length.
        let frame_length = samples_per_frame * kbps as usize * 1000 / (sample_rate as usize * 8);
        let side_len = match version {
            MpegVersion::Mpeg1 => {
                if channels == 1 {
                    17
                } else {
                    32
                }
            }
            _ => {
                if channels == 1 {
                    9
                } else {
                    17
                }
            }
        };
        Ok(Self {
            sample_rate,
            channels,
            kbps,
            version,
            frame_length,
            side_len,
            granules,
            samples_per_frame,
            layout: BandLayout::new(version, sample_rate),
            sim: DecoderSim::new(),
            buffer: VecDeque::new(),
            ready: VecDeque::new(),
        })
    }

    /// Sample rate in Hz.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Number of channels.
    #[must_use]
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// CBR bitrate in kbps.
    #[must_use]
    pub fn bitrate_kbps(&self) -> u32 {
        self.kbps
    }

    /// Frame length in bytes.
    #[must_use]
    pub fn frame_length(&self) -> usize {
        self.frame_length
    }

    /// Samples per frame per channel (1152 for MPEG-1, 576
    /// for MPEG-2/2.5).
    #[must_use]
    pub fn samples_per_frame(&self) -> usize {
        self.samples_per_frame
    }

    /// Frame header bytes (ISO/IEC 11172-3 section 2.4.1):
    /// sync, MPEG-1/2/2.5, Layer III, no CRC, the bitrate and
    /// sample-rate indexes, no padding, plain stereo or mono.
    fn header_bytes(&self) -> [u8; 4] {
        let version_bits = match self.version {
            MpegVersion::Mpeg1 => 0b11,
            MpegVersion::Mpeg2 => 0b10,
            MpegVersion::Mpeg25 => 0b00,
        };
        let bitrate = bitrate_index(self.version, self.kbps).unwrap_or(0);
        let rate = rate_index(self.sample_rate);
        let channel_mode: u8 = if self.channels == 1 { 0b11 } else { 0b00 };
        [
            0xFF,
            0xE0 | (version_bits << 3) | 0x02 | 0x01,
            (bitrate << 4) | (rate << 2),
            channel_mode << 6,
        ]
    }

    /// Side information for the frame's granule channels
    /// (ISO/IEC 11172-3 section 2.4.2 and ISO/IEC 13818-3
    /// section 2.4.2). `params` is granule-major, channel
    /// inside granule, matching the main-data order.
    fn side_info_bytes(&self, params: &[ChannelParams]) -> Result<Vec<u8>> {
        let mut writer = BitWriter::new();
        let is_mpeg1 = self.version == MpegVersion::Mpeg1;
        if is_mpeg1 {
            writer.write_bits(0, 9)?;
            if self.channels == 1 {
                writer.write_bits(0, 5)?;
                writer.write_bits(0, 4)?;
            } else {
                writer.write_bits(0, 3)?;
                writer.write_bits(0, 4)?;
                writer.write_bits(0, 4)?;
            }
        } else {
            writer.write_bits(0, 8)?;
            if self.channels == 1 {
                writer.write_bits(0, 1)?;
            } else {
                writer.write_bits(0, 2)?;
            }
        }
        for param in params {
            writer.write_bits(u32::from(param.part_23_length), 12)?;
            writer.write_bits(u32::from(param.big_values), 9)?;
            writer.write_bits(u32::from(param.global_gain), 8)?;
            writer.write_bits(
                u32::from(param.scalefac_compress),
                if is_mpeg1 { 4 } else { 9 },
            )?;
            // Long block: no window switching.
            writer.write_bit(false);
            for &table in &param.table_select {
                writer.write_bits(u32::from(table), 5)?;
            }
            writer.write_bits(u32::from(param.region_count[0]), 4)?;
            writer.write_bits(u32::from(param.region_count[1]), 3)?;
            if is_mpeg1 {
                // No preflag.
                writer.write_bit(false);
            }
            // Scalefactor scale 0.
            writer.write_bit(false);
            writer.write_bit(param.count1_table);
        }
        let bytes = writer.finish();
        if bytes.len() != self.side_len {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "mp3 side info length does not match the header",
            });
        }
        Ok(bytes)
    }

    /// Encodes one frame into a packet. The frame must hold
    /// exactly `samples_per_frame` samples per channel at the
    /// encoder's rate and channel count; samples outside
    /// [-1, 1] are clamped and non-finite samples are treated
    /// as silence. Every frame is independently decodable
    /// (`main_data_begin` is 0), so every packet is a
    /// keyframe.
    ///
    /// The packet returned is not necessarily for this
    /// frame: the encoder solves each granule jointly with
    /// the two following granules, so it holds a window of
    /// lookahead. The first call(s) return silent priming
    /// frames (see `priming_frames`); the packets for the
    /// input frames follow in order, and the last frames are
    /// returned by `flush`. Feed frames in order from the
    /// start of the stream.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the frame's channel
    /// count, sample rate, or length does not match the
    /// encoder, and `InvalidData`/`Overflow` when the
    /// bitstream cannot be assembled.
    pub fn encode(&mut self, frame: &AudioFrame) -> Result<Packet> {
        if frame.channels != self.channels {
            return Err(Error::InvalidArgument(
                "frame channel count does not match mp3 encoder config",
            ));
        }
        if frame.sample_rate != self.sample_rate {
            return Err(Error::InvalidArgument(
                "frame sample rate does not match mp3 encoder config",
            ));
        }
        if frame.frame_count() != self.samples_per_frame {
            return Err(Error::InvalidArgument(
                "mp3 encoder requires exactly one frame of samples per encode call",
            ));
        }
        let channels = self.channels as usize;
        // Deinterleaved, sanitized input, granule-major.
        let mut granules: Vec<[[f32; LINES]; 2]> = Vec::with_capacity(self.granules);
        for granule in 0..self.granules {
            let mut targets = [[0.0f32; LINES]; 2];
            for (ch, target) in targets.iter_mut().enumerate().take(channels) {
                for (line, slot) in target.iter_mut().enumerate() {
                    let index = (granule * LINES + line) * channels + ch;
                    let value = frame.samples.get(index).copied().unwrap_or(0.0);
                    *slot = if value.is_finite() {
                        value.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    };
                }
            }
            granules.push(targets);
        }
        self.buffer.push_back(BufferedFrame {
            timestamp: frame.timestamp,
            granules,
        });
        self.commit_available()?;
        if let Some(packet) = self.ready.pop_front() {
            return Ok(packet);
        }
        // Startup: the buffer does not yet hold a full
        // window, so no frame has been committed. Emit one
        // silent priming frame; the decoder plays it as
        // encoder delay and the real frames follow in order.
        self.encode_silent_frame(frame.timestamp)
    }

    /// Drains the buffered tail: commits every remaining
    /// frame (the last window has no lookahead beyond the
    /// stream, so its spectra are solved against a silent
    /// future) and returns all packets not yet returned, in
    /// order. Call it after the last `encode` to retrieve
    /// the final frames.
    ///
    /// # Errors
    ///
    /// Returns the bitstream errors of `encode`.
    pub fn flush(&mut self) -> Result<Vec<Packet>> {
        while self.buffered_granules() > 0 {
            self.commit_window(true)?;
        }
        Ok(self.ready.drain(..).collect())
    }

    /// Silent priming frames emitted at the start of the
    /// stream (the encoder delay): one frame for MPEG-1,
    /// three for MPEG-2/2.5.
    #[must_use]
    pub fn priming_frames(&self) -> usize {
        WINDOW_GRANULES / self.granules - 1
    }

    /// Granules currently buffered.
    fn buffered_granules(&self) -> usize {
        self.buffer.iter().map(|frame| frame.granules.len()).sum()
    }

    /// Commits frames while a full window is buffered.
    fn commit_available(&mut self) -> Result<()> {
        while self.buffered_granules() >= WINDOW_GRANULES {
            self.commit_window(false)?;
        }
        Ok(())
    }

    /// Solves the oldest window of granules jointly and
    /// commits its first granules. With `commit_all` (the
    /// flush path) every buffered granule is committed;
    /// otherwise only the first `COMMIT_GRANULES` are, and
    /// the rest stay buffered as lookahead for the next
    /// window. The solve window is always `WINDOW_GRANULES`
    /// wide: on the flush path the buffer may hold fewer
    /// granules, so the lookahead is padded by repeating the
    /// last buffered granule (assume the signal continues).
    /// The padded spectra are solved but never committed;
    /// without them the tail granules are solved in a short
    /// window and quantize badly (measured: a 0.94 spike on
    /// the stream's last sample). The window always ends on
    /// a frame boundary (all frames of an encoder hold the
    /// same granule count, and both the window and the commit
    /// span are multiples of it), so whole frames are
    /// committed.
    fn commit_window(&mut self, commit_all: bool) -> Result<()> {
        let channels = self.channels as usize;
        let Some(solver) = joint_solver() else {
            return Err(Error::Unsupported(
                "mp3 joint spectrum map could not be built",
            ));
        };
        let buffered = self.buffered_granules();
        let window = if commit_all {
            WINDOW_GRANULES
        } else {
            buffered.min(WINDOW_GRANULES)
        };
        let real = buffered.min(window);
        if real == 0 {
            return Ok(());
        }
        // The window's targets, granule-major, per channel:
        // the buffered granules, then repeats of the last one
        // to fill the window (flush padding, never committed).
        let mut targets: Vec<[[f64; LINES]; 2]> = Vec::with_capacity(window);
        let mut taken = 0usize;
        for frame in &self.buffer {
            for granule in &frame.granules {
                if taken >= real {
                    break;
                }
                let mut target = [[0.0f64; LINES]; 2];
                for ch in 0..channels {
                    for line in 0..LINES {
                        target[ch][line] = f64::from(granule[ch][line]);
                    }
                }
                targets.push(target);
                taken += 1;
            }
            if taken >= real {
                break;
            }
        }
        while targets.len() < window {
            let Some(last) = targets.last().copied() else {
                return Ok(());
            };
            targets.push(last);
        }
        // The state's additive contribution over the window,
        // from a probe run on a copy so the live state is
        // untouched.
        let mut probe = self.sim.clone();
        let mut baselines: Vec<[[f64; LINES]; 2]> = Vec::with_capacity(window);
        for _ in 0..window {
            let zero = [[0.0f32; LINES]; 2];
            let state_pcm = probe.run_granule(&zero, channels)?;
            let mut baseline = [[0.0f64; LINES]; 2];
            for ch in 0..channels {
                for line in 0..LINES {
                    baseline[ch][line] = f64::from(state_pcm[line * channels + ch]);
                }
            }
            baselines.push(baseline);
        }
        // Solve each channel's window, then sanitize to f32.
        let mut window_spectra: Vec<[[f32; LINES]; 2]> = Vec::with_capacity(window);
        for _ in 0..window {
            window_spectra.push([[0.0f32; LINES]; 2]);
        }
        for ch in 0..channels {
            let targets_ch: Vec<[f64; LINES]> = targets.iter().map(|t| t[ch]).collect();
            let baselines_ch: Vec<[f64; LINES]> = baselines.iter().map(|b| b[ch]).collect();
            let spectra = solver.solve_channel(&targets_ch, &baselines_ch);
            for (k, spectrum) in spectra.iter().enumerate() {
                for (line, &value) in spectrum.iter().enumerate() {
                    // The solved spectra are bounded magnitudes
                    // (measured max |x| < 1) and finite-checked,
                    // so the f64 -> f32 narrowing loses nothing.
                    #[allow(clippy::cast_possible_truncation)]
                    let sample = if value.is_finite() { value as f32 } else { 0.0 };
                    window_spectra[k][ch][line] = sample;
                }
            }
        }
        // Commit the window's first granules, whole frames
        // only, in order (the padded lookahead is never
        // committed).
        let commit = if commit_all { real } else { COMMIT_GRANULES };
        let mut remaining = commit;
        let mut spectra_idx = 0usize;
        while remaining > 0 {
            let Some(front) = self.buffer.front() else {
                break;
            };
            let count = front.granules.len();
            if count > remaining {
                break;
            }
            let Some(frame) = self.buffer.pop_front() else {
                break;
            };
            let packet = self.commit_frame(
                &window_spectra[spectra_idx..spectra_idx + count],
                frame.timestamp,
            )?;
            spectra_idx += count;
            self.ready.push_back(packet);
            remaining -= count;
        }
        Ok(())
    }

    /// Commits one silent frame (all-zero spectra) and
    /// returns its packet. Used for the startup priming
    /// frames; the silence advances the simulation exactly
    /// as a decoder would after playing a silent frame.
    fn encode_silent_frame(&mut self, timestamp: u64) -> Result<Packet> {
        let spectra = vec![[[0.0f32; LINES]; 2]; self.granules];
        self.commit_frame(&spectra, timestamp)
    }

    /// Quantizes one frame's solved granule spectra,
    /// Huffman-encodes them, advances the live simulation
    /// with the spectra actually written, and assembles the
    /// frame packet. `spectra` is granule-major, channel
    /// inside granule, and must hold `self.granules`
    /// granules.
    fn commit_frame(&mut self, spectra: &[[[f32; LINES]; 2]], timestamp: u64) -> Result<Packet> {
        if spectra.len() != self.granules {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "mp3 encode frame does not hold its granule count",
            });
        }
        let channels = self.channels as usize;
        let tables = huffman_tables();
        let is_mpeg1 = self.version == MpegVersion::Mpeg1;
        let main_data_bits = (self.frame_length - 4 - self.side_len) * 8;
        let per_channel_budget =
            main_data_bits.saturating_sub(ANCILLARY_RESERVE_BITS) / (self.granules * channels);
        let per_channel_budget = per_channel_budget.min(4095);
        let mut all_params: Vec<ChannelParams> = Vec::with_capacity(self.granules * channels);
        let mut main_writer = BitWriter::new();
        for granule_spectra in spectra {
            let mut dequantized = [[0.0f32; LINES]; 2];
            for ch in 0..channels {
                let mut accepted: Option<(ChannelParams, BitWriter)> = None;
                for (qtarget, cutoff) in QUANT_ATTEMPTS {
                    let quantized = if cutoff == 0 {
                        let mut silent =
                            quantize_channel(&[0.0f32; LINES], &self.layout, is_mpeg1, 1.0, 0);
                        // The silent attempt must carry no
                        // scalefactor bits: all steps equal.
                        silent.global_gain = 128;
                        silent.iscf = [0u8; BANDS];
                        silent.scalefac_compress = 0;
                        silent
                    } else {
                        quantize_channel(
                            &granule_spectra[ch],
                            &self.layout,
                            is_mpeg1,
                            qtarget,
                            cutoff,
                        )
                    };
                    let mut attempt = BitWriter::new();
                    let params = encode_channel_bits(
                        &quantized,
                        &self.layout,
                        is_mpeg1,
                        tables,
                        &mut attempt,
                    )?;
                    if usize::from(params.part_23_length) <= per_channel_budget {
                        dequantized[ch].copy_from_slice(&quantized.dequantized);
                        accepted = Some((params, attempt));
                        break;
                    }
                }
                let Some((params, attempt)) = accepted else {
                    return Err(Error::InvalidData {
                        offset: 0,
                        detail: "mp3 encode found no quantization that fits the frame",
                    });
                };
                // Copy the accepted bits into the frame's main
                // data, in order.
                let bits = attempt.finish();
                for bit in 0..usize::from(params.part_23_length) {
                    let byte = bits.get(bit / 8).copied().unwrap_or(0);
                    main_writer.write_bit(byte & (1 << (7 - bit % 8)) != 0);
                }
                all_params.push(params);
            }
            // Advance the live simulation with the spectrum
            // that was actually written.
            let _ = self.sim.run_granule(&dequantized, channels)?;
        }
        let side_info = self.side_info_bytes(&all_params)?;
        let mut data = Vec::with_capacity(self.frame_length);
        data.extend_from_slice(&self.header_bytes());
        data.extend_from_slice(&side_info);
        let main_bytes = main_writer.finish();
        data.extend_from_slice(&main_bytes);
        // Zero ancillary padding to the exact CBR length.
        while data.len() < self.frame_length {
            data.push(0);
        }
        if data.len() != self.frame_length {
            return Err(Error::Overflow("mp3 frame exceeds its cbr length"));
        }
        Ok(Packet {
            codec: Codec::Mp3,
            data,
            timestamp,
            is_keyframe: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_tables_derive_a_prefix_code_for_every_table() {
        let tables = huffman_tables();
        for num in 1..32usize {
            if num == 4 || num == 14 {
                assert!(tables.big[num].is_none(), "table {num} must be unused");
                continue;
            }
            let table = tables.big[num]
                .as_ref()
                .unwrap_or_else(|| panic!("table {num} missing"));
            assert!(!table.codes.is_empty(), "table {num} has no codes");
            // No codeword is a prefix of another (prefix property).
            let mut codes: Vec<(u32, u32)> =
                table.codes.values().map(|c| (c.code, c.len)).collect();
            codes.sort_unstable();
            for i in 0..codes.len() {
                for j in (i + 1)..codes.len() {
                    let (c1, l1) = codes[i];
                    let (c2, l2) = codes[j];
                    if l1 <= l2 {
                        assert_ne!(
                            c1,
                            c2 >> (l2 - l1),
                            "table {num}: code {c1}/{l1} prefixes {c2}/{l2}"
                        );
                    }
                }
            }
            println!(
                "big table {num}: {} symbols max_mag={} linbits={}",
                table.codes.len(),
                table.max_mag,
                table.linbits
            );
        }
    }

    #[test]
    fn count1_tables_cover_all_quad_patterns() {
        let tables = huffman_tables();
        for (i, table) in tables.count1.iter().enumerate() {
            for pattern in 0..16usize {
                assert!(
                    table.codes[pattern].is_some(),
                    "count1 table {i} lacks pattern {pattern:04b}"
                );
            }
            println!("count1 table {i} complete");
        }
    }

    // The test indices are bounded by 8 granules x 576 lines, so
    // the usize-to-f64 casts are exact; the f64-to-f32 cast is
    // intentional because the pipeline stores spectra as f32.
    #[allow(clippy::cast_precision_loss)]
    #[allow(clippy::cast_possible_truncation)]
    #[test]
    fn joint_window_solver_reproduces_a_sine_stream() {
        let solver = joint_solver().expect("joint spectrum map must build");
        // Eight granules of a 440 Hz sine at 44.1 kHz, mono.
        let granules = 8usize;
        let mut targets: Vec<[f64; LINES]> = Vec::with_capacity(granules);
        for g in 0..granules {
            let mut t = [0.0f64; LINES];
            for (line, v) in t.iter_mut().enumerate() {
                let idx = g * LINES + line;
                *v = 0.5 * (2.0 * std::f64::consts::PI * 440.0 * idx as f64 / 44100.0).sin();
            }
            targets.push(t);
        }
        let mut sim = DecoderSim::new();
        let mut g_start = 0usize;
        while g_start < granules {
            let w = (granules - g_start).min(WINDOW_GRANULES);
            // Baseline: the state's contribution over the
            // window, from a probe on a copy.
            let mut probe = sim.clone();
            let mut baselines: Vec<[f64; LINES]> = Vec::with_capacity(w);
            for _ in 0..w {
                let pcm = probe.run_granule(&[[0.0f32; LINES]; 2], 1).unwrap();
                let mut baseline = [0.0f64; LINES];
                for (i, &v) in pcm.iter().enumerate() {
                    baseline[i] = f64::from(v);
                }
                baselines.push(baseline);
            }
            let spectra = solver.solve_channel(&targets[g_start..g_start + w], &baselines);
            let commit = COMMIT_GRANULES.min(w);
            for (k, spectrum) in spectra.iter().enumerate().take(commit) {
                let g = g_start + k;
                let mut spec = [0.0f32; LINES];
                for (i, &v) in spectrum.iter().enumerate() {
                    spec[i] = v as f32;
                }
                let max_spec = spectrum.iter().fold(0.0f64, |a, &v| a.max(v.abs()));
                assert!(
                    max_spec < 1.0,
                    "granule {g}: solved spectrum implausibly large: {max_spec}"
                );
                let pcm = sim.run_granule(&[spec, [0.0f32; LINES]], 1).unwrap();
                let mut se = 0.0f64;
                for (i, &v) in pcm.iter().enumerate() {
                    let d = f64::from(v) - targets[g][i];
                    se += d * d;
                }
                let rms = (se / LINES as f64).sqrt();
                println!("granule {g}: pipeline rms={rms:.4}");
                // Granules 0-1 carry the startup transient
                // (the state starts at zero); from granule 2
                // the joint window reproduces the sine.
                let limit = if g < 2 { 0.4 } else { 0.01 };
                assert!(
                    rms < limit,
                    "granule {g}: pipeline rms {rms:.4} exceeds {limit}"
                );
            }
            g_start += commit;
        }
    }
}
