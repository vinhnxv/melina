# melina

A fast, native process monitor for [Claude Code](https://docs.anthropic.com/en/docs/claude-code). Track sessions, agent teams, MCP servers, tmux swarms, and orphans — all from your terminal.

Built in Rust for minimal overhead — melina runs alongside your Claude Code sessions without impacting their performance.

## The Problem

Claude Code's multi-agent workflows (`/rune:strive`, `/rune:arc`, agent teams) spawn complex process trees — sessions, teammates, MCP servers, tmux swarms, hooks, and bash tools. These can easily grow to dozens of concurrent processes.

**The issue: Claude Code doesn't always clean up after itself.** When workflows finish, crash, or get interrupted, many processes are left behind:

- **Zombie teams** — the parent session crashed but teammates keep running, each consuming 200-500 MB of memory
- **Orphan tmux servers** — `claude-swarm` servers whose lead process died, holding open sockets and child processes
- **Stale panes** — teammates that finished work but their tmux pane lingers indefinitely
- **Stuck teammates** — agents with in-progress tasks that stopped making progress, burning CPU cycles
- **Cascading resource waste** — a single arc run can leave behind 10-20 orphan processes, eating gigabytes of RAM

Over a day of heavy Claude Code usage, this adds up. Your machine slows down, swap usage spikes, and you're left wondering why 8 GB of memory disappeared. The root cause is invisible — these orphan processes don't show up in Claude Code's own UI.

## Why melina?

melina gives you full visibility into every Claude Code process on your system. It builds a complete picture — sessions, teams, tmux swarms, child processes — and lets you clean up what isn't needed, either manually or automatically.

Think of it as Activity Monitor / htop, but purpose-built for Claude Code's process model.

## Recommended: Use `--teammate-mode tmux`

For best results with melina, launch Claude Code with tmux-based teammate mode:

```bash
claude --teammate-mode tmux
```

This makes every teammate a tmux pane, which gives melina (and you) much better visibility:

- **Each teammate is individually observable** — see its name, team, status, last output, CPU, and memory
- **Clean shutdown is possible** — melina can kill individual panes instead of entire process groups
- **Orphan detection works** — melina detects panes whose parent session died and marks them for cleanup
- **Shell state is visible** — melina distinguishes between active agents, completed agents, and empty shells

Without tmux mode, teammates run as background processes that are harder to inspect and manage.

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
- **Kill swarm** (`kill-swarm <team-name>`) — safely terminate an entire agent team (tmux server + teammates + config) with self-kill guard
- Safety checks: only kills claude-related PIDs, validates paths before directory removal

### Resource Tracking
- CPU%, memory, uptime, and start time for every process
- Per-session total memory aggregation
- Per-pane resource tracking in tmux swarms

## Install

### Quick install (macOS / Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/vinhnxv/melina/main/install.sh | bash
```

Tự động detect OS và architecture (macOS ARM/Intel, Linux x86_64), tải binary từ GitHub Releases và cài vào `/usr/local/bin`. Tuỳ chỉnh đường dẫn cài đặt:

```bash
INSTALL_DIR=~/.local/bin curl -fsSL https://raw.githubusercontent.com/vinhnxv/melina/main/install.sh | bash
```

### Homebrew (macOS / Linux)

```bash
brew tap vinhnxv/tap
brew install melina
```

This installs both `melina` (TUI dashboard) and `melina-cli`.

To upgrade:

```bash
brew upgrade melina
```

### Download binary

Pre-built binaries are available on the [Releases](https://github.com/vinhnxv/melina/releases) page for:

| Platform | Architecture | File |
|----------|-------------|------|
| macOS | Apple Silicon (M1/M2/M3/M4) | `melina-vX.Y.Z-aarch64-apple-darwin.tar.gz` |
| macOS | Intel | `melina-vX.Y.Z-x86_64-apple-darwin.tar.gz` |
| Linux | x86_64 | `melina-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` |

```bash
# Example: download and install on Apple Silicon
curl -L https://github.com/vinhnxv/melina/releases/latest/download/melina-v0.3.0-aarch64-apple-darwin.tar.gz | tar xz
sudo mv melina melina-cli /usr/local/bin/
```

### Cargo install

```bash
cargo install --git https://github.com/vinhnxv/melina.git melina-tui   # melina (TUI dashboard)
cargo install --git https://github.com/vinhnxv/melina.git melina-cli   # melina-cli
```

### From source

Requires Rust 1.85+ (edition 2024).

```bash
git clone https://github.com/vinhnxv/melina.git
cd melina
make install    # builds release + symlinks to /usr/local/bin
```

Or build manually:

```bash
cargo build --release
# Binaries at target/release/melina (TUI) and target/release/melina-cli
```

### Verify installation

```bash
melina              # opens TUI dashboard (q to quit)
melina-cli --version    # should print: melina-cli 0.3.0
```

### Uninstall

```bash
# Homebrew
brew uninstall melina && brew untap vinhnxv/tap

# Manual / make install
make uninstall    # removes symlinks from /usr/local/bin
```

## Usage

### TUI Dashboard (default)

```bash
melina
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

### CLI

```bash
melina-cli                              # One-shot snapshot
melina-cli --watch 2                    # Live refresh every 2 seconds
melina-cli --watch 2 --auto-cleanup     # Auto-refresh + periodic zombie cleanup
melina-cli --json                       # JSON output (pipe to jq, etc.)
melina-cli --json --teams               # Include team details in JSON
melina-cli --kill-zombies               # Clean up dead teams + orphan tmux servers
melina-cli --kill 12345                 # Kill a specific Claude process by PID
melina-cli --kill 12345 --kill 67890    # Kill multiple PIDs
melina-cli kill-swarm my-team           # Kill an entire agent team safely
melina-cli kill-swarm my-team --force   # Kill team even if it's your own session
melina-cli --pane-lines 3               # Show last 3 lines from each tmux pane
```

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

## How It Works

melina scans the OS process table (via `sysinfo`) every few seconds and builds a model of your Claude Code world:

1. **Discover** — find all processes whose binary resolves to a Claude Code installation
2. **Classify** — for each session, classify child processes as MCP servers, teammates, hooks, or bash tools
3. **Build trees** — link sessions to their parent tmux panes (if any) to reconstruct the full hierarchy
4. **Scan teams** — read `.claude/teams/` directories to discover agent teams, members, and task states
5. **Detect swarms** — find `claude-swarm-*` tmux servers and map each pane to its agent
6. **Health check** — cross-reference running processes against team directories to detect zombies, orphans, and stale panes
7. **Clean up** — safely terminate dead processes (with confirmation) and remove orphaned team directories

All of this runs in a single Rust binary with no external dependencies beyond the OS.

## How melina helps manage Claude Code

### During development
- See all your Claude Code sessions at a glance — which are working, which are idle
- Monitor agent team progress: how many teammates are active, completed, or stuck
- Spot resource-heavy sessions before they slow down your machine

### After workflows complete
- Identify leftover processes from finished arc/strive/appraise runs
- Clean up zombie teams whose parent session was closed
- Kill stale tmux panes that linger after teammates finish

### For long-running sessions
- Auto-cleanup runs periodically to remove zombies without manual intervention
- Only targets processes with 30+ min uptime — won't kill things that just started
- CPU-aware health checks avoid false positives from teammates waiting on API calls

### Real-world impact
- A typical heavy-usage day can accumulate 2-4 GB of wasted memory from orphan processes
- melina's auto-cleanup mode keeps this in check without any manual intervention
- One-shot `melina --kill-zombies` is useful as a quick cleanup after a long coding session

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

## Claude Code Skill

melina includes a built-in Claude Code skill for process management. Add the skill to your project:

```
.claude/skills/melina.md
```

Then use it directly in Claude Code:

```
/melina status              # Show all sessions, teams, and process health
/melina cleanup             # Preview zombie cleanup (dry-run by default)
/melina cleanup --execute   # Execute zombie cleanup
/melina kill <team-or-pid>  # Kill a specific swarm team or process
/melina watch               # Live monitoring
```

This gives Claude Code itself the ability to inspect and manage its own process ecosystem — useful for self-cleanup after long workflows.
