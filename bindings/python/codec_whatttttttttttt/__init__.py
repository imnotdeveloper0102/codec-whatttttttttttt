"""Python bindings for codec-whatttttttttttt (Phase 10).

Thin ctypes wrapper over the C ABI. No decode or encode logic
lives here; every call forwards to the shared library built by
the C crate (`cargo build -p codec-whatttttttttttt-c`).

Example::

    from codec_whatttttttttttt import encode_flac, Decoder

    flac = encode_flac(samples, frames=4410, channels=2, sample_rate=44100)
    decoder = Decoder.from_flac_streaminfo(flac[8:42])
    frame = decoder.decode(first_frame_bytes)
"""

from __future__ import annotations

import ctypes
from dataclasses import dataclass

from . import _ffi

# Error codes (must match codec_whatttttttttttt.h).
OK = 0
ERR_IO = 1
ERR_INVALID_DATA = 2
ERR_TRUNCATED = 3
ERR_UNSUPPORTED = 4
ERR_OVERFLOW = 5
ERR_ALLOCATION_LIMIT = 6
ERR_INVALID_ARGUMENT = 7

# Codec ids (must match codec_whatttttttttttt.h).
CODEC_UNKNOWN = -1
CODEC_PCM = 0
CODEC_FLAC = 1
CODEC_OPUS = 2
CODEC_VORBIS = 3
CODEC_AV1 = 4
CODEC_VP9 = 5

# Container ids (must match codec_whatttttttttttt.h).
CONTAINER_UNKNOWN = 0
CONTAINER_WAV = 1
CONTAINER_RAW_FLAC = 2
CONTAINER_OGG = 3
CONTAINER_WEBM = 4
CONTAINER_MP4 = 5
CONTAINER_IVF = 6

CONTAINER_NAMES = {
    CONTAINER_UNKNOWN: "unknown",
    CONTAINER_WAV: "wav",
    CONTAINER_RAW_FLAC: "raw-flac",
    CONTAINER_OGG: "ogg",
    CONTAINER_WEBM: "webm",
    CONTAINER_MP4: "mp4",
    CONTAINER_IVF: "ivf",
}

CODEC_NAMES = {
    CODEC_UNKNOWN: "unknown",
    CODEC_PCM: "pcm",
    CODEC_FLAC: "flac",
    CODEC_OPUS: "opus",
    CODEC_VORBIS: "vorbis",
    CODEC_AV1: "av1",
    CODEC_VP9: "vp9",
}


class CodecError(Exception):
    """An error from the codec library, with its C ABI error code."""

    def __init__(self, code: int, detail: str = "") -> None:
        self.code = code
        message = _ffi.codec_error_string(code).decode()
        if detail:
            message += f": {detail}"
        super().__init__(message)


def _check(rc: int) -> None:
    if rc == OK:
        return
    detail = _ffi.codec_last_error()
    raise CodecError(rc, detail.decode() if detail else "")


def version() -> str:
    """Library version, e.g. "0.1.0"."""
    return _ffi.codec_version().decode()


@dataclass(frozen=True)
class ProbeResult:
    container: int
    codec: int

    @property
    def container_name(self) -> str:
        return CONTAINER_NAMES.get(self.container, "unknown")

    @property
    def codec_name(self) -> str:
        return CODEC_NAMES.get(self.codec, "unknown")


def probe(data: bytes) -> ProbeResult:
    """Identify the container and codec of the first bytes of a file.

    The codec is ``CODEC_UNKNOWN`` for Ogg, WebM, and MP4: those
    containers hold several streams and need a demuxer to name the
    codec.
    """
    result = _ffi._Probe()
    _check(_ffi.codec_probe(data, len(data), ctypes.byref(result)))
    return ProbeResult(container=result.container, codec=result.codec)


@dataclass
class AudioFrame:
    sample_rate: int
    channels: int
    frames: int
    timestamp_us: int
    samples: list[float]  # interleaved f32, frames * channels


class Decoder:
    """Audio decoder. Build from a WAV file or a FLAC STREAMINFO
    block, then decode one packet at a time."""

    def __init__(self, handle: int) -> None:
        self._handle = handle

    @classmethod
    def from_wav(cls, data: bytes) -> Decoder:
        """Build a decoder from a WAV file's bytes."""
        handle = ctypes.c_void_p()
        _check(
            _ffi.codec_decoder_from_wav_header(data, len(data), ctypes.byref(handle))
        )
        return cls(handle.value)

    @classmethod
    def from_flac_streaminfo(cls, streaminfo: bytes) -> Decoder:
        """Build a decoder from a 34-byte FLAC STREAMINFO payload."""
        if len(streaminfo) != 34:
            raise CodecError(
                ERR_INVALID_ARGUMENT,
                f"streaminfo must be 34 bytes, got {len(streaminfo)}",
            )
        handle = ctypes.c_void_p()
        _check(
            _ffi.codec_decoder_from_flac_streaminfo(
                streaminfo, len(streaminfo), ctypes.byref(handle)
            )
        )
        return cls(handle.value)

    @classmethod
    def from_vp9_frame_header(cls, frame: bytes) -> Decoder:
        """Build a decoder from a VP9 frame header. decode() on it
        raises ``CodecError`` with code ``ERR_UNSUPPORTED``: VP9 is
        parser-only in this phase."""
        handle = ctypes.c_void_p()
        _check(
            _ffi.codec_decoder_from_vp9_frame_header(
                frame, len(frame), ctypes.byref(handle)
            )
        )
        return cls(handle.value)

    def decode(self, packet: bytes, timestamp_us: int = 0) -> AudioFrame:
        """Decode one packet. A FLAC packet holds exactly one frame;
        an Opus packet may hold several frames of audio."""
        view = ctypes.POINTER(_ffi._AudioFrame)()
        _check(
            _ffi.codec_decoder_decode(
                self._handle,
                packet,
                len(packet),
                timestamp_us,
                ctypes.byref(view),
            )
        )
        frame = view.contents
        count = frame.frames * frame.channels
        samples = list(frame.samples[:count])
        return AudioFrame(
            sample_rate=frame.sample_rate,
            channels=frame.channels,
            frames=frame.frames,
            timestamp_us=frame.timestamp_us,
            samples=samples,
        )

    def close(self) -> None:
        if self._handle is not None:
            _ffi.codec_decoder_free(self._handle)
            self._handle = None

    # `Self` would need typing_extensions on Python 3.9; the
    # concrete return type is exact here.
    def __enter__(self) -> Decoder:  # noqa: PYI034
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


class Encoder:
    """FLAC encoder (streaming). One input frame becomes one FLAC
    frame, block size 1..=4096."""

    def __init__(self, handle: int, sample_rate: int, channels: int) -> None:
        self._handle = handle
        self.sample_rate = sample_rate
        self.channels = channels

    @classmethod
    def flac(cls, sample_rate: int, channels: int) -> Encoder:
        handle = ctypes.c_void_p()
        _check(_ffi.codec_encoder_new_flac(sample_rate, channels, ctypes.byref(handle)))
        return cls(handle.value, sample_rate, channels)

    def streaminfo(self) -> bytes:
        """The streaming STREAMINFO header (42 bytes: "fLaC" +
        block header + 34-byte STREAMINFO, total samples 0)."""
        view = ctypes.POINTER(_ffi._Packet)()
        _check(_ffi.codec_encoder_flac_streaminfo(self._handle, ctypes.byref(view)))
        packet = view.contents
        return bytes(packet.data[: packet.len])

    def encode(self, samples: list[float], frames: int) -> bytes:
        """Encode one frame of interleaved f32 samples. ``frames``
        is the number of frames (samples per channel), 1..=4096."""
        if len(samples) != frames * self.channels:
            raise CodecError(
                ERR_INVALID_ARGUMENT,
                f"expected {frames * self.channels} samples, got {len(samples)}",
            )
        array = (ctypes.c_float * len(samples))(*samples)
        view = ctypes.POINTER(_ffi._Packet)()
        _check(
            _ffi.codec_encoder_encode(self._handle, array, frames, ctypes.byref(view))
        )
        packet = view.contents
        return bytes(packet.data[: packet.len])

    def close(self) -> None:
        if self._handle is not None:
            _ffi.codec_encoder_free(self._handle)
            self._handle = None

    # See Decoder.__enter__ for why this is not `Self`.
    def __enter__(self) -> Encoder:  # noqa: PYI034
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


def encode_flac(
    samples: list[int], frames: int, channels: int, sample_rate: int
) -> bytes:
    """One-shot FLAC encode: interleaved i16 samples -> a complete
    FLAC file with exact STREAMINFO. ``frames`` is the number of
    frames (samples per channel)."""
    if len(samples) != frames * channels:
        raise CodecError(
            ERR_INVALID_ARGUMENT,
            f"expected {frames * channels} samples, got {len(samples)}",
        )
    array = (ctypes.c_int16 * len(samples))(*samples)
    out_data = ctypes.POINTER(ctypes.c_uint8)()
    out_len = ctypes.c_size_t(0)
    _check(
        _ffi.codec_encode_flac(
            array,
            frames,
            channels,
            sample_rate,
            ctypes.byref(out_data),
            ctypes.byref(out_len),
        )
    )
    try:
        return bytes(out_data[: out_len.value])
    finally:
        _ffi.codec_free(out_data)


@dataclass(frozen=True)
class Vp9FrameHeader:
    profile: int
    frame_type: int  # 0 = key, 1 = inter
    show_frame: bool
    show_existing_frame: bool
    width: int
    height: int
    render_width: int
    render_height: int
    base_q_idx: int
    loop_filter_level: int
    tile_cols_log2: int
    tile_rows_log2: int
    header_size_in_bytes: int


def parse_vp9_frame_header(frame: bytes) -> Vp9FrameHeader:
    """Parse a VP9 frame header (parser-only; no reconstruction).
    A key frame parses; an inter frame needs a prior key frame and
    raises ``CodecError`` here."""
    result = _ffi._Vp9FrameHeader()
    _check(_ffi.codec_vp9_parse_frame_header(frame, len(frame), ctypes.byref(result)))
    return Vp9FrameHeader(
        profile=result.profile,
        frame_type=result.frame_type,
        show_frame=bool(result.show_frame),
        show_existing_frame=bool(result.show_existing_frame),
        width=result.width,
        height=result.height,
        render_width=result.render_width,
        render_height=result.render_height,
        base_q_idx=result.base_q_idx,
        loop_filter_level=result.loop_filter_level,
        tile_cols_log2=result.tile_cols_log2,
        tile_rows_log2=result.tile_rows_log2,
        header_size_in_bytes=result.header_size_in_bytes,
    )


@dataclass(frozen=True)
class Av1SequenceHeader:
    seq_profile: int
    seq_level_idx_0: int
    seq_tier_0: int
    max_frame_width_minus_1: int
    max_frame_height_minus_1: int
    frame_width_bits_minus_1: int
    frame_height_bits_minus_1: int
    reduced_still_picture_header: bool


def parse_av1_sequence_header(obu: bytes) -> Av1SequenceHeader:
    """Parse one AV1 sequence-header OBU (header + payload)."""
    result = _ffi._Av1SequenceHeader()
    _check(_ffi.codec_av1_parse_sequence_header(obu, len(obu), ctypes.byref(result)))
    return Av1SequenceHeader(
        seq_profile=result.seq_profile,
        seq_level_idx_0=result.seq_level_idx_0,
        seq_tier_0=result.seq_tier_0,
        max_frame_width_minus_1=result.max_frame_width_minus_1,
        max_frame_height_minus_1=result.max_frame_height_minus_1,
        frame_width_bits_minus_1=result.frame_width_bits_minus_1,
        frame_height_bits_minus_1=result.frame_height_bits_minus_1,
        reduced_still_picture_header=bool(result.reduced_still_picture_header),
    )


__all__ = [
    "CODEC_AV1",
    "CODEC_FLAC",
    "CODEC_OPUS",
    "CODEC_PCM",
    "CODEC_UNKNOWN",
    "CODEC_VORBIS",
    "CODEC_VP9",
    "CONTAINER_IVF",
    "CONTAINER_MP4",
    "CONTAINER_OGG",
    "CONTAINER_RAW_FLAC",
    "CONTAINER_UNKNOWN",
    "CONTAINER_WAV",
    "CONTAINER_WEBM",
    "ERR_ALLOCATION_LIMIT",
    "ERR_INVALID_ARGUMENT",
    "ERR_INVALID_DATA",
    "ERR_IO",
    "ERR_OVERFLOW",
    "ERR_TRUNCATED",
    "ERR_UNSUPPORTED",
    "OK",
    "AudioFrame",
    "Av1SequenceHeader",
    "CodecError",
    "Decoder",
    "Encoder",
    "ProbeResult",
    "Vp9FrameHeader",
    "encode_flac",
    "parse_av1_sequence_header",
    "parse_vp9_frame_header",
    "probe",
    "version",
]
