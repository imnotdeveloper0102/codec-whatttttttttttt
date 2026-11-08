# @codec-whatttttttttttt/wasm (Phase 10)

Low-level wrapper over the codec WebAssembly module. The module
is built from `crates/codec-whatttttttttttt-wasm` (thin exports
over the C ABI); no decode or encode logic lives in this package.
Errors are `CodecError` with the C ABI error code.

## Layout

- `src/index.ts` — `CodecWasm` loader and wrapper, error codes
  and ids, frame and header types.
- `codec_whatttttttttttt_wasm.wasm` — the built module (regenerate
  with `build.sh`).
- `build.sh` — rebuild the module from Rust and copy it here.

## Build and test

```text
npm install          # from bindings/typescript
npm run build --workspace @codec-whatttttttttttt/wasm
```

The TypeScript tests live in `@codec-whatttttttttttt/core`
(`packages/core/__tests__/`); they load this package's `.wasm`
file and exercise the whole surface.

## Scope

Same as the C ABI: version, probe, WAV/FLAC audio decode, FLAC
encode (one-shot and streaming via `encoderNewFlac` /
`encoderStreaminfo` / `encoderEncode` / `encoderFree`), VP9/AV1
header parsers. The video decoders are parser-only, so the
binding exposes the parsers, not a fake decode. See
`docs/LIMITS.md`.
