#!/bin/sh
# Make the docsvg CLI available for the plugin and extension installers.
#
# With cargo present the CLI is built from this checkout, so it matches the
# source being installed. Without cargo a checksum-verified release build is
# downloaded into the shared cache instead, which the wrappers also search.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
resolver="$repo_root/plugins/document-svg/skills/document-svg/scripts/ensure-docsvg.py"

if command -v cargo >/dev/null 2>&1; then
  cargo install --path "$repo_root" --locked --force
  exit 0
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "either Rust (https://rustup.rs) or Python 3 is required to install docsvg" >&2
  exit 69
fi

echo "cargo was not found; installing a verified docsvg release build instead." >&2
python3 "$resolver" --install
