use anyhow::Result;
use chrono::Local;
use clap::Parser;
use melina_core::{scan, build_trees, scan_teams, check_team_health, scan_tmux_servers, kill_tmux_server, ChildKind, TeammateHealth, PaneStatus};
use sysinfo::System;

#[derive(Parser)]
#[command(name = "melina", about = "Claude Code process monitor")]
struct Cli {
    /// Output as JSON instead of human-readable
    #[arg(long)]
    json: bool,

    /// Watch mode — refresh every N seconds
    #[arg(short, long)]
    watch: Option<u64>,

    /// Show teams info
    #[arg(long)]
    teams: bool,

    /// Show orphan processes only
    #[arg(long)]
    orphans: bool,

    /// Kill zombie teams (remove dead team directories)
    #[arg(long)]
    kill_zombies: bool,

    /// Kill process by PID (sends SIGTERM, then SIGKILL after 5s)
    #[arg(long, value_name = "PID")]
    kill: Option<Vec<u32>>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    if let Some(pids) = &cli.kill {
        return kill_pids(pids);
    }

    if cli.kill_zombies {
        return kill_zombies();
    }

    if let Some(interval) = cli.watch {
        loop {
            print!("\x1B[2J\x1B[H");
            render(&cli)?;
            std::thread::sleep(std::time::Duration::from_secs(interval));
        }
    } else {
        render(&cli)?;
    }

    Ok(())
}

fn render(cli: &Cli) -> Result<()> {
    // Create System instance once for all health checks (expensive operation)
    let sys = System::new_all();
    let processes = scan();
    let trees = build_trees(processes);

    if cli.json {
        let output = if cli.teams {
            let teams = scan_teams();
            let health: Vec<_> = teams.iter().map(|t| check_team_health(t, &sys)).collect();
            serde_json::json!({ "sessions": trees, "teams": teams, "team_health": health })
        } else {
            serde_json::json!({ "sessions": trees })
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    if trees.is_empty() {
        println!("No active Claude Code sessions found.");
        return Ok(());
    }

    let total_sessions = trees.len();
    let total_children: usize = trees.iter().map(|t| t.children.len()).sum();
    let total_memory: u64 = trees.iter().map(|t| t.total_memory_bytes).sum();

    let now = Local::now().format("%Y-%m-%d %H:%M:%S");
    println!("╔═══════════════════════════════════════════════════════════╗");
    println!("║          melina — Claude Code Monitor                    ║");
    println!("║          {now}                                  ║");
    println!("╠═══════════════════════════════════════════════════════════╣");

    for (i, tree) in trees.iter().enumerate() {
        let config = tree.config_label();
        let uptime = format_uptime(tree.root.start_time);
        let mem = format_bytes(tree.total_memory_bytes);
        let flags = tree.root.cmd.iter()
            .filter(|c| c.starts_with("--"))
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");

        let started = format_timestamp(tree.root.start_time);
        println!("║                                                           ║");
        let cpu: f32 = tree.root.cpu_percent
            + tree.children.iter().map(|c| c.info.cpu_percent).sum::<f32>();
        let ver = tree.claude_version.as_deref().unwrap_or("?");
        println!("║  Session {} [PID {}] {:<20} {:>8}  ║",
            i + 1, tree.root.pid, config, mem);
        println!("║    version: {:<47} ║", ver);
        println!("║    started: {:<47} ║", started);
        println!("║    uptime:  {:<47} ║",
            format!("{}  CPU: {:.1}%  {}", uptime, cpu, flags));
        if let Some(ref cwd) = tree.working_dir {
            println!("║    cwd:     {:<47} ║", truncate_path(cwd, 47));
        }
        if let Some(ref sid) = tree.session_id {
            println!("║    session: {:<47} ║", sid);
        }
        if let Some(ref tmux) = tree.host_tmux {
            println!("║    tmux:    {:<47} ║",
                format!("{} (pane {} PID:{})", tmux, tmux.pane_id, tmux.server_pid));
        }

        // Teams with teammate health
        for team in &tree.teams {
            let report = check_team_health(team, &sys);
            let mates = team.teammates();
            let zombie_count = report.members.iter()
                .filter(|m| !m.health.is_healthy())
                .count();

            let team_status = if !report.owner_alive {
                " ZOMBIE-TEAM"
            } else if zombie_count > 0 {
                " (has issues)"
            } else {
                ""
            };

            println!("║    team:    {:<47} ║",
                format!("{} ({} mates, {} tasks){}",
                    team.name, mates.len(), team.task_count, team_status));

            for entry in &report.members {
                let health_icon = match &entry.health {
                    TeammateHealth::Active => "\x1b[32m●\x1b[0m",     // green
                    TeammateHealth::Completed => "\x1b[36m✓\x1b[0m",  // cyan
                    TeammateHealth::Zombie => "\x1b[31m✗\x1b[0m",     // red
                    TeammateHealth::Stale { .. } => "\x1b[33m◌\x1b[0m", // yellow
                    TeammateHealth::Stuck { .. } => "\x1b[31m!\x1b[0m", // red
                };

                // Find teammate member data for resource info
                let member = team.members.iter().find(|m| m.name == entry.name);
                let pid_str = member
                    .and_then(|m| m.tmux_pid)
                    .map(|p| format!("PID:{}", p))
                    .unwrap_or_default();
                let mem_str = member
                    .filter(|m| m.memory_bytes > 0)
                    .map(|m| format_bytes(m.memory_bytes))
                    .unwrap_or_default();
                let cpu_str = member
                    .filter(|m| m.tmux_pid.is_some())
                    .map(|m| format!("{:.1}%", m.cpu_percent))
                    .unwrap_or_default();
                let uptime_str = member
                    .filter(|m| m.start_time > 0)
                    .map(|m| format_uptime(m.start_time))
                    .unwrap_or_default();

                let health_str = format!("{}", entry.health);
                println!("║    ├─ {} {:<22} {:<8} {:<10} {:>8} {:>6} {} {} ║",
                    health_icon,
                    format!("MATE:{}", entry.name),
                    health_str,
                    pid_str, mem_str, cpu_str, uptime_str,
                    entry.agent_type);
            }
        }

        for child in &tree.children {
            let kind_label = match &child.kind {
                ChildKind::McpServer { server_name } => format!("MCP:{}", server_name),
                ChildKind::Teammate { name } => format!("TEAMMATE:{}", name.as_deref().unwrap_or("?")),
                ChildKind::HookScript => "HOOK".to_string(),
                ChildKind::BashTool => "BASH".to_string(),
                ChildKind::Unknown => "???".to_string(),
            };
            let child_mem = format_bytes(child.info.memory_bytes);
            let child_cpu = format!("{:.1}%", child.info.cpu_percent);
            let child_started = format_timestamp(child.info.start_time);
            println!("║    ├─ {:<22} PID:{:<6} {:>8} {:>6} {} ║",
                kind_label, child.info.pid, child_mem, child_cpu, child_started);
        }

        if tree.children.is_empty() && tree.teams.is_empty() {
            println!("║    └─ (no children)                                       ║");
        }
    }

    println!("║                                                           ║");
    println!("╠═══════════════════════════════════════════════════════════╣");
    println!("║  {} sessions │ {} children │ total: {:<23} ║",
        total_sessions, total_children, format_bytes(total_memory));
    println!("╚═══════════════════════════════════════════════════════════╝");

    // Show orphan teams (not matched to any live session)
    let all_teams = scan_teams();
    let orphan_teams: Vec<_> = all_teams.iter().filter(|t| {
        let report = check_team_health(t, &sys);
        !report.owner_alive
    }).collect();

    if !orphan_teams.is_empty() {
        println!();
        println!("\x1b[31mOrphan Teams (owner dead):\x1b[0m");
        for team in &orphan_teams {
            let config_label = team.config_dir.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "?".to_string());
            println!("  \x1b[31m✗\x1b[0m {} ({} members) [{}]",
                team.name, team.members.len(), config_label);
            println!("    dir: {}/teams/{}", team.config_dir.display(), team.name);
        }
        println!("  Run: melina --kill-zombies  to clean up");
    }

    // Show tmux servers (claude-swarm)
    let tmux_servers = scan_tmux_servers();
    if !tmux_servers.is_empty() {
        println!();
        println!("╔═══════════════════════════════════════════════════════════╗");
        println!("║          Tmux Servers (claude-swarm)                   ║");
        println!("╠═══════════════════════════════════════════════════════════╣");
        for srv in &tmux_servers {
            let status = if srv.lead_alive {
                "\x1b[32mACTIVE\x1b[0m"
            } else {
                "\x1b[31mORPHAN\x1b[0m"
            };
            let mem = format_bytes(srv.memory_bytes);
            let pid_str = srv.server_pid.map(|p| format!("PID:{}", p)).unwrap_or_default();
            let started = if srv.start_time > 0 { format_timestamp(srv.start_time) } else { String::new() };
            let uptime = if srv.start_time > 0 { format_uptime(srv.start_time) } else { String::new() };
            println!("║  {} {:<30} lead:{:<8} {:<10} {:>8} ║",
                status, srv.socket_name, srv.lead_pid, pid_str, mem);
            println!("║    started: {:<20} uptime: {:<26} ║",
                started, uptime);
            for pane in &srv.panes {
                let pane_status = match pane.status {
                    PaneStatus::Active => "\x1b[32m●\x1b[0m",  // green
                    PaneStatus::Idle   => "\x1b[33m◌\x1b[0m",  // yellow
                    PaneStatus::Done   => "\x1b[90m✓\x1b[0m",  // gray
                    PaneStatus::Shell  => "\x1b[90m·\x1b[0m",  // dim
                };
                let agent = pane.agent_name.as_deref().unwrap_or("shell");
                let claude_str = pane.claude_pid
                    .map(|p| format!("PID:{}", p))
                    .unwrap_or_default();
                let mem_str = if pane.memory_bytes > 0 {
                    format!("{:.1}MB", pane.memory_bytes as f64 / 1_048_576.0)
                } else {
                    String::new()
                };
                let team_label = pane.team_name.as_ref().map(|tn| {
                    if pane.team_exists {
                        format!("[{}]", tn)
                    } else {
                        format!("\x1b[31m[{} DELETED]\x1b[0m", tn)
                    }
                }).unwrap_or_default();
                let status_label = pane.status.label();
                println!("║    {} {:<20} {:<6} {:<10} {:>8} pane:{} sh:{} ║",
                    pane_status, agent, status_label, claude_str, mem_str, pane.pane_id, pane.shell_pid);
                if !team_label.is_empty() {
                    println!("║      team: {:<49} ║", team_label);
                }
            }
        }
        let orphan_count = tmux_servers.iter().filter(|s| s.is_orphan()).count();
        if orphan_count > 0 {
            println!("║                                                           ║");
            println!("║  \x1b[31m{} orphan server(s)\x1b[0m — run: melina --kill-zombies         ║",
                orphan_count);
        }
        println!("╚═══════════════════════════════════════════════════════════╝");
    }

    if cli.teams {
        let teams = scan_teams();
        if !teams.is_empty() {
            println!();
            println!("All Teams:");
            for team in &teams {
                let report = check_team_health(&team, &sys);
                let status = if report.owner_alive { "ALIVE" } else { "DEAD" };
                println!("  [{}] {} ({} members, {} tasks) @ {}",
                    status, team.name, team.members.len(), team.task_count,
                    team.config_dir.display());
                for member in &team.members {
                    println!("    - {} [{}]", member.name, member.agent_type);
                }
            }
        }
    }

    Ok(())
}

fn kill_pids(pids: &[u32]) -> Result<()> {
    use sysinfo::{System, Pid};

    let mut sys = System::new_all();
    sys.refresh_all();

    // Build tmux pane map to find which pane a process belongs to
    let tmux_servers = scan_tmux_servers();

    for &pid in pids {
        let sysinfo_pid = Pid::from_u32(pid);

        // First: check if this PID is a shell inside a claude-swarm tmux pane
        let tmux_pane_match = tmux_servers.iter().find_map(|srv| {
            srv.panes.iter().find_map(|pane| {
                if pane.shell_pid == pid || pane.claude_pid == Some(pid) {
                    Some((srv.socket_name.clone(), pane.pane_id.clone(), pane.shell_pid, pane.claude_pid, pane.agent_name.clone()))
                } else {
                    None
                }
            })
        });

        // If it's a tmux pane shell/process, kill the whole pane
        if let Some((socket, pane_id, shell_pid, claude_pid, agent_name)) = &tmux_pane_match {
            let label = agent_name.as_deref().unwrap_or("shell");
            println!("Killing tmux pane {} ({}) in {}…", pane_id, label, socket);

            // Kill claude process first if alive
            if let Some(cpid) = claude_pid {
                if let Some(proc_) = sys.process(Pid::from_u32(*cpid)) {
                    proc_.kill();
                    println!("  \x1b[32m✓\x1b[0m claude PID {} killed", cpid);
                }
            }

            // Kill the tmux pane (takes out shell + children)
            let digits = &pane_id[1..];
            if pane_id.starts_with('%') && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                let result = std::process::Command::new("tmux")
                    .args(["-L", socket, "kill-pane", "-t", pane_id])
                    .output();
                if result.is_ok_and(|o| o.status.success()) {
                    println!("  \x1b[32m✓\x1b[0m tmux pane {} killed (shell PID {})", pane_id, shell_pid);
                } else {
                    // Fallback: kill shell directly
                    if let Some(proc_) = sys.process(Pid::from_u32(*shell_pid)) {
                        if proc_.kill() {
                            println!("  \x1b[32m✓\x1b[0m shell PID {} killed (tmux kill-pane failed)", shell_pid);
                        }
                    }
                }
            }
            continue;
        }

        match sys.process(sysinfo_pid) {
            Some(proc_) => {
                let name = proc_.name().to_string_lossy().to_string();
                let cmd_str = proc_.cmd().iter()
                    .map(|s| s.to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join(" ");

                // Safety: only kill claude-related processes
                if !cmd_str.contains("claude") && !cmd_str.contains("--agent-id") && !name.contains("claude") {
                    println!("\x1b[33m!\x1b[0m PID {} ({}) is not a Claude process — skipping", pid, name);
                    println!("  cmd: {}…", &cmd_str[..cmd_str.len().min(100)]);
                    continue;
                }

                // Extract agent name if available
                let agent = cmd_str.split("--agent-name ")
                    .nth(1)
                    .and_then(|s| s.split_whitespace().next())
                    .unwrap_or(&name)
                    .to_string();

                println!("Killing PID {} ({})…", pid, agent);

                // SIGTERM first
                if proc_.kill() {
                    println!("  \x1b[32m✓\x1b[0m claude process killed");
                } else {
                    println!("  \x1b[31m✗\x1b[0m Failed to kill (permission denied?)");
                    continue;
                }
            }
            None => {
                println!("\x1b[33m!\x1b[0m PID {} not found — already dead?", pid);
            }
        }
    }

    Ok(())
}

fn kill_zombies() -> Result<()> {
    let sys = System::new_all();
    let teams = scan_teams();
    let mut cleaned = 0;

    for team in &teams {
        let report = check_team_health(team, &sys);
        if !report.owner_alive {
            let team_dir = team.config_dir.join("teams").join(&team.name);
            let tasks_dir = team.config_dir.join("tasks").join(&team.name);

            println!("\x1b[31m✗\x1b[0m Zombie team: {} (owner dead)", team.name);
            println!("  config:    {}", team.config_dir.display());
            println!("  team dir:  {}", team_dir.display());
            println!("  tasks dir: {}", tasks_dir.display());

            // Step 1: Kill any tmux teammates that might still be running
            for member in &team.members {
                if member.name == "team-lead" {
                    continue;
                }
                // tmux teammates have a real tmuxPaneId, not "in-process"
                if !member.tmux_pane_id.is_empty() {
                    // Validate pane ID format to prevent injection (must be % followed by one or more digits)
                    let pane_id = &member.tmux_pane_id;
                    let digits = &pane_id[1..];
                    if !pane_id.starts_with('%') || digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                        println!("  Skipping invalid pane ID for {}: {}", member.name, pane_id);
                        continue;
                    }
                    println!("  Killing tmux teammate: {} (pane: {})",
                        member.name, pane_id);
                    // Try to kill the tmux pane
                    let _ = std::process::Command::new("tmux")
                        .args(["kill-pane", "-t", pane_id])
                        .output();
                }
            }

            // Step 2: Remove filesystem artifacts (with path validation)
            if team_dir.exists() {
                // Canonicalize and validate path is under .claude directory
                match team_dir.canonicalize() {
                    Ok(canonical) => {
                        if canonical.to_string_lossy().contains("/.claude/") {
                            std::fs::remove_dir_all(&canonical)?;
                            println!("  \x1b[32m✓\x1b[0m team dir removed");
                        } else {
                            println!("  \x1b[33m!\x1b[0m Skipping team dir - path not under .claude: {}", canonical.display());
                        }
                    }
                    Err(e) => println!("  \x1b[33m!\x1b[0m Cannot canonicalize team dir: {}", e),
                }
            }
            if tasks_dir.exists() {
                // Canonicalize and validate path is under .claude directory
                match tasks_dir.canonicalize() {
                    Ok(canonical) => {
                        if canonical.to_string_lossy().contains("/.claude/") {
                            std::fs::remove_dir_all(&canonical)?;
                            println!("  \x1b[32m✓\x1b[0m tasks dir removed");
                        } else {
                            println!("  \x1b[33m!\x1b[0m Skipping tasks dir - path not under .claude: {}", canonical.display());
                        }
                    }
                    Err(e) => println!("  \x1b[33m!\x1b[0m Cannot canonicalize tasks dir: {}", e),
                }
            }
            cleaned += 1;
        }
    }

    // Also kill orphan tmux servers
    let tmux_servers = scan_tmux_servers();
    let mut tmux_cleaned = 0;
    for srv in &tmux_servers {
        if srv.is_orphan() {
            println!("\x1b[31m✗\x1b[0m Orphan tmux server: {} (lead {} dead, {} panes)",
                srv.socket_name, srv.lead_pid, srv.panes.len());
            if kill_tmux_server(&srv.socket_name) {
                println!("  \x1b[32m✓\x1b[0m killed tmux server");
                tmux_cleaned += 1;
            } else {
                println!("  \x1b[33m!\x1b[0m failed to kill tmux server");
            }
        }
    }

    if cleaned == 0 && tmux_cleaned == 0 {
        println!("No zombie teams or orphan tmux servers found. All clean.");
    } else {
        if cleaned > 0 {
            println!("\nCleaned up {} zombie team(s).", cleaned);
        }
        if tmux_cleaned > 0 {
            println!("Killed {} orphan tmux server(s).", tmux_cleaned);
        }
    }

    println!();
    println!("Note:");
    println!("  - In-process teammates cannot be killed individually");
    println!("    (they share the Node.js process with the team lead)");
    println!("  - To kill a stuck in-process team: kill <lead PID>");

    Ok(())
}

fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        return path.to_string();
    }
    // Show "…/last/two/parts"
    let parts: Vec<&str> = path.rsplitn(3, '/').collect();
    if parts.len() >= 2 {
        let suffix = &path[path.len() - (parts[0].len() + parts[1].len() + 1)..];
        let truncated = format!("…{}", suffix);
        if truncated.len() <= max_len {
            return truncated;
        }
    }
    format!("…{}", &path[path.len() - (max_len - 1)..])
}

fn format_bytes(bytes: u64) -> String {
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

fn format_timestamp(epoch: u64) -> String {
    use chrono::{DateTime, Local, TimeZone};
    Local
        .timestamp_opt(epoch as i64, 0)
        .single()
        .map(|dt: DateTime<Local>| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn format_uptime(start_time: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let elapsed = now.saturating_sub(start_time);
    let hours = elapsed / 3600;
    let mins = (elapsed % 3600) / 60;
    if hours > 0 {
        format!("{}h{}m", hours, mins)
    } else {
        format!("{}m", mins)
    }
}
