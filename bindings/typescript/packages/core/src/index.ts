// @codec-whatttttttttttt/core — TypeScript bindings for
// codec-whatttttttttttt (Phase 10). Thin wrappers over the WASM
// module (`@codec-whatttttttttttt/wasm`); no decode or encode
// logic lives here.

import {
  type CodecWasm,
  CodecError,
  ERR_INVALID_ARGUMENT,
} from "@codec-whatttttttttttt/wasm";

export { Decoder } from "./decoder";
export { Encoder } from "./encoder";
export {
  CodecError,
  ERR_IO,
  ERR_INVALID_DATA,
  ERR_TRUNCATED,
  ERR_UNSUPPORTED,
  ERR_OVERFLOW,
  ERR_ALLOCATION_LIMIT,
  ERR_INVALID_ARGUMENT,
  ERR_LICENSE_REQUIRED,
  CODEC_UNKNOWN,
  CODEC_PCM,
  CODEC_FLAC,
  CODEC_OPUS,
  CODEC_VORBIS,
  CODEC_AV1,
  CODEC_VP9,
  CONTAINER_UNKNOWN,
  CONTAINER_WAV,
  CONTAINER_RAW_FLAC,
  CONTAINER_OGG,
  CONTAINER_WEBM,
  CONTAINER_MP4,
  CONTAINER_IVF,
  type ProbeResult,
  type AudioFrameData,
  type Vp9FrameHeader,
  type Av1SequenceHeader,
  type CodecWasm,
} from "@codec-whatttttttttttt/wasm";

/** Library version, e.g. "0.1.0". */
export function version(wasm: CodecWasm): string {
  return wasm.version();
}

/**
 * Identify the container and codec of the first bytes of a file.
 * The codec is `CODEC_UNKNOWN` for Ogg, WebM, and MP4: those
 * containers hold several streams and need a demuxer to name the
 * codec.
 */
export function probe(wasm: CodecWasm, data: Uint8Array) {
  return wasm.probe(data);
}

/**
 * One-shot FLAC encode: interleaved i16 samples -> a complete
 * FLAC file with exact STREAMINFO. `frames` is the number of
 * frames (samples per channel).
 */
export function encodeFlac(
  wasm: CodecWasm,
  samples: Int16Array,
  frames: number,
  channels: number,
  sampleRate: number,
): Uint8Array {
  if (samples.length !== frames * channels) {
    throw new CodecError(
      ERR_INVALID_ARGUMENT,
      `expected ${frames * channels} samples, got ${samples.length}`,
    );
  }
  return wasm.encodeFlac(samples, frames, channels, sampleRate);
}

/** Parse a VP9 frame header (parser-only; no reconstruction). */
export function parseVp9FrameHeader(wasm: CodecWasm, frame: Uint8Array) {
  return wasm.parseVp9FrameHeader(frame);
}

/** Parse one AV1 sequence-header OBU (header + payload). */
export function parseAv1SequenceHeader(wasm: CodecWasm, obu: Uint8Array) {
  return wasm.parseAv1SequenceHeader(obu);
}
