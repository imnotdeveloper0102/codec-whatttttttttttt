package dev.codecwhat

import android.util.Base64
import com.facebook.react.bridge.Arguments
import com.facebook.react.bridge.Promise
import com.facebook.react.bridge.ReactApplicationContext
import com.facebook.react.bridge.ReactContextBaseJavaModule
import com.facebook.react.bridge.ReactMethod
import com.facebook.react.bridge.ReadableArray
import com.facebook.react.bridge.WritableArray
import com.facebook.react.bridge.WritableMap
import com.facebook.react.module.annotations.ReactModule

/**
 * React Native module for codec-whatttttttttttt (Phase 10).
 *
 * Thin adapter over the Kotlin binding (`dev.codecwhat`): every
 * method converts base64 / bridge arrays, delegates to the
 * Kotlin binding, and converts the result back. No decode or
 * encode logic lives here.
 *
 * Binary data crosses the bridge as base64 strings and audio
 * samples as arrays of numbers; see the package README for the
 * cost of that.
 *
 * This is a plain `@ReactMethod` module (not the codegen
 * `NativeCodecWhatSpec`): it works on both architectures through
 * the TurboModule interop layer, and avoids requiring the app to
 * run codegen before this module compiles. A later version should
 * switch to the generated spec.
 *
 * UNVERIFIED: not built or run — no Android SDK in this
 * environment (see the package README).
 */
@ReactModule(name = CodecWhatModule.NAME)
class CodecWhatModule(reactContext: ReactApplicationContext) :
    ReactContextBaseJavaModule(reactContext) {

    companion object {
        const val NAME = "CodecWhat"
    }

    override fun getName(): String = NAME

    override fun getConstants(): Map<String, Any> =
        mapOf("version" to version())

    // Streaming encoders held by handle (small integers from 1).
    private val encoders = mutableMapOf<Int, Encoder>()
    private var nextEncoderHandle = 1

    /** Run `block`, resolving the promise or rejecting with the codec error code. */
    private fun <T> resolveOrReject(promise: Promise, block: () -> T) {
        try {
            promise.resolve(block())
        } catch (e: CodecError) {
            promise.reject(e.code.toString(), e.message, e)
        } catch (e: Exception) {
            promise.reject("ERR_UNKNOWN", e.message, e)
        }
    }

    private fun decodeBase64(text: String): ByteArray =
        Base64.decode(text, Base64.DEFAULT)

    private fun encodeBase64(data: ByteArray): String =
        Base64.encodeToString(data, Base64.NO_WRAP)

    private fun frameToMap(frame: AudioFrame): WritableMap {
        val map = Arguments.createMap()
        map.putInt("sampleRate", frame.sampleRate)
        map.putInt("channels", frame.channels)
        map.putDouble("frames", frame.frames.toDouble())
        val samples = Arguments.createArray()
        for (value in frame.samples) {
            samples.pushDouble(value.toDouble())
        }
        map.putArray("samples", samples)
        return map
    }

    /**
     * The PCM data chunk of a WAV file. The decoder needs the data
     * chunk, not the RIFF header: walk the chunks and return the
     * "data" payload (chunks are word-aligned).
     */
    private fun wavDataChunk(wav: ByteArray): ByteArray {
        if (wav.size < 12) {
            throw CodecError(ERR_INVALID_DATA, "wav file too short")
        }
        var offset = 12
        while (offset + 8 <= wav.size) {
            val id = String(wav, offset, 4, Charsets.US_ASCII)
            val size = (wav[offset + 4].toInt() and 0xFF) or
                ((wav[offset + 5].toInt() and 0xFF) shl 8) or
                ((wav[offset + 6].toInt() and 0xFF) shl 16) or
                ((wav[offset + 7].toInt() and 0xFF) shl 24)
            if (size < 0) {
                throw CodecError(ERR_INVALID_DATA, "wav chunk size overflows")
            }
            if (id == "data") {
                val start = offset + 8
                if (start + size > wav.size) {
                    throw CodecError(ERR_TRUNCATED, "wav data chunk truncated")
                }
                return wav.copyOfRange(start, start + size)
            }
            offset += 8 + size + (size and 1)
        }
        throw CodecError(ERR_INVALID_DATA, "wav file has no data chunk")
    }

    @ReactMethod
    fun probe(dataBase64: String, promise: Promise) {
        resolveOrReject(promise) {
            val result = probe(decodeBase64(dataBase64))
            val map = Arguments.createMap()
            map.putInt("container", result.container)
            map.putInt("codec", result.codec)
            map
        }
    }

    @ReactMethod
    fun decodeWav(dataBase64: String, promise: Promise) {
        resolveOrReject(promise) {
            val wav = decodeBase64(dataBase64)
            Decoder.fromWav(wav).use { decoder ->
                frameToMap(decoder.decode(wavDataChunk(wav)))
            }
        }
    }

    @ReactMethod
    fun decodeFlac(streaminfoBase64: String, packetBase64: String, promise: Promise) {
        resolveOrReject(promise) {
            Decoder.fromFlacStreaminfo(decodeBase64(streaminfoBase64)).use { decoder ->
                frameToMap(decoder.decode(decodeBase64(packetBase64)))
            }
        }
    }

    @ReactMethod
    fun encodeFlac(
        samples: ReadableArray,
        frames: Double,
        channels: Double,
        sampleRate: Double,
        promise: Promise
    ) {
        resolveOrReject(promise) {
            val shorts = ShortArray(samples.size()) { index ->
                samples.getDouble(index).toInt().toShort()
            }
            val flac = encodeFlac(
                shorts,
                frames.toLong(),
                channels.toInt(),
                sampleRate.toInt()
            )
            encodeBase64(flac)
        }
    }

    @ReactMethod
    fun parseVp9FrameHeader(frameBase64: String, promise: Promise) {
        resolveOrReject(promise) {
            val header = parseVp9FrameHeader(decodeBase64(frameBase64))
            val map = Arguments.createMap()
            map.putInt("profile", header.profile)
            map.putInt("frameType", header.frameType)
            map.putBoolean("showFrame", header.showFrame)
            map.putBoolean("showExistingFrame", header.showExistingFrame)
            map.putInt("width", header.width)
            map.putInt("height", header.height)
            map.putInt("renderWidth", header.renderWidth)
            map.putInt("renderHeight", header.renderHeight)
            map.putInt("baseQIdx", header.baseQIdx)
            map.putInt("loopFilterLevel", header.loopFilterLevel)
            map.putInt("tileColsLog2", header.tileColsLog2)
            map.putInt("tileRowsLog2", header.tileRowsLog2)
            map.putInt("headerSizeInBytes", header.headerSizeInBytes)
            map
        }
    }

    @ReactMethod
    fun parseAv1SequenceHeader(obuBase64: String, promise: Promise) {
        resolveOrReject(promise) {
            val header = parseAv1SequenceHeader(decodeBase64(obuBase64))
            val map = Arguments.createMap()
            map.putInt("seqProfile", header.seqProfile)
            map.putInt("seqLevelIdx0", header.seqLevelIdx0)
            map.putInt("seqTier0", header.seqTier0)
            map.putInt("maxFrameWidthMinus1", header.maxFrameWidthMinus1)
            map.putInt("maxFrameHeightMinus1", header.maxFrameHeightMinus1)
            map.putInt("frameWidthBitsMinus1", header.frameWidthBitsMinus1)
            map.putInt("frameHeightBitsMinus1", header.frameHeightBitsMinus1)
            map.putBoolean(
                "reducedStillPictureHeader",
                header.reducedStillPictureHeader
            )
            map
        }
    }

    @ReactMethod
    fun encoderNewFlac(sampleRate: Double, channels: Double, promise: Promise) {
        resolveOrReject(promise) {
            val encoder = Encoder.flac(sampleRate.toInt(), channels.toInt())
            synchronized(encoders) {
                val handle = nextEncoderHandle
                nextEncoderHandle += 1
                encoders[handle] = encoder
                handle
            }
        }
    }

    @ReactMethod
    fun encoderStreaminfo(handle: Double, promise: Promise) {
        resolveOrReject(promise) {
            val encoder = synchronized(encoders) {
                encoders[handle.toInt()]
                    ?: throw CodecError(ERR_INVALID_ARGUMENT, "unknown encoder handle")
            }
            encodeBase64(encoder.streaminfo())
        }
    }

    @ReactMethod
    fun encoderEncode(
        handle: Double,
        samples: ReadableArray,
        frames: Double,
        promise: Promise
    ) {
        resolveOrReject(promise) {
            val encoder = synchronized(encoders) {
                encoders[handle.toInt()]
                    ?: throw CodecError(ERR_INVALID_ARGUMENT, "unknown encoder handle")
            }
            val floats = FloatArray(samples.size()) { index ->
                samples.getDouble(index).toFloat()
            }
            encodeBase64(encoder.encode(floats, frames.toLong()))
        }
    }

    @ReactMethod
    fun encoderFree(handle: Double, promise: Promise) {
        resolveOrReject(promise) {
            val encoder = synchronized(encoders) {
                encoders.remove(handle.toInt())
                    ?: throw CodecError(ERR_INVALID_ARGUMENT, "unknown encoder handle")
            }
            encoder.close()
            null
        }
    }

    override fun invalidate() {
        synchronized(encoders) {
            for (encoder in encoders.values) {
                encoder.close()
            }
            encoders.clear()
        }
        super.invalidate()
    }
}
