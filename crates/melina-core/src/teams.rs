//! Team discovery — read Agent Team config from filesystem.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use sysinfo::{System, Pid};

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
        self.members.iter().filter(|m| m.name != "team-lead").collect()
    }
}

/// Scan all CLAUDE_CONFIG_DIRs for active teams.
pub fn scan_teams() -> Vec<TeamInfo> {
    let mut teams = Vec::new();
    let config_dirs = discover_config_dirs();

    for config_dir in config_dirs {
        let teams_dir = config_dir.join("teams");
        if teams_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&teams_dir) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        if let Some(team) = read_team(&entry.path(), &config_dir) {
                            teams.push(team);
                        }
                    }
                }
            }
        }
    }

    teams
}


fn read_team(team_dir: &Path, config_dir: &Path) -> Option<TeamInfo> {
    let config_path = team_dir.join("config.json");
    let config_str = std::fs::read_to_string(&config_path).ok()?;
    let config: serde_json::Value = serde_json::from_str(&config_str).ok()?;

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

    if tasks_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&tasks_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json")
                    && path.file_name().is_some_and(|n| n != ".lock")
                {
                    task_count += 1;
                    // Read task owner to discover in-process teammates
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(task) = serde_json::from_str::<serde_json::Value>(&content) {
                            if let Some(owner) = task.get("owner").and_then(|v| v.as_str()) {
                                if !owner.is_empty()
                                    && owner != "-"
                                    && !task_owners.contains(&owner.to_string())
                                {
                                    task_owners.push(owner.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Add in-process teammates discovered from task owners but missing from config.json
    // Use fuzzy match: skip if owner is a prefix/substring of any known member name
    // (e.g. task owner "codex-phase-handler" matches member "codex-phase-handler-sv")
    let known_names: Vec<String> = members.iter().map(|m| m.name.clone()).collect();
    for owner in &task_owners {
        let already_known = known_names.iter().any(|n| n == owner || n.starts_with(owner) || owner.starts_with(n));
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
/// We find these sockets via `ps`, then query each for pane→PID mapping.
/// Accepts a pre-created `System` to avoid redundant process table loads.
pub fn resolve_tmux_pids(teams: &mut [TeamInfo], sys: &System) {
    let pane_map = build_tmux_pane_map();
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
                        let cmd_str = p.cmd().iter()
                            .map(|s| s.to_string_lossy())
                            .collect::<Vec<_>>()
                            .join(" ");
                        name.contains("claude")
                            || name.contains("node")
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

/// Build a map of tmux socket → (pane_id → pid) by:
/// 1. Finding `tmux -L claude-swarm` processes
/// 2. Querying each socket for its panes
fn build_tmux_pane_map() -> Vec<(String, std::collections::HashMap<String, u32>)> {
    use std::collections::HashMap;
    use std::process::Command;

    let mut result = Vec::new();

    // Find tmux server processes to discover socket names
    let ps_output = Command::new("ps")
        .args(["-eo", "args"])
        .output();

    let ps_str = match ps_output {
        Ok(ref out) => String::from_utf8_lossy(&out.stdout).to_string(),
        Err(_) => return result,
    };

    let mut sockets: Vec<String> = Vec::new();
    for line in ps_str.lines() {
        // Match: tmux -L claude-swarm-NNNNN ...
        if let Some(pos) = line.find("claude-swarm-") {
            let after = &line[pos..];
            let socket_name: String = after.chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
            if !socket_name.is_empty() && !sockets.contains(&socket_name) {
                // Validate socket name format to prevent injection.
                // Expected pattern: claude-swarm-{pid} where pid is numeric.
                let suffix = &socket_name["claude-swarm-".len()..];
                if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                    sockets.push(socket_name);
                }
            }
        }
    }

    // Query each socket for pane→pid mapping
    for socket in &sockets {
        let output = Command::new("tmux")
            .args(["-L", socket, "list-panes", "-a",
                   "-F", "#{pane_id}|#{pane_pid}"])
            .output();

        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let mut panes = HashMap::new();
            for line in stdout.lines() {
                let parts: Vec<&str> = line.splitn(2, '|').collect();
                if parts.len() == 2 {
                    if let Ok(pid) = parts[1].parse::<u32>() {
                        panes.insert(parts[0].to_string(), pid);
                    }
                }
            }
            if !panes.is_empty() {
                result.push((socket.clone(), panes));
            }
        }
    }

    result
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
    /// Derived pane status based on signals (cpu, last_line content).
    pub status: PaneStatus,
    /// Whether the team config dir still exists on disk.
    pub team_exists: bool,
}

/// Health/activity status of a tmux pane.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
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
/// Accepts a pre-created `System` to avoid redundant process table loads.
/// When `skip_status` is true, skips expensive capture-pane/jsonl for pane status detection.
pub fn scan_tmux_servers(sys: &System, skip_status: bool) -> Vec<TmuxServer> {
    use std::process::Command;

    // Find tmux server processes: `tmux -L claude-swarm-NNNNN ...`
    let ps_output = Command::new("ps")
        .args(["-eo", "pid,args"])
        .output();

    let ps_str = match ps_output {
        Ok(ref out) => String::from_utf8_lossy(&out.stdout).to_string(),
        Err(_) => return Vec::new(),
    };

    let mut servers: Vec<TmuxServer> = Vec::new();
    let mut seen_sockets: Vec<String> = Vec::new();

    for line in ps_str.lines() {
        let line = line.trim();
        if !line.contains("claude-swarm-") || !line.contains("tmux") {
            continue;
        }

        // Extract PID from the start of the line
        let server_pid: Option<u32> = line.split_whitespace().next()
            .and_then(|s| s.parse().ok());

        // Extract socket name
        if let Some(pos) = line.find("claude-swarm-") {
            let after = &line[pos..];
            let socket_name: String = after.chars()
                .take_while(|c| !c.is_whitespace())
                .collect();

            let suffix = &socket_name["claude-swarm-".len()..];
            if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if seen_sockets.contains(&socket_name) {
                continue;
            }
            seen_sockets.push(socket_name.clone());

            let lead_pid: u32 = suffix.parse().ok().filter(|&p| p > 0).unwrap_or(0);
            if lead_pid == 0 {
                continue; // Skip invalid PID
            }
            let lead_alive = sys.process(Pid::from_u32(lead_pid)).is_some();

            // Get tmux server memory and start time
            let server_proc = server_pid
                .and_then(|pid| sys.process(Pid::from_u32(pid)));
            let memory_bytes = server_proc.map(|p| p.memory()).unwrap_or(0);
            let start_time = server_proc.map(|p| p.start_time()).unwrap_or(0);

            // Query panes
            let panes = query_tmux_panes(&socket_name, sys, skip_status);

            servers.push(TmuxServer {
                socket_name,
                lead_pid,
                server_pid,
                lead_alive,
                panes,
                memory_bytes,
                start_time,
            });
        }
    }

    servers
}

/// Query panes from a tmux socket and check for claude child processes.
/// When `skip_status` is true, skips capture-pane and jsonl scanning for status detection.
fn query_tmux_panes(socket: &str, sys: &System, skip_status: bool) -> Vec<TmuxPane> {
    use std::process::Command;

    let output = Command::new("tmux")
        .args(["-L", socket, "list-panes", "-a",
               "-F", "#{pane_id}|#{pane_pid}"])
        .output();

    let stdout = match output {
        Ok(ref out) => String::from_utf8_lossy(&out.stdout).to_string(),
        Err(_) => return Vec::new(),
    };

    stdout.lines().filter_map(|line| {
        let parts: Vec<&str> = line.splitn(2, '|').collect();
        if parts.len() != 2 { return None; }
        let shell_pid: u32 = parts[1].parse().ok()?;
        let pane_id = parts[0].to_string();

        // Look for a claude child process under this shell
        let claude_proc = sys.processes().values().find(|p| {
            p.parent() == Some(Pid::from_u32(shell_pid)) && {
                let cmd_str = p.cmd().iter()
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ");
                cmd_str.contains("claude") || cmd_str.contains("--agent-id")
            }
        });

        let (claude_pid, claude_alive, agent_name, agent_type, team_name, memory_bytes, cpu_percent, start_time) =
            match claude_proc {
                Some(p) => {
                    let cmd: Vec<String> = p.cmd().iter()
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
                None => (None, false, None, None, None, 0, 0.0, 0),
            };

        let (last_line, status, team_exists) = if skip_status {
            // Quick mode: skip capture-pane, jsonl, and filesystem checks
            let quick_status = if !claude_alive {
                PaneStatus::Shell
            } else if cpu_percent < 0.5 {
                PaneStatus::Idle
            } else {
                PaneStatus::Active
            };
            (None, quick_status, true)
        } else {
            let line = capture_pane_last_line(socket, &pane_id);
            let st = derive_pane_status(
                claude_alive, cpu_percent, line.as_deref(),
                team_name.as_deref(), agent_name.as_deref(),
            );
            let exists = team_name.as_ref().map_or(true, |tn| {
                discover_config_dirs().iter().any(|d| d.join("teams").join(tn).is_dir())
            });
            (line, st, exists)
        };

        Some(TmuxPane {
            pane_id, shell_pid, claude_pid, claude_alive,
            agent_name, agent_type, team_name, memory_bytes, cpu_percent, start_time,
            last_line, status, team_exists,
        })
    }).collect()
}

/// Capture the last meaningful line from a tmux pane.
fn capture_pane_last_line(socket: &str, pane_id: &str) -> Option<String> {
    use std::process::Command;

    let output = Command::new("tmux")
        .args(["-L", socket, "capture-pane", "-t", pane_id, "-p", "-S", "-30"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Find last non-empty, meaningful line (strip control/box-drawing chars)
    stdout.lines()
        .rev()
        .filter_map(|l| {
            // Strip non-ASCII control chars and box-drawing Unicode
            let clean: String = l.chars()
                .filter(|c| {
                    let cp = *c as u32;
                    // Keep ASCII printable (0x20-0x7E) and common Unicode text
                    // Skip: C0/C1 control (0-0x1F, 0x80-0x9F), box drawing (0x2500-0x257F),
                    // block elements (0x2580-0x259F), private use, etc.
                    (0x20..=0x7E).contains(&cp)
                        || (cp > 0x9F && !(0x2500..=0x259F).contains(&cp)
                            && !(0xE000..=0xF8FF).contains(&cp))
                })
                .collect();
            let trimmed = clean.trim().to_string();

            if trimmed.is_empty() { return None; }

            // Skip Claude Code UI chrome lines (prompt, status bar, permissions)
            let skip_patterns = [
                "❯", "bypass permissions", "rune-plugin", "melina",
                "shift+tab", "⏵", "⏺", "✻ Worked for", "⎇",
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
            if trimmed.len() < 4 { return None; }

            Some(trimmed)
        })
        .next()
        .map(|s| {
            if s.len() > 80 { format!("{}…", &s[..79]) } else { s }
        })
}

/// Derive pane status from available signals.
fn derive_pane_status(
    claude_alive: bool,
    cpu_percent: f32,
    last_line: Option<&str>,
    team_name: Option<&str>,
    agent_name: Option<&str>,
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
        if check_transcript_has_shutdown(agent) {
            return PaneStatus::Done;
        }
    }

    // Signal 3: Check inbox for shutdown/idle messages
    if let (Some(team), Some(agent)) = (team_name, agent_name) {
        if check_inbox_has_shutdown(team, agent) {
            return PaneStatus::Done;
        }
    }

    if cpu_percent < 0.5 {
        return PaneStatus::Idle;
    }

    PaneStatus::Active
}

/// Check transcript .jsonl files for shutdown_request SendMessage to this agent.
/// Scans recent transcripts (last modified) for efficiency.
fn check_transcript_has_shutdown(agent_name: &str) -> bool {
    let config_dirs = discover_config_dirs();
    let needle_recipient = format!("\"recipient\":\"{}\"", agent_name);
    let needle_type = "\"type\":\"shutdown_request\"";

    for config_dir in &config_dirs {
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
                            let mtime = path.metadata()
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
fn scan_jsonl_for_shutdown(path: &std::path::Path, needle_recipient: &str, needle_type: &str) -> bool {
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
fn check_inbox_has_shutdown(team_name: &str, agent_name: &str) -> bool {
    let config_dirs = discover_config_dirs();
    let done_keywords = [
        "shutdown", "shut down", "terminate",
        "no work left", "no unblocked tasks", "all tasks complete",
        "awaiting shutdown", "idle", "tasks done", "seal:",
        "final integration", "all done", "work complete",
    ];

    for config_dir in &config_dirs {
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

/// Find all possible Claude config directories.
fn discover_config_dirs() -> Vec<PathBuf> {
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
