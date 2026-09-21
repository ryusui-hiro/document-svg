#!/bin/sh
# Assemble the GitHub Pages site in one folder: the generated pages from site/
# plus the in-browser viewer demo at viewer/, built from bindings/wasm.
# Needs Rust with the wasm32-unknown-unknown target and wasm-bindgen-cli
# (the version pinned in .github/workflows/browser-wasm.yml).
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
OUT=${1:-"$ROOT/_site"}

rm -rf "$OUT"
mkdir -p "$OUT"
cp -R "$ROOT/site/." "$OUT/"

sh "$ROOT/bindings/wasm/scripts/build-wasm.sh"
mkdir -p "$OUT/viewer/web"
cp -R "$ROOT/examples/static-viewer/." "$OUT/viewer/"
cp -R "$ROOT/bindings/wasm/web/." "$OUT/viewer/web/"
# The example imports the viewer from the repository layout; here it sits beside it.
sed "s#'../../bindings/wasm/web/#'./web/#" "$ROOT/examples/static-viewer/app.js" > "$OUT/viewer/app.js"
grep -q "'./web/docsvg-viewer.js" "$OUT/viewer/app.js"
