#!/bin/sh
set -eu

usage() {
  echo "usage: reverse-document.sh INPUT.svg-or-directory OUTPUT.pptx-or-docx-or-xlsx [docsvg reverse options]" >&2
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

[ -e "$input" ] || {
  echo "input does not exist: $input" >&2
  exit 66
}

case "$output" in
  *.[pP][pP][tT][xX]|*.[dD][oO][cC][xX]|*.[xX][lL][sS][xX]) ;;
  *)
    echo "unsupported output extension: $output" >&2
    exit 65
    ;;
esac

[ ! -e "$output" ] || {
  echo "output already exists: $output" >&2
  exit 73
}

docsvg_bin=${DOCSVG_BIN:-}
if [ -z "$docsvg_bin" ]; then
  docsvg_bin=$(command -v docsvg || true)
fi
if [ -z "$docsvg_bin" ] && [ -x "$HOME/.cargo/bin/docsvg" ]; then
  docsvg_bin="$HOME/.cargo/bin/docsvg"
fi

if [ -n "$docsvg_bin" ]; then
  "$docsvg_bin" reverse "$input" --output "$output" "$@"
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
    reverse "$input" --output "$output" "$@"
fi
