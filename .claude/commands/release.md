Release a new version of melina.

Usage: /melina:release <patch|minor|major> [description]
- First argument: version bump type (default: patch)
- Remaining arguments: short description of what changed (used in changelog)

## Steps

1. **Pre-flight checks**:
   - Run `cargo test` — abort if any test fails
   - Run `cargo clippy` — abort if any warnings
   - Ensure working tree is clean (no uncommitted changes besides what we're about to do)

2. **Determine new version**:
   - Read current version from `Cargo.toml` `[workspace.package] version`
   - Bump according to the argument: patch (0.2.0 → 0.2.1), minor (0.2.0 → 0.3.0), major (0.2.0 → 1.0.0)
   - Default to patch if no argument given

3. **Update version in `Cargo.toml`** (workspace-level `version` field)

4. **Update `CHANGELOG.md`**:
   - Add a new `## [X.Y.Z] - YYYY-MM-DD` section at the top (after the header)
   - Include the description from the argument, or summarize from recent git log if no description given
   - Add the release link at the bottom

5. **Build** (`cargo build --release`) to verify and update Cargo.lock

6. **Commit**: `chore: bump version to X.Y.Z, update changelog`
   - Stage: `Cargo.toml`, `CHANGELOG.md`, `crates/melina-core/src/discovery.rs` (if changed), and any other modified source files

7. **Push and tag**:
   - `git push origin main`
   - `git tag vX.Y.Z`
   - `git push origin vX.Y.Z`
   - This triggers the GitHub Actions release workflow which builds binaries and updates the Homebrew tap automatically

8. **Verify**: Run `gh run list -w release.yml -L 1` to confirm the release workflow was triggered

Report the new version and release URL when done.
