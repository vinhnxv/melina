//! Process and teammate health assessment.

use crate::ProcessInfo;
use crate::discovery::create_process_system;
use crate::teams::{TeamInfo, TeamMember};
use serde::Serialize;
use sysinfo::{System, Pid, ProcessesToUpdate, ProcessRefreshKind};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Create a lightweight System for kill/lookup operations (no CPU, single refresh).
fn create_light_system() -> System {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    sys
}

/// Health status of a process.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum Health {
    Ok,
    Zombie,
    Orphan,
    Stale,
}

impl Health {
    pub fn label(&self) -> &'static str {
        match self {
            Health::Ok => "OK",
            Health::Zombie => "ZOMBIE",
            Health::Orphan => "ORPHAN",
            Health::Stale => "STALE",
        }
    }

    pub fn is_healthy(&self) -> bool {
        matches!(self, Health::Ok)
    }
}

impl std::fmt::Display for Health {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Check health of an OS process.
pub fn check_health(proc: &ProcessInfo, is_mcp: bool, sys: &System) -> Health {
    let status_lower = proc.status.to_lowercase();
    if status_lower.contains("zombie") {
        return Health::Zombie;
    }
    if proc.ppid <= 1 || !is_pid_alive(proc.ppid, sys) {
        return Health::Orphan;
    }
    let now = now_epoch();
    let uptime_secs = now.saturating_sub(proc.start_time);
    let stale_threshold = if is_mcp { 12 * 3600 } else { 3600 };
    if proc.cpu_percent < 0.1 && uptime_secs > stale_threshold {
        return Health::Stale;
    }
    Health::Ok
}

// ── Teammate Health ──────────────────────────────────────────────

/// Health status of a teammate (from config.json + filesystem signals).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum TeammateHealth {
    /// Teammate is active (recent inbox activity).
    Active,
    /// Teammate completed its tasks.
    Completed,
    /// Team's owner process is dead → entire team is zombie.
    Zombie,
    /// Teammate has no inbox activity for too long.
    Stale { idle_secs: u64 },
    /// Teammate owns in_progress tasks but has no recent activity.
    Stuck { task_ids: Vec<String> },
}

impl TeammateHealth {
    pub fn label(&self) -> &'static str {
        match self {
            TeammateHealth::Active => "ACTIVE",
            TeammateHealth::Completed => "DONE",
            TeammateHealth::Zombie => "ZOMBIE",
            TeammateHealth::Stale { .. } => "STALE",
            TeammateHealth::Stuck { .. } => "STUCK",
        }
    }

    pub fn is_healthy(&self) -> bool {
        matches!(self, TeammateHealth::Active | TeammateHealth::Completed)
    }
}

impl std::fmt::Display for TeammateHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeammateHealth::Stale { idle_secs } => {
                let mins = idle_secs / 60;
                write!(f, "STALE ({}m idle)", mins)
            }
            TeammateHealth::Stuck { task_ids } => {
                write!(f, "STUCK (tasks: {})", task_ids.join(","))
            }
            _ => f.write_str(self.label()),
        }
    }
}

/// Full health report for a team.
#[derive(Debug, Clone, Serialize)]
pub struct TeamHealthReport {
    pub team_name: String,
    pub owner_alive: bool,
    pub members: Vec<TeammateHealthEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeammateHealthEntry {
    pub name: String,
    pub agent_type: String,
    pub health: TeammateHealth,
    pub last_activity_secs: Option<u64>,
}

/// Stale threshold: teammate with no inbox activity for 5+ minutes.
const TEAMMATE_STALE_SECS: u64 = 300;

/// Assess health of all teammates in a team.
pub fn check_team_health(team: &TeamInfo, sys: &System) -> TeamHealthReport {
    let owner_alive = check_team_owner_alive(team, sys);
    let now = now_epoch();

    let members = team.teammates().into_iter().map(|m| {
        let health = if !owner_alive {
            TeammateHealth::Zombie
        } else {
            check_teammate_health(m, team, now)
        };

        let last_activity_secs = get_inbox_age(team, &m.name, now);

        TeammateHealthEntry {
            name: m.name.clone(),
            agent_type: m.agent_type.clone(),
            health,
            last_activity_secs,
        }
    }).collect();

    TeamHealthReport {
        team_name: team.name.clone(),
        owner_alive,
        members,
    }
}

/// Check if the team's owner process is still alive.
fn check_team_owner_alive(team: &TeamInfo, sys: &System) -> bool {
    let session_path = team.config_dir.join("teams").join(&team.name).join(".session");
    if let Ok(content) = std::fs::read_to_string(&session_path) {
        if let Ok(session) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(pid_str) = session.get("owner_pid").and_then(|v| v.as_str()) {
                if let Ok(pid) = pid_str.parse::<u32>() {
                    return is_pid_alive(pid, sys);
                }
            }
        }
    }
    false
}

/// Check individual teammate health from inbox + task signals.
fn check_teammate_health(member: &TeamMember, team: &TeamInfo, now: u64) -> TeammateHealth {
    let inbox_age = get_inbox_age(team, &member.name, now);

    // Check if teammate has completed tasks
    let (completed_count, stuck_tasks) = check_teammate_tasks(team, &member.name);

    // If teammate has completed tasks and no stuck ones, it's done
    if completed_count > 0 && stuck_tasks.is_empty() {
        return TeammateHealth::Completed;
    }

    // If stuck tasks exist and inbox is stale
    if !stuck_tasks.is_empty() {
        if let Some(age) = inbox_age {
            if age > TEAMMATE_STALE_SECS {
                return TeammateHealth::Stuck { task_ids: stuck_tasks };
            }
        }
    }

    // If inbox is stale
    if let Some(age) = inbox_age {
        if age > TEAMMATE_STALE_SECS {
            return TeammateHealth::Stale { idle_secs: age };
        }
    }

    TeammateHealth::Active
}

/// Get seconds since last inbox modification for a teammate.
fn get_inbox_age(team: &TeamInfo, member_name: &str, now: u64) -> Option<u64> {
    let inbox_path = team.config_dir
        .join("teams")
        .join(&team.name)
        .join("inboxes")
        .join(format!("{}.json", member_name));

    file_mtime_epoch(&inbox_path).map(|mtime| now.saturating_sub(mtime))
}

/// Check tasks owned by this teammate: (completed_count, stuck_task_ids).
fn check_teammate_tasks(team: &TeamInfo, member_name: &str) -> (usize, Vec<String>) {
    let tasks_dir = team.config_dir.join("tasks").join(&team.name);
    let mut completed = 0;
    let mut stuck = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&tasks_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") && path.file_name().is_some_and(|n| n != ".lock") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(task) = serde_json::from_str::<serde_json::Value>(&content) {
                        let owner = task.get("owner").and_then(|v| v.as_str()).unwrap_or("");
                        let status = task.get("status").and_then(|v| v.as_str()).unwrap_or("");

                        if owner == member_name {
                            match status {
                                "completed" => completed += 1,
                                "in_progress" => {
                                    let id = task.get("id")
                                        .map(|v| v.to_string())
                                        .unwrap_or_else(|| "?".to_string());
                                    stuck.push(id);
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }

    (completed, stuck)
}

// ── Zombie Detection & Cleanup ──────────────────────────────────

/// A detected zombie entry — something that should be cleaned up.
#[derive(Debug, Clone)]
pub enum ZombieEntry {
    /// A team whose owner process is dead.
    Team {
        name: String,
        config_dir: std::path::PathBuf,
        member_count: usize,
        task_count: usize,
    },
    /// An orphan tmux server whose lead is dead.
    OrphanTmux {
        socket_name: String,
        lead_pid: u32,
        pane_count: usize,
        server_pid: Option<u32>,
    },
    /// An orphan shell pane inside a claude-swarm (no claude child process).
    OrphanShell {
        socket_name: String,
        pane_id: String,
        shell_pid: u32,
    },
}

impl ZombieEntry {
    /// Short description for display.
    pub fn label(&self) -> String {
        match self {
            ZombieEntry::Team { name, member_count, task_count, .. } => {
                format!("ZOMBIE TEAM: {} ({} members, {} tasks)", name, member_count, task_count)
            }
            ZombieEntry::OrphanTmux { socket_name, lead_pid, pane_count, .. } => {
                format!("ORPHAN TMUX: {} (lead:{}, {} panes)", socket_name, lead_pid, pane_count)
            }
            ZombieEntry::OrphanShell { socket_name, pane_id, shell_pid } => {
                format!("ORPHAN SHELL: pane {} (sh:{}) in {}", pane_id, shell_pid, socket_name)
            }
        }
    }

    /// Reason why this is considered a zombie.
    pub fn reason(&self) -> &'static str {
        match self {
            ZombieEntry::Team { .. } => "owner process is dead",
            ZombieEntry::OrphanTmux { .. } => "lead process is dead",
            ZombieEntry::OrphanShell { .. } => "claude process exited, empty shell remains",
        }
    }
}

/// Scan for zombie teams and orphan tmux servers without killing anything.
pub fn scan_zombies() -> Vec<ZombieEntry> {
    use crate::teams::{scan_teams, scan_tmux_servers};

    let sys = create_process_system();
    let teams = scan_teams();
    let mut entries = Vec::new();

    for team in &teams {
        let report = check_team_health(team, &sys);
        if !report.owner_alive {
            entries.push(ZombieEntry::Team {
                name: team.name.clone(),
                config_dir: team.config_dir.clone(),
                member_count: team.members.len(),
                task_count: team.task_count,
            });
        }
    }

    let tmux_servers = scan_tmux_servers(&sys, true);
    for srv in &tmux_servers {
        if srv.is_orphan() {
            entries.push(ZombieEntry::OrphanTmux {
                socket_name: srv.socket_name.clone(),
                lead_pid: srv.lead_pid,
                pane_count: srv.panes.len(),
                server_pid: srv.server_pid,
            });
        } else {
            // Active server — check for orphan shell panes (claude exited, shell remains)
            for pane in &srv.panes {
                if !pane.claude_alive && pane.agent_name.is_none() {
                    entries.push(ZombieEntry::OrphanShell {
                        socket_name: srv.socket_name.clone(),
                        pane_id: pane.pane_id.clone(),
                        shell_pid: pane.shell_pid,
                    });
                }
            }
        }
    }

    entries
}

/// Result of a kill_zombies operation.
#[derive(Debug, Default)]
pub struct KillZombiesResult {
    /// Number of zombie teams cleaned up.
    pub teams_cleaned: usize,
    /// Number of orphan tmux servers killed.
    pub tmux_cleaned: usize,
    /// Number of orphan shell panes killed.
    pub shells_cleaned: usize,
    /// Error messages from failed operations.
    pub errors: Vec<String>,
}

impl KillZombiesResult {
    pub fn total(&self) -> usize {
        self.teams_cleaned + self.tmux_cleaned + self.shells_cleaned
    }
}

/// Kill all zombie teams, orphan tmux servers, and orphan shell panes.
///
/// For zombie teams: kills tmux teammates, removes team/task directories.
/// For orphan tmux servers: kills the tmux server process.
/// For orphan shells: kills the empty tmux pane.
pub fn kill_zombies() -> KillZombiesResult {
    use crate::teams::{scan_teams, scan_tmux_servers, kill_tmux_server};

    let sys = create_process_system();
    let teams = scan_teams();
    let mut result = KillZombiesResult::default();

    for team in &teams {
        let report = check_team_health(team, &sys);
        if !report.owner_alive {
            // Kill tmux teammates
            for member in &team.members {
                if member.name == "team-lead" {
                    continue;
                }
                if !member.tmux_pane_id.is_empty() {
                    let pane_id = &member.tmux_pane_id;
                    let digits = &pane_id[1..];
                    if !pane_id.starts_with('%') || digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                        continue;
                    }
                    let _ = std::process::Command::new("tmux")
                        .args(["kill-pane", "-t", pane_id])
                        .output();
                }
            }

            // Remove filesystem artifacts (with path validation)
            let team_dir = team.config_dir.join("teams").join(&team.name);
            let tasks_dir = team.config_dir.join("tasks").join(&team.name);

            for dir in [&team_dir, &tasks_dir] {
                if dir.exists() {
                    match dir.canonicalize() {
                        Ok(canonical) => {
                            if canonical.to_string_lossy().contains("/.claude") {
                                if let Err(e) = std::fs::remove_dir_all(&canonical) {
                                    result.errors.push(format!("rm {}: {}", canonical.display(), e));
                                }
                            }
                        }
                        Err(e) => {
                            result.errors.push(format!("canonicalize {}: {}", dir.display(), e));
                        }
                    }
                }
            }

            result.teams_cleaned += 1;
        }
    }

    // Kill orphan tmux servers and orphan shell panes
    let tmux_servers = scan_tmux_servers(&sys, true);
    for srv in &tmux_servers {
        if srv.is_orphan() {
            if kill_tmux_server(&srv.socket_name) {
                result.tmux_cleaned += 1;
            } else {
                result.errors.push(format!("failed to kill tmux server {}", srv.socket_name));
            }
        } else {
            // Active server — kill orphan shell panes (claude exited, empty shell remains)
            for pane in &srv.panes {
                if !pane.claude_alive && pane.agent_name.is_none() {
                    let digits = &pane.pane_id[1..];
                    if pane.pane_id.starts_with('%') && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                        let kill_result = std::process::Command::new("tmux")
                            .args(["-L", &srv.socket_name, "kill-pane", "-t", &pane.pane_id])
                            .output();
                        if kill_result.is_ok_and(|o| o.status.success()) {
                            result.shells_cleaned += 1;
                        } else {
                            result.errors.push(format!("failed to kill orphan shell pane {} in {}", pane.pane_id, srv.socket_name));
                        }
                    }
                }
            }
        }
    }

    result
}

// ── Process Kill by PID ─────────────────────────────────────────

/// Info about a process looked up by PID before killing.
#[derive(Debug, Clone)]
pub struct ProcessLookup {
    pub pid: u32,
    /// Human-readable label (agent name, process name, or pane agent).
    pub label: String,
    /// What kind of process this is.
    pub kind: ProcessLookupKind,
    /// Whether this is a Claude-related process (safe to kill).
    pub is_claude: bool,
}

/// Classification of a looked-up process.
#[derive(Debug, Clone)]
pub enum ProcessLookupKind {
    /// A tmux pane (shell or claude process inside claude-swarm).
    TmuxPane {
        socket_name: String,
        pane_id: String,
        agent_name: Option<String>,
    },
    /// A regular OS process.
    Process {
        name: String,
        cmd_preview: String,
    },
    /// PID not found.
    NotFound,
}

/// Look up a PID to get info about what it is. Does NOT kill anything.
pub fn lookup_process(pid: u32) -> ProcessLookup {
    use crate::teams::scan_tmux_servers;

    let sys = create_process_system();
    let tmux_servers = scan_tmux_servers(&sys, true);

    // Check if PID matches a tmux pane
    for srv in &tmux_servers {
        for pane in &srv.panes {
            if pane.shell_pid == pid || pane.claude_pid == Some(pid) {
                let label = pane.agent_name.as_deref().unwrap_or("shell").to_string();
                return ProcessLookup {
                    pid,
                    label: format!("{} (tmux pane {} in {})", label, pane.pane_id, srv.socket_name),
                    kind: ProcessLookupKind::TmuxPane {
                        socket_name: srv.socket_name.clone(),
                        pane_id: pane.pane_id.clone(),
                        agent_name: pane.agent_name.clone(),
                    },
                    is_claude: true,
                };
            }
        }
    }

    // Reuse the same sys for process lookup
    match sys.process(Pid::from_u32(pid)) {
        Some(proc_) => {
            let name = proc_.name().to_string_lossy().to_string();
            let cmd_str: String = proc_.cmd().iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(" ");

            let is_claude = cmd_str.contains("claude")
                || cmd_str.contains("--agent-id")
                || name.contains("claude");

            let agent_name = cmd_str.split("--agent-name ")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .unwrap_or(&name)
                .to_string();

            let cmd_preview = if cmd_str.len() > 80 {
                format!("{}...", &cmd_str[..77])
            } else {
                cmd_str
            };

            ProcessLookup {
                pid,
                label: agent_name,
                kind: ProcessLookupKind::Process { name, cmd_preview },
                is_claude,
            }
        }
        None => ProcessLookup {
            pid,
            label: "not found".to_string(),
            kind: ProcessLookupKind::NotFound,
            is_claude: false,
        },
    }
}

/// Kill a process by PID. Returns Ok(description) or Err(reason).
///
/// Safety: only kills claude-related processes. Refuses non-claude PIDs.
pub fn kill_process(pid: u32) -> Result<String, String> {
    let lookup = lookup_process(pid);

    if !lookup.is_claude {
        return match lookup.kind {
            ProcessLookupKind::NotFound => Err(format!("PID {} not found", pid)),
            ProcessLookupKind::Process { name, .. } =>
                Err(format!("PID {} ({}) is not a Claude process", pid, name)),
            _ => Err(format!("PID {} is not a Claude process", pid)),
        };
    }

    match lookup.kind {
        ProcessLookupKind::TmuxPane { socket_name, pane_id, agent_name } => {
            let label = agent_name.as_deref().unwrap_or("shell");
            // Kill the tmux pane
            let digits = &pane_id[1..];
            if pane_id.starts_with('%') && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                let result = std::process::Command::new("tmux")
                    .args(["-L", &socket_name, "kill-pane", "-t", &pane_id])
                    .output();
                if result.is_ok_and(|o| o.status.success()) {
                    return Ok(format!("Killed tmux pane {} ({})", pane_id, label));
                }
            }
            // Fallback: kill the process directly
            let sys = create_light_system();
            if let Some(proc_) = sys.process(Pid::from_u32(pid)) {
                if proc_.kill() {
                    return Ok(format!("Killed PID {} ({})", pid, label));
                }
            }
            Err(format!("Failed to kill PID {} ({})", pid, label))
        }
        ProcessLookupKind::Process { .. } => {
            let sys = create_light_system();
            if let Some(proc_) = sys.process(Pid::from_u32(pid)) {
                if proc_.kill() {
                    return Ok(format!("Killed PID {} ({})", pid, lookup.label));
                }
                return Err(format!("Failed to kill PID {} (permission denied?)", pid));
            }
            Err(format!("PID {} disappeared before kill", pid))
        }
        ProcessLookupKind::NotFound => Err(format!("PID {} not found", pid)),
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn is_pid_alive(pid: u32, sys: &System) -> bool {
    sys.process(Pid::from_u32(pid)).is_some()
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::ZERO)
        .as_secs()
}

fn file_mtime_epoch(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}
