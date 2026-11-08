//! Integration tests for Ogg demuxing (RFC 3533).
//!
//! The reference vector is an Ogg FLAC file produced from the local
//! WAV vector with ffmpeg 8.1.2. Continued-packet and muxer tests use
//! packets built with the library's own `OggMuxer`, so they exercise
//! the page format without depending on an external encoder.

use std::io::Cursor;

use codec_whatttttttttttt_core::{
    parse_ogg_flac_first_packet, Codec, Decoder, Error, Frame, OggDemuxer, OggMuxer,
};

fn ogg_bytes() -> Vec<u8> {
    include_bytes!("data/sine_stereo_44100.ogg").to_vec()
}

#[test]
fn parses_ffmpeg_ogg_pages() {
    let demuxer = OggDemuxer::new(Cursor::new(ogg_bytes())).unwrap();
    assert_eq!(demuxer.codec(), Codec::Flac);
    assert_eq!(demuxer.page_count(), 3);
    assert_eq!(demuxer.packet_count(), 3);
}

#[test]
fn granule_positions_are_preserved() {
    let mut demuxer = OggDemuxer::new(Cursor::new(ogg_bytes())).unwrap();
    let first = demuxer.next_packet().unwrap().unwrap();
    let second = demuxer.next_packet().unwrap().unwrap();
    let third = demuxer.next_packet().unwrap().unwrap();
    assert_eq!(first.timestamp, 0);
    assert_eq!(second.timestamp, 0);
    assert_eq!(third.timestamp, 4410);
}

#[test]
fn ogg_flac_first_packet_parses_streaminfo() {
    let mut demuxer = OggDemuxer::new(Cursor::new(ogg_bytes())).unwrap();
    let first = demuxer.next_packet().unwrap().unwrap();
    let (streaminfo, header_count) = parse_ogg_flac_first_packet(&first.data).unwrap();
    assert_eq!(streaminfo.sample_rate, 44100);
    assert_eq!(streaminfo.channels, 2);
    assert_eq!(streaminfo.bits_per_sample, 16);
    assert_eq!(header_count, 1);
}

#[test]
fn ogg_flac_end_to_end_decode_matches_original_wav() {
    let mut demuxer = OggDemuxer::new(Cursor::new(ogg_bytes())).unwrap();
    let first = demuxer.next_packet().unwrap().unwrap();
    let (streaminfo, header_count) = parse_ogg_flac_first_packet(&first.data).unwrap();
    let mut decoder = Decoder::from_flac_streaminfo(&streaminfo).unwrap();
    let mut samples = Vec::new();
    let mut skipped = 0usize;
    while let Some(packet) = demuxer.next_packet().unwrap() {
        if skipped < header_count as usize {
            skipped += 1;
            continue;
        }
        match decoder.decode(&packet).unwrap() {
            Frame::Audio(audio) => samples.extend_from_slice(&audio.samples),
            Frame::Video(_) => panic!("expected an audio frame"),
        }
    }
    let wav = include_bytes!("data/valid_i16_stereo_44100.wav");
    let wav_demuxer =
        codec_whatttttttttttt_core::WavDemuxer::new(Cursor::new(wav.to_vec())).unwrap();
    assert_eq!(samples, wav_demuxer.samples().unwrap());
}

#[test]
fn bad_crc_returns_invalid_data() {
    let mut bytes = ogg_bytes();
    // Flip a byte in the first page payload; only the page CRC can
    // catch the change.
    bytes[30] ^= 0x01;
    let err = OggDemuxer::new(Cursor::new(bytes)).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn truncated_page_returns_truncated() {
    // Cut inside the second page (page 0 ends at byte 79).
    let bytes = ogg_bytes();
    let err = OggDemuxer::new(Cursor::new(bytes[..100].to_vec())).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn empty_stream_returns_invalid_data() {
    let err = OggDemuxer::new(Cursor::new(Vec::new())).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn continued_packet_is_reassembled() {
    // First packet identifies the codec as Opus; the second packet is
    // 70000 bytes, so it must span several pages.
    let large = vec![0x5Au8; 70000];
    let mut out = Vec::new();
    let mut muxer = OggMuxer::new(&mut out, 99).unwrap();
    muxer.write_packet(b"OpusHead", 0, true, false).unwrap();
    muxer.write_packet(&large, 48000, false, true).unwrap();
    muxer.finish().unwrap();

    let mut demuxer = OggDemuxer::new(Cursor::new(out)).unwrap();
    assert_eq!(demuxer.codec(), Codec::Opus);
    assert_eq!(demuxer.packet_count(), 2);
    let first = demuxer.next_packet().unwrap().unwrap();
    assert_eq!(first.data, b"OpusHead");
    let second = demuxer.next_packet().unwrap().unwrap();
    assert_eq!(second.data, large);
    assert_eq!(second.timestamp, 48000);
    assert_eq!(demuxer.next_packet().unwrap(), None);
}

#[test]
fn muxer_demuxer_round_trip_with_edge_sizes() {
    let packets: Vec<Vec<u8>> = vec![
        b"OpusHead".to_vec(),
        vec![],
        vec![0u8; 255],
        vec![1u8; 510],
        b"tail".to_vec(),
    ];
    let mut out = Vec::new();
    let mut muxer = OggMuxer::new(&mut out, 5).unwrap();
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

    let mut demuxer = OggDemuxer::new(Cursor::new(out)).unwrap();
    assert_eq!(demuxer.packet_count(), packets.len());
    for expected in &packets {
        let packet = demuxer.next_packet().unwrap().unwrap();
        assert_eq!(&packet.data, expected);
    }
    assert_eq!(demuxer.next_packet().unwrap(), None);
}
