// build.zig — Zig binding for codec-whatttttttttttt (Phase 10).
//
// `zig build test` runs the binding tests in src/decoder.zig.
// `zig build run -- <wav file>` runs the decode_wav example.
// The binding links the Rust static library, so build the C crate
// first: `cargo build --release -p codec-whatttttttttttt-c` from
// the workspace root.

const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const include_dir = b.path("../../crates/codec-whatttttttttttt-c/include");
    const static_lib = b.path("../../target/release/libcodec_whatttttttttttt.a");

    // The binding as a module, for the example to import.
    const module = b.addModule("codec_whatttttttttttt", .{
        .root_source_file = b.path("src/decoder.zig"),
        .target = target,
        .optimize = optimize,
    });
    module.addIncludePath(include_dir);

    const lib_tests = b.addTest(.{
        .root_source_file = b.path("src/decoder.zig"),
        .target = target,
        .optimize = optimize,
    });
    lib_tests.addIncludePath(include_dir);
    lib_tests.addObjectFile(static_lib);
    linkSystem(lib_tests);

    const run_tests = b.addRunArtifact(lib_tests);
    const test_step = b.step("test", "Run the binding tests");
    test_step.dependOn(&run_tests.step);

    const example = b.addExecutable(.{
        .name = "decode_wav",
        .root_source_file = b.path("examples/decode_wav.zig"),
        .target = target,
        .optimize = optimize,
    });
    example.root_module.addImport("codec_whatttttttttttt", module);
    example.addIncludePath(include_dir);
    example.addObjectFile(static_lib);
    linkSystem(example);

    b.installArtifact(example);
    const run_example = b.addRunArtifact(example);
    if (b.args) |args| run_example.addArgs(args);
    const run_step = b.step("run", "Run the decode_wav example");
    run_step.dependOn(&run_example.step);
}

// The Rust static library needs libc and its usual companions.
// libgcc_s provides the _Unwind_* symbols the Rust standard
// library's panic and backtrace code references.
fn linkSystem(step: *std.Build.Step.Compile) void {
    step.linkLibC();
    step.linkSystemLibrary("m");
    step.linkSystemLibrary("pthread");
    step.linkSystemLibrary("dl");
    step.linkSystemLibrary("gcc_s");
}
