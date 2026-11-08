// jni_bridge.cpp — JNI shim for the codec-whatttttttttttt Kotlin
// binding (Phase 10).
//
// Every function forwards to the C ABI
// (crates/codec-whatttttttttttt-c/include/codec_whatttttttttttt.h).
// On failure the C error is thrown as dev.codecwhat.CodecError
// with the code from the C ABI and the detail from
// codec_last_error(). Borrowed views (decoded frames, encoded
// packets) are copied into Java arrays before returning, so the
// caller never holds a view past the next native call.

#include "codec_whatttttttttttt.h"

#include <jni.h>

#include <cstdint>
#include <cstring>

namespace {

// Throws dev.codecwhat.CodecError(code, detail). Returns false
// when the exception itself cannot be raised (missing class or
// constructor); callers then return a null/zero sentinel.
bool throw_codec_error(JNIEnv* env, codec_error_t code) {
    const char* detail = codec_last_error();
    jclass cls = env->FindClass("dev/codecwhat/CodecError");
    if (cls == nullptr) {
        return false;
    }
    jmethodID ctor =
        env->GetMethodID(cls, "<init>", "(ILjava/lang/String;)V");
    if (ctor == nullptr) {
        return false;
    }
    jstring message =
        env->NewStringUTF(detail != nullptr ? detail : codec_error_string(code));
    if (message == nullptr) {
        return false;
    }
    jobject error = env->NewObject(cls, ctor, static_cast<jint>(code), message);
    if (error == nullptr) {
        return false;
    }
    env->Throw(static_cast<jthrowable>(error));
    return true;
}

bool check(codec_error_t rc, JNIEnv* env) {
    if (rc == CODEC_OK) {
        return true;
    }
    throw_codec_error(env, rc);
    return false;
}

jobject new_audio_frame(JNIEnv* env, const codec_audio_frame_t* view) {
    jclass cls = env->FindClass("dev/codecwhat/AudioFrame");
    if (cls == nullptr) {
        return nullptr;
    }
    jmethodID ctor = env->GetMethodID(cls, "<init>", "(IIJJ[F)V");
    if (ctor == nullptr) {
        return nullptr;
    }
    const jsize count =
        static_cast<jsize>(view->frames * view->channels);
    jfloatArray samples = env->NewFloatArray(count);
    if (samples == nullptr) {
        return nullptr;
    }
    if (count > 0) {
        env->SetFloatArrayRegion(samples, 0, count, view->samples);
    }
    return env->NewObject(cls, ctor, static_cast<jint>(view->sample_rate),
                          static_cast<jint>(view->channels),
                          static_cast<jlong>(view->frames),
                          static_cast<jlong>(view->timestamp_us), samples);
}

jbyteArray copy_bytes(JNIEnv* env, const uint8_t* data, std::size_t len) {
    jbyteArray out = env->NewByteArray(static_cast<jsize>(len));
    if (out == nullptr) {
        return nullptr;
    }
    if (len > 0) {
        env->SetByteArrayRegion(
            out, 0, static_cast<jsize>(len),
            reinterpret_cast<const jbyte*>(data));
    }
    return out;
}

} // namespace

extern "C" {

JNIEXPORT jstring JNICALL
Java_dev_codecwhat_Native_nativeVersion(JNIEnv* env, jobject /* this */) {
    return env->NewStringUTF(codec_version());
}

JNIEXPORT jintArray JNICALL
Java_dev_codecwhat_Native_nativeProbe(JNIEnv* env, jobject /* this */,
                                      jbyteArray data) {
    const jsize len = env->GetArrayLength(data);
    jbyte* bytes = env->GetByteArrayElements(data, nullptr);
    if (bytes == nullptr) {
        return nullptr;
    }
    codec_probe_t probe{};
    const codec_error_t rc = codec_probe(
        reinterpret_cast<const uint8_t*>(bytes), static_cast<std::size_t>(len),
        &probe);
    env->ReleaseByteArrayElements(data, bytes, JNI_ABORT);
    if (!check(rc, env)) {
        return nullptr;
    }
    jintArray out = env->NewIntArray(2);
    if (out == nullptr) {
        return nullptr;
    }
    const jint values[2] = {probe.container, probe.codec};
    env->SetIntArrayRegion(out, 0, 2, values);
    return out;
}

JNIEXPORT jlong JNICALL
Java_dev_codecwhat_Native_nativeDecoderFromWav(JNIEnv* env, jobject /* this */,
                                               jbyteArray data) {
    const jsize len = env->GetArrayLength(data);
    jbyte* bytes = env->GetByteArrayElements(data, nullptr);
    if (bytes == nullptr) {
        return 0;
    }
    codec_decoder_t* handle = nullptr;
    const codec_error_t rc = codec_decoder_from_wav_header(
        reinterpret_cast<const uint8_t*>(bytes), static_cast<std::size_t>(len),
        &handle);
    env->ReleaseByteArrayElements(data, bytes, JNI_ABORT);
    if (!check(rc, env)) {
        return 0;
    }
    return reinterpret_cast<jlong>(handle);
}

JNIEXPORT jlong JNICALL
Java_dev_codecwhat_Native_nativeDecoderFromFlacStreaminfo(
    JNIEnv* env, jobject /* this */, jbyteArray streaminfo) {
    const jsize len = env->GetArrayLength(streaminfo);
    jbyte* bytes = env->GetByteArrayElements(streaminfo, nullptr);
    if (bytes == nullptr) {
        return 0;
    }
    codec_decoder_t* handle = nullptr;
    const codec_error_t rc = codec_decoder_from_flac_streaminfo(
        reinterpret_cast<const uint8_t*>(bytes), static_cast<std::size_t>(len),
        &handle);
    env->ReleaseByteArrayElements(streaminfo, bytes, JNI_ABORT);
    if (!check(rc, env)) {
        return 0;
    }
    return reinterpret_cast<jlong>(handle);
}

JNIEXPORT jlong JNICALL
Java_dev_codecwhat_Native_nativeDecoderFromVp9FrameHeader(
    JNIEnv* env, jobject /* this */, jbyteArray frame) {
    const jsize len = env->GetArrayLength(frame);
    jbyte* bytes = env->GetByteArrayElements(frame, nullptr);
    if (bytes == nullptr) {
        return 0;
    }
    codec_decoder_t* handle = nullptr;
    const codec_error_t rc = codec_decoder_from_vp9_frame_header(
        reinterpret_cast<const uint8_t*>(bytes), static_cast<std::size_t>(len),
        &handle);
    env->ReleaseByteArrayElements(frame, bytes, JNI_ABORT);
    if (!check(rc, env)) {
        return 0;
    }
    return reinterpret_cast<jlong>(handle);
}

JNIEXPORT jobject JNICALL
Java_dev_codecwhat_Native_nativeDecoderDecode(JNIEnv* env, jobject /* this */,
                                              jlong handle, jbyteArray packet,
                                              jlong timestamp_us) {
    const jsize len = env->GetArrayLength(packet);
    jbyte* bytes = env->GetByteArrayElements(packet, nullptr);
    if (bytes == nullptr) {
        return nullptr;
    }
    const codec_audio_frame_t* view = nullptr;
    const codec_error_t rc = codec_decoder_decode(
        reinterpret_cast<codec_decoder_t*>(handle),
        reinterpret_cast<const uint8_t*>(bytes), static_cast<std::size_t>(len),
        static_cast<uint64_t>(timestamp_us), &view);
    env->ReleaseByteArrayElements(packet, bytes, JNI_ABORT);
    if (!check(rc, env)) {
        return nullptr;
    }
    return new_audio_frame(env, view);
}

JNIEXPORT void JNICALL
Java_dev_codecwhat_Native_nativeDecoderFree(JNIEnv* /* env */,
                                            jobject /* this */, jlong handle) {
    codec_decoder_free(reinterpret_cast<codec_decoder_t*>(handle));
}

JNIEXPORT jlong JNICALL
Java_dev_codecwhat_Native_nativeEncoderNewFlac(JNIEnv* env, jobject /* this */,
                                               jint sample_rate,
                                               jint channels) {
    codec_encoder_t* handle = nullptr;
    const codec_error_t rc = codec_encoder_new_flac(
        static_cast<uint32_t>(sample_rate), static_cast<uint16_t>(channels),
        &handle);
    if (!check(rc, env)) {
        return 0;
    }
    return reinterpret_cast<jlong>(handle);
}

JNIEXPORT jbyteArray JNICALL
Java_dev_codecwhat_Native_nativeEncoderFlacStreaminfo(JNIEnv* env,
                                                       jobject /* this */,
                                                       jlong handle) {
    const codec_packet_t* view = nullptr;
    const codec_error_t rc = codec_encoder_flac_streaminfo(
        reinterpret_cast<codec_encoder_t*>(handle), &view);
    if (!check(rc, env)) {
        return nullptr;
    }
    return copy_bytes(env, view->data, view->len);
}

JNIEXPORT jbyteArray JNICALL
Java_dev_codecwhat_Native_nativeEncoderEncode(JNIEnv* env, jobject /* this */,
                                              jlong handle,
                                              jfloatArray samples,
                                              jlong frames) {
    jfloat* values = env->GetFloatArrayElements(samples, nullptr);
    if (values == nullptr) {
        return nullptr;
    }
    const codec_packet_t* view = nullptr;
    const codec_error_t rc = codec_encoder_encode(
        reinterpret_cast<codec_encoder_t*>(handle), values,
        static_cast<uint64_t>(frames), &view);
    env->ReleaseFloatArrayElements(samples, values, JNI_ABORT);
    if (!check(rc, env)) {
        return nullptr;
    }
    return copy_bytes(env, view->data, view->len);
}

JNIEXPORT void JNICALL
Java_dev_codecwhat_Native_nativeEncoderFree(JNIEnv* /* env */,
                                            jobject /* this */, jlong handle) {
    codec_encoder_free(reinterpret_cast<codec_encoder_t*>(handle));
}

JNIEXPORT jbyteArray JNICALL
Java_dev_codecwhat_Native_nativeEncodeFlac(JNIEnv* env, jobject /* this */,
                                           jshortArray samples, jlong frames,
                                           jint channels, jint sample_rate) {
    jshort* values = env->GetShortArrayElements(samples, nullptr);
    if (values == nullptr) {
        return nullptr;
    }
    uint8_t* data = nullptr;
    std::size_t len = 0;
    const codec_error_t rc = codec_encode_flac(
        reinterpret_cast<const int16_t*>(values),
        static_cast<uint64_t>(frames), static_cast<uint16_t>(channels),
        static_cast<uint32_t>(sample_rate), &data, &len);
    env->ReleaseShortArrayElements(samples, values, JNI_ABORT);
    if (!check(rc, env)) {
        return nullptr;
    }
    jbyteArray out = copy_bytes(env, data, len);
    codec_free(data);
    return out;
}

JNIEXPORT jobject JNICALL
Java_dev_codecwhat_Native_nativeParseVp9FrameHeader(JNIEnv* env,
                                                     jobject /* this */,
                                                     jbyteArray frame) {
    const jsize len = env->GetArrayLength(frame);
    jbyte* bytes = env->GetByteArrayElements(frame, nullptr);
    if (bytes == nullptr) {
        return nullptr;
    }
    codec_vp9_frame_header_t header{};
    const codec_error_t rc = codec_vp9_parse_frame_header(
        reinterpret_cast<const uint8_t*>(bytes), static_cast<std::size_t>(len),
        &header);
    env->ReleaseByteArrayElements(frame, bytes, JNI_ABORT);
    if (!check(rc, env)) {
        return nullptr;
    }
    jclass cls = env->FindClass("dev/codecwhat/Vp9FrameHeader");
    if (cls == nullptr) {
        return nullptr;
    }
    jmethodID ctor =
        env->GetMethodID(cls, "<init>", "(IIZZIIIIIIIII)V");
    if (ctor == nullptr) {
        return nullptr;
    }
    return env->NewObject(
        cls, ctor, static_cast<jint>(header.profile),
        static_cast<jint>(header.frame_type),
        static_cast<jboolean>(header.show_frame),
        static_cast<jboolean>(header.show_existing_frame),
        static_cast<jint>(header.width), static_cast<jint>(header.height),
        static_cast<jint>(header.render_width),
        static_cast<jint>(header.render_height),
        static_cast<jint>(header.base_q_idx),
        static_cast<jint>(header.loop_filter_level),
        static_cast<jint>(header.tile_cols_log2),
        static_cast<jint>(header.tile_rows_log2),
        static_cast<jint>(header.header_size_in_bytes));
}

JNIEXPORT jobject JNICALL
Java_dev_codecwhat_Native_nativeParseAv1SequenceHeader(JNIEnv* env,
                                                        jobject /* this */,
                                                        jbyteArray obu) {
    const jsize len = env->GetArrayLength(obu);
    jbyte* bytes = env->GetByteArrayElements(obu, nullptr);
    if (bytes == nullptr) {
        return nullptr;
    }
    codec_av1_sequence_header_t header{};
    const codec_error_t rc = codec_av1_parse_sequence_header(
        reinterpret_cast<const uint8_t*>(bytes), static_cast<std::size_t>(len),
        &header);
    env->ReleaseByteArrayElements(obu, bytes, JNI_ABORT);
    if (!check(rc, env)) {
        return nullptr;
    }
    jclass cls = env->FindClass("dev/codecwhat/Av1SequenceHeader");
    if (cls == nullptr) {
        return nullptr;
    }
    jmethodID ctor = env->GetMethodID(cls, "<init>", "(IIIIIIIZ)V");
    if (ctor == nullptr) {
        return nullptr;
    }
    return env->NewObject(
        cls, ctor, static_cast<jint>(header.seq_profile),
        static_cast<jint>(header.seq_level_idx_0),
        static_cast<jint>(header.seq_tier_0),
        static_cast<jint>(header.max_frame_width_minus_1),
        static_cast<jint>(header.max_frame_height_minus_1),
        static_cast<jint>(header.frame_width_bits_minus_1),
        static_cast<jint>(header.frame_height_bits_minus_1),
        static_cast<jboolean>(header.reduced_still_picture_header));
}

} // extern "C"
