"""The pull request preview must find every changed document and post a harmless comment."""

import json
import os
from pathlib import Path
import subprocess
import sys
import textwrap

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/preview-changed-documents.py"
HOSTILE = "![pixel](https://tracker.invalid/p.png) [click](https://evil.invalid) @someone | `x`"


def git(repo, *args):
    subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)


@pytest.fixture
def repo(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    git(repo, "init", "-q")
    git(repo, "config", "user.email", "test@example.invalid")
    git(repo, "config", "user.name", "Test")
    git(repo, "config", "core.quotePath", "true")  # Git's default quoting
    (repo / "README.md").write_text("base\n")
    git(repo, "add", ".")
    git(repo, "commit", "-qm", "base")
    for name in ("資料.pdf", "--help.pdf", "ok.docx"):
        (repo / name).write_bytes(b"not really a document")
    git(repo, "add", "--", ".")
    git(repo, "commit", "-qm", "documents")

    # Stands in for docsvg: records its arguments and writes a report whose
    # warning carries markdown a document could smuggle into the comment.
    fake = tmp_path / "fake-docsvg"
    fake.write_text(textwrap.dedent(f"""\
        #!/usr/bin/env python3
        import json, pathlib, sys
        with open({str(tmp_path / 'calls.txt')!r}, 'a', encoding='utf-8') as log:
            log.write(sys.argv[1] + '\\n')
        out = pathlib.Path(sys.argv[sys.argv.index('--output') + 1])
        out.mkdir(parents=True)
        (out / 'page-0001.svg').write_text('<svg xmlns="http://www.w3.org/2000/svg"/>')
        (out / 'conversion.json').write_text(json.dumps({{
            'page_count': 1, 'source_format': 'pdf',
            'pages': [{{'warning_count': 0}}], 'warnings': [{HOSTILE!r}]}}))
        """), encoding="utf-8")
    fake.chmod(0o755)
    return repo, fake


def run(repo, fake):
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--base", "HEAD~1", "--head", "HEAD", "--output", "out",
         "--summary", "summary.md", "--docsvg", str(fake)],
        cwd=repo, capture_output=True, text=True, env={**os.environ, "GITHUB_STEP_SUMMARY": ""})


def test_non_ascii_and_dash_named_documents_are_converted(repo):
    repo, fake = repo
    result = run(repo, fake)
    assert result.returncode == 0, result.stderr
    calls = (fake.parent / "calls.txt").read_text(encoding="utf-8").splitlines()
    assert sorted(calls) == sorted(["./--help.pdf", "./ok.docx", "./資料.pdf"])


def test_comment_cannot_carry_links_images_or_mentions(repo):
    repo, fake = repo
    run(repo, fake)
    summary = (repo / "summary.md").read_text(encoding="utf-8")
    warnings = [line for line in summary.splitlines() if "tracker.invalid" in line]
    assert warnings
    for line in warnings:
        # Each hostile warning sits inside one code span, where GitHub shows
        # links, images and mentions as plain text.
        body = line.removeprefix("- ")
        ticks = body[: len(body) - len(body.lstrip("`"))]
        assert len(ticks) >= 2 and body.startswith(ticks + " ") and body.endswith(" " + ticks)
        assert ticks not in body[len(ticks):-len(ticks)]
    rows = [line for line in summary.splitlines() if line.startswith("| `")]
    assert len(rows) == 3
    assert all(line.count(" | ") == 3 for line in rows), rows
