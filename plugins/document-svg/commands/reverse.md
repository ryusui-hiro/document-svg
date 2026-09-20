---
description: Package SVG pages back into a PPTX, DOCX, XLSX, PDF, draw.io, HTML viewer, or WebP file.
argument-hint: <svg-file-or-directory> <output.pptx|docx|xlsx|pdf|drawio|html|webp>
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/skills/document-svg/scripts/*), Read, Glob
---

Package `$1` into the target file `$2`.

1. Resolve `$1` to one `.svg` file or a directory of SVG pages. Directory pages
   are ordered lexicographically, so `page-NNNN.svg` names give the intended
   order; check the names before running and warn if the order looks wrong.
2. `$2` must not already exist and must end in `.pptx`, `.docx`, `.xlsx`, `.pdf`, `.drawio`, `.html` or `.webp`.
3. Run:

   ```bash
   ${CLAUDE_PLUGIN_ROOT}/skills/document-svg/scripts/reverse-document.sh <input> <output>
   ```

4. Report the output format and page count.

Always disclose that the SVG pages are embedded as vector images with PNG
compatibility fallbacks. Paragraphs, tables, spreadsheet cells and formulas,
charts, comments and masters are not reconstructed. Never describe this as a
lossless restoration of an original Office document.

A `.drawio` output is the one case where pages can come back as editable shapes:
that happens only for an SVG that still carries its diagram source, and the
report says how many pages were restored that way. Report that number rather
than assuming it; the rest are pictures inside a diagram.
