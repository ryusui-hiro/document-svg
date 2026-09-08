"""Regression checks for release identity, ABI and bundled notices."""
import importlib.util
import json
from pathlib import Path
import zipfile

import pytest


SPEC = importlib.util.spec_from_file_location(
    "release_artifacts", Path(__file__).resolve().parents[1] / "scripts/release-artifacts.py"
)
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


def wheel(tmp_path, *, version="0.1.1", tag="cp310-abi3-linux_x86_64", omit=None, extra=None):
    path = tmp_path / "test.whl"
    root = f"document_svg-{version}.dist-info"
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr(f"{root}/METADATA", f"Name: document-svg\nVersion: {version}\nLicense-Expression: MIT OR Apache-2.0\n")
        archive.writestr(f"{root}/WHEEL", f"Tag: {tag}\n")
        for name in ("LICENSE-MIT", "LICENSE-APACHE", "THIRD_PARTY_LICENSES.txt"):
            if name != omit:
                archive.writestr(f"{root}/licenses/legal/{name}", "test notice")
        archive.writestr("document_svg/_native.abi3.so", b"fixture")
        if extra:
            archive.writestr(extra, b"fixture")
    return path


def test_accepts_nested_license_directory(tmp_path):
    release.check_wheel(wheel(tmp_path), "0.1.1")


@pytest.mark.parametrize("options", [
    {"version": "0.1.0"},
    {"tag": "cp313-cp313-linux_x86_64"},
    {"omit": "THIRD_PARTY_LICENSES.txt"},
    {"extra": "document_svg.libs/libexample.so.1"},
])
def test_rejects_unexpected_wheel_or_missing_notices(tmp_path, options):
    with pytest.raises(ValueError):
        release.check_wheel(wheel(tmp_path, **options), "0.1.1")


def test_refuses_mixed_source_commits_before_packaging(tmp_path, monkeypatch):
    artifacts = tmp_path / "artifacts"
    artifacts.mkdir()
    tag = release.TARGETS[0]["tag"]
    (artifacts / "build-info.json").write_text(json.dumps({
        "tag": tag, "version": "0.1.1", "commit": "wrong", "files": {},
    }))
    monkeypatch.setattr(release, "version", lambda: "0.1.1")
    with pytest.raises(ValueError, match="revision/version mismatch"):
        release.assemble(artifacts, tmp_path / "output", "expected")


def test_python_license_copies_match_binding_notices():
    root = Path(__file__).resolve().parents[1] / "bindings/python"
    for name in release.LICENSES:
        assert (root / "legal" / name).read_bytes() == (root / name).read_bytes()
