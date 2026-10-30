"""Check that every workflow action is SHA-pinned and allowed by the repository.

GitHub refuses a run whose actions are outside the repository's "Allow select
actions" list before any job starts. That startup failure is not reported as a
pull request check, so a Dependabot bump to an unlisted SHA looks green until it
reaches main. This script makes that mismatch fail the publication check instead.
It also fails on allowlist entries no workflow uses, so a bump replaces the old
pin rather than leaving it allowed.

Pass --remote to also compare .github/allowed-actions.txt with the live
repository setting through the GitHub CLI (needs repository admin access).
"""
import argparse
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
ALLOWLIST = ROOT / '.github/allowed-actions.txt'
USES = re.compile(r'^\s*(?:-\s*)?uses:\s*["\']?([^\s"\'#]+)')
PINNED = re.compile(r'^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}$')


def allowed_patterns():
    return {line.strip() for line in ALLOWLIST.read_text(encoding='utf-8').splitlines()
            if line.strip() and not line.lstrip().startswith('#')}


def workflow_files():
    github = ROOT / '.github'
    return sorted([*github.glob('workflows/*.yml'), *github.glob('workflows/*.yaml'),
                   *github.glob('actions/**/action.yml'), *github.glob('actions/**/action.yaml')])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--remote', action='store_true',
                        help='also compare the allowlist file with the repository setting')
    args = parser.parse_args()
    allowed = allowed_patterns()
    errors, used = [], set()
    for path in workflow_files():
        relative = path.relative_to(ROOT)
        for number, line in enumerate(path.read_text(encoding='utf-8').splitlines(), 1):
            match = USES.match(line)
            if not match:
                continue
            action = match[1]
            if action.startswith('./') or action.startswith('docker://'):
                continue
            used.add(action)
            if not PINNED.match(action):
                errors.append(f'{relative}:{number}: {action} is not pinned to a full commit SHA')
            elif action not in allowed:
                errors.append(f'{relative}:{number}: {action} is not in .github/allowed-actions.txt; '
                              'add it to the repository Actions allowlist and to that file')
    for pattern in sorted(allowed - used):
        errors.append(f'{pattern} is in .github/allowed-actions.txt but no workflow uses it; '
                      'remove it from that file and from the repository Actions allowlist')
    if args.remote:
        repository = subprocess.check_output(
            ['gh', 'repo', 'view', '--json', 'nameWithOwner', '--jq', '.nameWithOwner'],
            cwd=ROOT, text=True).strip()
        live = set(json.loads(subprocess.check_output(
            ['gh', 'api', f'repos/{repository}/actions/permissions/selected-actions'],
            cwd=ROOT, text=True))['patterns_allowed'])
        for pattern in sorted(allowed - live):
            errors.append(f'{pattern} is listed in .github/allowed-actions.txt but not allowed by {repository}')
        for pattern in sorted(live - allowed):
            errors.append(f'{pattern} is allowed by {repository} but missing from .github/allowed-actions.txt')
    for error in errors:
        print(f'FAIL: {error}')
    print(f'Checked {len(used)} distinct actions against {len(allowed)} allowed patterns; {len(errors)} findings.')
    sys.exit(bool(errors))


if __name__ == '__main__':
    main()
