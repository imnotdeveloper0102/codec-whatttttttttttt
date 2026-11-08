// Encoder — FLAC encoder for the React Native binding
// (Phase 10). Thin wrapper over the native TurboModule; no encode
// logic lives here.

import { CodecError, ERR_INVALID_ARGUMENT, callNative } from "./errors";
import { decodeBase64 } from "./base64";
import NativeCodecWhat from "./NativeCodecWhat";

/**
 * FLAC encoder (streaming). One input frame becomes one FLAC
 * frame, block size 1..=4096. Call `close()` to free the native
 * handle; there is no finalizer, so `close()` is required.
 */
export class Encoder {
  private handle: number | null;

  private constructor(
    handle: number,
    readonly sampleRate: number,
    readonly channels: number,
  ) {
    this.handle = handle;
  }

  /** Build a FLAC encoder. `sampleRate` is 1..=192000, `channels` 1..=8. */
  static async flac(sampleRate: number, channels: number): Promise<Encoder> {
    const handle = await callNative(() =>
      NativeCodecWhat.encoderNewFlac(sampleRate, channels),
    );
    return new Encoder(handle, sampleRate, channels);
  }

  /**
   * The streaming STREAMINFO header (42 bytes: "fLaC" + block
   * header + 34-byte STREAMINFO, total samples 0).
   */
  async streaminfo(): Promise<Uint8Array> {
    const base64 = await callNative(() =>
      NativeCodecWhat.encoderStreaminfo(this.requireHandle()),
    );
    return decodeBase64(base64);
  }

  /**
   * Encode one frame of interleaved f32 samples. `frames` is the
   * number of frames (samples per channel), 1..=4096.
   */
  async encode(samples: Float32Array, frames: number): Promise<Uint8Array> {
    if (samples.length !== frames * this.channels) {
      throw new CodecError(
        ERR_INVALID_ARGUMENT,
        `expected ${frames * this.channels} samples, got ${samples.length}`,
      );
    }
    const base64 = await callNative(() =>
      NativeCodecWhat.encoderEncode(
        this.requireHandle(),
        Array.from(samples),
        frames,
      ),
    );
    return decodeBase64(base64);
  }

  /** Free the native handle. Safe to call twice. */
  async close(): Promise<void> {
    if (this.handle !== null) {
      await callNative(() => NativeCodecWhat.encoderFree(this.handle as number));
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
