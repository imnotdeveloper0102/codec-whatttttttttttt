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

## Next phase: passthrough + container fixtures

Video is `testsrc2` at 64x64, 10 fps, 1 s; audio is a local
sine AAC (44100 Hz, mono). All files were produced with
ffmpeg 8.1.2 from local CC0 sources. The exact ffmpeg
command lines were recorded only in the worker task report
and are not checked in here; the properties below were
verified with `ffprobe` on the checked-in files
(2026-11-08/09), and the packet counts are the values the
integration tests assert (`tests/passthrough.rs`,
`tests/mpeg_ts.rs`, `tests/mp4_tag.rs`).

- `avc_aac.mp4` (7990 bytes): H.264 64x64 10 fps +
  AAC 44100 Hz mono. 55 packets (10 AVC + 45 AAC).
  `meta`/`ilst` tags: title "Passthrough AVC", artist
  "Test Artist", album "Test Album". The `avc1` sample
  entry's `avcC` and the `mp4a` entry's `esds` are the
  passthrough `codec_private` blobs.
- `hevc_aac.mp4` (9384 bytes): HEVC 64x64 10 fps +
  AAC 44100 Hz mono. 55 packets (10 HEVC + 45 AAC).
  The `hev1` sample entry's `hvcC` is the passthrough
  `codec_private` blob.
- `avc_aac.mkv` (7125 bytes): the same streams in
  Matroska (`V_MPEG4/ISO/AVC` + `A_AAC`). 55 packets
  (10 AVC + 45 AAC).
- `avc_aac.ts` (12220 bytes): the same streams in
  MPEG-TS: 65 TS packets of 188 bytes, 13 PES packets
  (10 video + 3 audio). One audio PES holds many ADTS
  frames — ffmpeg packs ~15 AAC frames per PES — so the
  demuxer must split ADTS frames out of a PES payload,
  not assume one frame per PES.
- `sine_aac.m4a` (5143 bytes): AAC 44100 Hz mono, 45
  frames, 1.000 s, with an `ilst` title tag "M4A Title".
  ffmpeg's MP4 muxer writes an edit list that skips the
  first AAC frame (encoder priming), so the
  `_m4a_ref.raw` reference starts at stream frame 1.

## Next phase: ADTS and AIFF fixtures

- `sine_aac.adts` (4438 bytes): AAC-LC ADTS, 44100 Hz,
  mono, 45 frames, 1.045 s, `protection_absent` set (no
  CRC). First frame is 160 bytes (7-byte header +
  153-byte payload). Encoded by ffmpeg 8.1.2 from a
  local CC0 sine source; exact command not checked in.
- `sine_aac_crc.adts` (162 bytes): one ADTS frame with
  CRC (`protection_absent` clear, 9-byte header). Built
  locally from the ffmpeg frame in `sine_aac.adts` by
  appending a Python-computed CRC-16 (polynomial
  `0x8005`, init `0xFFFF`); see `tests/adts.rs`.
- `sine_aiff.aiff` (88254 bytes): PCM s16be, 44100 Hz,
  mono, 44100 frames, 88200 data bytes. Encoded by
  ffmpeg 8.1.2 from a local CC0 sine source; exact
  command not checked in.
- `sine_aifc.aifc` (88272 bytes): the same audio as
  AIFF-C with `sowt` compression (little-endian
  samples), so the demuxer must byte-swap. Encoded by
  ffmpeg 8.1.2 from a local CC0 sine source; exact
  command not checked in.

## Next phase: MP3 fixtures

MP3 files encoded with ffmpeg 8.1.2 (libmp3lame) from
locally generated WAVs (0.5 s sine, 1 s sweep/transient;
the source WAVs are not checked in). The reference
`.ref.raw` files (note the dot: `*.ref.raw`, unlike the
AAC/ALAC `_ref.raw` suffix) are ffmpeg's own decode
(`-f f32le`) of the same files. The exact ffmpeg command
lines were recorded only in the worker task report and
are not checked in here.

Conformance thresholds (ISO/IEC 11172-4 section 2.6.3):
full compliance is RMS < 8.80967e-06 with max abs diff
<= 6.10352e-05. Measured 2026-11-08 (all at alignment
offset 2257 samples): mono 96k RMS 1.84e-7 / max
1.13e-6; stereo 128k RMS 2.69e-7 / max 1.91e-6; stereo
64k RMS 2.68e-7 / max 1.37e-6; stereo 48000 320k RMS
2.74e-7 / max 1.28e-6; sweep 128k RMS 6.44e-8 / max
6.26e-7; transient 128k RMS 1.98e-8 / max 6.56e-7.
Every fixture passes with at least 30x headroom. The
decoder outputs raw frames including the encoder-delay
priming samples; ffmpeg's decode trims that delay, so
the comparison aligns by searching the offset (in
samples per channel) that minimises the RMS difference
— around 2257: the 1152-sample Xing/LAME info frame
plus the 576-sample encoder delay plus ffmpeg's
528-sample decoder delay (measured, not assumed). See
`tests/mp3.rs`.

- `sine_mono_44100_96k.mp3` (6941 bytes): mono,
  44100 Hz, 96 kbps, 0.5 s; ref 88200 bytes
  (22050 f32 samples).
- `sine_stereo_44100_128k.mp3` (9239 bytes): stereo,
  44100 Hz, 128 kbps, 0.5 s; ref 176400 bytes
  (22050 frames).
- `sine_stereo_44100_64k.mp3` (4641 bytes): stereo,
  44100 Hz, 64 kbps, 0.5 s; ref 176400 bytes.
- `sine_stereo_48000_320k.mp3` (22125 bytes): stereo,
  48000 Hz, 320 kbps, 0.5 s; ref 192000 bytes
  (24000 frames).
- `sweep_stereo_44100_128k.mp3` (17180 bytes): stereo,
  44100 Hz, 128 kbps, 1.0 s sine sweep; ref 352800
  bytes (44100 frames).
- `transient_stereo_44100_128k.mp3` (9239 bytes):
  stereo, 44100 Hz, 128 kbps, 0.5 s transient signal;
  ref 176400 bytes.
- `sine_stereo_44100_layer2.mp2` (8359 bytes): Layer II
  (MP2), stereo, 44100 Hz, 128 kbps, 0.522 s. Negative
  fixture: Layer I/II decode is `Unsupported`, so this
  file has no reference PCM.

## Next phase: AAC fixtures

AAC-LC files encoded by ffmpeg 8.1.2 from local CC0
WAV sources. The reference `_ref.raw` files are
ffmpeg's own decode (`-f f32le`) of the same files.
The exact ffmpeg command lines were recorded only in
the worker task report and are not checked in here.

Thresholds (ISO/IEC 13818-4 conformance style, stated
in `tests/aac.rs`): max absolute sample error 2.0e-3,
RMS error 5.0e-4. Measured 2026-11-08: worst case max
abs 7.9e-5 / RMS 3.0e-6 (burst, 128k, short windows);
all other fixtures measure max abs <= 3e-6 and RMS <=
2e-6. The `.m4a` references start at stream frame 1
(ffmpeg's edit list skips the priming frame; see the
passthrough section above).

- `aac_stereo_44100_128k.aac` (1893 bytes): stereo,
  44100 Hz, 128 kbps, 6 frames (0.139 s); ref 49152
  bytes (6144 frames).
- `aac_stereo_44100_64k.aac` (918 bytes): stereo,
  44100 Hz, 64 kbps, 6 frames; ref 49152 bytes.
- `aac_stereo_44100_320k.aac` (3526 bytes): stereo,
  44100 Hz, 320 kbps, 6 frames; ref 49152 bytes.
- `aac_stereo_48000_96k.aac` (411 bytes): stereo,
  48000 Hz, 96 kbps, 2 frames (0.043 s); ref 16384
  bytes (2048 frames).
- `aac_mono_8000_64k.aac` (1153 bytes): mono, 8000 Hz,
  64 kbps, 2 frames; ref 8192 bytes (2048 samples).
  (ffprobe on this box cannot probe raw ADTS — its
  ffmpeg build has no ADTS demuxer — so the rate and
  channel count here come from the test asserts in
  `tests/aac.rs`, not from ffprobe.)
- `aac_tone_44100_128k.aac` (16129 bytes): stereo,
  44100 Hz, 128 kbps, 45 frames (1.045 s), pure tone;
  ref 368640 bytes (46080 frames). Source WAV
  `aac_tone_stereo_44100.wav` (checked in: 1 s stereo
  16-bit 44100 Hz).
- `aac_burst_44100_128k.aac` (16017 bytes): stereo,
  44100 Hz, 128 kbps, 45 frames, burst signal chosen
  to force short windows (`EIGHT_SHORT`); ref 368640
  bytes. Source WAV `aac_burst_stereo_44100.wav`
  (checked in: 1 s stereo 16-bit 44100 Hz).
- `aac_stereo_44100_128k.m4a` (2682 bytes): stereo,
  44100 Hz, 128 kbps, 0.100 s; ref
  `aac_stereo_44100_128k_m4a_ref.raw` 40960 bytes
  (5120 frames, starting at stream frame 1).
- `aac_stereo_48000_96k.m4a` (1212 bytes): stereo,
  48000 Hz, 96 kbps, 0.010 s (1 frame after priming);
  ref `aac_stereo_48000_96k_m4a_ref.raw` 8192 bytes
  (1024 frames).
- `aac_mono_8000_64k.m4a` (1954 bytes): mono, 8000 Hz,
  64 kbps, 0.100 s; ref `aac_mono_8000_64k_m4a_ref.raw`
  4096 bytes (1024 samples, starting at stream
  frame 1).

## Next phase: ALAC fixtures

ALAC files encoded by ffmpeg 8.1.2 (native ALAC
encoder) in `.m4a` from the existing WAV fixtures in
this directory. Decoding must be bit-exact: ALAC is
lossless, so every integer sample must equal ffmpeg's
decode of the same file, sample for sample. The
reference `_ref.raw` files are ffmpeg's own decode
(`-f s16le` / `-f s32le`), checked in alongside the
fixtures. The exact ffmpeg command lines were recorded
only in the worker task report and are not checked in
here.

- `alac_mono_8000.m4a` (1789 bytes): mono, 8000 Hz,
  0.100 s (800 samples), 16-bit; source
  `valid_i16_mono_8000.wav`; ref 1600 bytes s16le.
- `alac_mono_8000_s32.m4a` (2589 bytes): mono, 8000 Hz,
  0.100 s, 32-bit source (the 32-bit source WAV is not
  checked in); ffmpeg's ALAC encoder writes 24-bit
  ALAC for s32 input; ref 3200 bytes s32le.
- `alac_noise_mono_8000.m4a` (2335 bytes): mono,
  8000 Hz, 0.100 s, white noise; source
  `noise_mono_8000.wav`; ref 1600 bytes s16le.
- `alac_ramp_mono_8000.m4a` (840 bytes): mono, 8000 Hz,
  0.100 s, linear ramp; source `ramp_mono_8000.wav`;
  ref 1600 bytes s16le.
- `alac_stereo_44100.m4a` (4375 bytes): stereo,
  44100 Hz, 0.100 s (4410 frames), 16-bit; source
  `valid_i16_stereo_44100.wav`; ref 17640 bytes s16le.
- `alac_stereo_44100_s32.m4a` (13195 bytes): stereo,
  44100 Hz, 0.100 s, 32-bit source (the 32-bit source
  WAV is not checked in); ref 35280 bytes s32le.

## Next phase: ID3 fixture

- `tagged.mp3` (4516 bytes): MP3 mono, 44100 Hz,
  32 kbps, 1.000 s, from a local CC0 sine source, with
  ID3v2.3 tags: title "ID3 Title", artist "ID3 Artist",
  album "ID3 Album", track 3, year "2026", genre "Rock".
  Produced with ffmpeg 8.1.2; the exact command was
  recorded only in the worker task report and is not
  checked in here. ID3v1 vectors are built in code in
  `tests/id3.rs` (`write_id3v1` round-trip), not stored
  as files.

## Next phase: DSD fixtures

The eight `dsf_dsd{64,128,256,512}_stereo.dsf` /
`dff_dsd{64,128,256,512}_stereo.dff` files were
generated by `/tmp/gen_dsd_fixtures.py` (kept outside
the repo; the algorithm is re-implemented in Rust in
`tests/dsd.rs` for the in-memory variants). Each holds
0.1 s of stereo audio: left 1000 Hz sine, right
1500 Hz sine, amplitude 0.5, produced by a first-order
sigma-delta modulator written for the fixtures. No
real DSD recording was used, and no "looks like DSD"
bytes were invented: every bit comes out of the
modulator from the known sine.

- `dsf_dsd64_stereo.dsf` (73820 bytes) / `dff_dsd64_stereo.dff`
  (70708 bytes): DSD64, 2.8224 MHz.
- `dsf_dsd128_stereo.dsf` (147548 bytes) / `dff_dsd128_stereo.dff`
  (141268 bytes): DSD128, 5.6448 MHz.
- `dsf_dsd256_stereo.dsf` (286812 bytes) / `dff_dsd256_stereo.dff`
  (282388 bytes): DSD256, 11.2896 MHz.
- `dsf_dsd512_stereo.dsf` (565340 bytes) / `dff_dsd512_stereo.dff`
  (564628 bytes): DSD512, 22.5792 MHz.

ffmpeg 8.1.2 has DSD *decoders* but no PCM-to-DSD
encoder (`ffmpeg -encoders | grep -i dsd` returns
nothing), so no reference encoder could have produced
these files. Independent cross-check: ffmpeg 8.1.2
decodes the DFF fixtures to exactly 0.500 amplitude on
both channels (measured 2026-11-08), which validates
the modulator and the MSB-first packing. ffmpeg's DSF
path does NOT decode these DSF files (it returns noise
for both the spec-correct block layout and a
byte-interleaved variant, and a DC test file decodes to
toggling +/-1), so ffmpeg is not used as the DSF
reference: the DSF layout is validated against the Sony
DSF spec text (block size per channel 4096, LSB-first)
and the sacd-ripper/PipeWire implementations instead.
The fixtures are synthetic data generated for this
project (no third-party audio), so there is no
third-party license to record; they are covered by the
workspace license like any other test vector.

## Final round (2026-11-09): small items, encoders, PCM→DSD, CELT, VP9, AV1

All fixtures below are synthetic data generated for this
project (no third-party audio); they are covered by the
workspace license like any other test vector. Where a
worker's exact generation command was recorded, it is
quoted; where it was not recorded, the file's properties
are stated instead and no command is invented.

### MP3-in-TS, FLAC wide

- `mp3ts_sine_stereo_44100_128k.ts` (20868 bytes) +
  `mp3ts_sine_stereo_44100_128k_ref.raw` (368640 bytes):
  MPEG-TS carrying MP3 Layer III (sine stereo, 44100 Hz,
  128 kbps), encoded with ffmpeg 8.1.2; the reference is
  ffmpeg's decode of the same file (`-f f32le`). Exact
  command not recorded.
- `flac24_sine_stereo_44100.flac` (32799 bytes) +
  `flac24_sine_stereo_44100_ref.raw` (176400 bytes): 24-bit
  stereo FLAC, 44100 Hz, encoded with ffmpeg 8.1.2; the
  reference is ffmpeg's decode. Exact command not recorded.
- `flac51_sine_44100.flac` (203069 bytes) +
  `flac51_sine_44100_ref.raw` (529200 bytes): 5.1-channel
  FLAC, 44100 Hz, encoded with ffmpeg 8.1.2; the reference
  is ffmpeg's decode. Exact command not recorded.
- `flac20_verbatim_mono_8000.flac` (92 bytes) +
  `flac20_verbatim_mono_8000_ref.raw` (64 bytes): 20-bit
  mono FLAC, 8000 Hz, hand-crafted (ffmpeg cannot emit
  20-bit FLAC); the reference is the known PCM (verbatim
  subframe).

### MP3 encode

Source WAVs are sine tones made with ffmpeg 8.1.2
(`lavfi sine` + `volume` + `pcm_s16le`; exact commands
were not recorded):

- `mp3enc_sine_mono_22050.wav` (44178 bytes),
  `mp3enc_sine_mono_44100.wav` (88278 bytes),
  `mp3enc_sine_stereo_44100.wav` (176478 bytes),
  `mp3enc_tones_mono_44100.wav` (88278 bytes).
- `mp3enc_sine_stereo_44100_128k.mp3` (16263 bytes) is the
  library encoder's own output at CBR 128 kbps, and
  `mp3enc_sine_stereo_44100_128k_ref.raw` (359424 bytes) is
  ffmpeg's decode of it (`-f f32le`).

### AAC encode

Source WAVs made with ffmpeg 8.1.2:

- `ffmpeg -f lavfi -i "sine=frequency=440:sample_rate=44100:duration=2" -f lavfi -i "sine=frequency=660:sample_rate=44100:duration=2" -filter_complex "[0:a][1:a]amerge=inputs=2[a]" -map "[a]" -c:a pcm_s16le aacenc_sine_44100_stereo.wav` (352878 bytes, stereo 44100 Hz)
- `ffmpeg -f lavfi -i "sine=frequency=440:sample_rate=8000:duration=2" -c:a pcm_s16le aacenc_sine_8000_mono.wav` (32078 bytes, mono 8000 Hz)

The `.adts` files are the library encoder's own output
(test `generate_aacenc_fixtures`, run with `-- --ignored`),
and the `_ffdec.raw` files are ffmpeg's decode of them
(`ffmpeg -i <adts> -f f32le -acodec pcm_f32le <raw>`):

- `aacenc_sine_44100_stereo.adts` (12747 bytes, 87 frames)
  + `aacenc_sine_44100_stereo_ffdec.raw` (712704 bytes)
- `aacenc_sine_8000_mono.adts` (2462 bytes, 16 frames) +
  `aacenc_sine_8000_mono_ffdec.raw` (65536 bytes)

### ALAC encode

Source WAVs are sine/noise tones made with ffmpeg 8.1.2
(exact commands not recorded); the `.m4a` files are the
library encoder's own output and the `_ref.raw` files are
ffmpeg's decode of them (`-f f32le`):

- `alacenc_sine_mono_8000.wav` (8236 bytes) /
  `alacenc_sine_mono_8000.m4a` (7287 bytes) /
  `alacenc_sine_mono_8000_ref.raw` (8192 bytes): 16-bit mono
- `alacenc_sine_mono_8000_24bit.wav` (12332 bytes) /
  `alacenc_sine_mono_8000_24bit.m4a` (13019 bytes) /
  `alacenc_sine_mono_8000_24bit_ref.raw` (16384 bytes):
  24-bit mono
- `alacenc_noise_mono_8000.wav` (8236 bytes) /
  `alacenc_noise_mono_8000.m4a` (8923 bytes) /
  `alacenc_noise_mono_8000_ref.raw` (8192 bytes): 16-bit mono
  noise
- `alacenc_sine_stereo_44100.wav` (32812 bytes) /
  `alacenc_sine_stereo_44100.m4a` (18740 bytes) /
  `alacenc_sine_stereo_44100_ref.raw` (32768 bytes): 16-bit
  stereo

### PCM→DSD

- `dsdmod_sine_stereo_44100.wav` (17684 bytes): source sine
  stereo, 44100 Hz, made with ffmpeg 8.1.2 (exact command
  not recorded).
- `dsdmod_dsd64_stereo.dsf` (73820 bytes),
  `dsdmod_dsd64_stereo.dff` (73842 bytes),
  `dsdmod_dsd128_stereo.dsf` (147548 bytes): the library
  modulator's own output from that WAV (test generator, run
  with `-- --ignored`). ffmpeg cross-check on the DFF:
  amplitude 0.4998/0.4995 vs target 0.5.

### Opus CELT

WAVs are synthetic, made with numpy (seeded where noted);
the `.opus` files are CELT-only, fullband, 960
samples/frame, stereo, 101 frames, pre-skip 312, encoded
with ffmpeg 8.1.2:

- `ffmpeg -i <wav> -c:a libopus -b:a 128k -vbr on -application audio -frame_duration 20 <name>.opus`
- Reference: `ffmpeg -i <opus> -f f32le -ac 2 -ar 48000 <name>_ref.raw`

- `celt_sine_stereo_48000.opus` (45570 bytes): sine 440 Hz
  L / 550 Hz R, amp 0.5.
- `celt_noise_stereo_48000.opus` (30178 bytes): white noise
  uniform ±0.5, seed 1234.
- `celt_music_stereo_48000.opus` (35289 bytes): 4 chords
  (C, Am, F, G) × 0.5 s, 3 partials + 6 Hz tremolo.
- `celt_speech_stereo_48000.opus` (42467 bytes):
  speech-like formant bursts (300/900/2400 Hz, f0 90–160 Hz,
  seed 77).
- Each `*_ref.raw` is 768000 bytes (96000 samples/channel
  f32le). The 8 kHz mono SILK fixture was not used for CELT.

### VP9 reconstruction

All files are 10 fps, 10 frames, encoded with ffmpeg
8.1.2 (libvpx-vp9, Profile 0, yuv420p); each `*_ref.yuv`
is ffmpeg's rawvideo yuv420p decode of the same file.
Exact encode commands were not recorded.

- `vp9rec_keyonly_64x64.ivf` (17874 bytes) +
  `vp9rec_keyonly_64x64_ref.yuv` (61440 bytes): keyframes
  only, 64×64.
- `vp9rec_lossless_64x64.ivf` (23026 bytes) +
  `vp9rec_lossless_64x64_ref.yuv` (61440 bytes): lossless
  (WHT path), 64×64.
- `vp9rec_320x240.ivf` (19728 bytes) +
  `vp9rec_320x240_ref.yuv` (1152000 bytes): 320×240.
- `vp9rec_320x240_fp0.ivf` (19454 bytes) +
  `vp9rec_320x240_fp0_ref.yuv` (1152000 bytes): same size,
  frame-parallel decoding off.
- `vp9rec_640x480.ivf` (55836 bytes) +
  `vp9rec_640x480_ref.yuv` (4608000 bytes): 640×480, 2 tile
  columns.
- `vp9rec_mandel_64x64.ivf` (3386 bytes) +
  `vp9rec_mandel_64x64_ref.yuv` (61440 bytes),
  `vp9rec_mandel_320x64.ivf` (3810 bytes) +
  `vp9rec_mandel_320x64_ref.yuv` (307200 bytes),
  `vp9rec_mandel_320x128.ivf` (9982 bytes) +
  `vp9rec_mandel_320x128_ref.yuv` (614400 bytes),
  `vp9rec_mandel_320x240.ivf` (23896 bytes) +
  `vp9rec_mandel_320x240_ref.yuv` (1152000 bytes):
  Mandelbrot test pattern at four sizes.
- `vp9rec_mandel_320x128_lossless.ivf` (95820 bytes) +
  `vp9rec_mandel_320x128_lossless_ref.yuv` (614400 bytes):
  lossless Mandelbrot, 320×128.
- `vp9rec_testsrc2_320x128.ivf` (15964 bytes) +
  `vp9rec_testsrc2_320x128_ref.yuv` (614400 bytes):
  ffmpeg `testsrc2`, 320×128.
- Reference YUVs generated for the Phase 8 fixtures:
  `vp9_64x64_ref.yuv` (61440 bytes, 10 frames),
  `vp9_keyframes_ref.yuv` (61440 bytes),
  `vp9_tiles_ref.yuv` (491520 bytes),
  `vp9_superframe_ref.yuv` (12288 bytes).
- `vp9rec_tiles_640x480.ivf` was deleted: byte-identical to
  `vp9rec_640x480.ivf` (this ffmpeg/libvpx ignores
  `-tile-rows`).

### AV1 reconstruction

Encoded with ffmpeg 8.1.2 (libaom-av1), keyframe-only,
CDEF + loop restoration off; each `*_f0.yuv` is ffmpeg's
rawvideo yuv420p decode of frame 0. Exact encode commands
were not recorded.

- `av1rec_64x64.ivf` (6855 bytes) +
  `av1rec_64x64_f0.yuv` (6144 bytes): 64×64.
- `av1rec_320x240.ivf` (20769 bytes) +
  `av1rec_320x240_f0.yuv` (115200 bytes): 320×240.
- `av1rec_640x480.ivf` (50826 bytes) +
  `av1rec_640x480_f0.yuv` (460800 bytes): 640×480.
