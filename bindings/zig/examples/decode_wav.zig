// decode_wav.zig — example for the Zig binding (Phase 10).
//
// Decodes a WAV file and prints the frame count, sample rate,
// channel count, and first sample.
//
// Run from bindings/zig:
//   zig build run -- ../../crates/codec-whatttttttttttt-core/tests/data/valid_i16_stereo_44100.wav

const std = @import("std");
const codec = @import("codec_whatttttttttttt");

pub fn main() !void {
    var arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    const args = try std.process.argsAlloc(allocator);
    const path = if (args.len > 1)
        args[1]
    else
        "../../crates/codec-whatttttttttttt-core/tests/data/valid_i16_stereo_44100.wav";

    std.debug.print("codec-whatttttttttttt {s}\n", .{codec.version()});

    const wav = std.fs.cwd().readFileAlloc(allocator, path, 512 * 1024 * 1024) catch |err| {
        std.debug.print("cannot read {s}: {s}\n", .{ path, @errorName(err) });
        std.process.exit(1);
    };
    const probe = try codec.probe(wav);
    std.debug.print("probe: container={s} codec={s}\n", .{
        @tagName(probe.container),
        @tagName(probe.codec),
    });

    var decoder = try codec.Decoder.fromWav(wav);
    defer decoder.deinit();
    const frame = try decoder.decode(wav[44..], 0);
    std.debug.print(
        "decoded {d} frames at {d} Hz, {d} channels, first sample {d}\n",
        .{ frame.frames, frame.sample_rate, frame.channels, frame.samples[0] },
    );
}
