"""Download only checksum-verified PyPI artifacts from this repository's release."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import zipfile

repo = 'ryusui-hiro/document-svg'
tag = os.environ['RELEASE_TAG']
if not re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+', tag):
    raise SystemExit('expected a version tag such as v0.1.1')
version = tag[1:]
destination = Path('dist/pypi')
destination.mkdir(parents=True, exist_ok=False)
subprocess.run(['gh', 'release', 'download', tag, '--repo', repo, '--dir', str(destination),
                '--pattern', '*.whl', '--pattern', '*.tar.gz', '--pattern', 'release-manifest.json'], check=True)
manifest = json.loads((destination / 'release-manifest.json').read_text())
if manifest['version'] != version:
    raise SystemExit('release manifest version mismatch')
ref = json.loads(subprocess.check_output(['gh', 'api', f'repos/{repo}/git/ref/tags/{tag}']))['object']
if ref['type'] == 'tag':
    ref = json.loads(subprocess.check_output(['gh', 'api', f'repos/{repo}/git/tags/{ref["sha"]}']))['object']
if ref['sha'] != manifest['commit']:
    raise SystemExit('release artifacts do not match the tagged commit')
artifacts = list(destination.glob('*.whl')) + list(destination.glob('*.tar.gz'))
expected = {name for name in manifest['files'] if name.endswith(('.whl', '.tar.gz'))}
if {path.name for path in artifacts} != expected or not expected:
    raise SystemExit('release artifacts are missing')
for path in artifacts:
    with path.open('rb') as stream:
        hasher = hashlib.sha256()
        while chunk := stream.read(1024 * 1024):
            hasher.update(chunk)
        digest = hasher.hexdigest()
    if digest != manifest['files'][path.name]:
        raise SystemExit(f'checksum mismatch: {path.name}')
    if path.suffix == '.whl':
        with zipfile.ZipFile(path) as archive:
            metadata = archive.read(next(name for name in archive.namelist() if name.endswith('.dist-info/METADATA'))).decode()
    else:
        with tarfile.open(path) as archive:
            info = next(item for item in archive.getmembers() if item.name.endswith('/PKG-INFO'))
            if info.size > 1024 * 1024:
                raise SystemExit('oversized package metadata')
            metadata = archive.extractfile(info).read().decode()
    if 'Name: document-svg\n' not in metadata or f'Version: {version}\n' not in metadata:
        raise SystemExit(f'unexpected package identity: {path.name}')
(destination / 'release-manifest.json').unlink()
print(f'Checked {len(artifacts)} artifacts for document-svg {version}')
