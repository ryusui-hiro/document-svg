#!/usr/bin/env python3
"""Locate the docsvg executable, and optionally install a verified release.

Resolution order:

1. ``DOCSVG_BIN``
2. ``docsvg`` on ``PATH``
3. ``~/.cargo/bin/docsvg``
4. the cache this script manages (``~/.cache/document-svg/bin``)
5. a ``document-svg`` repository checkout, through ``cargo run``

Without ``--install`` the script never touches the network: it prints the
resolved path, or exits 69 with the commands a user can run. With ``--install``
it downloads the release archive for this platform from the project's own
GitHub releases and verifies it against the release ``SHA256SUMS`` before use.
"""

import argparse
import hashlib
import json
import os
import platform
import stat
import subprocess
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

INSTALL_HELP = """docsvg is unavailable. Any one of these makes it available:

  install a verified release build (no Rust toolchain required):
    "{script}" --install

  install from crates.io:
    cargo install document-svg --locked

  download an archive for your platform from:
    https://github.com/{repo}/releases

Set DOCSVG_BIN to use an executable from a different location."""


def executable_name():
    return 'docsvg.exe' if platform.system().lower() == 'windows' else 'docsvg'


def cache_directory():
    override = os.environ.get('DOCSVG_CACHE_DIR')
    if override:
        return Path(override).expanduser()
    if platform.system().lower() == 'windows':
        base = os.environ.get('LOCALAPPDATA') or (Path.home() / 'AppData' / 'Local')
        return Path(base) / 'document-svg' / 'bin'
    base = os.environ.get('XDG_CACHE_HOME') or (Path.home() / '.cache')
    return Path(base) / 'document-svg' / 'bin'


def usable(candidate):
    return candidate is not None and candidate.is_file() and os.access(candidate, os.X_OK)


def is_repository(candidate):
    manifest = Path(candidate) / 'Cargo.toml'
    if not manifest.is_file():
        return False
    return any(line.strip().startswith('name') and '"document-svg"' in line
               for line in manifest.read_text(encoding='utf-8', errors='replace').splitlines())


def repository_root():
    override = os.environ.get('DOCUMENT_SVG_REPO')
    if override and is_repository(override):
        return Path(override)
    if is_repository(Path.cwd()):
        return Path.cwd()
    # The skill ships inside the repository, so walk up from this file too.
    for parent in Path(__file__).resolve().parents:
        if is_repository(parent):
            return parent
    return None


def locate():
    override = os.environ.get('DOCSVG_BIN')
    if override:
        candidate = Path(override).expanduser()
        if usable(candidate):
            return candidate
    from shutil import which
    found = which('docsvg')
    if found:
        return Path(found)
    for candidate in (Path.home() / '.cargo' / 'bin' / executable_name(),
                      cache_directory() / executable_name()):
        if usable(candidate):
            return candidate
    return None


def fetch(url):
    request = urllib.request.Request(url, headers={'User-Agent': 'ensure-docsvg'})
    token = os.environ.get('GH_TOKEN') or os.environ.get('GITHUB_TOKEN')
    if token:
        request.add_header('Authorization', f'Bearer {token}')
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            return response.read()
    except urllib.error.HTTPError as error:
        raise SystemExit(f'{url}: HTTP {error.code}') from error
    except urllib.error.URLError as error:
        raise SystemExit(f'{url}: {error.reason}') from error


def resolve_version(version):
    if version and version != 'latest':
        return version.lstrip('v')
    tag = json.loads(fetch(LATEST_API)).get('tag_name', '')
    if not tag:
        raise SystemExit('could not resolve the latest docsvg release')
    return tag.lstrip('v')


def install(version):
    system = platform.system().lower()
    machine = MACHINES.get(platform.machine().lower())
    target = TARGETS.get((system, machine))
    if target is None:
        raise SystemExit(f'no docsvg release build for {platform.system()} {platform.machine()}; '
                         'install with: cargo install document-svg --locked')

    version = resolve_version(version)
    archive_name = f'docsvg-{version}-{target}.zip'
    payload = fetch(f'{RELEASES}/v{version}/{archive_name}')

    digest = hashlib.sha256(payload).hexdigest()
    sums = fetch(f'{RELEASES}/v{version}/SHA256SUMS').decode()
    expected = next((line.split('  ')[0].strip() for line in sums.splitlines()
                     if line.strip().endswith(archive_name)), None)
    if expected is None:
        raise SystemExit(f'{archive_name} is not listed in SHA256SUMS for v{version}')
    if digest != expected:
        raise SystemExit(f'{archive_name}: checksum mismatch (expected {expected}, got {digest})')

    name = executable_name()
    destination = cache_directory()
    destination.mkdir(parents=True, exist_ok=True)
    binary = destination / name
    with tempfile.TemporaryDirectory() as work:
        staged = Path(work) / archive_name
        staged.write_bytes(payload)
        with zipfile.ZipFile(staged) as archive:
            if name not in archive.namelist():
                raise SystemExit(f'{archive_name} does not contain {name}')
            extracted = Path(archive.extract(name, work))
        binary.write_bytes(extracted.read_bytes())
    binary.chmod(binary.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return binary


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--install', action='store_true',
                        help='download a checksum-verified release build when docsvg is missing')
    parser.add_argument('--version', default=os.environ.get('DOCSVG_VERSION', 'latest'),
                        help='release version to install, or "latest"')
    parser.add_argument('--allow-cargo-run', action='store_true',
                        help='fall back to "cargo run" inside a document-svg checkout')
    arguments = parser.parse_args()

    binary = locate()
    if binary is None and arguments.install:
        binary = install(arguments.version)
    if binary is not None:
        print(binary)
        return 0

    if arguments.allow_cargo_run:
        from shutil import which
        root = repository_root()
        if root is not None and which('cargo'):
            print(f'cargo-run:{root}')
            return 0

    script = Path(__file__).with_name('ensure-docsvg.sh')
    print(INSTALL_HELP.format(script=script, repo=REPO), file=sys.stderr)
    return 69


if __name__ == '__main__':
    sys.exit(main())
