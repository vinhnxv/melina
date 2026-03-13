//! Process discovery — find all Claude Code related processes via sysinfo.

use serde::Serialize;
use std::path::PathBuf;
use sysinfo::{Pid, Process, ProcessesToUpdate, System};

/// Raw process info extracted from the OS.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub cmd: Vec<String>,
    pub cwd: PathBuf,
    pub memory_bytes: u64,
    pub cpu_percent: f32,
    pub start_time: u64,
    pub status: String,
}

impl ProcessInfo {
    fn from_process(pid: Pid, proc_: &Process) -> Self {
        Self {
            pid: pid.as_u32(),
            ppid: proc_.parent().map(|p| p.as_u32()).unwrap_or(0),
            name: proc_.name().to_string_lossy().to_string(),
            cmd: proc_
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect(),
            cwd: proc_.cwd().map(|p| p.to_path_buf()).unwrap_or_default(),
            memory_bytes: proc_.memory(),
            cpu_percent: proc_.cpu_usage(),
            start_time: proc_.start_time(),
            status: format!("{:?}", proc_.status()),
        }
    }

    /// Check if this process is a Claude Code root session.
    pub fn is_claude_session(&self) -> bool {
        let name = self.name.to_lowercase();
        if !name.contains("claude") && !name.contains("node") {
            return false;
        }
        // Root sessions: `claude` binary or node running claude
        let cmd_str = self.cmd.join(" ").to_lowercase();
        (cmd_str.contains("claude") && !cmd_str.contains("server.py"))
            && self.cmd.first().is_some_and(|c| {
                let c_lower = c.to_lowercase();
                c_lower.contains("claude") || c_lower.contains("node")
            })
    }

    /// Check if this is a Claude-related child (MCP server, hook, etc.).
    pub fn is_claude_related(&self) -> bool {
        let cmd_str = self.cmd.join(" ");
        cmd_str.contains("claude") || cmd_str.contains(".claude/")
    }
}

/// Create a `System` that loads process info with accurate CPU values.
///
/// macOS requires 3 refreshes for `cpu_usage()` to return non-zero values:
/// 1. `new_all()` — initializes global CPU state + first process snapshot
/// 2. `refresh_all()` — sets the CPU time baseline
/// 3. sleep + `refresh_processes()` — calculates CPU delta
///
/// After initial creation, only `refresh_processes()` is needed for updates
/// (no disks/networks/components overhead on subsequent calls).
pub fn create_process_system() -> System {
    let mut sys = System::new_all();
    sys.refresh_all();
    std::thread::sleep(std::time::Duration::from_millis(200));
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys
}

/// Lightweight refresh for an existing System — no allocations, no sleep.
/// Call this for subsequent ticks after `create_process_system()`.
pub fn refresh_process_system(sys: &mut System) {
    sys.refresh_processes(ProcessesToUpdate::All, true);
}

/// Scan all processes and return Claude-related ones.
/// Uses a pre-created `System` to avoid redundant allocations.
pub fn scan(sys: &System) -> Vec<ProcessInfo> {
    sys.processes()
        .iter()
        .filter_map(|(&pid, proc_)| {
            let info = ProcessInfo::from_process(pid, proc_);
            if info.is_claude_session() || info.is_claude_related() {
                Some(info)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_process_info(pid: u32, ppid: u32, name: &str, cmd: Vec<&str>) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid,
            name: name.to_string(),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            cwd: std::path::PathBuf::new(),
            memory_bytes: 0,
            cpu_percent: 0.0,
            start_time: 0,
            status: "Run".to_string(),
        }
    }

    // Tests for ProcessInfo::is_claude_session()

    #[test]
    fn test_is_claude_session_binary() {
        let info = make_process_info(1234, 1, "claude", vec!["claude"]);
        assert!(info.is_claude_session());
    }

    #[test]
    fn test_is_claude_session_node() {
        let info = make_process_info(1234, 1, "node", vec!["node", "claude"]);
        assert!(info.is_claude_session());
    }

    #[test]
    fn test_is_claude_session_mcp_server() {
        let info = make_process_info(1234, 1, "node", vec!["node", "server.py"]);
        assert!(!info.is_claude_session());
    }

    #[test]
    fn test_is_claude_session_unrelated() {
        let info = make_process_info(1234, 1, "bash", vec!["bash", "-l"]);
        assert!(!info.is_claude_session());
    }

    #[test]
    fn test_is_claude_session_case_insensitive() {
        let info = make_process_info(1234, 1, "Claude", vec!["Claude"]);
        assert!(info.is_claude_session());
    }

    // Tests for ProcessInfo::is_claude_related()

    #[test]
    fn test_is_claude_related_in_cmd() {
        let info = make_process_info(1234, 1, "node", vec!["node", "claude", "some-arg"]);
        assert!(info.is_claude_related());
    }

    #[test]
    fn test_is_claude_related_config_dir() {
        let info = make_process_info(1234, 1, "node", vec!["node", "/home/user/.claude/config"]);
        assert!(info.is_claude_related());
    }

    #[test]
    fn test_is_claude_related_unrelated() {
        let info = make_process_info(1234, 1, "bash", vec!["bash", "-l"]);
        assert!(!info.is_claude_related());
    }
}
