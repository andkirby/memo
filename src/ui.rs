use crate::app::App;
use crate::group::GroupMode;
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
            Constraint::Length(6),   // System overview
            Constraint::Min(0),      // Process table
            Constraint::Length(4),   // Status bar
        ])
        .split(f.area());

    render_overview(f, app, chunks[0]);
    render_process_table(f, app, chunks[1]);
    app.table_area = chunks[1];
    render_status(f, app, chunks[2]);

    if app.detail_view_open {
        render_detail_popup(f, app);
    }
}

fn render_overview(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .style(Style::default().bg(Color::Reset));
    f.render_widget(block.clone(), area);
    let inner = block.inner(area);

    let cols = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),   // RAM segmented bar
            Constraint::Length(1),   // RAM label
            Constraint::Length(1),   // Swap bar
            Constraint::Length(1),   // Swap label + health
        ])
        .split(inner);

    if let Some(mem) = &app.system_memory {
        let bar_width = inner.width as usize;

        // ── RAM segmented bar (btop style) ──
        let total = mem.total_bytes.max(1) as f64;
        let app_pct = mem.app_memory as f64 / total;
        let wired_pct = mem.wired as f64 / total;
        let comp_pct = 0 as f64 / total;
        let cache_pct = mem.cache as f64 / total;
        let used_pct = app_pct + wired_pct + comp_pct + cache_pct;

        // Build segmented bar with unicode block characters
        let segments = [
            (app_pct,    Color::Rgb(220, 120, 60)),  // warm orange — app
            (wired_pct,  Color::Rgb(70, 180, 220)),  // cyan — wired
            (comp_pct,   Color::Rgb(180, 100, 220)), // purple — compressed
            (cache_pct,  Color::Rgb(80, 180, 120)),  // green — cache
        ];
        let filled: usize = (used_pct * bar_width as f64).round() as usize;
        let empty = bar_width.saturating_sub(filled);

        let mut bar_spans: Vec<Span> = Vec::new();
        let mut remaining = filled as f64;
        for (pct, color) in &segments {
            let w = (*pct * bar_width as f64).round() as usize;
            let w = w.min(remaining as usize);
            if w > 0 {
                bar_spans.push(Span::styled(
                    "▄".repeat(w),
                    Style::default().fg(*color),
                ));
                remaining -= w as f64;
            }
        }
        if empty > 0 {
            bar_spans.push(Span::styled(
                "─".repeat(empty),
                Style::default().fg(Color::Rgb(60, 60, 70)),
            ));
        }
        let ram_bar = Line::from(bar_spans);
        f.render_widget(Paragraph::new(ram_bar), cols[0]);

        // RAM label line
        let ram_label = Line::from(vec![
            Span::styled(" Mem ", Style::default().fg(Color::Rgb(220, 120, 60)).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("{:.1}%", used_pct * 100.0),
                Style::default().fg(if used_pct > 0.90 { Color::Red } else if used_pct > 0.75 { Color::Yellow } else { Color::Green }).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(format!("{} / {}", format_size(mem.used_bytes), format_size(mem.total_bytes)), Style::default().fg(Color::Rgb(180, 180, 190))),
            Span::raw("    "),
            Span::styled("■", Style::default().fg(Color::Rgb(220, 120, 60))),
            Span::styled(format!(" App {}", format_size(mem.app_memory)), Style::default().fg(Color::Rgb(140, 140, 150))),
            Span::raw("  "),
            Span::styled("■", Style::default().fg(Color::Rgb(70, 180, 220))),
            Span::styled(format!(" Wired {}", format_size(mem.wired)), Style::default().fg(Color::Rgb(140, 140, 150))),
            Span::raw("  "),
            Span::styled("■", Style::default().fg(Color::Rgb(180, 100, 220))),
            Span::styled(format!(" Compressed {}", format_size(0)), Style::default().fg(Color::Rgb(140, 140, 150))),
            Span::raw("  "),
            Span::styled("■", Style::default().fg(Color::Rgb(80, 180, 120))),
            Span::styled(format!(" Cache {}", format_size(mem.cache)), Style::default().fg(Color::Rgb(140, 140, 150))),
        ]);
        f.render_widget(Paragraph::new(ram_label), cols[1]);

        // ── Swap bar ──
        let swap_pct = mem.swap_pct() / 100.0;
        let swap_filled = (swap_pct * bar_width as f64).round() as usize;
        let swap_empty = bar_width.saturating_sub(swap_filled);
        let swap_color = if swap_pct > 0.80 { Color::Rgb(220, 60, 60) } else if swap_pct > 0.50 { Color::Rgb(220, 160, 60) } else { Color::Rgb(60, 180, 120) };

        let mut swap_bar_spans: Vec<Span> = Vec::new();
        if swap_filled > 0 {
            swap_bar_spans.push(Span::styled(
                "▄".repeat(swap_filled),
                Style::default().fg(swap_color),
            ));
        }
        if swap_empty > 0 {
            swap_bar_spans.push(Span::styled(
                "─".repeat(swap_empty),
                Style::default().fg(Color::Rgb(60, 60, 70)),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(swap_bar_spans)), cols[2]);

        // Swap label + health
        let (health, health_color) = app.health_status();
        let warning = if mem.swap_pct() > 80.0 { " ⚠ swapping" } else { "" };
        let noun = match app.group_mode { GroupMode::Project => "projects", GroupMode::App => "apps" };
        let swap_label = Line::from(vec![
            Span::styled(" Swap ", Style::default().fg(Color::Rgb(70, 180, 220)).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("{:.1}%", mem.swap_pct()),
                Style::default().fg(if swap_pct > 0.80 { Color::Red } else if swap_pct > 0.50 { Color::Yellow } else { Color::Green }).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(format!("{} / {}", format_size(mem.swap_used), format_size(mem.swap_total)), Style::default().fg(Color::Rgb(180, 180, 190))),
            Span::raw("   "),
            Span::styled("● ", Style::default().fg(health_color)),
            Span::styled(health, Style::default().fg(health_color).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(
                format!("{} {} · {} procs · {} phys · {} swap{}",
                    app.groups.len(),
                    noun,
                    app.all_processes.len(),
                    format_size(app.total_phys),
                    format_size(app.total_swap),
                    warning,
                ),
                Style::default().fg(Color::Rgb(130, 130, 140)),
            ),
        ]);
        f.render_widget(Paragraph::new(swap_label), cols[3]);
    } else {
        let spinner = SPINNER[app.spinner_idx % SPINNER.len()];
        f.render_widget(
            Paragraph::new(format!("{} Loading system memory...", spinner)),
            cols[0],
        );
    }
}
fn render_process_table(f: &mut Frame, app: &mut App, area: Rect) {
    let title = match app.group_mode {
        GroupMode::Project => " Top Projects (grouped by project dir) ",
        GroupMode::App => " Top Apps (grouped by application) ",
    };
    let (name_header, bridge_header) = match app.group_mode {
        GroupMode::Project => ("PROJECT", "RUNTIMES"),
        GroupMode::App => ("APP", "PROJECTS"),
    };

    let header_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let selected_style = Style::default().add_modifier(Modifier::REVERSED);

    let header_row = [name_header, "PROCS", bridge_header, "RSS", "SWAP", "TOTAL"];
    let header_cells = header_row
        .iter()
        .map(|h| Cell::from(*h).style(header_style));
    let header = Row::new(header_cells).height(1).bottom_margin(1);

    let expanded = app.expanded_group;

    let mut rows: Vec<Row> = Vec::new();
    let mut row_map: Vec<(usize, Option<usize>)> = Vec::new();
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

        let bridge = group.bridge(app.group_mode);
        let row = Row::new(vec![
            Cell::from(format!("{}{}", prefix, group.name)).style(name_style),
            Cell::from(group.processes.len().to_string()),
            Cell::from(bridge).style(Style::default().fg(Color::DarkGray)),
            Cell::from(format_size(group.total_rss)).style(Style::default().fg(Color::Green)),
            Cell::from(format_size(group.total_swap)).style(swap_color(group.total_swap)),
            Cell::from(format_size(group.total())).style(total_color(group.total())),
        ]).height(1);

        rows.push(row);
        row_map.push((gi, None));  // group header row

        // If expanded, show child processes
        if is_expanded {
            for (pi, proc) in group.processes.iter().enumerate() {
                let child_row = Row::new(vec![
                    Cell::from(format!("    PID {}", proc.pid)).style(Style::default().fg(Color::DarkGray)),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(format_size(proc.rss)).style(Style::default().fg(Color::DarkGray)),
                    Cell::from(format_size(proc.swap)).style(Style::default().fg(Color::DarkGray)),
                    Cell::from(format_size(proc.total())).style(total_color(proc.total())),
                ]).height(1);
                rows.push(child_row);
                row_map.push((gi, Some(pi)));  // sub-process row
            }
        }
    }

    // Store the row map so main.rs can use it for navigation
    app.row_map.entries = row_map;

    let widths = [
        Constraint::Percentage(26),
        Constraint::Length(6),
        Constraint::Percentage(26),
        Constraint::Percentage(13),
        Constraint::Percentage(13),
        Constraint::Percentage(13),
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

/// Color for total memory (phys + swap) — warm gradient from cool to hot
/// < 500M → teal, 500M-1G → green, 1-2G → yellow, 2-4G → orange, > 4G → red
fn total_color(bytes: u64) -> Style {
    let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    if gb >= 4.0 {
        Style::default().fg(Color::Rgb(220, 60, 60))      // red
    } else if gb >= 2.0 {
        Style::default().fg(Color::Rgb(220, 140, 50))      // orange
    } else if gb >= 1.0 {
        Style::default().fg(Color::Rgb(220, 200, 60))      // yellow
    } else if gb >= 0.5 {
        Style::default().fg(Color::Rgb(80, 190, 120))      // green
    } else {
        Style::default().fg(Color::Rgb(70, 170, 200))      // teal
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
            Span::styled("Enter", keys), Span::styled(" Expand  ", desc),
            Span::styled("D", keys), Span::styled(" Detail  ", desc),
            Span::styled("X", keys), Span::styled(" Kill  ", desc),
        ]),
        Line::from(vec![
            Span::styled("Tab", keys), Span::styled(" App/Project  ", desc),
            Span::styled("R", keys), Span::styled(" Refresh  ", desc),
            Span::styled("Sort: ", desc),
            Span::styled("T", keys), Span::styled(" ", desc),
            Span::styled("P", keys), Span::styled(" ", desc),
            Span::styled("S", keys), Span::styled(" ", desc),
            Span::styled("N", keys), Span::styled("  ", desc),
            Span::styled("Q", keys), Span::styled(" Quit", desc),
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
    let area = centered_rect(80, 75, f.area());
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
                Constraint::Length(3),   // header
                Constraint::Length(5),   // command info
                Constraint::Length(3),   // parent
                Constraint::Min(0),      // memory
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
        let cmd_str = details.cmd.join(" ");
        let cmd_text = vec![
            Line::from(vec![
                Span::styled("Cmd:  ", Style::default().fg(Color::Cyan)),
                Span::styled(&cmd_str, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("CWD:  ", Style::default().fg(Color::Cyan)),
                Span::raw(if details.cwd.is_empty() { "(unknown)" } else { &details.cwd }),
            ]),
            Line::from(vec![
                Span::styled("Exe:  ", Style::default().fg(Color::Cyan)),
                Span::raw(&details.exe),
            ]),
        ];
        f.render_widget(Paragraph::new(cmd_text).wrap(Wrap { trim: true }), chunks[1]);

        // Parent process
        let parent_text = if let Some(ppid) = details.parent_pid {
            let pcmd = details.parent_cmd.as_deref().unwrap_or("(unknown)");
            vec![
                Line::from(vec![
                    Span::styled("── Parent ──", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                ]),
                Line::from(vec![
                    Span::styled("PPID: ", Style::default().fg(Color::Yellow)),
                    Span::raw(ppid.to_string()),
                    Span::raw("   "),
                    Span::styled(pcmd, Style::default().fg(Color::White)),
                ]),
            ]
        } else {
            vec![
                Line::from(Span::styled("── Parent ──", Style::default().fg(Color::DarkGray))),
                Line::from("(no parent found)"),
            ]
        };
        f.render_widget(Paragraph::new(parent_text), chunks[2]);

        // Memory — same data as the table row (rss from sysinfo, swap from footprint)
        let mem_text = vec![
            Line::from(Span::styled("── Memory ──", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
            Line::from(vec![
                Span::raw("RSS (resident):  "),
                Span::styled(format_size(details.rss), Style::default().fg(Color::Green)),
            ]),
            Line::from(vec![
                Span::raw("Swap (disk):     "),
                Span::styled(format_size(details.swap), Style::default().fg(Color::Red)),
            ]),
            Line::from(vec![
                Span::raw("Total:           "),
                Span::styled(format_size(details.rss + details.swap), Style::default().fg(Color::Magenta)),
            ]),
        ];
        f.render_widget(Paragraph::new(mem_text), chunks[3]);
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
