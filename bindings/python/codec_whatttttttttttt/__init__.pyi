"""Type stubs for the codec-whatttttttttttt Python binding (Phase 10).

Mirrors `__init__.py`, which is a thin ctypes wrapper over the C
ABI. The stubs exist so type checkers (mypy) can verify callers
without importing the shared library.
"""

from dataclasses import dataclass

# Error codes (must match codec_whatttttttttttt.h).
OK: int
ERR_IO: int
ERR_INVALID_DATA: int
ERR_TRUNCATED: int
ERR_UNSUPPORTED: int
ERR_OVERFLOW: int
ERR_ALLOCATION_LIMIT: int
ERR_INVALID_ARGUMENT: int

# Codec ids (must match codec_whatttttttttttt.h).
CODEC_UNKNOWN: int
CODEC_PCM: int
CODEC_FLAC: int
CODEC_OPUS: int
CODEC_VORBIS: int
CODEC_AV1: int
CODEC_VP9: int

# Container ids (must match codec_whatttttttttttt.h).
CONTAINER_UNKNOWN: int
CONTAINER_WAV: int
CONTAINER_RAW_FLAC: int
CONTAINER_OGG: int
CONTAINER_WEBM: int
CONTAINER_MP4: int
CONTAINER_IVF: int

CONTAINER_NAMES: dict[int, str]
CODEC_NAMES: dict[int, str]

class CodecError(Exception):
    code: int
    def __init__(self, code: int, detail: str = ...) -> None: ...

@dataclass(frozen=True)
class ProbeResult:
    container: int
    codec: int
    @property
    def container_name(self) -> str: ...
    @property
    def codec_name(self) -> str: ...

def version() -> str: ...
def probe(data: bytes) -> ProbeResult: ...

@dataclass
class AudioFrame:
    sample_rate: int
    channels: int
    frames: int
    timestamp_us: int
    samples: list[float]

class Decoder:
    def __init__(self, handle: int) -> None: ...
    @classmethod
    def from_wav(cls, data: bytes) -> Decoder: ...
    @classmethod
    def from_flac_streaminfo(cls, streaminfo: bytes) -> Decoder: ...
    @classmethod
    def from_vp9_frame_header(cls, frame: bytes) -> Decoder: ...
    def decode(self, packet: bytes, timestamp_us: int = ...) -> AudioFrame: ...
    def close(self) -> None: ...
    def __enter__(self) -> Decoder: ...  # noqa: PYI034
    def __exit__(self, *exc: object) -> None: ...

class Encoder:
    sample_rate: int
    channels: int
    def __init__(self, handle: int, sample_rate: int, channels: int) -> None: ...
    @classmethod
    def flac(cls, sample_rate: int, channels: int) -> Encoder: ...
    def streaminfo(self) -> bytes: ...
    def encode(self, samples: list[float], frames: int) -> bytes: ...
    def close(self) -> None: ...
    def __enter__(self) -> Encoder: ...  # noqa: PYI034
    def __exit__(self, *exc: object) -> None: ...

def encode_flac(
    samples: list[int], frames: int, channels: int, sample_rate: int
) -> bytes: ...

@dataclass(frozen=True)
class Vp9FrameHeader:
    profile: int
    frame_type: int
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

def parse_vp9_frame_header(frame: bytes) -> Vp9FrameHeader: ...

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

def parse_av1_sequence_header(obu: bytes) -> Av1SequenceHeader: ...
