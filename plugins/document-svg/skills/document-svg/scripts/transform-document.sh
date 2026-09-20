#!/bin/sh
set -eu

usage() {
  echo "usage: transform-document.sh INPUT.svg [OUTPUT.svg] [docsvg transform options]" >&2
  exit 64
}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

[ "$#" -ge 1 ] || usage

input=$1
if [ "$#" -ge 2 ] && [ "${2#-}" = "$2" ]; then
  output=$2
  shift 2
else
  input_dir=$(dirname "$input")
  input_base=$(basename "$input")
  stem="${input_base%.*}"
  output="$input_dir/${stem}.transformed.svg"
  shift 1
fi

[ -e "$input" ] || {
  echo "input does not exist: $input" >&2
  exit 66
}

[ ! -e "$output" ] || {
  echo "output already exists: $output" >&2
  exit 73
}

"$script_dir/run-docsvg.sh" transform "$input" --output "$output" "$@"
