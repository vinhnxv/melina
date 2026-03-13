//! melina-core — Claude Code process discovery and classification
//!
//! Scans the OS process table to find all Claude Code sessions,
//! builds parent-child trees, classifies children (MCP server,
//! teammate, hook, bash tool), and detects orphans.

mod discovery;
mod classify;
mod git;
mod health;
mod status;
mod tree;
mod teams;

pub use discovery::{scan, create_process_system, refresh_process_system, ProcessInfo};
pub use classify::{ChildKind, classify_child};
pub use git::GitContext;
pub use health::{Health, check_health, TeammateHealth, TeammateHealthEntry, TeamHealthReport, check_team_health, ZombieEntry, scan_zombies, KillZombiesResult, kill_zombies, ProcessLookup, ProcessLookupKind, lookup_process, kill_process};
pub use status::{ClaudeSessionStatus, detect_status, detect_pane_status};
pub use tree::{SessionTree, ChildProcess, HostTmux, build_trees};
pub use teams::{TeamInfo, TeamMember, ConfigDirCache, TmuxSnapshot, scan_teams, scan_teams_cached, resolve_tmux_pids, TmuxServer, TmuxPane, PaneStatus, scan_tmux_servers, scan_tmux_servers_cached, scan_tmux_servers_with_snapshot, kill_tmux_server};
