"""Stage and verify versioned release files; never publishes to a registry."""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = json.loads((ROOT / 'scripts/release-targets.json').read_text())
LICENSES = ['LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD_PARTY_NOTICES.md', 'THIRD_PARTY_LICENSES.txt']


def sha256(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def version():
    rust = tomllib.loads((ROOT / 'Cargo.toml').read_text())['package']['version']
    node = json.loads((ROOT / 'bindings/node/package.json').read_text())['version']
    python = tomllib.loads((ROOT / 'bindings/python/pyproject.toml').read_text())['project']['version']
    if not rust == node == python:
        raise ValueError('release versions do not agree')
    for path in ('bindings/node/Cargo.toml', 'bindings/python/Cargo.toml'):
        if tomllib.loads((ROOT / path).read_text())['package']['version'] != rust:
            raise ValueError(f'{path}: release version mismatch')
    for path in ('plugins/document-svg/.codex-plugin/plugin.json', 'plugins/document-svg/.claude-plugin/plugin.json'):
        if json.loads((ROOT / path).read_text())['version'].split('+')[0] != rust:
            raise ValueError(f'{path}: release version mismatch')
    return rust


def check_wheel(path, expected):
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        metadata = archive.read(next(name for name in names if name.endswith('.dist-info/METADATA'))).decode()
        wheel = archive.read(next(name for name in names if name.endswith('.dist-info/WHEEL'))).decode()
        if ('Name: document-svg\n' not in metadata or f'Version: {expected}\n' not in metadata
                or 'License-Expression: MIT OR Apache-2.0' not in metadata or 'Tag: cp310-abi3-' not in wheel):
            raise ValueError(f'{path.name}: unexpected wheel metadata')
        for name in ('LICENSE-MIT', 'LICENSE-APACHE', 'THIRD_PARTY_LICENSES.txt'):
            if not any(entry.endswith('/' + name) for entry in names):
                raise ValueError(f'{path.name}: missing {name}')
        extra_libraries = [entry for entry in names if re.search(r'\.(?:so(?:\.[0-9]+)*|pyd|dll)$', entry) and '/_native' not in entry]
        if extra_libraries:
            raise ValueError(f'{path.name}: review bundled native library licenses: {extra_libraries}')


def stage_target(tag):
    target = next(item for item in TARGETS if item['tag'] == tag)
    release_version = version()
    destination = ROOT / 'dist/artifacts' / tag
    destination.mkdir(parents=True, exist_ok=False)
    binding = ROOT / 'bindings/node' / f'document-svg.{tag}.node'
    if not binding.is_file():
        raise ValueError(f'missing native binding for {tag}')
    shutil.copy2(binding, destination / binding.name)
    wheels = list((ROOT / 'dist/wheels').glob('*.whl'))
    if len(wheels) != 1:
        raise ValueError(f'expected one ABI3 wheel, got {len(wheels)}')
    check_wheel(wheels[0], release_version)
    shutil.copy2(wheels[0], destination / wheels[0].name)
    if target.get('libc') != 'glibc':
        executable = 'docsvg.exe' if target['os'] == 'win32' else 'docsvg'
        binary = ROOT / 'target' / target['target'] / 'release' / executable
        output = subprocess.check_output([str(binary), '--version'], text=True).strip()
        if output != f'docsvg {release_version}':
            raise ValueError(f'unexpected CLI version: {output}')
        archive_path = destination / f'docsvg-{release_version}-{target["target"]}.zip'
        with zipfile.ZipFile(archive_path, 'w', zipfile.ZIP_DEFLATED) as archive:
            archive.write(binary, executable)
            for filename in LICENSES:
                archive.write(ROOT / filename, filename)
    info = dict(version=release_version, target=target['target'], tag=tag,
                commit=os.environ.get('GITHUB_SHA', subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()),
                files={path.name: sha256(path) for path in sorted(destination.iterdir())})
    (destination / 'build-info.json').write_text(json.dumps(info, indent=2) + '\n')
    print(f'Staged {tag}: {len(info["files"])} checked artifacts')


def copy_licenses(destination):
    for filename in LICENSES:
        shutil.copy2(ROOT / filename, destination / filename)


def assemble(artifacts, output, commit):
    release_version = version()
    output.mkdir(parents=True, exist_ok=False)
    packages = output / 'npm-packages'
    packages.mkdir()
    source_config = json.loads((ROOT / 'bindings/node/package.json').read_text())
    optional = {}
    for target in TARGETS:
        tag = target['tag']
        candidates = [path for path in artifacts.rglob('build-info.json')
                      if json.loads(path.read_text()).get('tag') == tag]
        if len(candidates) != 1:
            raise ValueError(f'missing or duplicate artifact manifest: {tag}')
        info_path = candidates[0]
        info = json.loads(info_path.read_text())
        if info['version'] != release_version or info['commit'] != commit:
            raise ValueError(f'{tag}: source revision/version mismatch')
        for filename, expected in info['files'].items():
            if Path(filename).name != filename:
                raise ValueError('unsafe artifact filename')
            path = info_path.parent / filename
            if sha256(path) != expected:
                raise ValueError(f'checksum mismatch: {filename}')
            if path.suffix == '.whl':
                check_wheel(path, release_version)
                shutil.copy2(path, output / filename)
            elif path.suffix == '.zip':
                shutil.copy2(path, output / filename)
        name = f'document-svg-{tag}'
        optional[name] = release_version
        package = packages / name
        package.mkdir()
        binary_name = f'document-svg.{tag}.node'
        shutil.copy2(info_path.parent / binary_name, package / binary_name)
        copy_licenses(package)
        config = dict(name=name, version=release_version, description=f'Native document-svg binding for {tag}',
                      license='MIT OR Apache-2.0', main=binary_name, os=[target['os']], cpu=[target['cpu']],
                      files=[binary_name] + LICENSES, engines={'node': '>=18'},
                      repository=source_config['repository'], publishConfig={'access': 'public'})
        if target.get('libc'):
            config['libc'] = [target['libc']]
        (package / 'package.json').write_text(json.dumps(config, indent=2) + '\n')
        subprocess.run(['npm', 'pack', '--ignore-scripts', '--pack-destination', str(output.resolve())], cwd=package, check=True)

    root_package = packages / 'document-svg'
    root_package.mkdir()
    for name in ['index.js', 'index.d.ts', 'preview-ui.js', 'preview-ui.d.ts', 'README.md']:
        shutil.copy2(ROOT / 'bindings/node' / name, root_package / name)
    for name in ['docs', 'examples']:
        shutil.copytree(ROOT / 'bindings/node' / name, root_package / name)
    copy_licenses(root_package)
    config = dict(source_config)
    config.pop('devDependencies', None)
    config.pop('scripts', None)
    config['optionalDependencies'] = optional
    config['publishConfig'] = {'access': 'public'}
    config['files'] = [name for name in config['files'] if not name.endswith('.node')]
    (root_package / 'package.json').write_text(json.dumps(config, indent=2) + '\n')
    subprocess.run(['npm', 'pack', '--ignore-scripts', '--pack-destination', str(output.resolve())], cwd=root_package, check=True)

    # GitHub Packages requires an owner scope. Build a separate authenticated
    # mirror, including scoped native dependencies and matching loader names.
    mirrors = output / 'github-packages'
    mirrors.mkdir()
    for source in sorted(packages.iterdir()):
        mirror = mirrors / source.name
        shutil.copytree(source, mirror)
        config = json.loads((mirror / 'package.json').read_text())
        config['name'] = '@ryusui-hiro/' + config['name']
        config['publishConfig'] = {'access': 'public', 'registry': 'https://npm.pkg.github.com'}
        if 'optionalDependencies' in config:
            config['optionalDependencies'] = {'@ryusui-hiro/' + name: value for name, value in config['optionalDependencies'].items()}
            index = (mirror / 'index.js').read_text()
            index = re.sub(r"require\((['\"])(document-svg-[^'\"]+)(['\"])\)", r"require(\1@ryusui-hiro/\2\3)", index)
            (mirror / 'index.js').write_text(index)
        (mirror / 'package.json').write_text(json.dumps(config, indent=2) + '\n')
        subprocess.run(['npm', 'pack', '--ignore-scripts', '--pack-destination', str(output.resolve())], cwd=mirror, check=True)

    for pattern in ('*.crate', '*.tar.gz'):
        sources = list(artifacts.rglob(pattern))
        if len(sources) != 1:
            raise ValueError(f'expected one source artifact for {pattern}')
        shutil.copy2(sources[0], output / sources[0].name)
    for filename in ('THIRD_PARTY_LICENSES.txt',):
        shutil.copy2(ROOT / filename, output / filename)
    shutil.copy2(ROOT / 'docs/DEPENDENCY_LICENSES.json', output / 'DEPENDENCY_LICENSES.json')
    manifest = dict(version=release_version, commit=commit,
                    files={path.name: sha256(path) for path in sorted(output.iterdir()) if path.is_file()})
    (output / 'release-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    (output / 'SHA256SUMS').write_text(''.join(f'{digest}  {name}\n' for name, digest in manifest['files'].items()))
    print(f'Assembled {release_version}: {len(manifest["files"])} files')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    stage = commands.add_parser('stage-target')
    stage.add_argument('--tag', required=True, choices=[item['tag'] for item in TARGETS])
    collect = commands.add_parser('assemble')
    collect.add_argument('--artifacts', type=Path, required=True)
    collect.add_argument('--output', type=Path, required=True)
    collect.add_argument('--commit', required=True)
    args = parser.parse_args()
    if args.command == 'stage-target':
        stage_target(args.tag)
    else:
        assemble(args.artifacts, args.output, args.commit)


if __name__ == '__main__':
    main()
