#!/bin/sh
set -eu

usage() {
  echo "usage: convert-document.sh INPUT [OUTPUT] [docsvg options]" >&2
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
  output="$input_dir/${stem}-svg"
  shift 1
fi

[ -e "$input" ] || {
  echo "input does not exist: $input" >&2
  exit 66
}

if [ -d "$output" ] && [ -n "$(find "$output" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
  echo "output directory is not empty: $output" >&2
  exit 73
fi

if [ -e "$output" ] && [ ! -d "$output" ]; then
  echo "output path exists and is not a directory: $output" >&2
  exit 73
fi

mkdir -p "$output"

"$script_dir/run-docsvg.sh" "$input" --output "$output" "$@"

[ -f "$output/conversion.json" ] || {
  echo "conversion completed without conversion.json" >&2
  exit 70
}
