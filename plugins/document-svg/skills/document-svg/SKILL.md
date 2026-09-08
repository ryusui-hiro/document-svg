---
name: document-svg
description: Convert PDF, PPTX, XLSX, or DOCX files into deterministic per-page SVG files, or package SVG pages back into PPTX, DOCX, or XLSX with the repository's docsvg CLI. Use for document-to-SVG conversion, SVG previews, batch rendering, and appearance-preserving SVG-to-OOXML conversion; do not claim that reverse conversion reconstructs original Office semantics.
---

# Document SVG

Use this skill when the requested output is a directory of self-contained SVG pages. The converter selects the parser from the input file and writes `page-NNNN.svg` plus `conversion.json`.

## Convert

1. Resolve the input to a local `.pdf`, `.pptx`, `.xlsx`, or `.docx` file.
2. Choose a dedicated output directory. If the user did not name one, use `<input-stem>-svg` beside the input. Never reuse a non-empty directory unless the user explicitly asks to mix or replace its contents.
3. Run the bundled wrapper using its absolute path from this skill directory:

   ```bash
   scripts/convert-document.sh <input> <output> [docsvg options]
   ```

   Use `--jobs N` only to parallelize PDF pages. Keep the default limits unless the user explicitly needs different bounds.
   Use `--outline-embedded-pdf-text` only when the user explicitly prioritizes PDF visual fidelity over editable SVG text. Report every resulting font-rights warning and do not describe outlined text as editable.
4. Read `<output>/conversion.json`. Report the page count, source format, output directory, and every top-level or page warning. Treat a non-empty `warnings` array or a positive page `warning_count` as requiring review; do not describe that result as a perfect reproduction.
5. When visual fidelity matters, render or inspect representative SVG pages with the available image/browser tooling.
   For Codex/app previews, prefer a deterministic PNG companion because Markdown, Quick Look,
   and embedded browser SVG support can differ. Run:

   ```bash
   scripts/render-preview.sh <svg-file-or-directory> <new-preview-directory> [width]
   ```

   Display the generated absolute `.png` path in Codex and link the original `.svg` separately.
   Read `preview.json` for page ordering. The preview runner rejects active SVG content and refuses
   non-empty output directories.

## Reverse to OOXML

1. Resolve the input to one `.svg` file or a directory containing SVG pages. Directory pages are ordered lexicographically, so prefer `page-NNNN.svg` names.
2. Choose a new output file ending in `.pptx`, `.docx`, or `.xlsx`.
3. Run:

   ```bash
   scripts/reverse-document.sh <input.svg-or-directory> <output.pptx-or-docx-or-xlsx>
   ```

4. Report the output format and page count, and always disclose that the SVGs are embedded as vector images with PNG compatibility fallbacks. PPTX uses one SVG per slide, DOCX one SVG per page, and XLSX one SVG per sheet.
5. Do not describe reverse conversion as a lossless restoration of the original Office document. Paragraphs, tables, spreadsheet cells and formulas, charts, comments, masters, and other semantics are not reconstructed.

## CLI availability

The repository installer normally places `docsvg` on `PATH`. If it is unavailable, the wrapper can run the checked-out Rust repository when either the current directory is that repository or `DOCUMENT_SVG_REPO` points to it. Otherwise, ask the user to run `./scripts/install-codex-plugin.sh` from the repository root.

## Constraints

- The converter intentionally rejects encrypted PDFs and does not bypass access controls.
- Preserve source files. The runner refuses non-empty output directories to avoid mixing stale and new pages.
- Reverse conversion refuses to overwrite an existing OOXML output file.
- Do not expose internal PDF/OOXML parser modules for ordinary use; the `docsvg` CLI is the supported entrypoint.
