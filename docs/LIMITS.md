# Limits and supported subsets

This file records what the library actually does, per phase. It is
updated whenever a phase lands; a claim here must have a passing
test behind it.

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

## VP9 subset (Phase 8)

The VP9 implementation parses frame headers (uncompressed header
section 6.2 and compressed header section 6.3 of the VP9 Bitstream
and Decoding Process Specification v0.6), superframe indexes
(Appendix B), and tile framing (section 6.4). It does **not**
reconstruct pixels: tile data is framed, never decoded, and
`Decoder::decode` for `Codec::Vp9` returns `Unsupported` naming
the missing reconstruction.

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
  reported fields are `tx_mode`, `reference_mode`, and the count
  of probability updates consumed. The updated probability
  *values* are not tracked: the spec's default probability tables
  are not ported, and the values feed only tile decoding, which
  is not implemented. The parse is verified by the marker bit
  (must be 0) and by the bool decoder never running out of bits
  (`BoolMaxBits` exhaustion is `InvalidData`); the uncompressed
  header values are additionally cross-checked against
  ffmpeg's `trace_headers` output for every test vector frame.
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

- Pixel reconstruction: partition/block/mode/coefficient decode,
  inverse transform, intra/inter prediction, loop filter
  application. No `VideoFrame` is produced for VP9.
- Profiles 1–3, bit depth 10/12, subsampling other than 4:2:0,
  RGB: rejected by the subset check when constructing a decoder.
- The reference-frame-size conformance check of section 7.2.5
  (2·W ≥ refW etc.) is not enforced; only the size resolution
  itself is parsed.
- Multi-tile frames are framed (sizes/offsets parsed) but the
  tiles are not decoded.

## FLAC encode (Phase 9)

- `encode_flac_stream` (one-shot) and `FlacEncoder` (streaming,
  one frame per `encode` call) write 16-bit FLAC: `fLaC` marker,
  STREAMINFO (exact min/max block size and total samples in the
  one-shot path; total 0 and min 0 in the streaming path, MD5
  unset in both), frame headers with CRC-8, subframes, CRC-16
  footer. Frame size is 4096 interchannel samples for full
  frames; the last frame is the remainder (1..=4096).
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

## AV1 subset (Phase 7)

The AV1 implementation parses OBUs, sequence headers, frame headers,
and tile-group framing. It does **not** reconstruct pixels: tile data
is framed, never decoded, and `Decoder::decode` for `Codec::Av1`
returns `Unsupported` naming the missing reconstruction.

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

- Pixel reconstruction: prediction, inverse transform, loop filter
  application, CDEF application, loop restoration application,
  film grain synthesis. No `VideoFrame` is produced for AV1.
- Profiles 1 and 2, bit depth 10/12, monochrome, chroma
  subsampling other than 4:2:0, film grain: rejected by the subset
  check when constructing a decoder.
- Multi-tile frames are framed (sizes/offsets parsed) but the
  tiles are not decoded; there is no multi-tile test vector yet
  (all current vectors are single-tile).
- Screen content tools beyond the header flag (intra block copy
  prediction) are not implemented.

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

## Opus (unchanged from Phase 4)

- Packet parsing, OpusHead/Tags, range decoder, CELT frame-header
  symbols, silence, and packet-loss concealment are implemented.
  CELT sample reconstruction and SILK return `Unsupported`; no
  PCM is fabricated. See `CHANGELOG.md` for the reason (the CELT
  allocation/PVQ tables exist only in the RFC 6716 Appendix A
  reference source).

## Vorbis (unchanged from Phase 5)

- Decode is complete for floor type 1. Floor type 0 parses its
  header but packet decode returns `Unsupported`.
