use anyhow::Result;
use chrono::{Local, TimeZone};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use melina_core::{scan, build_trees, check_team_health, resolve_tmux_pids, scan_tmux_servers, ChildKind, SessionTree, TeammateHealth, TmuxServer, PaneStatus};
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
    let mut last_tick = Instant::now();
    let (mut trees, mut tmux_servers) = refresh();

    loop {
        terminal.draw(|frame| ui(frame, &trees, &tmux_servers))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => break,
                        KeyCode::Char('r') => { let r = refresh(); trees = r.0; tmux_servers = r.1; },
                        _ => {}
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            let r = refresh();
            trees = r.0;
            tmux_servers = r.1;
            last_tick = Instant::now();
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

fn refresh() -> (Vec<SessionTree>, Vec<TmuxServer>) {
    let mut trees = build_trees(scan());
    for tree in &mut trees {
        resolve_tmux_pids(&mut tree.teams);
    }
    let tmux_servers = scan_tmux_servers();
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

fn ui(frame: &mut Frame, trees: &[SessionTree], tmux_servers: &[TmuxServer]) {
    let area = frame.area();
    let sys = System::new_all();

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
        let sid_short = tree.session_id.as_deref()
            .map(|s| &s[..s.len().min(8)])
            .unwrap_or("");
        let tmux_label = tree.host_tmux.as_ref()
            .map(|t| format!("{} [{}]", t, t.server_pid))
            .unwrap_or_default();
        let info = if !sid_short.is_empty() {
            format!("{} MCP, {} mates | {} [{}…]",
                tree.mcp_count(), tree.teammate_count(), cwd_short, sid_short)
        } else if !cwd_short.is_empty() {
            format!("{} MCP, {} mates | {}",
                tree.mcp_count(), tree.teammate_count(), cwd_short)
        } else {
            format!("{} MCP, {} mates", tree.mcp_count(), tree.teammate_count())
        };

        let ver = tree.claude_version.as_deref().unwrap_or("?");
        let kind_str = format!("SESSION [{}]", ver);
        rows.push(Row::new(vec![
            format!("S{}", i + 1),
            format!("{}", tree.root.pid),
            kind_str,
            tree.config_label(),
            format!("{:.1}%", cpu),
            format!("{:.1}MB", tree.total_memory_bytes as f64 / 1_048_576.0),
            started,
            uptime,
            String::new(), // HEALTH
            tmux_label,
            info,
        ]).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));

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
                String::new(),
                String::new(), // HEALTH
                String::new(), // TMUX
                format!("{} mates, {} tasks{}", mates.len(), team.task_count, team_status),
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
                let pid_str = m.and_then(|m| m.tmux_pid)
                    .map(|p| format!("{}", p))
                    .unwrap_or_default();
                let cpu_str = m.filter(|m| m.tmux_pid.is_some())
                    .map(|m| format!("{:.1}%", m.cpu_percent))
                    .unwrap_or_default();
                let mem_str = m.filter(|m| m.memory_bytes > 0)
                    .map(|m| format!("{:.1}MB", m.memory_bytes as f64 / 1_048_576.0))
                    .unwrap_or_default();
                let started = m.filter(|m| m.start_time > 0)
                    .map(|m| format_ts(m.start_time))
                    .unwrap_or_default();
                let uptime = m.filter(|m| m.start_time > 0)
                    .map(|m| format_uptime(m.start_time))
                    .unwrap_or_default();
                rows.push(Row::new(vec![
                    "    ".to_string(),
                    pid_str,
                    format!("MATE:{}", entry.name),
                    String::new(),
                    cpu_str,
                    mem_str,
                    started,
                    uptime,
                    health_str,
                    String::new(), // TMUX
                    format!("{}", entry.agent_type),
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
                String::new(),
                format!("{:.1}%", child.info.cpu_percent),
                format!("{:.1}MB", child.info.memory_bytes as f64 / 1_048_576.0),
                child_started,
                child_uptime,
                String::new(), // HEALTH
                String::new(), // TMUX
                child.info.name.clone(),
            ]).style(style));
        }
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(6),   // #
            Constraint::Length(8),   // PID
            Constraint::Length(26),  // KIND (includes version)
            Constraint::Length(14),  // CONFIG
            Constraint::Length(8),   // CPU
            Constraint::Length(10),  // MEM
            Constraint::Length(20),  // STARTED
            Constraint::Length(8),   // UPTIME
            Constraint::Length(12),  // HEALTH
            Constraint::Length(30),  // TMUX
            Constraint::Fill(1),    // INFO
        ],
    )
    .header(
        Row::new(vec!["#", "PID", "KIND", "CONFIG", "CPU", "MEM", "STARTED", "UPTIME", "HEALTH", "TMUX", "INFO"])
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
    let footer = Paragraph::new(" q: quit | r: refresh | auto-refresh: 2s")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[footer_idx]);
}
