//! Opus CELT sample reconstruction (RFC 6716 sections 4.3, 4.3.1,
//! 4.3.3, 4.3.4, 4.3.5, 4.3.6, 4.3.7, 4.3.7.1, 4.3.7.2).
//!
//! This is a faithful port of the float-mode path of
//! `celt_decode_with_ec` from the RFC 6716 Appendix A reference
//! implementation (Opus 1.0, `celt/celt.c`, `bands.c`,
//! `quant_bands.c`, `rate.c`, `vq.c`, `cwrs.c`, `laplace.c`,
//! `mdct.c`), for the single mode Opus uses at 48 kHz:
//! `mode48000_960_120` (960-sample frames, 120-sample overlap, 21
//! critical bands, fullband). Only CELT-only packets are decoded;
//! SILK and hybrid packets stay `Unsupported` (see `decoder.rs`).
//!
//! All tables are copied verbatim from the reference C source; each
//! table carries a comment naming its symbol and file in that
//! source, plus the RFC 6716 section that defines its use. No
//! coefficient is invented. The one table the task text mentions
//! that does not exist in this reference version is
//! `celt_pulse_cache`: in Opus 1.0 the PVQ uses the combinatorial
//! coder in `cwrs.c` directly, with the static `cache_index50` /
//! `cache_bits50` / `cache_caps50` tables below (the pulse cache is
//! only computed at runtime for custom modes, which Opus never
//! uses).
//!
//! Float-mode semantics: in the reference, every fixed-point shift
//! macro is the identity in float mode (`arch.h`), so the port
//! works in plain `f32`: energy prediction is
//! `old = coef * old + prev + qi`, `prev += qi - beta * qi`; the
//! band amplitude is `2^(old + eMeans[i])` (`celt_exp2` is the exact
//! `exp2` here, not the `FLOAT_APPROX` polynomial, which matches
//! what the reference build and libopus on x86 use); the comb
//! filter and de-emphasis are plain multiplies with the
//! `CELT_SIG_SCALE` (32768) division only at the final output.
//!
//! Inverse MDCT: the reference `clt_mdct_backward` is the same
//! cosine sum `mdct_inverse` computes (verified against the
//! reference build: the recovered half-IMDCT equals the direct
//! cosine sum at scale 1.0, max error 1.2e-5 for a 960-sample
//! block and the same for a 120-sample short block). The note in
//! `mdct.rs` claiming a 1/2 pre-scale is therefore NOT applied:
//! pre-scaling would make every decoded frame 6 dB too quiet. The
//! mirror/window mixing of `clt_mdct_backward` is reproduced
//! exactly (first `overlap` samples accumulate, the rest assign).
//!
//! Range-decoder seed: the reference carries the range decoder's
//! internal `rng` into the next frame (`st->rng = dec->rng`) and
//! uses it only as the LCG seed for noise fill (empty bands, fold
//! dither, anti-collapse). `RangeDecoder` does not expose `rng`, so
//! `CeltRangeReader` below tracks it exactly by mirroring the
//! `rng` update of every range-coded symbol; `decode_frame` takes
//! the seed from the reader at the end of each frame. The seed
//! affects audio values only, never symbols, and with the exact
//! seed the noise fixture matches the reference to ~3e-6 RMS.
//!
//! Intensity stereo: `quant_band` takes no band energies here.
//! In the reference, `intensity_stereo` (which is the only use of
//! `bandE` inside `quant_all_bands` / `quant_band`) is called only
//! under `if (encode)` (`bands.c:866` and `:882`), so the decoder
//! never calls it — and both the RFC reference and modern libopus
//! pass `bandE = NULL` to `quant_all_bands` in
//! `celt_decode_with_ec`. The left/right split for an
//! intensity-coded band is recovered by `stereo_merge`, which
//! derives the gains from the decoded mid/side norms and
//! correlation, not from the band energies. This was verified
//! against an instrumented build of the reference: it calls
//! `intensity_stereo` zero times on every fixture (128k stereo
//! sine/noise/music/speech and 32k music).
//!
//! API: `CeltDecoder` holds the per-stream state (band energies,
//! overlap memory, post-filter, de-emphasis memory) and decodes
//! one frame at a time with `decode_frame`. The caller supplies
//! the CELT payload (one frame from `parse_opus_packet`), the end
//! band for the packet bandwidth (`celt_end_band`), and the frame
//! duration in samples. `decode_frame` returns interleaved `f32`
//! samples in [-1, 1].
//!
//! Clippy: this file is a faithful port of C reference code, so
//! several pedantic lints are allowed module-wide, each for a
//! reason tied to the port:
//! - the integer casts (`cast_*`) mirror C's implicit conversions
//!   at the same points in the same expressions;
//! - the table literals (`unreadable_literal`,
//!   `excessive_precision`) must stay byte-for-byte identical to
//!   the C source so they can be verified against it;
//! - `doc_markdown`, `many_single_char_names`, `too_many_lines`
//!   and `too_many_arguments` follow the reference's identifiers
//!   and function shapes; `large_stack_arrays` is the decode
//!   buffer the reference also keeps on the stack.

#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::unreadable_literal)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::large_stack_arrays)]

use crate::celt::decode_celt_frame_header_from;
use crate::error::{Error, Result};
use crate::lsb_bitreader::ilog;
use crate::mdct::mdct_inverse;
use crate::opus_packet::OpusBandwidth;
use crate::range_decoder::RangeDecoder;

// ---------------------------------------------------------------------------
// Constants (RFC 6716 sections 4.1, 4.3, 4.3.5, 4.3.6, 4.3.7.1)
// ---------------------------------------------------------------------------

/// Range-coder resolution: 1/8 bit (RFC 6716 section 4.1).
const BITRES: i32 = 3;
/// Shortest pitch period the comb filter accepts.
const COMBFILTER_MINPERIOD: i32 = 15;
/// Size of the per-channel decode (overlap-add) buffer.
const DECODE_BUFFER_SIZE: usize = 2048;
/// Maximum number of pulses in one PVQ vector (RFC 6716 section
/// 4.3.6).
const MAX_PULSES: i32 = 128;
/// Maximum fine-energy bits per band (RFC 6716 section 4.3.4).
const MAX_FINE_BITS: i32 = 8;
/// Fine-energy offset in the bit allocation (RFC 6716 section
/// 4.3.5).
const FINE_OFFSET: i32 = 21;
/// Theta-resolution offset for a split (RFC 6716 section 4.3.6).
const QTHETA_OFFSET: i32 = 4;
/// Theta-resolution offset for a two-phase (N=2 stereo) split.
const QTHETA_OFFSET_TWOPHASE: i32 = 16;
/// Bisection steps in `interp_bits2pulses` (RFC 6716 section 4.3.5).
const ALLOC_STEPS: i32 = 6;
/// Pulse spreading choices (RFC 6716 section 4.3.6, bands.h).
const SPREAD_NONE: i32 = 0;
const SPREAD_NORMAL: i32 = 2;
const SPREAD_AGGRESSIVE: i32 = 3;

/// Mode `mode48000_960_120` (RFC 6716 section 4.3.7, Table 59):
/// the only CELT mode Opus uses at 48 kHz.
const NB_EBANDS: usize = 21;
const EFF_EBANDS: usize = 21;
const OVERLAP: usize = 120;
const SHORT_MDCT_SIZE: usize = 120;
const NB_ALLOC_VECTORS: usize = 11;
/// Pre-emphasis coefficients (RFC 6716 section 4.3.7.2): the
/// encoder applies `x - 0.85 * x[-1]`; the decoder inverts it.
const PREEMPH: [f32; 4] = [0.85, 0.0, 1.0, 1.0];
/// `CELT_SIG_SCALE` (arch.h): internal signals are 32768x the
/// output sample; the final division happens in de-emphasis.
const CELT_SIG_SCALE: f32 = 32768.0;
/// `Q15ONE` in float mode (arch.h): 1.0.
const Q15_ONE: f32 = 1.0;
/// `NORM_SCALING` in float mode (arch.h): 1.0.
const NORM_SCALING: f32 = 1.0;
/// `EPSILON` in float mode (arch.h): keeps empty-band energies
/// non-zero so normalisation never divides by zero.
const EPSILON: f32 = 1e-15;

/// End band for a CELT-only packet, by Opus bandwidth
/// (`opus_decoder.c`: NB 13, MB/WB 17, SWB 19, FB 21). The end band
/// is a band index, not a sample count: bands `end..21` are zeroed.
#[must_use]
pub fn celt_end_band(bandwidth: OpusBandwidth) -> usize {
    match bandwidth {
        OpusBandwidth::Narrowband => 13,
        OpusBandwidth::Mediumband | OpusBandwidth::Wideband => 17,
        OpusBandwidth::SuperWideband => 19,
        OpusBandwidth::Fullband => 21,
    }
}

// ---------------------------------------------------------------------------
// Range reader with exact `rng` tracking
// ---------------------------------------------------------------------------

/// Renormalisation threshold for the range coder (RFC 6716 section
/// 4.1.2.1): the range is renormalised until `rng > 2**23`. Mirrors
/// `RENORM_THRESHOLD` in `range_decoder.rs`.
const RANGE_RENORM_THRESHOLD: u32 = 1 << 23;

/// A range decoder that also tracks the range coder's `rng` interval
/// exactly (RFC 6716 section 4.1).
///
/// `RangeDecoder` keeps `rng` private, but the CELT noise seed for
/// the next frame IS that interval: the reference ends
/// `celt_decode_with_ec` with `st->rng = dec->rng` (`celt.c`) and
/// uses it only as the LCG seed for noise fill (empty bands, fold
/// dither, anti-collapse). A wrong seed makes white-noise-like
/// content decode to different random values (same spectrum, wrong
/// samples), which is why the seed is tracked here rather than
/// approximated. This wrapper delegates every symbol to
/// `RangeDecoder` and mirrors the `rng` update of each range-coded
/// symbol, so `rng()` returns exactly the interval the reference
/// would carry into the next frame. Raw-bit reads (`decode_bits`)
/// do not touch the range coder and need no tracking.
pub(crate) struct CeltRangeReader<'a> {
    dec: RangeDecoder<'a>,
    /// Shadow of the range decoder's `rng` interval.
    rng: u32,
}

impl<'a> CeltRangeReader<'a> {
    /// Creates a reader over `data`. The initial `rng` is the value
    /// after `RangeDecoder::new` renormalises from 128: it shifts
    /// left by 8 while `rng <= 2**23`, landing on `2**31`.
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let dec = RangeDecoder::new(data);
        let mut rng = 128u32;
        while rng <= RANGE_RENORM_THRESHOLD {
            rng <<= 8;
        }
        Self { dec, rng }
    }

    /// The range coder's `rng` interval after the symbols decoded
    /// so far. This is the noise seed the reference carries into
    /// the next frame.
    pub(crate) fn rng(&self) -> u32 {
        self.rng
    }

    /// Mirror `RangeDecoder::update`'s `rng` transition (section
    /// 4.1.2): `r = rng/ft`, then `rng = r*(fh-fl)` when `fl > 0`,
    /// else `rng -= r*(ft-fh)`, then renormalise.
    fn track_update(&mut self, fl: u32, fh: u32, ft: u32) {
        let r = self.rng / ft;
        if fl > 0 {
            self.rng = r * (fh - fl);
        } else {
            self.rng -= r * (ft - fh);
        }
        while self.rng <= RANGE_RENORM_THRESHOLD {
            self.rng <<= 8;
        }
    }

    /// `RangeDecoder::decode` (section 4.1.2). Reading a symbol
    /// does not change `rng`; the matching `update` does.
    pub(crate) fn decode(&mut self, ft: u32) -> Result<u32> {
        self.dec.decode(ft)
    }

    /// `RangeDecoder::update`, with `rng` tracking.
    pub(crate) fn update(&mut self, fl: u32, fh: u32, ft: u32) -> Result<()> {
        self.dec.update(fl, fh, ft)?;
        self.track_update(fl, fh, ft);
        Ok(())
    }

    /// `RangeDecoder::decode_icdf`, with `rng` tracking. The
    /// `(fl, fh)` pair is recomputed from the table and the
    /// returned symbol exactly as `decode_icdf` derives it.
    pub(crate) fn decode_icdf(&mut self, icdf: &[u8], ftb: u32) -> Result<u32> {
        let symbol = self.dec.decode_icdf(icdf, ftb)?;
        let ft = 1u32 << ftb;
        let k = symbol as usize;
        let fh_entry = icdf.get(k).copied().ok_or(Error::InvalidData {
            offset: 0,
            detail: "range decoder icdf symbol out of range",
        })?;
        let fh = ft - u32::from(fh_entry);
        let fl = if k == 0 {
            0
        } else {
            let prev = icdf.get(k - 1).copied().ok_or(Error::InvalidData {
                offset: 0,
                detail: "range decoder icdf symbol out of range",
            })?;
            ft - u32::from(prev)
        };
        self.track_update(fl, fh, ft);
        Ok(symbol)
    }

    /// `RangeDecoder::decode_bit_logp`, with `rng` tracking.
    pub(crate) fn decode_bit_logp(&mut self, logp: u32) -> Result<bool> {
        let bit = self.dec.decode_bit_logp(logp)?;
        let ft = 1u32 << logp;
        if bit {
            self.track_update(ft - 1, ft, ft);
        } else {
            self.track_update(0, ft - 1, ft);
        }
        Ok(bit)
    }

    /// `RangeDecoder::decode_bits` (section 4.1.4). Raw bits come
    /// from the end of the frame and do not touch the range coder.
    pub(crate) fn decode_bits(&mut self, n: u32) -> Result<u32> {
        self.dec.decode_bits(n)
    }

    /// `RangeDecoder::decode_uint`, with `rng` tracking. Values
    /// above 8 bits are split into a range-coded high part and raw
    /// low bits; only the high part touches the range coder.
    pub(crate) fn decode_uint(&mut self, ft: u32) -> Result<u32> {
        let value = self.dec.decode_uint(ft)?;
        let ftb = ilog(ft - 1);
        if ftb <= 8 {
            self.track_update(value, value + 1, ft);
        } else {
            let ft_hi = ((ft - 1) >> (ftb - 8)) + 1;
            let value_hi = value >> (ftb - 8);
            self.track_update(value_hi, value_hi + 1, ft_hi);
        }
        Ok(value)
    }

    /// `RangeDecoder::tell` (section 4.1.6.1).
    pub(crate) fn tell(&self) -> u32 {
        self.dec.tell()
    }

    /// `RangeDecoder::tell_frac` (section 4.1.6.2).
    pub(crate) fn tell_frac(&self) -> u32 {
        self.dec.tell_frac()
    }
}

/// Per-stream CELT decoder state (RFC 6716 section 4.3.7.2,
/// `OpusCustomDecoder` in `celt.c`). One instance decodes a whole
/// Opus stream; the state is what makes consecutive frames join
/// (overlap-add, energy prediction, post-filter, de-emphasis).
pub struct CeltDecoder {
    /// Stream channel count (1 or 2); also the output count.
    channels: usize,
    /// LCG seed for noise fill (see the module docs: an
    /// approximation of the reference's `dec->rng`).
    rng: u32,
    /// Post-filter pitch period and gain of the previous frame.
    postfilter_period_old: i32,
    postfilter_gain_old: f32,
    postfilter_tapset_old: i32,
    /// Post-filter pitch period and gain of the current frame.
    postfilter_period: i32,
    postfilter_gain: f32,
    postfilter_tapset: i32,
    /// De-emphasis memory, one per channel.
    preemph_mem: [f32; 2],
    /// Per-channel overlap-add buffer, `DECODE_BUFFER_SIZE +
    /// OVERLAP` samples (see `celt_decode_with_ec`: `out_mem` sits
    /// at `DECODE_BUFFER_SIZE - MAX_PERIOD`, `overlap_mem` at
    /// `DECODE_BUFFER_SIZE`, `out_syn` at `DECODE_BUFFER_SIZE -
    /// frame_size`).
    decode_mem: [[f32; DECODE_BUFFER_SIZE + OVERLAP]; 2],
    /// Previous coarse band energies, per channel (2 x 21).
    old_band_e: [[f32; NB_EBANDS]; 2],
    /// Previous log energies, for anti-collapse (2 x 21).
    old_log_e: [[f32; NB_EBANDS]; 2],
    /// Log energies two frames back (2 x 21).
    old_log_e2: [[f32; NB_EBANDS]; 2],
    /// Running background log energies (2 x 21).
    background_log_e: [[f32; NB_EBANDS]; 2],
}

impl CeltDecoder {
    /// Creates a decoder for `channels` (1 or 2) output channels.
    /// The state starts exactly as the reference does after
    /// `celt_decoder_init`: zeroed memories, log energies at -28.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a channel count other than 1
    /// or 2.
    pub fn new(channels: usize) -> Result<Self> {
        if channels != 1 && channels != 2 {
            return Err(Error::InvalidArgument(
                "CELT decoder channel count must be 1 or 2",
            ));
        }
        Ok(Self {
            channels,
            rng: 0,
            postfilter_period_old: 0,
            postfilter_gain_old: 0.0,
            postfilter_tapset_old: 0,
            postfilter_period: 0,
            postfilter_gain: 0.0,
            postfilter_tapset: 0,
            preemph_mem: [0.0; 2],
            decode_mem: [[0.0; DECODE_BUFFER_SIZE + OVERLAP]; 2],
            old_band_e: [[0.0; NB_EBANDS]; 2],
            old_log_e: [[-28.0; NB_EBANDS]; 2],
            old_log_e2: [[-28.0; NB_EBANDS]; 2],
            background_log_e: [[0.0; NB_EBANDS]; 2],
        })
    }

    /// Output channel count.
    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels
    }
}

// ---------------------------------------------------------------------------
// Tables, copied verbatim from the RFC 6716 Appendix A reference
// implementation (Opus 1.0). Each table names its symbol and source
// file there; the RFC section is the one that defines its use. No
// coefficient is invented.
// ---------------------------------------------------------------------------

/// RFC 6716 section 4.3.7 (MDCT window). window120 — static_modes_float.h (MDCT window, 120 samples)
const CELT_WINDOW_120: [f32; 120] = [
    6.7286966e-05f32,
    0.00060551348f32,
    0.0016815970f32,
    0.0032947962f32,
    0.0054439943f32,
    0.0081276923f32,
    0.011344001f32,
    0.015090633f32,
    0.019364886f32,
    0.024163635f32,
    0.029483315f32,
    0.035319905f32,
    0.041668911f32,
    0.048525347f32,
    0.055883718f32,
    0.063737999f32,
    0.072081616f32,
    0.080907428f32,
    0.090207705f32,
    0.099974111f32,
    0.11019769f32,
    0.12086883f32,
    0.13197729f32,
    0.14351214f32,
    0.15546177f32,
    0.16781389f32,
    0.18055550f32,
    0.19367290f32,
    0.20715171f32,
    0.22097682f32,
    0.23513243f32,
    0.24960208f32,
    0.26436860f32,
    0.27941419f32,
    0.29472040f32,
    0.31026818f32,
    0.32603788f32,
    0.34200931f32,
    0.35816177f32,
    0.37447407f32,
    0.39092462f32,
    0.40749142f32,
    0.42415215f32,
    0.44088423f32,
    0.45766484f32,
    0.47447104f32,
    0.49127978f32,
    0.50806798f32,
    0.52481261f32,
    0.54149077f32,
    0.55807973f32,
    0.57455701f32,
    0.59090049f32,
    0.60708841f32,
    0.62309951f32,
    0.63891306f32,
    0.65450896f32,
    0.66986776f32,
    0.68497077f32,
    0.69980010f32,
    0.71433873f32,
    0.72857055f32,
    0.74248043f32,
    0.75605424f32,
    0.76927895f32,
    0.78214257f32,
    0.79463430f32,
    0.80674445f32,
    0.81846456f32,
    0.82978733f32,
    0.84070669f32,
    0.85121779f32,
    0.86131698f32,
    0.87100183f32,
    0.88027111f32,
    0.88912479f32,
    0.89756398f32,
    0.90559094f32,
    0.91320904f32,
    0.92042270f32,
    0.92723738f32,
    0.93365955f32,
    0.93969656f32,
    0.94535671f32,
    0.95064907f32,
    0.95558353f32,
    0.96017067f32,
    0.96442171f32,
    0.96834849f32,
    0.97196334f32,
    0.97527906f32,
    0.97830883f32,
    0.98106616f32,
    0.98356480f32,
    0.98581869f32,
    0.98784191f32,
    0.98964856f32,
    0.99125274f32,
    0.99266849f32,
    0.99390969f32,
    0.99499004f32,
    0.99592297f32,
    0.99672162f32,
    0.99739874f32,
    0.99796667f32,
    0.99843728f32,
    0.99882195f32,
    0.99913147f32,
    0.99937606f32,
    0.99956527f32,
    0.99970802f32,
    0.99981248f32,
    0.99988613f32,
    0.99993565f32,
    0.99996697f32,
    0.99998518f32,
    0.99999457f32,
    0.99999859f32,
    0.99999982f32,
    1.0000000f32,
];

/// RFC 6716 section 4.3.5 (log2 of band size, for bit allocation). logN400 — static_modes_float.h
const LOG_N_400: [i16; 21] = [
    0, 0, 0, 0, 0, 0, 0, 0, 8, 8, 8, 8, 16, 16, 16, 21, 21, 24, 29, 34, 36,
];

/// RFC 6716 section 4.3.5 (pulse cache index). cache_index50 — static_modes_float.h
const CACHE_INDEX_50: [i16; 105] = [
    -1, -1, -1, -1, -1, -1, -1, -1, 0, 0, 0, 0, 41, 41, 41, 82, 82, 123, 164, 200, 222, 0, 0, 0, 0,
    0, 0, 0, 0, 41, 41, 41, 41, 123, 123, 123, 164, 164, 240, 266, 283, 295, 41, 41, 41, 41, 41,
    41, 41, 41, 123, 123, 123, 123, 240, 240, 240, 266, 266, 305, 318, 328, 336, 123, 123, 123,
    123, 123, 123, 123, 123, 240, 240, 240, 240, 305, 305, 305, 318, 318, 343, 351, 358, 364, 240,
    240, 240, 240, 240, 240, 240, 240, 305, 305, 305, 305, 343, 343, 343, 351, 351, 370, 376, 382,
    387,
];

/// RFC 6716 section 4.3.5 (pulse cache: bits per pulse count). cache_bits50 — static_modes_float.h
const CACHE_BITS_50: [u8; 392] = [
    40, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 40, 15, 23, 28, 31, 34, 36, 38, 39, 41, 42, 43, 44, 45, 46, 47,
    47, 49, 50, 51, 52, 53, 54, 55, 55, 57, 58, 59, 60, 61, 62, 63, 63, 65, 66, 67, 68, 69, 70, 71,
    71, 40, 20, 33, 41, 48, 53, 57, 61, 64, 66, 69, 71, 73, 75, 76, 78, 80, 82, 85, 87, 89, 91, 92,
    94, 96, 98, 101, 103, 105, 107, 108, 110, 112, 114, 117, 119, 121, 123, 124, 126, 128, 40, 23,
    39, 51, 60, 67, 73, 79, 83, 87, 91, 94, 97, 100, 102, 105, 107, 111, 115, 118, 121, 124, 126,
    129, 131, 135, 139, 142, 145, 148, 150, 153, 155, 159, 163, 166, 169, 172, 174, 177, 179, 35,
    28, 49, 65, 78, 89, 99, 107, 114, 120, 126, 132, 136, 141, 145, 149, 153, 159, 165, 171, 176,
    180, 185, 189, 192, 199, 205, 211, 216, 220, 225, 229, 232, 239, 245, 251, 21, 33, 58, 79, 97,
    112, 125, 137, 148, 157, 166, 174, 182, 189, 195, 201, 207, 217, 227, 235, 243, 251, 17, 35,
    63, 86, 106, 123, 139, 152, 165, 177, 187, 197, 206, 214, 222, 230, 237, 250, 25, 31, 55, 75,
    91, 105, 117, 128, 138, 146, 154, 161, 168, 174, 180, 185, 190, 200, 208, 215, 222, 229, 235,
    240, 245, 255, 16, 36, 65, 89, 110, 128, 144, 159, 173, 185, 196, 207, 217, 226, 234, 242, 250,
    11, 41, 74, 103, 128, 151, 172, 191, 209, 225, 241, 255, 9, 43, 79, 110, 138, 163, 186, 207,
    227, 246, 12, 39, 71, 99, 123, 144, 164, 182, 198, 214, 228, 241, 253, 9, 44, 81, 113, 142,
    168, 192, 214, 235, 255, 7, 49, 90, 127, 160, 191, 220, 247, 6, 51, 95, 134, 170, 203, 234, 7,
    47, 87, 123, 155, 184, 212, 237, 6, 52, 97, 137, 174, 208, 240, 5, 57, 106, 151, 192, 231, 5,
    59, 111, 158, 202, 243, 5, 55, 103, 147, 187, 224, 5, 60, 113, 161, 206, 248, 4, 65, 122, 175,
    224, 4, 67, 127, 182, 234,
];

/// RFC 6716 section 4.3.5 (pulse cache: usable-bits cap per band). cache_caps50 — static_modes_float.h
const CACHE_CAPS_50: [u8; 168] = [
    224, 224, 224, 224, 224, 224, 224, 224, 160, 160, 160, 160, 185, 185, 185, 178, 178, 168, 134,
    61, 37, 224, 224, 224, 224, 224, 224, 224, 224, 240, 240, 240, 240, 207, 207, 207, 198, 198,
    183, 144, 66, 40, 160, 160, 160, 160, 160, 160, 160, 160, 185, 185, 185, 185, 193, 193, 193,
    183, 183, 172, 138, 64, 38, 240, 240, 240, 240, 240, 240, 240, 240, 207, 207, 207, 207, 204,
    204, 204, 193, 193, 180, 143, 66, 40, 185, 185, 185, 185, 185, 185, 185, 185, 193, 193, 193,
    193, 193, 193, 193, 183, 183, 172, 138, 65, 39, 207, 207, 207, 207, 207, 207, 207, 207, 204,
    204, 204, 204, 201, 201, 201, 188, 188, 176, 141, 66, 40, 193, 193, 193, 193, 193, 193, 193,
    193, 193, 193, 193, 193, 194, 194, 194, 184, 184, 173, 139, 65, 39, 204, 204, 204, 204, 204,
    204, 204, 204, 201, 201, 201, 201, 198, 198, 198, 187, 187, 175, 140, 66, 40,
];

/// RFC 6716 section 4.3.3, Table 52 (critical-band edges, 5 ms bands). eband5ms — modes.c (band edges, 21 bands, units of 120-sample short MDCT bins)
const EBAND_5MS: [i16; 22] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 34, 40, 48, 60, 78, 100,
];

/// RFC 6716 section 4.3.5 (bit-allocation vectors). band_allocation — modes.c (11 allocation vectors x 21 bands, 1/32 bit/sample)
const BAND_ALLOCATION: [u8; 231] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 90, 80, 75, 69, 63, 56, 49, 40,
    34, 29, 20, 18, 10, 0, 0, 0, 0, 0, 0, 0, 0, 110, 100, 90, 84, 78, 71, 65, 58, 51, 45, 39, 32,
    26, 20, 12, 0, 0, 0, 0, 0, 0, 118, 110, 103, 93, 86, 80, 75, 70, 65, 59, 53, 47, 40, 31, 23,
    15, 4, 0, 0, 0, 0, 126, 119, 112, 104, 95, 89, 83, 78, 72, 66, 60, 54, 47, 39, 32, 25, 17, 12,
    1, 0, 0, 134, 127, 120, 114, 103, 97, 91, 85, 78, 72, 66, 60, 54, 47, 41, 35, 29, 23, 16, 10,
    1, 144, 137, 130, 124, 113, 107, 101, 95, 88, 82, 76, 70, 64, 57, 51, 45, 39, 33, 26, 15, 1,
    152, 145, 138, 132, 123, 117, 111, 105, 98, 92, 86, 80, 74, 67, 61, 55, 49, 43, 36, 20, 1, 162,
    155, 148, 142, 133, 127, 121, 115, 108, 102, 96, 90, 84, 77, 71, 65, 59, 53, 46, 30, 1, 172,
    165, 158, 152, 143, 137, 131, 125, 118, 112, 106, 100, 94, 87, 81, 75, 69, 63, 56, 45, 20, 200,
    200, 200, 200, 200, 200, 200, 200, 198, 193, 188, 183, 178, 173, 168, 163, 158, 153, 148, 129,
    104,
];

/// RFC 6716 section 4.3.4 (mean band energies). eMeans — quant_bands.c (float branch; 25 entries, only the first 21 are used)
const E_MEANS: [f32; 25] = [
    6.437500f32,
    6.250000f32,
    5.750000f32,
    5.312500f32,
    5.062500f32,
    4.812500f32,
    4.500000f32,
    4.375000f32,
    4.875000f32,
    4.687500f32,
    4.562500f32,
    4.437500f32,
    4.875000f32,
    4.625000f32,
    4.312500f32,
    4.500000f32,
    4.375000f32,
    4.625000f32,
    4.750000f32,
    4.437500f32,
    3.750000f32,
    3.750000f32,
    3.750000f32,
    3.750000f32,
    3.750000f32,
];

/// RFC 6716 section 4.3.4 (energy prediction coefficients). pred_coef — quant_bands.c (float branch: 29440/32768., 26112/32768., 21248/32768., 16384/32768.)
const PRED_COEF: [f32; 4] = [0.8984375f32, 0.796875f32, 0.6484375f32, 0.5f32];

/// RFC 6716 section 4.3.4 (energy prediction damping). beta_coef — quant_bands.c (float branch: 30147/32768., 22282/32768., 12124/32768., 6554/32768.)
const BETA_COEF: [f32; 4] = [
    0.920013427734375f32,
    0.67999267578125f32,
    0.3699951171875f32,
    0.20001220703125f32,
];

/// RFC 6716 section 4.3.4 (intra-frame energy prediction damping). beta_intra — quant_bands.c (float branch: 4915/32768.)
const BETA_INTRA: f32 = 0.149993896484375f32;

/// RFC 6716 section 4.3.4 (Laplace model for coarse-energy deltas). e_prob_model — quant_bands.c (4 frame sizes x inter/intra x 21 bands x (p0, decay))
const E_PROB_MODEL: [[[u8; 42]; 2]; 4] = [
    [
        // 120-sample frames
        [
            72, 127, 65, 129, 66, 128, 65, 128, 64, 128, 62, 128, 64, 128, 64, 128, 92, 78, 92, 79,
            92, 78, 90, 79, 116, 41, 115, 40, 114, 40, 132, 26, 132, 26, 145, 17, 161, 12, 176, 10,
            177, 11,
        ], // Inter
        [
            24, 179, 48, 138, 54, 135, 54, 132, 53, 134, 56, 133, 55, 132, 55, 132, 61, 114, 70,
            96, 74, 88, 75, 88, 87, 74, 89, 66, 91, 67, 100, 59, 108, 50, 120, 40, 122, 37, 97, 43,
            78, 50,
        ], // Intra
    ],
    [
        // 240-sample frames
        [
            83, 78, 84, 81, 88, 75, 86, 74, 87, 71, 90, 73, 93, 74, 93, 74, 109, 40, 114, 36, 117,
            34, 117, 34, 143, 17, 145, 18, 146, 19, 162, 12, 165, 10, 178, 7, 189, 6, 190, 8, 177,
            9,
        ], // Inter
        [
            23, 178, 54, 115, 63, 102, 66, 98, 69, 99, 74, 89, 71, 91, 73, 91, 78, 89, 86, 80, 92,
            66, 93, 64, 102, 59, 103, 60, 104, 60, 117, 52, 123, 44, 138, 35, 133, 31, 97, 38, 77,
            45,
        ], // Intra
    ],
    [
        // 480-sample frames
        [
            61, 90, 93, 60, 105, 42, 107, 41, 110, 45, 116, 38, 113, 38, 112, 38, 124, 26, 132, 27,
            136, 19, 140, 20, 155, 14, 159, 16, 158, 18, 170, 13, 177, 10, 187, 8, 192, 6, 175, 9,
            159, 10,
        ], // Inter
        [
            21, 178, 59, 110, 71, 86, 75, 85, 84, 83, 91, 66, 88, 73, 87, 72, 92, 75, 98, 72, 105,
            58, 107, 54, 115, 52, 114, 55, 112, 56, 129, 51, 132, 40, 150, 33, 140, 29, 98, 35, 77,
            42,
        ], // Intra
    ],
    [
        // 960-sample frames
        [
            42, 121, 96, 66, 108, 43, 111, 40, 117, 44, 123, 32, 120, 36, 119, 33, 127, 33, 134,
            34, 139, 21, 147, 23, 152, 20, 158, 25, 154, 26, 166, 21, 173, 16, 184, 13, 184, 10,
            150, 13, 139, 15,
        ], // Inter
        [
            22, 178, 63, 114, 74, 82, 84, 83, 92, 82, 103, 62, 96, 72, 96, 67, 101, 73, 107, 72,
            113, 55, 118, 52, 125, 52, 118, 52, 117, 55, 135, 49, 137, 39, 157, 32, 145, 29, 97,
            33, 77, 40,
        ], // Intra
    ],
];

/// RFC 6716 section 4.3.4 (small-energy fallback CDF). small_energy_icdf — quant_bands.c
const SMALL_ENERGY_ICDF: [u8; 3] = [2, 1, 0];

/// RFC 6716 section 4.3.5 (allocation-trim CDF). trim_icdf — celt.c
const TRIM_ICDF: [u8; 11] = [126, 124, 119, 109, 87, 41, 19, 9, 4, 2, 0];

/// RFC 6716 section 4.3.6 (pulse-spreading CDF). spread_icdf — celt.c
const SPREAD_ICDF: [u8; 4] = [25, 23, 2, 0];

/// RFC 6716 section 4.3.1 (time-frequency resolution selection). tf_select_table — celt.c
const TF_SELECT_TABLE: [[i8; 8]; 4] = [
    [0, -1, 0, -1, 0, -1, 0, -1],
    [0, -1, 0, -2, 1, 0, 1, -1],
    [0, -2, 0, -3, 2, 0, 1, -1],
    [0, -2, 0, -3, 3, 0, 1, -1],
];

/// RFC 6716 section 4.3.7.1 (comb-filter tapset gains). comb-filter tapset gains — celt.c (QCONST16(x, 15) values; identity in float mode)
const COMB_GAINS: [[f32; 3]; 3] = [
    [0.3066406250f32, 0.2170410156f32, 0.1296386719f32],
    [0.4638671875f32, 0.2680664062f32, 0.0f32],
    [0.7998046875f32, 0.1000976562f32, 0.0f32],
];

/// RFC 6716 section 4.3.5 (fractional log2 for intensity/dual-stereo reservation). LOG2_FRAC_TABLE — rate.c
const LOG2_FRAC_TABLE: [u8; 24] = [
    0, 8, 13, 16, 19, 21, 23, 24, 26, 27, 28, 29, 30, 31, 32, 32, 33, 34, 34, 35, 36, 36, 37, 37,
];

/// RFC 6716 section 4.3.6 (Hadamard de/interleave order). ordery_table — bands.c (Hadamard de/interleave order, N=2,4,8,16)
const ORDERY_TABLE: [i32; 30] = [
    1, 0, 3, 0, 2, 1, 7, 0, 4, 3, 6, 1, 5, 2, 15, 0, 8, 7, 12, 3, 11, 4, 14, 1, 9, 6, 13, 2, 10, 5,
];

/// RFC 6716 section 4.3.6 (theta resolution in compute_qn). exp2_table8 — bands.c (compute_qn)
const EXP2_TABLE8: [i16; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];

/// RFC 6716 section 4.3.6 (multiplicative inverses of odd numbers mod 2^32, for CWRS division). INV_TABLE — cwrs.c (multiplicative inverses of odd numbers mod 2^32, for CWRS division)
const INV_TABLE: [u32; 53] = [
    0x00000001, 0xAAAAAAAB, 0xCCCCCCCD, 0xB6DB6DB7, 0x38E38E39, 0xBA2E8BA3, 0xC4EC4EC5, 0xEEEEEEEF,
    0xF0F0F0F1, 0x286BCA1B, 0x3CF3CF3D, 0xE9BD37A7, 0xC28F5C29, 0x684BDA13, 0x4F72C235, 0xBDEF7BDF,
    0x3E0F83E1, 0x8AF8AF8B, 0x914C1BAD, 0x96F96F97, 0xC18F9C19, 0x2FA0BE83, 0xA4FA4FA5, 0x677D46CF,
    0x1A1F58D1, 0xFAFAFAFB, 0x8C13521D, 0x586FB587, 0xB823EE09, 0xA08AD8F3, 0xC10C9715, 0xBEFBEFBF,
    0xC0FC0FC1, 0x07A44C6B, 0xA33F128D, 0xE327A977, 0xC7E3F1F9, 0x962FC963, 0x3F2B3885, 0x613716AF,
    0x781948B1, 0x2B2E43DB, 0xFCFCFCFD, 0x6FD0EB67, 0xFA3F47E9, 0xD2FD2FD3, 0x3F4FD3F5, 0xD4E25B9F,
    0x5F02A3A1, 0xBF5A814B, 0x7C32B16D, 0xD3431B57, 0xD8FD8FD9,
];

impl CeltDecoder {
    /// Decodes one CELT-only frame (RFC 6716 section 4.3,
    /// `celt_decode_with_ec`).
    ///
    /// `frame` is one CELT payload as returned by
    /// `parse_opus_packet` (the Opus frame bytes, without the TOC);
    /// `end_band` is the end band for the packet bandwidth (see
    /// `celt_end_band`); `frame_size` is the frame duration in
    /// samples at 48 kHz (120, 240, 480 or 960). Returns the
    /// interleaved `f32` samples (`frame_size * channels`), in
    /// [-1, 1].
    ///
    /// The decoder is stateful: call it once per frame, in stream
    /// order, on the same instance. A silence frame returns the
    /// previous frame's overlap tail followed by zeros, exactly as
    /// the reference does (the band energies are forced to zero,
    /// so the inverse MDCT contributes nothing new).
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a frame size other than
    /// 120/240/480/960, an end band outside 1..=21, or a channel
    /// count mismatch; `InvalidData` for a frame shorter than 2
    /// bytes, longer than 1275 bytes, or whose symbols do not
    /// decode (including raw bits running out).
    pub fn decode_frame(
        &mut self,
        frame: &[u8],
        end_band: usize,
        frame_size: usize,
    ) -> Result<Vec<f32>> {
        // Trust boundary: the reference rejects len > 1275, and a
        // frame under 2 bytes is a lost frame (PLC), not a CELT
        // frame; the caller handles lost frames separately.
        if frame.len() < 2 || frame.len() > 1275 {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "CELT frame length must be between 2 and 1275 bytes",
            });
        }
        let lm = match frame_size {
            120 => 0i32,
            240 => 1,
            480 => 2,
            960 => 3,
            _ => {
                return Err(Error::InvalidArgument(
                    "CELT frame size must be 120, 240, 480 or 960 samples",
                ));
            }
        };
        if end_band == 0 || end_band > EFF_EBANDS {
            return Err(Error::InvalidArgument(
                "CELT end band must be between 1 and 21",
            ));
        }
        let m = 1usize << lm;
        let n = m * SHORT_MDCT_SIZE;
        let c = self.channels;
        let start = 0usize;
        let eff_end = end_band.min(EFF_EBANDS);
        let total_bits = (frame.len() * 8) as u32;

        let mut dec = CeltRangeReader::new(frame);

        // Mono stream: keep the louder of the two stored band
        // energies, as the reference does before the header.
        if c == 1 {
            for i in 0..NB_EBANDS {
                self.old_band_e[0][i] = self.old_band_e[0][i].max(self.old_band_e[1][i]);
            }
        }

        let header = decode_celt_frame_header_from(&mut dec)?;
        let is_transient = header.transient;
        let short_blocks = if is_transient { m } else { 0 };

        // Post-filter parameters of this frame (section 4.3.7.1);
        // zero when the flag is clear.
        let (postfilter_pitch, postfilter_gain, postfilter_tapset) = match header.post_filter {
            Some(pf) => (pf.period as i32, pf.gain(), pf.tapset as i32),
            None => (0, 0.0, 0),
        };

        // Normalised MDCT coefficients (X) and band amplitudes
        // (bandE); both start at zero and are filled by the
        // bitstream decode, or stay zero for a silence frame.
        let mut x = vec![0.0f32; c * n];
        let mut band_e = [[0.0f32; NB_EBANDS]; 2];

        if header.silence {
            // Reference: the silence block forces every band
            // amplitude to zero and the stored energies to -28 dB.
            // The bitstream sections below are skipped; the
            // synthesis still runs, so the output is the previous
            // frame's overlap tail followed by zeros.
            for ch in 0..2 {
                for i in 0..NB_EBANDS {
                    self.old_band_e[ch][i] = -28.0;
                }
            }
        } else {
            // Coarse band energies (section 4.3.4).
            unquant_coarse_energy(
                start,
                end_band,
                &mut self.old_band_e,
                header.intra,
                &mut dec,
                total_bits,
                c,
                lm,
            )?;

            // Time-frequency resolution per band (section 4.3.1).
            let mut tf_res = [0i32; NB_EBANDS];
            tf_decode(
                start,
                end_band,
                is_transient,
                &mut tf_res,
                lm,
                &mut dec,
                total_bits,
            )?;

            // Pulse spreading (section 4.3.6).
            let mut spread = SPREAD_NORMAL;
            if dec.tell() + 4 <= total_bits {
                spread = dec.decode_icdf(&SPREAD_ICDF, 5)? as i32;
            }

            let cap = init_caps(lm, c);

            // Dynamic allocation (section 4.3.5): extra bits per
            // band, signalled with a decreasing log-probability.
            let mut offsets = [0i32; NB_EBANDS];
            let mut dynalloc_logp = 6i32;
            let mut total_frac = (total_bits as i32) << BITRES;
            let mut tell_frac = dec.tell_frac() as i32;
            for i in start..end_band {
                let width = ((c as i32) * (EBAND_5MS[i + 1] as i32 - EBAND_5MS[i] as i32)) << lm;
                let quanta = (width << BITRES).min((6 << BITRES).max(width));
                let mut loop_logp = dynalloc_logp;
                let mut boost = 0i32;
                while tell_frac + (loop_logp << BITRES) < total_frac && boost < cap[i] {
                    let flag = dec.decode_bit_logp(loop_logp as u32)?;
                    tell_frac = dec.tell_frac() as i32;
                    if !flag {
                        break;
                    }
                    boost += quanta;
                    total_frac -= quanta;
                    loop_logp = 1;
                }
                offsets[i] = boost;
                if boost > 0 {
                    dynalloc_logp = 2.max(dynalloc_logp - 1);
                }
            }

            // Allocation trim (section 4.3.5).
            let alloc_trim = if tell_frac + (6 << BITRES) <= total_frac {
                dec.decode_icdf(&TRIM_ICDF, 7)? as i32
            } else {
                5
            };

            // Bits left for PVQ + fine energy, in 1/8-bit units.
            let mut bits = (((frame.len() as i32) * 8) << BITRES) - dec.tell_frac() as i32 - 1;
            let anti_collapse_rsv = if is_transient && lm >= 2 && bits >= ((lm + 2) << BITRES) {
                1 << BITRES
            } else {
                0
            };
            bits -= anti_collapse_rsv;

            // Bit allocation (section 4.3.5).
            let mut pulses = [0i32; NB_EBANDS];
            let mut fine_quant = [0i32; NB_EBANDS];
            let mut fine_priority = [0i32; NB_EBANDS];
            let mut intensity = 0i32;
            let mut dual_stereo = 0i32;
            let mut balance = 0i32;
            let coded_bands = compute_allocation(
                start,
                end_band,
                &offsets,
                &cap,
                alloc_trim,
                &mut intensity,
                &mut dual_stereo,
                bits,
                &mut balance,
                &mut pulses,
                &mut fine_quant,
                &mut fine_priority,
                c,
                lm,
                &mut dec,
            )?;

            // Fine band energies (section 4.3.4).
            unquant_fine_energy(
                start,
                end_band,
                &mut self.old_band_e,
                &fine_quant,
                &mut dec,
                c,
            )?;

            // PVQ shape decode (section 4.3.6).
            let mut collapse_masks = [0u8; NB_EBANDS * 2];
            quant_all_bands(
                &mut x,
                &mut collapse_masks,
                start,
                end_band,
                &pulses,
                short_blocks,
                spread,
                dual_stereo,
                intensity,
                &tf_res,
                (frame.len() as i32) * (8 << BITRES) - anti_collapse_rsv,
                balance,
                &mut dec,
                lm,
                coded_bands,
                &mut self.rng,
                c,
                m,
                n,
            )?;

            let anti_collapse_on = if anti_collapse_rsv > 0 {
                dec.decode_bits(1)? != 0
            } else {
                false
            };

            // Remaining bits go to one more fine-energy pass.
            let bits_left = (frame.len() as i32) * 8 - dec.tell() as i32;
            unquant_energy_finalise(
                start,
                end_band,
                &mut self.old_band_e,
                &fine_quant,
                &fine_priority,
                bits_left,
                &mut dec,
                c,
            )?;

            if anti_collapse_on {
                anti_collapse(
                    &mut x,
                    &collapse_masks,
                    lm,
                    c,
                    n,
                    start,
                    end_band,
                    &self.old_band_e,
                    &self.old_log_e,
                    &self.old_log_e2,
                    &pulses,
                    self.rng,
                );
            }

            // Band amplitudes from the log energies (section 4.3.4).
            log2_amp(start, end_band, &mut band_e, &self.old_band_e, c);
        }

        // Synthesis: denormalise, overlap-add, post-filter,
        // de-emphasis. The denormalised spectrum is zero outside
        // [start, eff_end).
        let mut freq = vec![0.0f32; c * n];
        denormalise_bands(&x, &mut freq, &band_e, eff_end, c, m);
        for ch in 0..c {
            // Shift the decode buffer left by one frame.
            self.decode_mem[ch].copy_within(n..DECODE_BUFFER_SIZE, 0);
            for i in 0..m * EBAND_5MS[start] as usize {
                freq[ch * n + i] = 0.0;
            }
            let bound = m * EBAND_5MS[eff_end] as usize;
            for i in bound..n {
                freq[ch * n + i] = 0.0;
            }
        }

        // Inverse MDCTs into out_syn (decode_mem[2048 - N .. 2048]).
        compute_inv_mdcts(&freq, &mut self.decode_mem, c, short_blocks, lm, n)?;

        // Comb-filter post-filter (section 4.3.7.1). Periods are
        // clamped to the minimum, as the reference does per channel.
        self.postfilter_period = self.postfilter_period.max(COMBFILTER_MINPERIOD);
        self.postfilter_period_old = self.postfilter_period_old.max(COMBFILTER_MINPERIOD);
        let out_syn_start = DECODE_BUFFER_SIZE - n;
        for ch in 0..c {
            // The first call covers only the first short block
            // (`shortMdctSize`, not the whole frame): the second
            // call below covers the rest. Passing the whole frame
            // here would comb-filter the tail twice.
            comb_filter(
                &mut self.decode_mem[ch],
                out_syn_start,
                SHORT_MDCT_SIZE,
                self.postfilter_period_old,
                self.postfilter_period,
                self.postfilter_gain_old,
                self.postfilter_gain,
                self.postfilter_tapset_old,
                self.postfilter_tapset,
            );
            if lm != 0 {
                comb_filter(
                    &mut self.decode_mem[ch],
                    out_syn_start + SHORT_MDCT_SIZE,
                    n - SHORT_MDCT_SIZE,
                    self.postfilter_period,
                    postfilter_pitch,
                    self.postfilter_gain,
                    postfilter_gain,
                    self.postfilter_tapset,
                    postfilter_tapset,
                );
            }
        }
        self.postfilter_period_old = self.postfilter_period;
        self.postfilter_gain_old = self.postfilter_gain;
        self.postfilter_tapset_old = self.postfilter_tapset;
        self.postfilter_period = postfilter_pitch;
        self.postfilter_gain = postfilter_gain;
        self.postfilter_tapset = postfilter_tapset;
        if lm != 0 {
            self.postfilter_period_old = self.postfilter_period;
            self.postfilter_gain_old = self.postfilter_gain;
            self.postfilter_tapset_old = self.postfilter_tapset;
        }

        if c == 1 {
            for i in 0..NB_EBANDS {
                self.old_band_e[1][i] = self.old_band_e[0][i];
            }
        }

        // Log-energy history (section 4.3.4).
        if is_transient {
            for ch in 0..2 {
                for i in 0..NB_EBANDS {
                    self.old_log_e[ch][i] = self.old_log_e[ch][i].min(self.old_band_e[ch][i]);
                }
            }
        } else {
            self.old_log_e2 = self.old_log_e;
            self.old_log_e = self.old_band_e;
            for ch in 0..2 {
                for i in 0..NB_EBANDS {
                    self.background_log_e[ch][i] = (self.background_log_e[ch][i]
                        + (m as f32) * 0.001)
                        .min(self.old_band_e[ch][i]);
                }
            }
        }
        for ch in 0..2 {
            for i in 0..start {
                self.old_band_e[ch][i] = 0.0;
                self.old_log_e[ch][i] = -28.0;
                self.old_log_e2[ch][i] = -28.0;
            }
            for i in end_band..NB_EBANDS {
                self.old_band_e[ch][i] = 0.0;
                self.old_log_e[ch][i] = -28.0;
                self.old_log_e2[ch][i] = -28.0;
            }
        }

        // The reference carries the range decoder's `rng` into the
        // next frame as the noise seed (`st->rng = dec->rng`); the
        // reader tracked it exactly, so take it from there.
        self.rng = dec.rng();

        // De-emphasis (section 4.3.7.2) into interleaved PCM.
        let mut pcm = vec![0.0f32; n * c];
        deemphasis(
            &self.decode_mem,
            out_syn_start,
            n,
            c,
            &mut self.preemph_mem,
            &mut pcm,
        );

        // The reference reports an internal error when the decoder
        // consumed more bits than the frame holds.
        if dec.tell() > total_bits {
            return Err(Error::InvalidData {
                offset: frame.len(),
                detail: "CELT frame consumed more bits than it contains",
            });
        }
        Ok(pcm)
    }
}

// ---------------------------------------------------------------------------
// Band energies (RFC 6716 section 4.3.4, quant_bands.c)
// ---------------------------------------------------------------------------

/// LCG used for noise fill (bands.c `celt_lcg_rand`).
fn celt_lcg_rand(seed: u32) -> u32 {
    seed.wrapping_mul(1664525).wrapping_add(1013904223)
}

/// Laplace decode (laplace.c `ec_laplace_decode`): a symbol with a
/// two-sided geometric distribution, total frequency 32768. `fs`
/// is the probability of zero and `decay` the decay per step (both
/// taken from `e_prob_model`).
fn ec_laplace_decode(dec: &mut CeltRangeReader, fs: u32, decay: i32) -> Result<i32> {
    // LAPLACE_MINP = 1, LAPLACE_NMIN = 16, LAPLACE_LOG_MINP = 0, so
    // ec_laplace_get_freq1(fs, decay) = (32768 - 32 - fs) *
    // (16384 - decay) / 32768, and the tail step is 1.
    let fm = dec.decode(32768)?;
    let mut fl = 0u32;
    let mut fs = fs;
    let mut val = 0i32;
    if fm >= fs {
        val = 1;
        fl = fs;
        fs = (32768 - 32 - fs) * (16384 - decay as u32) / 32768 + 1;
        while fs > 1 && fm >= fl + 2 * fs {
            fs *= 2;
            fl += fs;
            fs = ((fs - 2) * decay as u32) / 32768 + 1;
            val += 1;
        }
        if fs <= 1 {
            let di = (fm - fl) >> 1;
            val += di as i32;
            fl += 2 * di;
        }
        if fm < fl + fs {
            val = -val;
        } else {
            fl += fs;
        }
    }
    let fh = (fl + fs).min(32768);
    dec.update(fl, fh, 32768)?;
    Ok(val)
}

/// Coarse energy decode (quant_bands.c `unquant_coarse_energy`).
/// Updates `old_band_e` in place: each band's log energy is
/// predicted from the previous frame and corrected by a Laplace
/// symbol. Float mode: `old = coef * old + prev + qi` and
/// `prev += qi - beta * qi`, with `old` clamped below at -9.
/// `total_bits` is the frame length in bits (the reference's
/// `dec->storage * 8`); the branches below compare the remaining
/// bits against 15/2/1.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::needless_range_loop)]
fn unquant_coarse_energy(
    start: usize,
    end: usize,
    old_band_e: &mut [[f32; NB_EBANDS]; 2],
    intra: bool,
    dec: &mut CeltRangeReader,
    total_bits: u32,
    channels: usize,
    lm: i32,
) -> Result<()> {
    let prob_model = &E_PROB_MODEL[lm as usize][usize::from(intra)];
    let (coef, beta) = if intra {
        (0.0f32, BETA_INTRA)
    } else {
        (PRED_COEF[lm as usize], BETA_COEF[lm as usize])
    };
    let mut prev = [0.0f32; 2];
    for i in start..end {
        for ch in 0..channels {
            // Remaining bits, signed like the reference: tell() is
            // an upper bound, so this can go negative on a corrupt
            // frame, which the last branch handles (qi = -1).
            let remaining = total_bits as i32 - dec.tell() as i32;
            let qi = if remaining >= 15 {
                let pi = 2 * i.min(20);
                ec_laplace_decode(
                    dec,
                    u32::from(prob_model[pi]) << 7,
                    i32::from(prob_model[pi + 1]) << 6,
                )?
            } else if remaining >= 2 {
                let s = dec.decode_icdf(&SMALL_ENERGY_ICDF, 2)? as i32;
                (s >> 1) ^ -(s & 1)
            } else if remaining >= 1 {
                -(dec.decode_bit_logp(1)? as i32)
            } else {
                -1
            };
            let q = qi as f32;
            let old = old_band_e[ch][i].max(-9.0);
            old_band_e[ch][i] = coef * old + prev[ch] + q;
            prev[ch] = prev[ch] + q - beta * q;
        }
    }
    Ok(())
}

/// Time-frequency resolution decode (celt.c `tf_decode`, section
/// 4.3.1). Each band gets a resolution change in -3..=3; the
/// per-band values are differentially coded, then mapped through
/// `TF_SELECT_TABLE` by frame size and transient flag.
/// `total_bits` is the frame length in bits.
fn tf_decode(
    start: usize,
    end: usize,
    is_transient: bool,
    tf_res: &mut [i32; NB_EBANDS],
    lm: i32,
    dec: &mut CeltRangeReader,
    total_bits: u32,
) -> Result<()> {
    let budget = total_bits;
    let mut tell = dec.tell();
    let mut logp = if is_transient { 2 } else { 4 };
    // Reserve space to code the tf_select decision.
    let tf_select_rsv = lm > 0 && tell + logp < budget;
    let budget = budget - u32::from(tf_select_rsv);
    let mut curr = 0i32;
    let mut tf_changed = 0i32;
    for tf in &mut tf_res[start..end] {
        if tell + logp <= budget {
            curr ^= dec.decode_bit_logp(logp)? as i32;
            tell = dec.tell();
            tf_changed |= curr;
        }
        *tf = curr;
        logp = if is_transient { 4 } else { 5 };
    }
    let mut tf_select = 0i32;
    if tf_select_rsv
        && TF_SELECT_TABLE[lm as usize][4 * usize::from(is_transient) + tf_changed as usize]
            != TF_SELECT_TABLE[lm as usize][4 * usize::from(is_transient) + 2 + tf_changed as usize]
    {
        tf_select = dec.decode_bit_logp(1)? as i32;
    }
    for i in start..end {
        tf_res[i] = i32::from(
            TF_SELECT_TABLE[lm as usize]
                [4 * usize::from(is_transient) + 2 * tf_select as usize + tf_res[i] as usize],
        );
    }
    Ok(())
}

/// Per-band usable-bits caps (celt.c `init_caps`, section 4.3.5):
/// `cap[i] = (cache_caps[(2*LM + C - 1) * 21 + i] + 64) * C * N / 4`,
/// where `N` is the band width in coefficients.
fn init_caps(lm: i32, channels: usize) -> [i32; NB_EBANDS] {
    let mut cap = [0i32; NB_EBANDS];
    for i in 0..NB_EBANDS {
        let n = (i32::from(EBAND_5MS[i + 1]) - i32::from(EBAND_5MS[i])) << lm;
        let row = 2 * lm + channels as i32 - 1;
        let idx = (row as usize) * NB_EBANDS + i;
        cap[i] = (i32::from(CACHE_CAPS_50[idx]) + 64) * (channels as i32) * n / 4;
    }
    cap
}

// ---------------------------------------------------------------------------
// Bit allocation (RFC 6716 section 4.3.5, rate.c)
// ---------------------------------------------------------------------------

/// Number of pulses for a pseudo-pulse index (rate.h
/// `get_pulses`): `i < 8 ? i : (8 + (i & 7)) << ((i >> 3) - 1)`.
fn get_pulses(i: i32) -> i32 {
    if i < 8 {
        i
    } else {
        (8 + (i & 7)) << ((i >> 3) - 1)
    }
}

/// Pulse-cache row for a band size `(LM + 1, band)` (rate.h
/// `cache_index50`, RFC 6716 section 4.3.5).
///
/// `lm` is -1 for the shortest blocks: the split in
/// `quant_band` decrements it, so the row `(lm + 1)` must be
/// computed in signed arithmetic before any cast to `usize`.
/// Casting `lm` first turns -1 into `usize::MAX`, and the
/// `+ 1` then overflows in debug builds (overflow checks on)
/// while silently wrapping to 0 in release. An out-of-range
/// row is `InvalidData`, never a panic, matching the rest of
/// this module.
///
/// # Errors
///
/// Returns `InvalidData` when `(lm + 1, band)` is outside
/// `CACHE_INDEX_50`.
fn pulse_cache_index(band: usize, lm: i32) -> Result<i16> {
    let out_of_range = || Error::InvalidData {
        offset: 0,
        detail: "CELT pulse cache index out of range",
    };
    let nb = i64::try_from(NB_EBANDS).map_err(|_| out_of_range())?;
    let band = i64::try_from(band).map_err(|_| out_of_range())?;
    let idx = (i64::from(lm) + 1)
        .checked_mul(nb)
        .and_then(|row| row.checked_add(band))
        .ok_or_else(out_of_range)?;
    let idx = usize::try_from(idx).map_err(|_| out_of_range())?;
    CACHE_INDEX_50.get(idx).copied().ok_or_else(out_of_range)
}

/// Pulses for a bit count (rate.h `bits2pulses`): binary search
/// in the pulse cache for the band size `(LM + 1, band)`.
fn bits2pulses(band: usize, lm: i32, bits: i32) -> Result<i32> {
    let cache_idx = pulse_cache_index(band, lm)?;
    if cache_idx < 0 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "CELT pulse cache has no entry for this band size",
        });
    }
    let cache = &CACHE_BITS_50[cache_idx as usize..];
    let cache_at = |i: i32| -> Result<i32> {
        cache
            .get(i as usize)
            .map(|&v| i32::from(v))
            .ok_or(Error::InvalidData {
                offset: 0,
                detail: "CELT pulse cache entry out of range",
            })
    };
    let mut lo = 0i32;
    let mut hi = cache_at(0)?;
    let bits = bits - 1;
    for _ in 0..6 {
        let mid = (lo + hi + 1) >> 1;
        if cache_at(mid)? >= bits {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let lo_bits = if lo == 0 { -1 } else { cache_at(lo)? };
    if bits - lo_bits <= cache_at(hi)? - bits {
        Ok(lo)
    } else {
        Ok(hi)
    }
}

/// Bits for a pulse count (rate.h `pulses2bits`).
fn pulses2bits(band: usize, lm: i32, pulses: i32) -> Result<i32> {
    if pulses == 0 {
        return Ok(0);
    }
    let cache_idx = pulse_cache_index(band, lm)?;
    if cache_idx < 0 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "CELT pulse cache has no entry for this band size",
        });
    }
    // A negative pulse count must not be cast to `usize`
    // (it would wrap to a huge index and the `+` could
    // overflow in debug builds); it is out of range.
    let pulses = usize::try_from(pulses).map_err(|_| Error::InvalidData {
        offset: 0,
        detail: "CELT pulse cache entry out of range",
    })?;
    let idx = (cache_idx as usize)
        .checked_add(pulses)
        .ok_or(Error::InvalidData {
            offset: 0,
            detail: "CELT pulse cache entry out of range",
        })?;
    CACHE_BITS_50
        .get(idx)
        .map(|&v| i32::from(v) + 1)
        .ok_or(Error::InvalidData {
            offset: 0,
            detail: "CELT pulse cache entry out of range",
        })
}

/// Bit allocation (rate.c `compute_allocation`): pure integer
/// arithmetic, ported exactly. Returns the number of coded bands
/// and fills `pulses` (PVQ bits per band, 1/8-bit units),
/// `fine_quant` (fine-energy bits per band), `fine_priority`,
/// `intensity`, `dual_stereo` and `balance`.
#[allow(clippy::too_many_arguments)]
fn compute_allocation(
    start: usize,
    end: usize,
    offsets: &[i32; NB_EBANDS],
    cap: &[i32; NB_EBANDS],
    alloc_trim: i32,
    intensity: &mut i32,
    dual_stereo: &mut i32,
    total: i32,
    balance: &mut i32,
    pulses: &mut [i32; NB_EBANDS],
    fine_quant: &mut [i32; NB_EBANDS],
    fine_priority: &mut [i32; NB_EBANDS],
    channels: usize,
    lm: i32,
    dec: &mut CeltRangeReader,
) -> Result<usize> {
    let mut total = total.max(0);
    let skip_start = start;
    // Reserve a bit to signal the end of manually skipped bands.
    let skip_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
    total -= skip_rsv;
    // Reserve bits for the intensity and dual-stereo parameters.
    let mut intensity_rsv = 0i32;
    let mut dual_stereo_rsv = 0i32;
    if channels == 2 {
        intensity_rsv = i32::from(LOG2_FRAC_TABLE[end - start]);
        if intensity_rsv > total {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_stereo_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
            total -= dual_stereo_rsv;
        }
    }

    let mut bits1 = [0i32; NB_EBANDS];
    let mut bits2 = [0i32; NB_EBANDS];
    let mut thresh = [0i32; NB_EBANDS];
    let mut trim_offset = [0i32; NB_EBANDS];
    for j in start..end {
        let n = i32::from(EBAND_5MS[j + 1]) - i32::from(EBAND_5MS[j]);
        thresh[j] = ((channels as i32) << BITRES).max((((3 * n) << lm) << BITRES) >> 4);
        // The C expression is (C*N*(alloc_trim-5-LM)*(end-j-1) *
        // (1<<(LM+BITRES))) >> 6; the shift binds last, and the
        // product can go negative (trim below 5), which Rust's
        // arithmetic >> on i32 matches.
        trim_offset[j] = ((channels as i32)
            * n
            * (alloc_trim - 5 - lm)
            * ((end - j - 1) as i32)
            * (1 << (lm + BITRES)))
            >> 6;
        if (n << lm) == 1 {
            trim_offset[j] -= (channels as i32) << BITRES;
        }
    }

    // Bisection over the allocation vectors.
    let mut lo = 1i32;
    let mut hi = NB_ALLOC_VECTORS as i32 - 1;
    while lo <= hi {
        let mid = (lo + hi) >> 1;
        let mut done = false;
        let mut psum = 0i32;
        for j in (start..end).rev() {
            let n = i32::from(EBAND_5MS[j + 1]) - i32::from(EBAND_5MS[j]);
            let mut bits_here = (((channels as i32)
                * n
                * i32::from(BAND_ALLOCATION[mid as usize * NB_EBANDS + j]))
                << lm)
                >> 2;
            if bits_here > 0 {
                bits_here = 0.max(bits_here + trim_offset[j]);
            }
            bits_here += offsets[j];
            if bits_here >= thresh[j] || done {
                done = true;
                psum += bits_here.min(cap[j]);
            } else if bits_here >= (channels as i32) << BITRES {
                psum += (channels as i32) << BITRES;
            }
        }
        if psum > total {
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
    }
    hi = lo;
    lo -= 1;

    let mut skip_start = skip_start;
    for j in start..end {
        let n = i32::from(EBAND_5MS[j + 1]) - i32::from(EBAND_5MS[j]);
        let mut bits1j =
            (((channels as i32) * n * i32::from(BAND_ALLOCATION[lo as usize * NB_EBANDS + j]))
                << lm)
                >> 2;
        let mut bits2j = if hi >= NB_ALLOC_VECTORS as i32 {
            cap[j]
        } else {
            (((channels as i32) * n * i32::from(BAND_ALLOCATION[hi as usize * NB_EBANDS + j]))
                << lm)
                >> 2
        };
        if bits1j > 0 {
            bits1j = 0.max(bits1j + trim_offset[j]);
        }
        if bits2j > 0 {
            bits2j = 0.max(bits2j + trim_offset[j]);
        }
        if lo > 0 {
            bits1j += offsets[j];
        }
        bits2j += offsets[j];
        if offsets[j] > 0 {
            skip_start = j;
        }
        bits2j = 0.max(bits2j - bits1j);
        bits1[j] = bits1j;
        bits2[j] = bits2j;
    }

    interp_bits2pulses(
        start,
        end,
        skip_start,
        &bits1,
        &bits2,
        &thresh,
        cap,
        total,
        balance,
        skip_rsv,
        intensity,
        intensity_rsv,
        dual_stereo,
        dual_stereo_rsv,
        pulses,
        fine_quant,
        fine_priority,
        channels,
        lm,
        dec,
    )
}

/// Interpolated pulse allocation (rate.c `interp_bits2pulses`):
/// bisects the interpolation factor, decides which high bands to
/// skip, codes intensity/dual-stereo, spreads the remaining bits,
/// and splits each band's bits into PVQ bits and fine-energy bits.
#[allow(clippy::too_many_arguments)]
fn interp_bits2pulses(
    start: usize,
    end: usize,
    skip_start: usize,
    bits1: &[i32; NB_EBANDS],
    bits2: &[i32; NB_EBANDS],
    thresh: &[i32; NB_EBANDS],
    cap: &[i32; NB_EBANDS],
    total: i32,
    balance: &mut i32,
    skip_rsv: i32,
    intensity: &mut i32,
    intensity_rsv: i32,
    dual_stereo: &mut i32,
    dual_stereo_rsv: i32,
    bits: &mut [i32; NB_EBANDS],
    ebits: &mut [i32; NB_EBANDS],
    fine_priority: &mut [i32; NB_EBANDS],
    channels: usize,
    lm: i32,
    dec: &mut CeltRangeReader,
) -> Result<usize> {
    let mut total = total;
    let alloc_floor = (channels as i32) << BITRES;
    let stereo = channels > 1;
    let log_m = lm << BITRES;

    let mut lo = 0i32;
    let mut hi = 1 << ALLOC_STEPS;
    for _ in 0..ALLOC_STEPS {
        let mid = (lo + hi) >> 1;
        let mut psum = 0i32;
        let mut done = false;
        for j in (start..end).rev() {
            let tmp = bits1[j] + ((mid * bits2[j]) >> ALLOC_STEPS);
            if tmp >= thresh[j] || done {
                done = true;
                psum += tmp.min(cap[j]);
            } else if tmp >= alloc_floor {
                psum += alloc_floor;
            }
        }
        if psum > total {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let mut psum = 0i32;
    let mut done = false;
    for j in (start..end).rev() {
        let mut tmp = bits1[j] + ((lo * bits2[j]) >> ALLOC_STEPS);
        if tmp < thresh[j] && !done {
            if tmp >= alloc_floor {
                tmp = alloc_floor;
            } else {
                tmp = 0;
            }
        } else {
            done = true;
        }
        tmp = tmp.min(cap[j]);
        bits[j] = tmp;
        psum += tmp;
    }

    // Decide which bands to skip, working backwards from the end.
    let mut coded_bands = end;
    let mut intensity_rsv = intensity_rsv;
    loop {
        let j = coded_bands - 1;
        if j <= skip_start {
            total += skip_rsv;
            break;
        }
        let left = total - psum;
        let width_total = i32::from(EBAND_5MS[coded_bands]) - i32::from(EBAND_5MS[start]);
        if width_total <= 0 {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "CELT band allocation has a non-positive width",
            });
        }
        let percoeff = left / width_total;
        let left = left - width_total * percoeff;
        let rem = (left - (i32::from(EBAND_5MS[j]) - i32::from(EBAND_5MS[start]))).max(0);
        let band_width = i32::from(EBAND_5MS[coded_bands]) - i32::from(EBAND_5MS[j]);
        let mut band_bits = bits[j] + percoeff * band_width + rem;
        if band_bits >= thresh[j].max(alloc_floor + (1 << BITRES)) {
            if dec.decode_bit_logp(1)? {
                break;
            }
            psum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }
        psum -= bits[j] + intensity_rsv;
        if intensity_rsv > 0 {
            intensity_rsv = i32::from(LOG2_FRAC_TABLE[j - start]);
        }
        psum += intensity_rsv;
        if band_bits >= alloc_floor {
            psum += alloc_floor;
            bits[j] = alloc_floor;
        } else {
            bits[j] = 0;
        }
        coded_bands -= 1;
    }

    if intensity_rsv > 0 {
        *intensity = start as i32 + dec.decode_uint((coded_bands + 1 - start) as u32)? as i32;
    } else {
        *intensity = 0;
    }
    let mut dual_stereo_rsv = dual_stereo_rsv;
    if *intensity <= start as i32 {
        total += dual_stereo_rsv;
        dual_stereo_rsv = 0;
    }
    if dual_stereo_rsv > 0 {
        *dual_stereo = dec.decode_bit_logp(1)? as i32;
    } else {
        *dual_stereo = 0;
    }

    // Allocate the remaining bits.
    let mut left = total - psum;
    let width_total = i32::from(EBAND_5MS[coded_bands]) - i32::from(EBAND_5MS[start]);
    if width_total <= 0 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "CELT band allocation has a non-positive width",
        });
    }
    let percoeff = left / width_total;
    left -= width_total * percoeff;
    for j in start..coded_bands {
        bits[j] += percoeff * (i32::from(EBAND_5MS[j + 1]) - i32::from(EBAND_5MS[j]));
    }
    for j in start..coded_bands {
        let tmp = left.min(i32::from(EBAND_5MS[j + 1]) - i32::from(EBAND_5MS[j]));
        bits[j] += tmp;
        left -= tmp;
    }

    let mut bal = 0i32;
    for j in start..coded_bands {
        let n0 = i32::from(EBAND_5MS[j + 1]) - i32::from(EBAND_5MS[j]);
        let n = n0 << lm;
        bits[j] += bal;
        if n > 1 {
            let mut excess = (bits[j] - cap[j]).max(0);
            bits[j] -= excess;
            let den = (channels as i32) * n
                + i32::from(channels == 2 && n > 2 && *dual_stereo == 0 && (j as i32) < *intensity);
            let nclogn = den * (i32::from(LOG_N_400[j]) + log_m);
            let mut offset = (nclogn >> 1) - den * FINE_OFFSET;
            if n == 2 {
                offset += den << BITRES >> 2;
            }
            if bits[j] + offset < ((den * 2) << BITRES) {
                offset += nclogn >> 2;
            } else if bits[j] + offset < ((den * 3) << BITRES) {
                offset += nclogn >> 3;
            }
            let mut ebit = (bits[j] + offset + (den << (BITRES - 1))) / (den << BITRES);
            ebit = ebit.max(0);
            if (channels as i32) * ebit > (bits[j] >> BITRES) {
                ebit = bits[j] >> usize::from(stereo) >> BITRES;
            }
            ebit = ebit.min(MAX_FINE_BITS);
            fine_priority[j] = i32::from(ebit * (den << BITRES) >= bits[j] + offset);
            bits[j] -= ((channels as i32) * ebit) << BITRES;
            ebits[j] = ebit;
            if excess > 0 {
                let extra_fine =
                    (excess >> (usize::from(stereo) + BITRES as usize)).min(MAX_FINE_BITS - ebit);
                ebits[j] += extra_fine;
                let extra_bits = (extra_fine * (channels as i32)) << BITRES;
                fine_priority[j] = i32::from(extra_bits >= excess - bal);
                excess -= extra_bits;
            }
            bal = excess;
        } else {
            let mut excess = (bits[j] - ((channels as i32) << BITRES)).max(0);
            bits[j] -= excess;
            ebits[j] = 0;
            fine_priority[j] = 1;
            if excess > 0 {
                let extra_fine =
                    (excess >> (usize::from(stereo) + BITRES as usize)).min(MAX_FINE_BITS);
                ebits[j] += extra_fine;
                let extra_bits = (extra_fine * (channels as i32)) << BITRES;
                fine_priority[j] = i32::from(extra_bits >= excess - bal);
                excess -= extra_bits;
            }
            bal = excess;
        }
    }
    *balance = bal;

    for j in coded_bands..end {
        ebits[j] = bits[j] >> usize::from(stereo) >> BITRES;
        bits[j] = 0;
        fine_priority[j] = i32::from(ebits[j] < 1);
    }
    Ok(coded_bands)
}

/// Fine energy decode (quant_bands.c `unquant_fine_energy`).
/// `offset = (q2 + 0.5) * 2^-fine_quant - 0.5`, added to the log
/// energy. Float mode: the reference's `(1 << (14 - q)) / 16384`
/// is exactly `2^-q`.
fn unquant_fine_energy(
    start: usize,
    end: usize,
    old_band_e: &mut [[f32; NB_EBANDS]; 2],
    fine_quant: &[i32; NB_EBANDS],
    dec: &mut CeltRangeReader,
    channels: usize,
) -> Result<()> {
    for i in start..end {
        if fine_quant[i] <= 0 {
            continue;
        }
        for old_e in &mut old_band_e[..channels] {
            let q2 = dec.decode_bits(fine_quant[i] as u32)? as i32;
            let offset = (q2 as f32 + 0.5) * 2f32.powi(-fine_quant[i]) - 0.5;
            old_e[i] += offset;
        }
    }
    Ok(())
}

/// Final fine-energy pass (quant_bands.c
/// `unquant_energy_finalise`): spends the remaining bits, one bit
/// per band per priority level, `offset = (q2 - 0.5) *
/// 2^(-fine_quant - 1)`.
#[allow(clippy::too_many_arguments)]
fn unquant_energy_finalise(
    start: usize,
    end: usize,
    old_band_e: &mut [[f32; NB_EBANDS]; 2],
    fine_quant: &[i32; NB_EBANDS],
    fine_priority: &[i32; NB_EBANDS],
    bits_left: i32,
    dec: &mut CeltRangeReader,
    channels: usize,
) -> Result<()> {
    let mut bits_left = bits_left;
    for prio in 0..2 {
        for i in start..end {
            if bits_left < channels as i32 {
                break;
            }
            if fine_quant[i] >= MAX_FINE_BITS || fine_priority[i] != prio {
                continue;
            }
            for old_e in &mut old_band_e[..channels] {
                let q2 = dec.decode_bits(1)? as i32;
                let offset = (q2 as f32 - 0.5) * 2f32.powi(-fine_quant[i] - 1);
                old_e[i] += offset;
                bits_left -= 1;
            }
        }
    }
    Ok(())
}

/// Band amplitudes from log energies (quant_bands.c `log2Amp`):
/// `bandE = 2^(oldE + eMeans[i])`, zero outside [start, end).
/// `celt_exp2` is the exact `exp2` here (no `FLOAT_APPROX`).
fn log2_amp(
    start: usize,
    end: usize,
    band_e: &mut [[f32; NB_EBANDS]; 2],
    old_band_e: &[[f32; NB_EBANDS]; 2],
    channels: usize,
) {
    for ch in 0..channels {
        band_e[ch][..start].fill(0.0);
        for (e, (old, mean)) in band_e[ch][start..end].iter_mut().zip(
            old_band_e[ch][start..end]
                .iter()
                .zip(E_MEANS[start..end].iter()),
        ) {
            *e = 2f32.powf(*old + *mean);
        }
        band_e[ch][end..].fill(0.0);
    }
}

// ---------------------------------------------------------------------------
// Combinatorial pulse coding (RFC 6716 section 4.3.6, cwrs.c)
// ---------------------------------------------------------------------------
//
// The PVQ codebook is indexed by combinations: `decode_pulses`
// turns a range-coded index into a signed pulse vector. All
// arithmetic here is wrapping `u32`, exactly like the reference's
// `opus_uint32` (the counts wrap past 2^32 for large vectors, and
// encoder and decoder agree because both wrap the same way).

/// Integer square root (mathops.c `isqrt32`): largest `g` with
/// `g*g <= val`, by the bit-by-bit method. `ilog` is `EC_ILOG`.
fn isqrt32(val: u32) -> u32 {
    if val == 0 {
        return 0;
    }
    let mut val = val;
    let mut g: u32 = 0;
    let mut bshift = (ilog(val) as i32 - 1) >> 1;
    let mut b: u32 = 1u32 << bshift;
    while bshift >= 0 {
        let t = g
            .wrapping_shl(1)
            .wrapping_add(b)
            .wrapping_shl(bshift as u32);
        if t <= val {
            g += b;
            val -= t;
        }
        b >>= 1;
        bshift -= 1;
    }
    g
}

/// `(a*b - c) / (2*d + 1)`, exact division via the inverse table
/// (cwrs.c `imusdiv32odd`). `INV_TABLE[d]` is the inverse of
/// `2*d + 1` modulo 2^32.
fn imusdiv32odd(a: u32, b: u32, c: u32, d: usize) -> Result<u32> {
    let inv = INV_TABLE.get(d).copied().ok_or(Error::InvalidData {
        offset: 0,
        detail: "CELT CWRS inverse table index out of range",
    })?;
    Ok(a.wrapping_mul(b).wrapping_sub(c).wrapping_mul(inv))
}

/// `(a*b - c) / d`, exact division (cwrs.c `imusdiv32even`).
/// `d` is small and the quotient is known to be exact.
fn imusdiv32even(a: u32, b: u32, c: u32, d: i32) -> Result<u32> {
    if d <= 0 || d > 54 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "CELT CWRS even divisor out of range",
        });
    }
    let d_u = d as u32;
    let mut shift = ilog(d_u ^ d_u.wrapping_sub(1)) as i32;
    let inv = INV_TABLE
        .get(((d_u - 1) >> shift) as usize)
        .copied()
        .ok_or(Error::InvalidData {
            offset: 0,
            detail: "CELT CWRS inverse table index out of range",
        })?;
    shift -= 1;
    let one: u32 = 1u32 << shift;
    let mask = one - 1;
    let shift_u = shift as u32;
    Ok(a.wrapping_mul(b >> shift_u)
        .wrapping_sub(c >> shift_u)
        .wrapping_add(
            (a.wrapping_mul(b & mask)
                .wrapping_add(one)
                .wrapping_sub(c & mask))
                >> shift_u,
        )
        .wrapping_sub(1)
        .wrapping_mul(inv))
}

/// `U(2, k)` (cwrs.c `ucwrs2`): `2*k - 1`, for `k >= 1`.
fn ucwrs2(k: u32) -> u32 {
    k.wrapping_add(k.wrapping_sub(1))
}

/// `V(2, k)` (cwrs.c `ncwrs2`): `4*k`, for `k >= 1`.
fn ncwrs2(k: i32) -> u32 {
    (4 * k) as u32
}

/// `U(3, k)` (cwrs.c `ucwrs3`): `(2*k - 2)*k + 1`, for `k >= 1`.
fn ucwrs3(k: u32) -> u32 {
    (2u32.wrapping_mul(k).wrapping_sub(2))
        .wrapping_mul(k)
        .wrapping_add(1)
}

/// `V(3, k)` (cwrs.c `ncwrs3`): `2*(2*k*k + 1)`, for `k >= 1`.
fn ncwrs3(k: i32) -> u32 {
    (2 * (2 * k * k + 1)) as u32
}

/// `U(4, k)` (cwrs.c `ucwrs4`), for `k >= 1`.
fn ucwrs4(k: i32) -> Result<u32> {
    if k < 1 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "CELT CWRS U(4, k) needs k >= 1",
        });
    }
    let ku = k as u32;
    imusdiv32odd(
        2u32.wrapping_mul(ku),
        (2u32.wrapping_mul(ku).wrapping_sub(3))
            .wrapping_mul(ku)
            .wrapping_add(4),
        3,
        1,
    )
}

/// `V(4, k)` (cwrs.c `ncwrs4`): `((k*k + 2)*k / 3) << 3`, for
/// `k >= 1`. The division is exact: `(k*k + 2)*k` is always a
/// multiple of 3.
fn ncwrs4(k: i32) -> u32 {
    let ku = k as u32;
    (((ku * ku + 2) * ku) / 3) << 3
}

/// Next row of the `U` recurrence `u[i][j] = u[i-1][j] +
/// u[i][j-1] + u[i-1][j-1]` (cwrs.c `unext`). Operates in place
/// on `ui[0..len]`; `len >= 2`.
fn unext(ui: &mut [u32], len: usize, ui0: u32) {
    let mut ui0 = ui0;
    let mut j = 1usize;
    loop {
        let ui1 = ui[j].wrapping_add(ui[j - 1]).wrapping_add(ui0);
        ui[j - 1] = ui0;
        ui0 = ui1;
        j += 1;
        if j >= len {
            break;
        }
    }
    ui[j - 1] = ui0;
}

/// Previous row of the `U` recurrence (cwrs.c `uprev`).
/// Operates in place on `ui[0..n]`; `n >= 2`.
fn uprev(ui: &mut [u32], n: usize, ui0: u32) {
    let mut ui0 = ui0;
    let mut j = 1usize;
    loop {
        let ui1 = ui[j].wrapping_sub(ui[j - 1]).wrapping_sub(ui0);
        ui[j - 1] = ui0;
        ui0 = ui1;
        j += 1;
        if j >= n {
            break;
        }
    }
    ui[j - 1] = ui0;
}

/// `V(n, k)` and the row `U(n, 0..=k+1)` (cwrs.c
/// `ncwrs_urow`). `u` has `k + 2` entries on entry; on exit
/// `u[i] = U(n, i)` and the return is `V(n, k) = u[k] + u[k+1]`.
/// `n >= 2`, `k >= 1`.
fn ncwrs_urow(n: u32, k: u32, u: &mut [u32]) -> Result<u32> {
    let len = (k + 2) as usize;
    u[0] = 0;
    u[1] = 1;
    if n <= 6 {
        let mut kk = 2u32;
        loop {
            u[kk as usize] = (kk << 1) - 1;
            kk += 1;
            if kk >= len as u32 {
                break;
            }
        }
        let mut kk = 2u32;
        while kk < n {
            unext(&mut u[1..], (k + 1) as usize, 1);
            kk += 1;
        }
    } else {
        let n2m1 = (n << 1) - 1;
        let mut um1 = n2m1;
        let mut um2: u32 = 1;
        u[2] = n2m1;
        let mut kk = 3u32;
        while kk < len as u32 {
            let v = imusdiv32even(n2m1, um1, um2, (kk - 1) as i32)?.wrapping_add(um2);
            u[kk as usize] = v;
            um2 = v;
            kk += 1;
            if kk >= len as u32 {
                break;
            }
            let v2 = imusdiv32odd(n2m1, um2, um1, ((kk - 1) >> 1) as usize)?.wrapping_add(um1);
            u[kk as usize] = v2;
            um1 = v2;
            kk += 1;
        }
    }
    Ok(u[k as usize].wrapping_add(u[k as usize + 1]))
}

/// The index-0 pulse vector of dimension 1 (cwrs.c `cwrsi1`):
/// `y[0] = k` or `-k` by the sign bit in `i`.
fn cwrsi1(k: i32, i: u32, y: &mut [i32]) {
    let s = (i as i32).wrapping_neg();
    y[0] = k.wrapping_add(s) ^ s;
}

/// Dimension 2 (cwrs.c `cwrsi2`).
fn cwrsi2(k: i32, mut i: u32, y: &mut [i32]) {
    let p = ucwrs2((k + 1) as u32);
    let s = if i >= p { -1i32 } else { 0 };
    i = i.wrapping_sub(p & (s as u32));
    let mut yj = k;
    let k2 = ((i.wrapping_add(1)) >> 1) as i32;
    let p2 = if k2 == 0 { 0 } else { ucwrs2(k2 as u32) };
    i = i.wrapping_sub(p2);
    yj -= k2;
    y[0] = yj.wrapping_add(s) ^ s;
    cwrsi1(k2, i, &mut y[1..]);
}

/// Dimension 3 (cwrs.c `cwrsi3`).
fn cwrsi3(k: i32, mut i: u32, y: &mut [i32]) {
    let p = ucwrs3((k + 1) as u32);
    let s = if i >= p { -1i32 } else { 0 };
    i = i.wrapping_sub(p & (s as u32));
    let mut yj = k;
    let k2 = if i > 0 {
        ((isqrt32(i.wrapping_mul(2).wrapping_sub(1)) + 1) >> 1) as i32
    } else {
        0
    };
    let p2 = if k2 == 0 { 0 } else { ucwrs3(k2 as u32) };
    i = i.wrapping_sub(p2);
    yj -= k2;
    y[0] = yj.wrapping_add(s) ^ s;
    cwrsi2(k2, i, &mut y[1..]);
}

/// Dimension 4 (cwrs.c `cwrsi4`): binary search on `U(4, K)`
/// for the largest `k` with `U(4, k) <= i`.
fn cwrsi4(k: i32, mut i: u32, y: &mut [i32]) -> Result<()> {
    let p = ucwrs4(k + 1)?;
    let s = if i >= p { -1i32 } else { 0 };
    i = i.wrapping_sub(p & (s as u32));
    let mut yj = k;
    let mut kl = 0i32;
    let mut kr = k;
    let mut k2: i32;
    let mut p2: u32;
    loop {
        k2 = (kl + kr) >> 1;
        p2 = if k2 <= 0 { 0 } else { ucwrs4(k2)? };
        match p2.cmp(&i) {
            std::cmp::Ordering::Less => {
                if k2 >= kr {
                    break;
                }
                kl = k2 + 1;
            }
            std::cmp::Ordering::Greater => {
                kr = k2 - 1;
            }
            std::cmp::Ordering::Equal => {
                break;
            }
        }
    }
    i = i.wrapping_sub(p2);
    yj -= k2;
    y[0] = yj.wrapping_add(s) ^ s;
    cwrsi3(k2, i, &mut y[1..]);
    Ok(())
}

/// General dimension (cwrs.c `cwrsi`). `u` holds row `n` of
/// `U` on entry (`k + 2` entries) and is destructively modified.
fn cwrsi(n: i32, mut k: i32, mut i: u32, y: &mut [i32], u: &mut [u32]) {
    let mut j = 0usize;
    loop {
        let p = u[(k + 1) as usize];
        let s = if i >= p { -1i32 } else { 0 };
        i = i.wrapping_sub(p & (s as u32));
        let mut yj = k;
        let mut p2 = u[k as usize];
        while p2 > i {
            k -= 1;
            p2 = u[k as usize];
        }
        i = i.wrapping_sub(p2);
        yj -= k;
        y[j] = yj.wrapping_add(s) ^ s;
        uprev(u, (k + 2) as usize, 0);
        j += 1;
        if j >= n as usize {
            break;
        }
    }
}

/// Pulse-vector decode (cwrs.c `decode_pulses`): reads one
/// uniform index in `[0, V(n, k))` and expands it into `y`
/// (length `n`), whose absolute values sum to `k`. `k` is
/// bounded by `MAX_PULSES`; a larger count is corrupt input.
fn decode_pulses(y: &mut [i32], n: usize, k: i32, dec: &mut CeltRangeReader) -> Result<()> {
    if k <= 0 || k > MAX_PULSES {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "CELT PVQ pulse count out of range",
        });
    }
    match n {
        2 => {
            let idx = dec.decode_uint(ncwrs2(k))?;
            cwrsi2(k, idx, y);
        }
        3 => {
            let idx = dec.decode_uint(ncwrs3(k))?;
            cwrsi3(k, idx, y);
        }
        4 => {
            let idx = dec.decode_uint(ncwrs4(k))?;
            cwrsi4(k, idx, y)?;
        }
        _ => {
            let mut u = vec![0u32; (k + 2) as usize];
            let total = ncwrs_urow(n as u32, k as u32, &mut u)?;
            let idx = dec.decode_uint(total)?;
            cwrsi(n as i32, k, idx, y, &mut u);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Pyramid vector quantisation (RFC 6716 section 4.3.6, vq.c)
// ---------------------------------------------------------------------------

/// One rotation step (vq.c `exp_rotation1`): rotates pairs
/// `(x[i], x[i+stride])` by `(c, s)`, forward then backward over
/// the vector, so the transform propagates sequentially. Float
/// mode: the `>> 15` shifts are identity, so this is a plain
/// rotation `x[i+stride] = c*x2 + s*x1`, `x[i] = c*x1 - s*x2`.
fn exp_rotation1(x: &mut [f32], len: usize, stride: usize, c: f32, s: f32) {
    let mut i = 0usize;
    while i + stride < len {
        let x1 = x[i];
        let x2 = x[i + stride];
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 - s * x2;
        i += 1;
    }
    if len > 2 * stride {
        let mut i = len - 2 * stride - 1;
        loop {
            let x1 = x[i];
            let x2 = x[i + stride];
            x[i + stride] = c * x2 + s * x1;
            x[i] = c * x1 - s * x2;
            if i == 0 {
                break;
            }
            i -= 1;
        }
    }
}

/// Spreading rotation (vq.c `exp_rotation`): spreads pulses
/// across the vector so a sparse pulse train does not collapse
/// onto a few coefficients. `dir < 0` undoes the rotation
/// (decode), `dir > 0` applies it (encode). No-op when
/// `2*K >= len` or spreading is off.
fn exp_rotation(x: &mut [f32], len: usize, dir: i32, stride: usize, k: i32, spread: i32) {
    const SPREAD_FACTOR: [i32; 3] = [15, 10, 5];
    if 2 * k >= len as i32 || spread == SPREAD_NONE {
        return;
    }
    let factor = SPREAD_FACTOR[(spread - 1) as usize];
    let gain = len as f32 / (len as f32 + factor as f32 * k as f32);
    let theta = 0.5 * gain * gain;
    let c = (0.5 * std::f32::consts::PI * theta).cos();
    let s = (0.5 * std::f32::consts::PI * (1.0 - theta)).cos();
    let mut stride2 = 0usize;
    if len >= 8 * stride {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }
    let sub_len = len / stride;
    for i in 0..stride {
        let chunk = &mut x[i * sub_len..(i + 1) * sub_len];
        if dir < 0 {
            if stride2 != 0 {
                exp_rotation1(chunk, sub_len, stride2, s, c);
            }
            exp_rotation1(chunk, sub_len, 1, c, s);
        } else {
            exp_rotation1(chunk, sub_len, 1, c, -s);
            if stride2 != 0 {
                exp_rotation1(chunk, sub_len, stride2, s, -c);
            }
        }
    }
}

/// Normalise a decoded pulse vector to unit energy times `gain`
/// (vq.c `normalise_residual`, float mode): `X[i] = gain *
/// iy[i] / sqrt(sum(iy^2))`. A zero residual (impossible for a
/// valid `K >= 1`) decodes to silence rather than infinities.
fn normalise_residual(iy: &[i32], x: &mut [f32], gain: f32) {
    let mut ryy: f32 = 0.0;
    for &v in iy {
        let v = v as f32;
        ryy += v * v;
    }
    if ryy <= 0.0 {
        for v in x.iter_mut() {
            *v = 0.0;
        }
        return;
    }
    let g = gain / ryy.sqrt();
    for (i, v) in x.iter_mut().enumerate() {
        *v = g * iy[i] as f32;
    }
}

/// Collapse mask of a pulse vector (vq.c
/// `extract_collapse_mask`): bit `i` is set when short block
/// `i` of the vector holds a pulse. `B <= 1` always sets bit 0.
fn extract_collapse_mask(iy: &[i32], b: i32) -> u32 {
    if b <= 1 {
        return 1;
    }
    let n0 = iy.len() / b as usize;
    let mut mask: u32 = 0;
    for i in 0..b as usize {
        for j in 0..n0 {
            if iy[i * n0 + j] != 0 {
                mask |= 1u32 << i;
            }
        }
    }
    mask
}

/// Renormalise a vector to unit energy times `gain` (vq.c
/// `renormalise_vector`, float mode): `X[i] = gain * X[i] /
/// sqrt(EPSILON + sum(X^2))`.
fn renormalise_vector(x: &mut [f32], gain: f32) {
    let mut e: f32 = EPSILON;
    for &v in x.iter() {
        e += v * v;
    }
    let g = gain / e.sqrt();
    for v in x.iter_mut() {
        *v *= g;
    }
}

/// PVQ decode of one vector (vq.c `alg_unquant`): reads the
/// pulses, normalises the residual, undoes the spreading
/// rotation, and returns the collapse mask. `x` has length `n`,
/// `k >= 1`, `n >= 2`.
fn alg_unquant(
    x: &mut [f32],
    n: usize,
    k: i32,
    spread: i32,
    b: i32,
    dec: &mut CeltRangeReader,
    gain: f32,
) -> Result<u32> {
    let mut iy = vec![0i32; n];
    decode_pulses(&mut iy, n, k, dec)?;
    normalise_residual(&iy, x, gain);
    exp_rotation(x, n, -1, b as usize, k, spread);
    Ok(extract_collapse_mask(&iy, b))
}

// ---------------------------------------------------------------------------
// Band quantisation helpers (RFC 6716 section 4.3.6, bands.c)
// ---------------------------------------------------------------------------

/// `(16384 + (i16)a * (i16)b) >> 15` (mathops.h `FRAC_MUL16`):
/// multiplies two 16-bit fractions, rounding. Both operands are
/// truncated to `i16` first, exactly as the C cast does.
fn frac_mul16(a: i32, b: i32) -> i32 {
    (16384 + (a as i16) as i32 * (b as i16) as i32) >> 15
}

/// Bit-exact cosine approximation (bands.c `bitexact_cos`):
/// `cos(pi/2 * x/16384)` in Q15, for `x` in `0..=16384`.
/// Bit-exactness matters because it feeds the bit allocation.
fn bitexact_cos(x: i16) -> i16 {
    let tmp = (4096 + (x as i32) * (x as i32)) >> 13;
    let mut x2 = tmp as i16;
    let inner1 = frac_mul16(-626, x2 as i32);
    let inner2 = frac_mul16(x2 as i32, 8277 + inner1);
    let inner3 = frac_mul16(x2 as i32, -7651 + inner2);
    x2 = (32767 - x2 as i32 + inner3) as i16;
    (1 + x2 as i32) as i16
}

/// Bit-exact `log2(tan)` (bands.c `bitexact_log2tan`), used for
/// the mid/side bit split. `isin`/`icos` are the Q15 sine and
/// cosine of the stereo angle, both positive here.
fn bitexact_log2tan(isin: i32, icos: i32) -> i32 {
    let lc = ilog(icos as u32) as i32;
    let ls = ilog(isin as u32) as i32;
    let icos = icos << (15 - lc);
    let isin = isin << (15 - ls);
    (ls - lc) * (1 << 11) + frac_mul16(isin, frac_mul16(isin, -2597) + 7932)
        - frac_mul16(icos, frac_mul16(icos, -2597) + 7932)
}

/// Theta resolution for a split (bands.c `compute_qn`): how
/// finely the mid/side (or time-split) angle is coded, from the
/// bit budget `b`, the `offset` and the `pulse_cap`. Returns 1
/// when there are (almost) no bits for the angle, which selects
/// intensity stereo for a stereo split.
fn compute_qn(n: i32, b: i32, offset: i32, pulse_cap: i32, stereo: bool) -> i32 {
    let mut n2 = 2 * n - 1;
    if stereo && n == 2 {
        n2 -= 1;
    }
    let mut qb = (b - pulse_cap - (4 << BITRES)).min((b + n2 * offset) / n2);
    qb = qb.min(8 << BITRES);
    if qb < (1 << BITRES >> 1) {
        1
    } else {
        let mut qn = i32::from(EXP2_TABLE8[(qb & 0x7) as usize]) >> (14 - (qb >> BITRES));
        qn = (qn + 1) >> 1 << 1;
        qn
    }
}

/// One Haar step (bands.c `haar1`): replaces each pair by its
/// sum and difference, scaled by `1/sqrt(2)`. `x` has length
/// `n0 * stride`.
fn haar1(x: &mut [f32], n0: usize, stride: usize) {
    const SQRT_HALF: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let n0 = n0 / 2;
    for i in 0..stride {
        for j in 0..n0 {
            let tmp1 = SQRT_HALF * x[stride * 2 * j + i];
            let tmp2 = SQRT_HALF * x[stride * (2 * j + 1) + i];
            x[stride * 2 * j + i] = tmp1 + tmp2;
            x[stride * (2 * j + 1) + i] = tmp1 - tmp2;
        }
    }
}

/// Reorder time-ordered blocks back to frequency order (bands.c
/// `deinterleave_hadamard`): `x[j*stride + i]` moves to
/// `tmp[ordery[i]*n0 + j]` when `hadamard` (a long block split
/// into short ones), else to `tmp[i*n0 + j]`. `x` has length
/// `n0 * stride`.
fn deinterleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) -> Result<()> {
    let n = n0 * stride;
    let mut tmp = vec![0.0f32; n];
    if hadamard {
        for i in 0..stride {
            let order = ORDERY_TABLE
                .get(stride - 2 + i)
                .copied()
                .ok_or(Error::InvalidData {
                    offset: 0,
                    detail: "CELT Hadamard order table index out of range",
                })? as usize;
            for j in 0..n0 {
                tmp[order * n0 + j] = x[j * stride + i];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[i * n0 + j] = x[j * stride + i];
            }
        }
    }
    x[..n].copy_from_slice(&tmp);
    Ok(())
}

/// Inverse of `deinterleave_hadamard` (bands.c
/// `interleave_hadamard`).
fn interleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) -> Result<()> {
    let n = n0 * stride;
    let mut tmp = vec![0.0f32; n];
    if hadamard {
        for i in 0..stride {
            let order = ORDERY_TABLE
                .get(stride - 2 + i)
                .copied()
                .ok_or(Error::InvalidData {
                    offset: 0,
                    detail: "CELT Hadamard order table index out of range",
                })? as usize;
            for j in 0..n0 {
                tmp[j * stride + i] = x[order * n0 + j];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[i * n0 + j];
            }
        }
    }
    x[..n].copy_from_slice(&tmp);
    Ok(())
}

/// Merge decoded mid/side back to left/right (bands.c
/// `stereo_merge`, float mode): the gains come from the decoded
/// mid/side norms and correlation, not from the band energies
/// (see the module docs on intensity stereo). `mid` is the Q15
/// mid gain as a float in `[0, 1]`. When either side's energy
/// is below `6e-4`, both channels take the mid.
fn stereo_merge(x: &mut [f32], y: &mut [f32], mid: f32) {
    let n = x.len();
    let mut xp = 0.0f32;
    let mut side = 0.0f32;
    for j in 0..n {
        xp += x[j] * y[j];
        side += y[j] * y[j];
    }
    xp *= mid;
    // `mid2 = SHR32(mid, 1)` is the identity in float mode.
    let el = mid * mid + side - 2.0 * xp;
    let er = mid * mid + side + 2.0 * xp;
    if er < 6e-4 || el < 6e-4 {
        y.copy_from_slice(x);
        return;
    }
    let lgain = 1.0 / el.sqrt();
    let rgain = 1.0 / er.sqrt();
    for j in 0..n {
        let l = mid * x[j];
        let r = y[j];
        x[j] = lgain * (l - r);
        y[j] = rgain * (l + r);
    }
}

/// Bit-interleave table for the collapse mask (bands.c
/// `quant_band`, recombine step).
const BIT_INTERLEAVE_TABLE: [i32; 16] = [0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3];
/// Bit-deinterleave table for the collapse mask (bands.c
/// `quant_band`, end of the level-0 block).
const BIT_DEINTERLEAVE_TABLE: [u32; 16] = [
    0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F, 0xC0, 0xC3, 0xCC, 0xCF, 0xF0, 0xF3, 0xFC, 0xFF,
];

/// One band, mono or stereo (bands.c `quant_band`, decode only).
/// Decodes the PVQ shape of a band of `n` coefficients in `x`
/// (and `y` for the side channel when stereo), recursing into
/// time/mid-side splits. Returns the collapse mask: bit `i` is
/// set when short block `i` holds a pulse. `lowband` is the
/// folding source (a slice of the `norm` array, or `None`);
/// `lowband_out` receives the scaled band for later folding.
/// `remaining_bits` and `seed` are shared across the recursion.
/// `bandE` is intentionally absent: in decode it is only used by
/// `intensity_stereo`, which is encode-only (see module docs).
#[allow(clippy::too_many_arguments)]
#[allow(clippy::cast_sign_loss)]
#[allow(unused_assignments)]
fn quant_band(
    band: usize,
    x: &mut [f32],
    y: Option<&mut [f32]>,
    n: usize,
    b: i32,
    spread: i32,
    b_blocks: i32,
    intensity: i32,
    tf_change: i32,
    lowband: Option<&mut [f32]>,
    dec: &mut CeltRangeReader,
    remaining_bits: &mut i32,
    lm: i32,
    lowband_out: Option<&mut [f32]>,
    level: i32,
    seed: &mut u32,
    gain: f32,
    fill: i32,
) -> Result<u32> {
    let stereo = y.is_some();
    let mut split = stereo;
    let n0 = n;
    let mut n_b = n / b_blocks as usize;
    let mut n_b0 = n_b;
    let mut b0 = b_blocks;
    let mut b_cur = b_blocks;
    let mut time_divide = 0i32;
    let mut recombine = 0i32;
    let mut inv = false;
    let mut mid = 0.0f32;
    let mut side = 0.0f32;
    let long_blocks = b0 == 1;
    let mut cm = 0u32;
    let mut b = b;
    let mut lm = lm;
    let mut n_cur = n;
    let mut fill = fill;
    let mut tf_change = tf_change;
    let mut lowband = lowband;
    let mut lowband_out = lowband_out;
    let mut y = y;

    // Special case for one sample (bands.c `quant_band`).
    if n == 1 {
        let mut sign_x = false;
        if *remaining_bits >= 1 << BITRES {
            sign_x = dec.decode_bits(1)? != 0;
            *remaining_bits -= 1 << BITRES;
        }
        x[0] = if sign_x { -NORM_SCALING } else { NORM_SCALING };
        if let Some(y) = y {
            let mut sign_y = false;
            if *remaining_bits >= 1 << BITRES {
                sign_y = dec.decode_bits(1)? != 0;
                *remaining_bits -= 1 << BITRES;
            }
            y[0] = if sign_y { -NORM_SCALING } else { NORM_SCALING };
        }
        if let Some(lowband_out) = lowband_out {
            lowband_out[0] = x[0];
        }
        return Ok(1);
    }

    // Time-frequency handling at the top level of a mono band.
    // The folding source is copied to a local buffer when it is
    // about to be modified in place, so the caller's `norm`
    // array is left alone (the reference copies into
    // `lowband_scratch`). In decode, `x` itself is not touched
    // here: the encoder's Haar/deinterleave is undone at the end.
    let mut lowband_owned: Vec<f32> = Vec::new();
    if !stereo && level == 0 {
        if tf_change > 0 {
            recombine = tf_change;
        }
        let need_copy =
            lowband.is_some() && (recombine != 0 || ((n_b & 1) == 0 && tf_change < 0) || b0 > 1);
        if need_copy {
            if let Some(lb) = lowband.as_deref() {
                lowband_owned = lb.to_vec();
            }
            lowband = Some(&mut lowband_owned[..]);
        }
        for k in 0..recombine {
            if let Some(lb) = lowband.as_deref_mut() {
                haar1(lb, n_cur >> k, 1 << k);
            }
            fill = BIT_INTERLEAVE_TABLE[(fill & 0xF) as usize]
                | BIT_INTERLEAVE_TABLE[((fill >> 4) & 0xF) as usize] << 2;
        }
        b_cur >>= recombine;
        n_b <<= recombine;
        while (n_b & 1) == 0 && tf_change < 0 {
            if let Some(lb) = lowband.as_deref_mut() {
                haar1(lb, n_b, b_cur as usize);
            }
            fill |= fill << b_cur;
            b_cur <<= 1;
            n_b >>= 1;
            time_divide += 1;
            tf_change += 1;
        }
        b0 = b_cur;
        n_b0 = n_b;
        if b0 > 1 {
            if let Some(lb) = lowband.as_deref_mut() {
                deinterleave_hadamard(
                    lb,
                    n_b >> recombine,
                    (b0 << recombine) as usize,
                    long_blocks,
                )?;
            }
        }
    }

    // Split the band in two when there are more bits than the
    // pulse cache can hold for this band size.
    let cache_idx = pulse_cache_index(band, lm)?;
    if cache_idx < 0 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "CELT pulse cache has no entry for this band size",
        });
    }
    let cache = &CACHE_BITS_50[cache_idx as usize..];
    let cache_top = cache.first().copied().ok_or(Error::InvalidData {
        offset: 0,
        detail: "CELT pulse cache entry out of range",
    })? as usize;
    let cache_max = cache.get(cache_top).copied().ok_or(Error::InvalidData {
        offset: 0,
        detail: "CELT pulse cache entry out of range",
    })? as i32;
    if !stereo && lm != -1 && b > cache_max + 12 && n_cur > 2 {
        n_cur >>= 1;
        split = true;
        lm -= 1;
        if b_cur == 1 {
            fill = (fill & 1) | (fill << 1);
        }
        b_cur = (b_cur + 1) >> 1;
    }

    if split {
        let pulse_cap = i32::from(LOG_N_400[band]) + lm * (1 << BITRES);
        let offset = (pulse_cap >> 1)
            - if stereo && n_cur == 2 {
                QTHETA_OFFSET_TWOPHASE
            } else {
                QTHETA_OFFSET
            };
        let mut qn = compute_qn(n_cur as i32, b, offset, pulse_cap, stereo);
        if stereo && band as i32 >= intensity {
            qn = 1;
        }
        let mut itheta = 0i32;
        let imid: i32;
        let iside: i32;
        let mut delta: i32;
        let tell = dec.tell_frac() as i32;
        if qn != 1 {
            if stereo && n_cur > 2 {
                // Step pdf for the stereo angle.
                let p0 = 3i32;
                let x0 = qn / 2;
                let ft = p0 * (x0 + 1) + x0;
                let fs = dec.decode(ft as u32)? as i32;
                let x = if fs < (x0 + 1) * p0 {
                    fs / p0
                } else {
                    x0 + 1 + (fs - (x0 + 1) * p0)
                };
                let (fl, fh) = if x <= x0 {
                    (p0 * x, p0 * (x + 1))
                } else {
                    ((x - 1 - x0) + (x0 + 1) * p0, (x - x0) + (x0 + 1) * p0)
                };
                dec.update(fl as u32, fh as u32, ft as u32)?;
                itheta = x;
            } else if b0 > 1 || stereo {
                // Uniform pdf.
                itheta = dec.decode_uint((qn + 1) as u32)? as i32;
            } else {
                // Triangular pdf.
                let ft = ((qn >> 1) + 1) * ((qn >> 1) + 1);
                let fm = dec.decode(ft as u32)? as i32;
                let (it, fs, fl);
                if fm < (((qn >> 1) * ((qn >> 1) + 1)) >> 1) {
                    it = (isqrt32(8u32.wrapping_mul(fm as u32).wrapping_add(1)) as i32 - 1) >> 1;
                    fs = it + 1;
                    fl = (it * (it + 1)) >> 1;
                } else {
                    it = (2 * (qn + 1)
                        - isqrt32(8u32.wrapping_mul((ft - fm - 1) as u32).wrapping_add(1)) as i32)
                        >> 1;
                    fs = qn + 1 - it;
                    fl = ft - (((qn + 1 - it) * (qn + 2 - it)) >> 1);
                }
                dec.update(fl as u32, (fl + fs) as u32, ft as u32)?;
                itheta = it;
            }
            itheta = itheta * 16384 / qn;
            // In decode, no `intensity_stereo` / `stereo_split`
            // call here: both are encode-only (see module docs).
        } else if stereo {
            if b > 2 << BITRES && *remaining_bits > 2 << BITRES {
                inv = dec.decode_bit_logp(2)?;
            } else {
                inv = false;
            }
            itheta = 0;
        }
        let qalloc = dec.tell_frac() as i32 - tell;
        b -= qalloc;
        let orig_fill = fill;
        if itheta == 0 {
            imid = 32767;
            iside = 0;
            fill &= (1 << b_cur) - 1;
            delta = -16384;
        } else if itheta == 16384 {
            imid = 0;
            iside = 32767;
            fill &= ((1 << b_cur) - 1) << b_cur;
            delta = 16384;
        } else {
            imid = i32::from(bitexact_cos(itheta as i16));
            iside = i32::from(bitexact_cos((16384 - itheta) as i16));
            delta = frac_mul16((n_cur as i32 - 1) << 7, bitexact_log2tan(iside, imid));
        }
        mid = (1.0 / 32768.0) * imid as f32;
        side = (1.0 / 32768.0) * iside as f32;

        if n_cur == 2 && stereo {
            // N=2 stereo special case: the side is derived from
            // the mid by a 90-degree rotation plus one sign bit.
            let mut mbits = b;
            let mut sbits = 0i32;
            if itheta != 0 && itheta != 16384 {
                sbits = 1 << BITRES;
            }
            mbits -= sbits;
            let c = itheta > 8192;
            *remaining_bits -= qalloc + sbits;
            let mut bit = false;
            if sbits != 0 {
                bit = dec.decode_bits(1)? != 0;
            }
            let sgn: f32 = if bit { -1.0 } else { 1.0 };
            let Some(y_side) = y.as_deref_mut() else {
                return Err(Error::InvalidData {
                    offset: 0,
                    detail: "CELT stereo split without a side channel",
                });
            };
            if c {
                cm = quant_band(
                    band,
                    &mut *y_side,
                    None,
                    n_cur,
                    mbits,
                    spread,
                    b_cur,
                    intensity,
                    tf_change,
                    lowband.as_deref_mut(),
                    dec,
                    remaining_bits,
                    lm,
                    lowband_out.as_deref_mut(),
                    level,
                    seed,
                    gain,
                    orig_fill,
                )?;
                let (y0, y1) = (y_side[0], y_side[1]);
                x[0] = -sgn * y1;
                x[1] = sgn * y0;
            } else {
                cm = quant_band(
                    band,
                    &mut *x,
                    None,
                    n_cur,
                    mbits,
                    spread,
                    b_cur,
                    intensity,
                    tf_change,
                    lowband.as_deref_mut(),
                    dec,
                    remaining_bits,
                    lm,
                    lowband_out.as_deref_mut(),
                    level,
                    seed,
                    gain,
                    orig_fill,
                )?;
                let (x0, x1) = (x[0], x[1]);
                y_side[0] = -sgn * x1;
                y_side[1] = sgn * x0;
            }
            x[0] *= mid;
            x[1] *= mid;
            y_side[0] *= side;
            y_side[1] *= side;
            let tmp = x[0];
            x[0] = tmp - y_side[0];
            y_side[0] += tmp;
            let tmp = x[1];
            x[1] = tmp - y_side[1];
            y_side[1] += tmp;
        } else {
            // Normal split: mid/side (stereo) or two halves (mono).
            // Give more bits to low-energy MDCTs than they would
            // otherwise deserve (mono only).
            if b0 > 1 && !stereo && (itheta & 0x3fff) != 0 {
                if itheta > 8192 {
                    delta -= delta >> (4 - lm);
                } else {
                    delta = (delta + (((n_cur as i32) << BITRES) >> (5 - lm))).min(0);
                }
            }
            let mut mbits = (b - delta) / 2;
            mbits = mbits.max(0).min(b);
            let mut sbits = b - mbits;
            *remaining_bits -= qalloc;
            let rebalance = *remaining_bits;
            if stereo {
                let Some(y_side) = y.as_deref_mut() else {
                    return Err(Error::InvalidData {
                        offset: 0,
                        detail: "CELT stereo split without a side channel",
                    });
                };
                if mbits >= sbits {
                    // Stereo mid is not scaled: the normalized
                    // mid is needed for folding later.
                    cm = quant_band(
                        band,
                        &mut *x,
                        None,
                        n_cur,
                        mbits,
                        spread,
                        b_cur,
                        intensity,
                        tf_change,
                        lowband.as_deref_mut(),
                        dec,
                        remaining_bits,
                        lm,
                        lowband_out.as_deref_mut(),
                        level,
                        seed,
                        Q15_ONE,
                        fill,
                    )?;
                    let reb = mbits - (rebalance - *remaining_bits);
                    if reb > 3 << BITRES && itheta != 0 {
                        sbits += reb - (3 << BITRES);
                    }
                    cm |= quant_band(
                        band,
                        &mut *y_side,
                        None,
                        n_cur,
                        sbits,
                        spread,
                        b_cur,
                        intensity,
                        tf_change,
                        None,
                        dec,
                        remaining_bits,
                        lm,
                        None,
                        level,
                        seed,
                        gain * side,
                        fill >> b_cur,
                    )?;
                } else {
                    cm = quant_band(
                        band,
                        &mut *y_side,
                        None,
                        n_cur,
                        sbits,
                        spread,
                        b_cur,
                        intensity,
                        tf_change,
                        None,
                        dec,
                        remaining_bits,
                        lm,
                        None,
                        level,
                        seed,
                        gain * side,
                        fill >> b_cur,
                    )?;
                    let reb = sbits - (rebalance - *remaining_bits);
                    if reb > 3 << BITRES && itheta != 16384 {
                        mbits += reb - (3 << BITRES);
                    }
                    cm |= quant_band(
                        band,
                        &mut *x,
                        None,
                        n_cur,
                        mbits,
                        spread,
                        b_cur,
                        intensity,
                        tf_change,
                        lowband.as_deref_mut(),
                        dec,
                        remaining_bits,
                        lm,
                        lowband_out.as_deref_mut(),
                        level,
                        seed,
                        Q15_ONE,
                        fill,
                    )?;
                }
            } else {
                let (x_first, x_second) = x.split_at_mut(n_cur);
                let (lb_first, lb_second) = match lowband.as_deref_mut() {
                    Some(lb) => {
                        let (a, b) = lb.split_at_mut(n_cur);
                        (Some(a), Some(b))
                    }
                    None => (None, None),
                };
                if mbits >= sbits {
                    cm = quant_band(
                        band,
                        x_first,
                        None,
                        n_cur,
                        mbits,
                        spread,
                        b_cur,
                        intensity,
                        tf_change,
                        lb_first,
                        dec,
                        remaining_bits,
                        lm,
                        None,
                        level + 1,
                        seed,
                        gain * mid,
                        fill,
                    )?;
                    let reb = mbits - (rebalance - *remaining_bits);
                    if reb > 3 << BITRES && itheta != 0 {
                        sbits += reb - (3 << BITRES);
                    }
                    cm |= quant_band(
                        band,
                        x_second,
                        None,
                        n_cur,
                        sbits,
                        spread,
                        b_cur,
                        intensity,
                        tf_change,
                        lb_second,
                        dec,
                        remaining_bits,
                        lm,
                        None,
                        level + 1,
                        seed,
                        gain * side,
                        fill >> b_cur,
                    )? << ((b0 >> 1) as u32);
                } else {
                    cm = quant_band(
                        band,
                        x_second,
                        None,
                        n_cur,
                        sbits,
                        spread,
                        b_cur,
                        intensity,
                        tf_change,
                        lb_second,
                        dec,
                        remaining_bits,
                        lm,
                        None,
                        level + 1,
                        seed,
                        gain * side,
                        fill >> b_cur,
                    )? << ((b0 >> 1) as u32);
                    let reb = sbits - (rebalance - *remaining_bits);
                    if reb > 3 << BITRES && itheta != 16384 {
                        mbits += reb - (3 << BITRES);
                    }
                    cm |= quant_band(
                        band,
                        x_first,
                        None,
                        n_cur,
                        mbits,
                        spread,
                        b_cur,
                        intensity,
                        tf_change,
                        lb_first,
                        dec,
                        remaining_bits,
                        lm,
                        None,
                        level + 1,
                        seed,
                        gain * mid,
                        fill,
                    )?;
                }
            }
        }
    } else {
        // Basic no-split case: plain PVQ on the whole band.
        let mut q = bits2pulses(band, lm, b)?;
        let mut curr_bits = pulses2bits(band, lm, q)?;
        *remaining_bits -= curr_bits;
        // Never bust the budget: reduce pulses until it fits.
        while *remaining_bits < 0 && q > 0 {
            *remaining_bits += curr_bits;
            q -= 1;
            curr_bits = pulses2bits(band, lm, q)?;
            *remaining_bits -= curr_bits;
        }
        if q != 0 {
            let k = get_pulses(q);
            cm = alg_unquant(x, n_cur, k, spread, b_cur, dec, gain)?;
        } else if let Some(lb) = lowband.as_deref() {
            // No pulses: fold the band from the folding source
            // with a small LCG dither (about 48 dB below the
            // normal folding level).
            let cm_mask = (1u32 << b_cur as u32) - 1;
            fill &= cm_mask as i32;
            if fill == 0 {
                for v in x.iter_mut().take(n_cur) {
                    *v = 0.0;
                }
            } else {
                for j in 0..n_cur {
                    *seed = celt_lcg_rand(*seed);
                    let tmp = if (*seed & 0x8000) != 0 {
                        1.0 / 256.0
                    } else {
                        -1.0 / 256.0
                    };
                    x[j] = lb[j] + tmp;
                }
                renormalise_vector(&mut x[..n_cur], gain);
                cm = fill as u32;
            }
        } else {
            // No pulses and no folding source: LCG noise.
            let cm_mask = (1u32 << b_cur as u32) - 1;
            fill &= cm_mask as i32;
            if fill == 0 {
                for v in x.iter_mut().take(n_cur) {
                    *v = 0.0;
                }
            } else {
                for xj in &mut x[..n_cur] {
                    *seed = celt_lcg_rand(*seed);
                    // The reference casts the seed to a signed
                    // int and shifts it right by 20, so the
                    // value is an integer in [-2048, 2047].
                    *xj = ((*seed as i32) >> 20) as f32;
                }
                renormalise_vector(&mut x[..n_cur], gain);
                cm = cm_mask;
            }
        }
    }

    // Resynthesis: merge mid/side back, undo the time-frequency
    // reorganisation for a mono top-level band, and scale the
    // output for later folding.
    if stereo {
        let Some(y_side) = y else {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "CELT stereo split without a side channel",
            });
        };
        if n_cur != 2 {
            stereo_merge(x, &mut *y_side, mid);
        }
        if inv {
            for v in y_side.iter_mut() {
                *v = -*v;
            }
        }
    } else if level == 0 {
        if b0 > 1 {
            interleave_hadamard(x, n_b >> recombine, (b0 << recombine) as usize, long_blocks)?;
        }
        let mut n_b = n_b0;
        let mut b = b0;
        for _ in 0..time_divide {
            b >>= 1;
            n_b <<= 1;
            cm |= cm >> b;
            haar1(x, n_b, b as usize);
        }
        for k in 0..recombine {
            // `cm` is a 4-bit interleaved mask here (the fill was
            // interleaved to 4 bits at the top), so the table
            // index is always in range; the mask is only defence
            // against a corrupt fill.
            cm = BIT_DEINTERLEAVE_TABLE[(cm & 0xF) as usize];
            haar1(x, n0 >> (k as usize), 1usize << (k as usize));
        }
        b <<= recombine;
        if let Some(lowband_out) = lowband_out {
            // Float mode: SHL32 is the identity, so
            // `n = sqrt(N0)` and MULT16_16_Q15 is a plain
            // multiply (arch.h).
            let n = (n0 as f32).sqrt();
            for j in 0..n0 {
                lowband_out[j] = n * x[j];
            }
        }
        cm &= (1 << b) - 1;
    }
    Ok(cm)
}

/// All bands of one frame (bands.c `quant_all_bands`, decode
/// only). `x` holds the normalised MDCT coefficients, channel 0
/// at `[0..n)`, channel 1 at `[n..2n)` when stereo.
/// All bands of one frame (bands.c `quant_all_bands`, decode
/// only). `x` holds the normalised MDCT coefficients, channel 0
/// at `[0..n)`, channel 1 at `[n..2n)` when stereo.
/// `collapse_masks` is filled per band (two entries per band for
/// stereo). `total_bits` is the frame size in 1/8-bit units.
/// `bandE` is intentionally absent: in decode it is only used by
/// `intensity_stereo`, which is encode-only (see module docs).
#[allow(clippy::too_many_arguments)]
#[allow(clippy::cast_sign_loss)]
fn quant_all_bands(
    x: &mut [f32],
    collapse_masks: &mut [u8; NB_EBANDS * 2],
    start: usize,
    end: usize,
    pulses: &[i32; NB_EBANDS],
    short_blocks: usize,
    spread: i32,
    dual_stereo: i32,
    intensity: i32,
    tf_res: &[i32; NB_EBANDS],
    total_bits: i32,
    balance: i32,
    dec: &mut CeltRangeReader,
    lm: i32,
    coded_bands: usize,
    seed: &mut u32,
    c: usize,
    m: usize,
    n: usize,
) -> Result<()> {
    let mut balance = balance;
    let mut dual_stereo = dual_stereo;
    let b_blocks = if short_blocks != 0 { m } else { 1 };
    // Normalised-band scratch: one `M*100` block per channel.
    let norm_len = m * EBAND_5MS[NB_EBANDS] as usize;
    let mut norm = vec![0.0f32; c * norm_len];
    // Split the coefficient buffer into channels once; the
    // per-band slices below are reborrows of these.
    let (x_ch0, mut x_ch1): (&mut [f32], Option<&mut [f32]>) = if c == 2 {
        let (a, b) = x.split_at_mut(n);
        (a, Some(b))
    } else {
        (x, None)
    };
    // Split the norm scratch into its per-channel blocks once.
    let (norm0, norm1) = norm.split_at_mut(norm_len);
    let mut lowband_offset = 0usize;
    let mut update_lowband = true;
    for i in start..end {
        let band_start = m * EBAND_5MS[i] as usize;
        let band_end = m * EBAND_5MS[i + 1] as usize;
        let band_n = band_end - band_start;
        let tell = dec.tell_frac() as i32;
        if i != start {
            balance -= tell;
        }
        let mut remaining_bits = total_bits - tell - 1;
        let b = if i < coded_bands {
            let curr_balance = balance / 3.min((coded_bands - i) as i32);
            0i32.max(16383.min((remaining_bits + 1).min(pulses[i] + curr_balance)))
        } else {
            0
        };
        if band_start >= band_n + m * EBAND_5MS[start] as usize
            && (update_lowband || lowband_offset == 0)
        {
            lowband_offset = i;
        }
        let tf_change = tf_res[i];
        // `i >= effEBands` would redirect the band into `norm`;
        // it cannot happen because `end <= EFF_EBANDS` (21) is
        // enforced by `decode_frame`.
        let mut effective_lowband: i32 = -1;
        let mut x_cm: u32;
        let mut y_cm: u32;
        if lowband_offset != 0 && (spread != SPREAD_AGGRESSIVE || b_blocks > 1 || tf_change < 0) {
            // Conservative collapse-mask estimate for the bands
            // the folding source comes from. This never repeats
            // spectral content within one band.
            // Signed arithmetic in the reference
            // (`IMAX(M*eBands[start], M*eBands[lowband_offset]-N)`):
            // the second term can go negative when the folding
            // source band is narrower than this band, so the
            // unsigned subtraction must saturate instead of
            // underflowing (debug panic) or wrapping (release).
            effective_lowband = (m * EBAND_5MS[start] as usize)
                .max((m * EBAND_5MS[lowband_offset] as usize).saturating_sub(band_n))
                as i32;
            // `while (M*eBands[--fold_start] > effective_lowband);`
            // Decrement first, stop at the first band whose end
            // is at or below the folding source.
            let mut fold_start = lowband_offset;
            while fold_start > 0 {
                fold_start -= 1;
                if m * (EBAND_5MS[fold_start] as usize) <= (effective_lowband as usize) {
                    break;
                }
            }
            // `while (M*eBands[++fold_end] < effective_lowband+N);`
            // Increment first, stop at the first band whose start
            // is at or past the end of the folding source.
            let mut fold_end = lowband_offset;
            while fold_end <= NB_EBANDS
                && m * (EBAND_5MS[fold_end] as usize) < ((effective_lowband as usize) + band_n)
            {
                fold_end += 1;
            }
            x_cm = 0;
            y_cm = 0;
            let mut fold_i = fold_start;
            while fold_i < fold_end {
                x_cm |= u32::from(collapse_masks[fold_i * c]);
                y_cm |= u32::from(collapse_masks[fold_i * c + c - 1]);
                fold_i += 1;
            }
        } else {
            // Otherwise the LCG is used for folding, so all blocks
            // will (almost always) be non-zero.
            x_cm = (1u32 << b_blocks as u32) - 1;
            y_cm = x_cm;
        }

        if dual_stereo != 0 && (i as i32) == intensity {
            // Switch off dual stereo to do intensity.
            dual_stereo = 0;
            for j in (m * EBAND_5MS[start] as usize)..band_start {
                norm0[j] = 0.5 * (norm0[j] + norm1[j]);
            }
        }

        if dual_stereo != 0 {
            // Two mono bands, one per channel, each with half
            // the bits.
            let x_band = &mut x_ch0[band_start..band_end];
            let y_band = match x_ch1.as_deref_mut() {
                Some(y) => &mut y[band_start..band_end],
                None => {
                    return Err(Error::InvalidData {
                        offset: 0,
                        detail: "CELT dual stereo without a second channel",
                    });
                }
            };
            let (lb0, out0) = norm_band_slices(&mut *norm0, effective_lowband, band_start, band_n);
            let (lb1, out1) = norm_band_slices(&mut *norm1, effective_lowband, band_start, band_n);
            x_cm = quant_band(
                i,
                x_band,
                None,
                band_n,
                b / 2,
                spread,
                b_blocks as i32,
                intensity,
                tf_change,
                lb0,
                dec,
                &mut remaining_bits,
                lm,
                out0,
                0,
                seed,
                Q15_ONE,
                x_cm as i32,
            )?;
            y_cm = quant_band(
                i,
                y_band,
                None,
                band_n,
                b / 2,
                spread,
                b_blocks as i32,
                intensity,
                tf_change,
                lb1,
                dec,
                &mut remaining_bits,
                lm,
                out1,
                0,
                seed,
                Q15_ONE,
                y_cm as i32,
            )?;
        } else {
            let x_band = &mut x_ch0[band_start..band_end];
            let (lb, out) = norm_band_slices(&mut *norm0, effective_lowband, band_start, band_n);
            let fill = (x_cm | y_cm) as i32;
            if let Some(y_slice) = x_ch1.as_deref_mut() {
                let y_band = &mut y_slice[band_start..band_end];
                x_cm = quant_band(
                    i,
                    x_band,
                    Some(y_band),
                    band_n,
                    b,
                    spread,
                    b_blocks as i32,
                    intensity,
                    tf_change,
                    lb,
                    dec,
                    &mut remaining_bits,
                    lm,
                    out,
                    0,
                    seed,
                    Q15_ONE,
                    fill,
                )?;
                y_cm = x_cm;
            } else {
                x_cm = quant_band(
                    i,
                    x_band,
                    None,
                    band_n,
                    b,
                    spread,
                    b_blocks as i32,
                    intensity,
                    tf_change,
                    lb,
                    dec,
                    &mut remaining_bits,
                    lm,
                    out,
                    0,
                    seed,
                    Q15_ONE,
                    fill,
                )?;
                y_cm = x_cm;
            }
        }
        collapse_masks[i * c] = x_cm as u8;
        collapse_masks[i * c + c - 1] = y_cm as u8;
        balance += pulses[i] + tell;
        // Update the folding position only as long as there is
        // 1 bit/sample depth.
        update_lowband = b > (band_n as i32) << BITRES;
    }
    Ok(())
}

/// Split one channel's `norm` scratch into the folding source
/// (`lowband`, `effective_lowband..effective_lowband + band_n`)
/// and the output slot for this band (`lowband_out`,
/// `band_start..band_start + band_n`). The two are disjoint
/// because the folding source always ends at or before the
/// band starts; `None` when there is no folding source.
fn norm_band_slices(
    norm_ch: &mut [f32],
    effective_lowband: i32,
    band_start: usize,
    band_n: usize,
) -> (Option<&mut [f32]>, Option<&mut [f32]>) {
    let (lo, hi) = norm_ch.split_at_mut(band_start);
    let lowband = if effective_lowband >= 0 {
        let lb = effective_lowband as usize;
        Some(&mut lo[lb..lb + band_n])
    } else {
        None
    };
    let lowband_out = Some(&mut hi[..band_n]);
    (lowband, lowband_out)
}
fn denormalise_bands(
    x: &[f32],
    freq: &mut [f32],
    band_e: &[[f32; NB_EBANDS]; 2],
    end: usize,
    c: usize,
    m: usize,
) {
    let n = m * SHORT_MDCT_SIZE;
    for ch in 0..c {
        for i in 0..end {
            let g = band_e[ch][i];
            let j0 = m * EBAND_5MS[i] as usize;
            let j1 = m * EBAND_5MS[i + 1] as usize;
            for j in j0..j1 {
                freq[ch * n + j] = x[ch * n + j] * g;
            }
        }
        for j in (m * EBAND_5MS[end] as usize)..n {
            freq[ch * n + j] = 0.0;
        }
    }
}

/// Prevent energy collapse for transients with multiple short
/// MDCTs (bands.c `anti_collapse`, float branch). The seed is
/// passed by value, as in the reference: the noise it generates
/// here does not propagate to the next frame.
#[allow(clippy::cast_sign_loss)]
#[allow(clippy::too_many_arguments)]
fn anti_collapse(
    x: &mut [f32],
    collapse_masks: &[u8; NB_EBANDS * 2],
    lm: i32,
    c: usize,
    n: usize,
    start: usize,
    end: usize,
    log_e: &[[f32; NB_EBANDS]; 2],
    prev1_log_e: &[[f32; NB_EBANDS]; 2],
    prev2_log_e: &[[f32; NB_EBANDS]; 2],
    pulses: &[i32; NB_EBANDS],
    seed: u32,
) {
    let mut seed = seed;
    for i in start..end {
        let n0 = (EBAND_5MS[i + 1] - EBAND_5MS[i]) as usize;
        // Depth in 1/8 bits (integer division, as in C).
        let depth = (1 + pulses[i]) / (((EBAND_5MS[i + 1] - EBAND_5MS[i]) as i32) << lm);
        let thresh = 0.5 * 2.0f32.powf(-0.125 * depth as f32);
        let sqrt_1 = 1.0 / ((n0 << lm) as f32).sqrt();
        for ch in 0..c {
            let mut prev1 = prev1_log_e[ch][i];
            let mut prev2 = prev2_log_e[ch][i];
            if c == 1 {
                // Mono: keep the louder of the two stored histories.
                prev1 = prev1.max(prev1_log_e[1][i]);
                prev2 = prev2.max(prev2_log_e[1][i]);
            }
            let mut ediff = log_e[ch][i] - prev1.min(prev2);
            if ediff < 0.0 {
                ediff = 0.0;
            }
            // Short blocks do not have the same energy as long
            // ones, so `r` is multiplied by 2 (or 2*sqrt(2) for
            // LM==3).
            let mut r = 2.0 * 2.0f32.powf(-ediff);
            if lm == 3 {
                r *= std::f32::consts::SQRT_2;
            }
            r = thresh.min(r);
            r *= sqrt_1;
            let band_start = ch * n + ((EBAND_5MS[i] as usize) << lm);
            let band_len = n0 << lm;
            let mut renormalize = false;
            for k in 0..(1usize << lm) {
                if collapse_masks[i * c + ch] & (1u8 << k) == 0 {
                    for j in 0..n0 {
                        seed = celt_lcg_rand(seed);
                        x[band_start + (j << lm) + k] = if seed & 0x8000 != 0 { r } else { -r };
                    }
                    renormalize = true;
                }
            }
            if renormalize {
                renormalise_vector(&mut x[band_start..band_start + band_len], Q15_ONE);
            }
        }
    }
}

/// One inverse MDCT block (celt.c `clt_mdct_backward`, via
/// `mdct_inverse`). `mdct_inverse` with an all-ones window
/// returns the direct cosine sum `y` (length `2*S`); the CELT
/// half-IMDCT is the middle `S` samples, `f2[j] = y[j+S/2]`,
/// and the mirror below rebuilds the `S + OVERLAP` time-domain
/// samples. The first `OVERLAP` samples are added onto what is
/// already in `out` (the overlap-add between short blocks); the
/// rest overwrite. No pre-scale: the note in `mdct.rs` claiming
/// CELT needs a 1/2 pre-scale is wrong for the backward
/// direction (verified against the reference: scale 1.0).
fn imdct_block(input: &[f32], out: &mut [f32], s: usize) -> Result<()> {
    let mut y = vec![0.0f32; 2 * s];
    let ones = vec![1.0f32; 2 * s];
    mdct_inverse(input, &mut y, &ones)?;
    let h = OVERLAP / 2;
    let ov = OVERLAP;
    for j in 0..h {
        out[j] += -CELT_WINDOW_120[j] * y[s / 2 + (h - 1 - j)];
    }
    for j in 0..h {
        out[h + j] += CELT_WINDOW_120[h + j] * y[s / 2 + j];
    }
    for j in ov..s {
        out[j] = y[s / 2 + (j - ov / 2)];
    }
    for j in 0..h {
        out[s + j] = CELT_WINDOW_120[ov - 1 - j] * y[s / 2 + (s - h + j)];
    }
    for j in 0..h {
        out[s + h + j] = CELT_WINDOW_120[h - 1 - j] * y[s / 2 + (s - 1 - j)];
    }
    Ok(())
}

/// Compute the inverse MDCTs and overlap-add into `out_syn`
/// (celt.c `compute_inv_mdcts`). `decode_mem[ch]` is
/// `[0..2048)` data plus `[2048..2168)` overlap memory;
/// `out_syn` is `[2048-n..2048)`. The scratch `x` is fresh
/// per call and only its first `OVERLAP` samples are cleared
/// per channel, as in the reference.
fn compute_inv_mdcts(
    freq: &[f32],
    decode_mem: &mut [[f32; DECODE_BUFFER_SIZE + OVERLAP]; 2],
    c: usize,
    short_blocks: usize,
    lm: i32,
    n: usize,
) -> Result<()> {
    let _ = lm;
    let overlap = OVERLAP;
    let mut x = vec![0.0f32; n + overlap];
    for ch in 0..c {
        let (n2, b_count) = if short_blocks != 0 {
            (SHORT_MDCT_SIZE, short_blocks)
        } else {
            (n, 1)
        };
        for v in x.iter_mut().take(overlap) {
            *v = 0.0;
        }
        for b in 0..b_count {
            // Short blocks read the interleaved spectrum with
            // stride `B`; a long block reads it contiguously.
            let input: Vec<f32> = if short_blocks != 0 {
                (0..n2).map(|k| freq[ch * n + b + k * b_count]).collect()
            } else {
                freq[ch * n..ch * n + n].to_vec()
            };
            let out_start = n2 * b;
            imdct_block(&input, &mut x[out_start..out_start + n2 + overlap], n2)?;
        }
        let (head, tail) = decode_mem[ch].split_at_mut(DECODE_BUFFER_SIZE);
        let out_syn = &mut head[DECODE_BUFFER_SIZE - n..DECODE_BUFFER_SIZE];
        let overlap_mem = &mut tail[..overlap];
        for j in 0..overlap {
            out_syn[j] = x[j] + overlap_mem[j];
        }
        out_syn[overlap..n].copy_from_slice(&x[overlap..n]);
        overlap_mem[..overlap].copy_from_slice(&x[n..n + overlap]);
    }
    Ok(())
}

/// Read a history sample for the comb filter, with a 0.0
/// fallback outside the buffer (periods are bounded, so this
/// only fires on corrupt input).
fn hist(row: &[f32], idx: isize) -> f32 {
    if idx < 0 {
        0.0
    } else {
        row.get(idx as usize).copied().unwrap_or(0.0)
    }
}

/// Comb-filter post-filter (celt.c `comb_filter`, RFC 6716
/// section 4.3.7.1). Operates in place on one `decode_mem`
/// row; `out_syn_start` is where this frame's synthesis
/// starts. Reads at already-written positions are intentional
/// (the filter feeds forward). The RFC version has no early
/// return for zero gains and no zero-overlap optimisation
/// (those are later libopus additions).
#[allow(clippy::cast_sign_loss)]
#[allow(clippy::too_many_arguments)]
fn comb_filter(
    row: &mut [f32; DECODE_BUFFER_SIZE + OVERLAP],
    out_syn_start: usize,
    n: usize,
    period_old: i32,
    period: i32,
    gain_old: f32,
    gain: f32,
    tapset_old: i32,
    tapset: i32,
) {
    // Tapset indices come from the header (0..=2); clamp rather
    // than panic on corrupt input.
    let t0 = (tapset_old as usize).min(COMB_GAINS.len() - 1);
    let t1 = (tapset as usize).min(COMB_GAINS.len() - 1);
    let g00 = gain_old * COMB_GAINS[t0][0];
    let g01 = gain_old * COMB_GAINS[t0][1];
    let g02 = gain_old * COMB_GAINS[t0][2];
    let g10 = gain * COMB_GAINS[t1][0];
    let g11 = gain * COMB_GAINS[t1][1];
    let g12 = gain * COMB_GAINS[t1][2];
    let overlap = OVERLAP;
    let t0 = period_old as isize;
    let t1 = period as isize;
    for (i, w) in CELT_WINDOW_120.iter().enumerate().take(overlap) {
        // The first `overlap` samples blend the old and new
        // gains with `f = window[i]^2`.
        let f = w * w;
        let idx = out_syn_start + i;
        let v = row[idx]
            + (1.0 - f) * g00 * hist(row, idx as isize - t0)
            + (1.0 - f) * g01 * hist(row, idx as isize - t0 - 1)
            + (1.0 - f) * g01 * hist(row, idx as isize - t0 + 1)
            + (1.0 - f) * g02 * hist(row, idx as isize - t0 - 2)
            + (1.0 - f) * g02 * hist(row, idx as isize - t0 + 2)
            + f * g10 * hist(row, idx as isize - t1)
            + f * g11 * hist(row, idx as isize - t1 - 1)
            + f * g11 * hist(row, idx as isize - t1 + 1)
            + f * g12 * hist(row, idx as isize - t1 - 2)
            + f * g12 * hist(row, idx as isize - t1 + 2);
        row[idx] = v;
    }
    for i in overlap..n {
        let idx = out_syn_start + i;
        let v = row[idx]
            + g10 * hist(row, idx as isize - t1)
            + g11 * hist(row, idx as isize - t1 - 1)
            + g11 * hist(row, idx as isize - t1 + 1)
            + g12 * hist(row, idx as isize - t1 - 2)
            + g12 * hist(row, idx as isize - t1 + 2);
        row[idx] = v;
    }
}

/// De-emphasis (celt.c `deemphasis`, RFC 6716 section
/// 4.3.7.2): `tmp = x + m; m = 0.85*tmp; out = tmp/32768`,
/// with the pre-emphasis memory carried across frames.
/// `pcm` is interleaved (`n * c` samples).
fn deemphasis(
    decode_mem: &[[f32; DECODE_BUFFER_SIZE + OVERLAP]; 2],
    out_syn_start: usize,
    n: usize,
    c: usize,
    preemph_mem: &mut [f32; 2],
    pcm: &mut [f32],
) {
    for ch in 0..c {
        let mut m = preemph_mem[ch];
        for j in 0..n {
            let x = decode_mem[ch][out_syn_start + j];
            let tmp = x + m;
            m = PREEMPH[0] * tmp - PREEMPH[1] * x;
            let tmp = PREEMPH[3] * tmp;
            pcm[j * c + ch] = tmp / CELT_SIG_SCALE;
        }
        preemph_mem[ch] = m;
    }
}
