(function () {
  "use strict";

  var SAMPLE_SOURCE_FILES = {
    pdf: "sample.pdf",
    pptx: "sample.pptx",
    xlsx: "sample.xlsx",
    docx: "sample.docx",
    "project-xml": "sample.project.xml",
    html: "sample.html",
    adoc: "sample.adoc",
    markdown: "sample-table.md",
    drawio: "sample.drawio",
    plantuml: "sample.puml",
    d2: "sample.d2",
    dot: "sample-architecture.dot",
    mermaid: "sample-sequence.mmd",
    dxf: "sample.dxf",
    gerber: "sample.gbr",
    hpgl: "sample.plt",
    gcode: "sample.nc",
    excellon: "sample.drl",
    stl: "sample.stl",
    obj: "sample.obj",
    ply: "sample.ply",
    step: "sample.step",
    ifc: "sample.ifc",
    ifczip: "sample.ifczip",
    msh: "sample.msh",
    su2: "sample.su2",
    vtk: "sample.vtk",
    csv: "sample.csv",
    toml: "sample.toml",
    yaml: "sample.yaml",
    "generic-xml": "sample-config.xml",
    properties: "sample.properties",
    bpmn: "sample.bpmn",
    dmn: "sample.dmn",
    cmmn: "sample.cmmn",
    reqif: "sample.reqif",
    xmi: "sample.xmi",
    chart: "sample-metrics.chart.json",
    tex: "sample-math.tex",
    qr: "sample-qr.qr",
    raster: "sample-signature.png",
    "esri-grid": "sample-elevation.asc",
    dbf: "sample-parcels.dbf",
    "json-text-seq": "sample.jsons",
    "geojson-seq": "sample.geojsons",
    topojson: "sample.topojson",
    gpkg: "sample.gpkg",
    "gpkg-tiles": "sample.gpkg"
  };

  var REPO = "https://github.com/ryusui-hiro/document-svg";
  var REPO_RAW = REPO + "/blob/main/samples/source/";

  // Page key (in content.json strings) → file. Order is the navigation order.
  var PAGES = [
    ["home", "index.html"],
    ["useCases", "use-cases.html"],
    ["formats", "formats.html"],
    ["samples", "samples.html"],
    ["start", "start.html"],
    ["safety", "safety.html"],
    ["ai", "ai.html"]
  ];

  // Sections of the old single-page site, so existing links keep working.
  var OLD_ANCHORS = {
    usecases: "use-cases.html",
    "for-ai": "ai.html",
    formats: "formats.html",
    samples: "samples.html",
    install: "start.html#choose",
    usage: "start.html#convert"
  };

  var HOME_SAMPLES = ["pptx", "xlsx", "drawio", "dxf", "stl", "chart"];
  var USAGE_TAB_ORDER = ["cli", "node", "python", "rust"];
  var DESKTOP = window.matchMedia("(min-width: 861px)");

  var state = {
    lang: "en",
    page: document.body.getAttribute("data-page") || "home",
    content: null,
    formats: null,
    usageTab: "cli",
    query: "",
    reverseOnly: new URLSearchParams(location.search).get("reverse") === "1"
  };

  // --- helpers -------------------------------------------------------------

  function el(tag, props, children) {
    var node = document.createElement(tag);
    props = props || {};
    Object.keys(props).forEach(function (key) {
      var value = props[key];
      if (value === undefined || value === null || value === false) return;
      if (key === "className") node.className = value;
      else if (key === "text") node.textContent = value;
      else node.setAttribute(key, value === true ? "" : value);
    });
    (children || []).forEach(function (child) {
      if (child === null || child === undefined) return;
      node.appendChild(typeof child === "string" ? document.createTextNode(child) : child);
    });
    return node;
  }

  function strings() { return state.content.strings[state.lang]; }

  function isInternal(href) {
    return !/^[a-z]+:/i.test(href) && /\.html(#|$)/.test(href);
  }

  // Internal page links carry the language so it survives without localStorage.
  function localize(href) {
    if (!isInternal(href)) return href;
    var parts = href.split("#");
    return parts[0] + "?lang=" + state.lang + (parts[1] ? "#" + parts[1] : "");
  }

  function link(href, text, className) {
    var external = /^https?:/.test(href);
    return el("a", {
      href: localize(href),
      className: className,
      text: text,
      rel: external ? "noopener" : null
    });
  }

  function extTokens(ext) {
    return ext.split("/").map(function (s) { return s.trim(); }).filter(Boolean);
  }

  function firstExt(ext) {
    return ext.split(/[\s/]+/).filter(Boolean)[0] || ext;
  }

  function allItems() {
    var items = [];
    state.formats.categories.forEach(function (cat) { items = items.concat(cat.items); });
    return items;
  }

  function detectDefaultLang(content) {
    var q = new URLSearchParams(location.search).get("lang");
    if (q && content.strings[q]) return q;
    try {
      var saved = localStorage.getItem("docsvg-lang");
      if (saved && content.strings[saved]) return saved;
    } catch (e) {}
    var nav = (navigator.language || "en").toLowerCase();
    if (nav.indexOf("ja") === 0) return "ja";
    if (nav.indexOf("zh") === 0) return "zh";
    return content.defaultLang || "en";
  }

  // --- shared chrome -------------------------------------------------------

  function renderHeader() {
    var s = strings().site;
    var header = document.getElementById("site-header");
    header.innerHTML = "";
    var nav = el("nav", { className: "topnav", "aria-label": "Site" });
    PAGES.slice(1).forEach(function (entry) {
      var a = link(entry[1], s.nav[entry[0]]);
      if (entry[0] === state.page) a.setAttribute("aria-current", "page");
      nav.appendChild(a);
    });
    nav.appendChild(link(REPO, s.nav.github));

    var langs = el("div", { className: "langswitch", role: "group", "aria-label": "Language" });
    state.content.languages.forEach(function (lang) {
      var btn = el("button", {
        type: "button",
        text: lang.code === "en" ? "EN" : lang.label,
        "aria-pressed": String(lang.code === state.lang)
      });
      btn.addEventListener("click", function () { setLang(lang.code); });
      langs.appendChild(btn);
    });

    header.appendChild(el("div", { className: "topbar-inner" }, [
      link("index.html", "document-svg", "brand plain"),
      nav,
      langs
    ]));
  }

  function renderFooter() {
    var f = strings().site.footer;
    var footer = document.getElementById("site-footer");
    footer.innerHTML = "";
    footer.appendChild(el("div", { className: "footer-links" }, [
      link(REPO, f.repo),
      link(REPO + "/releases", f.releases),
      link(REPO + "/tree/main/docs", f.docs),
      link(REPO + "/blob/main/docs/SUPPORT.md", f.support),
      link(REPO + "/blob/main/docs/ARCHITECTURE.md", f.architecture)
    ]));
    footer.appendChild(el("p", { text: f.tagline }));
  }

  function applyMeta() {
    var meta = strings()[state.page].meta;
    document.documentElement.lang = state.lang;
    document.title = meta.title;
    var desc = document.querySelector('meta[name="description"]');
    if (desc) desc.setAttribute("content", meta.description);
  }

  // Sidebar: a sticky list on wide screens, a collapsible "On this page" on phones.
  function sidebar(entries) {
    var list = el("ol");
    entries.forEach(function (entry) {
      list.appendChild(el("li", {}, [el("a", { href: "#" + entry.id, text: entry.label, "data-target": entry.id })]));
    });
    var details = el("details", { className: "sidenav" }, [
      el("summary", { text: strings().site.toc }),
      el("nav", { "aria-label": strings().site.toc }, [list])
    ]);
    details.open = DESKTOP.matches;
    list.addEventListener("click", function (event) {
      if (event.target.tagName === "A" && !DESKTOP.matches) details.open = false;
    });
    return details;
  }

  function pageHead(title, description) {
    return el("div", { className: "page-head" }, [
      el("h1", { text: title }),
      el("p", { text: description })
    ]);
  }

  function withSidebar(main, entries, body) {
    main.className = "wrap page-grid";
    main.appendChild(sidebar(entries));
    main.appendChild(el("article", { className: "page-body" }, body));
  }

  function linkList(links) {
    if (!links || !links.length) return null;
    return el("p", { className: "text-links" }, links.map(function (l) {
      return link(l.href, l.label + " →");
    }));
  }

  function card(item, href) {
    var children = [
      el("div", { className: "ico", text: item.icon, "aria-hidden": "true" }),
      el("h3", { text: item.title }),
      el("p", { text: item.description || item.summary })
    ];
    if (href) return el("a", { className: "usecase-card plain linked", href: localize(href) }, children);
    return el("div", { className: "usecase-card" }, children);
  }

  function galleryCard(item, sampleKey, title, category) {
    var s = strings().samples;
    var svgPath = "assets/samples/" + sampleKey + "/page-0001.svg";
    var links = el("div", { className: "gallery-links" }, [
      el("a", { href: svgPath, target: "_blank", rel: "noopener", text: s.viewSvg })
    ]);
    if (SAMPLE_SOURCE_FILES[sampleKey]) {
      links.appendChild(el("a", {
        href: REPO_RAW + SAMPLE_SOURCE_FILES[sampleKey], target: "_blank", rel: "noopener", text: s.viewSource
      }));
    }
    return el("div", { className: "gallery-card" }, [
      el("div", { className: "gallery-thumb" }, [el("img", { alt: title, src: svgPath, loading: "lazy" })]),
      el("div", { className: "gallery-body" }, [
        el("h4", { text: title }),
        el("div", { className: "ext-tag", text: category.name[state.lang] }),
        links
      ])
    ]);
  }

  // Every sample in formats.json, grouped by category, each key shown once.
  function samplesByCategory() {
    var seen = {};
    return state.formats.categories.map(function (cat) {
      var cards = [];
      cat.items.forEach(function (item) {
        if (!item.sample) return;
        var keys = Array.isArray(item.sample) ? item.sample : [item.sample];
        var tokens = extTokens(item.ext);
        keys.forEach(function (key, i) {
          if (seen[key]) return;
          seen[key] = true;
          cards.push({ item: item, cat: cat, key: key, title: (keys.length > 1 ? tokens[i] : firstExt(item.ext)) || key });
        });
      });
      return { cat: cat, cards: cards };
    }).filter(function (group) { return group.cards.length; });
  }

  function tierPill(value) {
    if (!value) return el("span", { className: "tier-pill none", text: "—" });
    var letter = value.trim().charAt(0);
    return el("span", {
      className: "tier-pill " + (["A", "B", "C"].indexOf(letter) >= 0 ? letter : "none"),
      text: value
    });
  }

  function tierLegend() {
    var wrap = el("div", { className: "tier-legend" });
    ["A", "B", "C"].forEach(function (tier) {
      wrap.appendChild(el("div", { className: "tier-chip" }, [
        el("div", { className: "tier-badge " + tier, text: tier }),
        el("span", { text: state.formats.tiers[tier][state.lang] })
      ]));
    });
    return wrap;
  }

  // --- home ----------------------------------------------------------------

  function renderStats() {
    var s = strings().home.stats;
    var items = allItems();
    var extensions = {};
    items.forEach(function (item) {
      if (!item.forward) return;
      extTokens(item.ext).forEach(function (token) {
        if (token.charAt(0) === ".") extensions[token.toLowerCase()] = true;
      });
    });
    var locale = state.lang === "zh" ? "zh-CN" : state.lang;
    function stat(n, label) {
      return el("div", { className: "stat" }, [el("b", { text: n.toLocaleString(locale) }), el("span", { text: label })]);
    }
    return el("div", { className: "stats" }, [
      stat(items.filter(function (i) { return i.forward; }).length, s.formats),
      stat(Object.keys(extensions).length, s.extensions),
      stat(items.filter(function (i) { return i.reverse; }).length, s.reverse),
      stat(0, s.uploads)
    ]);
  }

  function section(id, title, description, children) {
    var head = el("div", { className: "section-head" }, [el("h2", { text: title }), description ? el("p", { text: description }) : null]);
    return el("section", { id: id, className: "wrap" }, [head].concat(children));
  }

  function renderHome(main) {
    var h = strings().home;
    var useCases = strings().useCases.items;

    var byKey = {};
    samplesByCategory().forEach(function (group) {
      group.cards.forEach(function (c) { byKey[c.key] = c; });
    });

    main.appendChild(el("header", { className: "wrap hero" }, [
      el("span", { className: "eyebrow", text: h.hero.eyebrow }),
      el("h1", { text: h.hero.title }),
      el("p", { text: h.hero.description }),
      el("div", { className: "hero-ctas" }, [
        link("start.html", h.hero.ctaPrimary, "btn primary"),
        link("formats.html", h.hero.ctaSecondary, "btn"),
        link("samples.html", h.hero.ctaTertiary, "btn")
      ]),
      renderStats()
    ]));

    main.appendChild(section("why", h.why.title, h.why.description, [
      el("div", { className: "usecases" }, h.why.items.map(function (item) { return card(item); }))
    ]));

    main.appendChild(section("use-cases", h.useCases.title, h.useCases.description, [
      el("div", { className: "usecases" }, useCases.map(function (item) {
        return card(item, "use-cases.html#" + item.id);
      })),
      linkList([{ href: "use-cases.html", label: h.useCases.more }])
    ]));

    main.appendChild(section("samples", h.samples.title, h.samples.description, [
      el("div", { className: "gallery" }, HOME_SAMPLES.filter(function (k) { return byKey[k]; }).map(function (k) {
        return galleryCard(byKey[k].item, k, byKey[k].title, byKey[k].cat);
      })),
      linkList([{ href: "samples.html", label: h.samples.more }])
    ]));

    main.appendChild(section("next", strings().site.next, null, [
      el("div", { className: "next-grid" }, h.next.map(function (n) {
        return el("a", { className: "next-card plain", href: localize(n.href) }, [
          el("h3", { text: n.title + " →" }),
          el("p", { text: n.description })
        ]);
      }))
    ]));
  }

  // --- use cases -----------------------------------------------------------

  function renderUseCases(main) {
    var u = strings().useCases;
    var body = [pageHead(u.title, u.description)];
    u.items.forEach(function (item) {
      var rows = el("dl", { className: "detail-grid" });
      ["who", "problem", "after", "start"].forEach(function (key) {
        rows.appendChild(el("dt", { text: u.labels[key] }));
        rows.appendChild(el("dd", { text: item[key] }));
      });
      body.push(el("section", { id: item.id, className: "doc-section" }, [
        el("h2", {}, [el("span", { className: "h-ico", text: item.icon, "aria-hidden": "true" }), item.title]),
        el("p", { className: "lead", text: item.summary }),
        rows,
        linkList([item.link])
      ]));
    });
    withSidebar(main, u.items.map(function (i) { return { id: i.id, label: i.title }; }), body);
  }

  // --- formats -------------------------------------------------------------

  function matches(item, cat, query) {
    if (state.reverseOnly && !item.reverse) return false;
    if (!query) return true;
    var haystack = [item.ext, item.note[state.lang], item.note.en, cat.name[state.lang], cat.name.en].join(" ").toLowerCase();
    return query.split(/\s+/).every(function (word) { return haystack.indexOf(word) >= 0; });
  }

  function renderFormatRows(container, countNode, emptyNode) {
    var f = strings().formats;
    var query = state.query.trim().toLowerCase();
    var total = 0;
    container.innerHTML = "";
    state.formats.categories.forEach(function (cat) {
      var rows = cat.items.filter(function (item) { return matches(item, cat, query); });
      total += rows.length;
      var tbody = el("tbody");
      rows.forEach(function (item) {
        var ext = el("td", { className: "ext" });
        extTokens(item.ext).forEach(function (token, i) {
          if (i > 0) ext.appendChild(document.createTextNode(" "));
          ext.appendChild(el("code", { text: token }));
        });
        tbody.appendChild(el("tr", {}, [
          ext,
          el("td", {}, [tierPill(item.forward)]),
          el("td", {}, [tierPill(item.reverse)]),
          el("td", { className: "notes-cell", text: item.note[state.lang] })
        ]));
      });
      var sec = el("section", { id: cat.id, className: "doc-section format-group" }, [
        el("h2", {}, [cat.name[state.lang], el("span", { className: "count-chip", text: String(rows.length) })]),
        el("div", { className: "table-scroll" }, [
          el("table", { className: "formats" }, [
            el("thead", {}, [el("tr", {}, [
              el("th", { text: f.colExt }), el("th", { text: f.colForward }),
              el("th", { text: f.colReverse }), el("th", { text: f.colNotes })
            ])]),
            tbody
          ])
        ])
      ]);
      sec.hidden = rows.length === 0;
      container.appendChild(sec);
    });
    countNode.textContent = f.count.replace("{n}", total.toLocaleString(state.lang === "zh" ? "zh-CN" : state.lang));
    emptyNode.hidden = total > 0;
  }

  function renderFormats(main) {
    var f = strings().formats;
    var groups = el("div");
    var count = el("span", { className: "result-count", "aria-live": "polite" });
    var empty = el("p", { className: "empty-note", text: f.none });
    var input = el("input", { type: "search", id: "format-search", placeholder: f.searchPlaceholder, autocomplete: "off" });
    input.value = state.query;
    input.addEventListener("input", function () {
      state.query = input.value;
      renderFormatRows(groups, count, empty);
    });
    var reverse = el("input", { type: "checkbox", id: "reverse-only" });
    reverse.checked = state.reverseOnly;
    reverse.addEventListener("change", function () {
      state.reverseOnly = reverse.checked;
      renderFormatRows(groups, count, empty);
    });

    var body = [
      pageHead(f.title, f.description),
      el("div", { className: "how-box" }, [
        el("h2", { text: f.howTitle }),
        el("ul", {}, f.how.map(function (t) { return el("li", { text: t }); })),
        el("h3", { text: f.legendTitle }),
        tierLegend()
      ]),
      el("div", { className: "search-bar" }, [
        el("label", { for: "format-search", text: f.searchLabel }),
        input,
        el("label", { className: "check", for: "reverse-only" }, [reverse, f.reverseOnly]),
        count
      ]),
      empty,
      groups,
      el("p", { className: "text-links" }, [link(REPO + "/issues", f.request + " →")])
    ];
    withSidebar(main, state.formats.categories.map(function (c) { return { id: c.id, label: c.name[state.lang] }; }), body);
    renderFormatRows(groups, count, empty);
  }

  // --- samples -------------------------------------------------------------

  function renderSamples(main) {
    var s = strings().samples;
    var groups = samplesByCategory();
    var body = [pageHead(s.title, s.description)];
    groups.forEach(function (group) {
      body.push(el("section", { id: group.cat.id, className: "doc-section" }, [
        el("h2", { text: group.cat.name[state.lang] }),
        el("div", { className: "gallery" }, group.cards.map(function (c) { return galleryCard(c.item, c.key, c.title, c.cat); }))
      ]));
    });
    withSidebar(main, groups.map(function (g) { return { id: g.cat.id, label: g.cat.name[state.lang] }; }), body);
  }

  // --- generic section pages (start, safety, ai) ---------------------------

  function usageBlock() {
    var tabs = el("div", { className: "usage-tabs", role: "tablist" });
    var panels = el("div", { className: "usage-panels" });
    function draw() {
      tabs.innerHTML = "";
      panels.innerHTML = "";
      USAGE_TAB_ORDER.forEach(function (key) {
        var btn = el("button", { type: "button", role: "tab", "aria-selected": String(state.usageTab === key), text: strings().usageTabs[key] });
        btn.addEventListener("click", function () { state.usageTab = key; draw(); });
        tabs.appendChild(btn);
      });
      (state.content.usageSnippets[state.usageTab] || []).forEach(function (snippet) {
        panels.appendChild(el("div", { className: "usage-snippet" }, [
          el("h4", {}, [snippet.caption[state.lang], el("span", { className: "lang-tag", text: snippet.lang })]),
          el("pre", {}, [el("code", { text: snippet.code })])
        ]));
      });
    }
    draw();
    return el("div", {}, [tabs, panels]);
  }

  function renderSectionPage(main) {
    var p = strings()[state.page];
    var body = [pageHead(p.title, p.description)];
    p.sections.forEach(function (sec) {
      var parts = [el("h2", { text: sec.title })];
      (sec.paras || []).forEach(function (t) { parts.push(el("p", { text: t })); });
      if (sec.cards) {
        parts.push(el("div", { className: "info-grid" }, sec.cards.map(function (c) {
          return el("div", { className: "info-card" }, [
            el("h3", { text: c.title }),
            el("p", { text: c.text }),
            c.code ? el("pre", {}, [el("code", { text: c.code })]) : null
          ]);
        })));
      }
      if (sec.code) parts.push(el("pre", {}, [el("code", { text: sec.code })]));
      if (sec.bullets) parts.push(el("ul", { className: "bullets" }, sec.bullets.map(function (t) { return el("li", { text: t }); })));
      if (sec.usage) parts.push(usageBlock());
      if (sec.contract) {
        parts.push(el("ol", { className: "contract-list" }, p.contractItems.map(function (t) { return el("li", { text: t }); })));
      }
      parts.push(linkList(sec.links));
      body.push(el("section", { id: sec.id, className: "doc-section" }, parts));
    });
    withSidebar(main, p.sections.map(function (s) { return { id: s.id, label: s.title }; }), body);
  }

  // --- wiring --------------------------------------------------------------

  var observer = null;

  function watchSections(main) {
    if (observer) observer.disconnect();
    var links = main.querySelectorAll(".sidenav a[data-target]");
    if (!links.length || !("IntersectionObserver" in window)) return;
    observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (!entry.isIntersecting) return;
        links.forEach(function (a) {
          a.setAttribute("aria-current", String(a.getAttribute("data-target") === entry.target.id));
        });
      });
    }, { rootMargin: "0px 0px -70% 0px" });
    links.forEach(function (a) {
      var target = document.getElementById(a.getAttribute("data-target"));
      if (target) observer.observe(target);
    });
  }

  function render() {
    applyMeta();
    renderHeader();
    renderFooter();
    var main = document.getElementById("main");
    main.innerHTML = "";
    main.className = "";
    var renderers = {
      home: renderHome,
      useCases: renderUseCases,
      formats: renderFormats,
      samples: renderSamples,
      start: renderSectionPage,
      safety: renderSectionPage,
      ai: renderSectionPage
    };
    renderers[state.page](main);
    watchSections(main);
  }

  function setLang(lang) {
    state.lang = lang;
    try { localStorage.setItem("docsvg-lang", lang); } catch (e) {}
    var url = new URL(location.href);
    url.searchParams.set("lang", lang);
    history.replaceState(null, "", url);
    render();
  }

  DESKTOP.addEventListener("change", function () {
    var nav = document.querySelector(".sidenav");
    if (nav) nav.open = DESKTOP.matches;
  });

  var oldAnchor = location.hash.slice(1);
  if (state.page === "home" && OLD_ANCHORS[oldAnchor]) {
    var target = OLD_ANCHORS[oldAnchor].split("#");
    location.replace(target[0] + location.search + (target[1] ? "#" + target[1] : ""));
    return;
  }

  Promise.all([
    fetch("assets/data/content.json").then(function (r) { return r.json(); }),
    fetch("assets/data/formats.json").then(function (r) { return r.json(); })
  ]).then(function (results) {
    state.content = results[0];
    state.formats = results[1];
    state.lang = detectDefaultLang(state.content);
    render();
    // The content is rendered after load, so the browser could not jump to the hash itself.
    if (location.hash) {
      var node = document.getElementById(decodeURIComponent(location.hash.slice(1)));
      if (node) node.scrollIntoView();
    }
  }).catch(function (err) {
    console.error("document-svg site: failed to load data", err);
  });
})();
