// Encoder — FLAC encoder for the codec-whatttttttttttt
// TypeScript binding (Phase 10). Thin wrapper over the WASM
// module; no encode logic lives here.

import { CodecError, ERR_INVALID_ARGUMENT, type CodecWasm } from "@codec-whatttttttttttt/wasm";

/**
 * FLAC encoder (streaming). One input frame becomes one FLAC
 * frame, block size 1..=4096. Call `close()` to free the native
 * handle; it is also freed when the instance is garbage
 * collected only if `close()` was called — there is no finalizer,
 * so `close()` is required.
 */
export class Encoder {
  private handle: number | null;

  private constructor(
    private readonly wasm: CodecWasm,
    handle: number,
    readonly sampleRate: number,
    readonly channels: number,
  ) {
    this.handle = handle;
  }

  /** Build a FLAC encoder. `sampleRate` is 1..=192000, `channels` 1..=8. */
  static flac(
    wasm: CodecWasm,
    sampleRate: number,
    channels: number,
  ): Encoder {
    const handle = wasm.encoderNewFlac(sampleRate, channels);
    return new Encoder(wasm, handle, sampleRate, channels);
  }

  /**
   * The streaming STREAMINFO header (42 bytes: "fLaC" + block
   * header + 34-byte STREAMINFO, total samples 0).
   */
  streaminfo(): Uint8Array {
    return this.wasm.encoderStreaminfo(this.requireHandle());
  }

  /**
   * Encode one frame of interleaved f32 samples. `frames` is the
   * number of frames (samples per channel), 1..=4096.
   */
  encode(samples: Float32Array, frames: number): Uint8Array {
    if (samples.length !== frames * this.channels) {
      throw new CodecError(
        ERR_INVALID_ARGUMENT,
        `expected ${frames * this.channels} samples, got ${samples.length}`,
      );
    }
    return this.wasm.encoderEncode(this.requireHandle(), samples, frames);
  }

  /** Free the native handle. Safe to call twice. */
  close(): void {
    if (this.handle !== null) {
      this.wasm.encoderFree(this.handle);
      this.handle = null;
    }
  }

  private requireHandle(): number {
    if (this.handle === null) {
      throw new CodecError(ERR_INVALID_ARGUMENT, "encoder is closed");
    }
    return this.handle;
  }
}
