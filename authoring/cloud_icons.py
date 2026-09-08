#!/usr/bin/env python3
"""Official cloud icon catalog and deterministic diagram authoring, independent of docsvg.

Python 3.10+, standard library only. See docs/CLOUD_ARCHITECTURE.md.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import html
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import sys
import tempfile
import subprocess
import unicodedata
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))
DEFAULT_CACHE = ROOT / '.cache' / 'cloud-icons'
SOURCES = Path(__file__).with_name('icon-sources.json')
SVG_NS = 'http://www.w3.org/2000/svg'
MAX_ARCHIVE = 128 * 1024 * 1024
MAX_ICON = 2 * 1024 * 1024
MAX_EXPANDED = 128 * 1024 * 1024
ALIASES = {
    'aks': 'kubernetes', 'gke': 'gke', 'gce': 'compute engine',
    's3': 's3', 'lambda': 'lambda', '関数': 'function', 'ストレージ': 'storage',
    '仮想マシン': 'virtual machines', 'データベース': 'database',
    'k8s': 'kubernetes', 'google kubernetes engine': 'gke',
    'azure functions': 'function apps', 'functions': 'function apps',
}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read_bounded(path, limit):
    with Path(path).open('rb') as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f'file exceeds {limit} bytes: {path}')
    return data


def load_json(path, limit=16 * 1024 * 1024):
    return json.loads(read_bounded(path, limit))


def atomic_write(path, data):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        stream.write(data)
    try:
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def json_bytes(value):
    return (json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False) + '\n').encode()


def slug(value):
    return re.sub(r'[^a-z0-9]+', '-', value.lower()).strip('-')


def validate_svg(data):
    """Accept passive, self-contained vector icons; never rewrite vendor SVGs."""
    if len(data) > MAX_ICON:
        raise ValueError('icon exceeds size limit')
    source = data.decode('utf-8-sig')
    if re.search(r'<!DOCTYPE|<!ENTITY|<\?(?!xml\s)', source, re.I):
        raise ValueError('DTD, entities and processing instructions are not allowed')
    root = ET.fromstring(source)
    if root.tag != f'{{{SVG_NS}}}svg':
        raise ValueError('expected SVG namespace and root')
    for element in root.iter():
        name = element.tag.rsplit('}', 1)[-1].lower()
        if name in {'script', 'foreignobject', 'iframe', 'object', 'embed', 'animate',
                    'animatetransform', 'animatemotion', 'set', 'discard', 'image', 'feimage'}:
            raise ValueError(f'unsupported icon element: {name}')
        values = []
        for key, value in element.attrib.items():
            local = key.rsplit('}', 1)[-1].lower()
            if local.startswith('on') or local == 'base':
                raise ValueError(f'active/base attribute: {local}')
            if local == 'href' and not value.strip().startswith('#'):
                raise ValueError('external reference')
            if local == 'style':
                values.append(value)
            elif re.search(r'url\s*\(', value, re.I):
                values.append(value)
        if name == 'style':
            values.append(''.join(element.itertext()))
        for value in values:
            # Reject CSS escapes/comments to avoid obfuscated external references.
            if any(token in value.lower() for token in ('\\', '/*', '@', 'javascript:', 'expression(')):
                raise ValueError('unsupported CSS in icon')
            for target in re.findall(r'url\s*\(([^)]*)\)', value, re.I):
                if not target.strip().strip('\'"').startswith('#'):
                    raise ValueError('external CSS reference')
    box = root.get('viewBox')
    if box:
        parts = [float(v) for v in re.split(r'[\s,]+', box.strip())]
        if len(parts) != 4 or not all(math.isfinite(v) for v in parts) or min(parts[2:]) <= 0:
            raise ValueError('invalid viewBox')
        return parts
    def dimension(key):
        value = root.get(key, '')
        match = re.fullmatch(r'(\d+(?:\.\d+)?)(?:px)?', value)
        if not match or float(match[1]) <= 0:
            raise ValueError(f'icon needs viewBox or positive pixel {key}')
        return float(match[1])
    return [0, 0, dimension('width'), dimension('height')]


def identity(source, path):
    parts = PurePosixPath(path).parts
    stem = PurePosixPath(path).stem
    provider, kind = source['provider'], source['kind']
    size = 0
    category = parts[-2] if len(parts) > 1 else 'general'
    if provider == 'azure':
        match = re.fullmatch(r'(\d+)-icon-service-(.+)', stem)
        name = match[2] if match else stem
        ident = slug(name) + ('-' + match[1] if match else '')
    elif provider == 'aws':
        kind = ('group' if 'Group-Icons' in parts[0] else
                'category' if 'Category-Icons' in parts[0] else
                'resource' if 'Resource-Icons' in parts[0] else 'service')
        name = re.sub(r'^(?:Arch[-_]Category|Arch|Res|Category)_', '', stem)
        match = re.search(r'_(\d+)(?:_(Dark|Light))?$', name, re.I)
        if match:
            size = int(match[1])
            name = name[:match.start()] + ('-' + match[2] if match[2] else '')
        ident = slug(name)
        category = next((p for p in parts[1:-1] if not p.isdigit()), kind)
    else:
        name = parts[-3] if len(parts) >= 3 and parts[-2] == 'SVG' else stem
        ident = slug(name)
        category = 'core products' if kind == 'service' else 'general categories'
    return f'{provider}/{kind}/{ident}', name.replace('_', ' ').replace('-', ' '), category, size


def index_archive(source, archive, cache):
    """Extract selected SVG bytes to content-addressed paths, never ZIP entry paths."""
    records = {}
    rejected = []
    total = 0
    with zipfile.ZipFile(archive) as bundle:
        entries = bundle.infolist()
        if len(entries) > 30000:
            raise ValueError('too many ZIP entries')
        for info in sorted(entries, key=lambda item: item.filename):
            path = PurePosixPath(info.filename)
            if (not info.filename.lower().endswith('.svg') or '__MACOSX' in path.parts
                    or path.name.startswith('._')):
                continue
            if path.is_absolute() or '..' in path.parts or '\\' in info.filename:
                raise ValueError('unsafe ZIP entry path')
            total += info.file_size
            if info.file_size > MAX_ICON or total > MAX_EXPANDED:
                raise ValueError('expanded icon size limit exceeded')
            with bundle.open(info) as stream:
                data = stream.read(MAX_ICON + 1)
            try:
                box = validate_svg(data)
            except (ValueError, ET.ParseError) as error:
                rejected.append({'source_path': info.filename, 'reason': str(error)})
                continue
            ident, name, category, size = identity(source, info.filename)
            sha = digest(data)
            relative = f'assets/{sha}.svg'
            destination = cache / relative
            if not destination.exists() or digest(read_bounded(destination, MAX_ICON)) != sha:
                atomic_write(destination, data)
            item = dict(id=ident, provider=source['provider'], kind=ident.split('/')[1],
                        name=name, category=category, path=relative, sha256=sha,
                        view_box=box, source_key=source['key'], source_path=info.filename,
                        source_page=source['source_page'], release=source['release'], size=size)
            previous = records.get(ident)
            variants = ([] if previous is None else previous['variants']) + [
                dict(source_path=info.filename, sha256=sha, category=category, size=size)]
            # Prefer the largest official vector variant; path tie-break is deterministic.
            if previous is None or (size, info.filename) > (previous['size'], previous['source_path']):
                records[ident] = item
            records[ident]['variants'] = variants
            records[ident]['categories'] = sorted({variant['category'] for variant in variants})
    if not records:
        raise ValueError(f'no usable SVG icons in {source["key"]}')
    return list(records.values()), rejected


def fetch_catalog(cache, archive_dir=None):
    registry = load_json(SOURCES)
    records, sources = [], []
    for source in registry['sources']:
        archive = cache / 'archives' / (source['sha256'] + '.zip')
        if archive_dir:
            data = read_bounded(Path(archive_dir) / (source['key'] + '.zip'), MAX_ARCHIVE)
        elif archive.exists():
            data = read_bounded(archive, MAX_ARCHIVE)
        else:
            request = urllib.request.Request(source['url'], headers={'User-Agent': 'docsvg-cloud-icons/1'})
            with urllib.request.urlopen(request, timeout=60) as response:
                if not response.url.startswith('https://'):
                    raise ValueError('icon download redirected away from HTTPS')
                data = response.read(MAX_ARCHIVE + 1)
            if len(data) > MAX_ARCHIVE:
                raise ValueError('download exceeds archive size limit')
        if digest(data) != source['sha256']:
            raise ValueError(f'{source["key"]}: archive checksum changed; review the official release and update icon-sources.json')
        atomic_write(archive, data)
        items, rejected = index_archive(source, archive, cache)
        records.extend(items)
        sources.append(dict(source, imported=len(items), rejected=rejected))
    records.sort(key=lambda item: item['id'])
    if len({item['id'] for item in records}) != len(records):
        raise ValueError('duplicate IDs across sources')
    catalog = dict(schema_version=1, verified_on=registry['verified_on'], sources=sources, icons=records)
    atomic_write(cache / 'catalog.json', json_bytes(catalog))
    return catalog


def catalog_at(cache):
    catalog = load_json(cache / 'catalog.json')
    if catalog.get('schema_version') != 1:
        raise ValueError('unsupported catalog version')
    return catalog


def search(catalog, query, provider=None, limit=20):
    query = query.casefold().strip()
    normalized = ALIASES.get(query, query)
    words = re.findall(r'\w+', normalized)
    matches = []
    for item in catalog['icons']:
        if provider and item['provider'] != provider:
            continue
        haystack = ' '.join(str(item.get(k, '')) for k in ('id', 'name', 'categories', 'category')).casefold()
        searchable = ' '.join(re.findall(r'\w+', haystack.replace('-', ' ')))
        if all(word in searchable for word in words):
            score = (100 if query == item['id'] or normalized == item['name'].casefold() else 0)
            score += 10 if item['kind'] == 'service' else 0
            score += 5 if normalized in item['name'].casefold() else 0
            score += 3 if item['name'].casefold().startswith(normalized) else 0
            matches.append((score, item))
    return [item for _, item in sorted(matches, key=lambda pair: (-pair[0], pair[1]['id']))[:limit]]


def resolve(catalog, cache, ident):
    matches = [item for item in catalog['icons'] if item['id'] == ident]
    if len(matches) != 1:
        raise ValueError(f'unknown or ambiguous icon ID: {ident}; run search first')
    item = matches[0]
    path = (cache / item['path']).resolve()
    if not path.is_relative_to(cache.resolve()):
        raise ValueError('icon path escapes catalog')
    data = read_bounded(path, MAX_ICON)
    if digest(data) != item['sha256']:
        raise ValueError(f'icon checksum mismatch: {ident}')
    validate_svg(data)
    return item, 'data:image/svg+xml;base64,' + base64.b64encode(data).decode('ascii')


def text(value, maximum=200):
    if not isinstance(value, str) or not value.strip() or len(value) > maximum:
        raise ValueError(f'text must contain 1..{maximum} characters')
    if any(ord(c) < 32 and c not in '\n\t' for c in value):
        raise ValueError('control character in text')
    return html.escape(value, quote=True)


def wrap_label(value, width=26):
    lines, line, units = [], '', 0
    for char in value:
        size = 2 if unicodedata.east_asian_width(char) in 'WF' else 1
        if char == '\n' or units + size > width:
            lines.append(line.rstrip())
            line, units = '', 0
            if char == '\n':
                continue
        line += char
        units += size
    if line:
        lines.append(line.rstrip())
    return lines


def render_diagram(spec, catalog, cache):
    """Fixed grid with explicit connections. No inferred service relationships."""
    if isinstance(spec, dict) and spec.get('schema_version') == 2:
        from authoring.architecture import render
        return render(spec, catalog, cache, resolve)
    unknown = set(spec) - {'schema_version', 'title', 'nodes', 'edges'}
    if unknown or spec.get('schema_version') != 1:
        raise ValueError(f'unsupported diagram schema/fields: {sorted(unknown)}')
    title = text(spec.get('title'), 100)
    nodes, edges = spec.get('nodes', []), spec.get('edges', [])
    if not isinstance(nodes, list) or not 1 <= len(nodes) <= 64 or not isinstance(edges, list) or len(edges) > 128:
        raise ValueError('diagram requires 1..64 nodes and at most 128 edges')
    positions, cells, cards, used = {}, set(), [], {}
    for node in nodes:
        if set(node) - {'id', 'icon', 'label', 'row', 'column'}:
            raise ValueError('unknown node fields')
        ident = node.get('id', '')
        if not isinstance(ident, str) or not re.fullmatch(r'[A-Za-z][A-Za-z0-9_-]{0,63}', ident) or ident in positions:
            raise ValueError('node IDs must be unique ASCII identifiers')
        row, column = node.get('row'), node.get('column')
        if type(row) is not int or type(column) is not int or not 0 <= row <= 15 or not 0 <= column <= 7:
            raise ValueError('row must be 0..15 and column 0..7')
        if (row, column) in cells:
            raise ValueError('nodes overlap in the same grid cell')
        cells.add((row, column))
        item, uri = resolve(catalog, cache, node['icon'])
        label = node.get('label', item['name'])
        text(label, 100)
        lines = wrap_label(label)
        if len(lines) > 2:
            raise ValueError(f'label too long for card: {ident}; use a shorter label')
        x, y = 40 + column * 320, 100 + row * 210
        positions[ident] = (x, y)
        used[item['id']] = item
        parts = [f'<g id="node-{ident}"><title>{text(label)}</title>',
                 f'<rect x="{x}" y="{y}" width="230" height="142" rx="12" fill="white" stroke="#b9c8d8"/>',
                 f'<text x="{x+16}" y="{y+23}" font-size="11" fill="#53657a">{item["provider"].upper()} · {item["kind"]}</text>',
                 f'<image href="{uri}" x="{x+83}" y="{y+32}" width="64" height="64" preserveAspectRatio="xMidYMid meet"/>']
        for index, line in enumerate(lines):
            parts.append(f'<text x="{x+115}" y="{y+115+18*index}" text-anchor="middle" font-size="14" fill="#192c42">{text(line)}</text>')
        cards.append('\n'.join(parts) + '</g>')
    width = max(x for x, _ in positions.values()) + 270
    title_units = sum(2 if unicodedata.east_asian_width(c) in 'WF' else 1 for c in spec['title'])
    width = max(width, 80 + title_units * 16)
    height = max(y for _, y in positions.values()) + 185
    connectors = []
    warnings = []
    for edge in edges:
        if set(edge) - {'from', 'to', 'label'}:
            raise ValueError('unknown edge fields')
        start, end = edge.get('from'), edge.get('to')
        if start not in positions or end not in positions or start == end:
            raise ValueError('edge endpoints must reference two different existing nodes')
        x1, y1 = positions[start]
        x2, y2 = positions[end]
        if y1 == y2:
            direction = 1 if x2 > x1 else -1
            a = (x1 + (230 if direction > 0 else 0), y1 + 71)
            b = (x2 + (0 if direction > 0 else 230), y2 + 71)
        elif x1 == x2:
            direction = 1 if y2 > y1 else -1
            a = (x1 + 115, y1 + (142 if direction > 0 else 0))
            b = (x2 + 115, y2 + (0 if direction > 0 else 142))
        else:
            raise ValueError('edges must share a row or column; use explicit layout to keep routing readable')
        for other, (x, y) in positions.items():
            if other in (start, end):
                continue
            if ((a[1] == b[1] and y < a[1] < y+142 and min(a[0], b[0]) < x+230 and max(a[0], b[0]) > x)
                or (a[0] == b[0] and x < a[0] < x+230 and min(a[1], b[1]) < y+142 and max(a[1], b[1]) > y)):
                raise ValueError(f'edge {start}->{end} crosses node {other}; adjust grid positions')
        connectors.append(f'<path d="M {a[0]} {a[1]} L {b[0]} {b[1]}" fill="none" stroke="#64768b" stroke-width="2" marker-end="url(#arrow)"/>')
        if edge.get('label'):
            label = text(edge['label'], 24)
            if sum(2 if unicodedata.east_asian_width(c) in 'WF' else 1 for c in edge['label']) > 12:
                raise ValueError('edge label exceeds 12 display units; shorten it')
            mx, my = (a[0]+b[0])/2, (a[1]+b[1])/2
            connectors.append(f'<text x="{mx+8 if a[0]==b[0] else mx}" y="{my-10}" text-anchor="{("start" if a[0]==b[0] else "middle")}" font-size="11" fill="#53657a">{label}</text>')
    for item in used.values():
        if item['kind'] == 'category':
            warnings.append(f'{item["id"]}: category icon shared by multiple products; verify the product label against vendor guidance')
    report = dict(schema_version=1, title=spec['title'], node_count=len(nodes), edge_count=len(edges),
                  warnings=warnings, icons=[used[key] for key in sorted(used)])
    metadata = html.escape(json.dumps(report, ensure_ascii=False), quote=False)
    svg = '\n'.join([
        '<?xml version="1.0" encoding="UTF-8"?>',
        f'<svg xmlns="{SVG_NS}" width="{width}px" height="{height}px" viewBox="0 0 {width} {height}" font-family="Arial, Noto Sans CJK JP, sans-serif">',
        f'<title>{title}</title><desc>Architecture diagram using official cloud icons. Connections are specified by the author.</desc>',
        f'<metadata>{metadata}</metadata>',
        '<defs><marker id="arrow" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z" fill="#64768b"/></marker></defs>',
        f'<rect width="{width}" height="{height}" fill="#f3f6fa"/>',
        f'<text x="40" y="55" font-size="26" font-weight="bold" fill="#192c42">{title}</text>',
        *connectors, *cards, '</svg>', ''])
    return svg, report


def gallery(catalog, cache, output):
    """Self-contained searchable index; image URLs isolate vendor CSS and IDs."""
    entries = []
    for item in catalog['icons']:
        _, uri = resolve(catalog, cache, item['id'])
        label = text(item['name'])
        ident = text(item['id'])
        searchable = html.escape(' '.join([item['id'], item['name'], item['category']]).lower(), quote=True)
        entries.append(f'<article data-search="{searchable}"><img loading="lazy" src="{uri}" alt=""><h2>{label}</h2><p>{item["provider"].upper()} / {item["kind"]}</p><code>{ident}</code><p><button type="button" data-copy="{ident}" aria-label="Copy ID for {label}">Copy ID</button> <a href="{uri}" download="{ident.replace("/", "-")}.svg">SVG</a></p><a href="{html.escape(item["source_page"],quote=True)}">Official source</a></article>')
    content = '''<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Cloud architecture icons</title><style>body{margin:32px;background:#f3f6fa;color:#192c42;font:15px system-ui}input{padding:12px;width:min(90%,600px);font:inherit}main{display:grid;grid-template-columns:repeat(auto-fill,minmax(240px,1fr));gap:16px;margin-top:24px}article{background:white;border:1px solid #c4d0dd;border-radius:10px;padding:20px;overflow-wrap:anywhere}article[hidden]{display:none}img{width:64px;height:64px;object-fit:contain}h2{font-size:16px}code{font-size:12px}a{color:#174ea6}</style>
<h1>Cloud architecture icons</h1><p>Official assets · exact IDs for diagram authoring · ''' + html.escape(catalog['verified_on']) + ''' snapshot</p>
<label for="search">Search service, provider, or ID</label><br><input id="search" type="search" placeholder="e.g. azure function, aws lambda, gcp cloud run"><p id="count" role="status"></p><p id="copy-status" role="status"></p><main>''' + '\n'.join(entries) + '''</main><script>
const field=document.getElementById('search'),cards=[...document.querySelectorAll('article')],count=document.getElementById('count');
function filter(){const words=field.value.toLowerCase().trim().split(/\\s+/);let n=0;for(const c of cards){c.hidden=!words.every(w=>c.dataset.search.includes(w));if(!c.hidden)n++}count.textContent=n+(n===1?' icon':' icons');}field.addEventListener('input',filter);filter();
document.addEventListener('click',async event=>{const button=event.target.closest('button[data-copy]');if(!button)return;const status=document.getElementById('copy-status');try{await navigator.clipboard.writeText(button.dataset.copy);status.textContent='Copied: '+button.dataset.copy;}catch(error){status.textContent='Clipboard unavailable. Select the displayed ID and copy it.';}});
</script></html>'''
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open('x', encoding='utf-8') as stream:
        stream.write(content)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cache', type=Path, default=DEFAULT_CACHE)
    commands = parser.add_subparsers(dest='command', required=True)
    fetch = commands.add_parser('fetch', help='download pinned official archives and build catalog')
    fetch.add_argument('--archive-dir', type=Path, help='offline directory containing azure.zip, aws.zip, gcp-core.zip, gcp-category.zip')
    find = commands.add_parser('search', help='search names and return exact IDs as JSON')
    find.add_argument('query', nargs='?', default='')
    find.add_argument('--provider', choices=['azure', 'aws', 'gcp'])
    find.add_argument('--limit', type=int, choices=range(1, 201), default=20, metavar='1..200')
    get = commands.add_parser('resolve', help='resolve exact ID; optionally include embedded image URI')
    get.add_argument('id')
    get.add_argument('--embed', action='store_true')
    render = commands.add_parser('render', help='render a diagram JSON to self-contained SVG')
    render.add_argument('input', type=Path)
    render.add_argument('--output', type=Path, required=True)
    render.add_argument('--report', type=Path, help='write geometry, warnings, routes and sources as JSON')
    commands.add_parser('templates', help='list reusable business architecture templates')
    copy = commands.add_parser('template', help='copy a business template for editing')
    copy.add_argument('name')
    copy.add_argument('--output', type=Path, required=True)
    validate = commands.add_parser('validate', help='validate architecture and route geometry without writing SVG')
    validate.add_argument('input', type=Path)
    examples = commands.add_parser('build-examples', help='build Azure/AWS/GCP SVG, PNG, JSON and review HTML')
    examples.add_argument('--output', type=Path, required=True)
    examples.add_argument('--png-width', type=int, default=2400)
    browse = commands.add_parser('gallery', help='write a searchable standalone HTML catalog')
    browse.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == 'templates':
            from authoring.business_bundle import list_templates
            result = list_templates()
        elif args.command == 'template':
            from authoring.business_bundle import template
            spec = template(args.name)
            args.output.parent.mkdir(parents=True, exist_ok=True)
            with args.output.open('x', encoding='utf-8') as stream:
                stream.write(json_bytes(spec).decode())
            result = dict(output=str(args.output), title=spec['title'])
        elif args.command == 'fetch':
            catalog = fetch_catalog(args.cache, args.archive_dir)
            result = dict(catalog=str(args.cache / 'catalog.json'), icons=len(catalog['icons']),
                          sources=[dict(key=s['key'], imported=s['imported'], rejected=s['rejected']) for s in catalog['sources']])
        else:
            catalog = catalog_at(args.cache)
            if args.command == 'build-examples':
                from authoring.business_bundle import build
                result = build(args.output, catalog, args.cache, render_diagram, args.png_width)
            elif args.command == 'validate':
                _, result = render_diagram(load_json(args.input, 1024 * 1024), catalog, args.cache)
            elif args.command == 'search':
                result = search(catalog, args.query, args.provider, args.limit)
            elif args.command == 'resolve':
                item, uri = resolve(catalog, args.cache, args.id)
                result = dict(item, absolute_path=str((args.cache / item['path']).resolve()))
                if args.embed:
                    result['data_uri'] = uri
            elif args.command == 'gallery':
                gallery(catalog, args.cache, args.output)
                result = dict(output=str(args.output), icons=len(catalog['icons']))
            else:
                if args.output.suffix.lower() != '.svg':
                    raise ValueError('diagram output must end in .svg')
                if args.output.exists() or (args.report and args.report.exists()):
                    raise ValueError('SVG or report output already exists')
                if args.report and args.report.resolve() == args.output.resolve():
                    raise ValueError('SVG and report paths must differ')
                svg, report = render_diagram(load_json(args.input, 1024 * 1024), catalog, args.cache)
                if args.report:
                    args.report.parent.mkdir(parents=True, exist_ok=True)
                    with args.report.open('x', encoding='utf-8') as stream:
                        stream.write(json_bytes(report).decode())
                args.output.parent.mkdir(parents=True, exist_ok=True)
                with args.output.open('x', encoding='utf-8') as stream:
                    stream.write(svg)
                result = dict(report, output=str(args.output))
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0
    except (ValueError, OSError, KeyError, TypeError, ET.ParseError, zipfile.BadZipFile, subprocess.SubprocessError) as error:
        print(f'cloud-icons: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
