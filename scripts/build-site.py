#!/usr/bin/env python3
"""Render the project site (site/) into static HTML, one folder per language.

Every page is written as complete HTML so search engines and people without
JavaScript see the same content. The copy comes from site/assets/data/
content.json and the format list from formats.json; edit those, then run:

    python3 scripts/build-site.py          # rewrite the generated files
    python3 scripts/build-site.py --check  # fail if they are out of date

English lives at the site root, Japanese under ja/ and Chinese under zh/.
assets/app.js only adds behaviour (search, tabs, the sidebar) on top.
"""

import argparse
from html import escape
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
SITE = ROOT / "site"
BASE_URL = "https://ryusui-hiro.github.io/document-svg/"
REPO = "https://github.com/ryusui-hiro/document-svg"
REPO_RAW = REPO + "/blob/main/samples/source/"
# Social preview card; assets/og.svg is its source.
OG_IMAGE = BASE_URL + "assets/og.png"
# Google Search Console ownership token for the URL-prefix property above; public by design.
GOOGLE_SITE_VERIFICATION = "sye-NAawOjxnr-8EcugowaQyv_USJT87rgJIHBKPo3w"

PAGES = [
    ("home", "index.html"),
    ("useCases", "use-cases.html"),
    ("formats", "formats.html"),
    ("samples", "samples.html"),
    ("start", "start.html"),
    ("reference", "reference.html"),
    ("safety", "safety.html"),
    ("ai", "ai.html"),
]
# Language code → folder under site/ and the hreflang / og:locale values.
LANG_DIRS = {"en": "", "ja": "ja/", "zh": "zh/"}
HREFLANG = {"en": "en", "ja": "ja", "zh": "zh-Hans"}
OG_LOCALE = {"en": "en_US", "ja": "ja_JP", "zh": "zh_CN"}
LANG_LABEL = {"en": "EN", "ja": "日本語", "zh": "中文"}

HOME_SAMPLES = ["pptx", "xlsx", "drawio", "dxf", "stl", "chart"]
USAGE_TAB_ORDER = ["cli", "node", "python", "rust"]

SAMPLE_SOURCE_FILES = {
    "pdf": "sample.pdf", "pptx": "sample.pptx", "xlsx": "sample.xlsx", "docx": "sample.docx",
    "project-xml": "sample.project.xml", "html": "sample.html", "adoc": "sample.adoc",
    "markdown": "sample-table.md", "drawio": "sample.drawio", "plantuml": "sample.puml",
    "d2": "sample.d2", "dot": "sample-architecture.dot", "mermaid": "sample-sequence.mmd",
    "dxf": "sample.dxf", "gerber": "sample.gbr", "hpgl": "sample.plt", "gcode": "sample.nc",
    "excellon": "sample.drl", "stl": "sample.stl", "obj": "sample.obj", "ply": "sample.ply",
    "step": "sample.step", "ifc": "sample.ifc", "ifczip": "sample.ifczip", "msh": "sample.msh",
    "su2": "sample.su2", "vtk": "sample.vtk", "csv": "sample.csv", "toml": "sample.toml",
    "yaml": "sample.yaml", "generic-xml": "sample-config.xml", "properties": "sample.properties",
    "bpmn": "sample.bpmn", "dmn": "sample.dmn", "cmmn": "sample.cmmn", "reqif": "sample.reqif",
    "xmi": "sample.xmi", "chart": "sample-metrics.chart.json", "tex": "sample-math.tex",
    "qr": "sample-qr.qr", "raster": "sample-signature.png", "esri-grid": "sample-elevation.asc",
    "dbf": "sample-parcels.dbf", "json-text-seq": "sample.jsons", "geojson-seq": "sample.geojsons",
    "topojson": "sample.topojson", "gpkg": "sample.gpkg", "gpkg-tiles": "sample.gpkg",
}

ICON = ("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'%3E"
        "%3Crect width='100' height='100' rx='20' fill='%233454d1'/%3E%3Ctext x='50' y='62' "
        "font-size='46' text-anchor='middle' fill='white' font-family='sans-serif'%3ES%3C/text%3E%3C/svg%3E")


def e(text):
    return escape(str(text), quote=True)


def ext_tokens(ext):
    return [token.strip() for token in ext.split("/") if token.strip()]


def number(n, lang):
    return f"{n:,}"


class Page:
    """Renders one page in one language."""

    def __init__(self, content, formats, lang, key, filename):
        self.content = content
        self.formats = formats
        self.lang = lang
        self.key = key
        self.filename = filename
        self.s = content["strings"][lang]
        self.prefix = "../" if LANG_DIRS[lang] else ""

    # --- links -------------------------------------------------------------

    def href(self, target):
        """Resolve a link from content.json relative to this page's folder."""
        if target.startswith(("http:", "https:", "#")) or ".html" in target:
            return target
        return self.prefix + target  # assets/…, llms.txt: shared at the site root

    def link(self, target, text, cls=None):
        attrs = f' class="{cls}"' if cls else ""
        rel = ' rel="noopener"' if target.startswith("http") else ""
        return f'<a href="{e(self.href(target))}"{attrs}{rel}>{e(text)}</a>'

    def lang_href(self, lang):
        """This page in another language, relative to this page."""
        return self.prefix + LANG_DIRS[lang] + self.filename

    def url_of(self, filename, lang=None):
        name = "" if filename == "index.html" else filename
        return BASE_URL + LANG_DIRS[lang or self.lang] + name

    def url(self, lang=None):
        lang = lang or self.lang
        name = "" if self.filename == "index.html" else self.filename
        return BASE_URL + LANG_DIRS[lang] + name

    # --- shared pieces -----------------------------------------------------

    def head(self):
        meta = self.s[self.key]["meta"]
        alternates = "\n".join(
            f'<link rel="alternate" hreflang="{HREFLANG[lang]}" href="{self.url(lang)}" />' for lang in LANG_DIRS
        )
        site = self.s["site"]
        crumbs = [{"@type": "ListItem", "position": 1, "name": "document-svg", "item": self.url_of("index.html")}]
        if self.key != "home":
            crumbs.append({"@type": "ListItem", "position": 2, "name": site["nav"][self.key], "item": self.url()})
        structured = ('\n<script type="application/ld+json">'
                      + json.dumps({"@context": "https://schema.org", "@type": "BreadcrumbList",
                                    "itemListElement": crumbs}, ensure_ascii=False) + "</script>")
        if self.key == "home":
            data = {
                "@context": "https://schema.org",
                "@type": "SoftwareApplication",
                "name": "document-svg",
                "alternateName": "docsvg",
                "description": meta["description"],
                "url": self.url(),
                "inLanguage": HREFLANG[self.lang],
                "applicationCategory": "DeveloperApplication",
                "operatingSystem": "Windows, macOS, Linux",
                "license": REPO + "/blob/main/LICENSE",
                "isAccessibleForFree": True,
                "offers": {"@type": "Offer", "price": "0", "priceCurrency": "USD"},
                "codeRepository": REPO,
                "image": OG_IMAGE,
                "sameAs": [
                    REPO,
                    "https://www.npmjs.com/package/document-svg",
                    "https://pypi.org/project/document-svg/",
                    "https://crates.io/crates/document-svg",
                ],
            }
            structured += ('\n<script type="application/ld+json">'
                           + json.dumps(data, ensure_ascii=False) + "</script>")
        return f"""<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>{e(meta['title'])}</title>
<meta name="description" content="{e(meta['description'])}" />
<link rel="canonical" href="{self.url()}" />
{alternates}
<link rel="alternate" hreflang="x-default" href="{self.url('en')}" />
<meta property="og:type" content="website" />
<meta property="og:site_name" content="document-svg" />
<meta property="og:title" content="{e(meta['title'])}" />
<meta property="og:description" content="{e(meta['description'])}" />
<meta property="og:url" content="{self.url()}" />
<meta property="og:locale" content="{OG_LOCALE[self.lang]}" />
<meta property="og:image" content="{OG_IMAGE}" />
<meta property="og:image:width" content="1200" />
<meta property="og:image:height" content="630" />
<meta property="og:image:alt" content="{e(site['ogImageAlt'])}" />
<meta name="twitter:card" content="summary_large_image" />
<meta name="google-site-verification" content="{GOOGLE_SITE_VERIFICATION}" />
<link rel="icon" href="{ICON}" />
<link rel="stylesheet" href="{self.prefix}assets/style.css" />
<link rel="alternate" type="text/plain" href="{self.prefix}llms.txt" title="llms.txt" />{structured}
</head>"""

    def header(self):
        nav = self.s["site"]["nav"]
        links = []
        for key, filename in PAGES[1:]:
            current = ' aria-current="page"' if key == self.key else ""
            links.append(f'<a href="{filename}"{current}>{e(nav[key])}</a>')
        links.append(self.link(REPO, nav["github"]))
        langs = []
        for lang in LANG_DIRS:
            current = ' aria-current="true"' if lang == self.lang else ""
            langs.append(f'<a href="{self.lang_href(lang)}" hreflang="{HREFLANG[lang]}" '
                         f'lang="{HREFLANG[lang]}" data-lang="{lang}"{current}>{LANG_LABEL[lang]}</a>')
        return f"""<header id="site-header" class="topbar">
<div class="topbar-inner">
<a class="brand plain" href="index.html">document-svg</a>
<nav class="topnav" aria-label="{e(nav['home'])}">{''.join(links)}</nav>
<div class="langswitch" role="group" aria-label="Language">{''.join(langs)}</div>
</div>
</header>"""

    def footer(self):
        f = self.s["site"]["footer"]
        links = [
            self.link(REPO, f["repo"]),
            self.link(REPO + "/releases", f["releases"]),
            self.link(REPO + "/tree/main/docs", f["docs"]),
            self.link(REPO + "/blob/main/docs/SUPPORT.md", f["support"]),
            self.link(REPO + "/blob/main/docs/ARCHITECTURE.md", f["architecture"]),
        ]
        return f"""<footer id="site-footer" class="wrap">
<div class="footer-links">{''.join(links)}</div>
<p>{e(f['tagline'])}</p>
</footer>"""

    def sidebar(self, entries):
        toc = self.s["site"]["toc"]
        items = "".join(f'<li><a href="#{e(i)}" data-target="{e(i)}">{e(label)}</a></li>' for i, label in entries)
        return (f'<details class="sidenav" open><summary>{e(toc)}</summary>'
                f'<nav aria-label="{e(toc)}"><ol>{items}</ol></nav></details>')

    def with_sidebar(self, entries, body):
        return (f'<main id="main" class="wrap page-grid">{self.sidebar(entries)}'
                f'<article class="page-body">{"".join(body)}</article></main>')

    def page_head(self, title, description):
        return f'<div class="page-head"><h1>{e(title)}</h1><p>{e(description)}</p></div>'

    def link_list(self, links):
        links = [link for link in (links or []) if link]
        if not links:
            return ""
        return '<p class="text-links">' + "".join(self.link(l["href"], l["label"] + " →") for l in links) + "</p>"

    def card(self, item, target=None):
        inner = (f'<div class="ico" aria-hidden="true">{e(item["icon"])}</div>'
                 f'<h3>{e(item["title"])}</h3><p>{e(item.get("description") or item["summary"])}</p>')
        if target:
            return f'<a class="usecase-card plain linked" href="{e(target)}">{inner}</a>'
        return f'<div class="usecase-card">{inner}</div>'

    def samples_by_category(self):
        seen = set()
        groups = []
        for cat in self.formats["categories"]:
            cards = []
            for item in cat["items"]:
                keys = item.get("sample") or []
                keys = [keys] if isinstance(keys, str) else keys
                tokens = ext_tokens(item["ext"])
                for i, key in enumerate(keys):
                    if key in seen:
                        continue
                    seen.add(key)
                    if len(keys) == 1:
                        title = item["ext"].split()[0]
                    else:
                        title = tokens[i] if i < len(tokens) else key
                    cards.append((key, title, cat))
            if cards:
                groups.append((cat, cards))
        return groups

    def gallery_card(self, key, title, cat):
        s = self.s["samples"]
        svg = f"{self.prefix}assets/samples/{key}/page-0001.svg"
        alt = f"{title} — {cat['name'][self.lang]}"
        links = f'<a href="{svg}" target="_blank" rel="noopener">{e(s["viewSvg"])}</a>'
        if key in SAMPLE_SOURCE_FILES:
            links += (f'<a href="{REPO_RAW + SAMPLE_SOURCE_FILES[key]}" target="_blank" rel="noopener">'
                      f'{e(s["viewSource"])}</a>')
        return (f'<div class="gallery-card"><div class="gallery-thumb">'
                f'<img alt="{e(alt)}" src="{svg}" loading="lazy" width="220" height="165" /></div>'
                f'<div class="gallery-body"><h3>{e(title)}</h3>'
                f'<div class="ext-tag">{e(cat["name"][self.lang])}</div>'
                f'<div class="gallery-links">{links}</div></div></div>')

    def tier_legend(self):
        chips = "".join(
            f'<div class="tier-chip"><div class="tier-badge {t}">{t}</div>'
            f'<span>{e(self.formats["tiers"][t][self.lang])}</span></div>' for t in "ABC")
        return f'<div class="tier-legend">{chips}</div>'

    @staticmethod
    def tier_pill(value):
        if not value:
            return '<span class="tier-pill none">—</span>'
        letter = value.strip()[0]
        cls = letter if letter in "ABC" else "none"
        return f'<span class="tier-pill {cls}">{e(value)}</span>'

    # --- pages -------------------------------------------------------------

    def home(self):
        h = self.s["home"]
        items = [item for cat in self.formats["categories"] for item in cat["items"]]
        extensions = {t.lower() for item in items if item["forward"] for t in ext_tokens(item["ext"]) if t.startswith(".")}
        stats = [
            (sum(1 for i in items if i["forward"]), h["stats"]["formats"]),
            (len(extensions), h["stats"]["extensions"]),
            (sum(1 for i in items if i["reverse"]), h["stats"]["reverse"]),
            (0, h["stats"]["uploads"]),
        ]
        stat_html = "".join(f'<div class="stat"><b>{number(n, self.lang)}</b><span>{e(label)}</span></div>' for n, label in stats)
        by_key = {key: (key, title, cat) for _, cards in self.samples_by_category() for key, title, cat in cards}

        def section(sid, title, description, inner):
            desc = f"<p>{e(description)}</p>" if description else ""
            return (f'<section id="{sid}" class="wrap"><div class="section-head"><h2>{e(title)}</h2>{desc}</div>'
                    f"{inner}</section>")

        hero = h["hero"]
        out = [f"""<main id="main">
<header class="wrap hero">
<span class="eyebrow">{e(hero['eyebrow'])}</span>
<h1>{e(hero['title'])}</h1>
<p>{e(hero['description'])}</p>
<div class="hero-ctas">{self.link('start.html', hero['ctaPrimary'], 'btn primary')}{self.link('formats.html', hero['ctaSecondary'], 'btn')}{self.link('samples.html', hero['ctaTertiary'], 'btn')}</div>
<div class="stats">{stat_html}</div>
</header>"""]
        out.append(section("why", h["why"]["title"], h["why"]["description"],
                           '<div class="usecases">' + "".join(self.card(i) for i in h["why"]["items"]) + "</div>"))
        out.append(section("use-cases", h["useCases"]["title"], h["useCases"]["description"],
                           '<div class="usecases">'
                           + "".join(self.card(i, "use-cases.html#" + i["id"]) for i in self.s["useCases"]["items"])
                           + "</div>" + self.link_list([{"href": "use-cases.html", "label": h["useCases"]["more"]}])))
        out.append(section("samples", h["samples"]["title"], h["samples"]["description"],
                           '<div class="gallery">' + "".join(self.gallery_card(*by_key[k]) for k in HOME_SAMPLES if k in by_key)
                           + "</div>" + self.link_list([{"href": "samples.html", "label": h["samples"]["more"]}])))
        nxt = "".join(f'<a class="next-card plain" href="{e(n["href"])}"><h3>{e(n["title"])} →</h3>'
                      f'<p>{e(n["description"])}</p></a>' for n in h["next"])
        out.append(section("next", self.s["site"]["next"], None, f'<div class="next-grid">{nxt}</div>'))
        out.append("</main>")
        return "\n".join(out)

    def use_cases(self):
        u = self.s["useCases"]
        body = [self.page_head(u["title"], u["description"])]
        for item in u["items"]:
            rows = "".join(f"<dt>{e(u['labels'][k])}</dt><dd>{e(item[k])}</dd>" for k in ("who", "problem", "after", "start"))
            body.append(f'<section id="{e(item["id"])}" class="doc-section">'
                        f'<h2><span class="h-ico" aria-hidden="true">{e(item["icon"])}</span>{e(item["title"])}</h2>'
                        f'<p class="lead">{e(item["summary"])}</p><dl class="detail-grid">{rows}</dl>'
                        f"{self.link_list([item['link']])}</section>")
        return self.with_sidebar([(i["id"], i["title"]) for i in u["items"]], body)

    def formats_page(self):
        f = self.s["formats"]
        groups = []
        total = 0
        for cat in self.formats["categories"]:
            rows = []
            for item in cat["items"]:
                total += 1
                # The row's own text is searched too; this adds the category and,
                # on translated pages, the English wording people may type.
                extra = [cat["name"][self.lang]]
                if self.lang != "en":
                    extra += [item["note"]["en"], cat["name"]["en"]]
                search = " ".join(extra).lower()
                exts = " ".join(f"<code>{e(t)}</code>" for t in ext_tokens(item["ext"]))
                reverse = ' data-reverse="1"' if item["reverse"] else ""
                rows.append(f'<tr data-search="{e(search)}"{reverse}><td class="ext">{exts}</td>'
                            f"<td>{self.tier_pill(item['forward'])}</td><td>{self.tier_pill(item['reverse'])}</td>"
                            f'<td class="notes-cell">{e(item["note"][self.lang])}</td></tr>')
            head = "".join(f"<th>{e(f[c])}</th>" for c in ("colExt", "colForward", "colReverse", "colNotes"))
            groups.append(f'<section id="{e(cat["id"])}" class="doc-section format-group">'
                          f'<h2>{e(cat["name"][self.lang])}<span class="count-chip">{len(cat["items"])}</span></h2>'
                          f'<div class="table-scroll"><table class="formats"><thead><tr>{head}</tr></thead>'
                          f'<tbody>{"".join(rows)}</tbody></table></div></section>')
        how = "".join(f"<li>{e(t)}</li>" for t in f["how"])
        count = f["count"].replace("{n}", number(total, self.lang))
        body = [
            self.page_head(f["title"], f["description"]),
            f'<div class="how-box"><h2>{e(f["howTitle"])}</h2><ul>{how}</ul>'
            f'<h3>{e(f["legendTitle"])}</h3>{self.tier_legend()}</div>',
            f'<div class="search-bar" id="format-tools" hidden>'
            f'<label for="format-search">{e(f["searchLabel"])}</label>'
            f'<input type="search" id="format-search" placeholder="{e(f["searchPlaceholder"])}" autocomplete="off" />'
            f'<label class="check" for="reverse-only"><input type="checkbox" id="reverse-only" />{e(f["reverseOnly"])}</label>'
            f'<span class="result-count" aria-live="polite" data-template="{e(f["count"])}">{e(count)}</span></div>',
            f'<p class="empty-note" id="format-empty" hidden>{e(f["none"])}</p>',
            '<div id="format-groups">' + "".join(groups) + "</div>",
            self.link_list([{"href": REPO + "/issues", "label": f["request"]}]),
        ]
        return self.with_sidebar([(c["id"], c["name"][self.lang]) for c in self.formats["categories"]], body)

    def samples_page(self):
        s = self.s["samples"]
        groups = self.samples_by_category()
        body = [self.page_head(s["title"], s["description"])]
        for cat, cards in groups:
            body.append(f'<section id="{e(cat["id"])}" class="doc-section"><h2>{e(cat["name"][self.lang])}</h2>'
                        f'<div class="gallery">{"".join(self.gallery_card(*c) for c in cards)}</div></section>')
        return self.with_sidebar([(cat["id"], cat["name"][self.lang]) for cat, _ in groups], body)

    def usage_block(self):
        tabs, panels = [], []
        for i, key in enumerate(USAGE_TAB_ORDER):
            selected = "true" if i == 0 else "false"
            tabs.append(f'<button type="button" role="tab" id="tab-{key}" aria-controls="panel-{key}" '
                        f'aria-selected="{selected}">{e(self.s["usageTabs"][key])}</button>')
            snippets = "".join(
                f'<div class="usage-snippet"><h3>{e(sn["caption"][self.lang])}<span class="lang-tag">{e(sn["lang"])}</span></h3>'
                f'<pre><code>{e(sn["code"])}</code></pre></div>' for sn in self.content["usageSnippets"][key])
            hidden = "" if i == 0 else " hidden"
            panels.append(f'<div class="usage-panel" role="tabpanel" id="panel-{key}" aria-labelledby="tab-{key}"{hidden}>{snippets}</div>')
        return f'<div class="usage"><div class="usage-tabs" role="tablist">{"".join(tabs)}</div>{"".join(panels)}</div>'

    def section_page(self):
        p = self.s[self.key]
        body = [self.page_head(p["title"], p["description"])]
        for sec in p["sections"]:
            parts = [f"<h2>{e(sec['title'])}</h2>"]
            parts += [f"<p>{e(t)}</p>" for t in sec.get("paras", [])]
            if sec.get("cards"):
                cards = "".join(
                    f'<div class="info-card"><h3>{e(c["title"])}</h3><p>{e(c["text"])}</p>'
                    + (f'<pre><code>{e(c["code"])}</code></pre>' if c.get("code") else "") + "</div>"
                    for c in sec["cards"])
                parts.append(f'<div class="info-grid">{cards}</div>')
            if sec.get("code"):
                parts.append(f"<pre><code>{e(sec['code'])}</code></pre>")
            for tbl in sec.get("tables", []):
                parts.append(self.data_table(tbl))
            if sec.get("bullets"):
                parts.append('<ul class="bullets">' + "".join(f"<li>{e(t)}</li>" for t in sec["bullets"]) + "</ul>")
            if sec.get("usage"):
                parts.append(self.usage_block())
            if sec.get("contract"):
                parts.append('<ol class="contract-list">' + "".join(f"<li>{e(t)}</li>" for t in p["contractItems"]) + "</ol>")
            parts.append(self.link_list(sec.get("links")))
            body.append(f'<section id="{e(sec["id"])}" class="doc-section">{"".join(parts)}</section>')
        return self.with_sidebar([(s["id"], s["title"]) for s in p["sections"]], body)

    @staticmethod
    def data_table(tbl):
        code = set(tbl.get("codeColumns", []))
        head = "".join(f"<th>{e(c)}</th>" for c in tbl["columns"])
        rows = "".join(
            "<tr>" + "".join(f"<td><code>{e(v)}</code></td>" if i in code else f"<td>{e(v)}</td>"
                             for i, v in enumerate(row)) + "</tr>"
            for row in tbl["rows"])
        caption = f"<h3>{e(tbl['caption'])}</h3>" if tbl.get("caption") else ""
        return (f'{caption}<div class="table-scroll"><table class="formats ref-table">'
                f"<thead><tr>{head}</tr></thead><tbody>{rows}</tbody></table></div>")

    def render(self):
        main = {
            "home": self.home, "useCases": self.use_cases, "formats": self.formats_page,
            "samples": self.samples_page, "start": self.section_page, "reference": self.section_page,
            "safety": self.section_page,
            "ai": self.section_page,
        }[self.key]()
        return f"""<!doctype html>
<html lang="{HREFLANG[self.lang]}">
{self.head()}
<!-- Generated by scripts/build-site.py from assets/data/*.json. Do not edit by hand. -->
<body data-page="{self.key}" data-lang="{self.lang}">
<a class="skip-link" href="#main">{e(self.s['site']['skip'])}</a>
{self.header()}
{main}
{self.footer()}
<script src="{self.prefix}assets/app.js" defer></script>
</body>
</html>
"""


def sitemap():
    entries = []
    for _, filename in PAGES:
        name = "" if filename == "index.html" else filename
        alternates = "".join(
            f'\n    <xhtml:link rel="alternate" hreflang="{HREFLANG[l]}" href="{BASE_URL}{LANG_DIRS[l]}{name}"/>'
            for l in LANG_DIRS)
        alternates += f'\n    <xhtml:link rel="alternate" hreflang="x-default" href="{BASE_URL}{name}"/>'
        for lang in LANG_DIRS:
            entries.append(f"  <url>\n    <loc>{BASE_URL}{LANG_DIRS[lang]}{name}</loc>{alternates}\n  </url>")
    return ('<?xml version="1.0" encoding="UTF-8"?>\n'
            '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" '
            'xmlns:xhtml="http://www.w3.org/1999/xhtml">\n' + "\n".join(entries) + "\n</urlset>\n")


MD_FILES = {"en": "docs/API.md", "ja": "docs/API.ja.md", "zh": "docs/API.zh-CN.md"}


def reference_markdown(content, lang):
    """The developer reference as Markdown for the repository (docs/API*.md)."""
    ref = content["strings"][lang]["reference"]
    others = " · ".join(f"[{LANG_LABEL[l]}]({Path(MD_FILES[l]).name})" for l in LANG_DIRS if l != lang)
    site = BASE_URL + LANG_DIRS[lang] + "reference.html"
    lines = [f"# {ref['title']}", "",
             f"{others} · [{site.split('//')[1]}]({site})", "",
             "<!-- Generated by scripts/build-site.py from site/assets/data/content.json. Do not edit by hand. -->", "",
             ref["description"], ""]

    def md_link(href):
        if href.startswith("http"):
            return href
        if ".html" in href:  # a page, which exists in every language
            return BASE_URL + LANG_DIRS[lang] + href
        return BASE_URL + href  # shared files at the site root, such as the viewer demo

    def cell(value, code):
        value = str(value).replace("|", "\\|")
        return f"`{value}`" if code else value

    for sec in ref["sections"]:
        lines += [f"## {sec['title']}", ""]
        for para in sec.get("paras", []):
            lines += [para, ""]
        if sec.get("code"):
            lines += ["```", sec["code"], "```", ""]
        for tbl in sec.get("tables", []):
            if tbl.get("caption"):
                lines += [f"### {tbl['caption']}", ""]
            code = set(tbl.get("codeColumns", []))
            lines.append("| " + " | ".join(tbl["columns"]) + " |")
            lines.append("|" + "---|" * len(tbl["columns"]))
            for row in tbl["rows"]:
                lines.append("| " + " | ".join(cell(v, i in code) for i, v in enumerate(row)) + " |")
            lines.append("")
        for bullet in sec.get("bullets", []):
            lines.append(f"- {bullet}")
        if sec.get("bullets"):
            lines.append("")
        for link in sec.get("links", []):
            lines.append(f"- [{link['label']}]({md_link(link['href'])})")
        if sec.get("links"):
            lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def outputs():
    content = json.loads((SITE / "assets/data/content.json").read_text(encoding="utf-8"))
    formats = json.loads((SITE / "assets/data/formats.json").read_text(encoding="utf-8"))
    files = {}
    for lang, folder in LANG_DIRS.items():
        for key, filename in PAGES:
            files[SITE / folder / filename] = Page(content, formats, lang, key, filename).render()
    # robots.txt is only honoured at the host root, which a project site does not own;
    # submit sitemap.xml in Google Search Console instead.
    files[SITE / "sitemap.xml"] = sitemap()
    for lang, name in MD_FILES.items():
        files[ROOT / name] = reference_markdown(content, lang)
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail if generated files are out of date")
    args = parser.parse_args()
    stale = []
    for path, text in outputs().items():
        current = path.read_text(encoding="utf-8") if path.exists() else None
        if current == text:
            continue
        if args.check:
            stale.append(path.relative_to(ROOT))
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
            print(f"wrote {path.relative_to(ROOT)}")
    if stale:
        print("Out of date (run python3 scripts/build-site.py):", *stale, sep="\n  ", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
