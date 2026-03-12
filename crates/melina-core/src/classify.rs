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
        return ChildKind::Teammate { name: extract_teammate_name(&cmd_str) };
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
