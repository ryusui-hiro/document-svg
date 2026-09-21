# document-svg for Node.js

**Preview PDF, Word, Excel, PowerPoint, draw.io and CAD files as SVG pages in your Node.js or Electron app — locally, without Office.**

[Project site](https://ryusui-hiro.github.io/document-svg/) ·
[Formats](https://ryusui-hiro.github.io/document-svg/formats.html) ·
[Samples](https://ryusui-hiro.github.io/document-svg/samples.html) ·
Preview guides: [English](https://github.com/ryusui-hiro/document-svg/blob/main/bindings/node/docs/preview.en.md) ·
[日本語](https://github.com/ryusui-hiro/document-svg/blob/main/bindings/node/docs/preview.ja.md) ·
[简体中文](https://github.com/ryusui-hiro/document-svg/blob/main/bindings/node/docs/preview.zh-CN.md)

`document-svg` turns a document, diagram or drawing into ordinary SVG images,
one per page, that you can show in any browser. It also tells you, page by
page, what it could not reproduce exactly, so you can decide whether a preview
is good enough to show.

- **Files stay on your server.** Nothing is uploaded and there is no account.
- **Nothing inside a file is run.** Macros, scripts and external links are shown or skipped, never executed.
- **No Office, no viewer SDK, no headless browser.** One package handles Office files, PDFs, diagrams, CAD and 3D models, and many data formats.

The same converter is also available [for Python on PyPI](https://pypi.org/project/document-svg/)
and [for Rust, with the `docsvg` command, on crates.io](https://crates.io/crates/document-svg).

## Install

Requires Node.js 18 or newer.

```sh
npm install document-svg
```

Type definitions are included. Ready-made native code is installed for
Windows, macOS and Linux on x64 and ARM64, so you don't need Rust. Keep npm's
optional dependencies enabled: that is how the right one for your machine is
chosen.

The package goes into your project's `node_modules/document-svg`, and npm adds
one native package for your system next to it (for example
`node_modules/document-svg-darwin-arm64`). `npm ls document-svg` shows the
installed version.

Conversion runs in Node.js — on a server or in Electron's main process — and
does not block the event loop. Only the small `document-svg/preview-ui` helper
is meant for the browser.

## Get SVG pages for an in-app preview

`preview()` converts a file and gives you every page as SVG text. No output
folder is needed, and temporary files are removed before it returns.

```ts
import { preview } from "document-svg"

const result = await preview("slides.pptx", { maxPages: 100 })

const firstPageSvg = result.pages[0].svg
if (result.needsReview) {
  console.log("Some parts were approximated:", result.warnings)
}
```

Send the SVG to your frontend and show it as an image with
`document-svg/preview-ui`. This helper does not load native code, so it works
in a browser bundle:

```ts
import {
  createSvgPreviewUrl,
  revokeSvgPreviewUrl,
  copySvgToClipboard,
} from "document-svg/preview-ui"

const url = createSvgPreviewUrl(firstPageSvg)
image.src = url                     // an <img> element

// When the image is replaced or the view is closed:
revokeSvgPreviewUrl(url)

// From a button click: copies the image where supported, otherwise its source
await copySvgToClipboard(firstPageSvg)
```

Show pages as images rather than inserting SVG markup into the page. The
helpers do basic checks, but they are not a filter that makes any SVG
harmless. `createSvgPreviewDataUrl()` gives a `data:` URL when a Blob URL cannot
cross a boundary, and `copySvgSourceToClipboard()` always copies the source
text. Copying needs a secure page (HTTPS or localhost) and a user action.

A complete React component with cleanup and a copy button is in
[`examples/SvgPreview.tsx`](https://github.com/ryusui-hiro/document-svg/blob/main/bindings/node/examples/SvgPreview.tsx).

## Save SVG pages to a folder

```js
const { convert } = require("document-svg")

async function main() {
  const report = await convert("report.pdf", "out/report")
  console.log(report.pageCount, report.warnings)
}

main().catch(console.error)
```

The output folder must be new or empty. It receives `page-0001.svg`,
`page-0002.svg`, … and a `conversion.json` report.

Useful options:

- `maxPages` — stop after this many pages.
- `jobs` — convert several pages at once.
- `outlineEmbeddedPdfText` — for PDFs, draw text as shapes so it looks exactly
  like the original on any computer (the text can no longer be selected).
- `embedDrawioSource` — for draw.io files, keep the diagram inside each SVG so
  it can later be turned back into an editable diagram.

## Read the result before you share it

Every result has `warnings`: things that were approximated or left out, such
as a substituted font or an unsupported fill. Finishing without an error is not
the same as perfect. When there are warnings (`needsReview` is true for
`preview()`), have a person look at the pages.

Text uses the fonts available where the SVG is shown, so it can look slightly
different on another computer.

## Turn SVG pages back into files

```js
const { reverse } = require("document-svg")

async function main() {
  await reverse("out/slides", "slides.pptx")   // a folder of pages
  await reverse("drawing.svg", "drawing.dxf")  // or a single SVG
}

main().catch(console.error)
```

The extension decides the format: PowerPoint, Word, Excel, draw.io, DXF,
G-code, Gerber, HP-GL and more. What comes back is how the pages look;
paragraphs, cells and formulas are not rebuilt. A draw.io diagram converted
with `embedDrawioSource` comes back editable.

## Tidy up an SVG

```js
const { transform } = require("document-svg")

const smaller = transform(svg, { minify: true, removeMetadata: true })
```

`transform()` can also recolour to one colour, make the SVG scale to its
container, and clean up paths and empty groups.

## Which files work?

PDF, Word, Excel and PowerPoint (current and older formats), OpenDocument,
e-mail, e-books, draw.io, Visio, Mermaid, PlantUML, DXF, Gerber, G-code, STL,
STEP, IFC, simulation meshes, CSV, JSON, maps, images and more. Some formats
are drawn in full; others show a summary of their contents. Look up your file
type in the [searchable format list](https://ryusui-hiro.github.io/document-svg/formats.html).

## Safety

- Treat uploaded files as untrusted. On a public service, run conversion in a
  separate process with memory and time limits.
- Input size, archive contents and page count are limited by default. Don't
  raise the limits just to push a difficult file through.
- A PDF that needs a password to open is refused.

More in [Safety and limits](https://ryusui-hiro.github.io/document-svg/safety.html)
and the [security policy](https://github.com/ryusui-hiro/document-svg/blob/main/SECURITY.md).

## Troubleshooting

- **The package installs but won't load.** Check that optional dependencies
  were installed and that Node.js matches your machine's CPU. Copying
  `node_modules` to another system does not work.
- **I want to convert in the browser only.** This package needs Node.js. For
  PDF, Word, Excel and PowerPoint there is a separate
  [WebAssembly build](https://github.com/ryusui-hiro/document-svg/tree/main/bindings/wasm)
  you build yourself.

## Building from source

For contributors: `npm install && npm run build` in `bindings/node` builds the
native code for the current machine (Rust required). Maintainers publish with
the [release procedure](https://github.com/ryusui-hiro/document-svg/blob/main/docs/PUBLISHING.md).

## License

MIT OR Apache-2.0. See the
[repository](https://github.com/ryusui-hiro/document-svg#license) for the
license texts and third-party notices.
