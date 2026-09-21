# Show document previews in your app (Node.js)

[日本語](preview.ja.md) · [English](preview.en.md) · [简体中文](preview.zh-CN.md)

Let's show the PDFs, Word, Excel and PowerPoint files, diagrams and CAD
drawings your users upload inside a web or Electron app. This guide walks
through it step by step, up to showing each page as an image. You don't need
Office on the server, and files are never sent anywhere.

Look up which files work in the [format list](https://ryusui-hiro.github.io/document-svg/formats.html).

## How it fits together

Conversion and display happen in different places.

1. **On the Node.js side** (a server, or Electron's main process), `preview()`
   turns the file into one SVG string per page. It runs on a separate thread,
   so other requests keep being served.
2. **On the screen side** (a browser, or Electron's renderer),
   `document-svg/preview-ui` shows each SVG as an `<img>`.

`preview-ui` is a small helper that loads no native code, so it is safe to put
in a browser bundle. The conversion itself (`preview()` and friends) does not
run in a browser.

## Install

Requires Node.js 18 or newer.

```bash
npm install document-svg
```

Ready-made native code for Windows, macOS and Linux (x64 and ARM64) comes with
it, so you don't need Rust. Keep npm's optional dependencies enabled: that is
how the right one for your machine is chosen.

## Step 1: convert on the Node.js side

```js
const { preview } = require('document-svg')

async function renderPreview(filePath) {
  const result = await preview(filePath, { maxPages: 50 })
  return {
    pages: result.pages.map((page) => ({
      number: page.number,
      svg: page.svg,
      width: page.widthPoints,   // in points (1 pt = 1/72 inch)
      height: page.heightPoints,
    })),
    needsReview: result.needsReview,
    warnings: result.warnings,
  }
}
```

`preview()` takes a file path. Save an uploaded file to a temporary folder
first. The temporary files `preview()` makes for itself are removed before it
returns.

```js
const { mkdtemp, writeFile, rm } = require('node:fs/promises')
const { join, extname, basename } = require('node:path')
const { tmpdir } = require('node:os')

async function renderUpload(buffer, originalName) {
  const dir = await mkdtemp(join(tmpdir(), 'preview-'))
  try {
    // The format is chosen by extension, so keep the original one
    const file = join(dir, 'upload' + extname(basename(originalName)).toLowerCase())
    await writeFile(file, buffer)
    return await renderPreview(file)
  } finally {
    await rm(dir, { recursive: true, force: true })
  }
}
```

In Electron, convert in the main process and hand the result to the renderer:

```js
// main.js (main process)
const { ipcMain } = require('electron')
ipcMain.handle('document:preview', (_event, filePath) => renderPreview(filePath))
```

## Step 2: show it on screen

Show each SVG you receive as an image.

```js
import { createSvgPreviewUrl, revokeSvgPreviewUrl } from 'document-svg/preview-ui'

const url = createSvgPreviewUrl(page.svg)
image.src = url            // an <img> element

// When switching pages or closing the view, release it
revokeSvgPreviewUrl(url)
```

`createSvgPreviewUrl()` makes a Blob URL in the browser's memory; always
release it with `revokeSvgPreviewUrl()`. Where a Blob URL can't be used — when
passing the URL to another window or process, or embedding it in Markdown —
`createSvgPreviewDataUrl(page.svg)` gives a `data:` URL instead. It is a much
longer string, so prefer Blob URLs within one page.

**Don't insert the SVG markup into your page with `innerHTML`.** `preview-ui`
refuses SVG that contains scripts, event attributes, external URLs or
animation, but it is not a filter that makes any SVG harmless, so always show
pages with `<img>`. Text in an image cannot be selected or searched.

## Step 3: tell people when something was approximated

Finishing without an error does not mean the page looks exactly like the
original. When a chart or a font had to be approximated or left out,
`needsReview` is `true` and the details are in `warnings`.

```js
const result = await preview(file)
if (result.needsReview) {
  banner.textContent = 'This preview may differ from the original document.'
  console.warn(result.warnings)
}
```

Each page also has its own `page.warnings`. Compare important documents with
the original before relying on them. Even with no warnings, the pages are not
guaranteed to match Office pixel for pixel: text, for example, uses the fonts
available where it is shown.

## Add a copy button

```js
import { copySvgToClipboard } from 'document-svg/preview-ui'

copyButton.addEventListener('click', async () => {
  const copied = await copySvgToClipboard(page.svg)
  status.textContent = copied === 'image/svg+xml' ? 'Image copied' : 'SVG source copied'
})
```

Browsers that can't copy SVG as an image get the SVG source as text. Use
`copySvgSourceToClipboard()` to always copy the source. The clipboard only
works on HTTPS or localhost pages, when called from a user action such as a
click.

## A React component

[`examples/SvgPreview.tsx`](../examples/SvgPreview.tsx) is a ready-made
component with Blob URL cleanup, a copy button, and success/failure messages.

```tsx
<SvgPreview svg={pages[currentPage].svg} />
```

## Write every page to one HTML file

Handy for checking results before you build any UI.

```bash
npm run example:preview -- ./slides.pptx ./preview.html
```

Open `preview.html` in a browser to see every page. Warnings are shown on the
page and the exit code is `2`. The code is in
[`examples/preview-to-html.cjs`](../examples/preview-to-html.cjs).

## Large files and public services

If anyone can upload files to your service, treat every file as untrusted, and
run conversion in a separate process with memory and time limits.

Input size and page count are limited by default. Lowering the limits for your
use case is fine:

```js
await preview(file, {
  maxInputBytes: 64 * 1024 * 1024,      // largest input file
  maxPages: 50,                         // most pages to convert
  maxSvgBytes: 16 * 1024 * 1024,        // largest SVG for one page
  maxTotalSvgBytes: 128 * 1024 * 1024,  // largest total for all pages
  jobs: 1,                              // pages converted at once
})
```

Don't raise them just to push a difficult file through; they protect the
machine from huge or broken files.

## API at a glance

| Use | What it does | Leaves files? |
|---|---|---|
| `preview(file, options)` | Get one SVG string per page, for showing on screen | No |
| `convert(file, folder, options)` | Write SVG files and a `conversion.json` report to a folder | Yes |
| `reverse(svg, file)` | Package SVG pages as a PowerPoint, Word, Excel, CAD or other file | Yes |
| `createSvgPreviewUrl(svg)` | Show a page in an `<img>` on the same page | No (a Blob URL) |
| `createSvgPreviewDataUrl(svg)` | Show a page in another window or process | No |
| `copySvgToClipboard(svg)` | Copy the image or its source | No |

`reverse()` brings back how pages look; paragraphs, cells and formulas are not
rebuilt.

## More

- [Node.js package README](../README.md)
- [Safety and limits](https://ryusui-hiro.github.io/document-svg/safety.html)
- [npm package page](https://www.npmjs.com/package/document-svg)

## License

`MIT OR Apache-2.0`, at your option. When you redistribute it, keep the
copyright and license notices required by the license you choose
([LICENSE](../LICENSE)).
