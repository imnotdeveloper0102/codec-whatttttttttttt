# Changelog

All notable changes to this project are documented in this file.
The format is based on Keep a Changelog, and this project adheres to
Semantic Versioning.

## [Unreleased]

### Added

- Phase 0: Cargo workspace (`codec-whatttttttttttt-core`,
  `codec-whatttttttttttt-cli`), typed `Error` enum, hard limits in
  `limits.rs`, bounded `BitReader`, bounded `BoundedReader`
  (`byte_source`), CI workflow, lint and format configuration.
- Phase 1: WAV/PCM support. `wav_header` parses RIFF/WAVE, `fmt `
  (PCM I16/I24/F32 and WAVE_FORMAT_EXTENSIBLE), and `data` chunks;
  a declared `data` size larger than the file returns `Truncated`.
  `pcm` converts I16/I24/F32 with saturating scale factors.
  `resample` provides linear interpolation; the polyphase FIR
  resampler is not implemented yet. `Decoder` and `Encoder` support
  `Codec::Pcm`; other codecs return `Unsupported` naming the codec.
  `WavDemuxer`/`WavMuxer` demux and mux single WAV files. CLI
  commands `info` and `decode`. Example `decode_wav`.
- Test vectors in `crates/codec-whatttttttttttt-core/tests/data/`,
  generated locally with CPython `wave` (CC0), including truncated
  and corrupt files.
- Phase 2: FLAC decode (RFC 9639). `flac_frame` parses STREAMINFO
  and other metadata blocks (Vorbis comment entries are kept in
  `Metadata`, keys as written), frame headers with CRC-8, and
  subframes of all four kinds (constant, verbatim, fixed predictor,
  LPC) with CRC-16 frame checks. `huffman` provides a canonical
  Huffman table for FLAC and later Vorbis use. `Decoder` supports
  `Codec::Flac` via `Decoder::from_flac_streaminfo`; `FlacDemuxer`
  returns one packet per frame. CLI `info` prints FLAC format,
  channels, rate, bits, total samples, MD5, and metadata count;
  CLI `decode` decodes FLAC to WAV.
- Phase 3: Ogg (RFC 3533). `ogg_page` parses and writes Ogg pages
  with CRC-32 verification. `ogg_stream` provides `OggDemuxer`
  (reassembles continued packets across pages, keeps granule
  position as the packet timestamp in samples, identifies FLAC /
  Opus / Vorbis from the first packet) and `OggMuxer`. FLAC-in-Ogg
  decodes end to end through `OggDemuxer` + `Decoder`; Opus and
  Vorbis packets demux but do not decode yet. CLI `info` prints Ogg
  codec, serial, page count, and packet count; CLI `decode` decodes
  Ogg FLAC to WAV.
- FLAC and Ogg test vectors in `tests/data/`, encoded from the local
  CC0 WAV vectors with ffmpeg 8.1.2 (Lavc FLAC encoder, Lavf Ogg
  muxer); see `tests/data/README.md` for the exact commands and
  what each file exercises.
- Phase 4: Opus decode, partial (RFC 6716, RFC 7845). `opus_packet`
  parses the TOC byte, frame packing codes 0–3 (VBR/CBR, padding),
  `OpusHead` (mapping families 0/1/255) and `OpusTags`, enforcing
  the R1–R7 validity rules. `range_decoder` implements the range
  decoder and a test encoder (RFC 6716 sections 4.1, 5.1).
  `celt` decodes the CELT frame-header symbols (silence,
  post-filter, transient, intra, Table 56 order) and provides an
  explicit `Decoder::conceal_loss()` PLC API returning silence of
  the last frame's duration. `Decoder` supports `Codec::Opus` from
  `OpusHead` (family 0 only); pre-skip is subtracted from the
  stream start and output gain (Q7.8 dB) is applied. CLI `info`
  prints Opus channels, pre-skip, sample rate, gain, mapping
  family, vendor, and comment count. CELT sample reconstruction
  and SILK return `Unsupported` naming the unimplemented part;
  no PCM is fabricated for them.
- Phase 5: Vorbis decode (Vorbis I specification). `mdct`
  implements the Vorbis MDCT (forward carries the 2/N scale,
  inverse unscaled, matching libvorbis). `lsb_bitreader` provides
  the LSB-first bit reader and `ilog`. `vorbis_setup` parses all
  three headers (identification, comment, setup: codebooks, floor
  type 1, residues type 0/1/2, mappings, modes); floor type 0
  parses its header but packet decode returns `Unsupported`.
  `vorbis_packet` decodes audio packets (mode, window, floor 1,
  residue, inverse coupling, inverse MDCT, overlap-add) with
  priming and a `flush()` tail. `Decoder` supports `Codec::Vorbis`
  from `VorbisSetup`. CLI `info` prints Vorbis channels, sample
  rate, blocksizes, vendor, comment count, and codebook/floor/
  residue/mapping/mode counts; CLI `decode` decodes Ogg Vorbis to
  WAV. Decoded PCM matches ffmpeg/libvorbis to within 1e-6 (f32
  rounding); against the original WAV the lossy difference is
  stereo RMS 0.0059 / max 0.016, mono RMS 0.0103 / max 0.039.
- Opus and Vorbis test vectors in `tests/data/`, encoded from the
  local CC0 WAV vectors with ffmpeg 8.1.2 (libopus, libvorbis);
  see `tests/data/README.md` for the exact commands and what each
  file exercises. Integration tests `tests/opus.rs` (18 tests) and
  `tests/vorbis.rs` (10 tests) cover the real files end to end.
- Phase 6: WebM/Matroska and MP4 demux. `ebml` reads EBML
  variable-length integers (signed and unsigned, including unknown
  sizes), element headers, and iterates children without recursion
  (depth 32). `matroska` parses Segment, SeekHead, Info, Tracks,
  Cluster, BlockGroup/Block, SimpleBlock, Cues, and Tags, with
  Xiph/EBML/fixed lacing and keyframe seeking via Cues; unknown
  codecs return `Unsupported` naming the codec. `mp4_box` parses
  ISO BMFF boxes iteratively (depth 32) — `ftyp`, `mdat`, `moov`,
  `mvhd`, `tkhd`, `mdhd`, `hdlr`, `minf`, `stbl`, `stsd`, `stts`,
  `stss`, `stsc`, `stsz`, `stco`, `co64`, `ctts`, `elst` — and
  walks the sample tables into packets with full validation; the
  edit list is parsed but deliberately not applied to timestamps.
  `demux` provides the common `Demuxer` (probe by EBML/`ftyp`
  magic, `next_packet`, `seek` to the nearest keyframe). CLI `info`
  prints container, duration, tracks, packets, metadata, and
  codec configuration (AV1 sequence header from `av1C` /
  CodecPrivate, OpusHead from OpusHead / `dOps`, Vorbis setup from
  the WebM header table, VP9 `vpcC` fields). Test vectors are real
  ffmpeg 8.1.2 files (AV1+Opus and VP9+Vorbis/Opus in both
  containers, plus keyframe files for seek tests); integration
  tests `tests/ebml.rs` (9 tests), `tests/matroska.rs` (5 tests),
  `tests/mp4.rs` (9 tests), and `tests/demux.rs` (7 tests) cover
  the real files end to end.
- Phase 7: AV1 decode, parser-only (AV1 spec v1.0.0). `av1_obu`
  parses OBU headers, LEB128 sizes, and all OBU types; `ObuType`
  gains a `value()` method returning the 4-bit wire value.
  `av1_sequence` parses sequence headers (profiles 0–2, operating
  points, dimensions, color config) with a `validate_subset()`
  that accepts profile 0, 8-bit, YUV420 only. `av1_frame` parses
  frame headers (show_existing, frame types, reference state,
  tile info, quantization, segmentation, loop filter, CDEF, loop
  restoration, TX mode, skip mode, global motion, film grain
  parameters) and tile-group framing, and implements the AV1
  symbol decoder (spec section 8.2) as `Av1SymbolDecoder`. The
  parser is checked against ffmpeg's `trace_headers` output for
  the same file, bit for bit, including the cases where the trace
  prints a raw bit and the spec infers a different final value
  (error_resilient_mode, force_integer_mv, allow_warped_motion).
  `Decoder` supports `Codec::Av1` from a sequence header, but
  `decode` returns `Unsupported`: tile data is framed, never
  reconstructed, and no `VideoFrame` is produced. CLI `info`
  prints IVF files (fourcc, resolution, frame rate, OBU type
  counts, sequence header) and raw AV1 OBU streams. Integration
  tests `tests/av1.rs` (14 tests) cover OBU headers, the sequence
  header, the key and first inter frame headers, show_existing,
  truncation, and corrupt input.
- Phase 8: VP9 decode, parser-only (VP9 Bitstream & Decoding
  Process Specification v0.6). `vp9_bitstream` parses the
  uncompressed header (section 6.2: profile, color config, frame
  size, render size, frame size with references, interpolation
  filter, loop filter, quantization, segmentation, tile info),
  the compressed header (section 6.3: TX mode and probs,
  coefficient probs, skip prob, and the inter sections) through
  `Vp9BoolDecoder` (section 9.2), superframe indexes
  (Appendix B), and tile framing (section 6.4: non-last tile
  size is big-endian `f(32)`, superframe sizes are
  little-endian). `Vp9ReferenceState` tracks the 8 reference
  slots, loop-filter deltas, and segmentation state across
  frames. `Decoder` supports `Codec::Vp9` from a frame header,
  but `decode` returns `Unsupported`: tile data is framed, never
  reconstructed, and no `VideoFrame` is produced. The compressed
  header parse was fixed against the libvpx reference source
  (`vp9_dsubexp.c` `decode_uniform`): in the subexponential
  delta (section 6.3.4) the extra bit after the 7-bit value is
  read only when `v >= 65`; reading it unconditionally
  desynchronised the parse on larger key frames (80–796 bits
  short). All 10 frames of `vp9_64x64.ivf` and the multi-tile
  and superframe vectors now parse and pass `exit_bool`. CLI
  `info` prints IVF files with fourcc VP90 (frame headers,
  superframe splits, key-frame count). Integration tests
  `tests/vp9.rs` (13 tests) cover every frame of
  `vp9_64x64.ivf` against ffmpeg's `trace_headers` output,
  multi-tile framing, superframes, truncation, and corrupt
  input.
- Phase 9: FLAC encode, 16-bit (RFC 9639). `bitwriter` is the
  MSB-first counterpart of `bitreader`. `flac_frame` gains the
  encoder: `write_streaminfo_bytes` / `write_flac_stream_header`
  (inverse of `parse_streaminfo`), `encode_flac_frame` (frame
  header with CRC-8, one subframe per channel, CRC-16 footer),
  `FlacEncoder` (streaming, one frame per `encode` call, running
  sample number), and `encode_flac_stream` (one-shot, 4096-sample
  frames, exact STREAMINFO). Subframes are chosen by exact encoded
  size: constant, verbatim, fixed predictor orders 0–4, LPC orders
  1–8 (autocorrelation + Levinson-Durbin in f64, 15-bit quantized
  coefficients, shift chosen so the largest coefficient fits;
  the residual is computed with the quantized coefficients exactly
  as the decoder restores them, so the round-trip is exact by
  construction). Residuals use a single Rice partition
  (parameters 0–14); a predicted subframe that does not beat
  verbatim is written verbatim. Channels are independent.
  `Encoder` supports `Codec::Flac` (one input frame becomes one
  FLAC frame, block size 1..=4096) with
  `flac_streaminfo_bytes()` for the streaming header; Opus,
  Vorbis, VP9, and AV1 encode return `Unsupported` naming the
  missing encoder. CLI `encode` writes WAV as FLAC. Integration
  tests `tests/flac_encode.rs` (22 tests) round-trip sine,
  silence, noise, and ramp signals byte-identically through the
  Phase 2 decoder, including a 10000-sample stream (4096 + 4096 +
  1808) and the streaming/one-shot equivalence; ffmpeg's FLAC
  decoder was also checked externally and decodes the output to
  byte-identical PCM.
- Phase 10: bindings over the C ABI. `codec-whatttttttttttt-c`
  (cdylib/staticlib/rlib) exposes version, error codes and
  messages, probe, audio decode (WAV header / FLAC STREAMINFO,
  one frame per packet), FLAC encode (one-shot and the streaming
  encoder), and the VP9/AV1 header parsers; opaque handles are
  freed with the matching `codec_*_free`, library-allocated
  buffers with `codec_free`; `include/codec_whatttttttttttt.hpp`
  is the C++ RAII wrapper. `bindings/python/` (ctypes, 13
  unittest, `.pyi` stub, ruff + mypy clean), `bindings/kotlin/`
  (JNI, package `dev.codecwhat`, 15 checks via `kotlinc`
  directly — Gradle is not installed here),
  `bindings/swift/` (SwiftPM, module `CodecWhat`, vendored C
  header kept identical to the canonical one, 10 XCTest),
  `bindings/zig/` (build.zig + `@cImport`, links the static
  archive, 10 tests), and `bindings/typescript/` (npm workspace:
  `packages/wasm/` with a new `codec-whatttttttttttt-wasm`
  crate exporting raw wasm functions over the C ABI, streaming
  encoder included; `packages/core/` with `Decoder`/`Encoder`
  over the wasm module, 10 node:test tests;
  `packages/react-native/` with a TurboModule spec, an Android
  Kotlin module, and an iOS Objective-C++ module — the
  TypeScript type-checks against real react-native 0.88.1
  types, the native modules are written but never built here).
  Bindings only call the core through the C ABI; no decode or
  encode logic is re-implemented.
- Phase 11: hardening. `crates/codec-whatttttttttttt-core/fuzz/`
  has cargo-fuzz targets for all nine parsers (WAV, FLAC, Ogg,
  EBML/Matroska, MP4, AV1 OBU, VP9 bitstream, Opus TOC, Vorbis
  setup) with seed corpora under `fuzz/corpus/`; a 60 s run per
  target found 3 crashes, all fixed (see Fixed). Property tests
  in `tests/property.rs` (6 tests: WAV/FLAC/Ogg/EBML round-trips,
  resample identity, FLAC encode/decode round-trip). Internal
  benchmark `examples/bench.rs` (no criterion dependency):
  FLAC decode 90 µs for 4410 frames (1101.6x realtime), Vorbis
  decode 190972 µs for 5824 frames (0.7x realtime — the naive
  O(N²) `mdct_inverse`; a performance limit, not a bug), FLAC
  encode 330222 µs for 220500 frames (2.7 MB/s in). ASan/UBSan:
  the C example and the C++ driver run clean under
  `-fsanitize=address,undefined` against an ASan-built static
  library (target-specific rustflags, because sanitizers cannot
  build proc-macro crates). CI gains per-binding jobs (headers,
  python, typescript, swift, kotlin, zig) and a sanitizer
  workflow (fuzz 60 s per target, ASan/UBSan, Miri lib tests).
- `docs/LIMITS.md` records the AV1 subset, the WebM/MP4 limits,
  and the known limitations per phase, so a claim there always
  has a passing test behind it.

### Changed

- Design: video order is AV1 before VP9 (Phase 7 AV1 subset,
  Phase 8 VP9 subset), confirmed 2026-11-08.
- Design: the `io` module from the Step 1 design is implemented as
  `byte_source.rs` (`BoundedReader`); `DecoderConfig` gains
  `pcm_format`, `pcm_channels`, and `pcm_sample_rate` so PCM
  decoding has a defined format source.
- `Decoder::probe` on an `OggS` signature now returns `Unsupported`
  ("ogg container: use OggDemuxer") instead of `Codec::Opus`;
  Ogg is a container and its codec is identified from the first
  packet by `OggDemuxer`.
- FLAC decoding converts samples to f32 via `i32`; 32-bit samples
  round below the f32 mantissa, which is inherent to the f32
  output format.
- `HuffmanTable` now uses the Vorbis codeword assignment
  (Vorbis I spec section 3.2.1; libvorbis `_make_words`):
  codewords are assigned in entry order via a marker array, not by
  sorting symbols by (length, symbol). Decode binary-searches the
  codewords of each length, because the Vorbis assignment does not
  make them consecutive. This table is used only by Vorbis; FLAC
  uses Rice codes, not Huffman codes.
- `mdct_forward` now carries the 2/N scale and `mdct_inverse`
  applies no scale, matching libvorbis (`mdct_forward` scales by
  4/n, `mdct_backward` is unscaled). The CELT IMDCT scales by 1/2
  (RFC 6716 section 4.3.7), so a CELT spectrum must be pre-scaled
  by 1/2 before calling `mdct_inverse`.
- The Ogg Opus mapping is RFC 7845, not RFC 8251 (RFC 8251 is the
  RTP payload format for Opus); code comments and docs cite
  RFC 7845.
- Phase 6: `Demuxer::from_bytes` takes a byte slice (`&[u8]`)
  instead of a `Vec<u8>`; the caller keeps ownership of the
  buffer. `MatroskaDemuxer` and `Mp4Demuxer` keep their own
  `parse(&[u8])` constructors.
- Phase 7: `parse_tile_group_obu` takes a `base_offset` so tile
  offsets and error offsets are reported relative to the enclosing
  frame OBU payload, not to the sliced tile data. `ObuType` gains
  `value()`, the 4-bit wire value (needed because `Padding` is
  type 15, not 9). The EBML helpers `read_element_id`,
  `read_vint_unknown`, `read_uint`, `read_sint`, `read_float`, and
  `read_string` are now re-exported from the crate root.
- Phase 7: `Decoder::probe` returns `Unsupported` for EBML and
  `ftyp` signatures (use `Demuxer` for containers) and `Ok(Av1)` /
  `Ok(Vp9)` for IVF `DKIF` with fourcc `AV01` / `VP90`.
  `Decoder::from_av1_sequence_header` validates the AV1 subset
  (profile 0, 8-bit, YUV420, no film grain) and returns
  `Unsupported` outside it.
- Phase 7: the Step 1 design's claim that `range_decoder.rs`
  covers the AV1 symbol decoder is outdated. The Opus range
  decoder (RFC 6716) and the AV1 symbol decoder (AV1 spec
  section 8.2) are different algorithms; AV1 uses the separate
  `Av1SymbolDecoder` in `av1_frame.rs`. See `docs/LIMITS.md`.

### Fixed

- MDCT kernel: the first implementation used the angle
  (2pi/N)(n + 1/2 + N/4)(k + 1/2), which is not a valid MDCT
  (non-orthogonal rows, TDAC fails). It now uses
  (pi/N)(n + 1/2 + N/2)(k + 1/2) with the cosine table indexed by
  m = (2n+1+N)(2k+1) mod 8N. Verified by exact TDAC overlap-add
  and by comparison with libvorbis.
- MDCT scale split: the inverse MDCT wrongly carried a 2/N scale,
  shrinking every decoded Vorbis block by 2/N (128x for a
  512-sample block, 512x for a 2048-block). The forward now
  carries 2/N and the inverse is unscaled, matching libvorbis.
- Vorbis Huffman codeword assignment: `HuffmanTable` used the
  standard canonical assignment (sort by length, then symbol),
  but Vorbis assigns codewords in entry order
  (libvorbis `_make_words`). The wrong table decoded the wrong
  codebook entries (e.g. mono packet 4 classword read as entry 30
  instead of 88), producing near-silence plus noise. Found by
  replaying the packet against the real libvorbis and comparing
  classwords bit for bit. The fix implements `_make_words` and
  pins it with a regression test on codebook 5 (entry 88 code 29,
  length 5; entry 30 code 51, length 8). Decoded Vorbis PCM now
  matches ffmpeg/libvorbis to within 1e-6.

- Three test expectations written from wrong assumptions, found by
  the test run: a single-symbol Huffman table is incomplete by
  construction (length 1 covers half the code space), a complete
  prefix table has no bit pattern that matches no code (the
  invalid-code test needs an incomplete table), and ffmpeg writes
  the Vorbis comment key as lowercase `encoder`, not `ENCODER`.
  The decoder was correct in all three cases; the tests were fixed.

- Phase 6 + 7, found by the first full test run (the new unit
  tests had never run before; `cargo check` and clippy do not
  codegen, so they had not caught these):
  - `read_sint` sign extension: the `i128` shift rewrite kept the
    shifted value positive, so `0xFF` read as 255 instead of -1.
    It now pads a big-endian 8-byte buffer with the sign bit.
  - `parse_frame_obu` returned tile offsets relative to the
    sliced tile data (0) instead of the frame OBU payload (10 for
    the key frame, 17 for the first inter frame). Fixed by
    threading a `base_offset` through `parse_tile_group_obu`.
  - `(1u8 << NUM_REF_FRAMES) - 1` overflows: shifting a `u8`
    left by 8 is a compile error under codegen. The all-refresh
    mask is now `u8::MAX`.
  - Matroska tag values kept a trailing NUL (ffmpeg pads
    `TagString` with one); they now go through `read_string`
    like tag names.
  - Four test expectations written from wrong assumptions, found
    by the test run: the signed-vint test used a width-2 vint
    (`0x40 0x00`) for a width-1 bias (-63 vs -8191); the Xiph
    lace test used `0xC2` for a +2 delta (it encodes +3, so the
    byte is `0xC1`); the MP4 box-header test declared a 24-byte
    box in a 12-byte buffer (a box overrunning the file is
    `Truncated`, so the buffer is now 24 bytes); and the IVF
    test helper read the file with a workspace-root-relative
    path, which fails when cargo runs tests from the package
    directory — it now uses `include_bytes!` like the other
    tests. The library was correct in each case except
    `read_sint` above.
  - Three AV1 test expectations confused ffmpeg's trace output
    with the spec's final values: the trace prints no
    `error_resilient_mode` for a shown key frame (the spec
    forces it to 1 without reading a bit), prints the raw
    `force_integer_mv` bit (0) before the spec's intra override
    (1), and prints no `allow_warped_motion` for an intra frame
    (the spec forces it to 0). The parser follows the spec in
    all three cases; the tests now assert the spec values.
  - CLI `info`: the IVF header stores the timebase denominator
    before the numerator (bytes 16–19 den, 20–23 num); the
    frame rate was inverted (0.1 fps instead of 10). MP4 `dOps`
    fields are big-endian while OpusHead fields are
    little-endian, and the `dOps` version (0) is not the
    OpusHead version (1); the reconstruction now converts both.
    `vpcC` is a FullBox: the 4 version/flags bytes precede the
    record, so profile/level/bit depth were read from the wrong
    offsets. WebM VP9 tracks carry no CodecPrivate, which is
    now reported as a note instead of an error.
  - On the final re-run after the docs were written, clippy
    (with `--all-targets -D warnings`) failed on
    `tests/av1.rs`: `for byte in flipped.iter_mut()` must be
    `for byte in &mut flipped` (`clippy::explicit_iter_loop`).
    The earlier clean run had not caught it; the test file is
    fixed and the full suite is green again.

### Fixed

- PCM I16/I24 encode used a different scale factor from decode
  (32767 / 8388607 vs 32768 / 8388608), so a decode-then-encode
  round-trip changed large-magnitude samples by one quantization
  step. Found by the `round_trip_pcm_i16_preserves_samples`
  integration test. Encode now uses the same power-of-two scale as
  decode and saturates f32 1.0 to the largest positive integer.
- Phase 10, C ABI length header: `alloc_with_length_header` /
  `codec_free` used `usize::to_le_bytes` / `from_le_bytes` for a
  fixed 8-byte length header, so the wasm32 build failed to
  compile (`usize` is 4 bytes on 32-bit targets) and the alloc
  side would have read past the array. Both sides now use
  fixed-width `u64`.
- Phase 10, WASM streaming encoder handles: `take_encoder` and
  `wasm_encoder_free` indexed the handle table with `handle`
  instead of `handle - 1` (handles are stored indices + 1, 0 is
  invalid). Fixed with `checked_sub(1)`; a unit test covers
  free-then-reuse and handle 0.
- Phase 11, three fuzz crashes (each fixed by the fix process:
  regression test first, root cause, fix, full re-run):
  - FLAC LPC prediction overflow: `restore_lpc` multiplied an
    i64 coefficient by a sample without checking, panicking on
    a crafted subframe. Prediction is now computed with a
    `checked_predict_term` helper (checked mul/add, returning
    `Error::Overflow`); `restore_fixed` was rewritten the same
    way as a coefficient-table loop. Regression test
    `fuzz_lpc_prediction_overflow_returns_error` with fixture
    `tests/data/flac_lpc_overflow.flac` (the fuzz artifact,
    kept whole — `cargo fuzz tmin` could not reduce it).
  - OpusHead family 1 sum overflow: `stream_count` 235 +
    `coupled_count` 43 overflowed u8 in the mapping-entry
    check. The comparison is widened to u16 and the real RFC
    7845 section 5.1.1 bound is enforced: the decoded channel
    count (M + N) MUST be no larger than 255. (The output
    channel count C is not constrained to match M + N — a first
    version of the fix wrongly required the sum to equal C,
    which rejects the valid existing test
    `opus_head_family_1_mapping`; the RFC text was checked and
    the fix corrected.) Mapping entry 255 stays allowed as a
    silent channel. Regression test
    `opus_head_family1_stream_count_sum_overflow_returns_error`
    with the 23-byte input inline in `tests/opus.rs`.
  - MP4 `stco`/`co64` allocation: the entry count (a u32 from
    the file) was passed straight to `Vec::with_capacity`,
    allocating ~2 GiB for a crafted box. Both parsers now
    check `count * entry_size` against the remaining box size
    before allocating and return `InvalidData`. Regression test
    `fuzz_stco_huge_count_returns_error` with fixture
    `tests/data/mp4_stco_huge_count.mp4` (the fuzz artifact,
    kept whole — `cargo fuzz tmin` could not reduce it).
  After the fixes, all three original crash inputs run clean
  through the rebuilt ASan fuzz binaries, and a 60 s re-fuzz
  of each target found no further crashes.

### Known limitations

- `WavDemuxer`/`WavMuxer` hold the whole file in memory up to
  `MAX_WAV_FILE_BYTES` (512 MiB); a streaming demuxer is a later phase.
- `resample` is linear interpolation only; no FIR resampler yet.
- `FlacDemuxer` finds frame boundaries by decoding each frame once;
  `Decoder` then decodes it again (double decode).
- `OggDemuxer` handles one logical stream only; chained or
  multiplexed Ogg streams return `Unsupported`. No seeking.
- `OggDemuxer` packet `timestamp` holds the granule position in
  samples, not microseconds, because Ogg does not know the sample
  rate; the caller converts with the codec's sample rate.
- FLAC decode has no seeking and no streaming; the whole file is
  bounded by `MAX_FLAC_FILE_BYTES` (512 MiB).
- AV1 is parser-only: OBU, sequence header, frame header, and
  tile-group framing parse and match ffmpeg's `trace_headers`
  output, but no pixels are reconstructed. `Decoder::decode` for
  `Codec::Av1` returns `Unsupported`. The subset is profile 0,
  8-bit, YUV420, no film grain; multi-tile frames are framed but
  not decoded, and no multi-tile test vector exists yet. See
  `docs/LIMITS.md`.
- VP9 is parser-only (Phase 8): frame headers, superframe
  indexes, and tile framing parse and match ffmpeg's
  `trace_headers` output and libvpx behaviour, but no pixels are
  reconstructed. `Decoder::decode` for `Codec::Vp9` returns
  `Unsupported`. The subset is profile 0, 8-bit, YUV420; the
  compressed header's updated probability values are not
  tracked (the spec's default probability tables are not
  ported). VP9 packets demux from WebM and MP4, and the CLI
  prints `vpcC` fields and VP90 IVF frame headers.
- WebM/MP4 demux: the Opus and Vorbis pre-skips are not applied
  by the demuxer (raw container timestamps are returned); the MP4
  edit list is parsed but not applied to timestamps;
  `DiscardPadding`, `BlockDuration`, `ReferenceBlock`, and
  `CodecState` are skipped, not applied. FLAC in MP4 is parsed but
  unverified (ffmpeg cannot produce it, so no test vector
  exists). Seeking without Cues falls back to scanning packets.
- Opus is partial: CELT sample reconstruction (coarse/fine energy,
  bit allocation, PVQ, inverse MDCT, post-filter, de-emphasis) and
  SILK decode are not implemented. The CELT allocation and PVQ
  tables exist only inside the reference C source, which RFC 6716
  Appendix A ships as a base64-encoded gzipped tarball that cannot
  be extracted from the specification text; porting them from the
  reference source is follow-up work. CELT silence frames decode
  to silence, and `conceal_loss()` returns silence PLC. Channel
  mapping families 1 and 255 return `Unsupported`.
- Vorbis floor type 0 parses its header but packet decode returns
  `Unsupported`; floor type 1 is fully implemented. Vorbis has no
  PLC; a lost packet is skipped and the next overlap-add uses the
  previous tail.
- No FFI crate or bindings yet (Phase 10).
