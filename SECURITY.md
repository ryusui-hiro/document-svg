# Security policy

Only the latest release receives security fixes. Before the first release, the default branch is the supported development version.

Report vulnerabilities through GitHub's private vulnerability reporting on this repository once enabled. Do not post confidential documents, credentials, or exploit details in public issues. If the private reporting option is unavailable, wait until the maintainer enables it before submitting sensitive details.

Treat document input as untrusted. Use conversion limits and process isolation for public upload services. Conversion warnings require review; they do not certify security or visual fidelity.

Changes require maintainer review. No workflow in this repository publishes packages automatically or receives publishing credentials. Repository-side protections must be activated after repository creation; CODEOWNERS alone does not enforce them.

## Dependency maintenance

The 2026-09-08 RustSec audit reported no known vulnerable dependencies, but reported `ttf-parser 0.25.1` as unmaintained (RUSTSEC-2026-0192). The current version is retained for font-outline compatibility while a replacement is evaluated. This maintenance warning is not a security certification or a guarantee against undiscovered vulnerabilities.

The browser preview helpers perform conservative checks but are not a general-purpose SVG sanitizer. Display previews in an image element; never inject arbitrary SVG into the application DOM. Public document-upload services should isolate conversion in a restricted worker process with memory and time limits.
