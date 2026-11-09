//! Sample-rate conversion.
//!
//! Phase 1 provides linear interpolation only. It is adequate for a
//! rate change where the caller accepts interpolation error, and it is
//! honest about that: the polyphase FIR resampler planned in the design
//! is not implemented yet and is not claimed here. Resampling never
//! changes the channel count and never clips; input samples are used
//! as-is.

use crate::error::{Error, Result};
use crate::frame::AudioFrame;
use crate::limits::{MAX_ALLOCATION_BYTES, MAX_SAMPLE_RATE};
use crate::polyphase::resample_polyphase;

/// Resampling method. `Linear` is the Phase 1 interpolator:
/// cheap, honest about its error. `Polyphase` is the
/// windowed-sinc filterbank in `polyphase.rs`: the quality path
/// for rate changes where interpolation error is audible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ResampleMethod {
    /// Linear interpolation (the historical default).
    #[default]
    Linear,
    /// Windowed-sinc polyphase FIR resampling.
    Polyphase,
}

/// Resamples `input` to `target_sample_rate` with linear interpolation.
///
/// The output frame count is `round(frame_count * target / source)`,
/// computed in 64-bit arithmetic so a large frame cannot overflow. When
/// the rates are equal the input is returned unchanged.
///
/// # Errors
///
/// Returns `InvalidArgument` when either rate is zero, when the target
/// rate exceeds `MAX_SAMPLE_RATE`, or when the channel count is zero;
/// `AllocationLimit` when the output would exceed the per-call limit.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "positions stay in [0, frame_count) with frame_count below 2^24, so they are exact in f64 and non-negative; the fraction-to-f32 rounding is the sample format's own precision"
)]
pub fn resample(input: &AudioFrame, target_sample_rate: u32) -> Result<AudioFrame> {
    resample_with_method(input, target_sample_rate, ResampleMethod::Linear)
}

/// Resamples `input` to `target_sample_rate` with the given method.
///
/// `Linear` interpolates; `Polyphase` runs the windowed-sinc
/// filterbank. Both validate rates and channels the same way and
/// return the input unchanged when the rates are equal.
///
/// # Errors
///
/// Returns `InvalidArgument` when either rate is zero or the
/// channel count is zero, `Overflow` when the target rate exceeds
/// `MAX_SAMPLE_RATE`, and `AllocationLimit` when the output would
/// exceed the per-call limit.
pub fn resample_with_method(
    input: &AudioFrame,
    target_sample_rate: u32,
    method: ResampleMethod,
) -> Result<AudioFrame> {
    match method {
        ResampleMethod::Linear => resample_linear(input, target_sample_rate),
        ResampleMethod::Polyphase => resample_polyphase(input, target_sample_rate),
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "positions stay in [0, frame_count) with frame_count below 2^24, so they are exact in f64 and non-negative; the fraction-to-f32 rounding is the sample format's own precision"
)]
fn resample_linear(input: &AudioFrame, target_sample_rate: u32) -> Result<AudioFrame> {
    if input.sample_rate == 0 {
        return Err(Error::InvalidArgument(
            "source sample rate must be positive",
        ));
    }
    if target_sample_rate == 0 {
        return Err(Error::InvalidArgument(
            "target sample rate must be positive",
        ));
    }
    if target_sample_rate > MAX_SAMPLE_RATE {
        return Err(Error::Overflow("target sample rate exceeds maximum"));
    }
    if input.channels == 0 {
        return Err(Error::InvalidArgument("channel count must be positive"));
    }
    if target_sample_rate == input.sample_rate {
        return Ok(input.clone());
    }
    let channels = input.channels as usize;
    let in_frames = input.frame_count();
    if in_frames == 0 {
        return Ok(AudioFrame {
            sample_rate: target_sample_rate,
            channels: input.channels,
            samples: Vec::new(),
            timestamp: input.timestamp,
        });
    }
    // u64 arithmetic: in_frames <= 16M, rates <= 192k, product fits easily.
    let out_frames_u64 =
        (in_frames as u64 * u64::from(target_sample_rate)) / u64::from(input.sample_rate);
    let out_frames = usize::try_from(out_frames_u64)
        .map_err(|_| Error::Overflow("resampled frame count exceeds usize"))
        .map(|frames| frames.max(1))?;
    let out_samples = out_frames
        .checked_mul(channels)
        .ok_or(Error::Overflow("resampled frame too large"))?;
    if out_samples * 4 > MAX_ALLOCATION_BYTES {
        return Err(Error::AllocationLimit {
            requested: out_samples * 4,
            limit: MAX_ALLOCATION_BYTES,
        });
    }
    let mut samples = Vec::with_capacity(out_samples);
    let ratio = f64::from(input.sample_rate) / f64::from(target_sample_rate);
    for out_index in 0..out_frames {
        let position = out_index as f64 * ratio;
        let base = position.floor() as usize;
        let frac = (position - base as f64) as f32;
        let base = base.min(in_frames - 1);
        let next = (base + 1).min(in_frames - 1);
        for channel in 0..channels {
            let a = input.samples[base * channels + channel];
            let b = input.samples[next * channels + channel];
            samples.push(a + (b - a) * frac);
        }
    }
    Ok(AudioFrame {
        sample_rate: target_sample_rate,
        channels: input.channels,
        samples,
        timestamp: input.timestamp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(
        clippy::cast_precision_loss,
        reason = "test ramp only: frame counts are below 256, exactly representable in f32"
    )]
    fn ramp_frames(frames: usize, channels: u32, rate: u32) -> AudioFrame {
        let samples: Vec<f32> = (0..frames * channels as usize)
            .map(|i| i as f32 / (frames * channels as usize) as f32)
            .collect();
        AudioFrame {
            sample_rate: rate,
            channels,
            samples,
            timestamp: 0,
        }
    }

    #[test]
    fn same_rate_returns_equal_frame() {
        let input = ramp_frames(100, 2, 44100);
        let out = resample(&input, 44100).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn upsampling_doubles_frame_count() {
        let input = ramp_frames(100, 1, 8000);
        let out = resample(&input, 16000).unwrap();
        assert_eq!(out.frame_count(), 200);
        assert_eq!(out.sample_rate, 16000);
    }

    #[test]
    fn downsampling_halves_frame_count() {
        let input = ramp_frames(100, 1, 16000);
        let out = resample(&input, 8000).unwrap();
        assert_eq!(out.frame_count(), 50);
    }

    #[test]
    fn zero_target_rate_returns_invalid_argument() {
        let input = ramp_frames(10, 1, 8000);
        let err = resample(&input, 0).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    fn empty_input_resamples_to_empty() {
        let input = AudioFrame {
            sample_rate: 8000,
            channels: 1,
            samples: Vec::new(),
            timestamp: 42,
        };
        let out = resample(&input, 16000).unwrap();
        assert_eq!(out.samples.len(), 0);
        assert_eq!(out.timestamp, 42);
    }
}
