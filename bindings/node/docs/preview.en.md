# document-svg preview guide

[日本語](preview.ja.md) · [English](preview.en.md) · [简体中文](preview.zh-CN.md)

`document-svg` is a Node.js module that turns supported PDF, Office/OpenDocument/Visio (`.vsdx`, `.vsdm`, `.vstx`, `.vstm`, `.vdx`), iCalendar (`.ics`), legacy vCalendar (`.vcs`), vCard contacts (`.vcf`, `.vcard`), MIME e-mail (`.eml`), Apple Mail messages (`.emlx`), Outlook messages (`.msg`), MBOX archives (`.mbox`), MHTML web archives (`.mht`, `.mhtml`), DocBook 4/5 (`.dbk`, `.docbook`), DITA topics/maps (`.dita`, `.ditamap`), PDB coordinate models (`.pdb`, `.ent`), HWPX (`.hwpx`), COLLADA (`.dae`), X3D (`.x3d`), XMind (`.xmind`), NIfTI (`.nii`, `.nii.gz`), FITS (`.fits`, `.fit`, `.fts`, `.fits.gz`), MRC (`.mrc`, `.map`, `.mrc.gz`), SQLite (`.sqlite`, `.sqlite3`, `.db`), mmCIF/PDBx (`.cif`, `.mmcif`), MOL2 (`.mol2`), RDF Turtle (`.ttl`, `.nt`, `.nq`), EPS/PostScript (`.eps`, `.ps`), DICOM medical images (`.dcm`, `.dicom`), Jupyter notebooks, Quarto/R Markdown, reStructuredText (`.rst`, `.rest`), Org-mode (`.org`), GNU gettext catalogs (`.po`, `.pot`), BibTeX bibliographies (`.bib`, `.bibtex`), web/text, ARFF datasets (`.arff`), JSON-LD 1.1 (`.jsonld`, `.json-ld`), NetCDF classic (`.nc`, `.nc3`, `.cdf`), GraphML (`.graphml`), GEXF (`.gexf`), XGMML (`.xgmml`), Graph Modeling Language (`.gml`), CSV/TSV tables, TOML configuration (`.toml`), YAML 1.2 configuration (`.yaml`, `.yml`), generic XML (`.xml` fallback), Java Properties (`.properties`), BPMN 2.0 (`.bpmn`, `.bpmn2`), CMMN case plans (`.cmmn`), DMN decision tables (`.dmn`), ReqIF requirements (`.reqif`), XMI models (`.xmi`), diagram, CAD, simulation, glTF/GLB, raster PNG/JPEG/BMP/GIF/WebP, CBZ comic archives, standalone JPEG 2000 (`.jp2`, `.j2k`, `.j2c`, `.jpc`, `.jpx`), multi-page TIFF/BigTIFF, OFF polygon meshes, ASCII XYZ, PCL PCD, ASTM E57, Leica PTS/PTX and ASPRS LAS/LAZ point clouds, Abaqus, LS-DYNA, MEDIT ASCII/binary and Nastran mesh decks, GeoJSON, GeoRSS, GML, GPX, KML/KMZ, ESRI Shapefile, dBASE III/III+ tables, and WKT/EWKT maps into page-by-page SVG previews. Its Rust-native conversion runs on a Node.js worker, so it does not block the event loop.

DICOMDIR media directories (`DICOMDIR` or `.dicomdir`) follow directory-record offsets and resolve File IDs only inside the DICOMDIR folder.

BCFZIP issue packages (`.bcfzip`) render buildingSMART project/topic metadata and bounded markup counts. Snapshot images, IFC/model payloads, document URLs and collaboration actions remain inert; package entries are never executed or dereferenced.

Flat OPC packages (`.flatopc`, `.fopc`, `.flatopc.xml`) are validated and reconstructed in memory before using the existing DOCX/XLSX/PPTX renderers. Macros, external relationships, URLs, active content and filesystem extraction remain inert.

AASX packages (`.aasx`) render bounded Asset Administration Shell relationship and specification metadata. Supplementary CAD/manual files, identifiers, values, URLs, signatures and encryption material remain inert.

## What it can do

- Convert supported inputs such as PDF, legacy Word Binary `.doc`/`.dot`, text-only legacy PowerPoint Binary `.ppt`, legacy Excel `.xls` and Excel Binary Workbook `.xlsb`, PPTX, XLSX, DOCX, Flat OPC (`.flatopc`, `.fopc`, `.flatopc.xml`), AASX (`.aasx`), OpenSCAD (`.scad`), AMF (`.amf`), PLMXML (`.plmxml`, `.plm.xml`), STEP-XML (`.stepxml`, `.stpx`), QIF (`.qif`, `.qif.xml`), B2MML/JDF/XJDF (`.b2mml`, `.jdf`, `.xjdf`), CDA/CCD (`.cda`, `.cda.xml`), ISO 20022 (`.iso20022.xml`), SBML (`.sbml`), CellML (`.cellml`), OCEL XML (`.xmlocel`), EnergyPlus IDF/EPW (`.idf`, `.epw`), RINEX (`.rnx`, `.obs`, `.nav`) and ACIS SAT (`.sat`), TMX/TBX (`.tmx`, `.tbx`), gbXML (`.gbxml`), FHIR XML (`.fhir.xml`) and Adobe IDML (`.idml`), ODT/ODS, Visio `.vsdx`/`.vsdm`/`.vstx`/`.vstm` and `.vdx`, iCalendar `.ics`, vCalendar `.vcs`, vCard `.vcf`/`.vcard`, EML `.eml`, Apple Mail `.emlx`, Outlook `.msg`, MBOX `.mbox`, MHTML `.mht`/`.mhtml`, HTML, DocBook 4/5 `.dbk`/`.docbook`, EPUB, Jupyter `.ipynb`, Quarto `.qmd`, R Markdown `.Rmd`, reStructuredText `.rst`/`.rest`, Org-mode `.org`, GNU gettext `.po`/`.pot`, BibTeX `.bib`/`.bibtex`, raster PNG/JPEG/BMP/GIF/WebP, CBZ comic archives, multi-page TIFF/BigTIFF, glTF/GLB `.gltf`/`.glb`, OFF `.off`, IFC4 BIM `.ifc`/`.ifczip`, buildingSMART BCFZIP `.bcfzip`, KiCad PCB `.kicad_pcb`, ASCII XYZ, ASTM E57 and Leica PTS/PTX point clouds, Abaqus `.inp`, LS-DYNA `.k`/`.key`, MEDIT `.mesh`/`.meshb`, Nastran `.bdf`/`.nas`, SU2 CFD `.su2`, OpenFOAM `.foam`, draw.io, quoted CSV/TSV tables, ESRI ASCII Grid rasters (`.asc`), dBASE III/III+ tables (`.dbf`), GeoJSON/GeoRSS/GML/GPX/KML/KMZ, WKT/EWKT maps, VTK and CAD files into complete SVG strings, one per page
- Produce in-memory previews without retaining output files
- Run in Node.js servers, Electron main processes, and server-side TypeScript
- Create Blob URLs or Data URLs for an `<img>`
- Copy an SVG image or its exact XML source to the clipboard
- Surface conversion warnings through `needsReview`
- Package SVG pages into PPTX, DOCX, or XLSX as vector images

For Jupyter notebooks, the preview displays stored code and saved outputs; it never starts a kernel or runs notebook code.
Quarto and R Markdown source chunks are also displayed without evaluating code or YAML execution options.

This is not a browser-only WASM converter. Run document conversion in Node.js and use only `document-svg/preview-ui` in a renderer or browser context.

## Install

After publication:

```bash
npm install document-svg
```

To try the repository build:

```bash
cd bindings/node
npm install
npm run build
npm test
```

Node.js 18 or newer is required. A published release installs the native package matching the user's operating system and CPU.

## Minimal example: get SVG markup

```js
const { preview } = require('document-svg')

async function main() {
  const report = await preview('slides.pptx', { maxPages: 100 })
  console.log(`${report.pageCount} pages`)
  console.log(`review required: ${report.needsReview}`)
  const firstPageSvg = report.pages[0]?.svg
  console.log(firstPageSvg)
}
main().catch(console.error)
```

Each entry in `report.pages` includes its page number, complete SVG, dimensions in points, warnings, and estimated IR size. Temporary conversion files are removed before the Promise resolves.

## Display in an `<img>`

```js
import {
  createSvgPreviewUrl,
  revokeSvgPreviewUrl,
} from 'document-svg/preview-ui'

const url = createSvgPreviewUrl(svgMarkup)
imageElement.src = url

// Release the URL when replacing the page or unmounting the view.
revokeSvgPreviewUrl(url)
```

Use a Data URL across process, Markdown, or renderer boundaries where Blob URLs cannot be shared:

```js
import { createSvgPreviewDataUrl } from 'document-svg/preview-ui'
imageElement.src = createSvgPreviewDataUrl(svgMarkup)
```

Data URLs are larger strings, so prefer Blob URLs within one renderer.

## React example

[`../examples/SvgPreview.tsx`](../examples/SvgPreview.tsx) includes Blob URL lifecycle management, copying, and an accessible `aria-live` status message.

```tsx
const report = await preview(filePath)
return <SvgPreview svg={report.pages[currentPage].svg} />
```

## Runnable HTML example

The included example converts every page into one standalone HTML preview:

```bash
npm run example:preview -- ./slides.pptx ./preview.html
```

See [`../examples/preview-to-html.cjs`](../examples/preview-to-html.cjs). It renders conversion warnings in the output and exits with status `2` when review is required.

## Copy to the clipboard

```js
import { copySvgToClipboard } from 'document-svg/preview-ui'

button.addEventListener('click', async () => {
  const copiedType = await copySvgToClipboard(svgMarkup)
  console.log(copiedType) // image/svg+xml or text/plain
})
```

Clipboard APIs may require HTTPS or localhost and a user gesture such as a click. When the browser cannot write the SVG MIME type, the helper copies the exact SVG source as text.

## Warnings and limits

```js
const report = await preview('report.docx', {
  maxInputBytes: 512 * 1024 * 1024,
  maxZipEntryBytes: 128 * 1024 * 1024,
  maxPages: 100,
  maxXmlEvents: 20_000_000,
  maxSvgBytes: 64 * 1024 * 1024,
  maxTotalSvgBytes: 256 * 1024 * 1024,
  jobs: 1,
})

if (report.needsReview) {
  console.warn(report.warnings)
  console.warn(report.pages.flatMap((page) => page.warnings))
}
```

`preview-ui` rejects SVG containing scripts, event attributes, external URLs, DOCTYPE declarations, or animation. Display previews through `<img>` instead of injecting them with `innerHTML`. Visually compare important files with their originals; zero warnings does not promise pixel-identical Office layout.

## Choosing an API

| API | Use case | Keeps files |
|---|---|---|
| `preview(input, options)` | SVG strings for a UI | No |
| `convert(input, output, options)` | Batch conversion and saved artifacts | Yes |
| `reverse(input, output, options)` | Put SVG into an Office file | Yes |
| `createSvgPreviewUrl(svg)` | `<img>` in the same renderer | Blob URL only |
| `createSvgPreviewDataUrl(svg)` | Preview across process boundaries | No |
| `copySvgToClipboard(svg)` | Copy image or source | No |

The generated SVG aims to preserve editable appearance. It does not preserve Office semantics such as paragraphs, cells, or formulas.

## License

`MIT OR Apache-2.0`, at your option. When redistributing, you must retain copyright and license notices as required by the license you choose. See the [full terms](../LICENSE).
