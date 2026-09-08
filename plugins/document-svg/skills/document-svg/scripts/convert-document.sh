#!/bin/sh
set -eu

usage() {
  echo "usage: convert-document.sh INPUT OUTPUT [docsvg options]" >&2
  exit 64
}

is_document_svg_repo() {
  candidate=$1
  [ -f "$candidate/Cargo.toml" ] && grep -Eq '^name[[:space:]]*=[[:space:]]*"document-svg"' "$candidate/Cargo.toml"
}

[ "$#" -ge 2 ] || usage

input=$1
output=$2
shift 2

[ -f "$input" ] || {
  echo "input is not a file: $input" >&2
  exit 66
}

case "$input" in
  *.[pP][dD][fF]|*.[pP][pP][tT][xX]|*.[xX][lL][sS][xX]|*.[dD][oO][cC][xX]) ;;
  *)
    echo "unsupported input extension: $input" >&2
    exit 65
    ;;
esac

if [ -d "$output" ] && [ -n "$(find "$output" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
  echo "output directory is not empty: $output" >&2
  exit 73
fi

if [ -e "$output" ] && [ ! -d "$output" ]; then
  echo "output path exists and is not a directory: $output" >&2
  exit 73
fi

mkdir -p "$output"

docsvg_bin=${DOCSVG_BIN:-}
if [ -z "$docsvg_bin" ]; then
  docsvg_bin=$(command -v docsvg || true)
fi
if [ -z "$docsvg_bin" ] && [ -x "$HOME/.cargo/bin/docsvg" ]; then
  docsvg_bin="$HOME/.cargo/bin/docsvg"
fi

if [ -n "$docsvg_bin" ]; then
  "$docsvg_bin" "$input" --output "$output" "$@"
else
  repo=${DOCUMENT_SVG_REPO:-}
  if [ -z "$repo" ] && is_document_svg_repo "$PWD"; then
    repo=$PWD
  fi
  if [ -z "$repo" ] || ! is_document_svg_repo "$repo" || ! command -v cargo >/dev/null 2>&1; then
    echo "docsvg is unavailable; run ./scripts/install-codex-plugin.sh from the document-svg repository" >&2
    exit 69
  fi
  cargo run --quiet --release --manifest-path "$repo/Cargo.toml" --bin docsvg -- \
    "$input" --output "$output" "$@"
fi

[ -f "$output/conversion.json" ] || {
  echo "conversion completed without conversion.json" >&2
  exit 70
}
