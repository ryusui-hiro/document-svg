"""Audit locked dependencies and generate redistributable Rust license notices.

Run in the Python verification environment after installing the local package:
python scripts/audit-licenses.py --cargo-deny /path/to/cargo-deny
Uses Cargo.lock, npm's lockfile, installed Python metadata and the exact maturin
version recorded by the installed wheel. No dependency source is modified.
"""
import argparse
import hashlib
import importlib.metadata as metadata
import json
from pathlib import Path
import re
import subprocess
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
JS_LICENSES = {'MIT', 'Apache-2.0', 'ISC', '0BSD', 'Python-2.0'}
PY_LICENSES = {'MIT', 'BSD-2-Clause', 'BSD-3-Clause', 'Apache-2.0',
               'Apache-2.0 OR BSD-2-Clause', 'MIT OR Apache-2.0'}
AFM_REVISION = '0675784d24b28a55c607cad6b74596ce19ce333c'
AFM_TABLES = {'HELVETICA': 'Helvetica', 'HELVETICA_BOLD': 'Helvetica-Bold',
              'TIMES_ROMAN': 'Times-Roman', 'TIMES_BOLD': 'Times-Bold',
              'TIMES_ITALIC': 'Times-Italic', 'TIMES_BOLD_ITALIC': 'Times-BoldItalic',
              'SYMBOL': 'Symbol', 'ZAPF_DINGBATS': 'ZapfDingbats'}


def run_json(arguments):
    return json.loads(subprocess.check_output(arguments, cwd=ROOT, text=True))


def license_files(package):
    directory = Path(package['manifest_path']).parent
    files = {path for path in directory.iterdir()
             if path.is_file() and re.match(r'(?i)^(licen[sc]e|copying|copyright|notice|authors)', path.name)}
    if package.get('license_file'):
        files.add(directory / package['license_file'])
    for subdirectory in ('licenses', 'LICENSES'):
        folder = directory / subdirectory
        if folder.is_dir():
            files.update(path for path in folder.rglob('*') if path.is_file())
    notices = []
    for path in sorted(files):
        if path.stat().st_size > 512 * 1024:
            raise ValueError(f'{package["name"]}: license file requires manual size review')
        notices.append((path.relative_to(directory).as_posix(), path.read_text(encoding='utf-8')))
    if package['name'] == 'jpeg-encoder':
        # The mandatory IJG notice is in the implementation header, not in
        # the crate's root MIT/Apache files.
        source = (directory / 'src/fdct.rs').read_text(encoding='utf-8')
        header = source.split('*/', 1)[0] + '*/\n'
        if 'Independent JPEG Group' not in header:
            raise ValueError('JPEG implementation notice changed; review required')
        notices.append(('src/fdct.rs (license header)', header))
    if not notices and package.get('repository') == 'https://github.com/napi-rs/napi-rs':
        # These published workspace crates omit the repository-level LICENSE.
        # Recover it from the exact source commit recorded in their archive.
        vcs = json.loads((directory / '.cargo_vcs_info.json').read_text())
        commit = vcs['git']['sha1']
        if not re.fullmatch(r'[a-f0-9]{40}', commit):
            raise ValueError('unexpected upstream source revision')
        url = f'https://raw.githubusercontent.com/napi-rs/napi-rs/{commit}/LICENSE'
        with urllib.request.urlopen(url, timeout=30) as response:
            data = response.read(64 * 1024 + 1)
        if len(data) > 64 * 1024:
            raise ValueError('upstream license size requires manual review')
        text = data.decode('utf-8')
        if 'Permission is hereby granted' not in text:
            raise ValueError('upstream NAPI license requires manual review')
        notices.append((url, text))
    if not notices:
        raise ValueError(f'{package["name"]}: no bundled license text found')
    return notices


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cargo-deny', default='cargo-deny')
    parser.add_argument('--check', action='store_true', help='verify committed notices without rewriting them')
    args = parser.parse_args()
    subprocess.run([str(Path(args.cargo_deny).resolve()) if '/' in args.cargo_deny else args.cargo_deny,
                    '--workspace', '--locked', 'check', 'licenses'], cwd=ROOT, check=True)
    cargo = run_json(['cargo', 'metadata', '--locked', '--all-features', '--format-version', '1'])
    inventory, texts = [], {}
    for package in sorted(cargo['packages'], key=lambda value: (value['name'], value['version'])):
        if not package['source']:
            continue
        if not package['source'].startswith('registry+'):
            raise ValueError(f'{package["name"]}: non-registry dependency requires manual review')
        record = dict(name=package['name'], version=package['version'], license=package['license'],
                      source=f'https://crates.io/crates/{package["name"]}/{package["version"]}',
                      notice_files=[])
        for file, text in license_files(package):
            digest = hashlib.sha256(text.encode()).hexdigest()
            group = texts.setdefault(digest, dict(text=text, packages=[]))
            group['packages'].append(f'{package["name"]} {package["version"]} — {file}')
            record['notice_files'].append(dict(path=file, sha256=digest))
        inventory.append(record)

    afm_notice = (ROOT / 'licenses/Adobe-Core14-AFM.txt').read_text(encoding='utf-8')
    source = (ROOT / 'src/pdf_base14.rs').read_text(encoding='utf-8')
    afm_records = []
    for table, font in AFM_TABLES.items():
        url = f'https://raw.githubusercontent.com/tecnickcom/tc-font-core14-afms/{AFM_REVISION}/{font}.afm'
        with urllib.request.urlopen(url, timeout=30) as response:
            data = response.read(2 * 1024 * 1024 + 1)
        if len(data) > 2 * 1024 * 1024:
            raise ValueError('unexpected AFM source size')
        original = data.decode('utf-8')
        expected = {name: int(width) for width, name in re.findall(r'WX\s+(\d+)\s*;\s*N\s+(\S+)\s*;', original)}
        body = re.search(r'static ' + table + r':.*?= &\[(.*?)\];', source, re.S)[1]
        actual = {name: int(width) for name, width in re.findall(r'\("([^"]+)",\s*(\d+)\)', body)}
        if actual != expected:
            raise ValueError(f'{font}: Rust widths differ from the attributed AFM source')
        for line in original.splitlines():
            if line.startswith(('Comment Copyright', 'Notice Copyright')) and line not in afm_notice:
                raise ValueError(f'{font}: missing original copyright/trademark notice')
        afm_records.append(dict(font=font, source=url, sha256=hashlib.sha256(data).hexdigest(), glyph_count=len(actual)))
    texts[hashlib.sha256(afm_notice.encode()).hexdigest()] = dict(
        text=afm_notice, packages=['Adobe Core 14 AFM-derived widths — src/pdf_base14.rs'])

    node_lock = json.loads((ROOT / 'bindings/node/package-lock.json').read_text())
    node = []
    for path, package in sorted(node_lock['packages'].items()):
        if not path:
            continue
        license = package.get('license')
        if license not in JS_LICENSES:
            raise ValueError(f'{path}: unreviewed npm license {license!r}')
        node.append(dict(name=path.rsplit('node_modules/', 1)[-1], version=package['version'],
                         license=license, scope='development' if package.get('dev') else 'runtime',
                         optional=bool(package.get('optional'))))
    python = []
    for distribution in sorted(metadata.distributions(), key=lambda dist: dist.metadata['Name'].lower()):
        name = distribution.metadata['Name']
        if name == 'document-svg':
            continue
        license = distribution.metadata.get('License-Expression') or distribution.metadata.get('License')
        if license not in PY_LICENSES:
            raise ValueError(f'{name}: unreviewed Python license {license!r}')
        python.append(dict(name=name, version=distribution.version, license=license,
                           scope='build frontend' if name == 'pip' else 'test'))
    wheel = metadata.distribution('document-svg').read_text('WHEEL') or ''
    match = re.search(r'Generator: maturin \(([^)]+)\)', wheel)
    if not match:
        raise ValueError('install a locally built wheel to identify the maturin build version')
    version = match[1]
    if not re.fullmatch(r'[0-9.]+', version):
        raise ValueError('unexpected maturin version')
    with urllib.request.urlopen(f'https://pypi.org/pypi/maturin/{version}/json', timeout=30) as response:
        info = json.load(response)['info']
    license = info.get('license_expression') or info.get('license')
    if license not in PY_LICENSES:
        raise ValueError(f'maturin: unreviewed license {license!r}')
    python.append(dict(name='maturin', version=version, license=license, scope='build backend'))

    header = '''Third-party Rust dependency and font-metric notices

Generated from Cargo.lock by scripts/audit-licenses.py. Covers all workspace
dependencies, including build/test and target-specific packages; some entries
are not linked into a particular distribution. Original license alternatives
are retained below. The permissive selection policy is in deny.toml; including
an alternative license text does not select that alternative.

This software is based in part on the work of the Independent JPEG Group.

'''
    sections = []
    for digest, group in sorted(texts.items(), key=lambda item: item[1]['packages'][0]):
        sections.append('\n'.join(group['packages']) + f'\nText SHA-256: {digest}\n\n' + group['text'].rstrip() + '\n')
    bundle = header + ('\n' + '=' * 72 + '\n\n').join(sections)
    for directory in (ROOT, ROOT / 'bindings/node', ROOT / 'bindings/python', ROOT / 'bindings/python/legal'):
        destination = directory / 'THIRD_PARTY_LICENSES.txt'
        if args.check:
            if destination.read_text(encoding='utf-8') != bundle:
                raise ValueError(f'{destination.relative_to(ROOT)} needs regeneration')
        else:
            destination.write_text(bundle, encoding='utf-8')
    data = dict(schema_version=1, rust_policy='deny.toml', rust=inventory, node=node, python=python, font_metrics=afm_records,
                lockfiles={path: hashlib.sha256((ROOT / path).read_bytes()).hexdigest()
                           for path in ('Cargo.lock', 'bindings/node/package-lock.json')})
    destination = ROOT / 'docs/DEPENDENCY_LICENSES.json'
    if args.check:
        committed = json.loads(destination.read_text(encoding='utf-8'))
        # Test/frontend Python tool versions can differ across CI hosts;
        # their current license metadata was checked above independently.
        for key in ('rust', 'node', 'font_metrics', 'lockfiles'):
            if committed.get(key) != data[key]:
                raise ValueError(f'dependency inventory {key} needs regeneration')
    else:
        destination.write_text(json.dumps(data, indent=2, ensure_ascii=False) + '\n', encoding='utf-8')
    print(f'Checked {len(inventory)} external Rust packages, {len(node)} npm lock entries and {len(python)} Python tools/packages.')
    print(f'Generated {len(texts)} distinct license texts, {len(bundle.encode())} bytes per bundle.')


if __name__ == '__main__':
    main()
