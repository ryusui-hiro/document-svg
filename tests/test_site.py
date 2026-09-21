"""Consistency checks for the static project site under site/."""

from html.parser import HTMLParser
import importlib.util
import json
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]
SITE = ROOT / "site"
SPEC = importlib.util.spec_from_file_location("build_site", ROOT / "scripts/build-site.py")
build = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(build)

CONTENT = json.loads((SITE / "assets/data/content.json").read_text(encoding="utf-8"))
FORMATS = json.loads((SITE / "assets/data/formats.json").read_text(encoding="utf-8"))
LANGS = [lang["code"] for lang in CONTENT["languages"]]
HTML_FILES = sorted(p for p in build.outputs() if p.suffix == ".html")


class Collect(HTMLParser):
    def __init__(self):
        super().__init__()
        self.ids = set()
        self.refs = []
        self.links = {}
        self.h1 = 0
        self.title = ""
        self._in_title = False

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if "id" in attrs:
            self.ids.add(attrs["id"])
        for key in ("href", "src"):
            if attrs.get(key) and tag in ("a", "img", "script", "link"):
                self.refs.append(attrs[key])
        if tag == "link":
            self.links.setdefault(attrs.get("rel"), []).append(attrs)
        if tag == "h1":
            self.h1 += 1
        self._in_title = tag == "title"

    def handle_data(self, data):
        if self._in_title:
            self.title += data

    def handle_endtag(self, tag):
        self._in_title = False


def parse(path):
    parser = Collect()
    parser.feed(path.read_text(encoding="utf-8"))
    return parser


def shape(value):
    if isinstance(value, dict):
        return {key: shape(item) for key, item in value.items()}
    if isinstance(value, list):
        return [shape(item) for item in value]
    return type(value).__name__


def test_generated_pages_are_up_to_date():
    stale = [str(path.relative_to(ROOT)) for path, text in build.outputs().items()
             if not path.exists() or path.read_text(encoding="utf-8") != text]
    assert not stale, "run python3 scripts/build-site.py: " + ", ".join(stale)


def test_every_language_has_the_same_strings():
    reference = shape(CONTENT["strings"][LANGS[0]])
    for lang in LANGS[1:]:
        assert shape(CONTENT["strings"][lang]) == reference, lang


def test_every_page_exists_in_every_language():
    assert len(HTML_FILES) == len(build.PAGES) * len(build.LANG_DIRS)
    for lang, folder in build.LANG_DIRS.items():
        for key, filename in build.PAGES:
            assert (SITE / folder / filename).is_file(), (lang, filename)


def test_local_links_and_anchors_resolve():
    ids = {path: parse(path).ids for path in HTML_FILES}
    for path in HTML_FILES:
        for ref in parse(path).refs:
            parts = urlsplit(ref)
            if parts.scheme or ref.startswith("data:"):
                continue
            target = (path.parent / parts.path).resolve() if parts.path else path
            if target == (SITE / "viewer").resolve():
                continue  # the browser viewer demo is built by scripts/stage-pages.sh at deploy time
            assert target.is_file(), f"{path.relative_to(SITE)} → {ref}"
            if parts.fragment and target.suffix == ".html":
                assert parts.fragment in ids[target], f"{path.relative_to(SITE)} → {ref}"


def test_pages_carry_search_metadata():
    for path in HTML_FILES:
        page = parse(path)
        rel = path.relative_to(SITE)
        assert page.title.strip() and page.h1 == 1, rel
        canonical = page.links["canonical"][0]["href"]
        assert canonical.startswith(build.BASE_URL), rel
        hreflangs = {link["hreflang"] for link in page.links["alternate"] if "hreflang" in link}
        assert hreflangs == set(build.HREFLANG.values()) | {"x-default"}, rel


def test_sitemap_lists_every_page():
    sitemap = (SITE / "sitemap.xml").read_text(encoding="utf-8")
    assert sitemap.count("<loc>") == len(HTML_FILES)


def test_every_referenced_sample_is_published():
    for category in FORMATS["categories"]:
        assert set(category["name"]) >= set(LANGS), category["id"]
        for item in category["items"]:
            samples = item.get("sample") or []
            for key in [samples] if isinstance(samples, str) else samples:
                assert (SITE / "assets/samples" / key / "page-0001.svg").is_file(), key
            assert set(item["note"]) >= set(LANGS), item["ext"]
    for tier in ("A", "B", "C"):
        assert set(FORMATS["tiers"][tier]) >= set(LANGS)


def test_structured_data_and_social_card():
    assert (SITE / "assets/og.png").is_file()
    for path in HTML_FILES:
        text = path.read_text(encoding="utf-8")
        assert f'<meta property="og:image" content="{build.OG_IMAGE}" />' in text, path.name
        blocks = text.split('<script type="application/ld+json">')[1:]
        types = [json.loads(block.split("</script>", 1)[0])["@type"] for block in blocks]
        assert "BreadcrumbList" in types, path.name


def test_api_docs_are_generated_for_every_language():
    for lang, name in build.MD_FILES.items():
        text = (ROOT / name).read_text(encoding="utf-8")
        assert text.startswith("# ") and "Do not edit by hand" in text, name
        for section in CONTENT["strings"][lang]["reference"]["sections"]:
            assert f"## {section['title']}" in text, (name, section["id"])


def test_pages_workflow_stages_the_viewer():
    workflow = (ROOT / ".github/workflows/pages.yml").read_text(encoding="utf-8")
    assert "sh scripts/stage-pages.sh _site" in workflow and "path: _site" in workflow
    stage = (ROOT / "scripts/stage-pages.sh").read_text(encoding="utf-8")
    assert '"$OUT/viewer/web/"' in stage
