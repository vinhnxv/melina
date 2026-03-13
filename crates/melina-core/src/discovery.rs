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
    ///
    /// On macOS, `claude` is often a symlink to a versioned binary like
    /// `.local/share/claude/versions/2.1.75`. Since sysinfo resolves symlinks
    /// via `proc_pidpath()`, `proc.name()` may return `"2.1.75"` instead of
    /// `"claude"`. We therefore rely on cmd args (argv), not the process name.
    pub fn is_claude_session(&self) -> bool {
        let cmd_str = self.cmd.join(" ").to_lowercase();

        // Must reference "claude" somewhere in the command line
        if !cmd_str.contains("claude") {
            return false;
        }

        // Exclude MCP servers (python scripts)
        if cmd_str.contains("server.py") {
            return false;
        }

        // Exclude teammate/agent processes (spawned with --agent-id)
        if cmd_str.contains("--agent-id") {
            return false;
        }

        // First arg must be a claude binary (direct name, node, or versioned path)
        self.cmd.first().is_some_and(|c| {
            let c_lower = c.to_lowercase();
            c_lower.contains("claude") || c_lower.contains("node")
                || Self::is_claude_versioned_binary(&c_lower)
        })
    }

    /// Check if a binary path looks like a Claude versioned binary.
    /// e.g. `/Users/x/.local/share/claude/versions/2.1.75`
    fn is_claude_versioned_binary(path: &str) -> bool {
        path.contains(".local/share/claude/versions/")
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

    // BUG FIX: symlink-resolved binary name (macOS proc_pidpath resolves symlinks)
    // `claude` -> `.local/share/claude/versions/2.1.75`, so proc.name() = "2.1.75"

    #[test]
    fn test_is_claude_session_resolved_symlink_name() {
        // sysinfo reports resolved binary name, not the symlink
        let info = make_process_info(
            1234, 1, "2.1.75",
            vec!["claude", "--dangerously-skip-permissions", "--teammate-mode", "tmux"],
        );
        assert!(info.is_claude_session());
    }

    #[test]
    fn test_is_claude_session_versioned_binary_path() {
        // Process launched via full versioned path
        let info = make_process_info(
            1234, 1, "2.1.75",
            vec!["/Users/x/.local/share/claude/versions/2.1.75"],
        );
        assert!(info.is_claude_session());
    }

    #[test]
    fn test_is_claude_session_excludes_teammates() {
        // Teammate processes have --agent-id and should NOT be sessions
        let info = make_process_info(
            1234, 1, "2.1.75",
            vec![
                "/Users/x/.local/share/claude/versions/2.1.75",
                "--agent-id", "worker-1@rune-work-123",
                "--agent-name", "worker-1",
                "--dangerously-skip-permissions",
            ],
        );
        assert!(!info.is_claude_session());
    }

    #[test]
    fn test_is_claude_session_excludes_mcp_with_claude_path() {
        // MCP server in a .claude/ path should not be a session
        let info = make_process_info(
            1234, 1, "python3",
            vec!["python3", "/home/user/.claude/plugins/echo-search/server.py"],
        );
        assert!(!info.is_claude_session());
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
