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

pub mod aac_encode;
pub mod aac_frame;
pub mod aac_huffman;
pub mod aac_tables;
pub mod adts_frame;
pub mod aiff_chunk;
pub mod alac_encode;
pub mod alac_frame;
pub mod alac_predictor;
pub mod av1_frame;
pub mod av1_intra;
pub mod av1_obu;
pub mod av1_reconstruct;
pub mod av1_sequence;
pub mod av1_tile;
pub mod bitrate;
pub mod bitreader;
pub mod bitwriter;
pub mod byte_source;
pub mod celt;
pub mod celt_reconstruct;
pub mod decoder;
pub mod demux;
pub mod dither;
pub mod dop;
pub mod dsd_coefficients;
pub mod dsd_decimator;
pub mod dsd_modulator;
pub mod dsd_stream;
pub mod ebml;
pub mod encoder;
pub mod error;
pub mod flac_frame;
pub mod frame;
pub mod frame_pool;
pub mod huffman;
pub mod hw;
pub mod id3_tag;
pub mod license;
pub mod limits;
pub mod lsb_bitreader;
pub mod matroska;
pub mod mdct;
pub mod mp3_encode;
pub mod mp3_frame;
pub mod mp3_huffman;
pub mod mp3_synth;
pub mod mp3_tables;
pub mod mp4_box;
pub mod mp4_tag;
pub mod mpeg_ts;
pub mod ogg_page;
pub mod ogg_stream;
pub mod opus_packet;
pub mod pcm;
pub mod polyphase;
pub mod quality;
pub mod range_decoder;
pub mod resample;
pub mod resolution;
pub mod scaler;
pub mod vorbis_comment;
pub mod vorbis_packet;
pub mod vorbis_setup;
pub mod vp9_bitstream;
pub mod vp9_loopfilter;
pub mod vp9_predict;
pub mod vp9_reconstruct;
pub mod vp9_token;
pub mod wav_header;
pub mod wav_stream;

pub use aac_encode::AacEncoder;
pub use aac_frame::{AacConfig, AacDecodedFrame, AacDecoder};
pub use aac_huffman::{
    decode_scale_factor, decode_spectral, huffman_is_magnitude, huffman_is_quad, HuffmanEntry,
};
pub use adts_frame::{
    adts_crc16, parse_adts_header, sample_rate_for_index, verify_adts_frame, write_adts_frame,
    AdtsDemuxer, AdtsFrame, AdtsHeader,
};
pub use aiff_chunk::{
    parse_aiff, read_extended_float, write_aiff, write_extended_float, AiffComment, AiffDemuxer,
    AiffForm, AiffHeader, AiffMarker, AiffStream,
};
pub use alac_encode::AlacEncoder;
pub use alac_frame::{
    decode_alac_audio_frame, decode_alac_frame, parse_alac_config, AlacConfig, AlacDecodedFrame,
};
pub use alac_predictor::{dyn_decompress, unpc_block, AgParams, AlacBitReader};
pub use av1_frame::{
    check_trailing_bits, parse_frame_header as parse_av1_frame_header, parse_frame_obu,
    parse_tile_group_obu, Av1ReferenceState, Av1SymbolDecoder, FrameHeader as Av1FrameHeader,
    GrainParams, TileGroup, TileInfo,
};
pub use av1_obu::{parse_obu, parse_obu_header, Obu, ObuHeader, ObuReader, ObuType};
pub use av1_reconstruct::{reconstruct_av1_frame_obu, reconstruct_av1_keyframe};
pub use av1_sequence::{parse_sequence_header, ColorConfig, OperatingPoint, SequenceHeader};
pub use bitrate::{validate_bitrate, vorbis_quality, BitrateCodec, BitrateMode};
pub use bitreader::{BitReader, BitSource};
pub use bitwriter::BitWriter;
pub use byte_source::BoundedReader;
pub use celt::{decode_celt_frame_header, CeltFrameHeader, PostFilter};
pub use celt_reconstruct::{celt_end_band, CeltDecoder};
pub use decoder::{Decoder, DecoderConfig};
pub use demux::{probe_container, ContainerKind, Demuxer, TrackInfo, TrackKind};
pub use dither::{quantize_f32_to_i16, quantize_i24_to_i16, DitherOptions};
pub use dop::{
    dop_pcm_rate, pack_dop, reverse_bit_order, unpack_dop, DOP_MARKER_EVEN, DOP_MARKER_ODD,
};
pub use dsd_decimator::{dsd_bits_to_f32, BitOrder, DsdBlockLayout, DsdDecimator, DsdLevel};
pub use dsd_stream::{probe_dsd, DsdDemuxer, DsdFormat, DsdHeader};
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
pub use frame_pool::{bytes_per_sample, frame_bytes, plane_bytes, FramePool, FramePoolStats};
pub use huffman::HuffmanTable;
pub use hw::{
    licensed_feature_enabled, licensed_software_path_error, passthrough_allowed, HardwareDecoder,
    HardwareEncoder, StubHardwareDecoder, StubHardwareEncoder,
};
pub use id3_tag::{
    id3v1_genre_index, id3v1_genre_name, parse_id3, parse_id3v1, parse_id3v2, write_id3v1,
    write_id3v2, AttachedPicture, Id3Tag,
};
pub use license::{AuditRecord, LicenseContext, LicenseType};
pub use lsb_bitreader::{ilog, LsbBitReader};
pub use matroska::MatroskaDemuxer;
pub use mdct::{mdct_forward, mdct_inverse};
pub use mp3_encode::Mp3Encoder;
pub use mp3_frame::{ChannelMode, Mp3Decoder, Mp3FrameHeader, Mp3Layer, MpegVersion};
pub use mp4_box::{parse_box_header, BoxCursor, BoxHeader, Mp4Demuxer};
pub use mp4_tag::{parse_mp4_metadata, parse_mp4_tags};
pub use mpeg_ts::{crc32_mpeg, TsDemuxer, TsMuxer, TsStream, TsStreamStats};
pub use ogg_page::{parse_ogg_page, write_ogg_page, OggPage};
pub use ogg_stream::{OggDemuxer, OggMuxer};
pub use opus_packet::{parse_opus_head, parse_opus_packet, parse_opus_tags, OpusHead, OpusTags};
pub use pcm::{decode_i32_samples, decode_samples, encode_i32_samples, encode_samples, PcmFormat};
pub use polyphase::{resample_polyphase, PolyphaseResampler};
pub use quality::{
    classify_quality, QualityLabel, QualityParams, HI_RES_MAX_BITS_PER_SAMPLE,
    HI_RES_MAX_SAMPLE_RATE, LOSSLESS_MAX_BITS_PER_SAMPLE, LOSSLESS_MAX_SAMPLE_RATE,
};
pub use range_decoder::{RangeDecoder, RangeEncoder};
pub use resample::{resample, resample_with_method, ResampleMethod};
pub use resolution::{
    av1_level, minimum_av1_level, suggest_av1, suggest_vp9, validate_av1_frame,
    validate_av1_sequence_header, validate_vp9_frame, validate_vp9_frame_header,
    vp9_profile_limits, Av1Level, EncodingSuggestion, FrameSize, Vp9ProfileLimits,
};
pub use scaler::{scale_frame, ScaleFilter};
pub use vorbis_comment::{
    parse_vorbis_comment_packet, validate_key, write_vorbis_comment_packet, VorbisComment,
};
pub use vorbis_packet::{VorbisDecodedPacket, VorbisPacketDecoder};
pub use vorbis_setup::VorbisSetup;
pub use vp9_bitstream::{
    parse_superframe, parse_vp9_frame, parse_vp9_frame_header, Vp9BoolDecoder, Vp9ColorConfig,
    Vp9ColorSpace, Vp9CompressedHeader, Vp9Frame, Vp9FrameHeader, Vp9InterpolationFilter,
    Vp9ReferenceMode, Vp9ReferenceState, Vp9Tile, Vp9TxMode, VP9_FRAME_SYNC_CODE,
};
pub use vp9_reconstruct::{reconstruct_vp9_frame, Vp9ReconstructionState};
pub use wav_header::{parse_wav_header, write_wav_header, WavHeader};
pub use wav_stream::{encode_wav, WavDemuxer, WavMuxer};
