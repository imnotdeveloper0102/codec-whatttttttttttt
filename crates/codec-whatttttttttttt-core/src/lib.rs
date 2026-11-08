//! `codec-whatttttttttttt-core`: open-format audio and video codecs.
//!
//! Core in Rust. Decode first, encode second; only open formats
//! (PCM/WAV, FLAC, Opus, Vorbis, AV1, VP9). Phase 0 + Phase 1 implement
//! the skeleton, the trust boundary, and WAV/PCM. Phase 2 adds FLAC
//! decode and Phase 3 adds Ogg demuxing. Other codecs return
//! `Error::Unsupported` naming the codec until their phase lands.
//!
//! # Example
//!
//! ```no_run
//! use codec_whatttttttttttt_core::{Decoder, WavDemuxer};
//! use std::io::Cursor;
//!
//! let wav_bytes: Vec<u8> = Vec::new();
//! let mut demuxer = WavDemuxer::new(Cursor::new(wav_bytes)).unwrap();
//! let mut decoder = Decoder::from_wav_header(demuxer.header()).unwrap();
//! while let Some(packet) = demuxer.next_packet().unwrap() {
//!     let _frame = decoder.decode(&packet).unwrap();
//! }
//! ```

pub mod av1_frame;
pub mod av1_obu;
pub mod av1_sequence;
pub mod bitreader;
pub mod bitwriter;
pub mod byte_source;
pub mod celt;
pub mod decoder;
pub mod demux;
pub mod ebml;
pub mod encoder;
pub mod error;
pub mod flac_frame;
pub mod frame;
pub mod huffman;
pub mod limits;
pub mod lsb_bitreader;
pub mod matroska;
pub mod mdct;
pub mod mp4_box;
pub mod ogg_page;
pub mod ogg_stream;
pub mod opus_packet;
pub mod pcm;
pub mod range_decoder;
pub mod resample;
pub mod vorbis_packet;
pub mod vorbis_setup;
pub mod vp9_bitstream;
pub mod wav_header;
pub mod wav_stream;

pub use av1_frame::{
    check_trailing_bits, parse_frame_header as parse_av1_frame_header, parse_frame_obu,
    parse_tile_group_obu, Av1ReferenceState, Av1SymbolDecoder, FrameHeader as Av1FrameHeader,
    GrainParams, TileGroup, TileInfo,
};
pub use av1_obu::{parse_obu, parse_obu_header, Obu, ObuHeader, ObuReader, ObuType};
pub use av1_sequence::{parse_sequence_header, ColorConfig, OperatingPoint, SequenceHeader};
pub use bitreader::{BitReader, BitSource};
pub use bitwriter::BitWriter;
pub use byte_source::BoundedReader;
pub use celt::{decode_celt_frame_header, CeltFrameHeader, PostFilter};
pub use decoder::{Decoder, DecoderConfig};
pub use demux::{probe_container, ContainerKind, Demuxer, TrackInfo, TrackKind};
pub use ebml::{
    parse_element_header, read_element_id, read_float, read_signed_vint, read_sint, read_string,
    read_uint, read_vint, read_vint_unknown, EbmlCursor, EbmlElementHeader,
};
pub use encoder::{Encoder, EncoderConfig};
pub use error::{Error, Result};
pub use flac_frame::{
    decode_flac_frame, encode_flac_frame, encode_flac_stream, is_metadata_block, parse_flac_stream,
    parse_frame_header, parse_ogg_flac_first_packet, parse_streaminfo, write_flac_stream_header,
    ChannelAssignment, FlacDemuxer, FlacEncoder, FlacStream, FrameHeader, StreamInfo,
};
pub use frame::{
    AudioFrame, Codec, ColourRange, ColourSpace, Frame, Metadata, MetadataEntry, Packet,
    PixelFormat, VideoFrame,
};
pub use huffman::HuffmanTable;
pub use lsb_bitreader::{ilog, LsbBitReader};
pub use matroska::MatroskaDemuxer;
pub use mdct::{mdct_forward, mdct_inverse};
pub use mp4_box::{parse_box_header, BoxCursor, BoxHeader, Mp4Demuxer};
pub use ogg_page::{parse_ogg_page, write_ogg_page, OggPage};
pub use ogg_stream::{OggDemuxer, OggMuxer};
pub use opus_packet::{parse_opus_head, parse_opus_packet, parse_opus_tags, OpusHead, OpusTags};
pub use pcm::PcmFormat;
pub use range_decoder::{RangeDecoder, RangeEncoder};
pub use resample::resample;
pub use vorbis_packet::{VorbisDecodedPacket, VorbisPacketDecoder};
pub use vorbis_setup::VorbisSetup;
pub use vp9_bitstream::{
    parse_superframe, parse_vp9_frame, parse_vp9_frame_header, Vp9BoolDecoder, Vp9ColorConfig,
    Vp9ColorSpace, Vp9CompressedHeader, Vp9Frame, Vp9FrameHeader, Vp9InterpolationFilter,
    Vp9ReferenceMode, Vp9ReferenceState, Vp9Tile, Vp9TxMode, VP9_FRAME_SYNC_CODE,
};
pub use wav_header::{parse_wav_header, write_wav_header, WavHeader};
pub use wav_stream::{encode_wav, WavDemuxer, WavMuxer};
