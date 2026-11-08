//! WebAssembly exports for `codec-whatttttttttttt` (Phase 10).
//!
//! Thin wrapper over the C ABI (`codec-whatttttttttttt-c`): the
//! functions here are the same operations, adapted to the linear
//! memory of a WebAssembly module. JavaScript allocates input
//! buffers with `wasm_alloc`, calls a function, reads the result
//! out of linear memory, and frees with `wasm_free`. No decode or
//! encode logic lives here.
//!
//! Output layouts (all integers little-endian):
//!
//! * `wasm_probe` writes two `i32` values: container, codec.
//! * `wasm_decode_wav` / `wasm_decode_flac` write three `u32`
//!   values to `meta_out`: sample rate, channels, frames. The
//!   decoded f32 samples are written to `samples_out`, which must
//!   hold at least `frames * channels` values.
//! * `wasm_vp9_parse_header` writes 20 bytes: profile, frame
//!   type, show frame, show-existing frame (4 bytes), width,
//!   height, render width, render height (`u16` at offsets 4, 6,
//!   8, 10), base Q index, loop-filter level, tile columns log2,
//!   tile rows log2 (4 bytes at offsets 12..16), header size in
//!   bytes (`u32` at offset 16).
//! * `wasm_av1_parse_sequence_header` writes 12 bytes: profile,
//!   level, tier (3 bytes), width minus 1, height minus 1 (`u16`
//!   at offsets 4 and 6), frame-width bits minus 1,
//!   frame-height bits minus 1, reduced still picture header
//!   (3 bytes at offsets 8, 9, 10).
//!
//! Every function returns a C ABI error code (`0` is success);
//! the detail of the last failure is available through
//! `wasm_last_error`. Buffers returned by `wasm_encode_flac` are
//! allocated with `wasm_alloc` and freed with `wasm_free`.

use codec_whatttttttttttt::{
    codec_av1_parse_sequence_header, codec_decoder_decode, codec_decoder_free,
    codec_decoder_from_flac_streaminfo, codec_decoder_from_wav_header, codec_encode_flac,
    codec_encoder_encode, codec_encoder_flac_streaminfo, codec_encoder_free,
    codec_encoder_new_flac, codec_error_string, codec_free, codec_last_error, codec_probe,
    codec_version, CodecAudioFrame, CodecAv1SequenceHeader, CodecDecoder, CodecEncoder,
    CodecPacket, CodecProbe, CodecVp9FrameHeader, CODEC_ERR_ALLOCATION_LIMIT,
    CODEC_ERR_INVALID_ARGUMENT, CODEC_ERR_OVERFLOW, CODEC_OK,
};
use codec_whatttttttttttt_core::parse_wav_header;
use std::alloc::{alloc, dealloc, Layout};
use std::ffi::CStr;
use std::ptr;
use std::slice;

fn layout(size: usize) -> Option<Layout> {
    Layout::from_size_align(size, 1).ok()
}

/// Allocate `size` bytes in linear memory. Returns null when the
/// allocation fails.
#[no_mangle]
pub extern "C" fn wasm_alloc(size: usize) -> *mut u8 {
    let Some(layout) = layout(size) else {
        return ptr::null_mut();
    };
    // SAFETY: the layout is valid (size any, align 1); `alloc`
    // returns null on failure.
    unsafe { alloc(layout) }
}

/// Free a buffer returned by `wasm_alloc` or `wasm_encode_flac`.
/// `size` must be the size passed to `wasm_alloc` (or the length
/// returned through `out_len` by `wasm_encode_flac`).
///
/// # Safety
///
/// The caller must pass a pointer returned by `wasm_alloc` or
/// `wasm_encode_flac` with its original size, freed at most once.
#[no_mangle]
pub unsafe extern "C" fn wasm_free(ptr: *mut u8, size: usize) {
    if ptr.is_null() {
        return;
    }
    if let Some(layout) = layout(size) {
        // SAFETY: `ptr` was returned by `wasm_alloc` (or by
        // `wasm_encode_flac`, which allocates the same way) with
        // this size, and is freed at most once by the caller.
        unsafe {
            dealloc(ptr, layout);
        }
    }
}

/// Pointer to the static version string, e.g. "0.1.0".
#[no_mangle]
pub extern "C" fn wasm_version() -> *const u8 {
    codec_version().cast()
}

/// Length of the version string in bytes, without the NUL.
#[no_mangle]
pub extern "C" fn wasm_version_len() -> usize {
    // SAFETY: `codec_version` returns a valid NUL-terminated
    // static string.
    unsafe { CStr::from_ptr(codec_version()).to_bytes().len() }
}

/// Pointer to the static message for an error code.
#[no_mangle]
pub extern "C" fn wasm_error_string(code: i32) -> *const u8 {
    codec_error_string(code).cast()
}

/// Length of the error message in bytes, without the NUL.
#[no_mangle]
pub extern "C" fn wasm_error_string_len(code: i32) -> usize {
    // SAFETY: `codec_error_string` returns a valid
    // NUL-terminated static string for any code.
    unsafe { CStr::from_ptr(codec_error_string(code)).to_bytes().len() }
}

/// Copy the detail of the last failure into `out` (at most
/// `capacity` bytes, no NUL terminator) and return the number of
/// bytes written. Returns 0 when the last call succeeded.
///
/// # Safety
///
/// The caller must ensure `out` points to `capacity` writable
/// bytes.
#[no_mangle]
pub unsafe extern "C" fn wasm_last_error(out: *mut u8, capacity: usize) -> usize {
    let detail = codec_last_error();
    if detail.is_null() || out.is_null() {
        return 0;
    }
    // SAFETY: `codec_last_error` returns a valid NUL-terminated
    // string or null (checked above); `out` points to `capacity`
    // writable bytes by the caller.
    let bytes = unsafe { CStr::from_ptr(detail).to_bytes() };
    let count = bytes.len().min(capacity);
    // SAFETY: `bytes` holds at least `count` bytes and `out`
    // points to `capacity` writable bytes.
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), out, count);
    }
    count
}

/// Identify the container and codec of the first bytes of a file.
/// Writes container and codec (`i32` each) to `out`.
///
/// # Safety
///
/// The caller must ensure `data` points to `len` readable bytes
/// and `out` points to two writable `i32` values.
#[no_mangle]
pub unsafe extern "C" fn wasm_probe(data: *const u8, len: usize, out: *mut i32) -> i32 {
    if data.is_null() || out.is_null() {
        return CODEC_ERR_INVALID_ARGUMENT;
    }
    // SAFETY: null-checked above; the caller guarantees `data`
    // points to `len` readable bytes and `out` to two writable
    // `i32` values.
    let bytes = unsafe { slice::from_raw_parts(data, len) };
    let mut probe = CodecProbe {
        container: 0,
        codec: 0,
    };
    // SAFETY: `probe` is a valid `CodecProbe` to write into.
    let rc = unsafe { codec_probe(bytes.as_ptr(), bytes.len(), &mut probe) };
    if rc != CODEC_OK {
        return rc;
    }
    // SAFETY: `out` points to two writable `i32` values.
    unsafe {
        ptr::write(out, probe.container);
        ptr::write(out.add(1), probe.codec);
    }
    CODEC_OK
}

fn write_frame_meta(
    frame: &CodecAudioFrame,
    samples_out: *mut f32,
    capacity: usize,
    meta_out: *mut u32,
) -> i32 {
    let count = frame.frames as usize * frame.channels as usize;
    if count > capacity {
        return CODEC_ERR_INVALID_ARGUMENT;
    }
    // SAFETY: `samples_out` points to `capacity` writable f32
    // values and `count <= capacity`; `frame.samples` points to
    // `count` valid f32 values borrowed from the decoder.
    unsafe {
        ptr::copy_nonoverlapping(frame.samples, samples_out, count);
        ptr::write(meta_out, frame.sample_rate);
        ptr::write(meta_out.add(1), u32::from(frame.channels));
        ptr::write(
            meta_out.add(2),
            u32::try_from(frame.frames).unwrap_or(u32::MAX),
        );
    }
    CODEC_OK
}

/// Decode a WAV file: `data` is the whole file. The decoded f32
/// samples are written to `samples_out` (capacity in f32 values);
/// `meta_out` receives sample rate, channels, frames.
///
/// # Safety
///
/// The caller must ensure `data` points to `len` readable bytes,
/// `samples_out` points to `capacity` writable f32 values, and
/// `meta_out` points to three writable `u32` values.
#[no_mangle]
pub unsafe extern "C" fn wasm_decode_wav(
    data: *const u8,
    len: usize,
    samples_out: *mut f32,
    capacity: usize,
    meta_out: *mut u32,
) -> i32 {
    if data.is_null() || samples_out.is_null() || meta_out.is_null() {
        return CODEC_ERR_INVALID_ARGUMENT;
    }
    // SAFETY: null-checked above; the caller guarantees `data`
    // points to `len` readable bytes.
    let bytes = unsafe { slice::from_raw_parts(data, len) };
    let header = match parse_wav_header(bytes) {
        Ok(header) => header,
        Err(err) => {
            return match err {
                codec_whatttttttttttt_core::Error::InvalidData { .. } => CODEC_ERR_INVALID_ARGUMENT,
                codec_whatttttttttttt_core::Error::Truncated { .. } => {
                    codec_whatttttttttttt::CODEC_ERR_TRUNCATED
                }
                codec_whatttttttttttt_core::Error::Overflow(_) => CODEC_ERR_OVERFLOW,
                codec_whatttttttttttt_core::Error::AllocationLimit { .. } => {
                    CODEC_ERR_ALLOCATION_LIMIT
                }
                _ => CODEC_ERR_INVALID_ARGUMENT,
            };
        }
    };
    let Some(pcm) = bytes.get(header.data_offset..header.data_offset + header.data_size as usize)
    else {
        return CODEC_ERR_INVALID_ARGUMENT;
    };
    let mut handle: *mut CodecDecoder = ptr::null_mut();
    // SAFETY: `handle` is a valid out-pointer for the decoder.
    let rc = unsafe { codec_decoder_from_wav_header(bytes.as_ptr(), bytes.len(), &mut handle) };
    if rc != CODEC_OK {
        return rc;
    }
    let mut view: *const CodecAudioFrame = ptr::null();
    // SAFETY: `handle` was just created; `pcm` is the file's data
    // chunk; `view` is a valid out-pointer.
    let rc = unsafe { codec_decoder_decode(handle, pcm.as_ptr(), pcm.len(), 0, &mut view) };
    if rc != CODEC_OK {
        // SAFETY: `handle` is a live decoder handle.
        unsafe {
            codec_decoder_free(handle);
        }
        return rc;
    }
    // SAFETY: `view` is set by the successful decode call.
    let frame = unsafe { &*view };
    let rc = write_frame_meta(frame, samples_out, capacity, meta_out);
    // SAFETY: `handle` is a live decoder handle.
    unsafe {
        codec_decoder_free(handle);
    }
    rc
}

/// Decode one FLAC frame: `streaminfo` is the 34-byte STREAMINFO
/// payload, `packet` is one frame. The decoded f32 samples are
/// written to `samples_out` (capacity in f32 values); `meta_out`
/// receives sample rate, channels, frames.
///
/// # Safety
///
/// The caller must ensure `streaminfo` points to 34 readable
/// bytes, `packet` points to `packet_len` readable bytes,
/// `samples_out` points to `capacity` writable f32 values, and
/// `meta_out` points to three writable `u32` values.
#[no_mangle]
pub unsafe extern "C" fn wasm_decode_flac(
    streaminfo: *const u8,
    packet: *const u8,
    packet_len: usize,
    samples_out: *mut f32,
    capacity: usize,
    meta_out: *mut u32,
) -> i32 {
    if streaminfo.is_null() || packet.is_null() || samples_out.is_null() || meta_out.is_null() {
        return CODEC_ERR_INVALID_ARGUMENT;
    }
    let mut handle: *mut CodecDecoder = ptr::null_mut();
    // SAFETY: `handle` is a valid out-pointer; `streaminfo`
    // points to 34 readable bytes by the caller.
    let rc = unsafe { codec_decoder_from_flac_streaminfo(streaminfo, 34, &mut handle) };
    if rc != CODEC_OK {
        return rc;
    }
    let mut view: *const CodecAudioFrame = ptr::null();
    // SAFETY: `handle` was just created; `packet` points to
    // `packet_len` readable bytes by the caller.
    let rc = unsafe { codec_decoder_decode(handle, packet, packet_len, 0, &mut view) };
    if rc != CODEC_OK {
        // SAFETY: `handle` is a live decoder handle.
        unsafe {
            codec_decoder_free(handle);
        }
        return rc;
    }
    // SAFETY: `view` is set by the successful decode call.
    let frame = unsafe { &*view };
    let rc = write_frame_meta(frame, samples_out, capacity, meta_out);
    // SAFETY: `handle` is a live decoder handle.
    unsafe {
        codec_decoder_free(handle);
    }
    rc
}

/// One-shot FLAC encode: interleaved i16 samples -> a complete
/// FLAC file. On success returns `CODEC_OK` and writes the
/// `wasm_alloc` buffer's address to `out_ptr` and its length to
/// `out_len`; free the buffer with `wasm_free`. On failure returns
/// the error code and writes nothing.
///
/// # Safety
///
/// The caller must ensure `samples` points to
/// `frames * channels` readable i16 values and `out_ptr` and
/// `out_len` each point to one writable `usize`.
#[no_mangle]
pub unsafe extern "C" fn wasm_encode_flac(
    samples: *const i16,
    frames: u64,
    channels: u16,
    sample_rate: u32,
    out_ptr: *mut usize,
    out_len: *mut usize,
) -> i32 {
    if samples.is_null() || out_ptr.is_null() || out_len.is_null() {
        return CODEC_ERR_INVALID_ARGUMENT;
    }
    let mut data: *mut u8 = ptr::null_mut();
    let mut len: usize = 0;
    // SAFETY: null-checked above; the caller guarantees `samples`
    // points to `frames * channels` readable i16 values.
    let rc =
        unsafe { codec_encode_flac(samples, frames, channels, sample_rate, &mut data, &mut len) };
    if rc != CODEC_OK {
        return rc;
    }
    // Copy into a `wasm_alloc` buffer: the C buffer is owned by
    // the C allocator and must be freed with `codec_free`, which
    // JavaScript cannot call directly.
    let out = wasm_alloc(len);
    if out.is_null() {
        // SAFETY: `data` was returned by `codec_encode_flac`.
        unsafe {
            codec_free(data.cast());
        }
        return CODEC_ERR_ALLOCATION_LIMIT;
    }
    // SAFETY: `data` holds `len` bytes from `codec_encode_flac`;
    // `out` holds `len` writable bytes from `wasm_alloc`.
    unsafe {
        ptr::copy_nonoverlapping(data, out, len);
        codec_free(data.cast());
        ptr::write(out_ptr, out as usize);
        ptr::write(out_len, len);
    }
    CODEC_OK
}

/// Streaming FLAC encoders, kept in a table indexed by handle.
/// Handles are small integers starting at 1; 0 is never a valid
/// handle. The table stores addresses as `usize`, not raw
/// pointers: a `*mut CodecEncoder` is not `Send`, and a static
/// table must be `Sync`. The table is behind a `Mutex` because
/// statics must be `Sync`; the module is single-threaded, so the
/// lock never contends. A freed slot is reused by the next
/// encoder.
static ENCODERS: std::sync::Mutex<Vec<Option<usize>>> = std::sync::Mutex::new(Vec::new());

fn take_encoder(handle: u32) -> Option<*mut CodecEncoder> {
    // Handles are stored indices + 1 (handle 0 is invalid).
    let index = (handle as usize).checked_sub(1)?;
    let table = ENCODERS.lock().ok()?;
    let address = table.get(index).copied().flatten()?;
    // SAFETY: the address came from a live encoder handle stored
    // by `wasm_encoder_new_flac` and not yet freed (freed slots
    // hold `None`).
    Some(address as *mut CodecEncoder)
}

fn copy_packet(packet: &CodecPacket) -> Option<(*mut u8, usize)> {
    // SAFETY: `packet.data` points to `packet.len` valid bytes
    // borrowed from the encoder; the copy is made immediately.
    let bytes = unsafe { slice::from_raw_parts(packet.data, packet.len) };
    let out = wasm_alloc(packet.len);
    if out.is_null() {
        return None;
    }
    // SAFETY: `out` holds `packet.len` writable bytes from
    // `wasm_alloc`.
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), out, packet.len);
    }
    Some((out, packet.len))
}

/// Create a streaming FLAC encoder. On success returns `CODEC_OK`
/// and writes the handle to `out_handle`; free it with
/// `wasm_encoder_free`.
///
/// # Safety
///
/// The caller must ensure `out_handle` points to one writable
/// `u32`.
#[no_mangle]
pub unsafe extern "C" fn wasm_encoder_new_flac(
    sample_rate: u32,
    channels: u16,
    out_handle: *mut u32,
) -> i32 {
    if out_handle.is_null() {
        return CODEC_ERR_INVALID_ARGUMENT;
    }
    let mut handle: *mut CodecEncoder = ptr::null_mut();
    // SAFETY: `handle` is a valid out-pointer for the encoder.
    let rc = unsafe { codec_encoder_new_flac(sample_rate, channels, &mut handle) };
    if rc != CODEC_OK {
        return rc;
    }
    let mut table = match ENCODERS.lock() {
        Ok(table) => table,
        Err(_) => {
            // SAFETY: `handle` was just created.
            unsafe {
                codec_encoder_free(handle);
            }
            return CODEC_ERR_INVALID_ARGUMENT;
        }
    };
    // Reuse a freed slot when one exists, so handles stay small.
    let index = match table.iter().position(|slot| slot.is_none()) {
        Some(index) => {
            table[index] = Some(handle as usize);
            index
        }
        None => {
            table.push(Some(handle as usize));
            table.len() - 1
        }
    };
    // SAFETY: `out_handle` points to one writable `u32` by the
    // caller; index 0 is never handed out (handle 0 is invalid),
    // so the stored handle is index + 1.
    unsafe {
        ptr::write(out_handle, u32::try_from(index + 1).unwrap_or(0));
    }
    CODEC_OK
}

/// The streaming STREAMINFO header (42 bytes) for encoder
/// `handle`. On success returns `CODEC_OK` and writes the
/// `wasm_alloc` buffer's address to `out_ptr` and its length to
/// `out_len`; free the buffer with `wasm_free`.
///
/// # Safety
///
/// The caller must ensure `out_ptr` and `out_len` each point to
/// one writable `usize`.
#[no_mangle]
pub unsafe extern "C" fn wasm_encoder_flac_streaminfo(
    handle: u32,
    out_ptr: *mut usize,
    out_len: *mut usize,
) -> i32 {
    if out_ptr.is_null() || out_len.is_null() {
        return CODEC_ERR_INVALID_ARGUMENT;
    }
    let Some(encoder) = take_encoder(handle) else {
        return CODEC_ERR_INVALID_ARGUMENT;
    };
    let mut view: *const CodecPacket = ptr::null();
    // SAFETY: `encoder` is a live handle from
    // `wasm_encoder_new_flac`; `view` is a valid out-pointer.
    let rc = unsafe { codec_encoder_flac_streaminfo(encoder, &mut view) };
    if rc != CODEC_OK {
        return rc;
    }
    // SAFETY: `view` is set by the successful call.
    let packet = unsafe { &*view };
    let Some((out, len)) = copy_packet(packet) else {
        return CODEC_ERR_ALLOCATION_LIMIT;
    };
    // SAFETY: `out_ptr` and `out_len` point to writable `usize`
    // values by the caller.
    unsafe {
        ptr::write(out_ptr, out as usize);
        ptr::write(out_len, len);
    }
    CODEC_OK
}

/// Encode one frame of interleaved f32 samples with encoder
/// `handle`. `frames` is the number of frames (samples per
/// channel), 1..=4096. On success returns `CODEC_OK` and writes
/// the `wasm_alloc` buffer holding the FLAC frame to `out_ptr`
/// and its length to `out_len`; free the buffer with `wasm_free`.
///
/// # Safety
///
/// The caller must ensure `samples` points to
/// `frames * channels` readable f32 values and `out_ptr` and
/// `out_len` each point to one writable `usize`.
#[no_mangle]
pub unsafe extern "C" fn wasm_encoder_encode(
    handle: u32,
    samples: *const f32,
    frames: u64,
    out_ptr: *mut usize,
    out_len: *mut usize,
) -> i32 {
    if samples.is_null() || out_ptr.is_null() || out_len.is_null() {
        return CODEC_ERR_INVALID_ARGUMENT;
    }
    let Some(encoder) = take_encoder(handle) else {
        return CODEC_ERR_INVALID_ARGUMENT;
    };
    let mut view: *const CodecPacket = ptr::null();
    // SAFETY: `encoder` is a live handle; `samples` points to
    // `frames * channels` readable f32 values by the caller.
    let rc = unsafe { codec_encoder_encode(encoder, samples, frames, &mut view) };
    if rc != CODEC_OK {
        return rc;
    }
    // SAFETY: `view` is set by the successful call.
    let packet = unsafe { &*view };
    let Some((out, len)) = copy_packet(packet) else {
        return CODEC_ERR_ALLOCATION_LIMIT;
    };
    // SAFETY: `out_ptr` and `out_len` point to writable `usize`
    // values by the caller.
    unsafe {
        ptr::write(out_ptr, out as usize);
        ptr::write(out_len, len);
    }
    CODEC_OK
}

/// Free a streaming FLAC encoder. The handle must not be used
/// afterwards; freeing an invalid handle is a no-op that returns
/// `CODEC_ERR_INVALID_ARGUMENT`.
#[no_mangle]
pub extern "C" fn wasm_encoder_free(handle: u32) -> i32 {
    let mut table = match ENCODERS.lock() {
        Ok(table) => table,
        Err(_) => return CODEC_ERR_INVALID_ARGUMENT,
    };
    // Handles are stored indices + 1 (handle 0 is invalid).
    let Some(index) = (handle as usize).checked_sub(1) else {
        return CODEC_ERR_INVALID_ARGUMENT;
    };
    let Some(slot) = table.get_mut(index) else {
        return CODEC_ERR_INVALID_ARGUMENT;
    };
    let Some(address) = slot.take() else {
        return CODEC_ERR_INVALID_ARGUMENT;
    };
    // SAFETY: `address` is a live handle from
    // `wasm_encoder_new_flac`, freed at most once: the slot is
    // cleared above, so a second free finds `None`.
    unsafe {
        codec_encoder_free(address as *mut CodecEncoder);
    }
    CODEC_OK
}

fn write_u16(out: *mut u8, offset: usize, value: u16) {
    // SAFETY: the caller guarantees `out` points to at least
    // `offset + 2` writable bytes; the value is written
    // little-endian.
    unsafe {
        ptr::write(out.add(offset), (value & 0xff) as u8);
        ptr::write(out.add(offset + 1), (value >> 8) as u8);
    }
}

fn write_u32(out: *mut u8, offset: usize, value: u32) {
    // SAFETY: the caller guarantees `out` points to at least
    // `offset + 4` writable bytes; the value is written
    // little-endian.
    unsafe {
        ptr::write(out.add(offset), (value & 0xff) as u8);
        ptr::write(out.add(offset + 1), ((value >> 8) & 0xff) as u8);
        ptr::write(out.add(offset + 2), ((value >> 16) & 0xff) as u8);
        ptr::write(out.add(offset + 3), ((value >> 24) & 0xff) as u8);
    }
}

/// Parse a VP9 frame header (parser-only; no reconstruction).
/// Writes the 20-byte layout from the module docs to `out`.
///
/// # Safety
///
/// The caller must ensure `frame` points to `len` readable bytes
/// and `out` points to 20 writable bytes.
#[no_mangle]
pub unsafe extern "C" fn wasm_vp9_parse_header(frame: *const u8, len: usize, out: *mut u8) -> i32 {
    if frame.is_null() || out.is_null() {
        return CODEC_ERR_INVALID_ARGUMENT;
    }
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
    // SAFETY: null-checked above; `frame` points to `len`
    // readable bytes and `header` is a valid out-struct.
    let rc =
        unsafe { codec_whatttttttttttt::codec_vp9_parse_frame_header(frame, len, &mut header) };
    if rc != CODEC_OK {
        return rc;
    }
    // SAFETY: `out` points to 20 writable bytes by the caller.
    unsafe {
        ptr::write(out, header.profile);
        ptr::write(out.add(1), header.frame_type);
        ptr::write(out.add(2), header.show_frame);
        ptr::write(out.add(3), header.show_existing_frame);
    }
    write_u16(out, 4, header.width);
    write_u16(out, 6, header.height);
    write_u16(out, 8, header.render_width);
    write_u16(out, 10, header.render_height);
    // SAFETY: `out` points to 20 writable bytes by the caller.
    unsafe {
        ptr::write(out.add(12), header.base_q_idx);
        ptr::write(out.add(13), header.loop_filter_level);
        ptr::write(out.add(14), header.tile_cols_log2);
        ptr::write(out.add(15), header.tile_rows_log2);
    }
    write_u32(out, 16, header.header_size_in_bytes);
    CODEC_OK
}

/// Parse one AV1 sequence-header OBU (header + payload). Writes
/// the 12-byte layout from the module docs to `out`.
///
/// # Safety
///
/// The caller must ensure `obu` points to `len` readable bytes
/// and `out` points to 12 writable bytes.
#[no_mangle]
pub unsafe extern "C" fn wasm_av1_parse_sequence_header(
    obu: *const u8,
    len: usize,
    out: *mut u8,
) -> i32 {
    if obu.is_null() || out.is_null() {
        return CODEC_ERR_INVALID_ARGUMENT;
    }
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
    // SAFETY: null-checked above; `obu` points to `len` readable
    // bytes and `header` is a valid out-struct.
    let rc = unsafe { codec_av1_parse_sequence_header(obu, len, &mut header) };
    if rc != CODEC_OK {
        return rc;
    }
    // SAFETY: `out` points to 12 writable bytes by the caller.
    unsafe {
        ptr::write(out, header.seq_profile);
        ptr::write(out.add(1), header.seq_level_idx_0);
        ptr::write(out.add(2), header.seq_tier_0);
    }
    write_u16(out, 4, header.max_frame_width_minus_1);
    write_u16(out, 6, header.max_frame_height_minus_1);
    // SAFETY: `out` points to 12 writable bytes by the caller.
    unsafe {
        ptr::write(out.add(8), header.frame_width_bits_minus_1);
        ptr::write(out.add(9), header.frame_height_bits_minus_1);
        ptr::write(out.add(10), header.reduced_still_picture_header);
    }
    CODEC_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_bytes() -> Vec<u8> {
        // A minimal valid WAV: 44-byte header + 8 bytes of i16
        // mono data (4 frames of silence at 8000 Hz).
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&44u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8000u32.to_le_bytes());
        bytes.extend_from_slice(&16000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 8]);
        bytes
    }

    #[test]
    fn probe_identifies_wav() {
        let wav = wav_bytes();
        let mut out = [0i32; 2];
        let rc = unsafe { wasm_probe(wav.as_ptr(), wav.len(), out.as_mut_ptr()) };
        assert_eq!(rc, CODEC_OK);
        assert_eq!(out, [1, 0]);
    }

    #[test]
    fn decode_wav_returns_silence() {
        let wav = wav_bytes();
        let mut samples = [0f32; 8];
        let mut meta = [0u32; 3];
        let rc = unsafe {
            wasm_decode_wav(
                wav.as_ptr(),
                wav.len(),
                samples.as_mut_ptr(),
                samples.len(),
                meta.as_mut_ptr(),
            )
        };
        assert_eq!(rc, CODEC_OK);
        assert_eq!(meta, [8000, 1, 4]);
        assert_eq!(samples, [0.0; 8]);
    }

    #[test]
    fn streaming_encoder_exports_round_trip() {
        let mut handle = 0u32;
        let rc = unsafe { wasm_encoder_new_flac(8000, 1, &mut handle) };
        assert_eq!(rc, CODEC_OK);
        assert_ne!(handle, 0);

        let mut ptr = 0usize;
        let mut len = 0usize;
        let rc = unsafe { wasm_encoder_flac_streaminfo(handle, &mut ptr, &mut len) };
        assert_eq!(rc, CODEC_OK);
        assert_eq!(len, 42);
        // SAFETY: `ptr` holds `len` bytes from the encoder export.
        let header = unsafe { slice::from_raw_parts(ptr as *const u8, len) };
        assert_eq!(&header[0..4], b"fLaC");
        unsafe {
            wasm_free(ptr as *mut u8, len);
        }

        let samples = [0.0f32; 100];
        let rc = unsafe { wasm_encoder_encode(handle, samples.as_ptr(), 100, &mut ptr, &mut len) };
        assert_eq!(rc, CODEC_OK);
        assert!(len > 0);
        unsafe {
            wasm_free(ptr as *mut u8, len);
        }

        // A freed handle is invalid afterwards.
        assert_eq!(wasm_encoder_free(handle), CODEC_OK);
        assert_eq!(wasm_encoder_free(handle), CODEC_ERR_INVALID_ARGUMENT);
        assert_eq!(wasm_encoder_free(0), CODEC_ERR_INVALID_ARGUMENT);
    }

    #[test]
    fn encode_flac_round_trips_through_wasm_alloc() {
        let samples = [0i16; 100];
        let mut ptr = 0usize;
        let mut len = 0usize;
        let rc = unsafe { wasm_encode_flac(samples.as_ptr(), 100, 1, 8000, &mut ptr, &mut len) };
        assert_eq!(rc, CODEC_OK);
        assert_ne!(ptr, 0);
        assert!(len > 42);
        // SAFETY: `ptr` holds `len` bytes from `wasm_encode_flac`.
        let bytes = unsafe { slice::from_raw_parts(ptr as *const u8, len) };
        assert_eq!(&bytes[0..4], b"fLaC");
        unsafe {
            wasm_free(ptr as *mut u8, len);
        }
    }
}
