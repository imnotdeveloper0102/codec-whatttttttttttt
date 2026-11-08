// Encoder.swift — FLAC encoder for the codec-whatttttttttttt
// Swift binding (Phase 10). Thin wrapper over the C ABI.

import CCodecWhat

/// FLAC encoder (streaming). One input frame becomes one FLAC
/// frame, block size 1..=4096. The native handle is freed in
/// `deinit`; call `close()` to free it earlier.
public final class Encoder {
    private var handle: OpaquePointer?
    public let sampleRate: UInt32
    public let channels: UInt16

    private init(
        handle: OpaquePointer,
        sampleRate: UInt32, channels: UInt16
    ) {
        self.handle = handle
        self.sampleRate = sampleRate
        self.channels = channels
    }

    deinit { close() }

    /// Build a FLAC encoder. `sampleRate` is 1...192000,
    /// `channels` 1...8.
    public static func flac(sampleRate: UInt32, channels: UInt16) throws -> Encoder {
        var handle: OpaquePointer?
        try check(codec_encoder_new_flac(sampleRate, channels, &handle))
        guard let handle else {
            throw CodecError.invalidArgument("encoder creation returned no handle")
        }
        return Encoder(handle: handle, sampleRate: sampleRate, channels: channels)
    }

    /// The streaming STREAMINFO header (42 bytes: "fLaC" + block
    /// header + 34-byte STREAMINFO, total samples 0).
    public func streaminfo() throws -> [UInt8] {
        guard let handle else {
            throw CodecError.invalidArgument("encoder is closed")
        }
        var view: UnsafePointer<codec_packet_t>?
        try check(codec_encoder_flac_streaminfo(handle, &view))
        guard let packet = view?.pointee else {
            throw CodecError.invalidArgument("encoder returned no packet")
        }
        guard let base = packet.data else { return [] }
        return Array(UnsafeBufferPointer(start: base, count: packet.len))
    }

    /// Encode one frame of interleaved f32 samples. `frames` is
    /// the number of frames (samples per channel), 1...4096.
    public func encode(samples: [Float], frames: UInt64) throws -> [UInt8] {
        guard let handle else {
            throw CodecError.invalidArgument("encoder is closed")
        }
        guard samples.count == Int(frames) * Int(channels) else {
            throw CodecError.invalidArgument(
                "expected \(frames * UInt64(channels)) samples, got \(samples.count)")
        }
        var view: UnsafePointer<codec_packet_t>?
        try samples.withUnsafeBufferPointer { buffer in
            try check(codec_encoder_encode(
                handle, buffer.baseAddress, frames, &view))
        }
        guard let packet = view?.pointee else {
            throw CodecError.invalidArgument("encoder returned no packet")
        }
        guard let base = packet.data else { return [] }
        return Array(UnsafeBufferPointer(start: base, count: packet.len))
    }

    /// Free the native handle. Safe to call twice.
    public func close() {
        if let handle {
            codec_encoder_free(handle)
            self.handle = nil
        }
    }
}
