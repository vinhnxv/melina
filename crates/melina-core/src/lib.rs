//! melina-core — Claude Code process discovery and classification
//!
//! Scans the OS process table to find all Claude Code sessions,
//! builds parent-child trees, classifies children (MCP server,
//! teammate, hook, bash tool), and detects orphans.

mod classify;
mod discovery;
mod git;
mod health;
mod status;
mod teams;
mod tree;

pub use classify::{ChildKind, classify_child};
pub use discovery::{ProcessInfo, create_process_system, scan};
pub use git::GitContext;
pub use health::{
    Health, KillZombiesResult, ProcessLookup, ProcessLookupKind, TeamHealthReport, TeammateHealth,
    TeammateHealthEntry, ZombieEntry, check_health, check_team_health, kill_process, kill_zombies,
    lookup_process, scan_zombies,
};
pub use status::{ClaudeSessionStatus, detect_pane_status, detect_status};
pub use teams::{
    PaneStatus, TeamInfo, TeamMember, TmuxPane, TmuxServer, kill_tmux_server, resolve_tmux_pids,
    scan_teams, scan_tmux_servers,
};
pub use tree::{ChildProcess, HostTmux, SessionTree, build_trees};
