// TurboModule spec for the React Native binding (Phase 10).
// Binary data crosses the bridge as base64 strings: the
// TurboModule bridge has no byte-array type. Audio samples cross
// as number arrays (f32 values as JS numbers); that is slow for
// long audio and is noted in the README — this binding is for
// short clips, probes, and header parsing, not realtime playback.
//
// The native side (android/, ios/) implements this spec. The
// module name "CodecWhat" must match the native module name on
// both platforms.

import type { TurboModule } from "react-native";
import { TurboModuleRegistry } from "react-native";

/** An interleaved f32 audio frame. `samples` has `frames * channels` values. */
export interface AudioFrame {
  sampleRate: number;
  channels: number;
  frames: number;
  samples: number[];
}

/** Result of `probe`: container and codec of the first bytes. */
export interface ProbeResult {
  container: number;
  codec: number;
}

/** A parsed VP9 frame header (parser-only; no reconstruction). */
export interface Vp9FrameHeader {
  profile: number;
  frameType: number; // 0 = key, 1 = inter
  showFrame: boolean;
  showExistingFrame: boolean;
  width: number;
  height: number;
  renderWidth: number;
  renderHeight: number;
  baseQIdx: number;
  loopFilterLevel: number;
  tileColsLog2: number;
  tileRowsLog2: number;
  headerSizeInBytes: number;
}

/** A parsed AV1 sequence header (first operating point). */
export interface Av1SequenceHeader {
  seqProfile: number;
  seqLevelIdx0: number;
  seqTier0: number;
  maxFrameWidthMinus1: number;
  maxFrameHeightMinus1: number;
  frameWidthBitsMinus1: number;
  frameHeightBitsMinus1: number;
  reducedStillPictureHeader: boolean;
}

export interface Spec extends TurboModule {
  getConstants: () => { version: string };

  /** Identify the container and codec of the first bytes of a file. */
  probe: (dataBase64: string) => Promise<ProbeResult>;

  /** Decode a WAV file: `dataBase64` is the whole file. */
  decodeWav: (dataBase64: string) => Promise<AudioFrame>;

  /** Decode one FLAC frame: `streaminfoBase64` is the 34-byte STREAMINFO payload. */
  decodeFlac: (
    streaminfoBase64: string,
    packetBase64: string,
  ) => Promise<AudioFrame>;

  /** One-shot FLAC encode: interleaved i16 samples -> a complete FLAC file. */
  encodeFlac: (
    samples: number[],
    frames: number,
    channels: number,
    sampleRate: number,
  ) => Promise<string>;

  /** Parse a VP9 frame header (parser-only; no reconstruction). */
  parseVp9FrameHeader: (frameBase64: string) => Promise<Vp9FrameHeader>;

  /** Parse one AV1 sequence-header OBU (header + payload). */
  parseAv1SequenceHeader: (obuBase64: string) => Promise<Av1SequenceHeader>;

  /** Create a streaming FLAC encoder. Returns a handle (small integer starting at 1). */
  encoderNewFlac: (sampleRate: number, channels: number) => Promise<number>;

  /** The streaming STREAMINFO header (42 bytes) for encoder `handle`. */
  encoderStreaminfo: (handle: number) => Promise<string>;

  /** Encode one frame of interleaved f32 samples with encoder `handle`. */
  encoderEncode: (
    handle: number,
    samples: number[],
    frames: number,
  ) => Promise<string>;

  /** Free a streaming FLAC encoder. The handle must not be used afterwards. */
  encoderFree: (handle: number) => Promise<void>;
}

export default TurboModuleRegistry.getEnforcing<Spec>("CodecWhat");
