# Third-party notices

Complete Rust dependency and font-metric copyright/license texts are included in
`THIRD_PARTY_LICENSES.txt`. Retain that file when redistributing this native package.
This software is based in part on the work of the Independent JPEG Group.

The Node.js package embeds the `document-svg` Rust core and its permissively
licensed dependencies. Direct binding dependencies:

| crate | version | license |
|---|---:|---|
| napi | 3.12.2 | MIT |
| napi-derive | 3.6.3 | MIT |
| napi-build | 2.4.1 | MIT |
| tempfile | 3.27.0 | MIT OR Apache-2.0 |

The complete dependency audit and license-selection policy is maintained in
the repository root `THIRD_PARTY_NOTICES.md`. Re-run that audit from the lock
file before publishing release packages.
