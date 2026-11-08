// Decoder — audio decoder for the React Native binding
// (Phase 10). Thin wrapper over the native TurboModule; no decode
// logic lives here.

import {
  CodecError,
  ERR_INVALID_ARGUMENT,
  ERR_UNSUPPORTED,
  callNative,
} from "./errors";
import { decodeBase64, encodeBase64 } from "./base64";
import NativeCodecWhat, { type AudioFrame } from "./NativeCodecWhat";

/** An interleaved f32 audio frame. `samples` has `frames * channels` values. */
export interface AudioFrameData {
  sampleRate: number;
  channels: number;
  frames: number;
  samples: Float32Array;
}

function toAudioFrameData(frame: AudioFrame): AudioFrameData {
  return {
    sampleRate: frame.sampleRate,
    channels: frame.channels,
    frames: frame.frames,
    samples: Float32Array.from(frame.samples),
  };
}

/**
 * Audio decoder. Build from a WAV file, a FLAC STREAMINFO block,
 * or a VP9 frame header, then decode one packet at a time. A
 * decoder is used from one thread at a time.
 *
 * A WAV file decodes to a single frame: `decode()` decodes the
 * whole file each call. A FLAC packet holds exactly one frame.
 */
export class Decoder {
  private constructor(
    private readonly kind: "wav" | "flac" | "vp9",
    private readonly source: Uint8Array,
  ) {}

  /** Build a decoder from a WAV file's bytes. */
  static fromWav(data: Uint8Array): Decoder {
    return new Decoder("wav", data);
  }

  /** Build a decoder from a 34-byte FLAC STREAMINFO payload. */
  static fromFlacStreaminfo(streaminfo: Uint8Array): Decoder {
    if (streaminfo.length !== 34) {
      throw new CodecError(
        ERR_INVALID_ARGUMENT,
        `streaminfo must be 34 bytes, got ${streaminfo.length}`,
      );
    }
    return new Decoder("flac", streaminfo);
  }

  /**
   * Build a decoder from a VP9 frame header. The header is
   * validated here; `decode` on it throws `CodecError` with code
   * `ERR_UNSUPPORTED`: VP9 is parser-only in this phase.
   */
  static async fromVp9FrameHeader(frame: Uint8Array): Promise<Decoder> {
    // Validate the header now, so a bad frame fails here rather
    // than at decode time.
    await callNative(() =>
      NativeCodecWhat.parseVp9FrameHeader(encodeBase64(frame)),
    );
    return new Decoder("vp9", frame);
  }

  /** Decode one packet (required for FLAC; ignored for WAV). */
  async decode(packet?: Uint8Array): Promise<AudioFrameData> {
    switch (this.kind) {
      case "wav": {
        const frame = await callNative(() =>
          NativeCodecWhat.decodeWav(encodeBase64(this.source)),
        );
        return toAudioFrameData(frame);
      }
      case "flac": {
        if (packet === undefined) {
          throw new CodecError(
            ERR_INVALID_ARGUMENT,
            "decode requires a packet for FLAC",
          );
        }
        const frame = await callNative(() =>
          NativeCodecWhat.decodeFlac(
            encodeBase64(this.source),
            encodeBase64(packet),
          ),
        );
        return toAudioFrameData(frame);
      }
      case "vp9":
        throw new CodecError(
          ERR_UNSUPPORTED,
          "vp9 decode is parser-only: reconstruction is not implemented",
        );
    }
  }
}

/** Decode a base64 string returned by the native module to bytes. */
export function fromBase64(text: string): Uint8Array {
  return decodeBase64(text);
}
