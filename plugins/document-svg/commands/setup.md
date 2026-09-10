---
description: Check whether the docsvg CLI is available and show how to install it.
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/skills/document-svg/scripts/ensure-docsvg.sh:*)
---

Check the docsvg installation:

```bash
${CLAUDE_PLUGIN_ROOT}/skills/document-svg/scripts/ensure-docsvg.sh
```

If it prints a path, docsvg is ready; report the path and stop.

If it exits with code 69, docsvg is missing. Show the user the listed options
and ask which one they want:

- `ensure-docsvg.sh --install` downloads the release build for their platform
  from the project's GitHub releases and verifies it against the release
  `SHA256SUMS`. No Rust toolchain is needed.
- `cargo install document-svg --locked` builds it from crates.io.
- A manual download from <https://github.com/ryusui-hiro/document-svg/releases>.

Run an installation only after the user picks one. Do not install anything just
because this command was invoked.
