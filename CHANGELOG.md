# Changelog

## Unreleased

- Undo PNG predictors on Flate images here instead of relying on the PDF library. `/DecodeParms` given as an indirect reference or as an array parallel to `/Filter` was not read at all, and the Average row filter was reconstructed as `left + above / 2` rather than `(left + above) / 2`. Either one turns a predicted image into noise with no warning anywhere; a 15-page deck rendered as static on every page.
- Parse an embedded Type 1 font program once per page instead of once per text-showing operator. `parse_type1` decrypts the eexec section and scans the whole program, and re-running it per operator dominated Type 1-heavy PDFs: a 252-page document went from 168 s to 11 s with byte-identical output.
- Emit each image's data URI once, as SVG 2 `href`, instead of repeating it in `xlink:href` for pre-2019 renderers. Image-heavy output shrinks by 40-48%. Renderers older than librsvg 2.46 will no longer show these images.
- Resolve XML entity references in PPTX shape text, PPTX table text, DOCX text and chart labels. quick-xml reports these separately from text, so "R&D" reached the SVG as "RD"; 12.7% of the documents in the review corpus were losing characters this way.
- Ignore `a14:hiddenFill` / `a14:hiddenLine` compatibility paint on DOCX text boxes. Their black line colour was being read as the shape fill, covering the text with a solid black rectangle.
- Keep DOCX text box paragraphs that overflow the declared frame. Word's default `noAutofit` lets text spill out, so cutting the layout off at the frame height silently dropped every paragraph after the first in a short box.
- Read a PPTX shape's extent as a magnitude, so a shape written with a negative `cx`/`cy` keeps its text instead of being dropped, and say "zero-sized transform" rather than "no explicit transform" when a shape really has one.
- Resolve PPTX slide-number fields to the page number instead of emitting PowerPoint's cached `<#>` placeholder text.
- Materialize a DOCX `PAGE` field that has no `fldCharType="separate"` marker, so footers that Word has never calculated still show a page number.
- Label an embedded Office image from its byte signature instead of its file name. A JPEG stored as `media/image1.png` was declared `image/png` in the data URI and dropped entirely by strict SVG renderers.
- Keep converting a PDF when one image cannot be decoded. An unusable image is now a page warning instead of an error that discards every other page, and the JBIG2 filter is named in that warning rather than relaying the decoder's "missing feature" text.
- Budget a PDF image against the samples it actually expands to instead of assuming RGBA. Ordinary 600 DPI bilevel scans no longer fail the whole document against the "RGBA limit".
- Warn when a PDF page consumes content operators but draws nothing, and say so explicitly when the document was encrypted — a stream left encrypted by a failed per-object decryption previously produced a silently blank page.
- Name the empty slide list or worksheet list when a PPTX or XLSX declares none, instead of reporting "input contains no renderable pages".
- Describe the actual encrypted-PDF behaviour in the README, `SUPPORT.md` and `ARCHITECTURE.md`: a user password is refused, while the empty-user-password form that only carries permission flags is opened, as any viewer opens it.

## 0.1.1

- Add multi-platform release builds for Node.js bindings, Python ABI3 wheels and standalone CLI archives.
- Add checksum-verified publishing workflows for npm, PyPI and GitHub Packages, with separate build and publish permissions.
- Make the main and package READMEs English-first, with pip/npm/Cargo installation examples and an operational release guide.

- Integrate standard PDF font advances, bounded damaged-xref recovery, paper-aware XLSX pagination, PPTX fill handling and clearer encrypted/legacy Office errors.
- Add reproducible PDF/PPTX/XLSX/DOCX samples with checked source hashes.
- Bundle full third-party license notices, audit the locked dependencies and AFM provenance, and check notice freshness in CI.
- Upgrade the pinned checkout action and avoid duplicate branch/PR workflow runs.

- Replace the unmaintained font parser with `skrifa`, preserving TrueType/OpenType outlines and raw Type1/CFF fallbacks.
- Count serialized page size without retaining a second page-sized JSON buffer.
- Borrow SVG attribute/text strings when no escaping is needed, avoiding large image-data copies.
- Reuse the system font database across all pages of one reverse conversion.
- Ignore Python/native shared-library build artifacts in publication candidates.

## 0.1.0 — initial source release

- PDF, PPTX, XLSX and DOCX conversion to per-page SVG.
- Node.js in-memory previews, browser display helpers and Python bindings.
- SVG packaging into PPTX, DOCX and XLSX, without reconstructing Office semantics.
- Codex and Claude Code plugins, plus Japanese, English and Simplified Chinese preview guides.
- Bounded preview rendering, external-reference checks and non-overwriting atomic output publication.
- Dual licensing under MIT OR Apache-2.0, with required notice retention under the chosen license.

This is an alpha source release. Prebuilt npm/PyPI distributions and a separate GitHub CLI extension release are not yet published. Visual fidelity and supported format limitations are documented in `docs/SUPPORT.md`.
