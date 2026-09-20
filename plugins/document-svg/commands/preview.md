---
description: Render SVG pages to deterministic PNG previews for inline viewing.
argument-hint: <svg-file-or-directory> [preview-directory] [width]
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/skills/document-svg/scripts/*), Read, Glob
---

Render `$1` to PNG previews.

Markdown viewers, Quick Look and embedded browsers disagree about SVG support,
so a PNG companion is the reliable way to show a page inline.

1. Use `$2` as the preview directory when given; otherwise pick a new directory
   beside the SVG pages. The runner refuses a non-empty directory.
2. Run:

   ```bash
   ${CLAUDE_PLUGIN_ROOT}/skills/document-svg/scripts/render-preview.sh <input> <preview-directory> [width]
   ```

3. Read `preview.json` for page ordering, display the generated absolute `.png`
   paths, and link the original `.svg` files separately.

The preview runner rejects SVG files containing active content. If it refuses an
input, report that rather than working around it.
