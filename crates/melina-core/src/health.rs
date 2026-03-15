//! Process and teammate health assessment.

use crate::ProcessInfo;
use crate::discovery::create_process_system;
use crate::teams::{PaneStatus, TeamInfo, TeamMember};
use serde::Serialize;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

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
    // PPID=1 is valid for daemon processes (launchd on macOS, init on Linux)
    // Only mark as orphan if parent is dead AND not a daemon (PPID != 1)
    if proc.ppid == 0 || (!is_pid_alive(proc.ppid, sys) && proc.ppid != 1) {
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

    let members = team
        .teammates()
        .into_iter()
        .map(|m| {
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
        })
        .collect();

    TeamHealthReport {
        team_name: team.name.clone(),
        owner_alive,
        members,
    }
}

/// Check if the team's owner process is still alive.
fn check_team_owner_alive(team: &TeamInfo, sys: &System) -> bool {
    let session_path = team
        .config_dir
        .join("teams")
        .join(&team.name)
        .join(".session");
    let alive = std::fs::read_to_string(&session_path)
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .and_then(|session| {
            session
                .get("owner_pid")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .and_then(|pid_str| pid_str.parse::<u32>().ok())
        .map(|pid| is_pid_alive(pid, sys));
    alive.unwrap_or(false)
}

/// Minimum CPU usage to consider a teammate still working,
/// even if its inbox is stale. Covers LLM API wait, network I/O, etc.
const TEAMMATE_ACTIVE_CPU_THRESHOLD: f32 = 0.5;

/// Check individual teammate health from inbox + task + CPU signals.
fn check_teammate_health(member: &TeamMember, team: &TeamInfo, now: u64) -> TeammateHealth {
    let inbox_age = get_inbox_age(team, &member.name, now);

    // Check if teammate has completed tasks
    let (completed_count, stuck_tasks) = check_teammate_tasks(team, &member.name);

    // If teammate has completed tasks and no stuck ones, it's done
    if completed_count > 0 && stuck_tasks.is_empty() {
        return TeammateHealth::Completed;
    }

    // CPU override: if process is using CPU, it's likely still working
    // (waiting for LLM response, processing, etc.) — don't mark as stuck/stale
    let is_cpu_active = member.cpu_percent > TEAMMATE_ACTIVE_CPU_THRESHOLD;

    // If stuck tasks exist and inbox is stale (but not actively using CPU)
    if !stuck_tasks.is_empty()
        && let Some(age) = inbox_age
        && age > TEAMMATE_STALE_SECS
        && !is_cpu_active
    {
        return TeammateHealth::Stuck {
            task_ids: stuck_tasks,
        };
    }

    // If inbox is stale (but not actively using CPU)
    if let Some(age) = inbox_age
        && age > TEAMMATE_STALE_SECS
        && !is_cpu_active
    {
        return TeammateHealth::Stale { idle_secs: age };
    }

    TeammateHealth::Active
}

/// Get seconds since last inbox modification for a teammate.
fn get_inbox_age(team: &TeamInfo, member_name: &str, now: u64) -> Option<u64> {
    let inbox_path = team
        .config_dir
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
            if path.extension().is_some_and(|e| e == "json")
                && path.file_name().is_some_and(|n| n != ".lock")
                && let Ok(content) = std::fs::read_to_string(&path)
                && let Ok(task) = serde_json::from_str::<serde_json::Value>(&content)
            {
                let owner = task.get("owner").and_then(|v| v.as_str()).unwrap_or("");
                let status = task.get("status").and_then(|v| v.as_str()).unwrap_or("");

                if owner == member_name {
                    match status {
                        "completed" => completed += 1,
                        "in_progress" => {
                            let id = task
                                .get("id")
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
    /// An idle shell pane inside an active claude-swarm server:
    /// claude process exited, shell remains with uptime > IDLE_SHELL_UPTIME_MIN_SECS.
    IdleShell {
        socket_name: String,
        pane_id: String,
        shell_pid: u32,
        uptime_secs: u64,
    },
    /// A stale tmux pane whose team was deleted or whose work is done.
    /// Claude process may still be alive but the pane is no longer useful.
    StalePane {
        socket_name: String,
        pane_id: String,
        shell_pid: u32,
        claude_pid: Option<u32>,
        agent_name: String,
        reason: StalePaneReason,
    },
}

/// Why a tmux pane is considered stale.
#[derive(Debug, Clone)]
pub enum StalePaneReason {
    /// Team dir deleted + teammate finished (DONE status).
    TeamDeletedDone,
    /// Team dir deleted + teammate idle (no work to do).
    TeamDeletedIdle,
    /// Team dir deleted + teammate still active (may be finishing up).
    TeamDeletedActive,
    /// Team exists but teammate finished and idle > threshold.
    DoneStale { uptime_secs: u64 },
}

impl StalePaneReason {
    pub fn label(&self) -> &'static str {
        match self {
            StalePaneReason::TeamDeletedDone => "team deleted, work done",
            StalePaneReason::TeamDeletedIdle => "team deleted, idle",
            StalePaneReason::TeamDeletedActive => "team deleted, still active",
            StalePaneReason::DoneStale { .. } => "work done, pane lingering",
        }
    }

    /// Whether this is safe to auto-kill without confirmation.
    pub fn is_safe_to_kill(&self) -> bool {
        matches!(
            self,
            StalePaneReason::TeamDeletedDone
                | StalePaneReason::TeamDeletedIdle
                | StalePaneReason::DoneStale { .. }
        )
    }
}

impl ZombieEntry {
    /// Short description for display.
    pub fn label(&self) -> String {
        match self {
            ZombieEntry::Team {
                name,
                member_count,
                task_count,
                ..
            } => {
                format!(
                    "ZOMBIE TEAM: {} ({} members, {} tasks)",
                    name, member_count, task_count
                )
            }
            ZombieEntry::OrphanTmux {
                socket_name,
                lead_pid,
                pane_count,
                ..
            } => {
                format!(
                    "ORPHAN TMUX: {} (lead:{}, {} panes)",
                    socket_name, lead_pid, pane_count
                )
            }
            ZombieEntry::OrphanShell {
                socket_name,
                pane_id,
                shell_pid,
            } => {
                format!(
                    "ORPHAN SHELL: pane {} (sh:{}) in {}",
                    pane_id, shell_pid, socket_name
                )
            }
            ZombieEntry::IdleShell {
                socket_name,
                pane_id,
                shell_pid,
                uptime_secs,
            } => {
                format!(
                    "IDLE SHELL: pane {} (sh:{}, {}m up) in {}",
                    pane_id,
                    shell_pid,
                    uptime_secs / 60,
                    socket_name
                )
            }
            ZombieEntry::StalePane {
                socket_name,
                pane_id,
                agent_name,
                reason,
                ..
            } => {
                format!(
                    "STALE PANE: {} pane {} ({}) in {}",
                    agent_name,
                    pane_id,
                    reason.label(),
                    socket_name
                )
            }
        }
    }

    /// Reason why this is considered a zombie.
    pub fn reason(&self) -> &'static str {
        match self {
            ZombieEntry::Team { .. } => "owner process is dead",
            ZombieEntry::OrphanTmux { .. } => "lead process is dead",
            ZombieEntry::OrphanShell { .. } => "claude process exited, empty shell remains",
            ZombieEntry::IdleShell { .. } => "claude process exited, shell idle too long",
            ZombieEntry::StalePane { reason, .. } => reason.label(),
        }
    }
}

/// Minimum uptime (in seconds) for an idle shell to be considered for cleanup.
/// Shells alive for less than this are likely still initializing.
const IDLE_SHELL_UPTIME_MIN_SECS: u64 = 8 * 60; // 8 minutes

/// Scan for zombie teams and orphan tmux servers without killing anything.
pub fn scan_zombies() -> Vec<ZombieEntry> {
    let sys = create_process_system();
    scan_zombies_with(&sys)
}

/// Scan for zombies using an existing System (no new allocation).
pub fn scan_zombies_with(sys: &System) -> Vec<ZombieEntry> {
    use crate::teams::{scan_teams, scan_tmux_servers};

    let teams = scan_teams();
    let mut entries = Vec::new();

    for team in &teams {
        let report = check_team_health(team, sys);
        if !report.owner_alive {
            entries.push(ZombieEntry::Team {
                name: team.name.clone(),
                config_dir: team.config_dir.clone(),
                member_count: team.members.len(),
                task_count: team.task_count,
            });
        }
    }

    let tmux_servers = scan_tmux_servers(sys, true, 0);
    let now = now_epoch();
    for srv in &tmux_servers {
        if srv.is_orphan() {
            entries.push(ZombieEntry::OrphanTmux {
                socket_name: srv.socket_name.clone(),
                lead_pid: srv.lead_pid,
                pane_count: srv.panes.len(),
                server_pid: srv.server_pid,
            });
        } else {
            // Active server — check for orphan, idle, and stale panes
            for pane in &srv.panes {
                let uptime = if pane.start_time > 0 {
                    now.saturating_sub(pane.start_time)
                } else {
                    0
                };

                if !pane.claude_alive {
                    // Claude process exited — check for idle/orphan shells
                    if uptime >= IDLE_SHELL_UPTIME_MIN_SECS {
                        entries.push(ZombieEntry::IdleShell {
                            socket_name: srv.socket_name.clone(),
                            pane_id: pane.pane_id.clone(),
                            shell_pid: pane.shell_pid,
                            uptime_secs: uptime,
                        });
                    } else if pane.agent_name.is_none() {
                        entries.push(ZombieEntry::OrphanShell {
                            socket_name: srv.socket_name.clone(),
                            pane_id: pane.pane_id.clone(),
                            shell_pid: pane.shell_pid,
                        });
                    }
                }

                // Stale pane detection: team deleted or work done but pane lingers
                if let Some(agent) = &pane.agent_name {
                    let reason = if !pane.team_exists {
                        // Team config dir was deleted.
                        // If lead is still alive AND agent process is alive,
                        // this is normal: orchestrator cleaned config while
                        // agents are still running/finishing. Not stale.
                        if srv.lead_alive && pane.claude_alive {
                            None
                        } else {
                            // Lead or agent is dead → truly stale
                            match pane.status {
                                PaneStatus::Done => Some(StalePaneReason::TeamDeletedDone),
                                PaneStatus::Idle if !pane.claude_alive => {
                                    Some(StalePaneReason::TeamDeletedIdle)
                                }
                                PaneStatus::Idle => None, // agent alive but idle — not stale yet
                                PaneStatus::Active => None, // agent alive and active — definitely not stale
                                PaneStatus::Shell => None,  // already caught above
                            }
                        }
                    } else if pane.status == PaneStatus::Done
                        && uptime >= IDLE_SHELL_UPTIME_MIN_SECS
                    {
                        // Team exists but teammate finished and pane lingers
                        Some(StalePaneReason::DoneStale {
                            uptime_secs: uptime,
                        })
                    } else {
                        None
                    };

                    if let Some(reason) = reason {
                        entries.push(ZombieEntry::StalePane {
                            socket_name: srv.socket_name.clone(),
                            pane_id: pane.pane_id.clone(),
                            shell_pid: pane.shell_pid,
                            claude_pid: pane.claude_pid,
                            agent_name: agent.clone(),
                            reason,
                        });
                    }
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
    /// Number of idle shell panes killed.
    pub idle_shells_cleaned: usize,
    /// Number of stale teammate panes killed (team deleted or done+lingering).
    pub stale_panes_cleaned: usize,
    /// Error messages from failed operations.
    pub errors: Vec<String>,
}

impl KillZombiesResult {
    pub fn total(&self) -> usize {
        self.teams_cleaned
            + self.tmux_cleaned
            + self.shells_cleaned
            + self.idle_shells_cleaned
            + self.stale_panes_cleaned
    }
}

/// Kill all zombie teams, orphan tmux servers, and orphan/idle shell panes.
///
/// For zombie teams: kills tmux teammates, removes team/task directories.
/// For orphan tmux servers: kills the tmux server process.
/// For orphan/idle shells: kills the empty tmux pane.
/// Kill all zombies regardless of uptime (used by CLI `--kill-zombies` and TUI `k` key).
pub fn kill_zombies() -> KillZombiesResult {
    let sys = create_process_system();
    kill_zombies_filtered(&sys, 0)
}

/// Kill zombies using an existing System (no new allocation).
/// Used by CLI `--kill-zombies` and TUI `k` key — no uptime filter.
pub fn kill_zombies_with(sys: &System) -> KillZombiesResult {
    kill_zombies_filtered(sys, 0)
}

/// Kill zombies that have been alive longer than `min_uptime_secs`.
/// Used by auto-cleanup to avoid killing recently-started processes.
pub fn kill_zombies_auto(sys: &System, min_uptime_secs: u64) -> KillZombiesResult {
    kill_zombies_filtered(sys, min_uptime_secs)
}

/// Internal: kill zombies, optionally filtering by minimum uptime.
fn kill_zombies_filtered(sys: &System, min_uptime_secs: u64) -> KillZombiesResult {
    use crate::teams::{kill_tmux_server, scan_teams, scan_tmux_servers};

    let teams = scan_teams();
    let now = now_epoch();
    let mut result = KillZombiesResult::default();

    for team in &teams {
        let report = check_team_health(team, sys);
        if !report.owner_alive {
            // Skip young teams if uptime filter is active
            if min_uptime_secs > 0 {
                let oldest_start = team
                    .members
                    .iter()
                    .filter(|m| m.start_time > 0)
                    .map(|m| m.start_time)
                    .min()
                    .unwrap_or(now);
                if now.saturating_sub(oldest_start) < min_uptime_secs {
                    continue;
                }
            }
            // Kill tmux teammates
            for member in &team.members {
                if member.name == "team-lead" {
                    continue;
                }
                if !member.tmux_pane_id.is_empty() {
                    let pane_id = &member.tmux_pane_id;
                    let digits = &pane_id[1..];
                    if !pane_id.starts_with('%')
                        || digits.is_empty()
                        || !digits.chars().all(|c| c.is_ascii_digit())
                    {
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
                            let canonical_str = canonical.to_string_lossy();
                            // Validate path is within .claude directory
                            if !canonical_str.contains("/.claude") {
                                result.errors.push(format!(
                                    "rejected path outside .claude: {}",
                                    canonical.display()
                                ));
                                continue;
                            }
                            // TOCTOU mitigation: verify it's still a directory (not symlink swapped in)
                            match std::fs::symlink_metadata(&canonical) {
                                Ok(meta) if meta.is_dir() => {
                                    if let Err(e) = std::fs::remove_dir_all(&canonical) {
                                        result.errors.push(format!(
                                            "rm {}: {}",
                                            canonical.display(),
                                            e
                                        ));
                                    }
                                }
                                Ok(meta) => {
                                    result.errors.push(format!(
                                        "rejected non-directory: {:?} at {}",
                                        meta.file_type(),
                                        canonical.display()
                                    ));
                                }
                                Err(e) => {
                                    result.errors.push(format!(
                                        "metadata check failed for {}: {}",
                                        canonical.display(),
                                        e
                                    ));
                                }
                            }
                        }
                        Err(e) => {
                            result
                                .errors
                                .push(format!("canonicalize {}: {}", dir.display(), e));
                        }
                    }
                }
            }

            result.teams_cleaned += 1;
        }
    }

    // Kill orphan tmux servers and orphan/idle shell panes
    let tmux_servers = scan_tmux_servers(sys, true, 0);
    for srv in &tmux_servers {
        if srv.is_orphan() {
            // Skip young orphan servers if uptime filter is active
            if min_uptime_secs > 0
                && srv.start_time > 0
                && now.saturating_sub(srv.start_time) < min_uptime_secs
            {
                continue;
            }
            if kill_tmux_server(&srv.socket_name) {
                result.tmux_cleaned += 1;
            } else {
                result
                    .errors
                    .push(format!("failed to kill tmux server {}", srv.socket_name));
            }
        } else {
            // Active server — kill orphan and idle shell panes
            for pane in &srv.panes {
                // Skip young panes if uptime filter is active
                let pane_uptime = if pane.start_time > 0 {
                    now.saturating_sub(pane.start_time)
                } else {
                    0
                };
                if min_uptime_secs > 0 && pane_uptime < min_uptime_secs {
                    continue;
                }

                if !pane.claude_alive {
                    let uptime = if pane.start_time > 0 {
                        now.saturating_sub(pane.start_time)
                    } else {
                        0
                    };

                    let is_idle = uptime >= IDLE_SHELL_UPTIME_MIN_SECS && pane.start_time > 0;
                    let is_orphan = pane.agent_name.is_none() && !is_idle;

                    if is_idle || is_orphan {
                        // Guard before slicing to prevent panic on empty pane_id
                        if pane.pane_id.starts_with('%') && pane.pane_id.len() > 1 {
                            let digits = &pane.pane_id[1..];
                            if digits.chars().all(|c| c.is_ascii_digit()) {
                                let kill_result = std::process::Command::new("tmux")
                                    .args([
                                        "-L",
                                        &srv.socket_name,
                                        "kill-pane",
                                        "-t",
                                        &pane.pane_id,
                                    ])
                                    .output();
                                if kill_result.is_ok_and(|o| o.status.success()) {
                                    if is_idle {
                                        result.idle_shells_cleaned += 1;
                                    } else {
                                        result.shells_cleaned += 1;
                                    }
                                } else {
                                    result.errors.push(format!(
                                        "failed to kill {} pane {} in {}",
                                        if is_idle { "idle" } else { "orphan" },
                                        pane.pane_id,
                                        srv.socket_name
                                    ));
                                }
                            } else {
                                result.errors.push(format!(
                                    "skipped {} pane with invalid id {:?} in {}",
                                    if is_idle { "idle" } else { "orphan" },
                                    pane.pane_id,
                                    srv.socket_name
                                ));
                            }
                        } else {
                            result.errors.push(format!(
                                "skipped {} pane with invalid id {:?} in {}",
                                if is_idle { "idle" } else { "orphan" },
                                pane.pane_id,
                                srv.socket_name
                            ));
                        }
                    }
                }

                // Stale pane: team deleted or done+lingering — kill if safe
                if let Some(agent) = &pane.agent_name {
                    let uptime = if pane.start_time > 0 {
                        now.saturating_sub(pane.start_time)
                    } else {
                        0
                    };
                    let stale_reason = if !pane.team_exists {
                        match pane.status {
                            PaneStatus::Done | PaneStatus::Idle => true,
                            PaneStatus::Active => false, // still active, skip auto-kill
                            PaneStatus::Shell => false,  // already handled above
                        }
                    } else {
                        pane.status == PaneStatus::Done && uptime >= IDLE_SHELL_UPTIME_MIN_SECS
                    };

                    if stale_reason {
                        if kill_tmux_pane(&srv.socket_name, &pane.pane_id) {
                            result.stale_panes_cleaned += 1;
                        } else {
                            result.errors.push(format!(
                                "failed to kill stale pane {} ({}) in {}",
                                pane.pane_id, agent, srv.socket_name
                            ));
                        }
                    }
                }
            }
        }
    }

    result
}

/// Kill a tmux pane by socket name and pane ID. Returns true on success.
fn kill_tmux_pane(socket_name: &str, pane_id: &str) -> bool {
    // Validate socket_name format to prevent command injection
    if !socket_name.starts_with("claude-swarm-") {
        return false;
    }
    // Validate pane_id format
    if !pane_id.starts_with('%') || pane_id.len() <= 1 {
        return false;
    }
    let digits = &pane_id[1..];
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    std::process::Command::new("tmux")
        .args(["-L", socket_name, "kill-pane", "-t", pane_id])
        .output()
        .is_ok_and(|o| o.status.success())
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
    Process { name: String, cmd_preview: String },
    /// PID not found.
    NotFound,
}

/// Look up a PID to get info about what it is. Does NOT kill anything.
pub fn lookup_process(pid: u32) -> ProcessLookup {
    use crate::teams::scan_tmux_servers;

    let sys = create_process_system();
    let tmux_servers = scan_tmux_servers(&sys, true, 0);

    // Check if PID matches a tmux pane
    for srv in &tmux_servers {
        for pane in &srv.panes {
            if pane.shell_pid == pid || pane.claude_pid == Some(pid) {
                let label = pane.agent_name.as_deref().unwrap_or("shell").to_string();
                return ProcessLookup {
                    pid,
                    label: format!(
                        "{} (tmux pane {} in {})",
                        label, pane.pane_id, srv.socket_name
                    ),
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
            let cmd_str: String = proc_
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(" ");

            let is_claude = cmd_str.contains("claude")
                || cmd_str.contains("--agent-id")
                || name.contains("claude");

            let agent_name = cmd_str
                .split("--agent-name ")
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
            ProcessLookupKind::Process { name, .. } => {
                Err(format!("PID {} ({}) is not a Claude process", pid, name))
            }
            _ => Err(format!("PID {} is not a Claude process", pid)),
        };
    }

    match lookup.kind {
        ProcessLookupKind::TmuxPane {
            socket_name,
            pane_id,
            agent_name,
        } => {
            let label = agent_name.as_deref().unwrap_or("shell");
            // Kill the tmux pane - validate pane_id format before slicing
            if pane_id.starts_with('%')
                && pane_id.len() > 1
                && pane_id[1..].chars().all(|c| c.is_ascii_digit())
            {
                let result = std::process::Command::new("tmux")
                    .args(["-L", &socket_name, "kill-pane", "-t", &pane_id])
                    .output();
                if result.is_ok_and(|o| o.status.success()) {
                    return Ok(format!("Killed tmux pane {} ({})", pane_id, label));
                }
            }
            // Fallback: kill the process directly
            let sys = create_light_system();
            if let Some(proc_) = sys.process(Pid::from_u32(pid))
                && proc_.kill()
            {
                return Ok(format!("Killed PID {} ({})", pid, label));
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

// ── Auto-Cleanup Timer ───────────────────────────────────────────

/// Lightweight timer for periodic auto-cleanup. Zero allocation between runs.
/// Only cost per tick: one `Instant::elapsed()` comparison (~5ns).
pub struct AutoCleanup {
    enabled: bool,
    interval: Duration,
    last_run: Option<Instant>,
}

impl Default for AutoCleanup {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoCleanup {
    pub fn new() -> Self {
        Self {
            enabled: false,
            interval: Duration::from_secs(15 * 60), // 15 minutes
            last_run: None,
        }
    }

    /// Toggle auto-cleanup on/off. Returns the new state.
    pub fn toggle(&mut self) -> bool {
        self.enabled = !self.enabled;
        if self.enabled {
            // Set last_run to now so it doesn't fire immediately on toggle
            self.last_run = Some(Instant::now());
        }
        self.enabled
    }

    /// Update the cleanup interval duration.
    pub fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Check if it's time to run. Returns true at most once per interval.
    /// Cost: one `Instant::elapsed()` comparison (~5ns) — essentially free.
    pub fn should_run(&mut self) -> bool {
        if !self.enabled {
            return false;
        }
        if self.last_run.is_none_or(|t| t.elapsed() >= self.interval) {
            self.last_run = Some(Instant::now());
            true
        } else {
            false
        }
    }
}

/// Format a cleanup result for display in TUI status bar or CLI output.
pub fn format_cleanup_result(result: &KillZombiesResult) -> String {
    let mut parts = Vec::new();
    if result.teams_cleaned > 0 {
        parts.push(format!("{} team(s)", result.teams_cleaned));
    }
    if result.tmux_cleaned > 0 {
        parts.push(format!("{} tmux", result.tmux_cleaned));
    }
    if result.shells_cleaned > 0 {
        parts.push(format!("{} shell(s)", result.shells_cleaned));
    }
    if result.idle_shells_cleaned > 0 {
        parts.push(format!("{} idle shell(s)", result.idle_shells_cleaned));
    }
    if result.stale_panes_cleaned > 0 {
        parts.push(format!("{} stale pane(s)", result.stale_panes_cleaned));
    }
    if parts.is_empty() {
        "No zombies found".to_string()
    } else {
        let cleaned = format!("Cleaned: {}", parts.join(", "));
        if result.errors.is_empty() {
            cleaned
        } else {
            format!("{} ({} error(s))", cleaned, result.errors.len())
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn is_pid_alive(pid: u32, sys: &System) -> bool {
    sys.process(Pid::from_u32(pid)).is_some()
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
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

// ── Tests ─────────────────────────────────────────────────────────

// ── Kill Swarm ───────────────────────────────────────────────────────────────

/// Result of killing an entire swarm team.
#[derive(Debug, Clone, Default, Serialize)]
pub struct KillSwarmResult {
    pub team_name: String,
    pub killed_pids: Vec<u32>,
    pub sigkill_pids: Vec<u32>,
    pub already_dead: Vec<u32>,
    pub killed_tmux_server: bool,
    pub removed_config: bool,
    pub errors: Vec<String>,
}

impl KillSwarmResult {
    pub fn new(team_name: &str) -> Self {
        Self {
            team_name: team_name.to_string(),
            ..Default::default()
        }
    }
}

/// Check if `target_pid` is an ancestor of the current process.
/// Walks the parent chain via sysinfo.
pub fn is_ancestor_of_self(sys: &System, target_pid: u32) -> bool {
    let my_pid = std::process::id();
    let mut current = Pid::from_u32(my_pid);
    let target = Pid::from_u32(target_pid);

    loop {
        if current == target {
            return true;
        }
        match sys.process(current).and_then(|p| p.parent()) {
            Some(parent) if parent != current => current = parent,
            _ => return false,
        }
    }
}

/// Kill an entire claude-swarm team safely.
///
/// # Arguments
/// * `team_name` - Team directory name under .claude/teams/
/// * `sys` - Process system for PID lookups
/// * `force` - Override self-kill guard
///
/// # Safety
/// - SIGTERM before SIGKILL (2s grace period)
/// - Path validation before remove_dir_all
/// - Self-kill guard (unless --force)
pub fn kill_swarm(team_name: &str, sys: &System, force: bool) -> anyhow::Result<KillSwarmResult> {
    use crate::teams::{TmuxSnapshot, kill_tmux_server, scan_teams, scan_tmux_servers};
    use std::fs;
    use std::thread;
    use std::time::Duration;

    let mut result = KillSwarmResult::new(team_name);

    // 1. Find team in scan results
    let teams = scan_teams();
    let team = teams
        .iter()
        .find(|t| t.name == team_name)
        .ok_or_else(|| anyhow::anyhow!("Team '{}' not found", team_name))?;

    // 2. Get lead PID and check self-kill guard
    let config_dir: std::path::PathBuf = std::env::var("CLAUDE_CONFIG_DIR")
        .map(|s| Path::new(&s).to_path_buf())
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".claude"))
                .unwrap_or_else(|| Path::new("/tmp/.claude").to_path_buf())
        });

    let team_dir = config_dir.join("teams").join(team_name);
    if !team_dir.starts_with(&config_dir) || team_dir == config_dir {
        anyhow::bail!("Invalid team directory path");
    }

    // 3. Self-kill guard
    if !force {
        // Find lead PID from team config
        let config_path = team_dir.join("config.json");
        if let Ok(config_content) = fs::read_to_string(&config_path)
            && let Ok(config) = serde_json::from_str::<serde_json::Value>(&config_content)
            && let Some(lead_pid) = config.get("lead_pid").and_then(|p| p.as_u64())
            && is_ancestor_of_self(sys, lead_pid as u32)
        {
            anyhow::bail!(
                "Refusing to kill own session (lead_pid={} is ancestor). Use --force to override.",
                lead_pid
            );
        }
    }

    // 4. Collect PIDs from team members
    let mut pids: Vec<u32> = Vec::new();
    for member in &team.members {
        if let Some(pid) = member.tmux_pid {
            pids.push(pid);
        }
        // Also try to find from tmux panes
    }

    // 5. Find tmux socket for this team
    let _snapshot = TmuxSnapshot::new();
    let tmux_servers = scan_tmux_servers(sys, true, 0);

    let socket_name = tmux_servers.iter().find_map(|srv| {
        let has_team_pane = srv
            .panes
            .iter()
            .any(|p| p.team_name.as_deref() == Some(team_name));
        if has_team_pane {
            Some(srv.socket_name.clone())
        } else {
            None
        }
    });

    // 6. Collect PIDs from tmux panes
    if let Some(ref socket) = socket_name {
        for srv in &tmux_servers {
            if &srv.socket_name == socket {
                for pane in &srv.panes {
                    if pane.team_name.as_deref() == Some(team_name) {
                        pids.push(pane.shell_pid);
                        if let Some(claude_pid) = pane.claude_pid {
                            pids.push(claude_pid);
                        }
                    }
                }
            }
        }
    }

    pids.sort();
    pids.dedup();

    // 7. SIGTERM all PIDs
    for pid in &pids {
        match send_signal(*pid, Signal::Term) {
            Ok(()) => result.killed_pids.push(*pid),
            Err(e) if e.contains("not found") || e.contains("ESRCH") => {
                result.already_dead.push(*pid);
            }
            Err(e) => result.errors.push(format!("SIGTERM {}: {}", pid, e)),
        }
    }

    // 8. Grace period
    thread::sleep(Duration::from_secs(2));

    // 9. SIGKILL survivors
    for pid in &pids {
        if sys.process(Pid::from_u32(*pid)).is_some()
            && let Ok(()) = send_signal(*pid, Signal::Kill)
        {
            result.sigkill_pids.push(*pid);
        }
    }

    // 10. Kill tmux server
    if let Some(ref socket) = socket_name {
        result.killed_tmux_server = kill_tmux_server(socket);
    }

    // 11. Remove team config directory
    match fs::remove_dir_all(&team_dir) {
        Ok(()) => result.removed_config = true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            result.removed_config = true; // Idempotent
        }
        Err(e) => result.errors.push(format!("rm team config: {}", e)),
    }

    Ok(result)
}

/// Signal type for process termination.
enum Signal {
    Term,
    Kill,
}

/// Send a signal to a process.
fn send_signal(pid: u32, signal: Signal) -> Result<(), String> {
    use std::process::Command;

    // Use numeric signal values for robustness (15=SIGTERM, 9=SIGKILL)
    let signal_num: i32 = match signal {
        Signal::Term => 15,
        Signal::Kill => 9,
    };

    let output = Command::new("kill")
        .arg(format!("-{}", signal_num))
        .arg(pid.to_string())
        .output();

    match output {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if stderr.contains("No such process") || stderr.contains("ESRCH") {
                Err("process not found".to_string())
            } else {
                Err(stderr.trim().to_string())
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Health enum tests ─────────────────────────────────────────

    #[test]
    fn test_health_label() {
        assert_eq!(Health::Ok.label(), "OK");
        assert_eq!(Health::Zombie.label(), "ZOMBIE");
        assert_eq!(Health::Orphan.label(), "ORPHAN");
        assert_eq!(Health::Stale.label(), "STALE");
    }

    #[test]
    fn test_health_is_healthy() {
        assert!(Health::Ok.is_healthy());
        assert!(!Health::Zombie.is_healthy());
        assert!(!Health::Orphan.is_healthy());
        assert!(!Health::Stale.is_healthy());
    }

    #[test]
    fn test_health_display() {
        assert_eq!(format!("{}", Health::Ok), "OK");
        assert_eq!(format!("{}", Health::Zombie), "ZOMBIE");
        assert_eq!(format!("{}", Health::Orphan), "ORPHAN");
        assert_eq!(format!("{}", Health::Stale), "STALE");
    }

    // ── TeammateHealth enum tests ─────────────────────────────────

    #[test]
    fn test_teammate_health_label() {
        assert_eq!(TeammateHealth::Active.label(), "ACTIVE");
        assert_eq!(TeammateHealth::Completed.label(), "DONE");
        assert_eq!(TeammateHealth::Zombie.label(), "ZOMBIE");
        assert_eq!(TeammateHealth::Stale { idle_secs: 100 }.label(), "STALE");
        assert_eq!(
            TeammateHealth::Stuck {
                task_ids: vec!["1".to_string()]
            }
            .label(),
            "STUCK"
        );
    }

    #[test]
    fn test_teammate_health_is_healthy() {
        assert!(TeammateHealth::Active.is_healthy());
        assert!(TeammateHealth::Completed.is_healthy());
        assert!(!TeammateHealth::Zombie.is_healthy());
        assert!(!TeammateHealth::Stale { idle_secs: 100 }.is_healthy());
        assert!(
            !TeammateHealth::Stuck {
                task_ids: vec!["1".to_string()]
            }
            .is_healthy()
        );
    }

    #[test]
    fn test_teammate_health_display_stale() {
        let stale = TeammateHealth::Stale { idle_secs: 300 };
        assert_eq!(format!("{}", stale), "STALE (5m idle)");

        let stale_zero = TeammateHealth::Stale { idle_secs: 0 };
        assert_eq!(format!("{}", stale_zero), "STALE (0m idle)");

        let stale_large = TeammateHealth::Stale { idle_secs: 7200 };
        assert_eq!(format!("{}", stale_large), "STALE (120m idle)");
    }

    #[test]
    fn test_teammate_health_display_stuck() {
        let stuck_single = TeammateHealth::Stuck {
            task_ids: vec!["42".to_string()],
        };
        assert_eq!(format!("{}", stuck_single), "STUCK (tasks: 42)");

        let stuck_multiple = TeammateHealth::Stuck {
            task_ids: vec!["1".to_string(), "2".to_string(), "3".to_string()],
        };
        assert_eq!(format!("{}", stuck_multiple), "STUCK (tasks: 1,2,3)");

        let stuck_empty = TeammateHealth::Stuck { task_ids: vec![] };
        assert_eq!(format!("{}", stuck_empty), "STUCK (tasks: )");
    }

    #[test]
    fn test_teammate_health_display_other_variants() {
        assert_eq!(format!("{}", TeammateHealth::Active), "ACTIVE");
        assert_eq!(format!("{}", TeammateHealth::Completed), "DONE");
        assert_eq!(format!("{}", TeammateHealth::Zombie), "ZOMBIE");
    }

    // ── ZombieEntry tests ──────────────────────────────────────────

    #[test]
    fn test_zombie_entry_label_team() {
        let team = ZombieEntry::Team {
            name: "my-team".to_string(),
            config_dir: std::path::PathBuf::from("/tmp/.claude"),
            member_count: 3,
            task_count: 5,
        };
        assert_eq!(team.label(), "ZOMBIE TEAM: my-team (3 members, 5 tasks)");
    }

    #[test]
    fn test_zombie_entry_label_orphan_tmux() {
        let orphan_tmux = ZombieEntry::OrphanTmux {
            socket_name: "claude-swarm".to_string(),
            lead_pid: 12345,
            pane_count: 4,
            server_pid: Some(67890),
        };
        assert_eq!(
            orphan_tmux.label(),
            "ORPHAN TMUX: claude-swarm (lead:12345, 4 panes)"
        );

        let orphan_tmux_no_server = ZombieEntry::OrphanTmux {
            socket_name: "test-socket".to_string(),
            lead_pid: 999,
            pane_count: 1,
            server_pid: None,
        };
        assert_eq!(
            orphan_tmux_no_server.label(),
            "ORPHAN TMUX: test-socket (lead:999, 1 panes)"
        );
    }

    #[test]
    fn test_zombie_entry_label_orphan_shell() {
        let orphan_shell = ZombieEntry::OrphanShell {
            socket_name: "claude-swarm".to_string(),
            pane_id: "%0".to_string(),
            shell_pid: 54321,
        };
        assert_eq!(
            orphan_shell.label(),
            "ORPHAN SHELL: pane %0 (sh:54321) in claude-swarm"
        );
    }

    #[test]
    fn test_zombie_entry_reason() {
        let team = ZombieEntry::Team {
            name: "test".to_string(),
            config_dir: std::path::PathBuf::from("/tmp"),
            member_count: 1,
            task_count: 0,
        };
        assert_eq!(team.reason(), "owner process is dead");

        let orphan_tmux = ZombieEntry::OrphanTmux {
            socket_name: "test".to_string(),
            lead_pid: 1,
            pane_count: 1,
            server_pid: None,
        };
        assert_eq!(orphan_tmux.reason(), "lead process is dead");

        let orphan_shell = ZombieEntry::OrphanShell {
            socket_name: "test".to_string(),
            pane_id: "%0".to_string(),
            shell_pid: 1,
        };
        assert_eq!(
            orphan_shell.reason(),
            "claude process exited, empty shell remains"
        );
    }

    // ── KillZombiesResult tests ─────────────────────────────────────

    #[test]
    fn test_kill_zombies_result_total() {
        // All zeros
        let result = KillZombiesResult::default();
        assert_eq!(result.total(), 0);

        // Only teams cleaned
        let result = KillZombiesResult {
            teams_cleaned: 2,
            tmux_cleaned: 0,
            shells_cleaned: 0,
            idle_shells_cleaned: 0,
            stale_panes_cleaned: 0,
            errors: vec![],
        };
        assert_eq!(result.total(), 2);

        // Only tmux cleaned
        let result = KillZombiesResult {
            teams_cleaned: 0,
            tmux_cleaned: 3,
            shells_cleaned: 0,
            idle_shells_cleaned: 0,
            stale_panes_cleaned: 0,
            errors: vec![],
        };
        assert_eq!(result.total(), 3);

        // Only shells cleaned
        let result = KillZombiesResult {
            teams_cleaned: 0,
            tmux_cleaned: 0,
            shells_cleaned: 5,
            idle_shells_cleaned: 0,
            stale_panes_cleaned: 0,
            errors: vec![],
        };
        assert_eq!(result.total(), 5);

        // Only idle shells cleaned
        let result = KillZombiesResult {
            teams_cleaned: 0,
            tmux_cleaned: 0,
            shells_cleaned: 0,
            idle_shells_cleaned: 4,
            stale_panes_cleaned: 0,
            errors: vec![],
        };
        assert_eq!(result.total(), 4);

        // Mixed counts
        let result = KillZombiesResult {
            teams_cleaned: 2,
            tmux_cleaned: 3,
            shells_cleaned: 5,
            idle_shells_cleaned: 1,
            stale_panes_cleaned: 0,
            errors: vec!["some error".to_string()],
        };
        assert_eq!(result.total(), 11);
    }

    // ── IdleShell variant tests ─────────────────────────────────────

    #[test]
    fn test_idle_shell_label() {
        let idle = ZombieEntry::IdleShell {
            socket_name: "claude-swarm-123".to_string(),
            pane_id: "%5".to_string(),
            shell_pid: 99999,
            uptime_secs: 600,
        };
        assert_eq!(
            idle.label(),
            "IDLE SHELL: pane %5 (sh:99999, 10m up) in claude-swarm-123"
        );
    }

    #[test]
    fn test_idle_shell_reason() {
        let idle = ZombieEntry::IdleShell {
            socket_name: "test".to_string(),
            pane_id: "%0".to_string(),
            shell_pid: 1,
            uptime_secs: 500,
        };
        assert_eq!(idle.reason(), "claude process exited, shell idle too long");
    }

    // ── StalePane variant tests ────────────────────────────────────

    #[test]
    fn test_stale_pane_label_team_deleted_done() {
        let stale = ZombieEntry::StalePane {
            socket_name: "claude-swarm-123".to_string(),
            pane_id: "%5".to_string(),
            shell_pid: 99999,
            claude_pid: Some(88888),
            agent_name: "decree-arbiter".to_string(),
            reason: StalePaneReason::TeamDeletedDone,
        };
        assert!(stale.label().contains("decree-arbiter"));
        assert!(stale.label().contains("team deleted, work done"));
        assert_eq!(stale.reason(), "team deleted, work done");
    }

    #[test]
    fn test_stale_pane_reason_safe_to_kill() {
        assert!(StalePaneReason::TeamDeletedDone.is_safe_to_kill());
        assert!(StalePaneReason::TeamDeletedIdle.is_safe_to_kill());
        assert!(StalePaneReason::DoneStale { uptime_secs: 600 }.is_safe_to_kill());
        assert!(!StalePaneReason::TeamDeletedActive.is_safe_to_kill());
    }

    // ── AutoCleanup tests ────────────────────────────────────────────

    #[test]
    fn test_auto_cleanup_new_starts_disabled() {
        let ac = AutoCleanup::new();
        assert!(!ac.is_enabled());
    }

    #[test]
    fn test_auto_cleanup_toggle() {
        let mut ac = AutoCleanup::new();
        assert!(ac.toggle()); // now enabled
        assert!(ac.is_enabled());
        assert!(!ac.toggle()); // now disabled
        assert!(!ac.is_enabled());
    }

    #[test]
    fn test_auto_cleanup_should_run_when_disabled() {
        let mut ac = AutoCleanup::new();
        assert!(!ac.should_run());
    }

    #[test]
    fn test_auto_cleanup_should_run_not_before_interval() {
        let mut ac = AutoCleanup::new();
        ac.toggle(); // enable — sets last_run to now
        // Should not fire immediately after toggle
        assert!(!ac.should_run());
    }

    // ── format_cleanup_result tests ──────────────────────────────────

    #[test]
    fn test_format_cleanup_result_empty() {
        let result = KillZombiesResult::default();
        assert_eq!(format_cleanup_result(&result), "No zombies found");
    }

    #[test]
    fn test_format_cleanup_result_mixed() {
        let result = KillZombiesResult {
            teams_cleaned: 1,
            tmux_cleaned: 2,
            shells_cleaned: 0,
            idle_shells_cleaned: 3,
            stale_panes_cleaned: 0,
            errors: vec!["err".to_string()],
        };
        assert_eq!(
            format_cleanup_result(&result),
            "Cleaned: 1 team(s), 2 tmux, 3 idle shell(s) (1 error(s))"
        );
    }

    #[test]
    fn test_format_cleanup_result_teams_only() {
        let result = KillZombiesResult {
            teams_cleaned: 2,
            tmux_cleaned: 0,
            shells_cleaned: 0,
            idle_shells_cleaned: 0,
            stale_panes_cleaned: 0,
            errors: vec![],
        };
        assert_eq!(format_cleanup_result(&result), "Cleaned: 2 team(s)");
    }
}
