//! PCM sample formats and conversion.
//!
//! Decoded audio in this library is always interleaved f32 in [-1.0, 1.0].
//! Conversion from integer formats is saturating: a value outside the
//! target range clamps to the nearest representable value. Saturation is
//! the documented behaviour, not silent clipping of a different kind.
//! Decode and encode use the same power-of-two scale (32768 for I16,
//! 8388608 for I24), so decoding then encoding reproduces the original
//! integer samples exactly; the only loss is that f32 1.0 encodes to
//! the largest positive integer (32767 / 8388607) instead of
//! overflowing, because 32768 itself is not representable in i16.
//!
//! The f32 pipeline cannot carry 32-bit integers bit-exactly: f32 has
//! a 24-bit significand, so the low bits of an i32 sample do not
//! survive a round trip through f32. Callers that need the lossless
//! bit-exact path use `decode_i32_samples` / `encode_i32_samples`,
//! which move i32 samples without passing through f32.

use crate::error::{Error, Result};
use crate::limits::{MAX_CHANNELS, MAX_FRAME_SAMPLES};

/// PCM sample format, as declared by a container header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PcmFormat {
    /// Signed 16-bit little-endian, the WAV format tag 1 default.
    I16,
    /// Signed 24-bit little-endian, stored as 3 bytes per sample.
    I24,
    /// Signed 32-bit little-endian, the WAV format tag 1, 32-bit form.
    I32,
    /// 32-bit float little-endian, the WAV format tag 3 format.
    F32,
}

impl PcmFormat {
    /// Bytes per sample.
    #[must_use]
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::I16 => 2,
            Self::I24 => 3,
            Self::I32 | Self::F32 => 4,
        }
    }

    /// WAV `wFormatTag` for this format (1 for integer PCM, 3 for float).
    #[must_use]
    pub fn wav_format_tag(self) -> u16 {
        match self {
            Self::I16 | Self::I24 | Self::I32 => 1,
            Self::F32 => 3,
        }
    }

    /// Bits per sample as stored in a WAV `fmt ` chunk.
    #[must_use]
    pub fn bits_per_sample(self) -> u16 {
        match self {
            Self::I16 => 16,
            Self::I24 => 24,
            Self::I32 | Self::F32 => 32,
        }
    }
}

/// Decodes interleaved PCM bytes to interleaved f32 samples.
///
/// # Errors
///
/// Returns `InvalidData` when `bytes` is not a whole number of samples
/// for `channels`, `InvalidArgument` when `channels` is zero or above
/// the limit, and `AllocationLimit` when the sample count exceeds the
/// per-call allocation limit.
#[allow(
    clippy::cast_precision_loss,
    reason = "a 24-bit signed value is exactly representable in f32, whose significand holds 24 bits"
)]
pub fn decode_samples(bytes: &[u8], format: PcmFormat, channels: u32) -> Result<Vec<f32>> {
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(Error::InvalidArgument(
            "pcm channel count must be between 1 and 8",
        ));
    }
    let bytes_per_sample = format.bytes_per_sample();
    if bytes.len() % bytes_per_sample != 0 {
        return Err(Error::InvalidData {
            offset: bytes.len(),
            detail: "pcm data is not a whole number of samples",
        });
    }
    let sample_count = bytes.len() / bytes_per_sample;
    if sample_count > MAX_FRAME_SAMPLES {
        return Err(Error::AllocationLimit {
            requested: sample_count * 4,
            limit: MAX_FRAME_SAMPLES * 4,
        });
    }
    if sample_count % channels as usize != 0 {
        return Err(Error::InvalidData {
            offset: bytes.len(),
            detail: "pcm sample count is not a whole number of frames",
        });
    }
    let mut samples = Vec::with_capacity(sample_count);
    match format {
        PcmFormat::I16 => {
            for chunk in bytes.chunks_exact(2) {
                let value = i16::from_le_bytes([chunk[0], chunk[1]]);
                samples.push(f32::from(value) / 32768.0);
            }
        }
        PcmFormat::I24 => {
            for chunk in bytes.chunks_exact(3) {
                let raw =
                    i32::from(chunk[0]) | (i32::from(chunk[1]) << 8) | (i32::from(chunk[2]) << 16);
                // Sign-extend the 24-bit value to 32 bits.
                let value = (raw << 8) >> 8;
                samples.push(value as f32 / 8_388_608.0);
            }
        }
        PcmFormat::I32 => {
            for chunk in bytes.chunks_exact(4) {
                let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                samples.push(value as f32 / 2_147_483_648.0);
            }
        }
        PcmFormat::F32 => {
            for chunk in bytes.chunks_exact(4) {
                samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
        }
    }
    Ok(samples)
}

/// Encodes interleaved f32 samples to interleaved PCM bytes, saturating
/// values outside [-1.0, 1.0] to the nearest representable sample.
///
/// # Errors
///
/// Returns `InvalidArgument` when `channels` is zero or above the limit,
/// or when the sample count is not a whole number of frames.
#[allow(
    clippy::cast_possible_truncation,
    reason = "samples are clamped to [-1.0, 1.0] before scaling, so the rounded value always fits the target integer"
)]
pub fn encode_samples(samples: &[f32], format: PcmFormat, channels: u32) -> Result<Vec<u8>> {
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(Error::InvalidArgument(
            "pcm channel count must be between 1 and 8",
        ));
    }
    if samples.len() % channels as usize != 0 {
        return Err(Error::InvalidArgument(
            "pcm sample count is not a whole number of frames",
        ));
    }
    let mut out = Vec::with_capacity(samples.len() * format.bytes_per_sample());
    match format {
        PcmFormat::I16 => {
            for &sample in samples {
                let clamped = sample.clamp(-1.0, 1.0);
                let value = (clamped * 32768.0).round() as i32;
                // 1.0 scales to 32768, one past i16::MAX; saturate there.
                out.extend_from_slice(&i16::try_from(value).unwrap_or(i16::MAX).to_le_bytes());
            }
        }
        PcmFormat::I24 => {
            for &sample in samples {
                let clamped = sample.clamp(-1.0, 1.0);
                let value = (clamped * 8_388_608.0).round() as i32;
                let value = value.clamp(-8_388_608, 8_388_607);
                out.extend_from_slice(&value.to_le_bytes()[..3]);
            }
        }
        PcmFormat::I32 => {
            for &sample in samples {
                let clamped = sample.clamp(-1.0, 1.0);
                // i64 holds the scaled value: 1.0 scales to 2^31, one
                // past i32::MAX, so the saturating step below needs it.
                let value = (clamped * 2_147_483_648.0).round() as i64;
                let value = value.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
                out.extend_from_slice(&i32::try_from(value).unwrap_or(i32::MAX).to_le_bytes());
            }
        }
        PcmFormat::F32 => {
            for &sample in samples {
                out.extend_from_slice(&sample.to_le_bytes());
            }
        }
    }
    Ok(out)
}

/// Decodes interleaved 32-bit integer PCM bytes to interleaved i32
/// samples, without passing through f32.
///
/// The f32 pipeline rounds every i32 value to 24 significant bits,
/// so a lossless path that must preserve the low bits uses this
/// function instead of `decode_samples` with `PcmFormat::I32`.
///
/// # Errors
///
/// Returns `InvalidData` when `bytes` is not a whole number of
/// samples for `channels`, `InvalidArgument` when `channels` is zero
/// or above the limit, and `AllocationLimit` when the sample count
/// exceeds the per-call allocation limit.
pub fn decode_i32_samples(bytes: &[u8], channels: u32) -> Result<Vec<i32>> {
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(Error::InvalidArgument(
            "pcm channel count must be between 1 and 8",
        ));
    }
    if bytes.len() % 4 != 0 {
        return Err(Error::InvalidData {
            offset: bytes.len(),
            detail: "pcm data is not a whole number of samples",
        });
    }
    let sample_count = bytes.len() / 4;
    if sample_count > MAX_FRAME_SAMPLES {
        return Err(Error::AllocationLimit {
            requested: sample_count * 4,
            limit: MAX_FRAME_SAMPLES * 4,
        });
    }
    if sample_count % channels as usize != 0 {
        return Err(Error::InvalidData {
            offset: bytes.len(),
            detail: "pcm sample count is not a whole number of frames",
        });
    }
    let mut samples = Vec::with_capacity(sample_count);
    for chunk in bytes.chunks_exact(4) {
        samples.push(i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(samples)
}

/// Encodes interleaved i32 samples to interleaved 32-bit integer PCM
/// bytes, exactly; the inverse of `decode_i32_samples`.
///
/// # Errors
///
/// Returns `InvalidArgument` when `channels` is zero or above the
/// limit, or when the sample count is not a whole number of frames.
pub fn encode_i32_samples(samples: &[i32], channels: u32) -> Result<Vec<u8>> {
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(Error::InvalidArgument(
            "pcm channel count must be between 1 and 8",
        ));
    }
    if samples.len() % channels as usize != 0 {
        return Err(Error::InvalidArgument(
            "pcm sample count is not a whole number of frames",
        ));
    }
    let mut out = Vec::with_capacity(samples.len() * 4);
    for &sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i16_round_trip_within_one_step() {
        let original = vec![0.0, 0.5, -0.5, 1.0, -1.0, 0.25];
        let bytes = encode_samples(&original, PcmFormat::I16, 2).unwrap();
        let decoded = decode_samples(&bytes, PcmFormat::I16, 2).unwrap();
        for (a, b) in original.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 1.0 / 32768.0 + f32::EPSILON);
        }
    }

    #[test]
    fn i24_sign_extends_negative_values() {
        // -1 in 24-bit little-endian is 0xFF 0xFF 0xFF.
        let decoded = decode_samples(&[0xFF, 0xFF, 0xFF], PcmFormat::I24, 1).unwrap();
        assert!((decoded[0] + 1.0 / 8_388_608.0).abs() < 1e-6);
    }

    #[test]
    fn i24_round_trip_within_one_step() {
        let original = vec![0.0, 0.5, -0.75];
        let bytes = encode_samples(&original, PcmFormat::I24, 1).unwrap();
        let decoded = decode_samples(&bytes, PcmFormat::I24, 1).unwrap();
        for (a, b) in original.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 1.0 / 8_388_608.0 + f32::EPSILON);
        }
    }

    #[test]
    fn i32_round_trip_is_bit_exact() {
        let original = vec![0, 1, -1, 123_456_789, -987_654_321, i32::MIN, i32::MAX];
        let bytes = encode_i32_samples(&original, 1).unwrap();
        assert_eq!(decode_i32_samples(&bytes, 1).unwrap(), original);
    }

    #[test]
    fn i32_through_f32_keeps_representable_values() {
        // Multiples of 256 fit in the 24-bit f32 significand exactly.
        let original = vec![0.0, 0.5, -0.25, 256.0 / 2_147_483_648.0];
        let bytes = encode_samples(&original, PcmFormat::I32, 1).unwrap();
        let decoded = decode_samples(&bytes, PcmFormat::I32, 1).unwrap();
        for (a, b) in original.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 1e-9);
        }
    }

    #[test]
    fn f32_round_trip_is_exact() {
        let original = vec![0.0, 0.5, -0.5, 1.0, -1.0];
        let bytes = encode_samples(&original, PcmFormat::F32, 1).unwrap();
        assert_eq!(decode_samples(&bytes, PcmFormat::F32, 1).unwrap(), original);
    }

    #[test]
    fn partial_sample_returns_invalid_data() {
        let err = decode_samples(&[0x00], PcmFormat::I16, 1).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn zero_channels_returns_invalid_argument() {
        let err = decode_samples(&[], PcmFormat::I16, 0).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    fn encode_saturates_out_of_range_samples() {
        let bytes = encode_samples(&[2.0, -2.0], PcmFormat::I16, 1).unwrap();
        assert_eq!(
            decode_samples(&bytes, PcmFormat::I16, 1).unwrap()[0],
            32767.0 / 32768.0
        );
    }
}
