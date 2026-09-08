# document-svg for Python

Python bindings for the Rust `document-svg` converter.

## Install

Requires Python 3.10+ (GIL-enabled CPython). In your virtual environment:

```sh
python -m pip install document-svg
```

Install `document-svg`, but import `document_svg`. Matching prebuilt wheels do
not require Rust. This package provides a Python API, not the `docsvg` CLI.

## Convert a document

```python
from document_svg import convert

report = convert("slides.pptx", "output", jobs=4)
print(report["page_count"])

fidelity = convert(
    "input.pdf",
    "output-fidelity",
    outline_embedded_pdf_text=True,
)
```

Package SVG pages as vector images in PPTX, DOCX or XLSX:

```python
from document_svg import reverse

report = reverse("svg-pages", "slides.pptx")
print(report["page_count"])
```

The input is a single SVG or a directory of SVG pages. This does not reconstruct
the original paragraphs, cells, formulas or other Office semantics.

Inspect the returned warnings before relying on conversion fidelity. Output
directories for conversion must be new or empty.

## Build from source

Build a local wheel from this directory:

```bash
python -m pip wheel --no-deps --wheel-dir dist .
```

Publish the generated wheels for each supported operating system and CPU to
PyPI. End users do not need a Rust toolchain when a matching wheel is present.
