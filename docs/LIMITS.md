# Limits and supported subsets

This file records what the library actually does, per phase. It is
updated whenever a phase lands; a claim here must have a passing
test behind it.

## Final round (2026-11-09): small items, encoders, PCM→DSD, CELT, VP9, AV1

- MP3 in MPEG-TS: stream types 0x03/0x04 map to `Codec::Mp3`;
  a TS file carrying MP3 decodes (40 frames, RMS 3.328e-8 vs
  ffmpeg). Vorbis floor type 0 packet decode works, verified
  on a synthetic stream only (no real floor-0 file exists).
  FLAC 20/24-bit and >2 channels decode and encode
  (`encode_flac_frame_wide`, `FlacEncoder::encode_i32`,
  `EncoderConfig.bits_per_sample`); 32-bit FLAC encode is
  intentionally not done.
- MP3 CBR encode exists (MPEG-1 64/96/128/320 kbps, MPEG-2/2.5
  64/96/128); VBR/ABR are `Unsupported` by design. One frame
  of encoder delay: `encode()` returns the previous frame and
  the tail only appears on `flush()`.
- AAC-LC encode exists behind the `licensed-aac` gate
  (feature off → `Unsupported`, no/expired license →
  `LicenseRequired` at `Encoder` construction). CBR/ABR/VBR;
  one scalefactor per frame; no bit reservoir, so frames can
  be smaller than the CBR budget. HE-AAC/SBR `Unsupported`.
- ALAC encode exists (16/24-bit, 1–8 channels, frames
  1–4096), bit-exact round-trip; 20/32-bit encode is
  intentionally unsupported.
- PCM→DSD exists: CIFB order-5 delta-sigma modulator,
  DSD64/128/256/512, THD+N −84.9 dB (DSD64→44.1k); not
  realtime (DSD128 0.39x, DSD512 0.12x on this VM). STF droop
  ≈0.87 (−1.2 dB) at 18 kHz is documented, unfixed. DSF 1–7
  channels, DFF mono/stereo only; input clamp ±0.75.
- Opus CELT sample reconstruction exists and is verified vs
  ffmpeg (RMS ~1e-5 on CELT-only fullband stereo 20 ms
  frames); SILK/Hybrid stay `Unsupported`. Only 20 ms
  fullband stereo frames were verified end-to-end; 2.5/5/10
  ms frames, mono, and non-fullband end-bands follow the
  same code paths but have no fixture in this round.
- VP9 keyframes decode bit-exactly (max_diff 0 on every
  fixture); inter frames are NOT pixel-correct and `Decoder`
  returns `Unsupported` for them instead of wrong pixels.
  Tile rows untested (ffmpeg/libvpx ignores `-tile-rows`).
- AV1 produces no pixels yet: tile decode + intra
  reconstruction exist, but the coefficient CDF tables
  (spec §9.3, ~15k values) are missing. Which `Unsupported`
  a stream hits first depends on the stream: a default
  libaom encode (CDEF on) stops at CDEF index decode
  (`"av1 cdef index decode is not implemented in this
  round"` — the `av1_64x64.ivf` Decoder test asserts this);
  a CDEF-off stream reaches the tile walk's end and the
  pipeline returns `Unsupported(COEFFICIENT_GAP)` (asserted
  in `av1_reconstruct.rs`). Intra block copy is not
  implemented (the 320×240/640×480 fixtures use it); the
  loop filter, CDEF, and loop restoration are not
  implemented; the ADST8/16 spec-factorization discrepancy
  is open (see the AV1 subset section). The tile-area parser
  bug is fixed (see below).

## Revision round (2026-11-08): license gate, resolution, pool, scale, quality, dither

### License gate — behaviour and error rules

H.264/AVC, H.265/HEVC, and AAC are patent-encumbered. The library
contains no software encoder or decoder for any of them, and the
software `Decoder` / `Encoder` refuse `Codec::Avc` and
`Codec::Hevc` outright. The only path is the hardware path in
`hw.rs` (traits `HardwareDecoder` / `HardwareEncoder`; the stub
implementations call no real hardware API — a successful stub
decode returns a 0×0 frame, a successful stub encode an empty
packet).

| Situation | Error |
|---|---|
| `licensed-avc-hevc` feature off (the default) | `Unsupported` — the path is not in the build |
| Feature on, decoder/encoder used without a license | `LicenseRequired { codec }` |
| Feature on, stub `open` without a valid license | `LicenseRequired` (or `InvalidArgument` for a malformed license) |
| `LicenseContext::new` with empty licensee/scope, or an id outside 8–64 ASCII letters/digits/`-` | `InvalidArgument` |
| `validate()` on a well-formed but expired license | `LicenseRequired` (expiry checked against `SystemTime`, chrono-free) |
| `decode` / `encode` on a stub that was never opened | `LicenseRequired` |

`LicenseContext::validate` records every check in an in-memory
audit log (`audit_log()`); the license id is stored masked (first
4 characters + `***`), never raw. The gate performs no online
verification, holds no key, and has no bypass. It is a compliance
mechanism only, not a substitute for a real patent license — see
`NOTICE`. `Codec::Aac` exists since the next phase: AAC-LC
software *decode* is implemented behind the `licensed-aac`
feature (feature off → `Unsupported`, feature on without a valid
license → `LicenseRequired`); AAC-LC software *encode* is
implemented behind the same gate since the final round
(`Encoder::new` fails at construction when the gate is closed,
via `EncoderConfig.license`). There is still no AAC hardware
path. C ABI code: `CODEC_ERR_LICENSE_REQUIRED = 8`, mapped by
all five bindings (Python, TypeScript, Kotlin, Swift, Zig).

### Resolution limits — AV1 levels, VP9 profiles

- `FrameSize::new` rejects a zero side (`InvalidArgument`) and a
  side above the library limit 8192 (`Overflow`).
- AV1 (`resolution.rs`, spec Annex A §10.3): 14 defined levels,
  `seq_level_idx` 0, 1, 4, 5, 8, 9, 12–19. Indices 2, 3, 6, 7, 10,
  11 are undefined in the spec and indices above 19 are draft or
  undefined — both are rejected with `InvalidArgument`, never
  mapped to a neighbouring level. `validate_av1_frame` checks
  width, height, picture size, and — when a frame rate is given
  and the level's rate is verified — the display rate
  (width × height × fps). `validate_av1_sequence_header` checks
  a parsed sequence header against its own declared level
  (operating point 0); `minimum_av1_level` returns the smallest
  level whose size caps fit, or `None`.
- **AV1 level 5.2 (`seq_level_idx` 14): `max_display_rate =
  None` — cần xác minh.** Its display rate is unverified
  against the spec table, so only its size caps
  (8,912,896 samples, 8192×4352) are checked; the display-rate
  check is skipped for this level, not guessed.
- VP9 (spec v0.6) has **no level system**: profiles 0–3
  constrain bit depth and chroma subsampling only (profile 0:
  8-bit, 4:2:0; profile 1: 8-bit, 4:2:2/4:4:4; profile 2:
  10/12-bit, 4:2:0; profile 3: 10/12-bit, 4:2:2/4:4:4).
  `validate_vp9_frame` therefore only rejects an unknown
  profile (`InvalidArgument`) and a frame above the library
  8192 limit (`Overflow`); it never invents a per-profile size
  cap. `validate_vp9_frame_header` rejects show-existing frames
  (they carry no dimensions).
- `suggest_av1` / `suggest_vp9` return `EncodingSuggestion {
  bitrate_kbps_min, bitrate_kbps_max, crf }`, computed from
  pixel count (bands assume 30 fps, scaled by the caller's fps).
  These are ladder-style suggestions, not spec values; a caller
  may ignore them and still produce a conformant stream.
- These are header-level checks only. AV1 and VP9 are still
  parser-only: no pixels are reconstructed, and the checks say
  nothing about a decoded frame.

### Frame pool — the 64 MiB cap vs 8K 10-bit (intentional block)

`MAX_ALLOCATION_BYTES` is 64 MiB per decode call (`limits.rs`).
`frame_bytes` / `FramePool::checkout` enforce it on every new
allocation:

- 8K YUV420 8-bit (7680×4320): 49,766,400 bytes (~47.5 MiB) —
  fits, pool checkout succeeds.
- 8K YUV420 10-bit: 99,532,800 bytes (~94.9 MiB, two bytes per
  sample) — exceeds the cap, and the pool returns
  `AllocationLimit` **by design**. The cap was deliberately not
  raised in this round; a caller that needs 10-bit 8K must raise
  `MAX_ALLOCATION_BYTES` in `limits.rs` as a separate, explicit
  decision. The pool itself caps its total at `max_frames` × 64
  MiB (`FramePool::new`); `with_total_bytes` sets an explicit
  total cap, but the per-frame cap still applies. A checked-in
  frame keeps its old pixel data until the caller overwrites it.

### Scaling

`scale_frame` (`scaler.rs`) scales a `VideoFrame` to any
non-zero destination up to 8192 per side, with
`ScaleFilter::{Bilinear, Bicubic, Lanczos3}` — all three
implemented for real (Lanczos-3 is sinc×sinc, 6-tap, with the
kernel widened on downscale against aliasing). YUV420 chroma
planes scale at half resolution (rounded up); packed formats
scale per channel. The input frame is not modified; colour
space, range, and timestamp carry over. There is still no
pixel-format conversion (`pixel.rs` does not exist): scaling
keeps the frame's `PixelFormat`.

### Passthrough AVC/HEVC/AAC (next phase)

The demux no longer rejects licensed tracks. MP4 `avc1` /
`hev1` / `mp4a` and Matroska `V_MPEG4/ISO/AVC`,
`V_MPEGH/ISO/HEVC`, `A_AAC` are parsed as passthrough tracks:
the sample entry is read, `codec_private` (`avcC` / `hvcC` /
`esds`) is kept, and packets are produced byte-identical —
no software decode or encode of AVC/HEVC ever happens, and
AAC packets are only decoded by the licensed AAC software
decoder behind its own gate (see below). The demuxer holds
an `Option<LicenseContext>`; `set_license_context` validates
eagerly (expired license is rejected at set time, not at
first packet). Parsing succeeds without a license (tracks are
listed, `codec_private` is kept); only `next_packet` refuses
licensed packets — `LicenseRequired` with the features on,
`Unsupported` with them off. `hvc1` (HEVC with the parameter
sets in the sample entry) is not accepted; only `hev1` is.
Any other sample entry / codec id still returns `Unsupported`
at track parse time.

### Quality labels, dither, I32 PCM

- Quality labels (`quality.rs`) are commercial labels computed
  from measured stream parameters, not formats and not codec
  names: `classify_quality(QualityParams { lossless,
  bits_per_sample, sample_rate })` returns `Lossless` for
  lossless at most 16-bit / 48 kHz, `HiResLossless` for lossless
  above that and at most 24-bit / 192 kHz, and `None` for lossy
  streams, for lossless beyond 24-bit / 192 kHz, and for zero
  bit depth or sample rate. A label says nothing about which
  codec produced the stream.
- Dither (`dither.rs`): `quantize_i24_to_i16` applies TPDF
  noise (one output LSB, seeded xorshift64 written here, no
  `rand` dependency; equal seeds give equal output) and
  optional first-order error-feedback noise shaping
  (`DitherOptions { dither, noise_shaping, seed }`).
  `quantize_f32_to_i16` goes through the 24-bit path, so
  callers holding original 24-bit samples should prefer the
  i24 entry point — dithering after the f32 pre-rounding cannot
  recover bits already lost. The noise generator is a
  statistical source, not a cryptographic one.
- I32 PCM (`pcm.rs`): `PcmFormat::I32` with bit-exact
  `decode_i32_samples` / `encode_i32_samples`. The f32 pipeline
  (`decode_samples` / `encode_samples` with `I32`) rounds i32
  values to 24 significant bits and cannot carry i32 exactly;
  use the i32 pair when bit-exactness matters. WAV tag (1, 32)
  maps to `I32` in `wav_header.rs`; tag (3, 32) is `F32`.

### Benchmark note

The 8K scale + pool benchmark added to
`crates/codec-whatttttttttttt-core/examples/bench.rs` is a
manual run (`cargo run --release --example bench`); the CI rust
job does not run benchmarks.

## Next phase (2026-11-08/09): passthrough, MP3/AAC/ALAC, bitrate, resampler, DSD

### MP3

- Layer III decode only. Layer I/II decode is `Unsupported`
  (the `sine_stereo_44100_layer2.mp2` fixture asserts this);
  there is no Layer I/II parser beyond the shared frame
  header.
- Encode is CBR only, since the final round: MPEG-1
  64/96/128/320 kbps, MPEG-2/2.5 64/96/128 (320 rejected per
  ISO/IEC 11172-3 Table B.1). VBR/ABR are `Unsupported` by
  design. Long blocks only, plain stereo, no bit reservoir;
  one frame of encoder delay (see `mp3_encode.rs`).
  Round-trip RMS ~0.005–0.007; ffmpeg cross-check RMS/max
  0.0000.
- Intensity stereo has no verifying fixture: the fixtures
  exercise joint stereo (MS) but no frame with intensity
  stereo was produced, so that path is unmeasured — see
  `tests/mp3.rs`. Do not claim it is verified.
- Full ISO/IEC 11172-4 compliance is measured, not assumed:
  RMS ~1.8e-7–2.7e-7, max ~1.1e-6–1.9e-6 against
  ffmpeg/libmp3lame, bounds RMS < 8.80967e-06 and max <=
  6.10352e-05.

### AAC-LC

- Decode only, behind the `licensed-aac` feature with a valid
  `LicenseContext`. Feature off → `Unsupported`; no license,
  malformed license, or expired license → `LicenseRequired`.
- Encode exists since the final round, behind the same gate:
  `AacEncoder` (CBR 64/96/128/320, ABR, VBR quality 0..=9),
  one scalefactor per frame, no bit reservoir. `Encoder::new`
  fails at construction when the gate is closed (feature off
  → `Unsupported`, no/expired license → `LicenseRequired`).
  Round-trip vs source: stereo 128k RMS 5.1e-4, mono 8k RMS
  3.9e-4; our decoder vs ffmpeg decode of the same stream:
  0.000000 / 0.000000. Only sine fixtures were used; no
  music/speech/noise encode fixture exists yet, and no rate
  other than 44100/8000 Hz was tested.
- HE-AAC (SBR, object type 5), AAC-LD (23), USAC (39) are
  `Unsupported` at config time. LTP is not implemented.
- CCE (coupling channel element) is unverified: ffmpeg does
  not generate CCE streams, so no fixture exercises it, and
  `gain_element_scale` in the coupling path is currently
  ignored — a CCE stream that depends on it would decode at
  the wrong level. Recorded here so the gap is visible.
- TNS is implemented but has no fixture that measures it
  against ffmpeg: no test asserts TNS behaviour on real
  audio yet.
- PNS uses an LCG with ffmpeg's seed (`0x1f2e3d4c`) and
  matches ffmpeg to ~1e-6 on the fixtures; it is a
  deterministic stand-in for the standard's noise generation,
  not a bit-exact PNS implementation.
- Thresholds are measured: worst max abs 7.9e-5 / RMS 3.0e-6
  (burst, 128k, short windows), bounds max abs <= 2.0e-3 /
  RMS <= 5.0e-4.

### ALAC

- Decode only, bit-exact vs ffmpeg (16-bit and 24-bit from
  s32 input). No license gate.
- Encode exists since the final round: `AlacEncoder`
  (16-bit and 24-bit, mono/stereo/multi-channel 1–8, frames
  1–4096), round-trip bit-exact via the library decoder and
  ffmpeg decode of the `.m4a`. 20/32-bit encode is
  intentionally unsupported.
- CCE/PCE (coupling / program config elements) are
  `Unsupported`. Channel configuration 0 (needs a PCE) is
  rejected at config time.
- ffmpeg's ALAC encoder writes 24-bit ALAC for s32 input, so
  the s32 references are the 24-bit samples shifted left by 8;
  tests shift them back before comparing.

### ADTS

- One ADTS header parser exists: `adts_frame.rs` owns it
  (plus CRC-16, the streaming demuxer, and the writer). The
  AAC decoder's `aac_frame::parse_adts_header` is a thin
  wrapper that adds the LC-only check, the channel-count
  mapping, and the frame-fits-in-input check. Deliberate
  behaviour differences from the old duplicated parser: a
  reserved profile (3) is now `InvalidData` from the shared
  parser instead of `Unsupported` (it is a corrupt header,
  not an unsupported flavour); error detail strings and
  offsets for bad fields now come from the shared parser
  (same `Error` variants); and a frame longer than
  `MAX_AUDIO_PACKET_BYTES` is now `AllocationLimit`, matching
  the rest of the library.
- `split_adts_frames` (one-shot splitter in `aac_frame.rs`)
  and `AdtsDemuxer` (streaming, in `adts_frame.rs`) are two
  APIs over the same parser, not two parsers.

### Containers and tags

- MPEG-TS: the demuxer parses every program's PMT (streams
  from all programs are merged, not separated per program)
  and assembles each PES from the consecutive TS packets of
  one PID; one audio PES holds many ADTS frames (ffmpeg packs
  ~15 per PES) and the demuxer splits them out. A continuity
  gap drops the partial PES being assembled (counted in
  `TsStreamStats`, not silently completed). The muxer writes
  one PES per `packet()` call. Stream types AVC/HEVC/AAC and,
  since the final round, MP3 (`0x03`/`0x04`) yield packets;
  any other PMT stream type is listed with `codec: None` and
  skipped.
- AIFF-C: only `NONE` (no compression) and `sowt`
  (little-endian) are supported; any other compression type
  is `Unsupported`.
- ID3v2: the extended header's size field is read and
  bounds-checked (synchsafe in v2.4, big-endian u32 in
  v2.3) and that many bytes are skipped, but the extended
  header's inner fields (extended flags, CRC) are not
  interpreted.
- Vorbis comment: two parsers coexist on purpose.
  `vorbis_comment.rs` is the strict packet-level API (magic,
  vendor string, comments, framing bit): a malformed packet
  is rejected outright, and it round-trips. The lenient
  header parser in `vorbis_setup.rs` is left alone for
  header parsing during stream setup. Callers that need
  round-tripping use the strict one; callers that only need
  the setup headers use the lenient one.
- MP4 tags: `trkn`/`disk` numbers and `gnre` genre index
  follow the iTunes convention (genre is the ID3v1 index +
  1); cover art (`covr`) is kept raw and never decoded.

### Bitrate and resampler

- `BitrateMode::Vbr(u8)` takes 0..=9 with 0 best (LAME -V
  convention); values above 9 are rejected at validation.
  Opus bitrates validate only at 8/12/16/24/48 kHz.
- The polyphase resampler (`polyphase.rs`) is stateless per
  call: every call treats its input as a whole signal with
  zero padding outside it, so block boundaries carry the
  same edge transient as the signal edges (about
  `(prototype_len - 1) / down` output frames — see
  `edge_transient_in_output_frames()`). Callers that split a
  stream into blocks and need continuity must resample the
  whole stream in one call or keep the overlap themselves.
  `delay_in_input_frames()` is the group delay, not the
  transient length.
- The linear interpolator in `resample.rs` is still the
  default for callers that accept interpolation error; the
  polyphase path is the quality path and costs one
  multiply-accumulate per tap per output sample per channel
  (128 taps/phase, 2048-tap prototype floor).

### DSD

- DSF/DFF demux and DSD-to-PCM decimation only. PCM-to-DSD
  (delta-sigma modulation) is implemented since the final
  round: CIFB order-5 modulator (`dsd_modulator.rs`), DSD64/
  128/256/512, DSF LSB-first / DFF MSB-first, THD+N −84.9 dB
  (sine 1 kHz, amp 0.5, DSD64→44.1k). Not realtime on this VM
  (DSD128 0.39x, DSD512 0.12x, release). STF droop ≈0.87
  (−1.2 dB) at 18 kHz is documented in the module, unfixed
  (needs complex Butterworth poles). DSF 1–7 channels only,
  DFF mono/stereo only; input clamp ±0.75. `Encoder` emits raw
  DSD bytes in DSF layout; `write_dsf` / `write_dff` wrap
  them into containers.
- DoP is DSD64/128 only (176.4/352.8 kHz); DSD256/512 DoP
  is `Unsupported`.
- The 48 kHz-family rational resampling stage keeps its own
  local `RationalResampler` in `dsd_decimator.rs` instead of
  the shared `polyphase.rs`: the shared resampler is
  stateless per call, while DSD decimation is streaming
  (per-channel history across `feed` calls). Routing a long
  DSD stream through the shared resampler would either buffer
  the whole decimated stream (past the 64 MiB per-call cap)
  or seam the zero-padding transient at every block boundary.
  The filter also differs on purpose (on-the-fly Blackman
  windowed sinc, normalised by the weights actually used).
- DSF fixtures are validated against the Sony DSF spec text
  (block size per channel 4096, LSB-first), not against
  ffmpeg: ffmpeg's DSF path returns noise for these files
  (and a DC test file decodes to toggling +/-1), so ffmpeg
  is not a DSF reference. DFF fixtures are cross-checked
  against ffmpeg decode (exactly 0.5 amplitude).
- Benchmark (release, this VM, throttled): DSD128→176.4 kHz
  1.1x realtime, DSD512→352.8 kHz 0.3x realtime. The
  decimator is not yet fast enough for realtime DSD512 on
  this machine.

### Unchanged limits (carried over)

- Opus SILK sample reconstruction is still `Unsupported`
  (CELT reconstruction landed in the final round; see the
  Opus section below). Only packet parsing, headers, silence,
  and PLC work for SILK/Hybrid.
- AV1 produces no pixels yet (coefficient CDF tables missing);
  VP9 inter frames are not pixel-correct. See the AV1/VP9
  subset sections below.
- FLAC 32-bit encode is intentionally not done; 32-bit
  decode converts samples to f32 via i32 (see `CHANGELOG.md`).
- The 64 MiB per-call allocation cap is unchanged, by
  decision (2026-11-08 22:24): an 8K 10-bit frame
  (99,532,800 bytes) is still rejected with
  `AllocationLimit`. An 8K YUV420 8-bit frame (49,766,400
  bytes) fits.

## Phase 10: Bindings — verified 2026-11-08

The Rust core stays the single implementation; every binding is
a thin wrapper over one C ABI (`crates/codec-whatttttttttttt-c/`,
`codec_whatttttttttttt.h`). No binding re-implements decode or
encode logic.

- Order: C ABI + C++ first, then Python, Kotlin, Swift,
  TypeScript (WASM), React Native, Zig.
- The C ABI covers: version, error codes + messages, probe
  (container/codec from the first bytes), audio decode (decoder
  from a WAV header or a FLAC STREAMINFO block, decode one packet
  to an interleaved f32 frame), FLAC encode (one-shot and the
  streaming encoder), and the VP9/AV1 header parsers (the video
  decoders are parser-only, so the bindings expose the parsers,
  not a fake decode). Opaque handles are freed with the matching
  `codec_*_free`; buffers the library allocates are freed with
  `codec_free`. A decoded frame (and an encoded packet) is
  borrowed from its decoder/encoder and stays valid only until
  the next call on the same handle or its free — the caller must
  copy before the next call.
- Each binding has its own README, a runnable example, and its
  own tests, in its own per-language layout (`bindings/<lang>/`).
  A binding is only reported as working when its tests pass in
  this environment; a binding whose toolchain is not installed
  here is written carefully but marked unverified below, and its
  code is never claimed to compile.
- Toolchains in this environment (2026-11-08): gcc/g++ 13.3,
  clang 18.1, Python 3.12, Node 24, cargo 1.99.1. Installed
  during Phase 10: Zig 0.14.1 (`~/workspace/.tools/`), Swift
  6.1.2 (downloaded tarball, `~/workspace/.tools/`), Kotlin
  1.3.31 + OpenJDK 17 (apt), nightly Rust 1.102.0 (for
  cargo-fuzz, Phase 11). Not installed: wasm-pack/wasm-bindgen
  (the WASM binding uses raw exports, no bindgen), Gradle
  (apt offers 4.4.1, too old for the Kotlin 1.3 plugin), an
  Android SDK, Metro, macOS/Xcode.

Verification results (2026-11-08, all re-run after the Phase 11
fixes):

| Binding | Result |
|---|---|
| C ABI (`tests/ffi.rs`) | 9 passed |
| C example (`examples/encode_decode.c`) | runs: one-shot 4097 B FLAC, streaminfo 42 B, decoded 4410 frames, round-trip exact |
| C++ wrapper (`tests/test_cpp.cpp`) | all checks passed |
| Python (`bindings/python/`) | 13 unittest passed; example round-trip exact; ruff check + format clean, mypy clean |
| Kotlin (`bindings/kotlin/`) | 15/15 checks passed (JNI lib built with g++, test jar via `kotlinc` directly); `build.gradle.kts` provided but NOT run — Gradle not installed |
| Swift (`bindings/swift/`) | 10/10 XCTest passed; vendored C header identical to the canonical one (`diff -q` clean) |
| Zig (`bindings/zig/`) | 10/10 tests passed; example runs (probe wav/pcm, decoded 4410 frames) |
| TypeScript WASM (`packages/wasm/` + `packages/core/`) | 4 wasm unit tests (in the workspace count) + 10/10 node:test tests passed; wasm binary ~200 KB |
| React Native (`packages/react-native/`) | TypeScript type-checks clean against real react-native 0.88.1 types (`tsc`, exit 0). **UNVERIFIED: the Android Kotlin module and the iOS Objective-C++ module were never built or run** — no Android SDK, Metro, or macOS/Xcode in this environment. The native code is written carefully and documented as unverified in its README; it must not be claimed to compile. |

## Phase 11: Hardening — verified 2026-11-08

- cargo-fuzz: 9 targets, one per parser (WAV, FLAC, Ogg,
  EBML/Matroska, MP4, AV1 OBU, VP9 bitstream, Opus TOC, Vorbis
  setup), seed corpora under
  `crates/codec-whatttttttttttt-core/fuzz/corpus/`. First 60 s
  run per target: wav 887,567 runs, ogg 4,675,419, ebml 819,319,
  av1 4,696,667, vp9 329,241, vorbis 1,534,411 — all clean.
  Three crashes found: FLAC LPC prediction overflow
  (`flac_frame.rs`, i64 overflow), MP4 `stco`/`co64` allocation
  (~2 GiB from a file-controlled u32 count), OpusHead family-1
  sum overflow (u8 addition, 235 + 43). All three fixed per the
  fix process with regression tests (see `CHANGELOG.md`);
  re-fuzz 60 s each after the fixes (flac 389,475 runs, mp4
  1,707,864, opus 3,899,063) found no further crashes, and the
  three original crash inputs run clean through the rebuilt ASan
  fuzz binaries.
- Property-based tests: `tests/property.rs`, 6 tests (WAV/FLAC/
  Ogg/EBML round-trips, resample identity, FLAC encode/decode
  round-trip with random samples). All pass.
- Benchmarks: internal bench binary
  (`examples/bench.rs`, `Instant`, warmup 3, 10 timed
  iterations, median; no criterion dependency), release build:
  FLAC decode 90 µs for 4410 frames (1101.6x realtime), Vorbis
  decode 190,972 µs for 5824 frames (0.7x realtime), FLAC
  encode 330,222 µs for 220,500 frames (2.7 MB/s in, 201,588
  bytes out). **Vorbis decode is 0.7x realtime: `mdct_inverse`
  is a naive O(N²) double loop with a per-iteration table lookup
  and u64 modulo. This is a performance limitation, not a
  correctness bug; a fast FFT-based MDCT would be a new
  implementation phase, not Phase 11.**
- Sanitizers: the C example and the C++ driver run clean under
  ASan/UBSan (`clang -fsanitize=address,undefined`) against an
  ASan-built static library (target-specific rustflags; global
  `-Zsanitizer=address` fails because proc-macro crates cannot
  be built with sanitizers). Miri: the core lib-test run
  (`cargo +nightly miri test -p codec-whatttttttttttt-core
  --lib`, `MIRIFLAGS="-Zmiri-disable-isolation"`) completed
  2026-11-08: the first run (against the pre-fix code) passed
  235/236, the single failure being the Opus sum check that was
  itself wrong and already fixed (see `CHANGELOG.md`); the
  re-run against the fixed code passed **236/236 in 538.97 s**.
  Miri covers the lib unit tests only (integration tests under
  `tests/` were not run under Miri — that would take several
  times longer and is left to CI).
- CI: `.github/workflows/ci.yml` gains per-binding jobs
  (headers, python, typescript, swift, kotlin, zig) on top of
  the existing `rust` job; `.github/workflows/sanitizer.yml`
  adds fuzz (60 s per target), ASan/UBSan, and Miri. Neither
  workflow has run on GitHub (no git repo, no push); they are
  written, not verified by a CI run.

## VP9 subset (Phase 8 + final round)

The VP9 implementation parses frame headers (uncompressed header
section 6.2 and compressed header section 6.3 of the VP9 Bitstream
and Decoding Process Specification v0.6), superframe indexes
(Appendix B), and tile framing (section 6.4). Since the final
round it also reconstructs pixels: keyframes decode bit-exactly
(max_diff 0 vs ffmpeg on every fixture), while inter frames are
not pixel-correct and `Decoder::decode` returns `Unsupported`
for them instead of wrong pixels. The default probability tables
are ported and verified against libvpx (1728/1728 coef probs,
10/10 scan tables).

Supported (VP9 spec v0.6):

- Frame syntax (section 6.1): uncompressed header, trailing
  zero bits to the byte boundary, compressed header of
  `header_size_in_bytes` bytes, tile data. A `show_existing_frame`
  frame is the short form: header only, no compressed header and
  no tiles.
- Uncompressed header (section 6.2): frame marker (must be 2),
  profile 0–3 (with the reserved zero bit for profile 3),
  `show_existing_frame` with `frame_to_show_map_idx`, frame type,
  show frame, error-resilient mode, frame sync code
  (0x49 0x83 0x42), color config (section 6.2.2: bit depth 8/10/12,
  color space 0–7, color range, subsampling; RGB is rejected for
  profiles 0 and 2 per section 7.2.2), frame size, render size,
  frame size with references (resolved through
  `Vp9ReferenceState`, which tracks the 8 reference slots by
  `refresh_frame_flags`), interpolation filter (section 6.2.7,
  including the `literal_to_type` table), loop filter parameters
  (section 6.2.8, deltas carried across frames and reset by
  `setup_past_independence`), quantization parameters
  (section 6.2.9, including `read_delta_q` and the `lossless`
  flag), segmentation parameters (section 6.2.11, including the
  tree/pred probabilities and the per-segment feature values,
  carried across frames), tile info (section 6.2.13, with the
  min/max log2 tile-column calculation of section 6.2.14),
  `header_size_in_bytes`.
- Signed values use descriptor `s(n)` (section 4.9.2): magnitude
  `f(n)` first, then a sign bit.
- Compressed header (section 6.3): read structurally through
  `Vp9BoolDecoder` (section 9.2): TX mode (section 6.3.1,
  including the `TX_MODE_SELECT` extension) and TX mode probs,
  coefficient probs, skip prob, inter mode probs, interpolation
  filter probs, is-inter probs, frame reference mode and its
  probs, Y mode probs, partition probs, and MV probs. The
  default probability tables are ported (`vp9_token.rs`) and
  verified programmatically against libvpx: 1728/1728 coef
  probs exact (`DEFAULT_COEF_PROBS_{4X4,8X8,16X16,32X32}` vs
  `vp9_entropy.c`), 10/10 scan tables exact (`vp9_scan.c`).
  The compressed header's updated probability values are
  tracked across frames by the reconstruction state; a
  frame-parallel-off stream adapts them (the fp0 fixture
  exercises this and currently over-reads the tile bool
  decoder on frame 1 — see below).
  The subexponential delta (section 6.3.4) follows libvpx
  `vp9_dsubexp.c` (`decode_uniform`): after the three prefix
  bits, a 7-bit value `v` is read; the extra bit is read **only
  when `v >= 65`** (`v + 64` below 65, `(v << 1) - 1 + bit` at
  or above). Reading it unconditionally desynchronises the
  parse on any frame whose updates reach that branch with
  `v < 65` — this was the root cause of the Phase 8 compressed
  header under-consumption (80–796 bits short on larger key
  frames), found by comparing against the libvpx source and
  confirmed by all 7 probe frames passing `exit_bool` after the
  fix. The printed spec text places the extra-bit read
  ambiguously; libvpx is the authority here.
- Bool decoder (section 9.2): `init_bool` (first byte as
  `BoolValue`, range 255, `BoolMaxBits` = 8·sz − 8, marker bit
  `read_bool(128)` must be 0), `read_bool` with the split
  formula and renormalization, `read_literal` as repeated
  `read_bool(128)`, `exit_bool` (remaining padding bits must be
  zero and land exactly at the end of the `sz`-byte segment).
- Tiles (section 6.4): tile grid from `tile_cols_log2` /
  `tile_rows_log2`, tile offsets from `get_tile_offset`
  (section 6.4.1), per-tile data offset and size. A non-last
  tile size is `f(32)`, big-endian (verified empirically: the
  little-endian reading gives an absurd size, the big-endian
  reading gives the exact split). The last tile takes the rest
  of the frame. A tile size that overruns the remaining bytes
  is `Truncated`.
- Superframes (Appendix B): marker `0b110`, size-bytes-minus-1
  (2 bits), frames-minus-1 (3 bits), first index byte must equal
  the last byte, frame sizes are **little-endian** (verified
  empirically: a synthetic superframe built from two real frames
  splits into 2 frames with ffmpeg when sizes are little-endian,
  and is rejected as "Invalid frame size" when big-endian). A
  size table that does not exactly fill the chunk is
  `InvalidData`; a zero-size frame is `InvalidData`.
- `validate_subset()` rejects profile 1–3, bit depth above 8,
  subsampling other than 4:2:0, and RGB color space for decoding;
  parsing itself accepts the wider syntax and reports the values.

Not implemented (returns `Unsupported` or is rejected by
`validate_subset()`):

- Inter-frame pixel reconstruction: partition/block/mode/
  coefficient decode, inverse transform, intra/inter
  prediction, and the loop filter exist and are verified on
  keyframes, but inter frames are not pixel-correct (the
  first inter frame already differs and errors compound
  through the reference chain), so `Decoder::decode` refuses
  them with `Unsupported`. The root cause is not yet isolated
  — the token/transform path is verified correct, so it lies
  in MV prediction, motion compensation, or probability
  adaptation, not in the tables. No reference tool on this VM
  dumps per-block modes/MVs, so the faulty block cannot be
  pinpointed by pixel diffs alone. The 7 inter tests in
  `tests/vp9_reconstruct.rs` are `#[ignore]`d with the reason
  recorded; keyframe tests (11) pass bit-exactly.
- Profiles 1–3, bit depth 10/12, subsampling other than 4:2:0,
  RGB: rejected by the subset check when constructing a decoder.
- The reference-frame-size conformance check of section 7.2.5
  (2·W ≥ refW etc.) is not enforced; only the size resolution
  itself is parsed.
- Tile rows (`tile_rows_log2 > 0`) are untested: no fixture
  covers them (ffmpeg/libvpx ignores `-tile-rows`); the code
  follows the spec only.
- `show_existing_frame` colour metadata comes from the minimal
  show-existing header, not the stored frame (the stored
  buffer carries no colour). Pixels are unaffected.

## FLAC encode (Phase 9 + final round)

- `encode_flac_stream` (one-shot) and `FlacEncoder` (streaming,
  one frame per `encode` call) write 16-bit FLAC: `fLaC` marker,
  STREAMINFO (exact min/max block size and total samples in the
  one-shot path; total 0 and min 0 in the streaming path, MD5
  unset in both), frame headers with CRC-8, subframes, CRC-16
  footer. Frame size is 4096 interchannel samples for full
  frames; the last frame is the remainder (1..=4096).
- Since the final round the encoder also writes 20/24-bit FLAC:
  `encode_flac_frame_wide` / `encode_flac_stream_wide` /
  `FlacEncoder::with_bits_per_sample` / `encode_i32`. The
  20-bit fixture is hand-crafted (ffmpeg cannot emit 20-bit
  FLAC); 24-bit and 5.1-channel decode/encode are verified
  max diff 0 vs ffmpeg. 32-bit encode is intentionally not
  done.
- Subframe choice is by exact encoded-size cost: constant,
  verbatim, fixed predictor orders 0–4, LPC orders 1–8
  (autocorrelation + Levinson-Durbin in f64, 15-bit quantized
  coefficients, shift chosen so the largest coefficient fits).
  Residuals use a single Rice partition (parameters 0–14);
  a subframe whose best Rice cost is not better than verbatim
  is written verbatim. Channels are independent (no mid/side
  decorrelation in Phase 9).
- Round-trip: encode → decode with the Phase 2 decoder is
  byte-identical to the original i16 PCM, verified for sine,
  silence, noise, and ramp vectors, and the encoded files also
  decode with ffmpeg's FLAC decoder to the same PCM.
- `Encoder` supports `Codec::Flac`: one input frame becomes one
  FLAC frame (block size = frame count, 1..=4096; larger frames
  return `InvalidArgument`). `Encoder::flac_streaminfo_bytes`
  returns the `fLaC` + STREAMINFO block to write before the
  frames (total samples 0, streaming form).
- Opus, Vorbis, VP9, and AV1 encode return `Unsupported`:
  Opus encode needs CELT/SILK encoders, Vorbis encode needs
  codebook/floor/residue encoders, VP9/AV1 basic encode needs
  full video encoders. None of them is attempted, and no output
  is fabricated.
- Verified (2026-11-08): the Phase 2 decoder round-trips sine,
  silence, noise, and ramp byte-identically (`tests/flac_encode.rs`,
  22 tests); ffmpeg's FLAC decoder decodes the CLI output to
  byte-identical PCM, including a 88200-frame stereo file; the
  CLI `decode` path decodes the CLI `encode` output
  byte-identically. The encoder's sine output (4094 bytes for
  4410 stereo frames) is smaller than ffmpeg's default encode of
  the same audio (9613 bytes), because LPC predicts a pure sine
  almost perfectly.

## AV1 subset (Phase 7 + final round)

The AV1 implementation parses OBUs, sequence headers, frame headers,
and tile-group framing. Since the final round it also decodes
tiles (partition walk, intra modes, TX sizes) and intra
prediction / inverse transforms / dequant — but it produces no
pixels yet: coefficient decode needs the default coefficient CDF
tables (spec §9.3, ~15k values), which are not ported. A
CDEF-off stream reaches the tile walk's end and the pipeline
returns `Unsupported(COEFFICIENT_GAP)`; a default libaom
encode (CDEF on) stops earlier at CDEF index decode, so
`Decoder::decode` for `Codec::Av1` returns whichever
`Unsupported` its stream hits first.

Supported (AV1 spec v1.0.0):

- OBU headers (section 5.3.2): forbidden bit, type (4 bits),
  extension flag, size field, reserved bit; LEB128 sizes; OBU types
  0–8 and 15 (sequence header, temporal delimiter, frame header,
  tile group, metadata, frame, redundant frame header, tile list,
  padding). Reserved types 9–14 return `Unsupported`.
- Sequence headers (section 5.5.1): profiles 0–2, still and reduced
  still picture headers, timing info, decoder model info, operating
  points (idc, level, tier, initial display delay), frame dimensions
  (max 8192x8192, else `Overflow`), frame id numbers, superblock
  size, filter-intra / intra-edge / interintra / masked / warped /
  dual / order-hint / joint-compound / ref-frame-mvs flags, screen
  content and integer MV forcing (including `SELECT`), order hint
  bits, superres / CDEF / restoration flags, color config (bit depth
  8/10/12, monochrome, primaries/transfer/matrix, color range,
  subsampling, chroma sample position, separate UV delta Q),
  film-grain presence flag. `validate_subset()` additionally rejects
  profile 1 and 2, bit depth above 8, non-4:2:0 subsampling,
  monochrome, and film grain for decoding; parsing itself accepts
  the wider syntax and reports the values.
- Frame headers (section 5.9.2): show_existing_frame (with the
  reference-slot state machine), key / inter / intra-only / switch
  frame types, show / showable flags, error-resilient mode (forced
  to 1 for a shown key frame or a switch frame, per spec), CDF
  update disabling, screen content tools, integer MV forcing
  (forced to 1 for intra frames, per spec), frame size override,
  order hints, primary reference frame, refresh flags, reference
  frame indices (including short signaling and `set_frame_refs`),
  frame size and render size (including super-resolution),
  tile info (uniform and non-uniform spacing), quantization
  (including delta Q and quantization matrices), segmentation
  (including feature tables copied from the primary reference),
  delta Q / delta LF, loop filter, CDEF, loop restoration, TX mode,
  frame reference mode, skip mode, warped motion (forced to 0 for
  intra frames, per spec), global motion parameters (all types,
  with subexponential decoding), and film grain parameters
  (parsed; grain is not applied, and the subset rejects it).
- Tile groups (section 5.11): start/end range, per-tile sizes
  (little-endian, `tile_size_bytes` wide), last tile takes the rest
  of the OBU. Tile offsets are reported relative to the enclosing
  frame OBU payload.
- The AV1 symbol decoder (section 8.2) is implemented as
  `Av1SymbolDecoder` (`av1_frame.rs`): `init_symbol`, `read_symbol`,
  `read_bool`, `exit_symbol`, with CDF adaptation. It is used for
  tile data only; the frame header is parsed with the plain bit
  reader. The Opus range decoder (`range_decoder.rs`, RFC 6716) is
  a different algorithm and is **not** the AV1 symbol decoder; the
  Step 1 design's claim that it covers AV1 is outdated.

Not implemented (returns `Unsupported` or is rejected by
`validate_subset()`):

- Coefficient decode: the blocker for any pixel. Needs the
  ~15k default CDF values (spec §9.3: `Coeff_Base`,
  `Coeff_Br`, `Eob_Pt_16/32/64/128/256/512/1024`, `Eob_Extra`,
  `Coeff_Base_Eob`, `Dc_Sign`, `Txb_Skip`; §9.3 L24144–24854).
  Hand transcription in one round is infeasible and
  error-prone; until it lands, no pixels exist. The pixel
  test in `tests/av1_reconstruct.rs` is `#[ignore]`d.
- Pixel reconstruction beyond coefficients: loop filter
  (§8.8), CDEF, loop restoration application, film grain
  synthesis. The fixtures have the loop filter on
  (`loop_filter_level` [2, 3, 8, 7]), so even after
  coefficients land, frames will differ from ffmpeg until
  the loop filter lands.
- Inter frames: `Unsupported` by design; inter prediction is
  a much larger piece of work.
- Intra block copy: not implemented. The 320×240/640×480
  fixtures have `allow_intrabc = 1` (confirmed by ffmpeg
  `trace_headers`) and a block uses it, so their tile tests
  stay `#[ignore]`d even though the headers now parse (see
  the tile-area fix below).
- Profiles 1 and 2, bit depth 10/12, monochrome, chroma
  subsampling other than 4:2:0, film grain: rejected by the subset
  check when constructing a decoder.
- Multi-tile frames are framed (sizes/offsets parsed); the
  320×240/640×480 headers parse to a single tile, matching
  ffmpeg/dav1d, but the tiles are not decoded to pixels.
- Screen content tools beyond the header flag (intra block copy
  prediction) are not implemented.

Open discrepancies (not verified against the spec text):

- ADST8/16: the spec-verbatim factorization produces a
  DST-IV-family matrix (col 0 = [401, 1189, 1930, 2598, 3165,
  3612, 3919, 4076]); the textbook DST-VII inverse predicts
  different values. ADST4 matches DST-VII exactly; the spec
  (v1.0.0 + Errata 1, validated against libaom) is kept as
  written and the model test is ignored, not deleted. The
  correct arbiter is pixel comparison against ffmpeg/dav1d
  once coefficients exist.
- `has_palette_uv` CDF context = `(PaletteSizeY > 0) ? 1 : 0`
  (flagged in code); `get_palette_color_context` tail from
  pre-compaction notes (head verified); `Round2Signed` as the
  sign-symmetric form (only affects filter-intra/CFL blocks,
  ≤1 per sample if wrong).

Tile-area parser bug, fixed in the final round:
`parse_tile_info` in `av1_frame.rs` used `MAX_TILE_AREA_SB =
4096` with `>> (2 * sb_size)`, giving 1 (64×64 superblocks) /
0 (128×128) and forcing extra tiles, which desynced the
320×240/640×480 headers. Per spec §3 (`MAX_TILE_AREA =
4096*2304` luma samples) and §5.11, the constant is 2304 SB
(64×64) / 576 SB (128×128) via `>> (2 * (sb_size - 6))`;
verified against ffmpeg `trace_headers` + dav1d (1 tile).
A reserved OBU type is returned by the OBU reader as an OBU
(its doc claims `Unsupported` for reserved types, which the
implementation does not do); a packet holding only reserved
OBUs therefore fails as "no frame OBU" (`InvalidData`), which
is reasonable for garbage input but the doc/implementation
mismatch is recorded here rather than fixed in this round.

## WebM / Matroska demux (Phase 6)

- Parses EBML (variable-length integers, signed and unsigned,
  element headers including unknown sizes), Segment, SeekHead
  (validated, positions not used for seeking), Info (timestamp
  scale, duration, muxing/writing app), Tracks (video/audio
  settings, codec ID, codec private, default duration), Cluster,
  BlockGroup / Block, SimpleBlock, Cues (used for seeking), Tags
  (kept as `Metadata`; tag strings have trailing NUL padding
  stripped).
- Lacing: none, Xiph, EBML, and fixed-size. Lace timestamps advance
  by `DefaultDuration` when the track declares one; otherwise all
  laces share the block timestamp. The keyframe flag applies to
  lace 0 only.
- Codecs accepted: `V_AV1`, `V_VP9`, `V_VP8` is rejected with
  `Unsupported` (no VP8 codec in the library), `A_OPUS`,
  `A_VORBIS`, `A_FLAC`, `A_PCM`. Any other codec ID returns
  `Unsupported` naming the codec. DocType must be `webm` or
  `matroska`.
- Timestamps are `(cluster_timestamp + timecode) * scale / 1000`
  in microseconds, computed in `u128`; a negative total is
  `InvalidData`. Audio timestamps are the raw container values:
  the Opus pre-skip (and the Vorbis pre-skip) is **not** applied
  by the demuxer; the decoder drops pre-skip samples.
- `DiscardPadding`, `BlockDuration`, `ReferenceBlock`, and
  `CodecState` are parsed and skipped, not applied. SeekHead
  positions are validated but seeking uses Cues; when Cues are
  absent, seeking falls back to the last video keyframe at or
  before the target found by scanning packets.
- Limits: file 512 MiB, 64 tracks, 1,000,000 samples per track,
  video packet 16 MiB, audio packet 1 MiB, EBML nesting depth 32.
- Single logical stream only: no chained or multiplexed
  Matroska; a second Segment returns `InvalidData`.

## MP4 demux (Phase 6, ISO BMFF)

- Parses boxes iteratively (depth 32): `ftyp`, `free`, `mdat`,
  `moov`, `mvhd`, `tkhd`, `mdhd`, `hdlr`, `minf`, `stbl`, `stsd`,
  `stts`, `stss`, `stsc`, `stsz`, `stco`, `co64`, `ctts`, `elst`,
  `sgpd`, `sbgp` (the last two are skipped by size). Unknown boxes
  are skipped by size, never parsed.
- A box size smaller than its header is `InvalidData`; a size
  overrunning the remaining file is `Truncated`; size 0 means "to
  the end of the parent"; size 1 reads the 64-bit largesize.
- Sample entries: `av01` (`av1C`), `vp09` (`vpcC`, a FullBox —
  the 4 version/flags bytes precede the record), `Opus` (`dOps`),
  `fLaC` (STREAMINFO as codec private). `mp4a` (AAC) and any other
  entry return `Unsupported`.
- Sample tables are walked with full validation: `stsc` first
  chunk must be 1, totals must match `stsz`, `stss` entries must
  be within the sample count, sample offsets must be inside the
  file and inside an `mdat` range. Packets are returned sorted by
  (dts, track index) with pts from `ctts` when present.
- The edit list (`elst`) is parsed and validated, then
  deliberately **not** applied to timestamps: decode times come
  from `stts`, and the Opus decoder drops the pre-skip. Seeking
  uses `stss` over video tracks only (audio packets are all sync
  samples and would pull the seek past the video keyframe).
- FLAC in MP4 (`fLaC` entry) is parsed but **unverified**: ffmpeg
  cannot mux FLAC into MP4, so no test vector exists for that
  path. It is listed here, not claimed as tested.
- Limits: file 512 MiB, 64 tracks, 1,000,000 samples per track,
  video packet 16 MiB, audio packet 1 MiB, box nesting depth 32.

## IVF (Phase 7)

- The 32-byte header is read (DKIF, version, header length,
  fourcc, width, height, timebase denominator then numerator,
  frame count). Frames are walked by their 12-byte headers (size,
  timestamp). The CLI `info` command reports the fourcc,
  resolution, frame rate, declared and actual frame counts, OBU
  type counts, and the sequence header. A temporal unit may hold
  several frames (only the last is shown); the demuxer does not
  split temporal units into frames.

## Opus (Phase 4 + final round)

- Packet parsing, OpusHead/Tags, range decoder, CELT frame-header
  symbols, silence, and packet-loss concealment are implemented.
  CELT sample reconstruction is implemented since the final
  round (`celt_reconstruct.rs`, ported from the RFC 6716
  Appendix A reference, Opus 1.0): band-energy decode, TF
  resolution, bit allocation, PVQ shape decode, denormalisation,
  anti-collapse, inverse MDCT + overlap-add, comb-filter
  postfilter, de-emphasis. Verified vs ffmpeg on CELT-only
  fullband stereo 20 ms frames: sine RMS 1.330e-05, noise
  3.318e-06, music 6.778e-06, speech 3.362e-06 (thresholds RMS
  < 1e-4, max < 1e-3). SILK returns `Unsupported`; no PCM is
  fabricated for it. Only 20 ms fullband stereo frames were
  verified end-to-end; 2.5/5/10 ms frames, mono, and
  non-fullband end-bands follow the same code paths but have no
  fixture in this round. The CWRS `INV_TABLE` latent
  out-of-bounds read (index up to 63 vs a 53-entry table) never
  triggered on the fixtures; the Rust code returns `InvalidData`
  if it ever does. Note: the `mdct.rs` comment claiming the CELT
  IMDCT needs a 1/2 pre-scale is wrong for the backward direction
  as CELT uses it (verified numerically vs the reference: the
  scale is exactly 1.0; pre-scaling would make output 6 dB
  quiet) — the comment is left as-is because the file was out of
  scope for the CELT round; it must be corrected, not the code.
  See `CHANGELOG.md` for the table provenance.

## Vorbis (Phase 5 + final round)

- Decode is complete for floor type 1. Floor type 0 packet
  decode is implemented since the final round, verified on a
  synthetic stream only (no real floor-0 file exists —
  libvorbis has not emitted floor 0 since beta 4): header
  parse + packet decode + hand-computed math, max diff 0 vs an
  independent f64 reference.
