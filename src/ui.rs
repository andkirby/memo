use crate::app::{App, SortColumn};
use crate::group::GroupMode;
use crate::scanner::format_size;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Gauge, Paragraph, Row, Table, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Column layout shared by the Table widget and `bridge_column_width`, so the
/// width-aware RUNTIMES cell always matches what is actually rendered.
const TABLE_WIDTHS: [Constraint; 6] = [
    Constraint::Percentage(26),
    Constraint::Length(6),
    Constraint::Percentage(26),
    Constraint::Percentage(13),
    Constraint::Percentage(13),
    Constraint::Percentage(13),
];

const HIGHLIGHT_SYMBOL: &str = ">> ";

pub fn ui(f: &mut Frame, app: &mut App) {
    app.popup_area = None;   // cleared each frame; set by render_detail_popup
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
        let comp_pct = mem.compressed as f64 / total;
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
            Span::styled(format!(" Compressed {}", format_size(mem.compressed)), Style::default().fg(Color::Rgb(140, 140, 150))),
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
        GroupMode::Project => ("PROJECT", "RUNTIMES / COMMAND"),
        GroupMode::App => ("APP", "PROJECTS"),
    };

    let header_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let selected_style = Style::default().add_modifier(Modifier::REVERSED);

    let arrow = |col: SortColumn| sort_arrow(app.sort_column, col, app.sort_desc);
    let right = |s: String, style: Style| Cell::from(Line::styled(s, style).alignment(Alignment::Right));

    let header_row = [
        format!("{}{}", name_header, arrow(SortColumn::Name)),
        "PROCS".to_string(),
        bridge_header.to_string(),
        format!("RSS{}", arrow(SortColumn::Physical)),
        format!("SWAP{}", arrow(SortColumn::Swap)),
        format!("TOTAL{}", arrow(SortColumn::Total)),
    ];
    let header_cells = header_row.iter().enumerate().map(|(i, h)| {
        if i == 2 { Cell::from(h.as_str()).style(header_style) }  // bridge column stays left-aligned
        else { right(h.clone(), header_style) }
    });
    let header = Row::new(header_cells).height(1).bottom_margin(1);

    let cols = table_columns(area, app.group_state.selected().is_some());
    let bridge_width = cols[2].width as usize;
    let name_width = cols[0].width as usize;

    let mut rows: Vec<Row> = Vec::new();
    let mut row_map: Vec<(String, Option<i32>)> = Vec::new();
    for group in app.groups.iter() {
        let is_expanded = app.expanded_group.as_deref() == Some(group.name.as_str());
        let prefix = if is_expanded { "▾ " } else { "▸ " };
        // Expanded parent is highlighted; collapsed parents are bold so the
        // hierarchy reads even before any child rows are shown.
        let name_style = if is_expanded {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        };

        // App mode keeps bridge()'s human formatting ("3 projects"); Project
        // mode gets width-aware token fitting ("node:100 +2").
        let bridge_cell = if app.group_mode == GroupMode::App {
            group.bridge(app.group_mode)
        } else {
            fit_bridge_tokens(&group.bridge_tokens(app.group_mode), bridge_width)
        };
        let row = Row::new(vec![
            Cell::from(format!("{}{}", prefix, group.name)).style(name_style),
            right(group.processes.len().to_string(), Style::default()),
            Cell::from(bridge_cell).style(Style::default().fg(Color::DarkGray)),
            right(format_size(group.total_rss), Style::default().fg(Color::Green)),
            right(format_size(group.total_swap), swap_color(group.total_swap)),
            right(format_size(group.total()), total_color(group.total())),
        ]).height(1);

        rows.push(row);
        row_map.push((group.name.clone(), None));  // group header row

        // If expanded, show child processes
        if is_expanded {
            let n = group.processes.len();
            for (_pi, proc) in group.processes.iter().enumerate() {
                let guide = if _pi + 1 == n { "└─ " } else { "├─ " };
                // Reserve room for the pid suffix so long names clip, not the pid.
                let pid_span = format!(" · {}", proc.pid);
                let name_budget = name_width
                    .saturating_sub(2 + guide.width() + pid_span.width());
                let child_row = Row::new(vec![
                    // Name stays readable; only the tree guide and pid are dimmed.
                    Cell::from(Line::from(vec![
                        Span::styled(
                            format!("  {}{}", guide, truncate_ellipsis(&proc.name, name_budget)),
                            Style::default().fg(Color::White),
                        ),
                        Span::styled(pid_span, Style::default().fg(Color::DarkGray)),
                    ])),
                    Cell::from(""),
                    Cell::from(short_cmd(&proc.cmdline, bridge_width)).style(Style::default().fg(Color::DarkGray)),
                    right(format_size(proc.rss), Style::default().fg(Color::Green)),
                    right(format_size(proc.swap), swap_color(proc.swap)),
                    right(format_size(proc.total()), total_color(proc.total())),
                ]).height(1);
                rows.push(child_row);
                row_map.push((group.name.clone(), Some(proc.pid)));  // sub-process row
            }
        }
    }

    // Store the row map so main.rs can use it for navigation, then re-anchor
    // the visual selection on the selected identity — groups re-sort on every
    // scan, so a raw index can silently point at a different row.
    app.row_map.entries = row_map;
    app.resync_selection();

    let widths = TABLE_WIDTHS;

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(selected_style)
        .highlight_symbol(HIGHLIGHT_SYMBOL);

    f.render_stateful_widget(table, area, &mut app.group_state);
}

fn sort_arrow(active: SortColumn, col: SortColumn, desc: bool) -> &'static str {
    if active == col {
        if desc { " ↓" } else { " ↑" }
    } else {
        ""
    }
}

/// Widths of all six table columns, mirroring ratatui 0.29's Table layout:
/// the selection gutter is allocated first (`Length(gutter) + Fill(0)`), then
/// the shared `TABLE_WIDTHS` with `Flex::Start` and spacing 1 on what remains.
fn table_columns(area: Rect, has_selection: bool) -> Vec<Rect> {
    let inner_w = area.width.saturating_sub(2);  // block borders
    let [_gutter, columns] = Layout::horizontal([
        Constraint::Length(if has_selection { HIGHLIGHT_SYMBOL.len() as u16 } else { 0 }),
        Constraint::Fill(0),
    ])
    .areas(Rect::new(0, 0, inner_w, 1));
    Layout::horizontal(TABLE_WIDTHS)
        .flex(Flex::Start)
        .spacing(1)
        .split(columns)
        .to_vec()
}

/// Compact command for a child row: executable basename plus first argument
/// (so a leading flag stays visible), truncated to `max` display columns.
/// The cmdline is a flattened string, so whitespace splitting is a display
/// heuristic, not argv parsing.
fn short_cmd(cmdline: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let mut parts = cmdline.split_whitespace();
    let exe = parts.next().unwrap_or("").rsplit('/').next().unwrap_or("");
    if exe.is_empty() {
        return String::new();
    }
    let out = match parts.next() {
        Some(arg) => format!("{} {}", exe, arg),
        None => exe.to_string(),
    };
    truncate_ellipsis(&out, max)
}

/// Truncate `s` to `max` display columns with a trailing ellipsis when needed.
fn truncate_ellipsis(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut w = 0usize;
    let mut end = s.len();
    for (i, c) in s.char_indices() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max - 1 {
            end = i;
            break;
        }
        w += cw;
    }
    format!("{}…", &s[..end])
}

/// Fit whole `family:count` tokens into `width` display columns. When tokens
/// must be dropped, appends ` +N` where N counts the omitted families —
/// including any token displaced by the suffix itself. Falls back to `…` and
/// to empty at width 0.
fn fit_bridge_tokens(tokens: &[(String, usize)], width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let rendered: Vec<String> = tokens.iter().map(|(f, c)| format!("{}:{}", f, c)).collect();
    for keep in (0..=tokens.len()).rev() {
        let omitted = tokens.len() - keep;
        let mut line = rendered[..keep].join(" ");
        if omitted > 0 {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&format!("+{}", omitted));
        }
        if line.width() <= width {
            return line;
        }
    }
    "…".to_string()
}

fn swap_color(bytes: u64) -> Style {
    if bytes == 0 {
        Style::default().fg(Color::DarkGray)  // zero swap is neutral, not "good"
    } else if bytes > 1024 * 1024 * 1024 {
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
    app.popup_area = Some(area);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::GroupKind;
    use crate::scanner::ProcessMemory;
    use ratatui::{backend::TestBackend, Terminal};

    // ─── fit_bridge_tokens ──────────────────────────────────────────

    fn toks(pairs: &[(&str, usize)]) -> Vec<(String, usize)> {
        pairs.iter().map(|(f, c)| (f.to_string(), *c)).collect()
    }

    #[test]
    fn bridge_fits_exactly_and_drops_whole_tokens() {
        let t = toks(&[("esbuild", 100), ("node", 100), ("workerd", 100)]);
        // 11+1+9+1+12 = 34 display cols — all three fit whole only at width ≥ 34
        assert_eq!(fit_bridge_tokens(&t, 34), "esbuild:100 node:100 workerd:100");
        assert_eq!(fit_bridge_tokens(&t, 30), "esbuild:100 node:100 +1");
        assert_eq!(fit_bridge_tokens(&t, 19), "esbuild:100 +2");
        assert_eq!(fit_bridge_tokens(&t, 11), "+3");
        assert_eq!(fit_bridge_tokens(&t, 2), "+3");
        assert_eq!(fit_bridge_tokens(&t, 1), "…");
        assert_eq!(fit_bridge_tokens(&t, 0), "");
    }

    #[test]
    fn bridge_single_token_overflow_has_no_suffix() {
        let t = toks(&[("node", 1)]);
        assert_eq!(fit_bridge_tokens(&t, 3), "+1");
    }

    #[test]
    fn bridge_unicode_display_width() {
        // "日本:2" is 6 display columns (2 wide chars + ":2")
        let t = toks(&[("日本", 2), ("node", 1)]);
        assert_eq!(fit_bridge_tokens(&t, 13), "日本:2 node:1");
        assert_eq!(fit_bridge_tokens(&t, 12), "日本:2 +1");
        assert_eq!(fit_bridge_tokens(&t, 6), "+2");
    }

    #[test]
    fn bridge_empty_tokens() {
        assert_eq!(fit_bridge_tokens(&[], 10), "");
        assert_eq!(fit_bridge_tokens(&[], 0), "");
    }

    // ─── short_cmd ──────────────────────────────────────────────────

    #[test]
    fn cmd_strips_basename_keeps_flag() {
        assert_eq!(short_cmd("/opt/homebrew/bin/vite --host", 40), "vite --host");
        assert_eq!(short_cmd("node server.js", 40), "node server.js");
    }

    #[test]
    fn cmd_empty_and_truncation() {
        assert_eq!(short_cmd("", 10), "");
        assert_eq!(short_cmd("esbuild --service=0.18.0", 8), "esbuild…");
        assert_eq!(short_cmd("x", 0), "");
    }

    // ─── Rendered column width (TestBackend, amendment 2) ──────────

    fn bridge_group() -> crate::group::AppGroup {
        let mut p = |pid: i32, name: &str, rss: u64| {
            let mut x = ProcessMemory::new(pid, name.to_string(), rss);
            x.cmdline = format!("{} serve", name);
            x
        };
        crate::group::AppGroup {
            name: "~/demo".into(),
            kind: GroupKind::Project,
            processes: vec![p(1, "node", 100), p(2, "bun", 50), p(3, "workerd", 25)],
            total_rss: 175,
            total_swap: 0,
            thread_count: 3,
        }
    }

    /// Renders the table and returns the text of the first data row.
    fn rendered_first_row(width: u16, with_selection: bool) -> String {
        let mut app = App::new();
        app.groups = vec![bridge_group()];
        app.expanded_group = Some("~/demo".into());
        if with_selection {
            app.group_state.select(Some(0));
        }
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal
            .draw(|f| render_process_table(f, &mut app, f.area()))
            .unwrap();
        let buf = terminal.backend().buffer();
        // border(1) + header(1) + bottom_margin(1) → first data row is y=3
        (0..buf.area.width)
            .map(|x| buf[(x, 3)].symbol().to_string())
            .collect::<String>()
    }

    #[test]
    fn rendered_bridge_is_never_clipped_or_overlapped() {
        for width in [60u16, 100, 160] {
            for sel in [true, false] {
                let line = rendered_first_row(width, sel);
                let expected = fit_bridge_tokens(
                    &[("bun".into(), 1), ("node".into(), 1), ("workerd".into(), 1)],
                    table_columns(Rect::new(0, 0, width, 24), sel)[2].width as usize,
                );
                assert!(
                    line.contains(&expected),
                    "w={} sel={}: {:?} must contain {:?}",
                    width, sel, line, expected
                );
                // The fit must be tight: expected was computed for the real
                // column, so the row must also still hold the RSS value.
                assert!(line.contains("175B"), "w={} sel={}: {}", width, sel, line);
            }
        }
    }

    #[test]
    fn rendered_children_show_name_pid_and_command() {
        let child_line = {
            let mut app = App::new();
            app.groups = vec![bridge_group()];
            app.expanded_group = Some("~/demo".into());
            app.group_state.select(Some(1)); // first child row
            let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
            terminal
                .draw(|f| render_process_table(f, &mut app, f.area()))
                .unwrap();
            let buf = terminal.backend().buffer();
            (0..buf.area.width)
                .map(|x| buf[(x, 4)].symbol().to_string()) // row after the parent
                .collect::<String>()
        };
        assert!(child_line.contains("├─"), "guide: {}", child_line);
        assert!(child_line.contains("node"), "name: {}", child_line);
        assert!(child_line.contains("· 1"), "pid: {}", child_line);
        assert!(child_line.contains("node serve"), "cmd: {}", child_line);
    }

    #[test]
    fn header_shows_active_sort_arrow() {
        let mut app = App::new();
        app.groups = vec![bridge_group()];
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|f| render_process_table(f, &mut app, f.area()))
            .unwrap();
        let buf = terminal.backend().buffer();
        let header: String = (0..buf.area.width)
            .map(|x| buf[(x, 1)].symbol().to_string()) // y=1 is the header row
            .collect();
        assert!(header.contains("TOTAL ↓"), "default sort is total desc: {}", header);
        assert!(!header.contains("PROJECT ↓"), "{}", header);
        assert!(!header.contains("RSS ↓"), "{}", header);
    }

    #[test]
    fn long_child_name_keeps_pid_visible() {
        let mut g = bridge_group();
        g.processes.insert(
            0,
            ProcessMemory::new(56597, "Google Chrome Helper (Renderer)".into(), 40),
        );
        let mut app = App::new();
        app.groups = vec![g];
        app.expanded_group = Some("~/demo".into());
        app.group_state.select(Some(1)); // first child row
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|f| render_process_table(f, &mut app, f.area()))
            .unwrap();
        let buf = terminal.backend().buffer();
        let line: String = (0..buf.area.width)
            .map(|x| buf[(x, 4)].symbol().to_string())
            .collect();
        assert!(line.contains("56597"), "pid must survive: {}", line);
        assert!(line.contains("…"), "name must be clipped, not the pid: {}", line);
        assert!(!line.contains("(Renderer)"), "full name must be truncated: {}", line);
    }

    #[test]
    fn rendered_click_lands_on_displayed_row_after_scroll() {
        // 30 groups in a 24-row viewport: selecting item 26 forces a scroll
        // (offset > 0); a click on the first visible row must land on the
        // item displayed there (offset + 0), not item 0.
        let mut app = App::new();
        app.groups = (0..30)
            .map(|i| {
                let mut g = bridge_group();
                g.name = format!("~/g{}", i);
                g.total_rss = 1000 - i; // stable total-desc order g0..g29
                g
            })
            .collect();
        app.group_state.select(Some(26));
        let area = Rect::new(0, 0, 100, 24);
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|f| render_process_table(f, &mut app, f.area()))
            .unwrap();
        let offset = app.group_state.offset();
        assert!(offset > 0, "long list must have scrolled, offset={}", offset);

        app.table_area = area;
        app.click_table(3, 50); // first data row under the header
        assert_eq!(app.selected_process().map(|(name, _)| name), Some(format!("~/g{offset}")));
    }

    #[test]
    fn rendered_resort_keeps_highlight_on_clicked_group() {
        // The reported bug: click bb while com is expanded; a scan refresh
        // re-sorts groups; the highlight used to stay at the old physical
        // row and the expansion jumped to whatever landed there. Identity-
        // based selection must keep the highlight on bb and expansion on com.
        let mut com = bridge_group();
        com.name = "~/com".into();
        com.processes.insert(0, ProcessMemory::new(1, "worker".into(), 10));
        let mut bb = bridge_group();
        bb.name = "~/bb".into();
        let mut app = App::new();
        app.groups = vec![com, bb];
        app.expanded_group = Some("~/com".into());
        app.group_state.select(Some(0));
        let area = Rect::new(0, 0, 100, 24);
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| render_process_table(f, &mut app, f.area())).unwrap();

        // Click bb's header: com = 1 header + 4 children → bb at data row 5.
        app.table_area = area;
        app.click(3 + 5, 50, std::time::Instant::now());
        assert_eq!(app.selected_process().map(|(n, _)| n), Some("~/bb".into()));

        // Refresh re-sorts: bb (bigger total) jumps above com.
        app.groups.reverse();
        terminal.draw(|f| render_process_table(f, &mut app, f.area())).unwrap();
        assert_eq!(app.group_state.selected(), Some(0), "highlight follows bb to its new row");
        assert_eq!(app.selected_process().map(|(n, _)| n), Some("~/bb".into()));
        assert_eq!(app.expanded_group.as_deref(), Some("~/com"), "expansion follows com, not the position");
    }

    #[test]
    fn rendered_app_mode_bridge_shows_projects() {
        let real_home = std::env::var("HOME").unwrap_or_default();
        let tmp = std::env::temp_dir().join(format!("memo-ui-app-{}", std::process::id()));
        let a = tmp.join("projA");
        let b = tmp.join("projB");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("package.json"), "{}").unwrap();
        std::fs::write(b.join("Makefile"), "").unwrap();
        // SAFETY (test): no other running test reads HOME's value.
        unsafe { std::env::set_var("HOME", &tmp); }

        let proc_in = |pid: i32, dir: &std::path::Path| {
            let mut p = ProcessMemory::new(pid, "node".into(), 100);
            p.cwd = dir.to_string_lossy().to_string();
            p
        };

        for (procs, expected) in [
            (vec![proc_in(1, &a)], "~/projA".to_string()),
            (vec![proc_in(1, &a), proc_in(2, &b)], "2 projects".to_string()),
        ] {
            let mut app = App::new();
            app.group_mode = GroupMode::App;
            app.groups = vec![crate::group::AppGroup {
                name: "node".into(),
                kind: crate::group::GroupKind::App,
                processes: procs,
                total_rss: 100,
                total_swap: 0,
                thread_count: 1,
            }];
            let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
            terminal
                .draw(|f| render_process_table(f, &mut app, f.area()))
                .unwrap();
            let buf = terminal.backend().buffer();
            let line: String = (0..buf.area.width)
                .map(|x| buf[(x, 3)].symbol().to_string())
                .collect();
            assert!(line.contains(&expected), "expected {:?} in {}", expected, line);
        }

        unsafe { std::env::set_var("HOME", &real_home); }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
