#!/bin/sh
# Rebuild the WebAssembly module from the Rust crate and copy it
# next to this package. Run from the workspace root.
set -e
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --target wasm32-unknown-unknown \
  -p codec-whatttttttttttt-wasm --release
cp target/wasm32-unknown-unknown/release/codec_whatttttttttttt_wasm.wasm \
  bindings/typescript/packages/wasm/codec_whatttttttttttt_wasm.wasm
