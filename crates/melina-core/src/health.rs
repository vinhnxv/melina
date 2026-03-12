//! Process and teammate health assessment.

use crate::ProcessInfo;
use crate::teams::{TeamInfo, TeamMember};
use serde::Serialize;
use sysinfo::{System, Pid};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

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
            assess_teammate(m, team, now)
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

/// Assess individual teammate health from inbox + task signals.
fn assess_teammate(member: &TeamMember, team: &TeamInfo, now: u64) -> TeammateHealth {
    let inbox_age = get_inbox_age(team, &member.name, now);

    // Check if teammate has completed tasks
    let (completed_count, stuck_tasks) = check_teammate_tasks(team, &member.name);

    // If teammate has completed tasks and no stuck ones, it's done
    if completed_count > 0 && stuck_tasks.is_empty() {
        // But check if inbox is very stale too (finished and idle)
        if let Some(age) = inbox_age {
            if age > TEAMMATE_STALE_SECS {
                return TeammateHealth::Completed;
            }
        }
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

// ── Helpers ──────────────────────────────────────────────────────

fn is_pid_alive(pid: u32, sys: &System) -> bool {
    sys.process(Pid::from_u32(pid)).is_some()
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
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
