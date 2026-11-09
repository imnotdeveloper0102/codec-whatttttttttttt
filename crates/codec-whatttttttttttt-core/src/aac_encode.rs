// Casts in this module are bounded by construction: every value
// cast to a narrower integer comes from a clamped range (the
// quantizer clamps to i16 before casting, scalefactors are
// clamped to 0..=255, bit counts are below 32), and every float
// cast is an intentional conversion of a table value or a
// spectrum magnitude. The pedantic cast lints therefore do not
// find real truncation risks here; the bounds are checked where
// the values are produced, not where they are cast.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]

//! AAC-LC encoder (ISO/IEC 13818-7 / ISO/IEC 14496-3 section 4.5.2.3).
//!
//! Only AAC-LC (audio object type 2) is encoded; HE-AAC (SBR),
//! AAC-LD and USAC are not, and this encoder never writes them.
//! Every frame is one long window (sine shape, 1024 samples),
//! one SCE for mono or one CPE with a common window for stereo,
//! no TNS, no pulse data, no gain control, no PNS, no MS or
//! intensity stereo: the two stereo channels are coded
//! independently. Spectral data uses the pair codebooks 5, 7, 9
//! and 11 from ISO/IEC 13818-7 Annex B, chosen per
//! scalefactor band by exact bit cost (the encode tables are
//! the decode tables in `aac_huffman` read backwards, so no
//! second copy of the codeword assignments exists).
//!
//! The quantizer inverts the decoder's dequantization exactly:
//! the decoder rebuilds `sign(q) * |q|^(4/3) * 2^(sf/4 - 25)`
//! and runs it through an IMDCT scaled by `1 / (1024 * 32768)`,
//! while `mdct_forward` scales by `2 / 1024`, so the spectrum
//! the encoder must hit is `mdct_forward(windowed input) *
//! 2^25`, and the quantized value is
//! `(|spec| * 2^(25 - sf/4))^(3/4)` per band.
//!
//! Bitrate control is a search, not a psychoacoustic model: a
//! scalefactor offset is bisected until the frame fits the
//! budget for the configured `BitrateMode` (CBR/ABR) or the
//! quality-mapped budget (VBR), dropping high scalefactor
//! bands when even the coarsest offset does not fit. There is
//! no bit reservoir: frames may come out under budget. The
//! result is a valid, decodable AAC-LC stream, not a
//! rate-distortion-optimal one.
//!
//! The license gate is the same as the decoder's (see
//! `Decoder::from_aac_config` and `hw::licensed_software_path_error`):
//! feature `licensed-aac` off means the path does not exist in
//! the build (`Unsupported`); feature on but no license, or an
//! expired license, means the path exists and is closed
//! (`LicenseRequired`); only feature on plus a validated
//! `LicenseContext` opens it. The license is validated (and
//! audited) once, at construction.

use crate::aac_frame::{AacConfig, AAC_SAMPLE_RATES};
use crate::aac_huffman::{
    huffman_is_magnitude, HUFFMAN_CB11, HUFFMAN_CB5, HUFFMAN_CB7, HUFFMAN_CB9, HUFFMAN_SF,
};
use crate::aac_tables::{NUM_SWB_1024, SINE_LONG_1024, SWB_OFFSET_1024_WINDOW};
use crate::adts_frame::write_adts_frame;
use crate::bitrate::{BitrateCodec, BitrateMode};
use crate::bitwriter::BitWriter;
use crate::error::{Error, Result};
use crate::frame::{AudioFrame, Codec, Packet};
use crate::hw::licensed_feature_enabled;
use crate::license::LicenseContext;
use crate::mdct::mdct_forward;

/// Samples per AAC-LC frame (ISO/IEC 14496-3 section 4.5.2.3).
const FRAME_LEN: usize = 1024;
/// ADTS header bytes without CRC (ISO/IEC 13818-7 section 6.2.1).
const ADTS_HEADER_BYTES: usize = 7;
/// Peak quantized magnitude the scalefactor search aims for in
/// each band before the rate loop moves it.
const TARGET_PEAK_QUANT: f32 = 12.0;
/// Finest scalefactor offset the rate loop tries: finer
/// quantization (larger quantized values, more bits).
const FINEST_OFFSET: i32 = -32;
/// Coarsest scalefactor offset the rate loop tries.
const COARSEST_OFFSET: i32 = 160;
/// Pair codebooks tried per band, in order of increasing range
/// (ISO/IEC 13818-7 Annex B: 5 covers |q| <= 4, 7 covers
/// |q| <= 7, 9 covers |q| <= 12, 11 covers |q| <= 16 plus the
/// escape extension for larger magnitudes).
const PAIR_CODEBOOKS: [u8; 4] = [5, 7, 9, 11];
/// Largest quantized magnitude the escape extension can carry:
/// codebook 11 reads at most 15 escape bits (ISO/IEC 13818-7
/// section 8.5.3, via `read_escape` in `aac_huffman`).
const MAX_QUANT_MAGNITUDE: i16 = 32_767;
/// Widest scalefactor delta the scalefactor Huffman table
/// covers: values 0..=120, delta = value - 60.
const MAX_SCALEFACTOR_DELTA: i32 = 60;

/// Maps a unified VBR quality (0..=9, 0 best, the LAME `-V`
/// convention from `bitrate.rs`) onto a target bitrate in kbps.
/// This is a library choice, not a value from the standard:
/// quality 0 aims at the top of the ladder (320 kbps) and
/// quality 9 at the bottom (64 kbps).
fn vbr_target_kbps(quality: u8) -> u32 {
    match quality {
        0 => 320,
        1 => 256,
        2 => 224,
        3 => 192,
        4 => 160,
        5 => 128,
        6 => 112,
        7 => 96,
        8 => 80,
        _ => 64,
    }
}

/// Stateful AAC-LC encoder for one stream: holds the previous
/// frame's samples per channel, which the MDCT window needs.
pub struct AacEncoder {
    sample_rate: u32,
    channels: u32,
    mode: BitrateMode,
    /// Target bits per ADTS frame (header included).
    target_bits_per_frame: u32,
    num_swb: usize,
    /// Scalefactor-band offsets, `num_swb + 1` entries ending
    /// at 1024.
    swb_offset: Vec<u16>,
    /// Previous 1024 samples per channel (zeros before the
    /// first frame: encoder priming, like every AAC encoder).
    prev: Vec<Vec<f32>>,
}

impl AacEncoder {
    /// Creates an encoder for AAC-LC at `sample_rate` with
    /// `channels` channels and the given bitrate mode.
    ///
    /// The license gate runs first, in the same order as
    /// `Decoder::from_aac_config`: feature off is
    /// `Unsupported` however the license looks, a missing or
    /// expired license is `LicenseRequired`, and a malformed
    /// license is `InvalidArgument` from `validate`.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the `licensed-aac` feature
    /// is off, `LicenseRequired` when no license is presented
    /// or it has expired, `InvalidArgument` for a malformed
    /// license, a bitrate mode the ladder rejects, an
    /// unsupported sample rate, or a channel count other than
    /// 1 or 2 (only SCE and CPE elements are written).
    pub fn new(
        sample_rate: u32,
        channels: u32,
        mode: BitrateMode,
        license: Option<LicenseContext>,
    ) -> Result<Self> {
        if !licensed_feature_enabled(Codec::Aac) {
            return Err(Error::Unsupported(
                "aac encode requires the licensed-aac feature; rebuild with --features licensed-aac",
            ));
        }
        let Some(mut license) = license else {
            return Err(Error::LicenseRequired { codec: "aac" });
        };
        license.validate("aac")?;
        mode.validate_for(BitrateCodec::Aac, sample_rate)?;
        if channels != 1 && channels != 2 {
            return Err(Error::InvalidArgument(
                "aac encoder supports mono and stereo only",
            ));
        }
        let Some(sf_index) = AAC_SAMPLE_RATES
            .iter()
            .position(|&rate| rate == sample_rate)
        else {
            return Err(Error::InvalidArgument(
                "aac sample rate has no scalefactor-band table",
            ));
        };
        if sf_index == 12 {
            // 7350 Hz: the standard's tables stop at 8000 Hz,
            // and the decoder rejects it for the same reason.
            return Err(Error::Unsupported(
                "aac sample rate 7350 Hz is not supported",
            ));
        }
        let target_kbps = match mode {
            BitrateMode::Cbr(kbps) | BitrateMode::Abr(kbps) => kbps,
            BitrateMode::Vbr(quality) => vbr_target_kbps(quality),
        };
        let target_bits_per_frame = target_kbps
            .checked_mul(1000)
            .and_then(|bps| bps.checked_mul(FRAME_LEN as u32))
            .and_then(|bits| bits.checked_div(sample_rate))
            .ok_or(Error::Overflow("aac target bits per frame overflows"))?;
        let num_swb = NUM_SWB_1024[sf_index] as usize;
        let table = SWB_OFFSET_1024_WINDOW[sf_index];
        let mut swb_offset: Vec<u16> = table[..num_swb].to_vec();
        swb_offset.push(FRAME_LEN as u16);
        Ok(Self {
            sample_rate,
            channels,
            mode,
            target_bits_per_frame,
            num_swb,
            swb_offset,
            prev: vec![vec![0.0; FRAME_LEN]; channels as usize],
        })
    }

    /// Sample rate this encoder was created with, in Hz.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Channel count this encoder was created with.
    #[must_use]
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Bitrate mode this encoder was created with.
    #[must_use]
    pub fn bitrate_mode(&self) -> BitrateMode {
        self.mode
    }

    /// The stream configuration this encoder writes: AAC-LC,
    /// 1024-sample frames.
    #[must_use]
    pub fn config(&self) -> AacConfig {
        AacConfig {
            audio_object_type: 2,
            sample_rate: self.sample_rate,
            channels: self.channels,
            frame_length: FRAME_LEN as u32,
        }
    }

    /// Encodes one 1024-sample frame into one ADTS frame
    /// (header plus raw data block). Each packet is
    /// independently decodable. The encoder keeps the frame's
    /// samples as the MDCT overlap for the next call, so the
    /// first frame is primed with silence and the caller must
    /// feed consecutive frames.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the frame's channel
    /// count or sample rate does not match the encoder, when
    /// it does not hold exactly 1024 samples per channel, or
    /// when a sample is not finite.
    pub fn encode(&mut self, frame: &AudioFrame) -> Result<Packet> {
        if frame.channels != self.channels {
            return Err(Error::InvalidArgument(
                "frame channel count does not match encoder config",
            ));
        }
        if frame.sample_rate != self.sample_rate {
            return Err(Error::InvalidArgument(
                "frame sample rate does not match encoder config",
            ));
        }
        if frame.samples.len() != FRAME_LEN * self.channels as usize {
            return Err(Error::InvalidArgument(
                "aac frame must hold exactly 1024 samples per channel",
            ));
        }
        if frame.samples.iter().any(|sample| !sample.is_finite()) {
            return Err(Error::InvalidArgument("aac frame samples must be finite"));
        }
        // One MDCT spectrum per channel, from the previous
        // frame plus this one, windowed with the sine window
        // the decoder's filter bank expects (ISO/IEC 14496-3
        // section 4.6.3.1, window shape 0).
        let mut spectra: Vec<Vec<f32>> = Vec::with_capacity(self.channels as usize);
        for channel in 0..self.channels as usize {
            let mut block = vec![0.0f32; 2 * FRAME_LEN];
            for index in 0..FRAME_LEN {
                block[index] = self.prev[channel][index] * SINE_LONG_1024[index];
                let current = frame.samples[index * self.channels as usize + channel];
                block[FRAME_LEN + index] = current * SINE_LONG_1024[FRAME_LEN - 1 - index];
            }
            let mut spectrum = vec![0.0f32; FRAME_LEN];
            mdct_forward(&block, &mut spectrum)?;
            // Undo the decoder's IMDCT scale (see the module
            // docs): the spectrum the quantizer must hit is
            // the MDCT output times 2^25.
            for value in &mut spectrum {
                *value *= 33_554_432.0;
            }
            spectra.push(spectrum);
            for index in 0..FRAME_LEN {
                self.prev[channel][index] = frame.samples[index * self.channels as usize + channel];
            }
        }
        let raw = self.encode_spectra(&spectra)?;
        let data = write_adts_frame(
            &raw,
            1,
            self.sample_rate,
            u8::try_from(self.channels)
                .map_err(|_| Error::InvalidArgument("aac channel count exceeds u8"))?,
            false,
        )?;
        Ok(Packet {
            codec: Codec::Aac,
            data,
            timestamp: frame.timestamp,
            is_keyframe: true,
        })
    }

    /// Encodes already-computed spectra into one raw data
    /// block, fitting the frame budget by bisecting the
    /// scalefactor offset and, when even the coarsest offset
    /// does not fit, dropping high scalefactor bands.
    fn encode_spectra(&self, spectra: &[Vec<f32>]) -> Result<Vec<u8>> {
        let payload_budget_bits = self
            .target_bits_per_frame
            .saturating_sub((ADTS_HEADER_BYTES * 8) as u32);
        let mut band_limit = self.num_swb;
        loop {
            if let Some(raw) =
                self.encode_within_budget(spectra, band_limit, payload_budget_bits)?
            {
                return Ok(raw);
            }
            if band_limit == 0 {
                // An all-zero block always fits; reaching
                // here means the budget itself is below the
                // smallest possible frame, so emit it anyway
                // rather than failing an encodable frame.
                return self.build_raw(spectra, COARSEST_OFFSET, 0);
            }
            band_limit = band_limit.saturating_sub(8);
        }
    }

    /// Tries to encode the spectra within `budget_bits`,
    /// returning `None` when even the coarsest offset does
    /// not fit at this band limit.
    fn encode_within_budget(
        &self,
        spectra: &[Vec<f32>],
        band_limit: usize,
        budget_bits: u32,
    ) -> Result<Option<Vec<u8>>> {
        let fits = |raw: &[u8]| raw.len() * 8 <= budget_bits as usize;
        let finest = self.build_raw(spectra, FINEST_OFFSET, band_limit)?;
        if fits(&finest) {
            return Ok(Some(finest));
        }
        let coarsest = self.build_raw(spectra, COARSEST_OFFSET, band_limit)?;
        if !fits(&coarsest) {
            return Ok(None);
        }
        // `finest` does not fit and `coarsest` does; bisect
        // for the finest offset that fits. Bits are not
        // strictly monotone in the offset, so the result is
        // the best fitting offset found, not a proof of
        // optimality.
        let mut low = FINEST_OFFSET;
        let mut high = COARSEST_OFFSET;
        let mut best = coarsest;
        while high - low > 1 {
            let mid = (low + high) / 2;
            let raw = self.build_raw(spectra, mid, band_limit)?;
            if fits(&raw) {
                high = mid;
                best = raw;
            } else {
                low = mid;
            }
        }
        Ok(Some(best))
    }

    /// Builds one raw data block from the spectra at one
    /// scalefactor offset and band limit, with no budget
    /// check. The caller (the rate loop) decides whether
    /// the result fits.
    fn build_raw(&self, spectra: &[Vec<f32>], offset: i32, band_limit: usize) -> Result<Vec<u8>> {
        let mut plans: Vec<ChannelPlan> = Vec::with_capacity(spectra.len());
        for spectrum in spectra {
            plans.push(plan_channel(spectrum, &self.swb_offset, offset, band_limit));
        }
        let max_sfb = plans.iter().map(|plan| plan.max_sfb).max().unwrap_or(0);
        let mut writer = BitWriter::new();
        if self.channels == 1 {
            // Single channel element (ISO/IEC 14496-3 Table
            // 4.6.5): id 0, tag 0, then its own ICS info.
            writer.write_bits(0, 3)?;
            writer.write_bits(0, 4)?;
            write_ics(&mut writer, plans[0].global_gain, max_sfb)?;
            write_channel_body(&mut writer, &plans[0], max_sfb, &self.swb_offset)?;
        } else {
            // Channel pair element with a common window
            // (ISO/IEC 14496-3 section 4.5.2.3.3): id 1,
            // tag 0, common_window 1, one shared ICS info,
            // ms_mask_present 0 (no MS stereo), then one
            // global gain plus body per channel.
            writer.write_bits(1, 3)?;
            writer.write_bits(0, 4)?;
            writer.write_bit(true);
            write_ics_header(&mut writer, max_sfb)?;
            writer.write_bits(0, 2)?;
            for plan in &plans {
                writer.write_bits(u32::from(plan.global_gain), 8)?;
                write_channel_body(&mut writer, plan, max_sfb, &self.swb_offset)?;
            }
        }
        // ID_END (7) ends the raw data block; the writer pads
        // the last byte with zeros.
        writer.write_bits(7, 3)?;
        Ok(writer.finish())
    }
}

/// One channel's coding decisions for a frame: the quantized
/// spectrum, one scalefactor and one codebook per band, the
/// number of bands actually coded, and the starting point of
/// the scalefactor DPCM chain.
struct ChannelPlan {
    quant: Vec<i16>,
    scale_factors: Vec<i16>,
    codebooks: Vec<u8>,
    max_sfb: usize,
    global_gain: u8,
}

/// Plans one channel: one scalefactor for the whole frame,
/// quantization, and per-band codebook choice. Bands at or
/// above `band_limit` are forced silent, which is how the rate
/// loop sheds high frequencies when the budget is too small
/// for the whole spectrum.
///
/// The scalefactor is constant across bands on purpose: it is
/// derived from the frame's peak, so the loudest band hits
/// `TARGET_PEAK_QUANT` and quieter bands quantize with the
/// same step, which rounds most of their values to zero (or
/// to 1-2, coded cheaply) instead of spending as many bits on
/// an inaudible sidelobe as on the main band. A per-band peak
/// normalization was tried first and did exactly that: a pure
/// sine needed more than 900 bytes per stereo frame at
/// 320 kbps, because every band was coded at peak 12. The
/// rate loop moves the single scalefactor instead, which is
/// also why the scalefactor DPCM deltas are almost all zero.
fn plan_channel(spec: &[f32], swb_offset: &[u16], offset: i32, band_limit: usize) -> ChannelPlan {
    let num_swb = swb_offset.len() - 1;
    let mut scale_factors = vec![0i16; num_swb];
    let mut quant = vec![0i16; FRAME_LEN];
    let mut codebooks = vec![0u8; num_swb];
    // The frame peak sets one scalefactor for the whole
    // channel: the loudest value quantizes near
    // TARGET_PEAK_QUANT (inverting the decoder's
    // dequantization: |q|^(4/3) * 2^(sf/4 - 25) = |spec|).
    let mut frame_peak = 0.0f32;
    for value in spec
        .iter()
        .take(swb_offset[band_limit.min(num_swb)] as usize)
    {
        frame_peak = frame_peak.max(value.abs());
    }
    let mut peaks = vec![0.0f32; num_swb];
    for band in 0..num_swb {
        let start = swb_offset[band] as usize;
        let end = swb_offset[band + 1] as usize;
        let mut peak = 0.0f32;
        for value in &spec[start..end] {
            peak = peak.max(value.abs());
        }
        peaks[band] = peak;
    }
    let global_gain = if frame_peak > 0.0 {
        let sf = 100.0 + 4.0 * frame_peak.log2() - (16.0 / 3.0) * TARGET_PEAK_QUANT.log2()
            + offset as f32;
        let gain = sf.round().clamp(0.0, 255.0) as i16;
        scale_factors.fill(gain);
        gain as u8
    } else {
        0
    };
    // Forward pass over the bands, in order, so the DPCM
    // chain the writer emits is exactly the chain used here:
    // a band is coded only when its quantized values are not
    // all zero, and a scalefactor that would push the delta
    // past the table's +-60 range is pulled back to the
    // range edge and the band re-quantized with it.
    let mut prev_sf = i32::from(global_gain);
    let mut max_sfb = 0usize;
    for band in 0..band_limit.min(num_swb) {
        if peaks[band] == 0.0 {
            continue;
        }
        quantize_band(
            spec,
            &mut quant,
            swb_offset[band] as usize,
            swb_offset[band + 1] as usize,
            scale_factors[band],
        );
        if band_is_silent(
            &quant,
            swb_offset[band] as usize,
            swb_offset[band + 1] as usize,
        ) {
            continue;
        }
        let delta = i32::from(scale_factors[band]) - prev_sf;
        if delta.abs() > MAX_SCALEFACTOR_DELTA {
            scale_factors[band] = (prev_sf
                + delta.clamp(-MAX_SCALEFACTOR_DELTA, MAX_SCALEFACTOR_DELTA))
            .clamp(0, 255) as i16;
            quantize_band(
                spec,
                &mut quant,
                swb_offset[band] as usize,
                swb_offset[band + 1] as usize,
                scale_factors[band],
            );
            if band_is_silent(
                &quant,
                swb_offset[band] as usize,
                swb_offset[band + 1] as usize,
            ) {
                continue;
            }
        }
        codebooks[band] =
            choose_codebook(&quant[swb_offset[band] as usize..swb_offset[band + 1] as usize]);
        prev_sf = i32::from(scale_factors[band]);
        max_sfb = band + 1;
    }
    ChannelPlan {
        quant,
        scale_factors,
        codebooks,
        max_sfb,
        global_gain,
    }
}

/// Quantizes one band: `q = sign * (|spec| * 2^(25 - sf/4))^(3/4)`,
/// the inverse of the decoder's dequantization, clamped to
/// the i16 range the escape extension can carry.
fn quantize_band(spec: &[f32], quant: &mut [i16], start: usize, end: usize, sf: i16) {
    let factor = 2f32.powf(25.0 - f32::from(sf) * 0.25);
    for index in start..end {
        let value = spec[index];
        if value == 0.0 {
            quant[index] = 0;
        } else {
            let magnitude = (value.abs() * factor).powf(3.0 / 4.0);
            let rounded = magnitude.round().clamp(0.0, f32::from(MAX_QUANT_MAGNITUDE));
            let quantized = rounded as i16;
            quant[index] = if value < 0.0 { -quantized } else { quantized };
        }
    }
}

/// True when every quantized value in the band is zero.
fn band_is_silent(quant: &[i16], start: usize, end: usize) -> bool {
    quant[start..end].iter().all(|&value| value == 0)
}

/// Chooses the cheapest pair codebook for one band by exact
/// bit cost. Codebook 11 always applies (its escape covers
/// any magnitude up to `MAX_QUANT_MAGNITUDE`), so this never
/// fails; it is the fallback when no smaller book covers the
/// band's values.
fn choose_codebook(quant: &[i16]) -> u8 {
    let mut best_codebook = 11u8;
    let mut best_bits = spectral_bits(11, quant).unwrap_or(u32::MAX);
    for &codebook in &PAIR_CODEBOOKS[..3] {
        if let Some(bits) = spectral_bits(codebook, quant) {
            if bits < best_bits {
                best_bits = bits;
                best_codebook = codebook;
            }
        }
    }
    best_codebook
}

/// Encode lookup for one pair codebook, built once from the
/// decode table: cell `(x + 16) * 33 + (y + 16)` holds the
/// (codeword, length) for the pair, or `None` when the book
/// has no entry for it. Magnitude books are keyed by
/// magnitudes (0..=16); the signed book 5 by signed values
/// (-4..=4, shifted by 16 like the rest). A direct index
/// replaces a linear scan per pair, which is what makes the
/// per-band cost search affordable.
fn spectral_encode_table(codebook: u8) -> Option<&'static [Option<(u32, u8)>; 33 * 33]> {
    use std::sync::OnceLock;
    static CB5: OnceLock<[Option<(u32, u8)>; 33 * 33]> = OnceLock::new();
    static CB7: OnceLock<[Option<(u32, u8)>; 33 * 33]> = OnceLock::new();
    static CB9: OnceLock<[Option<(u32, u8)>; 33 * 33]> = OnceLock::new();
    static CB11: OnceLock<[Option<(u32, u8)>; 33 * 33]> = OnceLock::new();
    let (cell, table) = match codebook {
        5 => (&CB5, HUFFMAN_CB5),
        7 => (&CB7, HUFFMAN_CB7),
        9 => (&CB9, HUFFMAN_CB9),
        11 => (&CB11, HUFFMAN_CB11),
        _ => return None,
    };
    Some(cell.get_or_init(|| {
        let mut lookup = [None; 33 * 33];
        for entry in table {
            let index =
                (i32::from(entry.x) + 16) as usize * 33 + (i32::from(entry.y) + 16) as usize;
            lookup[index] = Some((entry.code, entry.len));
        }
        lookup
    }))
}

/// Finds the codeword for one pair in a codebook: signed
/// values for the signed books, magnitudes for the magnitude
/// books (the sign bits follow the codeword in the stream).
/// Codebook 11 stores magnitudes only up to 16; larger
/// magnitudes are coded as 16 plus the escape extension, so
/// the lookup clamps there. Returns `None` when the book has
/// no entry for the pair, which is how a book is ruled out
/// for a band.
fn find_spectral_entry(codebook: u8, x: i16, y: i16) -> Option<(u32, u8)> {
    let table = spectral_encode_table(codebook)?;
    let magnitude_book = huffman_is_magnitude(codebook);
    let (lookup_x, lookup_y) = if magnitude_book {
        let clamp = |value: i16| {
            if codebook == 11 {
                i32::from(value.unsigned_abs()).min(16)
            } else {
                i32::from(value.unsigned_abs())
            }
        };
        (clamp(x), clamp(y))
    } else {
        (i32::from(x), i32::from(y))
    };
    if !(-16..=16).contains(&lookup_x) || !(-16..=16).contains(&lookup_y) {
        return None;
    }
    let index = (lookup_x + 16) as usize * 33 + (lookup_y + 16) as usize;
    table[index]
}

/// Escape bits for one codebook-11 value: none below 16, and
/// for magnitude `v >= 16` the count field writes
/// `count - 4` one bits, one zero bit, then `count` offset
/// bits, where `count = floor(log2(v))` (the mirror of
/// `read_escape` in `aac_huffman`).
fn escape_bits(value: i16) -> u32 {
    let magnitude = u32::from(value.unsigned_abs());
    if magnitude < 16 {
        return 0;
    }
    let count = 32 - magnitude.leading_zeros() - 1;
    2 * count - 3
}

/// Exact spectral bit cost of one band under one codebook, or
/// `None` when the book cannot code some pair in the band.
/// The count includes the codeword, one sign bit per nonzero
/// value in the magnitude books, and the codebook-11 escape
/// bits.
fn spectral_bits(codebook: u8, quant: &[i16]) -> Option<u32> {
    let magnitude_book = huffman_is_magnitude(codebook);
    let mut bits: u32 = 0;
    for pair in quant.chunks_exact(2) {
        let (x, y) = (pair[0], pair[1]);
        let (_, len) = find_spectral_entry(codebook, x, y)?;
        bits += u32::from(len);
        if magnitude_book {
            if x != 0 {
                bits += 1;
            }
            if y != 0 {
                bits += 1;
            }
        }
        if codebook == 11 {
            bits += escape_bits(x) + escape_bits(y);
        }
    }
    Some(bits)
}

/// Writes one scalefactor delta with the scalefactor Huffman
/// codebook (ISO/IEC 13818-7 section 8.5.3.1): the table
/// value is `delta + 60`.
fn write_scale_factor(writer: &mut BitWriter, delta: i32) -> Result<()> {
    let wanted = delta + 60;
    let entry = HUFFMAN_SF
        .iter()
        .find(|entry| i32::from(entry.2) == wanted)
        .ok_or(Error::InvalidArgument(
            "aac scalefactor delta is outside the huffman table range",
        ))?;
    writer.write_bits(entry.0, u32::from(entry.1))
}

/// Writes the ICS header shared by the channels of a pair
/// (ISO/IEC 14496-3 section 4.5.2.3.2): reserved bit 0,
/// window sequence 0 (`ONLY_LONG`), window shape 0 (sine),
/// `max_sfb`, predictor-present 0.
fn write_ics_header(writer: &mut BitWriter, max_sfb: usize) -> Result<()> {
    writer.write_bit(false);
    writer.write_bits(0, 2)?;
    writer.write_bit(false);
    writer.write_bits(
        u32::try_from(max_sfb).map_err(|_| Error::InvalidArgument("aac max_sfb exceeds u32"))?,
        6,
    )?;
    writer.write_bit(false);
    Ok(())
}

/// Writes one channel's global gain plus its ICS header (the
/// form a single channel element uses).
fn write_ics(writer: &mut BitWriter, global_gain: u8, max_sfb: usize) -> Result<()> {
    writer.write_bits(u32::from(global_gain), 8)?;
    write_ics_header(writer, max_sfb)
}

/// Writes one channel's section data, scalefactors, and
/// spectral data for the bands below `max_sfb`. The section
/// list is rebuilt here from the plan's per-band codebooks
/// (adjacent bands sharing a codebook form one section), so
/// the sections always cover exactly `max_sfb` bands.
fn write_channel_body(
    writer: &mut BitWriter,
    plan: &ChannelPlan,
    max_sfb: usize,
    swb_offset: &[u16],
) -> Result<()> {
    // Section data: one (codebook, length) run per group of
    // adjacent bands sharing a codebook. Lengths use the
    // 5-bit escape form (31 means "31 more, read again").
    let mut band = 0usize;
    let mut sections: Vec<(u8, usize, usize)> = Vec::new();
    while band < max_sfb {
        let codebook = plan.codebooks[band];
        let start = band;
        while band < max_sfb && plan.codebooks[band] == codebook {
            band += 1;
        }
        sections.push((codebook, start, band));
    }
    for (codebook, start, end) in &sections {
        writer.write_bits(u32::from(*codebook), 4)?;
        let mut length = end - start;
        while length >= 31 {
            writer.write_bits(31, 5)?;
            length -= 31;
        }
        writer.write_bits(length as u32, 5)?;
    }
    // Scalefactors: DPCM from the global gain, one Huffman
    // delta per coded band (zero-codebook bands carry no
    // bits, matching the decoder's read).
    let mut prev_sf = i32::from(plan.global_gain);
    for band in 0..max_sfb {
        if plan.codebooks[band] == 0 {
            continue;
        }
        let delta = i32::from(plan.scale_factors[band]) - prev_sf;
        write_scale_factor(writer, delta)?;
        prev_sf = i32::from(plan.scale_factors[band]);
    }
    // No pulse data, no TNS, no gain control.
    writer.write_bit(false);
    writer.write_bit(false);
    writer.write_bit(false);
    // Spectral data, section by section, pair by pair.
    for (codebook, start_band, end_band) in &sections {
        if *codebook == 0 {
            continue;
        }
        let magnitude_book = huffman_is_magnitude(*codebook);
        let start = swb_offset[*start_band] as usize;
        let end = swb_offset[*end_band] as usize;
        for pair in plan.quant[start..end].chunks_exact(2) {
            let (x, y) = (pair[0], pair[1]);
            let Some((code, len)) = find_spectral_entry(*codebook, x, y) else {
                return Err(Error::InvalidArgument(
                    "aac spectral values have no codeword in the chosen codebook",
                ));
            };
            writer.write_bits(code, u32::from(len))?;
            if magnitude_book {
                if x != 0 {
                    writer.write_bit(x < 0);
                }
                if y != 0 {
                    writer.write_bit(y < 0);
                }
            }
            if *codebook == 11 {
                write_escape(writer, x)?;
                write_escape(writer, y)?;
            }
        }
    }
    Ok(())
}

/// Writes the codebook-11 escape extension for one value:
/// nothing below magnitude 16, otherwise `count - 4` one
/// bits, one zero bit, then `count` offset bits, where
/// `count = floor(log2(|v|))` and `offset = |v| - 2^count`.
fn write_escape(writer: &mut BitWriter, value: i16) -> Result<()> {
    let magnitude = u32::from(value.unsigned_abs());
    if magnitude < 16 {
        return Ok(());
    }
    let count = 32 - magnitude.leading_zeros() - 1;
    if count > 4 {
        let ones = (1u32 << (count - 4)) - 1;
        writer.write_bits(ones, count - 4)?;
    }
    writer.write_bit(false);
    let offset = magnitude - (1u32 << count);
    writer.write_bits(offset, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vbr_quality_maps_to_the_ladder_ends() {
        assert_eq!(vbr_target_kbps(0), 320);
        assert_eq!(vbr_target_kbps(9), 64);
        assert_eq!(vbr_target_kbps(5), 128);
    }

    #[test]
    fn escape_bits_match_the_decoder_counts() {
        assert_eq!(escape_bits(15), 0);
        assert_eq!(escape_bits(-15), 0);
        assert_eq!(escape_bits(16), 5);
        assert_eq!(escape_bits(31), 5);
        assert_eq!(escape_bits(32), 7);
        assert_eq!(escape_bits(100), 9);
    }

    #[test]
    fn codebook_5_covers_small_pairs_and_11_covers_large_ones() {
        let small = [1i16, -2];
        assert!(spectral_bits(5, &small).is_some());
        let large = [100i16, -50];
        assert!(spectral_bits(5, &large).is_none());
        assert!(spectral_bits(11, &large).is_some());
    }
}
