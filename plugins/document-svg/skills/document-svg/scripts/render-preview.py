"""Validate self-contained SVG before rendering bounded PNG previews."""
import base64
import html
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET

MAX_BYTES = 64 * 1024 * 1024
SVG_NS = 'http://www.w3.org/2000/svg'


def validate_css(value):
    if any(token in value.lower() for token in ('\\', '/*', '@', 'expression(')):
        raise ValueError('unsupported CSS syntax')
    for target in re.findall(r'url\s*\(([^)]*)\)', value, re.I):
        if not re.fullmatch(r'#[\w.:-]+', target.strip().strip('\'"')):
            raise ValueError('external CSS reference')


def validate_svg(data, depth=0):
    if len(data) > MAX_BYTES or depth > 4:
        raise ValueError('SVG size or nesting limit exceeded')
    text = data.decode('utf-8-sig')
    if re.search(r'<!DOCTYPE|<!ENTITY|<\?(?!xml\s)', text, re.I):
        raise ValueError('DTD, entities or processing instructions are not allowed')
    root = ET.fromstring(text)
    if root.tag not in ('svg', '{' + SVG_NS + '}svg'):
        raise ValueError('expected SVG root')
    for element in root.iter():
        name = element.tag.rsplit('}', 1)[-1].lower()
        if name in {'script', 'foreignobject', 'iframe', 'object', 'embed', 'animate',
                    'animatemotion', 'animatetransform', 'set', 'discard'}:
            raise ValueError('active SVG element')
        for key, value in element.attrib.items():
            local = key.rsplit('}', 1)[-1].lower()
            if local.startswith('on') or local == 'base':
                raise ValueError('active SVG attribute')
            if local == 'href':
                target = value.strip()
                if target.startswith('#'):
                    pass
                elif re.fullmatch(r'data:image/(png|jpeg|gif|webp);base64,[a-z0-9+/=\s]*', target, re.I):
                    pass
                elif target.lower().startswith('data:image/svg+xml;base64,'):
                    validate_svg(base64.b64decode(target.split(',', 1)[1], validate=True), depth + 1)
                else:
                    raise ValueError('external SVG reference')
            if local == 'style' or 'url' in value.lower():
                validate_css(value)
        if name == 'style':
            validate_css(''.join(element.itertext()))


def main():
    if len(sys.argv) not in (3, 4):
        raise ValueError('usage: render-preview.sh INPUT OUTPUT [WIDTH]')
    source, output = Path(sys.argv[1]), Path(sys.argv[2])
    width = int(sys.argv[3]) if len(sys.argv) == 4 else 1400
    if not 1 <= width <= 4096:
        raise ValueError('width must be between 1 and 4096')
    binary = os.environ.get('RSVG_CONVERT_BIN') or shutil.which('rsvg-convert')
    if not binary:
        raise ValueError('rsvg-convert is required')
    paths = sorted(source.glob('*.svg')) if source.is_dir() else [source]
    if not paths or len(paths) > 10000:
        raise ValueError('expected 1 to 10000 SVG pages')
    if output.is_symlink() or (output.exists() and (not output.is_dir() or any(output.iterdir()))):
        raise ValueError('output must be a new or empty directory')
    output.mkdir(parents=True, exist_ok=True)
    pages, figures = [], []
    for number, path in enumerate(paths, 1):
        if path.suffix.lower() != '.svg' or not path.is_file():
            raise ValueError('expected regular SVG input')
        with path.open('rb') as stream:
            data = stream.read(MAX_BYTES + 1)
        validate_svg(data)
        png = f'page-{number:04}.png'
        # Render exactly the inspected bytes via stdin, avoiding input races.
        result = subprocess.run(
            [binary, '--format=png', '--width', str(width), '--height', '4096', '--keep-aspect-ratio'],
            input=data, capture_output=True, check=True, timeout=60,
        )
        with (output / png).open('xb') as stream:
            stream.write(result.stdout)
        pages.append(dict(page=number, svg=path.name, png=png))
        figures.append(f'<figure><img src="{png}" alt="Page {number}"><figcaption>{html.escape(path.name)}</figcaption></figure>')
    document = '''<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src 'self'; style-src 'unsafe-inline'">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>Document SVG preview</title>
<style>body{background:#eef2f6;font:16px system-ui;margin:24px}main{max-width:1600px;margin:auto}figure{background:white;padding:16px;margin:20px 0}img{display:block;max-width:100%;height:auto}</style></head><body><main>'''
    with (output / 'index.html').open('x', encoding='utf-8') as stream:
        stream.write(document + ''.join(figures) + '</main></body></html>')
    with (output / 'preview.json').open('x', encoding='utf-8') as stream:
        json.dump(dict(renderer='rsvg-convert', width=width, pages=pages), stream, ensure_ascii=False)
    print(f'rendered {len(pages)} SVG page(s) to {output}')


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, ET.ParseError, subprocess.SubprocessError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
