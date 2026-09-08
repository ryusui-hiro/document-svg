# Publishing and releases

## Distribution channels

| Channel | Name | Purpose |
|---|---|---|
| npm | `document-svg` | Public Node.js / TypeScript binding |
| PyPI | `document-svg` | Python binding; import `document_svg` |
| crates.io | `document-svg` | Rust library and the `docsvg` CLI |
| GitHub Releases | `vMAJOR.MINOR.PATCH` | CLI archives, wheels, npm tarballs, sources and checksums |
| GitHub Packages | `@ryusui-hiro/document-svg` | Authenticated npm mirror |

The workflows are prepared independently of registry availability. A configured
publisher does not mean a version is already published. Confirm the version on
each registry before announcing it.

Python wheel targets exclude musllinux for this release. The musllinux build
adds a standalone `libgcc_s` shared library; the existing dependency notice
inventory does not cover its separate distribution requirements. Keep the
unexpected-native-library rejection in place. npm musl packages and static CLI
archives remain supported, and Python users on Alpine can build the sdist.

## Authentication setup

References: [npm trusted publishing](https://docs.npmjs.com/trusted-publishers/)
and [PyPI trusted publishers](https://docs.pypi.org/trusted-publishers/).

- PyPI: register `document-svg` with GitHub owner `ryusui-hiro`, repository
  `document-svg`, workflow `publish-pypi.yml`, environment `pypi`. A pending
  publisher supports the first release; later releases use the resulting
  project publisher. No long-lived PyPI token is needed.
- npm: bootstrap each new package through an authenticated maintainer publish,
  then configure its GitHub trusted publisher with the same owner/repository,
  workflow `publish-npm.yml`, environment `npm`. Configure the root package and
  every platform package listed in `scripts/release-targets.json`.
- GitHub Packages: `publish-github.yml` uses the job's short-lived
  `GITHUB_TOKEN` with `packages: write`, in environment `github-packages`.
  Review package visibility and repository access after the first publish.
- crates.io: use a maintainer's local Cargo authentication for the first
  release. Do not put credentials in the repository or release assets.

Create the named GitHub environments and restrict publishing to the protected
`main` branch. Configure required reviewers where available. Publishing jobs
also reject dispatches from other branches. Keep build and publish separate:
building a branch must not publish its packages.

## Release procedure

1. Update the Rust, Node, Python and plugin versions together, including lock
   files. Review licenses, changes, supported targets and installation docs.
2. Merge tested changes into `main`. Run **Build release artifacts** against the
   exact intended commit. All native and source jobs must succeed.
3. Download that run's artifacts into a new directory. On a clean checkout of
   the same commit, assemble the distribution using Python 3.11+ and npm:

   ```sh
   gh run download RUN_ID --dir dist/downloaded
   python3 scripts/release-artifacts.py assemble \
     --artifacts dist/downloaded --output dist/release \
     --commit FULL_COMMIT_SHA
   ```

4. Smoke-test installation of the assembled packages. Create the version tag
   at that exact commit and a draft GitHub Release. Upload the top-level files
   from `dist/release`, including `release-manifest.json` and `SHA256SUMS`.
   Do not upload staging directories. Review notes, assets and checksums, then
   publish the release. Do not move an existing published tag.
5. Dispatch **Publish PyPI**, **Publish npm**, and **Publish GitHub Packages**
   from `main`, providing the verified release tag. These jobs download release
   assets and check their checksums, version and tagged source commit before
   publishing. Complete npm's first-publish bootstrap before using its OIDC job.
6. Publish the matching Rust source with `cargo publish -p document-svg --locked`
   from the clean tagged checkout. Verify registry versions and test clean
   installations of npm, pip and Cargo packages.

Native npm dependencies are published before the root package. npm retries skip
only existing versions with identical integrity. PyPI skips existing files;
always verify registry hashes after a partial retry. A failed publication is
not a reason to replace release assets or overwrite a version: investigate,
then resume the same verified release or make a new patch release.

## Installing the GitHub Packages mirror

Most users should use `npm install document-svg` from the public npm registry.
GitHub Packages is a separate authenticated distribution, not a prerequisite.
Configure your npm scope without putting tokens into source control:

```sh
npm config set @ryusui-hiro:registry https://npm.pkg.github.com
npm login --scope=@ryusui-hiro --registry=https://npm.pkg.github.com
npm install @ryusui-hiro/document-svg
```

Use GitHub-supported package authentication with read access. Import
`@ryusui-hiro/document-svg` (and its `/preview-ui` subpath) when using this mirror.
Its native dependencies are also scoped. Never commit an authentication token
or paste one into issue reports.
