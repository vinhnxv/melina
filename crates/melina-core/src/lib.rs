//! melina-core — Claude Code process discovery and classification
//!
//! Scans the OS process table to find all Claude Code sessions,
//! builds parent-child trees, classifies children (MCP server,
//! teammate, hook, bash tool), and detects orphans.

mod classify;
mod discovery;
pub mod format;
mod git;
mod health;
mod status;
mod teams;
mod tree;

pub use classify::{
    ChildKind, ConfigProcessType, classify_child, classify_child_simple, describe_child,
};
pub use discovery::{ProcessInfo, create_process_system, refresh_process_system, scan, scan_simple};
pub use format::{format_bytes, format_timestamp, format_uptime};
pub use git::GitContext;
pub use health::{
    AutoCleanup, Health, KillSwarmResult, KillZombiesResult, ProcessLookup, ProcessLookupKind,
    StalePaneReason, TeamHealthReport, TeammateHealth, TeammateHealthEntry, ZombieEntry,
    check_health, check_team_health, format_cleanup_result, is_ancestor_of_self, kill_process,
    kill_swarm, kill_zombies, kill_zombies_auto, kill_zombies_with, lookup_process, scan_zombies,
    scan_zombies_with,
};
pub use status::{ClaudeSessionStatus, detect_pane_status, detect_status};
pub use teams::{
    ConfigDirCache, PaneStatus, TeamInfo, TeamMember, TmuxPane, TmuxServer, TmuxSnapshot,
    discover_config_dirs, kill_tmux_server, resolve_tmux_pids, scan_teams, scan_teams_cached,
    scan_tmux_servers, scan_tmux_servers_cached, scan_tmux_servers_with_snapshot,
};
pub use tree::{ChildProcess, HostTmux, SessionTree, build_trees, build_trees_with_context};
