// Decoder.swift — audio decoder for the codec-whatttttttttttt
// Swift binding (Phase 10). Thin wrapper over the C ABI.

import CCodecWhat

/// Audio decoder. Build from a WAV file, a FLAC STREAMINFO block,
/// or a VP9 frame header, then decode one packet at a time. A
/// decoder is used from one thread at a time. The native handle is
/// freed in `deinit`; call `close()` to free it earlier.
public final class Decoder {
    private var handle: OpaquePointer?

    private init(handle: OpaquePointer) {
        self.handle = handle
    }

    deinit { close() }

    /// Build a decoder from a WAV file's bytes.
    public static func fromWav(_ data: [UInt8]) throws -> Decoder {
        var handle: OpaquePointer?
        try data.withUnsafeBufferPointer { buffer in
            try check(codec_decoder_from_wav_header(
                buffer.baseAddress, numericCast(data.count), &handle))
        }
        guard let handle else {
            throw CodecError.invalidArgument("decoder creation returned no handle")
        }
        return Decoder(handle: handle)
    }

    /// Build a decoder from a 34-byte FLAC STREAMINFO payload.
    public static func fromFlacStreaminfo(_ streaminfo: [UInt8]) throws -> Decoder {
        guard streaminfo.count == 34 else {
            throw CodecError.invalidArgument(
                "streaminfo must be 34 bytes, got \(streaminfo.count)")
        }
        var handle: OpaquePointer?
        try streaminfo.withUnsafeBufferPointer { buffer in
            try check(codec_decoder_from_flac_streaminfo(
                buffer.baseAddress, numericCast(streaminfo.count), &handle))
        }
        guard let handle else {
            throw CodecError.invalidArgument("decoder creation returned no handle")
        }
        return Decoder(handle: handle)
    }

    /// Build a decoder from a VP9 frame header. `decode` on it
    /// throws `CodecError.unsupported`: VP9 is parser-only.
    public static func fromVp9FrameHeader(_ frame: [UInt8]) throws -> Decoder {
        var handle: OpaquePointer?
        try frame.withUnsafeBufferPointer { buffer in
            try check(codec_decoder_from_vp9_frame_header(
                buffer.baseAddress, numericCast(frame.count), &handle))
        }
        guard let handle else {
            throw CodecError.invalidArgument("decoder creation returned no handle")
        }
        return Decoder(handle: handle)
    }

    /// Decode one packet. A FLAC packet holds exactly one frame;
    /// an Opus packet may hold several frames of audio.
    public func decode(_ packet: [UInt8], timestampUs: UInt64 = 0) throws -> AudioFrame {
        guard let handle else {
            throw CodecError.invalidArgument("decoder is closed")
        }
        var view: UnsafePointer<codec_audio_frame_t>?
        try packet.withUnsafeBufferPointer { buffer in
            try check(codec_decoder_decode(
                handle, buffer.baseAddress, numericCast(packet.count),
                timestampUs, &view))
        }
        guard let frame = view?.pointee else {
            throw CodecError.invalidArgument("decoder returned no frame")
        }
        let count = Int(frame.frames) * Int(frame.channels)
        let samples: [Float]
        if count == 0 {
            samples = []
        } else {
            guard let base = frame.samples else {
                throw CodecError.invalidArgument("decoder returned no samples")
            }
            samples = Array(UnsafeBufferPointer(start: base, count: count))
        }
        return AudioFrame(
            sampleRate: frame.sample_rate, channels: frame.channels,
            frames: frame.frames, timestampUs: frame.timestamp_us,
            samples: samples)
    }

    /// Free the native handle. Safe to call twice.
    public func close() {
        if let handle {
            codec_decoder_free(handle)
            self.handle = nil
        }
    }
}
