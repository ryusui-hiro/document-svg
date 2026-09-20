---
description: Transform an SVG file with minification, responsive scaling, monochrome conversion, precision tuning, or metadata removal.
argument-hint: <input-svg> [output-svg] [--minify] [--responsive] [--monochrome <#hex>] [--precision <digits>] [--remove-metadata]
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/skills/document-svg/scripts/*), Read, Glob
---

Transform `$1` into `$2` (or `<input-stem>.transformed.svg` if omitted).

1. Resolve `$1` to an existing `.svg` file.
2. Use `$2` as the output file when provided; otherwise default to `<input-stem>.transformed.svg` beside the input.
3. Run:

   ```bash
   ${CLAUDE_PLUGIN_ROOT}/skills/document-svg/scripts/transform-document.sh <input-svg> <output-svg> [options...]
   ```

   Available options:
   - `--minify`: Compact SVG content by collapsing whitespace, removing comments and redundant tokens
   - `--responsive`: Remove fixed width/height attributes and ensure viewBox is set for responsive web scaling
   - `--monochrome <color>`: Unify all stroke and fill colors to a single color (e.g. `#000000`)
   - `--precision <digits>`: Round coordinate and path numbers to N decimal digits
   - `--remove-metadata`: Strip `<metadata>`, `<desc>`, and `data-*` attributes

4. Report the resulting file size and applied transforms. If `--minify` was applied, report the byte reduction and percentage saved.
