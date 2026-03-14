# Melina — Claude Code Process Monitor

## Project Overview

Rust workspace that scans the OS process table to discover and monitor all running Claude Code sessions, agent teams, tmux servers, and child processes.

## Architecture

```
crates/
  melina-core/   — Library: process discovery, classification, tree building, health checks, team scanning
  melina-cli/    — Binary (melina-cli): CLI with one-shot, watch, JSON, and kill modes
  melina-tui/    — Binary (melina): interactive ratatui terminal dashboard (default)
```

## Build & Run

```bash
cargo build --release            # Build all binaries
cargo run --bin melina            # Run TUI dashboard (default)
cargo run --bin melina-cli        # Run CLI
make install                      # Symlink to /usr/local/bin
```

## Key Commands

```bash
melina                    # Interactive TUI dashboard (q=quit, r=refresh, a=auto-cleanup)
melina-cli                # One-shot snapshot
melina-cli --watch 2      # Auto-refresh every 2s
melina-cli --json --teams # JSON output with team info
melina-cli --kill-zombies # Clean up dead teams + orphan tmux servers
melina-cli --kill <PID>   # Kill a Claude process by PID
melina-cli --watch 2 --auto-cleanup  # Auto-refresh + periodic cleanup every 15 min
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
