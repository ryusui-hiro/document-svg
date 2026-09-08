# document-svg

**Preview PDF and Office documents as SVG pages in your application.**

A Rust library and CLI with Node.js and Python bindings. Convert PDF, PowerPoint,
Excel and Word files into one SVG per page, display them in a browser, and inspect
conversion warnings before sharing the result.

[日本語](README.ja.md) · [简体中文预览指南](bindings/node/docs/preview.zh-CN.md) ·
[Examples](samples/) · [Releases](https://github.com/ryusui-hiro/document-svg/releases)

## What you can do

| Task | API or command |
|---|---|
| Get SVG strings for an application preview | Node.js `preview()` |
| Save a document as SVG pages | `docsvg`, Rust `convert_path()`, Node/Python `convert()` |
| Display an SVG in your frontend | `document-svg/preview-ui` |
| Copy an SVG image or its source | `copySvgToClipboard()` |
| Put SVG pages into an Office file | `docsvg reverse`, Node/Python `reverse()` |

Supported inputs: **PDF, PPTX, XLSX and DOCX**. SVG-to-Office export supports
**PPTX, DOCX and XLSX**.

Document conversion runs locally in Rust. In a web application, run it on the
server or in an Electron main process and send the SVG to your frontend. The
browser helper does not load the native binding. This is not a browser-only
document converter.

## Install

| Environment | Command |
|---|---|
| Node.js 18+ | `npm install document-svg` |
| Python 3.10+ | `python -m pip install document-svg` |
| CLI, with Rust installed | `cargo install document-svg --locked` |

Choose **one** installation method for your use case; you do not need all three.
Use **npm** to call the converter from a Node.js/TypeScript application, **pip**
to call it from Python, or **Cargo** to install the standalone `docsvg` command.
The npm and pip packages are language bindings, not installations of the CLI.

### Python: install with pip

Create and activate a virtual environment, then install the Python package:

```sh
python -m venv .venv
# macOS / Linux:
source .venv/bin/activate
# Windows PowerShell instead: .venv\Scripts\Activate.ps1
python -m pip install --upgrade pip
python -m pip install document-svg
python -c "from document_svg import convert; print('Ready')"
```

The package name is `document-svg` (hyphen); the Python import is
`document_svg` (underscore). If your system uses `python3`, substitute it for
`python`. See the [Python example](#python) below.

### JavaScript / TypeScript: install with npm

Run this in your application's directory:

```sh
npm install document-svg
node -e "require('document-svg'); console.log('Ready')"
```

TypeScript declarations are included; no separate `@types` package is needed.
Keep npm optional dependencies enabled: they supply the native binary for your
platform. Run conversion in Node.js, not directly in a browser. See the
[Node.js example](#nodejs--typescript) below.

### Standalone command: install with Cargo

```sh
cargo install document-svg --locked
docsvg --version
docsvg report.pdf --output output/report
```

For use as a Rust library instead, run `cargo add document-svg` in your Rust
project. For a compiler-free CLI installation, download the archive matching
your operating system and CPU from GitHub Releases, extract it, and put `docsvg`
(or `docsvg.exe`) on your `PATH`.

### Platform support and troubleshooting

Native packages target Windows, macOS and Linux on x64 and arm64, with separate
glibc and musl Linux builds. Python wheels use the stable CPython ABI for
GIL-enabled Python. Matching prebuilt packages do not require a Rust compiler.

If pip attempts a source build, a matching wheel may not be available for your
Python/platform combination; update pip and check the release assets first.
Source builds require Rust and a native linker. If npm cannot load the native
binding, check that optional dependencies were installed and that your Node.js
architecture matches the machine. Do not copy `node_modules` between platforms.

For a prebuilt CLI or source archives, see [GitHub Releases](https://github.com/ryusui-hiro/document-svg/releases).
For authenticated GitHub Packages distribution, see [Publishing and releases](docs/PUBLISHING.md).
The public npm registry is the simplest choice for most JavaScript users.

### Build from source

```sh
git clone https://github.com/ryusui-hiro/document-svg.git
cd document-svg
cargo install --path . --locked
```

The Rust crate requires Rust 1.88 or newer. Release builds are tested with Rust 1.93.

## Quick start

### CLI

```sh
docsvg samples/source/sample.pptx --output output/slides
docsvg report.pdf --output output/report --max-pages 100
```

The output directory must be new or empty:

```text
output/slides/
├── page-0001.svg
├── page-0002.svg
└── conversion.json
```

Open the SVGs in a compatible browser, or use them as image sources in your app.
Excel worksheets may span several SVG pages according to paper size and print
settings. [The sample workbook](samples/source/sample.xlsx) produces four pages.

### Node.js / TypeScript

```js
const { preview } = require('document-svg')

async function main() {
  const report = await preview('slides.pptx', { maxPages: 100 })
  console.log(report.pageCount, report.needsReview)
  console.log(report.pages[0].svg)
}

main().catch(console.error)
```

`preview()` returns complete SVG strings and removes its temporary files.
Conversion runs on a worker without blocking the JavaScript event loop.

In your frontend bundle:

```js
import {
  createSvgPreviewUrl,
  revokeSvgPreviewUrl,
} from 'document-svg/preview-ui'

const url = createSvgPreviewUrl(svgMarkup)
imageElement.src = url

// Call this when replacing the image or disposing of the view.
const disposePreview = () => revokeSvgPreviewUrl(url)
```

Use an `<img>` for display rather than injecting arbitrary SVG into your DOM.
Text inside an `<img>` cannot be selected or searched. The clipboard helper
copies the SVG image when supported, otherwise its XML source.

[English preview guide](bindings/node/docs/preview.en.md) ·
[React example](bindings/node/examples/SvgPreview.tsx) ·
[HTML export example](bindings/node/examples/preview-to-html.cjs)

### Python

```python
from document_svg import convert

report = convert("report.docx", "output/report", max_pages=100)
print(report["page_count"])
print(report["warnings"])
```

Python conversion writes SVG files and returns a dictionary. See the
[Python binding guide](bindings/python/README.md) for options and source builds.

### Rust

```rust
use document_svg::{convert_path, ConvertOptions};

let report = convert_path("report.pdf", "output/report", &ConvertOptions::default())?;
println!("{} pages", report.page_count);
# Ok::<(), document_svg::Error>(())
```

### Export SVG pages to Office

```sh
docsvg reverse output/slides --output slides.pptx
```

Each SVG becomes a slide, document page or worksheet. Export embeds vector
images with PNG fallbacks; it does not reconstruct the original paragraphs,
cells, formulas or charts.

## Accuracy and safety

- Check document and page warnings. They identify unsupported features,
  approximations and review requirements; zero warnings is not a guarantee of
  pixel-identical Office rendering.
- Normal SVG text uses fonts available in the viewing environment. PDF embedded
  text can be outlined with `--outline-embedded-pdf-text` when appearance is the
  priority, at the cost of text editing and selection.
- Input size, ZIP entries, XML events and page count have configurable limits.
  Public upload services should also impose process-level memory and time limits.
- Existing output files are protected against overwriting. Encrypted documents
  are rejected; access controls are not bypassed.
- Preview helpers perform conservative checks, not universal SVG sanitization.

See [supported features and limitations](docs/SUPPORT.md), the
[security policy](SECURITY.md), and [performance measurements](docs/FONT_AND_PERFORMANCE_REVIEW.md).

## Codex and Claude Code

Both plugins are included in this repository. From a trusted source checkout:

```sh
./scripts/install-codex-plugin.sh
# Start a new Codex task.

./scripts/install-claude-plugin.sh
# Restart Claude Code.
```

Example request: “Use document-svg to convert this presentation into SVG previews
and report any warnings.”

[Plugin installation guide](docs/PLUGIN_INSTALLATION.md)

## Development

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 scripts/check-publication.py
```

[Contributing](CONTRIBUTING.md) · [Release operations](docs/PUBLISHING.md) ·
[Changelog](CHANGELOG.md) · [License audit](docs/LICENSE_AUDIT.md)

## License

**MIT OR Apache-2.0**, at your option. When redistributing the software, **retain
the copyright and license notices required by the license you choose**.

See [LICENSE](LICENSE), [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)
and the bundled [third-party notices](THIRD_PARTY_LICENSES.txt).
