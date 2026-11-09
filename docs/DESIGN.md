# codec-whatttttttttttt — Design (Step 1)

Status: Confirmed by Hoàng on 2026-11-08 (see section 12).
Version: 0.1.0-design
Date: 2026-11-08
License: MIT OR Apache-2.0 (confirmed 2026-11-08)
Location: local only, `~/workspace/codec-whatttttttttttt/`
GitHub: not pushed. GitHub remains for chat-backup files only, per standing rule from 2026-11-08 00:36.

## 1. Goal and non-goals

### 1.1 Goal

A production-quality, multi-platform codec library for open formats only:

- Audio decode first, encode second: PCM/WAV, FLAC, Opus, Vorbis.
- Video decode first, encode basic second: VP9, AV1.
- Container demux/mux: WAV, Ogg, WebM/Matroska, MP4.
- High-level API for frame-based and streaming decode/encode, seek, metadata, resample, pixel-format conversion (YUV420/RGB).

Core in Rust. Thin bindings call the core through a C ABI. No logic duplicated in bindings.

### 1.2 Non-goals for v0.1–v1.0

- No patented codecs in software: no H.264, H.265, or AAC
  encode in software in this library, and no H.264/H.265
  software decode. H.264/H.265 exist only behind the license
  gate added in the 2026-11-08 revision round: Cargo features
  `licensed-avc-hevc` / `licensed-aac`, off by default,
  hardware path only (`hw.rs`), never software decode or
  encode. AAC-LC software decode landed in the next phase
  behind `licensed-aac`; MP3 Layer III and ALAC software
  decode also landed in the next phase (MP3 encode and ALAC
  encode are still not implemented). See `NOTICE` and
  section 3.8.
- No DRM, no network streaming protocol, no player UI, no capture.
- No hardware encoder/decoder abstraction in v0.1, except the
  license-gated hardware path for AVC/HEVC added in the
  2026-11-08 revision round (`hw.rs`: traits plus a stub that
  calls no real hardware API). Software decode first.
- No full AV1 or VP9 encoder in v0.1. Basic encode only, after decode is verified.
- No promise of real-time performance on all devices until benchmarks exist.

### 1.3 Honest scope note

A from-scratch, bit-exact AV1 decoder is a multi-year effort in the reference implementations. This design therefore phases video strictly:

- v0.1: WAV/PCM + FLAC decode, Ogg page parsing, container framing.
- v0.2: Opus decode, Vorbis decode.
- v0.3: AV1 OBU parsing and decode of a defined subset, checked against libaom test vectors (AV1 before VP9, confirmed 2026-11-08).
- v0.4: VP9 bitstream parsing and decode of a defined subset, checked against libvpx test vectors.
- v0.5: Basic encode for PCM/FLAC/Opus/VP9/AV1, round-trip only where the spec defines it.

No stage claims bit-exactness until the corresponding conformance test passes. Any section that is parser-only is labelled parser-only.

## 2. Repository layout

Cargo workspace at the root. Bindings live beside the core, each in its own ecosystem layout. No empty directories. No `utils`, `helpers`, `common`, `manager`, `handler`, `service` as module or file names.

```
codec-whatttttttttttt/
  Cargo.toml                 # workspace
  Cargo.lock
  README.md
  NOTICE                     # license-gate and patent-status notes (revision round 2026-11-08)
  LICENSE-MIT
  LICENSE-APACHE
  CHANGELOG.md
  CONTRIBUTING.md
  .gitignore
  .editorconfig
  rustfmt.toml
  clippy.toml
  crates/
    codec-whatttttttttttt-core/
      Cargo.toml
      src/lib.rs
      src/bitreader.rs
      src/range_decoder.rs
      src/huffman.rs
      src/mdct.rs
      src/ogg_page.rs
      src/ebml.rs
      src/flac_frame.rs
      src/opus_packet.rs
      src/vorbis_setup.rs
      src/vp9_bitstream.rs
      src/av1_obu.rs
      src/pcm.rs
      src/resample.rs
      src/license.rs         # license gate (revision round 2026-11-08)
      src/hw.rs              # hardware path traits + stub (revision round 2026-11-08)
      src/resolution.rs      # AV1 level / VP9 profile limits, suggestions (revision round)
      src/frame_pool.rs      # reusable frame buffers (revision round)
      src/scaler.rs          # frame scaling (revision round)
      src/quality.rs         # Lossless / Hi-Res Lossless labels (revision round)
      src/dither.rs          # TPDF dither, noise shaping (revision round)
      src/error.rs
      src/limits.rs
      src/frame.rs           # Frame/Packet/Metadata/PixelFormat (no separate metadata.rs)
      tests/                 # integration tests, incl. tests/data/ vectors
      examples/              # bench.rs and the decode examples (no benches/ directory)
      fuzz/                  # cargo-fuzz targets (standalone crate, not in the workspace)
    codec-whatttttttttttt-c/
      Cargo.toml
      src/lib.rs
      include/codec_whatttttttttttt.h
      include/codec_whatttttttttttt.hpp   # C++ RAII wrapper (no separate bindings/cpp/)
    codec-whatttttttttttt-wasm/
      Cargo.toml
      src/lib.rs
    codec-whatttttttttttt-cli/
      Cargo.toml
      src/main.rs
  bindings/
    kotlin/
      build.gradle.kts
      settings.gradle.kts
      src/main/kotlin/dev/codecwhat/Codec.kt
      src/main/kotlin/dev/codecwhat/Decoder.kt
      src/main/kotlin/dev/codecwhat/Encoder.kt
      src/main/cpp/jni_bridge.cpp
      src/test/kotlin/dev/codecwhat/DecoderTest.kt
    swift/
      Package.swift
      Sources/CodecWhat/Decoder.swift
      Sources/CodecWhat/Encoder.swift
      Sources/CCodecWhat/module.modulemap
      Tests/CodecWhatTests/DecoderTests.swift
    typescript/
      packages/core/
        package.json
        tsconfig.json
        src/decoder.ts
        src/encoder.ts
        src/types.ts
        __tests__/decoder.test.ts
      packages/react-native/
        package.json
        src/index.ts
        src/NativeCodecWhat.ts
        android/src/main/java/dev/codecwhat/CodecWhatModule.kt
        ios/CodecWhat.mm
      packages/wasm/
        package.json
        src/index.ts
    python/
      pyproject.toml
      codec_whatttttttttttt/__init__.py
      codec_whatttttttttttt/_ffi.py
      codec_whatttttttttttt/__init__.pyi
      tests/test_codec.py
    zig/
      build.zig
      build.zig.zon
      src/decoder.zig
      examples/decode_wav.zig
  docs/
    DESIGN.md                # this file
    LIMITS.md
  .github/workflows/
    ci.yml
    sanitizer.yml            # fuzz + ASan/UBSan + Miri (no separate fuzz.yml)
```

Layout corrections (2026-11-08 revision round — the tree above
now matches the repository; the earlier tree promised files
that do not exist):

- `src/pixel.rs`, `src/metadata.rs`, and `src/entropy.rs` do
  not exist and never landed. `Metadata` / `MetadataEntry`
  live in `src/frame.rs`; pixel-format conversion was never
  implemented (only the `PixelFormat` enum in `frame.rs`);
  entropy coding lives in `huffman.rs` and `range_decoder.rs`
  (plus the AV1 symbol decoder in `av1_frame.rs`).
- The FFI crate is `codec-whatttttttttttt-c`, not
  `codec-whatttttttttttt-ffi`, and there is no `cbindgen.toml`:
  the header is maintained by hand (`include/
  codec_whatttttttttttt.h`), and the C++ wrapper is
  `include/codec_whatttttttttttt.hpp` in the same crate —
  there is no `bindings/cpp/` directory.
- `docs/API.md`, `docs/BUILD.md`, and `docs/TEST_COVERAGE.md`
  do not exist; they were planned in section 10 and are still
  unwritten. The docs that exist are `DESIGN.md` and
  `LIMITS.md`, plus the per-binding READMEs.
- Test vectors live in
  `crates/codec-whatttttttttttt-core/tests/data/`, not in a
  root `tests/` directory. Examples live in
  `crates/codec-whatttttttttttt-core/examples/`; there is no
  root `examples/` and no `benches/` directory — the benchmark
  is `examples/bench.rs` inside the core crate, run by hand.
- The WASM crate `codec-whatttttttttttt-wasm` was added in
  Phase 10 and was missing from the earlier tree.

Crate names:

- `codec-whatttttttttttt-core`
- `codec-whatttttttttttt-c` (the FFI crate; the Step 1 name
  `codec-whatttttttttttt-ffi` was never used)
- `codec-whatttttttttttt-wasm` (added in Phase 10)
- `codec-whatttttttttttt-cli`

Rust import name for the core crate is `codec_whatttttttttttt_core` (hyphens become underscores). C header is `codec_whatttttttttttt.h`. C++ namespace is `codec_whatttttttttttt`. Python package is `codec_whatttttttttttt`. Swift module is `CodecWhat`. Kotlin package is `dev.codecwhat`. TypeScript package is `@codec-whatttttttttttt/core`. These spellings are fixed in this design so bindings do not drift.

## 3. Modules in the core

The core is split by trust boundary and by spec domain, not by language convenience.

### 3.1 `bitreader`

Bit-level reader over a bounded slice. No I/O here. All reads are checked: `read_bits(n)` fails with `Truncated` if fewer than `n` bits remain, fails with `InvalidData` if `n > 32` for `u32` reads. No unchecked indexing. Used by FLAC, Vorbis, Opus, VP9, AV1, EBML variable-length integers.

Public surface (internal to crate unless a test needs it):

```rust
pub struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self;
    pub fn remaining_bits(&self) -> usize;
    pub fn read_bits(&mut self, n: u32) -> Result<u32, Error>;
    pub fn read_bit(&mut self) -> Result<bool, Error>;
    pub fn read_u8(&mut self) -> Result<u8, Error>;
    pub fn read_leb128(&mut self, max_bytes: usize) -> Result<u64, Error>;
    pub fn align_to_byte(&mut self);
}
```

Why a separate module: every parser depends on the same bound checks. A bug here affects every codec, so it gets its own property tests and fuzz target.

### 3.2 `huffman` and `range_decoder`

Canonical Huffman table construction and decoding, and range decoding for VP9/AV1. There is no `entropy.rs`: the entropy work lives in `huffman.rs` and `range_decoder.rs` (plus the AV1 symbol decoder in `av1_frame.rs`).

- `huffman.rs`: build from code lengths per RFC 9639 section 9.5 and Vorbis codebook setup. Rejects over-subscribed and incomplete tables unless the spec explicitly permits incompleteness for that codec. Maximum code length is enforced per codec.
- `range_decoder.rs`: boolean decoder for VP9 (VP9 spec section 8.2) and AV1 symbol decoder (AV1 spec section 8.2). State is explicit, no global state. `read_symbol` returns `InvalidData` on exhausted input rather than looping.

### 3.3 `mdct`

Modified discrete cosine transform, fixed-point where the spec requires it, float where the spec permits it. Used by Vorbis and Opus (CELT mode). The module exposes only:

```rust
pub fn mdct_forward(input: &[f32], output: &mut [f32]) -> Result<(), Error>;
pub fn mdct_inverse(input: &[f32], output: &mut [f32], window: &[f32]) -> Result<(), Error>;
```

Window selection and overlap-add live in the codec modules, not here. This keeps MDCT testable against known vectors without codec state.

### 3.4 `pcm`, `resample`, and pixel formats

- `pcm.rs`: PCM sample formats `I16`, `I24`, `I32`, `F32`.
  Conversion is saturating, documented per format. No silent
  clipping beyond saturation, and saturation is counted only in
  diagnostics, not hidden. `I32` (revision round 2026-11-08)
  has a bit-exact `decode_i32_samples` / `encode_i32_samples`
  pair, because the f32 pipeline rounds i32 to 24 significant
  bits.
- `resample.rs`: sample-rate conversion. Phase 1 provides linear interpolation only; the polyphase FIR resampler for the frame path is a later phase and is not claimed until it exists. Cutoff, taps, and delay are documented constants, not magic numbers.
- Pixel conversion: **not implemented**. There is no `pixel.rs`
  (see section 2, layout corrections). Only the `PixelFormat`
  enum (`Yuv420`, `Rgb24`, `Rgba32`) exists, in `frame.rs`.
  The Step 1 plan for YUV420↔RGB conversion (BT.601 limited
  range by default, BT.709 full range as an explicit option) is
  still a plan, not code. Frame *scaling* is implemented
  separately in `scaler.rs` (section 3.8); it does not change
  colour space.

### 3.5 Container parsing

- `ogg_page.rs`: Ogg page header, segment table, CRC32 check (RFC 3533). A page with a bad CRC is `InvalidData`, not skipped silently. Maximum page size is bounded by `limits.rs`.
- `ebml.rs`: EBML variable-length integer, element header, master element bounds. Used by WebM/Matroska. Unknown elements are skipped by size, with a size check against the remaining buffer. A size that exceeds the parent element is `InvalidData`.
- `wav.rs` (in `container` grouping, file `wav_header.rs`): RIFF/WAVE header, `fmt ` chunk, `data` chunk, extensible format. Declared `data` size larger than the file is `Truncated`.
- `mp4_box.rs`: ISO BMFF box header (`size`, `largesize`, `type`). Box size smaller than the header is `InvalidData`. Traversal is iterative, not recursive, with a depth limit. This prevents stack exhaustion on crafted files.

Container modules return `Packet` values, not decoded frames. Demuxing and decoding are separate steps so a corrupt packet does not corrupt decoder state.

### 3.6 Codec modules

Each codec module owns its own state machine and its own limits. Shared code lives only in `bitreader`, `huffman`, `range_decoder`, `mdct`, `entropy`.

- `flac_frame.rs`: FLAC frame header, subframe decode (constant, verbatim, fixed predictor, LPC), CRC-8 and CRC-16 checks. Spec: RFC 9639. Decode before encode. Encode in a later phase reuses the same frame header writer.
- `opus_packet.rs`: Opus TOC byte, frame packing, CELT and SILK mode dispatch, PLC on packet loss is an explicit API, not implicit concealment. Spec: RFC 6716, RFC 8251 for Ogg mapping.
- `vorbis_setup.rs`: Vorbis identification, comment, and setup headers, codebooks, floors, residues. Spec: Vorbis I specification. Decode before encode.
- `vp9_bitstream.rs`: VP9 uncompressed header, compressed header, tile parsing. Spec: VP9 Bitstream and Decoding Process specification. Decode subset is defined in section 9.
- `av1_obu.rs`: AV1 OBU header, sequence header OBU, temporal delimiter, frame header OBU. Spec: AV1 Bitstream and Decoding Process specification. Decode subset is defined in section 9.

### 3.7 `byte_source`, `metadata`, `limits`, `error`

- `byte_source.rs`: a minimal `Read` adapter (`BoundedReader`) that turns a streaming byte source into bounded slices for parsers. It never allocates an unbounded buffer. The caller sets a maximum buffered size. This is the module the Step 1 design called `io`, renamed before implementation per section 13 item 1.
- `frame.rs`: `Frame` / `Packet` / `Metadata` / `PixelFormat`.
  There is no separate `metadata.rs`: `Metadata` and
  `MetadataEntry` live here. There is no `pixel.rs`: pixel
  conversion was never implemented, only the `PixelFormat`
  enum (`Yuv420`, `Rgb24`, `Rgba32`).
- `limits.rs`: single place for all hard limits (see section 6).
- `error.rs`: the error enum (see section 5).

### 3.8 Revision-round modules (2026-11-08): license gate, resolution, pool, scale, quality, dither

- `license.rs`: `LicenseContext { licensee, license_type:
  LicenseType::{Commercial, Evaluation, Internal}, license_id,
  expires: u64, scope }` plus an in-memory audit log
  (`AuditRecord { timestamp_secs, codec, license_id_masked,
  granted, reason }`, ids masked to the first 4 characters +
  `***`). `new()` rejects an empty licensee/scope or a
  malformed id (8–64 ASCII letters, digits, or `-`) with
  `InvalidArgument`. `validate(codec)` re-checks the format,
  then expiry against `SystemTime` (chrono-free); malformed →
  `InvalidArgument`, expired → `LicenseRequired`. No network
  call, no hard-coded key, no bypass. The gate is a compliance
  mechanism only; see `NOTICE`.
- `hw.rs`: the hardware path for licensed codecs. Traits
  `HardwareDecoder` / `HardwareEncoder` (`codec()`, `open(&mut
  LicenseContext)`, `decode` / `encode`), `StubHardwareDecoder`
  / `StubHardwareEncoder` (closed until `open` succeeds;
  successful decode returns a 0×0 frame, successful encode an
  empty packet — neither calls a real hardware API), and
  `licensed_software_path_error` /
  `licensed_feature_enabled`. Error rule: feature off →
  `Unsupported`, feature on without a valid license →
  `LicenseRequired`. `Codec::Aac` exists since the next phase
  (AAC-LC software decode behind `licensed-aac`); there is
  still no AAC hardware path and no AAC software encoder.
  Features `licensed-avc-hevc` / `licensed-aac`
  are off by default (`default = []` in the core `Cargo.toml`).
- `resolution.rs`: `FrameSize` (non-zero sides, at most 8192),
  `Av1Level` with the 14 defined levels of AV1 spec Annex A
  §10.3, `av1_level` / `validate_av1_frame` /
  `validate_av1_sequence_header` / `minimum_av1_level`,
  `Vp9ProfileLimits` / `vp9_profile_limits` /
  `validate_vp9_frame` / `validate_vp9_frame_header` (VP9 spec
  v0.6 has no level system: profiles 0–3 constrain bit depth
  and chroma subsampling only; frame size is bounded by the
  library 8192 limit), and `suggest_av1` / `suggest_vp9`
  returning `EncodingSuggestion { bitrate_kbps_min,
  bitrate_kbps_max, crf }` — suggestions computed from pixel
  count, not spec values. Level 5.2 (`seq_level_idx` 14) has
  `max_display_rate = None`: its display rate is unverified
  against the spec table and is not checked — cần xác minh.
- `frame_pool.rs`: `FramePool` (`new(max_frames)` /
  `with_total_bytes`, `checkout` / `checkin`, `stats()` →
  `FramePoolStats`), `frame_bytes`, `plane_bytes`,
  `bytes_per_sample` (8-bit: 1 byte/sample; 10/12-bit: 2).
  Checkout reuses an idle frame of the same shape or allocates
  a fresh one, subject to the 64 MiB per-decode-call cap
  (`MAX_ALLOCATION_BYTES`): an 8K YUV420 8-bit frame is
  49,766,400 bytes and fits; 10-bit 8K is 99,532,800 bytes and
  returns `AllocationLimit` by design — the cap was deliberately
  not raised in this round.
- `scaler.rs`: `ScaleFilter::{Bilinear, Bicubic, Lanczos3}`
  and `scale_frame`. All three filters are implemented for
  real: bilinear (2-tap tent), bicubic Catmull-Rom (4-tap),
  Lanczos-3 (sinc×sinc, 6-tap); the kernel is widened on
  downscale against aliasing, weights are renormalised, and
  taps are clamped to the plane bounds. YUV420 chroma scales
  at half resolution; colour space, range, and timestamp carry
  over. The input frame is not modified.
- `quality.rs`: `QualityParams { lossless, bits_per_sample,
  sample_rate }` and `classify_quality() ->
  Option<QualityLabel::{Lossless, HiResLossless}>`. Lossless is
  lossless at most 16-bit / 48 kHz; Hi-Res Lossless is lossless
  above that, at most 24-bit / 192 kHz; anything else is `None`.
  The labels are commercial labels computed from measured
  parameters, not formats and not codec names.
- `dither.rs`: `DitherOptions { dither, noise_shaping, seed }`,
  `quantize_i24_to_i16`, `quantize_f32_to_i16` (via the 24-bit
  path, so callers holding original 24-bit samples should
  prefer the i24 entry point). TPDF noise comes from a seeded
  xorshift64 written here (no `rand` dependency; equal seeds
  give equal output; seed 0 falls back to a fixed non-zero
  constant); noise shaping is first-order error feedback.
  `pcm.rs` also gains `PcmFormat::I32` with bit-exact
  `decode_i32_samples` / `encode_i32_samples`, and
  `wav_header.rs` maps WAV tag (1, 32) to `I32` — the f32
  pipeline rounds i32 to 24 significant bits and cannot carry
  i32 exactly.

### 3.9 Next-phase modules (2026-11-08/09): passthrough, MP3/AAC/ALAC, bitrate, resampler, DSD

- `mpeg_ts.rs`: MPEG-TS demux/mux (`TsDemuxer`, `TsMuxer`,
  `TsStreamStats`). Parses PAT/PMT (every program's PMT),
  assembles PES per PID, reads PCR, checks continuity
  counters; a continuity gap drops the partial PES. AVC/HEVC/
  AAC stream types yield packets as passthrough behind the
  license gate; MP3 stream types 0x03/0x04 yield packets
  since the final round. The muxer writes PAT/PMT once,
  then one PES per `packet()` call.
- `adts_frame.rs`: the single ADTS header parser in the
  crate (`parse_adts_header`, `verify_adts_frame`,
  `AdtsDemuxer`, `write_adts_frame`, `adts_crc16`,
  `AdtsHeader` / `AdtsFrame`). `aac_frame.rs` delegates to
  it (see below).
- `aiff_chunk.rs`: AIFF/AIFF-C demux/mux (`parse_aiff`,
  `AiffDemuxer`, `write_aiff`, `write_extended_float`).
  COMM/SSND/MARK/COMT/FVER; AIFF-C `NONE`/`sowt` only.
- `id3_tag.rs`: ID3v1/v2 read/write (`parse_id3`,
  `parse_id3v1`, `write_id3v1`, `write_id3v2`, `Id3Tag`).
- `mp4_tag.rs`: MP4 `meta`/`ilst` tags (`parse_mp4_tags`,
  `parse_mp4_metadata`); text, integer, track/disk, genre,
  cover art (kept raw).
- `vorbis_comment.rs`: strict Vorbis comment packet parse /
  write with case-insensitive key lookup. The lenient header
  parser in `vorbis_setup.rs` is untouched.
- `mp3_frame.rs`, `mp3_huffman.rs`, `mp3_synth.rs`,
  `mp3_tables.rs`: MP3 Layer III decode (`Mp3FrameHeader`,
  `Mp3Decoder`). Tables are from minimp3 (CC0). Layer I/II
  and intensity stereo are not verified (see
  `docs/LIMITS.md`); CBR encode is in `mp3_encode.rs` (final
  round), VBR/ABR are `Unsupported`.
- `aac_frame.rs`, `aac_huffman.rs`, `aac_tables.rs`: AAC-LC
  raw-data-block decode (`AacDecoder`, `AacConfig`,
  `parse_audio_specific_config`). `parse_adts_header` is a
  thin wrapper over `adts_frame::parse_adts_header` that
  adds the LC-only check, the channel-count mapping, and
  the frame-fits-in-input check. CCE is unverified, TNS has
  no measured fixture, HE-AAC/SBR/LTP are `Unsupported`.
- `alac_frame.rs`, `alac_predictor.rs`: ALAC decode
  (`decode_alac_frame`, `decode_alac_audio_frame`,
  `parse_alac_config`), bit-exact vs ffmpeg. No license
  gate. CCE/PCE are `Unsupported`.
- `bitrate.rs`: `BitrateMode::{Cbr, Vbr(u8), Abr}` and
  per-codec validation (`BitrateCodec::from_codec`,
  `validate_for`); Opus only at 8/12/16/24/48 kHz.
- `polyphase.rs`: windowed-sinc polyphase resampler
  (`PolyphaseResampler`, `resample_polyphase`). Kaiser
  window (A = 90 dB), 128 taps/phase, 2048-tap prototype
  floor. Stateless per call; see `docs/LIMITS.md` for the
  edge-transient rule. `resample.rs` gains `ResampleMethod`
  (linear stays the default).
- `dsd_stream.rs`, `dsd_decimator.rs`,
  `dsd_coefficients.rs`, `dop.rs`: DSF/DFF demux
  (`DsdDemuxer`, `DsdHeader`), DSD-to-PCM decimation
  (`DsdDecimator`, `DsdLevel`), decimation/resample taps,
  and DoP packing (`pack_dop`, `unpack_dop`). DoP is
  DSD64/128 only. PCM-to-DSD is in `dsd_modulator.rs`
  (final round).
- Passthrough (in `mp4_box.rs`, `matroska.rs`,
  `mpeg_ts.rs`, `demux.rs`, `hw.rs`): MP4 `avc1`/`hev1`/
  `mp4a` and Matroska `V_MPEG4/ISO/AVC`,
  `V_MPEGH/ISO/HEVC`, `A_AAC` parse as passthrough tracks
  with `codec_private` kept; the demuxer holds an
  `Option<LicenseContext>` validated eagerly by
  `set_license_context`, and only `next_packet` refuses
  licensed packets (`hw::passthrough_allowed`).

### 3.10 Final-round modules (2026-11-09): small items, encoders, PCM→DSD, CELT, VP9, AV1

- `flac_frame.rs` (extended): 20/24-bit and >2-channel
  decode (`encode_flac_frame_wide` is the encoder side);
  `FlacEncoder::with_bits_per_sample` /
  `encode_i32` encode at 16/20/24 bits. 32-bit encode is
  intentionally not done.
- `vorbis_setup.rs` / `vorbis_packet.rs` (extended):
  Vorbis floor type 0 header parse + packet decode
  (verified on a synthetic stream only — no real floor-0
  file exists).
- `mp3_encode.rs`: MP3 Layer III CBR encode (`Mp3Encoder`:
  MPEG-1 64/96/128/320 kbps, MPEG-2/2.5 64/96/128; long
  blocks only, plain stereo, no bit reservoir; one frame of
  encoder delay, tail on `flush()`). VBR/ABR are
  `Unsupported`.
- `aac_encode.rs`: AAC-LC encode (`AacEncoder`: MDCT +
  sine window 1024, one scalefactor per frame, codebook
  pairs 5/7/9/11 by exact bit cost, SCE/CPE + ADTS).
  CBR/ABR/VBR. The gate is checked in `AacEncoder::new`
  exactly like `Decoder::from_aac_config`: feature off →
  `Unsupported`, no/expired license → `LicenseRequired`.
- `alac_encode.rs`: ALAC lossless encode (`AlacEncoder`:
  16/24-bit, 1–8 channels, frames 1–4096), bit-exact
  round-trip. 20/32-bit encode is intentionally
  unsupported.
- `dsd_modulator.rs`: PCM→DSD delta-sigma modulation
  (`DsdModulator`: CIFB order-5, NTF
  `((1−z⁻¹)/(1−0.85·z⁻¹))⁵`, streaming `feed()` /
  `flush()`, `write_dsf` / `write_dff`). DSD64/128/256/512.
  Not realtime on this VM; see `docs/LIMITS.md`.
- `celt_reconstruct.rs`: Opus CELT sample reconstruction
  (`CeltDecoder`, `celt_end_band`): band-energy decode, TF
  resolution, bit allocation, PVQ shape decode (CWRS),
  denormalisation, anti-collapse, inverse MDCT +
  overlap-add, comb-filter postfilter, de-emphasis. 22
  tables from the RFC 6716 Appendix A reference, each with
  provenance in a comment. SILK/Hybrid stay `Unsupported`.
- `vp9_token.rs`, `vp9_predict.rs`, `vp9_loopfilter.rs`,
  `vp9_reconstruct.rs`: VP9 reconstruction
  (`reconstruct_vp9_frame`, `Vp9ReconstructionState`):
  token/coefficient decode with the default prob tables
  (verified vs libvpx), intra (all 10 modes) + inter
  prediction + motion compensation, loop filter (§8.8).
  Keyframes decode bit-exactly; inter frames are not
  pixel-correct and `Decoder` refuses them.
- `av1_tile.rs`, `av1_intra.rs`, `av1_reconstruct.rs`:
  AV1 tile decode + intra reconstruction
  (`reconstruct_av1_frame_obu`, `reconstruct_av1_keyframe`):
  partition walk, intra modes, inverse transforms,
  dequant. No pixels yet: coefficient decode needs the
  default coefficient CDF tables (spec §9.3) and returns
  `Unsupported(COEFFICIENT_GAP)`. Intra block copy, loop
  filter, CDEF, and loop restoration are not implemented.
- `decoder.rs` / `encoder.rs` (wiring): `Decoder` now
  decodes Opus CELT, VP9 keyframes, and AV1 (coefficient-gap
  `Unsupported`); `Encoder` now encodes MP3 (CBR), AAC-LC
  (gated, via `EncoderConfig.license`), ALAC (16/24-bit),
  DSD (raw DSF-layout bytes, via `EncoderConfig.dsd_level`),
  and FLAC at 16/20/24-bit (via
  `EncoderConfig.bits_per_sample`). `Encoder::flush` drains
  the MP3 tail queue and the DSD tail.
  `BitrateCodec::from_codec` now maps `Codec::Mp3`.

## 4. Public API

### 4.1 Rust API (core)

The high-level API is frame-based and streaming-based on the same types. There is no separate API for each codec at the top level; the codec is selected by probing or by explicit construction.

```rust
use codec_whatttttttttttt_core::{Decoder, Encoder, Packet, Frame, Config};

pub enum Codec {
    Pcm,
    Flac,
    Opus,
    Vorbis,
    Vp9,
    Av1,
    /// H.264/AVC: licensed, hardware only, never software.
    /// `Decoder::decode` / `Encoder::encode` refuse it
    /// (`Unsupported` with the feature off, `LicenseRequired`
    /// with it on). Added in the 2026-11-08 revision round.
    Avc,
    /// H.265/HEVC: same gate as `Avc`.
    Hevc,
}

pub struct DecoderConfig {
    pub codec: Codec,
    pub max_frame_bytes: usize,
    pub max_channels: u32,
    pub max_sample_rate: u32,
    pub max_width: u32,
    pub max_height: u32,
}

pub struct Decoder { /* private state */ }

impl Decoder {
    pub fn new(config: DecoderConfig) -> Result<Self, Error>;
    pub fn probe(data: &[u8]) -> Result<Codec, Error>;
    pub fn decode(&mut self, packet: &Packet) -> Result<Frame, Error>;
    pub fn flush(&mut self) -> Result<Option<Frame>, Error>;
    pub fn metadata(&self) -> &Metadata;
}

pub struct EncoderConfig {
    pub codec: Codec,
    pub sample_rate: u32,
    pub channels: u32,
    pub bitrate: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl Encoder {
    pub fn new(config: EncoderConfig) -> Result<Self, Error>;
    pub fn encode(&mut self, frame: &Frame) -> Result<Packet, Error>;
    pub fn flush(&mut self) -> Result<Option<Packet>, Error>;
}
```

Frames:

```rust
pub enum Frame {
    Audio(AudioFrame),
    Video(VideoFrame),
}

pub struct AudioFrame {
    pub sample_rate: u32,
    pub channels: u32,
    pub samples: Vec<f32>,
    pub timestamp: u64,
}

pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub planes: Vec<Vec<u8>>,
    pub strides: Vec<usize>,
    pub timestamp: u64,
}

pub enum PixelFormat {
    Yuv420,
    Rgb24,
    Rgba32,
}

pub struct Packet {
    pub codec: Codec,
    pub data: Vec<u8>,
    pub timestamp: u64,
    pub is_keyframe: bool,
}
```

Streaming API:

```rust
pub struct Demuxer<R: std::io::Read> {
    pub fn new(reader: R) -> Result<Self, Error>;
    pub fn next_packet(&mut self) -> Result<Option<Packet>, Error>;
    pub fn seek(&mut self, timestamp: u64) -> Result<(), Error>;
}

pub struct Muxer<W: std::io::Write> {
    pub fn new(writer: W, config: MuxerConfig) -> Result<Self, Error>;
    pub fn write_packet(&mut self, packet: &Packet) -> Result<(), Error>;
    pub fn finish(self) -> Result<(), Error>;
}
```

Seek semantics: seek moves to the nearest keyframe at or before the requested timestamp for video, and to the nearest packet boundary for audio. Exact sample-accurate seek is a decode-and-discard operation on top of packet seek and is documented as such. No seek implementation pretends to be exact when it is not.

Resample and pixel conversion are explicit, not hidden inside `decode`:

```rust
pub fn resample(input: &AudioFrame, target_sample_rate: u32) -> Result<AudioFrame, Error>;
pub fn convert_pixel(input: &VideoFrame, target: PixelFormat, colour: ColourSpace) -> Result<VideoFrame, Error>;
```

### 4.2 C ABI (FFI crate)

The FFI crate exposes only opaque handles and plain C types. No Rust type crosses the boundary. Every function returns an error code; output is written through an out-pointer only on success.

```c
typedef int32_t codec_error_t;
#define CODEC_OK 0
#define CODEC_ERR_IO 1
#define CODEC_ERR_INVALID_DATA 2
#define CODEC_ERR_TRUNCATED 3
#define CODEC_ERR_UNSUPPORTED 4
#define CODEC_ERR_OVERFLOW 5
#define CODEC_ERR_ALLOCATION_LIMIT 6
#define CODEC_ERR_INVALID_ARGUMENT 7
#define CODEC_ERR_LICENSE_REQUIRED 8
```

`CODEC_ERR_LICENSE_REQUIRED` (8) was added in the 2026-11-08
revision round for `Error::LicenseRequired { codec }`; code 6 is
the allocation limit, not an out-of-memory code. The canonical
header is `crates/codec-whatttttttttttt-c/include/
codec_whatttttttttttt.h` (maintained by hand, not generated by
cbindgen — see section 2, layout corrections).

codec_error_t codec_decoder_from_wav_header(const uint8_t *header, size_t len, codec_decoder_t **out);
codec_error_t codec_decoder_decode(codec_decoder_t *decoder, const uint8_t *packet, size_t len, uint64_t timestamp_us, const codec_audio_frame_t **out_frame);
void codec_decoder_free(codec_decoder_t *decoder);
const char *codec_error_string(codec_error_t code);
```

Header: the canonical header is maintained by hand in `crates/codec-whatttttttttttt-c/include/codec_whatttttttttttt.h` (no cbindgen, no `codec-whatttttttttttt-ffi` crate — see section 2, layout corrections). The Swift vendored copy must stay identical; CI diffs them. This prevents drift between Rust and C.

Memory ownership across FFI:

- The caller allocates input buffers and retains ownership.
- A decoded frame (and an encoded packet) is borrowed from its decoder/encoder and stays valid only until the next call on the same handle or its free — the caller must copy before the next call. Buffers the library allocates (one-shot FLAC encode output) are released with `codec_free`; opaque handles with the matching `codec_*_free`.
- A null handle, null data pointer with non-zero length, or null out-pointer is `CW_ERROR_INVALID_ARGUMENT`, not a crash.
- No function retains a pointer to caller memory after it returns.

### 4.3 Binding APIs

Each binding mirrors the Rust high-level API in its own idioms and does no decoding itself:

- C++: RAII wrappers `Decoder` and `Encoder`, move-only, `std::expected` or exception per the binding README decision recorded before implementation. No raw `new`/`delete` in user code.
- Kotlin: `Decoder` and `Encoder` classes, `ByteArray` in, `Frame` out, JNI functions are `private external` and never called directly by users.
- Swift: `Decoder` and `Encoder` structs/classes with `throws`, SwiftPM package, module map for the C header.
- TypeScript: `Decoder` and `Encoder` classes, `Uint8Array` in, typed `Frame` out. React Native uses JSI/TurboModule on native and WASM on web/Node. The same TypeScript surface is used for both; the backend is selected at build time, not by user branching.
- Python: `Decoder` and `Encoder` classes with type hints in `.pyi`, exceptions mapped from the error enum, context-manager support for deterministic close.
- Zig: `Decoder` and `Encoder` structs with explicit allocator, error union return, `deinit` required. Example `examples/decode_wav.zig` is runnable with `zig build run`.

## 5. Error model

One error enum in Rust, mapped 1:1 to C error codes and to each binding's native error type. Errors are values, not panics. `unwrap` and `expect` are forbidden in library code (enforced by clippy and by review). `panic` is only permitted in tests and examples where a test failure is the correct outcome.

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid data at offset {offset}: {detail}")]
    InvalidData { offset: usize, detail: &'static str },

    #[error("truncated input: needed {needed} bytes, had {had}")]
    Truncated { needed: usize, had: usize },

    #[error("unsupported feature: {0}")]
    Unsupported(&'static str),

    #[error("value out of range: {0}")]
    Overflow(&'static str),

    #[error("allocation limit exceeded: requested {requested} bytes, limit {limit} bytes")]
    AllocationLimit { requested: usize, limit: usize },

    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),

    /// A licensed codec was requested without a valid license.
    /// Added in the 2026-11-08 revision round (`license.rs`,
    /// `hw.rs`). Feature off for that codec means the caller
    /// gets `Unsupported` instead; this variant means the gate
    /// exists and is closed (no license, malformed license is
    /// `InvalidArgument`, expired license is this variant).
    #[error("license required for codec {codec}")]
    LicenseRequired { codec: &'static str },
}
```

Rules:

- A corrupt file returns `InvalidData` or `Truncated`, never a partial frame presented as valid.
- A CRC mismatch is `InvalidData`, not a warning.
- An unsupported codec feature returns `Unsupported` with the feature name, not a generic failure.
- Errors are not swallowed: a helper that maps an error must preserve the offset and detail, or add context explicitly.
- `Display` output is stable enough for tests to match on variant, not on exact string, except for `codec_error_string` in C.

## 6. Memory model and limits

### 6.1 Ownership

- Rust core: all decoded data is owned by the caller after `decode` returns. The decoder holds only codec state (codebooks, prediction state, reference frames bounded by the spec), not unbounded history.
- No global mutable state. No `static mut`. `lazy_static` or `once_cell` only for immutable tables (Huffman tables, window tables) that are built once and never mutated.
- Reference frames for video are bounded by the maximum decoded picture buffer size for the declared resolution. A stream that declares a larger resolution than the configured maximum is `Unsupported` or `Overflow`, not an allocation of the declared size.

### 6.2 Allocation limits

All limits live in `limits.rs` and are enforced at the trust boundary (where external data enters), not deep inside a decoder that already trusts its input.

| Limit | Default | Rationale |
|---|---:|---|
| Max frame bytes (audio packet) | 1 MiB | Opus max packet is far smaller; FLAC frame is bounded by block size |
| Max frame bytes (video packet) | 16 MiB | Covers 4K intra frames with margin; larger is rejected |
| Max channels | 8 | FLAC/Opus/Vorbis spec maxima are within this |
| Max sample rate | 192000 Hz | FLAC maximum |
| Max width / height | 8192 | Above 8K is out of scope for v0.1 |
| Max metadata bytes | 1 MiB | Prevents a metadata block from exhausting memory |
| Max box / element nesting depth | 32 | MP4/EBML traversal is iterative with this cap |
| Max Huffman code length | 32 | Per-codec maxima are stricter where the spec requires it |
| Max allocation for one decode call | 64 MiB | Sum of output planes / samples for one frame |

If a limit is hit, the API returns `AllocationLimit` or `Overflow`. It does not attempt a smaller allocation and return a truncated frame.

### 6.3 Thread safety

- `Decoder` and `Encoder` are `Send` but not `Sync`. One decoder instance is used from one thread at a time. Sharing across threads requires an external `Mutex`, documented in the type docs.
- `BitReader`, `Huffman` tables, and window tables are `Send + Sync` where they hold only immutable data.
- No data race is permitted: shared state is either immutable or behind a `Mutex`/`RwLock` with the lock scope documented.
- The FFI layer documents that a single handle must not be used concurrently from two threads.

### 6.4 Unsafe

Unsafe is avoided. Where it is unavoidable (FFI pointer conversion, a proven hot path in MDCT or pixel conversion), the `unsafe` block must:

- be as small as possible,
- have a comment stating the invariant that makes it sound (for example, "pointer is non-null and points to `len` valid bytes because the caller checked `len` against the slice length at the FFI boundary"),
- be covered by a dedicated test that exercises the boundary condition (null, zero length, maximum length).

`cargo miri` runs on the core in CI. A Miri failure blocks merge.

## 7. Data flow

### 7.1 Frame path

```
bytes -> Demuxer -> Packet -> Decoder -> Frame -> resample / convert_pixel -> Encoder -> Packet -> Muxer -> bytes
```

Each arrow is a function boundary with a checked type. No stage passes raw bytes with an implicit format.

### 7.2 Streaming path

```
Read -> byte_source adapter (bounded buffer) -> Demuxer::next_packet -> Decoder::decode
```

The streaming path never loads a whole file into memory. The `io` adapter reads in fixed-size chunks (default 64 KiB). A parser that needs more than the buffered data returns `Truncated` and the caller supplies more data; the parser does not grow the buffer without a limit check.

### 7.3 Seek path

```
seek(timestamp) -> container index (cue / sample table / Ogg granule) -> nearest keyframe packet -> decode-and-discard to timestamp
```

If a container has no index, seek scans forward from the start in bounded steps. A seek past the end returns `InvalidArgument`. A seek into a corrupt index returns `InvalidData`.

### 7.4 Metadata path

```
container header -> metadata blocks -> Metadata struct -> caller
```

Metadata is read once at open time and cached. It is not re-parsed on every packet. Invalid UTF-8 in a text field does not fail the whole file; that field is returned as bytes with `is_valid_utf8 == false`.

## 8. Phases (Step 2 order)

Each phase is complete, compiles, and passes its tests before the next phase starts. Each phase ends with a review against sections 4, 5, and 6 of the user spec (quality, fix process, real-dev style) and a CHANGELOG entry.

### Phase 0 — Skeleton and trust boundary

- Workspace, `limits.rs`, `error.rs`, `bitreader.rs`, `io`, CI, lint configs, README, CHANGELOG.
- Commits: `chore: init workspace`, `feat(bitreader): bounded bit reader`, `feat(error): typed error enum`, `ci: add clippy and fmt checks`.
- Exit: `cargo test`, `cargo clippy -D warnings`, `cargo fmt --check` pass.

### Phase 1 — WAV / PCM

- `wav_header.rs`, `pcm.rs`, frame decode/encode for PCM, CLI `decode_wav` example.
- Tests: valid WAV vectors, truncated header returns `Truncated`, declared size larger than file returns `Truncated`, round-trip PCM I16/F32.
- Commits: `feat(wav): parse RIFF header`, `feat(pcm): saturating conversion`, `test(wav): truncated header returns error`.

### Phase 2 — FLAC decode

- `flac_frame.rs`, `huffman.rs`, metadata blocks, CRC checks.
- Tests: RFC 9639 test vectors, corrupt CRC returns `InvalidData`, truncated frame returns `Truncated`, round-trip after encode phase.
- Commits: `feat(flac): frame header parsing`, `feat(flac): subframe decode`, `test(flac): corrupt crc returns error`.

### Phase 3 — Ogg

- `ogg_page.rs`, Ogg stream demux, granule position handling.
- Tests: valid Ogg pages, bad CRC returns `InvalidData`, continued packet across pages, truncated page returns `Truncated`.
- Commits: `feat(ogg): page parsing and crc check`, `feat(ogg): packet reassembly`.

### Phase 4 — Opus decode

- `opus_packet.rs`, TOC parsing, CELT path, SILK path dispatch, PLC API.
- Spec: RFC 6716, RFC 8251.
- Tests: RFC 6716 test vectors, packet loss concealment does not panic, invalid TOC returns `InvalidData`.
- Commits: `feat(opus): toc and frame packing`, `feat(opus): celt decode path`.

### Phase 5 — Vorbis decode

- `vorbis_setup.rs`, `mdct.rs`, floors, residues, codebooks.
- Tests: Vorbis test vectors, truncated setup header returns `Truncated`, invalid codebook returns `InvalidData`.
- Commits: `feat(vorbis): setup header parsing`, `feat(vorbis): floor and residue decode`.

### Phase 6 — Container: WebM/Matroska and MP4 demux

- `ebml.rs`, `mp4_box.rs`, `Demuxer` for WebM and MP4, `Muxer` for WebM.
- Tests: valid files, box size smaller than header returns `InvalidData`, EBML size exceeding parent returns `InvalidData`, depth limit returns `InvalidData`.
- Commits: `feat(ebml): variable-length integer`, `feat(mp4): box traversal with depth limit`.

### Phase 7 — AV1 decode (subset)

- `av1_obu.rs`, OBU parsing, sequence header, frame header for the subset defined in `docs/LIMITS.md`.
- Tests: libaom test vectors for the subset, unknown OBU type returns `Unsupported`, truncated OBU returns `Truncated`.
- Commits: `feat(av1): obu header parsing`, `feat(av1): sequence header parsing`.

### Phase 8 — VP9 decode (subset)

- `vp9_bitstream.rs`, `range_decoder.rs`, uncompressed header, tile decode for the subset defined in `docs/LIMITS.md` before implementation starts.
- Tests: libvpx test vectors for the subset, truncated tile returns `Truncated`.
- Commits: `feat(vp9): uncompressed header parsing`, `feat(vp9): tile decode subset`.

### Phase 9 — Encode (basic)

- FLAC encode, Opus encode, PCM encode, VP9/AV1 basic encode. Round-trip tests for each codec that has an encoder.
- Commits: `feat(flac): basic encoder`, `feat(opus): basic encoder`, `test(roundtrip): flac encode decode matches`.

### Phase 10 — Bindings, in order

1. C ABI + C++ wrapper.
2. Python (ctypes, not PyO3 — see section 2, layout corrections).
3. Kotlin JNI.
4. Swift SwiftPM.
5. TypeScript WASM, then React Native JSI/TurboModule.
6. Zig `@cImport` example.

Each binding has its own README, runnable example, and tests. No binding ships without its tests passing.

### Phase 11 — Hardening

- `cargo-fuzz` targets for every parser (`bitreader`, `ogg_page`, `ebml`, `mp4_box`, `flac_frame`, `opus_packet`, `vorbis_setup`, `vp9_bitstream`, `av1_obu`).
- Property tests: round-trip, resample identity at same rate, pixel conversion round-trip YUV420 -> RGB -> YUV420 within documented tolerance.
- Benchmarks: the internal bench binary `examples/bench.rs` in the core crate (no `benches/` directory; the Step 1 plan for `benches/decode_*.rs` was never implemented as separate files).
- Sanitizers: ASan/UBSan for C/C++/JNI, Miri for Rust.

### Next phase (2026-11-08/09) — passthrough, MP3/AAC/ALAC, bitrate, resampler, DSD

- Passthrough AVC/HEVC/AAC after the license gate (MP4
  `avc1`/`hev1`/`mp4a`, Matroska `V_MPEG4/ISO/AVC`,
  `V_MPEGH/ISO/HEVC`, `A_AAC`); MPEG-TS demux/mux; ADTS
  demux/mux + CRC-16; AIFF/AIFF-C; ID3v1/v2; MP4 tags;
  Vorbis comment.
- MP3 Layer III decode (ISO full compliance, measured);
  AAC-LC decode behind `licensed-aac`; ALAC decode
  bit-exact vs ffmpeg. No encoder for any of the three.
- `BitrateMode` validation; windowed-sinc polyphase
  resampler; DSD DSF/DFF to PCM + DoP (DSD64/128).
- Tests: `tests/audio_matrix.rs` (lossless bit-exact rows,
  sample matrix §6 round-trips, lossy bitrate rows at
  64/96/128/320 kbps, Opus `Unsupported`, encode refusals),
  four new fuzz targets (`fuzz_mp3`, `fuzz_adts`,
  `fuzz_dsf`, `fuzz_dff`), `fuzz_mp4` extended with
  passthrough seeds.
- Commits: `feat(passthrough)`, `feat(mp3)`, `feat(aac)`,
  `feat(alac)`, `feat(bitrate)`, `feat(resampler)`,
  `feat(dsd)`, `fix(polyphase)`, `fix(adts)`,
  `fix(clippy)`, `fix(encoder)` (see `CHANGELOG.md`).

## 9. Testing, CI, and quality gates

### 9.1 Tests required per phase

- Unit tests named by behaviour, for example `decodes_truncated_frame_returns_error`, not `test1` or `test_basic`.
- Standard test vectors in `tests/data/` with origin and license in `tests/data/README.md`.
- Round-trip tests after encode exists.
- Property tests with `proptest` for parsers where input is structured.
- Corrupt/truncated file tests for every parser.
- Fuzz targets for every parser.

### 9.2 CI

`.github/workflows/ci.yml` runs on every push and pull request:

- `cargo fmt --check`
- `cargo clippy --all-targets -D warnings`
- `cargo test --workspace`
- `cargo test --workspace --release` for the decode path at least once per phase
- C++ build with `-Wall -Wextra -Werror`
- TypeScript `tsc --noEmit` with `strict: true`
- Python `ruff check` and `mypy`
- Swift `swift build` and `swift test` where the runner supports it
- Kotlin `./gradlew test` where the runner supports it

`.github/workflows/sanitizer.yml` runs ASan/UBSan and Miri on a schedule and on changes to parsers or FFI.

A CI failure blocks the phase. No phase is reported as done with a failing or skipped test.

### 9.3 Fix process

When a bug is found:

1. Write a failing test that reproduces it first.
2. Find the root cause, not the symptom.
3. Fix it.
4. Run the full test suite.
5. Check whether the same class of bug exists elsewhere.
6. Record it in CHANGELOG.md.

If code cannot be run here (for example, a Kotlin or Swift toolchain is missing in this environment), that is stated explicitly as not verified, with the exact command that was not run. No claim of "fixed" without a test run.

## 10. Documentation

- `README.md`: install, minimal example, supported codec table, status, links to docs. No advertising language.
- `NOTICE`: license-gate and patent-status notes for H.264/H.265/AAC, MP3, and ALAC (added in the 2026-11-08 revision round).
- `docs/API.md`: public API per language, generated from rustdoc and checked after each phase. **Not written yet** — see section 2, layout corrections.
- `docs/BUILD.md`: build per platform, toolchain versions, what is verified in this environment and what is not. **Not written yet** — see section 2, layout corrections.
- `docs/LIMITS.md`: the limits table from section 6, the VP9/AV1 decode subsets, and known limitations as issue notes.
- `CHANGELOG.md`: Keep a Changelog format. Every phase adds an entry.
- `CONTRIBUTING.md`: how to add a codec, how to add a test vector, commit format.

## 11. Risks and known limitations

- AV1/VP9 full decode is large. The subset approach in Phases 7–8 is intentional. Expanding the subset requires a separate design note and new test vectors.
- Bit-exactness against libFLAC, libopus, libaom, and libvpx is a test goal, not an assumption. Differences in float MDCT rounding must be documented with tolerance, not hidden.
- WASM and React Native JSI have different threading models. The TypeScript binding must not assume a decoder can be used from two threads at once.
- MP4 has many box types. The demuxer supports the boxes needed for the codecs in scope first; unknown boxes are skipped by size, not parsed.
- No test vector is invented. If a vector cannot be obtained with a clear origin and license, the test is marked as missing in `docs/LIMITS.md` and the phase does not claim coverage for it.

## 12. Decisions — confirmed by Hoàng, 2026-11-08

1. Name: `codec-whatttttttttttt`. Crate and package spellings in section 2 are fixed unless Hoàng changes them.
2. License: MIT OR Apache-2.0 (dual).
3. Video priority: AV1 before VP9. Phase 7 is the AV1 subset, Phase 8 is the VP9 subset.
4. Audio priority: FLAC decode before Opus.
5. Scope: local only. Push to GitHub only when the whole project is finished, not during development.
6. First phases: Phase 0 + Phase 1 implemented together. Phase 0 + Phase 1 are complete; see `CHANGELOG.md`.
7. Phase 2 + Phase 3 implemented together, 2026-11-08. FLAC decode and Ogg demux/mux are complete; see `CHANGELOG.md`.
8. Phase 4 + Phase 5 implemented together, 2026-11-08. Opus is partial (packet parsing, OpusHead/Tags, range decoder, CELT frame-header symbols, silence PLC; CELT sample reconstruction and SILK return `Unsupported`) and Vorbis decode is complete; see `CHANGELOG.md`. The Ogg Opus mapping is RFC 7845, not RFC 8251 (RFC 8251 is the RTP payload format).
9. Phase 6 + Phase 7 implemented together, 2026-11-08. WebM/Matroska and MP4 demux are complete (see `docs/LIMITS.md` for the exact limits); AV1 is parser-only (OBU + sequence header + frame header + tile-group framing; reconstruction returns `Unsupported`); see `CHANGELOG.md` and `docs/LIMITS.md`.
10. Phase 8 + Phase 9 are pre-authorized by Hoàng (2026-11-08 16:29): after Phase 6+7, do VP9 decode subset + basic encode next, without asking again. Phase 8 and Phase 9 are complete; see `CHANGELOG.md` and `docs/LIMITS.md`.
11. Phase 10 + Phase 11 are pre-authorized by Hoàng (2026-11-08 17:39): after Phase 8+9, do the remaining phases (bindings, then hardening) without asking again, and report a final summary only when all 12 phases are done. Phase 10 and Phase 11 are complete; see `CHANGELOG.md` and `docs/LIMITS.md`. All 12 phases are now complete.
12. Revision round (2026-11-08, after all 12 phases): the license gate, resolution limits, frame pool, scaler, quality labels, TPDF dither, and I32 PCM from section 3.8. Rules: no H.264/H.265 software code ever existed, so none was removed; the gate is a compliance mechanism only (see `NOTICE`); the 64 MiB per-call allocation cap is deliberately not raised for 10-bit 8K; demux still rejects AVC/HEVC/AAC tracks with `Unsupported` (no passthrough behind the gate yet); AV1 level 5.2 display rate is unverified and not checked. See `CHANGELOG.md` (Unreleased, revision round) and `docs/LIMITS.md`.

Phase 10 + 11 API notes (recorded during implementation, not a silent change):

- The C ABI lives in `crates/codec-whatttttttttttt-c/`: `src/lib.rs` (opaque handles, `catch_unwind`, thread-local last error), `include/codec_whatttttttttttt.h` (the canonical header — the Swift vendored copy must stay identical; CI diffs them), `include/codec_whatttttttttttt.hpp` (C++ RAII wrapper, no separate `bindings/cpp/` directory). Crate types: `cdylib`, `staticlib`, `rlib`. The WASM crate (`crates/codec-whatttttttttttt-wasm/`) exports raw wasm functions over the same C ABI (no wasm-bindgen): `wasm_alloc`/`wasm_free`, `wasm_probe`, `wasm_decode_wav`/`wasm_decode_flac`, `wasm_encode_flac`, `wasm_vp9_parse_header`, `wasm_av1_parse_sequence_header`, and the streaming encoder exports (`wasm_encoder_new_flac`, `_flac_streaminfo`, `_encode`, `_free`); handles are index+1 with 0 invalid, freed slots reused, and the table stores addresses as `usize`, not raw pointers, because `*mut CodecEncoder` is not `Send`.
- The C ABI length header is fixed-width `u64` (8 bytes), not `usize`: `usize` is 4 bytes on 32-bit/wasm32 targets, so `usize::to_le_bytes` failed to compile there. The WASM streaming-encoder handle table is indexed with `handle - 1` via `checked_sub(1)`.
- Bindings only call the core through the C ABI. Kotlin JNI copies borrowed views (a decoded frame stays valid only until the next call on the same handle). Swift uses `OpaquePointer` for the incomplete-struct handle typedefs and links the static archive by path (linking by `-l` would pick the cdylib `.so` in the same directory). The TypeScript core computes the FLAC decode capacity from STREAMINFO (max blocksize, 0 meaning 65535) times the channel count, not from the packet length. The React Native TypeScript passes binary data as base64 strings and samples as number arrays; its Android module is a plain `@ReactMethod` module delegating to the Kotlin binding, its iOS module a plain RCTBridgeModule calling the C ABI directly.
- Fuzzing uses `cargo-fuzz` (nightly) with one target per parser in `crates/codec-whatttttttttttt-core/fuzz/` (a standalone crate, excluded from the workspace so `cargo test --workspace` does not touch it), seed corpora under `fuzz/corpus/`, and a 60 s run per target. The three crashes found (FLAC LPC overflow, OpusHead family-1 sum overflow, MP4 `stco`/`co64` allocation) are fixed per the fix process and pinned by regression tests; the fuzz artifacts are kept whole as fixtures because `cargo fuzz tmin` could not reduce them.
- The OpusHead family-1 check follows RFC 7845 section 5.1.1 as written: the decoded channel count (M + N) MUST be no larger than 255; the output channel count C is not constrained to match it, and mapping entry 255 means silence. A first version of the fix wrongly required M + N to equal C and was corrected after re-reading the RFC — the existing unit test `opus_head_family_1_mapping` (N=2, M=1, C=4, mapping `[0, 2, 1, 2]`) is valid per the RFC.
- Sanitizers: ASan for the C crate needs target-specific rustflags (`-Zsanitizer=address` on the target, not globally), because building proc-macro crates with sanitizers enabled is not supported. Miri runs the core lib tests with `MIRIFLAGS="-Zmiri-disable-isolation"`; it is slow and its result is recorded in `docs/LIMITS.md`, not assumed.
- The internal benchmark (`examples/bench.rs`) uses `Instant`, warmup 3, 10 timed iterations, median — no criterion dependency. Vorbis decode measures packet decoding only (headers are parsed once, the parsed `VorbisSetup` is cloned per iteration).

Phase 6 + 7 API notes (recorded during implementation, not a silent change):

- `Demuxer::from_bytes` takes `&[u8]`; the caller keeps the buffer. `MatroskaDemuxer`/`Mp4Demuxer` parse from slices. `Decoder::probe` returns `Unsupported` for EBML/`ftyp` signatures (use `Demuxer`); IVF `DKIF` with fourcc `AV01`/`VP90` probes to `Av1`/`Vp9`.
- `Decoder::from_av1_sequence_header` validates the AV1 subset (profile 0, 8-bit, YUV420, no film grain, no mono); `Decoder::decode` for `Codec::Av1` returns `Unsupported` (parser-only; tile data is framed, never reconstructed).
- The Step 1 design's claim that `range_decoder.rs` covers AV1 is outdated: the Opus range decoder (RFC 6716) and the AV1 symbol decoder (AV1 spec section 8.2) are different algorithms. AV1 uses the separate `Av1SymbolDecoder` in `av1_frame.rs`, used for tile data only; the frame header is parsed with the plain bit reader.
- AV1 header details verified against the final spec (v1.0.0) and ffmpeg's `trace_headers`, not the 2018 draft (the draft differs: seq_profile f(2) vs f(3), operating-points fields, frame-header field order, no `film_grain_params_present`, no `color_range`/`separate_uv_delta_q` reads in the same places). The trace prints raw bits; the spec then infers final values: `error_resilient_mode` is forced to 1 for a shown key frame or a switch frame, `force_integer_mv` is forced to 1 for intra frames, `allow_warped_motion` is forced to 0 for intra frames (section 5.9.2). `delta_q_present` is read only when `base_q_idx > 0`; `delta_lf_present` only when delta Q is present and intra block copy is not allowed; `read_delta_q` reads su(7); `render_and_frame_size_different` is read unconditionally; `byte_alignment` (section 5.3.5) reads only zero bits (no trailing one bit), while sequence headers use `trailing_bits()` (one bit then zeros). Color config reads `color_description_present_flag` even for monochrome, and the monochrome branch reads `color_range` f(1) before returning (it does not force `color_range` = 1). `seq_choose_integer_mv` is read only when `seq_force_screen_content_tools > 0`.
- Container details verified against ffmpeg output: the WebM temporal delimiter is the 2-byte OBU `0x12 0x00` (size field present, zero length), and WebM strips it from video packets; an IVF temporal unit can hold several frames (only the last is shown); the IVF header stores the timebase denominator before the numerator; MP4 `vpcC` is a FullBox (4 version/flags bytes precede the record); MP4 `dOps` fields are big-endian while OpusHead fields are little-endian, and the `dOps` version (0) is not the OpusHead version (1). The MP4 edit list is parsed but not applied to timestamps; the demuxer returns raw container timestamps and the Opus decoder drops the pre-skip.
- Test vectors for Phase 6+7 are ffmpeg 8.1.2 encodes of local CC0 sources (testsrc2 64x64 10 fps 1 s + local sine WAVs), not official conformance vectors; see `tests/data/README.md`.

Phase 8 API notes (recorded during implementation, not a silent change):

- `Decoder::from_vp9_frame_header` validates the VP9 subset (profile 0, 8-bit, YUV420, not RGB); `Decoder::decode` for `Codec::Vp9` returns `Unsupported` (parser-only; tile data is framed, never reconstructed). `Vp9ReferenceState` carries the 8 reference slots, loop-filter deltas, and segmentation state across frames; `setup_past_independence` resets them for key / error-resilient / intra-only frames.
- The subexponential delta (VP9 spec section 6.3.4) follows libvpx `vp9_dsubexp.c` (`decode_uniform`), the reference implementation: after the three prefix bits, a 7-bit value `v` is read and the extra bit is read **only when `v >= 65`** (`v + 64` below 65, `(v << 1) - 1 + bit` at or above). The printed spec text places the extra-bit read ambiguously; reading it unconditionally desynchronised the compressed-header parse on larger key frames (80–796 bits short, `exit_bool` failing on padding 1-bits). Found by reading the libvpx source and confirmed by all 7 probe frames passing `exit_bool` after the fix; the bool decoder itself was verified against the spec and Chromium's `vp9_bool_decoder.cc` and was not the bug.
- VP9 header details verified against ffmpeg's `trace_headers` and the libvpx source: color_space value 0 is UNKNOWN, not BT_601 (1) — the trace prints 0 and the parser returns `Unknown`; a non-last tile size is big-endian `f(32)`; superframe frame sizes are little-endian (verified by a synthetic superframe built from two real frames, which ffmpeg splits into 2 frames only with little-endian sizes). `s(n)` reads magnitude `f(n)` first, then a sign bit. The compressed header's updated probability values are not tracked (the spec's default probability tables are not ported); only `tx_mode`, `reference_mode`, and the update count are reported.
- Test vectors for Phase 8 are ffmpeg 8.1.2 libvpx-vp9 encodes of local CC0 sources (testsrc2, 10 fps, 1 s), plus a synthetic superframe built from two real frames with a spec Appendix B index; see `tests/data/README.md`.

Phase 9 API notes (recorded during implementation, not a silent change):

- `bitwriter.rs` (`BitWriter`) is the MSB-first counterpart of `bitreader.rs` (`BitReader`); FLAC subframes are written MSB-first and padded with zero bits to the byte boundary before the CRC-16.
- The FLAC encoder lives in `flac_frame.rs` next to the decoder, reusing `crc8`/`crc16` and `StreamInfo`. `encode_flac_frame` writes a variable-blocking-strategy frame (sample number as the coded number, minimal UTF-8-like form); block-size code 12 for 4096, code 6 (1 trailing byte) for sizes up to 256, code 7 (2 trailing bytes) otherwise; sample-rate code from the table, falling back to code 0 (rate from STREAMINFO) when no code represents the rate. `encode_flac_stream` (one-shot) writes exact STREAMINFO (min/max block size, total samples, actual min/max frame sizes, MD5 unset); `FlacEncoder` (streaming) writes the streaming form (total 0, min block 0, max block 4096).
- Subframe choice is by exact encoded bit cost (header + warm-up + predictor data + residual header + Rice codes), constant / verbatim / fixed 0–4 / LPC 1–8; a predicted subframe that does not beat verbatim is written verbatim. LPC uses autocorrelation + Levinson-Durbin (verified against the closed form for a sine wave: order 2 gives `2cos(w)`, `-1`), 15-bit quantized coefficients, and the residual is computed with the quantized coefficients exactly as the decoder restores them, so the round-trip is exact by construction. Rice uses a single partition, parameters 0–14 (15 is the escape code and is never emitted).
- `Encoder` supports `Codec::Flac`: f32 samples are quantized to i16 with the same saturating scale as PCM I16 (`s * 32768`, 32768 saturating to 32767); one input frame becomes one FLAC frame (block size 1..=4096, larger frames return `InvalidArgument`). `Encoder::flac_streaminfo_bytes()` returns the streaming header, or an empty vector for a non-FLAC encoder. Opus, Vorbis, VP9, and AV1 encode return `Unsupported` naming the missing encoder.
- FLAC encode was verified three ways: the Phase 2 decoder round-trips sine, silence, noise, and ramp byte-identically; ffmpeg's FLAC decoder decodes the CLI output to byte-identical PCM (including a 88200-frame stereo file, 22 frames); and the CLI `decode` path decodes the CLI `encode` output byte-identically. No new fixture files were needed: the encode tests generate their samples in code.

Phase 1 API notes (recorded during implementation, not a silent change):

- `DecoderConfig` gains `pcm_format`, `pcm_channels`, and `pcm_sample_rate`: PCM decoding needs a format source, and the Step 1 design did not name one. `Decoder::from_wav_header` is the normal way to set them.
- The Step 1 `io` module is implemented as `byte_source.rs` (`BoundedReader`).

Phase 2 + 3 API notes (recorded during implementation, not a silent change):

- `Decoder::from_flac_streaminfo` constructs a FLAC decoder from a parsed STREAMINFO block; a FLAC packet is one frame.
- `Decoder::probe` on an `OggS` signature returns `Unsupported` ("ogg container: use OggDemuxer") instead of guessing a codec; the codec is identified from the first packet by `OggDemuxer`.
- `FlacDemuxer` finds frame boundaries by decoding each frame once; `Decoder` decodes it again (double decode, recorded as a known limitation).
- `OggDemuxer` handles one logical stream only; chained or multiplexed streams return `Unsupported`. Packet `timestamp` holds the granule position in samples, not microseconds.
- Vorbis comment keys are kept as written (`Metadata::get` is an exact match); ffmpeg writes the key as lowercase `encoder`.
- FLAC and Ogg test vectors are encoded from the local CC0 WAV vectors with ffmpeg 8.1.2 (Lavc FLAC encoder, Lavf Ogg muxer), not official RFC/libFLAC vectors; see `tests/data/README.md`.

Phase 4 + 5 API notes (recorded during implementation, not a silent change):

- `Decoder::from_opus_head` constructs an Opus decoder (mapping family 0 only; families 1 and 255 return `Unsupported`). Pre-skip is subtracted from the stream start in 48 kHz samples; output gain (Q7.8 dB) is applied to decoded samples. Opus is always decoded at the 48 kHz internal rate.
- `Decoder::conceal_loss()` is the explicit PLC API: Opus returns a silence frame of the last frame's duration (`None` before the first packet); Vorbis and other codecs return `None`.
- `Decoder::from_vorbis_setup` constructs a Vorbis decoder from a parsed `VorbisSetup`. The first packet primes and returns an empty frame; `flush()` returns the last block's second half once.
- `HuffmanTable` uses the Vorbis codeword assignment (Vorbis I spec section 3.2.1; libvorbis `_make_words`), not the standard canonical assignment. Decode binary-searches the codewords of each length, because the Vorbis assignment does not make them consecutive. The table is used only by Vorbis; FLAC uses Rice codes.
- `mdct_forward` carries the 2/N scale and `mdct_inverse` applies no scale, matching libvorbis. CELT's IMDCT scales by 1/2 (RFC 6716 section 4.3.7), so a CELT spectrum must be pre-scaled by 1/2 before calling `mdct_inverse`.
- CELT sample reconstruction is not implemented: the allocation and PVQ tables exist only inside the reference C source, which RFC 6716 Appendix A ships as a base64-encoded gzipped tarball that cannot be extracted from the specification text. Porting them from the reference source is follow-up work.
- Vorbis floor type 0 parses its header but packet decode returns `Unsupported`; floor type 1 is fully implemented. End-of-packet during Vorbis floor and residue decode is nominal per the spec (sections 4.3.2, 8.6.2) and yields partial output, not an error; only an empty packet returns `Truncated`.
- Opus and Vorbis test vectors are encoded from the local CC0 WAV vectors with ffmpeg 8.1.2 (libopus, libvorbis), not official conformance vectors; see `tests/data/README.md`.

## 13. Self-check for this design (per user spec section 6D)

1. Generic file/function names: none in the module list. The bounded read adapter is implemented as `byte_source.rs` (`BoundedReader`), not `io`.
2. Comments that repeat code: this design contains no such comments. Code comments in implementation will explain why, with spec section references.
3. Unused abstractions: no abstraction is introduced for single use. `bitreader`, `huffman`, `range_decoder`, and `mdct` are each used by at least two codecs.
4. Ecosystem layout: Cargo workspace, C++ `include/<lib>/`, Kotlin Gradle module, Swift SwiftPM, Python `src/<lib>/`, TypeScript `packages/`, Zig `build.zig` — each follows its ecosystem.
5. Format/lint: configs are listed in section 2. They are run in CI in section 9.2. No phase is complete without them passing.

---

End of Step 1 design. Awaiting confirmation of section 12 before Step 2.
