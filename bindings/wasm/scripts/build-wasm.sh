#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)

cd "$REPO_ROOT"
cargo build --release -p document-svg-wasm --target wasm32-unknown-unknown
wasm-bindgen \
  --target web \
  --out-dir "$SCRIPT_DIR/web/pkg" \
  --out-name document_svg_wasm \
  "$REPO_ROOT/target/wasm32-unknown-unknown/release/document_svg_wasm.wasm"
