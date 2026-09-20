# document-svg for Node.js

Preview guides: [日本語](docs/preview.ja.md) · [English](docs/preview.en.md) · [简体中文](docs/preview.zh-CN.md)

Node-API bindings for the Rust `document-svg` converter.

Published as [`document-svg` on npm](https://www.npmjs.com/package/document-svg).
The project also provides a [Python package on PyPI](https://pypi.org/project/document-svg/)
and a [Rust crate and CLI on crates.io](https://crates.io/crates/document-svg).

## Install

Requires Node.js 18+. Run this in your application directory:

```sh
npm install document-svg
```

TypeScript declarations are included. Keep optional dependencies enabled: npm
selects a native package for your OS and CPU. This is a Node.js library, not an
installation of the `docsvg` CLI. Conversion runs server-side or in Electron's
main process; only the `/preview-ui` helper is browser-compatible.

## Convert a document

```js
const { convert } = require("document-svg")

async function main() {
  const report = await convert("slides.pptx", "output", { jobs: 4 })
  console.log(report.pageCount)

  const fidelity = await convert("input.pdf", "output-fidelity", {
    outlineEmbeddedPdfText: true,
  })
  console.log(fidelity.warnings)
}

main().catch(console.error)
```

Pass `embedDrawioSource: true` when converting a draw.io file to keep a copy of
the diagram in each SVG, so `reverse()` can restore the editable diagram from it.

Package SVG pages into PPTX, DOCX, XLSX, draw.io, AutoCAD DXF, CNC G-code, Gerber RS-274X, or HP-GL:

```js
const { reverse } = require("document-svg")

async function main() {
  const report = await reverse("svg-pages", "slides.pptx")
  console.log(report.pageCount)

  // Or export to CAD DXF / CNC G-code / Gerber / HP-GL:
  await reverse("drawing.svg", "output.dxf")
  await reverse("toolpath.svg", "output.gcode")
  await reverse("pcb_top.svg", "output.gbr")
  await reverse("plot.svg", "output.plt")
}

main().catch(console.error)
```

The input is a single SVG or a directory of SVG pages. This does not reconstruct
the original paragraphs, cells, formulas or other Office application semantics.

The preview API also accepts legacy Visio CFB (`.vsd`, `.vss`, `.vst`, `.vsw`),
Apple iWork packages (`.pages`, `.numbers`, `.key`), Microsoft Access ACE/Jet
headers (`.accdb`, `.accde`, `.mdb`, `.mde`), Dassault 3DXML (`.3dxml`), HDF5/CGNS (`.h5`, `.hdf5`,
`.hdf`, `.h5part`, `.cgns`), Exodus II (`.e`, `.exo`, `.ex2`, `.ex2m`, `.exii`),
Autodesk DWG headers (`.dwg`), and Rhino/OpenNURBS 3DM markers (`.3dm`).
These binary adapters expose bounded metadata only and never execute macros,
links, plug-ins, solver payloads, or embedded objects.

## TypeScript and application previews

`preview()` converts any supported input, including PDF/FDF/XFDF form data (`.fdf`, `.xfdf`), legacy Word Binary `.doc` and CFB `.dot` templates, text-only legacy PowerPoint Binary `.ppt`, legacy Excel `.xls`, Excel Binary Workbook `.xlsb`, and SYLK `.slk` and DIF `.dif`, FASTA/FASTQ `.fa`/`.fasta`/`.fq`/`.fastq`, GFF3/GTF `.gff`/`.gff3`/`.gtf`, BED/BEDGraph `.bed`/`.bedgraph`/`.bg`, VCF `.vcf`, SAM `.sam`, WIG/Wiggle `.wig`/`.wiggle`, MAF `.maf`, Newick phylogenetic trees `.nwk`/`.newick`/`.tree`, Stockholm alignments `.sto`/`.stockholm`, CLUSTAL alignments `.aln`/`.clustal`/`.clustalw`, NEXUS trees `.nex`/`.nexus`, GenBank flat files `.gb`/`.gbk`/`.genbank`, EMBL-Bank flat files `.embl`/`.emb`, UniProtKB/Swiss-Prot `.dat`/`.uniprot`/`.swissprot`, RIS bibliography `.ris`, SPICE/ngspice netlists `.cir`/`.sp`/`.spice`/`.ckt`/`.net`, KiCad legacy Eeschema schematics `.sch`, KiCad 6+ S-expression schematics `.kicad_sch`, LTspice schematics `.asc` (Version signature), Autodesk EAGLE XML schematics `.sch` (eagle signature), Microsoft Project XML (`.xml`/`.mspdi`), OOXML Transitional/Strict including Flat OPC (`.flatopc`, `.fopc`, `.flatopc.xml`), AASX (`.aasx`), OpenSCAD (`.scad`), AMF (`.amf`), PLMXML (`.plmxml`, `.plm.xml`), STEP-XML (`.stepxml`, `.stpx`), QIF (`.qif`, `.qif.xml`), B2MML/JDF/XJDF (`.b2mml`, `.jdf`, `.xjdf`), TMX/TBX (`.tmx`, `.tbx`), gbXML (`.gbxml`), FHIR XML (`.fhir.xml`) and Adobe IDML (`.idml`), OpenDocument, Visio `.vsdx`/`.vsdm`/`.vstx`/`.vstm` and legacy `.vdx`, iCalendar `.ics`, vCalendar `.vcs`, vCard `.vcf`/`.vcard`, MIME e-mail `.eml`, Apple Mail `.emlx`, Outlook `.msg`, MBOX `.mbox`, MHTML `.mht`/`.mhtml`, HTML/EPUB/FictionBook 2 (`.fb2`, `.fb2.zip`)/PalmDOC MOBI (`.mobi`, `.prc`, `.azw`), DocBook 4/5 (`.dbk`, `.docbook`), DITA topics/maps (`.dita`, `.ditamap`), PDB coordinate models (`.pdb`, `.ent`), HWPX (`.hwpx`), COLLADA (`.dae`), X3D (`.x3d`), XMind (`.xmind`), NIfTI (`.nii`, `.nii.gz`), FITS (`.fits`, `.fit`, `.fts`, `.fits.gz`), MRC (`.mrc`, `.map`, `.mrc.gz`), SQLite (`.sqlite`, `.sqlite3`, `.db`), mmCIF/PDBx (`.cif`, `.mmcif`), MOL2 (`.mol2`), RDF Turtle (`.ttl`, `.nt`, `.nq`), EPS/PostScript (`.eps`, `.ps`), JATS article XML (`.jats`, `.nxml`), TEI P5 scholarly XML (`.tei`, `.tei.xml`), ALTO OCR/layout XML (`.alto`, `.alto.xml`), MARCXML (`.marcxml`, `.marc.xml`), MARC21 ISO 2709 (`.marc`, `.iso2709`), PREMIS XML (`.premis`, `.premis.xml`), IIIF Presentation JSON (`.iiif.json`, `.manifest.json`), EAD finding-aid XML (`.ead`, `.ead.xml`), EAC-CPF authority XML (`.eac-cpf`, `.eac`), Dublin Core XML (`.dc.xml`, `.dublin.xml`), ISO 19115/19139 metadata XML (`.iso19115`, `.iso19139`, `.gmd.xml`), OASIS UBL 2.x business XML (`.ubl`, `.ubl.xml`), XBRL 2.1 instances (`.xbrl`, `.xbrl.xml`, `.xbrli`), LandXML 1.2 civil models (`.landxml`, `.landxml.xml`), MathML 3 formulas (`.mathml`, `.mathml.xml`, `.mml`), Adobe XMP metadata (`.xmp`, `.xmp.xml`), Adobe XDP/XFA packages (`.xdp`, `.xdp.xml`), Excel 2003 XML SpreadsheetML (`.spreadsheetml`, `.xmlss`, `.excel.xml`), Chemical Markup Language XML (`.cml`, `.cml.xml`), RDF/XML graphs (`.rdf`, `.rdf.xml`), S1000D Data Module XML (`.s1000d`, `.dmodule`), MODS XML (`.mods`, `.mods.xml`), SubRip `.srt`, WebVTT `.vtt`, TTML/DFXP `.ttml`/`.dfxp`, XLIFF `.xlf`/`.xliff`, Jupyter `.ipynb`, Quarto/R Markdown, reStructuredText `.rst`/`.rest`, Org-mode `.org`, GNU gettext catalogs `.po`/`.pot`, BibTeX `.bib`/`.bibtex`, generic JSON `.json` and RFC 7464 JSON Text Sequences (`.jsons`, `.jsonseq`, `.jsonl`), bounded ARFF datasets (`.arff`), OpenAPI/Swagger (`.openapi.json`, `.swagger.json`, `.openapi.yaml`, `.swagger.yaml`), AsyncAPI (`.asyncapi.json`, `.asyncapi.yaml`, `.asyncapi.yml`), JSON Schema (`.schema.json`, `.jsonschema`, `.schema.yaml`, `.schema.yml`), HAR (`.har`), WARC (`.warc`, `.warc.gz`), JSON-LD 1.1 (`.jsonld`, `.json-ld`), NetCDF classic (`.nc`, `.nc3`, `.cdf`), HDF5/CGNS (`.h5`, `.hdf5`, `.hdf`, `.h5part`, `.cgns`), and Exodus II (`.e`, `.exo`, `.ex2`, `.ex2m`, `.exii`), GraphML (`.graphml`), GEXF (`.gexf`), XGMML (`.xgmml`), Graph Modeling Language (`.gml`), CSV/TSV tables, TOML configuration (`.toml`), YAML 1.2 configuration (`.yaml`, `.yml`), generic XML (`.xml` fallback), Java Properties (`.properties`), BPMN 2.0 (`.bpmn`, `.bpmn2`), CMMN 1.1 case plans (`.cmmn`), DMN 1.1–1.5 decision tables (`.dmn`), ReqIF requirements (`.reqif`), XMI models (`.xmi`), raster PNG/JPEG/BMP/GIF/WebP, Netpbm PBM/PGM/PPM/PAM, standalone JPEG 2000 (`.jp2`, `.j2k`, `.j2c`, `.jpc`, `.jpx`), multi-page TIFF/BigTIFF, CBZ, glTF/GLB `.gltf`/`.glb`, OFF `.off`, IFC4 BIM `.ifc`/`.ifcxml`/`.ifczip` including mapped tessellation instances, buildingSMART BCFZIP issue packages `.bcfzip`, KiCad PCB `.kicad_pcb`, ASCII XYZ `.xyz`, PCL point clouds `.pcd`, ASTM E57 `.e57`, Leica PTS `.pts` and PTX `.ptx` point clouds, and ASPRS LAS/LAZ `.las`/`.laz`, ANSYS CDB `.cdb`, Abaqus `.inp`, LS-DYNA `.k`/`.key`, MEDIT `.mesh`/`.meshb`, Nastran Bulk Data `.bdf`/`.nas` and Output2 `.op2`, IPC-2581 (`.ipc2581`, `.ipc-2581`, `.cvg`) and Siemens JT (`.jt`), SU2 CFD `.su2`, OpenFOAM `.foam`, Tecplot ASCII `.dat`/`.tec`/`.tecplot`/`.tp`, EnSight Gold `.case`/`.geo`, PLOT3D ASCII `.p3d`/`.plot3d`/`.p3`, VRML97 `.wrl`/`.vrml`, UNV `.unv`, ESRI ASCII Grid heatmaps (`.asc`), dBASE III/III+ tables (`.dbf`), GeoPackage vector layers and EPSG:3857 PNG/JPEG tile pyramids (`.gpkg`), RFC 8142 GeoJSON Text Sequences (`.geojsons`, `.geojsonseq`) and newline-delimited `.geojsonl`, TopoJSON `.topojson`, GeoJSON/GeoRSS/GML/GPX/KML/KMZ, ESRI Shapefile `.shp`, and WKT/EWKT maps, text, diagrams and CAD files, in temporary storage and returns
complete SVG strings for every page. No output directory is required; temporary
files are removed before its Promise resolves.

The same API also covers CDA/CCD, ISO 20022, SBML, CellML, OCEL XML, EnergyPlus
IDF/EPW, RINEX and ACIS SAT inputs. These adapters expose bounded structural
metadata and keep clinical, financial, model, weather, GNSS and CAD values inert.

DICOM Part 10 `.dcm`/`.dicom` images produce one embedded-PNG SVG per frame. DICOM Structured Reports (`.sr.dcm`, `.dicom-sr` or SR SOP Class) produce bounded Content Sequence tables. JPEG 2000 Part 1 lossless/general (`.90`/`.91`) and the supported JPEG/RLE syntaxes are decoded; JPEG 2000 SIZ dimensions, components, sample precision, signedness and subsampling are checked before decode. Patient/study attributes are not written, but burned-in text in the pixels remains.
DICOMDIR (`DICOMDIR` or `.dicomdir`) follows record offsets and resolves safe File IDs under the DICOMDIR directory; referenced frames become sequential SVG pages.
DICOM Encapsulated PDF Storage is rendered when its SOP Class and `application/pdf` MIME type agree. The embedded stream is limited to 64 MiB and passed to the PDF renderer; DICOM attributes are omitted, while identifying text inside the PDF remains.
V2000 MOL/SDF (`.mol`, `.sdf`, `.sd`) previews use stored coordinates and explicit bonds, one molecule per page. They show common bond orders, charges, isotopes, and up/down wedge/hash marks; V3000 Sgroups and advanced chemistry semantics are omitted. Core V3000 atom/bond tables are supported.

EML prefers the first safe HTML body (otherwise plain text) and embeds bounded
PNG/JPEG Content-ID parts. Exact Content-Location matches are accepted within the
same or an enclosing `multipart/related` tree. CSS and image placement are approximated;
other attachments are omitted and remote resources are never fetched.

MHTML honors `multipart/related`'s `start` root (or the first part by default)
and resolves Content-Location image references in the same or an enclosing
related tree using HTML `<base href>`, MIME Content-Base/Content-Location, and
the `thismessage:/` fallback. Remote resources are never fetched.

```ts
import { preview } from "document-svg"

const report = await preview("slides.pptx", {
  maxPages: 100,
  maxSvgBytes: 64 * 1024 * 1024,
  maxTotalSvgBytes: 256 * 1024 * 1024,
})

const firstPageSvg = report.pages[0].svg
console.log("review required:", report.needsReview)
```

After sending the SVG to your frontend, use `document-svg/preview-ui` to create
a Blob URL for an `img`. This subpath does not load the native binding and can
be used in a browser renderer.

```ts
import {
  copySvgToClipboard,
  createSvgPreviewDataUrl,
  createSvgPreviewUrl,
  revokeSvgPreviewUrl,
} from "document-svg/preview-ui"

const previewUrl = createSvgPreviewUrl(firstPageSvg)
const portablePreviewUrl = createSvgPreviewDataUrl(firstPageSvg)

// <img src={previewUrl} alt="Office document preview" />
// Call revokeSvgPreviewUrl(previewUrl) when disposing of the view.
// Use portablePreviewUrl across boundaries that cannot share Blob URLs.

// Call from a user action, such as a button click handler.
const copiedFormat = await copySvgToClipboard(firstPageSvg)
console.log(`copied as ${copiedFormat}`)
```

`copySvgToClipboard()` copies `image/svg+xml` when supported, otherwise the full
SVG source as `text/plain`. Use `copySvgSourceToClipboard()` to always copy source
text. The Clipboard API may require a secure context (HTTPS or localhost) and
a user action such as a click.

See [`examples/SvgPreview.tsx`](examples/SvgPreview.tsx) for a React example with
Blob URL cleanup, a copy button, and success/failure feedback.

Conversion is a native API for Node.js, Electron's main process and server-side
TypeScript. It is not a browser-only WASM API.

The conversion runs on a libuv worker and does not block the JavaScript event
loop. Build the native package for the current platform with:

```bash
npm install
npm run build
```

Maintainers should follow the repository's
[release procedure](https://github.com/ryusui-hiro/document-svg/blob/main/docs/PUBLISHING.md)
to publish the root package together with its platform-specific dependencies.

V2000 RXN (`.rxn`) renders one bounded reaction diagram with up to two reactants and products; reaction conditions, agents, and chemistry validation are omitted.

Docker Compose files (`compose.yaml`, `compose.yml`, `docker-compose.yaml`, and `docker-compose.yml`) are previewed as an inert service inventory. Image/build, ports, dependencies, and volume/network/secret counts are shown; Docker daemon access, image pulls/builds, commands, healthchecks, interpolation, and secret/environment values are never executed or exposed.

GitHub Actions workflow YAML under `.github/workflows/` is previewed as an inert job plan; commands, expressions, secrets, permissions, artifacts, and runner operations are never executed.

JUnit-compatible XML reports are previewed as inert suite summaries; failure logs and test execution are never accessed.

SARIF 2.1.0 static-analysis reports are previewed as inert tool/rule severity summaries; messages, locations, and upload operations are never accessed.

Terraform JSON plans (`tfplan.json`, `.tfplan.json`) are previewed as inert resource action summaries; values and provider operations are never accessed.

CycloneDX BOMs (`bom.json`, `bom.xml`, `*.cdx.json`, `*.cdx.xml`) are previewed as inert component inventories; hashes, licenses, PURLs, vulnerability details, and external references are never accessed.

SPDX 2.x JSON/tag:value (`spdx.json`, `sbom.spdx.json`, `*.spdx.json`, `.spdx`, `.spdx.txt`) is previewed as an inert package/file inventory; checksums, licenses, PURLs, and external references are never accessed.

JaCoCo/Cobertura coverage XML (`jacoco.xml`, `cobertura.xml`, `coverage.xml`) is previewed as inert package counter summaries; source paths and execution data are never accessed.

LCOV tracefiles (`.info`, `lcov.info`, `coverage.info`) are previewed as inert file counter summaries with absolute paths reduced to basenames.

RFC 6902 JSON Patch (`.jsonpatch`, `.json-patch`, `.patch.json`) is previewed as inert ordered operations; values are omitted and patches are never applied.

RFC 7396 JSON Merge Patch (`.mergepatch`, `.json-merge-patch`, `.merge-patch.json`) is previewed as inert path actions; values are omitted and patches are never applied.

CSL-JSON citation data (`.csl.json`, `.csl-json`, `.cite.json`) is previewed as inert bibliography metadata; styles, links, and external resources are never resolved.

JSON Feed 1.0/1.1 (`.jsonfeed`, `.json-feed`, `.feed.json`) is previewed as inert item metadata; content and URLs are never fetched or executed.

CloudEvents JSON 1.0 (`.cloudevent.json`, `.cloud-event.json`, `.cloudevents.json`, `.ce.json`, `.cloudevent`, `.cloudevents`) is previewed as inert event metadata, including source host and data type/size; URI query values are masked and payloads, schemas and extension values are omitted. No URI or network resource is resolved.

HL7 FHIR R4 JSON (`.fhir.json`, `.fhirjson`, `.fhir`, `.bundle.fhir.json`, `.fhir-bundle.json`) is previewed as inert resource metadata; clinical values, narratives, identifiers, extension values and references are omitted, and no terminology or external resource is resolved.

Apache Avro JSON schemas/protocols (`.avsc`, `.avpr`, `.avro.json`, `.avro-schema.json`) are previewed as inert type and field metadata; defaults, docs, imports, code generation and RPC execution are never evaluated.

OpenTelemetry OTLP JSON (`.otlp.json`, `.otlp.trace.json`, `.otlp.metrics.json`, `.otlp.logs.json`) is previewed as inert trace, metric and log signal metadata; attribute values and bodies are omitted and no telemetry is exported.

OCEL 2.0 JSON (`.jsonocel`, `.ocel.json`, `.ocel-json`) is previewed as inert event/object metadata with relationship counts; attribute values and qualifiers are omitted and no process discovery runs.

JSON:API 1.1 (`.jsonapi`, `.json-api.json`, `.jsonapi.json`) is previewed as inert primary/included resource metadata with attribute and relationship counts; values and link URLs are omitted and no API endpoint is contacted.

ASAM OpenDRIVE (`.xodr`, `.opendrive`, `.opendrive.xml`, `.open-drive.xml`) is previewed as a bounded planar road reference-line map; only line and constant-curvature arc geometry is rendered and simulation behavior remains inert.

ASAM OpenSCENARIO XML (`.xosc`, `.openscenario`, `.openscenario.xml`, `.open-scenario.xml`) is previewed as bounded scenario hierarchy metadata; catalogs, controllers, expressions and simulation behavior remain inert.

ASAM OpenLABEL 1.0 JSON (`.openlabel.json`, `.openlabel`, `.open-label.json`) is previewed as bounded annotation collection metadata; sensor payloads, coordinates, values, ontology URLs and external resources remain inert.

OGC CityJSON 1.x/2.0 (`.cityjson`, `.city.json`, `.cityjson.json`) is previewed as bounded CityObject and geometry metadata; coordinates, textures, attributes and external resources remain inert.

OASIS STIX 2.1 JSON (`.stix.json`, `.stix-json`, `.stix`) is previewed as inert threat-object metadata; patterns, descriptions, hashes, URLs and references are omitted and no TAXII/API/network operation runs.

ASAM OpenCRG (`.crg`, `.opencrg`) is previewed as bounded header metadata; road-surface payloads and file references are not decoded or followed.

OASIS TAXII 2.1 JSON (`.taxii.json`, `.taxii-json`, `.taxii`) is previewed as inert envelope/manifest metadata; STIX payloads, URLs and endpoints are omitted and no HTTP client runs.

WSDL 1.1/2.0 (`.wsdl`, `.wsdl.xml`) is previewed as inert service and operation metadata; imports, schemas, endpoints and SOAP/HTTP calls are never resolved or executed.

OPML 1.0/2.0 (`.opml`, `.opml.xml`) is previewed as inert hierarchical outline metadata; feed URLs, owner addresses and linked resources are never resolved or fetched.

RSS 2.0 and Atom 1.0 (`.rss`, `.atom`, `.georss` without GeoRSS geometry) are previewed as inert channel/item metadata; links, content payloads, enclosures and external resources are never resolved or fetched.

Apple Property Lists (`.plist`, `.plist.xml`) are previewed as inert XML key paths or binary `bplist00` object metadata; URL/secret values, data payloads and external resources are never resolved or executed.

TEI P5 scholarly XML (`.tei`, `.tei.xml`) is previewed as inert header and text-structure metadata; targets, facsimiles, external images, scripts and entities are never resolved or executed.

ALTO OCR/layout XML (`.alto`, `.alto.xml`) is previewed as inert page, line, OCR-word and confidence metadata; source images and external resources are never opened.

METS archive XML (`.mets`, `.mets.xml`) is previewed as inert file and structural-map metadata; location references, payloads and linked ALTO/image files are never followed.

MARCXML (`.marcxml`, `.marc.xml`) is previewed as inert record/field metadata; catalog URLs and external resources are never resolved.

MODS 3.x XML (`.mods`, `.mods.xml`) is previewed as inert bibliographic metadata; authority/location URLs, notes and external resources are never resolved.

MARC21 ISO 2709 (`.marc`, `.iso2709`) is previewed as inert leader and field metadata; URL values and catalog services are never resolved.

PREMIS 2.x/3.0 XML (`.premis`, `.premis.xml`) is previewed as inert preservation-entity metadata; checksums, URIs, rights payloads and preservation actions are never resolved or executed.

IIIF Presentation 2.1/3.0 manifests (`.iiif.json`, `.iiif-manifest.json`, `.manifest.json`) are previewed as inert Manifest/Canvas metadata; image services, thumbnails and annotation bodies are never fetched.

EAD2/EAD3 finding-aid XML (`.ead`, `.ead.xml`) is previewed as inert archival-component metadata; digital-object URLs and external resources are never resolved.

EAC-CPF authority XML (`.eac-cpf`, `.eac`) is previewed as inert identity and relation metadata; authority URIs, biographies and external resources are never resolved.

Dublin Core XML (`.dc.xml`, `.dublin.xml`, `.dublin-core.xml`) is previewed as inert descriptive metadata; identifier, relation, rights and URL payloads are never resolved.

ISO 19115/19139 metadata XML (`.iso19115`, `.iso19115.xml`, `.iso19139`, `.gmd.xml`) is previewed as an inert bounded summary; contacts, identifiers, abstract/lineage payloads and online-resource URLs are never resolved.

FDF form data (`.fdf`) is previewed as an inert bounded field hierarchy; password values, actions, submit targets, embedded files and external URLs are never resolved.

XFDF XML form data (`.xfdf`, `.xfdf.xml`) is previewed as an inert bounded field/value hierarchy; sensitive fields, PDF targets, rich-text payloads, actions and URLs are never resolved.

OASIS UBL 2.x business XML (`.ubl`, `.ubl.xml`) is previewed as an inert bounded document summary; party payloads, identifiers, amounts, account data, attachments and URLs are never resolved.

XBRL 2.1 instances (`.xbrl`, `.xbrl.xml`, `.xbrli`) are previewed as an inert bounded fact/context/unit summary; taxonomy, entity identifier, linkbase and URL payloads are never resolved, and financial values may contain confidential information.

LandXML 1.2 civil models (`.landxml`, `.landxml.xml`) are previewed as an inert bounded summary; coordinate/design payloads, external schemas and survey references are never resolved.

MathML 3 formulas (`.mathml`, `.mathml.xml`, `.mml`) are previewed as inert bounded formula text and structure counts; annotations, scripts, URLs and external resources are never resolved.

Adobe XMP metadata (`.xmp`, `.xmp.xml`) is previewed as an inert bounded descriptive metadata summary; identifiers, thumbnails, URLs, private schemas and binary payloads are never resolved.

Adobe XDP/XFA packages (`.xdp`, `.xdp.xml`) are previewed as an inert bounded packet/field summary; values, embedded packets, scripts, submit actions and external resources are never resolved.

Excel 2003 XML SpreadsheetML (`.spreadsheetml`, `.xmlss`, `.excel.xml`) is previewed as an inert bounded worksheet/cell table; formulas, macros, styles and external links are never evaluated or resolved.

Chemical Markup Language XML (`.cml`, `.cml.xml`) is previewed as an inert bounded molecule/reaction/spectrum summary; coordinates, dictionaries, URLs, property payloads and chemistry calculations are never resolved.

RDF/XML graphs (`.rdf`, `.rdf.xml`) are previewed as inert bounded predicate/literal rows; subject/resource URLs, nested XML literals, vocabularies and linked resources are never resolved.

S1000D Data Module XML (`.s1000d`, `.dmodule`, `.dmodule.xml`) is previewed as inert technical-publication metadata; DM/ICN references, graphics and external resources are never resolved.
