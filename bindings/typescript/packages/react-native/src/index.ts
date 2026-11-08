// @codec-whatttttttttttt/react-native — React Native bindings
// for codec-whatttttttttttt (Phase 10). Thin wrappers over the
// native TurboModule (`CodecWhat`); no decode or encode logic
// lives here. Binary data crosses the bridge as base64 strings
// and audio samples as number arrays — see the README for the
// cost of that and for the platform setup.
//
// UNVERIFIED: this package has not been run on a device or
// simulator (no Metro, Android SDK, or macOS in this
// environment). The TypeScript compiles; the native modules are
// written against the C ABI and the Kotlin binding's public API
// but have not been built.

import NativeCodecWhat, {
  type Av1SequenceHeader,
  type ProbeResult,
  type Vp9FrameHeader,
} from "./NativeCodecWhat";
import { CodecError, ERR_INVALID_ARGUMENT, callNative } from "./errors";
import { decodeBase64, encodeBase64 } from "./base64";

export { Decoder, type AudioFrameData } from "./decoder";
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
} from "./errors";
export type {
  AudioFrame,
  ProbeResult,
  Vp9FrameHeader,
  Av1SequenceHeader,
} from "./NativeCodecWhat";

/** Library version, e.g. "0.1.0". */
export function version(): string {
  return NativeCodecWhat.getConstants().version;
}

/**
 * Identify the container and codec of the first bytes of a file.
 * The codec is `CODEC_UNKNOWN` for Ogg, WebM, and MP4: those
 * containers hold several streams and need a demuxer to name the
 * codec.
 */
export async function probe(data: Uint8Array): Promise<ProbeResult> {
  return callNative(() => NativeCodecWhat.probe(encodeBase64(data)));
}

/**
 * One-shot FLAC encode: interleaved i16 samples -> a complete
 * FLAC file with exact STREAMINFO. `frames` is the number of
 * frames (samples per channel).
 */
export async function encodeFlac(
  samples: Int16Array,
  frames: number,
  channels: number,
  sampleRate: number,
): Promise<Uint8Array> {
  if (samples.length !== frames * channels) {
    throw new CodecError(
      ERR_INVALID_ARGUMENT,
      `expected ${frames * channels} samples, got ${samples.length}`,
    );
  }
  const base64 = await callNative(() =>
    NativeCodecWhat.encodeFlac(
      Array.from(samples),
      frames,
      channels,
      sampleRate,
    ),
  );
  return decodeBase64(base64);
}

/** Parse a VP9 frame header (parser-only; no reconstruction). */
export async function parseVp9FrameHeader(
  frame: Uint8Array,
): Promise<Vp9FrameHeader> {
  return callNative(() =>
    NativeCodecWhat.parseVp9FrameHeader(encodeBase64(frame)),
  );
}

/** Parse one AV1 sequence-header OBU (header + payload). */
export async function parseAv1SequenceHeader(
  obu: Uint8Array,
): Promise<Av1SequenceHeader> {
  return callNative(() =>
    NativeCodecWhat.parseAv1SequenceHeader(encodeBase64(obu)),
  );
}
