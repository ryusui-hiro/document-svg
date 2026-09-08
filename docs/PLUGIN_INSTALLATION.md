# Codex / Claude Code installation

Both integrations are included in the same repository and use the local `docsvg` CLI. They require Rust/Cargo plus the corresponding host CLI. PNG previews also require Python 3 and `rsvg-convert`.

Run from a trusted repository checkout:

| Host | Install | Activate |
|---|---|---|
| Codex | `./scripts/install-codex-plugin.sh` | Start a new task |
| Claude Code | `./scripts/install-claude-plugin.sh` | Restart Claude Code |

Example prompts:

```text
Use document-svg to preview report.docx as SVG pages and report conversion warnings.
document-svgを使ってslides.pptxをSVGプレビューにして、警告も確認して。
使用document-svg将report.pdf转换为逐页SVG预览，并检查转换警告。
```

Codex supports `$document-svg`; Claude Code supports `/document-svg:document-svg`.

The installers build the CLI locally. Registering only the marketplace does not install the native converter. A copied plugin can use an installed `docsvg` on PATH or an explicit `DOCUMENT_SVG_REPO` checkout. Installation is a user action; opening the repository does not trigger it.
