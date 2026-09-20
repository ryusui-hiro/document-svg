# GitHub and agent integration

How to reach `docsvg` from GitHub Actions, GitHub Copilot, the GitHub CLI,
Codex and Claude Code. Everything below lives in this repository; nothing
requires a second repository.

## GitHub Actions

### Install the CLI in a workflow

```yaml
- uses: ryusui-hiro/document-svg/.github/actions/setup-docsvg@main
  with:
    version: latest          # or a released version such as 2.0.0
- run: docsvg report.pdf --output preview/report
```

The action downloads the release archive for the runner's platform from this
repository's releases and verifies it against the release `SHA256SUMS` before
putting `docsvg` on `PATH`. No Rust toolchain is installed. Linux runners get
the statically linked musl build.

Outputs: `docsvg-path` and `version`.

### Preview the documents a pull request changes

Add this to any repository that keeps PDFs or Office files under review:

```yaml
name: Document preview
on:
  pull_request:
    paths: ['**.pdf', '**.pptx', '**.xlsx', '**.docx']
jobs:
  preview:
    uses: ryusui-hiro/document-svg/.github/workflows/document-preview.yml@main
    permissions:
      contents: read
      pull-requests: write
```

Each changed document is converted to SVG pages, the pages are attached to the
run as the `document-svg-preview` artifact, and a table of page counts and
warning counts is written to the job summary and posted as a pull request
comment. The comment step is skipped for pull requests from forks, where the
workflow token is read-only.

Inputs: `docsvg-version`, `max-documents` (default 20) and `comment`.

To build the report inside a workflow you already have, use the action directly:

```yaml
- uses: ryusui-hiro/document-svg/.github/actions/setup-docsvg@main
- uses: ryusui-hiro/document-svg/.github/actions/document-preview@main
  with:
    base: ${{ github.event.pull_request.base.sha }}
    head: ${{ github.event.pull_request.head.sha }}
```

Warnings are reported, not hidden. A non-zero warning count means the SVG pages
are not a guaranteed reproduction of the source document.

## GitHub Copilot

### In this repository

[`.github/copilot-instructions.md`](../.github/copilot-instructions.md) gives
Copilot Chat, Copilot in VS Code and the Copilot coding agent the build
commands, the conversion accuracy rules and the repository boundaries.
[`AGENTS.md`](../AGENTS.md) covers the same ground for Copilot CLI, which reads
that file instead.

[`.github/workflows/copilot-setup-steps.yml`](../.github/workflows/copilot-setup-steps.yml)
preinstalls the Rust, Node and Python toolchains in the coding agent's
environment, so a Copilot session starts with a warm build cache.

### In a repository that only consumes docsvg

Give the coding agent the CLI by adding the setup action to that repository's
`copilot-setup-steps.yml`:

```yaml
jobs:
  copilot-setup-steps:
    runs-on: ubuntu-latest
    steps:
      - uses: ryusui-hiro/document-svg/.github/actions/setup-docsvg@main
```

Copilot can then run `docsvg <input> --output <directory>` while it works.

## GitHub CLI

```bash
./scripts/install-gh-extension.sh
gh docsvg --help
gh docsvg convert report.pdf --output preview/report
gh docsvg reverse preview/report --output rebuilt.pptx
```

The installer makes the CLI available, then registers this checkout as a local
`gh` extension.

**`gh extension install ryusui-hiro/document-svg` does not work, and cannot be
made to work from this repository.** The GitHub CLI requires an extension's
repository name to begin with `gh-`, so a one-command remote install would need
a separate `gh-docsvg` repository. The local installer above is the supported
path. If remote installation matters more than keeping one repository, that
separate repository is the only thing on this page that requires one.

Because `gh docsvg` only wraps a local converter and never calls the GitHub API,
`cargo install document-svg --locked` or a release download gives the same
capability without the GitHub CLI.

## Codex and Claude Code

```bash
./scripts/install-codex-plugin.sh    # Codex plugin, then invoke $document-svg
./scripts/install-claude-plugin.sh   # Claude Code plugin
```

Both installers build `docsvg` from the checkout when cargo is available and
otherwise install a checksum-verified release build, so neither requires a Rust
toolchain.

Claude Code users can add the marketplace without cloning:

```
/plugin marketplace add ryusui-hiro/document-svg
/plugin install document-svg@document-svg
```

The plugin provides the `document-svg` skill plus five commands:

| Command | Purpose |
|---|---|
| `/document-svg:convert` | Convert documents, diagrams, CAD, 3D, mesh, charts, data to SVG pages |
| `/document-svg:reverse` | Package SVG pages into PPTX, DOCX, XLSX, draw.io, HTML viewer, or WebP |
| `/document-svg:transform` | Transform SVG pages (minify, format, scale, remove metadata) |
| `/document-svg:preview` | Render SVG pages to deterministic PNG previews |
| `/document-svg:setup` | Check the docsvg installation and show install options |

Codex loads the same skill through
[`.agents/plugins/marketplace.json`](../.agents/plugins/marketplace.json);
slash commands are a Claude Code feature.

### Finding the CLI

Every wrapper resolves `docsvg` through
`plugins/document-svg/skills/document-svg/scripts/ensure-docsvg.sh`, in this
order:

1. `DOCSVG_BIN`
2. `docsvg` on `PATH`
3. `~/.cargo/bin/docsvg`
4. the release cache (`~/.cache/document-svg/bin`, or `DOCSVG_CACHE_DIR`)
5. a `document-svg` checkout, through `cargo run`

When none of those exist the wrappers exit with code 69 and print the
installation options rather than installing anything. `ensure-docsvg.sh
--install` performs the verified release download on request.

## Why there is no MCP server

An MCP server would expose the same three operations that the skill already
covers, at the cost of keeping tool definitions in every agent's context on
every turn. The skill and the commands cost nothing until they are used. If a
client that cannot load skills needs `docsvg`, `gh docsvg` and the plain CLI
already serve it.
