// DecoderTests.swift — tests for the CodecWhat Swift binding
// (Phase 10). Run with `swift test` from bindings/swift. The
// fixtures are the pinned files in the core crate's tests/data.

import CCodecWhat
import Foundation
import XCTest

@testable import CodecWhat

final class DecoderTests: XCTestCase {
    /// Fixtures directory: five parents up from this file is the
    /// workspace root; `crates/.../tests/data` holds the vectors.
    static var fixtures: URL {
        var url = URL(fileURLWithPath: #file)
        for _ in 0..<5 {
            url = url.deletingLastPathComponent()
        }
        return url
            .appendingPathComponent("crates")
            .appendingPathComponent("codec-whatttttttttttt-core")
            .appendingPathComponent("tests")
            .appendingPathComponent("data")
    }

    func readFixture(_ name: String) throws -> [UInt8] {
        let url = Self.fixtures.appendingPathComponent(name)
        return [UInt8](try Data(contentsOf: url))
    }

    /// Offset of the first FLAC frame: skip the metadata blocks.
    func firstFlacFrameOffset(_ flac: [UInt8]) -> Int {
        var offset = 4
        while true {
            let header = flac[offset]
            let length = (Int(flac[offset + 1]) << 16)
                | (Int(flac[offset + 2]) << 8)
                | Int(flac[offset + 3])
            offset += 4 + length
            if header & 0x80 != 0 {
                break
            }
        }
        return offset
    }

    func testVersion() {
        XCTAssertEqual(version(), "0.1.0")
    }

    func testProbeWavAndFlac() throws {
        let wav = try readFixture("valid_i16_stereo_44100.wav")
        let wavProbe = try probe(wav)
        XCTAssertEqual(wavProbe.container, CODEC_CONTAINER_WAV)
        XCTAssertEqual(wavProbe.codec, CODEC_CODEC_PCM)

        let flac = try readFixture("sine_stereo_44100.flac")
        let flacProbe = try probe(flac)
        XCTAssertEqual(flacProbe.container, CODEC_CONTAINER_RAW_FLAC)
        XCTAssertEqual(flacProbe.codec, CODEC_CODEC_FLAC)
    }

    func testDecodeFlacFrame() throws {
        let flac = try readFixture("sine_stereo_44100.flac")
        let decoder = try Decoder.fromFlacStreaminfo(
            Array(flac[8..<42]))
        defer { decoder.close() }
        let frame = try decoder.decode(
            Array(flac[firstFlacFrameOffset(flac)...]))
        XCTAssertEqual(frame.sampleRate, 44100)
        XCTAssertEqual(frame.channels, 2)
        XCTAssertEqual(frame.frames, 4410)
        XCTAssertLessThan(abs(frame.samples[0]), 0.001)
    }

    func testDecodeWavFrame() throws {
        let wav = try readFixture("valid_i16_stereo_44100.wav")
        let decoder = try Decoder.fromWav(wav)
        defer { decoder.close() }
        let frame = try decoder.decode(Array(wav[44...]))
        XCTAssertEqual(frame.sampleRate, 44100)
        XCTAssertEqual(frame.frames, 4410)
    }

    func testEncodeFlacOneShotRoundTrips() throws {
        var samples = [Int16](repeating: 0, count: 2000)
        for n in 0..<1000 {
            let t = Double(n) / 44100.0
            samples[2 * n] = Int16(20000.0 * sin(2.0 * Double.pi * 440.0 * t))
            samples[2 * n + 1] = Int16(15000.0 * sin(2.0 * Double.pi * 660.0 * t))
        }
        let encoded = try encodeFlac(
            samples: samples, frames: 1000, channels: 2, sampleRate: 44100)
        XCTAssertGreaterThan(encoded.count, 42)
        XCTAssertEqual(Array(encoded[0..<4]), [UInt8]("fLaC".utf8))
        let decoder = try Decoder.fromFlacStreaminfo(
            Array(encoded[8..<42]))
        defer { decoder.close() }
        let frame = try decoder.decode(
            Array(encoded[firstFlacFrameOffset(encoded)...]))
        XCTAssertEqual(frame.frames, 1000)
        XCTAssertLessThan(abs(frame.samples[0]), 1.0)
    }

    func testStreamingEncoder() throws {
        let encoder = try Encoder.flac(sampleRate: 44100, channels: 2)
        defer { encoder.close() }
        let header = try encoder.streaminfo()
        XCTAssertEqual(header.count, 42)
        var floats = [Float](repeating: 0, count: 2000)
        for n in 0..<1000 {
            let t = Float(n) / 44100.0
            floats[2 * n] = 0.5 * sin(2.0 * Float.pi * 440.0 * t)
            floats[2 * n + 1] = 0.5 * sin(2.0 * Float.pi * 660.0 * t)
        }
        let packet = try encoder.encode(samples: floats, frames: 1000)
        XCTAssertFalse(packet.isEmpty)
    }

    func testParseVp9KeyFrameHeader() throws {
        let ivf = try readFixture("vp9_64x64.ivf")
        let frameSize = Int(ivf[32])
            | (Int(ivf[33]) << 8)
            | (Int(ivf[34]) << 16)
            | (Int(ivf[35]) << 24)
        let header = try parseVp9FrameHeader(Array(ivf[44..<(44 + frameSize)]))
        XCTAssertEqual(header.profile, 0)
        XCTAssertEqual(header.frameType, 0)
        XCTAssertEqual(header.width, 64)
        XCTAssertEqual(header.height, 64)
        XCTAssertEqual(header.baseQIdx, 33)
    }

    func testVp9DecodeIsUnsupported() throws {
        let ivf = try readFixture("vp9_64x64.ivf")
        let frameSize = Int(ivf[32])
            | (Int(ivf[33]) << 8)
            | (Int(ivf[34]) << 16)
            | (Int(ivf[35]) << 24)
        let frame = Array(ivf[44..<(44 + frameSize)])
        let decoder = try Decoder.fromVp9FrameHeader(frame)
        defer { decoder.close() }
        XCTAssertThrowsError(try decoder.decode(frame)) { error in
            guard case CodecError.unsupported = error else {
                return XCTFail("expected CodecError.unsupported, got \(error)")
            }
        }
    }

    func testParseAv1SequenceHeader() throws {
        let obu = try readFixture("av1_seq_header.obu")
        let header = try parseAv1SequenceHeader(obu)
        XCTAssertEqual(header.seqProfile, 0)
        XCTAssertEqual(header.maxFrameWidthMinus1, 63)
        XCTAssertEqual(header.maxFrameHeightMinus1, 63)
    }

    func testBadWavThrows() {
        XCTAssertThrowsError(try Decoder.fromWav([0, 1, 2, 3])) { error in
            guard let codecError = error as? CodecError else {
                return XCTFail("expected CodecError, got \(error)")
            }
            XCTAssertTrue(
                codecError.code == CODEC_ERR_TRUNCATED
                    || codecError.code == CODEC_ERR_INVALID_DATA)
        }
    }
}
