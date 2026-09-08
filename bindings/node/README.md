# document-svg for Node.js

Preview guides: [日本語](docs/preview.ja.md) · [English](docs/preview.en.md) · [简体中文](docs/preview.zh-CN.md)

Node-API bindings for the Rust `document-svg` converter.

```js
const { convert } = require("document-svg")

const report = await convert("slides.pptx", "output", { jobs: 4 })
console.log(report.pageCount)

const fidelity = await convert("input.pdf", "output-fidelity", {
  outlineEmbeddedPdfText: true,
})
```

SVGをPPTX、DOCX、XLSXへベクター画像として格納する逆変換:

```js
const { reverse } = require("document-svg")

const report = await reverse("svg-pages", "slides.pptx")
console.log(report.pageCount)
```

入力は単一SVGまたはSVGディレクトリです。元文書の段落、セル、数式などの意味構造は
復元されません。

## TypeScript／画面プレビュー

`preview()`はPDF、PPTX、XLSX、DOCXを一時領域で変換し、ページごとの完全なSVG文字列を
返します。出力ディレクトリは不要で、一時ファイルはPromiseが解決する前に削除されます。

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

ブラウザ画面へ渡した後は、ブラウザ専用の`document-svg/preview-ui`を使ってBlob URLを
作成し、`img`へ設定できます。このsubpathはネイティブbindingを読み込まないため、
renderer側で利用できます。

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
// 画面を破棄するときに revokeSvgPreviewUrl(previewUrl) を呼ぶ。
// portablePreviewUrl はBlob URLを共有できないrenderer/Markdown境界向け。

// 必ずbuttonのclick handlerなど、ユーザー操作の中から呼ぶ。
const copiedFormat = await copySvgToClipboard(firstPageSvg)
console.log(`copied as ${copiedFormat}`)
```

`copySvgToClipboard()`はブラウザが許可する場合は`image/svg+xml`としてコピーし、対応して
いない場合は完全なSVGソースを`text/plain`としてコピーします。常にソーステキストとして
コピーしたい場合は`copySvgSourceToClipboard()`を使います。Clipboard APIはHTTPSまたは
localhostなどのsecure contextと、クリック等のユーザー操作を要求する場合があります。

Blob URLの解放、コピーボタン、成功・失敗表示まで含むReact例は
[`examples/SvgPreview.tsx`](examples/SvgPreview.tsx)にあります。

これはNode.js、Electronのmain process、サーバー側TypeScript用のネイティブAPIです。
純粋なブラウザ内で直接実行するWASM APIではありません。

The conversion runs on a libuv worker and does not block the JavaScript event
loop. Build the native package for the current platform with:

```bash
npm install
npm run build
```

Use `napi create-npm-dirs` and the napi-rs release workflow to publish the root
package together with its platform-specific optional packages.
