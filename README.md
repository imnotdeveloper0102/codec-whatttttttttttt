# codec-whatttttttttttt

Open-format audio and video codec library. Core in Rust, with thin
bindings to C++, Kotlin, Swift, TypeScript/React Native, Python, and
Zig in later phases. Only open formats: PCM/WAV, FLAC, Opus, Vorbis,
AV1, VP9. No patented codecs.

Status: Phase 0–11 complete (all 12 phases). WAV/PCM decode and encode work, FLAC
decode works (native `.flac` and FLAC-in-Ogg), Ogg demux/mux
work, and Vorbis decode works end to end (Ogg Vorbis to PCM,
matching libvorbis to within 1e-6). Opus is partial: packet
parsing, OpusHead/Tags, range decoder, CELT frame-header symbols,
and packet-loss concealment work, but CELT sample reconstruction
and SILK return `Unsupported` until their tables land. WebM and
MP4 demux work (Phase 6): tracks, packets, timestamps, and
keyframe seeking, checked against ffmpeg-produced files. AV1 is
parser-only (Phase 7): OBU, sequence header, frame header, and
tile-group framing parse and match ffmpeg's `trace_headers` output
bit for bit; pixel reconstruction returns `Unsupported` until a
later phase. VP9 is parser-only (Phase 8): uncompressed and
compressed frame headers, superframe indexes, and tile framing
parse and match ffmpeg's `trace_headers` output and libvpx
behaviour; pixel reconstruction returns `Unsupported` until a
later phase. FLAC encode works (Phase 9): 16-bit FLAC with
constant, verbatim, fixed-predictor, and LPC subframes, chosen by
exact encoded size; encode→decode round-trips byte-identically
and ffmpeg's FLAC decoder decodes the output to the same PCM.
Opus, Vorbis, VP9, and AV1 encode return `Unsupported`. Bindings
(Phase 10): a C ABI crate (`crates/codec-whatttttttttttt-c/`)
with a C++ RAII wrapper, plus thin bindings in
`bindings/`: Python (ctypes), Kotlin (JNI), Swift (SwiftPM),
TypeScript (WASM + React Native TurboModule), and Zig. Every
binding is verified by its own tests in this environment except
the React Native native modules (Android/iOS), which type-check
but were never built here — no Android SDK, Metro, or macOS.
Hardening (Phase 11): cargo-fuzz targets for all nine parsers
(60 s each, 3 crashes found and fixed), property-based tests,
an internal benchmark (FLAC decode 1101.6x realtime, Vorbis
decode 0.7x — the naive O(N²) MDCT, a performance limit, not a
bug), ASan/UBSan runs of the C example and C++ driver, and CI
jobs for every binding plus fuzz and sanitizers. See
`docs/DESIGN.md`, `docs/LIMITS.md`, and `CHANGELOG.md`.

## Supported codecs

| Codec | Decode | Encode |
|---|---|---|
| PCM/WAV | yes | yes |
| FLAC | yes | yes (Phase 9: 16-bit, constant/verbatim/fixed/LPC) |
| Opus | partial (Phase 4: headers, silence, PLC) | no (needs CELT/SILK encoders) |
| Vorbis | yes | no (needs codebook/floor/residue encoders) |
| AV1 | parser-only (Phase 7: OBU + sequence + frame headers + tile framing; reconstruction returns `Unsupported`) | no (needs a full video encoder) |
| VP9 | parser-only (Phase 8: uncompressed + compressed headers, superframe, tile framing; reconstruction returns `Unsupported`) | no (needs a full video encoder) |

Containers: WAV in Phase 1. Ogg demux/mux in Phase 3 (single
logical stream; FLAC-in-Ogg and Vorbis-in-Ogg decode end to end,
Opus packets demux and parse, Opus audio samples return
`Unsupported`). WebM/Matroska and MP4 demux in Phase 6
(`Demuxer`: probe by magic, tracks, packets in demux order,
keyframe seek via Cues / `stss`). IVF and raw AV1 OBU streams are
read by the CLI `info` command in Phase 7; IVF with fourcc VP90
is read by the CLI `info` command in Phase 8 (VP9 frame headers
and superframe splits).

## Install

Requires a stable Rust toolchain (1.75 or later).

```text
cargo build --workspace
cargo test --workspace
```

## Minimal example

```text
cargo run -p codec-whatttttttttttt-cli -- info crates/codec-whatttttttttttt-core/tests/data/valid_i16_stereo_44100.wav
cargo run -p codec-whatttttttttttt-cli -- decode input.wav output.wav
cargo run -p codec-whatttttttttttt-cli -- info crates/codec-whatttttttttttt-core/tests/data/sine_stereo_44100.flac
cargo run -p codec-whatttttttttttt-cli -- decode input.flac output.wav
cargo run -p codec-whatttttttttttt-cli -- info crates/codec-whatttttttttttt-core/tests/data/sine_stereo_44100.ogg
cargo run -p codec-whatttttttttt-cli -- decode input.ogg output.wav
cargo run -p codec-whatttttttttttt-cli -- info crates/codec-whatttttttttttt-core/tests/data/av1_opus.webm
cargo run -p codec-whatttttttttttt-cli -- info crates/codec-whatttttttttttt-core/tests/data/av1_opus.mp4
cargo run -p codec-whatttttttttttt-cli -- info crates/codec-whatttttttttttt-core/tests/data/av1_64x64.ivf
cargo run -p codec-whatttttttttttt-cli -- info crates/codec-whatttttttttttt-core/tests/data/av1_seq_header.obu
```

```rust
use codec_whatttttttttttt_core::{Decoder, WavDemuxer};
use std::io::Cursor;

let wav_bytes: Vec<u8> = std::fs::read("input.wav").unwrap();
let mut demuxer = WavDemuxer::new(Cursor::new(wav_bytes)).unwrap();
let mut decoder = Decoder::from_wav_header(demuxer.header()).unwrap();
while let Some(packet) = demuxer.next_packet().unwrap() {
    let frame = decoder.decode(&packet).unwrap();
    let _ = frame;
}
```

## Documentation

- `docs/DESIGN.md` — design, module list, public API, error model,
  memory model, data flow, phases.
- `docs/LIMITS.md` — what each phase actually supports, per
  container and codec, and what is explicitly not done.
- `CHANGELOG.md` — changes per phase.

## License

MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
