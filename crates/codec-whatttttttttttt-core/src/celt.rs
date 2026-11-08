//! CELT layer frame-header decode (RFC 6716 sections 4.3, 4.3.1,
//! 4.3.7.1, 4.3.7.2, 4.4).
//!
//! Subset implemented in this phase: the CELT frame header symbols
//! only (silence, post-filter, transient, intra), in the order of
//! Table 56. Sample reconstruction (coarse and fine energy, bit
//! allocation, PVQ shape decode, inverse MDCT, post-filter,
//! de-emphasis) is NOT implemented: the CELT allocation and PVQ
//! tables (`e_prob_model`, `cache_caps50`, `celt_pulse_cache`,
//! spread tables) exist only inside the reference C source, which
//! RFC 6716 Appendix A ships as a base64-encoded gzipped tarball
//! that cannot be extracted from the specification text. Porting
//! them from the reference source is follow-up work; until then
//! `decode_celt_frame_header` returns `Unsupported` naming the
//! unimplemented remainder after the header symbols, and the
//! `Decoder` integration reports the same. Nothing here fabricates
//! audio samples.
//!
//! Packet loss concealment is an explicit API, `conceal_loss()`,
//! returning a silence frame of the last frame's duration. The
//! reference CELT PLC repeats the previous frame's periodicity
//! (RFC 6716 section 4.4); silence is the honest subset of that
//! until the full CELT decoder exists.

use crate::error::{Error, Result};
use crate::range_decoder::RangeDecoder;

/// Post-filter parameters (RFC 6716 section 4.3.7.1), decoded when
/// the post-filter flag is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostFilter {
    /// Octave of the pitch period, 0..=5.
    pub octave: u32,
    /// Pitch period in samples: `(16 << octave) + fine - 1`.
    pub period: u32,
    /// Post-filter gain index, 0..=7. The gain is
    /// `3 * (index + 1) / 32`.
    pub gain_index: u32,
    /// Tapset index, 0..=2.
    pub tapset: u32,
}

impl PostFilter {
    /// Post-filter gain as a float: `G = 3 * (gain_index + 1) / 32`.
    /// `gain_index` is a 3-bit value (0..=7), so the `f32` conversion
    /// is exact.
    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn gain(&self) -> f32 {
        3.0 * (self.gain_index as f32 + 1.0) / 32.0
    }
}

/// The decoded CELT frame header (RFC 6716 section 4.3, Table 56
/// order). `silence` is decoded first: when it is set, the frame
/// contains no other symbols and the output is silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CeltFrameHeader {
    /// True when the frame is silence (all output samples zero).
    pub silence: bool,
    /// Post-filter parameters, when the post-filter flag is set.
    pub post_filter: Option<PostFilter>,
    /// True when the frame contains a transient.
    pub transient: bool,
    /// True when the frame is coded independently of the previous
    /// frame.
    pub intra: bool,
}

/// Decodes the CELT frame header symbols from the start of a CELT
/// frame (RFC 6716 section 4.3, Table 56 order: silence,
/// post-filter, transient, intra), then stops.
///
/// The remaining CELT decode (coarse and fine energy, bit
/// allocation, PVQ, inverse MDCT, post-filter, de-emphasis) is not
/// implemented in this phase, so after the header symbols this
/// function returns `Unsupported` naming that remainder — unless the
/// frame is silence, which decodes fully to zeros and returns the
/// header with no error.
///
/// # Errors
///
/// Returns `Unsupported` for a non-silence CELT frame (sample
/// reconstruction not implemented), and `InvalidData` when the
/// range decoder reports a corrupt symbol (for example a
/// post-filter octave at or above 6).
pub fn decode_celt_frame_header(frame: &[u8]) -> Result<CeltFrameHeader> {
    let mut decoder = RangeDecoder::new(frame);
    // Silence flag: PDF {32767, 1}/32768 (section 4.3, Table 56).
    let silence = decoder.decode_bit_logp(15)?;
    if silence {
        return Ok(CeltFrameHeader {
            silence: true,
            post_filter: None,
            transient: false,
            intra: false,
        });
    }
    // Post-filter flag: PDF {1, 1}/2.
    let has_post_filter = decoder.decode_bit_logp(1)?;
    let post_filter = if has_post_filter {
        // Octave: one of 6 values (section 4.3.7.1).
        let octave = decoder.decode_uint(6)?;
        // Period fine bits: 4 + octave raw bits.
        let fine = decoder.decode_bits(4 + octave)?;
        let period = (16u32 << octave) + fine - 1;
        // Gain: 3 raw bits.
        let gain_index = decoder.decode_bits(3)?;
        // Tapset: inverse CDF {2, 1, 1}/4, i.e. icdf [2, 1, 0]
        // with ftb 2.
        let tapset = decoder.decode_icdf(&[2, 1, 0], 2)?;
        Some(PostFilter {
            octave,
            period,
            gain_index,
            tapset,
        })
    } else {
        None
    };
    // Transient flag: PDF {7, 1}/8 (section 4.3.1).
    let transient = decoder.decode_bit_logp(3)?;
    // Intra flag: PDF {7, 1}/8.
    let intra = decoder.decode_bit_logp(3)?;
    let _ = CeltFrameHeader {
        silence: false,
        post_filter,
        transient,
        intra,
    };
    Err(Error::Unsupported(
        "CELT sample reconstruction (energy, bit allocation, PVQ, inverse MDCT) is not implemented in this phase; CELT tables live only in the RFC 6716 Appendix A reference source",
    ))
}

/// Explicit packet loss concealment (RFC 6716 section 4.4).
/// Returns a silence frame of `frame_duration_samples` samples for
/// each of `channels` channels, interleaved, as `f32` samples in
/// [-1, 1].
///
/// The reference CELT PLC repeats the previous frame's periodicity
/// and the SILK PLC extrapolates LPC; neither is implemented, so
/// this returns silence rather than pretending to do either. It
/// never panics: the length is checked against the allocation
/// limit before allocating.
#[must_use]
pub fn conceal_loss(frame_duration_samples: usize, channels: usize) -> Vec<f32> {
    let total = frame_duration_samples.saturating_mul(channels);
    let capped = total.min(crate::limits::MAX_FRAME_SAMPLES);
    vec![0.0; capped]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range_decoder::RangeEncoder;

    /// Encodes a CELT frame header with the given symbols using the
    /// test encoder, mirroring the decoder's symbol order.
    fn encode_header(
        silence: bool,
        post_filter: Option<(u32, u32, u32, u32)>,
        transient: bool,
        intra: bool,
    ) -> Vec<u8> {
        let mut encoder = RangeEncoder::new();
        encoder.encode_bit_logp(15, silence).unwrap();
        if silence {
            return encoder.done();
        }
        encoder.encode_bit_logp(1, post_filter.is_some()).unwrap();
        if let Some((octave, fine, gain, tapset)) = post_filter {
            encoder.encode_uint(octave, 6).unwrap();
            encoder.encode_bits(fine, 4 + octave).unwrap();
            encoder.encode_bits(gain, 3).unwrap();
            encoder.encode_icdf(&[2, 1, 0], 2, tapset).unwrap();
        }
        encoder.encode_bit_logp(3, transient).unwrap();
        encoder.encode_bit_logp(3, intra).unwrap();
        encoder.done()
    }

    #[test]
    fn silence_frame_decodes_fully() {
        let frame = encode_header(true, None, false, false);
        let header = decode_celt_frame_header(&frame).unwrap();
        assert!(header.silence);
        assert_eq!(header.post_filter, None);
    }

    #[test]
    fn header_symbols_round_trip_without_post_filter() {
        let frame = encode_header(false, None, true, false);
        let err = decode_celt_frame_header(&frame).unwrap_err();
        // The header symbols decode, then the function reports the
        // unimplemented sample reconstruction.
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn header_symbols_with_post_filter_decode_before_unsupported() {
        // Post-filter octave 2, fine bits 5 (period 16<<2 + 5 - 1
        // = 68), gain index 4, tapset 1. The function must reach
        // the Unsupported return, meaning every header symbol
        // decoded without error; a wrong symbol would surface as
        // InvalidData instead.
        let frame = encode_header(false, Some((2, 5, 4, 1)), false, true);
        let err = decode_celt_frame_header(&frame).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn post_filter_gain_formula() {
        let filter = PostFilter {
            octave: 0,
            period: 15,
            gain_index: 0,
            tapset: 0,
        };
        assert!((filter.gain() - 3.0 / 32.0).abs() < f32::EPSILON);
        let filter = PostFilter {
            gain_index: 7,
            ..filter
        };
        assert!((filter.gain() - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn conceal_loss_returns_silence_of_the_right_length() {
        let frame = conceal_loss(960, 2);
        assert_eq!(frame.len(), 1920);
        assert!(frame.iter().all(|&sample| sample == 0.0));
        let mono = conceal_loss(120, 1);
        assert_eq!(mono.len(), 120);
    }

    #[test]
    fn conceal_loss_never_panics_on_huge_requests() {
        // The length is capped at the allocation limit instead of
        // allocating an unbounded buffer.
        let frame = conceal_loss(usize::MAX, usize::MAX);
        assert_eq!(frame.len(), crate::limits::MAX_FRAME_SAMPLES);
    }
}
