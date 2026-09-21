"""Consistency checks for the static project site under site/."""

import json
from pathlib import Path
import re

SITE = Path(__file__).resolve().parents[1] / "site"
CONTENT = json.loads((SITE / "assets/data/content.json").read_text(encoding="utf-8"))
FORMATS = json.loads((SITE / "assets/data/formats.json").read_text(encoding="utf-8"))
LANGS = [lang["code"] for lang in CONTENT["languages"]]


def shape(value):
    if isinstance(value, dict):
        return {key: shape(item) for key, item in value.items()}
    if isinstance(value, list):
        return [shape(item) for item in value]
    return type(value).__name__


def lookup(strings, path):
    for key in path.split("."):
        strings = strings[key]
    return strings


def test_every_language_has_the_same_strings():
    reference = shape(CONTENT["strings"][LANGS[0]])
    for lang in LANGS[1:]:
        assert shape(CONTENT["strings"][lang]) == reference, lang


def test_every_i18n_key_in_the_page_exists():
    html = (SITE / "index.html").read_text(encoding="utf-8")
    keys = re.findall(r'data-i18n="([^"]+)"', html)
    assert keys
    for lang in LANGS:
        for key in keys:
            assert isinstance(lookup(CONTENT["strings"][lang], key), str), (lang, key)


def test_every_referenced_sample_is_published():
    for category in FORMATS["categories"]:
        for item in category["items"]:
            samples = item.get("sample") or []
            for key in [samples] if isinstance(samples, str) else samples:
                assert (SITE / "assets/samples" / key / "page-0001.svg").is_file(), key
            assert set(item["note"]) >= set(LANGS), item["ext"]


def test_tier_legend_is_translated():
    for tier in ("A", "B", "C"):
        assert set(FORMATS["tiers"][tier]) >= set(LANGS)
