//! Vorbis audio packet decode (Vorbis I specification section 4.3).
//!
//! A packet holds one mode, a window, one floor curve per channel, one
//! residue per submap, and (for coupled channels) inverse coupling.
//! The spectrum is floor times residue, converted to time-domain
//! samples by the inverse MDCT and windowed, then overlap-added with
//! the second half of the previous block (section 4.3.8).
//!
//! The first packet primes the decoder and returns no output: the
//! overlap-add needs a previous block, and the first packet's samples
//! are the encoder's look-ahead. The caller must therefore expect
//! `None` from the first `decode_packet` call.

use crate::error::{Error, Result};
use crate::lsb_bitreader::{ilog, LsbBitReader};
use crate::mdct::mdct_inverse;
use crate::vorbis_setup::{Floor, Mapping, VorbisSetup};

/// One decoded Vorbis packet: the overlap-add output for this packet.
#[derive(Debug, Clone, PartialEq)]
pub struct VorbisDecodedPacket {
    /// Interleaved output samples.
    pub samples: Vec<f32>,
    /// Channels.
    pub channels: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Current blocksize in samples.
    pub blocksize: usize,
    /// Previous blocksize in samples.
    pub previous_blocksize: usize,
}

/// One decoded floor curve per channel (`None` when the floor
/// is unused) plus one `no_residue` flag per channel.
type FloorDecode = (Vec<Option<Vec<f32>>>, Vec<bool>);

/// Stateful Vorbis packet decoder: holds the setup and the
/// overlap-add tail (the second half of the previous block, already
/// windowed) from the previous packet.
#[derive(Debug)]
pub struct VorbisPacketDecoder {
    setup: VorbisSetup,
    /// Second half of the previous block, per channel. `None` before
    /// the first packet.
    previous: Option<Vec<Vec<f32>>>,
    /// Blocksize of the previous packet (0 before the first).
    previous_blocksize: usize,
}

impl VorbisPacketDecoder {
    /// Creates a packet decoder from a parsed setup.
    #[must_use]
    pub fn new(setup: VorbisSetup) -> Self {
        Self {
            setup,
            previous: None,
            previous_blocksize: 0,
        }
    }

    /// The parsed setup this decoder uses.
    #[must_use]
    pub fn setup(&self) -> &VorbisSetup {
        &self.setup
    }

    /// Floor decode (section 4.3.2) plus nonzero propagation
    /// (section 4.3.3). Returns one floor curve per channel
    /// (`None` when the floor is unused) and one `no_residue` flag
    /// per channel. An unused floor zeroes the channel's output
    /// and skips its residue; in a coupling pair, if either
    /// channel has residue, both do.
    fn decode_floors(
        &self,
        reader: &mut LsbBitReader<'_>,
        mapping: &Mapping,
        half: usize,
        channels: usize,
    ) -> Result<FloorDecode> {
        let mut floor_curves: Vec<Option<Vec<f32>>> = Vec::with_capacity(channels);
        let mut no_residue: Vec<bool> = vec![true; channels];
        for (channel, no_res) in no_residue.iter_mut().enumerate() {
            let submap = mapping.mux[channel] as usize;
            let floor_index = mapping.submap_floor[submap] as usize;
            match &self.setup.floors[floor_index] {
                Floor::Type0(floor0) => {
                    if let Some(curve) = floor0.decode(reader, &self.setup.codebooks, half)? {
                        floor_curves.push(Some(curve));
                        *no_res = false;
                    } else {
                        floor_curves.push(None);
                        *no_res = true;
                    }
                }
                Floor::Type1(floor1) => {
                    if let Some(curve) = floor1.decode(reader, &self.setup.codebooks, half)? {
                        floor_curves.push(Some(curve));
                        *no_res = false;
                    } else {
                        floor_curves.push(None);
                        *no_res = true;
                    }
                }
            }
        }
        // Nonzero propagation (section 4.3.3): in a coupling pair,
        // if either channel has residue, both do.
        for &(magnitude, angle) in &mapping.coupling_steps {
            let magnitude = magnitude as usize;
            let angle = angle as usize;
            if !no_residue[magnitude] || !no_residue[angle] {
                no_residue[magnitude] = false;
                no_residue[angle] = false;
            }
        }
        Ok((floor_curves, no_residue))
    }

    /// Decodes one audio packet. Returns `Ok(None)` for the first
    /// packet, which only primes the decoder (section 4.3.8).
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when the packet ends before the mode or
    /// window flags are read, `InvalidData` when the packet-type
    /// bit is set, the mode index is out of range, or a window is
    /// inconsistent.
    pub fn decode_packet(&mut self, data: &[u8]) -> Result<Option<VorbisDecodedPacket>> {
        if data.is_empty() {
            return Err(Error::Truncated { needed: 1, had: 0 });
        }
        let mut reader = LsbBitReader::new(data);
        // Section 4.3.1: the packet-type bit must be 0 for audio.
        if reader.read_bit()? {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "Vorbis audio packet type bit must be 0",
            });
        }
        let mode_bits = ilog(u32::try_from(self.setup.modes.len()).unwrap_or(0) - 1);
        let mode_index = reader.read_bits(mode_bits)? as usize;
        let mode = self.setup.modes.get(mode_index).ok_or(Error::InvalidData {
            offset: reader.bit_position() / 8,
            detail: "Vorbis mode index exceeds mode count",
        })?;
        let blocksize = if mode.blockflag {
            self.setup.identification.blocksize_1
        } else {
            self.setup.identification.blocksize_0
        };
        let channels = self.setup.identification.channels as usize;
        // Long blocks read the previous/next window flags; short
        // blocks use the fixed half-block slopes (section 4.3.1).
        let (previous_long, next_long) = if mode.blockflag {
            (reader.read_bit()?, reader.read_bit()?)
        } else {
            (false, false)
        };
        let window = vorbis_window(
            blocksize,
            self.setup.identification.blocksize_0,
            previous_long,
            next_long,
        );
        let mapping = &self.setup.mappings[mode.mapping as usize];
        let half = blocksize / 2;

        let (floor_curves, no_residue) =
            self.decode_floors(&mut reader, mapping, half, channels)?;

        // Residue decode (section 4.3.4): one residue per submap,
        // in submap order. A channel is skipped when its mux points
        // at another submap or its floor was unused.
        let mut residue_vectors: Vec<Vec<Vec<f32>>> = Vec::with_capacity(mapping.submaps as usize);
        for submap in 0..mapping.submaps as usize {
            let residue_index = mapping.submap_residue[submap] as usize;
            let residue = &self.setup.residues[residue_index];
            let do_not_decode: Vec<bool> = (0..channels)
                .map(|channel| mapping.mux[channel] as usize != submap || no_residue[channel])
                .collect();
            let rv = residue.decode(
                &mut reader,
                &self.setup.codebooks,
                channels,
                blocksize,
                &do_not_decode,
            );
            residue_vectors.push(rv);
        }

        // Per-channel spectra: the residue vector of the channel's
        // submap.
        let mut spectra: Vec<Vec<f32>> = Vec::with_capacity(channels);
        for (channel, &mux) in mapping.mux.iter().enumerate() {
            let submap = mux as usize;
            spectra.push(residue_vectors[submap][channel].clone());
        }

        // Inverse coupling (section 4.3.5), in descending step
        // order, on the residue vectors.
        for &(magnitude, angle) in mapping.coupling_steps.iter().rev() {
            let magnitude = magnitude as usize;
            let angle = angle as usize;
            let mut magnitude_vec = std::mem::take(&mut spectra[magnitude]);
            let mut angle_vec = std::mem::take(&mut spectra[angle]);
            for i in 0..half {
                let m = magnitude_vec[i];
                let a = angle_vec[i];
                if m > 0.0 {
                    if a > 0.0 {
                        angle_vec[i] = m - a;
                    } else {
                        angle_vec[i] = m;
                        magnitude_vec[i] = m + a;
                    }
                } else if a > 0.0 {
                    angle_vec[i] = m + a;
                } else {
                    angle_vec[i] = m;
                    magnitude_vec[i] = m - a;
                }
            }
            spectra[magnitude] = magnitude_vec;
            spectra[angle] = angle_vec;
        }

        let blocks = inverse_mdct(spectra, &floor_curves, &window, blocksize, channels)?;

        let previous_blocksize = self.previous_blocksize;
        let output = self.overlap_add(&blocks, blocksize, half, channels, previous_blocksize);

        // Save the second half of this block for the next packet.
        let mut next_previous = Vec::with_capacity(channels);
        for block in &blocks {
            next_previous.push(block[half..].to_vec());
        }
        self.previous = Some(next_previous);
        self.previous_blocksize = blocksize;
        Ok(output)
    }

    /// Overlap-add (section 4.3.8). The output is
    /// `prev_blocksize`/4 + `blocksize`/4 samples: the previous
    /// block's second half (starting at its midpoint) plus the
    /// current block's first half, aligned so the two windows'
    /// transition regions coincide. The current block's first
    /// sample lands at output index (prev - cur)/4, which is
    /// negative when the current block is longer than the
    /// previous one; its window is zero there, so nothing is
    /// lost by clipping it. Block sizes are at most 8192, so the
    /// `i64` conversions are exact; `cur_index` is guarded to be
    /// non-negative before the `usize` conversion.
    fn overlap_add(
        &mut self,
        blocks: &[Vec<f32>],
        blocksize: usize,
        half: usize,
        channels: usize,
        previous_blocksize: usize,
    ) -> Option<VorbisDecodedPacket> {
        let previous = self.previous.take()?;
        let hop = (previous_blocksize + blocksize) / 4;
        let offset = (i64::try_from(previous_blocksize).unwrap_or(0)
            - i64::try_from(blocksize).unwrap_or(0))
            / 4;
        let mut interleaved = vec![0.0f32; hop * channels];
        for (channel, prev_tail) in previous.iter().enumerate() {
            let cur_first = &blocks[channel][..half];
            for i in 0..hop {
                let mut value = if i < prev_tail.len() {
                    prev_tail[i]
                } else {
                    0.0
                };
                let cur_index = i64::try_from(i).unwrap_or(0) - offset;
                if cur_index >= 0 {
                    if let Some(&sample) =
                        cur_first.get(usize::try_from(cur_index).unwrap_or(usize::MAX))
                    {
                        value += sample;
                    }
                }
                interleaved[i * channels + channel] = value;
            }
        }
        Some(VorbisDecodedPacket {
            samples: interleaved,
            channels: self.setup.identification.channels,
            sample_rate: self.setup.identification.sample_rate,
            blocksize,
            previous_blocksize,
        })
    }

    /// Returns the remaining tail at end of stream: the second half
    /// of the last block, windowed, with no next block to overlap
    /// with. Returns `None` when no block was decoded, or when the
    /// tail was already flushed. The caller appends this after the
    /// last `decode_packet` output.
    #[must_use]
    pub fn flush(&mut self) -> Option<VorbisDecodedPacket> {
        let previous = self.previous.take()?;
        let channels = self.setup.identification.channels as usize;
        let half = previous.first().map_or(0, Vec::len);
        let mut interleaved = vec![0.0f32; half * channels];
        for channel in 0..channels {
            for i in 0..half {
                interleaved[i * channels + channel] = previous[channel][i];
            }
        }
        let previous_blocksize = self.previous_blocksize;
        self.previous_blocksize = 0;
        Some(VorbisDecodedPacket {
            samples: interleaved,
            channels: self.setup.identification.channels,
            sample_rate: self.setup.identification.sample_rate,
            blocksize: 0,
            previous_blocksize,
        })
    }
}

/// Dot product (section 4.3.6) and inverse MDCT with the
/// window (section 4.3.7), per channel. Returns one time-domain
/// block per channel.
fn inverse_mdct(
    mut spectra: Vec<Vec<f32>>,
    floor_curves: &[Option<Vec<f32>>],
    window: &[f32],
    blocksize: usize,
    channels: usize,
) -> Result<Vec<Vec<f32>>> {
    let mut blocks: Vec<Vec<f32>> = Vec::with_capacity(channels);
    for channel in 0..channels {
        if let Some(curve) = &floor_curves[channel] {
            for (i, value) in spectra[channel].iter_mut().enumerate() {
                *value *= curve[i];
            }
        }
        let mut time = vec![0.0f32; blocksize];
        mdct_inverse(&spectra[channel], &mut time, window)?;
        blocks.push(time);
    }
    Ok(blocks)
}

/// Builds the window for one block (section 4.3.1, steps 1-8).
/// The slope shape is section 1.3.2: the left slope rises as
/// `sin(0.5*pi*sin^2(theta))` and the right slope falls as
/// `sin(0.5*pi*cos^2(theta))`, where `theta` runs 0..pi/2 across
/// the slope. Outside the slopes the window is 0 on the outer
/// sides and 1 in the middle.
fn vorbis_window(
    blocksize: usize,
    blocksize_0: usize,
    previous_long: bool,
    next_long: bool,
) -> Vec<f32> {
    let n = blocksize;
    let (left_start, left_end, right_start, right_end) = if blocksize == blocksize_0 {
        (0, n / 2, n / 2, n)
    } else {
        let left = if previous_long {
            (0, n / 2)
        } else {
            (n / 4 - blocksize_0 / 4, n / 4 + blocksize_0 / 4)
        };
        let right = if next_long {
            (n / 2, n)
        } else {
            (3 * n / 4 - blocksize_0 / 4, 3 * n / 4 + blocksize_0 / 4)
        };
        (left.0, left.1, right.0, right.1)
    };
    let mut window = vec![0.0f32; n];
    let left_n = left_end - left_start;
    if left_n > 0 {
        // `rel` is (i - left_start); both `rel` and `left_n` are at
        // most the blocksize (8192), so the `f32` conversion is exact.
        let left_n_f = f32::from(u16::try_from(left_n).unwrap_or(0));
        for (rel, w) in window[left_start..left_end].iter_mut().enumerate() {
            let theta = f32::from(u16::try_from(rel).unwrap_or(0)) + 0.5;
            let theta = theta / left_n_f * std::f32::consts::FRAC_PI_2;
            *w = (0.5 * std::f32::consts::PI * theta.sin().powi(2)).sin();
        }
    }
    for w in &mut window[left_end..right_start] {
        *w = 1.0;
    }
    let right_n = right_end - right_start;
    if right_n > 0 {
        let right_n_f = f32::from(u16::try_from(right_n).unwrap_or(0));
        for (rel, w) in window[right_start..right_end].iter_mut().enumerate() {
            let theta = f32::from(u16::try_from(rel).unwrap_or(0)) + 0.5;
            let theta = theta / right_n_f * std::f32::consts::FRAC_PI_2;
            *w = (0.5 * std::f32::consts::PI * theta.cos().powi(2)).sin();
        }
    }
    window
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mdct_overlap_add_reproduces_signal() {
        // Time-domain aliasing cancellation: two consecutive
        // windowed blocks, forward then inverse, overlap-added,
        // reproduce the input in the middle region. This is the
        // property the Vorbis decoder relies on, and it pins the
        // MDCT phase and the libvorbis scale split (forward
        // carries `2/N`, inverse carries none) together.
        use crate::mdct::mdct_forward;
        let n = 32usize; // spectrum length; block length 2n
        let mut x = vec![0.0f32; 3 * n];
        for (i, sample) in x.iter_mut().enumerate() {
            let t = f32::from(u16::try_from(i).unwrap_or(0))
                / f32::from(u16::try_from(3 * n).unwrap_or(0));
            *sample = (2.0 * std::f32::consts::PI * 2.0 * t).sin() * 0.6 + t * 0.3;
        }
        let window = vorbis_window(2 * n, 2 * n, false, false);
        let block0: Vec<f32> = (0..2 * n).map(|i| x[i] * window[i]).collect();
        let block1: Vec<f32> = (0..2 * n).map(|i| x[n + i] * window[i]).collect();
        let mut spec0 = vec![0.0f32; n];
        let mut spec1 = vec![0.0f32; n];
        mdct_forward(&block0, &mut spec0).unwrap();
        mdct_forward(&block1, &mut spec1).unwrap();
        let mut y0 = vec![0.0f32; 2 * n];
        let mut y1 = vec![0.0f32; 2 * n];
        mdct_inverse(&spec0, &mut y0, &window).unwrap();
        mdct_inverse(&spec1, &mut y1, &window).unwrap();
        // The window is applied exactly twice in total: once to
        // the block before the forward transform (analysis) and
        // once inside the inverse transform (synthesis). The
        // overlap-add itself adds the windowed halves without
        // applying the window a third time.
        let mut out = vec![0.0f32; 3 * n];
        for i in 0..2 * n {
            out[i] += y0[i];
            out[n + i] += y1[i];
        }
        for i in n..2 * n {
            let err = (out[i] - x[i]).abs();
            assert!(err < 2e-3, "index {i}: {} != {} (err {err})", out[i], x[i]);
        }
    }

    #[test]
    fn window_short_block_is_two_slopes() {
        let window = vorbis_window(256, 256, false, false);
        assert_eq!(window.len(), 256);
        assert!(window[0] < 0.01);
        assert!((window[127] - 1.0).abs() < 0.02);
        assert!((window[128] - 1.0).abs() < 0.02);
        assert!(window[255] < 0.01);
    }

    #[test]
    fn window_long_block_short_neighbours_has_flat_middle() {
        let window = vorbis_window(2048, 256, false, false);
        assert_eq!(window.len(), 2048);
        // Left slope ends at 256/4 + 256/4 = 576; right slope
        // starts at 3*2048/4 - 64 = 1472.
        assert_eq!(window[576], 1.0);
        assert_eq!(window[1471], 1.0);
        assert!(window[0] < 0.01);
        assert!(window[2047] < 0.01);
        assert!((window[575] - 1.0).abs() < 0.02);
        assert!((window[1472] - 1.0).abs() < 0.02);
    }

    #[test]
    fn window_long_block_long_neighbours_is_one_arch() {
        let window = vorbis_window(2048, 256, true, true);
        assert_eq!(window.len(), 2048);
        assert!(window[0] < 0.01);
        assert!((window[1023] - 1.0).abs() < 0.02);
        assert!((window[1024] - 1.0).abs() < 0.02);
        assert!(window[2047] < 0.01);
    }
}
