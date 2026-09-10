#!/bin/sh
# Resolve docsvg through ensure-docsvg.py and run it with the given arguments.
set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

resolved=$("$script_dir/ensure-docsvg.sh" --allow-cargo-run)

case "$resolved" in
  cargo-run:*)
    repo=${resolved#cargo-run:}
    exec cargo run --quiet --release --manifest-path "$repo/Cargo.toml" --bin docsvg -- "$@"
    ;;
  *)
    exec "$resolved" "$@"
    ;;
esac
