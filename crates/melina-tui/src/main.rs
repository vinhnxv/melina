use anyhow::Result;
use chrono::{Local, TimeZone};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use melina_core::{scan, build_trees_with_context, create_process_system, refresh_process_system, check_team_health, scan_tmux_servers_with_snapshot, scan_zombies, kill_zombies, kill_process, ConfigDirCache, TmuxSnapshot, ChildKind, ClaudeSessionStatus, ZombieEntry, SessionTree, TeammateHealth, TmuxServer, PaneStatus};
use sysinfo::System;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Row, Table},
};
use std::io::stdout;
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let tick_rate = Duration::from_secs(2);
    let status_interval = Duration::from_secs(10);
    let mut last_tick = Instant::now();
    let mut last_status_refresh = Instant::now();
    let mut sys = create_process_system();
    let (mut trees, mut tmux_servers) = refresh_full(&mut sys);
    let mut status_msg: Option<(String, Instant)> = None;
    // Zombie confirmation dialog state
    let mut zombie_dialog: Option<Vec<ZombieEntry>> = None;
    // Kill-by-PID dialog state: list of selectable processes
    let mut kill_dialog: KillDialogState = KillDialogState::Closed;

    loop {
        terminal.draw(|frame| {
            ui(frame, &trees, &tmux_servers, status_msg.as_ref().map(|(s, _)| s.as_str()), &sys);
            if let Some(ref zombies) = zombie_dialog {
                draw_zombie_dialog(frame, zombies);
            }
            draw_kill_dialog(frame, &kill_dialog);
        })?;

        // Clear status message after 5 seconds
        if status_msg.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(5)) {
            status_msg = None;
        }

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    // Kill-by-PID dialog mode
                    if let KillDialogState::Selecting { ref entries, selected, .. } = kill_dialog {
                        let count = entries.len();
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => {
                                kill_dialog = KillDialogState::Closed;
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                kill_dialog.move_selection(count, -1);
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                kill_dialog.move_selection(count, 1);
                            }
                            KeyCode::Enter => {
                                if selected < count {
                                    let entry = entries[selected].clone();
                                    kill_dialog = KillDialogState::Confirm { entry };
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    if let KillDialogState::Confirm { ref entry } = kill_dialog {
                        let pid = entry.pid;
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                match kill_process(pid) {
                                    Ok(msg) => status_msg = Some((msg, Instant::now())),
                                    Err(msg) => status_msg = Some((msg, Instant::now())),
                                }
                                kill_dialog = KillDialogState::Closed;
                                let r = refresh_full(&mut sys);
                                trees = r.0;
                                tmux_servers = r.1;
                                last_status_refresh = Instant::now();
                            }
                            _ => {
                                kill_dialog = KillDialogState::Closed;
                                status_msg = Some(("Kill cancelled.".to_string(), Instant::now()));
                            }
                        }
                        continue;
                    }

                    // Zombie dialog mode — capture keys here first
                    if zombie_dialog.is_some() {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                let result = kill_zombies();
                                let msg = if result.total() == 0 {
                                    "No zombies killed (already gone?)".to_string()
                                } else {
                                    let mut parts = Vec::new();
                                    if result.teams_cleaned > 0 {
                                        parts.push(format!("{} team(s)", result.teams_cleaned));
                                    }
                                    if result.tmux_cleaned > 0 {
                                        parts.push(format!("{} tmux", result.tmux_cleaned));
                                    }
                                    if result.shells_cleaned > 0 {
                                        parts.push(format!("{} shell(s)", result.shells_cleaned));
                                    }
                                    if !result.errors.is_empty() {
                                        parts.push(format!("{} error(s)", result.errors.len()));
                                    }
                                    format!("Killed: {}", parts.join(", "))
                                };
                                status_msg = Some((msg, Instant::now()));
                                zombie_dialog = None;
                                let r = refresh_full(&mut sys);
                                trees = r.0;
                                tmux_servers = r.1;
                                last_status_refresh = Instant::now();
                            }
                            _ => {
                                // Any other key cancels
                                zombie_dialog = None;
                                status_msg = Some(("Kill cancelled.".to_string(), Instant::now()));
                            }
                        }
                        continue;
                    }

                    match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => break,
                        KeyCode::Char('r') => {
                            let r = refresh_full(&mut sys);
                            trees = r.0;
                            tmux_servers = r.1;
                            last_status_refresh = Instant::now();
                        },
                        KeyCode::Char('k') => {
                            let zombies = scan_zombies();
                            if zombies.is_empty() {
                                status_msg = Some(("No zombies found. All clean.".to_string(), Instant::now()));
                            } else {
                                zombie_dialog = Some(zombies);
                            }
                        },
                        KeyCode::Char('d') => {
                            let entries = build_killable_list(&trees, &tmux_servers);
                            if entries.is_empty() {
                                status_msg = Some(("No killable processes.".to_string(), Instant::now()));
                            } else {
                                kill_dialog = KillDialogState::Selecting { entries, selected: 0 };
                            }
                        },
                        _ => {}
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            // Don't auto-refresh while any dialog is open
            if zombie_dialog.is_none() && matches!(kill_dialog, KillDialogState::Closed) {
                let need_status = last_status_refresh.elapsed() >= status_interval;
                let r = if need_status {
                    last_status_refresh = Instant::now();
                    refresh_full(&mut sys)
                } else {
                    refresh_quick(&mut sys, &trees, &tmux_servers)
                };
                trees = r.0;
                tmux_servers = r.1;
                last_tick = Instant::now();
            }
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

/// Full refresh: process metrics + expensive status detection (capture-pane, jsonl).
fn refresh_full(sys: &mut System) -> (Vec<SessionTree>, Vec<TmuxServer>) {
    refresh_process_system(sys);
    let cache = ConfigDirCache::new();
    let snapshot = TmuxSnapshot::new();
    let trees = build_trees_with_context(scan(sys), sys, false, &cache, &snapshot);
    let tmux_servers = scan_tmux_servers_with_snapshot(sys, false, Some(&cache), &snapshot);
    (trees, tmux_servers)
}

/// Quick refresh: process metrics only, skips capture-pane/jsonl.
/// Merges cached status from previous full refresh.
fn refresh_quick(
    sys: &mut System,
    prev_trees: &[SessionTree],
    prev_tmux: &[TmuxServer],
) -> (Vec<SessionTree>, Vec<TmuxServer>) {
    refresh_process_system(sys);
    let cache = ConfigDirCache::new();
    let snapshot = TmuxSnapshot::new();
    let mut trees = build_trees_with_context(scan(sys), sys, true, &cache, &snapshot);
    let mut tmux_servers = scan_tmux_servers_with_snapshot(sys, true, Some(&cache), &snapshot);

    // Merge cached status from previous full refresh
    for tree in &mut trees {
        if let Some(prev) = prev_trees.iter().find(|t| t.root.pid == tree.root.pid) {
            tree.claude_status = prev.claude_status;
            if tree.git_context.is_none() {
                tree.git_context = prev.git_context.clone();
            }
        }
    }

    // Merge cached tmux pane data (last_line, status, team_exists) from previous full refresh
    for srv in &mut tmux_servers {
        if let Some(prev_srv) = prev_tmux.iter().find(|s| s.socket_name == srv.socket_name) {
            for pane in &mut srv.panes {
                if let Some(prev_pane) = prev_srv.panes.iter().find(|p| p.pane_id == pane.pane_id) {
                    if pane.last_line.is_none() {
                        pane.last_line = prev_pane.last_line.clone();
                    }
                    if pane.status != PaneStatus::Shell {
                        // Preserve richer status from full refresh (e.g. Done vs Idle)
                        pane.status = prev_pane.status.clone();
                    }
                    pane.team_exists = prev_pane.team_exists;
                }
            }
        }
    }

    (trees, tmux_servers)
}

fn format_ts(epoch: u64) -> String {
    Local
        .timestamp_opt(epoch as i64, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "—".to_string())
}

fn format_uptime(start_time: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::ZERO)
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

fn ui(frame: &mut Frame, trees: &[SessionTree], tmux_servers: &[TmuxServer], status_msg: Option<&str>, sys: &System) {
    let area = frame.area();

    let has_tmux = !tmux_servers.is_empty();
    let total_panes: usize = tmux_servers.iter().map(|s| s.panes.len()).sum();
    let tmux_height = (tmux_servers.len() + total_panes) as u16 + 4; // header + margin + rows
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if has_tmux {
            vec![
                Constraint::Length(3),   // header
                Constraint::Min(8),      // sessions table
                Constraint::Length(tmux_height.min(16)), // tmux servers
                Constraint::Length(3),   // footer
            ]
        } else {
            vec![
                Constraint::Length(3),  // header
                Constraint::Min(10),   // sessions table
                Constraint::Length(3), // footer
            ]
        })
        .split(area);

    // Header
    let total_sessions = trees.len();
    let total_children: usize = trees.iter().map(|t| t.children.len()).sum();
    let total_mem: u64 = trees.iter().map(|t| t.total_memory_bytes).sum();
    let now = Local::now().format("%Y-%m-%d %H:%M:%S");
    let header = Paragraph::new(format!(
        " melina — {} sessions | {} children | {:.0}MB total | {}",
        total_sessions,
        total_children,
        total_mem as f64 / 1_048_576.0,
        now
    ))
    .block(Block::default().borders(Borders::ALL).title(" Claude Code Monitor "));
    frame.render_widget(header, chunks[0]);

    // Build rows
    let mut rows = Vec::new();
    for (i, tree) in trees.iter().enumerate() {
        let started = format_ts(tree.root.start_time);
        let uptime = format_uptime(tree.root.start_time);
        let cpu: f32 = tree.root.cpu_percent
            + tree.children.iter().map(|c| c.info.cpu_percent).sum::<f32>();
        // Build info string with cwd and session ID
        let cwd_short = tree.working_dir.as_deref()
            .and_then(|p| p.rsplit('/').next())
            .unwrap_or("");
        let sid_short: String = tree.session_id.as_deref()
            .map(|s| s.chars().take(8).collect())
            .unwrap_or_default();
        let tmux_label = tree.host_tmux.as_ref()
            .map(|t| format!("[{}] {}", t.server_pid, t))
            .unwrap_or_default();
        let git_label = tree.git_context.as_ref()
            .map(|g| format!(" ({})", g.display()))
            .unwrap_or_default();
        let info = if !sid_short.is_empty() {
            format!("{} MCP, {} mates | {}{} [{}…]",
                tree.mcp_count(), tree.teammate_count(), cwd_short, git_label, sid_short)
        } else if !cwd_short.is_empty() {
            format!("{} MCP, {} mates | {}{}",
                tree.mcp_count(), tree.teammate_count(), cwd_short, git_label)
        } else {
            format!("{} MCP, {} mates", tree.mcp_count(), tree.teammate_count())
        };

        let ver = tree.claude_version.as_deref().unwrap_or("?");
        let config = tree.config_label();
        let kind_str = format!("SESSION [{}] [{}]", ver, config);
        let status_str = format!("{} {}", tree.claude_status.symbol(), tree.claude_status.label());
        let session_style = match tree.claude_status {
            ClaudeSessionStatus::Working => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ClaudeSessionStatus::Idle => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ClaudeSessionStatus::WaitingInput => Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            ClaudeSessionStatus::Unknown => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        };
        rows.push(Row::new(vec![
            format!("S{}", i + 1),
            format!("{}", tree.root.pid),
            kind_str,
            format!("{:.1}%", cpu),
            format!("{:.1}MB", tree.total_memory_bytes as f64 / 1_048_576.0),
            started,
            uptime,
            status_str,
            info,
            tmux_label,
        ]).style(session_style));

        // Teams & teammates (from config.json) with health
        for team in &tree.teams {
            let report = check_team_health(team, &sys);
            let mates = team.teammates();
            let unhealthy = report.members.iter().filter(|m| !m.health.is_healthy()).count();
            let team_status = if !report.owner_alive {
                " ZOMBIE"
            } else if unhealthy > 0 {
                " (issues)"
            } else {
                ""
            };
            rows.push(Row::new(vec![
                "  ".to_string(),
                String::new(),
                format!("TEAM:{}", team.name),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(), // STATUS
                format!("{} mates, {} tasks{}", mates.len(), team.task_count, team_status),
                String::new(), // TMUX
            ]).style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)));

            for entry in &report.members {
                let m = team.members.iter().find(|m| m.name == entry.name);
                let (health_str, style) = match &entry.health {
                    TeammateHealth::Active => ("ACTIVE".to_string(), Style::default().fg(Color::Green)),
                    TeammateHealth::Completed => ("DONE".to_string(), Style::default().fg(Color::Cyan)),
                    TeammateHealth::Zombie => ("ZOMBIE".to_string(), Style::default().fg(Color::Red)),
                    TeammateHealth::Stale { idle_secs } => {
                        (format!("STALE {}m", idle_secs / 60), Style::default().fg(Color::Yellow))
                    }
                    TeammateHealth::Stuck { task_ids } => {
                        (format!("STUCK({})", task_ids.len()), Style::default().fg(Color::Red))
                    }
                };
                // In-process agents share resources with lead — show lead's PID with ~ prefix
                let has_own_pid = m.and_then(|m| m.tmux_pid).is_some();
                let pid_str = if has_own_pid {
                    format!("{}", m.unwrap().tmux_pid.unwrap())
                } else {
                    format!("~{}", tree.root.pid)
                };
                let cpu_str = if has_own_pid {
                    m.map(|m| format!("{:.1}%", m.cpu_percent)).unwrap_or_default()
                } else {
                    String::new()
                };
                let mem_str = if has_own_pid {
                    m.filter(|m| m.memory_bytes > 0)
                        .map(|m| format!("{:.1}MB", m.memory_bytes as f64 / 1_048_576.0))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let started = if has_own_pid {
                    m.filter(|m| m.start_time > 0)
                        .map(|m| format_ts(m.start_time))
                        .unwrap_or_default()
                } else {
                    format_ts(tree.root.start_time)
                };
                let uptime = if has_own_pid {
                    m.filter(|m| m.start_time > 0)
                        .map(|m| format_uptime(m.start_time))
                        .unwrap_or_default()
                } else {
                    format_uptime(tree.root.start_time)
                };
                rows.push(Row::new(vec![
                    "    ".to_string(),
                    pid_str,
                    format!("MATE:{}", entry.name),
                    cpu_str,
                    mem_str,
                    started,
                    uptime,
                    health_str,
                    format!("{}", entry.agent_type),
                    String::new(), // TMUX
                ]).style(style));
            }
        }

        // Child processes
        for child in &tree.children {
            let kind_str = match &child.kind {
                ChildKind::McpServer { server_name } => format!("MCP:{}", server_name),
                ChildKind::Teammate { name } => format!("MATE:{}", name.as_deref().unwrap_or("?")),
                ChildKind::HookScript => "HOOK".to_string(),
                ChildKind::BashTool => "BASH".to_string(),
                ChildKind::Unknown => "???".to_string(),
            };
            let style = match &child.kind {
                ChildKind::McpServer { .. } => Style::default().fg(Color::Cyan),
                ChildKind::Teammate { .. } => Style::default().fg(Color::Green),
                ChildKind::HookScript => Style::default().fg(Color::Magenta),
                _ => Style::default().fg(Color::Blue).add_modifier(Modifier::DIM),
            };
            let child_started = format_ts(child.info.start_time);
            let child_uptime = format_uptime(child.info.start_time);
            rows.push(Row::new(vec![
                "  └─".to_string(),
                format!("{}", child.info.pid),
                kind_str,
                format!("{:.1}%", child.info.cpu_percent),
                format!("{:.1}MB", child.info.memory_bytes as f64 / 1_048_576.0),
                child_started,
                child_uptime,
                String::new(), // STATUS
                child.info.name.clone(),
                String::new(), // TMUX
            ]).style(style));
        }
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(6),   // #
            Constraint::Length(8),   // PID
            Constraint::Length(38),  // KIND (includes version + config)
            Constraint::Length(8),   // CPU
            Constraint::Length(10),  // MEM
            Constraint::Length(20),  // STARTED
            Constraint::Length(8),   // UPTIME
            Constraint::Length(12),  // STATUS
            Constraint::Fill(1),    // INFO
            Constraint::Length(30),  // TMUX
        ],
    )
    .header(
        Row::new(vec!["#", "PID", "KIND", "CPU", "MEM", "STARTED", "UPTIME", "STATUS", "INFO", "TMUX"])
            .style(Style::default().add_modifier(Modifier::BOLD))
            .bottom_margin(1),
    )
    .block(Block::default().borders(Borders::ALL).title(" Sessions "));

    frame.render_widget(table, chunks[1]);

    // Tmux servers table
    if has_tmux {
        let mut tmux_rows = Vec::new();
        for srv in tmux_servers {
            let status_style = if srv.lead_alive {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red)
            };
            let pid_str = srv.server_pid.map(|p| format!("{}", p)).unwrap_or_default();
            let mem_str = if srv.memory_bytes > 0 {
                format!("{:.1}KB", srv.memory_bytes as f64 / 1024.0)
            } else {
                String::new()
            };
            let started = if srv.start_time > 0 { format_ts(srv.start_time) } else { String::new() };
            let uptime_str = if srv.start_time > 0 { format_uptime(srv.start_time) } else { String::new() };
            tmux_rows.push(Row::new(vec![
                srv.socket_name.clone(),
                pid_str,
                format!("{}", srv.lead_pid),
                srv.label().to_string(),
                format!("{}", srv.panes.len()),
                mem_str,
                started,
                uptime_str,
                String::new(),
            ]).style(status_style));

            // Show each pane with agent details
            for pane in &srv.panes {
                let pane_style = match pane.status {
                    PaneStatus::Active => Style::default().fg(Color::Green),
                    PaneStatus::Idle => Style::default().fg(Color::Yellow),
                    PaneStatus::Done => Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                    PaneStatus::Shell => Style::default().fg(Color::Blue).add_modifier(Modifier::DIM),
                };
                let agent = pane.agent_name.as_deref().unwrap_or("shell");
                let claude_pid = pane.claude_pid
                    .map(|p| format!("{}", p))
                    .unwrap_or_default();
                let pane_mem = if pane.memory_bytes > 0 {
                    format!("{:.1}MB", pane.memory_bytes as f64 / 1_048_576.0)
                } else {
                    String::new()
                };
                let pane_cpu = if pane.claude_alive {
                    format!("{:.1}%", pane.cpu_percent)
                } else {
                    String::new()
                };
                let pane_started = if pane.start_time > 0 { format_ts(pane.start_time) } else { String::new() };
                let pane_uptime = if pane.start_time > 0 { format_uptime(pane.start_time) } else { String::new() };

                // Build info: team name (with deleted indicator) + last output
                let team_label = pane.team_name.as_ref().map(|tn| {
                    let short = tn.split('-').take(3).collect::<Vec<_>>().join("-");
                    if pane.team_exists {
                        short
                    } else {
                        format!("{} [DELETED]", short)
                    }
                }).unwrap_or_default();
                let last = pane.last_line.as_deref().unwrap_or("");
                let info = if !team_label.is_empty() && !last.is_empty() {
                    format!("{} | {}", team_label, last)
                } else if !team_label.is_empty() {
                    team_label
                } else {
                    last.to_string()
                };

                tmux_rows.push(Row::new(vec![
                    format!("  {} {}", pane.pane_id, agent),
                    claude_pid,
                    format!("sh:{}", pane.shell_pid),
                    pane.status.label().to_string(),
                    pane_cpu,
                    pane_mem,
                    pane_started,
                    pane_uptime,
                    info,
                ]).style(pane_style));
            }
        }

        let tmux_table = Table::new(
            tmux_rows,
            [
                Constraint::Length(28),  // SOCKET / PANE
                Constraint::Length(8),   // SRV PID / CLAUDE PID
                Constraint::Length(12),  // LEAD PID / SHELL PID
                Constraint::Length(8),   // STATUS
                Constraint::Length(7),   // PANES / CPU
                Constraint::Length(10),  // MEM
                Constraint::Length(20),  // STARTED
                Constraint::Length(8),   // UPTIME
                Constraint::Fill(1),    // AGENT TYPE
            ],
        )
        .header(
            Row::new(vec!["SOCKET/PANE", "PID", "LEAD/SHELL", "STATUS", "PANES", "MEM", "STARTED", "UPTIME", "TEAM / LAST OUTPUT"])
                .style(Style::default().add_modifier(Modifier::BOLD))
                .bottom_margin(1),
        )
        .block(Block::default().borders(Borders::ALL).title(" Tmux Servers (claude-swarm) "));

        frame.render_widget(tmux_table, chunks[2]);
    }

    // Footer
    let footer_idx = if has_tmux { 3 } else { 2 };
    let footer_text = if let Some(msg) = status_msg {
        format!(" {} | q: quit | r: refresh | k: kill zombies", msg)
    } else {
        " q: quit | r: refresh | k: kill zombies | d: kill PID | auto-refresh: 2s".to_string()
    };
    let footer_style = if status_msg.is_some() {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM)
    };
    let footer = Paragraph::new(footer_text)
        .style(footer_style)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[footer_idx]);
}

/// Draw a centered confirmation dialog listing zombie entries.
fn draw_zombie_dialog(frame: &mut Frame, zombies: &[ZombieEntry]) {
    use ratatui::widgets::{Clear, Wrap};

    let area = frame.area();

    // Size the dialog based on content
    let content_height = (zombies.len() as u16 * 2 + 6).min(area.height.saturating_sub(4));
    let dialog_width = 64.min(area.width.saturating_sub(4));
    let dialog = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(content_height),
            Constraint::Fill(1),
        ])
        .split(area);
    let dialog_area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(dialog_width),
            Constraint::Fill(1),
        ])
        .split(dialog[1])[1];

    // Clear the area behind the dialog
    frame.render_widget(Clear, dialog_area);

    // Build content
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(" Found {} zombie(s):", zombies.len()),
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    for (i, zombie) in zombies.iter().enumerate() {
        let (icon, label, detail) = match zombie {
            ZombieEntry::Team { name, member_count, task_count, config_dir } => {
                let config_label = config_dir.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "?".to_string());
                (
                    "✗",
                    format!("{}. TEAM: {}", i + 1, name),
                    format!("   {} members, {} tasks [{}] — owner dead", member_count, task_count, config_label),
                )
            }
            ZombieEntry::OrphanTmux { socket_name, lead_pid, pane_count, server_pid } => {
                let pid_str = server_pid.map(|p| format!(" PID:{}", p)).unwrap_or_default();
                (
                    "✗",
                    format!("{}. TMUX: {}", i + 1, socket_name),
                    format!("   lead:{} {} panes{} — lead dead", lead_pid, pane_count, pid_str),
                )
            }
            ZombieEntry::OrphanShell { socket_name, pane_id, shell_pid } => {
                (
                    "·",
                    format!("{}. SHELL: pane {} (sh:{})", i + 1, pane_id, shell_pid),
                    format!("   in {} — claude exited", socket_name),
                )
            }
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", icon), Style::default().fg(Color::Red)),
            Span::styled(label, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(Span::styled(
            detail,
            Style::default().fg(Color::Gray),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" y", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Span::styled(": kill all  ", Style::default().fg(Color::White)),
        Span::styled("any other key", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled(": cancel", Style::default().fg(Color::White)),
    ]));

    let dialog_widget = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(" Kill Zombies? ")
                .title_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        )
        .style(Style::default().bg(Color::Black));

    frame.render_widget(dialog_widget, dialog_area);
}

// ── Kill-by-PID Dialog ──────────────────────────────────────────

/// A killable process entry from the current monitor view.
#[derive(Debug, Clone)]
struct KillableEntry {
    pid: u32,
    label: String,    // e.g. "SESSION [2.1.74] [.claude-true]", "MCP:echo-search"
    detail: String,   // e.g. "melina (main *) 291.3MB"
    status: String,   // e.g. "working", "ACTIVE", "IDLE"
    uptime: String,   // e.g. "8h58m", "37m"
}

/// State machine for the kill-by-PID dialog.
enum KillDialogState {
    Closed,
    Selecting { entries: Vec<KillableEntry>, selected: usize },
    Confirm { entry: KillableEntry },
}

impl KillDialogState {
    fn move_selection(&mut self, count: usize, delta: isize) {
        if let KillDialogState::Selecting { selected, .. } = self {
            if count == 0 { return; }
            let new = (*selected as isize + delta).rem_euclid(count as isize) as usize;
            *selected = new;
        }
    }
}

/// Build a list of all killable processes from the current monitor state.
fn build_killable_list(trees: &[SessionTree], tmux_servers: &[TmuxServer]) -> Vec<KillableEntry> {
    let mut entries = Vec::new();

    // Sessions and their children
    for tree in trees {
        let cwd_short = tree.working_dir.as_deref()
            .and_then(|p| p.rsplit('/').next())
            .unwrap_or("");
        let git = tree.git_context.as_ref()
            .map(|g| format!(" ({})", g.display()))
            .unwrap_or_default();
        let mem = format!("{:.1}MB", tree.total_memory_bytes as f64 / 1_048_576.0);

        let ver = tree.claude_version.as_deref().unwrap_or("?");
        let config = tree.config_label();
        let uptime = format_uptime(tree.root.start_time);

        entries.push(KillableEntry {
            pid: tree.root.pid,
            label: format!("SESSION [{}] [{}]", ver, config),
            detail: format!("{}{} {}", cwd_short, git, mem),
            status: tree.claude_status.label().to_string(),
            uptime,
        });

        for child in &tree.children {
            let kind = match &child.kind {
                ChildKind::McpServer { server_name } => format!("MCP:{}", server_name),
                ChildKind::Teammate { name } => format!("MATE:{}", name.as_deref().unwrap_or("?")),
                ChildKind::HookScript => "HOOK".to_string(),
                ChildKind::BashTool => "BASH".to_string(),
                ChildKind::Unknown => "???".to_string(),
            };
            let mem = format!("{:.1}MB", child.info.memory_bytes as f64 / 1_048_576.0);
            entries.push(KillableEntry {
                pid: child.info.pid,
                label: kind,
                detail: format!("{} {}", child.info.name, mem),
                status: child.health.label().to_string(),
                uptime: format_uptime(child.info.start_time),
            });
        }
    }

    // Tmux swarm servers + individual panes
    for srv in tmux_servers {
        // Server entry (kills entire swarm)
        if let Some(server_pid) = srv.server_pid {
            let status = if srv.lead_alive { "ACTIVE" } else { "ORPHAN" };
            entries.push(KillableEntry {
                pid: server_pid,
                label: format!("SWARM:{}", srv.socket_name),
                detail: format!("lead:{} {} panes {:.1}KB", srv.lead_pid, srv.panes.len(), srv.memory_bytes as f64 / 1024.0),
                status: status.to_string(),
                uptime: String::new(),
            });
        }

        // Individual panes with claude processes
        for pane in &srv.panes {
            if let Some(claude_pid) = pane.claude_pid {
                let agent = pane.agent_name.as_deref().unwrap_or("shell");
                let mem = if pane.memory_bytes > 0 {
                    format!("{:.1}MB", pane.memory_bytes as f64 / 1_048_576.0)
                } else {
                    String::new()
                };
                let team = pane.team_name.as_deref().unwrap_or("");
                entries.push(KillableEntry {
                    pid: claude_pid,
                    label: format!("  PANE:{}", agent),
                    detail: format!("pane:{} {} {}", pane.pane_id, team, mem),
                    status: pane.status.label().to_string(),
                    uptime: String::new(),
                });
            }
        }
    }

    entries
}

/// Draw the kill-by-PID selection or confirmation dialog.
fn draw_kill_dialog(frame: &mut Frame, state: &KillDialogState) {
    use ratatui::widgets::{Cell, Clear, Table, TableState, Wrap};

    match state {
        KillDialogState::Closed => {}
        KillDialogState::Selecting { entries, selected } => {
            let area = frame.area();
            // Use 90% of terminal width and up to 80% height (+2 for border, +1 for header)
            let dialog_width = (area.width * 9 / 10).max(60);
            let list_height = (entries.len() as u16 + 4).min(area.height * 4 / 5);
            let dialog_area = centered_rect(dialog_width, list_height, area);

            frame.render_widget(Clear, dialog_area);

            let hdr_style = Style::default().fg(Color::Rgb(88, 110, 117)).add_modifier(Modifier::BOLD);
            let header = Row::new(vec![
                Cell::from("PID").style(hdr_style),
                Cell::from("KIND").style(hdr_style),
                Cell::from("STATUS").style(hdr_style),
                Cell::from("UPTIME").style(hdr_style),
                Cell::from("DETAIL").style(hdr_style),
            ]);

            let rows: Vec<Row> = entries.iter().enumerate().map(|(i, e)| {
                let marker = if i == *selected { "> " } else { "  " };
                Row::new(vec![
                    Cell::from(format!("{}{}", marker, e.pid))
                        .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Cell::from(e.label.as_str())
                        .style(Style::default().fg(Color::Cyan)),
                    Cell::from(e.status.as_str())
                        .style(Style::default().fg(Color::Green)),
                    Cell::from(e.uptime.as_str())
                        .style(Style::default().fg(Color::Yellow)),
                    Cell::from(e.detail.as_str())
                        .style(Style::default().fg(Color::Gray)),
                ])
            }).collect();

            let widths = [
                Constraint::Length(12),
                Constraint::Length(34),
                Constraint::Length(10),
                Constraint::Length(8),
                Constraint::Fill(1),
            ];

            let table = Table::new(rows, widths)
                .header(header)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Yellow))
                        .title(" Kill Process (↑↓ select, Enter confirm, Esc cancel) ")
                        .title_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                )
                .row_highlight_style(Style::default().bg(Color::DarkGray))
                .style(Style::default().bg(Color::Black));

            let mut table_state = TableState::default();
            table_state.select(Some(*selected));
            frame.render_stateful_widget(table, dialog_area, &mut table_state);
        }
        KillDialogState::Confirm { entry } => {
            let area = frame.area();
            let dialog_area = centered_rect(60, 8, area);

            frame.render_widget(Clear, dialog_area);

            let lines = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(" Kill ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("PID:{}", entry.pid), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" ({})?", entry.label), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(Span::styled(
                    format!(" {} — {}", entry.status, entry.detail),
                    Style::default().fg(Color::Gray),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled(" y", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                    Span::styled(": kill  ", Style::default().fg(Color::White)),
                    Span::styled("any other key", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                    Span::styled(": cancel", Style::default().fg(Color::White)),
                ]),
            ];

            let widget = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Red))
                        .title(" Confirm Kill ")
                        .title_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                )
                .style(Style::default().bg(Color::Black));

            frame.render_widget(widget, dialog_area);
        }
    }
}

/// Create a centered rect of given width and height within `area`.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height),
            Constraint::Fill(1),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(width),
            Constraint::Fill(1),
        ])
        .split(v[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    // ── format_ts() tests ─────────────────────────────────────────────

    #[test]
    fn test_format_ts_valid() {
        // Known epoch: 2024-01-15 12:30:45 UTC
        // This is a fixed timestamp that should format consistently
        let epoch: u64 = 1705319445;
        let result = format_ts(epoch);
        // The result should be in format "YYYY-MM-DD HH:MM:SS"
        // Exact value depends on timezone, but we can verify the format
        assert!(
            result.len() == 19,
            "Expected format 'YYYY-MM-DD HH:MM:SS' (19 chars), got '{}' ({} chars)",
            result,
            result.len()
        );
        // Verify the format pattern: digits and separators
        let parts: Vec<&str> = result.split(' ').collect();
        assert_eq!(parts.len(), 2, "Should have date and time parts");
        let date_parts: Vec<&str> = parts[0].split('-').collect();
        assert_eq!(date_parts.len(), 3, "Date should have 3 parts");
        let time_parts: Vec<&str> = parts[1].split(':').collect();
        assert_eq!(time_parts.len(), 3, "Time should have 3 parts");
    }

    #[test]
    fn test_format_ts_zero() {
        // Epoch 0 (Unix epoch start: 1970-01-01 00:00:00 UTC)
        // Depending on timezone, this may be 1969 or 1970
        let result = format_ts(0);
        // Should either be a valid date string or "—" for invalid
        // chrono's timestamp_opt returns None for ambiguous times,
        // but epoch 0 should be valid in most cases
        assert!(
            result == "—" || result.contains('-'),
            "Expected '—' or a date string, got '{}'",
            result
        );
    }

    #[test]
    fn test_format_ts_current() {
        // Current time should format correctly
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = format_ts(now_epoch);
        // Should produce a valid date string, not "—"
        assert_ne!(
            result, "—",
            "Current time should produce a valid date string"
        );
        // Should be in correct format
        assert!(
            result.len() == 19,
            "Expected format 'YYYY-MM-DD HH:MM:SS' (19 chars), got '{}'",
            result
        );
    }

    // ── format_uptime() tests ─────────────────────────────────────────

    #[test]
    fn test_format_uptime_minutes_only() {
        // Test uptime less than 1 hour (e.g., 37 minutes)
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // Start time 37 minutes ago
        let start_time = now - (37 * 60);
        let result = format_uptime(start_time);
        assert!(
            result.starts_with("37m"),
            "Expected '37m' format, got '{}'",
            result
        );
        assert!(
            !result.contains('h'),
            "Should not contain hours for < 1 hour uptime, got '{}'",
            result
        );
    }

    #[test]
    fn test_format_uptime_hours_and_minutes() {
        // Test uptime >= 1 hour (e.g., 2 hours 15 minutes)
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // Start time 2 hours 15 minutes ago (135 minutes total)
        let start_time = now - (2 * 3600 + 15 * 60);
        let result = format_uptime(start_time);
        assert!(
            result.contains('h') && result.contains('m'),
            "Expected 'NhNm' format, got '{}'",
            result
        );
        // Should show "2h15m"
        assert!(
            result == "2h15m",
            "Expected '2h15m', got '{}'",
            result
        );
    }

    #[test]
    fn test_format_uptime_zero() {
        // Start time = now should give 0 minutes uptime
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // Use a start time very close to now (might be slightly in the past due to timing)
        let result = format_uptime(now);
        assert!(
            result == "0m",
            "Expected '0m' for same start time, got '{}'",
            result
        );
    }

    // ── centered_rect() tests ─────────────────────────────────────────

    #[test]
    fn test_centered_rect_basic() {
        // Test with a standard terminal size
        let area = Rect::new(0, 0, 100, 50);
        let result = centered_rect(60, 20, area);

        // Should be centered horizontally: x should be around (100 - 60) / 2 = 20
        assert!(
            result.x == 20,
            "Expected x=20 for centered 60-width in 100-wide area, got x={}",
            result.x
        );
        // Should be centered vertically: y should be around (50 - 20) / 2 = 15
        assert!(
            result.y == 15,
            "Expected y=15 for centered 20-height in 50-tall area, got y={}",
            result.y
        );
        // Width and height should match requested values
        assert_eq!(
            result.width, 60,
            "Expected width=60, got {}",
            result.width
        );
        assert_eq!(
            result.height, 20,
            "Expected height=20, got {}",
            result.height
        );
    }

    #[test]
    fn test_centered_rect_small_area() {
        // Test when the requested rect is larger than available area
        // Note: Constraint::Length clamps to available space, so the output
        // will be smaller than requested when the area is too small
        let area = Rect::new(0, 0, 40, 10);
        let result = centered_rect(60, 20, area);

        // The layout will clamp to available area since 60 > 40 and 20 > 10
        // Width should be clamped to available width (40)
        assert!(
            result.width <= area.width,
            "Width should be clamped to available area, got width={}, area width={}",
            result.width,
            area.width
        );
        // Height should be clamped to available height (10)
        assert!(
            result.height <= area.height,
            "Height should be clamped to available area, got height={}, area height={}",
            result.height,
            area.height
        );
        // X and Y should still be computed (will be 0 when centered in small area)
        assert!(
            result.x < area.width,
            "X should be within bounds"
        );
        assert!(
            result.y < area.height,
            "Y should be within bounds"
        );
    }

    #[test]
    fn test_centered_rect_width_height() {
        // Verify that output has correct width and height
        let area = Rect::new(0, 0, 80, 24);
        let result = centered_rect(30, 10, area);

        assert_eq!(
            result.width, 30,
            "Output width should match requested width 30, got {}",
            result.width
        );
        assert_eq!(
            result.height, 10,
            "Output height should match requested height 10, got {}",
            result.height
        );

        // Verify centering math
        // x should be (80 - 30) / 2 = 25 (roughly, depends on layout)
        // y should be (24 - 10) / 2 = 7 (roughly)
        assert!(
            result.x < 80 - 30 || result.x >= 25 - 1 && result.x <= 25 + 1,
            "X should be approximately centered, got {}",
            result.x
        );
    }
}
