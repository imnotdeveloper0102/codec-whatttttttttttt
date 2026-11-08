# codec-whatttttttttttt — Kotlin binding (Phase 10)

Thin JNI wrapper over the C ABI. No decode or encode logic lives
here; every call forwards to the Rust core through
`crates/codec-whatttttttttttt-c`. Errors are `CodecError` with the
C ABI error code. `Decoder` and `Encoder` are `Closeable`; use
`use { }` (or `close()`) to free the native handle
deterministically. A decoder is used from one thread at a time.

## Layout

- `src/main/kotlin/dev/codecwhat/Codec.kt` — error, probe,
  `AudioFrame`, header types, top-level functions, JNI declarations.
- `src/main/kotlin/dev/codecwhat/Decoder.kt` — `Decoder`.
- `src/main/kotlin/dev/codecwhat/Encoder.kt` — `Encoder` (FLAC).
- `src/main/cpp/jni_bridge.cpp` — JNI shim. Borrowed views are
  copied into Java arrays before returning.
- `src/test/kotlin/dev/codecwhat/DecoderTest.kt` — checks, run as
  a `main` (no test framework dependency).
- `examples/EncodeDecode.kt` — runnable example (sine -> FLAC ->
  decode).
- `build.gradle.kts`, `settings.gradle.kts` — Gradle module (see
  below).

## Toolchains verified in this environment

- JDK 17 (`openjdk-17-jdk-headless`), Kotlin 1.3.31 (`kotlinc`).
- g++ 13.3 for the JNI library.
- Gradle is **not** installed here, so `build.gradle.kts` is
  provided but was not run; the binding was verified with `kotlinc`
  directly, which is the same compiler the Gradle plugin uses.

## Build and test (from the workspace root)

```text
export JAVA_HOME=/usr/lib/jvm/java-17-openjdk-amd64
cargo build -p codec-whatttttttttttt-c

# JNI library (links the Rust static library, so the result is
# self-contained apart from libc/libm).
g++ -shared -fPIC -O2 \
  -I"$JAVA_HOME/include" -I"$JAVA_HOME/include/linux" \
  -I crates/codec-whatttttttttttt-c/include \
  bindings/kotlin/src/main/cpp/jni_bridge.cpp \
  target/debug/libcodec_whatttttttttttt.a -lm -lpthread -ldl \
  -o bindings/kotlin/build/libcodecwhat_jni.so

# Library jar.
kotlinc bindings/kotlin/src/main/kotlin \
  -include-runtime -d bindings/kotlin/build/codecwhat.jar

# Test jar (library + checks) and run.
kotlinc bindings/kotlin/src/main/kotlin bindings/kotlin/src/test/kotlin \
  -include-runtime -d bindings/kotlin/build/codecwhat-test.jar
java -Djava.library.path=bindings/kotlin/build \
  -cp bindings/kotlin/build/codecwhat-test.jar \
  dev.codecwhat.DecoderTestKt

# Example.
kotlinc bindings/kotlin/src/main/kotlin bindings/kotlin/examples \
  -include-runtime -d bindings/kotlin/build/codecwhat-example.jar
java -Djava.library.path=bindings/kotlin/build \
  -cp bindings/kotlin/build/codecwhat-example.jar \
  dev.codecwhat.EncodeDecodeKt
```

## Scope

Same as the C ABI: version, probe, WAV/FLAC audio decode, FLAC
encode (one-shot and streaming), VP9/AV1 header parsers. The
video decoders are parser-only, so the binding exposes the
parsers, not a fake decode. See `docs/LIMITS.md`.
