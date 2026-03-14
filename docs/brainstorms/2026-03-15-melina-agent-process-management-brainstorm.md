# Brainstorm: Melina as AI Agent Process Management Platform

**Date**: 2026-03-15
**Mode**: Roundtable (3 advisors: User Advocate, Tech Realist, Devil's Advocate)
**Rounds**: 3 (Understanding → Approaches → Refinement)

## What We're Building

Enable AI agents (Claude Code) to use Melina for process management: check status, inspect sessions, cleanup zombies, and safely kill swarm teams — all via a Claude Code skill wrapping the existing CLI.

## Key Decisions

### 1. NO Daemon
**Unanimous consensus**: Daemon is over-engineering. `melina-cli` already has `--json`, `--kill-zombies`, `--watch --auto-cleanup`. A daemon adds: PID file races, stale sockets, cross-platform complexity (launchd vs systemd), privilege escalation risk, and "monitor the monitor" problem. A skill + hook achieves 90% of the value at 10% complexity.

### 2. Skill-First Approach
- Claude Code skill named `/melina` with 3 subcommands: `status`, `cleanup`, `kill`
- Skill wraps `melina-cli` via Bash with `timeout 10` wrapper
- Graceful fallback when melina not installed

### 3. Rich Context for Agent Decisions
- New `--pane-lines N` flag (cap 50, default 5) adds tmux capture-pane content to JSON
- Health signals (is_zombie, stale_secs, cpu_percent, pane_status) emitted by core
- Confidence tiers (high/medium/low) computed by skill for kill recommendations

### 4. Safe Swarm Kill
- New `kill-swarm <team-name>` subcommand
- Kill sequence: resolve team → SIGTERM → 2s grace → SIGKILL survivors → kill tmux server → rm team config
- Self-kill guard: refuse if target is ancestor of current PID
- Fully idempotent (ESRCH = logged, not fatal)

### 5. Cleanup UX
- `cleanup` defaults to dry-run (shows what WOULD be killed)
- `--execute` flag to actually perform kills
- Confidence-tiered: high=auto, medium=log, low=ask human

## Advisor Perspectives

### User Advocate
- Identified 3 distinct user personas (human passive, agent active, agent passive)
- Proposed layered JSON output (summary → recommended → sessions)
- Designed confidence-tiered kill confirmation
- Advocated dry-run default for safety

### Tech Realist
- Proposed concrete CLI flags and kill sequence
- Identified resource costs: sysinfo 15-25MB RSS, 200ms CPU sleep per scan
- Recommended melina-core emits raw signals, skill owns policy
- Designed idempotent kill-swarm with ESRCH handling

### Devil's Advocate
- Killed the daemon idea (YAGNI)
- Warned about capture-pane secret exfiltration (API keys, tokens in terminal output)
- Proposed basic denylist sanitization (~30 lines Rust)
- Challenged scope creep from "cleanup zombies" to 3 features

## Chosen Approach
Ship all features together as 3 separate plans:
1. Rich Status (`--pane-lines N` + enhanced JSON)
2. Kill Swarm (safe swarm termination command)
3. Claude Code Skill (`/melina` with status/cleanup/kill)

## Key Constraints
- No daemon, no IPC, no brew service
- Raw capture-pane content (user accepts risk)
- All Bash calls wrapped with `timeout 10`
- Self-kill guard on kill operations
- melina-core owns signals, skill owns policy

## Non-Goals
- Daemon/service mode
- Signal-based IPC
- Sanitization of capture-pane output (user decision)
- Auto-kill without agent/human decision

## Open Questions
- Should basic secret denylist be added despite user choosing raw? (low cost, high value)
- Exact confidence score thresholds for kill tiers
