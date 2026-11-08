# Test vectors

All files in this directory were generated locally for this project and
are released under CC0 (public domain). No third-party test vector is
used in Phase 0 + Phase 1.

## Generation

Generated with CPython 3.12 `wave` module (integer PCM) and manual
RIFF construction (float PCM), from a 440 Hz sine wave. The generator
is not checked in; the method is recorded here so any vector can be
regenerated:

- `valid_i16_stereo_44100.wav`: 4410 frames, stereo, 16-bit, 44100 Hz.
- `valid_i16_mono_8000.wav`: 800 frames, mono, 16-bit, 8000 Hz.
- `valid_i24_stereo_48000.wav`: 480 frames, stereo, 24-bit, 48000 Hz.
- `valid_f32_mono_44100.wav`: 4410 frames, mono, 32-bit float, 44100 Hz,
  built by hand because `wave` cannot write float samples.
- `truncated_header.wav`: first 20 bytes of the valid stereo file.
- `truncated_data.wav`: header plus only 4410 of 17640 data bytes.
- `declared_too_large.wav`: valid stereo file with the `data` chunk size
  field changed to 10,000,000.
- `bad_magic.wav`: valid stereo file with `RIFF` replaced by `RIFX`.

## Phase 2 + Phase 3: FLAC and Ogg vectors

FLAC and Ogg files below were encoded from the local WAV vectors with
ffmpeg 8.1.2 (Lavc FLAC encoder, Lavf Ogg muxer), which is the
reference encoder for this phase. The source audio is the local
CC0 sine/noise/ramp WAV vectors, so the FLAC files themselves are
CC0. Generation commands (run from this directory):

```text
ffmpeg -y -i valid_i16_stereo_44100.wav -c:a flac -compression_level 5 sine_stereo_44100.flac
ffmpeg -y -i valid_i16_stereo_44100.wav -c:a flac -compression_level 0 sine_stereo_fixed.flac
ffmpeg -y -i silence_mono_8000.wav -c:a flac -compression_level 5 silence_mono_8000.flac
ffmpeg -y -i noise_mono_8000.wav -c:a flac -compression_level 5 noise_mono_8000.flac
ffmpeg -y -i ramp_mono_8000.wav -c:a flac -compression_level 5 ramp_mono_lpc.flac
ffmpeg -y -i ramp_mono_8000.wav -c:a flac -compression_level 0 ramp_mono_fixed.flac
ffmpeg -y -i valid_i16_stereo_44100.wav -c:a flac -compression_level 5 sine_stereo_44100.ogg
```

What each file exercises:

- `sine_stereo_44100.flac`: one LPC frame (order 8), left/side stereo.
- `sine_stereo_fixed.flac`: fixed-predictor frames, including mid/side
  stereo frames (compression level 0).
- `silence_mono_8000.flac`: constant subframes (all-zero audio).
- `noise_mono_8000.flac`: verbatim subframes (white noise).
- `ramp_mono_lpc.flac`: LPC subframes on a linear ramp.
- `ramp_mono_fixed.flac`: fixed-predictor subframes on a linear ramp.
- `sine_stereo_44100.ogg`: FLAC-in-Ogg, 3 pages, granule 4410.
- `silence_mono_8000.wav`, `noise_mono_8000.wav`, `ramp_mono_8000.wav`:
  the mono 8000 Hz sources the FLAC files above were encoded from
  (CPython `wave`, random module seeded with 42 for the noise).

Corrupt and truncated cases are built in the tests by mutating these
files in memory, not stored as separate files.

Standard conformance vectors (RFC 6716 Opus, libaom and libvpx
vectors) are added in their respective phases, with origin and license
noted here when they arrive.

## Phase 4 + Phase 5: Opus and Vorbis vectors

Opus and Vorbis files below were encoded from the local WAV vectors
with ffmpeg 8.1.2 (libopus, libvorbis), which are the reference
encoders for these phases. The source audio is the local CC0
sine/noise/ramp WAV vectors, so the encoded files themselves are
CC0. Generation commands (run from this directory):

```text
ffmpeg -y -i valid_i16_stereo_44100.wav -c:a libopus -b:a 96k sine_stereo_44100.opus
ffmpeg -y -i valid_i16_mono_8000.wav -c:a libopus -b:a 32k sine_mono_8000.opus
ffmpeg -y -i valid_i16_stereo_44100.wav -c:a libvorbis -q:a 5 sine_stereo_44100_vorbis.ogg
ffmpeg -y -i valid_i16_mono_8000.wav -c:a libvorbis -q:a 5 sine_mono_8000_vorbis.ogg
ffmpeg -y -i noise_mono_8000.wav -c:a libvorbis -q:a 5 noise_mono_8000_vorbis.ogg
ffmpeg -y -i ramp_mono_8000.wav -c:a libvorbis -q:a 5 ramp_mono_lpc_vorbis.ogg
```

What each file exercises:

- `sine_stereo_44100.opus`: libopus, 96 kbps, 8 packets; OpusHead
  version 1, 2 channels, pre-skip 312, rate 48000, gain 0, mapping
  family 0; audio packets TOC 0xfc (config 31, CELT-only,
  fullband, 20 ms), stereo, code 0.
- `sine_mono_8000.opus`: libopus, 32 kbps; OpusHead 1 channel,
  pre-skip 312, rate 8000, mapping family 0; TOC 0x98 (config 19,
  CELT-only, narrowband, 20 ms), mono, code 0.
- `sine_stereo_44100_vorbis.ogg`: libvorbis q5; identification
  blocksize 256/2048, setup 4225 bytes, 10 packets, 42 codebooks,
  2 floors, 2 residues, 2 mappings, 2 modes.
- `sine_mono_8000_vorbis.ogg`: libvorbis q5; blocksize 512/512,
  setup 2476 bytes, 8 packets, 19 codebooks, 1 floor, 1 residue,
  1 mapping, 1 mode.
- `noise_mono_8000_vorbis.ogg`: libvorbis q5; blocksize 512/512.
- `ramp_mono_lpc_vorbis.ogg`: libvorbis q5; blocksize 512/512.

Measured PCM agreement (2026-11-08): decoded Vorbis PCM matches
ffmpeg/libvorbis decode of the same file to within 1e-6 (f32
rounding). Against the original WAV the lossy difference is stereo
RMS 0.0059 / max 0.016, mono RMS 0.0103 / max 0.039. Opus `decode`
honestly fails with `Unsupported` naming CELT sample reconstruction;
no Opus PCM is produced in this phase.

## Phase 6 + Phase 7: WebM/MP4 and AV1 vectors

Video is `testsrc2` at 64x64, 10 fps, 1 s; audio is a local sine
WAV. All files were produced with ffmpeg 8.1.2 (libaom-av1,
libvpx-vp9, libopus, libvorbis) and are CC0 like the rest of this
directory. The commands below reproduce equivalent files (same
streams, dimensions, frame counts, and keyframe positions); the
checked-in files are the pinned vectors the tests assert against,
and re-running the commands is not guaranteed to be byte-identical
(the original encoder settings were not recorded with full
fidelity — the default libaom-av1 encode of the same source is
3093 bytes, the pinned `av1_64x64.ivf` is 3559).

```text
ffmpeg -y -f lavfi -i testsrc2=size=64x64:rate=10:duration=1 -c:v libaom-av1 av1_64x64.ivf
ffmpeg -y -f lavfi -i testsrc2=size=64x64:rate=10:duration=1 -i sine_stereo_44100.wav -map 0:v -map 1:a -c:v libaom-av1 -c:a libopus -shortest av1_opus.webm
ffmpeg -y -f lavfi -i testsrc2=size=64x64:rate=10:duration=1 -i sine_stereo_44100.wav -map 0:v -map 1:a -c:v libaom-av1 -c:a libopus -movflags +faststart -shortest av1_opus.mp4
ffmpeg -y -f lavfi -i testsrc2=size=64x64:rate=10:duration=1 -c:v libvpx-vp9 -an vp9_64x64.webm
ffmpeg -y -f lavfi -i testsrc2=size=64x64:rate=10:duration=1 -i sine_stereo_44100.wav -map 0:v -map 1:a -c:v libvpx-vp9 -c:a libvorbis -shortest vp9_vorbis.webm
ffmpeg -y -f lavfi -i testsrc2=size=64x64:rate=10:duration=1 -i sine_stereo_44100.wav -map 0:v -map 1:a -c:v libvpx-vp9 -c:a libopus -movflags +faststart -shortest vp9_opus.mp4
ffmpeg -y -f lavfi -i testsrc2=size=64x64:rate=10:duration=1 -i <1s sine>.wav -map 0:v -map 1:a -c:v libvpx-vp9 -c:a libopus -force_key_frames expr:gte(t,n_forced*0.3) -cluster_time_limit 200 -shortest vp9_keyframes.webm
ffmpeg -y -f lavfi -i testsrc2=size=64x64:rate=10:duration=1 -i <1s sine>.wav -map 0:v -map 1:a -c:v libaom-av1 -c:a libopus -force_key_frames expr:gte(t,n_forced*0.3) -movflags +faststart -shortest av1_keyframes.mp4
ffmpeg -y -f lavfi -i testsrc2=size=512x64:rate=10:duration=1 -c:v libvpx-vp9 -tile-columns 1 -tile-rows 1 -an vp9_tiles.webm
```

`vp9_64x64.ivf` is `vp9_64x64.webm` re-muxed with
`ffmpeg -i vp9_64x64.webm -c:v copy vp9_64x64.ivf`.
`vp9_superframe.ivf` is synthetic: frame 0 (1729 bytes) and frame 1
(426 bytes) of `vp9_64x64.ivf`, concatenated with a spec Appendix B
superframe index (marker `0b110`, 1 size byte per frame,
little-endian sizes 1729/426); ffmpeg decodes it as 2 frames, and
rejects the same index with big-endian sizes. It is not a natural
libvpx output — no natural superframe could be produced even with
`-lag-in-frames 25 -auto-alt-ref 1`.

`av1_seq_header.obu` is the 12-byte sequence header OBU cut out
of frame 0 of `av1_64x64.ivf` (temporal delimiter, then the OBU),
not a separate encode.

What each file exercises:

- `av1_64x64.ivf`: 10 frames (IVF sizes 779, 1439, 5, 197, 5, 441,
  5, 185, 5, 346). Frame 0: temporal delimiter, sequence header
  (profile 0, 64x64, order_hint_bits 7, bit depth 8, YUV420),
  key frame (base_q 86, tile offset 10, size 752). Frame 1 holds
  four inter frames (show 0,0,0,1; base_q 98, 106, 112, 120).
  Frames 2, 4, 6, 8 are 5-byte show_existing frame headers.
- `av1_opus.webm`: 16 packets (6 Opus + 10 AV1). Audio track
  `A_OPUS`, 48000 Hz, 2 channels, OpusHead pre-skip 312; raw
  container timestamps 0, 21, 41, 61, 81, 101 ms (ffprobe shows
  them 7 ms earlier because it applies the pre-skip). Video track
  `V_AV1`, 64x64, DefaultDuration 100 ms; video timestamps
  0..900 ms in 100 ms steps; only the first frame is a keyframe.
  The WebM CodecPrivate for AV1 is the 16-byte
  AV1CodecConfigurationRecord.
- `av1_opus.mp4`: the same content in MP4 (faststart: `mdat`
  before `moov`). Audio `Opus` entry with 11-byte `dOps`
  (pre-skip 312, input rate 48000); edit list (100, media_time
  312) carries the pre-skip. Video `av01` entry with 16-byte
  `av1C` (same config OBUs as the WebM CodecPrivate). Audio
  timestamps are on the 20 ms grid (0..100 ms); video 0..900 ms.
- `vp9_64x64.webm`: 10 VP9 packets, 0..900 ms, sizes 1729, 426,
  444, 399, 568, 508, 504, 591, 629, 816; first frame is a keyframe.
- `vp9_vorbis.webm`: 17 packets (7 Vorbis + 10 VP9, same video as
  `vp9_64x64.webm`). Audio `A_VORBIS`, 44100 Hz, 2 channels;
  CodecPrivate is the WebM Vorbis header table (ident 30 bytes,
  comment 93 bytes, setup 4225 bytes). Audio timestamps 0, 3, 6,
  19, 42, 65, 89 ms.
- `vp9_opus.mp4`: the same VP9 video + Opus audio in MP4
  (faststart). Video `vp09` entry with 12-byte `vpcC` (a FullBox:
  version 1, then profile 0, level 10, bit depth 8, chroma 1).
- `vp9_keyframes.webm`: 61 packets (51 Opus mono + 10 VP9).
  Video keyframes at 0, 300, 600, 900 ms; 5 clusters; Cues at 0,
  300, 600, 900 ms. Audio is mono (1 channel) in this file.
- `vp9_tiles.webm`: 10 VP9 packets of testsrc2 at 512x64, 10 fps,
  1 s, encoded with `-tile-columns 1 -tile-rows 1` (2 tiles per
  frame: tile_cols_log2 1, tile_rows_log2 0). Frame 0 is 3787
  bytes (header_size 137, tile sizes 1900/1728, big-endian size
  field); frame 1 is 855 bytes (header_size 20, tiles 276/545).
  The compressed header of frame 0 is the vector that exposed
  the `decode_term_subexp` bug (see `docs/LIMITS.md`).
- `vp9_superframe.ivf`: 1 IVF unit holding a synthetic 2-frame
  superframe (frames 0 and 1 of `vp9_64x64.ivf`, 1729 + 426
  bytes). `parse_superframe` splits it into `(0, 1729)` and
  `(1729, 426)`; both parts are byte-identical to the original
  frames.
- `vp9_64x64.ivf`: the 10 frames of `vp9_64x64.webm` re-muxed
  (frame sizes 1729, 426, 444, 399, 568, 508, 504, 591, 629,
  816). Frame 0 is the key frame (base_q 33, loop_filter_level
  3, header_size 63); frames 1–9 are inter frames (base_q 74,
  86, 78, 69, 60, 55, 50, 46, 44; header_size 14, 5, 3, 3, 3, 3,
  10, 5, 11). All 10 headers are asserted against ffmpeg's
  `trace_headers` output in `tests/vp9.rs`.
- `av1_keyframes.mp4`: 61 packets (51 Opus mono + 10 AV1).
  Video sync samples at 0, 100, 300, 400, 600, 700, 900 ms —
  libaom emits an extra sync sample after each forced keyframe.
  Audio is mono (1 channel) in this file.

Verified against ffprobe (2026-11-08): stream counts, codecs,
dimensions, sample rates, durations, and packet timestamps above
match `ffprobe -show_streams` / `-show_packets` for these files,
except that ffprobe applies the Opus pre-skip to WebM audio
timestamps (7 ms earlier) and the Vorbis pre-skip to WebM Vorbis
timestamps, while the demuxer returns the raw container
timestamps. No official libaom/libvpx conformance vectors are
used; these are ffmpeg encodes of local CC0 sources.

## Phase 9: FLAC encode (no new fixture files)

The encode tests (`tests/flac_encode.rs`) generate their samples
in code (sine, silence, deterministic noise, ramp) and round-trip
them through the Phase 2 decoder, so no new files are pinned
here. Externally, the CLI output was checked with ffmpeg 8.1.2:

```text
codec-whatttttttttttt encode valid_i16_stereo_44100.wav out.flac
ffmpeg -i out.flac out.wav   # byte-identical to the input WAV
```

The same check was done for a 2-second stereo file (88200
frames, 22 FLAC frames) and for the CLI `decode` of the CLI
`encode` output; all three are byte-identical to the original
PCM.

## Phase 11: fuzz fixtures (kept whole, not reduced)

- `flac_lpc_overflow.flac` (8789 bytes): the
  `fuzz_flac` crash input from the first 60 s fuzz run
  (2026-11-08). An LPC subframe whose prediction overflows i64
  in `restore_lpc`; before the fix the decoder panicked, after
  it the decoder returns `Error::Overflow`. `cargo fuzz tmin`
  could not reduce it, so the artifact is kept whole.
- `mp4_stco_huge_count.mp4` (12406 bytes): the `fuzz_mp4`
  crash input from the first 60 s fuzz run (2026-11-08). A
  `stco` box declaring ~268M chunk offsets made the demuxer
  pre-allocate ~2 GiB and hit the ASan allocator limit; before
  the fix the demuxer allocated from the file-controlled count
  and then returned `Truncated`, after it the demuxer returns
  `InvalidData` without allocating. `cargo fuzz tmin` could not
  reduce it, so the artifact is kept whole.

The Opus fuzz crash input (23-byte `OpusHead`, family 1,
stream_count 235 + coupled_count 43) is small enough to live
inline in `tests/opus.rs`
(`opus_head_family1_stream_count_sum_overflow_returns_error`),
so it has no fixture file.
