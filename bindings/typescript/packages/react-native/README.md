# @codec-whatttttttttttt/react-native

React Native bindings for codec-whatttttttttttt (Phase 10): a
TurboModule named `CodecWhat` with the same API shape as
`@codec-whatttttttttttt/core` (Decoder / Encoder / probe /
encodeFlac / header parsers), async throughout.

Status: **UNVERIFIED**. The TypeScript compiles (see below), but
the native modules have not been built or run: no Metro, Android
SDK, or macOS/Xcode in this environment. Do not ship this package
until it has been built in a real app and exercised on a device.

## How it works

- JS side (`src/`): `NativeCodecWhat.ts` is the TurboModule
  spec; `decoder.ts` / `encoder.ts` / `index.ts` wrap it.
- Android (`android/`): `CodecWhatModule.kt` adapts the spec to
  the Kotlin binding (`dev.codecwhat`): it converts base64 /
  bridge arrays and delegates; no decode or encode logic lives
  there. `CodecWhatPackage.kt` registers the module.
- iOS (`ios/`): `CodecWhat.mm` calls the C ABI directly
  (`codec_whatttttttttttt.h`); no decode or encode logic lives
  there.

Binary data crosses the bridge as base64 strings (the bridge
has no byte-array type) and audio samples as arrays of numbers.
That is slow for long audio: this binding is for short clips,
probes, and header parsing, not realtime playback. A later
version should pass sample buffers through JSI typed arrays
instead.

## Android setup

1. Build the Kotlin binding's native library first (see
   `bindings/kotlin/README.md`): `libcodecwhat_jni.so` plus the
   Rust static library it links.
2. In the app's `settings.gradle`, include the Kotlin binding as
   a module named `:codec-what-kotlin` (or adjust the
   `project(":codec-what-kotlin")` reference in
   `android/build.gradle.kts` to the name the app uses).
3. Register `dev.codecwhat.CodecWhatPackage` in the app's
   `ReactNativeHost.getPackages()` (or the New Architecture
   equivalent).

## iOS setup

1. Build `libcodec_whatttttttttttt.a` for the app's targets:
   `cargo build --target aarch64-apple-ios -p codec-whatttttttttttt-c`
   (and `x86_64-apple-ios` for the simulator), and add the
   directory holding the `.a` to `LIBRARY_SEARCH_PATHS`.
2. Add the pod by path in the app's `Podfile`:
   `pod 'codec-whatttttttttttt', :path => '../node_modules/@codec-whatttttttttttt/react-native'`
   (adjust the path to where the package is installed).
3. Run `pod install`.

## API

```ts
import { Decoder, Encoder, probe, encodeFlac, version } from "@codec-whatttttttttttt/react-native";

const { container, codec } = await probe(fileBytes);
const decoder = Decoder.fromFlacStreaminfo(streaminfo);
const frame = await decoder.decode(packet); // { sampleRate, channels, frames, samples: Float32Array }

const encoder = await Encoder.flac(44100, 2);
const header = await encoder.streaminfo(); // 42-byte STREAMINFO
const packet = await encoder.encode(floatSamples, 1000);
await encoder.close();
```

Errors are `CodecError` with the C ABI numeric code
(`ERR_IO` 1, `ERR_INVALID_DATA` 2, `ERR_TRUNCATED` 3,
`ERR_UNSUPPORTED` 4, `ERR_OVERFLOW` 5, `ERR_ALLOCATION_LIMIT` 6,
`ERR_INVALID_ARGUMENT` 7). A native rejection carries the code
as a string; the JS wrapper converts it to `CodecError`.

## Limits

Same as the core library — see `docs/LIMITS.md` in the
workspace root. In particular: VP9 decode is parser-only
(`decode` throws `ERR_UNSUPPORTED`); only FLAC encode exists;
Opus CELT/SILK decode is unimplemented.
