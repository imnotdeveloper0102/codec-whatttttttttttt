# codec-whatttttttttttt — Zig binding (Phase 10)

Thin wrapper over the C ABI, through `@cImport` of
`codec_whatttttttttttt.h`. No decode or encode logic lives here.
Errors are a Zig error union mapped 1:1 from the C error codes.
Handles are freed with `deinit`; a decoded frame borrows its
samples from its decoder and stays valid only until the next
decode call on that decoder or its `deinit` — copy before the
next call. `encodeFlac` takes an explicit allocator and returns
an allocator-owned buffer.

## Layout

- `src/decoder.zig` — the binding (`Decoder`, `Encoder`,
  `probe`, `encodeFlac`, VP9/AV1 header parsers) and its tests.
- `examples/decode_wav.zig` — runnable example.
- `build.zig`, `build.zig.zon` — the Zig build.

(The Step 1 design sketched a `src/main.zig`; the binding is a
module, not a binary, so there is no `main.zig` — the example is
the binary.)

## Toolchain verified in this environment

Zig 0.14.1 (`~/workspace/.tools/zig-x86_64-linux-0.14.1/zig`).

## Build and test (from `bindings/zig`)

```text
cargo build --release -p codec-whatttttttttttt-c   # from the workspace root
zig build test
zig build run -- ../../crates/codec-whatttttttttttt-core/tests/data/valid_i16_stereo_44100.wav
```

The build links `target/release/libcodec_whatttttttttttt.a`
(the Rust static library) plus libc, libm, pthread, libdl, and
libgcc_s (the `_Unwind_*` symbols the Rust standard library's
panic and backtrace code references).

## Scope

Same as the C ABI: version, probe, WAV/FLAC audio decode, FLAC
encode (one-shot and streaming), VP9/AV1 header parsers. The
video decoders are parser-only, so the binding exposes the
parsers, not a fake decode. See `docs/LIMITS.md`.
