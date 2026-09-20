---
description: Convert documents, diagrams, CAD, 3D, mesh, charts, geospatial, math, or data files into per-page SVG files.
argument-hint: <input-document> [output-directory]
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/skills/document-svg/scripts/*), Read, Glob
---

Convert `$1` into per-page SVG files.

1. Resolve `$1` to any supported document (PDF, Office, Markdown, text, emails, subtitles), diagram (draw.io, Mermaid, Graphviz, PlantUML, D2), CAD/mesh (DXF, Gerber, STL, OBJ, STEP, IFC), chart (.chart.json), geospatial (GeoJSON, GPKG, Shapefile), math (.tex), or data (CSV, JSON) file. Ask the user if it is ambiguous rather than guessing.
2. Use `$2` as the output directory when given; otherwise use `<input-stem>-svg`
   beside the input. Never write into a directory that already has content
   unless the user explicitly asked for that.
3. Run:

   ```bash
   ${CLAUDE_PLUGIN_ROOT}/skills/document-svg/scripts/convert-document.sh <input> <output>
   ```

   If it exits with code 69, docsvg is not installed. Show the printed options
   and ask the user which one to run; do not install anything on your own.
4. Read `<output>/conversion.json` and report the page count, source format,
   output directory, and every top-level warning and per-page `warning_count`.

A non-empty `warnings` array or a positive `warning_count` means the result is
not a guaranteed visual reproduction. Say so plainly instead of calling the
conversion perfect.
