use crate::app::{App, ViewMode};
use crate::scanner::format_size;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Gauge, Paragraph, Row, Table, Wrap},
};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),   // System overview
            Constraint::Min(0),      // Process table
            Constraint::Length(4),   // Status bar
        ])
        .split(f.area());

    render_overview(f, app, chunks[0]);
    render_process_table(f, app, chunks[1]);
    render_status(f, app, chunks[2]);

    if app.detail_view_open {
        render_detail_popup(f, app);
    }
}

fn render_overview(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .title(" memo — Quick memory usage analysis")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Reset));
    f.render_widget(block.clone(), area);
    let inner = block.inner(area);

    let cols = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),   // RAM bar
            Constraint::Length(2),   // Swap bar
            Constraint::Length(2),   // Health + summary
        ])
        .split(inner);

    if let Some(mem) = &app.system_memory {
        // RAM bar
        let ram_pct = mem.used_pct() as u16;
        let ram_color = if ram_pct > 90 { Color::Red } else if ram_pct > 75 { Color::Yellow } else { Color::Green };
        let ram_label = format!(
            "{} / {} ({:.1}%)   App: {}  Wired: {}  Compressed: {}  Cache: {}",
            format_size(mem.used_bytes),
            format_size(mem.total_bytes),
            mem.used_pct(),
            format_size(mem.app_memory),
            format_size(mem.wired),
            format_size(mem.compressed),
            format_size(mem.cache),
        );
        let ram_gauge = Gauge::default()
            .gauge_style(Style::default().fg(ram_color))
            .percent(ram_pct.min(100))
            .label(Span::styled(ram_label, Style::default().fg(Color::White)));
        f.render_widget(ram_gauge, cols[0]);

        // Swap bar
        let swap_pct = mem.swap_pct() as u16;
        let swap_color = if swap_pct > 80 { Color::Red } else if swap_pct > 50 { Color::Yellow } else { Color::Green };
        let swap_label = format!(
            "{} / {} ({:.1}%)",
            format_size(mem.swap_used),
            format_size(mem.swap_total),
            mem.swap_pct(),
        );
        let swap_gauge = Gauge::default()
            .gauge_style(Style::default().fg(swap_color))
            .percent(swap_pct.min(100))
            .label(Span::styled(swap_label, Style::default().fg(Color::White)));
        f.render_widget(swap_gauge, cols[1]);

        // Health + summary
        let (health, health_color) = app.health_status();
        let warning = if mem.swap_pct() > 80.0 {
            format!(" ⚠ Swap at {:.0}% — heavy swapping likely", mem.swap_pct())
        } else {
            String::new()
        };

        let summary = Line::from(vec![
            Span::styled(" ● ", Style::default().fg(health_color)),
            Span::styled(format!("Health: {}  ", health), Style::default().fg(health_color).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("{} apps • {} procs • {} physical • {} swap{}",
                    app.groups.len(),
                    app.all_processes.len(),
                    format_size(app.total_phys),
                    format_size(app.total_swap),
                    warning,
                ),
                Style::default().fg(Color::White),
            ),
        ]);
        f.render_widget(Paragraph::new(summary), cols[2]);
    } else {
        let spinner = SPINNER[app.spinner_idx % SPINNER.len()];
        f.render_widget(
            Paragraph::new(format!("{} Loading system memory...", spinner)),
            cols[0],
        );
    }
}

fn render_process_table(f: &mut Frame, app: &mut App, area: Rect) {
    let title = match app.view_mode {
        ViewMode::Overview => " Top Apps (grouped by application) ",
        ViewMode::Ps => " All Processes (grouped) ",
    };

    let header_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let selected_style = Style::default().add_modifier(Modifier::REVERSED);

    let header_cells = ["APP", "PROCS", "RSS", "SWAP", "TOTAL", "THREADS"]
        .iter()
        .map(|h| Cell::from(*h).style(header_style));
    let header = Row::new(header_cells).height(1).bottom_margin(1);

    let expanded = app.expanded_group;

    let mut rows: Vec<Row> = Vec::new();
    for (gi, group) in app.groups.iter().enumerate() {
        let is_expanded = expanded == Some(gi);
        let prefix = if is_expanded { "▾ " } else { "▸ " };
        let name_style = if is_expanded {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else if group.processes.len() > 3 {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        let row = Row::new(vec![
            Cell::from(format!("{}{}", prefix, group.name)).style(name_style),
            Cell::from(group.processes.len().to_string()),
            Cell::from(format_size(group.total_footprint)).style(Style::default().fg(Color::Green)),
            Cell::from(format_size(group.total_swap)).style(swap_color(group.total_swap)),
            Cell::from(format_size(group.total())).style(Style::default().fg(Color::Magenta)),
            Cell::from(group.thread_count.to_string()),
        ]).height(1);

        rows.push(row);

        // If expanded, show child processes
        if is_expanded {
            for proc in &group.processes {
                let child_row = Row::new(vec![
                    Cell::from(format!("    PID {}", proc.pid)).style(Style::default().fg(Color::DarkGray)),
                    Cell::from(""),
                    Cell::from(format_size(proc.physical_footprint)).style(Style::default().fg(Color::DarkGray)),
                    Cell::from(format_size(proc.swap_disk)).style(Style::default().fg(Color::DarkGray)),
                    Cell::from(format_size(proc.total())).style(Style::default().fg(Color::DarkGray)),
                    Cell::from(proc.threads.to_string()).style(Style::default().fg(Color::DarkGray)),
                ]).height(1);
                rows.push(child_row);
            }
        }
    }

    let widths = [
        Constraint::Percentage(30),
        Constraint::Length(7),
        Constraint::Percentage(15),
        Constraint::Percentage(15),
        Constraint::Percentage(15),
        Constraint::Length(8),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(selected_style)
        .highlight_symbol(">> ");

    f.render_stateful_widget(table, area, &mut app.group_state);
}

fn swap_color(bytes: u64) -> Style {
    if bytes > 1024 * 1024 * 1024 {
        Style::default().fg(Color::Red)
    } else if bytes > 512 * 1024 * 1024 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Green)
    }
}

fn render_status(f: &mut Frame, app: &mut App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(area);

    // Controls
    let keys = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
    let desc = Style::default().fg(Color::DarkGray);

    let controls = vec![
        Line::from(vec![
            Span::styled("↑↓", keys), Span::styled(" Navigate  ", desc),
            Span::styled("Enter", keys), Span::styled(" Expand/Collapse  ", desc),
            Span::styled("D", keys), Span::styled(" Detail  ", desc),
            Span::styled("X", keys), Span::styled(" Kill  ", desc),
        ]),
        Line::from(vec![
            Span::styled("R", keys), Span::styled(" Refresh  ", desc),
            Span::styled("Q", keys), Span::styled(" Quit  ", desc),
            Span::styled("Sort: ", desc),
            Span::styled("T", keys), Span::styled("otal ", desc),
            Span::styled("P", keys), Span::styled("hys ", desc),
            Span::styled("S", keys), Span::styled("wap ", desc),
            Span::styled("N", keys), Span::styled("ame ", desc),
        ]),
    ];
    f.render_widget(
        Paragraph::new(controls).block(Block::default().borders(Borders::ALL).title("Controls")),
        chunks[0],
    );

    // Status / scan progress
    if let Some((current, total)) = app.deep_scan_progress {
        let pct = if total > 0 { ((current as f64 / total as f64) * 100.0) as u16 } else { 0 };
        let spinner = SPINNER[app.spinner_idx % SPINNER.len()];
        let gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("Deep Analysis"))
            .gauge_style(Style::default().fg(Color::Cyan))
            .percent(pct.min(100))
            .label(format!("{} Scanning {}/{} ({}%)", spinner, current, total, pct));
        f.render_widget(gauge, chunks[1]);
    } else if app.is_loading {
        let spinner = SPINNER[app.spinner_idx % SPINNER.len()];
        let msg = app.status_message.clone().unwrap_or_else(|| "Scanning...".into());
        let p = Paragraph::new(format!("{} {}", spinner, msg))
            .style(Style::default().fg(Color::Yellow))
            .block(Block::default().borders(Borders::ALL).title("Status"));
        f.render_widget(p, chunks[1]);
    } else {
        let msg = app.status_message.clone().unwrap_or_else(|| "Ready".into());
        let p = Paragraph::new(msg)
            .style(Style::default().fg(Color::Green))
            .block(Block::default().borders(Borders::ALL).title("Status"));
        f.render_widget(p, chunks[1]);
    }
}

fn render_detail_popup(f: &mut Frame, app: &mut App) {
    let area = centered_rect(70, 70, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Process Details (Esc to close) ")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black));
    f.render_widget(block.clone(), area);
    let inner = block.inner(area);

    if let Some(details) = &app.current_detail {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(4),
                Constraint::Min(0),
            ])
            .split(inner);

        // Header
        let header = vec![
            Line::from(vec![
                Span::styled("Name: ", Style::default().fg(Color::Yellow)),
                Span::raw(&details.name),
                Span::raw("   "),
                Span::styled("PID: ", Style::default().fg(Color::Yellow)),
                Span::raw(details.pid.to_string()),
                Span::raw("   "),
                Span::styled("Status: ", Style::default().fg(Color::Yellow)),
                Span::raw(&details.status),
            ]),
            Line::from(vec![
                Span::styled("CPU: ", Style::default().fg(Color::Yellow)),
                Span::raw(format!("{:.1}%", details.cpu_usage)),
                Span::raw("   "),
                Span::styled("Started: ", Style::default().fg(Color::Yellow)),
                Span::raw(format!("{}", details.start_time)),
            ]),
        ];
        f.render_widget(Paragraph::new(header), chunks[0]);

        // Command info
        let cmd_text = vec![
            Line::from(vec![
                Span::styled("Exe:  ", Style::default().fg(Color::Cyan)),
                Span::raw(&details.exe),
            ]),
            Line::from(vec![
                Span::styled("CWD:  ", Style::default().fg(Color::Cyan)),
                Span::raw(&details.cwd),
            ]),
            Line::from(vec![
                Span::styled("Cmd:  ", Style::default().fg(Color::Cyan)),
                Span::raw(details.cmd.join(" ")),
            ]),
        ];
        f.render_widget(Paragraph::new(cmd_text).wrap(Wrap { trim: true }), chunks[1]);

        // Memory
        let mem = &details.memory_info;
        let mem_text = vec![
            Line::from(Span::styled("── Memory ──", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
            Line::from(vec![
                Span::raw("Physical Footprint:  "),
                Span::styled(format_size(mem.physical_footprint), Style::default().fg(Color::Green)),
            ]),
            Line::from(vec![
                Span::raw("Compressed Memory:   "),
                Span::styled(format_size(mem.compressed), Style::default().fg(Color::Blue)),
            ]),
            Line::from(vec![
                Span::raw("Swap (Disk):         "),
                Span::styled(format_size(mem.swap_disk), Style::default().fg(Color::Red)),
            ]),
            Line::from(vec![
                Span::raw("Total Footprint:     "),
                Span::styled(format_size(mem.total()), Style::default().fg(Color::Magenta)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("Swapped Total:       "),
                Span::styled(format_size(mem.swapped_total), Style::default().fg(Color::DarkGray)),
            ]),
        ];
        f.render_widget(Paragraph::new(mem_text), chunks[2]);
    } else {
        let spinner = SPINNER[app.spinner_idx % SPINNER.len()];
        let p = Paragraph::new(format!("{} Fetching details...", spinner))
            .alignment(Alignment::Center);
        f.render_widget(p, inner);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
