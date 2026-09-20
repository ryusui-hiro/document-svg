#!/usr/bin/env python3
"""Create a reversible JP2 whose color channels are preblended with its matte."""

from __future__ import annotations

import hashlib
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests" / "fixtures"
SOURCE = FIXTURES / "sample_jpeg2000_rgba.pam"
OUTPUT = FIXTURES / "sample_jpeg2000_rgba_preblended.jp2"
EXPECTED_FFMPEG = "ffmpeg version 8.0.1"
MATTE = (0, 0, 255)


def preblend_pam(source: bytes) -> bytes:
    header, samples = source.split(b"ENDHDR\n", 1)
    fields = {}
    for line in header.splitlines()[1:]:
        parts = line.split(maxsplit=1)
        if len(parts) == 2:
            fields[parts[0]] = parts[1]
    if fields.get(b"DEPTH") != b"4" or fields.get(b"MAXVAL") != b"255":
        raise ValueError("source PAM must be 8-bit RGBA")
    width = int(fields[b"WIDTH"])
    height = int(fields[b"HEIGHT"])
    if len(samples) != width * height * 4:
        raise ValueError("source PAM sample count does not match its dimensions")
    output = bytearray(samples)
    for offset in range(0, len(samples), 4):
        alpha = samples[offset + 3]
        for component, matte in enumerate(MATTE):
            color = samples[offset + component]
            output[offset + component] = (
                color * alpha + matte * (255 - alpha) + 127
            ) // 255
    return header + b"ENDHDR\n" + output


def encode_with_ffmpeg(source: bytes) -> bytes:
    version = subprocess.run(
        ["ffmpeg", "-version"], check=True, capture_output=True, text=True
    ).stdout.splitlines()[0]
    if not version.startswith(EXPECTED_FFMPEG):
        raise RuntimeError(f"fixture is pinned to {EXPECTED_FFMPEG}; found {version}")
    with tempfile.TemporaryDirectory(prefix="docsvg-jpx-mode2-") as directory:
        pam = Path(directory) / "preblended.pam"
        jp2 = Path(directory) / "preblended.jp2"
        pam.write_bytes(preblend_pam(source))
        subprocess.run(
            [
                "ffmpeg",
                "-hide_banner",
                "-loglevel",
                "error",
                "-threads",
                "1",
                "-y",
                "-i",
                str(pam),
                "-pred",
                "dwt53",
                "-format",
                "jp2",
                str(jp2),
            ],
            check=True,
        )
        return jp2.read_bytes()


def main() -> None:
    output = encode_with_ffmpeg(SOURCE.read_bytes())
    OUTPUT.write_bytes(output)
    print(hashlib.sha256(output).hexdigest())


if __name__ == "__main__":
    main()
