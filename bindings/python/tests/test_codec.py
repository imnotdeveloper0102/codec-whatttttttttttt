"""Tests for the Python bindings (Phase 10). Run from the
bindings/python directory: `python3 -m unittest discover -s tests -v`.
The shared library must be built first
(`cargo build -p codec-whatttttttttttt-c`).
"""

import math
import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codec_whatttttttttttt import (
    CODEC_AV1,
    CODEC_FLAC,
    CODEC_PCM,
    CODEC_VP9,
    CONTAINER_IVF,
    CONTAINER_RAW_FLAC,
    CONTAINER_WAV,
    ERR_INVALID_ARGUMENT,
    ERR_UNSUPPORTED,
    CodecError,
    Decoder,
    Encoder,
    encode_flac,
    parse_av1_sequence_header,
    parse_vp9_frame_header,
    probe,
    version,
)

DATA = (
    Path(__file__).resolve().parents[3]
    / "crates"
    / ("codec-whatttttttttttt-core")
    / "tests"
    / "data"
)


def read_fixture(name):
    return (DATA / name).read_bytes()


def first_flac_frame_offset(flac):
    """Byte offset of the first frame, by skipping metadata blocks."""
    offset = 4
    while True:
        header = flac[offset]
        length = int.from_bytes(flac[offset + 1 : offset + 4], "big")
        offset += 4 + length
        if header & 0x80:
            return offset


def first_ivf_frame(ivf):
    """(frame_bytes) of the first frame in an IVF file."""
    size = struct.unpack_from("<I", ivf, 32)[0]
    return ivf[44 : 44 + size]


def find_sequence_header_obu(frame):
    """Walk the OBUs in a frame and return the sequence-header OBU
    bytes (header + payload)."""
    offset = 0
    while offset < len(frame):
        first = frame[offset]
        obu_type = (first >> 3) & 0x0F
        has_extension = (first >> 2) & 1
        has_size = (first >> 1) & 1
        pos = offset + 1
        if has_extension:
            pos += 1
        if has_size:
            size = 0
            shift = 0
            while True:
                byte = frame[pos]
                pos += 1
                size |= (byte & 0x7F) << shift
                shift += 7
                if not byte & 0x80:
                    break
            end = pos + size
        else:
            end = len(frame)
        if obu_type == 1:  # sequence header
            return frame[offset:end]
        offset = end
    raise AssertionError("no sequence header obu in frame")


class TestVersionAndProbe(unittest.TestCase):
    def test_version(self):
        self.assertEqual(version(), "0.1.0")

    def test_probe_wav_flac_ivf(self):
        result = probe(read_fixture("valid_i16_stereo_44100.wav"))
        self.assertEqual(result.container, CONTAINER_WAV)
        self.assertEqual(result.codec, CODEC_PCM)
        self.assertEqual(result.container_name, "wav")
        self.assertEqual(result.codec_name, "pcm")

        result = probe(read_fixture("sine_stereo_44100.flac"))
        self.assertEqual(result.container, CONTAINER_RAW_FLAC)
        self.assertEqual(result.codec, CODEC_FLAC)

        result = probe(read_fixture("vp9_64x64.ivf"))
        self.assertEqual(result.container, CONTAINER_IVF)
        self.assertEqual(result.codec, CODEC_VP9)

        result = probe(read_fixture("av1_64x64.ivf"))
        self.assertEqual(result.container, CONTAINER_IVF)
        self.assertEqual(result.codec, CODEC_AV1)

    def test_probe_garbage_is_unknown_not_error(self):
        result = probe(b"\xde\xad\xbe\xef\x00\x01\x02\x03")
        self.assertEqual(result.container, 0)
        self.assertEqual(result.codec, -1)

    def test_probe_short_input_is_truncated(self):
        with self.assertRaises(CodecError) as ctx:
            probe(b"RI")
        self.assertEqual(ctx.exception.code, 3)  # ERR_TRUNCATED


class TestWavDecode(unittest.TestCase):
    def test_decode_wav_round_trip(self):
        wav = read_fixture("valid_i16_stereo_44100.wav")
        with Decoder.from_wav(wav) as decoder:
            frame = decoder.decode(wav[44:])
        self.assertEqual(frame.sample_rate, 44100)
        self.assertEqual(frame.channels, 2)
        self.assertEqual(frame.frames, 4410)
        self.assertEqual(len(frame.samples), 8820)
        self.assertAlmostEqual(frame.samples[0], 0.0, places=3)
        self.assertTrue(any(s > 0.5 for s in frame.samples))

    def test_decode_bad_wav_raises(self):
        with self.assertRaises(CodecError):
            Decoder.from_wav(b"RIFF....WAVE" + b"\x00" * 32)


class TestFlac(unittest.TestCase):
    def test_decode_flac_frame(self):
        flac = read_fixture("sine_stereo_44100.flac")
        offset = first_flac_frame_offset(flac)
        with Decoder.from_flac_streaminfo(flac[8:42]) as decoder:
            frame = decoder.decode(flac[offset:])
        self.assertEqual(frame.sample_rate, 44100)
        self.assertEqual(frame.channels, 2)
        self.assertEqual(frame.frames, 4410)
        self.assertAlmostEqual(frame.samples[0], 0.0, places=3)

    def test_encode_flac_one_shot_round_trips(self):
        samples = []
        for n in range(1000):
            t = n / 44100.0
            samples.append(int(20000 * math.sin(2 * math.pi * 440 * t)))
            samples.append(int(15000 * math.sin(2 * math.pi * 660 * t)))
        encoded = encode_flac(samples, frames=1000, channels=2, sample_rate=44100)
        self.assertEqual(encoded[:4], b"fLaC")
        # Total samples is the 36-bit field at STREAMINFO bytes 13..17.
        total = ((encoded[21] & 0x0F) << 32) | int.from_bytes(encoded[22:26], "big")
        self.assertEqual(total, 1000)
        # Decode it back and compare.
        offset = first_flac_frame_offset(encoded)
        with Decoder.from_flac_streaminfo(encoded[8:42]) as decoder:
            frame = decoder.decode(encoded[offset:])
        self.assertEqual(frame.frames, 1000)
        for got, want in zip(frame.samples, samples):
            self.assertEqual(round(got * 32768), want)

    def test_streaming_encoder_round_trips(self):
        with Encoder.flac(8000, 1) as encoder:
            header = encoder.streaminfo()
        self.assertEqual(len(header), 42)
        self.assertEqual(header[:4], b"fLaC")
        with (
            Encoder.flac(8000, 1) as encoder,
            Decoder.from_flac_streaminfo(encoder.streaminfo()[8:42]) as decoder,
        ):
            samples = [
                0.5 * math.sin(2 * math.pi * 440 * n / 8000) for n in range(1000)
            ]
            packet = encoder.encode(samples, frames=1000)
            self.assertEqual(packet[0], 0xFF)
            frame = decoder.decode(packet)
        self.assertEqual(frame.frames, 1000)
        self.assertEqual(frame.sample_rate, 8000)
        for got, want in zip(frame.samples, samples):
            self.assertAlmostEqual(got, want, places=4)

    def test_encode_wrong_sample_count_raises(self):
        with self.assertRaises(CodecError) as ctx:
            encode_flac([0, 0, 0], frames=1000, channels=2, sample_rate=44100)
        self.assertEqual(ctx.exception.code, ERR_INVALID_ARGUMENT)


class TestVideoParsers(unittest.TestCase):
    def test_vp9_frame_header(self):
        ivf = read_fixture("vp9_64x64.ivf")
        header = parse_vp9_frame_header(first_ivf_frame(ivf))
        self.assertEqual(header.profile, 0)
        self.assertEqual(header.frame_type, 0)
        self.assertTrue(header.show_frame)
        self.assertEqual(header.width, 64)
        self.assertEqual(header.height, 64)
        self.assertEqual(header.base_q_idx, 33)
        self.assertEqual(header.loop_filter_level, 3)
        self.assertEqual(header.header_size_in_bytes, 63)

    def test_vp9_decode_is_unsupported(self):
        ivf = read_fixture("vp9_64x64.ivf")
        frame = first_ivf_frame(ivf)
        # Parenthesised `with` needs Python 3.10; the binding
        # supports 3.9, so the continuation uses a backslash.
        with (
            Decoder.from_vp9_frame_header(frame) as decoder,
            self.assertRaises(CodecError) as ctx,
        ):
            decoder.decode(frame)
        self.assertEqual(ctx.exception.code, ERR_UNSUPPORTED)

    def test_av1_sequence_header(self):
        ivf = read_fixture("av1_64x64.ivf")
        obu = find_sequence_header_obu(first_ivf_frame(ivf))
        header = parse_av1_sequence_header(obu)
        self.assertEqual(header.seq_profile, 0)
        self.assertEqual(header.max_frame_width_minus_1, 63)
        self.assertEqual(header.max_frame_height_minus_1, 63)


if __name__ == "__main__":
    unittest.main()
