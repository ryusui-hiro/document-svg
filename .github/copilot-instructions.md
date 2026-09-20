# Copilot instructions

`document-svg` is a Rust workspace that converts 60+ document, diagram,
CAD/CAM/3D, CAE mesh, chart, geospatial, math, and data formats into
deterministic per-page SVG files, transforms SVG files, and packages SVG pages
back into OOXML, draw.io, HTML viewer, and WebP.
The supported entrypoint is the `docsvg` CLI; `bindings/node` and
`bindings/python` wrap the same Rust core.

Read [AGENTS.md](../AGENTS.md) for the repository map. It is the source of truth
for plugin, skill and extension layout; this file only adds Copilot-specific
notes.

## Build and check

```bash
cargo build --bin docsvg
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 scripts/check-publication.py
```

Run the same checks as `.github/workflows/publication-check.yml` before
proposing a change. The pinned toolchain is Rust 1.93.0.

## Running the converter

```bash
cargo run --bin docsvg -- <input> --output <directory>
cargo run --bin docsvg -- reverse <svg-or-directory> --output <file.pptx|docx|xlsx|html|webp>
cargo run --bin docsvg -- transform <input.svg> --output <output.svg> [options]
```

Conversion writes `page-NNNN.svg` plus `conversion.json`. Always read
`conversion.json` and report every top-level warning and per-page
`warning_count`.

## Accuracy rules

These are correctness requirements, not style preferences.

- A non-empty `warnings` array or a positive `warning_count` means the output is
  **not** a guaranteed visual reproduction. Never describe such a result as
  perfect or lossless.
- Reverse conversion embeds SVG pages as vector images with PNG fallbacks. It
  does **not** reconstruct paragraphs, tables, cells, formulas, charts, comments
  or masters. Never call it a lossless restoration of the original document.
- `--outline-embedded-pdf-text` converts text to outlines. It trades editable
  text for fidelity and can raise font-rights warnings; report them and do not
  claim the text stays editable.
- The converter rejects encrypted PDFs by design. Do not add bypasses.
- The runner refuses non-empty output directories and refuses to overwrite an
  existing OOXML output. Preserve those guards.

## Boundaries

- Do not install the Codex plugin, Claude Code plugin or GitHub CLI extension
  just because the repository was opened. Install only when explicitly asked.
- Do not expose internal PDF/OOXML parser modules as a public interface; the
  `docsvg` CLI and the published bindings are the supported surface.
- Do not commit generated previews, compiled artifacts or sample documents that
  are not already tracked.
- Keep GitHub Actions pinned by commit SHA, matching the existing workflows.

## Using docsvg from your own workflow

```yaml
- uses: ryusui-hiro/document-svg/.github/actions/setup-docsvg@main
  with:
    version: latest
- run: docsvg report.pdf --output preview/report
```

See [docs/GITHUB_INTEGRATION.md](../docs/GITHUB_INTEGRATION.md) for the pull
request preview workflow and the MCP-free agent integrations.
