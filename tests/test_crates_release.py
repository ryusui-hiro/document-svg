"""Regression checks for crates.io release verification and safe retries."""

import importlib.util
import json
from pathlib import Path
import urllib.error

import pytest


SPEC = importlib.util.spec_from_file_location(
    "check_crates_release", Path(__file__).resolve().parents[1] / "scripts/check-crates-release.py"
)
crates = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(crates)


class Response:
    status = 200

    def __init__(self, payload):
        self.payload = payload

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False

    def read(self):
        return self.payload


def test_release_crate_must_match_tag_and_asset(tmp_path):
    local = tmp_path / "local" / "document-svg-2.0.0.crate"
    release = tmp_path / "release" / "document-svg-2.0.0.crate"
    local.parent.mkdir()
    release.parent.mkdir()
    local.write_bytes(b"verified crate")
    release.write_bytes(b"verified crate")

    version, checksum = crates.verify_release_crate("v2.0.0", local, release)

    assert version == "2.0.0"
    assert checksum == crates.sha256(local)


def test_release_crate_rejects_different_asset(tmp_path):
    local = tmp_path / "local" / "document-svg-2.0.0.crate"
    release = tmp_path / "release" / "document-svg-2.0.0.crate"
    local.parent.mkdir()
    release.parent.mkdir()
    local.write_bytes(b"tagged source")
    release.write_bytes(b"different release")

    with pytest.raises(ValueError, match="does not match"):
        crates.verify_release_crate("v2.0.0", local, release)


@pytest.mark.parametrize("tag", ["2.0.0", "v2", "v2.0", "release-2.0.0"])
def test_release_tag_requires_versioned_v_prefix(tag):
    with pytest.raises(ValueError, match="vMAJOR.MINOR.PATCH"):
        crates.version_from_tag(tag)


def test_release_tag_accepts_semver_prerelease_and_build_metadata():
    assert crates.version_from_tag("v2.1.0-rc.1+build.7") == "2.1.0-rc.1+build.7"


def test_missing_registry_version_requires_publish():
    def missing(request, timeout):
        raise urllib.error.HTTPError(request.full_url, 404, "not found", {}, None)

    assert crates.registry_publish_required("2.0.0", "abc", opener=missing)


def test_identical_registry_version_skips_publish():
    payload = json.dumps({"version": {"num": "2.0.0", "checksum": "abc"}}).encode()

    assert not crates.registry_publish_required(
        "2.0.0", "abc", opener=lambda _request, timeout: Response(payload)
    )


def test_registry_version_rejects_different_checksum():
    payload = json.dumps({"version": {"num": "2.0.0", "checksum": "different"}}).encode()

    with pytest.raises(ValueError, match="different checksum"):
        crates.registry_publish_required(
            "2.0.0", "abc", opener=lambda _request, timeout: Response(payload)
        )


def test_workflow_runs_main_tooling_against_tagged_source():
    workflow = (
        Path(__file__).resolve().parents[1] / ".github/workflows/publish-crates.yml"
    ).read_text(encoding="utf-8")

    # Older tags do not contain the verification script, so it must run from
    # the main checkout while cargo packages and publishes the tagged source.
    assert "path: release-source" in workflow
    assert "python3 scripts/check-crates-release.py" in workflow
    assert '--local-crate "release-source/target/package/$crate"' in workflow
    for step in ("Package and verify", "Publish"):
        block = workflow.split(f"- name: {step}\n", 1)[1].split("- name:", 1)[0]
        assert "working-directory: release-source" in block
