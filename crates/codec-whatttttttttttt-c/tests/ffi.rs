//! Tests for the C ABI, calling the FFI functions from Rust.
//! These verify the boundary (pointer handling, error mapping,
//! borrowed views, ownership), not the core algorithms, which
//! have their own tests.

use codec_whatttttttttttt::{
    codec_av1_parse_sequence_header, codec_decoder_decode, codec_decoder_free,
    codec_decoder_from_flac_streaminfo, codec_decoder_from_vp9_frame_header,
    codec_decoder_from_wav_header, codec_encode_flac, codec_encoder_encode,
    codec_encoder_flac_streaminfo, codec_encoder_free, codec_encoder_new_flac, codec_error_string,
    codec_free, codec_last_error, codec_probe, codec_version, codec_vp9_parse_frame_header,
    CodecAudioFrame, CodecAv1SequenceHeader, CodecDecoder, CodecEncoder, CodecPacket, CodecProbe,
    CodecVp9FrameHeader, CODEC_CODEC_AV1, CODEC_CODEC_FLAC, CODEC_CODEC_PCM, CODEC_CODEC_UNKNOWN,
    CODEC_CODEC_VP9, CODEC_CONTAINER_IVF, CODEC_CONTAINER_RAW_FLAC, CODEC_CONTAINER_UNKNOWN,
    CODEC_CONTAINER_WAV, CODEC_ERR_INVALID_ARGUMENT, CODEC_ERR_TRUNCATED, CODEC_ERR_UNSUPPORTED,
    CODEC_OK,
};
use codec_whatttttttttttt_core::{parse_obu, FlacDemuxer, ObuType};
use std::ffi::CStr;
use std::io::Cursor;
use std::ptr;

fn data_path(name: &str) -> String {
    format!("../codec-whatttttttttttt-core/tests/data/{name}")
}

fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(data_path(name)).unwrap_or_else(|_| panic!("missing fixture {name}"))
}

fn cstr(ptr: *const std::ffi::c_char) -> String {
    assert!(!ptr.is_null());
    // SAFETY: the FFI functions return pointers to NUL-terminated
    // static strings, or to a thread-local detail string that is
    // valid until the next FFI call.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

#[test]
fn version_and_error_strings() {
    assert_eq!(cstr(codec_version()), "0.1.0");
    assert_eq!(cstr(codec_error_string(CODEC_OK)), "ok");
    assert_eq!(
        cstr(codec_error_string(CODEC_ERR_UNSUPPORTED)),
        "unsupported feature"
    );
    assert_eq!(cstr(codec_error_string(999)), "unknown error code");
    assert!(codec_last_error().is_null());
}

#[test]
fn probe_identifies_wav_flac_and_ivf() {
    let mut probe = CodecProbe {
        container: -99,
        codec: -99,
    };
    let wav = read_fixture("valid_i16_stereo_44100.wav");
    let rc = unsafe { codec_probe(wav.as_ptr(), wav.len(), &mut probe) };
    assert_eq!(rc, CODEC_OK);
    assert_eq!(probe.container, CODEC_CONTAINER_WAV);
    assert_eq!(probe.codec, CODEC_CODEC_PCM);

    let flac = read_fixture("sine_stereo_44100.flac");
    let rc = unsafe { codec_probe(flac.as_ptr(), flac.len(), &mut probe) };
    assert_eq!(rc, CODEC_OK);
    assert_eq!(probe.container, CODEC_CONTAINER_RAW_FLAC);
    assert_eq!(probe.codec, CODEC_CODEC_FLAC);

    let ivf = read_fixture("vp9_64x64.ivf");
    let rc = unsafe { codec_probe(ivf.as_ptr(), ivf.len(), &mut probe) };
    assert_eq!(rc, CODEC_OK);
    assert_eq!(probe.container, CODEC_CONTAINER_IVF);
    assert_eq!(probe.codec, CODEC_CODEC_VP9);

    let av1 = read_fixture("av1_64x64.ivf");
    let rc = unsafe { codec_probe(av1.as_ptr(), av1.len(), &mut probe) };
    assert_eq!(rc, CODEC_OK);
    assert_eq!(probe.container, CODEC_CONTAINER_IVF);
    assert_eq!(probe.codec, CODEC_CODEC_AV1);

    // Garbage is unknown, not an error.
    let garbage = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03];
    let rc = unsafe { codec_probe(garbage.as_ptr(), garbage.len(), &mut probe) };
    assert_eq!(rc, CODEC_OK);
    assert_eq!(probe.container, CODEC_CONTAINER_UNKNOWN);
    assert_eq!(probe.codec, CODEC_CODEC_UNKNOWN);

    // Fewer than 4 bytes is truncated, and the detail is stored.
    let short = [0x52, 0x49];
    let rc = unsafe { codec_probe(short.as_ptr(), short.len(), &mut probe) };
    assert_eq!(rc, CODEC_ERR_TRUNCATED);
    assert!(cstr(codec_last_error()).contains("truncated"));
}

#[test]
fn null_pointers_return_invalid_argument_without_crashing() {
    let mut probe = CodecProbe {
        container: 0,
        codec: 0,
    };
    let rc = unsafe { codec_probe(ptr::null(), 0, &mut probe) };
    assert_eq!(rc, CODEC_ERR_INVALID_ARGUMENT);
    let rc = unsafe { codec_probe(b"RIFF".as_ptr(), 4, ptr::null_mut()) };
    assert_eq!(rc, CODEC_ERR_INVALID_ARGUMENT);

    let mut decoder: *mut CodecDecoder = ptr::null_mut();
    let rc = unsafe { codec_decoder_from_wav_header(ptr::null(), 0, &mut decoder) };
    assert_eq!(rc, CODEC_ERR_INVALID_ARGUMENT);
    assert!(decoder.is_null());

    let rc = unsafe { codec_decoder_decode(ptr::null_mut(), b"".as_ptr(), 0, 0, ptr::null_mut()) };
    assert_eq!(rc, CODEC_ERR_INVALID_ARGUMENT);

    // Freeing NULL is a no-op, not a crash.
    unsafe {
        codec_decoder_free(ptr::null_mut());
        codec_encoder_free(ptr::null_mut());
        codec_free(ptr::null_mut());
    }
}

#[test]
fn wav_decode_round_trip_via_ffi() {
    let wav = read_fixture("valid_i16_stereo_44100.wav");
    // The whole WAV file: `parse_wav_header` checks the RIFF size
    // against the length given, so a header-only slice is
    // truncated. The PCM data starts at offset 44.
    let mut decoder: *mut CodecDecoder = ptr::null_mut();
    let rc = unsafe { codec_decoder_from_wav_header(wav.as_ptr(), wav.len(), &mut decoder) };
    assert_eq!(rc, CODEC_OK);
    assert!(!decoder.is_null());

    let mut frame: *const CodecAudioFrame = ptr::null();
    let pcm = &wav[44..];
    let rc = unsafe { codec_decoder_decode(decoder, pcm.as_ptr(), pcm.len(), 0, &mut frame) };
    assert_eq!(rc, CODEC_OK);
    assert!(!frame.is_null());
    // SAFETY: `frame` points to the decoder's borrowed view, valid
    // until the next decode call or free.
    let view = unsafe { &*frame };
    assert_eq!(view.sample_rate, 44100);
    assert_eq!(view.channels, 2);
    assert_eq!(view.frames, 4410);
    assert!(!view.samples.is_null());
    // The first samples are a 440 Hz sine at amplitude 20000/32768.
    let samples = unsafe { std::slice::from_raw_parts(view.samples, view.frames as usize * 2) };
    assert!((samples[0]).abs() < 0.001, "sine starts at zero");
    assert!(
        samples[1..].iter().any(|&s| s > 0.5),
        "sine reaches a positive peak"
    );
    unsafe { codec_decoder_free(decoder) };
}

#[test]
fn flac_decode_via_ffi() {
    let flac = read_fixture("sine_stereo_44100.flac");
    // STREAMINFO is the 34 payload bytes at offset 8.
    let mut decoder: *mut CodecDecoder = ptr::null_mut();
    let rc = unsafe { codec_decoder_from_flac_streaminfo(flac.as_ptr().add(8), 34, &mut decoder) };
    assert_eq!(rc, CODEC_OK);

    // The first frame packet comes from the core demuxer; the FFI
    // decoder only sees the raw packet bytes.
    let mut demuxer = FlacDemuxer::new(Cursor::new(flac.clone())).unwrap();
    let packet = demuxer.next_packet().unwrap().unwrap();
    let mut frame: *const CodecAudioFrame = ptr::null();
    let rc = unsafe {
        codec_decoder_decode(
            decoder,
            packet.data.as_ptr(),
            packet.data.len(),
            packet.timestamp,
            &mut frame,
        )
    };
    assert_eq!(rc, CODEC_OK);
    let view = unsafe { &*frame };
    assert_eq!(view.sample_rate, 44100);
    assert_eq!(view.channels, 2);
    // ffmpeg encoded the 4410 samples as a single FLAC frame.
    assert_eq!(view.frames, 4410);
    let samples = unsafe { std::slice::from_raw_parts(view.samples, view.frames as usize * 2) };
    assert!((samples[0]).abs() < 0.001);
    unsafe { codec_decoder_free(decoder) };
}

#[test]
fn flac_encode_one_shot_via_ffi_round_trips() {
    // A 1000-frame stereo sine, as i16.
    let mut samples = Vec::with_capacity(2000);
    for n in 0..1000 {
        let t = n as f64 / 44100.0;
        let left = (20000.0 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16;
        let right = (15000.0 * (2.0 * std::f64::consts::PI * 660.0 * t).sin()) as i16;
        samples.push(left);
        samples.push(right);
    }
    let mut out_data: *mut u8 = ptr::null_mut();
    let mut out_len: usize = 0;
    let rc = unsafe {
        codec_encode_flac(
            samples.as_ptr(),
            1000,
            2,
            44100,
            &mut out_data,
            &mut out_len,
        )
    };
    assert_eq!(rc, CODEC_OK);
    assert!(!out_data.is_null());
    assert!(out_len > 42);
    let encoded = unsafe { std::slice::from_raw_parts(out_data, out_len) };
    assert_eq!(&encoded[0..4], b"fLaC");

    // Decode the FFI output with the core decoder and compare.
    let mut demuxer = FlacDemuxer::new(Cursor::new(encoded.to_vec())).unwrap();
    let mut decoded: Vec<i16> = Vec::new();
    let mut decoder =
        codec_whatttttttttttt_core::Decoder::from_flac_streaminfo(demuxer.streaminfo()).unwrap();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        let frame = decoder.decode(&packet).unwrap();
        let codec_whatttttttttttt_core::Frame::Audio(audio) = frame else {
            panic!("expected audio frame");
        };
        for &sample in &audio.samples {
            let value = (sample * 32768.0).round() as i32;
            decoded.push(value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16);
        }
    }
    assert_eq!(decoded, samples);
    unsafe { codec_free(out_data.cast()) };
}

#[test]
fn flac_encode_streaming_via_ffi() {
    let mut encoder: *mut CodecEncoder = ptr::null_mut();
    let rc = unsafe { codec_encoder_new_flac(8000, 1, &mut encoder) };
    assert_eq!(rc, CODEC_OK);

    let mut packet: *const CodecPacket = ptr::null();
    let rc = unsafe { codec_encoder_flac_streaminfo(encoder, &mut packet) };
    assert_eq!(rc, CODEC_OK);
    let header = unsafe { &*packet };
    assert_eq!(header.len, 42);
    let header_bytes = unsafe { std::slice::from_raw_parts(header.data, header.len) };
    assert_eq!(&header_bytes[0..4], b"fLaC");

    // One 1000-frame mono sine frame.
    let samples: Vec<f32> = (0..1000)
        .map(|n| {
            let t = n as f32 / 8000.0;
            (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5
        })
        .collect();
    let rc = unsafe { codec_encoder_encode(encoder, samples.as_ptr(), 1000, &mut packet) };
    assert_eq!(rc, CODEC_OK);
    let encoded = unsafe { &*packet };
    assert!(encoded.len > 4);
    let frame_bytes = unsafe { std::slice::from_raw_parts(encoded.data, encoded.len) };
    // A FLAC frame starts with the 0xFF sync byte.
    assert_eq!(frame_bytes[0], 0xFF);
    unsafe { codec_encoder_free(encoder) };
}

#[test]
fn vp9_decode_returns_unsupported_but_header_parses() {
    let ivf = read_fixture("vp9_64x64.ivf");
    // First frame: 32-byte IVF header, then a 12-byte frame header.
    let frame_size = u32::from_le_bytes([ivf[32], ivf[33], ivf[34], ivf[35]]) as usize;
    let frame = &ivf[44..44 + frame_size];

    let mut header = CodecVp9FrameHeader {
        profile: 0,
        frame_type: 0,
        show_frame: 0,
        show_existing_frame: 0,
        width: 0,
        height: 0,
        render_width: 0,
        render_height: 0,
        base_q_idx: 0,
        loop_filter_level: 0,
        tile_cols_log2: 0,
        tile_rows_log2: 0,
        header_size_in_bytes: 0,
    };
    let rc = unsafe { codec_vp9_parse_frame_header(frame.as_ptr(), frame.len(), &mut header) };
    assert_eq!(rc, CODEC_OK);
    assert_eq!(header.profile, 0);
    assert_eq!(header.frame_type, 0);
    assert_eq!(header.show_frame, 1);
    assert_eq!(header.width, 64);
    assert_eq!(header.height, 64);
    assert_eq!(header.base_q_idx, 33);
    assert_eq!(header.loop_filter_level, 3);
    assert_eq!(header.header_size_in_bytes, 63);

    // A decoder built from the header refuses to decode (parser-only).
    let mut decoder: *mut CodecDecoder = ptr::null_mut();
    let rc =
        unsafe { codec_decoder_from_vp9_frame_header(frame.as_ptr(), frame.len(), &mut decoder) };
    assert_eq!(rc, CODEC_OK);
    let mut out_frame: *const CodecAudioFrame = ptr::null();
    let rc =
        unsafe { codec_decoder_decode(decoder, frame.as_ptr(), frame.len(), 0, &mut out_frame) };
    assert_eq!(rc, CODEC_ERR_UNSUPPORTED);
    unsafe { codec_decoder_free(decoder) };
}

#[test]
fn av1_sequence_header_parses_via_ffi() {
    let ivf = read_fixture("av1_64x64.ivf");
    let frame_size = u32::from_le_bytes([ivf[32], ivf[33], ivf[34], ivf[35]]) as usize;
    let frame = &ivf[44..44 + frame_size];
    // Walk the OBUs: the frame starts with a temporal delimiter,
    // the sequence header follows.
    let mut offset = 0usize;
    let mut seq_obu: Option<(usize, usize)> = None;
    while offset < frame.len() {
        let (obu, next) = parse_obu(frame, offset).unwrap();
        if obu.header.obu_type == ObuType::SequenceHeader {
            seq_obu = Some((offset, next));
            break;
        }
        offset = next;
    }
    let (start, next) = seq_obu.expect("sequence header obu in frame");
    let obu_bytes = &frame[start..next];

    let mut header = CodecAv1SequenceHeader {
        seq_profile: 0,
        seq_level_idx_0: 0,
        seq_tier_0: 0,
        max_frame_width_minus_1: 0,
        max_frame_height_minus_1: 0,
        frame_width_bits_minus_1: 0,
        frame_height_bits_minus_1: 0,
        reduced_still_picture_header: 0,
    };
    let rc = unsafe {
        codec_av1_parse_sequence_header(obu_bytes.as_ptr(), obu_bytes.len(), &mut header)
    };
    assert_eq!(rc, CODEC_OK);
    assert_eq!(header.seq_profile, 0);
    assert_eq!(header.max_frame_width_minus_1, 63);
    assert_eq!(header.max_frame_height_minus_1, 63);

    // A non-sequence OBU is an invalid argument: take the second
    // OBU in the frame (a frame header or tile group), not the
    // sequence header.
    let (_, next2) = parse_obu(frame, next).unwrap();
    let second_obu = &frame[next..next2];
    let rc = unsafe {
        codec_av1_parse_sequence_header(second_obu.as_ptr(), second_obu.len(), &mut header)
    };
    assert_eq!(rc, CODEC_ERR_INVALID_ARGUMENT);
}
