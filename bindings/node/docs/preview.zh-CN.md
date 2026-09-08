# document-svg 预览指南

[日本語](preview.ja.md) · [English](preview.en.md) · [简体中文](preview.zh-CN.md)

`document-svg` 是一个 Node.js 模块，可将 PDF、PowerPoint、Excel 和 Word 文档逐页转换为 SVG 预览。转换由 Rust 原生代码在 Node.js worker 中执行，不会阻塞事件循环。

## 可以做什么

- 将 PDF、PPTX、XLSX、DOCX 转换为每页一个完整的 SVG 字符串
- 在内存中生成预览，不保留输出文件
- 用于 Node.js 服务端、Electron main process 和服务端 TypeScript
- 为 `<img>` 创建 Blob URL 或 Data URL
- 将 SVG 图像或完整 XML 源码复制到剪贴板
- 通过 `needsReview` 报告转换警告
- 将 SVG 页面作为矢量图打包到 PPTX、DOCX 或 XLSX

它不是仅在浏览器中运行的 WASM 转换器。文档转换应在 Node.js 端执行；renderer 或浏览器端只使用`document-svg/preview-ui`。

## 安装

发布后：

```bash
npm install document-svg
```

从本仓库试用：

```bash
cd bindings/node
npm install
npm run build
npm test
```

需要 Node.js 18 或更高版本。正式发布时，会同时安装与用户操作系统和CPU匹配的原生包。

## 最小示例：取得SVG字符串

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

`report.pages`中的每个项目都包含页码、完整SVG、以pt为单位的宽高、警告和估算的IR大小。Promise完成前，临时转换文件会被删除。

## 在`<img>`中显示

```js
import {
  createSvgPreviewUrl,
  revokeSvgPreviewUrl,
} from 'document-svg/preview-ui'

const url = createSvgPreviewUrl(svgMarkup)
imageElement.src = url

// 更换页面或卸载视图时释放URL
revokeSvgPreviewUrl(url)
```

如果需要跨进程、Markdown或renderer边界传递，Blob URL无法共享时可使用Data URL：

```js
import { createSvgPreviewDataUrl } from 'document-svg/preview-ui'
imageElement.src = createSvgPreviewDataUrl(svgMarkup)
```

Data URL字符串更大，因此在同一个renderer中建议使用Blob URL。

## React示例

[`../examples/SvgPreview.tsx`](../examples/SvgPreview.tsx)包含Blob URL的创建与释放、复制操作，以及可访问的`aria-live`状态提示。

```tsx
const report = await preview(filePath)
return <SvgPreview svg={report.pages[currentPage].svg} />
```

## 可运行的HTML示例

附带示例可将文档的所有页面写入一个独立HTML预览文件：

```bash
npm run example:preview -- ./slides.pptx ./preview.html
```

实现请参阅[`../examples/preview-to-html.cjs`](../examples/preview-to-html.cjs)。转换警告会显示在HTML中；需要人工检查时，进程退出码为`2`。

## 复制到剪贴板

```js
import { copySvgToClipboard } from 'document-svg/preview-ui'

button.addEventListener('click', async () => {
  const copiedType = await copySvgToClipboard(svgMarkup)
  console.log(copiedType) // image/svg+xml 或 text/plain
})
```

剪贴板API可能要求HTTPS或localhost环境，并且必须由点击等用户操作触发。如果浏览器不支持写入SVG MIME类型，辅助函数会将完整SVG源码作为文本复制。

## 警告和安全限制

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

`preview-ui`会拒绝包含脚本、事件属性、外部URL、DOCTYPE或动画的SVG。请通过`<img>`显示预览，不要用`innerHTML`注入。重要文档应与原文件进行目视比较；没有警告并不代表与Office像素级完全一致。

## API选择

| API | 用途 | 是否保留文件 |
|---|---|---|
| `preview(input, options)` | 为UI返回SVG字符串 | 否 |
| `convert(input, output, options)` | 批量转换并保存结果 | 是 |
| `reverse(input, output, options)` | 将SVG放入Office文件 | 是 |
| `createSvgPreviewUrl(svg)` | 在同一renderer的`<img>`中显示 | 仅Blob URL |
| `createSvgPreviewDataUrl(svg)` | 跨进程边界显示 | 否 |
| `copySvgToClipboard(svg)` | 复制图像或源码 | 否 |

生成的SVG以保留可编辑外观为目标，但不会保留段落、单元格、公式等Office语义结构。

## 许可证

`MIT OR Apache-2.0`，您可以选择其中一种。再分发时，必须按照所选许可证保留版权声明和许可证文本。参阅[完整条款](../LICENSE)。
