//! PCM to DSD (delta-sigma modulation) and DSF/DFF file writing.
//!
//! The reverse of `dsd_decimator`: where the decimator low-passes
//! and downsamples a 1-bit stream to PCM, this module interpolates
//! PCM up to the DSD rate and quantises it to 1 bit with a
//! noise-shaping loop, then packs the bits into DSF or DFF files.
//!
//! # Modulator design
//!
//! Fifth-order CIFB (cascade of integrators with feedback), the
//! structure of Schreier and Temes, "Understanding Delta-Sigma
//! Data Converters", chapter 3 (CIFB loop filter) and chapter 5
//! (NTF synthesis). The noise transfer function is
//!
//! ```text
//! NTF(z) = ((1 - z^-1) / (1 - p z^-1))^5,   p = 0.85,
//! ```
//!
//! i.e. all five NTF zeros at DC (maximum in-band shaping) and
//! all five poles on the real axis at `p`. The poles are inside
//! the unit circle, so the NTF is stable as a filter; the loop's
//! nonlinear stability is governed by the NTF's out-of-band
//! gain (Lee's rule, Schreier and Temes section 5.3: keep
//! `max |NTF|` near 1.5). Here `max |NTF| = (2 / 1.85)^5 ≈ 1.47`
//! at `z = -1`, which is why `p = 0.85` was chosen: smaller `p`
//! (larger out-of-band gain) was measured to run away even on a
//! 0.5-amplitude sine, and larger `p` shapes less. The choice
//! was verified by simulation before this implementation
//! (prototype in `/tmp`, not checked in): order 5, `p = 0.85`,
//! 1 kHz sine at 0.5 amplitude, DSD64, decoded with the
//! library's own decimator, measures THD+N ≈ -77 dB.
//!
//! The CIFB feedback coefficients are NOT hand-tuned constants:
//! they are computed at construction from the NTF above. The
//! loop filter is `L(z) = (1 - NTF) / NTF = (D - B) / B` with
//! `B(z) = (1 - z^-1)^5` and `D(z) = (1 - p z^-1)^5`; writing
//! `L` as a partial fraction in the delaying-integrator variable
//! `v = 1 / (z - 1) = z^-1 / (1 - z^-1)` gives
//! `L(z) = sum_j c_j v^j`, and `(D - B) = sum_j c_j s^j (1 - s)^(5-j)`
//! with `s = z^-1` is a triangular system in the `c_j` (the
//! `s^j` coefficient of the `j`-th term is 1), solved by forward
//! substitution below. Feedback into integrator `i` is
//! `a_i = c_{6-i}`; the input gain `b_1 = (1 - p)^5` makes the
//! signal transfer function 1 at DC. No coefficient in this file
//! is a magic number from an un-cited table: `p = 0.85` is the
//! one chosen design parameter, and its effect is measured in
//! `tests/dsd_modulate.rs`.
//!
//! Two dead ends are recorded here so nobody re-tries them:
//!
//! * Error feedback with the binomial NTF `(1 - z^-1)^N` is
//!   unstable for `N >= 3`: the quantiser error grows without
//!   bound even on silence (measured: order 3 reaches
//!   `|u| ~ 4e14` on a 0.5 sine; order 5 reaches `~1e29`).
//!   Damping the zeros (`(1 - r z^-1)^N`, `r < 1`) does not fix
//!   it either (`r = 0.9` still reaches `|u| ~ 5e4`). Only
//!   orders 1–2 are stable in that topology.
//! * A 7th-order CIFB with the same pole placement was
//!   prototyped and measured worse (THD+N ≈ -69 dB at 0.5
//!   amplitude vs ≈ -77 dB for order 5), so order 5 ships.
//!
//! # Stability limits (measured, not assumed)
//!
//! The loop is stable for inputs within ±0.75, verified on
//! sine (1 kHz and 18 kHz), silence, and DC 0.5. A 0.9-amplitude
//! sine overloads it (states run away, output becomes garbage).
//! Two guards keep an overload from poisoning the rest of a
//! stream: input samples are clamped to ±0.75 (counted in
//! `clamped_samples`), and if an integrator state ever exceeds
//! ±8 the loop state is reset to zero and the event is counted
//! in `overload_count`. The clamp range is the verified-stable
//! range; it is a limiter, not a claim that louder inputs are
//! handled faithfully.
//!
//! # Interpolator
//!
//! PCM is interpolated to the DSD rate with a polyphase
//! windowed-sinc filter (Smith, "The Scientist and Engineer's
//! Guide to Digital Signal Processing", chapter 16, same method
//! as `dsd_coefficients`): Blackman window, cutoff 95% of the
//! input Nyquist frequency, 33 taps per phase (±16 input
//! samples), weights normalised to unit DC gain per phase. The
//! phase count is `dsd_rate / gcd(pcm_rate, dsd_rate)`, so the
//! same code serves the 44.1 kHz family (integer oversampling
//! ratio, e.g. 64 for 44.1 kHz to DSD64) and the 48 kHz family
//! (rational ratio, e.g. 588/10 for 48 kHz to DSD64).
//!
//! # Bit packing and containers
//!
//! `feed` returns raw DSD bytes in DSF layout: LSB-first bits,
//! channel blocks of 4096 bytes back to back, whole block groups
//! only (a trailing partial block is held for the next call).
//! `flush` emits the final partial block padded with `0x55`
//! bytes (alternating bits, i.e. silence) and ends the stream.
//! `write_dsf` wraps those bytes in a Sony DSF header;
//! `write_dff` converts them to DFF layout (MSB-first bits via
//! `reverse_bit_order`, byte-interleaved channels) and wraps
//! them in a Philips DSDIFF 1.5 header. Both writers follow the
//! same header layouts the demuxer in `dsd_stream` parses, and
//! the round-trip tests read the written files back with
//! `DsdDemuxer`.

use crate::dop::reverse_bit_order;
use crate::dsd_decimator::DsdLevel;
use crate::error::{Error, Result};
use crate::limits::{MAX_ALLOCATION_BYTES, MAX_CHANNELS};

/// Modulator order (CIFB integrator count).
const ORDER: usize = 5;
/// NTF pole radius (see the module docs). The one chosen design
/// parameter; `max |NTF| = (2 / 1.85)^5 ≈ 1.47`.
const POLE_RADIUS: f64 = 0.85;
/// Largest input magnitude the loop is verified stable for.
/// Louder samples are clamped to this range (counted).
const MAX_INPUT_AMPLITUDE: f32 = 0.75;
/// Integrator state magnitude that triggers an overload reset.
const OVERLOAD_LIMIT: f64 = 8.0;
/// Windowed-sinc half-width in input samples (33 taps/phase).
const HALF_WINDOW: i64 = 16;
/// The same half-width as an f64, for the Blackman window.
const HALF_WINDOW_F: f64 = 16.0;
/// Taps per interpolator phase (`2 * HALF_WINDOW + 1`).
const TAPS_PER_PHASE: usize = 33;
/// DSF block size per channel in bytes (Sony DSF spec).
const DSF_BLOCK_BYTES: usize = 4096;
/// Padding byte for the final partial block: alternating bits
/// (LSB-first `1,0,1,0,...`), i.e. silence, not DC.
const SILENCE_PAD_BYTE: u8 = 0x55;

/// Binomial coefficient `n choose k`, computed exactly in u64
/// then converted (`n <= 5` here, so the value is at most 252).
#[allow(
    clippy::cast_precision_loss,
    reason = "binomial coefficients for n <= 5 are at most 252, exactly representable in f64"
)]
fn binomial(n: u64, k: u64) -> f64 {
    let mut result = 1u64;
    for i in 0..k {
        result = result * (n - i) / (i + 1);
    }
    result as f64
}

/// CIFB coefficients for the NTF in the module docs.
///
/// Returns `(a, b1)` where `a[i]` (`1..=ORDER`) is the feedback
/// gain into integrator `i` and `b1` is the input gain. Both
/// are computed from `POLE_RADIUS` by the triangular solve
/// described in the module docs; no value is looked up.
fn cifb_coefficients() -> ([f64; ORDER + 1], f64) {
    // Polynomials in s = z^-1, ascending coefficients:
    // B(s) = (1 - s)^5, D(s) = (1 - p s)^5. The p powers are
    // built by repeated multiplication, not powi.
    let mut b_poly = [0.0f64; ORDER + 1];
    let mut d_poly = [0.0f64; ORDER + 1];
    let mut p_pow = 1.0f64;
    for k in 0..=ORDER {
        let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
        let comb = binomial(ORDER as u64, k as u64);
        b_poly[k] = sign * comb;
        d_poly[k] = sign * comb * p_pow;
        p_pow *= POLE_RADIUS;
    }
    // diff = D - B; solve diff = sum_j c_j s^j (1 - s)^(5-j).
    let mut remaining = [0.0f64; ORDER + 1];
    for k in 0..=ORDER {
        remaining[k] = d_poly[k] - b_poly[k];
    }
    let mut c = [0.0f64; ORDER + 1];
    for j in 1..=ORDER {
        // The s^j coefficient of term j is 1, so c_j is the
        // remaining s^j coefficient; subtract the whole term.
        c[j] = remaining[j];
        for k in 0..=(ORDER - j) {
            let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
            remaining[j + k] -= c[j] * sign * binomial((ORDER - j) as u64, k as u64);
        }
    }
    // Feedback into integrator i is c_{6-i}.
    let mut a = [0.0f64; ORDER + 1];
    for i in 1..=ORDER {
        a[i] = c[ORDER + 1 - i];
    }
    // b1 = D(1) = (1 - p)^5, so STF(1) = b1 / D(1) = 1.
    let mut b1 = 1.0f64;
    for _ in 0..ORDER {
        b1 *= 1.0 - POLE_RADIUS;
    }
    (a, b1)
}

/// One channel's CIFB loop state: five delaying integrators.
/// `u[n]` is the last integrator's output; the quantiser is
/// `sign(u)` (+1 for `u >= 0`, -1 below).
#[derive(Debug, Clone)]
struct ModulatorChannel {
    states: [f64; ORDER],
}

impl ModulatorChannel {
    fn new() -> Self {
        Self {
            states: [0.0; ORDER],
        }
    }

    /// Runs one DSD sample. Returns `true` for a 1 bit, `false`
    /// for a 0 bit. `overload` is set when the state had to be
    /// reset (see the module docs).
    fn step(&mut self, input: f64, a: &[f64; ORDER + 1], b1: f64, overload: &mut bool) -> bool {
        let u = self.states[ORDER - 1];
        let bit = u >= 0.0;
        let y = if bit { 1.0 } else { -1.0 };
        let old = self.states;
        self.states[0] += b1 * input - a[1] * y;
        for i in 1..ORDER {
            self.states[i] += old[i - 1] - a[i + 1] * y;
        }
        // Overload guard: a runaway loop leaves finite, bounded
        // state behind instead of poisoning the rest of the
        // stream with garbage bits.
        if self
            .states
            .iter()
            .any(|&s| !s.is_finite() || s.abs() > OVERLOAD_LIMIT)
        {
            self.states = [0.0; ORDER];
            *overload = true;
        }
        bit
    }
}

/// PCM to DSD modulator, streaming over `feed` calls like
/// `DsdDecimator` (one call does not buffer a whole file).
///
/// Constructed for one DSD level, PCM input rate and channel
/// count. `feed` takes interleaved f32 PCM and returns raw DSD
/// bytes in DSF layout (LSB-first, 4096-byte channel blocks);
/// `flush` emits the final partial block and ends the stream.
/// Wrap the concatenated output with `write_dsf` or `write_dff`
/// to make a file.
#[derive(Debug)]
pub struct DsdModulator {
    level: DsdLevel,
    pcm_rate: u32,
    channels: usize,
    /// `dsd_rate / gcd(pcm_rate, dsd_rate)`: phase count of the
    /// polyphase interpolator, and the oversampling ratio when
    /// the PCM rate divides the DSD rate.
    up: u64,
    /// `pcm_rate / gcd(pcm_rate, dsd_rate)`.
    down: u64,
    /// Interpolator taps, one 33-tap phase per row.
    phases: Vec<Vec<f32>>,
    /// CIFB feedback gains (`a[1..=ORDER]`) and input gain.
    feedback: [f64; ORDER + 1],
    input_gain: f64,
    /// Per-channel interpolator history (fed PCM samples) and
    /// the stream index of `history[ch][0]`.
    history: Vec<Vec<f32>>,
    history_base: u64,
    /// PCM frames fed so far.
    fed_frames: u64,
    /// DSD samples emitted so far (per channel).
    emitted: u64,
    /// Current phase row, `(emitted * down) % up`.
    phase: u64,
    /// Per-channel loop state.
    modulators: Vec<ModulatorChannel>,
    /// Bits packed so far in the current byte, per channel.
    current_byte: Vec<u8>,
    bits_in_byte: Vec<u8>,
    /// Completed bytes per channel, up to one 4096-byte block.
    channel_bytes: Vec<Vec<u8>>,
    /// Input samples clamped to the stable range.
    clamped_samples: u64,
    /// Overload resets (see the module docs).
    overloads: u64,
    /// Set by `flush`; feeding after a flush is rejected.
    flushed: bool,
}

/// Blackman window at distance `d` from the centre of a
/// `2 * HALF_WINDOW + 1` span: 0 at both ends, 1 at the centre
/// (same formula as `dsd_coefficients::blackman`, re-derived
/// here because that helper is private to its module).
fn blackman_at(d: f64) -> f64 {
    let phase = std::f64::consts::PI * (d + HALF_WINDOW_F) / (2.0 * HALF_WINDOW_F);
    0.42 - 0.5 * (2.0 * phase).cos() + 0.08 * (4.0 * phase).cos()
}

/// Greatest common divisor (Euclid's algorithm).
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let rest = a % b;
        a = b;
        b = rest;
    }
    a
}

impl DsdModulator {
    /// Creates a modulator for `level` (which fixes the DSD
    /// rate), PCM input rate `sample_rate`, and `channels`
    /// channels.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a zero channel count, a
    /// channel count above 8, or a PCM rate outside the two
    /// supported families (44.1/88.2/176.4/352.8 kHz, or
    /// 48/96/192 kHz). The 48 kHz family is supported through
    /// the rational-ratio interpolator, not by resampling to
    /// 44.1 kHz first.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "phase counts and tap indices stay far below 2^53; taps are filter weights in [-1, 1] stored as f32, the sample format's own precision"
    )]
    pub fn new(level: DsdLevel, sample_rate: u32, channels: u32) -> Result<Self> {
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(Error::InvalidArgument(
                "dsd channel count must be between 1 and 8",
            ));
        }
        // The PCM rate must be in one of the two families; the
        // interpolator needs a rational ratio to the DSD rate,
        // and rates outside these families have none that a
        // caller could expect (e.g. 32 kHz or 22.05 kHz).
        let family_44100 = [44_100, 88_200, 176_400, 352_800];
        let family_48000 = [48_000, 96_000, 192_000];
        if !family_44100.contains(&sample_rate) && !family_48000.contains(&sample_rate) {
            return Err(Error::InvalidArgument(
                "dsd modulator pcm rate must be 44.1/88.2/176.4/352.8 khz or 48/96/192 khz",
            ));
        }
        let dsd_rate = u64::from(level.sample_rate());
        let pcm_rate = u64::from(sample_rate);
        let divisor = gcd(pcm_rate, dsd_rate);
        let up = dsd_rate / divisor;
        let down = pcm_rate / divisor;
        // Polyphase taps: one row per phase, 33 taps per row.
        // The phase count is at most 4704 (48 kHz to DSD512),
        // so the table is at most 4704 * 33 * 4 bytes.
        let up_usize = usize::try_from(up)
            .map_err(|_| Error::Overflow("dsd interpolator phase count overflows"))?;
        let table_bytes = up_usize
            .checked_mul(TAPS_PER_PHASE)
            .and_then(|n| n.checked_mul(4))
            .ok_or(Error::Overflow("dsd interpolator table size overflows"))?;
        if table_bytes > MAX_ALLOCATION_BYTES {
            return Err(Error::AllocationLimit {
                requested: table_bytes,
                limit: MAX_ALLOCATION_BYTES,
            });
        }
        let cutoff_hz = 0.95 * (f64::from(sample_rate) / 2.0);
        let mut phases: Vec<Vec<f32>> = Vec::with_capacity(up_usize);
        for row in 0..up {
            let frac = (row * down % up) as f64 / up as f64;
            let mut taps = Vec::with_capacity(TAPS_PER_PHASE);
            let mut sum = 0.0f64;
            for k in -HALF_WINDOW..=HALF_WINDOW {
                let distance = k as f64 - frac;
                let x = 2.0 * std::f64::consts::PI * cutoff_hz * distance / f64::from(sample_rate);
                let sinc = if distance == 0.0 { 1.0 } else { x.sin() / x };
                let weight = sinc * blackman_at(distance);
                taps.push(weight);
                sum += weight;
            }
            // Normalise to unit DC gain per phase, so a
            // constant input interpolates to the same constant.
            if sum != 0.0 {
                for tap in &mut taps {
                    *tap /= sum;
                }
            }
            phases.push(taps.iter().map(|&tap| tap as f32).collect());
        }
        let (feedback, input_gain) = cifb_coefficients();
        Ok(Self {
            level,
            pcm_rate: sample_rate,
            channels: channels as usize,
            up,
            down,
            phases,
            feedback,
            input_gain,
            history: vec![Vec::new(); channels as usize],
            history_base: 0,
            fed_frames: 0,
            emitted: 0,
            phase: 0,
            modulators: (0..channels as usize)
                .map(|_| ModulatorChannel::new())
                .collect(),
            current_byte: vec![0u8; channels as usize],
            bits_in_byte: vec![0u8; channels as usize],
            channel_bytes: vec![Vec::new(); channels as usize],
            clamped_samples: 0,
            overloads: 0,
            flushed: false,
        })
    }

    /// The DSD level this modulator produces.
    #[must_use]
    pub fn level(&self) -> DsdLevel {
        self.level
    }

    /// The DSD sample rate in Hz.
    #[must_use]
    pub fn dsd_rate(&self) -> u32 {
        self.level.sample_rate()
    }

    /// The PCM input rate in Hz.
    #[must_use]
    pub fn pcm_rate(&self) -> u32 {
        self.pcm_rate
    }

    /// Channel count.
    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// DSD samples emitted per channel so far (including the
    /// flush tail after `flush`).
    #[must_use]
    pub fn dsd_samples_emitted(&self) -> u64 {
        self.emitted
    }

    /// Input samples clamped to the verified-stable ±0.75 range.
    #[must_use]
    pub fn clamped_samples(&self) -> u64 {
        self.clamped_samples
    }

    /// Overload resets so far (see the module docs). Zero on a
    /// well-behaved stream.
    #[must_use]
    pub fn overload_count(&self) -> u64 {
        self.overloads
    }

    /// Interpolated value of one channel at the next output
    /// position. Samples outside the fed range read as zero
    /// (stream edges), which is what `flushing` relies on.
    #[allow(
        clippy::cast_possible_wrap,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "centre stays below the fed frame count and tap_index below 33, both far below 2^63; relative is checked non-negative before the cast to usize"
    )]
    fn interpolated(&self, channel: usize, centre: u64, taps: &[f32]) -> f32 {
        let mut acc = 0.0f32;
        let history = &self.history[channel];
        for (tap_index, &tap) in taps.iter().enumerate() {
            // Tap k = tap_index - HALF_WINDOW sits at input
            // sample centre + k.
            let index = centre as i64 + tap_index as i64 - HALF_WINDOW;
            let relative = index - self.history_base as i64;
            let sample = if relative >= 0 && (relative as usize) < history.len() {
                history[relative as usize]
            } else {
                0.0
            };
            acc += tap * sample;
        }
        acc
    }

    /// Emits DSD samples while the next output's interpolation
    /// window is covered by fed input (`flushing` also allows
    /// windows that run past the fed end, reading zeros). Stops
    /// when the next centre reaches the last real input sample.
    /// Returns the completed DSF block groups produced.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss,
        reason = "phase stays below the phase count (at most 4704); centres and history bases stay far below 2^63, bounded by the fed frames held in memory"
    )]
    fn emit_available(&mut self, flushing: bool) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let centre = self.emitted * self.down / self.up;
            if centre >= self.fed_frames {
                break;
            }
            // The window needs samples up to centre + 16; while
            // streaming, wait for the next feed to provide them.
            let last_needed = centre + HALF_WINDOW as u64;
            if !flushing && last_needed >= self.fed_frames {
                break;
            }
            let taps = &self.phases[self.phase as usize];
            let mut overload = false;
            for channel in 0..self.channels {
                let value = self.interpolated(channel, centre, taps);
                let clamped = value.clamp(-MAX_INPUT_AMPLITUDE, MAX_INPUT_AMPLITUDE);
                if value < -MAX_INPUT_AMPLITUDE || value > MAX_INPUT_AMPLITUDE {
                    self.clamped_samples += 1;
                }
                let bit = self.modulators[channel].step(
                    f64::from(clamped),
                    &self.feedback,
                    self.input_gain,
                    &mut overload,
                );
                if overload {
                    self.overloads += 1;
                }
                let bit_index = self.bits_in_byte[channel];
                if bit {
                    self.current_byte[channel] |= 1 << bit_index;
                }
                self.bits_in_byte[channel] += 1;
                if self.bits_in_byte[channel] == 8 {
                    self.channel_bytes[channel].push(self.current_byte[channel]);
                    self.current_byte[channel] = 0;
                    self.bits_in_byte[channel] = 0;
                }
            }
            self.emitted += 1;
            self.phase = (self.phase + self.down) % self.up;
            // Drop input no later output can use: the next
            // centre minus the half window.
            let next_centre = self.emitted * self.down / self.up;
            let drop_before = next_centre as i64 - HALF_WINDOW;
            if drop_before > self.history_base as i64 {
                let drop = (drop_before - self.history_base as i64) as usize;
                let drop = drop.min(self.history[0].len());
                for channel_history in &mut self.history {
                    channel_history.drain(..drop);
                }
                self.history_base += drop as u64;
            }
            // A completed block group is one 4096-byte block
            // per channel, back to back (DSF layout).
            if self
                .channel_bytes
                .iter()
                .all(|bytes| bytes.len() >= DSF_BLOCK_BYTES)
            {
                for channel in 0..self.channels {
                    out.extend_from_slice(&self.channel_bytes[channel][..DSF_BLOCK_BYTES]);
                    self.channel_bytes[channel].drain(..DSF_BLOCK_BYTES);
                }
            }
            // Bound the per-call output: a feed that produces
            // more than the allocation cap is refused rather
            // than buffered without bound.
            if out.len() > MAX_ALLOCATION_BYTES {
                return Err(Error::AllocationLimit {
                    requested: out.len(),
                    limit: MAX_ALLOCATION_BYTES,
                });
            }
        }
        Ok(out)
    }

    /// Feeds interleaved f32 PCM frames and returns the
    /// completed DSF-layout block groups produced. Short feeds
    /// may return nothing: the first outputs appear only after
    /// the interpolation window (16 input samples) is covered,
    /// and a trailing partial block is held for the next call.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the slice is not a whole
    /// number of frames, holds a non-finite sample, or arrives
    /// after `flush`, and `AllocationLimit` when the produced
    /// bytes would exceed the per-call allocation limit.
    pub fn feed(&mut self, pcm: &[f32]) -> Result<Vec<u8>> {
        if self.flushed {
            return Err(Error::InvalidArgument(
                "dsd modulator was flushed; create a new modulator for the next stream",
            ));
        }
        if pcm.len() % self.channels != 0 {
            return Err(Error::InvalidArgument(
                "dsd modulator input is not a whole number of frames",
            ));
        }
        if pcm.iter().any(|&sample| !sample.is_finite()) {
            return Err(Error::InvalidArgument(
                "dsd modulator input holds a non-finite sample",
            ));
        }
        let frames = pcm.len() / self.channels;
        // Bound the output before producing it: one DSD bit per
        // input frame per `up / down` ratio, per channel.
        let out_bits = (frames as u64)
            .checked_mul(self.up)
            .and_then(|n| n.checked_div(self.down))
            .and_then(|n| n.checked_mul(self.channels as u64))
            .ok_or(Error::Overflow("dsd modulator output size overflows"))?;
        if out_bits / 8 > MAX_ALLOCATION_BYTES as u64 {
            return Err(Error::AllocationLimit {
                requested: (out_bits / 8) as usize,
                limit: MAX_ALLOCATION_BYTES,
            });
        }
        for (index, &sample) in pcm.iter().enumerate() {
            self.history[index % self.channels].push(sample);
        }
        self.fed_frames += frames as u64;
        self.emit_available(false)
    }

    /// Flushes the interpolation tail and the final partial
    /// block (padded with silence bytes) and ends the stream.
    /// Returns the remaining DSF-layout bytes, possibly empty
    /// for an empty stream.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when called twice.
    pub fn flush(&mut self) -> Result<Vec<u8>> {
        if self.flushed {
            return Err(Error::InvalidArgument("dsd modulator was already flushed"));
        }
        self.flushed = true;
        let mut out = self.emit_available(true)?;
        // Pad the partial byte of each channel with the silence
        // pattern (bit j set when j is even), then pad each
        // channel block to 4096 bytes with silence bytes.
        let mut pending = false;
        for channel in 0..self.channels {
            if self.bits_in_byte[channel] > 0 {
                for bit_index in self.bits_in_byte[channel]..8 {
                    if bit_index % 2 == 0 {
                        self.current_byte[channel] |= 1 << bit_index;
                    }
                }
                self.channel_bytes[channel].push(self.current_byte[channel]);
                self.current_byte[channel] = 0;
                self.bits_in_byte[channel] = 0;
            }
            if !self.channel_bytes[channel].is_empty() {
                pending = true;
            }
        }
        if pending {
            for channel in 0..self.channels {
                while self.channel_bytes[channel].len() < DSF_BLOCK_BYTES {
                    self.channel_bytes[channel].push(SILENCE_PAD_BYTE);
                }
                out.extend_from_slice(&self.channel_bytes[channel]);
                self.channel_bytes[channel].clear();
            }
        }
        Ok(out)
    }
}

/// DSF channel type (Sony DSF spec, `fmt ` chunk): 1 mono,
/// 2 stereo, 3 three channels, 4 quad, 5 four channels,
/// 6 five channels, 7 5.1 channels. No type is defined for
/// 8 channels, so 8-channel DSF is refused below rather than
/// given a neighbouring type.
fn dsf_channel_type(channels: u32) -> Result<u32> {
    match channels {
        1..=7 => Ok(channels),
        _ => Err(Error::Unsupported(
            "dsf writer supports 1-7 channels (no dsf channel type is defined for 8 channels)",
        )),
    }
}

/// Validates raw DSF-layout bytes from `DsdModulator::feed` /
/// `flush`: a whole number of 4096-byte block groups, one block
/// per channel.
fn check_dsf_layout_bytes(bytes: &[u8], channels: u32) -> Result<()> {
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(Error::InvalidArgument(
            "dsd channel count must be between 1 and 8",
        ));
    }
    let group = DSF_BLOCK_BYTES
        .checked_mul(channels as usize)
        .ok_or(Error::Overflow("dsd block group size overflows"))?;
    if bytes.len() % group != 0 {
        return Err(Error::InvalidArgument(
            "dsd bytes are not a whole number of dsf block groups (4096 bytes per channel)",
        ));
    }
    Ok(())
}

/// Wraps raw DSF-layout bytes (as produced by `DsdModulator`)
/// in a Sony DSF file: `DSD ` chunk, `fmt ` chunk (version 1,
/// format id 0, LSB-first 1-bit DSD), `data` chunk.
///
/// `sample_count` is the DSD samples per channel the file
/// declares; it must fit the bytes supplied (the demuxer
/// rejects a file whose data chunk is shorter than the declared
/// count).
///
/// # Errors
///
/// Returns `InvalidArgument` for a bad layout, a channel count
/// outside 1..=8, or a sample count the bytes cannot hold,
/// `Unsupported` for 8 channels (no DSF channel type), and
/// `AllocationLimit` when the file would exceed the per-call
/// allocation limit.
pub fn write_dsf(
    bytes: &[u8],
    level: DsdLevel,
    channels: u32,
    sample_count: u64,
) -> Result<Vec<u8>> {
    check_dsf_layout_bytes(bytes, channels)?;
    let channel_type = dsf_channel_type(channels)?;
    let bits_per_channel = (bytes.len() / channels as usize) as u64 * 8;
    if sample_count > bits_per_channel {
        return Err(Error::InvalidArgument(
            "dsf sample count exceeds the dsd bytes supplied",
        ));
    }
    let total = 28u64 + 52 + 12 + bytes.len() as u64;
    let total_usize =
        usize::try_from(total).map_err(|_| Error::Overflow("dsf file size overflows"))?;
    if total_usize > MAX_ALLOCATION_BYTES {
        return Err(Error::AllocationLimit {
            requested: total_usize,
            limit: MAX_ALLOCATION_BYTES,
        });
    }
    let mut out = Vec::with_capacity(total_usize);
    out.extend_from_slice(b"DSD ");
    out.extend_from_slice(&28u64.to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&52u64.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&channel_type.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&level.sample_rate().to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&sample_count.to_le_bytes());
    // Block size per channel: the same 4096 as DSF_BLOCK_BYTES,
    // written as a u32 (the fmt chunk's field width).
    out.extend_from_slice(&4096u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(12u64 + bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(out)
}

/// DFF channel IDs (Philips DSDIFF 1.5 section 3.2.2): stereo
/// is `SLFT`/`SRGT`; a single channel is the centre channel
/// `C   `. Longer layouts use the multichannel IDs (`MLFT`,
/// `MRGT`, `LS  `, `RS  `, `LFE `), which this writer does not
/// build yet.
fn dff_channel_ids(channels: u32) -> Result<Vec<[u8; 4]>> {
    match channels {
        1 => Ok(vec![*b"C   "]),
        2 => Ok(vec![*b"SLFT", *b"SRGT"]),
        _ => Err(Error::Unsupported(
            "dff writer supports mono and stereo only (multichannel ids are not built yet)",
        )),
    }
}

fn dff_chunk(id: [u8; 4], payload: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&id);
    out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    out.extend_from_slice(payload);
}

/// Wraps raw DSF-layout bytes (as produced by `DsdModulator`)
/// in a Philips DSDIFF 1.5 file: `FRM8` envelope, `FVER`,
/// `PROP` (`SND ` with `FS  `, `CHNL`, `CMPR`), `DSD ` chunk.
/// The bytes are converted from DSF layout to DFF layout
/// (MSB-first bits via `reverse_bit_order`, channels
/// byte-interleaved) before wrapping.
///
/// # Errors
///
/// Returns `InvalidArgument` for a bad layout or a channel
/// count outside 1..=8, `Unsupported` for more than 2 channels,
/// and `AllocationLimit` when the file would exceed the per-call
/// allocation limit.
pub fn write_dff(bytes: &[u8], level: DsdLevel, channels: u32) -> Result<Vec<u8>> {
    check_dsf_layout_bytes(bytes, channels)?;
    let ids = dff_channel_ids(channels)?;
    let channels_usize = channels as usize;
    // De-block into per-channel MSB-first bytes, then
    // interleave one byte per channel (DFF Clustered Frame).
    let group = DSF_BLOCK_BYTES * channels_usize;
    let mut per_channel: Vec<Vec<u8>> = vec![Vec::new(); channels_usize];
    for block_group in bytes.chunks_exact(group) {
        for (channel, target) in per_channel.iter_mut().enumerate() {
            let start = channel * DSF_BLOCK_BYTES;
            let block = &block_group[start..start + DSF_BLOCK_BYTES];
            target.extend_from_slice(&reverse_bit_order(block));
        }
    }
    let mut data = Vec::with_capacity(bytes.len());
    let blocks = bytes.len() / group;
    for block in 0..blocks {
        for byte in 0..DSF_BLOCK_BYTES {
            for channel_data in &per_channel {
                data.push(channel_data[block * DSF_BLOCK_BYTES + byte]);
            }
        }
    }
    let mut prop = b"SND ".to_vec();
    dff_chunk(*b"FS  ", &level.sample_rate().to_be_bytes(), &mut prop);
    let channel_count =
        u16::try_from(channels).map_err(|_| Error::Overflow("dff channel count overflows"))?;
    let mut chnl = channel_count.to_be_bytes().to_vec();
    for id in &ids {
        chnl.extend_from_slice(id);
    }
    dff_chunk(*b"CHNL", &chnl, &mut prop);
    dff_chunk(*b"CMPR", b"DSD ", &mut prop);
    let mut body = Vec::new();
    dff_chunk(*b"FVER", &0x0105_0000u32.to_be_bytes(), &mut body);
    dff_chunk(*b"PROP", &prop, &mut body);
    dff_chunk(*b"DSD ", &data, &mut body);
    let total = 16usize
        .checked_add(body.len())
        .ok_or(Error::Overflow("dff file size overflows"))?;
    if total > MAX_ALLOCATION_BYTES {
        return Err(Error::AllocationLimit {
            requested: total,
            limit: MAX_ALLOCATION_BYTES,
        });
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"FRM8");
    out.extend_from_slice(&(4u64 + body.len() as u64).to_be_bytes());
    out.extend_from_slice(b"DSD ");
    out.extend_from_slice(&body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cifb_coefficients_match_the_prototype_design() {
        // Order 5, p = 0.85, computed by the same triangular
        // solve in an independent Python check (exact values:
        // a_1 = (1-p)^5, a_5 = 5(1-p), the rest in between).
        // The loose tolerance covers f64 rounding differences
        // between the two implementations.
        let (a, b1) = cifb_coefficients();
        let expected = [0.0, 7.593_75e-05, 0.002_531_25, 0.033_75, 0.225, 0.75];
        for i in 1..=ORDER {
            assert!(
                (a[i] - expected[i]).abs() < 1e-6,
                "a[{i}] = {} (expected {})",
                a[i],
                expected[i]
            );
        }
        assert!((b1 - 0.15f64.powi(5)).abs() < 1e-12, "b1 = {b1}");
    }

    #[test]
    fn unknown_pcm_rate_is_rejected() {
        let err = DsdModulator::new(DsdLevel::Dsd64, 32_000, 2).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    fn zero_channels_is_rejected() {
        let err = DsdModulator::new(DsdLevel::Dsd64, 44_100, 0).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    fn non_finite_input_is_rejected() {
        let mut modulator = DsdModulator::new(DsdLevel::Dsd64, 44_100, 1).unwrap();
        let err = modulator.feed(&[f32::NAN, 0.0]).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        reason = "test byte counts stay far below 2^53"
    )]
    fn silence_modulates_to_alternating_bits() {
        // Silence must average to zero: the first block group
        // of a silent stream holds roughly as many 1 bits as
        // 0 bits, and no overload is recorded.
        let mut modulator = DsdModulator::new(DsdLevel::Dsd64, 44_100, 1).unwrap();
        let pcm = vec![0.0f32; 4410];
        let mut bytes = modulator.feed(&pcm).unwrap();
        bytes.extend(modulator.flush().unwrap());
        let ones: usize = bytes.iter().map(|b| b.count_ones() as usize).sum();
        let total = bytes.len() * 8;
        let ratio = ones as f64 / total as f64;
        assert!((ratio - 0.5).abs() < 0.05, "silence one-bit ratio {ratio}");
        assert_eq!(modulator.overload_count(), 0);
    }

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        reason = "test sample indices stay far below 2^24"
    )]
    fn dsf_writer_round_trips_through_the_demuxer() {
        use crate::dsd_stream::DsdDemuxer;
        let mut modulator = DsdModulator::new(DsdLevel::Dsd64, 44_100, 2).unwrap();
        let pcm: Vec<f32> = (0..4410)
            .flat_map(|i| {
                let phase = 2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 44_100.0;
                [0.5 * phase.sin(), 0.5 * (phase * 1.5).sin()]
            })
            .collect();
        let mut bytes = modulator.feed(&pcm).unwrap();
        bytes.extend(modulator.flush().unwrap());
        let samples = modulator.dsd_samples_emitted();
        let file = write_dsf(&bytes, DsdLevel::Dsd64, 2, samples).unwrap();
        let demuxer = DsdDemuxer::new(std::io::Cursor::new(file)).unwrap();
        assert_eq!(demuxer.header().sample_rate, 2_822_400);
        assert_eq!(demuxer.header().channels, 2);
        assert_eq!(demuxer.header().sample_count, samples);
    }

    #[test]
    fn dff_writer_round_trips_through_the_demuxer() {
        use crate::dsd_stream::DsdDemuxer;
        let mut modulator = DsdModulator::new(DsdLevel::Dsd64, 44_100, 2).unwrap();
        let pcm = vec![0.0f32; 4410 * 2];
        let mut bytes = modulator.feed(&pcm).unwrap();
        bytes.extend(modulator.flush().unwrap());
        let file = write_dff(&bytes, DsdLevel::Dsd64, 2).unwrap();
        let demuxer = DsdDemuxer::new(std::io::Cursor::new(file)).unwrap();
        assert_eq!(demuxer.header().sample_rate, 2_822_400);
        assert_eq!(demuxer.header().channels, 2);
    }
}
