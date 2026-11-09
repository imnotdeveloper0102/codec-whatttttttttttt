// decoder.zig — Zig binding for codec-whatttttttttttt (Phase 10).
//
// Thin wrapper over the C ABI, through @cImport of
// codec_whatttttttttttt.h. No decode or encode logic lives here.
// Errors are a Zig error union mapped 1:1 from the C error codes.
// Handles are freed with deinit; a decoded frame borrows its
// samples from its decoder and stays valid only until the next
// decode call on that decoder or its deinit — copy before the next
// call.

const std = @import("std");
const c = @cImport(@cInclude("codec_whatttttttttttt.h"));

pub const Error = error{
    Io,
    InvalidData,
    Truncated,
    Unsupported,
    Overflow,
    AllocationLimit,
    InvalidArgument,
    LicenseRequired,
    OutOfMemory,
};

fn check(rc: c_int) Error!void {
    return switch (rc) {
        c.CODEC_OK => {},
        c.CODEC_ERR_IO => Error.Io,
        c.CODEC_ERR_INVALID_DATA => Error.InvalidData,
        c.CODEC_ERR_TRUNCATED => Error.Truncated,
        c.CODEC_ERR_UNSUPPORTED => Error.Unsupported,
        c.CODEC_ERR_OVERFLOW => Error.Overflow,
        c.CODEC_ERR_ALLOCATION_LIMIT => Error.AllocationLimit,
        c.CODEC_ERR_INVALID_ARGUMENT => Error.InvalidArgument,
        c.CODEC_ERR_LICENSE_REQUIRED => Error.LicenseRequired,
        else => Error.InvalidArgument,
    };
}

/// Library version, e.g. "0.1.0".
pub fn version() []const u8 {
    return std.mem.span(c.codec_version());
}

pub const Container = enum(i32) {
    unknown = c.CODEC_CONTAINER_UNKNOWN,
    wav = c.CODEC_CONTAINER_WAV,
    raw_flac = c.CODEC_CONTAINER_RAW_FLAC,
    ogg = c.CODEC_CONTAINER_OGG,
    webm = c.CODEC_CONTAINER_WEBM,
    mp4 = c.CODEC_CONTAINER_MP4,
    ivf = c.CODEC_CONTAINER_IVF,
};

pub const Codec = enum(i32) {
    unknown = c.CODEC_CODEC_UNKNOWN,
    pcm = c.CODEC_CODEC_PCM,
    flac = c.CODEC_CODEC_FLAC,
    opus = c.CODEC_CODEC_OPUS,
    vorbis = c.CODEC_CODEC_VORBIS,
    av1 = c.CODEC_CODEC_AV1,
    vp9 = c.CODEC_CODEC_VP9,
};

pub const ProbeResult = struct {
    container: Container,
    codec: Codec,
};

/// Identify the container and codec of the first bytes of a file.
/// The codec is `.unknown` for Ogg, WebM, and MP4: those containers
/// hold several streams and need a demuxer to name the codec.
pub fn probe(data: []const u8) Error!ProbeResult {
    var out: c.codec_probe_t = undefined;
    try check(c.codec_probe(data.ptr, data.len, &out));
    return .{
        .container = @enumFromInt(out.container),
        .codec = @enumFromInt(out.codec),
    };
}

/// An interleaved f32 audio frame. `samples` has
/// `frames * channels` values and is borrowed from the decoder.
pub const AudioFrame = struct {
    sample_rate: u32,
    channels: u16,
    frames: u64,
    timestamp_us: u64,
    samples: []const f32,
};

/// Audio decoder. Build from a WAV file, a FLAC STREAMINFO block,
/// or a VP9 frame header, then decode one packet at a time. A
/// decoder is used from one thread at a time. Call `deinit` to
/// free the native handle.
pub const Decoder = struct {
    handle: *c.codec_decoder_t,

    /// Build a decoder from a WAV file's bytes.
    pub fn fromWav(data: []const u8) Error!Decoder {
        var handle: ?*c.codec_decoder_t = null;
        try check(c.codec_decoder_from_wav_header(data.ptr, data.len, &handle));
        return .{ .handle = handle.? };
    }

    /// Build a decoder from a 34-byte FLAC STREAMINFO payload.
    pub fn fromFlacStreaminfo(streaminfo: []const u8) Error!Decoder {
        if (streaminfo.len != 34) return Error.InvalidArgument;
        var handle: ?*c.codec_decoder_t = null;
        try check(c.codec_decoder_from_flac_streaminfo(
            streaminfo.ptr,
            streaminfo.len,
            &handle,
        ));
        return .{ .handle = handle.? };
    }

    /// Build a decoder from a VP9 frame header. `decode` on it
    /// returns `error.Unsupported`: VP9 is parser-only.
    pub fn fromVp9FrameHeader(frame: []const u8) Error!Decoder {
        var handle: ?*c.codec_decoder_t = null;
        try check(c.codec_decoder_from_vp9_frame_header(
            frame.ptr,
            frame.len,
            &handle,
        ));
        return .{ .handle = handle.? };
    }

    /// Decode one packet. A FLAC packet holds exactly one frame;
    /// an Opus packet may hold several frames of audio. The
    /// returned frame borrows from this decoder.
    pub fn decode(self: *Decoder, packet: []const u8, timestamp_us: u64) Error!AudioFrame {
        var view: ?*const c.codec_audio_frame_t = null;
        try check(c.codec_decoder_decode(
            self.handle,
            packet.ptr,
            packet.len,
            timestamp_us,
            &view,
        ));
        const frame = view.?;
        const count = @as(usize, @intCast(frame.frames)) *
            @as(usize, @intCast(frame.channels));
        return .{
            .sample_rate = frame.sample_rate,
            .channels = frame.channels,
            .frames = frame.frames,
            .timestamp_us = frame.timestamp_us,
            .samples = frame.samples[0..count],
        };
    }

    pub fn deinit(self: *Decoder) void {
        c.codec_decoder_free(self.handle);
        self.handle = undefined;
    }
};

/// FLAC encoder (streaming). One input frame becomes one FLAC
/// frame, block size 1..=4096. Call `deinit` to free the handle.
pub const Encoder = struct {
    handle: *c.codec_encoder_t,
    sample_rate: u32,
    channels: u32,

    /// Build a FLAC encoder. `sample_rate` is 1..=192000,
    /// `channels` 1..=8.
    pub fn flac(sample_rate: u32, channels: u16) Error!Encoder {
        var handle: ?*c.codec_encoder_t = null;
        try check(c.codec_encoder_new_flac(sample_rate, channels, &handle));
        return .{
            .handle = handle.?,
            .sample_rate = sample_rate,
            .channels = channels,
        };
    }

    /// The streaming STREAMINFO header (42 bytes: "fLaC" + block
    /// header + 34-byte STREAMINFO, total samples 0). Borrowed
    /// from this encoder; copy before the next call.
    pub fn streaminfo(self: *Encoder) Error![]const u8 {
        var view: ?*const c.codec_packet_t = null;
        try check(c.codec_encoder_flac_streaminfo(self.handle, &view));
        const packet = view.?;
        return packet.data[0..packet.len];
    }

    /// Encode one frame of interleaved f32 samples. `frames` is
    /// the number of frames (samples per channel), 1..=4096. The
    /// returned packet is borrowed from this encoder.
    pub fn encode(self: *Encoder, samples: []const f32, frames: u64) Error![]const u8 {
        var view: ?*const c.codec_packet_t = null;
        try check(c.codec_encoder_encode(self.handle, samples.ptr, frames, &view));
        const packet = view.?;
        return packet.data[0..packet.len];
    }

    pub fn deinit(self: *Encoder) void {
        c.codec_encoder_free(self.handle);
        self.handle = undefined;
    }
};

/// One-shot FLAC encode: interleaved i16 samples -> a complete
/// FLAC file with exact STREAMINFO, owned by `allocator`.
/// `frames` is the number of frames (samples per channel).
pub fn encodeFlac(
    allocator: std.mem.Allocator,
    samples: []const i16,
    frames: u64,
    channels: u16,
    sample_rate: u32,
) Error![]u8 {
    var data: [*c]u8 = undefined;
    var len: usize = 0;
    try check(c.codec_encode_flac(samples.ptr, frames, channels, sample_rate, &data, &len));
    const out = allocator.dupe(u8, data[0..len]) catch return Error.OutOfMemory;
    c.codec_free(data);
    return out;
}

/// A parsed VP9 frame header (parser-only; no reconstruction).
pub const Vp9FrameHeader = struct {
    profile: u8,
    frame_type: u8, // 0 = key, 1 = inter
    show_frame: bool,
    show_existing_frame: bool,
    width: u16,
    height: u16,
    render_width: u16,
    render_height: u16,
    base_q_idx: u8,
    loop_filter_level: u8,
    tile_cols_log2: u8,
    tile_rows_log2: u8,
    header_size_in_bytes: u32,
};

/// Parse a VP9 frame header (parser-only; no reconstruction).
pub fn parseVp9FrameHeader(frame: []const u8) Error!Vp9FrameHeader {
    var out: c.codec_vp9_frame_header_t = undefined;
    try check(c.codec_vp9_parse_frame_header(frame.ptr, frame.len, &out));
    return .{
        .profile = out.profile,
        .frame_type = out.frame_type,
        .show_frame = out.show_frame != 0,
        .show_existing_frame = out.show_existing_frame != 0,
        .width = out.width,
        .height = out.height,
        .render_width = out.render_width,
        .render_height = out.render_height,
        .base_q_idx = out.base_q_idx,
        .loop_filter_level = out.loop_filter_level,
        .tile_cols_log2 = out.tile_cols_log2,
        .tile_rows_log2 = out.tile_rows_log2,
        .header_size_in_bytes = out.header_size_in_bytes,
    };
}

/// A parsed AV1 sequence header (first operating point).
pub const Av1SequenceHeader = struct {
    seq_profile: u8,
    seq_level_idx_0: u8,
    seq_tier_0: u8,
    max_frame_width_minus_1: u16,
    max_frame_height_minus_1: u16,
    frame_width_bits_minus_1: u8,
    frame_height_bits_minus_1: u8,
    reduced_still_picture_header: bool,
};

/// Parse one AV1 sequence-header OBU (header + payload).
pub fn parseAv1SequenceHeader(obu: []const u8) Error!Av1SequenceHeader {
    var out: c.codec_av1_sequence_header_t = undefined;
    try check(c.codec_av1_parse_sequence_header(obu.ptr, obu.len, &out));
    return .{
        .seq_profile = out.seq_profile,
        .seq_level_idx_0 = out.seq_level_idx_0,
        .seq_tier_0 = out.seq_tier_0,
        .max_frame_width_minus_1 = out.max_frame_width_minus_1,
        .max_frame_height_minus_1 = out.max_frame_height_minus_1,
        .frame_width_bits_minus_1 = out.frame_width_bits_minus_1,
        .frame_height_bits_minus_1 = out.frame_height_bits_minus_1,
        .reduced_still_picture_header = out.reduced_still_picture_header != 0,
    };
}

// ---------------------------------------------------------------------------
// Tests (Phase 10). Run with `zig build test` from bindings/zig.
// The fixtures are the pinned files in the core crate's tests/data;
// the first candidate directory that exists is used, so the tests
// also run when the test binary is started from the workspace root.
// ---------------------------------------------------------------------------

fn readFixture(allocator: std.mem.Allocator, name: []const u8) ![]u8 {
    const candidates = [_][]const u8{
        "../../crates/codec-whatttttttttttt-core/tests/data/",
        "crates/codec-whatttttttttttt-core/tests/data/",
    };
    for (candidates) |dir| {
        const path = try std.fs.path.join(allocator, &.{ dir, name });
        defer allocator.free(path);
        if (std.fs.cwd().readFileAlloc(allocator, path, 512 * 1024 * 1024)) |bytes| {
            return bytes;
        } else |_| {}
    }
    return error.FileNotFound;
}

/// Offset of the first FLAC frame: skip the metadata blocks.
fn firstFlacFrameOffset(flac: []const u8) usize {
    var offset: usize = 4;
    while (true) {
        const header = flac[offset];
        const length = (@as(usize, flac[offset + 1]) << 16) |
            (@as(usize, flac[offset + 2]) << 8) |
            @as(usize, flac[offset + 3]);
        offset += 4 + length;
        if (header & 0x80 != 0) break;
    }
    return offset;
}

test "version is 0.1.0" {
    try std.testing.expectEqualStrings("0.1.0", version());
}

test "probe wav and flac" {
    const allocator = std.testing.allocator;
    const wav = try readFixture(allocator, "valid_i16_stereo_44100.wav");
    defer allocator.free(wav);
    const wav_probe = try probe(wav);
    try std.testing.expectEqual(Container.wav, wav_probe.container);
    try std.testing.expectEqual(Codec.pcm, wav_probe.codec);

    const flac = try readFixture(allocator, "sine_stereo_44100.flac");
    defer allocator.free(flac);
    const flac_probe = try probe(flac);
    try std.testing.expectEqual(Container.raw_flac, flac_probe.container);
    try std.testing.expectEqual(Codec.flac, flac_probe.codec);
}

test "decode flac frame" {
    const allocator = std.testing.allocator;
    const flac = try readFixture(allocator, "sine_stereo_44100.flac");
    defer allocator.free(flac);
    var decoder = try Decoder.fromFlacStreaminfo(flac[8..42]);
    defer decoder.deinit();
    const offset = firstFlacFrameOffset(flac);
    const frame = try decoder.decode(flac[offset..], 0);
    try std.testing.expectEqual(@as(u32, 44100), frame.sample_rate);
    try std.testing.expectEqual(@as(u16, 2), frame.channels);
    try std.testing.expectEqual(@as(u64, 4410), frame.frames);
    try std.testing.expect(@abs(frame.samples[0]) < 0.001);
}

test "decode wav frame" {
    const allocator = std.testing.allocator;
    const wav = try readFixture(allocator, "valid_i16_stereo_44100.wav");
    defer allocator.free(wav);
    var decoder = try Decoder.fromWav(wav);
    defer decoder.deinit();
    const frame = try decoder.decode(wav[44..], 0);
    try std.testing.expectEqual(@as(u32, 44100), frame.sample_rate);
    try std.testing.expectEqual(@as(u64, 4410), frame.frames);
}

test "one-shot flac encode round-trips" {
    const allocator = std.testing.allocator;
    var samples: [2000]i16 = undefined;
    for (0..1000) |n| {
        const t = @as(f64, @floatFromInt(n)) / 44100.0;
        samples[2 * n] = @intFromFloat(20000.0 * std.math.sin(2.0 * std.math.pi * 440.0 * t));
        samples[2 * n + 1] = @intFromFloat(15000.0 * std.math.sin(2.0 * std.math.pi * 660.0 * t));
    }
    const encoded = try encodeFlac(allocator, &samples, 1000, 2, 44100);
    defer allocator.free(encoded);
    try std.testing.expect(encoded.len > 42);
    try std.testing.expectEqualStrings("fLaC", encoded[0..4]);
    var decoder = try Decoder.fromFlacStreaminfo(encoded[8..42]);
    defer decoder.deinit();
    const offset = firstFlacFrameOffset(encoded);
    const frame = try decoder.decode(encoded[offset..], 0);
    try std.testing.expectEqual(@as(u64, 1000), frame.frames);
    try std.testing.expect(@abs(frame.samples[0]) < 1.0);
}

test "streaming flac encoder" {
    var encoder = try Encoder.flac(44100, 2);
    defer encoder.deinit();
    const header = try encoder.streaminfo();
    try std.testing.expectEqual(@as(usize, 42), header.len);
    var floats: [2000]f32 = undefined;
    for (0..1000) |n| {
        const t = @as(f32, @floatFromInt(n)) / 44100.0;
        floats[2 * n] = 0.5 * std.math.sin(2.0 * std.math.pi * 440.0 * t);
        floats[2 * n + 1] = 0.5 * std.math.sin(2.0 * std.math.pi * 660.0 * t);
    }
    const packet = try encoder.encode(&floats, 1000);
    try std.testing.expect(packet.len > 0);
}

test "parse vp9 key frame header" {
    const allocator = std.testing.allocator;
    const ivf = try readFixture(allocator, "vp9_64x64.ivf");
    defer allocator.free(ivf);
    const frame_size = std.mem.readInt(u32, ivf[32..36], .little);
    const header = try parseVp9FrameHeader(ivf[44 .. 44 + frame_size]);
    try std.testing.expectEqual(@as(u8, 0), header.profile);
    try std.testing.expectEqual(@as(u8, 0), header.frame_type);
    try std.testing.expectEqual(@as(u16, 64), header.width);
    try std.testing.expectEqual(@as(u16, 64), header.height);
    try std.testing.expectEqual(@as(u8, 33), header.base_q_idx);
}

test "vp9 decode returns unsupported" {
    const allocator = std.testing.allocator;
    const ivf = try readFixture(allocator, "vp9_64x64.ivf");
    defer allocator.free(ivf);
    const frame_size = std.mem.readInt(u32, ivf[32..36], .little);
    const frame = ivf[44 .. 44 + frame_size];
    var decoder = try Decoder.fromVp9FrameHeader(frame);
    defer decoder.deinit();
    try std.testing.expectError(Error.Unsupported, decoder.decode(frame, 0));
}

test "parse av1 sequence header" {
    const allocator = std.testing.allocator;
    const obu = try readFixture(allocator, "av1_seq_header.obu");
    defer allocator.free(obu);
    const header = try parseAv1SequenceHeader(obu);
    try std.testing.expectEqual(@as(u8, 0), header.seq_profile);
    try std.testing.expectEqual(@as(u16, 63), header.max_frame_width_minus_1);
    try std.testing.expectEqual(@as(u16, 63), header.max_frame_height_minus_1);
}

test "bad wav returns an error" {
    // A 4-byte input is truncated as a RIFF header; a short but
    // RIFF-like input would be invalid data instead.
    const result = Decoder.fromWav(&[_]u8{ 0, 1, 2, 3 });
    if (result) |decoder| {
        var owned = decoder;
        owned.deinit();
        return error.TestExpectedError;
    } else |e| {
        try std.testing.expect(e == Error.Truncated or e == Error.InvalidData);
    }
}
