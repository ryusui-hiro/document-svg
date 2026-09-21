# 在应用中显示文档预览（Node.js）

[日本語](preview.ja.md) · [English](preview.en.md) · [简体中文](preview.zh-CN.md)

我们来把用户上传的 PDF、Word、Excel、PowerPoint、图表和 CAD 图纸显示在网页应用或 Electron 应用里。
本指南会一步步讲到把每一页显示成图片为止。服务器上不需要安装 Office，文件也不会发送到任何地方。

哪些文件能用，可以在[支持格式列表](https://ryusui-hiro.github.io/document-svg/zh/formats.html)里查到。

## 整体思路

转换和显示在不同的地方进行。

1. **在 Node.js 端**（服务器，或 Electron 的主进程），`preview()` 把文件转换成每页一个 SVG 字符串。转换在单独的线程中运行，不会阻塞其他请求。
2. **在界面端**（浏览器，或 Electron 的渲染进程），用 `document-svg/preview-ui` 把每个 SVG 显示成 `<img>` 图片。

`preview-ui` 是一个不加载原生代码的小工具，可以放心打包进浏览器端代码。
反过来，`preview()` 等转换功能不能在浏览器中运行。

## 安装

需要 Node.js 18 或更高版本。

```bash
npm install document-svg
```

会一起装上适用于 Windows、macOS、Linux（x64 和 ARM64）的预编译原生代码，不需要 Rust。
请不要禁用 npm 的可选依赖（optional dependencies），适合你系统的那一个就是从这里选出来的。

## 第 1 步：在 Node.js 端转换

```js
const { preview } = require('document-svg')

async function renderPreview(filePath) {
  const result = await preview(filePath, { maxPages: 50 })
  return {
    pages: result.pages.map((page) => ({
      number: page.number,
      svg: page.svg,
      width: page.widthPoints,   // 单位为点（1 pt = 1/72 英寸）
      height: page.heightPoints,
    })),
    needsReview: result.needsReview,
    warnings: result.warnings,
  }
}
```

`preview()` 接收文件路径。上传的文件请先保存到临时文件夹再传入。`preview()` 自己用到的临时文件会在它返回之前删除。

```js
const { mkdtemp, writeFile, rm } = require('node:fs/promises')
const { join, extname, basename } = require('node:path')
const { tmpdir } = require('node:os')

async function renderUpload(buffer, originalName) {
  const dir = await mkdtemp(join(tmpdir(), 'preview-'))
  try {
    // 格式按扩展名判断，所以要保留原来的扩展名
    const file = join(dir, 'upload' + extname(basename(originalName)).toLowerCase())
    await writeFile(file, buffer)
    return await renderPreview(file)
  } finally {
    await rm(dir, { recursive: true, force: true })
  }
}
```

在 Electron 中，在主进程里转换，再把结果交给渲染进程：

```js
// main.js（主进程）
const { ipcMain } = require('electron')
ipcMain.handle('document:preview', (_event, filePath) => renderPreview(filePath))
```

## 第 2 步：显示在界面上

把收到的 SVG 当作图片显示。

```js
import { createSvgPreviewUrl, revokeSvgPreviewUrl } from 'document-svg/preview-ui'

const url = createSvgPreviewUrl(page.svg)
image.src = url            // <img> 元素

// 切换页面或关闭界面时释放
revokeSvgPreviewUrl(url)
```

`createSvgPreviewUrl()` 会在浏览器内存中创建 Blob URL，用完后务必用 `revokeSvgPreviewUrl()` 释放。
在不能使用 Blob URL 的场合（把 URL 传给别的窗口或进程、嵌入 Markdown 等），可以改用 `createSvgPreviewDataUrl(page.svg)` 生成 `data:` URL。
不过它的字符串要长得多，同一个页面内请优先使用 Blob URL。

**不要用 `innerHTML` 把 SVG 内容插入页面。** `preview-ui` 会拒绝包含脚本、事件属性、外部 URL 或动画的 SVG，但它并不能让任意 SVG 都变得无害，所以请始终用 `<img>` 显示。
以图片形式显示的文字无法选中或搜索。

## 第 3 步：有近似处理时告诉用户

没有报错，不代表页面和原件完全一样。
图表或字体被近似处理或省略时，`needsReview` 会是 `true`，具体内容在 `warnings` 里。

```js
const result = await preview(file)
if (result.needsReview) {
  banner.textContent = '此预览可能与原文档有差异。'
  console.warn(result.warnings)
}
```

每一页也有自己的 `page.warnings`。重要文档请先和原件对照再使用。
即使没有警告，也不保证和用 Office 打开时逐像素一致。比如文字会使用显示环境中已有的字体。

## 添加复制按钮

```js
import { copySvgToClipboard } from 'document-svg/preview-ui'

copyButton.addEventListener('click', async () => {
  const copied = await copySvgToClipboard(page.svg)
  status.textContent = copied === 'image/svg+xml' ? '已复制图片' : '已复制 SVG 源码'
})
```

不能以图片形式复制 SVG 的浏览器，会以文本形式复制 SVG 源码。想始终复制源码，请用 `copySvgSourceToClipboard()`。
剪贴板只能在 HTTPS 或 localhost 页面中、由点击等用户操作触发时使用。

## React 组件示例

[`examples/SvgPreview.tsx`](../examples/SvgPreview.tsx) 是现成的组件，包含 Blob URL 的释放、复制按钮和成功/失败提示。

```tsx
<SvgPreview svg={pages[currentPage].svg} />
```

## 把所有页面写到一个 HTML 文件

在开发界面之前，想先看看效果时很方便。

```bash
npm run example:preview -- ./slides.pptx ./preview.html
```

用浏览器打开 `preview.html`，就能看到所有页面。有警告时会显示在页面中，退出码为 `2`。
代码见 [`examples/preview-to-html.cjs`](../examples/preview-to-html.cjs)。

## 大文件和公开服务的注意事项

如果任何人都能向你的服务上传文件，请把所有文件都当作不可信的，并在单独的、限制了内存和时间的进程中运行转换。

输入大小和页数默认就有上限。按用途调小是没问题的：

```js
await preview(file, {
  maxInputBytes: 64 * 1024 * 1024,      // 输入文件大小上限
  maxPages: 50,                         // 转换页数上限
  maxSvgBytes: 16 * 1024 * 1024,        // 单页 SVG 大小上限
  maxTotalSvgBytes: 128 * 1024 * 1024,  // 所有页面 SVG 的总大小上限
  jobs: 1,                              // 同时转换的页数
})
```

不要只是为了让难处理的文件通过而调高上限。上限是为了保护机器免受超大或损坏文件的影响。

## API 速查表

| 使用 | 作用 | 是否留下文件 |
|---|---|---|
| `preview(file, options)` | 为了在界面上显示，拿到每页一个 SVG 字符串 | 否 |
| `convert(file, folder, options)` | 把 SVG 文件和 `conversion.json` 报告写到文件夹 | 是 |
| `reverse(svg, file)` | 把 SVG 页面打包成 PowerPoint、Word、Excel、CAD 等文件 | 是 |
| `createSvgPreviewUrl(svg)` | 在同一页面的 `<img>` 中显示 | 否（Blob URL） |
| `createSvgPreviewDataUrl(svg)` | 在别的窗口或进程中显示 | 否 |
| `copySvgToClipboard(svg)` | 复制图片或源码 | 否 |

`reverse()` 转回去的是页面外观，段落、单元格和公式不会重建。

## 了解更多

- [Node.js 包的 README](../README.md)
- [安全与局限](https://ryusui-hiro.github.io/document-svg/zh/safety.html)
- [npm 软件包页面](https://www.npmjs.com/package/document-svg)

## 许可证

`MIT OR Apache-2.0`，任选其一。再分发时，请保留你所选许可证要求的版权声明和许可证文本（[LICENSE](../LICENSE)）。
