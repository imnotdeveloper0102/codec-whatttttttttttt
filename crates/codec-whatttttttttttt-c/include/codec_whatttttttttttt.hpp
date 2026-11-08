// codec_whatttttttttttt.hpp — C++ RAII wrapper over the C ABI (Phase 10).
//
// Header-only. Throws codec::error on failure; handles are move-only
// and free themselves. Decoded frames and encoded packets are copied
// into std::vector on return, so the caller never holds a borrowed
// view past the next call.

#ifndef CODEC_WHATTTTTTTTTTTT_HPP
#define CODEC_WHATTTTTTTTTTTT_HPP

#include "codec_whatttttttttttt.h"

#include <cstdint>
#include <stdexcept>
#include <string>
#include <vector>

namespace codec {

class error : public std::runtime_error {
public:
    explicit error(codec_error_t code, const std::string& detail)
        : std::runtime_error(detail), code_(code) {}
    [[nodiscard]] codec_error_t code() const noexcept { return code_; }

private:
    codec_error_t code_;
};

inline void throw_if_failed(codec_error_t rc) {
    if (rc == CODEC_OK) {
        return;
    }
    const char* detail = codec_last_error();
    std::string message = codec_error_string(rc);
    if (detail != nullptr && detail[0] != '\0') {
        message += ": ";
        message += detail;
    }
    throw error(rc, message);
}

inline std::string version() { return codec_version(); }

struct probe_result {
    codec_container_t container;
    codec_codec_t codec;
};

inline probe_result probe(const uint8_t* data, std::size_t len) {
    codec_probe_t out{};
    throw_if_failed(codec_probe(data, len, &out));
    return {out.container, out.codec};
}

struct audio_frame {
    uint32_t sample_rate = 0;
    uint16_t channels = 0;
    uint64_t frames = 0;
    uint64_t timestamp_us = 0;
    std::vector<float> samples; // interleaved, frames * channels
};

struct packet {
    std::vector<uint8_t> data;
    uint64_t timestamp_us = 0;
};

class decoder {
public:
    static decoder from_wav(const uint8_t* data, std::size_t len) {
        codec_decoder_t* handle = nullptr;
        throw_if_failed(codec_decoder_from_wav_header(data, len, &handle));
        return decoder(handle);
    }
    static decoder from_flac_streaminfo(const uint8_t* data, std::size_t len) {
        codec_decoder_t* handle = nullptr;
        throw_if_failed(
            codec_decoder_from_flac_streaminfo(data, len, &handle));
        return decoder(handle);
    }
    static decoder from_vp9_frame_header(const uint8_t* data,
                                         std::size_t len) {
        codec_decoder_t* handle = nullptr;
        throw_if_failed(
            codec_decoder_from_vp9_frame_header(data, len, &handle));
        return decoder(handle);
    }

    decoder(const decoder&) = delete;
    decoder& operator=(const decoder&) = delete;
    decoder(decoder&& other) noexcept : handle_(other.handle_) {
        other.handle_ = nullptr;
    }
    decoder& operator=(decoder&& other) noexcept {
        if (this != &other) {
            reset();
            handle_ = other.handle_;
            other.handle_ = nullptr;
        }
        return *this;
    }
    ~decoder() { reset(); }

    // Decodes one packet; the frame is copied, so it outlives the
    // next call.
    audio_frame decode(const uint8_t* data, std::size_t len,
                       uint64_t timestamp_us = 0) {
        const codec_audio_frame_t* view = nullptr;
        throw_if_failed(
            codec_decoder_decode(handle_, data, len, timestamp_us, &view));
        audio_frame out;
        out.sample_rate = view->sample_rate;
        out.channels = view->channels;
        out.frames = view->frames;
        out.timestamp_us = view->timestamp_us;
        out.samples.assign(view->samples,
                           view->samples + view->frames * view->channels);
        return out;
    }

private:
    explicit decoder(codec_decoder_t* handle) : handle_(handle) {}
    void reset() {
        if (handle_ != nullptr) {
            codec_decoder_free(handle_);
            handle_ = nullptr;
        }
    }
    codec_decoder_t* handle_ = nullptr;
};

class encoder {
public:
    static encoder flac(uint32_t sample_rate, uint16_t channels) {
        codec_encoder_t* handle = nullptr;
        throw_if_failed(
            codec_encoder_new_flac(sample_rate, channels, &handle));
        return encoder(handle);
    }

    encoder(const encoder&) = delete;
    encoder& operator=(const encoder&) = delete;
    encoder(encoder&& other) noexcept : handle_(other.handle_) {
        other.handle_ = nullptr;
    }
    encoder& operator=(encoder&& other) noexcept {
        if (this != &other) {
            reset();
            handle_ = other.handle_;
            other.handle_ = nullptr;
        }
        return *this;
    }
    ~encoder() { reset(); }

    packet streaminfo() {
        const codec_packet_t* view = nullptr;
        throw_if_failed(codec_encoder_flac_streaminfo(handle_, &view));
        return {std::vector<uint8_t>(view->data, view->data + view->len),
                view->timestamp_us};
    }

    // Encodes one frame of interleaved f32 samples; `frames` is the
    // number of frames (samples per channel), 1..=4096.
    packet encode(const float* samples, uint64_t frames) {
        const codec_packet_t* view = nullptr;
        throw_if_failed(
            codec_encoder_encode(handle_, samples, frames, &view));
        return {std::vector<uint8_t>(view->data, view->data + view->len),
                view->timestamp_us};
    }

private:
    explicit encoder(codec_encoder_t* handle) : handle_(handle) {}
    void reset() {
        if (handle_ != nullptr) {
            codec_encoder_free(handle_);
            handle_ = nullptr;
        }
    }
    codec_encoder_t* handle_ = nullptr;
};

// One-shot FLAC encode: interleaved i16 samples -> a complete FLAC
// file with exact STREAMINFO.
inline std::vector<uint8_t> encode_flac(const int16_t* samples,
                                        uint64_t frames, uint16_t channels,
                                        uint32_t sample_rate) {
    uint8_t* data = nullptr;
    std::size_t len = 0;
    throw_if_failed(codec_encode_flac(samples, frames, channels,
                                      sample_rate, &data, &len));
    std::vector<uint8_t> out(data, data + len);
    codec_free(data);
    return out;
}

inline codec_vp9_frame_header_t parse_vp9_frame_header(
    const uint8_t* frame, std::size_t len) {
    codec_vp9_frame_header_t out{};
    throw_if_failed(codec_vp9_parse_frame_header(frame, len, &out));
    return out;
}

inline codec_av1_sequence_header_t parse_av1_sequence_header(
    const uint8_t* obu, std::size_t len) {
    codec_av1_sequence_header_t out{};
    throw_if_failed(codec_av1_parse_sequence_header(obu, len, &out));
    return out;
}

} // namespace codec

#endif // CODEC_WHATTTTTTTTTTTT_HPP
