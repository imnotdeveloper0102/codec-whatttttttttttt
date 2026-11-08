//! Integration tests for linear resampling.

// Test data uses small integer ramps cast to f32; the values are far
// below the f32 precision limit, so the cast is exact.
#![allow(clippy::cast_precision_loss)]

use codec_whatttttttttttt_core::{resample, AudioFrame};

#[test]
fn resample_same_rate_is_identity() {
    let samples: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
    let input = AudioFrame {
        sample_rate: 44100,
        channels: 1,
        samples,
        timestamp: 0,
    };
    let out = resample(&input, 44100).unwrap();
    assert_eq!(out, input);
}

#[test]
fn resample_8000_to_16000_doubles_frames() {
    let samples: Vec<f32> = (0..80).map(|i| (i as f32 / 80.0) * 2.0 - 1.0).collect();
    let input = AudioFrame {
        sample_rate: 8000,
        channels: 1,
        samples,
        timestamp: 0,
    };
    let out = resample(&input, 16000).unwrap();
    assert_eq!(out.frame_count(), 160);
    // Endpoints are preserved by linear interpolation.
    assert!((out.samples[0] - input.samples[0]).abs() < f32::EPSILON);
}
