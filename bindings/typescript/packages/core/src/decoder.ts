// Decoder — audio decoder for the codec-whatttttttttttt
// TypeScript binding (Phase 10). Thin wrapper over the WASM
// module; no decode logic lives here.

import {
  CodecError,
  ERR_INVALID_ARGUMENT,
  ERR_UNSUPPORTED,
  type AudioFrameData,
  type CodecWasm,
} from "@codec-whatttttttttttt/wasm";

/**
 * Audio decoder. Build from a WAV file, a FLAC STREAMINFO block,
 * or a VP9 frame header, then decode one packet at a time. A
 * decoder is used from one thread at a time.
 *
 * A WAV file decodes to a single frame: `decode()` decodes the
 * whole file each call. A FLAC packet holds exactly one frame;
 * an Opus packet may hold several frames of audio.
 */
export class Decoder {
  private constructor(
    private readonly wasm: CodecWasm,
    private readonly kind: "wav" | "flac" | "vp9",
    private readonly source: Uint8Array,
  ) {}

  /** Build a decoder from a WAV file's bytes. */
  static fromWav(wasm: CodecWasm, data: Uint8Array): Decoder {
    return new Decoder(wasm, "wav", data);
  }

  /** Build a decoder from a 34-byte FLAC STREAMINFO payload. */
  static fromFlacStreaminfo(
    wasm: CodecWasm,
    streaminfo: Uint8Array,
  ): Decoder {
    if (streaminfo.length !== 34) {
      throw new CodecError(
        ERR_INVALID_ARGUMENT,
        `streaminfo must be 34 bytes, got ${streaminfo.length}`,
      );
    }
    return new Decoder(wasm, "flac", streaminfo);
  }

  /**
   * Build a decoder from a VP9 frame header. The header is
   * validated here; `decode` on it throws `CodecError` with code
   * `ERR_UNSUPPORTED`: VP9 is parser-only in this phase.
   */
  static fromVp9FrameHeader(wasm: CodecWasm, frame: Uint8Array): Decoder {
    // Validate the header now, so a bad frame fails here rather
    // than at decode time.
    wasm.parseVp9FrameHeader(frame);
    return new Decoder(wasm, "vp9", frame);
  }

  /** Decode one packet (required for FLAC; ignored for WAV). */
  decode(packet?: Uint8Array): AudioFrameData {
    switch (this.kind) {
      case "wav":
        return this.wasm.decodeWav(this.source);
      case "flac": {
        if (packet === undefined) {
          throw new CodecError(
            ERR_INVALID_ARGUMENT,
            "decode requires a packet for FLAC",
          );
        }
        return this.wasm.decodeFlac(this.source, packet);
      }
      case "vp9":
        throw new CodecError(
          ERR_UNSUPPORTED,
          "vp9 decode is parser-only: reconstruction is not implemented",
        );
    }
  }
}
