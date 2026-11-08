//! C ABI for `codec-whatttttttttttt` (Phase 10).
//!
//! Thin wrapper over the core. Every function validates its
//! pointers, runs the core call inside `catch_unwind` (a panic must
//! never cross the FFI boundary), and maps the core `Error` to a
//! `codec_error_t` code, storing the detail in a thread-local for
//! `codec_last_error`. The C header is
//! `include/codec_whatttttttttttt.h`; the codes there must match the
//! constants here.

use codec_whatttttttttttt_core::{
    parse_obu, parse_sequence_header, parse_streaminfo, parse_vp9_frame_header, parse_wav_header,
    AudioFrame, Codec, Decoder, Encoder, EncoderConfig, Error, Frame, ObuType, Packet,
    Vp9FrameHeader, Vp9ReferenceState,
};
use std::cell::RefCell;
use std::ffi::{c_void, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::slice;

// Error codes (must match codec_whatttttttttttt.h).
pub const CODEC_OK: i32 = 0;
pub const CODEC_ERR_IO: i32 = 1;
pub const CODEC_ERR_INVALID_DATA: i32 = 2;
pub const CODEC_ERR_TRUNCATED: i32 = 3;
pub const CODEC_ERR_UNSUPPORTED: i32 = 4;
pub const CODEC_ERR_OVERFLOW: i32 = 5;
pub const CODEC_ERR_ALLOCATION_LIMIT: i32 = 6;
pub const CODEC_ERR_INVALID_ARGUMENT: i32 = 7;

// Codec ids (must match codec_whatttttttttttt.h). Only the ids
// `codec_probe` can return are needed here; Opus and Vorbis need a
// demuxer to identify, so their ids live only in the header.
pub const CODEC_CODEC_UNKNOWN: i32 = -1;
pub const CODEC_CODEC_PCM: i32 = 0;
pub const CODEC_CODEC_FLAC: i32 = 1;
pub const CODEC_CODEC_AV1: i32 = 4;
pub const CODEC_CODEC_VP9: i32 = 5;

// Container ids (must match codec_whatttttttttttt.h).
pub const CODEC_CONTAINER_UNKNOWN: i32 = 0;
pub const CODEC_CONTAINER_WAV: i32 = 1;
pub const CODEC_CONTAINER_RAW_FLAC: i32 = 2;
pub const CODEC_CONTAINER_OGG: i32 = 3;
pub const CODEC_CONTAINER_WEBM: i32 = 4;
pub const CODEC_CONTAINER_MP4: i32 = 5;
pub const CODEC_CONTAINER_IVF: i32 = 6;

thread_local! {
    // The detail is stored as a `CString` so the pointer returned
    // by `codec_last_error` is always NUL-terminated; a plain
    // `String` has no terminator and a C caller would read past it.
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(detail: String) {
    // Error details come from thiserror and never contain a NUL in
    // practice; if one ever does, fall back to a fixed message
    // rather than truncating silently.
    let cstring = CString::new(detail)
        .unwrap_or_else(|_| CString::new("error detail contained a NUL byte").unwrap_or_default());
    LAST_ERROR.with(|cell| {
        *cell.borrow_mut() = Some(cstring);
    });
}

fn clear_last_error() {
    LAST_ERROR.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

fn error_code(err: &Error) -> i32 {
    match err {
        Error::Io(_) => CODEC_ERR_IO,
        Error::InvalidData { .. } => CODEC_ERR_INVALID_DATA,
        Error::Truncated { .. } => CODEC_ERR_TRUNCATED,
        Error::Unsupported(_) => CODEC_ERR_UNSUPPORTED,
        Error::Overflow(_) => CODEC_ERR_OVERFLOW,
        Error::AllocationLimit { .. } => CODEC_ERR_ALLOCATION_LIMIT,
        Error::InvalidArgument(_) => CODEC_ERR_INVALID_ARGUMENT,
    }
}

/// Runs a fallible core call, catching panics and mapping errors.
/// A panic is reported as `InvalidArgument` with a generic detail:
/// the FFI boundary must never unwind.
fn ffi_call<F>(f: F) -> i32
where
    F: FnOnce() -> Result<(), Error> + std::panic::UnwindSafe,
{
    clear_last_error();
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => CODEC_OK,
        Ok(Err(err)) => {
            let code = error_code(&err);
            set_last_error(err.to_string());
            code
        }
        Err(_) => {
            set_last_error("internal panic in codec library".to_string());
            CODEC_ERR_INVALID_ARGUMENT
        }
    }
}

/// A borrowed view of a decoded audio frame. Layout must match
/// `codec_audio_frame_t` in the header (sample_rate, channels,
/// frames, timestamp_us, samples).
#[repr(C)]
pub struct CodecAudioFrame {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    pub timestamp_us: u64,
    pub samples: *const f32,
}

/// A borrowed view of an encoded packet. Layout must match
/// `codec_packet_t` in the header (data, len, timestamp_us).
#[repr(C)]
pub struct CodecPacket {
    pub data: *const u8,
    pub len: usize,
    pub timestamp_us: u64,
}

/// Probe result. Layout must match `codec_probe_t` in the header
/// (container, codec).
#[repr(C)]
pub struct CodecProbe {
    pub container: i32,
    pub codec: i32,
}

/// VP9 frame-header view. Layout must match
/// `codec_vp9_frame_header_t` in the header.
#[repr(C)]
pub struct CodecVp9FrameHeader {
    pub profile: u8,
    pub frame_type: u8,
    pub show_frame: u8,
    pub show_existing_frame: u8,
    pub width: u16,
    pub height: u16,
    pub render_width: u16,
    pub render_height: u16,
    pub base_q_idx: u8,
    pub loop_filter_level: u8,
    pub tile_cols_log2: u8,
    pub tile_rows_log2: u8,
    pub header_size_in_bytes: u32,
}

/// AV1 sequence-header view. Layout must match
/// `codec_av1_sequence_header_t` in the header.
#[repr(C)]
pub struct CodecAv1SequenceHeader {
    pub seq_profile: u8,
    pub seq_level_idx_0: u8,
    pub seq_tier_0: u8,
    pub max_frame_width_minus_1: u16,
    pub max_frame_height_minus_1: u16,
    pub frame_width_bits_minus_1: u8,
    pub frame_height_bits_minus_1: u8,
    pub reduced_still_picture_header: u8,
}

/// Opaque decoder handle. The `view` points into `last_frame`'s
/// sample buffer (a heap allocation owned by the `Vec`), so it
/// stays valid while `last_frame` is untouched; it is refreshed on
/// every decode. The handle itself is heap-allocated once and
/// never moved, so the stored pointer is stable.
pub struct CodecDecoder {
    decoder: Decoder,
    codec: Codec,
    last_frame: Option<AudioFrame>,
    view: CodecAudioFrame,
}

/// Opaque encoder handle. Same borrowed-view pattern as the
/// decoder: `view` points into `last_packet`'s data buffer.
pub struct CodecEncoder {
    encoder: Encoder,
    sample_rate: u32,
    channels: u32,
    frames_encoded: u64,
    last_packet: Option<Packet>,
    view: CodecPacket,
}

/// Returns a pointer to a NUL-terminated static string. The
/// pointer is valid for the life of the process.
fn static_cstr(bytes: &'static [u8]) -> *const std::ffi::c_char {
    bytes.as_ptr().cast()
}

#[no_mangle]
pub extern "C" fn codec_version() -> *const std::ffi::c_char {
    static_cstr(b"0.1.0\0")
}

#[no_mangle]
pub extern "C" fn codec_error_string(code: i32) -> *const std::ffi::c_char {
    match code {
        CODEC_OK => static_cstr(b"ok\0"),
        CODEC_ERR_IO => static_cstr(b"io error\0"),
        CODEC_ERR_INVALID_DATA => static_cstr(b"invalid data\0"),
        CODEC_ERR_TRUNCATED => static_cstr(b"truncated input\0"),
        CODEC_ERR_UNSUPPORTED => static_cstr(b"unsupported feature\0"),
        CODEC_ERR_OVERFLOW => static_cstr(b"overflow\0"),
        CODEC_ERR_ALLOCATION_LIMIT => static_cstr(b"allocation limit exceeded\0"),
        CODEC_ERR_INVALID_ARGUMENT => static_cstr(b"invalid argument\0"),
        _ => static_cstr(b"unknown error code\0"),
    }
}

#[no_mangle]
pub extern "C" fn codec_last_error() -> *const std::ffi::c_char {
    LAST_ERROR.with(|cell| match cell.borrow().as_ref() {
        Some(detail) => detail.as_ptr().cast(),
        None => ptr::null(),
    })
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `data` points to `len` readable bytes and `out` points to one writable `CodecProbe`.
pub unsafe extern "C" fn codec_probe(data: *const u8, len: usize, out: *mut CodecProbe) -> i32 {
    ffi_call(|| {
        if data.is_null() || out.is_null() {
            return Err(Error::InvalidArgument("codec_probe: null pointer"));
        }
        // SAFETY: the caller guarantees `data` points to `len` readable
        // bytes and `out` to one writable `CodecProbe`; both were
        // checked non-null above.
        let bytes = unsafe { slice::from_raw_parts(data, len) };
        let probe = unsafe { &mut *out };
        if bytes.len() < 4 {
            return Err(Error::Truncated {
                needed: 4,
                had: bytes.len(),
            });
        }
        let (container, codec) = if &bytes[0..4] == b"RIFF" {
            (CODEC_CONTAINER_WAV, CODEC_CODEC_PCM)
        } else if &bytes[0..4] == b"fLaC" {
            (CODEC_CONTAINER_RAW_FLAC, CODEC_CODEC_FLAC)
        } else if &bytes[0..4] == b"OggS" {
            (CODEC_CONTAINER_OGG, CODEC_CODEC_UNKNOWN)
        } else if bytes[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
            (CODEC_CONTAINER_WEBM, CODEC_CODEC_UNKNOWN)
        } else if bytes.len() >= 8 && &bytes[4..8] == b"ftyp" {
            (CODEC_CONTAINER_MP4, CODEC_CODEC_UNKNOWN)
        } else if &bytes[0..4] == b"DKIF" {
            if bytes.len() < 12 {
                return Err(Error::Truncated {
                    needed: 12,
                    had: bytes.len(),
                });
            }
            let codec = if &bytes[8..12] == b"AV01" {
                CODEC_CODEC_AV1
            } else if &bytes[8..12] == b"VP90" {
                CODEC_CODEC_VP9
            } else {
                return Err(Error::Unsupported("unknown ivf fourcc"));
            };
            (CODEC_CONTAINER_IVF, codec)
        } else {
            (CODEC_CONTAINER_UNKNOWN, CODEC_CODEC_UNKNOWN)
        };
        probe.container = container;
        probe.codec = codec;
        Ok(())
    })
}

fn new_decoder(codec: Codec, decoder: Decoder) -> *mut CodecDecoder {
    Box::into_raw(Box::new(CodecDecoder {
        decoder,
        codec,
        last_frame: None,
        view: CodecAudioFrame {
            sample_rate: 0,
            channels: 0,
            frames: 0,
            timestamp_us: 0,
            samples: ptr::null(),
        },
    }))
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `header` points to `len` readable bytes and `out` points to one writable `*mut CodecDecoder`.
pub unsafe extern "C" fn codec_decoder_from_wav_header(
    header: *const u8,
    len: usize,
    out: *mut *mut CodecDecoder,
) -> i32 {
    ffi_call(|| {
        if header.is_null() || out.is_null() {
            return Err(Error::InvalidArgument(
                "codec_decoder_from_wav_header: null pointer",
            ));
        }
        // SAFETY: null-checked above; the caller guarantees `header`
        // points to `len` readable bytes.
        let bytes = unsafe { slice::from_raw_parts(header, len) };
        let wav_header = parse_wav_header(bytes)?;
        let decoder = Decoder::from_wav_header(&wav_header)?;
        // SAFETY: `out` was null-checked above.
        unsafe {
            *out = new_decoder(Codec::Pcm, decoder);
        }
        Ok(())
    })
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `streaminfo` points to `len` readable bytes and `out` points to one writable `*mut CodecDecoder`.
pub unsafe extern "C" fn codec_decoder_from_flac_streaminfo(
    streaminfo: *const u8,
    len: usize,
    out: *mut *mut CodecDecoder,
) -> i32 {
    ffi_call(|| {
        if streaminfo.is_null() || out.is_null() {
            return Err(Error::InvalidArgument(
                "codec_decoder_from_flac_streaminfo: null pointer",
            ));
        }
        // SAFETY: null-checked above; the caller guarantees
        // `streaminfo` points to `len` readable bytes.
        let bytes = unsafe { slice::from_raw_parts(streaminfo, len) };
        let info = parse_streaminfo(bytes)?;
        let decoder = Decoder::from_flac_streaminfo(&info)?;
        // SAFETY: `out` was null-checked above.
        unsafe {
            *out = new_decoder(Codec::Flac, decoder);
        }
        Ok(())
    })
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `frame` points to `len` readable bytes and `out` points to one writable `*mut CodecDecoder`.
pub unsafe extern "C" fn codec_decoder_from_vp9_frame_header(
    frame: *const u8,
    len: usize,
    out: *mut *mut CodecDecoder,
) -> i32 {
    ffi_call(|| {
        if frame.is_null() || out.is_null() {
            return Err(Error::InvalidArgument(
                "codec_decoder_from_vp9_frame_header: null pointer",
            ));
        }
        // SAFETY: null-checked above; the caller guarantees `frame`
        // points to `len` readable bytes.
        let bytes = unsafe { slice::from_raw_parts(frame, len) };
        let mut state = Vp9ReferenceState::default();
        let (header, _) = parse_vp9_frame_header(bytes, &mut state)?;
        let decoder = Decoder::from_vp9_frame_header(&header)?;
        // SAFETY: `out` was null-checked above.
        unsafe {
            *out = new_decoder(Codec::Vp9, decoder);
        }
        Ok(())
    })
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `decoder` is a handle returned by a `codec_decoder_from_*` call and not yet freed, `packet` points to `len` readable bytes, and `out_frame` points to one writable `*const CodecAudioFrame`.
pub unsafe extern "C" fn codec_decoder_decode(
    decoder: *mut CodecDecoder,
    packet: *const u8,
    len: usize,
    timestamp_us: u64,
    out_frame: *mut *const CodecAudioFrame,
) -> i32 {
    ffi_call(|| {
        if decoder.is_null() || packet.is_null() || out_frame.is_null() {
            return Err(Error::InvalidArgument("codec_decoder_decode: null pointer"));
        }
        // SAFETY: null-checked above; the caller guarantees `packet`
        // points to `len` readable bytes and `decoder` is a handle
        // returned by a `codec_decoder_from_*` call.
        let handle = unsafe { &mut *decoder };
        let bytes = unsafe { slice::from_raw_parts(packet, len) };
        let pkt = Packet {
            codec: handle.codec,
            data: bytes.to_vec(),
            timestamp: timestamp_us,
            is_keyframe: true,
        };
        let frame = handle.decoder.decode(&pkt)?;
        let audio = match frame {
            Frame::Audio(audio) => audio,
            Frame::Video(_) => {
                return Err(Error::Unsupported("video decode returns no audio frame"));
            }
        };
        let frames = audio.frame_count() as u64;
        let sample_rate = audio.sample_rate;
        let channels = audio.channels;
        handle.last_frame = Some(audio);
        let stored = handle.last_frame.as_ref().ok_or(Error::InvalidArgument(
            "codec_decoder_decode: frame storage missing",
        ))?;
        handle.view = CodecAudioFrame {
            sample_rate,
            channels: u16::try_from(channels).unwrap_or(0),
            frames,
            timestamp_us,
            samples: stored.samples.as_ptr(),
        };
        // SAFETY: `out_frame` was null-checked above.
        unsafe {
            *out_frame = &handle.view;
        }
        Ok(())
    })
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `decoder` is a handle returned by a `codec_decoder_from_*` call and is freed at most once; passing NULL is a no-op.
pub unsafe extern "C" fn codec_decoder_free(decoder: *mut CodecDecoder) {
    if decoder.is_null() {
        return;
    }
    // SAFETY: `decoder` is a handle returned by a
    // `codec_decoder_from_*` call and is freed at most once by the
    // caller; dropping the Box frees it.
    let _ = unsafe { Box::from_raw(decoder) };
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `out` points to one writable `*mut CodecEncoder`.
pub unsafe extern "C" fn codec_encoder_new_flac(
    sample_rate: u32,
    channels: u16,
    out: *mut *mut CodecEncoder,
) -> i32 {
    ffi_call(|| {
        if out.is_null() {
            return Err(Error::InvalidArgument(
                "codec_encoder_new_flac: null pointer",
            ));
        }
        let encoder = Encoder::new(EncoderConfig {
            codec: Codec::Flac,
            sample_rate,
            channels: u32::from(channels),
            ..EncoderConfig::default()
        })?;
        // SAFETY: `out` was null-checked above.
        unsafe {
            *out = Box::into_raw(Box::new(CodecEncoder {
                encoder,
                sample_rate,
                channels: u32::from(channels),
                frames_encoded: 0,
                last_packet: None,
                view: CodecPacket {
                    data: ptr::null(),
                    len: 0,
                    timestamp_us: 0,
                },
            }));
        }
        Ok(())
    })
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `encoder` is a handle returned by `codec_encoder_new_flac` and not yet freed, and `out_packet` points to one writable `*const CodecPacket`.
pub unsafe extern "C" fn codec_encoder_flac_streaminfo(
    encoder: *mut CodecEncoder,
    out_packet: *mut *const CodecPacket,
) -> i32 {
    ffi_call(|| {
        if encoder.is_null() || out_packet.is_null() {
            return Err(Error::InvalidArgument(
                "codec_encoder_flac_streaminfo: null pointer",
            ));
        }
        // SAFETY: null-checked above; `encoder` is a handle from
        // `codec_encoder_new_flac`.
        let handle = unsafe { &mut *encoder };
        let bytes = handle.encoder.flac_streaminfo_bytes();
        let packet = Packet {
            codec: Codec::Flac,
            data: bytes,
            timestamp: 0,
            is_keyframe: true,
        };
        let len = packet.data.len();
        handle.last_packet = Some(packet);
        let stored = handle.last_packet.as_ref().ok_or(Error::InvalidArgument(
            "codec_encoder_flac_streaminfo: packet storage missing",
        ))?;
        handle.view = CodecPacket {
            data: stored.data.as_ptr(),
            len,
            timestamp_us: 0,
        };
        // SAFETY: `out_packet` was null-checked above.
        unsafe {
            *out_packet = &handle.view;
        }
        Ok(())
    })
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `encoder` is a handle returned by `codec_encoder_new_flac` and not yet freed, `samples` points to `frames * channels` readable f32 values, and `out_packet` points to one writable `*const CodecPacket`.
pub unsafe extern "C" fn codec_encoder_encode(
    encoder: *mut CodecEncoder,
    samples: *const f32,
    frames: u64,
    out_packet: *mut *const CodecPacket,
) -> i32 {
    ffi_call(|| {
        if encoder.is_null() || samples.is_null() || out_packet.is_null() {
            return Err(Error::InvalidArgument("codec_encoder_encode: null pointer"));
        }
        // SAFETY: null-checked above; `encoder` is a handle from
        // `codec_encoder_new_flac`, and the caller guarantees
        // `samples` points to `frames * channels` readable f32
        // values, with `channels` from the encoder's config.
        let handle = unsafe { &mut *encoder };
        let count = frames
            .checked_mul(u64::from(handle.channels))
            .ok_or(Error::Overflow(
                "codec_encoder_encode: sample count overflows",
            ))?;
        let count = usize::try_from(count)
            .map_err(|_| Error::Overflow("codec_encoder_encode: sample count overflows"))?;
        let values = unsafe { slice::from_raw_parts(samples, count) };
        let timestamp = handle
            .frames_encoded
            .checked_mul(1_000_000)
            .and_then(|us| us.checked_div(u64::from(handle.sample_rate)))
            .ok_or(Error::Overflow("codec_encoder_encode: timestamp overflows"))?;
        let frame = Frame::Audio(AudioFrame {
            sample_rate: handle.sample_rate,
            channels: handle.channels,
            samples: values.to_vec(),
            timestamp,
        });
        let packet = handle.encoder.encode(&frame)?;
        let len = packet.data.len();
        handle.frames_encoded = handle.frames_encoded.saturating_add(frames);
        handle.last_packet = Some(packet);
        let stored = handle.last_packet.as_ref().ok_or(Error::InvalidArgument(
            "codec_encoder_encode: packet storage missing",
        ))?;
        handle.view = CodecPacket {
            data: stored.data.as_ptr(),
            len,
            timestamp_us: timestamp,
        };
        // SAFETY: `out_packet` was null-checked above.
        unsafe {
            *out_packet = &handle.view;
        }
        Ok(())
    })
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `encoder` is a handle returned by `codec_encoder_new_flac` and is freed at most once; passing NULL is a no-op.
pub unsafe extern "C" fn codec_encoder_free(encoder: *mut CodecEncoder) {
    if encoder.is_null() {
        return;
    }
    // SAFETY: `encoder` is a handle returned by
    // `codec_encoder_new_flac` and is freed at most once by the
    // caller.
    let _ = unsafe { Box::from_raw(encoder) };
}

/// Allocates `len` bytes plus an 8-byte length header, and returns
/// a pointer past the header. `codec_free` reads the header back
/// to know how much to deallocate. The layout is size `len + 8`,
/// alignment 1.
fn alloc_with_length_header(len: usize) -> Option<*mut u8> {
    use std::alloc::{alloc, Layout};
    let total = len.checked_add(8)?;
    let layout = Layout::from_size_align(total, 1).ok()?;
    // SAFETY: the layout is valid (size > 0, align 1); `alloc`
    // returns null on failure.
    let base = unsafe { alloc(layout) };
    if base.is_null() {
        return None;
    }
    // SAFETY: the allocation is `len + 8` bytes, so the first 8
    // bytes are writable; the length is stored little-endian as a
    // fixed-width u64, not a usize: on a 32-bit target usize is
    // only 4 bytes, and `len.to_le_bytes()` would be too short to
    // copy 8 bytes from (found by the wasm32 build).
    let len_u64 = len as u64;
    unsafe {
        ptr::copy_nonoverlapping(len_u64.to_le_bytes().as_ptr(), base, 8);
    }
    // SAFETY: `base` is 8 bytes into a live allocation.
    Some(unsafe { base.add(8) })
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `samples` points to `frames * channels` readable i16 values, and `out_data` and `out_len` each point to one writable value.
pub unsafe extern "C" fn codec_encode_flac(
    samples: *const i16,
    frames: u64,
    channels: u16,
    sample_rate: u32,
    out_data: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    ffi_call(|| {
        if samples.is_null() || out_data.is_null() || out_len.is_null() {
            return Err(Error::InvalidArgument("codec_encode_flac: null pointer"));
        }
        // SAFETY: null-checked above; the caller guarantees
        // `samples` points to `frames * channels` readable i16
        // values.
        let count = frames
            .checked_mul(u64::from(channels))
            .ok_or(Error::Overflow("codec_encode_flac: sample count overflows"))?;
        let count = usize::try_from(count)
            .map_err(|_| Error::Overflow("codec_encode_flac: sample count overflows"))?;
        let values = unsafe { slice::from_raw_parts(samples, count) };
        let encoded = codec_whatttttttttttt_core::encode_flac_stream(
            values,
            u32::from(channels),
            sample_rate,
        )?;
        let len = encoded.len();
        let Some(ptr) = alloc_with_length_header(len) else {
            return Err(Error::AllocationLimit {
                requested: len + 8,
                limit: usize::MAX,
            });
        };
        // SAFETY: `ptr` points to `len` writable bytes from the
        // allocator, and `encoded` holds exactly `len` bytes.
        unsafe {
            ptr::copy_nonoverlapping(encoded.as_ptr(), ptr, len);
            *out_data = ptr;
            *out_len = len;
        }
        Ok(())
    })
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `ptr` is NULL or a pointer returned by `codec_encode_flac` and is freed at most once.
pub unsafe extern "C" fn codec_free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    use std::alloc::{dealloc, Layout};
    // SAFETY: `ptr` was returned by `codec_encode_flac`, so the
    // 8 bytes before it hold the allocation length written by
    // `alloc_with_length_header`.
    let base = unsafe { (ptr as *mut u8).sub(8) };
    let mut len_bytes = [0u8; 8];
    unsafe {
        ptr::copy_nonoverlapping(base, len_bytes.as_mut_ptr(), 8);
    }
    // The header is a fixed-width u64 (see
    // `alloc_with_length_header`): `usize::from_le_bytes` would
    // expect 4 bytes on a 32-bit target. The stored length came
    // from a real allocation on this platform, so it fits in
    // usize.
    let len = u64::from_le_bytes(len_bytes) as usize;
    let Some(total) = len.checked_add(8) else {
        return;
    };
    let Ok(layout) = Layout::from_size_align(total, 1) else {
        return;
    };
    // SAFETY: `base` is the start of the allocation made by
    // `alloc_with_length_header` with this layout.
    unsafe {
        dealloc(base, layout);
    }
}

fn vp9_header_view(header: &Vp9FrameHeader) -> CodecVp9FrameHeader {
    CodecVp9FrameHeader {
        profile: u8::try_from(header.profile).unwrap_or(0),
        frame_type: u8::from(!header.is_keyframe),
        show_frame: u8::from(header.show_frame),
        show_existing_frame: u8::from(header.show_existing_frame),
        width: u16::try_from(header.frame_width).unwrap_or(0),
        height: u16::try_from(header.frame_height).unwrap_or(0),
        render_width: u16::try_from(header.render_width).unwrap_or(0),
        render_height: u16::try_from(header.render_height).unwrap_or(0),
        base_q_idx: u8::try_from(header.base_q_idx).unwrap_or(0),
        loop_filter_level: u8::try_from(header.loop_filter_level).unwrap_or(0),
        tile_cols_log2: u8::try_from(header.tile_cols_log2).unwrap_or(0),
        tile_rows_log2: u8::try_from(header.tile_rows_log2).unwrap_or(0),
        header_size_in_bytes: header.header_size_in_bytes,
    }
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `frame` points to `len` readable bytes and `out` points to one writable `CodecVp9FrameHeader`.
pub unsafe extern "C" fn codec_vp9_parse_frame_header(
    frame: *const u8,
    len: usize,
    out: *mut CodecVp9FrameHeader,
) -> i32 {
    ffi_call(|| {
        if frame.is_null() || out.is_null() {
            return Err(Error::InvalidArgument(
                "codec_vp9_parse_frame_header: null pointer",
            ));
        }
        // SAFETY: null-checked above; the caller guarantees `frame`
        // points to `len` readable bytes and `out` to one writable
        // `CodecVp9FrameHeader`.
        let bytes = unsafe { slice::from_raw_parts(frame, len) };
        let mut state = Vp9ReferenceState::default();
        let (header, _) = parse_vp9_frame_header(bytes, &mut state)?;
        let view = vp9_header_view(&header);
        // SAFETY: `out` was null-checked above.
        unsafe {
            *out = view;
        }
        Ok(())
    })
}

#[no_mangle]
/// # Safety
///
/// The caller must ensure `obu` points to `len` readable bytes and `out` points to one writable `CodecAv1SequenceHeader`.
pub unsafe extern "C" fn codec_av1_parse_sequence_header(
    obu: *const u8,
    len: usize,
    out: *mut CodecAv1SequenceHeader,
) -> i32 {
    ffi_call(|| {
        if obu.is_null() || out.is_null() {
            return Err(Error::InvalidArgument(
                "codec_av1_parse_sequence_header: null pointer",
            ));
        }
        // SAFETY: null-checked above; the caller guarantees `obu`
        // points to `len` readable bytes and `out` to one writable
        // `CodecAv1SequenceHeader`.
        let bytes = unsafe { slice::from_raw_parts(obu, len) };
        let (parsed, _) = parse_obu(bytes, 0)?;
        if parsed.header.obu_type != ObuType::SequenceHeader {
            return Err(Error::InvalidArgument(
                "codec_av1_parse_sequence_header: not a sequence header obu",
            ));
        }
        let sequence = parse_sequence_header(parsed.payload)?;
        let first = sequence
            .operating_points
            .first()
            .ok_or(Error::InvalidData {
                offset: 0,
                detail: "av1 sequence header has no operating point",
            })?;
        let view = CodecAv1SequenceHeader {
            seq_profile: sequence.profile,
            seq_level_idx_0: first.level,
            seq_tier_0: first.tier,
            max_frame_width_minus_1: u16::try_from(sequence.max_frame_width_minus_1)
                .unwrap_or(u16::MAX),
            max_frame_height_minus_1: u16::try_from(sequence.max_frame_height_minus_1)
                .unwrap_or(u16::MAX),
            frame_width_bits_minus_1: sequence.frame_width_bits_minus_1,
            frame_height_bits_minus_1: sequence.frame_height_bits_minus_1,
            reduced_still_picture_header: u8::from(sequence.reduced_still_picture_header),
        };
        // SAFETY: `out` was null-checked above.
        unsafe {
            *out = view;
        }
        Ok(())
    })
}
