# Changelog

## Unreleased

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
