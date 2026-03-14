---
name: melina:release
description: |
  Release a new version of melina.

  Performs a complete release including:
  - Version bump in Cargo.toml
  - Changelog update with release notes
  - Git tag creation (vX.Y.Z)
  - GitHub Release with binaries for macOS (Intel + ARM) and Linux
  - Homebrew tap auto-update via GitHub Actions

  Usage: `/melina:release <patch|minor|major> [description]`
  - First argument: version bump type (default: patch)
  - Remaining arguments: short description of what changed (used in changelog)

  <example>
  user: "/melina:release minor add kill-swarm command"
  assistant: "Starting release v0.3.0..."
  </example>

  <example>
  user: "/melina:release patch fix timestamp bug"
  assistant: "Starting release v0.2.2..."
  </example>
user-invocable: true
allowed-tools:
  - Read
  - Write
  - Edit
  - Bash
  - Glob
  - Grep
  - AskUserQuestion
---

# Release Steps

## 1. Pre-flight checks

- Run `cargo test` — abort if any test fails
- Run `cargo clippy` — abort if any warnings
- Ensure working tree is clean (no uncommitted changes besides what we're about to do)

## 2. Determine new version

- Read current version from `Cargo.toml` `[workspace.package] version`
- Bump according to the argument:
  - `patch`: 0.2.0 → 0.2.1 (bug fixes)
  - `minor`: 0.2.0 → 0.3.0 (new features)
  - `major`: 0.2.0 → 1.0.0 (breaking changes)
- Default to patch if no argument given

## 3. Update version in `Cargo.toml`

Update the workspace-level `version` field.

## 4. Update `CHANGELOG.md`

- Add a new `## [X.Y.Z] - YYYY-MM-DD` section at the top (after the header)
- Include the description from the argument, or summarize from recent git log if no description given
- Add the release link at the bottom: `[X.Y.Z]: https://github.com/vinhnxv/melina/releases/tag/vX.Y.Z`

## 5. Build

Run `cargo build --release` to verify everything compiles.

## 6. Commit

```bash
git add Cargo.toml CHANGELOG.md
git commit -m "chore: bump version to X.Y.Z, update changelog"
```

## 7. Push and tag

```bash
git push origin main
git tag vX.Y.Z
git push origin vX.Y.Z
```

This triggers the GitHub Actions release workflow.

## 8. GitHub Actions automatically

- Builds binaries for `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`
- Creates GitHub Release with downloadable tarballs
- Updates `vinhnx/homebrew-tap` with new version

## 9. Verify

```bash
gh run list -w release.yml -L 1
```

## Troubleshooting

If the release workflow fails:
- Check `gh run view <run-id>` for error details
- Common issues:
  - `macos-13` not available → use `macos-latest`
  - Homebrew tap token expired → regenerate `HOMEBREW_TAP_TOKEN` secret

## Prerequisites

- `HOMEBREW_TAP_TOKEN` secret must be set in GitHub repo settings
- Token needs write access to `vinhnx/homebrew-tap` repository

Report the new version and release URL when done.