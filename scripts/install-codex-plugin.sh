#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
marketplace_name=document-svg

codex_bin=${CODEX_BIN:-}
if [ -z "$codex_bin" ]; then
  codex_bin=$(command -v codex || true)
fi
[ -n "$codex_bin" ] || {
  echo "codex CLI is required: https://developers.openai.com/codex/cli" >&2
  exit 69
}

cargo_bin=${CARGO_BIN:-}
if [ -z "$cargo_bin" ]; then
  cargo_bin=$(command -v cargo || true)
fi
[ -n "$cargo_bin" ] || {
  echo "Rust and cargo are required: https://rustup.rs" >&2
  exit 69
}

"$cargo_bin" install --path "$repo_root" --locked --force

configured_path=$("$codex_bin" plugin marketplace list | awk -F '\t' -v name="$marketplace_name" '$1 == name { print $2; exit }')
if [ -z "$configured_path" ]; then
  "$codex_bin" plugin marketplace add "$repo_root"
elif [ "$configured_path" != "$repo_root" ]; then
  echo "marketplace '$marketplace_name' already points to: $configured_path" >&2
  echo "remove or rename that marketplace before installing this repository" >&2
  exit 73
fi

"$codex_bin" plugin add "document-svg@$marketplace_name"

echo "Installed docsvg and enabled the document-svg Codex plugin."
echo "Start a new Codex task, then invoke \$document-svg."
