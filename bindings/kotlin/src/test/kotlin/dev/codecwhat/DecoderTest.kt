package dev.codecwhat

import java.io.File
import kotlin.math.abs
import kotlin.math.sin

// Checks for the Kotlin binding (Phase 10). Run from the workspace
// root (or pass the fixtures directory as the first argument):
//   java -Djava.library.path=bindings/kotlin/build \
//     -cp bindings/kotlin/build/codecwhat-test.jar \
//     dev.codecwhat.DecoderTestKt
// Exits 0 when every check passes, 1 on the first failure.

private fun check(ok: Boolean, what: String) {
    if (!ok) {
        System.err.println("FAIL: $what")
        kotlin.system.exitProcess(1)
    }
    println("ok: $what")
}

private fun dataDir(args: Array<String>): File {
    val candidates = listOf(
        args.firstOrNull(),
        "crates/codec-whatttttttttttt-core/tests/data",
        "bindings/kotlin/../../crates/codec-whatttttttttttt-core/tests/data"
    )
    for (candidate in candidates) {
        if (candidate != null && File(candidate).isDirectory) {
            return File(candidate)
        }
    }
    System.err.println("fixtures directory not found")
    kotlin.system.exitProcess(1)
}

/** Offset of the first FLAC frame: skip the metadata blocks. */
private fun firstFlacFrameOffset(flac: ByteArray): Int {
    var offset = 4
    while (true) {
        val header = flac[offset].toInt() and 0xFF
        val length = ((flac[offset + 1].toInt() and 0xFF) shl 16) or
            ((flac[offset + 2].toInt() and 0xFF) shl 8) or
            (flac[offset + 3].toInt() and 0xFF)
        offset += 4 + length
        if (header and 0x80 != 0) {
            break
        }
    }
    return offset
}

fun main(args: Array<String>) {
    val data = dataDir(args)

    check(version() == "0.1.0", "version is 0.1.0")

    val wav = File(data, "valid_i16_stereo_44100.wav").readBytes()
    val wavProbe = probe(wav)
    check(
        wavProbe.container == CONTAINER_WAV && wavProbe.codec == CODEC_PCM,
        "probe wav"
    )

    val flac = File(data, "sine_stereo_44100.flac").readBytes()
    val flacProbe = probe(flac)
    check(
        flacProbe.container == CONTAINER_RAW_FLAC &&
            flacProbe.codec == CODEC_FLAC,
        "probe flac"
    )

    // Decode the FLAC file through the Kotlin binding.
    val frameOffset = firstFlacFrameOffset(flac)
    Decoder.fromFlacStreaminfo(flac.copyOfRange(8, 42)).use { decoder ->
        val frame = decoder.decode(flac.copyOfRange(frameOffset, flac.size))
        check(
            frame.sampleRate == 44100 && frame.channels == 2 &&
                frame.frames == 4410L,
            "decode flac frame via binding"
        )
        check(abs(frame.samples[0]) < 0.001f, "sine starts at zero")
    }

    // Decode the WAV file.
    Decoder.fromWav(wav).use { decoder ->
        val frame = decoder.decode(wav.copyOfRange(44, wav.size))
        check(
            frame.sampleRate == 44100 && frame.channels == 2 &&
                frame.frames == 4410L,
            "decode wav frame via binding"
        )
    }

    // One-shot FLAC encode + decode round-trip.
    val samples = ShortArray(2000)
    for (n in 0 until 1000) {
        val t = n.toDouble() / 44100.0
        samples[2 * n] = (20000.0 * sin(2.0 * Math.PI * 440.0 * t)).toInt().toShort()
        samples[2 * n + 1] = (15000.0 * sin(2.0 * Math.PI * 660.0 * t)).toInt().toShort()
    }
    val encoded = encodeFlac(samples, 1000, 2, 44100)
    check(
        encoded.size > 42 && encoded[0] == 'f'.toByte() &&
            encoded[1] == 'L'.toByte(),
        "one-shot encode produces a flac file"
    )
    Decoder.fromFlacStreaminfo(encoded.copyOfRange(8, 42)).use { decoder ->
        val frame = decoder.decode(
            encoded.copyOfRange(firstFlacFrameOffset(encoded), encoded.size)
        )
        check(frame.frames == 1000L, "round-trip decodes 1000 frames")
        check(abs(frame.samples[0]) < 1.0f, "round-trip first sample near zero")
    }

    // Streaming encoder.
    Encoder.flac(44100, 2).use { encoder ->
        val header = encoder.streaminfo()
        check(header.size == 42, "streaming streaminfo is 42 bytes")
        val floats = FloatArray(2000)
        for (n in 0 until 1000) {
            val t = n.toFloat() / 44100.0f
            floats[2 * n] = 0.5f * sin(2.0f * Math.PI.toFloat() * 440.0f * t)
            floats[2 * n + 1] = 0.5f * sin(2.0f * Math.PI.toFloat() * 660.0f * t)
        }
        val packet = encoder.encode(floats, 1000)
        check(packet.isNotEmpty(), "streaming encode produces a packet")
    }

    // VP9 header parse.
    val ivf = File(data, "vp9_64x64.ivf").readBytes()
    val frameSize = (ivf[32].toInt() and 0xFF) or
        ((ivf[33].toInt() and 0xFF) shl 8) or
        ((ivf[34].toInt() and 0xFF) shl 16) or
        ((ivf[35].toInt() and 0xFF) shl 24)
    val header = parseVp9FrameHeader(ivf.copyOfRange(44, 44 + frameSize))
    check(
        header.profile == 0 && header.frameType == 0 &&
            header.width == 64 && header.height == 64 &&
            header.baseQIdx == 33,
        "parse vp9 key frame header"
    )

    // VP9 decode is parser-only: the decoder refuses to decode.
    var refused = false
    try {
        Decoder.fromVp9FrameHeader(ivf.copyOfRange(44, 44 + frameSize)).use { vp9 ->
            vp9.decode(ivf.copyOfRange(44, 44 + frameSize))
        }
    } catch (e: CodecError) {
        refused = e.code == ERR_UNSUPPORTED
    }
    check(refused, "vp9 decode returns unsupported")

    // AV1 sequence header parse.
    val obu = File(data, "av1_seq_header.obu").readBytes()
    val sequence = parseAv1SequenceHeader(obu)
    check(
        sequence.seqProfile == 0 && sequence.maxFrameWidthMinus1 == 63 &&
            sequence.maxFrameHeightMinus1 == 63,
        "parse av1 sequence header"
    )

    // A bad WAV raises CodecError instead of crashing.
    var raised = false
    try {
        Decoder.fromWav(byteArrayOf(0, 1, 2, 3))
    } catch (e: CodecError) {
        raised = e.code == ERR_TRUNCATED || e.code == ERR_INVALID_DATA
    }
    check(raised, "bad wav raises CodecError")

    println("all Kotlin checks passed")
}
