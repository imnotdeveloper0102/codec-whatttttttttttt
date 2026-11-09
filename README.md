# codec-whatttttttttttt

Open-format audio and video codec library. Core in Rust, with thin
bindings to C++, Kotlin, Swift, TypeScript/React Native, Python, and
Zig in later phases. Open formats in software: PCM/WAV, FLAC, Opus, Vorbis,
AV1, VP9. H.264/AVC and H.265/HEVC exist only behind a license gate
(hardware path only, never software decode or encode); see
"Licensed codecs (license gate)" below and `NOTICE`.

Status: Phase 0–11 complete (all 12 phases), plus a next
phase (2026-11-08/09): AVC/HEVC/AAC passthrough behind the
license gate, MP3 Layer III decode, AAC-LC decode behind
`licensed-aac`, ALAC decode (bit-exact vs ffmpeg), MPEG-TS /
ADTS / AIFF demux-mux, ID3v1/v2, MP4 tags, Vorbis comment,
bitrate modes, a windowed-sinc polyphase resampler, and DSD
(DSF/DFF to PCM + DoP); plus a final round (2026-11-09):
MP3-in-TS, Vorbis floor type 0, FLAC 20/24-bit and >2-channel
decode/encode, MP3 CBR encode, AAC-LC encode behind the gate,
ALAC encode (bit-exact), PCM→DSD delta-sigma modulation,
Opus CELT sample reconstruction, VP9 reconstruction (keyframes
bit-exact, inter frames `Unsupported`), and AV1 tile decode +
intra reconstruction (no pixels yet — coefficient CDF tables
missing). WAV/PCM decode and encode work, FLAC decode works
(native `.flac` and FLAC-in-Ogg), Ogg demux/mux work, and
Vorbis decode works end to end (Ogg Vorbis to PCM, matching
libvorbis to within 1e-6). Opus CELT decode works (verified vs
ffmpeg, RMS ~1e-5 on CELT-only fullband stereo frames); SILK
returns `Unsupported`. WebM and MP4 demux work (Phase 6):
tracks, packets, timestamps, and keyframe seeking, checked
against ffmpeg-produced files. AV1 tile decode + intra
reconstruction exist (Phase 7 + final round) but no pixels are
produced yet — coefficient decode returns
`Unsupported(COEFFICIENT_GAP)`. VP9 reconstruction exists
(Phase 8 + final round): keyframes decode bit-exactly, inter
frames return `Unsupported`. FLAC encode works (Phase 9 +
final round): 16/20/24-bit FLAC with constant, verbatim,
fixed-predictor, and LPC subframes, chosen by exact encoded
size; encode→decode round-trips byte-identically and ffmpeg's
FLAC decoder decodes the output to the same PCM. MP3 CBR,
AAC-LC (behind the gate), ALAC, and DSD encode work; Opus and
Vorbis encode return `Unsupported`. Bindings (Phase 10): a C
ABI crate (`crates/codec-whatttttttttttt-c/`) with a C++ RAII
wrapper, plus thin bindings in `bindings/`: Python (ctypes),
Kotlin (JNI), Swift (SwiftPM), TypeScript (WASM + React Native
TurboModule), and Zig. Every binding is verified by its own
tests in this environment except the React Native native
modules (Android/iOS), which type-check but were never built
here — no Android SDK, Metro, or macOS. Hardening (Phase 11):
cargo-fuzz targets for all nine parsers (60 s each, 3 crashes
found and fixed), property-based tests, an internal benchmark
(FLAC decode 1101.6x realtime, Vorbis decode 0.7x — the naive
O(N²) MDCT, a performance limit, not a bug), ASan/UBSan runs of
the C example and C++ driver, and CI jobs for every binding
plus fuzz and sanitizers. See `docs/DESIGN.md`,
`docs/LIMITS.md`, and `CHANGELOG.md`.

## Supported codecs

| Codec | Decode | Encode |
|---|---|---|
| PCM/WAV | yes | yes |
| FLAC | yes (16/20/24-bit, up to 8 channels) | yes (16/20/24-bit, constant/verbatim/fixed/LPC) |
| Opus | CELT yes (verified vs ffmpeg, RMS ~1e-5); SILK/Hybrid `Unsupported` | no (needs CELT/SILK encoders) |
| Vorbis | yes (floor type 0 and 1) | no (needs codebook/floor/residue encoders) |
| AV1 | tile decode + intra reconstruction; no pixels yet — coefficient decode returns `Unsupported(COEFFICIENT_GAP)` | no (needs a full video encoder) |
| VP9 | keyframes bit-exact; inter frames return `Unsupported` | no (needs a full video encoder) |
| MP3 (Layer III) | yes (ISO full compliance, measured) | yes, CBR only (MPEG-1 64/96/128/320, MPEG-2/2.5 64/96/128); VBR/ABR `Unsupported`; Layer I/II decode `Unsupported` |
| AAC-LC | yes — behind `licensed-aac` (feature off → `Unsupported`, no valid license → `LicenseRequired`) | yes — same gate (feature off → `Unsupported`, no valid license → `LicenseRequired` at construction); HE-AAC/SBR/LTP `Unsupported` |
| ALAC | yes (bit-exact vs ffmpeg) | yes (16/24-bit, bit-exact); 20/32-bit `Unsupported` |
| DSD (DSF/DFF) | yes — DSD to PCM decimation; DoP DSD64/128 | yes — PCM→DSD delta-sigma (DSD64/128/256/512); THD+N −84.9 dB, not realtime |
| AVC (H.264) | passthrough only — packets behind the `licensed-avc-hevc` feature (off by default); no software decode, ever | no — same gate |
| HEVC (H.265) | passthrough only — same gate as AVC | no — same gate as AVC |

Containers: WAV in Phase 1. Ogg demux/mux in Phase 3 (single
logical stream; FLAC-in-Ogg, Vorbis-in-Ogg, and Opus CELT
decode end to end). WebM/Matroska and MP4 demux in Phase 6
(`Demuxer`: probe by magic, tracks, packets in demux order,
keyframe seek via Cues / `stss`). IVF and raw AV1 OBU streams are
read by the CLI `info` command in Phase 7; IVF with fourcc VP90
is read by the CLI `info` command in Phase 8 (VP9 frame headers
and superframe splits). Next phase: MPEG-TS demux/mux, ADTS
demux/mux, AIFF/AIFF-C demux/mux, DSF/DFF demux, ID3v1/v2
read/write, MP4 `meta`/`ilst` tags, and Vorbis comment
read/write; MP4 `avc1`/`hev1`/`mp4a` and Matroska
`V_MPEG4/ISO/AVC`, `V_MPEGH/ISO/HEVC`, `A_AAC` demux as
passthrough tracks behind the license gate; MPEG-TS stream
types 0x03/0x04 demux as MP3.

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

## Licensed codecs (license gate)

H.264/AVC, H.265/HEVC, and AAC are patent-encumbered. This
library has no software encoder or decoder for AVC/HEVC:
`Decoder` and `Encoder` refuse `Codec::Avc` / `Codec::Hevc`
outright. The only path for them is the hardware path (`hw.rs`:
`HardwareDecoder` / `HardwareEncoder`, currently stubs that
call no real hardware API), opened with a caller-supplied
`LicenseContext` (`license.rs`). AAC-LC software decode and
encode exist behind `licensed-aac` (see the table above);
MP3 Layer III software decode and CBR encode exist with no
gate (MP3's patent status still needs verification — see
`NOTICE`); ALAC software decode and encode exist with no
gate. The gate is off by default — features
`licensed-avc-hevc` / `licensed-aac` in the core `Cargo.toml`
(`default = []`). Feature off gives `Error::Unsupported`;
feature on without a valid license gives
`Error::LicenseRequired { codec }` (C ABI
`CODEC_ERR_LICENSE_REQUIRED = 8`); a malformed license gives
`InvalidArgument`, an expired one `LicenseRequired`. The gate
only checks a license's shape and expiry and keeps an
in-memory audit log; it is a compliance mechanism, not a
substitute for a real patent license and not legal advice. See
`NOTICE`. Demux: AVC/HEVC/AAC tracks demux as passthrough
packets behind the gate (MP4 `avc1`/`hev1`/`mp4a`, Matroska
`V_MPEG4/ISO/AVC`, `V_MPEGH/ISO/HEVC`, `A_AAC`); parsing
succeeds without a license, only `next_packet` refuses
licensed packets.

## Resolution, scaling, frame pool

`resolution.rs` validates frame sizes against AV1 levels (14
defined levels, AV1 spec Annex A §10.3; level 5.2 display rate
is unverified — see `docs/LIMITS.md`) and VP9 profiles 0–3 (VP9
has no level system; profiles constrain bit depth and chroma
only, size is bounded by the library 8192 limit), and suggests
bitrate/CRF values (`suggest_av1` / `suggest_vp9` — suggestions,
not spec values). `scaler.rs` scales frames with `Bilinear`,
`Bicubic`, or `Lanczos3` (`scale_frame`). `frame_pool.rs` reuses
decoded-frame buffers (`FramePool::checkout` / `checkin`); an 8K
YUV420 8-bit frame (49,766,400 bytes) fits the 64 MiB per-call
cap, while 10-bit 8K (99,532,800 bytes) is deliberately rejected
with `AllocationLimit` — the cap was not raised. See
`docs/LIMITS.md`.

## Quality labels, dither, I32 PCM

`quality.rs` classifies measured stream parameters as
`Lossless` (lossless, at most 16-bit / 48 kHz) or `Hi-Res
Lossless` (lossless, above that, at most 24-bit / 192 kHz),
else `None` — commercial labels, not formats. `dither.rs`
quantizes 24-bit or f32 samples to 16-bit with TPDF dither
(seeded, reproducible) and optional first-order noise shaping
(`quantize_i24_to_i16`, `quantize_f32_to_i16`). `pcm.rs` has
`PcmFormat::I32` with bit-exact `decode_i32_samples` /
`encode_i32_samples` (the f32 path rounds i32 to 24 significant
bits); WAV tag (1, 32) maps to `I32`.

## Documentation

- `NOTICE` — license-gate and patent-status notes.
- `docs/DESIGN.md` — design, module list, public API, error model,
  memory model, data flow, phases.
- `docs/LIMITS.md` — what each phase actually supports, per
  container and codec, and what is explicitly not done.
- `CHANGELOG.md` — changes per phase.

## License

MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
