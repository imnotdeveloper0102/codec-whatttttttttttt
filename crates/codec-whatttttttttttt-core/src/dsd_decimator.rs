//! DSD (1-bit sigma-delta) to PCM decimation.
//!
//! A DSD stream is a 1-bit pulse-density signal at 2.8224 MHz and
//! multiples of it. Decoding it to PCM is a low-pass filter plus
//! downsampling: the filter removes the shaped quantization noise
//! above the audio band, and the downsampler keeps one output
//! sample per `decimation` input samples. The filter taps live in
//! `dsd_coefficients` (windowed-sinc, Blackman window); this
//! module only applies them, so the filter design and its
//! application can be reviewed separately.
//!
//! Two output families are supported:
//!
//! * 44.1 kHz family (44.1/88.2/176.4/352.8/705.6 kHz): the DSD
//!   rate is an integer multiple of every one of these, so the
//!   conversion is a single integer decimation.
//! * 48 kHz family (48/96/192 kHz): the DSD rate is first
//!   decimated by 16 to the 44.1 kHz family's top rate
//!   (176.4/352.8/705.6 kHz, or 1.4112 MHz for DSD512), then a
//!   windowed-sinc rational resampler carries it to the 48 kHz
//!   rate. That stage is implemented here, in this module only,
//!   and stays here even though the shared `polyphase.rs`
//!   resampler now exists: the shared one is stateless per call
//!   (it treats its input as a whole signal with zero padding
//!   at the edges), while this one is streaming — it keeps
//!   per-channel history across `feed` calls and drops only
//!   input no later output can use. Routing a long DSD stream
//!   through the shared resampler would either buffer the whole
//!   decimated stream (past the 64 MiB per-call allocation cap
//!   for anything longer than seconds) or call it once per
//!   block and seam the zero-padding transient at every block
//!   boundary. The filter also differs on purpose (on-the-fly
//!   Blackman windowed sinc, normalised by the weights actually
//!   used, so DC gain stays 1 at truncated edges), and the DSD
//!   quality numbers were measured for this design.
//!
//! PCM to DSD (delta-sigma modulation) is deliberately NOT here:
//! it is scheduled last in the current round, and this module
//! must not grow an encoder by accident.

use crate::dsd_coefficients::{decimation_taps, resample_cutoff_hz, RESAMPLE_HALF_WINDOW};
use crate::error::{Error, Result};
use crate::limits::MAX_ALLOCATION_BYTES;

/// DSD sample rates. The level is derived from the declared
/// sample rate, never from a file name or a container label:
/// 2.8224 MHz is DSD64, 5.6448 MHz DSD128, 11.2896 MHz DSD256,
/// 22.5792 MHz DSD512 (each a multiple of 44.1 kHz).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DsdLevel {
    /// 2.8224 MHz.
    Dsd64,
    /// 5.6448 MHz.
    Dsd128,
    /// 11.2896 MHz.
    Dsd256,
    /// 22.5792 MHz.
    Dsd512,
}

impl DsdLevel {
    /// The level for a declared DSD sample rate, or `None` when
    /// the rate matches no DSD level.
    #[must_use]
    pub fn from_sample_rate(sample_rate: u32) -> Option<Self> {
        match sample_rate {
            2_822_400 => Some(Self::Dsd64),
            5_644_800 => Some(Self::Dsd128),
            11_289_600 => Some(Self::Dsd256),
            22_579_200 => Some(Self::Dsd512),
            _ => None,
        }
    }

    /// The DSD sample rate in Hz.
    #[must_use]
    pub fn sample_rate(self) -> u32 {
        match self {
            Self::Dsd64 => 2_822_400,
            Self::Dsd128 => 5_644_800,
            Self::Dsd256 => 11_289_600,
            Self::Dsd512 => 22_579_200,
        }
    }

    /// Short name, as used in errors and logs.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Dsd64 => "dsd64",
            Self::Dsd128 => "dsd128",
            Self::Dsd256 => "dsd256",
            Self::Dsd512 => "dsd512",
        }
    }

    /// Bytes per second of raw DSD data for `channels` channels:
    /// one bit per sample per channel. DSD512 stereo is
    /// `22_579_200` / 8 * 2 = `5_644_800` B/s.
    #[must_use]
    pub fn bytes_per_second(self, channels: u32) -> u64 {
        u64::from(self.sample_rate()) / 8 * u64::from(channels)
    }
}

/// Bit order of DSD bytes within one byte. DSF stores LSB first
/// (Sony DSF spec, annotation 4); DFF stores MSB first (Philips
/// DSDIFF 1.5, section 2.2: a Channel Byte holds 8 consecutive
/// DSD samples, most significant bit first in time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOrder {
    /// Bit 0 of the byte is the earliest sample.
    LsbFirst,
    /// Bit 7 of the byte is the earliest sample.
    MsbFirst,
}

/// Layout of one DSD data block, as delivered by a demuxer.
/// The decimator never assumes a whole file is in memory: a
/// block is one DSF block group or one DFF chunk slice, and any
/// trailing partial block is buffered until the next call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdBlockLayout {
    /// DSF: `block_size_per_channel` bytes per channel, channel
    /// blocks back to back, LSB-first bits.
    Dsf {
        /// Bytes per channel in one block group (4096 in
        /// practice, taken from the DSF `fmt ` chunk).
        block_size_per_channel: usize,
    },
    /// DFF: channel bytes interleaved, one byte per channel per
    /// 8-sample group, MSB-first bits.
    Dff,
}

/// Converts DSD bytes to f32 samples in [-1.0, 1.0]: bit 1 is
/// +1.0, bit 0 is -1.0. A DSD stream of alternating bits (silence)
/// averages to 0, and a stream of all ones is full-scale DC.
#[must_use]
pub fn dsd_bits_to_f32(bytes: &[u8], order: BitOrder) -> Vec<f32> {
    let mut out = Vec::with_capacity(bytes.len() * 8);
    for &byte in bytes {
        for bit_index in 0..8 {
            let shift = match order {
                BitOrder::LsbFirst => bit_index,
                BitOrder::MsbFirst => 7 - bit_index,
            };
            out.push(if byte >> shift & 1 == 1 { 1.0 } else { -1.0 });
        }
    }
    out
}

/// One channel's decimation state: a mirrored ring buffer
/// holding the last `taps.len()` input samples twice, so an
/// output sample's whole filter window is always one
/// contiguous slice. Sample `n` lives at
/// `ring[n % taps.len()]` and at `ring[n % taps.len() +
/// taps.len()]`; the mirror copy is what keeps the window
/// contiguous instead of wrapping mid-window. Without it the
/// hot loop paid one integer modulo per tap; measured
/// 2026-11-08, that capped the decimator at 0.0-0.1x realtime
/// (DSD512 to 352.8 kHz took 3.9 s for 0.1 s of audio).
#[derive(Debug)]
struct DecimationChannel {
    /// Mirrored ring of the last `taps.len()` input samples.
    ring: Vec<f32>,
    /// Stream index of `ring[0]`'s oldest slot is derived from
    /// `fed`: sample `n` lives at `ring[n % taps.len()]`.
    fed: u64,
    /// Output samples already emitted.
    emitted: u64,
}

impl DecimationChannel {
    fn new(taps_len: usize) -> Self {
        Self {
            ring: vec![0.0; taps_len * 2],
            fed: 0,
            emitted: 0,
        }
    }

    /// Pushes input samples, returning the decimated outputs
    /// whose filter window is now complete. Output `m` uses
    /// input samples `[m * decimation, m * decimation + taps)`,
    /// so it becomes computable once `fed` reaches
    /// `m * decimation + taps`.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "ring slots are always below the tap count (at most 16384)"
    )]
    fn push(&mut self, samples: &[f32], taps: &[f32], decimation: u64) -> Vec<f32> {
        let taps_len = taps.len() as u64;
        let taps_usize = taps.len();
        let mut out = Vec::new();
        for &sample in samples {
            let slot = (self.fed % taps_len) as usize;
            self.ring[slot] = sample;
            self.ring[slot + taps_usize] = sample;
            self.fed += 1;
            // The window for output `emitted` ends at
            // `emitted * decimation + taps`; while it is covered,
            // emit. The loop runs at most once per input sample
            // for decimation >= 1, but stays a loop so the
            // invariant holds for any decimation value. At
            // emission time `fed` is exactly
            // `emitted * decimation + taps`, so the window is
            // the last `taps` samples and `base + tap_index`
            // never runs past the mirrored ring.
            while self.fed >= self.emitted * decimation + taps_len {
                let start = self.emitted * decimation;
                let base = (start % taps_len) as usize;
                // Four partial sums break the serial dependency
                // chain of a single accumulator: a scalar f32
                // multiply-add stalls the pipeline for its
                // latency on every tap, so one accumulator caps
                // the loop at one tap per latency period.
                let mut acc0 = 0.0f32;
                let mut acc1 = 0.0f32;
                let mut acc2 = 0.0f32;
                let mut acc3 = 0.0f32;
                let mut tap_index = 0;
                while tap_index + 4 <= taps.len() {
                    acc0 += taps[tap_index] * self.ring[base + tap_index];
                    acc1 += taps[tap_index + 1] * self.ring[base + tap_index + 1];
                    acc2 += taps[tap_index + 2] * self.ring[base + tap_index + 2];
                    acc3 += taps[tap_index + 3] * self.ring[base + tap_index + 3];
                    tap_index += 4;
                }
                let mut acc = (acc0 + acc1) + (acc2 + acc3);
                while tap_index < taps.len() {
                    acc += taps[tap_index] * self.ring[base + tap_index];
                    tap_index += 1;
                }
                out.push(acc);
                self.emitted += 1;
            }
        }
        out
    }

    /// Flushes the tail: zero-pads the input so the remaining
    /// outputs (up to the filter's group delay) are emitted.
    /// Only outputs whose window starts before the last real
    /// input sample are emitted; a window starting at or after
    /// the end would be all zeros.
    fn flush(&mut self, taps: &[f32], decimation: u64) -> Vec<f32> {
        let real_fed = self.fed;
        let mut out = Vec::new();
        while self.emitted * decimation < real_fed {
            out.extend(self.push(&[0.0], taps, decimation));
        }
        out
    }
}

/// Windowed-sinc rational resampler (local to this module; see
/// the module docs). Converts between two PCM rates whose ratio
/// is rational, e.g. 176.4 kHz to 48 kHz (147/40). Output `m`
/// sits at input position `m * down / up`; its value is the
/// windowed sinc centred there, normalised by the sum of the
/// weights actually used, which keeps the DC gain at 1 even at
/// the stream edges where the window is truncated.
#[derive(Debug)]
struct RationalResampler {
    in_rate: u32,
    /// `up / down` reduced by the GCD, so `out = in * up / down`.
    up: u64,
    down: u64,
    cutoff_hz: f64,
    /// Per-channel input history, oldest first.
    history: Vec<Vec<f32>>,
    /// Stream index of `history[ch][0]`.
    base: u64,
    /// Output samples already emitted.
    emitted: u64,
}

impl RationalResampler {
    fn new(in_rate: u32, out_rate: u32, channels: usize) -> Self {
        let gcd = {
            let mut a = u64::from(in_rate);
            let mut b = u64::from(out_rate);
            while b != 0 {
                let rest = a % b;
                a = b;
                b = rest;
            }
            a
        };
        Self {
            in_rate,
            up: u64::from(out_rate) / gcd,
            down: u64::from(in_rate) / gcd,
            cutoff_hz: resample_cutoff_hz(in_rate, out_rate),
            history: vec![Vec::new(); channels],
            base: 0,
            emitted: 0,
        }
    }

    /// Pushes interleaved frames, returning the resampled
    /// frames whose window is fully inside the buffered input.
    fn push(&mut self, frames: &[f32], channels: usize) -> Vec<f32> {
        for (index, &sample) in frames.iter().enumerate() {
            self.history[index % channels].push(sample);
        }
        self.emit_available(channels, false)
    }

    /// Zero-pads the input so the tail outputs are emitted.
    fn flush(&mut self, channels: usize) -> Vec<f32> {
        let pad = RESAMPLE_HALF_WINDOW + 1;
        for channel in &mut self.history {
            channel.extend(std::iter::repeat(0.0).take(pad));
        }
        self.emit_available(channels, true)
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        clippy::cast_possible_wrap,
        reason = "indices stay inside the buffered history and rates stay below 1.5 MHz, so every cast here is exact"
    )]
    fn emit_available(&mut self, channels: usize, flushing: bool) -> Vec<f32> {
        let mut out = Vec::new();
        let half = RESAMPLE_HALF_WINDOW as i64;
        loop {
            // Centre of output `emitted`, in input samples.
            let centre_num = self.emitted * self.down;
            let centre = centre_num / self.up;
            let frac = centre_num % self.up;
            let first_needed = centre as i64 - half;
            let last_needed = centre as i64 + half;
            let available = self.history[0].len() as i64;
            let last_index = self.base as i64 + available - 1;
            if last_needed > last_index && !flushing {
                break;
            }
            if first_needed > last_index {
                break;
            }
            // The window may run past the buffered end while
            // flushing; missing samples count as zero, and the
            // normalisation below keeps the gain at 1.
            let mut frame = Vec::with_capacity(channels);
            for channel in 0..channels {
                let mut acc = 0.0f64;
                let mut weight_sum = 0.0f64;
                for offset in -half..=half {
                    let index = centre as i64 + offset;
                    let relative = index - self.base as i64;
                    let sample = if relative >= 0 && relative < available {
                        f64::from(self.history[channel][relative as usize])
                    } else {
                        0.0
                    };
                    // Distance from the centre in input samples,
                    // including the fractional phase.
                    let distance = offset as f64 - frac as f64 / self.up as f64;
                    let x = 2.0 * std::f64::consts::PI * self.cutoff_hz * distance
                        / f64::from(self.in_rate);
                    let sinc = if distance == 0.0 { 1.0 } else { x.sin() / x };
                    // Blackman window over the 2*half+1 span.
                    let window_phase =
                        std::f64::consts::PI * (distance + half as f64) / (2.0 * half as f64);
                    let window =
                        0.42 - 0.5 * (2.0 * window_phase).cos() + 0.08 * (4.0 * window_phase).cos();
                    let weight = sinc * window;
                    acc += weight * sample;
                    weight_sum += weight;
                }
                let value = if weight_sum == 0.0 {
                    0.0
                } else {
                    acc / weight_sum
                };
                frame.push(value as f32);
            }
            out.extend_from_slice(&frame);
            self.emitted += 1;
            // Drop input that no later output can use: the next
            // centre minus the half window.
            let next_centre = (self.emitted * self.down) / self.up;
            let drop_before = next_centre as i64 - half;
            if drop_before > self.base as i64 {
                let drop = (drop_before - self.base as i64) as usize;
                let drop = drop.min(self.history[0].len());
                for channel in &mut self.history {
                    channel.drain(..drop);
                }
                self.base += drop as u64;
            }
            // While flushing, stop once the centre has moved
            // past the last real input sample.
            if flushing && centre > last_index as u64 {
                break;
            }
        }
        out
    }
}

/// DSD to PCM decimator.
///
/// Constructed for one DSD rate, channel count and PCM target
/// rate. `feed` takes raw DSD blocks in file layout and returns
/// interleaved f32 PCM at the target rate; `flush` emits the
/// filter tail. The first outputs appear only after the filter's
/// group delay has passed, and the last outputs only on flush:
/// a caller that stops after `feed` loses the tail, which is
/// why `Decoder::flush` calls this.
#[derive(Debug)]
pub struct DsdDecimator {
    dsd_rate: u32,
    channels: usize,
    /// Target PCM rate.
    target_rate: u32,
    /// Intermediate 44.1 kHz-family rate (the direct decimation
    /// target; equal to `target_rate` for that family).
    intermediate_rate: u32,
    decimation: u64,
    taps: Vec<f32>,
    channels_state: Vec<DecimationChannel>,
    /// Present only for the 48 kHz family.
    resampler: Option<RationalResampler>,
    /// Bytes of a partial block held for the next `feed`.
    pending: Vec<u8>,
    /// Layout of the blocks passed to `feed`.
    layout: DsdBlockLayout,
}

impl DsdDecimator {
    /// Creates a decimator.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a zero channel count, a
    /// channel count above 8, a DSD rate that matches no level,
    /// or a target rate outside the two supported families
    /// (44.1/88.2/176.4/352.8/705.6 kHz, or 48/96/192 kHz), and
    /// `AllocationLimit` when the filter itself would exceed
    /// the per-call allocation limit.
    pub fn new(
        dsd_rate: u32,
        channels: u32,
        target_rate: u32,
        layout: DsdBlockLayout,
    ) -> Result<Self> {
        if channels == 0 || channels > 8 {
            return Err(Error::InvalidArgument(
                "dsd channel count must be between 1 and 8",
            ));
        }
        let level = DsdLevel::from_sample_rate(dsd_rate).ok_or(Error::InvalidArgument(
            "dsd sample rate matches no dsd level (2.8224/5.6448/11.2896/22.5792 MHz)",
        ))?;
        let _ = level;
        // The 44.1 kHz family divides the DSD rate exactly.
        let family_44100 = [44_100, 88_200, 176_400, 352_800, 705_600];
        let family_48000 = [48_000, 96_000, 192_000];
        let intermediate_rate;
        let decimation: u64;
        let resampler;
        if family_44100.contains(&target_rate) {
            if dsd_rate % target_rate != 0 {
                return Err(Error::InvalidArgument(
                    "target rate is not an integer divisor of the dsd rate",
                ));
            }
            intermediate_rate = target_rate;
            decimation = u64::from(dsd_rate / target_rate);
            resampler = None;
        } else if family_48000.contains(&target_rate) {
            // Decimate by 16 to the 44.1 kHz family's top rate
            // first (176.4 kHz for DSD64, 352.8 kHz for DSD128,
            // 705.6 kHz for DSD256, 1.4112 MHz for DSD512), then
            // resample rationally to the 48 kHz rate.
            intermediate_rate = dsd_rate / 16;
            decimation = 16;
            resampler = Some(RationalResampler::new(
                intermediate_rate,
                target_rate,
                channels as usize,
            ));
        } else {
            return Err(Error::InvalidArgument(
                "dsd target rate must be 44.1/88.2/176.4/352.8/705.6 kHz or 48/96/192 kHz",
            ));
        }
        let taps = decimation_taps(
            dsd_rate,
            u32::try_from(decimation)
                .map_err(|_| Error::InvalidArgument("dsd decimation factor exceeds u32"))?,
        );
        if taps.len() * 4 > MAX_ALLOCATION_BYTES {
            return Err(Error::AllocationLimit {
                requested: taps.len() * 4,
                limit: MAX_ALLOCATION_BYTES,
            });
        }
        let channels_state = (0..channels as usize)
            .map(|_| DecimationChannel::new(taps.len()))
            .collect();
        Ok(Self {
            dsd_rate,
            channels: channels as usize,
            target_rate,
            intermediate_rate,
            decimation,
            taps,
            channels_state,
            resampler,
            pending: Vec::new(),
            layout,
        })
    }

    /// The DSD input rate in Hz.
    #[must_use]
    pub fn dsd_rate(&self) -> u32 {
        self.dsd_rate
    }

    /// The PCM output rate in Hz.
    #[must_use]
    pub fn target_rate(&self) -> u32 {
        self.target_rate
    }

    /// The intermediate 44.1 kHz-family rate the DSD stream is
    /// decimated to before any 48 kHz-family resampling.
    #[must_use]
    pub fn intermediate_rate(&self) -> u32 {
        self.intermediate_rate
    }

    /// Channel count.
    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Feeds one block of raw DSD bytes in the layout given at
    /// construction and returns interleaved f32 PCM at the
    /// target rate. A trailing partial block is buffered, not
    /// decoded, until the next call completes it.
    ///
    /// # Errors
    ///
    /// Returns `AllocationLimit` when the decoded output would
    /// exceed the per-call allocation limit.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the decimation factor is at most 512"
    )]
    pub fn feed(&mut self, block: &[u8]) -> Result<Vec<f32>> {
        self.pending.extend_from_slice(block);
        let unit = match self.layout {
            DsdBlockLayout::Dsf {
                block_size_per_channel,
            } => block_size_per_channel
                .checked_mul(self.channels)
                .ok_or(Error::Overflow("dsf block size overflows"))?,
            DsdBlockLayout::Dff => self.channels,
        };
        if unit == 0 {
            return Err(Error::InvalidArgument("dsd block unit is zero"));
        }
        let complete = self.pending.len() / unit * unit;
        if complete == 0 {
            return Ok(Vec::new());
        }
        let bytes: Vec<u8> = self.pending.drain(..complete).collect();
        // Bound the output before allocating it: one output
        // frame per `decimation` input samples per channel.
        let input_samples = bytes.len() / self.channels * 8;
        let out_frames = input_samples / self.decimation as usize + 1;
        let out_samples = out_frames
            .checked_mul(self.channels)
            .ok_or(Error::Overflow("dsd output frame count overflows"))?;
        if out_samples * 4 > MAX_ALLOCATION_BYTES {
            return Err(Error::AllocationLimit {
                requested: out_samples * 4,
                limit: MAX_ALLOCATION_BYTES,
            });
        }
        let order = match self.layout {
            DsdBlockLayout::Dsf { .. } => BitOrder::LsbFirst,
            DsdBlockLayout::Dff => BitOrder::MsbFirst,
        };
        // Deinterleave the block into per-channel sample runs.
        let mut per_channel: Vec<Vec<f32>> = Vec::with_capacity(self.channels);
        match self.layout {
            DsdBlockLayout::Dsf {
                block_size_per_channel,
            } => {
                for channel in 0..self.channels {
                    let start = channel * block_size_per_channel;
                    let end = start + block_size_per_channel;
                    let mut channel_bytes = Vec::new();
                    for chunk in bytes.chunks_exact(unit) {
                        channel_bytes.extend_from_slice(&chunk[start..end]);
                    }
                    per_channel.push(dsd_bits_to_f32(&channel_bytes, order));
                }
            }
            DsdBlockLayout::Dff => {
                let mut channel_bytes: Vec<Vec<u8>> = vec![Vec::new(); self.channels];
                for chunk in bytes.chunks_exact(self.channels) {
                    for (target, &byte) in channel_bytes.iter_mut().zip(chunk.iter()) {
                        target.push(byte);
                    }
                }
                per_channel.extend(
                    channel_bytes
                        .iter()
                        .map(|bytes| dsd_bits_to_f32(bytes, order)),
                );
            }
        }
        // Decimate per channel, then interleave the outputs.
        let mut decimated: Vec<Vec<f32>> = Vec::with_capacity(self.channels);
        for (channel, samples) in per_channel.iter().enumerate() {
            let out = self.channels_state[channel].push(samples, &self.taps, self.decimation);
            decimated.push(out);
        }
        let frames = decimated[0].len();
        let mut interleaved = Vec::with_capacity(frames * self.channels);
        for frame in 0..frames {
            for channel_samples in &decimated {
                interleaved.push(channel_samples[frame]);
            }
        }
        if let Some(resampler) = &mut self.resampler {
            Ok(resampler.push(&interleaved, self.channels))
        } else {
            Ok(interleaved)
        }
    }

    /// Flushes the filter tail (and the resampler tail for the
    /// 48 kHz family) and returns the remaining PCM.
    ///
    /// # Errors
    ///
    /// Returns `AllocationLimit` when the tail would exceed the
    /// per-call allocation limit.
    pub fn flush(&mut self) -> Result<Vec<f32>> {
        // A partial block left in `pending` is incomplete data:
        // it holds fewer than 8 samples per channel and cannot
        // be decoded as a whole block, so it is dropped rather
        // than decoded as a truncated frame.
        self.pending.clear();
        let mut decimated: Vec<Vec<f32>> = Vec::with_capacity(self.channels);
        for channel_state in &mut self.channels_state {
            decimated.push(channel_state.flush(&self.taps, self.decimation));
        }
        let frames = decimated[0].len();
        if frames
            .checked_mul(self.channels)
            .map_or(true, |n| n * 4 > MAX_ALLOCATION_BYTES)
        {
            return Err(Error::AllocationLimit {
                requested: frames * self.channels * 4,
                limit: MAX_ALLOCATION_BYTES,
            });
        }
        let mut interleaved = Vec::with_capacity(frames * self.channels);
        for frame in 0..frames {
            for channel_samples in &decimated {
                interleaved.push(channel_samples[frame]);
            }
        }
        if let Some(resampler) = &mut self.resampler {
            let mut out = resampler.push(&interleaved, self.channels);
            out.extend(resampler.flush(self.channels));
            Ok(out)
        } else {
            Ok(interleaved)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_from_sample_rate_matches_all_four_levels() {
        assert_eq!(DsdLevel::from_sample_rate(2_822_400), Some(DsdLevel::Dsd64));
        assert_eq!(
            DsdLevel::from_sample_rate(5_644_800),
            Some(DsdLevel::Dsd128)
        );
        assert_eq!(
            DsdLevel::from_sample_rate(11_289_600),
            Some(DsdLevel::Dsd256)
        );
        assert_eq!(
            DsdLevel::from_sample_rate(22_579_200),
            Some(DsdLevel::Dsd512)
        );
        assert_eq!(DsdLevel::from_sample_rate(3_000_000), None);
        assert_eq!(DsdLevel::from_sample_rate(44_100), None);
    }

    #[test]
    fn dsd512_stereo_bytes_per_second_is_5_644_800() {
        assert_eq!(DsdLevel::Dsd512.bytes_per_second(2), 5_644_800);
        assert_eq!(DsdLevel::Dsd64.bytes_per_second(2), 705_600);
    }

    #[test]
    fn bits_to_f32_respects_bit_order() {
        // 0x01 LSB-first: first sample is 1, the rest are 0.
        let lsb = dsd_bits_to_f32(&[0x01], BitOrder::LsbFirst);
        assert_eq!(lsb[0], 1.0);
        // The converter only ever emits the exact literals
        // +1.0 and -1.0, so an epsilon check is exact here.
        assert!(lsb[1..].iter().all(|&v| (v + 1.0).abs() < 1e-6));
        // 0x01 MSB-first: the 1 is the last sample of the byte.
        let msb = dsd_bits_to_f32(&[0x01], BitOrder::MsbFirst);
        assert!(msb[..7].iter().all(|&v| (v + 1.0).abs() < 1e-6));
        assert_eq!(msb[7], 1.0);
    }

    #[test]
    fn unknown_rate_is_rejected() {
        let err = DsdDecimator::new(3_000_000, 2, 44_100, DsdBlockLayout::Dff).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    fn unsupported_target_rate_is_rejected() {
        let err = DsdDecimator::new(2_822_400, 2, 32_000, DsdBlockLayout::Dff).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }
}
