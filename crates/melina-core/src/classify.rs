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
    /// Bash tool execution
    BashTool,
    /// Unknown child
    Unknown,
}

/// Classify a child process based on its command line.
pub fn classify_child(proc: &ProcessInfo) -> ChildKind {
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
    if proc.name.to_lowercase().contains("claude") && !cmd_str.contains("server.py") {
        return ChildKind::Teammate {
            name: extract_teammate_name(&cmd_str),
        };
    }

    // Hook scripts
    if cmd_str.contains("hooks/") || cmd_str.contains("hook") {
        return ChildKind::HookScript;
    }

    // Bash/shell children (from Bash tool)
    if proc.name.contains("sh") || proc.name.contains("bash") || proc.name.contains("zsh") {
        return ChildKind::BashTool;
    }

    ChildKind::Unknown
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
        let result = classify_child(&proc);
        assert!(matches!(result, ChildKind::McpServer { .. }));
        if let ChildKind::McpServer { server_name } = result {
            assert_eq!(server_name, "echo-search");
        }

        // Test "/mcp/" in command
        let proc = make_process_info(101, "node", vec!["/mcp/some-server"]);
        assert!(matches!(classify_child(&proc), ChildKind::McpServer { .. }));

        // Test "mcp-server" in command
        let proc = make_process_info(102, "node", vec!["mcp-server-foo"]);
        assert!(matches!(classify_child(&proc), ChildKind::McpServer { .. }));
    }

    #[test]
    fn test_classify_teammate() {
        let proc = make_process_info(200, "claude", vec!["claude", "--some-flag"]);
        let result = classify_child(&proc);
        assert!(matches!(result, ChildKind::Teammate { .. }));

        // Test case insensitivity
        let proc = make_process_info(201, "Claude", vec!["Claude"]);
        assert!(matches!(classify_child(&proc), ChildKind::Teammate { .. }));
    }

    #[test]
    fn test_classify_hook_script() {
        // Test "hooks/" in command
        let proc = make_process_info(300, "sh", vec!["/hooks/pre-tool.sh"]);
        assert!(matches!(classify_child(&proc), ChildKind::HookScript));

        // Test "hook" in command
        let proc = make_process_info(301, "python", vec!["/some/hook-script.py"]);
        assert!(matches!(classify_child(&proc), ChildKind::HookScript));
    }

    #[test]
    fn test_classify_bash_tool() {
        // Test "sh" name
        let proc = make_process_info(400, "sh", vec!["sh", "-c", "echo test"]);
        assert!(matches!(classify_child(&proc), ChildKind::BashTool));

        // Test "bash" name
        let proc = make_process_info(401, "bash", vec!["bash"]);
        assert!(matches!(classify_child(&proc), ChildKind::BashTool));

        // Test "zsh" name
        let proc = make_process_info(402, "zsh", vec!["zsh"]);
        assert!(matches!(classify_child(&proc), ChildKind::BashTool));
    }

    #[test]
    fn test_classify_unknown() {
        // Process that doesn't match any category
        let proc = make_process_info(500, "some-random-process", vec!["--flag"]);
        assert!(matches!(classify_child(&proc), ChildKind::Unknown));

        // Another unknown
        let proc = make_process_info(501, "vim", vec!["vim", "file.txt"]);
        assert!(matches!(classify_child(&proc), ChildKind::Unknown));
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
}
