# CLAUDE.md

Instructions for Claude Code in `anamnesis-rust`.

## Layout

- `crates/core`: the whole engine (config, store, indexer, watcher, MCP) as a library plus the headless `anamnesis-core` binary. All logic and tests live here.
- `ui`: React panel (`src/`), Tauri shell (`src-tauri/`). The shell only wires Tauri commands to `Engine`.

## Checks before a PR

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
pnpm --dir ui typecheck && pnpm --dir ui test && pnpm --dir ui build
```

Without the WebKitGTK dev libraries, use `-p anamnesis-core` instead of `--workspace`; CI compiles the shell.

## Git workflow

`main` takes PRs from `hotfix/<slug>` (default) or `release/<slug>` branches only; CI's `branch-gate` rejects anything else.

## Releases

1. `gh workflow run release.yml -f version=X.Y.Z`: bumps the Cargo workspace and `ui/package.json`, opens a `hotfix/release-vX.Y.Z` PR. Merge it.
2. Run it again with the same version: it tags `vX.Y.Z` and dispatches `publish.yml`, which builds installers for Windows, macOS and Linux plus headless binaries into a **draft** release.
3. Check the assets, write real release notes, then `gh release edit vX.Y.Z --draft=false --notes "..."`.

`build.yml` builds installers as workflow artifacts without releasing.
