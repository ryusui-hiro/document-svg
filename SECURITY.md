# Security policy

Only the latest release receives security fixes.

Report vulnerabilities privately through [GitHub's vulnerability reporting](https://github.com/ryusui-hiro/document-svg/security/advisories/new) (the Security tab → Report a vulnerability). Do not post confidential documents, credentials, or exploit details in public issues.

Treat document input as untrusted. Use conversion limits and process isolation for public upload services. Conversion warnings require review; they do not certify security or visual fidelity.

Changes require maintainer review. Packages are published only by workflows a maintainer starts by hand from `main`, in protected environments, with short-lived OIDC credentials from npm, PyPI and crates.io; no long-lived registry token is stored in the repository. Repository-side protections are configured on GitHub; CODEOWNERS alone does not enforce them (see [docs/REPOSITORY_SECURITY.md](docs/REPOSITORY_SECURITY.md)).

## Dependency maintenance

The unmaintained font parser flagged by RUSTSEC-2026-0192 has been replaced with `skrifa 0.44.0`. The replacement shares the maintained Fontations stack already used by the SVG renderer. Run `cargo audit` against the current lockfile before releasing; historical audit results are not guarantees against undiscovered vulnerabilities.

The browser preview helpers perform conservative checks but are not a general-purpose SVG sanitizer. Display previews in an image element; never inject arbitrary SVG into the application DOM. Public document-upload services should isolate conversion in a restricted worker process with memory and time limits.
