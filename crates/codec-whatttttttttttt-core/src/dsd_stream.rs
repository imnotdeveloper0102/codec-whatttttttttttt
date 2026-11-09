//! DSF and DFF (DSDIFF) demuxing, streaming over `Read`.
//!
//! The file is never loaded into memory: the header is parsed
//! from small bounded reads, and `next_packet` returns one data
//! block at a time. DSF (Sony DSD Stream File spec) and DFF
//! (Philips DSDIFF 1.5 spec) differ in three ways that matter
//! here, and the demuxer keeps them separate instead of
//! normalising them away:
//!
//! * Endianness: DSF numbers are little-endian, DFF numbers are
//!   big-endian (DSDIFF is an IFF descendant).
//! * Bit order: DSF stores DSD bits LSB-first, DFF MSB-first.
//! * Data layout: DSF groups data in blocks of
//!   `block_size_per_channel` bytes per channel, channel blocks
//!   back to back; DFF interleaves one byte per channel
//!   (a "Clustered Frame" of N interleaved Channel Bytes in
//!   DSDIFF 1.5 section 2.2).
//!
//! The demuxer reports the layout in `DsdHeader`, and the
//! decoder's `DsdDecimator` consumes packets in that layout, so
//! no copy reorders the audio data.
//!
//! DST-compressed DFF (`DST ` chunk instead of `DSD `) is
//! rejected with `Unsupported`: DST is a separate lossless
//! codec, not raw DSD, and decoding it here would silently
//! produce garbage.

use std::io::Read;

use crate::dsd_decimator::{DsdBlockLayout, DsdLevel};
use crate::error::{Error, Result};
use crate::frame::{Codec, Packet};
use crate::limits::{MAX_ALLOCATION_BYTES, MAX_CHANNELS};

/// Largest header region the demuxer reads while scanning for
/// the data chunk. DSF headers are 92 bytes; DFF headers grow
/// with their PROP sub-chunks, but a PROP larger than this is
/// not a real-world DSD file and is rejected rather than read
/// without bound.
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Bytes read per data packet from a DFF file: 4096 bytes per
/// channel, matching the DSF block size, so both formats hand
/// the decoder the same packet shape.
const DFF_BYTES_PER_CHANNEL_PER_PACKET: usize = 4096;

/// Which container holds the DSD stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdFormat {
    /// Sony DSF (`.dsf`).
    Dsf,
    /// Philips DSDIFF (`.dff`).
    Dff,
}

/// Parsed DSD stream header. Everything the decoder needs is
/// here: the rate (which fixes the DSD level), the channel
/// count, and the data layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdHeader {
    /// Container format.
    pub format: DsdFormat,
    /// DSD sample rate in Hz (`2_822_400` for DSD64, and so on).
    pub sample_rate: u32,
    /// Channel count, 1..=8.
    pub channels: u32,
    /// DSD level derived from `sample_rate`.
    pub level: DsdLevel,
    /// DSF only: bytes per channel in one block group, from the
    /// `fmt ` chunk (4096 in practice). DFF reports the packet
    /// size used by this demuxer instead.
    pub block_size_per_channel: usize,
    /// Samples per channel declared by the header (DSF `fmt `
    /// chunk). DFF has no such field and reports 0; the data
    /// size is authoritative there.
    pub sample_count: u64,
    /// Byte offset of the first audio byte in the file.
    pub data_offset: u64,
    /// Audio bytes in the file, from the data chunk header.
    pub data_size: u64,
}

impl DsdHeader {
    /// Block layout of this stream's data, for the decimator.
    #[must_use]
    pub fn layout(&self) -> DsdBlockLayout {
        match self.format {
            DsdFormat::Dsf => DsdBlockLayout::Dsf {
                block_size_per_channel: self.block_size_per_channel,
            },
            DsdFormat::Dff => DsdBlockLayout::Dff,
        }
    }

    /// Raw DSD bytes per second for this stream.
    #[must_use]
    pub fn bytes_per_second(&self) -> u64 {
        self.level.bytes_per_second(self.channels)
    }

    /// Declared duration in seconds, from the DSF sample count
    /// when present, otherwise from the data size.
    #[allow(
        clippy::cast_precision_loss,
        reason = "sample counts and data sizes of real files stay far below 2^53"
    )]
    #[must_use]
    pub fn duration_seconds(&self) -> f64 {
        if self.sample_count > 0 {
            self.sample_count as f64 / f64::from(self.sample_rate)
        } else {
            self.data_size as f64 / self.bytes_per_second() as f64
        }
    }
}

/// Detects a DSD container from its first bytes: `DSD ` is DSF,
/// `FRM8` is DFF. Returns `None` for anything else.
#[must_use]
pub fn probe_dsd(data: &[u8]) -> Option<DsdFormat> {
    if data.len() >= 4 && &data[0..4] == b"DSD " {
        Some(DsdFormat::Dsf)
    } else if data.len() >= 4 && &data[0..4] == b"FRM8" {
        Some(DsdFormat::Dff)
    } else {
        None
    }
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32> {
    let slice = bytes.get(offset..offset + 4).ok_or(Error::Truncated {
        needed: offset + 4,
        had: bytes.len(),
    })?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_u64_le(bytes: &[u8], offset: usize) -> Result<u64> {
    let slice = bytes.get(offset..offset + 8).ok_or(Error::Truncated {
        needed: offset + 8,
        had: bytes.len(),
    })?;
    Ok(u64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

fn read_u32_be(bytes: &[u8], offset: usize) -> Result<u32> {
    let slice = bytes.get(offset..offset + 4).ok_or(Error::Truncated {
        needed: offset + 4,
        had: bytes.len(),
    })?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_u64_be(bytes: &[u8], offset: usize) -> Result<u64> {
    let slice = bytes.get(offset..offset + 8).ok_or(Error::Truncated {
        needed: offset + 8,
        had: bytes.len(),
    })?;
    Ok(u64::from_be_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

fn check_channels(channels: u32, offset: usize) -> Result<u32> {
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(Error::InvalidData {
            offset,
            detail: "dsd channel count is zero or above 8",
        });
    }
    Ok(channels)
}

fn check_rate(sample_rate: u32, offset: usize) -> Result<DsdLevel> {
    DsdLevel::from_sample_rate(sample_rate).ok_or(Error::InvalidData {
        offset,
        detail: "dsd sample rate matches no dsd level (2.8224/5.6448/11.2896/22.5792 MHz)",
    })
}

/// Parses a DSF header from the 92 header bytes (`DSD ` chunk
/// of 28 bytes plus `fmt ` chunk of 52 bytes plus the 12-byte
/// `data` chunk header). The caller supplies exactly the bytes
/// from the file start; the data chunk's declared size is read
/// from bytes 84..92.
///
/// # Errors
///
/// Returns `Truncated` for a short header, `InvalidData` for a
/// bad magic, version, format id, channel count or sample rate,
/// and `Unsupported` for a bits-per-sample value other than 1
/// (8-bit DSF is a different sample format, not 1-bit DSD).
fn parse_dsf_header(bytes: &[u8]) -> Result<(DsdHeader, u64)> {
    if bytes.len() < 92 {
        return Err(Error::Truncated {
            needed: 92,
            had: bytes.len(),
        });
    }
    if &bytes[0..4] != b"DSD " {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "dsf magic is not 'DSD '",
        });
    }
    let dsd_chunk_size = read_u64_le(bytes, 4)?;
    if dsd_chunk_size != 28 {
        return Err(Error::InvalidData {
            offset: 4,
            detail: "dsf dsd chunk size is not 28",
        });
    }
    if &bytes[28..32] != b"fmt " {
        return Err(Error::InvalidData {
            offset: 28,
            detail: "dsf fmt chunk id is not 'fmt '",
        });
    }
    let fmt_size = read_u64_le(bytes, 32)?;
    if fmt_size != 52 {
        return Err(Error::InvalidData {
            offset: 32,
            detail: "dsf fmt chunk size is not 52",
        });
    }
    let version = read_u32_le(bytes, 40)?;
    if version != 1 {
        return Err(Error::InvalidData {
            offset: 40,
            detail: "dsf format version is not 1",
        });
    }
    let format_id = read_u32_le(bytes, 44)?;
    if format_id != 0 {
        return Err(Error::InvalidData {
            offset: 44,
            detail: "dsf format id is not 0 (dsd raw)",
        });
    }
    let channels = check_channels(read_u32_le(bytes, 52)?, 52)?;
    let sample_rate = read_u32_le(bytes, 56)?;
    let level = check_rate(sample_rate, 56)?;
    let bits_per_sample = read_u32_le(bytes, 60)?;
    if bits_per_sample != 1 {
        return Err(Error::Unsupported(
            "dsf bits per sample is not 1 (8-bit dsf samples are not 1-bit dsd)",
        ));
    }
    let sample_count = read_u64_le(bytes, 64)?;
    let block_size = read_u32_le(bytes, 72)?;
    if block_size == 0 || block_size as usize > MAX_ALLOCATION_BYTES {
        return Err(Error::InvalidData {
            offset: 72,
            detail: "dsf block size per channel is zero or absurd",
        });
    }
    if &bytes[80..84] != b"data" {
        return Err(Error::InvalidData {
            offset: 80,
            detail: "dsf data chunk id is not 'data'",
        });
    }
    let data_chunk_size = read_u64_le(bytes, 84)?;
    if data_chunk_size < 12 {
        return Err(Error::InvalidData {
            offset: 84,
            detail: "dsf data chunk size is below its 12-byte header",
        });
    }
    let data_size = data_chunk_size - 12;
    // The data must hold at least the declared samples, rounded
    // up to whole bytes and whole blocks; a shorter chunk is a
    // truncated file, not a short stream.
    let needed_bytes = sample_count.div_ceil(8).saturating_mul(u64::from(channels));
    if data_size < needed_bytes {
        return Err(Error::InvalidData {
            offset: 84,
            detail: "dsf data chunk is shorter than the declared sample count",
        });
    }
    Ok((
        DsdHeader {
            format: DsdFormat::Dsf,
            sample_rate,
            channels,
            level,
            block_size_per_channel: block_size as usize,
            sample_count,
            data_offset: 92,
            data_size,
        },
        data_size,
    ))
}

/// Streaming DSD demuxer. `next_packet` returns one block of
/// raw DSD bytes per call (one DSF block group, or 4096 bytes
/// per channel of DFF), with timestamps in microseconds derived
/// from the samples delivered so far.
pub struct DsdDemuxer<R: Read> {
    reader: R,
    header: DsdHeader,
    /// Audio bytes still to deliver.
    remaining: u64,
    /// DSD samples per channel delivered so far.
    samples_delivered: u64,
    /// Scratch for skipping unknown chunks.
    scratch: Vec<u8>,
}

impl<R: Read> std::fmt::Debug for DsdDemuxer<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DsdDemuxer")
            .field("header", &self.header)
            .field("remaining", &self.remaining)
            .field("samples_delivered", &self.samples_delivered)
            .finish_non_exhaustive()
    }
}

impl<R: Read> DsdDemuxer<R> {
    /// Reads and parses the header from `reader`.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a cut-short header, `InvalidData`
    /// for a malformed header, `Unsupported` for DST-compressed
    /// DFF or non-1-bit DSF, and `AllocationLimit` when a
    /// declared chunk size would overflow the header bound.
    #[allow(
        clippy::too_many_lines,
        reason = "the constructor parses the DSF branch and the DFF chunk walk in one place, because the header must be complete before the demuxer exists"
    )]
    pub fn new(mut reader: R) -> Result<Self> {
        let mut magic = [0u8; 12];
        reader.read_exact(&mut magic).map_err(|err| {
            if err.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::Truncated { needed: 12, had: 0 }
            } else {
                Error::Io(err)
            }
        })?;
        if &magic[0..4] == b"DSD " {
            let mut rest = [0u8; 80];
            reader.read_exact(&mut rest).map_err(|_| Error::Truncated {
                needed: 92,
                had: 12,
            })?;
            let mut header_bytes = Vec::with_capacity(92);
            header_bytes.extend_from_slice(&magic);
            header_bytes.extend_from_slice(&rest);
            let (header, data_size) = parse_dsf_header(&header_bytes)?;
            return Ok(Self {
                reader,
                header,
                remaining: data_size,
                samples_delivered: 0,
                scratch: vec![0u8; 8192],
            });
        }
        if &magic[0..4] != b"FRM8" {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "not a dsd file (magic is neither 'DSD ' nor 'FRM8')",
            });
        }
        // DFF: the FRM8 header is 16 bytes (id, 8-byte size,
        // form type 'DSD '); the first 12 bytes read above only
        // cover the id and the size, so the form type needs 4
        // more bytes.
        let mut form_type = [0u8; 4];
        reader
            .read_exact(&mut form_type)
            .map_err(|_| Error::Truncated {
                needed: 16,
                had: 12,
            })?;
        if &form_type != b"DSD " {
            return Err(Error::InvalidData {
                offset: 12,
                detail: "dff form type is not 'DSD '",
            });
        }
        let mut demuxer = Self {
            reader,
            header: DsdHeader {
                format: DsdFormat::Dff,
                sample_rate: 0,
                channels: 0,
                level: DsdLevel::Dsd64,
                block_size_per_channel: DFF_BYTES_PER_CHANNEL_PER_PACKET,
                sample_count: 0,
                data_offset: 0,
                data_size: 0,
            },
            remaining: 0,
            samples_delivered: 0,
            scratch: vec![0u8; 8192],
        };
        // Byte offset of the next chunk header; the FRM8 header
        // itself is 16 bytes.
        let mut header_bytes = 16usize;
        loop {
            let (chunk_id, chunk_size) = demuxer.read_chunk_header()?;
            header_bytes = header_bytes
                .checked_add(12)
                .ok_or(Error::Overflow("dff header size overflows"))?;
            if header_bytes > MAX_HEADER_BYTES {
                return Err(Error::AllocationLimit {
                    requested: header_bytes,
                    limit: MAX_HEADER_BYTES,
                });
            }
            match &chunk_id {
                b"PROP" => {
                    demuxer.read_prop_chunk(chunk_size)?;
                    let chunk_len = usize::try_from(chunk_size)
                        .map_err(|_| Error::Overflow("dff header size overflows"))?;
                    header_bytes = header_bytes
                        .checked_add(chunk_len)
                        .ok_or(Error::Overflow("dff header size overflows"))?;
                }
                b"DSD " => {
                    if demuxer.header.sample_rate == 0 || demuxer.header.channels == 0 {
                        return Err(Error::InvalidData {
                            offset: 0,
                            detail: "dff dsd chunk appears before the prop chunk",
                        });
                    }
                    let level = check_rate(demuxer.header.sample_rate, 0)?;
                    demuxer.header.level = level;
                    demuxer.header.data_size = chunk_size;
                    // The payload starts right after this chunk's
                    // 12-byte header.
                    demuxer.header.data_offset = header_bytes as u64;
                    demuxer.remaining = chunk_size;
                    return Ok(demuxer);
                }
                b"DST " => {
                    return Err(Error::Unsupported(
                        "dff dst compression is not decoded (dst is a separate lossless codec)",
                    ));
                }
                _ => {
                    demuxer.skip_bytes(chunk_size)?;
                    let chunk_len = usize::try_from(chunk_size)
                        .map_err(|_| Error::Overflow("dff header size overflows"))?;
                    header_bytes = header_bytes
                        .checked_add(chunk_len)
                        .ok_or(Error::Overflow("dff header size overflows"))?;
                }
            }
        }
    }

    /// The parsed header.
    #[must_use]
    pub fn header(&self) -> &DsdHeader {
        &self.header
    }

    fn read_chunk_header(&mut self) -> Result<([u8; 4], u64)> {
        let mut header = [0u8; 12];
        self.reader
            .read_exact(&mut header)
            .map_err(|_| Error::Truncated { needed: 12, had: 0 })?;
        let size = read_u64_be(&header, 4)?;
        Ok(([header[0], header[1], header[2], header[3]], size))
    }

    fn skip_bytes(&mut self, mut count: u64) -> Result<()> {
        while count > 0 {
            let want = usize::try_from(count.min(self.scratch.len() as u64))
                .map_err(|_| Error::Overflow("dsd skip size exceeds usize"))?;
            let read = self.reader.read(&mut self.scratch[..want])?;
            if read == 0 {
                return Err(Error::Truncated {
                    needed: want,
                    had: 0,
                });
            }
            count -= read as u64;
        }
        Ok(())
    }

    fn read_exact_vec(&mut self, count: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; count];
        self.reader
            .read_exact(&mut buf)
            .map_err(|_| Error::Truncated {
                needed: count,
                had: 0,
            })?;
        Ok(buf)
    }

    /// Parses the PROP chunk: `SND ` followed by sub-chunks
    /// (`FS  ` sample rate, `CHNL` channel count and ids, `CMPR`
    /// compression type, and others skipped).
    fn read_prop_chunk(&mut self, chunk_size: u64) -> Result<()> {
        // The PROP chunk is read whole, so its declared size is
        // bounded before allocating anything for it.
        if chunk_size > MAX_HEADER_BYTES as u64 {
            return Err(Error::AllocationLimit {
                requested: usize::try_from(chunk_size)
                    .map_err(|_| Error::Overflow("dff prop chunk size exceeds usize"))?,
                limit: MAX_HEADER_BYTES,
            });
        }
        let bytes = self.read_exact_vec(
            usize::try_from(chunk_size)
                .map_err(|_| Error::Overflow("dff prop chunk size exceeds usize"))?,
        )?;
        if bytes.len() < 4 || &bytes[0..4] != b"SND " {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "dff prop chunk does not start with 'SND '",
            });
        }
        let mut offset = 4usize;
        while offset + 12 <= bytes.len() {
            let sub_id = &bytes[offset..offset + 4];
            let sub_size = usize::try_from(read_u64_be(&bytes, offset + 4)?)
                .map_err(|_| Error::Overflow("dff prop sub-chunk size exceeds usize"))?;
            let data_start = offset + 12;
            let data_end = data_start
                .checked_add(sub_size)
                .ok_or(Error::Overflow("dff prop sub-chunk size overflows"))?;
            if data_end > bytes.len() {
                return Err(Error::InvalidData {
                    offset,
                    detail: "dff prop sub-chunk overruns the prop chunk",
                });
            }
            match sub_id {
                b"FS  " => {
                    if sub_size != 4 {
                        return Err(Error::InvalidData {
                            offset,
                            detail: "dff fs sub-chunk size is not 4",
                        });
                    }
                    self.header.sample_rate = read_u32_be(&bytes, data_start)?;
                }
                b"CHNL" => {
                    if sub_size < 2 {
                        return Err(Error::InvalidData {
                            offset,
                            detail: "dff chnl sub-chunk is shorter than 2 bytes",
                        });
                    }
                    let count = u16::from_be_bytes([bytes[data_start], bytes[data_start + 1]]);
                    if sub_size != 2 + 4 * count as usize {
                        return Err(Error::InvalidData {
                            offset,
                            detail: "dff chnl sub-chunk size does not match its channel count",
                        });
                    }
                    self.header.channels = check_channels(u32::from(count), offset)?;
                }
                b"CMPR" if sub_size != 4 || &bytes[data_start..data_start + 4] != b"DSD " => {
                    return Err(Error::Unsupported(
                        "dff compression type is not 'DSD ' (uncompressed dsd)",
                    ));
                }
                _ => {}
            }
            offset = data_end;
        }
        if offset != bytes.len() {
            return Err(Error::InvalidData {
                offset,
                detail: "dff prop chunk has trailing bytes after its sub-chunks",
            });
        }
        Ok(())
    }

    /// Returns the next block of raw DSD bytes, or `None` when
    /// the data chunk is exhausted.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when the file ends before the
    /// declared data size, and `AllocationLimit` when one block
    /// would exceed the per-call allocation limit.
    pub fn next_packet(&mut self) -> Result<Option<Packet>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let per_packet = match self.header.format {
            DsdFormat::Dsf => self
                .header
                .block_size_per_channel
                .checked_mul(self.header.channels as usize)
                .ok_or(Error::Overflow("dsf packet size overflows"))?,
            DsdFormat::Dff => DFF_BYTES_PER_CHANNEL_PER_PACKET
                .checked_mul(self.header.channels as usize)
                .ok_or(Error::Overflow("dff packet size overflows"))?,
        };
        if per_packet > MAX_ALLOCATION_BYTES {
            return Err(Error::AllocationLimit {
                requested: per_packet,
                limit: MAX_ALLOCATION_BYTES,
            });
        }
        let remaining = usize::try_from(self.remaining).unwrap_or(usize::MAX);
        let want = remaining.min(per_packet);
        let mut data = vec![0u8; want];
        self.reader
            .read_exact(&mut data)
            .map_err(|_| Error::Truncated {
                needed: want,
                had: 0,
            })?;
        self.remaining -= want as u64;
        // Timestamp: samples delivered so far, in microseconds.
        let samples = want as u64 / u64::from(self.header.channels) * 8;
        let timestamp = self.samples_delivered * 1_000_000 / u64::from(self.header.sample_rate);
        self.samples_delivered += samples;
        Ok(Some(Packet {
            codec: Codec::Dsd,
            data,
            timestamp,
            is_keyframe: true,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_detects_dsf_and_dff_magic() {
        assert_eq!(probe_dsd(b"DSD xxxx"), Some(DsdFormat::Dsf));
        assert_eq!(probe_dsd(b"FRM8xxxx"), Some(DsdFormat::Dff));
        assert_eq!(probe_dsd(b"RIFFxxxx"), None);
        assert_eq!(probe_dsd(b"DS"), None);
    }

    #[test]
    fn bad_magic_is_rejected() {
        let err = DsdDemuxer::new(std::io::Cursor::new(b"RIFFxxxxxx12".to_vec())).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }
}
