// Behaviour for the static pages written by scripts/build-site.py.
// Every page already contains its full content; this only adds the format
// search, the code tabs, the sidebar, and redirects for older links.
(function () {
  "use strict";

  var body = document.body;
  var page = body.getAttribute("data-page");
  var lang = body.getAttribute("data-lang") || "en";
  var params = new URLSearchParams(location.search);
  var DESKTOP = window.matchMedia("(min-width: 861px)");

  // --- older links -----------------------------------------------------------

  // The first multi-page version chose the language with ?lang=; the pages now
  // live under ja/ and zh/.
  var wanted = params.get("lang");
  if (lang === "en" && (wanted === "ja" || wanted === "zh")) {
    params.delete("lang");
    var rest = params.toString();
    var file = location.pathname.split("/").pop() || "index.html";
    location.replace(wanted + "/" + file + (rest ? "?" + rest : "") + location.hash);
    return;
  }

  // Section anchors of the old single-page site.
  var OLD_ANCHORS = {
    usecases: "use-cases.html",
    "for-ai": "ai.html",
    formats: "formats.html",
    samples: "samples.html",
    install: "start.html#choose",
    usage: "start.html#convert"
  };
  var old = location.hash.slice(1);
  if (page === "home" && OLD_ANCHORS[old]) {
    location.replace(OLD_ANCHORS[old]);
    return;
  }

  // --- sidebar ---------------------------------------------------------------

  var sidenav = document.querySelector(".sidenav");
  if (sidenav) {
    sidenav.open = DESKTOP.matches;
    DESKTOP.addEventListener("change", function () { sidenav.open = DESKTOP.matches; });
    sidenav.addEventListener("click", function (event) {
      if (event.target.tagName === "A" && !DESKTOP.matches) sidenav.open = false;
    });

    var links = sidenav.querySelectorAll("a[data-target]");
    if ("IntersectionObserver" in window) {
      var observer = new IntersectionObserver(function (entries) {
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
  }

  // --- code tabs -------------------------------------------------------------

  document.querySelectorAll(".usage").forEach(function (usage) {
    var tabs = usage.querySelectorAll('[role="tab"]');
    tabs.forEach(function (tab) {
      tab.addEventListener("click", function () {
        tabs.forEach(function (other) {
          var selected = other === tab;
          other.setAttribute("aria-selected", String(selected));
          document.getElementById(other.getAttribute("aria-controls")).hidden = !selected;
        });
      });
    });
  });

  // --- format search ---------------------------------------------------------

  var tools = document.getElementById("format-tools");
  if (tools) {
    var input = document.getElementById("format-search");
    var reverse = document.getElementById("reverse-only");
    var count = tools.querySelector(".result-count");
    var empty = document.getElementById("format-empty");
    var groups = document.querySelectorAll(".format-group");
    var locale = lang === "zh" ? "zh-CN" : lang;

    var filter = function () {
      var words = input.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
      var total = 0;
      groups.forEach(function (group) {
        var shown = 0;
        group.querySelectorAll("tbody tr").forEach(function (row) {
          var text = (row.textContent + " " + row.getAttribute("data-search")).toLowerCase();
          var match = (!reverse.checked || row.hasAttribute("data-reverse")) &&
            words.every(function (word) { return text.indexOf(word) >= 0; });
          row.hidden = !match;
          if (match) shown += 1;
        });
        group.hidden = shown === 0;
        group.querySelector(".count-chip").textContent = String(shown);
        total += shown;
      });
      count.textContent = count.getAttribute("data-template").replace("{n}", total.toLocaleString(locale));
      empty.hidden = total > 0;
    };

    reverse.checked = params.get("reverse") === "1";
    input.addEventListener("input", filter);
    reverse.addEventListener("change", filter);
    tools.hidden = false;
    filter();
  }
})();
