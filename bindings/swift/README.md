# CodecWhat — Swift binding (Phase 10)

Thin SwiftPM wrapper over the C ABI. No decode or encode logic
lives here; every call forwards to the Rust core. Errors are
`CodecError` (`throws`), with the C ABI error code and the detail
from `codec_last_error()`. `Decoder` and `Encoder` are classes;
the native handle is freed in `deinit`, or earlier with
`close()`. A decoder is used from one thread at a time.

## Layout

- `Package.swift` — SwiftPM package (library `CodecWhat`).
- `Sources/CCodecWhat/module.modulemap` + vendored
  `codec_whatttttttttttt.h` — the C ABI as a system library
  target. The header is a copy of
  `crates/codec-whatttttttttttt-c/include/codec_whatttttttttttt.h`;
  CI checks the two stay identical.
- `Sources/CodecWhat/Codec.swift` — error, probe, frame and
  header types, `encodeFlac`.
- `Sources/CodecWhat/Decoder.swift` — `Decoder`.
- `Sources/CodecWhat/Encoder.swift` — `Encoder` (FLAC).
- `Tests/CodecWhatTests/DecoderTests.swift` — XCTest tests.

## Toolchain verified in this environment

Swift 6.1.2 (`~/workspace/.tools/swift-6.1.2-RELEASE-ubuntu24.04/usr/bin/swift`,
Linux x86_64).

## Build and test (from `bindings/swift`)

```text
export PATH="$HOME/workspace/.tools/swift-6.1.2-RELEASE-ubuntu24.04/usr/bin:$PATH"
cargo build --release -p codec-whatttttttttttt-c   # from the workspace root
swift build
swift test
```

The package links `target/release/libcodec_whatttttttttttt.a`
(the Rust static library) plus libm, pthread, libdl, and
libgcc_s. The linker flags in `Package.swift` are unsafe flags,
valid only for this root package; the package is not published
as a dependency.

## Scope

Same as the C ABI: version, probe, WAV/FLAC audio decode, FLAC
encode (one-shot and streaming), VP9/AV1 header parsers. The
video decoders are parser-only, so the binding exposes the
parsers, not a fake decode. See `docs/LIMITS.md`.
