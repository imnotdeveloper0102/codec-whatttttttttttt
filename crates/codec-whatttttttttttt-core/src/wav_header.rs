//! RIFF/WAVE header parsing and writing.
//!
//! The parser walks the chunk list once and tolerates unknown chunks and
//! either chunk order, but it is strict where the file makes a promise:
//! a declared `data` size larger than the bytes that follow is
//! `Truncated`, a missing `fmt ` or `data` chunk is `InvalidData`, and
//! an unsupported format tag is `Unsupported`. Chunks are word-aligned,
//! so a trailing pad byte after an odd-sized chunk is skipped.

use std::io::Write;

use crate::error::{Error, Result};
use crate::limits::{MAX_CHANNELS, MAX_SAMPLE_RATE};
use crate::pcm::PcmFormat;

/// Parsed WAV header. `data_offset` is the byte offset of the first
/// audio byte; `data_size` is the declared size of the `data` chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavHeader {
    /// PCM sample format.
    pub format: PcmFormat,
    /// Number of interleaved channels.
    pub channels: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Bytes per second, as declared in the `fmt ` chunk.
    pub byte_rate: u32,
    /// Bytes per sample frame, as declared in the `fmt ` chunk.
    pub block_align: u16,
    /// Bits per sample, as declared in the `fmt ` chunk.
    pub bits_per_sample: u16,
    /// Declared size of the `data` chunk in bytes.
    pub data_size: u32,
    /// Byte offset of the first audio byte.
    pub data_offset: usize,
}

impl WavHeader {
    /// Number of sample frames in the `data` chunk.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        if self.block_align == 0 {
            return 0;
        }
        self.data_size as usize / self.block_align as usize
    }

    /// Duration in microseconds, computed in 64-bit arithmetic.
    #[must_use]
    pub fn duration_micros(&self) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        (self.frame_count() as u64 * 1_000_000) / u64::from(self.sample_rate)
    }
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data.get(offset..offset + 2).ok_or(Error::Truncated {
        needed: offset + 2,
        had: data.len(),
    })?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data.get(offset..offset + 4).ok_or(Error::Truncated {
        needed: offset + 4,
        had: data.len(),
    })?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn parse_fmt(
    data: &[u8],
    chunk_start: usize,
    chunk_size: u32,
) -> Result<(PcmFormat, u32, u32, u32, u16, u16)> {
    if chunk_size < 16 {
        return Err(Error::InvalidData {
            offset: chunk_start,
            detail: "fmt chunk smaller than 16 bytes",
        });
    }
    let audio_format = read_u16(data, chunk_start)?;
    let channels = u32::from(read_u16(data, chunk_start + 2)?);
    let sample_rate = read_u32(data, chunk_start + 4)?;
    let byte_rate = read_u32(data, chunk_start + 8)?;
    let block_align = read_u16(data, chunk_start + 12)?;
    let bits_per_sample = read_u16(data, chunk_start + 14)?;
    if channels == 0 {
        return Err(Error::InvalidData {
            offset: chunk_start + 2,
            detail: "channel count is zero",
        });
    }
    if channels > MAX_CHANNELS {
        return Err(Error::Overflow("channel count exceeds maximum"));
    }
    if sample_rate == 0 {
        return Err(Error::InvalidData {
            offset: chunk_start + 4,
            detail: "sample rate is zero",
        });
    }
    if sample_rate > MAX_SAMPLE_RATE {
        return Err(Error::Overflow("sample rate exceeds maximum"));
    }
    if block_align == 0 {
        return Err(Error::InvalidData {
            offset: chunk_start + 12,
            detail: "block align is zero",
        });
    }
    // WAVE_FORMAT_EXTENSIBLE (0xFFFE): the real format is the first u16
    // of the SubFormat GUID at fmt offset 24, and the chunk is 40 bytes.
    let (tag, bits) = if audio_format == 0xFFFE {
        if chunk_size < 40 {
            return Err(Error::InvalidData {
                offset: chunk_start,
                detail: "extensible fmt chunk smaller than 40 bytes",
            });
        }
        (read_u16(data, chunk_start + 24)?, bits_per_sample)
    } else {
        (audio_format, bits_per_sample)
    };
    let format = match (tag, bits) {
        (1, 16) => PcmFormat::I16,
        (1, 24) => PcmFormat::I24,
        (1, 32) => PcmFormat::I32,
        (3, 32) => PcmFormat::F32,
        _ => {
            return Err(Error::Unsupported("wav format tag or bit depth"));
        }
    };
    let expected_align = channels * u32::try_from(format.bytes_per_sample()).unwrap_or(u32::MAX);
    if u32::from(block_align) != expected_align {
        return Err(Error::InvalidData {
            offset: chunk_start + 12,
            detail: "block align does not match format",
        });
    }
    Ok((format, channels, sample_rate, byte_rate, block_align, bits))
}

/// Parses a RIFF/WAVE header from the start of `data`.
///
/// # Errors
///
/// Returns `Truncated` when the header or a declared chunk is cut short,
/// `InvalidData` when the magic, structure, or a declared value is wrong,
/// `Unsupported` for a format tag or bit depth this library does not
/// decode, and `Overflow` for a channel count or sample rate above the
/// limits in `limits.rs`.
pub fn parse_wav_header(data: &[u8]) -> Result<WavHeader> {
    if data.len() < 12 {
        return Err(Error::Truncated {
            needed: 12,
            had: data.len(),
        });
    }
    if &data[0..4] != b"RIFF" {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "missing RIFF magic",
        });
    }
    if &data[8..12] != b"WAVE" {
        return Err(Error::InvalidData {
            offset: 8,
            detail: "missing WAVE magic",
        });
    }
    let riff_size = read_u32(data, 4)?;
    // A riff size of 0 or 0xFFFF_FFFF means "unknown" in streamed files;
    // any other value is a promise about the file length.
    if riff_size != 0 && riff_size != u32::MAX && (riff_size as usize + 8) > data.len() {
        return Err(Error::Truncated {
            needed: riff_size as usize + 8,
            had: data.len(),
        });
    }
    let mut fmt: Option<(PcmFormat, u32, u32, u32, u16, u16)> = None;
    let mut data_chunk: Option<(u32, usize)> = None;
    let mut offset = 12usize;
    while offset + 8 <= data.len() {
        let chunk_id = &data[offset..offset + 4];
        let chunk_size = read_u32(data, offset + 4)? as usize;
        let chunk_start = offset + 8;
        // The chunk body must be present in full; a short body is a
        // truncated file, not a reason to guess at the audio.
        if chunk_start
            .checked_add(chunk_size)
            .map_or(true, |end| end > data.len())
        {
            return Err(Error::Truncated {
                needed: chunk_start + chunk_size,
                had: data.len(),
            });
        }
        match chunk_id {
            b"fmt " => {
                fmt = Some(parse_fmt(
                    data,
                    chunk_start,
                    u32::try_from(chunk_size).unwrap_or(u32::MAX),
                )?);
            }
            b"data" if data_chunk.is_none() => {
                data_chunk = Some((u32::try_from(chunk_size).unwrap_or(u32::MAX), chunk_start));
            }
            _ => {}
        }
        // Chunks are word-aligned: skip the pad byte after an odd size.
        offset = chunk_start + chunk_size + (chunk_size % 2);
    }
    let (format, channels, sample_rate, byte_rate, block_align, bits) =
        fmt.ok_or(Error::InvalidData {
            offset: 12,
            detail: "missing fmt chunk",
        })?;
    let (data_size, data_offset) = data_chunk.ok_or(Error::InvalidData {
        offset: 12,
        detail: "missing data chunk",
    })?;
    if u32::from(block_align) != 0 && data_size % u32::from(block_align) != 0 {
        return Err(Error::InvalidData {
            offset: data_offset,
            detail: "data chunk is not frame aligned",
        });
    }
    Ok(WavHeader {
        format,
        channels,
        sample_rate,
        byte_rate,
        block_align,
        bits_per_sample: bits,
        data_size,
        data_offset,
    })
}

/// Writes a 44-byte canonical WAV header for `data_size` audio bytes.
///
/// # Errors
///
/// Returns `Io` on a write failure, and `InvalidArgument` when the
/// channel count or sample rate is zero.
pub fn write_wav_header<W: Write>(
    writer: &mut W,
    format: PcmFormat,
    channels: u32,
    sample_rate: u32,
    data_size: u32,
) -> Result<()> {
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(Error::InvalidArgument(
            "wav channel count must be between 1 and 8",
        ));
    }
    if sample_rate == 0 || sample_rate > MAX_SAMPLE_RATE {
        return Err(Error::InvalidArgument("wav sample rate out of range"));
    }
    let bytes_per_sample = u32::try_from(format.bytes_per_sample()).unwrap_or(0);
    let block_align = u16::try_from(channels * bytes_per_sample).unwrap_or(u16::MAX);
    let byte_rate = sample_rate * channels * bytes_per_sample;
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&36u32.saturating_add(data_size).to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&format.wav_format_tag().to_le_bytes());
    header.extend_from_slice(&u16::try_from(channels).unwrap_or(0).to_le_bytes());
    header.extend_from_slice(&sample_rate.to_le_bytes());
    header.extend_from_slice(&byte_rate.to_le_bytes());
    header.extend_from_slice(&block_align.to_le_bytes());
    header.extend_from_slice(&format.bits_per_sample().to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_size.to_le_bytes());
    writer.write_all(&header)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcm::{decode_samples, encode_samples};

    fn minimal_wav(format: PcmFormat, channels: u32, sample_rate: u32, samples: &[f32]) -> Vec<u8> {
        let bytes = encode_samples(samples, format, channels).unwrap();
        let mut out = Vec::new();
        write_wav_header(
            &mut out,
            format,
            channels,
            sample_rate,
            u32::try_from(bytes.len()).unwrap(),
        )
        .unwrap();
        out.extend_from_slice(&bytes);
        out
    }

    #[test]
    fn parses_valid_i16_wav() {
        let wav = minimal_wav(PcmFormat::I16, 2, 44100, &[0.0, 0.5, -0.5, 0.25]);
        let header = parse_wav_header(&wav).unwrap();
        assert_eq!(header.format, PcmFormat::I16);
        assert_eq!(header.channels, 2);
        assert_eq!(header.sample_rate, 44100);
        assert_eq!(header.data_offset, 44);
        assert_eq!(header.frame_count(), 2);
        let decoded =
            decode_samples(&wav[header.data_offset..], header.format, header.channels).unwrap();
        assert_eq!(decoded.len(), 4);
    }

    #[test]
    fn truncated_header_returns_truncated() {
        let err = parse_wav_header(&[0x52, 0x49, 0x46]).unwrap_err();
        assert!(matches!(err, Error::Truncated { needed: 12, had: 3 }));
    }

    #[test]
    fn bad_magic_returns_invalid_data() {
        let mut wav = minimal_wav(PcmFormat::I16, 1, 8000, &[0.0]);
        wav[0] = b'X';
        let err = parse_wav_header(&wav).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn declared_data_larger_than_file_returns_truncated() {
        let mut wav = minimal_wav(PcmFormat::I16, 1, 8000, &[0.0, 0.5]);
        // Inflate the data chunk size field (offset 40) past the file end.
        wav[40..44].copy_from_slice(&10_000u32.to_le_bytes());
        let err = parse_wav_header(&wav).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn missing_data_chunk_returns_invalid_data() {
        // Header only: RIFF + WAVE + fmt chunk, no data chunk.
        let mut out = Vec::new();
        write_wav_header(&mut out, PcmFormat::I16, 1, 8000, 0).unwrap();
        let mut header_only = out[..36].to_vec();
        // Declare an unknown RIFF size so the missing data chunk, not a
        // short RIFF size, is what the parser reports.
        header_only[4..8].copy_from_slice(&0u32.to_le_bytes());
        let err = parse_wav_header(&header_only).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn header_round_trip_through_writer() {
        let wav = minimal_wav(PcmFormat::F32, 1, 48000, &[0.25, -0.25]);
        let header = parse_wav_header(&wav).unwrap();
        assert_eq!(header.format, PcmFormat::F32);
        assert_eq!(header.duration_micros(), 41);
    }
}
