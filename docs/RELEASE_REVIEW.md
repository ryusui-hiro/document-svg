# Initial source-release review — 2026-09-08

Reviewed the Rust conversion entrypoints and format parsers, output writing, SVG serialization and reverse packaging, Node/Python bindings, preview helpers, plugin runners, authoring utilities, dependency metadata, and publication candidates. This is a targeted engineering review with regression checks, not an independent penetration test or proof that all defects are absent.

## Fixed before publication

- Forward conversion now rejects nonempty output directories. Forward and reverse outputs use random temporary files and non-overwriting publication, with automatic temporary-file cleanup on errors.
- SVG preview helpers reject relative, protocol-relative, encoded external and active nested SVG references. Reverse packaging rejects processing instructions, base URLs and CSS obfuscation.
- The PNG preview runner now validates XML, external references and nested SVG before rendering the exact inspected bytes. Output HTML escapes input names; PNG dimensions and render time are bounded.
- The HTML example refuses to overwrite existing files.
- Updated the yanked transitive `chacha20` dependency to 0.10.2.
- Excluded generated documents, caches and local QA outputs from Git. Restricted the Rust package to source and selected documentation; the verified package is approximately 231 KiB compressed.
- Unified root, language bindings and plugin license files under MIT OR Apache-2.0, with notice-retention instructions.

## Verification

- Rust: 25 unit, 78 forward integration and 8 reverse integration tests passed; formatting and workspace Clippy checks passed.
- Rust package: `cargo package --locked --allow-dirty` built and verified the extracted package.
- Node.js: fresh native release build and all 16 tests passed.
- Python: wheel build and all 4 binding tests passed; all 30 authoring/preview tests passed.
- Both plugin manifests validated. The PNG preview runner rendered a three-page input; the HTML example generated all three pages and rejected an existing output.
- Publication scan found no configured internal organization names, private absolute paths or supported credential patterns in publication candidates. The scan does not inspect files deliberately excluded from publication.
- RustSec: no known vulnerable dependencies reported at review time. One maintenance warning remains: `ttf-parser 0.25.1` is unmaintained (RUSTSEC-2026-0192). npm production dependency audit reported no vulnerabilities.

## Release boundaries

This is an alpha source release. Prebuilt cross-platform package publication is a separate step. Word pagination and some PDF/Office features remain approximate; see `SUPPORT.md`. The browser helper is not a universal SVG sanitizer. Applications accepting arbitrary uploads must impose process-level memory/time limits and display previews as images, not inline HTML.

Public repository source is readable and forkable. Maintainer review, protected history and read-only Actions tokens control changes to the upstream repository; they do not prevent lawful copying under the selected license.
