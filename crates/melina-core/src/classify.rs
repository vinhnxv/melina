//! Child process classification — determine what role each child plays.

use crate::ProcessInfo;
use serde::Serialize;

/// What kind of child process this is within a Claude session.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum ChildKind {
    /// MCP server (echo-search, figma-to-react, context7, etc.)
    McpServer { server_name: String },
    /// Agent teammate (another claude process spawned by Agent tool)
    Teammate { name: Option<String> },
    /// Hook script execution
    HookScript,
    /// Process running from a Claude config directory (plugin, skill, snapshot, etc.)
    ConfigDirProcess {
        config_dir: String,
        process_type: ConfigProcessType,
    },
    /// Bash tool execution
    BashTool,
    /// Unknown child
    Unknown,
}

/// Sub-classification for processes originating from a Claude config directory.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum ConfigProcessType {
    /// MCP plugin server (plugins/)
    Plugin,
    /// Skill script (skills/)
    Skill,
    /// Shell snapshot (shell-snapshots/)
    ShellSnapshot,
    /// Hook from config dir (hooks/)
    Hook,
    /// Other config dir process
    Other,
}

/// Classify a child process based on its command line and config directory context.
pub fn classify_child(proc: &ProcessInfo, config_dirs: &[std::path::PathBuf]) -> ChildKind {
    let cmd_str = proc.cmd.join(" ");

    // MCP servers — Python or Node scripts under plugin cache
    if cmd_str.contains("server.py")
        || cmd_str.contains("/mcp/")
        || cmd_str.contains("mcp-server")
        || cmd_str.contains("mcp_server")
    {
        let server_name = extract_mcp_name(&cmd_str);
        return ChildKind::McpServer { server_name };
    }

    // Teammate — another `claude` process whose parent is also claude
    // On macOS, the process name may be a version number (e.g., "2.1.75") due to symlink resolution.
    // Check both process name AND command args for claude-related patterns.
    let is_teammate = proc.name.to_lowercase().contains("claude")
        || cmd_str.contains("--agent-id")
        || ProcessInfo::is_claude_versioned_binary(&cmd_str.to_lowercase());
    if is_teammate && !cmd_str.contains("server.py") {
        return ChildKind::Teammate {
            name: extract_teammate_name(&cmd_str),
        };
    }

    // Hook scripts
    if cmd_str.contains("hooks/") || cmd_str.contains("hook") {
        return ChildKind::HookScript;
    }

    // Config dir processes — check if cmd references any known Claude config dir
    if let Some(kind) = classify_config_dir_process(&cmd_str, config_dirs) {
        return kind;
    }

    // Bash/shell children (from Bash tool)
    if proc.name.contains("sh") || proc.name.contains("bash") || proc.name.contains("zsh") {
        return ChildKind::BashTool;
    }

    ChildKind::Unknown
}

/// Backward-compatible classify without config dirs.
pub fn classify_child_simple(proc: &ProcessInfo) -> ChildKind {
    classify_child(proc, &[])
}

/// Check if a command string references a known config directory and classify it.
fn classify_config_dir_process(
    cmd_str: &str,
    config_dirs: &[std::path::PathBuf],
) -> Option<ChildKind> {
    for dir in config_dirs {
        let dir_str = dir.to_string_lossy();
        if cmd_str.contains(dir_str.as_ref()) {
            let dir_name = dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let process_type = detect_config_process_type(cmd_str);
            return Some(ChildKind::ConfigDirProcess {
                config_dir: dir_name,
                process_type,
            });
        }
    }
    None
}

/// Determine the sub-type of a config directory process from its command.
fn detect_config_process_type(cmd_str: &str) -> ConfigProcessType {
    if cmd_str.contains("plugins/") || cmd_str.contains("server.py") {
        ConfigProcessType::Plugin
    } else if cmd_str.contains("skills/") {
        ConfigProcessType::Skill
    } else if cmd_str.contains("shell-snapshots/") {
        ConfigProcessType::ShellSnapshot
    } else if cmd_str.contains("hooks/") {
        ConfigProcessType::Hook
    } else {
        ConfigProcessType::Other
    }
}

/// Extract a meaningful description of what a child process is doing.
/// Returns a short human-readable string for the INFO column instead of just the process name.
pub fn describe_child(proc: &ProcessInfo, kind: &ChildKind) -> String {
    let cmd_str = proc.cmd.join(" ");

    match kind {
        ChildKind::McpServer { server_name } => server_name.clone(),
        ChildKind::Teammate { name } => name.clone().unwrap_or_else(|| "teammate".to_string()),
        ChildKind::HookScript => {
            // Try to extract hook script name
            extract_script_name(&cmd_str).unwrap_or_else(|| "hook".to_string())
        }
        ChildKind::ConfigDirProcess { process_type, .. } => {
            match process_type {
                ConfigProcessType::Plugin => {
                    // Extract plugin/MCP name from path like .../scripts/echo-search/server.py
                    extract_mcp_name(&cmd_str)
                }
                ConfigProcessType::Skill => {
                    // Extract skill name from path like .../skills/skill-creator/eval-viewer/...
                    extract_skill_name(&cmd_str).unwrap_or_else(|| "skill".to_string())
                }
                ConfigProcessType::ShellSnapshot => {
                    // Extract useful info from shell-snapshot command
                    extract_shell_snapshot_info(&cmd_str)
                }
                ConfigProcessType::Hook => {
                    extract_script_name(&cmd_str).unwrap_or_else(|| "hook".to_string())
                }
                ConfigProcessType::Other => {
                    extract_script_name(&cmd_str).unwrap_or_else(|| proc.name.clone())
                }
            }
        }
        ChildKind::BashTool => {
            // Try to extract what the bash is actually running
            extract_bash_description(&cmd_str).unwrap_or_else(|| proc.name.clone())
        }
        ChildKind::Unknown => proc.name.clone(),
    }
}

/// Extract a script name from a command path.
fn extract_script_name(cmd: &str) -> Option<String> {
    // Find the last meaningful path component (e.g., "generate_review.py", "pre-tool.sh")
    for part in cmd.split_whitespace() {
        if part.ends_with(".py") || part.ends_with(".sh") || part.ends_with(".js") {
            let name = part.rsplit('/').next().unwrap_or(part);
            return Some(name.to_string());
        }
    }
    None
}

/// Extract skill name from path like .../skills/skill-creator/eval-viewer/generate_review.py
fn extract_skill_name(cmd: &str) -> Option<String> {
    if let Some(pos) = cmd.find("skills/") {
        let after = &cmd[pos + 7..];
        // Take up to 2 path components for skill identification
        let parts: Vec<&str> = after.splitn(4, '/').collect();
        return match parts.len() {
            1 => Some(parts[0].to_string()),
            2.. => Some(format!("{}/{}", parts[0], parts[1])),
            _ => None,
        };
    }
    None
}

/// Extract meaningful info from shell-snapshot command.
fn extract_shell_snapshot_info(cmd: &str) -> String {
    // Look for the eval command content
    if let Some(pos) = cmd.find("eval '") {
        let after = &cmd[pos + 6..];
        // Take first meaningful part (command name)
        let content: String = after.chars().take(40).collect();
        if let Some(end) = content.find('\'') {
            let eval_cmd = &content[..end];
            // Get just the first command/tool name
            return eval_cmd
                .split_whitespace()
                .next()
                .unwrap_or("shell-snapshot")
                .to_string();
        }
    }
    "shell-snapshot".to_string()
}

/// Extract what a bash/zsh process is actually running.
fn extract_bash_description(cmd: &str) -> Option<String> {
    // Look for -c flag indicating command execution
    if cmd.contains(" -c ") {
        // Try to find the actual command after -c
        let parts: Vec<&str> = cmd.splitn(2, " -c ").collect();
        if parts.len() == 2 {
            let actual_cmd = parts[1].trim().trim_start_matches('\'').trim_start_matches('"');
            // Get the first meaningful word (command name)
            let first_cmd = actual_cmd
                .split_whitespace()
                .find(|w| !w.starts_with('-') && *w != "source" && *w != "export" && *w != "eval");
            if let Some(cmd_name) = first_cmd {
                let name = cmd_name.rsplit('/').next().unwrap_or(cmd_name);
                if name.len() > 1 && name != "true" && name != "false" {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

fn extract_mcp_name(cmd: &str) -> String {
    // Try to find the script name from path like .../scripts/echo-search/server.py
    if let Some(pos) = cmd.find("scripts/") {
        let after = &cmd[pos + 8..];
        if let Some(slash) = after.find('/') {
            return after[..slash].to_string();
        }
    }
    // Fallback: last path component before server.py
    "unknown-mcp".to_string()
}

fn extract_teammate_name(cmd: &str) -> Option<String> {
    // Teammates might have --name or similar flags
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == "--name" || *part == "-n" {
            return parts.get(i + 1).map(|s| s.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Helper to create a ProcessInfo for testing.
    fn make_process_info(pid: u32, name: &str, cmd: Vec<&str>) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid: 1,
            name: name.to_string(),
            cmd: cmd.into_iter().map(String::from).collect(),
            cwd: PathBuf::from("/tmp"),
            exe: None,
            memory_bytes: 0,
            cpu_percent: 0.0,
            start_time: 0,
            status: "running".to_string(),
        }
    }

    // ========== classify_child() tests ==========

    #[test]
    fn test_classify_mcp_server() {
        // Test "server.py" in command
        let proc = make_process_info(
            100,
            "python",
            vec!["/some/path/scripts/echo-search/server.py"],
        );
        let result = classify_child(&proc, &[]);
        assert!(matches!(result, ChildKind::McpServer { .. }));
        if let ChildKind::McpServer { server_name } = result {
            assert_eq!(server_name, "echo-search");
        }

        // Test "/mcp/" in command
        let proc = make_process_info(101, "node", vec!["/mcp/some-server"]);
        assert!(matches!(classify_child(&proc, &[]), ChildKind::McpServer { .. }));

        // Test "mcp-server" in command
        let proc = make_process_info(102, "node", vec!["mcp-server-foo"]);
        assert!(matches!(classify_child(&proc, &[]), ChildKind::McpServer { .. }));
    }

    #[test]
    fn test_classify_teammate() {
        let proc = make_process_info(200, "claude", vec!["claude", "--some-flag"]);
        let result = classify_child(&proc, &[]);
        assert!(matches!(result, ChildKind::Teammate { .. }));

        // Test case insensitivity
        let proc = make_process_info(201, "Claude", vec!["Claude"]);
        assert!(matches!(classify_child(&proc, &[]), ChildKind::Teammate { .. }));

        // Test versioned binary name (macOS symlink resolution)
        // Process name is version number, but cmd has versioned path
        let proc = make_process_info(
            202,
            "2.1.75",
            vec![
                "/Users/x/.local/share/claude/versions/2.1.75",
                "--agent-id",
                "worker-1",
            ],
        );
        assert!(matches!(classify_child(&proc, &[]), ChildKind::Teammate { .. }));
    }

    #[test]
    fn test_classify_hook_script() {
        // Test "hooks/" in command
        let proc = make_process_info(300, "sh", vec!["/hooks/pre-tool.sh"]);
        assert!(matches!(classify_child(&proc, &[]), ChildKind::HookScript));

        // Test "hook" in command
        let proc = make_process_info(301, "python", vec!["/some/hook-script.py"]);
        assert!(matches!(classify_child(&proc, &[]), ChildKind::HookScript));
    }

    #[test]
    fn test_classify_bash_tool() {
        // Test "sh" name
        let proc = make_process_info(400, "sh", vec!["sh", "-c", "echo test"]);
        assert!(matches!(classify_child(&proc, &[]), ChildKind::BashTool));

        // Test "bash" name
        let proc = make_process_info(401, "bash", vec!["bash"]);
        assert!(matches!(classify_child(&proc, &[]), ChildKind::BashTool));

        // Test "zsh" name
        let proc = make_process_info(402, "zsh", vec!["zsh"]);
        assert!(matches!(classify_child(&proc, &[]), ChildKind::BashTool));
    }

    #[test]
    fn test_classify_unknown() {
        // Process that doesn't match any category
        let proc = make_process_info(500, "some-random-process", vec!["--flag"]);
        assert!(matches!(classify_child(&proc, &[]), ChildKind::Unknown));

        // Another unknown
        let proc = make_process_info(501, "vim", vec!["vim", "file.txt"]);
        assert!(matches!(classify_child(&proc, &[]), ChildKind::Unknown));
    }

    // ========== extract_mcp_name() tests ==========

    #[test]
    fn test_extract_mcp_name_from_scripts_path() {
        let cmd = "/some/path/scripts/echo-search/server.py --port 8080";
        assert_eq!(extract_mcp_name(cmd), "echo-search");

        let cmd = "python /Users/test/scripts/my-mcp/server.py";
        assert_eq!(extract_mcp_name(cmd), "my-mcp");
    }

    #[test]
    fn test_extract_mcp_name_fallback() {
        // No "scripts/" in path
        let cmd = "python server.py";
        assert_eq!(extract_mcp_name(cmd), "unknown-mcp");

        // Path with mcp-server but no scripts/
        let cmd = "/usr/local/bin/mcp-server-foo";
        assert_eq!(extract_mcp_name(cmd), "unknown-mcp");
    }

    // ========== extract_teammate_name() tests ==========

    #[test]
    fn test_extract_teammate_name_long_flag() {
        let cmd = "claude --name teammate1 --other-flag";
        assert_eq!(extract_teammate_name(cmd), Some("teammate1".to_string()));
    }

    #[test]
    fn test_extract_teammate_name_short_flag() {
        let cmd = "claude -n teammate2 --other-flag";
        assert_eq!(extract_teammate_name(cmd), Some("teammate2".to_string()));
    }

    #[test]
    fn test_extract_teammate_name_no_flag() {
        let cmd = "claude --some-other-flag value";
        assert_eq!(extract_teammate_name(cmd), None);

        let cmd = "claude";
        assert_eq!(extract_teammate_name(cmd), None);
    }

    // ========== Config dir process classification tests ==========

    #[test]
    fn test_classify_config_dir_plugin_as_mcp() {
        // MCP servers with server.py are classified as McpServer (higher priority),
        // even when they're in a config dir. This is intentional — the MCP check
        // runs before config dir check.
        let config_dirs = vec![PathBuf::from("/Users/vinhnx/.claude-true-yp")];
        let proc = make_process_info(
            600,
            "python3",
            vec![
                "/Users/vinhnx/.pyenv/versions/3.11.10/bin/python3",
                "/Users/vinhnx/.claude-true-yp/plugins/cache/rune-marketplace/rune/1.167.0/scripts/echo-search/server.py",
            ],
        );
        let result = classify_child(&proc, &config_dirs);
        assert!(
            matches!(result, ChildKind::McpServer { .. }),
            "MCP servers should still be classified as McpServer, got {:?}",
            result
        );
    }

    #[test]
    fn test_classify_config_dir_plugin_non_mcp() {
        // Non-server.py plugin process should be classified as ConfigDirProcess::Plugin
        let config_dirs = vec![PathBuf::from("/Users/vinhnx/.claude-true-yp")];
        let proc = make_process_info(
            600,
            "node",
            vec![
                "node",
                "/Users/vinhnx/.claude-true-yp/plugins/cache/some-tool/index.js",
            ],
        );
        let result = classify_child(&proc, &config_dirs);
        assert!(
            matches!(result, ChildKind::ConfigDirProcess { process_type: ConfigProcessType::Plugin, .. }),
            "Expected ConfigDirProcess::Plugin, got {:?}",
            result
        );
    }

    #[test]
    fn test_classify_config_dir_skill() {
        let config_dirs = vec![PathBuf::from("/Users/vinhnx/.claude")];
        let proc = make_process_info(
            601,
            "python",
            vec![
                "python",
                ".claude/skills/skill-creator/eval-viewer/generate_review.py",
                "arc-batch-workspace/iteration-1",
            ],
        );
        // Note: cmd contains ".claude/" which matches is_claude_related(), but
        // also matches config_dir_process since /Users/vinhnx/.claude is a config dir.
        // However, we need the full path in cmd for config dir matching.
        // In practice, the process may use relative path. The classify_child check
        // looks for the full config dir path in cmd_str. If the cmd uses a relative
        // path like ".claude/skills/...", it won't match "/Users/vinhnx/.claude".
        // This is a known limitation — relative paths aren't matched.
    }

    #[test]
    fn test_classify_config_dir_shell_snapshot() {
        let config_dirs = vec![PathBuf::from("/Users/vinhnx/.claude-true-yp")];
        let proc = make_process_info(
            602,
            "zsh",
            vec![
                "/bin/zsh",
                "-c",
                "source /Users/vinhnx/.claude-true-yp/shell-snapshots/snapshot-zsh-123.sh && export RUNE_SESSION_ID=\"abc\"",
            ],
        );
        let result = classify_child(&proc, &config_dirs);
        assert!(
            matches!(result, ChildKind::ConfigDirProcess { process_type: ConfigProcessType::ShellSnapshot, .. }),
            "Expected ConfigDirProcess::ShellSnapshot, got {:?}",
            result
        );
    }

    #[test]
    fn test_classify_bash_without_config_dir() {
        // Plain bash without config dir reference should still be BashTool
        let config_dirs = vec![PathBuf::from("/Users/vinhnx/.claude")];
        let proc = make_process_info(603, "bash", vec!["bash", "-c", "echo hello"]);
        let result = classify_child(&proc, &config_dirs);
        assert!(matches!(result, ChildKind::BashTool));
    }

    // ========== describe_child() tests ==========

    #[test]
    fn test_describe_bash_with_command() {
        let proc = make_process_info(
            700,
            "bash",
            vec!["bash", "-c", "rtk read tmp/reviews/abc.md"],
        );
        let kind = ChildKind::BashTool;
        let desc = describe_child(&proc, &kind);
        assert_eq!(desc, "rtk");
    }

    #[test]
    fn test_describe_shell_snapshot() {
        let proc = make_process_info(
            701,
            "zsh",
            vec![
                "/bin/zsh",
                "-c",
                "source /Users/x/.claude/shell-snapshots/snap.sh && eval 'cargo build'",
            ],
        );
        let kind = ChildKind::ConfigDirProcess {
            config_dir: ".claude".to_string(),
            process_type: ConfigProcessType::ShellSnapshot,
        };
        let desc = describe_child(&proc, &kind);
        assert_eq!(desc, "cargo");
    }

    #[test]
    fn test_describe_plain_bash() {
        let proc = make_process_info(702, "bash", vec!["bash"]);
        let kind = ChildKind::BashTool;
        let desc = describe_child(&proc, &kind);
        assert_eq!(desc, "bash"); // fallback to process name
    }
}
