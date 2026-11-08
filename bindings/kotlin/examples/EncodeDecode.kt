package dev.codecwhat

import kotlin.math.sin

// Example: encode a stereo sine wave to FLAC, then decode it back
// through the Kotlin binding.
//
// Build (see bindings/kotlin/README.md) and run:
//   java -Djava.library.path=bindings/kotlin/build \
//     -cp bindings/kotlin/build/codecwhat.jar \
//     dev.codecwhat.EncodeDecodeKt

fun main() {
    println("codec-whatttttttttttt ${version()}")

    val frames = 4410
    val samples = ShortArray(frames * 2)
    for (n in 0 until frames) {
        val t = n.toDouble() / 44100.0
        samples[2 * n] = (20000.0 * sin(2.0 * Math.PI * 440.0 * t)).toInt().toShort()
        samples[2 * n + 1] = (15000.0 * sin(2.0 * Math.PI * 660.0 * t)).toInt().toShort()
    }
    val flac = encodeFlac(samples, frames.toLong(), 2, 44100)
    println("encoded ${flac.size} bytes of FLAC")

    Decoder.fromFlacStreaminfo(flac.copyOfRange(8, 42)).use { decoder ->
        // Skip the metadata blocks to reach the first frame.
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
        val frame = decoder.decode(flac.copyOfRange(offset, flac.size))
        println(
            "decoded ${frame.frames} frames at ${frame.sampleRate} Hz, " +
                "${frame.channels} channels, first sample ${frame.samples[0]}"
        )
    }
}
