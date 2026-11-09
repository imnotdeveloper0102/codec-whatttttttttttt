#![allow(
    clippy::doc_markdown,
    reason = "module docs quote spec identifiers (DoP, DSDIFF, hex markers, rates)"
)]
//! DSD over PCM (DoP), version 1.1 (dCS).
//!
//! DoP carries a DSD bitstream inside ordinary PCM frames so it
//! can travel over interfaces that only understand PCM (USB,
//! S/PDIF). Each 24-bit PCM sample carries 16 DSD bits in its
//! low 16 bits and a marker byte in its top 8 bits; the marker
//! alternates between `0x05` and `0xFA` on successive samples,
//! which is how a DoP receiver recognises the stream as DSD
//! rather than PCM. The DSD bits are MSB-first in time within
//! each 16-bit group.
//!
//! Rates: DSD64 rides on 176.4 kHz PCM, DSD128 on 352.8 kHz
//! PCM (the PCM rate is the DSD rate divided by 16, one PCM
//! sample per 16 DSD samples). Higher levels have no DoP rate
//! in the spec: DSD256 would need 705.6 kHz PCM and DSD512
//! 1.4112 MHz PCM, beyond what the DoP 1.1 specification and
//! real-world interfaces define, so both are rejected with
//! `Unsupported` here rather than packed into a rate no
//! receiver would recognise. That limit is a property of the
//! DoP specification, not of this implementation.
//!
//! The packed bytes are little-endian 24-bit PCM samples
//! (low DSD byte, high DSD byte, marker byte), interleaved
//! across channels in the usual PCM order. Unpacking validates
//! the marker alternation and returns the DSD bytes per
//! channel, MSB-first, ready for the decimator's `Dff` layout.

use crate::error::{Error, Result};

/// DoP marker on even samples (0-based).
pub const DOP_MARKER_EVEN: u8 = 0x05;
/// DoP marker on odd samples (0-based).
pub const DOP_MARKER_ODD: u8 = 0xFA;

/// PCM sample rate that carries a DSD rate over DoP, or `None`
/// when the DSD level has no DoP rate (DSD256 and DSD512).
#[must_use]
pub fn dop_pcm_rate(dsd_rate: u32) -> Option<u32> {
    match dsd_rate {
        2_822_400 => Some(176_400),
        5_644_800 => Some(352_800),
        _ => None,
    }
}

/// Reverses the bit order of every byte: DSF stores DSD bits
/// LSB-first, DoP (and DFF) store them MSB-first, so a DSF
/// stream must pass through this before packing, and a DoP
/// stream unpacked for DSF storage must pass through it again.
#[must_use]
pub fn reverse_bit_order(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|byte| byte.reverse_bits()).collect()
}

/// Packs per-channel DSD bytes (MSB-first bit order) into DoP
/// 24-bit PCM frames.
///
/// `channels_data` holds one byte slice per channel; every
/// channel must hold the same number of bytes, and the length
/// must be even (each DoP sample carries 16 DSD bits, i.e.
/// two bytes). The returned bytes are little-endian 24-bit PCM
/// samples, interleaved L, R, ... per frame.
///
/// # Errors
///
/// Returns `InvalidArgument` for zero channels, mismatched
/// channel lengths, or an odd byte count, and `Unsupported`
/// for DSD256/DSD512, which have no DoP rate.
pub fn pack_dop(channels_data: &[&[u8]], dsd_rate: u32) -> Result<Vec<u8>> {
    let pcm_rate = dop_pcm_rate(dsd_rate).ok_or(Error::Unsupported(
        "dop supports dsd64 (176.4 khz pcm) and dsd128 (352.8 khz pcm) only; dsd256/dsd512 have no dop rate",
    ))?;
    let _ = pcm_rate;
    if channels_data.is_empty() {
        return Err(Error::InvalidArgument("dop requires at least one channel"));
    }
    let len = channels_data[0].len();
    if len % 2 != 0 {
        return Err(Error::InvalidArgument(
            "dop input must hold an even number of dsd bytes per channel (16 bits per pcm sample)",
        ));
    }
    for channel in channels_data {
        if channel.len() != len {
            return Err(Error::InvalidArgument(
                "dop channels must hold equal numbers of dsd bytes",
            ));
        }
    }
    let frames = len / 2;
    let channels = channels_data.len();
    let mut out = Vec::with_capacity(frames * channels * 3);
    for frame in 0..frames {
        let marker = if frame % 2 == 0 {
            DOP_MARKER_EVEN
        } else {
            DOP_MARKER_ODD
        };
        for channel in channels_data {
            let low = channel[frame * 2];
            let high = channel[frame * 2 + 1];
            out.push(low);
            out.push(high);
            out.push(marker);
        }
    }
    Ok(out)
}

/// Unpacks DoP 24-bit PCM frames back to per-channel DSD bytes
/// (MSB-first bit order).
///
/// `pcm24` holds little-endian 24-bit samples interleaved
/// across `channels` channels; its length must be a multiple
/// of `3 * channels`. The marker byte of every sample is
/// checked against the expected alternating `0x05`/`0xFA`
/// sequence; the first mismatch is `InvalidData`.
///
/// # Errors
///
/// Returns `InvalidArgument` for zero channels or a length
/// that is not a whole number of frames, `InvalidData` for a
/// marker mismatch, and `Unsupported` for DSD256/DSD512.
pub fn unpack_dop(pcm24: &[u8], channels: usize, dsd_rate: u32) -> Result<Vec<Vec<u8>>> {
    let pcm_rate = dop_pcm_rate(dsd_rate).ok_or(Error::Unsupported(
        "dop supports dsd64 (176.4 khz pcm) and dsd128 (352.8 khz pcm) only; dsd256/dsd512 have no dop rate",
    ))?;
    let _ = pcm_rate;
    if channels == 0 {
        return Err(Error::InvalidArgument("dop requires at least one channel"));
    }
    let frame_bytes = 3 * channels;
    if pcm24.len() % frame_bytes != 0 {
        return Err(Error::InvalidData {
            offset: pcm24.len(),
            detail: "dop pcm data is not a whole number of frames",
        });
    }
    let frames = pcm24.len() / frame_bytes;
    let mut out: Vec<Vec<u8>> = vec![Vec::with_capacity(frames * 2); channels];
    for frame in 0..frames {
        let expected = if frame % 2 == 0 {
            DOP_MARKER_EVEN
        } else {
            DOP_MARKER_ODD
        };
        for (channel, out_channel) in out.iter_mut().enumerate() {
            let base = (frame * channels + channel) * 3;
            let marker = pcm24[base + 2];
            if marker != expected {
                return Err(Error::InvalidData {
                    offset: base + 2,
                    detail: "dop marker does not alternate 0x05/0xfa",
                });
            }
            out_channel.push(pcm24[base]);
            out_channel.push(pcm24[base + 1]);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dop_pcm_rate_is_176k4_for_dsd64_and_352k8_for_dsd128() {
        assert_eq!(dop_pcm_rate(2_822_400), Some(176_400));
        assert_eq!(dop_pcm_rate(5_644_800), Some(352_800));
        assert_eq!(dop_pcm_rate(11_289_600), None);
        assert_eq!(dop_pcm_rate(22_579_200), None);
    }

    #[test]
    fn pack_unpack_round_trip_is_exact() {
        let left = [0xAAu8, 0x55, 0xFF, 0x00];
        let right = [0x12u8, 0x34, 0x56, 0x78];
        let packed = pack_dop(&[&left, &right], 2_822_400).unwrap();
        assert_eq!(packed.len(), 2 * 2 * 3);
        // First frame, left channel: low, high, marker 0x05.
        assert_eq!(&packed[0..3], &[0xAA, 0x55, 0x05]);
        // First frame, right channel.
        assert_eq!(&packed[3..6], &[0x12, 0x34, 0x05]);
        // Second frame carries marker 0xFA.
        assert_eq!(packed[8], 0xFA);
        let unpacked = unpack_dop(&packed, 2, 2_822_400).unwrap();
        assert_eq!(unpacked[0], left);
        assert_eq!(unpacked[1], right);
    }

    #[test]
    fn bad_marker_is_rejected() {
        let left = [0xAAu8, 0x55, 0xFF, 0x00];
        let mut packed = pack_dop(&[&left], 2_822_400).unwrap();
        packed[2] = 0x00;
        let err = unpack_dop(&packed, 1, 2_822_400).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn dsd256_pack_is_unsupported() {
        let left = [0xAAu8, 0x55];
        let err = pack_dop(&[&left], 11_289_600).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn odd_byte_count_is_rejected() {
        let left = [0xAAu8, 0x55, 0xFF];
        let err = pack_dop(&[&left], 2_822_400).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }
}
