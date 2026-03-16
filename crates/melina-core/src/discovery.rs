//! Process discovery — find all Claude Code related processes via sysinfo.

use serde::Serialize;
use std::path::PathBuf;
use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Raw process info extracted from the OS.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub cmd: Vec<String>,
    pub cwd: PathBuf,
    /// Full path to the executable binary (resolved by the OS).
    pub exe: Option<PathBuf>,
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
            exe: proc_.exe().map(|p| p.to_path_buf()),
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
        // Fast path: check first argument case-insensitively without joining
        // Most processes are not Claude-related, so this avoids the expensive
        // join().to_lowercase() allocation for the common case.
        let first_arg_is_claude = self.cmd.first().is_some_and(|c| {
            let c_lower = c.to_lowercase();
            Self::is_claude_binary(&c_lower) || Self::is_claude_versioned_binary(&c_lower)
        });

        // Early exit if first arg isn't a claude binary
        if !first_arg_is_claude {
            return false;
        }

        // Now check the full command line for exclusion patterns.
        // Only allocate the joined string if we passed the first check.
        let cmd_str = self.cmd.join(" ").to_lowercase();

        // Must reference "claude" somewhere in the command line
        if !cmd_str.contains("claude") {
            return false;
        }

        // Exclude Claude desktop app (Claude.app) — not Claude Code
        if cmd_str.contains("claude.app") {
            return false;
        }

        // Exclude claude-powerline and similar status-line tools
        if cmd_str.contains("claude-powerline") || cmd_str.contains("claude_powerline") {
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

        true
    }

    /// Check if a binary path is a Claude Code binary (direct name or path ending in /claude).
    fn is_claude_binary(path: &str) -> bool {
        path == "claude" || path.ends_with("/claude")
    }

    /// Check if a binary path looks like a Claude versioned binary.
    /// e.g. `/Users/x/.local/share/claude/versions/2.1.75`
    pub fn is_claude_versioned_binary(path: &str) -> bool {
        path.contains(".local/share/claude/versions/")
    }

    /// Check if this is a Claude-related child (MCP server, hook, etc.).
    pub fn is_claude_related(&self) -> bool {
        // Fast path: check individual args without joining
        // Avoids string allocation for the common case of non-Claude processes
        let has_claude_in_args = self.cmd.iter().any(|arg| {
            let arg_lower = arg.to_lowercase();
            arg_lower.contains("claude") || arg_lower.contains(".claude/")
        });

        if !has_claude_in_args {
            return false;
        }

        // Exclude Claude desktop app processes (check with original case for .app paths)
        self.cmd
            .iter()
            .all(|arg| !arg.contains("Claude.app") && !arg.to_lowercase().contains("claude.app"))
    }

    /// Check if this process references any known Claude config directory.
    /// Catches processes from custom config dirs like `.claude-true-yp/` that
    /// `is_claude_related()` would miss (it only matches `.claude/`).
    pub fn is_config_dir_process(&self, config_dirs: &[std::path::PathBuf]) -> bool {
        if config_dirs.is_empty() {
            return false;
        }
        self.cmd.iter().any(|arg| {
            config_dirs
                .iter()
                .any(|dir| arg.contains(&*dir.to_string_lossy()))
        })
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
///
/// Uses `refresh_processes_specifics` with `cmd` and `exe` set to `OnlyIfNotSet`
/// so that newly discovered processes get their command line populated.
/// Without this, `sysinfo::refresh_processes` defaults to `ProcessRefreshKind`
/// that omits `cmd`, leaving new processes with empty `cmd()` — which causes
/// `is_claude_session()` to miss them since it relies on argv content.
pub fn refresh_process_system(sys: &mut System) {
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_memory()
            .with_cpu()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_exe(UpdateKind::OnlyIfNotSet),
    );
}

/// Validate that a process with the given PID still matches the expected command fragment.
///
/// # Race Condition Warning
///
/// PIDs can be reused by the OS between a scan and subsequent kill operations.
/// After a process exits, its PID may be reassigned to an unrelated process.
///
/// **Callers MUST validate process identity before destructive operations** (kill, etc.)
/// by checking that the command line still matches expectations.
///
/// # Example
/// ```ignore
/// // Before killing, re-validate the process hasn't been replaced
/// if validate_process_identity(pid, "claude") {
///     // Safe to proceed with kill
/// }
/// ```
#[allow(dead_code)]
pub fn validate_process_identity(sys: &System, pid: u32, expected_cmd_fragment: &str) -> bool {
    sys.processes()
        .get(&Pid::from_u32(pid))
        .is_some_and(|proc_| {
            proc_.cmd().iter().any(|arg| {
                arg.to_string_lossy()
                    .to_lowercase()
                    .contains(expected_cmd_fragment)
            })
        })
}

/// Scan all processes and return Claude-related ones.
/// Uses a pre-created `System` to avoid redundant allocations.
/// Accepts `config_dirs` to detect processes from custom Claude config directories.
///
/// # Race Condition Warning
///
/// The returned `ProcessInfo` snapshots are point-in-time observations.
/// PIDs can be reused by the OS after a process exits. Before performing
/// destructive operations (kill, etc.), call `validate_process_identity()`
/// to confirm the process hasn't been replaced by an unrelated one.
pub fn scan(sys: &System, config_dirs: &[std::path::PathBuf]) -> Vec<ProcessInfo> {
    sys.processes()
        .iter()
        .filter_map(|(&pid, proc_)| {
            let info = ProcessInfo::from_process(pid, proc_);
            if info.is_claude_session()
                || info.is_claude_related()
                || info.is_config_dir_process(config_dirs)
            {
                Some(info)
            } else {
                None
            }
        })
        .collect()
}

/// Scan without config dir awareness (backward compatible).
pub fn scan_simple(sys: &System) -> Vec<ProcessInfo> {
    scan(sys, &[])
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
            exe: None,
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
    fn test_is_claude_session_node_running_claude_not_session() {
        // Node running "claude" as arg — but first arg is "node", not a claude binary
        let info = make_process_info(1234, 1, "node", vec!["node", "claude"]);
        assert!(!info.is_claude_session());
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
            1234,
            1,
            "2.1.75",
            vec![
                "claude",
                "--dangerously-skip-permissions",
                "--teammate-mode",
                "tmux",
            ],
        );
        assert!(info.is_claude_session());
    }

    #[test]
    fn test_is_claude_session_versioned_binary_path() {
        // Process launched via full versioned path
        let info = make_process_info(
            1234,
            1,
            "2.1.75",
            vec!["/Users/x/.local/share/claude/versions/2.1.75"],
        );
        assert!(info.is_claude_session());
    }

    #[test]
    fn test_is_claude_session_excludes_teammates() {
        // Teammate processes have --agent-id and should NOT be sessions
        let info = make_process_info(
            1234,
            1,
            "2.1.75",
            vec![
                "/Users/x/.local/share/claude/versions/2.1.75",
                "--agent-id",
                "worker-1@rune-work-123",
                "--agent-name",
                "worker-1",
                "--dangerously-skip-permissions",
            ],
        );
        assert!(!info.is_claude_session());
    }

    #[test]
    fn test_is_claude_session_excludes_desktop_app() {
        // Claude.app chrome-native-host is NOT Claude Code
        let info = make_process_info(
            26785,
            1,
            "chrome-native-host",
            vec![
                "/Applications/Claude.app/Contents/Helpers/chrome-native-host",
                "chrome-extension://fcoeoabgfenejglbffodgkkbkcdhcgfn/",
            ],
        );
        assert!(!info.is_claude_session());
    }

    #[test]
    fn test_is_claude_session_excludes_powerline() {
        // claude-powerline is a status line tool, not a Claude Code session
        let info = make_process_info(
            21346,
            1,
            "node",
            vec![
                "node",
                "/Users/x/.npm/_npx/abc/node_modules/.bin/claude-powerline",
                "--style=powerline",
            ],
        );
        assert!(!info.is_claude_session());
    }

    #[test]
    fn test_is_claude_session_excludes_mcp_with_claude_path() {
        // MCP server in a .claude/ path should not be a session
        let info = make_process_info(
            1234,
            1,
            "python3",
            vec![
                "python3",
                "/home/user/.claude/plugins/echo-search/server.py",
            ],
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
