# codec-whatttttttttttt-c — C ABI (Phase 10)

Thin C ABI over the Rust core. No decode or encode logic lives
here; every function forwards to the core and maps its error enum
to the codes in `include/codec_whatttttttttttt.h`.

## Layout

- `src/lib.rs` — the FFI implementation (`#[no_mangle]`,
  `unsafe extern "C"`, `catch_unwind` at the boundary, thread-local
  last-error detail).
- `include/codec_whatttttttttttt.h` — the C header (the contract).
- `include/codec_whatttttttttttt.hpp` — a header-only C++ RAII
  wrapper (`codec::decoder`, `codec::encoder`, `codec::encode_flac`,
  `codec::probe`).
- `examples/encode_decode.c` — runnable C example (one-shot FLAC
  encode, streaming round-trip).
- `tests/ffi.rs` — Rust tests calling the FFI functions (9 tests).
- `tests/test_cpp.cpp` — C++ wrapper test, compiled with g++.

## Build

```text
cargo build -p codec-whatttttttttttt-c
```

Produces `target/debug/libcodec_whatttttttttttt.so` (and `.a`).

## C example

```text
cc -I crates/codec-whatttttttttttt-c/include \
   crates/codec-whatttttttttttt-c/examples/encode_decode.c \
   -L target/debug -lcodec_whatttttttttttt -lm \
   -Wl,-rpath,target/debug -o /tmp/encode_decode
/tmp/encode_decode
```

## C++ test

```text
g++ -std=c++17 -Wall -Wextra \
  -I crates/codec-whatttttttttttt-c/include \
  crates/codec-whatttttttttttt-c/tests/test_cpp.cpp \
  -L target/debug -lcodec_whatttttttttttt -lm \
  -Wl,-rpath,target/debug -o /tmp/test_cpp
/tmp/test_cpp
```

## Ownership

Opaque handles are freed with the matching `codec_*_free`.
Buffers the library allocates are freed with `codec_free`. A
decoded frame (and an encoded packet) is borrowed from its
decoder/encoder and stays valid only until the next call on the
same handle or its free; copy it before the next call. The C++
wrapper copies on return, so it never holds a borrowed view.

## Scope

The C ABI covers version, error codes + messages, probe
(container/codec from the first bytes), audio decode (decoder from
a WAV file or a FLAC STREAMINFO block, decode one packet to an
interleaved f32 frame), FLAC encode (one-shot and the streaming
encoder), and the VP9/AV1 header parsers. The video decoders are
parser-only, so the bindings expose the parsers, not a fake decode.
See `docs/LIMITS.md` for what the core actually implements.
