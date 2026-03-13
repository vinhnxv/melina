//! Tree building — assemble flat process list into session trees.

use crate::git::GitContext;
use crate::status::{ClaudeSessionStatus, detect_pane_status};
use crate::teams::{TeamInfo, resolve_tmux_pids, scan_teams};
use crate::{ChildKind, Health, ProcessInfo, check_health, classify_child};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use sysinfo::{Pid, System};

/// A child process within a session tree.
#[derive(Debug, Clone, Serialize)]
pub struct ChildProcess {
    pub info: ProcessInfo,
    pub kind: ChildKind,
    pub health: Health,
}

/// Info about the host tmux session that a Claude process is running inside of.
/// This is the user's own tmux (not claude-swarm).
#[derive(Debug, Clone, Serialize)]
pub struct HostTmux {
    /// Tmux session name (e.g. "main", "dev").
    pub session_name: String,
    /// Window index within the session.
    pub window_index: u32,
    /// Pane index within the window.
    pub pane_index: u32,
    /// Pane ID (e.g. "%0").
    pub pane_id: String,
    /// PID of the tmux server process.
    pub server_pid: u32,
}

impl std::fmt::Display for HostTmux {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}.{}",
            self.session_name, self.window_index, self.pane_index
        )
    }
}

/// A complete Claude Code session with its child processes.
#[derive(Debug, Clone, Serialize)]
pub struct SessionTree {
    pub root: ProcessInfo,
    pub root_health: Health,
    pub children: Vec<ChildProcess>,
    pub config_dir: Option<PathBuf>,
    pub terminal: Option<String>,
    pub total_memory_bytes: u64,
    /// Teams owned by this session (from filesystem config.json).
    pub teams: Vec<TeamInfo>,
    /// Session ID extracted from child commands (RUNE_SESSION_ID) or .session files.
    pub session_id: Option<String>,
    /// Working directory of the root Claude process.
    pub working_dir: Option<String>,
    /// Claude Code version (e.g. "1.0.33").
    pub claude_version: Option<String>,
    /// Host tmux session this Claude process is running inside (user's tmux, not claude-swarm).
    pub host_tmux: Option<HostTmux>,
    /// Claude Code session status detected from tmux pane content.
    pub claude_status: ClaudeSessionStatus,
    /// Git context for the working directory (branch, dirty state, etc.).
    pub git_context: Option<GitContext>,
}

impl SessionTree {
    /// Human-readable config dir label.
    pub fn config_label(&self) -> String {
        self.config_dir
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string())
    }

    /// Count MCP server children (from process tree).
    pub fn mcp_count(&self) -> usize {
        self.children
            .iter()
            .filter(|c| matches!(c.kind, ChildKind::McpServer { .. }))
            .count()
    }

    /// Count teammates from team config (not process tree).
    /// Only counts non-lead members.
    pub fn teammate_count(&self) -> usize {
        self.teams.iter().map(|t| t.teammates().len()).sum()
    }

    /// Get all team names for display.
    pub fn team_names(&self) -> Vec<String> {
        self.teams.iter().map(|t| t.name.clone()).collect()
    }
}

/// Detect Claude Code version by running the binary with --version.
/// Caches per binary path to avoid repeated subprocess calls.
fn detect_claude_version(root: &ProcessInfo) -> Option<String> {
    // Find the claude binary path from cmd
    let binary_path = root.cmd.first().and_then(|first| {
        if first.contains("claude") {
            Some(first.as_str())
        } else {
            // Node might be running claude — look for claude in args
            root.cmd
                .iter()
                .find(|arg| {
                    arg.contains("claude") && !arg.contains("server.py") && !arg.starts_with("--")
                })
                .map(|s| s.as_str())
        }
    })?;

    // Run `<binary> --version` and capture output
    let output = std::process::Command::new(binary_path)
        .arg("--version")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let version_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // Output is like "2.1.74 (Claude Code)" — extract the version number (first token)
    let version = version_str
        .split_whitespace()
        .next()
        .unwrap_or(&version_str)
        .to_string();

    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

/// A raw tmux pane entry from `tmux list-panes`.
struct TmuxPaneEntry {
    pane_pid: u32,
    session_name: String,
    window_index: u32,
    pane_index: u32,
    pane_id: String,
    server_pid: u32,
}

/// Query all user tmux panes (excluding claude-swarm sockets).
fn query_host_tmux_panes() -> Vec<TmuxPaneEntry> {
    use std::process::Command;

    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_pid}|#{session_name}|#{window_index}|#{pane_index}|#{pane_id}|#{pid}",
        ])
        .output();

    let stdout = match output {
        Ok(ref out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
        _ => return Vec::new(),
    };

    stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(6, '|').collect();
            if parts.len() != 6 {
                return None;
            }
            Some(TmuxPaneEntry {
                pane_pid: parts[0].parse().ok()?,
                session_name: parts[1].to_string(),
                window_index: parts[2].parse().ok()?,
                pane_index: parts[3].parse().ok()?,
                pane_id: parts[4].to_string(),
                server_pid: parts[5].parse().ok()?,
            })
        })
        .collect()
}

/// Detect if a Claude root process is running inside a user's tmux session.
/// Walks up the process parent chain from root.pid until we find a PID
/// that matches a tmux pane's shell PID.
fn detect_host_tmux(
    root: &ProcessInfo,
    tmux_panes: &[TmuxPaneEntry],
    sys: &System,
) -> Option<HostTmux> {
    if tmux_panes.is_empty() {
        return None;
    }

    // Walk up the parent chain: root.pid → root.ppid → grandparent → ...
    // The claude process itself or one of its ancestors should match a pane_pid.
    let mut current_pid = root.pid;
    for _ in 0..10 {
        // Check if current_pid matches any tmux pane
        if let Some(entry) = tmux_panes.iter().find(|e| e.pane_pid == current_pid) {
            // Skip claude-swarm panes (those are agent tmux, not user tmux)
            if entry.session_name.starts_with("claude-swarm") {
                return None;
            }
            return Some(HostTmux {
                session_name: entry.session_name.clone(),
                window_index: entry.window_index,
                pane_index: entry.pane_index,
                pane_id: entry.pane_id.clone(),
                server_pid: entry.server_pid,
            });
        }

        // Move to parent
        let parent_pid = sys
            .process(Pid::from_u32(current_pid))
            .and_then(|p| p.parent())
            .map(|p| p.as_u32());

        match parent_pid {
            Some(ppid) if ppid > 1 && ppid != current_pid => current_pid = ppid,
            _ => break,
        }
    }

    None
}

/// Build session trees from a flat list of processes.
/// Accepts a pre-created `System` to avoid redundant process table loads.
/// When `skip_status` is true, skips expensive capture-pane/jsonl status detection
/// and sets `claude_status` to `Unknown` (caller should merge from cache).
pub fn build_trees(
    processes: Vec<ProcessInfo>,
    sys: &System,
    skip_status: bool,
) -> Vec<SessionTree> {
    let by_pid: HashMap<u32, &ProcessInfo> = processes.iter().map(|p| (p.pid, p)).collect();

    // Query host tmux panes once for all sessions
    let tmux_panes = query_host_tmux_panes();

    // Find root sessions
    let roots: Vec<&ProcessInfo> = processes
        .iter()
        .filter(|p| {
            p.is_claude_session()
                && !by_pid
                    .get(&p.ppid)
                    .is_some_and(|parent| parent.is_claude_session())
        })
        .collect();

    // Read all teams once and resolve tmux PIDs
    let mut all_teams = scan_teams();
    resolve_tmux_pids(&mut all_teams, sys);

    let mut trees = Vec::new();

    for root in roots {
        let children: Vec<ChildProcess> = processes
            .iter()
            .filter(|p| p.ppid == root.pid && p.pid != root.pid)
            .map(|p| {
                let kind = classify_child(p);
                let is_mcp = matches!(kind, ChildKind::McpServer { .. });
                let health = check_health(p, is_mcp, &sys);
                ChildProcess {
                    info: p.clone(),
                    kind,
                    health,
                }
            })
            .collect();

        let total_memory =
            root.memory_bytes + children.iter().map(|c| c.info.memory_bytes).sum::<u64>();

        let config_dir = detect_config_dir(root, &children);
        let root_health = check_health(root, false, &sys);

        // Match teams by session ID found in child shell-snapshot commands,
        // or by config_dir match
        let (teams, session_id) = match_teams_to_session(root, &children, &config_dir, &all_teams);

        // Working directory from root process cwd
        let working_dir = if root.cwd.as_os_str().is_empty() {
            None
        } else {
            Some(root.cwd.to_string_lossy().to_string())
        };

        // Detect Claude Code version from the binary
        let claude_version = detect_claude_version(root);

        // Detect if running inside a user tmux session
        let host_tmux = detect_host_tmux(root, &tmux_panes, &sys);

        // Detect Claude session status: skip expensive capture-pane if requested
        let claude_status = if skip_status {
            // CPU-based heuristic only (cheap)
            let total_cpu: f32 =
                root.cpu_percent + children.iter().map(|c| c.info.cpu_percent).sum::<f32>();
            if total_cpu > 0.5 {
                ClaudeSessionStatus::Working
            } else {
                ClaudeSessionStatus::Unknown
            }
        } else if let Some(ref tmux) = host_tmux {
            detect_pane_status(&tmux.pane_id)
        } else {
            // No tmux pane — fallback to CPU-based heuristic
            let total_cpu: f32 =
                root.cpu_percent + children.iter().map(|c| c.info.cpu_percent).sum::<f32>();
            if total_cpu > 0.5 {
                ClaudeSessionStatus::Working
            } else {
                ClaudeSessionStatus::Idle
            }
        };

        // Detect git context: skip on quick refresh (rarely changes)
        let git_context = if skip_status || root.cwd.as_os_str().is_empty() {
            None
        } else {
            GitContext::detect(&root.cwd)
        };

        trees.push(SessionTree {
            root: root.clone(),
            root_health,
            children,
            config_dir,
            terminal: detect_terminal(root),
            total_memory_bytes: total_memory,
            teams,
            session_id,
            working_dir,
            claude_version,
            host_tmux,
            claude_status,
            git_context,
        });
    }

    trees.sort_by_key(|t| t.root.start_time);
    trees
}

/// Match teams to a session using 2 strategies (in priority order):
/// 1. RUNE_SESSION_ID from child shell-snapshot commands → team.lead_session_id
/// 2. owner_pid in .session file → root PID
///
/// Also returns the first discovered session ID (if any).
fn match_teams_to_session(
    root: &ProcessInfo,
    children: &[ChildProcess],
    _config_dir: &Option<PathBuf>,
    all_teams: &[TeamInfo],
) -> (Vec<TeamInfo>, Option<String>) {
    // Strategy 1: extract session ID from child shell-snapshot commands
    let mut session_ids = Vec::new();
    for child in children {
        let cmd = child.info.cmd.join(" ");
        // RUNE_SESSION_ID (from Rune plugin shell-snapshots)
        if let Some(pos) = cmd.find("RUNE_SESSION_ID=\"") {
            let after = &cmd[pos + 17..];
            if let Some(end) = after.find('"') {
                session_ids.push(after[..end].to_string());
            }
        }
        // CLAUDE_SESSION_ID (from Claude Code env)
        if let Some(pos) = cmd.find("CLAUDE_SESSION_ID=\"") {
            let after = &cmd[pos + 19..];
            if let Some(end) = after.find('"') {
                let sid = after[..end].to_string();
                if !session_ids.contains(&sid) {
                    session_ids.push(sid);
                }
            }
        }
    }

    let mut matched = Vec::new();
    let mut found_session_id: Option<String> = session_ids.first().cloned();

    for team in all_teams {
        // Match by session ID (most precise)
        if let Some(lead_sid) = &team.lead_session_id {
            if session_ids.contains(lead_sid) {
                matched.push(team.clone());
                continue;
            }
        }

        // Match by owner_pid in .session file → root PID
        let session_path = team
            .config_dir
            .join("teams")
            .join(&team.name)
            .join(".session");
        if let Ok(content) = std::fs::read_to_string(&session_path) {
            if let Ok(session) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(pid_str) = session.get("owner_pid").and_then(|v| v.as_str()) {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        if pid == root.pid {
                            // Also grab session_id from .session file if we don't have one yet
                            if found_session_id.is_none() {
                                found_session_id = session
                                    .get("session_id")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                            }
                            matched.push(team.clone());
                            continue;
                        }
                    }
                }
            }
        }
    }

    (matched, found_session_id)
}

/// Detect which CLAUDE_CONFIG_DIR this session uses by inspecting MCP server paths.
fn detect_config_dir(_root: &ProcessInfo, children: &[ChildProcess]) -> Option<PathBuf> {
    for child in children {
        let cmd = child.info.cmd.join(" ");
        if let Some(pos) = cmd.find("/.claude") {
            // Verify this is actually a .claude directory (not .claude-backup etc.)
            // Valid patterns: /.claude/, /.claude-{name}/, or /.claude at end
            let after = &cmd[pos + 8..]; // after "/.claude" (8 chars)
            let is_valid = after.is_empty()
                || after.starts_with('/')
                || (after.starts_with('-') && after[1..].contains('/'));
            if !is_valid {
                continue; // Skip false positives like .claude-backup without slash
            }
            // Find the start of the path (look backwards for space or start of string)
            let path_start = cmd[..pos].rfind(' ').map(|s| s + 1).unwrap_or(0);
            let full_path = &cmd[path_start..];
            if let Some(plugins_pos) = full_path.find("/plugins/") {
                return Some(PathBuf::from(&full_path[..plugins_pos]));
            }
        }
    }
    None
}

fn detect_terminal(_root: &ProcessInfo) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Helper to create a ProcessInfo for testing.
    fn make_process_info(pid: u32, ppid: u32, name: &str, cmd: Vec<&str>) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid,
            name: name.to_string(),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            cwd: PathBuf::new(),
            memory_bytes: 0,
            cpu_percent: 0.0,
            start_time: 0,
            status: "Run".to_string(),
        }
    }

    /// Helper to create a ChildProcess for testing.
    fn make_child_process(info: ProcessInfo, kind: ChildKind) -> ChildProcess {
        ChildProcess {
            info,
            kind,
            health: Health::Ok,
        }
    }

    /// Helper to create a SessionTree for testing.
    fn make_session_tree(
        root: ProcessInfo,
        children: Vec<ChildProcess>,
        teams: Vec<TeamInfo>,
    ) -> SessionTree {
        SessionTree {
            root,
            root_health: Health::Ok,
            children,
            config_dir: None,
            terminal: None,
            total_memory_bytes: 0,
            teams,
            session_id: None,
            working_dir: None,
            claude_version: None,
            host_tmux: None,
            claude_status: ClaudeSessionStatus::Unknown,
            git_context: None,
        }
    }

    /// Helper to create a TeamInfo for testing.
    fn make_team_info(name: &str, member_names: &[&str]) -> TeamInfo {
        TeamInfo {
            name: name.to_string(),
            config_dir: PathBuf::new(),
            lead_session_id: None,
            members: member_names
                .iter()
                .map(|n| crate::teams::TeamMember {
                    name: n.to_string(),
                    agent_type: String::new(),
                    model: String::new(),
                    backend_type: String::new(),
                    cwd: String::new(),
                    tmux_pane_id: String::new(),
                    tmux_pid: None,
                    memory_bytes: 0,
                    cpu_percent: 0.0,
                    start_time: 0,
                })
                .collect(),
            task_count: 0,
        }
    }

    // ── config_label() tests ─────────────────────────────────────────

    #[test]
    fn test_config_label_with_dir() {
        let root = make_process_info(1, 0, "claude", vec!["claude"]);
        let tree = SessionTree {
            config_dir: Some(PathBuf::from("/home/user/.claude-work")),
            ..make_session_tree(root, vec![], vec![])
        };
        assert_eq!(tree.config_label(), ".claude-work");
    }

    #[test]
    fn test_config_label_none() {
        let root = make_process_info(1, 0, "claude", vec!["claude"]);
        let tree = make_session_tree(root, vec![], vec![]);
        assert_eq!(tree.config_label(), "default");
    }

    // ── mcp_count() tests ────────────────────────────────────────────

    #[test]
    fn test_mcp_count_empty() {
        let root = make_process_info(1, 0, "claude", vec!["claude"]);
        let tree = make_session_tree(root, vec![], vec![]);
        assert_eq!(tree.mcp_count(), 0);
    }

    #[test]
    fn test_mcp_count_mixed() {
        let root = make_process_info(1, 0, "claude", vec!["claude"]);
        let mcp_child = make_child_process(
            make_process_info(2, 1, "node", vec!["node", "server.py"]),
            ChildKind::McpServer {
                server_name: "echo-search".to_string(),
            },
        );
        let bash_child = make_child_process(
            make_process_info(3, 1, "bash", vec!["bash"]),
            ChildKind::BashTool,
        );
        let another_mcp = make_child_process(
            make_process_info(4, 1, "node", vec!["node", "mcp-server"]),
            ChildKind::McpServer {
                server_name: "figma".to_string(),
            },
        );
        let tree = make_session_tree(root, vec![mcp_child, bash_child, another_mcp], vec![]);
        assert_eq!(tree.mcp_count(), 2);
    }

    // ── teammate_count() tests ───────────────────────────────────────

    #[test]
    fn test_teammate_count_empty() {
        let root = make_process_info(1, 0, "claude", vec!["claude"]);
        let tree = make_session_tree(root, vec![], vec![]);
        assert_eq!(tree.teammate_count(), 0);
    }

    #[test]
    fn test_teammate_count_with_teams() {
        let root = make_process_info(1, 0, "claude", vec!["claude"]);
        // Team with team-lead + 2 teammates (should count only 2)
        let team1 = make_team_info("team-alpha", &["team-lead", "researcher", "coder"]);
        // Team with team-lead + 1 teammate (should count only 1)
        let team2 = make_team_info("team-beta", &["team-lead", "tester"]);
        let tree = make_session_tree(root, vec![], vec![team1, team2]);
        assert_eq!(tree.teammate_count(), 3);
    }

    // ── team_names() tests ───────────────────────────────────────────

    #[test]
    fn test_team_names_empty() {
        let root = make_process_info(1, 0, "claude", vec!["claude"]);
        let tree = make_session_tree(root, vec![], vec![]);
        assert!(tree.team_names().is_empty());
    }

    #[test]
    fn test_team_names_multiple() {
        let root = make_process_info(1, 0, "claude", vec!["claude"]);
        let team1 = make_team_info("alpha-team", &["team-lead"]);
        let team2 = make_team_info("beta-team", &["team-lead"]);
        let tree = make_session_tree(root, vec![], vec![team1, team2]);
        assert_eq!(tree.team_names(), vec!["alpha-team", "beta-team"]);
    }

    // ── HostTmux::Display tests ──────────────────────────────────────

    #[test]
    fn test_host_tmux_display() {
        let host_tmux = HostTmux {
            session_name: "main".to_string(),
            window_index: 0,
            pane_index: 1,
            pane_id: "%0".to_string(),
            server_pid: 12345,
        };
        assert_eq!(format!("{}", host_tmux), "main:0.1");
    }
}
