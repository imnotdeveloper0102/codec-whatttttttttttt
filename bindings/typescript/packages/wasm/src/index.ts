// Low-level wrapper over the codec-whatttttttttttt WebAssembly
// module (Phase 10). The module is built from
// crates/codec-whatttttttttttt-wasm; see build.sh. No decode or
// encode logic lives here; every method allocates input in linear
// memory, calls one wasm export, and copies the result out.
//
// The wrapper owns no memory across calls: every allocation is
// freed before the method returns, except the buffer returned by
// `encodeFlac`, which the caller receives as a copied Uint8Array.

import { promises as fs } from "node:fs";

/** An error from the codec library, with its C ABI error code. */
export class CodecError extends Error {
  readonly code: number;

  constructor(code: number, detail: string) {
    super(detail === "" ? `codec error ${code}` : detail);
    this.name = "CodecError";
    this.code = code;
  }
}

// Error codes (must match codec_whatttttttttttt.h).
export const ERR_IO = 1;
export const ERR_INVALID_DATA = 2;
export const ERR_TRUNCATED = 3;
export const ERR_UNSUPPORTED = 4;
export const ERR_OVERFLOW = 5;
export const ERR_ALLOCATION_LIMIT = 6;
export const ERR_INVALID_ARGUMENT = 7;

// Codec ids (must match codec_whatttttttttttt.h).
export const CODEC_UNKNOWN = -1;
export const CODEC_PCM = 0;
export const CODEC_FLAC = 1;
export const CODEC_OPUS = 2;
export const CODEC_VORBIS = 3;
export const CODEC_AV1 = 4;
export const CODEC_VP9 = 5;

// Container ids (must match codec_whatttttttttttt.h).
export const CONTAINER_UNKNOWN = 0;
export const CONTAINER_WAV = 1;
export const CONTAINER_RAW_FLAC = 2;
export const CONTAINER_OGG = 3;
export const CONTAINER_WEBM = 4;
export const CONTAINER_MP4 = 5;
export const CONTAINER_IVF = 6;

/** Result of `probe`: container and codec of the first bytes. */
export interface ProbeResult {
  container: number;
  codec: number;
}

/** An interleaved f32 audio frame. `samples` has `frames * channels` values. */
export interface AudioFrameData {
  sampleRate: number;
  channels: number;
  frames: number;
  timestampUs: number;
  samples: Float32Array;
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

interface WasmExports extends WebAssembly.Exports {
  memory: WebAssembly.Memory;
  wasm_alloc(size: number): number;
  wasm_free(ptr: number, size: number): void;
  wasm_version(): number;
  wasm_version_len(): number;
  wasm_error_string(code: number): number;
  wasm_error_string_len(code: number): number;
  wasm_last_error(out: number, capacity: number): number;
  wasm_probe(data: number, len: number, out: number): number;
  wasm_decode_wav(
    data: number,
    len: number,
    samplesOut: number,
    capacity: number,
    metaOut: number,
  ): number;
  wasm_decode_flac(
    streaminfo: number,
    packet: number,
    packetLen: number,
    samplesOut: number,
    capacity: number,
    metaOut: number,
  ): number;
  wasm_encode_flac(
    samples: number,
    frames: bigint,
    channels: number,
    sampleRate: number,
    outPtr: number,
    outLen: number,
  ): number;
  wasm_encoder_new_flac(
    sampleRate: number,
    channels: number,
    outHandle: number,
  ): number;
  wasm_encoder_flac_streaminfo(
    handle: number,
    outPtr: number,
    outLen: number,
  ): number;
  wasm_encoder_encode(
    handle: number,
    samples: number,
    frames: bigint,
    outPtr: number,
    outLen: number,
  ): number;
  wasm_encoder_free(handle: number): number;
  wasm_vp9_parse_header(frame: number, len: number, out: number): number;
  wasm_av1_parse_sequence_header(obu: number, len: number, out: number): number;
}

/**
 * A loaded codec WebAssembly module. Create it with
 * `loadFromFile` (Node) or `loadFromBytes` (browser or Node);
 * the instance is not shared between threads.
 */
export class CodecWasm {
  private readonly exports: WasmExports;
  private readonly decoder = new TextDecoder();

  private constructor(instance: WebAssembly.Instance) {
    this.exports = instance.exports as unknown as WasmExports;
  }

  /** Load the module from a `.wasm` file on disk (Node). */
  static async loadFromFile(path: string): Promise<CodecWasm> {
    const bytes = await fs.readFile(path);
    return CodecWasm.loadFromBytes(bytes);
  }

  /** Load the module from bytes (browser or Node). */
  static async loadFromBytes(bytes: BufferSource): Promise<CodecWasm> {
    const { instance } = await WebAssembly.instantiate(bytes, {});
    return new CodecWasm(instance);
  }

  /** Library version, e.g. "0.1.0". */
  version(): string {
    return this.readString(
      this.exports.wasm_version(),
      this.exports.wasm_version_len(),
    );
  }

  /**
   * Identify the container and codec of the first bytes of a
   * file. The codec is `CODEC_UNKNOWN` for Ogg, WebM, and MP4:
   * those containers hold several streams and need a demuxer to
   * name the codec.
   */
  probe(data: Uint8Array): ProbeResult {
    const input = this.alloc(data.length);
    const out = this.alloc(8);
    try {
      this.bytes().set(data, input);
      this.check(this.exports.wasm_probe(input, data.length, out));
      const view = new DataView(this.exports.memory.buffer, out, 8);
      return {
        container: view.getInt32(0, true),
        codec: view.getInt32(4, true),
      };
    } finally {
      this.free(input, data.length);
      this.free(out, 8);
    }
  }

  /**
   * Decode a WAV file: `data` is the whole file. Returns the
   * single audio frame (a WAV file decodes to one frame).
   */
  decodeWav(data: Uint8Array): AudioFrameData {
    const input = this.alloc(data.length);
    // A WAV frame holds at most one f32 sample per input byte
    // (i16 mono is one sample per two bytes; f32 is one per four),
    // so `data.length` slots always suffice.
    const capacity = Math.max(1, data.length);
    const samplesPtr = this.alloc(capacity * 4);
    const meta = this.alloc(12);
    try {
      this.bytes().set(data, input);
      this.check(
        this.exports.wasm_decode_wav(
          input,
          data.length,
          samplesPtr,
          capacity,
          meta,
        ),
      );
      return this.readFrame(samplesPtr, meta, 0);
    } finally {
      this.free(input, data.length);
      this.free(samplesPtr, capacity * 4);
      this.free(meta, 12);
    }
  }

  /**
   * Decode one FLAC frame: `streaminfo` is the 34-byte STREAMINFO
   * payload, `packet` is one frame.
   */
  decodeFlac(streaminfo: Uint8Array, packet: Uint8Array): AudioFrameData {
    if (streaminfo.length !== 34) {
      throw new CodecError(
        ERR_INVALID_ARGUMENT,
        `streaminfo must be 34 bytes, got ${streaminfo.length}`,
      );
    }
    const infoPtr = this.alloc(34);
    const packetPtr = this.alloc(packet.length);
    // The output capacity comes from STREAMINFO: a frame holds at
    // most max blocksize samples per channel (bytes 2-3, big
    // endian; 0 means unknown, so fall back to the FLAC maximum
    // of 65535), over `channels` channels (byte 12, bits 3-1,
    // plus one). The compressed packet is smaller than the
    // decoded samples, so the packet length is not a bound.
    const maxBlocksize = (streaminfo[2]! << 8) | streaminfo[3]!;
    const channels = ((streaminfo[12]! >> 1) & 0x7) + 1;
    const capacity = (maxBlocksize === 0 ? 65535 : maxBlocksize) * channels;
    const samplesPtr = this.alloc(capacity * 4);
    const meta = this.alloc(12);
    try {
      this.bytes().set(streaminfo, infoPtr);
      this.bytes().set(packet, packetPtr);
      this.check(
        this.exports.wasm_decode_flac(
          infoPtr,
          packetPtr,
          packet.length,
          samplesPtr,
          capacity,
          meta,
        ),
      );
      return this.readFrame(samplesPtr, meta, 0);
    } finally {
      this.free(infoPtr, 34);
      this.free(packetPtr, packet.length);
      this.free(samplesPtr, capacity * 4);
      this.free(meta, 12);
    }
  }

  /**
   * One-shot FLAC encode: interleaved i16 samples -> a complete
   * FLAC file with exact STREAMINFO. `frames` is the number of
   * frames (samples per channel).
   */
  encodeFlac(
    samples: Int16Array,
    frames: number,
    channels: number,
    sampleRate: number,
  ): Uint8Array {
    const input = this.alloc(samples.byteLength);
    const out = this.alloc(8);
    try {
      this.bytes().set(
        new Uint8Array(
          samples.buffer,
          samples.byteOffset,
          samples.byteLength,
        ),
        input,
      );
      this.check(
        this.exports.wasm_encode_flac(
          input,
          BigInt(frames),
          channels,
          sampleRate,
          out,
          out + 4,
        ),
      );
      const view = new DataView(this.exports.memory.buffer, out, 8);
      const ptr = view.getUint32(0, true);
      const len = view.getUint32(4, true);
      const copy = new Uint8Array(this.bytes().subarray(ptr, ptr + len));
      this.free(ptr, len);
      return copy;
    } finally {
      this.free(input, samples.byteLength);
      this.free(out, 8);
    }
  }

  /**
   * Create a streaming FLAC encoder. Returns a handle (a small
   * integer starting at 1); free it with `encoderFree`.
   */
  encoderNewFlac(sampleRate: number, channels: number): number {
    const out = this.alloc(4);
    try {
      this.check(
        this.exports.wasm_encoder_new_flac(sampleRate, channels, out),
      );
      return new DataView(this.exports.memory.buffer, out, 4).getUint32(
        0,
        true,
      );
    } finally {
      this.free(out, 4);
    }
  }

  /**
   * The streaming STREAMINFO header (42 bytes) for encoder
   * `handle`.
   */
  encoderStreaminfo(handle: number): Uint8Array {
    return this.encoderPacket((outPtr: number, outLen: number) =>
      this.exports.wasm_encoder_flac_streaminfo(handle, outPtr, outLen),
    );
  }

  /**
   * Encode one frame of interleaved f32 samples with encoder
   * `handle`. `frames` is the number of frames (samples per
   * channel), 1..=4096.
   */
  encoderEncode(
    handle: number,
    samples: Float32Array,
    frames: number,
  ): Uint8Array {
    const input = this.alloc(samples.byteLength);
    try {
      this.bytes().set(
        new Uint8Array(
          samples.buffer,
          samples.byteOffset,
          samples.byteLength,
        ),
        input,
      );
      return this.encoderPacket((outPtr: number, outLen: number) =>
        this.exports.wasm_encoder_encode(
          handle,
          input,
          BigInt(frames),
          outPtr,
          outLen,
        ),
      );
    } finally {
      this.free(input, samples.byteLength);
    }
  }

  /** Free a streaming FLAC encoder. The handle must not be used afterwards. */
  encoderFree(handle: number): void {
    this.check(this.exports.wasm_encoder_free(handle));
  }

  private encoderPacket(
    call: (outPtr: number, outLen: number) => number,
  ): Uint8Array {
    const out = this.alloc(8);
    try {
      this.check(call(out, out + 4));
      const view = new DataView(this.exports.memory.buffer, out, 8);
      const ptr = view.getUint32(0, true);
      const len = view.getUint32(4, true);
      const copy = new Uint8Array(this.bytes().subarray(ptr, ptr + len));
      this.free(ptr, len);
      return copy;
    } finally {
      this.free(out, 8);
    }
  }

  /** Parse a VP9 frame header (parser-only; no reconstruction). */
  parseVp9FrameHeader(frame: Uint8Array): Vp9FrameHeader {
    const input = this.alloc(frame.length);
    const out = this.alloc(20);
    try {
      this.bytes().set(frame, input);
      this.check(
        this.exports.wasm_vp9_parse_header(input, frame.length, out),
      );
      const view = new DataView(this.exports.memory.buffer, out, 20);
      return {
        profile: view.getUint8(0),
        frameType: view.getUint8(1),
        showFrame: view.getUint8(2) !== 0,
        showExistingFrame: view.getUint8(3) !== 0,
        width: view.getUint16(4, true),
        height: view.getUint16(6, true),
        renderWidth: view.getUint16(8, true),
        renderHeight: view.getUint16(10, true),
        baseQIdx: view.getUint8(12),
        loopFilterLevel: view.getUint8(13),
        tileColsLog2: view.getUint8(14),
        tileRowsLog2: view.getUint8(15),
        headerSizeInBytes: view.getUint32(16, true),
      };
    } finally {
      this.free(input, frame.length);
      this.free(out, 20);
    }
  }

  /** Parse one AV1 sequence-header OBU (header + payload). */
  parseAv1SequenceHeader(obu: Uint8Array): Av1SequenceHeader {
    const input = this.alloc(obu.length);
    const out = this.alloc(12);
    try {
      this.bytes().set(obu, input);
      this.check(
        this.exports.wasm_av1_parse_sequence_header(input, obu.length, out),
      );
      const view = new DataView(this.exports.memory.buffer, out, 12);
      return {
        seqProfile: view.getUint8(0),
        seqLevelIdx0: view.getUint8(1),
        seqTier0: view.getUint8(2),
        maxFrameWidthMinus1: view.getUint16(4, true),
        maxFrameHeightMinus1: view.getUint16(6, true),
        frameWidthBitsMinus1: view.getUint8(8),
        frameHeightBitsMinus1: view.getUint8(9),
        reducedStillPictureHeader: view.getUint8(10) !== 0,
      };
    } finally {
      this.free(input, obu.length);
      this.free(out, 12);
    }
  }

  private bytes(): Uint8Array {
    // Read the buffer fresh on every access: growing the memory
    // detaches the old buffer.
    return new Uint8Array(this.exports.memory.buffer);
  }

  private alloc(size: number): number {
    const ptr = this.exports.wasm_alloc(size);
    if (ptr === 0) {
      throw new CodecError(ERR_ALLOCATION_LIMIT, "wasm_alloc failed");
    }
    return ptr;
  }

  private free(ptr: number, size: number): void {
    this.exports.wasm_free(ptr, size);
  }

  private readString(ptr: number, len: number): string {
    return this.decoder.decode(this.bytes().subarray(ptr, ptr + len));
  }

  private check(rc: number): void {
    if (rc === 0) {
      return;
    }
    const buf = this.alloc(256);
    try {
      const count = this.exports.wasm_last_error(buf, 256);
      const detail = count > 0 ? this.readString(buf, count) : "";
      const message =
        this.readString(
          this.exports.wasm_error_string(rc),
          this.exports.wasm_error_string_len(rc),
        ) + (detail === "" ? "" : `: ${detail}`);
      throw new CodecError(rc, message);
    } finally {
      this.free(buf, 256);
    }
  }

  private readFrame(
    samplesPtr: number,
    metaPtr: number,
    timestampUs: number,
  ): AudioFrameData {
    const meta = new DataView(this.exports.memory.buffer, metaPtr, 12);
    const sampleRate = meta.getUint32(0, true);
    const channels = meta.getUint32(4, true);
    const frames = meta.getUint32(8, true);
    const count = frames * channels;
    const samples = new Float32Array(
      this.bytes().buffer,
      samplesPtr,
      count,
    ).slice();
    return { sampleRate, channels, frames, timestampUs, samples };
  }
}
