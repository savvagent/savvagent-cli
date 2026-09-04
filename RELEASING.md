# Releasing savvagent

This repository normally automates releases with
[release-plz](https://release-plz.dev/) (see `release-plz.toml` and
`.github/workflows/release-plz.yml`): on every push to `main`, it opens/updates
a "release PR" that bumps versions and updates `CHANGELOG.md`, and merging
that PR tags and publishes the release.

**That automation is currently non-functional** for this repo due to an
upstream bug in release-plz's `git_only` mode: it fails to package workspaces
with internal path+version dependencies when comparing against the local
tree (see [release-plz/release-plz#2595](https://github.com/release-plz/release-plz/issues/2595),
still open; fix tracked in [PR #2789](https://github.com/release-plz/release-plz/pull/2789)).
Until that lands in a release-plz release, **cut releases manually** using
the process below. Once #2789 ships, re-enable the automation by confirming
`release-plz.yml`'s `release-pr` job succeeds on a push to `main`, and drop
this manual process.

## Manual release process

1. **Bump the version.** In `Cargo.toml`, update `workspace.package.version`
   and every internal `workspace.dependencies` entry's `version` field to
   match (all internal crates share one version in this repo). Then run:

   ```sh
   cargo check --workspace
   ```

   to regenerate `Cargo.lock` with the new version.

2. **Update `CHANGELOG.md`.** Rename the `## [Unreleased]` section to
   `## X.Y.Z - YYYY-MM-DD` (today's date), and add a fresh empty
   `## [Unreleased]` section above it for future entries. Follow
   [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) categories
   (Added/Changed/Fixed/Removed) and this repo's SemVer convention
   (pre-1.0: MINOR = features/breaking changes, PATCH = fixes).

3. **Validate locally:**

   ```sh
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets
   cargo test --workspace
   ```

4. **Commit, open a PR, and merge to `main`** (issue → worktree → PR, per
   this repo's normal workflow). Get it reviewed like any other change.

5. **Tag the merge commit and push the tag:**

   ```sh
   git checkout main && git pull
   git tag -a vX.Y.Z -m "vX.Y.Z"
   git push origin vX.Y.Z
   ```

   Pushing the tag triggers `.github/workflows/release.yml` (cargo-dist),
   which builds binaries/installers for all supported platforms (macOS
   arm64, Linux x86_64/aarch64, Windows msvc) and publishes the GitHub
   Release with those assets.

6. **Linux `.deb`/`.rpm` packages attach automatically.** Once `release.yml`
   finishes, `.github/workflows/package-linux.yml` runs automatically via its
   `workflow_run` trigger and uploads `.deb`/`.rpm` packages to the same
   release. If it ever fails or needs to be re-run without rebuilding the
   whole release, dispatch it manually:

   ```sh
   gh workflow run "Package (deb/rpm)" -f tag=vX.Y.Z
   ```

7. **Verify the release:**

   ```sh
   gh release view vX.Y.Z
   ```

   Confirm all expected platform archives/installers plus `.deb`/`.rpm` are
   attached.
