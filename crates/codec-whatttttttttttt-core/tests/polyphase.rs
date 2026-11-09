//! Integration tests for the polyphase resampler (`polyphase.rs`).
//!
//! Quality numbers here are **measured, not asserted from a
//! datasheet**: each test fits a sine to the resampled output by
//! least squares (no FFT bin leakage) and reports the measured
//! value with `eprintln!` (visible with `-- --nocapture`).
//!
//! Measured on 2026-11-08 (debug build, this machine — rerun to
//! confirm on another machine; thresholds below keep headroom
//! over the measured values, they are not the measured values):
//! - THD+N, 1 kHz sine 44.1 -> 48 kHz: -117.27 dB; 48 -> 44.1 kHz:
//!   -116.35 dB; 8 -> 44.1 kHz: -115.93 dB (linear interpolator
//!   on the same tone: -62.40 dB)
//! - Passband ripple 44.1 -> 48 kHz, 100 Hz..20 kHz: worst 0.0001 dB
//! - Alias breakthrough 96 -> 48 kHz: 30 kHz -124.36 dB, 40 kHz
//!   -140.70 dB, 47 kHz -155.02 dB
//! - Alias breakthrough 192 -> 48 kHz: 30 kHz -117.86 dB, 60 kHz
//!   -140.24 dB, 90 kHz -159.12 dB
//! - Alias breakthrough 48 -> 44.1 kHz (23 kHz tone): -59.08 dB
//! - Stereo cross-talk (3 kHz leak into the 1 kHz channel):
//!   fitted amplitude 0.00137 (input amplitudes 0.8 / 0.6)
//! - Round-trip 48 -> 96 -> 48 kHz tone amplitude: 0.000 dB for
//!   I16/I24/I32/F32
//!
//! Thresholds asserted in the tests: THD+N -80 dB, ripple 0.1 dB,
//! aliasing -70 dB (96/192 -> 48) and -50 dB (48 -> 44.1, whose
//! 23 kHz tone sits 950 Hz from the cutoff, inside the
//! transition's stopband edge), tone amplitude 0.5 dB, stereo
//! leak 0.02. If a rerun measures worse than a threshold, the
//! filter or the measurement is what must change — not the
//! threshold, and any change must be recorded here.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::io::Cursor;

use codec_whatttttttttttt_core::{
    decode_samples, encode_flac_stream, encode_samples, encode_wav, resample, resample_with_method,
    AudioFrame, Codec, Decoder, Error, FlacDemuxer, Frame, PcmFormat, PolyphaseResampler,
    ResampleMethod, WavDemuxer,
};

/// Mono sine at `freq` Hz, `frames` long, amplitude `amp`.
fn sine(rate: u32, freq: f64, frames: usize, amp: f32) -> Vec<f32> {
    (0..frames)
        .map(|n| {
            let t = n as f64 / f64::from(rate);
            (2.0 * std::f64::consts::PI * freq * t).sin() as f32 * amp
        })
        .collect()
}

fn frame(samples: Vec<f32>, rate: u32, channels: u32) -> AudioFrame {
    AudioFrame {
        sample_rate: rate,
        channels,
        samples,
        timestamp: 0,
    }
}

/// Least-squares fit of `a*sin(2*pi*f*t) + b*cos(2*pi*f*t) + c`
/// over `samples[skip..len-skip]` at `rate`. Returns
/// `(amplitude, residual_rms)`: the fitted tone amplitude and the
/// RMS of everything the fit does not explain (harmonics, images,
/// noise, edge leakage). Fitting in the time domain avoids FFT
/// bin leakage entirely; the tone frequency must be exact.
fn fit_sine(samples: &[f32], rate: u32, freq: f64, skip: usize) -> (f64, f64) {
    let data = &samples[skip..samples.len() - skip];
    let count = data.len() as f64;
    let mut ss = 0.0;
    let mut cc = 0.0;
    let mut sc = 0.0;
    let mut sin_sum = 0.0;
    let mut cos_sum = 0.0;
    let mut ys = 0.0;
    let mut yc = 0.0;
    let mut sum_y = 0.0;
    for (i, &sample) in data.iter().enumerate() {
        let phase = 2.0 * std::f64::consts::PI * freq * (i + skip) as f64 / f64::from(rate);
        let (sin, cos) = phase.sin_cos();
        let value = f64::from(sample);
        ss += sin * sin;
        cc += cos * cos;
        sc += sin * cos;
        sin_sum += sin;
        cos_sum += cos;
        ys += value * sin;
        yc += value * cos;
        sum_y += value;
    }
    // Solve the 3x3 normal equations by Cramer's rule. The matrix
    // is well-conditioned for a tone over many periods; a
    // degenerate (zero-length or single-sample) input yields NaN
    // and the caller treats it as a failed fit.
    let det = ss * (cc * count - cos_sum * cos_sum) - sc * (sc * count - cos_sum * sin_sum)
        + sin_sum * (sc * cos_sum - cc * sin_sum);
    let coef_sin = (ys * (cc * count - cos_sum * cos_sum) - sc * (yc * count - cos_sum * sum_y)
        + sin_sum * (yc * cos_sum - cc * sum_y))
        / det;
    let coef_cos = (ss * (yc * count - cos_sum * sum_y) - ys * (sc * count - cos_sum * sin_sum)
        + sin_sum * (sc * sum_y - yc * sin_sum))
        / det;
    let offset = (ss * (cc * sum_y - yc * cos_sum) - sc * (sc * sum_y - ys * cos_sum)
        + ys * (sc * cos_sum - cc * sin_sum))
        / det;
    let amplitude = (coef_sin * coef_sin + coef_cos * coef_cos).sqrt();
    let mut residual = 0.0;
    for (i, &sample) in data.iter().enumerate() {
        let phase = 2.0 * std::f64::consts::PI * freq * (i + skip) as f64 / f64::from(rate);
        let (sin, cos) = phase.sin_cos();
        let fitted = coef_sin * sin + coef_cos * cos + offset;
        let diff = f64::from(sample) - fitted;
        residual += diff * diff;
    }
    (amplitude, (residual / count).sqrt())
}

/// RMS of `samples[skip..len-skip]`, in dB relative to `reference`.
fn trimmed_rms_db(samples: &[f32], reference: f64, skip: usize) -> f64 {
    let data = &samples[skip..samples.len() - skip];
    let mean_square: f64 = data
        .iter()
        .map(|&sample| {
            let value = f64::from(sample);
            value * value
        })
        .sum::<f64>()
        / data.len() as f64;
    20.0 * mean_square.sqrt().log10() - 20.0 * reference.log10()
}

/// Trims 10% off each end: far longer than the filter delay
/// (~64 input frames for 44.1->48 kHz, ~1024 for 192->48 kHz at
/// the signal lengths used here), so the edge transient is gone.
fn trim(frames: usize) -> usize {
    frames / 10
}

/// Trims each end of a polyphase output by at least the filter
/// delay (in output frames) plus 10% of the length, and at least
/// 10% overall. A fixed 10% trim is NOT enough: for L=2 the
/// 2048-tap prototype delays the output by ~1024 frames, more
/// than 10% of a 0.1 s signal, and the transient leaking into a
/// least-squares fit biases the amplitude by up to 0.7 dB
/// (measured 2026-11-08: 22050->44100 gave -0.175 dB at 10%
/// trim and 0.000 dB at 20% trim). The delay is computed from
/// the resampler itself, not guessed.
fn trim_polyphase(source: u32, target: u32, out_frames: usize) -> usize {
    let resampler = PolyphaseResampler::new(source, target, 1).unwrap();
    let delay_out =
        (resampler.delay_in_input_frames() * f64::from(target) / f64::from(source)).ceil() as usize;
    (out_frames / 10).max(delay_out + out_frames / 10)
}

#[test]
fn polyphase_same_rate_is_identity() {
    let input = frame(sine(44100, 1000.0, 4410, 0.8), 44100, 1);
    let out = resample_with_method(&input, 44100, ResampleMethod::Polyphase).unwrap();
    assert_eq!(out, input);
}

#[test]
fn polyphase_ratios_produce_expected_frame_counts() {
    let cases = [
        (44100, 48000),
        (48000, 44100),
        (96000, 48000),
        (192_000, 48000),
        (8000, 44100),
        (22050, 44100),
        (48000, 24000),
        (48000, 192_000),
        (192_000, 48000),
    ];
    for (source, target) in cases {
        let frames = source as usize / 10; // 0.1 s of input
        let input = frame(vec![0.5f32; frames], source, 1);
        let out = resample_with_method(&input, target, ResampleMethod::Polyphase).unwrap();
        let expected = frames * target as usize / source as usize;
        assert_eq!(
            out.frame_count(),
            expected,
            "{source} -> {target}: expected {expected} frames, got {}",
            out.frame_count()
        );
        assert_eq!(out.sample_rate, target);
    }
}

#[test]
fn polyphase_2x_and_4x_integer_ratios() {
    // 2x up, 2x down, 4x up, 4x down, with an in-band tone whose
    // amplitude must survive the rate change.
    let cases = [
        (22050, 44100, 1000.0),
        (48000, 24000, 1000.0),
        (48000, 192_000, 1000.0),
        (192_000, 48000, 1000.0),
    ];
    for (source, target, freq) in cases {
        let frames = source as usize / 5; // 0.2 s
        let input = frame(sine(source, freq, frames, 0.8), source, 1);
        let out = resample_with_method(&input, target, ResampleMethod::Polyphase).unwrap();
        let skip = trim_polyphase(source, target, out.frame_count());
        let (amp, _) = fit_sine(&out.samples, target, freq, skip);
        let db = 20.0 * (amp / 0.8).log10();
        eprintln!("polyphase {source}->{target} tone {freq} Hz: amplitude {db:.3} dB");
        assert!(
            db.abs() < 0.5,
            "{source}->{target}: tone amplitude changed by {db:.3} dB"
        );
    }
}

#[test]
fn polyphase_thdn_1khz_44100_to_48000() {
    // 0.5 s of 1 kHz at 44.1 kHz -> 48 kHz. THD+N is the residual
    // after fitting the 1 kHz tone, relative to the tone RMS.
    let input = frame(sine(44100, 1000.0, 22050, 0.8), 44100, 1);
    let out = resample_with_method(&input, 48000, ResampleMethod::Polyphase).unwrap();
    let skip = trim_polyphase(44100, 48000, out.frame_count());
    let (amp, residual) = fit_sine(&out.samples, 48000, 1000.0, skip);
    let thd_n_db = 20.0 * (residual / (amp / std::f64::consts::SQRT_2)).log10();
    eprintln!("polyphase THD+N 1 kHz 44.1->48 kHz: {thd_n_db:.2} dB (amp {amp:.4}, residual {residual:.6})");
    assert!(
        thd_n_db < -80.0,
        "THD+N {thd_n_db:.2} dB is worse than the -80 dB threshold"
    );
}

#[test]
fn polyphase_thdn_1khz_48000_to_44100() {
    let input = frame(sine(48000, 1000.0, 24000, 0.8), 48000, 1);
    let out = resample_with_method(&input, 44100, ResampleMethod::Polyphase).unwrap();
    let skip = trim_polyphase(48000, 44100, out.frame_count());
    let (amp, residual) = fit_sine(&out.samples, 44100, 1000.0, skip);
    let thd_n_db = 20.0 * (residual / (amp / std::f64::consts::SQRT_2)).log10();
    eprintln!("polyphase THD+N 1 kHz 48->44.1 kHz: {thd_n_db:.2} dB");
    assert!(
        thd_n_db < -80.0,
        "THD+N {thd_n_db:.2} dB is worse than the -80 dB threshold"
    );
}

#[test]
fn polyphase_passband_ripple_44100_to_48000() {
    // Amplitude of tones across the passband, measured in and out
    // by the same fit: the ratio cancels any generation error.
    let freqs = [100.0, 1000.0, 5000.0, 10000.0, 15000.0, 18000.0, 20000.0];
    let mut worst_db = 0.0f64;
    for freq in freqs {
        let input = frame(sine(44100, freq, 22050, 0.8), 44100, 1);
        let out = resample_with_method(&input, 48000, ResampleMethod::Polyphase).unwrap();
        let skip = trim_polyphase(44100, 48000, out.frame_count());
        let (amp_in, _) = fit_sine(&input.samples, 44100, freq, trim(22050));
        let (amp_out, _) = fit_sine(&out.samples, 48000, freq, skip);
        let db = 20.0 * (amp_out / amp_in).log10();
        eprintln!("polyphase ripple {freq} Hz 44.1->48 kHz: {db:.4} dB");
        worst_db = worst_db.max(db.abs());
    }
    eprintln!("polyphase passband ripple 44.1->48 kHz worst: {worst_db:.4} dB");
    assert!(
        worst_db < 0.1,
        "passband ripple {worst_db:.4} dB exceeds the 0.1 dB threshold"
    );
}

#[test]
fn polyphase_aliasing_96_to_48() {
    // Tones above the 24 kHz output Nyquist must not appear in
    // the output: measure output RMS relative to the input tone.
    for freq in [30000.0, 40000.0, 47000.0] {
        let frames = 9600; // 0.1 s at 96 kHz
        let input = frame(sine(96000, freq, frames, 0.8), 96000, 1);
        let out = resample_with_method(&input, 48000, ResampleMethod::Polyphase).unwrap();
        let skip = trim_polyphase(96000, 48000, out.frame_count());
        let db = trimmed_rms_db(&out.samples, 0.8 / std::f64::consts::SQRT_2, skip);
        eprintln!("polyphase alias 96->48 kHz tone {freq} Hz: output {db:.2} dB relative to input tone RMS");
        assert!(
            db < -70.0,
            "96->48 kHz alias breakthrough at {freq} Hz: {db:.2} dB"
        );
    }
}

#[test]
fn polyphase_aliasing_192_to_48() {
    for freq in [30000.0, 60000.0, 90000.0] {
        let frames = 19200; // 0.1 s at 192 kHz
        let input = frame(sine(192_000, freq, frames, 0.8), 192_000, 1);
        let out = resample_with_method(&input, 48000, ResampleMethod::Polyphase).unwrap();
        let skip = trim_polyphase(192_000, 48000, out.frame_count());
        let db = trimmed_rms_db(&out.samples, 0.8 / std::f64::consts::SQRT_2, skip);
        eprintln!("polyphase alias 192->48 kHz tone {freq} Hz: output {db:.2} dB relative to input tone RMS");
        assert!(
            db < -70.0,
            "192->48 kHz alias breakthrough at {freq} Hz: {db:.2} dB"
        );
    }
}

#[test]
fn polyphase_aliasing_48_to_44100() {
    // A 23 kHz tone at 48 kHz folds to 21.1 kHz at 44.1 kHz if the
    // anti-alias filter lets it through; measure total output.
    let input = frame(sine(48000, 23000.0, 4800, 0.8), 48000, 1);
    let out = resample_with_method(&input, 44100, ResampleMethod::Polyphase).unwrap();
    let skip = trim(out.frame_count());
    let db = trimmed_rms_db(&out.samples, 0.8 / std::f64::consts::SQRT_2, skip);
    eprintln!(
        "polyphase alias 48->44.1 kHz tone 23000 Hz: output {db:.2} dB relative to input tone RMS"
    );
    assert!(
        db < -50.0,
        "48->44.1 kHz alias breakthrough at 23 kHz: {db:.2} dB"
    );
}

#[test]
fn polyphase_8_to_44100_upsample_keeps_tone() {
    // The widest ratio in the required set: 8 kHz -> 44.1 kHz
    // (L=441, M=80). A 1 kHz tone must survive at full amplitude.
    let input = frame(sine(8000, 1000.0, 4000, 0.8), 8000, 1);
    let out = resample_with_method(&input, 44100, ResampleMethod::Polyphase).unwrap();
    assert_eq!(out.frame_count(), 4000 * 44100 / 8000);
    let skip = trim_polyphase(8000, 44100, out.frame_count());
    let (amp, residual) = fit_sine(&out.samples, 44100, 1000.0, skip);
    let db = 20.0 * (amp / 0.8).log10();
    let thd_n_db = 20.0 * (residual / (amp / std::f64::consts::SQRT_2)).log10();
    eprintln!("polyphase 8->44.1 kHz tone 1 kHz: amplitude {db:.3} dB, THD+N {thd_n_db:.2} dB");
    assert!(db.abs() < 0.5, "tone amplitude changed by {db:.3} dB");
    assert!(thd_n_db < -70.0, "THD+N {thd_n_db:.2} dB");
}

#[test]
fn polyphase_stereo_keeps_channels_separate() {
    let frames = 4800;
    let mut samples = Vec::with_capacity(frames * 2);
    for n in 0..frames {
        let t = n as f64 / 48000.0;
        samples.push((2.0 * std::f64::consts::PI * 1000.0 * t).sin() as f32 * 0.8);
        samples.push((2.0 * std::f64::consts::PI * 3000.0 * t).sin() as f32 * 0.6);
    }
    let input = frame(samples, 48000, 2);
    let out = resample_with_method(&input, 44100, ResampleMethod::Polyphase).unwrap();
    assert_eq!(out.channels, 2);
    let skip = trim_polyphase(48000, 44100, out.frame_count());
    let left: Vec<f32> = out.samples.iter().step_by(2).copied().collect();
    let right: Vec<f32> = out.samples.iter().skip(1).step_by(2).copied().collect();
    let (amp_left, _) = fit_sine(&left, 44100, 1000.0, skip);
    let (amp_right, _) = fit_sine(&right, 44100, 3000.0, skip);
    // Cross-talk check: the fitted 3 kHz *amplitude* in the left
    // channel must be ~0. (The fit residual there would be the
    // 1 kHz tone itself, not leakage, so amplitude is the right
    // measure.)
    let (leak_amp, _) = fit_sine(&left, 44100, 3000.0, skip);
    eprintln!(
        "polyphase stereo: left 1 kHz amp {amp_left:.3}, right 3 kHz amp {amp_right:.3}, left 3 kHz leak amp {leak_amp:.5}"
    );
    assert!((amp_left - 0.8).abs() < 0.05);
    assert!((amp_right - 0.6).abs() < 0.05);
    assert!(leak_amp < 0.02, "left channel leaked 3 kHz: {leak_amp:.5}");
}

#[test]
fn polyphase_rejects_invalid_rates() {
    let input = frame(vec![0.5f32; 100], 44100, 1);
    assert!(matches!(
        resample_with_method(&input, 0, ResampleMethod::Polyphase),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        resample_with_method(&input, 192_001, ResampleMethod::Polyphase),
        Err(Error::Overflow(_))
    ));
    let bad = frame(vec![0.5f32; 100], 0, 1);
    assert!(matches!(
        resample_with_method(&bad, 48000, ResampleMethod::Polyphase),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn polyphase_empty_input_stays_empty() {
    let input = frame(Vec::new(), 44100, 1);
    let out = resample_with_method(&input, 48000, ResampleMethod::Polyphase).unwrap();
    assert_eq!(out.samples.len(), 0);
    assert_eq!(out.sample_rate, 48000);
}

#[test]
fn linear_resample_still_works_through_method_dispatch() {
    // The old path must be untouched: `resample` and
    // `ResampleMethod::Linear` agree exactly.
    let input = frame(sine(8000, 440.0, 800, 0.8), 8000, 1);
    let via_default = resample(&input, 16000).unwrap();
    let via_method = resample_with_method(&input, 16000, ResampleMethod::Linear).unwrap();
    assert_eq!(via_default, via_method);
    assert_eq!(via_default.frame_count(), 1600);
}

#[test]
fn polyphase_beats_linear_on_thdn() {
    // The point of the polyphase path: on the same 1 kHz tone,
    // its residual must be smaller than linear interpolation's.
    // This guards against a regression that silently makes the
    // polyphase path linear.
    let input = frame(sine(44100, 1000.0, 22050, 0.8), 44100, 1);
    let linear = resample(&input, 48000).unwrap();
    let poly = resample_with_method(&input, 48000, ResampleMethod::Polyphase).unwrap();
    let skip = trim_polyphase(44100, 48000, poly.frame_count());
    let (amp_l, res_l) = fit_sine(&linear.samples, 48000, 1000.0, skip);
    let (amp_p, res_p) = fit_sine(&poly.samples, 48000, 1000.0, skip);
    let thd_l = 20.0 * (res_l / (amp_l / std::f64::consts::SQRT_2)).log10();
    let thd_p = 20.0 * (res_p / (amp_p / std::f64::consts::SQRT_2)).log10();
    eprintln!("THD+N linear {thd_l:.2} dB vs polyphase {thd_p:.2} dB (1 kHz, 44.1->48 kHz)");
    assert!(
        thd_p < thd_l - 20.0,
        "polyphase ({thd_p:.2} dB) must beat linear ({thd_l:.2} dB) by 20 dB"
    );
}

// ---- Sample-format matrix (lossless paths + resample) ----

/// A deterministic i16 sine, one channel, `frames` long.
fn sine_i16(rate: u32, freq: f64, frames: usize) -> Vec<i16> {
    (0..frames)
        .map(|n| {
            let t = n as f64 / f64::from(rate);
            ((2.0 * std::f64::consts::PI * freq * t).sin() * 20000.0) as i16
        })
        .collect()
}

/// WAV I16 round trip is bit-exact: encode -> demux -> encode
/// reproduces the original PCM bytes.
fn wav_i16_round_trip_is_bit_exact(rate: u32) {
    let original = sine_i16(rate, 440.0, rate as usize / 10);
    let f32_samples: Vec<f32> = original
        .iter()
        .map(|&value| f32::from(value) / 32768.0)
        .collect();
    let bytes = encode_wav(&f32_samples, PcmFormat::I16, 1, rate).unwrap();
    let demuxer = WavDemuxer::new(Cursor::new(bytes)).unwrap();
    assert_eq!(demuxer.header().sample_rate, rate);
    let decoded = demuxer.samples().unwrap();
    let re_encoded = encode_samples(&decoded, PcmFormat::I16, 1).unwrap();
    let original_bytes = encode_samples(&f32_samples, PcmFormat::I16, 1).unwrap();
    assert_eq!(
        re_encoded, original_bytes,
        "WAV I16 {rate} Hz not bit-exact"
    );
}

/// WAV I24 round trip is bit-exact (same check at 24 bits).
fn wav_i24_round_trip_is_bit_exact(rate: u32) {
    let f32_samples: Vec<f32> = (0..rate as usize / 10)
        .map(|n| {
            let t = n as f64 / f64::from(rate);
            ((2.0 * std::f64::consts::PI * 440.0 * t).sin() * 0.5) as f32
        })
        .collect();
    let bytes = encode_wav(&f32_samples, PcmFormat::I24, 1, rate).unwrap();
    let demuxer = WavDemuxer::new(Cursor::new(bytes)).unwrap();
    let decoded = demuxer.samples().unwrap();
    let re_encoded = encode_samples(&decoded, PcmFormat::I24, 1).unwrap();
    let original_bytes = encode_samples(&f32_samples, PcmFormat::I24, 1).unwrap();
    assert_eq!(
        re_encoded, original_bytes,
        "WAV I24 {rate} Hz not bit-exact"
    );
}

/// FLAC I16 round trip is bit-exact through the real decoder.
fn flac_i16_round_trip_is_bit_exact(rate: u32) {
    let original = sine_i16(rate, 440.0, rate as usize / 10);
    let bytes = encode_flac_stream(&original, 1, rate).unwrap();
    let mut demuxer = FlacDemuxer::new(Cursor::new(bytes)).unwrap();
    let mut decoder = Decoder::from_flac_streaminfo(demuxer.streaminfo()).unwrap();
    let mut decoded_f32 = Vec::new();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        match decoder.decode(&packet).unwrap() {
            Frame::Audio(audio) => decoded_f32.extend_from_slice(&audio.samples),
            Frame::Video(_) => panic!("expected an audio frame"),
        }
    }
    assert_eq!(decoded_f32.len(), original.len());
    for (index, (&expected, &got)) in original.iter().zip(decoded_f32.iter()).enumerate() {
        let expected_f32 = f32::from(expected) / 32768.0;
        assert_eq!(
            got, expected_f32,
            "FLAC I16 {rate} Hz sample {index} differs"
        );
    }
}

#[test]
fn sample_matrix_wav_i16_at_44100_48000_96000_192_000() {
    for rate in [44100, 48000, 96000, 192_000] {
        wav_i16_round_trip_is_bit_exact(rate);
    }
}

#[test]
fn sample_matrix_wav_i24_at_16000_48000_96000_192_000() {
    for rate in [16000, 48000, 96000, 192_000] {
        wav_i24_round_trip_is_bit_exact(rate);
    }
}

#[test]
fn sample_matrix_flac_i16_at_44100_48000_96000_192_000() {
    for rate in [44100, 48000, 96000, 192_000] {
        flac_i16_round_trip_is_bit_exact(rate);
    }
}

#[test]
fn sample_matrix_wav_i32_and_f32_at_48000_96000() {
    // I32/F32 through the f32 pipeline: encode -> decode ->
    // encode is stable (the f32 rounding is deterministic), so
    // the bytes reproduce exactly. True bit-exact i32 needs the
    // `decode_i32_samples` path in `pcm.rs`, not this one.
    for format in [PcmFormat::I32, PcmFormat::F32] {
        for rate in [48000, 96000] {
            let f32_samples: Vec<f32> = (0..rate as usize / 20)
                .map(|n| {
                    let t = n as f64 / f64::from(rate);
                    ((2.0 * std::f64::consts::PI * 440.0 * t).sin() * 0.5) as f32
                })
                .collect();
            let bytes = encode_wav(&f32_samples, format, 1, rate).unwrap();
            let demuxer = WavDemuxer::new(Cursor::new(bytes)).unwrap();
            let decoded = demuxer.samples().unwrap();
            let re_encoded = encode_samples(&decoded, format, 1).unwrap();
            let original_bytes = encode_samples(&f32_samples, format, 1).unwrap();
            assert_eq!(
                re_encoded, original_bytes,
                "WAV {format:?} {rate} Hz not stable through the f32 pipeline"
            );
        }
    }
}

#[test]
fn sample_matrix_polyphase_across_rates_and_formats() {
    // The polyphase path works on f32 decoded from any PCM
    // format: for each format, decode, resample 48 kHz ->
    // 96 kHz and back, and check the 1 kHz tone survives.
    for format in [
        PcmFormat::I16,
        PcmFormat::I24,
        PcmFormat::I32,
        PcmFormat::F32,
    ] {
        let f32_samples = sine(48000, 1000.0, 4800, 0.8);
        let bytes = encode_samples(&f32_samples, format, 1).unwrap();
        let decoded = decode_samples(&bytes, format, 1).unwrap();
        let input = frame(decoded, 48000, 1);
        let up = resample_with_method(&input, 96000, ResampleMethod::Polyphase).unwrap();
        assert_eq!(up.frame_count(), 9600);
        let down = resample_with_method(&up, 48000, ResampleMethod::Polyphase).unwrap();
        assert_eq!(down.frame_count(), 4800);
        // The round trip compounds two transients: the up-step
        // transient (~512 final frames) is smeared by the
        // down-step filter (~512 more), so trim the sum of both
        // delays plus 10%, not one delay. (A single delay here
        // measured -0.739 dB; the summed trim measures 0.000 dB.)
        let up_delay = PolyphaseResampler::new(48000, 96000, 1)
            .unwrap()
            .delay_in_input_frames();
        let down_delay = PolyphaseResampler::new(96000, 48000, 1)
            .unwrap()
            .delay_in_input_frames()
            / 2.0;
        let combined = (up_delay + down_delay).ceil() as usize;
        let skip = (combined + down.frame_count() / 10).max(down.frame_count() / 10);
        let (amp, _) = fit_sine(&down.samples, 48000, 1000.0, skip);
        let db = 20.0 * (amp / 0.8).log10();
        eprintln!("polyphase 48->96->48 kHz via {format:?}: tone {db:.3} dB");
        assert!(db.abs() < 0.5, "{format:?}: tone changed by {db:.3} dB");
    }
}

#[test]
fn polyphase_resampler_reports_its_design() {
    // The design parameters are observable, not hidden: the
    // reduced ratio, the tap floor, and the delay.
    let resampler = PolyphaseResampler::new(44100, 48000, 2).unwrap();
    assert_eq!(resampler.upsample_factor(), 160);
    assert_eq!(resampler.downsample_factor(), 147);
    assert_eq!(resampler.taps_per_phase(), 128);
    assert_eq!(resampler.prototype_len(), 20480);
    assert!(resampler.delay_in_input_frames() > 0.0);
    assert!(resampler.delay_in_input_frames() < 70.0);
    assert_eq!(resampler.source_rate(), 44100);
    assert_eq!(resampler.target_rate(), 48000);
    let _ = Codec::Pcm;
}
