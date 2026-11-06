# Releasing

Both crates share one version from `[workspace.package].version`, release as one `vX.Y.Z` tag and
feed one root `CHANGELOG.md`. [release-plz](https://release-plz.dev/) drives it; `technogym-mcp`
is the facade crate (it depends on `technogym`, so it publishes last and the tag appears only once
both crates are on crates.io).

## Versioning

- **Major**: any breaking change in either crate.
- **Minor**: new capabilities.
- **Patch**: fixes only.

`semver_check = false`: breaking changes are reviewed by hand in the release PR.

## Release cycle

On every push to `main`, `ci.yml` runs:

1. `release-plz-release` (after test, clippy, fmt, docker-build). If the workspace version is
   ahead of crates.io it publishes both crates via Trusted Publishing, pushes the tag, creates the
   GitHub Release with the changelog, then dispatches `release.yml` (GitHub suppresses `push`
   triggers for tags pushed with `GITHUB_TOKEN`). `release_always = true`: a release missed by a
   red or cancelled run is retried on the next push.
2. `release-plz-pr`, after the release job finished (success or failure), opens or updates the
   release PR with the next version and the aggregated changelog.
3. Merging the release PR triggers the next cycle.

`release.yml` then builds cargo-dist archives for six targets (Linux glibc/musl and macOS,
x86_64 and aarch64), the shell installer and checksums, signs every artifact with cosign
(keyless), attaches SLSA provenance, builds the multi-arch container image via `docker.yml`
(signed, attested), and attaches everything to the existing release.

Windows targets are off: `ring`'s C build fails under cargo-xwin. Windows users
`cargo install technogym-mcp`.

## One-time setup

1. **`release` environment**: Settings → Environments → New → `release`. No approval gate needed.
2. **`CARGO_REGISTRY_TOKEN` secret** with the `publish-new` scope, only for the bootstrap.
3. **Bootstrap each crate**: Actions → *Publish crate (initial)* → `technogym`, then
   `technogym-mcp`. Trusted Publishing cannot create crates. The workflow also registers the
   Trusted Publishing configs (`ci.yml` + `release`, `publish-crate-manual.yml` + `release`)
   best-effort; verify at `https://crates.io/crates/<crate>/settings`.

Bootstrap in dependency order: `cargo publish` verifies the build against crates.io, so
`technogym-mcp` cannot be published until `technogym` is up at the same version.

From a workstation instead (clean checkout, `cargo login` done):

```sh
git worktree add /tmp/technogym-publish origin/main
cd /tmp/technogym-publish
cargo publish -p technogym
cargo publish -p technogym-mcp
```

## Adding a crate

The merge that adds a crate fails `release-plz-release` (the new crate is at a version its
siblings are not published at yet) until the release PR bumps the version; then the release stops
at the new crate with HTTP 403 (Trusted Publishing cannot create it). Bootstrap it with *Publish
crate (initial)*, then push to `main` again (re-run failed jobs works). Add it to
`release-plz.toml`'s `changelog_include` and the choices in `publish-crate-manual.yml`.
Dev-dependencies on workspace crates carry `path` only, never `version`.

## Emergency manual publish

Actions → *Publish crate (manual)* → pick the crate → Run. Uses Trusted Publishing.

## Verifying artifacts

```sh
cosign verify-blob \
  --bundle technogym-mcp-x86_64-unknown-linux-gnu.tar.xz.bundle \
  --certificate-identity-regexp 'https://github.com/StephanMeijer/technogym-rs/.+' \
  --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
  technogym-mcp-x86_64-unknown-linux-gnu.tar.xz

gh attestation verify oci://ghcr.io/stephanmeijer/technogym-mcp:X.Y.Z --owner StephanMeijer
```

## Dependencies

`dependencies_update = false`: release-plz does not bump dependencies; Renovate does, weekly,
grouped, with advisories and base-image digests unscheduled. `cargo deny check advisories` runs
on every PR and on weekday mornings; it gates nothing, it puts findings in front of someone.
Pinned tool versions live in two places each and move by hand: cargo-deny and cargo-machete in
`ci.yml`/`advisories.yml`, release-plz in `ci.yml` (its `pr_body` template is copied into
`release-plz.toml`), cargo-dist in `Cargo.toml` and `release.yml`. gh (version and sha256
together) lives only in `ci.yml`'s release-PR job.
