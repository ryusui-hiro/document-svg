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

  var REPO_RAW = "https://github.com/ryusui-hiro/document-svg/blob/main/samples/source/";

  var state = {
    lang: "en",
    content: null,
    formats: null,
    activeCategory: "all",
    activeUsageTab: "cli"
  };

  function extTokens(ext) {
    return ext.split("/").map(function (s) { return s.trim(); }).filter(Boolean);
  }

  function firstExt(ext) {
    return ext.split(/[\s/]+/).filter(Boolean)[0] || ext;
  }

  function get(obj, path) {
    return path.split(".").reduce(function (o, k) {
      return o && o[k] !== undefined ? o[k] : undefined;
    }, obj);
  }

  function detectDefaultLang(content) {
    try {
      var saved = localStorage.getItem("docsvg-lang");
      if (saved && content.strings[saved]) return saved;
    } catch (e) {}
    var params = new URLSearchParams(location.search);
    var q = params.get("lang");
    if (q && content.strings[q]) return q;
    var nav = (navigator.language || "en").toLowerCase();
    if (nav.indexOf("ja") === 0) return "ja";
    if (nav.indexOf("zh") === 0) return "zh";
    return content.defaultLang || "en";
  }

  function applyI18n() {
    var strings = state.content.strings[state.lang];
    document.documentElement.lang = state.lang;
    document.querySelectorAll("[data-i18n]").forEach(function (el) {
      var value = get(strings, el.getAttribute("data-i18n"));
      if (typeof value === "string") el.textContent = value;
    });
    var metaDesc = document.querySelector('meta[name="description"]');
    if (metaDesc && strings.meta) metaDesc.setAttribute("content", strings.meta.description);
    document.querySelectorAll(".langswitch button").forEach(function (btn) {
      btn.setAttribute("aria-pressed", String(btn.getAttribute("data-lang") === state.lang));
    });
  }

  // Counted from formats.json so the headline numbers cannot drift from the map.
  function renderStats() {
    var items = [];
    state.formats.categories.forEach(function (cat) { items = items.concat(cat.items); });
    var extensions = {};
    items.forEach(function (item) {
      if (!item.forward) return;
      extTokens(item.ext).forEach(function (token) {
        if (token.charAt(0) === ".") extensions[token.toLowerCase()] = true;
      });
    });
    var locale = state.lang === "zh" ? "zh-CN" : state.lang;
    function show(id, n) { document.getElementById(id).textContent = n.toLocaleString(locale); }
    show("stat-formats", items.filter(function (item) { return item.forward; }).length);
    show("stat-extensions", Object.keys(extensions).length);
    show("stat-reverse", items.filter(function (item) { return item.reverse; }).length);
  }

  function renderCards(gridId, items) {
    var grid = document.getElementById(gridId);
    grid.innerHTML = "";
    (items || []).forEach(function (item) {
      var card = document.createElement("div");
      card.className = "usecase-card";
      card.innerHTML =
        '<div class="ico">' + item.icon + "</div>" +
        "<h3></h3><p></p>";
      card.querySelector("h3").textContent = item.title;
      card.querySelector("p").textContent = item.description;
      grid.appendChild(card);
    });
  }

  function renderUseCases() {
    var strings = state.content.strings[state.lang];
    renderCards("why-grid", strings.whySection.items);
    renderCards("usecases-grid", strings.useCasesSection.items);
  }

  function renderContract() {
    var strings = state.content.strings[state.lang];
    var list = document.getElementById("contract-list");
    list.innerHTML = "";
    (strings.forAISection.contractItems || []).forEach(function (text) {
      var li = document.createElement("li");
      li.textContent = text;
      list.appendChild(li);
    });
  }

  function renderTierLegend() {
    var strings = state.content.strings[state.lang];
    var wrap = document.getElementById("tier-legend");
    wrap.innerHTML = "";
    ["A", "B", "C"].forEach(function (tier) {
      var chip = document.createElement("div");
      chip.className = "tier-chip";
      var badge = document.createElement("div");
      badge.className = "tier-badge " + tier;
      badge.textContent = tier;
      var span = document.createElement("span");
      span.textContent = state.formats.tiers[tier][state.lang];
      chip.appendChild(badge);
      chip.appendChild(span);
      wrap.appendChild(chip);
    });
  }

  function tierPill(value) {
    if (!value) {
      var none = document.createElement("span");
      none.className = "tier-pill none";
      none.textContent = "—";
      return none;
    }
    var pill = document.createElement("span");
    var letter = value.trim().charAt(0);
    pill.className = "tier-pill " + (["A", "B", "C"].indexOf(letter) >= 0 ? letter : "none");
    pill.textContent = value;
    return pill;
  }

  function renderCategoryFilter() {
    var strings = state.content.strings[state.lang];
    var wrap = document.getElementById("category-filter");
    wrap.innerHTML = "";
    var allBtn = document.createElement("button");
    allBtn.type = "button";
    allBtn.textContent = strings.formatsSection.filterAll;
    allBtn.setAttribute("aria-pressed", String(state.activeCategory === "all"));
    allBtn.addEventListener("click", function () {
      state.activeCategory = "all";
      render();
    });
    wrap.appendChild(allBtn);
    state.formats.categories.forEach(function (cat) {
      var btn = document.createElement("button");
      btn.type = "button";
      btn.textContent = cat.name[state.lang];
      btn.setAttribute("aria-pressed", String(state.activeCategory === cat.id));
      btn.addEventListener("click", function () {
        state.activeCategory = cat.id;
        render();
      });
      wrap.appendChild(btn);
    });
  }

  function renderFormatsTable() {
    var body = document.getElementById("formats-body");
    body.innerHTML = "";
    var cats = state.formats.categories.filter(function (c) {
      return state.activeCategory === "all" || state.activeCategory === c.id;
    });
    cats.forEach(function (cat) {
      var headRow = document.createElement("tr");
      headRow.className = "cat-heading";
      var headCell = document.createElement("td");
      headCell.colSpan = 4;
      headCell.textContent = cat.name[state.lang];
      headRow.appendChild(headCell);
      body.appendChild(headRow);

      cat.items.forEach(function (item) {
        var row = document.createElement("tr");

        var extCell = document.createElement("td");
        extCell.className = "ext";
        extTokens(item.ext).forEach(function (token, i) {
          if (i > 0) extCell.appendChild(document.createTextNode(" "));
          var code = document.createElement("code");
          code.textContent = token;
          extCell.appendChild(code);
        });
        row.appendChild(extCell);

        var fwdCell = document.createElement("td");
        fwdCell.appendChild(tierPill(item.forward));
        row.appendChild(fwdCell);

        var revCell = document.createElement("td");
        revCell.appendChild(tierPill(item.reverse));
        row.appendChild(revCell);

        var noteCell = document.createElement("td");
        noteCell.className = "notes-cell";
        noteCell.textContent = item.note[state.lang];
        row.appendChild(noteCell);

        body.appendChild(row);
      });
    });
  }

  function renderGallery() {
    var strings = state.content.strings[state.lang];
    var grid = document.getElementById("gallery-grid");
    grid.innerHTML = "";
    var seen = {};
    state.formats.categories.forEach(function (cat) {
      cat.items.forEach(function (item) {
        if (!item.sample) return;
        var samples = Array.isArray(item.sample) ? item.sample : [item.sample];
        var tokens = extTokens(item.ext);
        samples.forEach(function (sampleKey, idx) {
          if (seen[sampleKey]) return;
          seen[sampleKey] = true;
          var card = document.createElement("div");
          card.className = "gallery-card";
          var svgPath = "assets/samples/" + sampleKey + "/page-0001.svg";
          var sourceFile = SAMPLE_SOURCE_FILES[sampleKey];
          card.innerHTML =
            '<div class="gallery-thumb"><img alt="" src="' + svgPath + '"></div>' +
            '<div class="gallery-body">' +
            "<h4></h4>" +
            '<div class="ext-tag"></div>' +
            '<div class="gallery-links"></div>' +
            "</div>";
          card.querySelector("h4").textContent = (samples.length > 1 ? tokens[idx] : firstExt(item.ext)) || sampleKey;
          card.querySelector(".ext-tag").textContent = item.note[state.lang].slice(0, 64) + (item.note[state.lang].length > 64 ? "…" : "");
          var links = card.querySelector(".gallery-links");
          var a1 = document.createElement("a");
          a1.href = svgPath;
          a1.target = "_blank";
          a1.rel = "noopener";
          a1.textContent = strings.gallerySection.viewSvg;
          links.appendChild(a1);
          if (sourceFile) {
            var a2 = document.createElement("a");
            a2.href = REPO_RAW + sourceFile;
            a2.target = "_blank";
            a2.rel = "noopener";
            a2.textContent = strings.gallerySection.viewSource;
            links.appendChild(a2);
          }
          grid.appendChild(card);
        });
      });
    });
  }

  var USAGE_TAB_ORDER = ["cli", "node", "python", "rust"];

  function renderUsageTabs() {
    var strings = state.content.strings[state.lang];
    var tabLabels = strings.usageSection.tabs;
    var wrap = document.getElementById("usage-tabs");
    wrap.innerHTML = "";
    USAGE_TAB_ORDER.forEach(function (key) {
      var btn = document.createElement("button");
      btn.type = "button";
      btn.setAttribute("role", "tab");
      btn.setAttribute("aria-selected", String(state.activeUsageTab === key));
      btn.textContent = tabLabels[key];
      btn.addEventListener("click", function () {
        state.activeUsageTab = key;
        renderUsageTabs();
        renderUsagePanels();
      });
      wrap.appendChild(btn);
    });
  }

  function renderUsagePanels() {
    var wrap = document.getElementById("usage-panels");
    wrap.innerHTML = "";
    USAGE_TAB_ORDER.forEach(function (key) {
      var panel = document.createElement("div");
      panel.className = "usage-panel";
      panel.hidden = state.activeUsageTab !== key;
      (state.content.usageSnippets[key] || []).forEach(function (snippet) {
        var card = document.createElement("div");
        card.className = "usage-snippet";
        var heading = document.createElement("h4");
        var langTag = document.createElement("span");
        langTag.className = "lang-tag";
        langTag.textContent = snippet.lang;
        heading.appendChild(document.createTextNode(snippet.caption[state.lang]));
        heading.appendChild(langTag);
        var pre = document.createElement("pre");
        var code = document.createElement("code");
        code.textContent = snippet.code;
        pre.appendChild(code);
        card.appendChild(heading);
        card.appendChild(pre);
        panel.appendChild(card);
      });
      wrap.appendChild(panel);
    });
  }

  function render() {
    applyI18n();
    renderStats();
    renderUseCases();
    renderContract();
    renderTierLegend();
    renderCategoryFilter();
    renderFormatsTable();
    renderGallery();
    renderUsageTabs();
    renderUsagePanels();
  }

  function setLang(lang) {
    state.lang = lang;
    try { localStorage.setItem("docsvg-lang", lang); } catch (e) {}
    var url = new URL(location.href);
    url.searchParams.set("lang", lang);
    history.replaceState(null, "", url);
    render();
  }

  document.querySelectorAll(".langswitch button").forEach(function (btn) {
    btn.addEventListener("click", function () {
      setLang(btn.getAttribute("data-lang"));
    });
  });

  Promise.all([
    fetch("assets/data/content.json").then(function (r) { return r.json(); }),
    fetch("assets/data/formats.json").then(function (r) { return r.json(); })
  ]).then(function (results) {
    state.content = results[0];
    state.formats = results[1];
    state.lang = detectDefaultLang(state.content);
    render();
  }).catch(function (err) {
    console.error("document-svg site: failed to load data", err);
  });
})();
