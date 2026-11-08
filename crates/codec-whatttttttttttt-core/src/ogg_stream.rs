//! Ogg logical-stream demuxing and muxing (RFC 3533).
//!
//! A logical stream is a sequence of pages with one serial number and
//! consecutive sequence numbers. Packets span pages: a segment of 255
//! bytes continues the packet, a shorter segment ends it. The demuxer
//! reassembles packets and identifies the codec from the first
//! (beginning-of-stream) packet: `0x7F "FLAC"` is FLAC, `OpusHead` is
//! Opus, `\x01vorbis` is Vorbis. Anything else is `Unsupported` at
//! construction time, so the demuxer never yields a packet whose codec
//! it cannot name.
//!
//! Packet timestamps are granule positions, not microseconds: the Ogg
//! layer does not know the codec sample rate, so `Packet.timestamp`
//! holds the granule position in samples (0 for header packets). The
//! decoder converts it when it knows the rate. This is the one place
//! where `Packet.timestamp` is not microseconds, and it is documented
//! here rather than silently converted with a guessed rate.
//!
//! Scope for Phase 3: a single logical stream. Chained streams (a new
//! serial after EOS) and multiplexed streams (interleaved serials) are
//! `Unsupported`, not silently merged. Seeking is a later phase.

use std::io::{Read, Write};

use crate::byte_source::read_ogg_bytes;
use crate::error::{Error, Result};
use crate::frame::{Codec, Packet};
use crate::limits::MAX_AUDIO_PACKET_BYTES;
use crate::ogg_page::{
    parse_ogg_page, write_ogg_page, OggPage, OGG_HEADER_BOS, OGG_HEADER_CONTINUED, OGG_HEADER_EOS,
    OGG_MAX_SEGMENTS,
};

/// Demuxer for a single Ogg logical stream. Pages are parsed and
/// packets reassembled at construction; `next_packet` then yields them
/// in order. The whole file is bounded by `MAX_OGG_FILE_BYTES`; a
/// streaming demuxer is a later phase.
#[derive(Debug)]
pub struct OggDemuxer {
    packets: Vec<Packet>,
    codec: Codec,
    serial: u32,
    page_count: usize,
    position: usize,
}

impl OggDemuxer {
    /// Reads and demuxes an Ogg stream from `reader`.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for a cut-short page or packet, `InvalidData`
    /// for a bad capture pattern, CRC, sequence number, or a packet
    /// that never terminates, `Unsupported` for multiple logical
    /// streams or an unknown codec, and `AllocationLimit` for a file
    /// or packet above the limits.
    pub fn new<R: Read>(reader: R) -> Result<Self> {
        let bytes = read_ogg_bytes(reader)?;
        if bytes.is_empty() {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "empty ogg stream",
            });
        }
        let mut pages: Vec<OggPage> = Vec::new();
        let mut offset = 0usize;
        while offset < bytes.len() {
            let (page, consumed) = parse_ogg_page(&bytes[offset..])?;
            if consumed == 0 {
                return Err(Error::InvalidData {
                    offset,
                    detail: "ogg page consumed zero bytes",
                });
            }
            pages.push(page);
            offset += consumed;
        }
        if pages.is_empty() {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "ogg stream contains no pages",
            });
        }
        if !pages[0].is_bos() {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "first ogg page must be bos",
            });
        }
        let serial = pages[0].serial;
        for (index, page) in pages.iter().enumerate() {
            if page.serial != serial {
                return Err(Error::Unsupported(
                    "multiple ogg logical streams (chaining or multiplexing)",
                ));
            }
            if page.sequence
                != u32::try_from(index)
                    .map_err(|_| Error::Overflow("ogg page count exceeds u32"))?
            {
                return Err(Error::InvalidData {
                    offset: 0,
                    detail: "ogg page sequence is not consecutive",
                });
            }
        }
        let mut packets = reassemble_packets(&pages)?;
        if packets.is_empty() {
            return Err(Error::InvalidData {
                offset: 0,
                detail: "ogg stream contains no packets",
            });
        }
        let codec = identify_codec(&packets[0].data)?;
        for packet in &mut packets {
            // Reassembly runs before the codec is known, so packets
            // are stamped with the identified codec here.
            packet.codec = codec;
            if packet.data.len() > MAX_AUDIO_PACKET_BYTES {
                return Err(Error::AllocationLimit {
                    requested: packet.data.len(),
                    limit: MAX_AUDIO_PACKET_BYTES,
                });
            }
        }
        let page_count = pages.len();
        Ok(Self {
            packets,
            codec,
            serial,
            page_count,
            position: 0,
        })
    }

    /// Codec identified from the first packet.
    #[must_use]
    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Serial number of the logical stream.
    #[must_use]
    pub fn serial(&self) -> u32 {
        self.serial
    }

    /// Number of pages in the stream.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.page_count
    }

    /// Number of reassembled packets in the stream.
    #[must_use]
    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }

    /// Returns the next packet, or `None` at end of stream.
    ///
    /// # Errors
    ///
    /// This implementation never fails after construction; the `Result`
    /// exists so a streaming demuxer can report read errors here.
    pub fn next_packet(&mut self) -> Result<Option<Packet>> {
        if self.position >= self.packets.len() {
            return Ok(None);
        }
        let packet = self.packets[self.position].clone();
        self.position += 1;
        Ok(Some(packet))
    }
}

fn identify_codec(first_packet: &[u8]) -> Result<Codec> {
    if first_packet.len() >= 5 && first_packet[0] == 0x7F && &first_packet[1..5] == b"FLAC" {
        return Ok(Codec::Flac);
    }
    if first_packet.starts_with(b"OpusHead") {
        return Ok(Codec::Opus);
    }
    if first_packet.starts_with(b"\x01vorbis") {
        return Ok(Codec::Vorbis);
    }
    Err(Error::Unsupported("unknown ogg codec"))
}

fn reassemble_packets(pages: &[OggPage]) -> Result<Vec<Packet>> {
    let mut packets: Vec<Packet> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    for page in pages {
        let mut data_offset = 0usize;
        for &segment in &page.segments {
            let size = usize::from(segment);
            let chunk =
                page.data
                    .get(data_offset..data_offset + size)
                    .ok_or(Error::InvalidData {
                        offset: 0,
                        detail: "ogg segment overruns page data",
                    })?;
            current.extend_from_slice(chunk);
            data_offset += size;
            if segment < 255 {
                // A short segment ends the packet. Its granule position
                // is the granule of the page it completes on.
                let timestamp = if page.granule_position >= 0 {
                    u64::try_from(page.granule_position)
                        .map_err(|_| Error::Overflow("ogg granule position exceeds u64 range"))?
                } else {
                    0
                };
                packets.push(Packet {
                    codec: Codec::Pcm,
                    data: std::mem::take(&mut current),
                    timestamp,
                    is_keyframe: packets.is_empty(),
                });
            }
        }
    }
    if !current.is_empty() {
        return Err(Error::Truncated {
            needed: current.len() + 1,
            had: current.len(),
        });
    }
    Ok(packets)
}

/// Muxer for a single Ogg logical stream. Packets are split into
/// 255-byte segments and pages of at most 255 segments; a packet that
/// ends exactly on a 255-byte boundary gets a trailing zero-length
/// segment, so the demuxer can tell where it ends.
#[derive(Debug)]
pub struct OggMuxer<W: Write> {
    writer: W,
    serial: u32,
    sequence: u32,
    finished: bool,
}

impl<W: Write> OggMuxer<W> {
    /// Creates a muxer for one logical stream with the given serial.
    ///
    /// # Errors
    ///
    /// This implementation never fails; the `Result` exists so later
    /// versions can validate the serial.
    pub fn new(writer: W, serial: u32) -> Result<Self> {
        Ok(Self {
            writer,
            serial,
            sequence: 0,
            finished: false,
        })
    }

    /// Writes one packet as one or more pages. `granule_position` is
    /// stamped on the last page of the packet; earlier pages of the
    /// same packet carry -1. `bos` marks the first packet of the
    /// stream, `eos` the last.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` after `finish`, and `Io` on a write
    /// failure.
    pub fn write_packet(
        &mut self,
        packet: &[u8],
        granule_position: i64,
        bos: bool,
        eos: bool,
    ) -> Result<()> {
        if self.finished {
            return Err(Error::InvalidArgument("muxer already finished"));
        }
        // Segment sizes for the whole packet, including the trailing
        // zero-length segment when the packet ends on a 255 boundary.
        let mut segments: Vec<u8> = Vec::new();
        let mut remaining = packet.len();
        while remaining > 0 {
            let take = remaining.min(255);
            segments.push(
                u8::try_from(take)
                    .map_err(|_| Error::Overflow("ogg segment size exceeds u8 range"))?,
            );
            remaining -= take;
        }
        if packet.is_empty() || packet.len() % 255 == 0 {
            segments.push(0);
        }
        let mut packet_offset = 0usize;
        let mut segment_index = 0usize;
        let mut first_page = true;
        while segment_index < segments.len() {
            let take = segments.len().min(segment_index + OGG_MAX_SEGMENTS) - segment_index;
            let page_segments = &segments[segment_index..segment_index + take];
            let data_len: usize = page_segments.iter().map(|&size| usize::from(size)).sum();
            let is_last_page = segment_index + take == segments.len();
            let mut header_type: u8 = 0;
            if !first_page {
                header_type |= OGG_HEADER_CONTINUED;
            }
            if bos && first_page {
                header_type |= OGG_HEADER_BOS;
            }
            if eos && is_last_page {
                header_type |= OGG_HEADER_EOS;
            }
            let page = OggPage {
                header_type,
                granule_position: if is_last_page { granule_position } else { -1 },
                serial: self.serial,
                sequence: self.sequence,
                segments: page_segments.to_vec(),
                data: packet[packet_offset..packet_offset + data_len].to_vec(),
            };
            self.writer.write_all(&write_ogg_page(&page)?)?;
            self.sequence += 1;
            packet_offset += data_len;
            segment_index += take;
            first_page = false;
        }
        Ok(())
    }

    /// Finishes the stream. There is no Ogg footer; this only prevents
    /// further writes.
    ///
    /// # Errors
    ///
    /// This implementation never fails.
    pub fn finish(mut self) -> Result<()> {
        self.finished = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn muxer_demuxer_round_trip() {
        // The packets are walked back manually here because the first
        // packet is not a recognizable codec header; the demuxer path
        // is covered by the ffmpeg-file tests in tests/ogg.rs.
        let packets: Vec<Vec<u8>> = vec![
            vec![0x7F, b'F', b'L', b'A', b'C'],
            vec![],
            vec![0u8; 255],
            vec![1u8; 70000],
            b"hello".to_vec(),
        ];
        let mut out = Vec::new();
        let mut muxer = OggMuxer::new(&mut out, 42).unwrap();
        for (i, packet) in packets.iter().enumerate() {
            muxer
                .write_packet(
                    packet,
                    i64::try_from(i).unwrap(),
                    i == 0,
                    i + 1 == packets.len(),
                )
                .unwrap();
        }
        muxer.finish().unwrap();
        // Walk the pages manually: every packet must reappear in order.
        let mut offset = 0usize;
        let mut seen: Vec<Vec<u8>> = Vec::new();
        let mut current: Vec<u8> = Vec::new();
        while offset < out.len() {
            let (page, consumed) = parse_ogg_page(&out[offset..]).unwrap();
            offset += consumed;
            let mut data_offset = 0usize;
            for &segment in &page.segments {
                let size = usize::from(segment);
                current.extend_from_slice(&page.data[data_offset..data_offset + size]);
                data_offset += size;
                if segment < 255 {
                    seen.push(std::mem::take(&mut current));
                }
            }
        }
        assert_eq!(seen, packets);
    }

    #[test]
    fn empty_packet_round_trips() {
        let mut out = Vec::new();
        let mut muxer = OggMuxer::new(&mut out, 7).unwrap();
        muxer.write_packet(&[], 0, true, true).unwrap();
        muxer.finish().unwrap();
        let (page, _) = parse_ogg_page(&out).unwrap();
        assert_eq!(page.segments, vec![0]);
        assert_eq!(page.data.len(), 0);
    }

    #[test]
    fn large_packet_spans_pages_with_continued_flag() {
        let mut out = Vec::new();
        let mut muxer = OggMuxer::new(&mut out, 7).unwrap();
        let packet = vec![0xAAu8; 70000];
        muxer.write_packet(&packet, -1, false, false).unwrap();
        muxer.finish().unwrap();
        let mut offset = 0usize;
        let mut pages = 0;
        let mut continued = 0;
        while offset < out.len() {
            let (page, consumed) = parse_ogg_page(&out[offset..]).unwrap();
            offset += consumed;
            pages += 1;
            if page.is_continued() {
                continued += 1;
            }
        }
        assert!(pages >= 2);
        assert_eq!(continued, pages - 1);
    }

    #[test]
    fn demuxer_rejects_unknown_codec() {
        let mut out = Vec::new();
        let mut muxer = OggMuxer::new(&mut out, 7).unwrap();
        muxer.write_packet(b"not-a-codec", 0, true, true).unwrap();
        muxer.finish().unwrap();
        let err = OggDemuxer::new(std::io::Cursor::new(out)).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn demuxer_rejects_second_serial() {
        let mut out = Vec::new();
        let mut muxer = OggMuxer::new(&mut out, 7).unwrap();
        muxer.write_packet(b"OpusHead", 0, true, false).unwrap();
        muxer.finish().unwrap();
        let mut muxer = OggMuxer::new(&mut out, 8).unwrap();
        muxer.write_packet(b"OpusHead", 0, false, true).unwrap();
        muxer.finish().unwrap();
        let err = OggDemuxer::new(std::io::Cursor::new(out)).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }
}
