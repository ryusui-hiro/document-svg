# document-svg for Node.js

Preview guides: [日本語](docs/preview.ja.md) · [English](docs/preview.en.md) · [简体中文](docs/preview.zh-CN.md)

Node-API bindings for the Rust `document-svg` converter.

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

Package SVG pages as vector images in PPTX, DOCX or XLSX:

```js
const { reverse } = require("document-svg")

async function main() {
  const report = await reverse("svg-pages", "slides.pptx")
  console.log(report.pageCount)
}

main().catch(console.error)
```

The input is a single SVG or a directory of SVG pages. This does not reconstruct
the original paragraphs, cells, formulas or other Office semantics.

## TypeScript and application previews

`preview()` converts PDF, PPTX, XLSX or DOCX in temporary storage and returns
complete SVG strings for every page. No output directory is required; temporary
files are removed before its Promise resolves.

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
