# Dependency license audit — 2026-09-09

The audit covers the locked Rust workspace (core, Node and Python bindings, including
build/test and target-specific dependencies), npm's complete lockfile, the Python
verification environment and the exact maturin version recorded by its wheel.

| Scope | Result |
|---|---|
| 184 external Rust packages | `cargo-deny 0.20.2 --workspace --locked check licenses` passes the explicit permissive policy in `deny.toml` |
| 116 npm lock entries | All are development dependencies, including optional platform packages; declared licenses are MIT, Apache-2.0, ISC, 0BSD or Python-2.0 |
| 7 Python tools/test packages | Permissive declared licenses; maturin 1.15.0 uses MIT OR Apache-2.0 |
| Python runtime dependencies | No additional Python package dependencies; the native Rust dependencies are covered above |
| 8 standard-font metric tables | Compared every glyph advance against the pinned AFM source; original notices and permission retained |

The full package/version/license inventory and notice-source hashes are in
[`DEPENDENCY_LICENSES.json`](DEPENDENCY_LICENSES.json). Rust package metadata and
license files were checked; npm entries and Python tools were checked against their
declared license metadata. Development tools are not bundled into the runtime package.

CI additionally uses `actions/checkout` under MIT and the `cargo-deny 0.20.2`
audit executable under MIT OR Apache-2.0. These tools run in the build environment
and are not included in the library distributions.

## Conditions that need more than the project license

- `encoding_rs`: `(Apache-2.0 OR MIT) AND BSD-3-Clause`; the BSD notices must also remain.
- `jpeg-encoder`: `(MIT OR Apache-2.0) AND IJG`; the IJG acknowledgement and original notice are retained.
- `unicode-ident`: `(MIT OR Apache-2.0) AND Unicode-3.0`; the Unicode notice must also remain.
- `target-lexicon`: Apache-2.0 with LLVM exception; the exception text is retained.
- `r-efi`: MIT OR Apache-2.0 OR LGPL-2.1-or-later. The policy permits the MIT/Apache choices, not the LGPL choice. Its license text is contained in the upstream `AUTHORS` file.
- The published NAPI crates omit their repository-level LICENSE. That MIT text was recovered from each crate's exact `.cargo_vcs_info.json` revision, not from an unpinned branch.
- Adobe AFM-derived metrics require original copyright notices, the permission text and a modification notice. These are included in `licenses/Adobe-Core14-AFM.txt` and the bundled notices.

No mandatory GPL/AGPL/LGPL, noncommercial-only or source-disclosure condition was
identified for the selected dependency licenses. This conclusion applies to the
reviewed versions and selection policy; dependency updates require another check.

## Re-run and package

```sh
cargo install cargo-deny --version 0.20.2 --locked
cargo deny --workspace --locked check licenses
# Run in the verification venv after installing the locally built Python wheel:
python scripts/audit-licenses.py
python scripts/audit-licenses.py --check
```

The generator writes `THIRD_PARTY_LICENSES.txt` into the root and both binding
directories. Cargo, npm and wheel packaging include it. Texts are deduplicated
without removing copyright holders or changing their conditions. CI fails if the
committed Rust/Node inventory, metric provenance or notice bundles are stale.

External conversion/QA programs such as LibreOffice, Poppler and librsvg are not
linked or redistributed by this package; their own installation terms remain
separate from this dependency inventory.
