# Releasing WebSkills

This repository publishes from Rust metadata and `cargo-dist`.
There is no manual npm workspace or per-platform package publishing step anymore.

## Release Prerequisites

- GitHub Actions must be enabled for the repository.
- The repository must have an `NPM_TOKEN` secret with permission to publish the `webskills` package.
- Your local checkout should be on the branch you intend to release from, typically `main`.
- The release version in `Cargo.toml` must match the Git tag you push.

## Files That Control Releases

- `Cargo.toml`
- `dist-workspace.toml`
- `.github/workflows/release.yml`
- `.github/workflows/ci.yml`

## What Happens On Release

Pushing a semver tag like `v0.0.2` triggers `.github/workflows/release.yml`.

That workflow:

1. Plans the release with `cargo-dist`.
2. Builds platform binaries and archives.
3. Builds global installer artifacts, including the npm package tarball.
4. Uploads artifacts to a GitHub Release.
5. Publishes the generated npm package to npm.

## Release Steps

1. Verify the working tree is clean enough for release work.

```bash
git status --short
```

2. Update the version in `Cargo.toml`.

Example:

```toml
version = "0.0.2"
```

3. Run the local checks.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
~/.cargo/bin/dist generate --mode ci --check --allow-dirty
~/.cargo/bin/dist plan --allow-dirty
```

4. Commit the version bump.

```bash
git add Cargo.toml Cargo.lock
git commit -m "Release v0.0.2"
```

5. Create the release tag.

```bash
git tag v0.0.2
```

6. Push the branch and tag.

```bash
git push origin main
git push origin v0.0.2
```

## After Pushing The Tag

Watch the `Release` workflow in GitHub Actions.

Successful completion should produce:

- a GitHub Release for `v0.0.2`
- release archives for each configured target
- an npm publish of `webskills`

## Verifying The Published Release

Check the GitHub Release page for the tag and confirm the artifacts exist.

Verify npm install behavior:

```bash
npx webskills --help
```

You can also inspect the package version on npm:

```bash
npm view webskills version
```

## Troubleshooting

- If the workflow fails before publish, check `.github/workflows/release.yml`.
- If npm publish fails, confirm `NPM_TOKEN` exists and still has publish permission.
- If the tag version and `Cargo.toml` version do not match, `cargo-dist` may refuse to release.
- If you change targets or installer types, update `dist-workspace.toml` and rerun:

```bash
~/.cargo/bin/dist generate --mode ci --allow-dirty
```

## Installing cargo-dist Locally

If `dist` is not installed:

```bash
cargo install cargo-dist --locked --version 0.31.0
```
