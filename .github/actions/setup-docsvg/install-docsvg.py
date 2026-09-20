#!/usr/bin/env python3
"""Install the docsvg CLI from a checksum-verified GitHub release archive.

Downloads happen only from this repository's own release assets, and every
archive is verified against the SHA256SUMS asset published with the release.
"""

import argparse
import hashlib
import json
import os
import platform
import stat
import sys
import tempfile
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

REPO = 'ryusui-hiro/document-svg'
RELEASES = f'https://github.com/{REPO}/releases/download'
LATEST_API = f'https://api.github.com/repos/{REPO}/releases/latest'

TARGETS = {
    ('darwin', 'arm64'): 'aarch64-apple-darwin',
    ('darwin', 'x86_64'): 'x86_64-apple-darwin',
    ('linux', 'x86_64'): 'x86_64-unknown-linux-musl',
    ('linux', 'arm64'): 'aarch64-unknown-linux-musl',
    ('windows', 'x86_64'): 'x86_64-pc-windows-msvc',
    ('windows', 'arm64'): 'aarch64-pc-windows-msvc',
}

MACHINES = {
    'x86_64': 'x86_64', 'amd64': 'x86_64', 'x64': 'x86_64',
    'aarch64': 'arm64', 'arm64': 'arm64',
}


def host_target():
    system = platform.system().lower()
    machine = MACHINES.get(platform.machine().lower())
    target = TARGETS.get((system, machine))
    if target is None:
        raise SystemExit(f'no docsvg release build for {platform.system()} {platform.machine()}')
    return system, target


def fetch(url, token=None):
    request = urllib.request.Request(url, headers={'User-Agent': 'setup-docsvg'})
    if token:
        request.add_header('Authorization', f'Bearer {token}')
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            return response.read()
    except urllib.error.HTTPError as error:
        raise SystemExit(f'{url}: HTTP {error.code}') from error
    except urllib.error.URLError as error:
        raise SystemExit(f'{url}: {error.reason}') from error


def resolve_version(version, token):
    if version and version != 'latest':
        return version.lstrip('v')
    tag = json.loads(fetch(LATEST_API, token)).get('tag_name', '')
    if not tag:
        raise SystemExit('could not resolve the latest docsvg release')
    return tag.lstrip('v')


def expected_digest(version, archive_name, token):
    sums = fetch(f'{RELEASES}/v{version}/SHA256SUMS', token).decode()
    for line in sums.splitlines():
        digest, _, name = line.partition('  ')
        if name.strip() == archive_name:
            return digest.strip()
    raise SystemExit(f'{archive_name} is not listed in SHA256SUMS for v{version}')


def install(version, destination, token):
    system, target = host_target()
    executable = 'docsvg.exe' if system == 'windows' else 'docsvg'
    archive_name = f'docsvg-{version}-{target}.zip'
    payload = fetch(f'{RELEASES}/v{version}/{archive_name}', token)

    digest = hashlib.sha256(payload).hexdigest()
    expected = expected_digest(version, archive_name, token)
    if digest != expected:
        raise SystemExit(f'{archive_name}: checksum mismatch (expected {expected}, got {digest})')

    destination.mkdir(parents=True, exist_ok=True)
    binary = destination / executable
    with tempfile.TemporaryDirectory() as work:
        staged = Path(work) / archive_name
        staged.write_bytes(payload)
        with zipfile.ZipFile(staged) as archive:
            if executable not in archive.namelist():
                raise SystemExit(f'{archive_name} does not contain {executable}')
            extracted = Path(archive.extract(executable, work))
        binary.write_bytes(extracted.read_bytes())
    binary.chmod(binary.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return binary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', default='latest', help='release version or "latest"')
    parser.add_argument('--dest', required=True, type=Path, help='directory to install docsvg into')
    parser.add_argument('--github-path', action='store_true', help='append the destination to GITHUB_PATH')
    arguments = parser.parse_args()

    token = os.environ.get('GH_TOKEN') or os.environ.get('GITHUB_TOKEN')
    version = resolve_version(arguments.version, token)
    binary = install(version, arguments.dest.expanduser().resolve(), token)

    github_path = os.environ.get('GITHUB_PATH')
    if arguments.github_path and github_path:
        with open(github_path, 'a', encoding='utf-8') as stream:
            stream.write(f'{binary.parent}\n')
    github_output = os.environ.get('GITHUB_OUTPUT')
    if github_output:
        with open(github_output, 'a', encoding='utf-8') as stream:
            stream.write(f'docsvg-path={binary}\nversion={version}\n')
    print(f'Installed docsvg {version} at {binary}')


if __name__ == '__main__':
    sys.exit(main())
