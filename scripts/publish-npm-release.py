"""Publish checksum-verified release tarballs, native dependencies first.

PUBLIC_REGISTRY is npm or github. GitHub Actions supplies OIDC for npm and an
ephemeral GITHUB_TOKEN for GitHub Packages; this script stores no credentials.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile

repo = 'ryusui-hiro/document-svg'
kind = os.environ.get('PUBLIC_REGISTRY', 'npm')
if kind not in ('npm', 'github'):
    raise SystemExit('unknown registry')
registry = 'https://registry.npmjs.org' if kind == 'npm' else 'https://npm.pkg.github.com'
tag = os.environ['RELEASE_TAG']
if not re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+', tag):
    raise SystemExit('invalid release tag')
version = tag[1:]
destination = Path('dist') / f'publish-{kind}'
destination.mkdir(parents=True, exist_ok=False)
prefix = 'document-svg-' if kind == 'npm' else 'ryusui-hiro-document-svg-'
subprocess.run(['gh', 'release', 'download', tag, '--repo', repo, '--dir', str(destination),
                '--pattern', prefix + '*.tgz', '--pattern', 'release-manifest.json'], check=True)
manifest = json.loads((destination / 'release-manifest.json').read_text())
ref = json.loads(subprocess.check_output(['gh', 'api', f'repos/{repo}/git/ref/tags/{tag}']))['object']
if ref['type'] == 'tag':
    ref = json.loads(subprocess.check_output(['gh', 'api', f'repos/{repo}/git/tags/{ref["sha"]}']))['object']
if ref['sha'] != manifest['commit'] or manifest['version'] != version:
    raise SystemExit('release source mismatch')
targets = json.loads(Path('scripts/release-targets.json').read_text())
root_name = 'document-svg' if kind == 'npm' else '@ryusui-hiro/document-svg'
expected_names = {root_name} | {root_name + '-' + target['tag'] for target in targets}
artifacts = []
for path in destination.glob('*.tgz'):
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != manifest['files'].get(path.name):
        raise SystemExit(f'checksum mismatch: {path.name}')
    with tarfile.open(path) as archive:
        package = json.loads(archive.extractfile('package/package.json').read())
        for license in ('LICENSE-MIT', 'LICENSE-APACHE', 'THIRD_PARTY_LICENSES.txt'):
            archive.getmember('package/' + license)
    if package['version'] != version or package['name'] not in expected_names:
        raise SystemExit('unexpected npm package identity')
    integrity = 'sha512-' + base64.b64encode(hashlib.sha512(data).digest()).decode()
    artifacts.append((package['name'], path, integrity))
if {name for name, _, _ in artifacts} != expected_names or len(artifacts) != len(expected_names):
    raise SystemExit('missing or duplicate npm packages')
for name, path, integrity in sorted(artifacts, key=lambda item: (item[0] == root_name, item[0])):
    found = subprocess.run(['npm', 'view', f'{name}@{version}', 'dist.integrity', '--json', '--registry', registry], capture_output=True, text=True)
    if found.returncode == 0:
        if json.loads(found.stdout) != integrity:
            raise SystemExit(f'{name}: existing version has different content')
        print(f'{name}@{version} is already published with matching content')
        continue
    if 'E404' not in found.stdout + found.stderr:
        raise SystemExit(f'{name}: registry lookup failed: {found.stderr}')
    subprocess.run(['npm', 'publish', str(path.resolve()), '--access', 'public', '--ignore-scripts', '--registry', registry], check=True)
