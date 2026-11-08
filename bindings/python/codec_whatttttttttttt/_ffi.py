"""ctypes bindings for the codec-whatttttttttttt C ABI (Phase 10).

Loads the shared library built by the C crate
(`cargo build -p codec-whatttttttttttt-c`) and declares the
function signatures. The search order is:
  1. the `CODEC_WHATTTTTTTTTTTT_LIB` environment variable,
  2. `target/debug/` and `target/release/` next to this package's
     workspace root,
  3. the system library path (`ctypes.util.find_library`).
"""

from __future__ import annotations

import ctypes
import os
import pathlib

_LIB_NAME = "libcodec_whatttttttttttt.so"


def _find_library() -> str:
    env = os.environ.get("CODEC_WHATTTTTTTTTTTT_LIB")
    if env:
        if not pathlib.Path(env).is_file():
            raise OSError(f"CODEC_WHATTTTTTTTTTTT_LIB points to a missing file: {env}")
        return env
    here = pathlib.Path(__file__).resolve()
    # bindings/python/codec_whatttttttttttt/_ffi.py -> workspace root
    # is three parents up.
    workspace = here.parents[3]
    for profile in ("debug", "release"):
        candidate = workspace / "target" / profile / _LIB_NAME
        if candidate.is_file():
            return str(candidate)
    import ctypes.util

    found = ctypes.util.find_library("codec_whatttttttttttt")
    if found:
        return found
    raise OSError(
        "shared library not found: build the C crate first "
        "(`cargo build -p codec-whatttttttttttt-c`) or set "
        "CODEC_WHATTTTTTTTTTTT_LIB to the .so path"
    )


_lib = ctypes.CDLL(_find_library())


class _AudioFrame(ctypes.Structure):
    _fields_ = [
        ("sample_rate", ctypes.c_uint32),
        ("channels", ctypes.c_uint16),
        ("frames", ctypes.c_uint64),
        ("timestamp_us", ctypes.c_uint64),
        ("samples", ctypes.POINTER(ctypes.c_float)),
    ]


class _Packet(ctypes.Structure):
    _fields_ = [
        ("data", ctypes.POINTER(ctypes.c_uint8)),
        ("len", ctypes.c_size_t),
        ("timestamp_us", ctypes.c_uint64),
    ]


class _Probe(ctypes.Structure):
    _fields_ = [
        ("container", ctypes.c_int32),
        ("codec", ctypes.c_int32),
    ]


class _Vp9FrameHeader(ctypes.Structure):
    _fields_ = [
        ("profile", ctypes.c_uint8),
        ("frame_type", ctypes.c_uint8),
        ("show_frame", ctypes.c_uint8),
        ("show_existing_frame", ctypes.c_uint8),
        ("width", ctypes.c_uint16),
        ("height", ctypes.c_uint16),
        ("render_width", ctypes.c_uint16),
        ("render_height", ctypes.c_uint16),
        ("base_q_idx", ctypes.c_uint8),
        ("loop_filter_level", ctypes.c_uint8),
        ("tile_cols_log2", ctypes.c_uint8),
        ("tile_rows_log2", ctypes.c_uint8),
        ("header_size_in_bytes", ctypes.c_uint32),
    ]


class _Av1SequenceHeader(ctypes.Structure):
    _fields_ = [
        ("seq_profile", ctypes.c_uint8),
        ("seq_level_idx_0", ctypes.c_uint8),
        ("seq_tier_0", ctypes.c_uint8),
        ("max_frame_width_minus_1", ctypes.c_uint16),
        ("max_frame_height_minus_1", ctypes.c_uint16),
        ("frame_width_bits_minus_1", ctypes.c_uint8),
        ("frame_height_bits_minus_1", ctypes.c_uint8),
        ("reduced_still_picture_header", ctypes.c_uint8),
    ]


def _bind(name, argtypes, restype):
    fn = getattr(_lib, name)
    fn.argtypes = argtypes
    fn.restype = restype
    return fn


codec_version = _bind("codec_version", [], ctypes.c_char_p)
codec_error_string = _bind("codec_error_string", [ctypes.c_int32], ctypes.c_char_p)
codec_last_error = _bind("codec_last_error", [], ctypes.c_char_p)
codec_probe = _bind(
    "codec_probe",
    [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(_Probe)],
    ctypes.c_int32,
)
codec_decoder_from_wav_header = _bind(
    "codec_decoder_from_wav_header",
    [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_void_p)],
    ctypes.c_int32,
)
codec_decoder_from_flac_streaminfo = _bind(
    "codec_decoder_from_flac_streaminfo",
    [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_void_p)],
    ctypes.c_int32,
)
codec_decoder_from_vp9_frame_header = _bind(
    "codec_decoder_from_vp9_frame_header",
    [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_void_p)],
    ctypes.c_int32,
)
codec_decoder_decode = _bind(
    "codec_decoder_decode",
    [
        ctypes.c_void_p,
        ctypes.c_char_p,
        ctypes.c_size_t,
        ctypes.c_uint64,
        ctypes.POINTER(ctypes.POINTER(_AudioFrame)),
    ],
    ctypes.c_int32,
)
codec_decoder_free = _bind("codec_decoder_free", [ctypes.c_void_p], None)
codec_encoder_new_flac = _bind(
    "codec_encoder_new_flac",
    [ctypes.c_uint32, ctypes.c_uint16, ctypes.POINTER(ctypes.c_void_p)],
    ctypes.c_int32,
)
codec_encoder_flac_streaminfo = _bind(
    "codec_encoder_flac_streaminfo",
    [ctypes.c_void_p, ctypes.POINTER(ctypes.POINTER(_Packet))],
    ctypes.c_int32,
)
codec_encoder_encode = _bind(
    "codec_encoder_encode",
    [
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_float),
        ctypes.c_uint64,
        ctypes.POINTER(ctypes.POINTER(_Packet)),
    ],
    ctypes.c_int32,
)
codec_encoder_free = _bind("codec_encoder_free", [ctypes.c_void_p], None)
codec_encode_flac = _bind(
    "codec_encode_flac",
    [
        ctypes.POINTER(ctypes.c_int16),
        ctypes.c_uint64,
        ctypes.c_uint16,
        ctypes.c_uint32,
        ctypes.POINTER(ctypes.POINTER(ctypes.c_uint8)),
        ctypes.POINTER(ctypes.c_size_t),
    ],
    ctypes.c_int32,
)
codec_free = _bind("codec_free", [ctypes.c_void_p], None)
codec_vp9_parse_frame_header = _bind(
    "codec_vp9_parse_frame_header",
    [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(_Vp9FrameHeader)],
    ctypes.c_int32,
)
codec_av1_parse_sequence_header = _bind(
    "codec_av1_parse_sequence_header",
    [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(_Av1SequenceHeader)],
    ctypes.c_int32,
)
