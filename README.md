# melina

A fast, native process monitor for [Claude Code](https://docs.anthropic.com/en/docs/claude-code). Track sessions, agent teams, MCP servers, tmux swarms, and orphans — all from your terminal.

Built in Rust for minimal overhead — melina runs alongside your Claude Code sessions without impacting their performance.

## Why melina?

Claude Code can spawn complex process trees: multiple sessions, agent teams with dozens of teammates, MCP servers, tmux swarms, hooks, and bash tools. Without visibility, it's easy to end up with:

- **Zombie teams** — the parent session crashed but teammates keep running, consuming memory
- **Orphan tmux servers** — `claude-swarm` servers whose lead process died
- **Stale panes** — teammates that finished work but their tmux pane lingers
- **Stuck teammates** — agents with in-progress tasks that stopped making progress
- **Resource waste** — dozens of idle processes eating hundreds of MB each

melina gives you a single dashboard to see everything, understand what's healthy, and clean up what isn't.

## Features

### Process Discovery
- Scans the OS process table to find all Claude Code sessions, including symlink-resolved binaries (e.g. `claude` → `.local/share/claude/versions/2.1.75`)
- Builds parent-child trees with child classification: MCP servers, teammates, hooks, bash tools
- Detects Claude Code version, working directory, git context (branch, dirty state), and session IDs
- Identifies which user tmux session each Claude process is running inside

### Team & Swarm Monitoring
- Reads `.claude/` config directories to discover agent teams, members, and task counts
- Monitors `claude-swarm` tmux servers with per-pane agent details (name, team, status)
- Tracks teammate health: Active, Completed, Stale, Stuck, Zombie
- CPU-aware health detection — teammates waiting for LLM API responses (CPU > 0.5%) aren't falsely marked as stuck

### Health Checks & Zombie Detection
- **Zombie teams** — owner process is dead, team dir still exists
- **Orphan tmux servers** — `claude-swarm` server whose lead process died
- **Orphan shells** — tmux panes where the claude process exited, leaving an empty shell
- **Idle shells** — shells that have been running 8+ minutes after claude exited
- **Stale panes** — teammates whose team directory was deleted (`[DELETED]` label) or work finished but pane lingers

### Cleanup Tools
- **Manual cleanup** (`k` key / `--kill-zombies`) — kills all detected zombies immediately
- **Auto-cleanup** (`a` key / `--auto-cleanup`) — periodic cleanup on a configurable interval (default 15 min), only targets zombies with 30+ minutes uptime to avoid killing recently-started processes
- **Kill by PID** (`d` key / `--kill <PID>`) — safely terminate specific Claude processes with tmux pane awareness
- Safety checks: only kills claude-related PIDs, validates paths before directory removal

### Resource Tracking
- CPU%, memory, uptime, and start time for every process
- Per-session total memory aggregation
- Per-pane resource tracking in tmux swarms

## Install

Requires Rust 1.85+ (edition 2024).

```bash
git clone https://github.com/vinhnx/melina.git
cd melina
make install    # builds release + symlinks to /usr/local/bin
```

Or build manually:

```bash
cargo build --release
# Binaries at target/release/melina and target/release/melina-tui
```

## Usage

### CLI

```bash
melina                              # One-shot snapshot
melina --watch 2                    # Live refresh every 2 seconds
melina --watch 2 --auto-cleanup     # Auto-refresh + periodic zombie cleanup
melina --json                       # JSON output (pipe to jq, etc.)
melina --json --teams               # Include team details in JSON
melina --kill-zombies               # Clean up dead teams + orphan tmux servers
melina --kill 12345                 # Kill a specific Claude process by PID
melina --kill 12345 --kill 67890    # Kill multiple PIDs
```

### TUI Dashboard

```bash
melina-tui
```

Interactive dashboard with Solarized Dark color palette:

| Key | Action |
|-----|--------|
| `q` / `Esc` | Quit |
| `r` | Force refresh |
| `k` | Scan & kill zombies (with confirmation) |
| `d` | Kill process by PID (selection dialog) |
| `a` | Toggle auto-cleanup (periodic, 30+ min uptime only) |
| `s` | Settings popup (adjust refresh rate, intervals) |

#### Settings (s key)

The settings popup lets you adjust these values live with `←`/`→` keys:

| Setting | Default | Options |
|---------|---------|---------|
| Refresh rate | 2s | 1, 2, 3, 5, 10s |
| Status refresh | 10s | 5, 10, 15, 20, 30, 60s |
| Cleanup interval | 15min | 5, 10, 15, 30, 60min |
| Status display | 5s | 3, 5, 8, 10, 15s |

#### What you see

**Sessions table** — each Claude Code session with:
- PID, version, config directory
- Status (working/idle/waiting input) with color coding
- CPU, memory, uptime
- Git context (branch, dirty state)
- Child processes (MCP servers, teammates) in a tree view

**Tmux Servers table** — each `claude-swarm` server with:
- Socket name, lead PID, pane count
- Per-pane: agent name, team, status (ACTIVE/IDLE/DONE/SHELL), last output line
- `[DELETED]` label for panes whose team was cleaned up

## How melina helps manage Claude Code

### During development
- See all your Claude Code sessions at a glance — which are working, which are idle
- Monitor agent team progress: how many teammates are active, completed, or stuck
- Spot resource-heavy sessions before they slow down your machine

### After workflows complete
- Identify leftover processes from finished Rune arc/strive/appraise runs
- Clean up zombie teams whose parent session was closed
- Kill stale tmux panes that linger after teammates finish

### For long-running sessions
- Auto-cleanup runs periodically to remove zombies without manual intervention
- Only targets processes with 30+ min uptime — won't kill things that just started
- CPU-aware health checks avoid false positives from teammates waiting on API calls

## Project Structure

```
crates/
  melina-core/    Core library — process discovery, classification, health checks
  melina-cli/     CLI binary — snapshots, watch mode, JSON, kill commands
  melina-tui/     TUI binary — interactive ratatui dashboard
```

### Core modules

| Module | Purpose |
|--------|---------|
| `discovery.rs` | OS process table scan via `sysinfo`, symlink-aware session detection |
| `classify.rs` | Child process classification (MCP, teammate, hook, bash) |
| `tree.rs` | Parent-child session tree builder with host tmux detection |
| `teams.rs` | Agent team scanning from `.claude/` config dirs, tmux server/pane detection |
| `health.rs` | Health checks, zombie detection, auto-cleanup timer, stale pane detection |
| `status.rs` | Claude session status detection from tmux pane content |
| `git.rs` | Git context detection (branch, dirty, ahead/behind) |

## License

MIT
