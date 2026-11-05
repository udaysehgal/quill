#!/bin/sh
# Build the playground into playground/web (serve that directory).
set -e
cd "$(dirname "$0")"
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/quill_playground.wasm web/quill.wasm
ls -l web/quill.wasm
