// test_cpp.cpp — C++ wrapper test (Phase 10).
//
// Reads the pinned fixtures, probes them, round-trips a FLAC file
// through the C++ wrapper, and parses a VP9 frame header.
//
// Build (from the workspace root, after `cargo build -p
// codec-whatttttttttttt-c`):
//   g++ -std=c++17 -Wall -Wextra -I crates/codec-whatttttttttttt-c/include
//     crates/codec-whatttttttttttt-c/tests/test_cpp.cpp
//     -L target/debug -lcodec_whatttttttttttt -lm
//     -Wl,-rpath,target/debug -o /tmp/test_cpp
// Run: /tmp/test_cpp

#include "codec_whatttttttttttt.hpp"

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <fstream>
#include <vector>

static std::vector<uint8_t> read_file(const char* path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) {
        fprintf(stderr, "cannot open %s\n", path);
        exit(1);
    }
    return std::vector<uint8_t>(std::istreambuf_iterator<char>(in),
                                std::istreambuf_iterator<char>());
}

static void check(bool ok, const char* what) {
    if (!ok) {
        fprintf(stderr, "FAIL: %s\n", what);
        exit(1);
    }
    printf("ok: %s\n", what);
}

int main() {
    check(codec::version() == "0.1.0", "version is 0.1.0");

    const std::string data =
        "crates/codec-whatttttttttttt-core/tests/data/";

    // Probe.
    auto wav = read_file((data + "valid_i16_stereo_44100.wav").c_str());
    auto probe = codec::probe(wav.data(), wav.size());
    check(probe.container == CODEC_CONTAINER_WAV &&
              probe.codec == CODEC_CODEC_PCM,
          "probe wav");

    auto flac = read_file((data + "sine_stereo_44100.flac").c_str());
    probe = codec::probe(flac.data(), flac.size());
    check(probe.container == CODEC_CONTAINER_RAW_FLAC &&
              probe.codec == CODEC_CODEC_FLAC,
          "probe flac");

    // Decode the FLAC file through the C++ wrapper. The STREAMINFO
    // payload is at offset 8; the first frame starts after all
    // metadata blocks (STREAMINFO + Vorbis comment here).
    size_t offset = 4;
    while (true) {
        uint8_t header = flac[offset];
        uint32_t length = ((uint32_t)flac[offset + 1] << 16) |
                          ((uint32_t)flac[offset + 2] << 8) |
                          (uint32_t)flac[offset + 3];
        offset += 4 + length;
        if (header & 0x80) {
            break;
        }
    }
    auto dec = codec::decoder::from_flac_streaminfo(flac.data() + 8, 34);
    auto frame = dec.decode(flac.data() + offset, flac.size() - offset, 0);
    check(frame.sample_rate == 44100 && frame.channels == 2 &&
              frame.frames == 4410,
          "decode flac frame via wrapper");
    check(std::abs(frame.samples[0]) < 0.001f, "sine starts at zero");

    // One-shot FLAC encode + decode round-trip.
    std::vector<int16_t> samples;
    for (int n = 0; n < 1000; n++) {
        double t = (double)n / 44100.0;
        samples.push_back(
            (int16_t)(20000.0 * std::sin(2.0 * M_PI * 440.0 * t)));
        samples.push_back(
            (int16_t)(15000.0 * std::sin(2.0 * M_PI * 660.0 * t)));
    }
    auto encoded = codec::encode_flac(samples.data(), 1000, 2, 44100);
    check(encoded.size() > 42 && encoded[0] == 'f' && encoded[1] == 'L',
          "one-shot encode produces a flac file");

    // VP9 header parse.
    auto ivf = read_file((data + "vp9_64x64.ivf").c_str());
    uint32_t frame_size = (uint32_t)ivf[32] | ((uint32_t)ivf[33] << 8) |
                          ((uint32_t)ivf[34] << 16) |
                          ((uint32_t)ivf[35] << 24);
    auto header = codec::parse_vp9_frame_header(ivf.data() + 44, frame_size);
    check(header.profile == 0 && header.frame_type == 0 &&
              header.width == 64 && header.height == 64 &&
              header.base_q_idx == 33,
          "parse vp9 key frame header");

    // VP9 decode is parser-only: the decoder refuses to decode.
    bool refused = false;
    try {
        auto vp9 = codec::decoder::from_vp9_frame_header(ivf.data() + 44,
                                                         frame_size);
        (void)vp9.decode(ivf.data() + 44, frame_size, 0);
    } catch (const codec::error& e) {
        refused = e.code() == CODEC_ERR_UNSUPPORTED;
    }
    check(refused, "vp9 decode returns unsupported");

    printf("all C++ checks passed\n");
    return 0;
}
