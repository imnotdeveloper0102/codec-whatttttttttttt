# @codec-whatttttttttttt/core

TypeScript bindings for codec-whatttttttttttt (Phase 10), built
on `@codec-whatttttttttttt/wasm`. Thin wrappers over the WASM
module; no decode or encode logic lives here.

## API

```ts
import { CodecWasm } from "@codec-whatttttttttttt/wasm";
import {
  Decoder,
  Encoder,
  encodeFlac,
  probe,
  version,
} from "@codec-whatttttttttttt/core";

const wasm = await CodecWasm.loadFromFile("codec_whatttttttttttt_wasm.wasm");
console.log(version(wasm)); // "0.1.0"

const { container, codec } = probe(wasm, fileBytes);

const decoder = Decoder.fromFlacStreaminfo(wasm, streaminfo);
const frame = decoder.decode(packet); // { sampleRate, channels, frames, samples: Float32Array }

const encoder = Encoder.flac(wasm, 44100, 2);
const header = encoder.streaminfo(); // 42-byte STREAMINFO
const packet = encoder.encode(floatSamples, 1000);
encoder.close();

const flac = encodeFlac(wasm, int16Samples, 1000, 2, 44100);
```

Errors are `CodecError` with the C ABI numeric code (`ERR_IO`
1, `ERR_INVALID_DATA` 2, `ERR_TRUNCATED` 3, `ERR_UNSUPPORTED` 4,
`ERR_OVERFLOW` 5, `ERR_ALLOCATION_LIMIT` 6,
`ERR_INVALID_ARGUMENT` 7).

## Build and test

```text
npm install          # from bindings/typescript
npm run build --workspace @codec-whatttttttttttt/wasm
npm run build --workspace @codec-whatttttttttttt/core
npm test --workspace @codec-whatttttttttttt/core
```

Tests are `node:test` (`__tests__/decoder.test.ts`, 10 tests)
against the pinned fixtures in
`crates/codec-whatttttttttttt-core/tests/data/`.

## Scope

Same as the core library — see `docs/LIMITS.md`. In
particular: VP9 decode is parser-only (`decode` throws
`ERR_UNSUPPORTED`); only FLAC encode exists; Opus CELT/SILK
decode is unimplemented.
