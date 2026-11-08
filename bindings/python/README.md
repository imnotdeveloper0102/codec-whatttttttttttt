# codec-whatttttttttttt — Python bindings (Phase 10)

Thin ctypes wrapper over the C ABI. No decode or encode logic
lives here; every call forwards to the shared library built by
the C crate.

## Build the shared library first

```text
cargo build -p codec-whatttttttttttt-c
```

The binding finds `target/debug/libcodec_whatttttttttttt.so`
(then `target/release/`, then the system library path). Override
with `CODEC_WHATTTTTTTTTTTT_LIB` to point at a specific `.so`.

## Use

```python
from codec_whatttttttttttt import Decoder, Encoder, encode_flac

flac = encode_flac(samples, frames=4410, channels=2, sample_rate=44100)

with Decoder.from_flac_streaminfo(flac[8:42]) as decoder:
    frame = decoder.decode(first_frame_bytes)
    print(frame.sample_rate, frame.channels, frame.frames)
```

## Tests

```text
cd bindings/python
python3 -m unittest discover -s tests -v
```

## Example

```text
cd bindings/python
python3 examples/encode_decode.py
```

## Scope

Decode (WAV, FLAC), FLAC encode (one-shot and streaming), probe,
and the VP9/AV1 header parsers. The video decoders are
parser-only, so the bindings expose the parsers, not a fake decode.
See `docs/LIMITS.md` for what the core actually implements.
