package dev.codecwhat

import java.io.Closeable

/**
 * Audio decoder. Build from a WAV file, a FLAC STREAMINFO block,
 * or a VP9 frame header, then decode one packet at a time.
 * A decoder is used from one thread at a time. Close it (or use
 * `use { }`) to free the native handle deterministically.
 */
class Decoder private constructor(private var handle: Long) : Closeable {

    companion object {
        /** Build a decoder from a WAV file's bytes. */
        fun fromWav(data: ByteArray): Decoder =
            Decoder(Native.nativeDecoderFromWav(data))

        /** Build a decoder from a 34-byte FLAC STREAMINFO payload. */
        fun fromFlacStreaminfo(streaminfo: ByteArray): Decoder {
            if (streaminfo.size != 34) {
                throw CodecError(
                    ERR_INVALID_ARGUMENT,
                    "streaminfo must be 34 bytes, got ${streaminfo.size}"
                )
            }
            return Decoder(Native.nativeDecoderFromFlacStreaminfo(streaminfo))
        }

        /**
         * Build a decoder from a VP9 frame header. [decode] on it
         * throws [CodecError] with code [ERR_UNSUPPORTED]: VP9 is
         * parser-only in this phase.
         */
        fun fromVp9FrameHeader(frame: ByteArray): Decoder =
            Decoder(Native.nativeDecoderFromVp9FrameHeader(frame))
    }

    /**
     * Decode one packet. A FLAC packet holds exactly one frame; an
     * Opus packet may hold several frames of audio.
     */
    fun decode(packet: ByteArray, timestampUs: Long = 0): AudioFrame {
        if (handle == 0L) {
            throw CodecError(ERR_INVALID_ARGUMENT, "decoder is closed")
        }
        return Native.nativeDecoderDecode(handle, packet, timestampUs)
    }

    override fun close() {
        if (handle != 0L) {
            Native.nativeDecoderFree(handle)
            handle = 0L
        }
    }
}
