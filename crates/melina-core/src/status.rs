//! Claude Code session status detection from tmux pane content.
//!
//! Detects whether a Claude Code session is idle, working, or waiting for input
//! by capturing and analyzing the visual content of its tmux pane.

use serde::Serialize;

/// Status of a Claude Code session detected from tmux pane content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub enum ClaudeSessionStatus {
    /// At prompt, ready for input (❯ with ─ border above)
    Idle,
    /// Actively processing ("ctrl+c to interrupt" visible)
    Working,
    /// Waiting for user confirmation ([y/n] prompt)
    WaitingInput,
    /// Cannot determine (no tmux pane, or not in tmux)
    #[default]
    Unknown,
}

impl ClaudeSessionStatus {
    /// Returns the display symbol for this status.
    pub fn symbol(&self) -> &'static str {
        match self {
            ClaudeSessionStatus::Idle => "○",
            ClaudeSessionStatus::Working => "●",
            ClaudeSessionStatus::WaitingInput => "◐",
            ClaudeSessionStatus::Unknown => "?",
        }
    }

    /// Returns the display label for this status.
    pub fn label(&self) -> &'static str {
        match self {
            ClaudeSessionStatus::Idle => "idle",
            ClaudeSessionStatus::Working => "working",
            ClaudeSessionStatus::WaitingInput => "input",
            ClaudeSessionStatus::Unknown => "unknown",
        }
    }

    /// Returns the ANSI-colored symbol for CLI output.
    pub fn colored_symbol(&self) -> &'static str {
        match self {
            ClaudeSessionStatus::Working => "\x1b[32m●\x1b[0m", // green
            ClaudeSessionStatus::Idle => "\x1b[33m○\x1b[0m",    // yellow
            ClaudeSessionStatus::WaitingInput => "\x1b[35m◐\x1b[0m", // magenta
            ClaudeSessionStatus::Unknown => "\x1b[90m?\x1b[0m", // gray
        }
    }
}

impl std::fmt::Display for ClaudeSessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Detect Claude Code status from tmux pane content.
pub fn detect_status(content: &str) -> ClaudeSessionStatus {
    // Step 1: Detect input field by its visual structure
    if has_input_field(content) {
        // Step 2: Check if interruptable (actively working)
        if content.contains("ctrl+c") && content.contains("to interrupt") {
            return ClaudeSessionStatus::Working;
        }
        return ClaudeSessionStatus::Idle;
    }

    // No input field — check for permission prompt
    if content.contains("[y/n]") || content.contains("[Y/n]") {
        return ClaudeSessionStatus::WaitingInput;
    }

    ClaudeSessionStatus::Unknown
}

/// Detect input field: prompt line (❯) with border (─) directly above it.
fn has_input_field(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        if line.contains('❯') {
            // Check if line above is a border
            if i > 0 && lines[i - 1].contains('─') {
                return true;
            }
        }
    }

    false
}

/// Capture the last N non-empty lines from a tmux pane.
/// Returns None if the pane cannot be captured or pane_id format is invalid.
pub fn capture_pane_content(pane_id: &str, lines: usize) -> Option<String> {
    use std::process::Command;

    // Validate pane_id format to prevent command injection
    // Pane IDs must be '%' followed by one or more digits (e.g., "%0", "%12")
    if !pane_id.starts_with('%') || pane_id.len() <= 1 {
        return None;
    }
    let digits = &pane_id[1..];
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-t",
            pane_id,
            "-p", // Print to stdout
            "-J", // Join wrapped lines
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let content = String::from_utf8_lossy(&output.stdout);

    // Filter out empty lines and take the last N
    let non_empty: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = non_empty.len().saturating_sub(lines);
    let last_lines = &non_empty[start..];
    Some(last_lines.join("\n"))
}

/// Detect Claude session status for a given tmux pane ID.
/// Captures the last 15 lines of the pane and runs content-based detection.
pub fn detect_pane_status(pane_id: &str) -> ClaudeSessionStatus {
    match capture_pane_content(pane_id, 15) {
        Some(content) => detect_status(&content),
        None => ClaudeSessionStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_working() {
        let content = "* (ctrl+c to interrupt)\n─────\n❯ hello";
        assert_eq!(detect_status(content), ClaudeSessionStatus::Working);
    }

    #[test]
    fn test_idle() {
        let content = "Done\n─────\n❯ hello";
        assert_eq!(detect_status(content), ClaudeSessionStatus::Idle);
    }

    #[test]
    fn test_no_border_above_prompt() {
        // Border exists but not directly above prompt — should be unknown
        let content = "─────\nsome text\n❯ hello";
        assert_eq!(detect_status(content), ClaudeSessionStatus::Unknown);
    }

    #[test]
    fn test_waiting_input() {
        let content = "Delete files? [y/n]";
        assert_eq!(detect_status(content), ClaudeSessionStatus::WaitingInput);
    }

    #[test]
    fn test_waiting_input_yes_default() {
        let content = "Approve changes? [Y/n]";
        assert_eq!(detect_status(content), ClaudeSessionStatus::WaitingInput);
    }

    #[test]
    fn test_unknown() {
        let content = "random stuff";
        assert_eq!(detect_status(content), ClaudeSessionStatus::Unknown);
    }

    #[test]
    fn test_empty_content() {
        assert_eq!(detect_status(""), ClaudeSessionStatus::Unknown);
    }

    #[test]
    fn test_symbols() {
        assert_eq!(ClaudeSessionStatus::Working.symbol(), "●");
        assert_eq!(ClaudeSessionStatus::Idle.symbol(), "○");
        assert_eq!(ClaudeSessionStatus::WaitingInput.symbol(), "◐");
        assert_eq!(ClaudeSessionStatus::Unknown.symbol(), "?");
    }

    #[test]
    fn test_labels() {
        assert_eq!(ClaudeSessionStatus::Working.label(), "working");
        assert_eq!(ClaudeSessionStatus::Idle.label(), "idle");
        assert_eq!(ClaudeSessionStatus::WaitingInput.label(), "input");
        assert_eq!(ClaudeSessionStatus::Unknown.label(), "unknown");
    }
}
