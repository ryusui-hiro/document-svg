# Contributing

Discuss substantial changes in an issue before implementation. Submit changes through a pull request; the repository owner reviews and merges them. Never include private documents, credentials, generated previews, or compiled artifacts.

By submitting a contribution, you agree to license your contribution under **MIT OR Apache-2.0**, without additional restrictions, and confirm that you have the right to submit it. Preserve existing copyright and license notices.

Run `./scripts/check-local.sh` before submitting a pull request. Use `./scripts/check-local.sh --all` to build WebAssembly and rebuild/test the Node.js and Python bindings too; this needs the `wasm32-unknown-unknown` target, matching `wasm-bindgen` CLI, `npm ci` in `bindings/node`, and a `bindings/python/.venv` containing maturin and pytest. The script follows the checks in `.github/workflows/publication-check.yml` and `.github/workflows/browser-wasm.yml`, and clears Cargo's stale `target/package` checkout before testing when needed. Bug reports should include a minimal synthetic input, expected behavior, actual behavior, and converter version. Follow `SECURITY.md` for private vulnerability reports.
