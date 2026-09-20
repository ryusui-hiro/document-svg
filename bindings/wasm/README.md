# Browser WebAssembly viewer

This package runs PDF, DOCX, XLSX, and PPTX conversion locally in a browser.
It uses the Rust `document-svg` parsers to emit one SVG per page, without
pdf.js, a conversion server, or document uploads. Conversion runs synchronously
inside a dedicated Web Worker so the application UI remains responsive.

## Build the browser package

Install the WebAssembly Rust target and matching `wasm-bindgen` command once:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.128 --locked
```

Then build the generated WebAssembly glue and binary:

```sh
cd bindings/wasm
npm run build:wasm
```

The output is written to `bindings/wasm/web/pkg/`. The generated files are
build artifacts and are not checked in.
When deploying a changed viewer build behind a long-lived static cache, bump
the `browser-viewer-*` query suffixes in the static example, viewer, and worker
asset URLs.

## Run the static example

From the repository root, serve the files over HTTP:

```sh
python3 -m http.server 4173
```

Open <http://localhost:4173/examples/static-viewer/> and choose or drop a PDF,
DOCX, XLSX, or PPTX document. The app fetches its own JavaScript and WASM files
from the static host. It sends the selected document nowhere. `file://` opening
is not supported because browsers restrict module workers and WebAssembly file
loading there.

For a site hosted under a static prefix such as GitHub Pages, serve the full
`examples/static-viewer/` and `bindings/wasm/web/` directories from the same
origin. No API, server runtime, or document storage is required.

## Use the Web Component

```html
<script type="module" src="/bindings/wasm/web/docsvg-viewer.js"></script>
<docsvg-viewer thumbnails></docsvg-viewer>
```

The component provides file selection and drag-and-drop, page navigation,
thumbnails, zoom, fit-to-width and fit-to-page, 90-degree rotation, full-screen
viewing, lazy SVG image loading, warning display, and current-page SVG download.
Arrow/Page Up/Page Down keys navigate pages, `+` and `-` zoom, `f` fits width,
and `r` rotates. SVG pages are shown through Blob URLs in `<img>`
elements; the component does not inject document SVG into the application DOM.
It revokes URLs as pages leave the nearby viewport and when the document is
replaced or the component is removed.

The `<docsvg-viewer>` element accepts an optional `options` property matching
[`web/index.d.ts`](web/index.d.ts). Defaults cap the source at 64 MiB, an
expanded ZIP entry at 32 MiB, SVG output at 128 MiB, text geometry at 50,000
spans/8 MiB, and the document at 1,000 pages. Hard limits prevent callers from
raising those browser bounds without limit. PDF parallelism is disabled for
the browser build.

## Conversion scope

The browser build supports PDF, DOCX, XLSX, PPTX, and macro-enabled OOXML
variants that share those parsers. Encrypted documents are rejected. Macros,
external relationships, and remote resources are not executed or fetched.
Rendering warnings remain visible in the viewer. Word pagination and some PDF
or Office effects are approximations as described in the root
[`docs/SUPPORT.md`](../../docs/SUPPORT.md).

The browser build does not decode JPEG 2000 (`JPXDecode`) images. It keeps the
original image stream in the SVG and reports a page warning; browsers without
JPEG 2000 image support may show that image as missing. Native Node.js and CLI
builds retain the existing JPEG 2000 decoder.

SVG is the display format, with a transparent text-position layer for text
selection and document search. Search highlights a matching text run and may
cover the whole run when only part of it matched. Text geometry follows the
converter's text runs and may be approximate when source effects or line layout
differ. Outlined PDF text and text omitted by the source parser are not
searchable. Annotations, printing, and in-document links are not implemented
in this viewer.
