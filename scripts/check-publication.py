"""Check Git publication candidates without printing potentially secret content."""
import pathlib
import os
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
paths = subprocess.check_output(
    ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=ROOT
).decode().split("\0")
# Supply private organization names locally; do not embed them in the repository.
names = list(filter(None, os.environ.get("PUBLICATION_DENY_TERMS", "").split(",")))
rules = [
    ("personal absolute path", re.compile(r"/(?:Users|home)/[A-Za-z0-9_.-]+/")),
    ("private key", re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----")),
    ("GitHub token", re.compile(r"(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})")),
    ("AWS access key", re.compile(r"(?:AKIA|ASIA)[A-Z0-9]{16}")),
]
if names:
    rules.append(("internal organization name", re.compile("|".join(map(re.escape, names)), re.I)))
errors = []
for relative in sorted(set(filter(None, paths))):
    path = ROOT / relative
    parts = pathlib.PurePosixPath(relative).parts
    if any(p in {"outputs", "output", "target", "node_modules", ".cache", ".tmp", "__pycache__"} for p in parts):
        errors.append((relative, "private/generated directory"))
        continue
    if path.is_symlink():
        errors.append((relative, "symlink requires explicit publication review"))
        continue
    if not path.is_file():
        continue
    if path.name.startswith(".env") and path.name != ".env.example":
        errors.append((relative, "environment file"))
    if path.suffix.lower() in {".pem", ".key", ".p12", ".pfx", ".node", ".whl", ".crate", ".pdf", ".pptx", ".docx", ".xlsx"}:
        errors.append((relative, "credential or binary artifact"))
    if path.stat().st_size > 2 * 1024 * 1024:
        errors.append((relative, "file larger than 2 MiB"))
        continue
    value = path.read_bytes().decode("utf-8", errors="replace")
    for label, pattern in rules:
        if pattern.search(relative) or pattern.search(value):
            errors.append((relative, label))
for relative, label in errors:
    print(f"FAIL: {relative}: {label}")
print(f"Checked {len(set(filter(None, paths)))} publication candidates; {len(errors)} findings.")
sys.exit(bool(errors))
