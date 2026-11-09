package dev.codecwhat

// Shared types and entry points for the codec-whatttttttttttt
// Kotlin binding (Phase 10). Thin JNI wrapper over the C ABI; no
// decode or encode logic lives here.

/** An error from the codec library, with its C ABI error code. */
class CodecError(val code: Int, message: String) : RuntimeException(message)

// Error codes (must match codec_whatttttttttttt.h).
const val ERR_IO = 1
const val ERR_INVALID_DATA = 2
const val ERR_TRUNCATED = 3
const val ERR_UNSUPPORTED = 4
const val ERR_OVERFLOW = 5
const val ERR_ALLOCATION_LIMIT = 6
const val ERR_INVALID_ARGUMENT = 7
const val ERR_LICENSE_REQUIRED = 8

// Codec ids (must match codec_whatttttttttttt.h).
const val CODEC_UNKNOWN = -1
const val CODEC_PCM = 0
const val CODEC_FLAC = 1
const val CODEC_OPUS = 2
const val CODEC_VORBIS = 3
const val CODEC_AV1 = 4
const val CODEC_VP9 = 5

// Container ids (must match codec_whatttttttttttt.h).
const val CONTAINER_UNKNOWN = 0
const val CONTAINER_WAV = 1
const val CONTAINER_RAW_FLAC = 2
const val CONTAINER_OGG = 3
const val CONTAINER_WEBM = 4
const val CONTAINER_MP4 = 5
const val CONTAINER_IVF = 6

/** Result of [probe]: container and codec of the first bytes. */
data class ProbeResult(val container: Int, val codec: Int) {
    val containerName: String
        get() = when (container) {
            CONTAINER_WAV -> "wav"
            CONTAINER_RAW_FLAC -> "raw-flac"
            CONTAINER_OGG -> "ogg"
            CONTAINER_WEBM -> "webm"
            CONTAINER_MP4 -> "mp4"
            CONTAINER_IVF -> "ivf"
            else -> "unknown"
        }
    val codecName: String
        get() = when (codec) {
            CODEC_PCM -> "pcm"
            CODEC_FLAC -> "flac"
            CODEC_OPUS -> "opus"
            CODEC_VORBIS -> "vorbis"
            CODEC_AV1 -> "av1"
            CODEC_VP9 -> "vp9"
            else -> "unknown"
        }
}

/** An interleaved f32 audio frame. `samples` has `frames * channels` values. */
data class AudioFrame(
    val sampleRate: Int,
    val channels: Int,
    val frames: Long,
    val timestampUs: Long,
    val samples: FloatArray
)

/** A parsed VP9 frame header (parser-only; no reconstruction). */
data class Vp9FrameHeader(
    val profile: Int,
    val frameType: Int, // 0 = key, 1 = inter
    val showFrame: Boolean,
    val showExistingFrame: Boolean,
    val width: Int,
    val height: Int,
    val renderWidth: Int,
    val renderHeight: Int,
    val baseQIdx: Int,
    val loopFilterLevel: Int,
    val tileColsLog2: Int,
    val tileRowsLog2: Int,
    val headerSizeInBytes: Int
)

/** A parsed AV1 sequence header (first operating point). */
data class Av1SequenceHeader(
    val seqProfile: Int,
    val seqLevelIdx0: Int,
    val seqTier0: Int,
    val maxFrameWidthMinus1: Int,
    val maxFrameHeightMinus1: Int,
    val frameWidthBitsMinus1: Int,
    val frameHeightBitsMinus1: Int,
    val reducedStillPictureHeader: Boolean
)

/**
 * JNI entry points. `private` on purpose: users call [Decoder],
 * [Encoder], and the top-level functions, never these. The native
 * library is `libcodecwhat_jni.so`, built from
 * `src/main/cpp/jni_bridge.cpp` (see README.md).
 */
internal object Native {
    init {
        System.loadLibrary("codecwhat_jni")
    }

    external fun nativeVersion(): String
    external fun nativeProbe(data: ByteArray): IntArray
    external fun nativeDecoderFromWav(data: ByteArray): Long
    external fun nativeDecoderFromFlacStreaminfo(streaminfo: ByteArray): Long
    external fun nativeDecoderFromVp9FrameHeader(frame: ByteArray): Long
    external fun nativeDecoderDecode(
        handle: Long,
        packet: ByteArray,
        timestampUs: Long
    ): AudioFrame

    external fun nativeDecoderFree(handle: Long)
    external fun nativeEncoderNewFlac(sampleRate: Int, channels: Int): Long
    external fun nativeEncoderFlacStreaminfo(handle: Long): ByteArray
    external fun nativeEncoderEncode(
        handle: Long,
        samples: FloatArray,
        frames: Long
    ): ByteArray

    external fun nativeEncoderFree(handle: Long)
    external fun nativeEncodeFlac(
        samples: ShortArray,
        frames: Long,
        channels: Int,
        sampleRate: Int
    ): ByteArray

    external fun nativeParseVp9FrameHeader(frame: ByteArray): Vp9FrameHeader
    external fun nativeParseAv1SequenceHeader(obu: ByteArray): Av1SequenceHeader
}

/** Library version, e.g. "0.1.0". */
fun version(): String = Native.nativeVersion()

/**
 * Identify the container and codec of the first bytes of a file.
 * The codec is [CODEC_UNKNOWN] for Ogg, WebM, and MP4: those
 * containers hold several streams and need a demuxer to name the
 * codec.
 */
fun probe(data: ByteArray): ProbeResult {
    val pair = Native.nativeProbe(data)
    return ProbeResult(pair[0], pair[1])
}

/**
 * One-shot FLAC encode: interleaved i16 samples -> a complete FLAC
 * file with exact STREAMINFO. `frames` is the number of frames
 * (samples per channel).
 */
fun encodeFlac(
    samples: ShortArray,
    frames: Long,
    channels: Int,
    sampleRate: Int
): ByteArray = Native.nativeEncodeFlac(samples, frames, channels, sampleRate)

/** Parse a VP9 frame header (parser-only; no reconstruction). */
fun parseVp9FrameHeader(frame: ByteArray): Vp9FrameHeader =
    Native.nativeParseVp9FrameHeader(frame)

/** Parse one AV1 sequence-header OBU (header + payload). */
fun parseAv1SequenceHeader(obu: ByteArray): Av1SequenceHeader =
    Native.nativeParseAv1SequenceHeader(obu)
