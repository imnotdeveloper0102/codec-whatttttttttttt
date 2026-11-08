/* codec_whatttttttttttt.h — C ABI for codec-whatttttttttttt (Phase 10).
 *
 * Thin wrapper over the Rust core. No decode or encode logic lives
 * here; every function forwards to the core and maps its error enum
 * to the codes below. See docs/LIMITS.md for what the core actually
 * implements (Opus is partial, AV1 and VP9 are parser-only, and
 * only FLAC encode exists).
 *
 * Ownership: opaque handles are freed with the matching
 * `codec_*_free`. Buffers the library allocates are freed with
 * `codec_free`. A decoded frame (and an encoded packet) is borrowed
 * from its decoder/encoder and stays valid only until the next call
 * on the same handle or its free; copy it before the next call.
 *
 * Thread-safety: a handle is used from one thread at a time.
 * `codec_version`, `codec_error_string`, and `codec_probe` are
 * thread-safe. `codec_last_error` is per-thread.
 */

#ifndef CODEC_WHATTTTTTTTTTTT_H
#define CODEC_WHATTTTTTTTTTTT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define CODEC_VERSION "0.1.0"

/* Error codes, mirroring the core `Error` enum. */
typedef int32_t codec_error_t;
#define CODEC_OK 0
#define CODEC_ERR_IO 1
#define CODEC_ERR_INVALID_DATA 2
#define CODEC_ERR_TRUNCATED 3
#define CODEC_ERR_UNSUPPORTED 4
#define CODEC_ERR_OVERFLOW 5
#define CODEC_ERR_ALLOCATION_LIMIT 6
#define CODEC_ERR_INVALID_ARGUMENT 7

/* Codec identifiers, mirroring the core `Codec` enum. */
typedef int32_t codec_codec_t;
#define CODEC_CODEC_UNKNOWN (-1)
#define CODEC_CODEC_PCM 0
#define CODEC_CODEC_FLAC 1
#define CODEC_CODEC_OPUS 2
#define CODEC_CODEC_VORBIS 3
#define CODEC_CODEC_AV1 4
#define CODEC_CODEC_VP9 5

/* Container kinds, from the first bytes of a file. */
typedef int32_t codec_container_t;
#define CODEC_CONTAINER_UNKNOWN 0
#define CODEC_CONTAINER_WAV 1
#define CODEC_CONTAINER_RAW_FLAC 2
#define CODEC_CONTAINER_OGG 3
#define CODEC_CONTAINER_WEBM 4
#define CODEC_CONTAINER_MP4 5
#define CODEC_CONTAINER_IVF 6

/* Library version, e.g. "0.1.0". Never NULL. */
const char *codec_version(void);

/* Human-readable message for an error code. Never NULL. */
const char *codec_error_string(codec_error_t code);

/* Detail of the most recent failure on this thread, or NULL when
 * the last call on this thread succeeded. The string is owned by
 * the library and valid until the next FFI call on this thread. */
const char *codec_last_error(void);

/* Identify the container and codec of the first bytes of a file.
 * The codec is UNKNOWN for Ogg, WebM, and MP4: those containers
 * hold several streams and need a demuxer to name the codec.
 * `out` must not be NULL; at least 4 bytes are needed. */
typedef struct {
    codec_container_t container;
    codec_codec_t codec;
} codec_probe_t;
codec_error_t codec_probe(const uint8_t *data, size_t len,
                          codec_probe_t *out);

/* Opaque handles. */
typedef struct codec_decoder codec_decoder_t;
typedef struct codec_encoder codec_encoder_t;

/* An interleaved f32 audio frame, borrowed from its decoder. */
typedef struct {
    uint32_t sample_rate;
    uint16_t channels;
    uint64_t frames; /* frames = samples per channel */
    uint64_t timestamp_us;
    const float *samples; /* length frames * channels */
} codec_audio_frame_t;

/* A packet of encoded bytes, borrowed from its encoder. */
typedef struct {
    const uint8_t *data;
    size_t len;
    uint64_t timestamp_us;
} codec_packet_t;

/* Build a decoder from a WAV file: pass the file bytes (the
 * RIFF-declared length is checked, so a truncated file returns
 * Truncated; a streamed file with RIFF size 0 or 0xFFFFFFFF needs
 * only the header). The header bytes are copied; the caller keeps
 * them. */
codec_error_t codec_decoder_from_wav_header(const uint8_t *header,
                                            size_t len,
                                            codec_decoder_t **out);

/* Build a decoder from a FLAC STREAMINFO block: exactly the 34
 * payload bytes, without the 4-byte metadata-block header. */
codec_error_t codec_decoder_from_flac_streaminfo(const uint8_t *streaminfo,
                                                 size_t len,
                                                 codec_decoder_t **out);

/* Build a decoder from a VP9 frame header. decode() on it returns
 * CODEC_ERR_UNSUPPORTED: VP9 is parser-only in this phase. The
 * header is validated against the VP9 subset in docs/LIMITS.md. */
codec_error_t codec_decoder_from_vp9_frame_header(const uint8_t *frame,
                                                  size_t len,
                                                  codec_decoder_t **out);

/* Decode one packet. The returned frame is borrowed from the
 * decoder and valid until the next decode call or
 * codec_decoder_free. A FLAC packet holds exactly one frame; an
 * Opus packet may hold several frames of audio. */
codec_error_t codec_decoder_decode(codec_decoder_t *decoder,
                                   const uint8_t *packet, size_t len,
                                   uint64_t timestamp_us,
                                   const codec_audio_frame_t **out_frame);
void codec_decoder_free(codec_decoder_t *decoder);

/* Encode, FLAC only. `sample_rate` is 1..=192000, `channels`
 * 1..=8. */
codec_error_t codec_encoder_new_flac(uint32_t sample_rate,
                                     uint16_t channels,
                                     codec_encoder_t **out);

/* The streaming STREAMINFO header (42 bytes: "fLaC" + block header
 * + 34-byte STREAMINFO, total samples 0). The returned packet is
 * borrowed from the encoder. */
codec_error_t codec_encoder_flac_streaminfo(codec_encoder_t *encoder,
                                            const codec_packet_t **out_packet);

/* Encode one frame of interleaved f32 samples. `frames` is the
 * number of frames (samples per channel), 1..=4096. The returned
 * packet is borrowed from the encoder. */
codec_error_t codec_encoder_encode(codec_encoder_t *encoder,
                                   const float *samples, uint64_t frames,
                                   const codec_packet_t **out_packet);
void codec_encoder_free(codec_encoder_t *encoder);

/* One-shot FLAC encode: interleaved i16 samples -> a complete FLAC
 * file with exact STREAMINFO. The output buffer is allocated by
 * the library; free it with codec_free. `frames` is the number of
 * frames (samples per channel), at least 1. */
codec_error_t codec_encode_flac(const int16_t *samples, uint64_t frames,
                                uint16_t channels, uint32_t sample_rate,
                                uint8_t **out_data, size_t *out_len);

/* Free a buffer allocated by the library (codec_encode_flac
 * output). Passing NULL is a no-op. */
void codec_free(void *ptr);

/* VP9 frame-header parser (parser-only; no reconstruction). The
 * header is parsed with a fresh reference state, so a key frame
 * parses; an inter frame needs a prior key frame and returns
 * CODEC_ERR_INVALID_DATA here. Use codec_decoder_from_vp9_frame_header
 * with a decoder that has already seen the key frame for inter
 * frames. */
typedef struct {
    uint8_t profile;
    uint8_t frame_type; /* 0 = key, 1 = inter */
    uint8_t show_frame;
    uint8_t show_existing_frame;
    uint16_t width;
    uint16_t height;
    uint16_t render_width;
    uint16_t render_height;
    uint8_t base_q_idx;
    uint8_t loop_filter_level;
    uint8_t tile_cols_log2;
    uint8_t tile_rows_log2;
    uint32_t header_size_in_bytes;
} codec_vp9_frame_header_t;
codec_error_t codec_vp9_parse_frame_header(const uint8_t *frame,
                                           size_t len,
                                           codec_vp9_frame_header_t *out);

/* AV1 sequence-header parser. `obu` is one sequence-header OBU
 * (header + payload); the first operating point (index 0) is
 * reported. */
typedef struct {
    uint8_t seq_profile;
    uint8_t seq_level_idx_0;
    uint8_t seq_tier_0;
    uint16_t max_frame_width_minus_1;
    uint16_t max_frame_height_minus_1;
    uint8_t frame_width_bits_minus_1;
    uint8_t frame_height_bits_minus_1;
    uint8_t reduced_still_picture_header;
} codec_av1_sequence_header_t;
codec_error_t codec_av1_parse_sequence_header(const uint8_t *obu,
                                              size_t len,
                                              codec_av1_sequence_header_t *out);

#ifdef __cplusplus
}
#endif

#endif /* CODEC_WHATTTTTTTTTTTT_H */
