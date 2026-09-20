# Samples

See what `docsvg` takes as input and what it produces, with real files.

[日本語](README.ja.md). A larger, trilingual (English/Japanese/Chinese) tour of
every supported format — with a live gallery — is published from
[`site/`](../site/) to GitHub Pages; see [`../site/README.md`](../site/README.md)
for how that site is built and deployed.

| | Input | Output |
|---|---|---|
| PDF | [`source/sample.pdf`](source/sample.pdf) | [`svg/pdf/`](svg/pdf/) — 2 pages |
| PPTX | [`source/sample.pptx`](source/sample.pptx) | [`svg/pptx/`](svg/pptx/) — 2 pages |
| XLSX | [`source/sample.xlsx`](source/sample.xlsx) | [`svg/xlsx/`](svg/xlsx/) — 4 pages |
| DOCX | [`source/sample.docx`](source/sample.docx) | [`svg/docx/`](svg/docx/) — 2 pages |
| Microsoft Project XML | [`source/sample.project.xml`](source/sample.project.xml) | [`svg/xml/`](svg/xml/) — 1 page |
| draw.io | [`source/sample.drawio`](source/sample.drawio) | [`svg/drawio/`](svg/drawio/) — 3 pages |
| DXF | [`source/sample.dxf`](source/sample.dxf) | [`svg/dxf/`](svg/dxf/) — 1 page |
| Gerber | [`source/sample.gbr`](source/sample.gbr) | [`svg/gbr/`](svg/gbr/) — 1 page |
| HP-GL | [`source/sample.plt`](source/sample.plt) | [`svg/plt/`](svg/plt/) — 1 page |
| G-code | [`source/sample.nc`](source/sample.nc) | [`svg/nc/`](svg/nc/) — 1 page |
| Excellon | [`source/sample.drl`](source/sample.drl) | [`svg/drl/`](svg/drl/) — 1 page |
| STL | [`source/sample.stl`](source/sample.stl) | [`svg/stl/`](svg/stl/) — 1 page |
| OBJ | [`source/sample.obj`](source/sample.obj) | [`svg/obj/`](svg/obj/) — 1 page |
| VTK | [`source/sample.vtk`](source/sample.vtk) | [`svg/vtk/`](svg/vtk/) — 1 page |
| Gmsh MSH | [`source/sample.msh`](source/sample.msh) | [`svg/msh/`](svg/msh/) — 1 page |
| SU2 CFD mesh | [`source/sample.su2`](source/sample.su2) | [`svg/su2/`](svg/su2/) — 1 page |
| STEP | [`source/sample.step`](source/sample.step) | [`svg/step/`](svg/step/) — 1 page |
| IFC4 | [`source/sample.ifc`](source/sample.ifc) | [`svg/ifc/`](svg/ifc/) — 1 page |
| IFCZIP | [`source/sample.ifczip`](source/sample.ifczip) | [`svg/ifczip/`](svg/ifczip/) — 1 page |
| Graphviz DOT | [`source/sample.dot`](source/sample.dot) | [`svg/dot/`](svg/dot/) — 1 page |
| Mermaid | [`source/sample.mmd`](source/sample.mmd) | [`svg/mmd/`](svg/mmd/) — 1 page |
| LaTeX math | [`source/sample.tex`](source/sample.tex) | [`svg/tex/`](svg/tex/) — 1 page |
| Chart JSON | [`source/sample.chart`](source/sample.chart) | [`svg/chart/`](svg/chart/) — 1 page |
| Markdown table | [`source/sample.md`](source/sample.md) | [`svg/md/`](svg/md/) — 1 page |
| QR code | [`source/sample.qr`](source/sample.qr) | [`svg/qr/`](svg/qr/) — 1 page |
| Raster vectorize | [`source/sample.png`](source/sample.png) | [`svg/png/`](svg/png/) — 1 page |
| ESRI ASCII Grid | [`source/sample-elevation.asc`](source/sample-elevation.asc) | [`svg/asc/`](svg/asc/) — 1 page |
| dBASE III/III+ table | [`source/sample-parcels.dbf`](source/sample-parcels.dbf) | [`svg/dbf/`](svg/dbf/) — 1 page |
| TOML configuration | [`source/sample.toml`](source/sample.toml) | [`svg/toml/`](svg/toml/) — 1 page |
| YAML 1.2 configuration | [`source/sample.yaml`](source/sample.yaml) | [`svg/yaml/`](svg/yaml/) — 2 pages |
| Generic XML configuration | [`source/sample-config.xml`](source/sample-config.xml) | [`svg/generic-xml/`](svg/generic-xml/) — 1 page |
| Java Properties configuration | [`source/sample.properties`](source/sample.properties) | [`svg/properties/`](svg/properties/) — 1 page |
| BPMN 2.0 process | [`source/sample.bpmn`](source/sample.bpmn) | [`svg/bpmn/`](svg/bpmn/) — 1 page |
| DMN 1.5 decision table | [`source/sample.dmn`](source/sample.dmn) | [`svg/dmn/`](svg/dmn/) — 1 page |
| CMMN 1.1 case plan | [`source/sample.cmmn`](source/sample.cmmn) | [`svg/cmmn/`](svg/cmmn/) — 1 page |
| ReqIF 1.0.1 requirements | [`source/sample.reqif`](source/sample.reqif) | [`svg/reqif/`](svg/reqif/) — 1 page |
| XMI 2.1 UML model | [`source/sample.xmi`](source/sample.xmi) | [`svg/xmi/`](svg/xmi/) — 1 page |
| GeoPackage | [`source/sample.gpkg`](source/sample.gpkg) | [`svg/gpkg/`](svg/gpkg/) — 2 pages |
| GeoJSON Text Sequence | [`source/sample.geojsons`](source/sample.geojsons) | [`svg/geojsons/`](svg/geojsons/) — 1 page |
| TopoJSON | [`source/sample.topojson`](source/sample.topojson) | [`svg/topojson/`](svg/topojson/) — 1 page |
| JSON Text Sequence | [`source/sample.jsons`](source/sample.jsons) | [`svg/jsons/`](svg/jsons/) — 1 page |

Open `svg/*/page-NNNN.svg` in a compatible browser. Images are embedded, so a
single file is enough to view a page on its own. Text rendering depends on the
fonts available in your viewer. `conversion.json` in the same folder records
page count, elapsed time, and warnings for that sample.

## What each sample exercises

- **PDF** — text, filled Bézier curves, stroked lines, an embedded RGB raster
  image, and multiple pages. The font is Helvetica declared without `/Widths`,
  which checks that standard-14 font metrics are applied correctly.
- **PPTX** — slide master, theme colors, shape fills, bullet lists, and a 16:9
  slide size.
- **XLSX** — 180 rows of data, formatted numbers, merged cells, and
  `<pageSetup>` paper settings. It does not fit on one sheet of paper, so it
  **splits into 4 pages, the same way printing it would**.
- **DOCX** — heading styles, a bordered table, an explicit page break, and a
  header/footer (the footer's `PAGE` field resolves to the page number).
- **Microsoft Project XML** — a bounded Gantt preview with summary/task bars,
  progress overlays, milestones, predecessor arrows, and an unscheduled row.
- **draw.io** — 2 pages: shapes and fills, shadows, orthogonal and curved
  connectors with arrowheads, edge labels, and swimlanes with nested children.
  The input is kept as readable XML, but the same converter equally handles
  the compressed `<diagram>` payload the editor writes by default.
- **DXF** — an architectural floor plan. Layer structure (WALLS, DOORS,
  FURNITURE, TEXT, ...), LINE/ARC/CIRCLE/TEXT entities, and the AutoCAD Color
  Index (ACI) palette. Also demonstrates the DXF R12 reverse round trip via
  `docsvg reverse <svg> --output <new.dxf>`.
- **IFC4** — two building proxies share a tessellated body and use separate local placements, under a minimal project/site/building/storey hierarchy.
- **IFCZIP** — the same IFC4 model in a root-level STEP file inside a ZIP archive, exercising bounded package inspection without extraction.
- **SU2 CFD mesh** — a triangular 2D mesh with zero-based connectivity and four named boundary markers, highlighted as orange edges in the preview.
- **Gerber (RS-274X)** — a printed circuit board (PCB) pattern: aperture
  definitions (circular, rectangular, oval), copper trace routing, SMD pads,
  through-hole vias, G36/G37 polygon copper fill, and a substrate preview.
- **HP-GL / HP-GL/2** — a plotter-language drawing: pen select (SP), absolute
  coordinate plotting (PA/PD/PU), arcs (AA), circles (CI), pen width (PW), and
  an 8-color pen palette.
- **Graphviz DOT** — a microservices / cloud-infrastructure diagram: DAG
  topological sort, hierarchical layout, rounded nodes, Bézier connectors, and
  arrowheads. `docsvg reverse <svg> --output <restored.dot>` fully restores an
  embedded DOT source, or falls back to geometric reconstruction from an SVG
  that has none.
- **Mermaid** — a sequence diagram (an OAuth2 login flow) and a flowchart:
  participant lifelines, sync/async message arrows, and automatic step
  numbering. `docsvg reverse <svg> --output <restored.mmd>` converts back to
  Mermaid DSL.
- **LaTeX math** — a probability-density formula: fractions (`\frac`), square
  roots (`\sqrt`), sub/superscripts, Greek letters, and a summation box model
  (`\sum`). `docsvg reverse <svg> --output <restored.tex>` converts back to
  LaTeX source.
- **Chart (JSON)** — bar, line, and pie charts: axis ticks, gridlines,
  per-series coloring, and a legend. `docsvg reverse <svg> --output
  <restored.csv>` extracts the underlying numbers and labels as CSV;
  `--output <chart.png>` rasterizes directly via `resvg`.
- **Markdown table** — a GFM-style table (an instance specification / pricing
  sheet): header cells, alignment, alternating row backgrounds, and borders.
  `docsvg reverse <svg> --output <restored.md>` fully restores the Markdown
  table text.
- **QR code** — a vector QR matrix as one clean path
  (`M ... h 12 v 12 ... Z`). `docsvg reverse` can also emit
  `<QRCode.tsx>` (React), `<QRCode.vue>` (Vue 3 SFC), or `<qrcode.datauri>`
  (Base64 data URI).
- **Raster vectorize** — a handwriting-style signature / bitmap image
  (PNG/JPEG): edge and luminance thresholding with run-length (RLE) vector
  path tracing.
- **ESRI ASCII Grid** — a small elevation raster with a transparent NoData
  coastline, continuous color ramp, min/max legend, and origin/cell-size
  metadata. It is generated by the reproducible sample script.
- **dBASE III/III+** — a parcel attribute table with numeric, text, logical, and
  date fields. It exercises Windows-1252 decoding and deleted-record filtering;
  the input and SVG are reproducibly generated by the sample script.
- **TOML** — a deployment configuration with nested tables, arrays, an array of
  target tables, booleans, numeric values, and a timestamp. Values are previewed
  as inert data; no setting or string is evaluated.
- **YAML 1.2** — a multi-document deployment configuration with nested mappings, sequences, an alias, and a custom tag. Aliases remain placeholders and tags are inert; no include or configuration is executed.
- **Generic XML** — an application configuration with namespaces, attributes, escaped text, and empty elements. It is rendered as inert rows; DTDs and external resources are not loaded.
- **Java Properties** — ISO-8859-1 text with escaped Unicode, a continued value, an inert `${HOME}` placeholder, and a duplicate key whose final value wins.
- **BPMN 2.0** — an order-fulfillment workflow with a start/end event, user/service tasks, an exclusive gateway, labeled branches, sequence flows, and explicit Diagram Interchange coordinates.
- **DMN 1.5** — a loan-approval decision table with two inputs, three rules, a hit policy, local input requirements, and FEEL expressions shown without evaluation.
- **CMMN 1.1** — a claims case plan with a case boundary, plan-item tasks, CMMNDI waypoint connectors, and inert sentry/lifecycle semantics.
- **ReqIF 1.0.1** — a vehicle-braking requirements exchange with typed values, hierarchy, an XHTML value flattened to text, and a local derive relation.
- **XMI 2.1** — a UML model with nested package/classes, attributes, an operation, and an association. Model IDs/types and references remain inert text.
- **GeoPackage** — a read-only SQLite database with a vector layer containing
  two polygons (one with an interior ring) and a raster tile layer, exercising
  GeoPackageBinary/WKB parsing, tile-matrix extents, geometry-only queries,
  bounded PNG re-encoding, and even-odd hole rendering.
- **GeoJSON Text Sequence** — RFC 8142 RS-delimited Feature, LineString, and
  FeatureCollection records, exercising heterogeneous records and property
  omission while assembling one bounded map page.
- **TopoJSON** — quantized coordinates, a shared district boundary referenced
  in reverse, a transformed Point and omitted feature properties.
- **JSON Text Sequence** — RFC 7464 Record Separator framing around objects,
  an array, and a top-level scalar; records are laid out in order without
  execution.
- **SVG transform** — reshaping an SVG for a specific destination with
  `docsvg transform <svg> --output <out.svg> --minify --monochrome "#1e293b"
  --responsive --precision 1`.

The round trip is testable too: convert with `--embed-drawio-source` and the
resulting SVG restores to the original drawing (editable shapes, not an
image) via `docsvg reverse <output-dir> --output <new>.drawio`.

Page 3 of the draw.io sample is a shape-library example. The first two shapes
come from [`source/sample-stencils.xml`](source/sample-stencils.xml) (an
mxStencil library written for this repository); the third carries its own
`shape=stencil(...)`, so no external library is needed for it.

```bash
docsvg samples/source/sample.drawio --output out/ --stencils samples/source/sample-stencils.xml
```

draw.io's own shape libraries (AWS, Azure, GCP, ...) are not bundled here, but
they use the same format — point `--stencils` at draw.io's `stencils`
directory and they render as-is.

## Regenerating

Sample inputs are not brought in from anywhere else — this repository's own
scripts generate them, so they carry the same license as the repository and
anyone can reuse them freely.

```bash
cargo build --release
python3 scripts/make_samples.py
```

`scripts/make_samples.py` rewrites the files in `source/` and runs them
through `docsvg` to rebuild `svg/`. Run it after changing the renderer to see
the output difference as a plain diff.

> `elapsed_ms` inside `conversion.json` changes on every run. That showing up
> in the diff is expected and not a problem.

`provenance.json` records the SHA-256 of each generated input file. The
publication check only allows samples that match it, and the test suite
confirms every sample is reproducible from the generation script.

## Showcase samples for the website

The [GitHub Pages site](../site/) additionally shows a second, hand-written
set of source files under `source/` — realistic, topical examples rather than
the generator's minimal regression fixtures, covering formats the generator
above does not yet build (`.adoc`, `.csv`, `.d2`, `.html`, `.puml`, `.ply`) and
richer variants of formats it does (`sample-architecture.dot`,
`sample-math.tex`, `sample-metrics.chart.json`, `sample-qr.qr`,
`sample-sequence.mmd`, `sample-signature.png`, `sample-table.md`). These are
not part of `provenance.json` or `make_samples.py` — they are converted
directly with `docsvg` and their SVG output is copied into
`site/assets/samples/` for the gallery. See
[`../site/assets/data/formats.json`](../site/assets/data/formats.json) for the
full mapping from sample key to source file.
