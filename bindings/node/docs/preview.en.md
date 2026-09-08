# document-svg preview guide

[日本語](preview.ja.md) · [English](preview.en.md) · [简体中文](preview.zh-CN.md)

`document-svg` is a Node.js module that turns PDF, PowerPoint, Excel, and Word files into page-by-page SVG previews. Its Rust-native conversion runs on a Node.js worker, so it does not block the event loop.

## What it can do

- Convert PDF, PPTX, XLSX, and DOCX files into complete SVG strings, one per page
- Produce in-memory previews without retaining output files
- Run in Node.js servers, Electron main processes, and server-side TypeScript
- Create Blob URLs or Data URLs for an `<img>`
- Copy an SVG image or its exact XML source to the clipboard
- Surface conversion warnings through `needsReview`
- Package SVG pages into PPTX, DOCX, or XLSX as vector images

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
