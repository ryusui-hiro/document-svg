"""Check Git publication candidates without printing potentially secret content."""
import pathlib
import hashlib
import json
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
sample_manifest = ROOT / 'samples/provenance.json'
sample_hashes = json.loads(sample_manifest.read_text()).get('sha256', {}) if sample_manifest.is_file() else {}
sample_paths = {f'samples/source/sample.{suffix}' for suffix in ('pdf', 'pptx', 'xlsx', 'docx')}
strict_fixture_manifest = ROOT / 'tests/fixtures/strict_ooxml.provenance.json'
strict_fixture_hashes = (json.loads(strict_fixture_manifest.read_text()).get('sha256', {})
                         if strict_fixture_manifest.is_file() else {})
strict_fixture_paths = {
    'tests/fixtures/sample_strict.docx',
    'tests/fixtures/sample_strict.pptx',
    'tests/fixtures/sample_strict.xlsx',
}
pdf_fixture_manifest = ROOT / 'tests/fixtures/pdf_images.provenance.json'
pdf_fixture_hashes = (json.loads(pdf_fixture_manifest.read_text()).get('sha256', {})
                      if pdf_fixture_manifest.is_file() else {})
pdf_fixture_paths = {
    'tests/fixtures/sample_jpx_alpha.pdf',
    'tests/fixtures/sample_jpx_soft_mask.pdf',
    'tests/fixtures/sample_jpx_smask_in_data_2.pdf',
}
legacy_doc_manifest = ROOT / 'tests/fixtures/legacy_doc.provenance.json'
legacy_doc_hashes = (json.loads(legacy_doc_manifest.read_text()).get('sha256', {})
                     if legacy_doc_manifest.is_file() else {})
legacy_doc_paths = {'tests/fixtures/sample_legacy.doc'}
legacy_ppt_manifest = ROOT / 'tests/fixtures/legacy_ppt.provenance.json'
legacy_ppt_hashes = (json.loads(legacy_ppt_manifest.read_text()).get('sha256', {})
                     if legacy_ppt_manifest.is_file() else {})
legacy_ppt_paths = {'tests/fixtures/sample_legacy.ppt'}
dicom_pdf_manifest = ROOT / 'tests/fixtures/dicom_pdf.provenance.json'
dicom_pdf_hashes = (json.loads(dicom_pdf_manifest.read_text()).get('sha256', {})
                    if dicom_pdf_manifest.is_file() else {})
dicom_pdf_paths = {'tests/fixtures/sample_encapsulated_pdf.dcm'}
browser_smoke_manifest = ROOT / 'tests/fixtures/browser_smoke.provenance.json'
browser_smoke_hashes = (json.loads(browser_smoke_manifest.read_text()).get('sha256', {})
                        if browser_smoke_manifest.is_file() else {})
browser_smoke_paths = {'tests/fixtures/sample_browser.pdf'}
e57_fixture_manifest = ROOT / 'tests/fixtures/e57_bunny.provenance.json'
e57_fixture_hashes = (json.loads(e57_fixture_manifest.read_text()).get('sha256', {})
                      if e57_fixture_manifest.is_file() else {})
e57_fixture_paths = {'tests/fixtures/sample_e57_bunny.e57'}
iwork_fixture_manifest = ROOT / 'tests/fixtures/iwork.provenance.json'
iwork_fixture_hashes = (json.loads(iwork_fixture_manifest.read_text()).get('sha256', {})
                        if iwork_fixture_manifest.is_file() else {})
iwork_fixture_paths = {'tests/fixtures/sample.key'}
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
    if path.suffix.lower() in {".pem", ".key", ".p12", ".pfx", ".node", ".so", ".pyd", ".dylib", ".whl", ".crate", ".pdf", ".doc", ".ppt", ".pptx", ".docx", ".xlsx", ".e57"}:
        approved_sample = (relative in sample_paths and path.stat().st_size <= 2 * 1024 * 1024
                           and hashlib.sha256(path.read_bytes()).hexdigest() == sample_hashes.get(relative.removeprefix('samples/')))
        approved_strict_fixture = (relative in strict_fixture_paths and path.stat().st_size <= 2 * 1024 * 1024
                                   and hashlib.sha256(path.read_bytes()).hexdigest() == strict_fixture_hashes.get(relative))
        approved_pdf_fixture = (relative in pdf_fixture_paths and path.stat().st_size <= 2 * 1024 * 1024
                                and hashlib.sha256(path.read_bytes()).hexdigest() == pdf_fixture_hashes.get(relative))
        approved_legacy_doc_fixture = (relative in legacy_doc_paths and path.stat().st_size <= 2 * 1024 * 1024
                                       and hashlib.sha256(path.read_bytes()).hexdigest() == legacy_doc_hashes.get(relative))
        approved_legacy_ppt_fixture = (relative in legacy_ppt_paths and path.stat().st_size <= 2 * 1024 * 1024
                                       and hashlib.sha256(path.read_bytes()).hexdigest() == legacy_ppt_hashes.get(relative))
        approved_dicom_pdf_fixture = (relative in dicom_pdf_paths and path.stat().st_size <= 2 * 1024 * 1024
                                      and hashlib.sha256(path.read_bytes()).hexdigest() == dicom_pdf_hashes.get(relative))
        approved_browser_smoke_fixture = (relative in browser_smoke_paths and path.stat().st_size <= 2 * 1024 * 1024
                                          and hashlib.sha256(path.read_bytes()).hexdigest() == browser_smoke_hashes.get(relative))
        approved_e57_fixture = (relative in e57_fixture_paths and path.stat().st_size <= 2 * 1024 * 1024
                                and hashlib.sha256(path.read_bytes()).hexdigest() == e57_fixture_hashes.get(relative))
        approved_iwork_fixture = (relative in iwork_fixture_paths and path.stat().st_size <= 2 * 1024 * 1024
                                  and hashlib.sha256(path.read_bytes()).hexdigest() == iwork_fixture_hashes.get(relative))
        if not approved_sample and not approved_strict_fixture and not approved_pdf_fixture and not approved_legacy_doc_fixture and not approved_legacy_ppt_fixture and not approved_dicom_pdf_fixture and not approved_browser_smoke_fixture and not approved_e57_fixture and not approved_iwork_fixture:
            errors.append((relative, "credential or unverified binary artifact"))
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
