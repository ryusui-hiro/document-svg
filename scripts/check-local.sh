#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: ./scripts/check-local.sh [--all]

Run the repository's publication, site, formatting, Rust test, and Clippy checks.
Use --all to also rebuild and test the Node.js and Python bindings.

The --all checks require bindings/node/node_modules and a Python environment
at bindings/python/.venv with maturin and pytest installed.
EOF
}

if [[ $# -gt 1 ]]; then
  usage >&2
  exit 2
fi
case "${1:-}" in
  "") check_bindings=false ;;
  --all) check_bindings=true ;;
  -h|--help) usage; exit 0 ;;
  *) usage >&2; exit 2 ;;
esac

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repo_root"

if [[ "$check_bindings" == true ]]; then
  if [[ ! -d bindings/node/node_modules ]]; then
    echo 'Node dependencies are missing; run npm ci in bindings/node.' >&2
    exit 1
  fi
  if [[ ! -x bindings/python/.venv/bin/maturin || ! -x bindings/python/.venv/bin/python ]]; then
    echo 'Python test tools are missing; set up bindings/python/.venv with maturin and pytest.' >&2
    exit 1
  fi
fi

if [[ -d target/package ]]; then
  echo 'Removing target/package and cleaning document-svg to avoid stale Cargo test artifacts.'
  rm -rf -- target/package
  cargo clean -p document-svg
fi

python3 scripts/check-publication.py
python3 scripts/build-site.py --check
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

if [[ "$check_bindings" == true ]]; then
  (
    cd bindings/node
    npm run build
    npm test
  )

  (
    cd bindings/python
    VIRTUAL_ENV="$PWD/.venv" .venv/bin/maturin develop
    .venv/bin/python -m pytest -q tests
  )
fi
