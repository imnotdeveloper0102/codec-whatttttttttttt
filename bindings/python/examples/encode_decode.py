"""Encode a sine to FLAC and decode it back (Phase 10 example).

Run from the bindings/python directory:
    python3 examples/encode_decode.py
The shared library must be built first
(`cargo build -p codec-whatttttttttttt-c`).
"""

import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codec_whatttttttttttt import Decoder, Encoder, encode_flac, version

SAMPLE_RATE = 44100
CHANNELS = 2
FRAMES = 4410


def main():
    print(f"codec-whatttttttttttt {version()}")

    samples = []
    for n in range(FRAMES):
        t = n / SAMPLE_RATE
        samples.append(int(20000 * math.sin(2 * math.pi * 440 * t)))
        samples.append(int(15000 * math.sin(2 * math.pi * 660 * t)))

    flac = encode_flac(
        samples, frames=FRAMES, channels=CHANNELS, sample_rate=SAMPLE_RATE
    )
    print(f"one-shot encoded {len(flac)} bytes of FLAC")

    with (
        Encoder.flac(SAMPLE_RATE, CHANNELS) as encoder,
        Decoder.from_flac_streaminfo(encoder.streaminfo()[8:42]) as decoder,
    ):
        f32 = [s / 32768.0 for s in samples]
        decoded = []
        done = 0
        while done < FRAMES:
            chunk = min(4096, FRAMES - done)
            packet = encoder.encode(
                f32[done * CHANNELS : (done + chunk) * CHANNELS],
                frames=chunk,
            )
            frame = decoder.decode(packet)
            decoded.extend(frame.samples)
            done += chunk
        print(f"decoded {len(decoded) // CHANNELS} frames")
        max_diff = max(
            abs(round(got * 32768) - want) for got, want in zip(decoded, samples)
        )
        print(f"max abs diff {max_diff}")
        assert max_diff == 0
    print("round-trip exact")


if __name__ == "__main__":
    main()
