# アプリに文書のプレビューを出す（Node.js）

[日本語](preview.ja.md) · [English](preview.en.md) · [简体中文](preview.zh-CN.md)

利用者がアップロードした PDF、Word、Excel、PowerPoint、図やCAD図面を、Webアプリや Electron アプリの画面に表示してみましょう。このガイドでは、ページごとの画像として表示するまでを順に説明します。
サーバーに Office を入れる必要はありません。ファイルが外部に送られることもありません。

対応している形式は[対応形式の一覧](https://ryusui-hiro.github.io/document-svg/ja/formats.html)で調べられます。

## 仕組み

変換と表示を、別の場所で行います。

1. **Node.js 側（サーバーや Electron のメインプロセス）** で、`preview()` がファイルをページごとのSVGの文字列に変換します。変換は別スレッドで動くので、ほかのリクエストの処理を止めません。
2. **画面側（ブラウザや Electron のレンダラー）** で、`document-svg/preview-ui` を使って、そのSVGを `<img>` の画像として表示します。

`preview-ui` はネイティブコードを読み込まない小さなヘルパーなので、ブラウザ向けのバンドルに入れても問題ありません。
逆に、`preview()` などの変換はブラウザの中では動きません。

## インストール

Node.js 18 以降が必要です。

```bash
npm install document-svg
```

Windows、macOS、Linux（x64・ARM64）向けのビルド済みパッケージが一緒に入るので、Rust は要りません。
npm の optional dependencies（オプションの依存パッケージ）は無効にしないでください。そこから自分のOSに合ったものが選ばれます。

## 手順1：Node.js 側でSVGに変換する

```js
const { preview } = require('document-svg')

async function renderPreview(filePath) {
  const result = await preview(filePath, { maxPages: 50 })
  return {
    pages: result.pages.map((page) => ({
      number: page.number,
      svg: page.svg,
      width: page.widthPoints,   // ポイント単位（1pt = 1/72 インチ）
      height: page.heightPoints,
    })),
    needsReview: result.needsReview,
    warnings: result.warnings,
  }
}
```

`preview()` はファイルのパスを受け取ります。アップロードされたファイルは、いったん一時フォルダに保存してから渡してください。
作業用の一時ファイルは、`preview()` が終わる前に自動で消えます。

```js
const { mkdtemp, writeFile, rm } = require('node:fs/promises')
const { join, extname, basename } = require('node:path')
const { tmpdir } = require('node:os')

async function renderUpload(buffer, originalName) {
  const dir = await mkdtemp(join(tmpdir(), 'preview-'))
  try {
    // 形式は拡張子で判断するので、元の拡張子を残す
    const file = join(dir, 'upload' + extname(basename(originalName)).toLowerCase())
    await writeFile(file, buffer)
    return await renderPreview(file)
  } finally {
    await rm(dir, { recursive: true, force: true })
  }
}
```

Electron なら、メインプロセスで変換して、結果をレンダラーに渡します。

```js
// main.js（メインプロセス）
const { ipcMain } = require('electron')
ipcMain.handle('document:preview', (_event, filePath) => renderPreview(filePath))
```

## 手順2：画面に表示する

受け取ったSVGは、画像として表示します。

```js
import { createSvgPreviewUrl, revokeSvgPreviewUrl } from 'document-svg/preview-ui'

const url = createSvgPreviewUrl(page.svg)
image.src = url            // <img> 要素

// ページを切り替えるとき、画面を閉じるときに解放する
revokeSvgPreviewUrl(url)
```

`createSvgPreviewUrl()` はブラウザのメモリ上に Blob URL を作ります。使い終わったら `revokeSvgPreviewUrl()` で必ず解放してください。
Blob URL が使えない場面（別のウィンドウやプロセスへURLを渡すとき、Markdown に埋め込むときなど）では、代わりに `createSvgPreviewDataUrl(page.svg)` で `data:` URL を作れます。
ただし文字列が大きくなるので、同じ画面の中では Blob URL を使ってください。

**SVGの中身を `innerHTML` でページに差し込まないでください。** `preview-ui` は、スクリプト、イベント属性、外部URL、アニメーションなどを含むSVGを受け付けません。
それでも、どんなSVGでも無害にするフィルターではないので、必ず `<img>` で表示してください。
画像として表示した文字は、選択も検索もできません。

## 手順3：警告を利用者に知らせる

変換はエラーなく終わっても、元の見た目を完全に再現できたとは限りません。
グラフやフォントを近似したり省略したりした場合は、`needsReview` が `true` になります。その内容は `warnings` に入ります。

```js
const result = await preview(file)
if (result.needsReview) {
  banner.textContent = 'このプレビューは元の文書と一部違う可能性があります。'
  console.warn(result.warnings)
}
```

ページごとの警告は `page.warnings` にあります。大事な文書は、元のファイルと見比べてから使ってください。
警告がゼロでも、Office で開いたときと1ピクセル単位で同じになる保証はありません。たとえば文字は、表示する環境にあるフォントで描かれます。

## コピーボタンを付ける

```js
import { copySvgToClipboard } from 'document-svg/preview-ui'

copyButton.addEventListener('click', async () => {
  const copied = await copySvgToClipboard(page.svg)
  status.textContent = copied === 'image/svg+xml' ? '画像をコピーしました' : 'SVGのソースをコピーしました'
})
```

画像としてコピーできないブラウザでは、SVGのソースをテキストとしてコピーします。常にソースをコピーしたいときは `copySvgSourceToClipboard()` を使います。
クリップボードは、HTTPS か localhost のページで、クリックなどの操作から呼んだときだけ使えます。

## React の例

Blob URL の作成と解放、コピーボタン、成功・失敗の表示までそろった部品が [`examples/SvgPreview.tsx`](../examples/SvgPreview.tsx) にあります。

```tsx
<SvgPreview svg={pages[currentPage].svg} />
```

## 全ページを1つのHTMLに書き出す例

画面を作る前に、仕上がりだけ確かめたいときに便利です。

```bash
npm run example:preview -- ./slides.pptx ./preview.html
```

`preview.html` をブラウザで開くと全ページが並びます。警告があればページ内に表示し、終了コードは `2` になります。
中身は [`examples/preview-to-html.cjs`](../examples/preview-to-html.cjs) です。

## 大きなファイルと公開サービスでの注意

誰でもファイルを上げられるサービスでは、すべてのファイルを信用できないものとして扱ってください。
変換は、メモリと時間に上限をかけた別のプロセスで動かすのが安全です。

入力の大きさやページ数には、はじめから上限があります。用途に合わせて小さくするのはかまいません。

```js
await preview(file, {
  maxInputBytes: 64 * 1024 * 1024,      // 入力ファイルの大きさの上限
  maxPages: 50,                         // 変換するページ数の上限
  maxSvgBytes: 16 * 1024 * 1024,        // 1ページ分のSVGの大きさの上限
  maxTotalSvgBytes: 128 * 1024 * 1024,  // 全ページのSVGの合計の上限
  jobs: 1,                              // 同時に変換するページ数
})
```

難しいファイルを通すためだけに、上限を引き上げないでください。上限は、巨大なファイルや壊れたファイルからマシンを守るためのものです。

## API の早見表

| 使うもの | 何をするか | ファイルを残すか |
|---|---|---|
| `preview(file, options)` | 画面に出すために、ページごとのSVGの文字列を受け取る | 残さない |
| `convert(file, folder, options)` | SVGファイルと記録（`conversion.json`）をフォルダに書き出す | 残す |
| `reverse(svg, file)` | SVGのページを PowerPoint、Word、Excel、CAD などのファイルにまとめる | 残す |
| `createSvgPreviewUrl(svg)` | 同じ画面の `<img>` に表示する | 残さない（Blob URL） |
| `createSvgPreviewDataUrl(svg)` | 別のウィンドウやプロセスで表示する | 残さない |
| `copySvgToClipboard(svg)` | 画像かソースをコピーする | 残さない |

`reverse()` で戻るのは見た目です。段落、セル、数式などの構造は戻りません。

## もっと知る

- [Node.js パッケージの README](../README.md)
- [安全性と限界](https://ryusui-hiro.github.io/document-svg/ja/safety.html)
- [npm のパッケージページ](https://www.npmjs.com/package/document-svg)

## ライセンス

`MIT OR Apache-2.0` です。どちらかを選んで使えます。再配布するときは、選んだライセンスが求める著作権表示とライセンス文を残してください（[LICENSE](../LICENSE)）。
