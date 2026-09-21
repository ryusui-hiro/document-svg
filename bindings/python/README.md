# document-svg for Python

**Turn PDF, Word, Excel, PowerPoint, diagram and CAD files into SVG pages from Python — locally, without Office.**

[Project site](https://ryusui-hiro.github.io/document-svg/) ·
[Formats](https://ryusui-hiro.github.io/document-svg/formats.html) ·
[Samples](https://ryusui-hiro.github.io/document-svg/samples.html) ·
[日本語](https://github.com/ryusui-hiro/document-svg/blob/main/README.ja.md) ·
[简体中文](https://github.com/ryusui-hiro/document-svg/blob/main/README.zh-CN.md)

`document-svg` turns a document, diagram or drawing into ordinary SVG images,
one per page, that any browser can show. It also tells you, page by page, what
it could not reproduce exactly, so you can decide whether the result is good
enough to use.

- **Files stay on your machine.** Nothing is uploaded and there is no account.
- **Nothing inside a file is run.** Macros, scripts and external links are shown or skipped, never executed.
- **No Office, no viewer, no headless browser.** One package handles Office files, PDFs, diagrams, CAD and 3D models, and many data formats.

The same converter is also available [for Node.js on npm](https://www.npmjs.com/package/document-svg)
and [for Rust, with the `docsvg` command, on crates.io](https://crates.io/crates/document-svg).

## Install

Requires Python 3.10 or newer.

```sh
python -m pip install document-svg
```

Install `document-svg`, import `document_svg`. Ready-made packages cover
Windows, macOS and Linux (Ubuntu, Debian and other glibc systems) on x64 and
ARM64, so you don't need Rust. On Alpine and other musl systems, pip builds
from source, which needs Rust and a C linker. This package gives you a Python
API; for the `docsvg` command, see crates.io.

## Save SVG pages to a folder

```python
from document_svg import convert

report = convert("report.pdf", "out/report")
print(report["page_count"], report["warnings"])
```

The output folder must be new or empty. It receives `page-0001.svg`,
`page-0002.svg`, … and a `conversion.json` report. Conversion runs without
holding Python's GIL, so other threads keep working.

Useful options:

- `max_pages` — stop after this many pages.
- `jobs` — convert several pages at once.
- `outline_embedded_pdf_text=True` — for PDFs, draw text as shapes so it looks
  exactly like the original on any computer (the text can no longer be
  selected).
- `embed_drawio_source=True` — for draw.io files, keep the diagram inside each
  SVG so it can later be turned back into an editable diagram.

## Get SVG pages in memory

`preview()` returns every page as SVG text without writing files — handy for
a web API or a notebook.

```python
from document_svg import preview

result = preview("slides.pptx", max_pages=20)
first_page_svg = result["pages"][0]["svg"]

if result["needs_review"]:
    print("Some parts were approximated:", result["warnings"])
```

When you show pages in a web page, show them as images (for example an
`<img>` element) rather than inserting the SVG markup into your HTML.

## Read the result before you share it

Every result has `warnings`: things that were approximated or left out, such
as a substituted font or an unsupported fill. Finishing without an error is not
the same as perfect. When there are warnings, have a person look at the pages.

Text uses the fonts available where the SVG is shown, so it can look slightly
different on another computer.

## Turn SVG pages back into files

```python
from document_svg import reverse

reverse("out/slides", "slides.pptx")   # a folder of pages
reverse("drawing.svg", "drawing.dxf")  # or a single SVG
```

The extension decides the format: PowerPoint, Word, Excel, draw.io, DXF,
G-code, Gerber, HP-GL and more. What comes back is how the pages look;
paragraphs, cells and formulas are not rebuilt. A draw.io diagram converted
with `embed_drawio_source=True` comes back editable.

## Tidy up an SVG

```python
from document_svg import transform

smaller = transform(svg, minify=True, remove_metadata=True)
```

`transform()` can also recolour to one colour, make the SVG scale to its
container, and clean up paths and empty groups. It returns `str` for `str`
input and `bytes` for `bytes` input.

## Which files work?

PDF, Word, Excel and PowerPoint (current and older formats), OpenDocument,
e-mail, e-books, draw.io, Visio, Mermaid, PlantUML, DXF, Gerber, G-code, STL,
STEP, IFC, simulation meshes, CSV, JSON, maps, images, DICOM and more. Some
formats are drawn in full; others show a summary of their contents. Look up
your file type in the [searchable format list](https://ryusui-hiro.github.io/document-svg/formats.html).

## Safety

- Treat uploaded files as untrusted. On a public service, run conversion in a
  separate process with memory and time limits.
- Input size, archive contents and page count are limited by default. Don't
  raise the limits just to push a difficult file through.
- A PDF that needs a password to open is refused.
- Medical images and reports are not anonymized: personal information in the
  file can appear in the output.

More in [Safety and limits](https://ryusui-hiro.github.io/document-svg/safety.html)
and the [security policy](https://github.com/ryusui-hiro/document-svg/blob/main/SECURITY.md).

## Building from source

For contributors, from `bindings/python` (Rust required):

```sh
python -m pip wheel --no-deps --wheel-dir dist .
```

## License

MIT OR Apache-2.0. See the
[repository](https://github.com/ryusui-hiro/document-svg#license) for the
license texts and third-party notices.
