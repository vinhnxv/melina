# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
- Homebrew formula (`brew tap vinhnx/tap && brew install melina`)
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

[0.2.0]: https://github.com/vinhnxv/melina/releases/tag/v0.2.0
[0.1.0]: https://github.com/vinhnxv/melina/releases/tag/v0.1.0
