# document-svg for Python

Python bindings for the Rust `document-svg` converter.

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

SVGをベクター画像のままPPTX、DOCX、XLSXへ格納する逆変換:

```python
from document_svg import reverse

report = reverse("svg-pages", "slides.pptx")
print(report["page_count"])
```

入力には単一SVGまたはSVGディレクトリを指定します。元文書の段落、セル、数式などの
意味構造は復元されません。

Build a local wheel from this directory:

```bash
python -m pip wheel --no-deps --wheel-dir dist .
```

Publish the generated wheels for each supported operating system and CPU to
PyPI. End users do not need a Rust toolchain when a matching wheel is present.
