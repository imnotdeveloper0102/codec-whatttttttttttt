// Codec.swift — shared types and entry points for the
// codec-whatttttttttttt Swift binding (Phase 10). Thin wrapper
// over the C ABI; no decode or encode logic lives here.

import CCodecWhat

/// An error from the codec library, with the C ABI error code and
/// the detail string from `codec_last_error()`.
public enum CodecError: Error, CustomStringConvertible {
    case io(String)
    case invalidData(String)
    case truncated(String)
    case unsupported(String)
    case overflow(String)
    case allocationLimit(String)
    case invalidArgument(String)
    case licenseRequired(String)

    public var code: codec_error_t {
        switch self {
        case .io: return CODEC_ERR_IO
        case .invalidData: return CODEC_ERR_INVALID_DATA
        case .truncated: return CODEC_ERR_TRUNCATED
        case .unsupported: return CODEC_ERR_UNSUPPORTED
        case .overflow: return CODEC_ERR_OVERFLOW
        case .allocationLimit: return CODEC_ERR_ALLOCATION_LIMIT
        case .invalidArgument: return CODEC_ERR_INVALID_ARGUMENT
        case .licenseRequired: return CODEC_ERR_LICENSE_REQUIRED
        }
    }

    public var detail: String {
        switch self {
        case .io(let detail),
            .invalidData(let detail),
            .truncated(let detail),
            .unsupported(let detail),
            .overflow(let detail),
            .allocationLimit(let detail),
            .invalidArgument(let detail),
            .licenseRequired(let detail):
            return detail
        }
    }

    public var description: String {
        "\(String(cString: codec_error_string(code))): \(detail)"
    }
}

func check(_ rc: codec_error_t) throws {
    guard rc != CODEC_OK else { return }
    let detail: String
    if let last = codec_last_error() {
        detail = String(cString: last)
    } else {
        detail = String(cString: codec_error_string(rc))
    }
    switch rc {
    case CODEC_ERR_IO: throw CodecError.io(detail)
    case CODEC_ERR_INVALID_DATA: throw CodecError.invalidData(detail)
    case CODEC_ERR_TRUNCATED: throw CodecError.truncated(detail)
    case CODEC_ERR_UNSUPPORTED: throw CodecError.unsupported(detail)
    case CODEC_ERR_OVERFLOW: throw CodecError.overflow(detail)
    case CODEC_ERR_ALLOCATION_LIMIT: throw CodecError.allocationLimit(detail)
    case CODEC_ERR_LICENSE_REQUIRED: throw CodecError.licenseRequired(detail)
    default: throw CodecError.invalidArgument(detail)
    }
}

/// Library version, e.g. "0.1.0".
public func version() -> String {
    String(cString: codec_version())
}

/// Result of `probe`: container and codec of the first bytes.
public struct ProbeResult {
    public let container: codec_container_t
    public let codec: codec_codec_t

    public var containerName: String {
        switch container {
        case CODEC_CONTAINER_WAV: return "wav"
        case CODEC_CONTAINER_RAW_FLAC: return "raw-flac"
        case CODEC_CONTAINER_OGG: return "ogg"
        case CODEC_CONTAINER_WEBM: return "webm"
        case CODEC_CONTAINER_MP4: return "mp4"
        case CODEC_CONTAINER_IVF: return "ivf"
        default: return "unknown"
        }
    }

    public var codecName: String {
        switch codec {
        case CODEC_CODEC_PCM: return "pcm"
        case CODEC_CODEC_FLAC: return "flac"
        case CODEC_CODEC_OPUS: return "opus"
        case CODEC_CODEC_VORBIS: return "vorbis"
        case CODEC_CODEC_AV1: return "av1"
        case CODEC_CODEC_VP9: return "vp9"
        default: return "unknown"
        }
    }
}

/// Identify the container and codec of the first bytes of a file.
/// The codec is `CODEC_CODEC_UNKNOWN` for Ogg, WebM, and MP4:
/// those containers hold several streams and need a demuxer to
/// name the codec.
public func probe(_ data: [UInt8]) throws -> ProbeResult {
    var out = codec_probe_t(container: 0, codec: 0)
    try data.withUnsafeBufferPointer { buffer in
        try check(codec_probe(
            buffer.baseAddress, numericCast(data.count), &out))
    }
    return ProbeResult(container: out.container, codec: out.codec)
}

/// An interleaved f32 audio frame. `samples` has
/// `frames * channels` values.
public struct AudioFrame {
    public let sampleRate: UInt32
    public let channels: UInt16
    public let frames: UInt64
    public let timestampUs: UInt64
    public let samples: [Float]
}

/// A parsed VP9 frame header (parser-only; no reconstruction).
public struct Vp9FrameHeader {
    public let profile: UInt8
    public let frameType: UInt8 // 0 = key, 1 = inter
    public let showFrame: Bool
    public let showExistingFrame: Bool
    public let width: UInt16
    public let height: UInt16
    public let renderWidth: UInt16
    public let renderHeight: UInt16
    public let baseQIdx: UInt8
    public let loopFilterLevel: UInt8
    public let tileColsLog2: UInt8
    public let tileRowsLog2: UInt8
    public let headerSizeInBytes: UInt32
}

/// Parse a VP9 frame header (parser-only; no reconstruction).
public func parseVp9FrameHeader(_ frame: [UInt8]) throws -> Vp9FrameHeader {
    var out = codec_vp9_frame_header_t(
        profile: 0, frame_type: 0, show_frame: 0, show_existing_frame: 0,
        width: 0, height: 0, render_width: 0, render_height: 0,
        base_q_idx: 0, loop_filter_level: 0, tile_cols_log2: 0,
        tile_rows_log2: 0, header_size_in_bytes: 0)
    try frame.withUnsafeBufferPointer { buffer in
        try check(codec_vp9_parse_frame_header(
            buffer.baseAddress, numericCast(frame.count), &out))
    }
    return Vp9FrameHeader(
        profile: out.profile, frameType: out.frame_type,
        showFrame: out.show_frame != 0,
        showExistingFrame: out.show_existing_frame != 0,
        width: out.width, height: out.height,
        renderWidth: out.render_width, renderHeight: out.render_height,
        baseQIdx: out.base_q_idx,
        loopFilterLevel: out.loop_filter_level,
        tileColsLog2: out.tile_cols_log2,
        tileRowsLog2: out.tile_rows_log2,
        headerSizeInBytes: out.header_size_in_bytes)
}

/// A parsed AV1 sequence header (first operating point).
public struct Av1SequenceHeader {
    public let seqProfile: UInt8
    public let seqLevelIdx0: UInt8
    public let seqTier0: UInt8
    public let maxFrameWidthMinus1: UInt16
    public let maxFrameHeightMinus1: UInt16
    public let frameWidthBitsMinus1: UInt8
    public let frameHeightBitsMinus1: UInt8
    public let reducedStillPictureHeader: Bool
}

/// Parse one AV1 sequence-header OBU (header + payload).
public func parseAv1SequenceHeader(_ obu: [UInt8]) throws -> Av1SequenceHeader {
    var out = codec_av1_sequence_header_t(
        seq_profile: 0, seq_level_idx_0: 0, seq_tier_0: 0,
        max_frame_width_minus_1: 0, max_frame_height_minus_1: 0,
        frame_width_bits_minus_1: 0, frame_height_bits_minus_1: 0,
        reduced_still_picture_header: 0)
    try obu.withUnsafeBufferPointer { buffer in
        try check(codec_av1_parse_sequence_header(
            buffer.baseAddress, numericCast(obu.count), &out))
    }
    return Av1SequenceHeader(
        seqProfile: out.seq_profile,
        seqLevelIdx0: out.seq_level_idx_0,
        seqTier0: out.seq_tier_0,
        maxFrameWidthMinus1: out.max_frame_width_minus_1,
        maxFrameHeightMinus1: out.max_frame_height_minus_1,
        frameWidthBitsMinus1: out.frame_width_bits_minus_1,
        frameHeightBitsMinus1: out.frame_height_bits_minus_1,
        reducedStillPictureHeader: out.reduced_still_picture_header != 0)
}

/// One-shot FLAC encode: interleaved i16 samples -> a complete
/// FLAC file with exact STREAMINFO. `frames` is the number of
/// frames (samples per channel).
public func encodeFlac(
    samples: [Int16], frames: UInt64, channels: UInt16, sampleRate: UInt32
) throws -> [UInt8] {
    var data: UnsafeMutablePointer<UInt8>?
    var len: Int = 0
    try samples.withUnsafeBufferPointer { buffer in
        try check(codec_encode_flac(
            buffer.baseAddress, frames, channels, sampleRate, &data, &len))
    }
    guard let data else {
        throw CodecError.invalidArgument("encoder returned no data")
    }
    defer { codec_free(data) }
    return Array(UnsafeBufferPointer(start: data, count: len))
}
