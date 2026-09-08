"""Compare two release CLIs on generated inputs, verifying output equality.

Usage: python3 scripts/benchmark-conversion.py --before PATH --after PATH
Requires macOS or Linux with /usr/bin/time. No external documents or fonts are
downloaded. All generated inputs and outputs live in an auto-cleaned tempdir.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import statistics
import struct
import subprocess
import sys
import tempfile
import time
import zipfile
import zlib


def png_chunk(kind, data):
    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))


def make_inputs(root, pages):
    slides = root / 'svg-pages'
    slides.mkdir()
    for number in range(pages):
        svg = f'<svg xmlns="http://www.w3.org/2000/svg" width="320" height="180"><rect width="320" height="180" fill="white"/><text x="20" y="90" font-family="sans-serif" font-size="20">Page {number + 1}</text></svg>'
        (slides / f'page-{number + 1:04}.svg').write_text(svg, encoding='utf-8')
    side = 2048
    # A valid large PNG with deliberately uncompressed image data. This
    # exercises the byte-counting path rather than JPEG decoding cost.
    row = b'\0' + bytes([40, 100, 160, 255]) * side
    image = (b'\x89PNG\r\n\x1a\n'
             + png_chunk(b'IHDR', struct.pack('>IIBBBBB', side, side, 8, 6, 0, 0, 0))
             + png_chunk(b'IDAT', zlib.compress(row * side, level=0))
             + png_chunk(b'IEND', b''))
    docx = root / 'image.docx'
    document = '''<w:document xmlns:w="w" xmlns:wp="wp" xmlns:a="a" xmlns:r="r"><w:body><w:p><w:r><w:drawing><wp:inline><wp:extent cx="2540000" cy="2540000"/><wp:docPr id="1" name="Synthetic image"/><a:graphic><a:graphicData><a:blip r:embed="rId1"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>'''
    with zipfile.ZipFile(docx, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
        archive.writestr('word/document.xml', document)
        archive.writestr('word/_rels/document.xml.rels', '<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>')
        archive.writestr('word/media/image1.png', image)
    return slides, docx


def run(binary, arguments):
    if sys.platform == 'darwin':
        prefix = ['/usr/bin/time', '-l']
    elif sys.platform.startswith('linux'):
        prefix = ['/usr/bin/time', '-f', 'MAX_RSS_KIB=%M']
    else:
        raise RuntimeError('benchmark requires macOS or Linux')
    start = time.perf_counter()
    result = subprocess.run(prefix + [str(binary)] + list(map(str, arguments)), capture_output=True, text=True, check=True, timeout=120)
    elapsed = time.perf_counter() - start
    if sys.platform == 'darwin':
        rss = int(re.search(r'(\d+)\s+maximum resident set size', result.stderr)[1])
    else:
        rss = int(re.search(r'MAX_RSS_KIB=(\d+)', result.stderr)[1]) * 1024
    return dict(seconds=elapsed, peak_rss_bytes=rss)


def digest(path):
    with path.open('rb') as stream:
        digest = hashlib.sha256()
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
        return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--before', type=Path, required=True)
    parser.add_argument('--after', type=Path, required=True)
    parser.add_argument('--pages', type=int, default=100)
    parser.add_argument('--repeats', type=int, default=3)
    args = parser.parse_args()
    if not 1 <= args.pages <= 1000 or not 1 <= args.repeats <= 20:
        parser.error('pages must be 1..1000 and repeats 1..20')
    binaries = dict(before=args.before.resolve(), after=args.after.resolve())
    observations = {case: {label: [] for label in binaries} for case in ('reverse', 'forward')}
    hashes, ir_sizes = {}, set()
    with tempfile.TemporaryDirectory(prefix='docsvg-benchmark-') as temporary:
        root = Path(temporary)
        slides, docx = make_inputs(root, args.pages)
        for iteration in range(args.repeats + 1):
            for label, binary in binaries.items():
                pptx = root / f'{label}-{iteration}.pptx'
                output = root / f'{label}-{iteration}-svg'
                reverse = run(binary, ['reverse', slides, '--output', pptx])
                forward = run(binary, [docx, '--output', output])
                report = json.loads((output / 'conversion.json').read_text())
                assert report['page_count'] == 1 and not report['warnings'], report
                ir_sizes.add(report['largest_page_ir_bytes'])
                for case, path in [('reverse', pptx), ('forward', output / 'page-0001.svg')]:
                    value = digest(path)
                    assert hashes.setdefault(case, value) == value, f'{case}: output changed'
                if iteration:  # Exclude first-run warm-up from measurements.
                    observations['reverse'][label].append(reverse)
                    observations['forward'][label].append(forward)
    assert len(ir_sizes) == 1, 'IR size metric changed'
    summary = {case: {label: {key: statistics.median(sample[key] for sample in samples)
                             for key in ('seconds', 'peak_rss_bytes')}
                      for label, samples in values.items()}
               for case, values in observations.items()}
    print(json.dumps(dict(pages=args.pages, repeats=args.repeats, output_identical=True,
                         ir_bytes=next(iter(ir_sizes)), medians=summary), indent=2))


if __name__ == '__main__':
    main()
