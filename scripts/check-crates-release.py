#!/usr/bin/env python3
"""Verify the release crate and decide whether crates.io needs publishing."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import urllib.error
import urllib.parse
import urllib.request


CRATE_NAME = "document-svg"
CRATES_API = "https://crates.io/api/v1/crates"
TAG_PATTERN = re.compile(
    r"^v(?P<version>[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$"
)


def sha256(path: Path) -> str:
    # hashlib.file_digest needs Python 3.11; the publish runner ships 3.10.
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def version_from_tag(tag: str) -> str:
    match = TAG_PATTERN.fullmatch(tag)
    if not match:
        raise ValueError(f"release tag must be vMAJOR.MINOR.PATCH: {tag}")
    return match.group("version")


def verify_release_crate(tag: str, local_crate: Path, release_crate: Path) -> tuple[str, str]:
    version = version_from_tag(tag)
    expected_name = f"{CRATE_NAME}-{version}.crate"
    for path in (local_crate, release_crate):
        if path.name != expected_name:
            raise ValueError(f"expected {expected_name}, got {path.name}")
        if not path.is_file():
            raise ValueError(f"missing crate: {path}")
    local_checksum = sha256(local_crate)
    release_checksum = sha256(release_crate)
    if local_checksum != release_checksum:
        raise ValueError(
            f"tagged source crate does not match the GitHub Release asset: "
            f"{local_checksum} != {release_checksum}"
        )
    return version, local_checksum


def parse_registry_version(payload: bytes, version: str, checksum: str) -> bool:
    data = json.loads(payload)
    published = data.get("version", {})
    if published.get("num") != version:
        raise ValueError(f"crates.io returned the wrong version for {version}")
    published_checksum = published.get("checksum")
    if published_checksum != checksum:
        raise ValueError(
            f"crates.io already has {CRATE_NAME} {version} with a different checksum: "
            f"{published_checksum} != {checksum}"
        )
    return False


def registry_publish_required(
    version: str,
    checksum: str,
    *,
    registry_api: str = CRATES_API,
    opener=urllib.request.urlopen,
) -> bool:
    url = f"{registry_api.rstrip('/')}/{CRATE_NAME}/{urllib.parse.quote(version, safe='')}"
    request = urllib.request.Request(
        url,
        headers={"User-Agent": "document-svg-release-check/2 (+https://github.com/ryusui-hiro/document-svg)"},
    )
    try:
        with opener(request, timeout=30) as response:
            if response.status != 200:
                raise RuntimeError(f"crates.io returned HTTP {response.status}")
            return parse_registry_version(response.read(), version, checksum)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return True
        raise RuntimeError(f"crates.io returned HTTP {error.code}") from error


def write_github_output(publish: bool) -> None:
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with Path(output).open("a", encoding="utf-8") as stream:
            stream.write(f"publish={'true' if publish else 'false'}\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--local-crate", required=True, type=Path)
    parser.add_argument("--release-crate", required=True, type=Path)
    args = parser.parse_args()

    version, checksum = verify_release_crate(args.tag, args.local_crate, args.release_crate)
    publish = registry_publish_required(version, checksum)
    write_github_output(publish)
    if publish:
        print(f"Verified {CRATE_NAME} {version} ({checksum}); crates.io publish required")
    else:
        print(f"Verified {CRATE_NAME} {version} ({checksum}); identical crates.io version already exists")


if __name__ == "__main__":
    main()
