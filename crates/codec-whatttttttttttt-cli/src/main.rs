//! Command-line tool for `codec-whatttttttttttt`.
//!
//! Phase 1 supports WAV/PCM; Phase 2 adds FLAC; Phase 3 adds Ogg
//! (FLAC-in-Ogg). Phase 6 adds WebM/Matroska and MP4 demuxing;
//! Phase 7 adds AV1 header inspection for IVF, raw OBU, and
//! container tracks. Phase 8 adds VP9 header inspection for IVF
//! (fourcc VP90) and superframe splits. Phase 9 adds `encode`
//! (WAV to FLAC). `decode` always writes WAV:
//!
//! ```text
//! codec-whatttttttttttt info input.wav
//! codec-whatttttttttttt info input.flac
//! codec-whatttttttttttt info input.ogg
//! codec-whatttttttttttt info input.webm
//! codec-whatttttttttttt info input.mp4
//! codec-whatttttttttttt info input.ivf
//! codec-whatttttttttttt info input.obu
//! codec-whatttttttttttt decode input.flac output.wav
//! codec-whatttttttttttt encode input.wav output.flac
//! ```

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use codec_whatttttttttttt_core::{
    parse_flac_stream, parse_ogg_flac_first_packet, parse_opus_head, parse_opus_tags,
    parse_sequence_header, parse_superframe, parse_vp9_frame, parse_wav_header, Codec, Decoder,
    Demuxer, Encoder, EncoderConfig, Error, FlacDemuxer, Frame, ObuReader, ObuType, OggDemuxer,
    PcmFormat, Result, SequenceHeader, TrackKind, VorbisSetup, Vp9FrameHeader, Vp9ReferenceState,
    WavDemuxer, WavMuxer,
};

/// Command-line interface for codec-whatttttttttttt.
#[derive(Debug, Parser)]
#[command(
    name = "codec-whatttttttttttt",
    version,
    about = "Open-format audio and video codecs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the header of a WAV, FLAC, Ogg, WebM, MP4, IVF, or
    /// AV1 OBU file.
    Info {
        /// Input file.
        file: PathBuf,
    },
    /// Decode a WAV, FLAC, or Ogg (FLAC) file and write it as WAV.
    Decode {
        /// Input file.
        input: PathBuf,
        /// Output WAV file.
        output: PathBuf,
    },
    /// Encode a WAV file as FLAC.
    Encode {
        /// Input WAV file.
        input: PathBuf,
        /// Output FLAC file.
        output: PathBuf,
    },
}

fn read_file(path: &PathBuf) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// WAV sample format used when decoding FLAC: the closest supported
/// WAV format that does not lose the decoded precision.
fn wav_format_for_flac(bits_per_sample: u32) -> PcmFormat {
    match bits_per_sample {
        0..=16 => PcmFormat::I16,
        17..=24 => PcmFormat::I24,
        _ => PcmFormat::F32,
    }
}

/// Prints the fields of a parsed AV1 sequence header.
fn print_av1_sequence_header(seq: &SequenceHeader) {
    println!("av1_profile: {}", seq.profile);
    if let Some(point) = seq.operating_points.first() {
        println!("av1_level: {}", point.level);
        println!("av1_tier: {}", point.tier);
    }
    println!(
        "av1_max_dimensions: {}x{}",
        seq.max_frame_width_minus_1 + 1,
        seq.max_frame_height_minus_1 + 1
    );
    println!("av1_bit_depth: {}", seq.color.bit_depth);
    println!("av1_mono_chrome: {}", seq.color.mono_chrome);
    println!(
        "av1_chroma_subsampling: {}x{}",
        u8::from(seq.color.subsampling_x) + 1,
        u8::from(seq.color.subsampling_y) + 1
    );
    println!(
        "av1_film_grain_params_present: {}",
        seq.film_grain_params_present
    );
}

/// Prints the fields of a parsed VP9 frame header (Phase 8:
/// parser-only, no pixel reconstruction).
fn print_vp9_frame_header(header: &Vp9FrameHeader) {
    println!("vp9_profile: {}", header.profile);
    println!(
        "vp9_frame_type: {}",
        if header.is_keyframe { "key" } else { "inter" }
    );
    println!(
        "vp9_dimensions: {}x{}",
        header.frame_width, header.frame_height
    );
    println!(
        "vp9_render_dimensions: {}x{}",
        header.render_width, header.render_height
    );
    println!("vp9_bit_depth: {}", header.color.bit_depth);
    println!("vp9_color_space: {:?}", header.color.color_space);
    println!("vp9_base_q_idx: {}", header.base_q_idx);
    println!("vp9_loop_filter_level: {}", header.loop_filter_level);
    println!(
        "vp9_tx_mode: {:?}",
        header
            .compressed_header
            .as_ref()
            .map(|compressed| compressed.tx_mode)
    );
    println!(
        "vp9_reference_mode: {:?}",
        header
            .compressed_header
            .as_ref()
            .map(|compressed| compressed.reference_mode)
    );
    println!("vp9_show_frame: {}", header.show_frame);
}

/// Finds the first sequence header OBU in `obu_bytes` and parses
/// it, returning `None` when there is no sequence header OBU.
fn find_av1_sequence_header(obu_bytes: &[u8]) -> Result<Option<SequenceHeader>> {
    let mut reader = ObuReader::new(obu_bytes);
    while let Some(obu) = reader.next_obu()? {
        if obu.header.obu_type == ObuType::SequenceHeader {
            return Ok(Some(parse_sequence_header(obu.payload)?));
        }
    }
    Ok(None)
}

/// Prints the AV1 sequence header from codec configuration bytes:
/// the 4-byte AV1CodecConfigurationRecord header is skipped when
/// present (first byte 0x81: marker 1, version 1), and the remaining
/// config OBUs are searched for the sequence header.
fn print_av1_config(codec_private: &[u8]) -> Result<()> {
    let obu_bytes = if codec_private.len() >= 4 && codec_private[0] == 0x81 {
        &codec_private[4..]
    } else {
        codec_private
    };
    match find_av1_sequence_header(obu_bytes)? {
        Some(seq) => print_av1_sequence_header(&seq),
        None => println!("av1_sequence_header: missing"),
    }
    Ok(())
}

/// Prints the Opus header from codec configuration bytes: WebM
/// stores the 19-byte OpusHead itself, MP4 stores the 11-byte dOps
/// payload (the OpusHead fields without the magic and the channel
/// mapping family byte).
fn print_opus_config(codec_private: &[u8]) -> Result<()> {
    let head = if codec_private.starts_with(b"OpusHead") {
        parse_opus_head(codec_private)?
    } else if codec_private.len() == 11 {
        // dOps (ISO BMFF) stores its multi-byte fields big-endian,
        // while OpusHead (RFC 7845) stores them little-endian; the
        // dOps version is 0, but the OpusHead version is 1.
        let pre_skip = u16::from_be_bytes([codec_private[2], codec_private[3]]);
        let input_rate = u32::from_be_bytes([
            codec_private[4],
            codec_private[5],
            codec_private[6],
            codec_private[7],
        ]);
        let gain = i16::from_be_bytes([codec_private[8], codec_private[9]]);
        let mut full = b"OpusHead".to_vec();
        full.push(1); // OpusHead version.
        full.push(codec_private[1]); // Output channel count.
        full.extend_from_slice(&pre_skip.to_le_bytes());
        full.extend_from_slice(&input_rate.to_le_bytes());
        full.extend_from_slice(&gain.to_le_bytes());
        full.push(0); // Channel mapping family 0 (mono/stereo).
        parse_opus_head(&full)?
    } else {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "opus codec configuration is neither opushead nor dops",
        });
    };
    println!("opus_channels: {}", head.channels);
    println!("opus_pre_skip: {} samples", head.pre_skip);
    println!("opus_input_sample_rate: {} Hz", head.input_sample_rate);
    println!("opus_output_gain: {:.2} dB", head.output_gain_db());
    println!(
        "opus_channel_mapping_family: {}",
        head.channel_mapping_family
    );
    Ok(())
}

/// Prints the Vorbis setup from WebM CodecPrivate: one byte with
/// the header count minus one (2), then the identification and
/// comment header lengths as single bytes, then the three headers.
fn print_vorbis_config(codec_private: &[u8]) -> Result<()> {
    if codec_private.len() < 3 || codec_private[0] != 2 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "vorbis codec private does not start with the webm header table",
        });
    }
    let ident_len = usize::from(codec_private[1]);
    let comment_len = usize::from(codec_private[2]);
    let ident_start = 3;
    let comment_start = ident_start + ident_len;
    let setup_start = comment_start + comment_len;
    if setup_start > codec_private.len() {
        return Err(Error::Truncated {
            needed: setup_start,
            had: codec_private.len(),
        });
    }
    let setup = VorbisSetup::from_headers(
        &codec_private[ident_start..comment_start],
        &codec_private[comment_start..setup_start],
        &codec_private[setup_start..],
    )?;
    println!("vorbis_channels: {}", setup.identification.channels);
    println!(
        "vorbis_sample_rate: {} Hz",
        setup.identification.sample_rate
    );
    println!("vorbis_blocksize_0: {}", setup.identification.blocksize_0);
    println!("vorbis_blocksize_1: {}", setup.identification.blocksize_1);
    println!("vorbis_codebooks: {}", setup.codebooks.len());
    println!("vorbis_floors: {}", setup.floors.len());
    println!("vorbis_residues: {}", setup.residues.len());
    println!("vorbis_mappings: {}", setup.mappings.len());
    println!("vorbis_modes: {}", setup.modes.len());
    Ok(())
}

/// Prints the VP9 configuration from a vpcC box payload (VP Codec
/// ISO Media File Format Binding, "VP Codec Configuration Box").
/// The box is a FullBox, so 4 version/flags bytes (version 1)
/// precede the VPCodecConfigurationRecord: profile, level, bit
/// depth, chroma subsampling, full-range flag, colour primaries,
/// transfer characteristics, and matrix coefficients. WebM VP9
/// tracks carry no CodecPrivate at all — the parameters are in the
/// bitstream — so an empty payload prints a note instead of failing.
fn print_vp9_config(codec_private: &[u8]) -> Result<()> {
    if codec_private.is_empty() {
        println!("vp9_codec_private: none (parameters are in the bitstream)");
        return Ok(());
    }
    if codec_private.len() < 12 {
        return Err(Error::Truncated {
            needed: 12,
            had: codec_private.len(),
        });
    }
    if codec_private[0] != 1 {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "vpcc fullbox version is not 1",
        });
    }
    let record = &codec_private[4..];
    println!("vp9_profile: {}", record[0]);
    println!("vp9_level: {}", record[1]);
    println!("vp9_bit_depth: {}", record[2] >> 4);
    println!("vp9_chroma_subsampling: {}", (record[2] >> 1) & 0x07);
    println!("vp9_full_range: {}", record[2] & 0x01);
    println!("vp9_colour_primaries: {}", record[3]);
    println!("vp9_transfer_characteristics: {}", record[4]);
    println!("vp9_matrix_coefficients: {}", record[5]);
    Ok(())
}

fn cmd_info_container(bytes: &[u8]) -> Result<()> {
    let demuxer = Demuxer::from_bytes(bytes)?;
    println!("container: {:?}", demuxer.container_kind());
    match demuxer.duration_us() {
        Some(duration) => println!("duration: {duration} us"),
        None => println!("duration: unknown"),
    }
    println!("tracks: {}", demuxer.tracks().len());
    println!("packets: {}", demuxer.packet_count());
    println!("metadata_entries: {}", demuxer.metadata().entries.len());
    for entry in &demuxer.metadata().entries {
        println!(
            "metadata: {} = {}",
            entry.key,
            String::from_utf8_lossy(&entry.value)
        );
    }
    for track in demuxer.tracks() {
        let kind = match track.kind {
            TrackKind::Video => "video",
            TrackKind::Audio => "audio",
        };
        println!(
            "track {}: {} {} {}",
            track.index,
            kind,
            track.codec_id,
            track.codec.name()
        );
        if let (Some(width), Some(height)) = (track.width, track.height) {
            println!("  resolution: {width}x{height}");
        }
        if let Some(sample_rate) = track.sample_rate {
            println!("  sample_rate: {sample_rate} Hz");
        }
        if let Some(channels) = track.channels {
            println!("  channels: {channels}");
        }
        if let Some(duration) = track.default_duration_us {
            println!("  default_duration: {duration} us");
        }
        if let Some(duration) = track.duration_us {
            println!("  track_duration: {duration} us");
        }
        match track.codec {
            Codec::Av1 => print_av1_config(&track.codec_private)?,
            Codec::Opus => print_opus_config(&track.codec_private)?,
            Codec::Vorbis => print_vorbis_config(&track.codec_private)?,
            Codec::Vp9 => print_vp9_config(&track.codec_private)?,
            _ => {}
        }
    }
    Ok(())
}

fn cmd_info_ivf(bytes: &[u8]) -> Result<()> {
    if bytes.len() < 32 {
        return Err(Error::Truncated {
            needed: 32,
            had: bytes.len(),
        });
    }
    let fourcc = &bytes[8..12];
    let width = u16::from_le_bytes(bytes[12..14].try_into().unwrap_or([0; 2]));
    let height = u16::from_le_bytes(bytes[14..16].try_into().unwrap_or([0; 2]));
    // The IVF header stores the timebase denominator first, then
    // the numerator (bytes 16-19 den, 20-23 num).
    let timebase_den = u32::from_le_bytes(bytes[16..20].try_into().unwrap_or([0; 4]));
    let timebase_num = u32::from_le_bytes(bytes[20..24].try_into().unwrap_or([0; 4]));
    let num_frames = u32::from_le_bytes(bytes[24..28].try_into().unwrap_or([0; 4]));
    println!("format: IVF");
    println!("fourcc: {}", String::from_utf8_lossy(fourcc));
    println!("resolution: {width}x{height}");
    if timebase_den > 0 {
        println!(
            "frame_rate: {} fps",
            f64::from(timebase_den) / f64::from(timebase_num)
        );
    }
    println!("declared_frames: {num_frames}");
    if fourcc == b"VP90" {
        return cmd_info_ivf_vp9(bytes);
    }
    let mut offset = 32usize;
    let mut frames = 0usize;
    let mut obu_counts = [0usize; 16];
    let mut sequence_header: Option<SequenceHeader> = None;
    while offset + 12 <= bytes.len() {
        let size =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4])) as usize;
        if offset + 12 + size > bytes.len() {
            return Err(Error::Truncated {
                needed: offset + 12 + size,
                had: bytes.len(),
            });
        }
        let payload = &bytes[offset + 12..offset + 12 + size];
        let mut reader = ObuReader::new(payload);
        while let Some(obu) = reader.next_obu()? {
            let index = usize::from(obu.header.obu_type.value());
            if index < obu_counts.len() {
                obu_counts[index] += 1;
            }
            if sequence_header.is_none() && obu.header.obu_type == ObuType::SequenceHeader {
                sequence_header = Some(parse_sequence_header(obu.payload)?);
            }
        }
        frames += 1;
        offset += 12 + size;
    }
    println!("frames: {frames}");
    for (index, count) in obu_counts.iter().enumerate() {
        if *count > 0 {
            println!("obu_type_{index}: {count}");
        }
    }
    match sequence_header {
        Some(seq) => print_av1_sequence_header(&seq),
        None => println!("av1_sequence_header: missing"),
    }
    Ok(())
}

/// VP9 branch of `cmd_info_ivf`: each IVF frame unit may hold a
/// superframe, so it is split first; the reference state is shared
/// across the whole file because inter frames refer to earlier
/// frames. Only the first frame's header is printed in full; the
/// rest are counted by type.
fn cmd_info_ivf_vp9(bytes: &[u8]) -> Result<()> {
    let mut offset = 32usize;
    let mut units = 0usize;
    let mut frames = 0usize;
    let mut key_frames = 0usize;
    let mut state = Vp9ReferenceState::default();
    let mut first_header: Option<Vp9FrameHeader> = None;
    while offset + 12 <= bytes.len() {
        let size =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4])) as usize;
        if offset + 12 + size > bytes.len() {
            return Err(Error::Truncated {
                needed: offset + 12 + size,
                had: bytes.len(),
            });
        }
        let payload = &bytes[offset + 12..offset + 12 + size];
        for (start, len) in parse_superframe(payload)? {
            let frame = parse_vp9_frame(&payload[start..start + len], &mut state)?;
            if frame.header.is_keyframe {
                key_frames += 1;
            }
            if first_header.is_none() {
                first_header = Some(frame.header.clone());
            }
            frames += 1;
        }
        units += 1;
        offset += 12 + size;
    }
    println!("frames: {frames}");
    println!("ivf_units: {units}");
    println!("vp9_key_frames: {key_frames}");
    match first_header {
        Some(header) => print_vp9_frame_header(&header),
        None => println!("vp9_frame_header: missing"),
    }
    Ok(())
}

fn cmd_info_obu(bytes: &[u8]) -> Result<()> {
    println!("format: AV1 OBU stream");
    let mut reader = ObuReader::new(bytes);
    let mut count = 0usize;
    let mut sequence_header: Option<SequenceHeader> = None;
    while let Some(obu) = reader.next_obu()? {
        println!(
            "obu {}: {:?} size {} bytes",
            count,
            obu.header.obu_type,
            obu.payload.len()
        );
        if sequence_header.is_none() && obu.header.obu_type == ObuType::SequenceHeader {
            sequence_header = Some(parse_sequence_header(obu.payload)?);
        }
        count += 1;
    }
    println!("obus: {count}");
    match sequence_header {
        Some(seq) => print_av1_sequence_header(&seq),
        None => println!("av1_sequence_header: missing"),
    }
    Ok(())
}

fn cmd_info(file: &PathBuf) -> Result<()> {
    let bytes = read_file(file)?;
    if bytes.starts_with(b"RIFF") {
        let header = parse_wav_header(&bytes)?;
        println!("format: {:?}", header.format);
        println!("channels: {}", header.channels);
        println!("sample_rate: {} Hz", header.sample_rate);
        println!("bits_per_sample: {}", header.bits_per_sample);
        println!("block_align: {} bytes", header.block_align);
        println!("data_size: {} bytes", header.data_size);
        println!("frames: {}", header.frame_count());
        println!("duration: {} us", header.duration_micros());
        return Ok(());
    }
    if bytes.starts_with(b"fLaC") {
        let stream = parse_flac_stream(&bytes)?;
        let info = stream.streaminfo;
        println!("format: FLAC");
        println!("channels: {}", info.channels);
        println!("sample_rate: {} Hz", info.sample_rate);
        println!("bits_per_sample: {}", info.bits_per_sample);
        println!("total_samples: {}", info.total_samples);
        println!(
            "md5: {}",
            info.md5
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        println!("metadata_entries: {}", stream.metadata.entries.len());
        return Ok(());
    }
    if bytes.starts_with(b"OggS") {
        let mut demuxer = OggDemuxer::new(std::io::Cursor::new(bytes))?;
        println!("format: Ogg");
        println!("codec: {}", demuxer.codec().name());
        println!("serial: {}", demuxer.serial());
        println!("pages: {}", demuxer.page_count());
        println!("packets: {}", demuxer.packet_count());
        match demuxer.codec() {
            Codec::Opus => {
                let head_packet = demuxer.next_packet()?.ok_or(Error::InvalidData {
                    offset: 0,
                    detail: "ogg opus stream contains no packets",
                })?;
                let head = parse_opus_head(&head_packet.data)?;
                println!("channels: {}", head.channels);
                println!("pre_skip: {} samples", head.pre_skip);
                println!("input_sample_rate: {} Hz", head.input_sample_rate);
                println!("output_gain: {:.2} dB", head.output_gain_db());
                println!("channel_mapping_family: {}", head.channel_mapping_family);
                if let Some(tags_packet) = demuxer.next_packet()? {
                    let tags = parse_opus_tags(&tags_packet.data)?;
                    println!("vendor: {}", String::from_utf8_lossy(&tags.vendor));
                    println!("comments: {}", tags.comments.len());
                }
            }
            Codec::Vorbis => {
                let identification = demuxer.next_packet()?.ok_or(Error::InvalidData {
                    offset: 0,
                    detail: "ogg vorbis stream contains no packets",
                })?;
                let comment = demuxer.next_packet()?.ok_or(Error::InvalidData {
                    offset: 0,
                    detail: "ogg vorbis stream is missing the comment header",
                })?;
                let setup_packet = demuxer.next_packet()?.ok_or(Error::InvalidData {
                    offset: 0,
                    detail: "ogg vorbis stream is missing the setup header",
                })?;
                let setup = VorbisSetup::from_headers(
                    &identification.data,
                    &comment.data,
                    &setup_packet.data,
                )?;
                println!("channels: {}", setup.identification.channels);
                println!("sample_rate: {} Hz", setup.identification.sample_rate);
                println!("blocksize_0: {}", setup.identification.blocksize_0);
                println!("blocksize_1: {}", setup.identification.blocksize_1);
                println!("vendor: {}", String::from_utf8_lossy(&setup.comment.vendor));
                println!("comments: {}", setup.comment.comments.len());
                println!("codebooks: {}", setup.codebooks.len());
                println!("floors: {}", setup.floors.len());
                println!("residues: {}", setup.residues.len());
                println!("mappings: {}", setup.mappings.len());
                println!("modes: {}", setup.modes.len());
            }
            _ => {}
        }
        return Ok(());
    }
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) || (bytes.len() >= 8 && &bytes[4..8] == b"ftyp")
    {
        return cmd_info_container(&bytes);
    }
    if bytes.starts_with(b"DKIF") {
        return cmd_info_ivf(&bytes);
    }
    if file.extension().is_some_and(|extension| extension == "obu") {
        return cmd_info_obu(&bytes);
    }
    Err(Error::Unsupported("unknown file signature"))
}

fn cmd_decode_wav(bytes: Vec<u8>, output: &PathBuf) -> Result<()> {
    let mut demuxer = WavDemuxer::new(std::io::Cursor::new(bytes))?;
    let header = *demuxer.header();
    let mut decoder = Decoder::from_wav_header(&header)?;
    let mut encoder = Encoder::new(EncoderConfig {
        sample_rate: header.sample_rate,
        channels: header.channels,
        pcm_format: header.format,
        ..EncoderConfig::default()
    })?;
    let out_file = File::create(output)?;
    let mut muxer = WavMuxer::new(out_file, header.format, header.channels, header.sample_rate)?;
    while let Some(packet) = demuxer.next_packet()? {
        let frame = decoder.decode(&packet)?;
        let encoded = encoder.encode(&frame)?;
        muxer.write_packet(&encoded)?;
    }
    muxer.finish()?;
    println!("decoded {} frames", header.frame_count());
    Ok(())
}

fn write_decoded_frames(
    frames: &[Frame],
    output: &PathBuf,
    format: PcmFormat,
    channels: u32,
    sample_rate: u32,
) -> Result<usize> {
    let out_file = File::create(output)?;
    let mut encoder = Encoder::new(EncoderConfig {
        sample_rate,
        channels,
        pcm_format: format,
        ..EncoderConfig::default()
    })?;
    let mut muxer = WavMuxer::new(out_file, format, channels, sample_rate)?;
    let mut total_frames = 0usize;
    for frame in frames {
        let encoded = encoder.encode(frame)?;
        muxer.write_packet(&encoded)?;
        if let Frame::Audio(audio) = frame {
            total_frames += audio.frame_count();
        }
    }
    muxer.finish()?;
    Ok(total_frames)
}

fn cmd_decode_flac(bytes: Vec<u8>, output: &PathBuf) -> Result<()> {
    let mut demuxer = FlacDemuxer::new(std::io::Cursor::new(bytes))?;
    let streaminfo = *demuxer.streaminfo();
    let mut decoder = Decoder::from_flac_streaminfo(&streaminfo)?;
    let format = wav_format_for_flac(streaminfo.bits_per_sample);
    let mut frames: Vec<Frame> = Vec::new();
    while let Some(packet) = demuxer.next_packet()? {
        let frame = decoder.decode(&packet)?;
        frames.push(frame);
    }
    let total = write_decoded_frames(
        &frames,
        output,
        format,
        streaminfo.channels,
        streaminfo.sample_rate,
    )?;
    println!("decoded {total} frames");
    Ok(())
}

fn cmd_decode_opus_ogg(demuxer: &mut OggDemuxer, output: &PathBuf) -> Result<()> {
    let head_packet = demuxer.next_packet()?.ok_or(Error::InvalidData {
        offset: 0,
        detail: "ogg opus stream contains no packets",
    })?;
    let head = parse_opus_head(&head_packet.data)?;
    // The second packet is OpusTags; it carries no audio.
    let _ = demuxer.next_packet()?;
    let mut decoder = Decoder::from_opus_head(&head)?;
    let mut frames: Vec<Frame> = Vec::new();
    while let Some(packet) = demuxer.next_packet()? {
        let frame = decoder.decode(&packet)?;
        frames.push(frame);
    }
    // Opus decodes at the 48 kHz internal rate. Decoded samples are
    // f32, so the WAV is written as F32 to preserve precision.
    let total = write_decoded_frames(
        &frames,
        output,
        PcmFormat::F32,
        u32::from(head.channels),
        48_000,
    )?;
    println!("decoded {total} frames");
    Ok(())
}

fn cmd_decode_vorbis_ogg(demuxer: &mut OggDemuxer, output: &PathBuf) -> Result<()> {
    let identification = demuxer.next_packet()?.ok_or(Error::InvalidData {
        offset: 0,
        detail: "ogg vorbis stream contains no packets",
    })?;
    let comment = demuxer.next_packet()?.ok_or(Error::InvalidData {
        offset: 0,
        detail: "ogg vorbis stream is missing the comment header",
    })?;
    let setup_packet = demuxer.next_packet()?.ok_or(Error::InvalidData {
        offset: 0,
        detail: "ogg vorbis stream is missing the setup header",
    })?;
    let setup = VorbisSetup::from_headers(&identification.data, &comment.data, &setup_packet.data)?;
    let channels = setup.identification.channels;
    let sample_rate = setup.identification.sample_rate;
    let mut decoder = Decoder::from_vorbis_setup(setup)?;
    let mut frames: Vec<Frame> = Vec::new();
    while let Some(packet) = demuxer.next_packet()? {
        let frame = decoder.decode(&packet)?;
        frames.push(frame);
    }
    // The last block's second half is only released by flush.
    if let Some(frame) = decoder.flush()? {
        frames.push(frame);
    }
    // Decoded samples are f32, so the WAV is written as F32 to
    // preserve precision.
    let total = write_decoded_frames(&frames, output, PcmFormat::F32, channels, sample_rate)?;
    println!("decoded {total} frames");
    Ok(())
}

fn cmd_decode_ogg(bytes: Vec<u8>, output: &PathBuf) -> Result<()> {
    let mut demuxer = OggDemuxer::new(std::io::Cursor::new(bytes))?;
    match demuxer.codec() {
        Codec::Opus => return cmd_decode_opus_ogg(&mut demuxer, output),
        Codec::Vorbis => return cmd_decode_vorbis_ogg(&mut demuxer, output),
        Codec::Flac => {}
        _ => {
            return Err(Error::Unsupported("ogg codec decode"));
        }
    }
    let first = demuxer.next_packet()?.ok_or(Error::InvalidData {
        offset: 0,
        detail: "ogg stream contains no packets",
    })?;
    let (streaminfo, header_packet_count) = parse_ogg_flac_first_packet(&first.data)?;
    let mut decoder = Decoder::from_flac_streaminfo(&streaminfo)?;
    let format = wav_format_for_flac(streaminfo.bits_per_sample);
    // Skip the header packets that follow the first packet. The count
    // is authoritative when present; when it is 0 (unknown), metadata
    // blocks are recognized by shape instead.
    let mut skipped = 0usize;
    let mut pending: Vec<codec_whatttttttttttt_core::Packet> = Vec::new();
    while let Some(packet) = demuxer.next_packet()? {
        if header_packet_count > 0 {
            if skipped < header_packet_count as usize {
                skipped += 1;
                continue;
            }
        } else if codec_whatttttttttttt_core::is_metadata_block(&packet.data) {
            continue;
        }
        pending.push(packet);
    }
    let mut frames: Vec<Frame> = Vec::new();
    for packet in &pending {
        let frame = decoder.decode(packet)?;
        frames.push(frame);
    }
    let total = write_decoded_frames(
        &frames,
        output,
        format,
        streaminfo.channels,
        streaminfo.sample_rate,
    )?;
    println!("decoded {total} frames");
    Ok(())
}

fn cmd_decode(input: &PathBuf, output: &PathBuf) -> Result<()> {
    let bytes = read_file(input)?;
    if bytes.starts_with(b"RIFF") {
        return cmd_decode_wav(bytes, output);
    }
    if bytes.starts_with(b"fLaC") {
        return cmd_decode_flac(bytes, output);
    }
    if bytes.starts_with(b"OggS") {
        return cmd_decode_ogg(bytes, output);
    }
    Err(Error::Unsupported("unknown file signature"))
}

/// Encodes a WAV file as FLAC (Phase 9): the WAV is decoded to
/// f32, quantized to i16, and written as a complete FLAC file with
/// exact STREAMINFO (total samples, min/max block and frame sizes).
/// The one-shot encoder is used because the whole WAV is already
/// in memory; the streaming `Encoder` API is for incremental use.
#[allow(clippy::cast_possible_truncation)]
fn cmd_encode(input: &PathBuf, output: &PathBuf) -> Result<()> {
    use std::io::Write as _;
    let bytes = read_file(input)?;
    if !bytes.starts_with(b"RIFF") {
        return Err(Error::Unsupported("encode input must be a WAV file"));
    }
    let demuxer = WavDemuxer::new(std::io::Cursor::new(bytes))?;
    let header = *demuxer.header();
    let samples = demuxer.samples()?;
    // Same saturating f32 -> i16 scale as `pcm` I16 encoding: the
    // input is clamped to [-1, 1], so the scaled value fits in i32
    // and the clamped result fits in i16.
    let samples_i16: Vec<i16> = samples
        .iter()
        .map(|&sample| {
            let clamped = sample.clamp(-1.0, 1.0);
            let value = (clamped * 32768.0).round() as i32;
            value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
        })
        .collect();
    let encoded = codec_whatttttttttttt_core::encode_flac_stream(
        &samples_i16,
        header.channels,
        header.sample_rate,
    )?;
    let mut out_file = File::create(output)?;
    out_file.write_all(&encoded)?;
    println!(
        "encoded {} frames",
        samples.len() / header.channels as usize
    );
    Ok(())
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Info { file } => cmd_info(file),
        Command::Decode { input, output } => cmd_decode(input, output),
        Command::Encode { input, output } => cmd_encode(input, output),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(Error::Io(err)) => {
            eprintln!("error: io error: {err}");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
