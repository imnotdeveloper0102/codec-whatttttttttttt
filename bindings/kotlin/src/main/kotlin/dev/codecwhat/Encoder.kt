package dev.codecwhat

import java.io.Closeable

/**
 * FLAC encoder (streaming). One input frame becomes one FLAC
 * frame, block size 1..=4096. Close it (or use `use { }`) to free
 * the native handle deterministically.
 */
class Encoder private constructor(
    private var handle: Long,
    val sampleRate: Int,
    val channels: Int
) : Closeable {

    companion object {
        /** Build a FLAC encoder. `sampleRate` is 1..=192000, `channels` 1..=8. */
        fun flac(sampleRate: Int, channels: Int): Encoder =
            Encoder(Native.nativeEncoderNewFlac(sampleRate, channels), sampleRate, channels)
    }

    /**
     * The streaming STREAMINFO header (42 bytes: "fLaC" + block
     * header + 34-byte STREAMINFO, total samples 0).
     */
    fun streaminfo(): ByteArray {
        if (handle == 0L) {
            throw CodecError(ERR_INVALID_ARGUMENT, "encoder is closed")
        }
        return Native.nativeEncoderFlacStreaminfo(handle)
    }

    /**
     * Encode one frame of interleaved f32 samples. `frames` is the
     * number of frames (samples per channel), 1..=4096.
     */
    fun encode(samples: FloatArray, frames: Long): ByteArray {
        if (handle == 0L) {
            throw CodecError(ERR_INVALID_ARGUMENT, "encoder is closed")
        }
        if (samples.size != (frames * channels).toInt()) {
            throw CodecError(
                ERR_INVALID_ARGUMENT,
                "expected ${frames * channels} samples, got ${samples.size}"
            )
        }
        return Native.nativeEncoderEncode(handle, samples, frames)
    }

    override fun close() {
        if (handle != 0L) {
            Native.nativeEncoderFree(handle)
            handle = 0L
        }
    }
}
