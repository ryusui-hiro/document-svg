#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)

command -v gh >/dev/null 2>&1 || {
  echo "GitHub CLI is required: https://cli.github.com/" >&2
  exit 69
}
command -v cargo >/dev/null 2>&1 || {
  echo "Rust and cargo are required: https://rustup.rs" >&2
  exit 69
}

cargo install --path "$repo_root" --locked --force

# gh derives the extension name from the local repository directory. Stage a
# persistent symlink named gh-docsvg so installation works regardless of the
# checkout folder's name.
staging_parent="$repo_root/.gh-extension"
staged_repo="$staging_parent/gh-docsvg"
mkdir -p "$staging_parent"
if [[ -L "$staged_repo" ]]; then
  current_target=$(readlink "$staged_repo")
  if [[ "$current_target" != "$repo_root" ]]; then
    echo "unexpected gh-docsvg staging target: $current_target" >&2
    exit 73
  fi
elif [[ -e "$staged_repo" ]]; then
  echo "gh extension staging path already exists: $staged_repo" >&2
  exit 73
else
  ln -s "$repo_root" "$staged_repo"
fi

if gh extension list | awk '$1 == "gh" && $2 == "docsvg" { found = 1 } END { exit !found }'; then
  echo "The gh docsvg command is already installed; leaving the existing extension in place."
else
  (CDPATH= cd -- "$staged_repo" && gh extension install .)
fi
echo "Installed the GitHub CLI extension. Run: gh docsvg --help"
