#!/bin/sh
set -eu

usage() {
  echo "usage: reverse-document.sh INPUT.svg-or-directory OUTPUT [docsvg reverse options]" >&2
  exit 64
}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

[ "$#" -ge 2 ] || usage

input=$1
output=$2
shift 2

[ -e "$input" ] || {
  echo "input does not exist: $input" >&2
  exit 66
}

[ ! -e "$output" ] || {
  echo "output already exists: $output" >&2
  exit 73
}

"$script_dir/run-docsvg.sh" reverse "$input" --output "$output" "$@"
