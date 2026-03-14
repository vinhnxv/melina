# /melina — Claude Code Process Management Skill

Inspect and manage Claude Code sessions, swarm teams, and zombie processes through Melina.

## Usage

```
/melina status              # Show all sessions, teams, and process health
/melina cleanup             # Preview zombie cleanup (dry-run by default)
/melina cleanup --execute   # Execute zombie cleanup
/melina kill <team-or-pid>  # Kill a specific swarm team or process
```

## When to Use Proactively

- After completing a long workflow (`/rune:arc`, `/rune:strive`, `/rune:audit`)
- When the system feels slow or unresponsive
- Before starting a new workflow (clean slate)
- When you see `TeammateIdle` notifications from dead teams
- When transitioning between major tasks in a session

## Prerequisites

This skill requires `melina-cli` to be installed and available in PATH.

**Install via Homebrew:**
```bash
brew install vinhnxv/tap/melina
```

**Install via Cargo:**
```bash
cargo install --git https://github.com/vinhnxv/melina
```

**Fallback (if not installed):**
```bash
ps aux | grep -E "claude|tmux" | grep -v grep
```

---

## Subcommand: `/melina status`

Read-only snapshot of all Claude Code sessions and swarm teams.

### Execution

```bash
if ! command -v melina-cli &>/dev/null; then
  echo "melina-cli not found. Install with: brew install vinhnxv/tap/melina"
  exit 1
fi
timeout 10 melina-cli --json --teams --pane-lines 5
```

### Output Interpretation

Parse the JSON output and present a summary:

```
## Summary
- Active sessions: {count}
- Swarm teams: {count} ({healthy} healthy, {zombie} zombie)
- Zombie processes: {count}
- Total memory: {total_mb} MB

## Recommendations
For each zombie/stale entry, classify by confidence:

### HIGH Confidence (Safe to kill)
- owner dead + uptime > 30min → Zombie team
- orphan tmux server (lead dead) → Orphan server
- idle shell > 8min, no agent name → Idle shell

### MEDIUM Confidence (Verify with user)
- stale teammate, cpu < 0.1%, uptime > 1h → May be waiting

### LOW Confidence (Ask before acting)
- stale teammate (no activity 5+ min) BUT cpu > 0.5% → May be waiting for LLM
- process with status_raw "working" → SKIP (actively working)
```

---

## Subcommand: `/melina cleanup`

Kill zombie processes. **Dry-run by default** — requires explicit `--execute` flag.

### Dry-run (Default)

```bash
timeout 10 melina-cli --json --teams
```

Then analyze the output for:
- Teams where `owner_alive: false`
- Orphan tmux servers where `lead_alive: false`
- Idle shells with no `agent_name`

Present to user:
```
## Would Kill
1. Team: rune-strive-old (owner dead, uptime 45min)
2. Tmux: claude-swarm-12345 (orphan, 3 panes)
3. Shell: pane %12 (idle 12min, no agent)

Proceed with cleanup? (yes/no)
```

### Execute

Only after user confirmation:
```bash
timeout 10 melina-cli --kill-zombies
```

### Cooldown

If cleanup was executed less than 5 minutes ago, suggest waiting:
```
Cleanup was run 2 minutes ago. Wait 3 more minutes before running again.
```

---

## Subcommand: `/melina kill <target>`

Targeted kill of a specific team or process.

### Target Types

| Target | Command |
|--------|---------|
| Team name (string) | `melina-cli kill-swarm <team>` |
| PID (number) | `melina-cli --kill <PID>` |

### Team Kill Flow

1. **Show team info first:**
   ```bash
   timeout 10 melina-cli --json --teams | jq '.teams[] | select(.name == "<TEAM>")'
   ```

2. **Ask confirmation:**
   ```
   Kill team "rune-strive-20260315"?
   - 3 members
   - Lead PID: 12345
   - tmux panes: %0, %1, %2

   This will send SIGTERM, wait 2s, then SIGKILL survivors.
   Proceed? (yes/no)
   ```

3. **Execute:**
   ```bash
   timeout 30 melina-cli kill-swarm "<TEAM>" --json
   ```

### PID Kill Flow

1. **Show process info:**
   ```bash
   timeout 10 melina-cli --json | jq # find process
   ```

2. **Ask confirmation**

3. **Execute:**
   ```bash
   timeout 10 melina-cli --kill <PID>
   ```

### Self-Kill Guard

Melina prevents killing your own session. If attempted:
```
Error: Refusing to kill own session (lead_pid=12345 is ancestor).
Use --force to override.
```

---

## Error Handling

| Error | Response |
|-------|----------|
| `melina-cli not found` | Return install instructions + `ps` fallback |
| `timeout (>10s)` | Return error + suggest running `melina-cli` directly |
| `JSON parse failure` | Return raw output + note parsing error |
| `kill-swarm self-kill` | Return error message, explain to user |
| `No zombies found` | Return "All clean — no zombie processes detected" |

---

## Session-End Hook (Optional)

For automatic cleanup when Claude Code sessions end, add this to `~/.claude/hooks/session-end.sh`:

```bash
#!/usr/bin/env bash
# Auto-cleanup zombie processes when Claude Code session ends
if command -v melina-cli &>/dev/null; then
  melina-cli --kill-zombies 2>/dev/null || true
fi
```

**Note**: This only kills dead-owner zombies, not live teams. Safe by design.

---

## Examples

### Check system status
```
User: /melina status

Assistant: Let me check the current process status...

[Runs: melina-cli --json --teams --pane-lines 5]

## Summary
- Active sessions: 1
- Swarm teams: 2 (1 healthy, 1 zombie)
- Zombie processes: 3
- Total memory: 1.2 GB

## Recommendations
1. **HIGH**: Team "rune-strive-old" — owner dead, uptime 47min
   → Safe to cleanup with `/melina cleanup --execute`
2. **MEDIUM**: Teammate "worker-2" — stale for 8min, cpu 0%
   → May be waiting for LLM response, verify before acting
```

### Preview cleanup
```
User: /melina cleanup

Assistant: Analyzing zombie processes...

## Would Kill
1. Team: rune-strive-old (owner dead, 3 members)
2. Orphan tmux: claude-swarm-98765 (lead dead)
3. Idle shell: pane %7 (no agent, idle 15min)

Total: 3 targets

Proceed with cleanup? (yes/no)
```

### Kill specific team
```
User: /melina kill rune-strive-old

Assistant: Let me show you the team details first...

Team: rune-strive-old
- Lead PID: 45678 (dead)
- Members: 3
- tmux panes: %3, %4, %5

This will:
1. Send SIGTERM to all processes
2. Wait 2 seconds
3. SIGKILL any survivors
4. Kill the tmux server
5. Remove team config directory

Proceed? (yes/no)
```
