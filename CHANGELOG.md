# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.2] - 2026-03-16

### Fixed
- Confirm kill dialog now shows key hints (`y: confirm kill`, `n/any: cancel`) with dynamic height to prevent text clipping

### Removed
- Remove deprecated `classify_child_simple()` function (no callers remain)

## [0.4.1] - 2026-03-16

### Fixed
- **SEC-001**: Add strict socket suffix validation in `capture_pane_last_lines()` to match `kill_tmux_server()` validation
- **BUG-005**: Add `start_time > 0` check for idle shell detection, preventing false positives with uninitialized timestamps
- **BUG-008**: Add `tracing::warn!` logging for `.session` parse failures in `check_team_owner_alive()`, improving debuggability

### Changed
- **QUAL-006**: Add `tracing::debug!` logging for skipped teams in `read_team()`
- **QUAL-007**: Add `#[deprecated]` attribute to `classify_child_simple()` with migration guidance
- **QUAL-008**: Replace external `date` command with pure Rust `chrono` implementation in `format_timestamp()`

### Removed
- **DEAD-001**: Remove unused `sol::ORANGE` and `sol::BLUE` constants from TUI color palette
- **DEAD-002**: Remove unnecessary `#[allow(dead_code)]` from `sol` module (41+ active usages)

## [0.4.0] - 2026-03-16

### Added
- **Config dir process detection**: New `ConfigDirProcess` classification with subtypes (Plugin, Skill, ShellSnapshot, Hook, Script) for processes running from `~/.claude*` directories
- **Rune plugin detection**: Detect processes from the Rune plugin ecosystem (`plugins/cache/rune-marketplace/rune/*`, `plugins/rune/*`)
- **`describe_child()` function**: Extract meaningful descriptions for the INFO column instead of just showing process names like "bash" or "zsh"
- **Relative `.claude/` path detection**: Classify processes using project-level `.claude/` paths (skills, plugins, hooks, shell-snapshots, agents, scripts)
- **`ConfigProcessType::Script`**: New sub-type for plugin scripts (distinct from MCP servers)
- **`is_config_dir_process()`**: Config-dir-aware process detection for custom dirs like `.claude-true-yp`
- **`scan()` with config dirs**: Process scanning now accepts config directories for enhanced detection
- **`discover_config_dirs()` made public**: Available for cross-crate use

### Changed
- `classify_child()` now accepts `config_dirs` parameter for config-dir-aware classification
- TUI shows config dir processes with violet color and descriptive labels (e.g., `PLUGIN[.claude-true-yp]`, `SCRIPT[rune]`)
- INFO column shows what processes actually do (e.g., "echo-search", "lib/workflow-lock.sh") instead of just "bash"/"zsh"

## [0.3.3] - 2026-03-15

### Fixed
- **BACK-001**: Fix potential panic in string slice operation with length check
- **BACK-002**: Add TOCTOU mitigation with symlink_metadata verification before directory removal
- **BACK-003**: Add `validate_process_identity()` helper for PID reuse protection
- **BACK-005**: Add warn logging for mutex poisoning recovery
- **BACK-006**: Add HashSet cycle detection in parent chain traversal
- **BACK-007**: Add 60-second TTL to ConfigDirCache with should_refresh() method
- **DEAD-001**: Remove unused `terminal` field and `detect_terminal()` stub
- **PERF-003**: Optimize string allocations in process discovery hot path
- **SEC-001**: Use numeric signal values (15/SIGTERM, 9/SIGKILL) instead of string names

### Changed
- Added missing `tracing` dependency to melina-core Cargo.toml

## [0.3.2] - 2026-03-15

### Fixed
- Resolved audit findings for security and correctness
- Corrected remaining `vinhnx` → `vinhnxv` references

### Changed
- Removed unused homebrew directory

## [0.3.0] - 2026-03-15

### Added
- **`/melina` Claude Code skill** — comprehensive process management skill for Claude Code with commands: status, list, kill, cleanup, kill-swarm, watch
- **`kill-swarm` subcommand** — safely terminate an entire agent team (tmux server + teammates + config) with self-kill guard and `--force` override
- **`--pane-lines <N>` flag** — capture last N lines from tmux panes for richer status display in CLI and TUI
- **Rich pane status preservation** — TUI quick refresh preserves last_line and status from full refresh for smoother updates

### Fixed
- `format_timestamp` now works on both BSD (macOS) and GNU (Linux) systems

## [0.2.1] - 2026-03-14

### Fixed
- **TUI not discovering new Claude sessions** — `sysinfo::refresh_processes()` defaults to `ProcessRefreshKind` without `cmd`, so processes spawned after TUI launch had empty `cmd()` and were invisible to `is_claude_session()`. Fixed by using `refresh_processes_specifics()` with `.with_cmd(UpdateKind::OnlyIfNotSet)`.

## [0.2.0] - 2026-03-14

### Changed
- **`melina` is now the TUI dashboard** (previously `melina-tui`) — the interactive dashboard is the default experience
- **`melina-cli` is the CLI** (previously `melina`) — one-shot snapshots, watch mode, JSON output, kill commands
- Exclude Claude desktop app (Claude.app) from session detection
- Exclude claude-powerline and similar status-line tools from session detection
- Improved version detection via `proc_pidpath` and versioned binary paths

### Added
- MIT LICENSE file
- CI workflow (fmt check, clippy, tests, multi-OS build)
- GitHub Actions release workflow with Homebrew tap auto-update
- Homebrew formula (`brew tap vinhnxv/tap && brew install melina`)
- `curl | bash` installer script with OS/arch auto-detection
- `--version` flag for CLI
- `exe` field on ProcessInfo for binary path tracking

### Fixed
- All 48 clippy warnings resolved (collapsible if, unnecessary deref, missing Default impl, etc.)
- `.gitignore` typo (`claude/echoes` → `.claude/echoes`)

## [0.1.0] - 2026-03-14

### Added
- Interactive TUI dashboard with ratatui (q=quit, r=refresh, k=kill, s=settings, a=auto-cleanup)
- Solarized Dark color palette for TUI
- Settings popup (s key) for runtime configuration
- CPU-aware health checks with adaptive thresholds
- Auto-cleanup mode: periodic zombie/idle shell cleanup every 15 minutes
- Detect and clean up stale swarm teammate panes
- Kill dialog with tree indentation, debounce, and uptime display
- Zombie team cleanup and kill-by-PID dialogs in TUI
- Session status detection and git context
- Agent team scanning from `.claude/` config directories
- Tmux server detection and pane enumeration
- Orphan shell detection and cleanup
- JSON output mode (`--json`)
- Team info display (`--teams`)
- Watch mode with configurable refresh interval (`--watch <seconds>`)
- Process classification: MCP servers, teammates, hooks, bash children
- Parent-child session tree builder
- Health checks: zombie teams, stale/stuck teammates, orphan/idle shells
- Comprehensive unit tests across all crates

### Fixed
- Always check `team_exists` regardless of `skip_status`
- Flatten nested if-let chain in `check_team_owner_alive` (clippy)
- Show STARTED/UPTIME for shell panes in tmux server view
- Share `TmuxSnapshot` and `ConfigDirCache` across `build_trees` and `scan_tmux_servers`
- Detect `.claude-{name}` config directories correctly
- Prevent panics in TUI and tmux socket parsing
- CPU usage always showing 0% for tmux panes
- Address audit findings: orphan detection, performance, safety

[0.3.3]: https://github.com/vinhnxv/melina/releases/tag/v0.3.3
[0.4.2]: https://github.com/vinhnxv/melina/releases/tag/v0.4.2
[0.4.1]: https://github.com/vinhnxv/melina/releases/tag/v0.4.1
[0.4.0]: https://github.com/vinhnxv/melina/releases/tag/v0.4.0
[0.3.3]: https://github.com/vinhnxv/melina/releases/tag/v0.3.3
[0.3.2]: https://github.com/vinhnxv/melina/releases/tag/v0.3.2
[0.3.0]: https://github.com/vinhnxv/melina/releases/tag/v0.3.0
[0.2.1]: https://github.com/vinhnxv/melina/releases/tag/v0.2.1
[0.2.0]: https://github.com/vinhnxv/melina/releases/tag/v0.2.0
[0.1.0]: https://github.com/vinhnxv/melina/releases/tag/v0.1.0
