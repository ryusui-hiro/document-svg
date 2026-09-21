# document-svg

**没有原来的软件，也能把 PDF、Word、Excel、PowerPoint、图表和 CAD 图纸的每一页变成 SVG 图片来看。转换在你自己的机器上完成。**

[English](README.md) · [日本語](README.ja.md) ·
[项目网站](https://ryusui-hiro.github.io/document-svg/zh/) ·
[示例](https://ryusui-hiro.github.io/document-svg/zh/samples.html) ·
[发布版本](https://github.com/ryusui-hiro/document-svg/releases)

[![npm](https://img.shields.io/npm/v/document-svg?label=npm)](https://www.npmjs.com/package/document-svg)
[![PyPI](https://img.shields.io/pypi/v/document-svg?label=PyPI)](https://pypi.org/project/document-svg/)
[![crates.io](https://img.shields.io/crates/v/document-svg?label=crates.io)](https://crates.io/crates/document-svg)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#许可证)

document-svg 把文档、图表和技术图纸转换成普通的 SVG 图片，一页一张。
SVG 在任何浏览器里都能显示，放大不会模糊，也能像其他图片一样直接放进网页。
和页面一起，它还会生成一份简短的报告，如实写明哪些内容没能原样还原。

它不是一个单独打开使用的应用，而是嵌进其他东西里的转换组件：网站、内部工具、审查流程，或者 AI 助手。
它以 Node.js 包、Python 包、Rust 库和命令行工具（`docsvg`）的形式提供。

| PowerPoint | Excel | draw.io | CAD 图纸（DXF） |
|---|---|---|---|
| <img src="https://ryusui-hiro.github.io/document-svg/assets/samples/pptx/page-0001.svg" alt="转换成 SVG 的 PowerPoint 幻灯片" width="200"> | <img src="https://ryusui-hiro.github.io/document-svg/assets/samples/xlsx/page-0001.svg" alt="转换成 SVG 的 Excel 工作表" width="200"> | <img src="https://ryusui-hiro.github.io/document-svg/assets/samples/drawio/page-0001.svg" alt="转换成 SVG 的 draw.io 图" width="200"> | <img src="https://ryusui-hiro.github.io/document-svg/assets/samples/dxf/page-0001.svg" alt="转换成 SVG 的 DXF 图纸" width="200"> |

上面每张图都是本仓库示例文件的原样转换结果，没有任何修饰。

## 为什么用它

- **文件不离开你手里。** 转换在你的电脑或你自己的服务器上完成。不上传、不注册账号、不收集使用数据，所以不能外发的合同和设计图也能生成预览。
- **谁都能打开。** 看图的人不需要 Office、AutoCAD 或专用查看器，有浏览器就够了。
- **没做到的，会告诉你。** 没有哪个转换工具能百分之百还原。document-svg 不会悄悄丢掉图表或字体，而是按页记录被省略或近似处理的地方，你看完再决定能不能拿去分享。
- **打开文件，不等于运行文件。** 宏、脚本、表单提交和外部链接只会被显示或跳过，绝不执行。文件大小和页数都有上限，超大或损坏的文件不会把内存耗尽。
- **一个工具顶很多个。** Office 文档、PDF、图表、CAD 和三维模型、科研数据和业务数据，都用同样的方式转换，输出形式也一样。
- **也能转回去。** SVG 页面可以重新打包成 PowerPoint、Word、Excel、draw.io 或 CAD 文件。

## 大家用它做什么

- **在代码审查中看到文档改动。** GitHub Action 会转换拉取请求中改动的幻灯片、表格和 PDF，审查者不再只看到“二进制文件已更改”。
- **在自己的应用里预览上传的文件，** 不用在服务器上装 Office，也不用购买查看器 SDK。
- **让图和代码一起保持最新：** 每次构建文档时，把 draw.io、Mermaid、PlantUML、D2 或 Graphviz 文件渲染成图片。
- **没有 CAD 也能检查图纸和三维模型：** 在网页上查看平面图、电路板、加工路径和模型。
- **让 AI 助手读懂文档：** 用一个可靠的步骤代替截图。

每个例子在[项目网站](https://ryusui-hiro.github.io/document-svg/zh/use-cases.html)上都有更详细的说明。

## 安装

按你想从哪里调用，选一种即可，不需要全部安装。

| 你想要… | 安装 |
|---|---|
| 在 Node.js 或 Electron 应用中转换（Node.js 18 或更高） | `npm install document-svg` |
| 在 Python 中转换（Python 3.10 或更高） | `python -m pip install document-svg` |
| 使用 `docsvg` 命令或 Rust 库 | `cargo install document-svg --locked` / `cargo add document-svg` |

npm 和 pip 的软件包已经为 Windows、macOS 和 Linux 预编译好，不需要 Rust。
如果只想用命令行又不想安装 Rust，可以从[发布页面](https://github.com/ryusui-hiro/document-svg/releases)下载适合你系统的文件，把 `docsvg` 放到 `PATH` 中。

### 获取位置与安装位置

| 软件包 | 页面 | 安装到哪里 |
|---|---|---|
| npm 的 `document-svg` | [npmjs.com/package/document-svg](https://www.npmjs.com/package/document-svg) | 项目的 `node_modules/document-svg`，另外还会装上一个适合你系统的预编译包，例如 `node_modules/document-svg-darwin-arm64` |
| PyPI 的 `document-svg` | [pypi.org/project/document-svg](https://pypi.org/project/document-svg/) | 当前环境的 `site-packages/document_svg` |
| crates.io 的 `document-svg` | [crates.io/crates/document-svg](https://crates.io/crates/document-svg) | `cargo install` 会把 `docsvg` 放到 `~/.cargo/bin`（Windows 为 `%USERPROFILE%\.cargo\bin`）；`cargo add` 会把库加入你的项目 |
| GitHub 发布页面 | [发布版本](https://github.com/ryusui-hiro/document-svg/releases) | 解压到的位置；把 `docsvg` 放到 `PATH` 中 |

可以用 `npm ls document-svg`、`python -m pip show document-svg` 或 `docsvg --version` 确认是否已安装以及安装位置。
Rust API 文档在 [docs.rs](https://docs.rs/document-svg)。GitHub Packages 上需要认证的 npm 镜像见 [docs/PUBLISHING.md](docs/PUBLISHING.md#installing-the-github-packages-mirror)。

## 试一试

```sh
docsvg slides.pptx --output out/slides
```

输出位置必须是新的或空的文件夹。你会得到每页一个 SVG，以及一份转换记录：

```text
out/slides/
├── page-0001.svg
├── page-0002.svg
└── conversion.json
```

页面在任何浏览器里都能打开。记录里会列出**警告（warnings）**，也就是近似处理或省略的内容，比如替换了字体、跳过了不支持的填充。
没有报错不代表完美：有警告时，请在分享之前由人检查一下页面。

“一页”就是每种文件最自然的单位：PDF 的一页、一张幻灯片、Word 排版后的一页、工作表打印时的一页，或者 draw.io 图的一页。

### 在 Node.js / TypeScript 中

```js
const { convert, preview } = require('document-svg')

async function main() {
  // 把 SVG 文件写到文件夹
  const report = await convert('slides.pptx', 'out/slides')
  console.log(report.pageCount, report.warnings)

  // 或者不写文件，直接在内存中拿到页面（例如发送给浏览器）
  const result = await preview('slides.pptx')
  const firstPageSvg = result.pages[0].svg
  console.log('需要检查:', result.needsReview)
}

main().catch(console.error)
```

转换在 Node.js（服务器或 Electron 主进程）中运行，不会阻塞主线程。
在浏览器端，用不加载原生代码的小工具 `document-svg/preview-ui` 把每一页当作图片显示。
详见 [Node.js 指南](bindings/node/README.md)和[预览指南](bindings/node/docs/preview.zh-CN.md)。

### 在 Python 中

```python
from document_svg import convert, preview

report = convert("report.docx", "out/report")
print(report["page_count"], report["warnings"])

result = preview("report.docx")          # 不写文件，直接在内存中拿到页面
first_page_svg = result["pages"][0]["svg"]
```

详见 [Python 指南](bindings/python/README.md)。

### 在 Rust 中

```rust
use document_svg::{convert_path, ConvertOptions};

fn main() -> Result<(), document_svg::Error> {
    let report = convert_path("report.pdf", "out/report", &ConvertOptions::default())?;
    println!("{} pages, {} warnings", report.page_count, report.warnings.len());
    Ok(())
}
```

API 文档在 [docs.rs](https://docs.rs/document-svg)。

### 只在浏览器里运行

对于 PDF、Word、Excel 和 PowerPoint，有一个完全在浏览器里完成转换的 WebAssembly 版本，不需要服务器，也不会上传文件。
它没有作为软件包发布，需要你从 [`bindings/wasm`](bindings/wasm/README.md) 自己构建。

## 在应用中显示页面

请把每一页当作图片显示（例如用 `<img>` 元素）。不要把 SVG 的内容直接贴进页面的 HTML：附带的显示辅助工具会做基本检查，但并不能让任意 SVG 都变得无害。
以图片形式显示的文字无法选中或搜索。

文字会使用显示 SVG 的环境中已有的字体，在别的电脑上可能看起来略有不同。
对于 PDF，如果外观比可选中的文字更重要，可以把嵌入字体的文字转换成轮廓。

## 把 SVG 转回 Office 或 CAD 文件

```sh
docsvg reverse out/slides --output slides.pptx
docsvg reverse out/report --output report.docx
docsvg reverse out/drawing --output drawing.dxf
```

输出文件的扩展名决定格式：PowerPoint、Word、Excel、PDF、draw.io、DXF、G-code、STL 等。
转回去的是页面的外观，段落、单元格、公式和图表不会重建。

draw.io 图是例外：转换时加上 `--embed-drawio-source`，之后 `reverse` 就能还原出原来可编辑的图。

## 支持的格式

| 领域 | 例子 |
|---|---|
| 办公与文档 | PDF、Word、Excel、PowerPoint（新旧格式都支持）、OpenDocument、RTF、电子邮件、日历、电子书、Markdown、HTML |
| 图表 | draw.io、Visio、Mermaid、PlantUML、D2、Graphviz、BPMN |
| CAD、CAM 与三维 | DXF、Gerber、G-code、HP-GL、STL、OBJ、STEP、IGES、glTF、IFC |
| 仿真 | Gmsh、VTK、OpenFOAM、Abaqus、Nastran 等网格 |
| 数据与地图 | CSV、JSON、YAML、图表、LaTeX 公式、GeoJSON、KML、GeoPackage |
| 图像 | PNG、JPEG、TIFF、WebP、DICOM、SVG |

有的格式能完整绘制；另一些格式（例如 DWG 或 Access 数据库）显示的是内容概要，而不是图形本身。
每种格式都有评级：A（直接按文件内容绘制）、B（从像素描出轮廓）、C（从图形推断表格等结构）。

- [可搜索的格式列表](https://ryusui-hiro.github.io/document-svg/zh/formats.html) —— 查查你的文件能不能用
- [技术格式矩阵](docs/FORMATS.md)（英文）—— 每个扩展名的保真度说明
- [支持的功能与限制](docs/SUPPORT.md)

## 从 AI 助手和 GitHub 中使用

| 在哪里 | 怎么做 |
|---|---|
| Claude Code | `/plugin marketplace add ryusui-hiro/document-svg`，然后 `/plugin install document-svg@document-svg` |
| Codex | 在本地仓库中运行 `./scripts/install-codex-plugin.sh`，然后开始一个新任务 |
| GitHub Copilot | 在 Copilot 的准备步骤中加上下面的安装 action |
| GitHub CLI | 运行 `./scripts/install-gh-extension.sh`，然后 `gh docsvg convert report.pdf --output preview/report` |
| GitHub Actions | `uses: ryusui-hiro/document-svg/.github/actions/setup-docsvg@main` |

如果没有安装 Rust，安装脚本会使用经过校验和验证的发布版本。
要预览拉取请求中改动的文档，把下面的工作流加到你的仓库：

```yaml
name: Document preview
on:
  pull_request:
    paths: ['**.pdf', '**.pptx', '**.xlsx', '**.docx']
jobs:
  preview:
    uses: ryusui-hiro/document-svg/.github/workflows/document-preview.yml@main
    permissions:
      contents: read
      pull-requests: write
```

每个改动的文档都会转换成页面，可以从运行结果中下载；每个文档有几页、出现了几条警告，会以评论形式告诉你。
更多内容见 [GitHub 与智能体集成指南](docs/GITHUB_INTEGRATION.md)和[面向 AI 智能体的指南](docs/AI_USAGE_GUIDE.md)（英文）。

## 安全与局限

- 文件不会上传到任何地方，宏、脚本和外部链接也不会被执行或获取。
- 需要密码才能打开的 PDF 会被拒绝，而不是去破解密码。只设置了权限密码（例如“禁止打印”）的 PDF 会像在其他查看器中一样被打开，这些限制不会被强制执行。
- 输入大小、压缩包内容和页数都有上限。不要只是为了让难处理的文件通过而调高。
- 已有的文件绝不会被覆盖。
- 如果陌生人也能向你的服务上传文件，请在单独的、限制了内存和时间的进程里运行转换。
- 警告是“请检查”的信号，不是准确性或安全性的证明。医学影像和报告不会做匿名化处理。

在用于公开服务之前，请阅读[安全与局限](https://ryusui-hiro.github.io/document-svg/zh/safety.html)和[安全策略](SECURITY.md)。

### 平台支持与故障排查

| 平台（x64 和 ARM64） | npm 包 | Python 包 | 命令行 |
|---|---|---|---|
| Windows | 预编译 | 预编译 | 预编译 |
| macOS | 预编译 | 预编译 | 预编译 |
| Linux（Ubuntu、Debian 等 glibc 系统） | 预编译 | 预编译 | 预编译 |
| Linux（Alpine 等 musl 系统） | 预编译 | 从源码构建 | 预编译 |

- 如果 pip 开始从源码构建，说明没有适合你的 Python 和系统的预编译包，请先升级 pip。构建需要 Rust 和 C 链接器。
- 如果 npm 安装后无法加载，请确认可选依赖已安装，并且 Node.js 与机器的 CPU 类型一致。把 `node_modules` 复制到别的系统上是不能用的。
- SVG 页面可以在较新的浏览器、resvg、librsvg 2.46 及以上版本中显示。

## 绘制云架构图

除了转换之外，仓库里还有一个用 Azure、AWS 和 Google Cloud 官方图标绘制架构图的工具。
见[云架构指南](docs/CLOUD_ARCHITECTURE.md)和[业务架构图模板](docs/BUSINESS_ARCHITECTURE.md)。

## 参与开发

```sh
cargo test --workspace --locked
```

完整的检查步骤见 [CONTRIBUTING.md](CONTRIBUTING.md)，转换器的结构见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。
发布步骤见 [docs/PUBLISHING.md](docs/PUBLISHING.md)，变更记录见 [CHANGELOG.md](CHANGELOG.md)。

## 许可证

**MIT OR Apache-2.0**，任选其一。再分发时，请保留你所选许可证要求的版权声明和许可证文本。
见 [LICENSE](LICENSE)、[LICENSE-MIT](LICENSE-MIT)、[LICENSE-APACHE](LICENSE-APACHE) 以及附带的[第三方许可证](THIRD_PARTY_LICENSES.txt)。
