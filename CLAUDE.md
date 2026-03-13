# Melina — Claude Code Process Monitor

## Project Overview

Rust workspace that scans the OS process table to discover and monitor all running Claude Code sessions, agent teams, tmux servers, and child processes.

## Architecture

```
crates/
  melina-core/   — Library: process discovery, classification, tree building, health checks, team scanning
  melina-cli/    — Binary: CLI with one-shot, watch, JSON, and kill modes
  melina-tui/    — Binary: interactive ratatui terminal dashboard
```

## Build & Run

```bash
cargo build --release        # Build all binaries
cargo run --bin melina        # Run CLI
cargo run --bin melina-tui    # Run TUI dashboard
make install                  # Symlink to /usr/local/bin
```

## Key Commands

```bash
melina                    # One-shot snapshot
melina --watch 2          # Auto-refresh every 2s
melina --json --teams     # JSON output with team info
melina --kill-zombies     # Clean up dead teams + orphan tmux servers
melina --kill <PID>       # Kill a Claude process by PID
melina --watch 2 --auto-cleanup  # Auto-refresh + periodic cleanup every 15 min
melina-tui                # Interactive dashboard (q=quit, r=refresh, a=auto-cleanup)
```

## Code Map

- `melina-core/src/discovery.rs` — OS process table scan via `sysinfo`
- `melina-core/src/classify.rs` — Child process classification (MCP, teammate, hook, bash)
- `melina-core/src/tree.rs` — Parent-child session tree builder
- `melina-core/src/teams.rs` — Agent team scanning from `.claude/` config dirs, tmux server detection
- `melina-core/src/health.rs` — Health checks: zombie teams, stale/stuck teammates, orphan/idle shell detection, auto-cleanup timer
- `melina-cli/src/main.rs` — CLI entry point with clap arg parsing
- `melina-tui/src/main.rs` — TUI dashboard with ratatui

## Conventions

- Rust edition 2024, workspace dependencies in root `Cargo.toml`
- Error handling via `anyhow::Result`
- Structured logging via `tracing`
- No unsafe code
- Safety checks before killing processes (only claude-related PIDs, path validation for dir removal)
