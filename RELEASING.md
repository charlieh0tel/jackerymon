# Releasing

A `vX.Y.Z` tag on a green `main` builds amd64 and arm64 `.deb`s and
publishes a GitHub Release (`.github/workflows/build-deb.yml`); a failed
`cargo audit` blocks it.

1. Bump `version` in both crates and the CLI's `jackerymon` dependency;
   `cargo update -w`.
2. Check: `make ci`, `make test-web` (once `make web-deps`),
   `cargo audit --ignore RUSTSEC-2023-0071` (see PLAN.md).
3. Merge through a PR; wait for CI.
4. Package tests are off for now (2026-10-16): `tools/test-package.sh`
   installs the CI-built `.deb` in Docker, and Docker here could not
   reach the Debian mirrors.  The script stays for when they are back.
5. `git tag -a vX.Y.Z -m "jackerymon X.Y.Z" && git push origin vX.Y.Z`;
   confirm both `.deb`s on the release.  The release triggers an
   [apt-repo](https://github.com/charlieh0tel/apt-repo) rebuild
   (`APT_REPO_TOKEN`); otherwise its daily run picks it up.

Never re-tag a release someone may have installed; fix forward with a
new patch version.  A broken release nobody consumed may be deleted
(`gh release delete vX.Y.Z --cleanup-tag`), as 0.1.0 was.

cargo-deb is pinned in the shared workflow (`cargo-deb-version`);
changing it can change maintainer scripts, so re-run step 4.
