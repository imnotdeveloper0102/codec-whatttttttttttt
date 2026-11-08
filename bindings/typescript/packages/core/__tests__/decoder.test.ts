// Tests for @codec-whatttttttttttt/core (Phase 10). Run with
// `npm test --workspace @codec-whatttttttttttt/core` from
// bindings/typescript. The fixtures are the pinned files in the
// core crate's tests/data.

import assert from "node:assert/strict";
import { promises as fs } from "node:fs";
import path from "node:path";
import test from "node:test";
import { CodecWasm } from "@codec-whatttttttttttt/wasm";
import {
  Decoder,
  Encoder,
  encodeFlac,
  parseAv1SequenceHeader,
  parseVp9FrameHeader,
  probe,
  version,
  CODEC_AV1,
  CODEC_FLAC,
  CODEC_PCM,
  CODEC_UNKNOWN,
  CONTAINER_IVF,
  CONTAINER_RAW_FLAC,
  CONTAINER_WAV,
  ERR_INVALID_DATA,
  ERR_TRUNCATED,
  ERR_UNSUPPORTED,
} from "../src/index";
import { CodecError } from "@codec-whatttttttttttt/wasm";

// The tests run from dist/__tests__/: six parents up is the
// workspace root (dist -> core -> packages -> typescript ->
// bindings -> codec-whatttttttttttt); three parents up is
// packages/.
const FIXTURES = path.join(
  __dirname,
  "..",
  "..",
  "..",
  "..",
  "..",
  "..",
  "crates",
  "codec-whatttttttttttt-core",
  "tests",
  "data",
);
const WASM = path.join(
  __dirname,
  "..",
  "..",
  "..",
  "wasm",
  "codec_whatttttttttttt_wasm.wasm",
);

async function loadWasm(): Promise<CodecWasm> {
  return CodecWasm.loadFromFile(WASM);
}

async function readFixture(name: string): Promise<Uint8Array> {
  return new Uint8Array(await fs.readFile(path.join(FIXTURES, name)));
}

/** Offset of the first FLAC frame: skip the metadata blocks. */
function firstFlacFrameOffset(flac: Uint8Array): number {
  let offset = 4;
  for (;;) {
    const header = flac[offset] as number;
    const length =
      ((flac[offset + 1] as number) << 16) |
      ((flac[offset + 2] as number) << 8) |
      (flac[offset + 3] as number);
    offset += 4 + length;
    if ((header & 0x80) !== 0) {
      break;
    }
  }
  return offset;
}

function firstIvfFrame(ivf: Uint8Array): Uint8Array {
  const view = new DataView(ivf.buffer, ivf.byteOffset, ivf.byteLength);
  const size = view.getUint32(32, true);
  return ivf.slice(44, 44 + size);
}

test("version is 0.1.0", async () => {
  const wasm = await loadWasm();
  assert.equal(version(wasm), "0.1.0");
});

test("probe wav, flac and ivf", async () => {
  const wasm = await loadWasm();
  const wav = await readFixture("valid_i16_stereo_44100.wav");
  const wavProbe = probe(wasm, wav);
  assert.equal(wavProbe.container, CONTAINER_WAV);
  assert.equal(wavProbe.codec, CODEC_PCM);

  const flac = await readFixture("sine_stereo_44100.flac");
  const flacProbe = probe(wasm, flac);
  assert.equal(flacProbe.container, CONTAINER_RAW_FLAC);
  assert.equal(flacProbe.codec, CODEC_FLAC);

  const ivf = await readFixture("av1_64x64.ivf");
  const ivfProbe = probe(wasm, ivf);
  assert.equal(ivfProbe.container, CONTAINER_IVF);
  // IVF names its codec in the header fourcc ("AV01").
  assert.equal(ivfProbe.codec, CODEC_AV1);
});

test("decode flac frame", async () => {
  const wasm = await loadWasm();
  const flac = await readFixture("sine_stereo_44100.flac");
  const decoder = Decoder.fromFlacStreaminfo(wasm, flac.slice(8, 42));
  const frame = decoder.decode(
    flac.slice(firstFlacFrameOffset(flac), flac.length),
  );
  assert.equal(frame.sampleRate, 44100);
  assert.equal(frame.channels, 2);
  assert.equal(frame.frames, 4410);
  assert.ok(Math.abs(frame.samples[0] as number) < 0.001);
});

test("decode wav frame", async () => {
  const wasm = await loadWasm();
  const wav = await readFixture("valid_i16_stereo_44100.wav");
  const decoder = Decoder.fromWav(wasm, wav);
  const frame = decoder.decode();
  assert.equal(frame.sampleRate, 44100);
  assert.equal(frame.channels, 2);
  assert.equal(frame.frames, 4410);
});

test("one-shot flac encode round-trips", async () => {
  const wasm = await loadWasm();
  const samples = new Int16Array(2000);
  for (let n = 0; n < 1000; n++) {
    const t = n / 44100;
    samples[2 * n] = Math.trunc(20000 * Math.sin(2 * Math.PI * 440 * t));
    samples[2 * n + 1] = Math.trunc(15000 * Math.sin(2 * Math.PI * 660 * t));
  }
  const encoded = encodeFlac(wasm, samples, 1000, 2, 44100);
  assert.ok(encoded.length > 42);
  assert.equal(encoded[0], "f".charCodeAt(0));
  assert.equal(encoded[1], "L".charCodeAt(0));
  const decoder = Decoder.fromFlacStreaminfo(wasm, encoded.slice(8, 42));
  const frame = decoder.decode(
    encoded.slice(firstFlacFrameOffset(encoded), encoded.length),
  );
  assert.equal(frame.frames, 1000);
  assert.ok(Math.abs(frame.samples[0] as number) < 1.0);
});

test("streaming encoder", async () => {
  const wasm = await loadWasm();
  const encoder = Encoder.flac(wasm, 44100, 2);
  try {
    const header = encoder.streaminfo();
    assert.equal(header.length, 42);
    const floats = new Float32Array(2000);
    for (let n = 0; n < 1000; n++) {
      const t = n / 44100;
      floats[2 * n] = 0.5 * Math.sin(2 * Math.PI * 440 * t);
      floats[2 * n + 1] = 0.5 * Math.sin(2 * Math.PI * 660 * t);
    }
    const packet = encoder.encode(floats, 1000);
    assert.ok(packet.length > 0);
  } finally {
    encoder.close();
  }
});

test("parse vp9 key frame header", async () => {
  const wasm = await loadWasm();
  const ivf = await readFixture("vp9_64x64.ivf");
  const header = parseVp9FrameHeader(wasm, firstIvfFrame(ivf));
  assert.equal(header.profile, 0);
  assert.equal(header.frameType, 0);
  assert.equal(header.width, 64);
  assert.equal(header.height, 64);
  assert.equal(header.baseQIdx, 33);
});

test("vp9 decode is unsupported", async () => {
  const wasm = await loadWasm();
  const ivf = await readFixture("vp9_64x64.ivf");
  const frame = firstIvfFrame(ivf);
  const decoder = Decoder.fromVp9FrameHeader(wasm, frame);
  assert.throws(
    () => decoder.decode(frame),
    (error: unknown) =>
      error instanceof CodecError && error.code === ERR_UNSUPPORTED,
  );
});

test("parse av1 sequence header", async () => {
  const wasm = await loadWasm();
  const obu = await readFixture("av1_seq_header.obu");
  const header = parseAv1SequenceHeader(wasm, obu);
  assert.equal(header.seqProfile, 0);
  assert.equal(header.maxFrameWidthMinus1, 63);
  assert.equal(header.maxFrameHeightMinus1, 63);
});

test("bad wav throws", async () => {
  const wasm = await loadWasm();
  assert.throws(
    () => Decoder.fromWav(wasm, new Uint8Array([0, 1, 2, 3])).decode(),
    (error: unknown) =>
      error instanceof CodecError &&
      (error.code === ERR_TRUNCATED || error.code === ERR_INVALID_DATA),
  );
});
