"""Consistency checks for the static project site under site/."""

import json
from pathlib import Path
import re

SITE = Path(__file__).resolve().parents[1] / "site"
CONTENT = json.loads((SITE / "assets/data/content.json").read_text(encoding="utf-8"))
FORMATS = json.loads((SITE / "assets/data/formats.json").read_text(encoding="utf-8"))
LANGS = [lang["code"] for lang in CONTENT["languages"]]
APP = (SITE / "assets/app.js").read_text(encoding="utf-8")
PAGES = dict(re.findall(r'\["(\w+)", "([\w-]+\.html)"\]', APP))


def shape(value):
    if isinstance(value, dict):
        return {key: shape(item) for key, item in value.items()}
    if isinstance(value, list):
        return [shape(item) for item in value]
    return type(value).__name__


def walk(value):
    if isinstance(value, dict):
        yield value
        for item in value.values():
            yield from walk(item)
    elif isinstance(value, list):
        for item in value:
            yield from walk(item)


def anchors(page):
    """Section ids a page renders, as app.js builds them."""
    strings = CONTENT["strings"]["en"][page]
    if page == "home":
        return {"why", "use-cases", "samples", "next"}
    if page == "useCases":
        return {item["id"] for item in strings["items"]}
    if page in ("formats", "samples"):
        return {category["id"] for category in FORMATS["categories"]}
    return {section["id"] for section in strings["sections"]}


def test_every_language_has_the_same_strings():
    reference = shape(CONTENT["strings"][LANGS[0]])
    for lang in LANGS[1:]:
        assert shape(CONTENT["strings"][lang]) == reference, lang


def test_every_page_has_a_shell_and_copy():
    assert set(PAGES) == {"home", "useCases", "formats", "samples", "start", "safety", "ai"}
    for page, filename in PAGES.items():
        html = (SITE / filename).read_text(encoding="utf-8")
        assert f'<body data-page="{page}">' in html, filename
        for lang in LANGS:
            meta = CONTENT["strings"][lang][page]["meta"]
            assert meta["title"] and meta["description"], (lang, page)
            assert CONTENT["strings"][lang]["site"]["nav"][page], (lang, page)


def test_internal_links_point_at_real_pages_and_sections():
    by_file = {filename: page for page, filename in PAGES.items()}
    for lang in LANGS:
        for node in walk(CONTENT["strings"][lang]):
            href = node.get("href")
            if not isinstance(href, str) or href.startswith("http"):
                continue
            path, _, fragment = href.partition("#")
            assert (SITE / path).is_file(), (lang, href)
            if fragment:
                assert fragment in anchors(by_file[path]), (lang, href)


def test_every_referenced_sample_is_published():
    for category in FORMATS["categories"]:
        for item in category["items"]:
            samples = item.get("sample") or []
            for key in [samples] if isinstance(samples, str) else samples:
                assert (SITE / "assets/samples" / key / "page-0001.svg").is_file(), key
            assert set(item["note"]) >= set(LANGS), item["ext"]


def test_home_highlights_exist():
    keys = re.search(r"HOME_SAMPLES = \[([^\]]+)\]", APP).group(1)
    for key in re.findall(r'"([\w-]+)"', keys):
        assert (SITE / "assets/samples" / key / "page-0001.svg").is_file(), key


def test_tier_legend_and_categories_are_translated():
    for tier in ("A", "B", "C"):
        assert set(FORMATS["tiers"][tier]) >= set(LANGS)
    for category in FORMATS["categories"]:
        assert set(category["name"]) >= set(LANGS), category["id"]
