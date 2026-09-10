# Repository guidance

This repository ships Codex and Claude Code plugins, a GitHub CLI extension and GitHub Actions for its `docsvg` CLI.
See `docs/GITHUB_INTEGRATION.md` for how each surface is wired together.

- The repository marketplace is `.agents/plugins/marketplace.json`.
- The plugin manifest is `plugins/document-svg/.codex-plugin/plugin.json`.
- The skill is `plugins/document-svg/skills/document-svg/SKILL.md`.
- The Claude Code slash commands are in `plugins/document-svg/commands/`; Codex uses the skill only.
- Every wrapper resolves the CLI through `plugins/document-svg/skills/document-svg/scripts/ensure-docsvg.sh`. Do not reintroduce per-script lookup logic.
- The Claude Code marketplace is `.claude-plugin/marketplace.json`; the same plugin directory also contains `.claude-plugin/plugin.json`.
- The GitHub CLI extension entrypoint is the root `gh-docsvg` executable. `gh extension install ryusui-hiro/document-svg` cannot work, because the GitHub CLI requires an extension repository name to start with `gh-`; the local installer is the supported path.
- The reusable GitHub Actions are `.github/actions/setup-docsvg` and `.github/actions/document-preview`, used by `.github/workflows/document-preview.yml`.
- Copilot reads `.github/copilot-instructions.md`; `.github/workflows/copilot-setup-steps.yml` prepares the coding agent environment.
- When a user asks to install or enable the plugin, run `./scripts/install-codex-plugin.sh` from the repository root and tell them to start a new Codex task afterward.
- When a user specifically asks for the Claude Code plugin, run `./scripts/install-claude-plugin.sh` and tell them to restart Claude Code afterward.
- When a user specifically asks for the GitHub CLI extension, run `./scripts/install-gh-extension.sh`; its command is `gh docsvg`.
- The installers no longer require Rust: without cargo they install a checksum-verified release build through `scripts/ensure-cli.sh`.
- Do not install any integration merely because the repository was opened or inspected, and do not run `ensure-docsvg.sh --install` without being asked.
- Cloud architecture authoring is separate from conversion: read `docs/CLOUD_ARCHITECTURE.md`, use `python3 authoring/cloud_icons.py search` to resolve real official icon IDs, then render a diagram JSON. Fetch missing assets with `fetch`; do not invent or redraw vendor icons. This tooling requires the repository checkout, not just the installed plugin.
- For business architecture diagrams, read `docs/BUSINESS_ARCHITECTURE.md` and start from an appropriate v2 template. Preserve cloud scope semantics, run geometry validation, inspect rendered SVG/PNG, and report warnings. Use sharp orthogonal connectors by default; rounded corners are optional. The renderer validates geometry, not actual cloud deployment correctness.
- For direct development use, run `cargo run --bin docsvg -- <input> --output <directory>`.
- For SVG-to-OOXML development use, run `cargo run --bin docsvg -- reverse <svg-or-directory> --output <file.pptx|docx|xlsx>` and disclose that Office semantics are not reconstructed.
- Shape libraries are drawn only when the caller passes stencil files (`--stencils`, `ConvertOptions::stencil_paths`); the repository does not bundle vendor icons. Say which shapes were drawn from a library and which stayed placeholders rather than implying full fidelity.
- `reverse` also writes `.drawio`. An SVG that carries its diagram source is restored as editable shapes; one that does not becomes a picture in a diagram, and that difference must be stated rather than implied. `--embed-drawio-source` is what puts the source into an SVG on the way out.
- `cargo package` leaves a copy of the crate under `target/package`, and cargo then stops rebuilding the ordinary debug artefacts from `src/`. Delete it and run `cargo clean -p document-svg` before the next `cargo test`, or the tests run against a stale binary and fail — or pass — for reasons that are not in the source.
- The Python binding tests need the extension module built and installed first: `python3 -m venv .venv`, then `VIRTUAL_ENV=$PWD/.venv .venv/bin/maturin develop` from `bindings/python`, then `.venv/bin/python -m pytest tests`. Running pytest without that step fails on `No module named 'document_svg'`, which is a missing build rather than a broken binding.
