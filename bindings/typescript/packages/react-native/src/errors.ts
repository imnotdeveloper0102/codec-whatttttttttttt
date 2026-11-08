// Error type for the React Native binding. The numeric codes
// must match codec_whatttttttttttt.h (the C ABI contract); the
// native modules report them, this class carries them to JS.

export const ERR_IO = 1;
export const ERR_INVALID_DATA = 2;
export const ERR_TRUNCATED = 3;
export const ERR_UNSUPPORTED = 4;
export const ERR_OVERFLOW = 5;
export const ERR_ALLOCATION_LIMIT = 6;
export const ERR_INVALID_ARGUMENT = 7;

export const CODEC_UNKNOWN = -1;
export const CODEC_PCM = 0;
export const CODEC_FLAC = 1;
export const CODEC_OPUS = 2;
export const CODEC_VORBIS = 3;
export const CODEC_AV1 = 4;
export const CODEC_VP9 = 5;

export const CONTAINER_UNKNOWN = 0;
export const CONTAINER_WAV = 1;
export const CONTAINER_RAW_FLAC = 2;
export const CONTAINER_OGG = 3;
export const CONTAINER_WEBM = 4;
export const CONTAINER_MP4 = 5;
export const CONTAINER_IVF = 6;

/** An error from the codec library, with its C ABI error code. */
export class CodecError extends Error {
  readonly code: number;

  constructor(code: number, message: string) {
    super(message);
    this.name = "CodecError";
    this.code = code;
  }
}

/**
 * Run a native-module call, converting its rejection to
 * `CodecError`. The native side rejects with the C ABI numeric
 * code as a string; anything else passes through unchanged.
 */
export async function callNative<T>(call: () => Promise<T>): Promise<T> {
  try {
    return await call();
  } catch (error) {
    if (error instanceof CodecError) {
      throw error;
    }
    if (typeof error === "object" && error !== null) {
      const code = (error as { code?: unknown }).code;
      if (typeof code === "string") {
        const parsed = Number.parseInt(code, 10);
        if (!Number.isNaN(parsed)) {
          throw new CodecError(
            parsed,
            error instanceof Error ? error.message : "codec error",
          );
        }
      }
    }
    throw error;
  }
}
