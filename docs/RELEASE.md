# Anemoi Release And Install Notes

Anemoi's first public beta release path is GitHub Releases with platform
archives for the operator binaries:

- `anemoi` - CLI/operator command
- `anemoi-daemon` - local control-plane daemon

The release workflow is `.github/workflows/release.yml`.

## Current Beta Install Path

Until a tagged GitHub Release exists, build from source:

```powershell
cargo build --release -p anemoi-cli -p anemoi-daemon
target\release\anemoi.exe --help
```

Start the daemon when ready, and stop it with Ctrl+C:

```powershell
target\release\anemoi-daemon.exe
```

On Linux/macOS:

```bash
cargo build --release -p anemoi-cli -p anemoi-daemon
./target/release/anemoi --help
```

Start the daemon when ready, and stop it with Ctrl+C:

```bash
./target/release/anemoi-daemon
```

## Cutting A Tagged Release

1. Confirm CI is green on `main`.
2. Choose a semver tag, for example `v0.1.0-beta.1`.
3. Create and push the tag:

```powershell
git tag v0.1.0-beta.1
git push origin v0.1.0-beta.1
```

4. The release workflow builds Linux, Windows, and macOS archives.
5. On tag pushes, the workflow creates a GitHub Release and uploads the archives.

## Release Validation Gates

The required CI workflow already runs:

```powershell
cargo fmt --check
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p anemoi-guard -- crates
```

The release workflow builds release-mode binaries. It does not yet publish a
Docker image or package-manager artifact.

## Unsupported Or Deferred Artifacts

- GHCR/Docker image publication is deferred until Docker/DNS readiness is fixed.
- `cargo install` publication is deferred until crate publishing is intentional.
- Frontend/dashboard artifacts are deferred until #130 lands.
