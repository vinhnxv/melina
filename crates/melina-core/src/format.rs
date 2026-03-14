//! Shared formatting utilities for CLI and TUI.

/// Format bytes as human-readable string (e.g., "1.5GB", "256MB", "1.2KB").
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1}GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1}MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{}B", bytes)
    }
}

/// Format uptime as human-readable string (e.g., "2h30m", "45m").
#[must_use]
pub fn format_uptime(start_time: u64) -> String {
    if start_time == 0 {
        return "0m".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let elapsed = now.saturating_sub(start_time);
    let hours = elapsed / 3600;
    let mins = (elapsed % 3600) / 60;
    if hours > 0 {
        format!("{}h{}m", hours, mins)
    } else {
        format!("{}m", mins)
    }
}

/// Format Unix epoch timestamp as human-readable local time string.
#[must_use]
pub fn format_timestamp(epoch: u64) -> String {
    use std::process::Command;
    if epoch == 0 {
        return "unknown".to_string();
    }
    // Try BSD/macOS date syntax first (`date -r epoch`), then GNU/Linux (`date -d @epoch`)
    let bsd_result = Command::new("date")
        .args(["-r", &epoch.to_string(), "+%Y-%m-%d %H:%M:%S"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok().map(|s| s.trim().to_string())
            } else {
                None
            }
        });

    if let Some(result) = bsd_result {
        return result;
    }

    // Fallback to GNU/Linux syntax
    Command::new("date")
        .args(["-d", &format!("@{epoch}"), "+%Y-%m-%d %H:%M:%S"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok().map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0B");
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(1024), "1.0KB");
        assert_eq!(format_bytes(1_048_576), "1.0MB");
        assert_eq!(format_bytes(1_073_741_824), "1.0GB");
    }

    #[test]
    fn test_format_uptime() {
        assert_eq!(format_uptime(0), "0m");
        // These tests depend on current time, so we just check format
        assert!(format_uptime(3600).ends_with('m')); // 1h0m
    }

    #[test]
    fn test_format_timestamp() {
        assert_eq!(format_timestamp(0), "unknown");
        // Non-zero epoch should produce a date string
        let result = format_timestamp(1700000000);
        assert!(result.contains("2023"), "expected 2023 in {}", result);
    }
}
