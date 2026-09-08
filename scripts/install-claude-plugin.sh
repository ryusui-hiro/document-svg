#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
marketplace_name=document-svg

command -v claude >/dev/null 2>&1 || {
  echo "Claude Code is required: https://code.claude.com/docs/en/setup" >&2
  exit 69
}
command -v cargo >/dev/null 2>&1 || {
  echo "Rust and cargo are required: https://rustup.rs" >&2
  exit 69
}

if ! marketplaces=$(claude plugin marketplace list --json); then
  echo "Claude Code could not read its settings. Ensure the active settings.json files contain valid JSON, then retry." >&2
  exit 78
fi

cargo install --path "$repo_root" --locked --force

if ! grep -Fq '"name": "document-svg"' <<<"$marketplaces"; then
  claude plugin marketplace add "$repo_root" --scope user
fi
if ! claude plugin install "document-svg@$marketplace_name" --scope user; then
  echo "Claude Code could not enable the plugin. If it reports an invalid settings file, remove JSON comments or otherwise repair that settings.json before retrying." >&2
  exit 78
fi

echo "Installed the document-svg Claude Code plugin."
echo "Restart Claude Code, then invoke /document-svg:document-svg."
