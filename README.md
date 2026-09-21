# document-svg

**See every page of a PDF, Word, Excel, PowerPoint, diagram or CAD file as an SVG image — on your own machine, without the app that made it.**

[日本語](README.ja.md) · [简体中文](README.zh-CN.md) ·
[Project site](https://ryusui-hiro.github.io/document-svg/) ·
[Samples](https://ryusui-hiro.github.io/document-svg/samples.html) ·
[Releases](https://github.com/ryusui-hiro/document-svg/releases)

[![npm](https://img.shields.io/npm/v/document-svg?label=npm)](https://www.npmjs.com/package/document-svg)
[![PyPI](https://img.shields.io/pypi/v/document-svg?label=PyPI)](https://pypi.org/project/document-svg/)
[![crates.io](https://img.shields.io/crates/v/document-svg?label=crates.io)](https://crates.io/crates/document-svg)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

document-svg turns documents, diagrams and technical drawings into ordinary
SVG images, one per page. Any browser can show them, they stay sharp when you
zoom in, and they drop into a web page like any other picture. Alongside the
pages you get a short report that says plainly which parts could not be
reproduced exactly.

It is a converter you build into something else — a website, an internal
tool, a review process or an AI assistant — and it comes as a Node.js
package, a Python package, a Rust library and a command-line tool (`docsvg`).

| PowerPoint | Excel | draw.io | CAD drawing (DXF) |
|---|---|---|---|
| <img src="https://ryusui-hiro.github.io/document-svg/assets/samples/pptx/page-0001.svg" alt="A PowerPoint slide converted to SVG" width="200"> | <img src="https://ryusui-hiro.github.io/document-svg/assets/samples/xlsx/page-0001.svg" alt="An Excel sheet converted to SVG" width="200"> | <img src="https://ryusui-hiro.github.io/document-svg/assets/samples/drawio/page-0001.svg" alt="A draw.io diagram converted to SVG" width="200"> | <img src="https://ryusui-hiro.github.io/document-svg/assets/samples/dxf/page-0001.svg" alt="A DXF drawing converted to SVG" width="200"> |

Every image above is an unretouched conversion of a sample file in this repository.

## Why use it

- **Your files stay with you.** Conversion runs on your computer or your own
  server. Nothing is uploaded, there is no account, and nothing is tracked, so
  contracts and design drawings that must not leave the company can still get
  a preview.
- **Anyone can look.** The people reviewing the result don't need Office,
  AutoCAD or a viewer licence — a browser is enough.
- **It tells you what it missed.** No converter reproduces everything. Instead
  of quietly dropping a chart or a font, document-svg lists what it skipped or
  approximated, page by page, so you can decide whether the result is good
  enough to share.
- **Opening a file never runs it.** Macros, scripts, form actions and links to
  outside resources are shown or skipped, never executed. Size and page limits
  stop a huge or broken file from exhausting memory.
- **One tool instead of many.** Office documents, PDFs, diagrams, CAD and 3D
  models, and scientific and business data all go through the same call and
  come out the same way.
- **And the way back.** SVG pages can be packaged back into PowerPoint, Word,
  Excel, draw.io or CAD files.

## What people use it for

- **Seeing document changes in code review.** A GitHub Action converts the
  slides, spreadsheets and PDFs changed in a pull request, instead of leaving
  reviewers with "binary file changed".
- **Previewing uploads in their own app,** without installing Office on the
  server or paying for a viewer SDK.
- **Keeping diagrams in step with the code** by rendering draw.io, Mermaid,
  PlantUML, D2 or Graphviz files every time the documentation is built.
- **Checking drawings and 3D models without CAD** — floor plans, circuit
  boards, machining paths and models — from a web page.
- **Letting an AI assistant read documents** through one dependable step
  instead of screenshots.

Each example is explained in more detail on the [project site](https://ryusui-hiro.github.io/document-svg/use-cases.html).

## Install

Pick the one that matches where you will call it from. You only need one.

| You want to… | Install |
|---|---|
| Convert files from a Node.js or Electron app (Node.js 18 or newer) | `npm install document-svg` |
| Convert files from Python (Python 3.10 or newer) | `python -m pip install document-svg` |
| Use the `docsvg` command, or the Rust library | `cargo install document-svg --locked` / `cargo add document-svg` |

No Rust is needed for the npm and pip packages: they come ready-made for
Windows, macOS and Linux. If you want the command without installing Rust,
download the file for your system from
[Releases](https://github.com/ryusui-hiro/document-svg/releases) and put
`docsvg` on your `PATH`.

### Where to get it and where it goes

| Package | Page | Installed into |
|---|---|---|
| npm `document-svg` | [npmjs.com/package/document-svg](https://www.npmjs.com/package/document-svg) | Your project's `node_modules/document-svg`, plus one ready-made native package for your system, such as `node_modules/document-svg-darwin-arm64` |
| PyPI `document-svg` | [pypi.org/project/document-svg](https://pypi.org/project/document-svg/) | The active environment's `site-packages/document_svg` |
| crates.io `document-svg` | [crates.io/crates/document-svg](https://crates.io/crates/document-svg) | `cargo install` puts `docsvg` in `~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on Windows); `cargo add` adds the library to your project |
| GitHub Releases | [Releases](https://github.com/ryusui-hiro/document-svg/releases) | Wherever you unpack the archive; put `docsvg` on your `PATH` |

To check what is installed and where: `npm ls document-svg`,
`python -m pip show document-svg`, or `docsvg --version`.
The Rust API reference is on [docs.rs](https://docs.rs/document-svg). An
authenticated npm mirror on GitHub Packages is described in
[docs/PUBLISHING.md](docs/PUBLISHING.md#installing-the-github-packages-mirror).

## Try it

```sh
docsvg slides.pptx --output out/slides
```

The output folder must be new or empty. You get one SVG per page and one
report of the conversion:

```text
out/slides/
├── page-0001.svg
├── page-0002.svg
└── conversion.json
```

Open a page in any browser. The report lists **warnings** — things that were
approximated or left out, such as a substituted font or an unsupported fill.
Finishing without an error is not the same as perfect: if there are warnings,
have a person look at the pages before you share them.

A "page" means what you would expect for each kind of file: a PDF page, a
slide, a printed page of a Word document, a printed page of a worksheet, or one
page of a draw.io diagram.

### From Node.js / TypeScript

```js
const { convert, preview } = require('document-svg')

async function main() {
  // Write SVG files to a folder
  const report = await convert('slides.pptx', 'out/slides')
  console.log(report.pageCount, report.warnings)

  // Or keep the pages in memory, for example to send them to a browser
  const result = await preview('slides.pptx')
  const firstPageSvg = result.pages[0].svg
  console.log('needs review:', result.needsReview)
}

main().catch(console.error)
```

Conversion runs in Node.js (a server or Electron's main process), off the main
thread. In the browser, show each page as an image with the small
`document-svg/preview-ui` helper, which does not load any native code. See the
[Node.js guide](bindings/node/README.md).

### From Python

```python
from document_svg import convert, preview

report = convert("report.docx", "out/report")
print(report["page_count"], report["warnings"])

result = preview("report.docx")          # pages in memory, no files written
first_page_svg = result["pages"][0]["svg"]
```

See the [Python guide](bindings/python/README.md).

### From Rust

```rust
use document_svg::{convert_path, ConvertOptions};

fn main() -> Result<(), document_svg::Error> {
    let report = convert_path("report.pdf", "out/report", &ConvertOptions::default())?;
    println!("{} pages, {} warnings", report.page_count, report.warnings.len());
    Ok(())
}
```

API documentation is on [docs.rs](https://docs.rs/document-svg).

### Only in the browser

A separate WebAssembly build converts PDF, Word, Excel and PowerPoint files
entirely inside the browser, without a server and without uploading the
file. It is not published as a package; you build it yourself from
[`bindings/wasm`](bindings/wasm/README.md).

## Show the pages in your app

Display each page as an image (for example an `<img>` element). Don't paste
the SVG markup straight into your page's HTML: the included display helpers do
basic checks, but they are not a filter that makes any SVG harmless. Text in
an image cannot be selected or searched.

Text uses the fonts available where the SVG is viewed, so it can look slightly
different on another computer. For PDFs, you can turn embedded text into
outlines when exact appearance matters more than selectable text.

## Turn SVG back into Office or CAD files

```sh
docsvg reverse out/slides --output slides.pptx
docsvg reverse out/report --output report.docx
docsvg reverse out/drawing --output drawing.dxf
```

The file extension you choose decides the format: PowerPoint, Word, Excel,
PDF, draw.io, DXF, G-code, STL and more. What comes back is how the pages
look — paragraphs, cells, formulas and charts are not rebuilt.

draw.io diagrams are the exception: convert with `--embed-drawio-source`, and
`reverse` restores the original, editable diagram.

## Supported formats

| Field | Examples |
|---|---|
| Office and documents | PDF, Word, Excel, PowerPoint (current and older formats), OpenDocument, RTF, e-mail, calendars, e-books, Markdown, HTML |
| Diagrams | draw.io, Visio, Mermaid, PlantUML, D2, Graphviz, BPMN |
| CAD, CAM and 3D | DXF, Gerber, G-code, HP-GL, STL, OBJ, STEP, IGES, glTF, IFC |
| Simulation | Gmsh, VTK, OpenFOAM, Abaqus, Nastran and other meshes |
| Data and maps | CSV, JSON, YAML, charts, LaTeX formulas, GeoJSON, KML, GeoPackage |
| Images | PNG, JPEG, TIFF, WebP, DICOM, SVG |

Some formats are drawn in full; for others, such as DWG or Access databases,
you get a clear summary of what is inside rather than the drawing. Each format
is rated A (drawn from the file's own contents), B (traced from pixels) or C
(structure such as tables inferred from the drawing).

- [Searchable format list](https://ryusui-hiro.github.io/document-svg/formats.html) — look up your file type
- [Technical format matrix](docs/FORMATS.md) — every extension with its fidelity notes
- [Supported features and limitations](docs/SUPPORT.md)

## Use it from AI assistants and GitHub

| Where | How |
|---|---|
| Claude Code | `/plugin marketplace add ryusui-hiro/document-svg`, then `/plugin install document-svg@document-svg` |
| Codex | Run `./scripts/install-codex-plugin.sh` in a checkout, then start a new task |
| GitHub Copilot | Add the setup action below to your Copilot setup steps |
| GitHub CLI | Run `./scripts/install-gh-extension.sh`, then `gh docsvg convert report.pdf --output preview/report` |
| GitHub Actions | `uses: ryusui-hiro/document-svg/.github/actions/setup-docsvg@main` |

The installers use a checksum-verified release build when Rust is not
installed. To preview the documents a pull request changes, add this workflow
to your repository:

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

Each changed document is converted into pages you can download from the run,
and a comment lists how many pages and warnings each one has. More in the
[GitHub and agent integration guide](docs/GITHUB_INTEGRATION.md) and the
[guide for AI agents](docs/AI_USAGE_GUIDE.md).

## Safety and limits

- Files are never uploaded, and macros, scripts and external links are never
  run or fetched.
- A PDF that needs a password to open is refused, not cracked. A PDF whose only
  restriction is an owner password (such as "no printing") opens as it would in
  any viewer; those flags are not enforced.
- Input size, archive contents and page count have limits. Don't raise them
  just to push a difficult file through.
- Existing files are never overwritten.
- If strangers can upload files to your service, run conversion in a separate,
  restricted process with memory and time limits.
- Warnings are a signal to review, not a certificate of accuracy or safety.
  Medical images and reports are not anonymized.

Read [Safety and limits](https://ryusui-hiro.github.io/document-svg/safety.html)
and the [security policy](SECURITY.md) before using it on a public service.

### Platform support and troubleshooting

| Platform (x64 and ARM64) | npm package | Python package | Command |
|---|---|---|---|
| Windows | Ready-made | Ready-made | Ready-made |
| macOS | Ready-made | Ready-made | Ready-made |
| Linux (Ubuntu, Debian and other glibc systems) | Ready-made | Ready-made | Ready-made |
| Linux (Alpine and other musl systems) | Ready-made | Built from source | Ready-made |

- If pip starts building from source, there is no ready-made package for your
  Python and system; update pip first. Building needs Rust and a C linker.
- If the npm package installs but won't load, check that optional dependencies
  were installed and that Node.js matches your machine's CPU. Copying
  `node_modules` to another system does not work.
- The SVG pages display in current browsers, resvg, and librsvg 2.46 or newer.

## Drawing cloud architecture diagrams

Separately from conversion, the repository includes a tool that builds
architecture diagrams with the official Azure, AWS and Google Cloud icons. See
the [cloud architecture guide](docs/CLOUD_ARCHITECTURE.md) and the
[business diagram templates](docs/BUSINESS_ARCHITECTURE.md).

## Contributing

```sh
cargo test --workspace --locked
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full checks, and
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how the converter is put
together. Release steps are in [docs/PUBLISHING.md](docs/PUBLISHING.md) and
changes in the [changelog](CHANGELOG.md).

## License

**MIT OR Apache-2.0**, at your option. When you redistribute the software, keep
the copyright and license notices required by the license you choose. See
[LICENSE](LICENSE), [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)
and the bundled [third-party notices](THIRD_PARTY_LICENSES.txt).
