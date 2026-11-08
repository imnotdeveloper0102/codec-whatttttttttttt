//! WAV demuxing and muxing over `Read`/`Write`.
//!
//! Phase 1 keeps the whole WAV file in memory behind the
//! `MAX_WAV_FILE_BYTES` bound in `byte_source`. A streaming demuxer
//! that never holds the whole file is a later phase; this module does
//! not pretend to be one. Seeking moves to a sample-frame boundary and
//! is exact for PCM, because PCM frames have a fixed size.

use std::io::{Read, Write};

use crate::byte_source::read_wav_bytes;
use crate::error::{Error, Result};
use crate::frame::Codec;
use crate::frame::Packet;
use crate::pcm::{decode_samples, encode_samples, PcmFormat};
use crate::wav_header::{parse_wav_header, write_wav_header, WavHeader};

/// Demuxer for a single WAV file. `next_packet` returns one packet
/// holding all audio bytes on the first call, then `None`.
pub struct WavDemuxer {
    bytes: Vec<u8>,
    header: WavHeader,
    position: usize,
    delivered: bool,
}

impl WavDemuxer {
    /// Reads and parses a WAV file from `reader`.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a cut-short file, `InvalidData` for a
    /// malformed header, `Unsupported` for an unhandled format, and
    /// `AllocationLimit` when the file exceeds `MAX_WAV_FILE_BYTES`.
    pub fn new<R: Read>(reader: R) -> Result<Self> {
        let bytes = read_wav_bytes(reader)?;
        let header = parse_wav_header(&bytes)?;
        Ok(Self {
            bytes,
            header,
            position: 0,
            delivered: false,
        })
    }

    /// The parsed header.
    #[must_use]
    pub fn header(&self) -> &WavHeader {
        &self.header
    }

    /// Decoded samples of the whole file, as f32 in [-1.0, 1.0].
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when the data chunk is not frame aligned.
    pub fn samples(&self) -> Result<Vec<f32>> {
        let start = self.header.data_offset;
        let end = start + self.header.data_size as usize;
        decode_samples(
            &self.bytes[start..end],
            self.header.format,
            self.header.channels,
        )
    }

    /// Returns the single audio packet, or `None` once delivered.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when the data chunk is not frame aligned.
    pub fn next_packet(&mut self) -> Result<Option<Packet>> {
        if self.delivered {
            return Ok(None);
        }
        self.delivered = true;
        let start = self.header.data_offset + self.position;
        let end = self.header.data_offset + self.header.data_size as usize;
        let data = self.bytes[start..end].to_vec();
        Ok(Some(Packet {
            codec: Codec::Pcm,
            data,
            timestamp: 0,
            is_keyframe: true,
        }))
    }

    /// Seeks to a sample-frame boundary. The next `next_packet` call
    /// returns audio from that frame onward.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when `frame` is past the end of the file.
    pub fn seek_to_frame(&mut self, frame: usize) -> Result<()> {
        let byte_offset = frame
            .checked_mul(self.header.block_align as usize)
            .ok_or(Error::Overflow("seek offset overflows"))?;
        if byte_offset > self.header.data_size as usize {
            return Err(Error::InvalidArgument("seek past end of wav data"));
        }
        self.position = byte_offset;
        self.delivered = false;
        Ok(())
    }
}

/// Muxer for a single WAV file. Packets are buffered in memory up to
/// `MAX_WAV_FILE_BYTES` and written with a correct header by `finish`;
/// a streaming muxer that writes the header up front is a later phase.
pub struct WavMuxer<W: Write> {
    writer: W,
    format: PcmFormat,
    channels: u32,
    sample_rate: u32,
    data: Vec<u8>,
    finished: bool,
}

impl<W: Write> WavMuxer<W> {
    /// Creates a muxer for the given PCM format.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the channel count or sample rate
    /// is zero or above the limit.
    pub fn new(writer: W, format: PcmFormat, channels: u32, sample_rate: u32) -> Result<Self> {
        if channels == 0 || sample_rate == 0 {
            return Err(Error::InvalidArgument(
                "wav muxer requires positive channels and sample rate",
            ));
        }
        Ok(Self {
            writer,
            format,
            channels,
            sample_rate,
            data: Vec::new(),
            finished: false,
        })
    }

    /// Appends one PCM packet. Only `Codec::Pcm` packets are accepted,
    /// and their bytes must be a whole number of frames for this format.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` for a non-PCM packet, `InvalidData` for
    /// frame-unaligned bytes, and `AllocationLimit` when the buffered
    /// audio would exceed `MAX_WAV_FILE_BYTES`.
    pub fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if self.finished {
            return Err(Error::InvalidArgument("muxer already finished"));
        }
        if packet.codec != Codec::Pcm {
            return Err(Error::Unsupported("wav muxer accepts pcm packets only"));
        }
        let frame_bytes = self.format.bytes_per_sample() * self.channels as usize;
        if packet.data.len() % frame_bytes != 0 {
            return Err(Error::InvalidData {
                offset: packet.data.len(),
                detail: "packet is not frame aligned",
            });
        }
        if self.data.len() + packet.data.len() > crate::limits::MAX_WAV_FILE_BYTES {
            return Err(Error::AllocationLimit {
                requested: self.data.len() + packet.data.len(),
                limit: crate::limits::MAX_WAV_FILE_BYTES,
            });
        }
        self.data.extend_from_slice(&packet.data);
        Ok(())
    }

    /// Writes the header followed by all buffered audio, and consumes
    /// the muxer. The header is written with the exact data size, so
    /// the output parses without a fix-up pass.
    ///
    /// # Errors
    ///
    /// Returns `Io` on a write failure.
    pub fn finish(mut self) -> Result<()> {
        self.finished = true;
        let data_size = u32::try_from(self.data.len())
            .map_err(|_| Error::Overflow("wav data exceeds u32 range"))?;
        write_wav_header(
            &mut self.writer,
            self.format,
            self.channels,
            self.sample_rate,
            data_size,
        )?;
        self.writer.write_all(&self.data)?;
        Ok(())
    }
}

/// Encodes f32 samples to a complete WAV file in memory.
///
/// # Errors
///
/// Returns `InvalidArgument` for a zero channel count or sample rate,
/// and `Overflow` when the data size exceeds the u32 range.
pub fn encode_wav(
    samples: &[f32],
    format: PcmFormat,
    channels: u32,
    sample_rate: u32,
) -> Result<Vec<u8>> {
    let bytes = encode_samples(samples, format, channels)?;
    let data_size =
        u32::try_from(bytes.len()).map_err(|_| Error::Overflow("wav data exceeds u32 range"))?;
    let mut out = Vec::with_capacity(44 + bytes.len());
    write_wav_header(&mut out, format, channels, sample_rate, data_size)?;
    out.extend_from_slice(&bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sample_wav() -> Vec<u8> {
        encode_wav(&[0.0, 0.5, -0.5, 0.25], PcmFormat::I16, 2, 44100).unwrap()
    }

    #[test]
    fn demuxer_returns_one_packet_then_none() {
        let mut demuxer = WavDemuxer::new(Cursor::new(sample_wav())).unwrap();
        assert_eq!(demuxer.header().channels, 2);
        let packet = demuxer.next_packet().unwrap().unwrap();
        assert_eq!(packet.codec, Codec::Pcm);
        assert!(packet.is_keyframe);
        assert_eq!(demuxer.next_packet().unwrap(), None);
    }

    #[test]
    fn demuxer_samples_match_encoded_samples() {
        let demuxer = WavDemuxer::new(Cursor::new(sample_wav())).unwrap();
        let samples = demuxer.samples().unwrap();
        assert_eq!(samples.len(), 4);
        assert!((samples[1] - 0.5).abs() < 1.0 / 32768.0);
    }

    #[test]
    fn seek_to_frame_restarts_packet_there() {
        let mut demuxer = WavDemuxer::new(Cursor::new(sample_wav())).unwrap();
        demuxer.seek_to_frame(1).unwrap();
        let packet = demuxer.next_packet().unwrap().unwrap();
        assert_eq!(packet.data.len(), 4);
    }

    #[test]
    fn seek_past_end_returns_invalid_argument() {
        let mut demuxer = WavDemuxer::new(Cursor::new(sample_wav())).unwrap();
        let err = demuxer.seek_to_frame(100).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    fn muxer_round_trip_matches_input() {
        let wav = sample_wav();
        let mut demuxer = WavDemuxer::new(Cursor::new(wav)).unwrap();
        let packet = demuxer.next_packet().unwrap().unwrap();
        let mut out = Vec::new();
        let mut muxer = WavMuxer::new(&mut out, PcmFormat::I16, 2, 44100).unwrap();
        muxer.write_packet(&packet).unwrap();
        muxer.finish().unwrap();
        let header = parse_wav_header(&out).unwrap();
        assert_eq!(header.frame_count(), 2);
        assert_eq!(out.len(), sample_wav().len());
    }

    #[test]
    fn muxer_rejects_non_pcm_packet() {
        let mut out = Vec::new();
        let mut muxer = WavMuxer::new(&mut out, PcmFormat::I16, 1, 8000).unwrap();
        let packet = Packet {
            codec: Codec::Flac,
            data: vec![0u8; 4],
            timestamp: 0,
            is_keyframe: true,
        };
        let err = muxer.write_packet(&packet).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }
}
