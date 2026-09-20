# document-svg for Python

Python bindings for the Rust `document-svg` converter.

Published as [`document-svg` on PyPI](https://pypi.org/project/document-svg/).
The project also provides a [Node.js package on npm](https://www.npmjs.com/package/document-svg)
and a [Rust crate and CLI on crates.io](https://crates.io/crates/document-svg).

## Install

Requires Python 3.10+ (GIL-enabled CPython). In your virtual environment:

```sh
python -m pip install document-svg
```

Install `document-svg`, but import `document_svg`. Matching prebuilt wheels do
not require Rust. Prebuilt wheels cover Windows, macOS and glibc-based Linux on
x64 and ARM64. Alpine/musl installations build from source and need Rust plus
native build tools; this release does not ship musllinux wheels.
This package provides a Python API, not the `docsvg` CLI.

Apple Mail `.emlx` messages use the declared byte count to isolate the embedded MIME message; the optional trailing property list is ignored.
Org-mode `.org` documents render headlines, lists, tables, source/example blocks, quote blocks and bounded local image links; Babel code, includes, table formulas and raw export blocks are never executed.
GNU gettext `.po`/`.pot` catalogs show message contexts, source/translated strings, plural forms, notes and fuzzy status; translations are displayed as text without evaluating format directives.
BibTeX `.bib`/`.bibtex` files show entry types, keys, and fields; string macros and BibTeX styles are not expanded or executed.
SubRip `.srt`, WebVTT `.vtt`, TTML/DFXP `.ttml`/`.dfxp`, and XLIFF `.xlf`/`.xliff` files render bounded text cues or localization source/target units. Styles and layout are approximated; active and external content is never loaded.

## Convert a document

DICOM Structured Reports (`.sr.dcm`, `.dicom-sr` or SR SOP Class) are rendered as inert Content Sequence tables; referenced objects and patient/study attributes are omitted, and the preview is not de-identification.

BCFZIP issue packages (`.bcfzip`) render buildingSMART project and topic metadata, including topic title/status/priority and comment/viewpoint/document-reference/component counts. Snapshots, IFC/model payloads, document URLs and collaboration actions remain inert; no package entry is extracted for execution or external lookup.

Flat OPC packages (`.flatopc`, `.fopc`, `.flatopc.xml`) are validated and rendered through the existing DOCX/XLSX/PPTX engines after bounded in-memory reconstruction. Macros, external relationships, URLs, active content and filesystem extraction remain inert.

AASX packages (`.aasx`) render bounded Asset Administration Shell relationship and specification metadata. Supplementary CAD/manual files, identifiers, values, URLs, signatures and encryption material remain inert.

DICOM Encapsulated PDF Storage is supported when its SOP Class and `application/pdf` MIME type agree. Its embedded stream is bounded at 64 MiB and rendered by the PDF engine; DICOM attributes are omitted, but identifying content inside the PDF remains.
V2000 MOL/SDF structures (`.mol`, `.sdf`, `.sd`) render one SVG page per molecule using the stored coordinates and bond graph. SDF data fields are omitted. Core V3000 atom/bond tables are supported, while Sgroups and advanced chemistry interpretation are omitted.
V2000 RXN files (`.rxn`) render one bounded reactant-to-product diagram with up to two components per side.

Binary metadata adapters also accept legacy Visio CFB (`.vsd`, `.vss`, `.vst`,
`.vsw`), Apple iWork packages (`.pages`, `.numbers`, `.key`), Microsoft Access
ACE/Jet (`.accdb`, `.accde`, `.mdb`, `.mde`), HDF5/CGNS, Exodus II, Autodesk DWG,
and Rhino/OpenNURBS 3DM. Their opaque payloads remain inert and are never
executed or followed.

The converter accepts the same format families as the Rust CLI, including Flat OPC (`.flatopc`, `.fopc`, `.flatopc.xml`), AASX (`.aasx`), OpenSCAD (`.scad`), AMF (`.amf`), PLMXML (`.plmxml`, `.plm.xml`), STEP-XML (`.stepxml`, `.stpx`), QIF (`.qif`, `.qif.xml`), B2MML/JDF/XJDF (`.b2mml`, `.jdf`, `.xjdf`), CDA/CCD (`.cda`, `.cda.xml`), ISO 20022 (`.iso20022.xml`), SBML (`.sbml`), CellML (`.cellml`), OCEL XML (`.xmlocel`), EnergyPlus IDF/EPW (`.idf`, `.epw`), RINEX (`.rnx`, `.obs`, `.nav`) and ACIS SAT (`.sat`), TMX/TBX (`.tmx`, `.tbx`), gbXML (`.gbxml`), FHIR XML (`.fhir.xml`) and Adobe IDML (`.idml`), Office Open XML Transitional/Strict DOCX, XLSX, and PPTX, legacy Word Binary `.doc` and CFB `.dot` templates, text-only legacy PowerPoint Binary `.ppt`, Microsoft Project XML (`.xml`/`.mspdi`), and DICOM Part 10 images (`.dcm`, `.dicom`) with JPEG 2000 Part 1 lossless/general transfer syntax support, DICOM Structured Reports (`.sr.dcm`, `.dicom-sr`), and DICOMDIR file sets (`DICOMDIR`, `.dicomdir`), legacy Excel `.xls`, Excel Binary Workbook `.xlsb`, and SYLK `.slk` and DIF `.dif`, FASTA/FASTQ `.fa`/`.fasta`/`.fq`/`.fastq`, GFF3/GTF `.gff`/`.gff3`/`.gtf`, BED/BEDGraph `.bed`/`.bedgraph`/`.bg`, VCF `.vcf`, SAM `.sam`, WIG/Wiggle `.wig`/`.wiggle`, MAF `.maf`, Newick phylogenetic trees `.nwk`/`.newick`/`.tree`, Stockholm alignments `.sto`/`.stockholm`, CLUSTAL alignments `.aln`/`.clustal`/`.clustalw`, NEXUS trees `.nex`/`.nexus`, GenBank flat files `.gb`/`.gbk`/`.genbank`, EMBL-Bank flat files `.embl`/`.emb`, UniProtKB/Swiss-Prot `.dat`/`.uniprot`/`.swissprot`, RIS bibliography `.ris`, SPICE/ngspice netlists `.cir`/`.sp`/`.spice`/`.ckt`/`.net`, KiCad legacy Eeschema schematics `.sch`, KiCad 6+ S-expression schematics `.kicad_sch`, LTspice schematics `.asc` (Version signature), Autodesk EAGLE XML schematics `.sch` (eagle signature), KiCad PCB S-expression boards (`.kicad_pcb`), generic JSON (`.json`) and RFC 7464 JSON Text Sequences (`.jsons`, `.jsonseq`, `.jsonl`), bounded ARFF datasets (`.arff`), OpenAPI/Swagger (`.openapi.json`, `.swagger.json`, `.openapi.yaml`, `.swagger.yaml`), AsyncAPI (`.asyncapi.json`, `.asyncapi.yaml`, `.asyncapi.yml`), JSON Schema (`.schema.json`, `.jsonschema`, `.schema.yaml`, `.schema.yml`), HAR (`.har`), WARC (`.warc`, `.warc.gz`), JSON-LD 1.1 (`.jsonld`, `.json-ld`), NetCDF classic (`.nc`, `.nc3`, `.cdf`), HDF5/CGNS (`.h5`, `.hdf5`, `.hdf`, `.h5part`, `.cgns`), and Exodus II (`.e`, `.exo`, `.ex2`, `.ex2m`, `.exii`), GraphML (`.graphml`), GEXF (`.gexf`), XGMML (`.xgmml`), Graph Modeling Language (`.gml`), CSV/TSV tables, TOML configuration (`.toml`), YAML 1.2 configuration (`.yaml`, `.yml`), generic XML (`.xml` fallback), Java Properties (`.properties`), BPMN 2.0 (`.bpmn`, `.bpmn2`), CMMN 1.1 case plans (`.cmmn`), DMN 1.1–1.5 decision tables (`.dmn`), ReqIF requirements (`.reqif`), XMI models (`.xmi`), ESRI ASCII Grid rasters (`.asc`), dBASE III/III+ attribute tables (`.dbf`), GeoPackage vector maps and EPSG:3857 PNG/JPEG tile layers (`.gpkg`), RFC 8142 GeoJSON Text Sequences (`.geojsons`, `.geojsonseq`) and newline-delimited `.geojsonl`, TopoJSON (`.topojson`), GeoJSON/GeoRSS/GML/GPX/KML/KMZ, ESRI Shapefile (`.shp`), and WKT/EWKT map geometry (`.jsons`, `.jsonseq`, `.jsonl`, `.gpkg`, `.geojsons`, `.geojsonseq`, `.geojsonl`, `.topojson`, `.geojson`, `.rss`, `.atom`, `.georss`, `.gml`, `.gpx`, `.kml`, `.kmz`, `.shp`, `.wkt`, `.ewkt`),
bounded raster inputs (PNG/JPEG/BMP/GIF/WebP), iCalendar calendars (`.ics`), legacy vCalendar events (`.vcs`), vCard contacts (`.vcf`, `.vcard`), MIME e-mail (`.eml`), Outlook messages (`.msg`),
MBOX archives (`.mbox`, one page per message), and MHTML web archives (`.mht`,
`.mhtml`). EML displays the first safe HTML or plain-text body and may embed
bounded PNG/JPEG Content-ID parts. Exact Content-Location matches are accepted
within the same or an enclosing multipart/related tree. CSS/layout is approximated and other attachments are omitted.
MSG displays common headers and a safe HTML or plain-text body without
expanding attachments. MBOX attachment payloads are omitted. MHTML honors the
related `start` root and may embed bounded Content-ID or URI-resolved
Content-Location PNG/JPEG parts from the same or an enclosing related scope. Relative MHTML URIs
use HTML `<base href>`, MIME Content-Base/Content-Location, and the
`thismessage:/` fallback; remote resources are never fetched. Visio Open XML drawings/templates
(`.vsdx`, `.vsdm`, `.vstx`, `.vstm`) and legacy
Visio XML drawings (`.vdx`), legacy Visio CFB (`.vsd`, `.vss`, `.vst`, `.vsw`), Apple iWork packages (`.pages`, `.numbers`, `.key`), Autodesk DWG headers (`.dwg`), Rhino/OpenNURBS 3DM markers (`.3dm`), and Microsoft Access ACE/Jet headers (`.accdb`, `.accde`, `.mdb`, `.mde`), Netpbm PBM/PGM/PPM/PAM (`.pbm`, `.pgm`, `.ppm`, `.pnm`, `.pam`), standalone JPEG 2000 (`.jp2`, `.j2k`, `.j2c`, `.jpc`, `.jpx`), multi-page TIFF/BigTIFF (`.tif` and `.tiff`), CBZ comic
archives (`.cbz`), FictionBook 2 (`.fb2`, `.fb2.zip`), PalmDOC/MOBI (`.mobi`, `.prc`, `.azw`), DocBook 4/5 (`.dbk`, `.docbook`), DITA topics/maps (`.dita`, `.ditamap`), PDB coordinate models (`.pdb`, `.ent`), HWPX (`.hwpx`), COLLADA (`.dae`), X3D (`.x3d`), XMind (`.xmind`), NIfTI (`.nii`, `.nii.gz`), FITS (`.fits`, `.fit`, `.fts`, `.fits.gz`), MRC (`.mrc`, `.map`, `.mrc.gz`), SQLite (`.sqlite`, `.sqlite3`, `.db`), mmCIF/PDBx (`.cif`, `.mmcif`), MOL2 (`.mol2`), RDF Turtle (`.ttl`, `.nt`, `.nq`), EPS/PostScript (`.eps`, `.ps`), JATS article XML (`.jats`, `.nxml`), TEI P5 scholarly XML (`.tei`, `.tei.xml`), ALTO OCR/layout XML (`.alto`, `.alto.xml`), MARCXML (`.marcxml`, `.marc.xml`), MARC21 ISO 2709 (`.marc`, `.iso2709`), PREMIS XML (`.premis`, `.premis.xml`), IIIF Presentation JSON (`.iiif.json`, `.manifest.json`), EAD finding-aid XML (`.ead`, `.ead.xml`), EAC-CPF authority XML (`.eac-cpf`, `.eac`), Dublin Core XML (`.dc.xml`, `.dublin.xml`), ISO 19115/19139 metadata XML (`.iso19115`, `.iso19139`, `.gmd.xml`), OASIS UBL 2.x business XML (`.ubl`, `.ubl.xml`), XBRL 2.1 instances (`.xbrl`, `.xbrl.xml`, `.xbrli`), LandXML 1.2 civil models (`.landxml`, `.landxml.xml`), MathML 3 formulas (`.mathml`, `.mathml.xml`, `.mml`), Adobe XMP metadata (`.xmp`, `.xmp.xml`), Adobe XDP/XFA packages (`.xdp`, `.xdp.xml`), Excel 2003 XML SpreadsheetML (`.spreadsheetml`, `.xmlss`, `.excel.xml`), Chemical Markup Language XML (`.cml`, `.cml.xml`), RDF/XML graphs (`.rdf`, `.rdf.xml`), S1000D Data Module XML (`.s1000d`, `.dmodule`), MODS XML (`.mods`, `.mods.xml`), Jupyter notebooks (`.ipynb`), Quarto/R Markdown sources
(`.qmd`, `.Rmd`), mesh-only Abaqus decks (`.inp`), LS-DYNA Keyword meshes
(`.k`, `.key`), Nastran Bulk Data meshes (`.bdf`, `.nas`), SU2 CFD meshes (`.su2`), OpenFOAM polyMesh cases (`.foam`), Tecplot ASCII meshes (`.dat`, `.tec`, `.tecplot`, `.tp`), EnSight Gold ASCII cases (`.case`, `.geo`), PLOT3D ASCII grids (`.p3d`, `.plot3d`, `.p3`), VRML97 meshes (`.wrl`, `.vrml`), UNV/UFF meshes (`.unv`), ASCII/binary MEDIT meshes
(`.mesh`, `.medit`, `.meshb`), OFF/COFF/NOFF polygon meshes (`.off`), IFC4 BIM-SPF/IFCXML models including reused mapped instances (`.ifc`, `.ifcxml`, `.ifczip`), buildingSMART BCFZIP issue packages (`.bcfzip`), and PCL
ASCII XYZ (`.xyz`), ASTM E57 (`.e57`), Leica PTS (`.pts`) and PTX (`.ptx`) point clouds, PCD point clouds (`.pcd`, ASCII/little-endian binary/LZF-compressed), and ASPRS LAS/LAZ (`.las`, `.laz`) with bounded XYZ/RGB sampling are supported.
Packed or separate RGB point colors are preserved, using a bounded 512-color
palette for denser color data.
Notebook/source code is previewed without execution.
reStructuredText `.rst`/`.rest` previews retain section titles, lists, tables, literal/code blocks, common admonitions, and bounded local PNG/JPEG images. File-insertion and raw-output directives are left inactive.
See the repository's format matrix for exact coverage and fidelity limits.

```python
from document_svg import convert

report = convert("slides.pptx", "output", jobs=4)
print(report["page_count"])

fidelity = convert(
    "input.pdf",
    "output-fidelity",
    outline_embedded_pdf_text=True,
)
```

Pass `embed_drawio_source=True` when converting a draw.io file to keep a copy of
the diagram in each SVG, so `reverse()` can restore the editable diagram from it.

Package SVG pages into PPTX, DOCX, XLSX, draw.io, AutoCAD DXF, CNC G-code, Gerber RS-274X, or HP-GL:

```python
from document_svg import reverse

report = reverse("svg-pages", "slides.pptx")
print(report["page_count"])

# Or export to CAD DXF / CNC G-code / Gerber / HP-GL:
reverse("drawing.svg", "output.dxf")
reverse("toolpath.svg", "output.gcode")
reverse("pcb_top.svg", "output.gbr")
reverse("plot.svg", "output.plt")
```

The input is a single SVG or a directory of SVG pages. This does not reconstruct
the original paragraphs, cells, formulas or other Office application semantics.

Inspect the returned warnings before relying on conversion fidelity. Output
directories for conversion must be new or empty.

## Build from source

Build a local wheel from this directory:

```bash
python -m pip wheel --no-deps --wheel-dir dist .
```

Publish the generated wheels for each supported operating system and CPU to
PyPI. End users do not need a Rust toolchain when a matching wheel is present.

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
