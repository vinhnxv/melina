//! Process discovery — find all Claude Code related processes via sysinfo.

use serde::Serialize;
use sysinfo::{System, Pid, Process};
use std::path::PathBuf;

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
            cmd: proc_.cmd().iter().map(|s| s.to_string_lossy().to_string()).collect(),
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
        let cmd_str = self.cmd.join(" ");
        (cmd_str.contains("claude") && !cmd_str.contains("server.py"))
            && self.cmd.first().is_some_and(|c| {
                c.contains("claude") || c.contains("node")
            })
    }

    /// Check if this is a Claude-related child (MCP server, hook, etc.).
    pub fn is_claude_related(&self) -> bool {
        let cmd_str = self.cmd.join(" ");
        cmd_str.contains("claude") || cmd_str.contains(".claude/")
    }
}

/// Scan all processes and return Claude-related ones.
/// Refreshes twice with a short pause so `cpu_usage()` returns real values.
pub fn scan() -> Vec<ProcessInfo> {
    let mut sys = System::new_all();
    sys.refresh_all();
    std::thread::sleep(std::time::Duration::from_millis(200));
    sys.refresh_all();

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
