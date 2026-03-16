//! Team discovery — read Agent Team config from filesystem.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use sysinfo::{Pid, System};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamMember {
    pub name: String,
    #[serde(default)]
    pub agent_type: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub backend_type: String,
    #[serde(default)]
    pub cwd: String,
    /// tmux pane ID from config.json (e.g., "%0").
    #[serde(default)]
    pub tmux_pane_id: String,
    /// Resolved OS process ID from tmux pane (populated at scan time, not from JSON).
    #[serde(skip_deserializing, default)]
    pub tmux_pid: Option<u32>,
    /// Memory usage in bytes (resolved from tmux PID).
    #[serde(skip_deserializing, default)]
    pub memory_bytes: u64,
    /// CPU usage percent (resolved from tmux PID).
    #[serde(skip_deserializing, default)]
    pub cpu_percent: f32,
    /// Process start time as epoch seconds (resolved from tmux PID).
    #[serde(skip_deserializing, default)]
    pub start_time: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamInfo {
    pub name: String,
    pub config_dir: PathBuf,
    /// The session ID of the team lead (maps to a claude process).
    pub lead_session_id: Option<String>,
    pub members: Vec<TeamMember>,
    pub task_count: usize,
}

impl TeamInfo {
    /// Non-lead members (actual teammates, not the lead itself).
    pub fn teammates(&self) -> Vec<&TeamMember> {
        self.members
            .iter()
            .filter(|m| m.name != "team-lead")
            .collect()
    }
}

/// Scan all CLAUDE_CONFIG_DIRs for active teams.
/// Uses a `ConfigDirCache` to avoid redundant filesystem reads.
pub fn scan_teams_cached(cache: &ConfigDirCache) -> Vec<TeamInfo> {
    let mut teams = Vec::new();

    for config_dir in cache.dirs() {
        let teams_dir = config_dir.join("teams");
        if teams_dir.is_dir()
            && let Ok(entries) = std::fs::read_dir(&teams_dir)
        {
            for entry in entries.flatten() {
                if entry.path().is_dir()
                    && let Some(team) = read_team(&entry.path(), config_dir)
                {
                    teams.push(team);
                }
            }
        }
    }

    teams
}

/// Scan all CLAUDE_CONFIG_DIRs for active teams (uncached, creates its own cache).
pub fn scan_teams() -> Vec<TeamInfo> {
    let mut teams = Vec::new();
    let config_dirs = discover_config_dirs();

    for config_dir in config_dirs {
        let teams_dir = config_dir.join("teams");
        if teams_dir.is_dir()
            && let Ok(entries) = std::fs::read_dir(&teams_dir)
        {
            for entry in entries.flatten() {
                if entry.path().is_dir()
                    && let Some(team) = read_team(&entry.path(), &config_dir)
                {
                    teams.push(team);
                }
            }
        }
    }

    teams
}

fn read_team(team_dir: &Path, config_dir: &Path) -> Option<TeamInfo> {
    let config_path = team_dir.join("config.json");
    let config_str = match std::fs::read_to_string(&config_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(
                "Skipping team {:?}: config.json not readable: {}",
                team_dir,
                e
            );
            return None;
        }
    };
    let config: serde_json::Value = match serde_json::from_str(&config_str) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("Skipping team {:?}: config.json malformed: {}", team_dir, e);
            return None;
        }
    };

    let name = team_dir.file_name()?.to_string_lossy().to_string();

    let lead_session_id = config
        .get("leadSessionId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut members: Vec<TeamMember> = config
        .get("members")
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();

    // Scan tasks for count + discover in-process teammates from task owners
    let tasks_dir = config_dir.join("tasks").join(&name);
    let mut task_count = 0;
    let mut task_owners: Vec<String> = Vec::new();

    if tasks_dir.is_dir()
        && let Ok(entries) = std::fs::read_dir(&tasks_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json")
                && path.file_name().is_some_and(|n| n != ".lock")
            {
                task_count += 1;
                // Read task owner to discover in-process teammates
                if let Ok(content) = std::fs::read_to_string(&path)
                    && let Ok(task) = serde_json::from_str::<serde_json::Value>(&content)
                    && let Some(owner) = task.get("owner").and_then(|v| v.as_str())
                    && !owner.is_empty()
                    && owner != "-"
                    && !task_owners.contains(&owner.to_string())
                {
                    task_owners.push(owner.to_string());
                }
            }
        }
    }

    // Add in-process teammates discovered from task owners but missing from config.json
    // Use fuzzy match: skip if owner is a prefix/substring of any known member name
    // (e.g. task owner "codex-phase-handler" matches member "codex-phase-handler-sv")
    let known_names: Vec<String> = members.iter().map(|m| m.name.clone()).collect();
    for owner in &task_owners {
        let already_known = known_names
            .iter()
            .any(|n| n == owner || n.starts_with(owner) || owner.starts_with(n));
        if !already_known {
            members.push(TeamMember {
                name: owner.clone(),
                agent_type: "in-process".to_string(),
                model: String::new(),
                backend_type: "in-process".to_string(),
                cwd: String::new(),
                tmux_pane_id: String::new(),
                tmux_pid: None,
                memory_bytes: 0,
                cpu_percent: 0.0,
                start_time: 0,
            });
        }
    }

    Some(TeamInfo {
        name,
        config_dir: config_dir.to_path_buf(),
        lead_session_id,
        members,
        task_count,
    })
}

/// Resolve tmux pane IDs to OS PIDs for all team members.
///
/// Claude Code uses custom tmux sockets named `claude-swarm-{lead_pid}`.
/// Uses a `TmuxSnapshot` to share ps/pane data with `scan_tmux_servers`.
/// Accepts a pre-created `System` to avoid redundant process table loads.
pub fn resolve_tmux_pids(teams: &mut [TeamInfo], sys: &System, snapshot: &TmuxSnapshot) {
    let pane_map = snapshot.pane_map();
    if pane_map.is_empty() {
        return;
    }

    // Collect all PIDs we need to look up
    let mut pids_to_lookup: Vec<u32> = Vec::new();
    for team in teams.iter_mut() {
        for member in &mut team.members {
            if !member.tmux_pane_id.is_empty() {
                for (_socket, panes) in &pane_map {
                    if let Some(&pid) = panes.get(&member.tmux_pane_id) {
                        member.tmux_pid = Some(pid);
                        pids_to_lookup.push(pid);
                        break;
                    }
                }
            }
        }
    }

    if pids_to_lookup.is_empty() {
        return;
    }

    // Populate resource info from sysinfo.
    // tmux pane_pid is typically a shell (zsh/bash). The actual Claude process
    // is a child of that shell. We need to find the claude child and use ITS stats.
    for team in teams.iter_mut() {
        for member in &mut team.members {
            if let Some(pane_pid) = member.tmux_pid {
                // Look for a claude child process under the pane shell.
                // The process name may be a version number (e.g., "2.1.72") since
                // the binary path is ~/.local/share/claude/versions/2.1.72.
                // So we check both process name AND command args for "claude".
                let claude_proc = sys.processes().values().find(|p| {
                    p.parent() == Some(Pid::from_u32(pane_pid)) && {
                        let name = p.name().to_string_lossy();
                        let cmd_str = p
                            .cmd()
                            .iter()
                            .map(|s| s.to_string_lossy())
                            .collect::<Vec<_>>()
                            .join(" ");
                        // The process name may be a version number (e.g., "2.1.75") on macOS due to symlink resolution.
                        // Rely on cmd_str patterns which are more reliable.
                        name.contains("claude")
                            || cmd_str.contains("claude")
                            || cmd_str.contains("--agent-id")
                    }
                });

                if let Some(proc_) = claude_proc {
                    // Use the actual claude process PID and stats
                    member.tmux_pid = Some(proc_.pid().as_u32());
                    member.memory_bytes = proc_.memory();
                    member.cpu_percent = proc_.cpu_usage();
                    member.start_time = proc_.start_time();
                } else if let Some(proc_) = sys.process(Pid::from_u32(pane_pid)) {
                    // Fallback to pane shell process if no claude child found
                    member.memory_bytes = proc_.memory();
                    member.cpu_percent = proc_.cpu_usage();
                    member.start_time = proc_.start_time();
                }
            }
        }
    }
}

/// Cached tmux snapshot: socket names (with server PIDs) and pane maps.
/// Built from a single `ps` call, shared between `resolve_tmux_pids` and `scan_tmux_servers`.
pub struct TmuxSnapshot {
    /// (socket_name, server_pid, pane_map)
    sockets: Vec<TmuxSocketInfo>,
}

struct TmuxSocketInfo {
    socket_name: String,
    server_pid: Option<u32>,
    panes: std::collections::HashMap<String, u32>,
}

impl Default for TmuxSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

impl TmuxSnapshot {
    /// Build snapshot from a single `ps -eo pid,args` call + per-socket `tmux list-panes`.
    pub fn new() -> Self {
        use std::collections::HashMap;
        use std::process::Command;

        let ps_output = Command::new("ps").args(["-eo", "pid,args"]).output();

        let ps_str = match ps_output {
            Ok(ref out) => String::from_utf8_lossy(&out.stdout).to_string(),
            Err(_) => {
                return Self {
                    sockets: Vec::new(),
                };
            }
        };

        let mut seen: Vec<String> = Vec::new();
        let mut sockets = Vec::new();

        for line in ps_str.lines() {
            let line = line.trim();
            if !line.contains("claude-swarm-") || !line.contains("tmux") {
                continue;
            }

            let server_pid: Option<u32> =
                line.split_whitespace().next().and_then(|s| s.parse().ok());

            if let Some(pos) = line.find("claude-swarm-") {
                let after = &line[pos..];
                let socket_name: String =
                    after.chars().take_while(|c| !c.is_whitespace()).collect();

                let suffix = &socket_name["claude-swarm-".len()..];
                if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                if seen.contains(&socket_name) {
                    continue;
                }
                seen.push(socket_name.clone());

                // Query pane→pid mapping for this socket
                let output = Command::new("tmux")
                    .args([
                        "-L",
                        &socket_name,
                        "list-panes",
                        "-a",
                        "-F",
                        "#{pane_id}|#{pane_pid}",
                    ])
                    .output();

                let mut panes = HashMap::new();
                if let Ok(out) = output {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    for pane_line in stdout.lines() {
                        let parts: Vec<&str> = pane_line.splitn(2, '|').collect();
                        if parts.len() == 2
                            && let Ok(pid) = parts[1].parse::<u32>()
                        {
                            panes.insert(parts[0].to_string(), pid);
                        }
                    }
                }

                sockets.push(TmuxSocketInfo {
                    socket_name,
                    server_pid,
                    panes,
                });
            }
        }

        Self { sockets }
    }

    /// Get pane map for resolve_tmux_pids (socket → pane_id → pid).
    fn pane_map(&self) -> Vec<(&str, &std::collections::HashMap<String, u32>)> {
        self.sockets
            .iter()
            .filter(|s| !s.panes.is_empty())
            .map(|s| (s.socket_name.as_str(), &s.panes))
            .collect()
    }
}

// ── Tmux Server Discovery ────────────────────────────────────

/// A Claude Code tmux server process (`tmux -L claude-swarm-{lead_pid}`).
#[derive(Debug, Clone, Serialize)]
pub struct TmuxServer {
    /// The socket name, e.g. "claude-swarm-63585".
    pub socket_name: String,
    /// Lead PID extracted from socket name.
    pub lead_pid: u32,
    /// PID of the tmux server process itself.
    pub server_pid: Option<u32>,
    /// Whether the lead process is still alive.
    pub lead_alive: bool,
    /// Panes inside this tmux server: (pane_id, shell_pid).
    pub panes: Vec<TmuxPane>,
    /// Memory used by the tmux server process.
    pub memory_bytes: u64,
    /// Start time of the tmux server process (epoch seconds).
    pub start_time: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TmuxPane {
    pub pane_id: String,
    pub shell_pid: u32,
    /// The Claude child process under this pane shell (if any).
    pub claude_pid: Option<u32>,
    pub claude_alive: bool,
    /// Agent name extracted from `--agent-name` arg (e.g. "decree-arbiter").
    pub agent_name: Option<String>,
    /// Agent type extracted from `--agent-type` arg (e.g. "rune:utility:decree-arbiter").
    pub agent_type: Option<String>,
    /// Team name extracted from `--team-name` arg.
    pub team_name: Option<String>,
    /// Memory bytes of the claude process.
    pub memory_bytes: u64,
    /// CPU percent of the claude process.
    pub cpu_percent: f32,
    /// Start time of the claude process (epoch seconds).
    pub start_time: u64,
    /// Last meaningful line captured from the tmux pane.
    pub last_line: Option<String>,
    /// Last N meaningful lines captured from the tmux pane.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub last_lines: Vec<String>,
    /// Derived pane status based on signals (cpu, last_line content).
    pub status: PaneStatus,
    /// Lowercase status string for JSON consumers.
    pub status_raw: String,
    /// Seconds since last meaningful activity (None if active).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_secs: Option<u64>,
    /// Whether this pane is a zombie (team gone but pane still running).
    pub is_zombie: bool,
    /// Whether the team config dir still exists on disk.
    pub team_exists: bool,
}

/// Health/activity status of a tmux pane.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum PaneStatus {
    /// Claude process is actively working (CPU > 0).
    Active,
    /// Claude process exists but is idle (CPU ~0, no shutdown message).
    Idle,
    /// Claude process received shutdown and is done.
    Done,
    /// No claude process in this pane (just a shell).
    Shell,
}

impl PaneStatus {
    pub fn label(&self) -> &'static str {
        match self {
            PaneStatus::Active => "ACTIVE",
            PaneStatus::Idle => "IDLE",
            PaneStatus::Done => "DONE",
            PaneStatus::Shell => "SHELL",
        }
    }

    /// Returns lowercase status string for JSON serialization.
    pub fn status_raw(&self) -> &'static str {
        match self {
            PaneStatus::Active => "active",
            PaneStatus::Idle => "idle",
            PaneStatus::Done => "done",
            PaneStatus::Shell => "shell",
        }
    }
}

impl std::fmt::Display for PaneStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl TmuxServer {
    pub fn is_orphan(&self) -> bool {
        !self.lead_alive
    }

    pub fn label(&self) -> &'static str {
        if self.lead_alive { "ACTIVE" } else { "ORPHAN" }
    }
}

/// Scan all `tmux -L claude-swarm` servers and check if their lead is alive.
/// Uses a `TmuxSnapshot` and `ConfigDirCache` for shared data.
pub fn scan_tmux_servers_cached(
    sys: &System,
    skip_status: bool,
    pane_lines: usize,
    cache: &ConfigDirCache,
) -> Vec<TmuxServer> {
    let snapshot = TmuxSnapshot::new();
    scan_tmux_servers_from_snapshot(sys, skip_status, pane_lines, Some(cache), &snapshot)
}

/// Scan all `tmux -L claude-swarm` servers (uncached).
pub fn scan_tmux_servers(sys: &System, skip_status: bool, pane_lines: usize) -> Vec<TmuxServer> {
    let snapshot = TmuxSnapshot::new();
    scan_tmux_servers_from_snapshot(sys, skip_status, pane_lines, None, &snapshot)
}

/// Scan tmux servers using a pre-built snapshot (shared with resolve_tmux_pids).
pub fn scan_tmux_servers_with_snapshot(
    sys: &System,
    skip_status: bool,
    pane_lines: usize,
    cache: Option<&ConfigDirCache>,
    snapshot: &TmuxSnapshot,
) -> Vec<TmuxServer> {
    scan_tmux_servers_from_snapshot(sys, skip_status, pane_lines, cache, snapshot)
}

fn scan_tmux_servers_from_snapshot(
    sys: &System,
    skip_status: bool,
    pane_lines: usize,
    cache: Option<&ConfigDirCache>,
    snapshot: &TmuxSnapshot,
) -> Vec<TmuxServer> {
    let mut servers: Vec<TmuxServer> = Vec::new();

    // Pre-scan shutdown agents once for all panes (only during full refresh)
    let shutdown_agents = if !skip_status {
        Some(scan_all_shutdown_agents(cache))
    } else {
        None
    };

    for socket_info in &snapshot.sockets {
        let suffix = &socket_info.socket_name["claude-swarm-".len()..];
        let lead_pid: u32 = match suffix.parse().ok().filter(|&p: &u32| p > 0) {
            Some(p) => p,
            None => continue,
        };
        let lead_alive = sys.process(Pid::from_u32(lead_pid)).is_some();

        // Get tmux server memory and start time
        let server_proc = socket_info
            .server_pid
            .and_then(|pid| sys.process(Pid::from_u32(pid)));
        let memory_bytes = server_proc.map(|p| p.memory()).unwrap_or(0);
        let start_time = server_proc.map(|p| p.start_time()).unwrap_or(0);

        // Query panes using snapshot data
        let panes = query_tmux_panes_from_snapshot(
            &socket_info.socket_name,
            &socket_info.panes,
            sys,
            skip_status,
            pane_lines,
            cache,
            shutdown_agents.as_ref(),
        );

        servers.push(TmuxServer {
            socket_name: socket_info.socket_name.clone(),
            lead_pid,
            server_pid: socket_info.server_pid,
            lead_alive,
            panes,
            memory_bytes,
            start_time,
        });
    }

    servers
}

/// Query panes using pre-built pane map from TmuxSnapshot.
fn query_tmux_panes_from_snapshot(
    socket: &str,
    pane_map: &std::collections::HashMap<String, u32>,
    sys: &System,
    skip_status: bool,
    pane_lines: usize,
    cache: Option<&ConfigDirCache>,
    shutdown_agents: Option<&std::collections::HashSet<String>>,
) -> Vec<TmuxPane> {
    pane_map
        .iter()
        .map(|(pane_id, &shell_pid)| {
            let pane_id = pane_id.clone();

            // Look for a claude child process under this shell
            let claude_proc = sys.processes().values().find(|p| {
                p.parent() == Some(Pid::from_u32(shell_pid)) && {
                    let cmd_str = p
                        .cmd()
                        .iter()
                        .map(|s| s.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" ");
                    cmd_str.contains("claude") || cmd_str.contains("--agent-id")
                }
            });

            let (
                claude_pid,
                claude_alive,
                agent_name,
                agent_type,
                team_name,
                memory_bytes,
                cpu_percent,
                start_time,
            ) = match claude_proc {
                Some(p) => {
                    let cmd: Vec<String> = p
                        .cmd()
                        .iter()
                        .map(|s| s.to_string_lossy().to_string())
                        .collect();
                    (
                        Some(p.pid().as_u32()),
                        true,
                        extract_arg(&cmd, "--agent-name"),
                        extract_arg(&cmd, "--agent-type"),
                        extract_arg(&cmd, "--team-name"),
                        p.memory(),
                        p.cpu_usage(),
                        p.start_time(),
                    )
                }
                None => {
                    // Fall back to shell process start_time so STARTED/UPTIME columns aren't blank
                    let shell_start = sys
                        .process(Pid::from_u32(shell_pid))
                        .map(|p| p.start_time())
                        .unwrap_or(0);
                    (None, false, None, None, None, 0, 0.0, shell_start)
                }
            };

            // team_exists check is cheap (filesystem stat) — always do it
            let team_exists = team_name.as_ref().is_none_or(|tn| {
                let dirs = match cache {
                    Some(c) => c.dirs().to_vec(),
                    None => discover_config_dirs(),
                };
                dirs.iter().any(|d| d.join("teams").join(tn).is_dir())
            });

            let (last_line, status) = if skip_status {
                // Quick mode: skip capture-pane and jsonl checks
                let quick_status = if !claude_alive {
                    PaneStatus::Shell
                } else if cpu_percent < 0.5 {
                    PaneStatus::Idle
                } else {
                    PaneStatus::Active
                };
                (None, quick_status)
            } else {
                let line = capture_pane_last_line(socket, &pane_id);
                let st = derive_pane_status(
                    claude_alive,
                    cpu_percent,
                    line.as_deref(),
                    team_name.as_deref(),
                    agent_name.as_deref(),
                    cache,
                    shutdown_agents,
                );
                (line, st)
            };

            // Capture multiple lines if requested
            let last_lines = if pane_lines > 0 && !skip_status {
                capture_pane_last_lines(socket, &pane_id, pane_lines)
            } else {
                Vec::new()
            };

            TmuxPane {
                pane_id,
                shell_pid,
                claude_pid,
                claude_alive,
                agent_name,
                agent_type,
                team_name,
                memory_bytes,
                cpu_percent,
                start_time,
                last_line,
                last_lines,
                status,
                status_raw: status.status_raw().to_string(),
                stale_secs: None,
                is_zombie: !team_exists,
                team_exists,
            }
        })
        .collect()
}

/// Capture the last meaningful line from a tmux pane.
fn capture_pane_last_line(socket: &str, pane_id: &str) -> Option<String> {
    capture_pane_last_lines(socket, pane_id, 1)
        .into_iter()
        .next()
}

/// Capture the last N meaningful lines from a tmux pane.
/// Returns empty Vec on failure or if no meaningful lines found.
/// Validates socket format (claude-swarm-{digits}) and pane_id format (%*).
pub fn capture_pane_last_lines(socket: &str, pane_id: &str, n: usize) -> Vec<String> {
    use std::process::Command;

    // Validate inputs to prevent command injection
    if !socket.starts_with("claude-swarm-") || !pane_id.starts_with('%') {
        return Vec::new();
    }
    // Strict validation: suffix must be all digits (consistent with kill_tmux_server)
    let suffix = &socket["claude-swarm-".len()..];
    if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
        return Vec::new();
    }

    let output = match Command::new("tmux")
        .args([
            "-L",
            socket,
            "capture-pane",
            "-t",
            pane_id,
            "-p",
            "-S",
            "-50", // Capture more lines to find N meaningful ones
        ])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Find last N non-empty, meaningful lines (strip control/box-drawing chars)
    stdout
        .lines()
        .rev()
        .filter_map(|l| {
            // Strip non-ASCII control chars and box-drawing Unicode
            let clean: String = l
                .chars()
                .filter(|c| {
                    let cp = *c as u32;
                    // Keep ASCII printable (0x20-0x7E) and common Unicode text
                    // Skip: C0/C1 control (0-0x1F, 0x80-0x9F), box drawing (0x2500-0x257F),
                    // block elements (0x2580-0x259F), private use, etc.
                    (0x20..=0x7E).contains(&cp)
                        || (cp > 0x9F
                            && !(0x2500..=0x259F).contains(&cp)
                            && !(0xE000..=0xF8FF).contains(&cp))
                })
                .collect();
            let trimmed = clean.trim().to_string();

            if trimmed.is_empty() {
                return None;
            }

            // Skip Claude Code UI chrome lines (prompt, status bar, permissions)
            let skip_patterns = [
                "❯",
                "bypass permissions",
                "rune-plugin",
                "melina",
                "shift+tab",
                "⏵",
                "⏺",
                "✻ Worked for",
                "⎇",
            ];
            if skip_patterns.iter().any(|p| trimmed.starts_with(p))
                || trimmed.contains("permissions on")
            {
                return None;
            }
            // Skip lines that are just a project/branch name (status bar remnants)
            if !trimmed.contains(' ') && trimmed.len() < 30 {
                return None;
            }
            // Skip lines that are mostly box-drawing remnants (very short after cleaning)
            if trimmed.len() < 4 {
                return None;
            }

            Some(trimmed)
        })
        .take(n)
        .map(|s| {
            if s.len() > 80 {
                format!("{}…", &s[..79])
            } else {
                s
            }
        })
        .collect()
}

/// Pre-scan all recent .jsonl files and build a set of agent names that have shutdown_request.
/// Called once per full refresh cycle to avoid per-pane scanning.
pub fn scan_all_shutdown_agents(
    cache: Option<&ConfigDirCache>,
) -> std::collections::HashSet<String> {
    use std::collections::HashSet;

    let owned;
    let config_dirs: &[PathBuf] = match cache {
        Some(c) => c.dirs(),
        None => {
            owned = discover_config_dirs();
            &owned
        }
    };
    let needle_type = "\"type\":\"shutdown_request\"";
    let mut agents = HashSet::new();

    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(7200))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

    for config_dir in config_dirs {
        let projects_dir = config_dir.join("projects");
        if !projects_dir.is_dir() {
            continue;
        }
        if let Ok(project_entries) = std::fs::read_dir(&projects_dir) {
            for project_entry in project_entries.flatten() {
                let project_path = project_entry.path();
                if let Ok(files) = std::fs::read_dir(&project_path) {
                    for file in files.flatten() {
                        let path = file.path();
                        if path.extension().is_some_and(|e| e == "jsonl") {
                            let mtime = path
                                .metadata()
                                .and_then(|m| m.modified())
                                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                            if mtime < cutoff {
                                continue;
                            }
                            // Scan for all shutdown_request entries and extract recipients
                            scan_jsonl_for_all_shutdowns(&path, needle_type, &mut agents);
                        }
                    }
                }
            }
        }
    }
    agents
}

/// Scan a .jsonl file tail for all shutdown_request recipients.
fn scan_jsonl_for_all_shutdowns(
    path: &std::path::Path,
    needle_type: &str,
    agents: &mut std::collections::HashSet<String>,
) {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader = BufReader::new(file);

    if file_len > 200_000 {
        let _ = reader.seek(SeekFrom::End(-200_000));
        let mut _skip = String::new();
        let _ = reader.read_line(&mut _skip);
    }

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if !line.contains(needle_type) {
            continue;
        }
        // Extract recipient from the line: "recipient":"<name>"
        if let Some(pos) = line.find("\"recipient\":\"") {
            let after = &line[pos + 14..]; // skip past "recipient":"
            if let Some(end) = after.find('"') {
                agents.insert(after[..end].to_string());
            }
        }
    }
}

/// Derive pane status from available signals.
fn derive_pane_status(
    claude_alive: bool,
    cpu_percent: f32,
    last_line: Option<&str>,
    team_name: Option<&str>,
    agent_name: Option<&str>,
    cache: Option<&ConfigDirCache>,
    shutdown_agents: Option<&std::collections::HashSet<String>>,
) -> PaneStatus {
    if !claude_alive {
        return PaneStatus::Shell;
    }

    // Signal 1: Check if last output indicates shutdown/completion
    let is_done_output = last_line.is_some_and(|l| {
        let lower = l.to_lowercase();
        lower.contains("shutting down")
            || lower.contains("shutdown")
            || lower.contains("shut down")
            || lower.contains("acknowledged")
            || lower.contains("phase complete")
            || lower.contains("review complete")
            || lower.contains("completed")
    });

    if is_done_output {
        return PaneStatus::Done;
    }

    // Signal 2: Check transcript for shutdown_request SendMessage to this agent
    if let Some(agent) = agent_name {
        let found = match shutdown_agents {
            Some(set) => set.contains(agent),
            None => check_transcript_has_shutdown(agent, cache),
        };
        if found {
            return PaneStatus::Done;
        }
    }

    // Signal 3: Check inbox for shutdown/idle messages
    if let (Some(team), Some(agent)) = (team_name, agent_name)
        && check_inbox_has_shutdown(team, agent, cache)
    {
        return PaneStatus::Done;
    }

    if cpu_percent < 0.5 {
        return PaneStatus::Idle;
    }

    PaneStatus::Active
}

/// Check transcript .jsonl files for shutdown_request SendMessage to this agent.
/// Scans recent transcripts (last modified) for efficiency.
fn check_transcript_has_shutdown(agent_name: &str, cache: Option<&ConfigDirCache>) -> bool {
    let owned;
    let config_dirs: &[PathBuf] = match cache {
        Some(c) => c.dirs(),
        None => {
            owned = discover_config_dirs();
            &owned
        }
    };
    let needle_recipient = format!("\"recipient\":\"{}\"", agent_name);
    let needle_type = "\"type\":\"shutdown_request\"";

    for config_dir in config_dirs {
        let projects_dir = config_dir.join("projects");
        if !projects_dir.is_dir() {
            continue;
        }
        // Find .jsonl files modified in last 2 hours
        let cutoff = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(7200))
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

        if let Ok(project_entries) = std::fs::read_dir(&projects_dir) {
            for project_entry in project_entries.flatten() {
                let project_path = project_entry.path();
                // Check direct .jsonl files in project dir
                if let Ok(files) = std::fs::read_dir(&project_path) {
                    for file in files.flatten() {
                        let path = file.path();
                        if path.extension().is_some_and(|e| e == "jsonl") {
                            // Check mtime
                            let mtime = path
                                .metadata()
                                .and_then(|m| m.modified())
                                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                            if mtime < cutoff {
                                continue;
                            }
                            // Scan file from end (last 100KB) for shutdown_request
                            if scan_jsonl_for_shutdown(&path, &needle_recipient, needle_type) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

/// Scan last portion of a .jsonl file for shutdown_request to a specific recipient.
fn scan_jsonl_for_shutdown(
    path: &std::path::Path,
    needle_recipient: &str,
    needle_type: &str,
) -> bool {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader = BufReader::new(file);

    // Seek to last 200KB to avoid reading huge files
    if file_len > 200_000 {
        let _ = reader.seek(SeekFrom::End(-200_000));
        // Skip partial line
        let mut _skip = String::new();
        let _ = reader.read_line(&mut _skip);
    }

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        // Fast string check before JSON parsing
        if line.contains(needle_type) && line.contains(needle_recipient) {
            return true;
        }
    }
    false
}

/// Check if an agent has shutdown signals in inbox or team-lead inbox.
fn check_inbox_has_shutdown(
    team_name: &str,
    agent_name: &str,
    cache: Option<&ConfigDirCache>,
) -> bool {
    let owned;
    let config_dirs: &[PathBuf] = match cache {
        Some(c) => c.dirs(),
        None => {
            owned = discover_config_dirs();
            &owned
        }
    };
    let done_keywords = [
        "shutdown",
        "shut down",
        "terminate",
        "no work left",
        "no unblocked tasks",
        "all tasks complete",
        "awaiting shutdown",
        "idle",
        "tasks done",
        "seal:",
        "final integration",
        "all done",
        "work complete",
    ];

    for config_dir in config_dirs {
        let inboxes_dir = config_dir.join("teams").join(team_name).join("inboxes");

        // Check 1: agent's own inbox for shutdown requests TO it
        let agent_inbox = inboxes_dir.join(format!("{}.json", agent_name));
        if check_inbox_file_for_keywords(&agent_inbox, None, &done_keywords) {
            return true;
        }

        // Check 2: team-lead inbox for messages FROM this agent saying it's done/idle
        let lead_inbox = inboxes_dir.join("team-lead.json");
        if check_inbox_file_for_keywords(&lead_inbox, Some(agent_name), &done_keywords) {
            return true;
        }
    }
    false
}

/// Check last N messages in an inbox file for keywords.
/// If `from_filter` is Some, only check messages from that sender.
fn check_inbox_file_for_keywords(
    path: &std::path::Path,
    from_filter: Option<&str>,
    keywords: &[&str],
) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let msgs: Vec<serde_json::Value> = match serde_json::from_str(&content) {
        Ok(m) => m,
        Err(_) => return false,
    };

    for msg in msgs.iter().rev().take(5) {
        // Filter by sender if specified
        if let Some(filter) = from_filter {
            let from = msg.get("from").and_then(|v| v.as_str()).unwrap_or("");
            if from != filter {
                continue;
            }
        }

        // Check text and summary fields
        for field in ["text", "summary"] {
            let val = msg.get(field).and_then(|v| v.as_str()).unwrap_or("");
            let lower = val.to_lowercase();
            if keywords.iter().any(|kw| lower.contains(kw)) {
                return true;
            }
        }
    }
    false
}

/// Extract a CLI flag value from args, e.g. `--agent-name decree-arbiter`.
fn extract_arg(cmd: &[String], flag: &str) -> Option<String> {
    cmd.iter()
        .position(|a| a == flag)
        .and_then(|i| cmd.get(i + 1))
        .cloned()
}

/// Kill an orphan tmux server by socket name.
/// Validates socket format to prevent command injection.
pub fn kill_tmux_server(socket: &str) -> bool {
    // Validate socket name format: claude-swarm-{digits}
    if !socket.starts_with("claude-swarm-") {
        return false;
    }
    let suffix = &socket["claude-swarm-".len()..];
    if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    use std::process::Command;
    Command::new("tmux")
        .args(["-L", socket, "kill-server"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Cached set of Claude config directories, computed once per refresh cycle.
/// Includes TTL-based invalidation to detect new config dirs.
#[derive(Debug, Clone)]
pub struct ConfigDirCache {
    dirs: Vec<PathBuf>,
    last_refresh: std::time::Instant,
}

/// TTL for ConfigDirCache before it should be refreshed (60 seconds).
const CACHE_TTL_SECS: u64 = 60;

impl Default for ConfigDirCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigDirCache {
    /// Discover config dirs once and cache them.
    pub fn new() -> Self {
        Self {
            dirs: discover_config_dirs(),
            last_refresh: std::time::Instant::now(),
        }
    }

    /// Access the cached directories.
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// Check if the cache should be refreshed based on TTL.
    pub fn should_refresh(&self) -> bool {
        self.last_refresh.elapsed().as_secs() >= CACHE_TTL_SECS
    }
}

/// Find all possible Claude config directories.
pub fn discover_config_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home) = dirs::home_dir() {
        let default = home.join(".claude");
        if default.is_dir() {
            dirs.push(default);
        }

        if let Ok(entries) = std::fs::read_dir(&home) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(".claude-") && entry.path().is_dir() {
                    dirs.push(entry.path());
                }
            }
        }
    }

    if let Ok(env_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let path = PathBuf::from(env_dir);
        if path.is_dir() && !dirs.contains(&path) {
            dirs.push(path);
        }
    }

    dirs
}
