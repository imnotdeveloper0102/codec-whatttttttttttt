/* encode_decode.c — C example for the C ABI (Phase 10).
 *
 * Generates a 0.1 s stereo sine in memory, encodes it to FLAC with
 * the one-shot encoder (checking the STREAMINFO it writes), then
 * round-trips it through the streaming encoder + decoder, one
 * frame at a time, and checks the PCM is exact.
 *
 * Build (from the workspace root, after `cargo build -p
 * codec-whatttttttttttt-c`):
 *   cc -I crates/codec-whatttttttttttt-c/include
 *      crates/codec-whatttttttttttt-c/examples/encode_decode.c
 *      -L target/debug -lcodec_whatttttttttttt -lm
 *      -Wl,-rpath,target/debug -o /tmp/encode_decode
 * Run: /tmp/encode_decode
 */

#include "codec_whatttttttttttt.h"

#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void die(codec_error_t rc, const char* what) {
    const char* detail = codec_last_error();
    fprintf(stderr, "%s failed: %s%s%s\n", what, codec_error_string(rc),
            detail ? ": " : "", detail ? detail : "");
    exit(1);
}

int main(void) {
    printf("codec-whatttttttttttt %s\n", codec_version());

    /* 0.1 s stereo sine, 44100 Hz, i16. */
    const uint32_t sample_rate = 44100;
    const uint16_t channels = 2;
    const uint64_t frames = 4410;
    int16_t* samples = malloc(frames * channels * sizeof(int16_t));
    if (!samples) {
        fprintf(stderr, "out of memory\n");
        return 1;
    }
    for (uint64_t n = 0; n < frames; n++) {
        double t = (double)n / (double)sample_rate;
        samples[n * 2] = (int16_t)(20000.0 * sin(2.0 * M_PI * 440.0 * t));
        samples[n * 2 + 1] =
            (int16_t)(15000.0 * sin(2.0 * M_PI * 660.0 * t));
    }

    /* Probe: the FLAC magic identifies a raw FLAC stream. */
    codec_probe_t probe = {0, 0};
    const uint8_t flac_magic[4] = {'f', 'L', 'a', 'C'};
    codec_error_t rc = codec_probe(flac_magic, 4, &probe);
    if (rc != CODEC_OK) {
        die(rc, "probe");
    }
    printf("probe: container=%d codec=%d (expect 2/1)\n", probe.container,
           probe.codec);

    /* One-shot encode, then read the STREAMINFO total samples back.
     * STREAMINFO is at file offset 8; its packed rate/channels/bps/
     * total field is STREAMINFO bytes 10..17, and the 36-bit total
     * is the low 36 bits (STREAMINFO bytes 13..17 = file 21..25). */
    uint8_t* flac = NULL;
    size_t flac_len = 0;
    rc = codec_encode_flac(samples, frames, channels, sample_rate, &flac,
                           &flac_len);
    if (rc != CODEC_OK) {
        die(rc, "codec_encode_flac");
    }
    printf("one-shot encoded %zu bytes of FLAC\n", flac_len);
    uint64_t total = ((uint64_t)(flac[21] & 0x0F) << 32) |
                     ((uint64_t)flac[22] << 24) |
                     ((uint64_t)flac[23] << 16) |
                     ((uint64_t)flac[24] << 8) | (uint64_t)flac[25];
    printf("streaminfo total samples: %llu (expect 4410)\n",
           (unsigned long long)total);
    codec_free(flac);
    if (total != frames) {
        fprintf(stderr, "streaminfo total mismatch\n");
        return 1;
    }

    /* Streaming round-trip: encode one frame, decode it, compare.
     * Each encoder call produces exactly one FLAC frame, so no
     * framing is needed. */
    codec_encoder_t* encoder = NULL;
    rc = codec_encoder_new_flac(sample_rate, channels, &encoder);
    if (rc != CODEC_OK) {
        die(rc, "codec_encoder_new_flac");
    }
    const codec_packet_t* header = NULL;
    rc = codec_encoder_flac_streaminfo(encoder, &header);
    if (rc != CODEC_OK) {
        die(rc, "codec_encoder_flac_streaminfo");
    }
    printf("streaminfo header: %zu bytes (expect 42)\n", header->len);

    codec_decoder_t* decoder = NULL;
    rc = codec_decoder_from_flac_streaminfo(header->data + 8, 34,
                                            &decoder);
    if (rc != CODEC_OK) {
        die(rc, "codec_decoder_from_flac_streaminfo");
    }

    float* f32 = malloc(frames * channels * sizeof(float));
    if (!f32) {
        fprintf(stderr, "out of memory\n");
        return 1;
    }
    for (uint64_t i = 0; i < frames * channels; i++) {
        f32[i] = (float)samples[i] / 32768.0f;
    }
    /* One encode call takes at most 4096 frames, so split 4410
     * into 4096 + 314 and decode each frame in turn. */
    uint64_t decoded_frames = 0;
    int max_abs_diff = 0;
    uint64_t done = 0;
    while (done < frames) {
        uint64_t chunk = frames - done;
        if (chunk > 4096) {
            chunk = 4096;
        }
        const codec_packet_t* packet = NULL;
        rc = codec_encoder_encode(encoder, f32 + done * channels, chunk,
                                  &packet);
        if (rc != CODEC_OK) {
            die(rc, "codec_encoder_encode");
        }
        const codec_audio_frame_t* view = NULL;
        rc = codec_decoder_decode(decoder, packet->data, packet->len,
                                  decoded_frames * 1000000 / sample_rate,
                                  &view);
        if (rc != CODEC_OK) {
            die(rc, "codec_decoder_decode");
        }
        for (uint64_t i = 0; i < view->frames * view->channels; i++) {
            int got = (int)(view->samples[i] * 32768.0f);
            if (got > 32767) {
                got = 32767;
            }
            int diff = (int)samples[(decoded_frames * channels) + i] - got;
            if (diff < 0) {
                diff = -diff;
            }
            if (diff > max_abs_diff) {
                max_abs_diff = diff;
            }
        }
        decoded_frames += view->frames;
        done += chunk;
    }
    printf("decoded %llu frames (expect 4410)\n",
           (unsigned long long)decoded_frames);
    codec_decoder_free(decoder);
    codec_encoder_free(encoder);
    free(f32);
    free(samples);

    if (decoded_frames != frames || max_abs_diff != 0) {
        fprintf(stderr, "round-trip mismatch\n");
        return 1;
    }
    printf("round-trip exact\n");
    return 0;
}
