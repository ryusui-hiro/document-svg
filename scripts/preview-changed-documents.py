#!/usr/bin/env python3
"""Convert changed documents to SVG pages and summarise the conversion warnings.

Used by the document preview workflow so a pull request that touches a PDF or
Office file carries reviewable SVG pages and an explicit warning report.
"""

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

SUPPORTED = {'.pdf', '.pptx', '.xlsx', '.docx', '.drawio', '.dio'}


def changed_documents(base, head):
    diff = subprocess.run(
        ['git', 'diff', '--name-only', '--diff-filter=ACMR', f'{base}...{head}'],
        check=True, capture_output=True, text=True)
    paths = []
    for line in diff.stdout.splitlines():
        candidate = Path(line.strip())
        if candidate.suffix.lower() in SUPPORTED and candidate.is_file():
            paths.append(candidate)
    return sorted(paths)


def slug(path):
    return str(path).replace(os.sep, '__').replace(' ', '_')


def convert(docsvg, document, output_root):
    destination = output_root / slug(document)
    result = subprocess.run([docsvg, str(document), '--output', str(destination)],
                            capture_output=True, text=True)
    if result.returncode != 0:
        return {'document': str(document), 'ok': False,
                'error': (result.stderr or result.stdout).strip()[:2000]}

    report = destination / 'conversion.json'
    if not report.is_file():
        return {'document': str(document), 'ok': False, 'error': 'conversion.json was not written'}

    data = json.loads(report.read_text(encoding='utf-8'))
    pages = data.get('pages') or []
    warnings = list(data.get('warnings') or [])
    page_warnings = sum(int(page.get('warning_count') or 0) for page in pages)
    return {'document': str(document), 'ok': True, 'output': str(destination),
            'source_format': data.get('source_format') or data.get('format') or 'unknown',
            'page_count': data.get('page_count', len(pages)),
            'warnings': warnings, 'page_warning_count': page_warnings}


def render_markdown(results, artifact_name):
    if not results:
        return 'No PDF, PPTX, XLSX, DOCX or draw.io files changed in this pull request.\n'

    lines = ['### Document SVG preview', '',
             '| Document | Format | Pages | Warnings |', '|---|---|---:|---:|']
    for item in results:
        if not item['ok']:
            lines.append(f'| `{item["document"]}` | — | — | conversion failed |')
            continue
        total = len(item['warnings']) + item['page_warning_count']
        lines.append(f'| `{item["document"]}` | {item["source_format"]} | '
                     f'{item["page_count"]} | {total} |')

    failures = [item for item in results if not item['ok']]
    if failures:
        lines += ['', '<details><summary>Conversion failures</summary>', '']
        for item in failures:
            lines += [f'**{item["document"]}**', '', '```', item['error'], '```', '']
        lines.append('</details>')

    detailed = [item for item in results if item['ok'] and item['warnings']]
    if detailed:
        lines += ['', '<details><summary>Top-level warnings</summary>', '']
        for item in detailed:
            lines.append(f'**{item["document"]}**')
            lines += [f'- {warning}' for warning in item['warnings'][:20]]
            lines.append('')
        lines.append('</details>')

    lines += ['', f'SVG pages are attached to this run as the `{artifact_name}` artifact.',
              'Warnings mean the conversion is not a guaranteed visual reproduction; review the pages before relying on them.', '']
    return '\n'.join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', required=True)
    parser.add_argument('--head', default='HEAD')
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--docsvg', default=os.environ.get('DOCSVG_BIN', 'docsvg'))
    parser.add_argument('--artifact-name', default='document-svg-preview')
    parser.add_argument('--summary', type=Path, help='write the markdown report here')
    parser.add_argument('--max-documents', type=int, default=20)
    arguments = parser.parse_args()

    documents = changed_documents(arguments.base, arguments.head)
    truncated = documents[arguments.max_documents:]
    documents = documents[:arguments.max_documents]

    arguments.output.mkdir(parents=True, exist_ok=True)
    results = [convert(arguments.docsvg, document, arguments.output) for document in documents]

    markdown = render_markdown(results, arguments.artifact_name)
    if truncated:
        markdown += f'\nOnly the first {arguments.max_documents} changed documents were converted.\n'

    if arguments.summary:
        arguments.summary.write_text(markdown, encoding='utf-8')
    step_summary = os.environ.get('GITHUB_STEP_SUMMARY')
    if step_summary:
        with open(step_summary, 'a', encoding='utf-8') as stream:
            stream.write(markdown)

    github_output = os.environ.get('GITHUB_OUTPUT')
    if github_output:
        with open(github_output, 'a', encoding='utf-8') as stream:
            stream.write(f'document-count={len(documents)}\n')

    print(markdown)
    return 1 if any(not item['ok'] for item in results) else 0


if __name__ == '__main__':
    sys.exit(main())
