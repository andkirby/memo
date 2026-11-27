mod app;
mod deep;
mod footprint;
mod group;
mod inspect;
mod scanner;
mod termination;
mod ui;

#[cfg(test)]
mod termination_tests;

use std::{
    collections::HashMap,
    fs::File,
    io, io::Write,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use app::{App, DeepPhase, DeepProgress, ProcessDetail, SortColumn};
use deep::{GroupDeep, GroupKey, Metric, ProcessDeep};
use clap::{CommandFactory, Parser, Subcommand, error::ErrorKind};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use scanner::{ProcessMemory, format_size, get_system_memory};
use sysinfo::{Pid, System};
use termination::ProcessIdentity;

#[derive(Parser, Debug)]
#[command(name = "memo", about = "Quick memory usage analysis", version, disable_version_flag = true,
    args_conflicts_with_subcommands = true)]
struct Args {
    /// Print version
    #[arg(short = 'v', long, action = clap::ArgAction::Version)]
    version: Option<bool>,

    /// CLI mode (dump to stdout and exit)
    #[arg(long)]
    cli: bool,

    /// RSS-only process scan (requires --cli, skips footprint)
    #[arg(long, requires = "cli")]
    quick: bool,

    /// Show ungrouped process list
    #[arg(long)]
    ps: bool,

    /// Sort by total (default: Apple footprint), rss, swap (compressed or
    /// swapped), or name. `footprint` is accepted for total.
    /// With --quick, use rss (default) or name.
    #[arg(long)]
    sort: Option<String>,

    /// Group mode for --cli: app, project
    #[arg(long, default_value = "app")]
    group: String,

    #[command(subcommand)]
    command: Option<Command>,
}

impl Args {
    fn sort_column(&self) -> Result<SortColumn, clap::Error> {
        match (self.quick, self.sort.as_deref()) {
            (_, Some("rss")) | (true, None) => Ok(SortColumn::Physical),
            (_, Some("name")) => Ok(SortColumn::Name),
            (false, Some("swap")) => Ok(SortColumn::CompressedOrSwapped),
            (false, _) => Ok(SortColumn::Footprint),
            (true, Some(value)) => Err(Self::command().error(
                ErrorKind::InvalidValue,
                format!("--sort {value} is unavailable with --quick. Use rss or name."),
            )),
        }
    }

    fn group_mode(&self) -> group::GroupMode {
        match self.group.as_str() {
            "project" => group::GroupMode::Project,
            _ => group::GroupMode::App,
        }
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Project-scoped memory snapshot for agents (RSS only)
    Inspect {
        /// Machine-readable JSON on stdout (single line)
        #[arg(long)]
        json: bool,

        /// Project directory (default: nearest enclosing .git / marker)
        #[arg(long, value_name = "PATH")]
        project: Option<String>,

        /// Max processes to list, >= 1
        #[arg(long, value_name = "N", default_value = "5")]
        limit: String,
    },
}

// ─── Scan events (background → main thread) ────────────────────────────────

// Scan events carry the scan id; the TUI drops events from an older scan.
enum ScanEvent {
    SystemMemory(scanner::SystemMemory),
    Processes(u64, Vec<ProcessMemory>),
    Progress(u64, DeepProgress),
    ProcessDeep(u64, Vec<(i32, ProcessDeep)>, DeepProgress),
    GroupDeep(u64, GroupKey, GroupDeep, DeepProgress),
    DetailResult(ProcessDetail),
    Done(u64),
}

// ─── Entry point ────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let raw_args: Vec<_> = std::env::args_os().collect();
    let json_requested = raw_args.iter().any(|arg| arg == "inspect")
        && raw_args.iter().any(|arg| arg == "--json");
    let args = match Args::try_parse_from(&raw_args) {
        Ok(args) => args,
        Err(error) => {
            if json_requested && error.use_stderr() {
                println!("{}", serde_json::json!({
                    "version": inspect::REPORT_VERSION,
                    "error": { "code": "bad_argument", "message": error.to_string() }
                }));
                std::process::exit(2);
            }
            error.exit();
        }
    };

    if let Some(Command::Inspect { json, project, limit }) = &args.command {
        let code = inspect::run(inspect::InspectArgs {
            json: *json,
            project: project.clone(),
            limit: limit.clone(),
        });
        std::process::exit(code);
    }

    if args.cli || args.ps {
        let sort = args.sort_column().unwrap_or_else(|error| error.exit());
        run_cli(args, sort)?;
    } else {
        if let Ok(file) = File::create("/dev/null") {
            use std::os::unix::io::AsRawFd;
            unsafe { libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO); }
        }
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
            hook(info);
        }));
        run_tui()?;
    }
    Ok(())
}

// ─── CLI Mode ───────────────────────────────────────────────────────────────

fn run_cli(args: Args, sort: SortColumn) -> io::Result<()> {
    let mut out = io::stdout().lock();

    write_system_memory(&mut out, &get_system_memory())?;

    // Collect processes via sysinfo
    writeln!(out, "Scanning processes...")?;
    let mut processes = collect_processes(false);
    let mut group_measurements = HashMap::new();

    // Quick keeps its RSS-only contract. The deep scan keeps every observed
    // PID: a process footprint cannot read still belongs to its row, and the
    // row's group call decides that row's coverage.
    if args.quick {
        writeln!(out, "RSS-only process scan.")?;
        processes.retain(|p| p.rss > 0);
    } else {
        writeln!(out, "Measuring {} processes with footprint: one call per process, \
            then one per multi-process row...", processes.len())?;
        out.flush()?;
        let mode = (!args.ps).then(|| args.group_mode());
        group_measurements = measure_cli(&mut processes, mode, &footprint::measure_set);
    }

    write_process_table(&mut out, &args, sort, processes, &group_measurements)
}

/// Measure every process alone, then, when `mode` is set, each multi-process
/// group of that mode as one set over all of its members.
fn measure_cli(
    processes: &mut [ProcessMemory],
    mode: Option<group::GroupMode>,
    measure: &deep::Measurer<'_>,
) -> HashMap<GroupKey, GroupDeep> {
    let pids: Vec<i32> = processes.iter().map(|p| p.pid).collect();
    let mut standalone = HashMap::new();
    deep::measure_processes(&pids, deep::process_jobs(), measure, |pid, d| {
        standalone.insert(pid, d);
    });
    for p in processes.iter_mut() {
        p.deep = standalone.get(&p.pid).copied().unwrap_or(ProcessDeep::Unavailable);
    }
    let mut groups = HashMap::new();
    if let Some(mode) = mode {
        let keys = group::multi_process_keys(processes, &[mode]);
        deep::measure_groups(&keys, deep::GROUP_JOBS, measure, |key, d| {
            groups.insert(key.clone(), d);
        });
    }
    groups
}

fn write_system_memory(out: &mut impl Write, sys_mem: &scanner::SystemMemory) -> io::Result<()> {
    writeln!(out, "━━━ System Memory ━━━")?;
    writeln!(out, "RAM:    {} / {} ({:.1}%)",
        format_size(sys_mem.used_bytes), format_size(sys_mem.total_bytes), sys_mem.used_pct())?;
    writeln!(out, "Disk swap: {} / {} ({:.1}%)",
        format_size(sys_mem.swap_used), format_size(sys_mem.swap_total), sys_mem.swap_pct())?;
    writeln!(out, "  App: {}  Wired: {}  Compressed: {}  Cache: {}",
        format_size(sys_mem.app_memory), format_size(sys_mem.wired),
        format_size(sys_mem.compressed), format_size(sys_mem.cache))?;
    writeln!(out)?;
    Ok(())
}

fn write_process_table(
    out: &mut impl Write,
    args: &Args,
    sort: SortColumn,
    mut processes: Vec<ProcessMemory>,
    group_measurements: &HashMap<GroupKey, GroupDeep>,
) -> io::Result<()> {
    let descending = sort != SortColumn::Name;
    let extra_columns = if args.quick { "" } else { " COMPRESSED OR SWAPPED FOOTPRINT" };
    if args.ps {
        processes.sort_by(|a, b| sort.compare_processes(a, b, descending).then_with(|| a.pid.cmp(&b.pid)));
        writeln!(out, "{:<8} {:<35} {:<12}{}", "PID", "NAME", "RSS", extra_columns)?;
        writeln!(out, "{}", "─".repeat(if args.quick { 57 } else { 92 }))?;
        for p in &processes {
            write!(out, "{:<8} {:<35} {:<12}", p.pid, truncate(&p.name, 35), format_size(p.rss))?;
            write_deep_scan_columns(out, args.quick, deep::GroupDeep::from_process(p.deep))?;
        }
        if !args.quick {
            writeln!(out)?;
            let (measured, unavailable) = coverage(&processes);
            writeln!(out, "{} processes • footprint read {} • unavailable {}",
                processes.len(), measured, unavailable)?;
            writeln!(out, "COMPRESSED OR SWAPPED and FOOTPRINT measure each process alone; \
                memory shared with other processes is included in each.")?;
            writeln!(out, "n/a: footprint could not read the process (root processes need sudo).")?;
        }
    } else {
        let mode = args.group_mode();
        let mut groups = group::group_processes(&processes, mode);
        for g in &mut groups {
            g.resolve_deep(group_measurements, true);
        }
        groups.sort_by(|a, b| sort.compare_groups(a, b, descending));
        let total_procs: usize = groups.iter().map(|g| g.processes.len()).sum();
        let (noun, noun_pl) = match mode {
            group::GroupMode::Project => ("PROJECT", "projects"),
            group::GroupMode::App => ("APP", "apps"),
        };

        writeln!(out, "{:<30} {:<7} {:<24} {:<12}{}",
            noun, "PROCS", "BREAKDOWN", "RSS SUM", extra_columns)?;
        writeln!(out, "{}", "─".repeat(if args.quick { 76 } else { 112 }))?;
        for g in &groups {
            write!(out, "{:<30} {:<7} {:<24} {:<12}",
                truncate(&g.name, 30), g.processes.len(), truncate(&g.bridge(mode), 24),
                format_size(g.total_rss))?;
            write_deep_scan_columns(out, args.quick, g.deep)?;
        }

        writeln!(out)?;
        let sum_rss: u64 = groups.iter().map(|g| g.total_rss).sum();
        write!(
            out,
            "{} {} • {} processes • {} RSS sum",
            groups.len(), noun_pl, total_procs, format_size(sum_rss),
        )?;
        if !args.quick {
            let (measured, unavailable) = coverage(&processes);
            write!(out, " • footprint read {} • unavailable {}", measured, unavailable)?;
            let partial = groups.iter().filter(|g| matches!(g.deep, GroupDeep::Partial { .. })).count();
            let missing = groups.iter().filter(|g| g.deep == GroupDeep::Unavailable).count();
            writeln!(out)?;
            write!(out, "Rows: {} complete • {} lower bound (≥) • {} n/a",
                groups.len() - partial - missing, partial, missing)?;
        }
        writeln!(out)?;
        writeln!(out, "RSS SUM adds member RSS, so pages shared by members count once per process.")?;
        if !args.quick {
            writeln!(out, "COMPRESSED OR SWAPPED and FOOTPRINT: one footprint call over all of a row's \
                processes; memory shared inside the row counts once.")?;
            writeln!(out, "≥: lower bound, that call could not read some processes in the row. \
                n/a: it read none. \"footprint read\" counts processes measured alone.")?;
        }
    }
    Ok(())
}

fn coverage(processes: &[ProcessMemory]) -> (usize, usize) {
    let measured = processes.iter().filter(|p| p.deep.memory().is_some()).count();
    (measured, processes.len() - measured)
}

fn write_deep_scan_columns(out: &mut impl Write, quick: bool, deep: GroupDeep) -> io::Result<()> {
    if !quick {
        write!(out, " {:<21} {:<12}",
            deep::group_cell(deep, Metric::CompressedOrSwapped),
            deep::group_cell(deep, Metric::Footprint))?;
    }
    writeln!(out)
}

// ─── TUI Mode ───────────────────────────────────────────────────────────────

fn run_tui() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let mut app = App::new();
    let (tx, rx) = mpsc::channel();

    let (scan_id, mode, tx_scan) = (app.start_scan(), app.group_mode, tx.clone());
    thread::spawn(move || perform_scan(tx_scan, scan_id, mode));

    let tick_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui::ui(f, &mut app))?;

        let timeout = tick_rate.checked_sub(last_tick.elapsed()).unwrap_or(Duration::ZERO);

        if crossterm::event::poll(timeout)? && let Ok(ev) = event::read() {
            match ev {
                Event::Mouse(mouse) if app.pending_termination.is_none() => match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        if !app.detail_view_open { app.scroll_table(true); }
                    }
                    MouseEventKind::ScrollDown => {
                        if !app.detail_view_open { app.scroll_table(false); }
                    }
                    MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                        // The detail popup is modal: a click outside dismisses it
                        // and the event is swallowed (no table interaction behind).
                        if app.detail_view_open {
                            app.dismiss_popup_if_outside(mouse.row, mouse.column);
                        } else {
                            // App::click always moves the selection to the
                            // clicked row (even on a double's second click) and
                            // toggles exactly the group under the cursor.
                            if app.click(mouse.row, mouse.column, Instant::now())
                                == app::ClickAction::OpenDetail
                            {
                                app.detail_view_open = true;
                                app.current_detail = None;
                                show_detail(&app, &tx);
                            }
                        }
                    }
                    MouseEventKind::Down(crossterm::event::MouseButton::Right) => {
                        if !app.detail_view_open { app.toggle_expand(); }
                    }
                    _ => {}
                },
                Event::Key(key)
                    if !app.detail_view_open
                        && handle_termination_key(&mut app, key, termination::send_sigkill) => {}
                Event::Key(key) => {
                    if app.detail_view_open {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => app.detail_view_open = false,
                            _ => {}
                        }
                    } else {
                        match key.code {
                            KeyCode::Char('q') => app.quit(),
                            KeyCode::Up | KeyCode::Char('k') => app.prev_group(),
                            KeyCode::Down | KeyCode::Char('j') => app.next_group(),
                            // macOS Fn+↑ / Fn+↓ send PageUp / PageDown
                            KeyCode::PageUp => app.page_up(),
                            KeyCode::PageDown => app.page_down(),
                            KeyCode::Enter => {
                                // If on a leaf process (expanded group sub-row), open detail
                                // If on a group header, toggle expand
                                if let Some((_name, pi)) = app.selected_process() {
                                    if pi.is_some() {
                                        // On a sub-process — open detail
                                        app.detail_view_open = true;
                                        app.current_detail = None;
                                        show_detail(&app, &tx);
                                    } else {
                                        // On a group header — toggle expand
                                        app.toggle_expand();
                                    }
                                }
                            }
                            KeyCode::Char('d') => {
                                app.detail_view_open = true;
                                app.current_detail = None;
                                show_detail(&app, &tx);
                            }
                            KeyCode::Char('r') => {
                                // One scan at a time: footprint calls are CPU-heavy.
                                if !app.is_loading && app.deep_settled {
                                    let (scan_id, mode, tx_scan) = (app.start_scan(), app.group_mode, tx.clone());
                                    thread::spawn(move || perform_scan(tx_scan, scan_id, mode));
                                }
                            }
                            KeyCode::Char('t') => { app.sort_column = SortColumn::Footprint; app.sort_desc = true; app.sort_groups(); }
                            KeyCode::Char('p') => { app.sort_column = SortColumn::Physical; app.sort_desc = true; app.sort_groups(); }
                            KeyCode::Char('s') => { app.sort_column = SortColumn::CompressedOrSwapped; app.sort_desc = true; app.sort_groups(); }
                            KeyCode::Char('n') => { app.sort_column = SortColumn::Name; app.sort_desc = false; app.sort_groups(); }
                            KeyCode::Tab => app.cycle_group(),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.on_tick();
            last_tick = Instant::now();
        }

        // Process background events
        while let Ok(event) = rx.try_recv() {
            let current = app.scan_id;
            match event {
                ScanEvent::SystemMemory(mem) => app.set_system_memory(mem),
                ScanEvent::Processes(id, procs) if id == current => {
                    app.set_processes(procs);
                    app.is_loading = false;
                    app.status_message = Some(format!("Quick scan done — {} procs", app.all_processes.len()));
                }
                ScanEvent::Progress(id, progress) if id == current => {
                    app.deep_scan_progress = Some(progress);
                }
                ScanEvent::ProcessDeep(id, results, progress) if id == current => {
                    app.apply_process_deep(&results);
                    app.deep_scan_progress = Some(progress);
                }
                ScanEvent::GroupDeep(id, key, deep, progress) if id == current => {
                    app.apply_group_deep(key, deep);
                    app.deep_scan_progress = Some(progress);
                }
                ScanEvent::DetailResult(mut details) => {
                    // Deep values may have arrived while metadata was loading.
                    if let Some(p) = app.all_processes.iter().find(|p| p.pid == details.pid) {
                        details.deep = p.deep;
                    }
                    app.current_detail = Some(details);
                }
                ScanEvent::Done(id) if id == current => {
                    app.finish_deep_scan();
                    let (measured, unavailable) = app.deep_coverage();
                    app.status_message = Some(format!(
                        "Ready — {} processes, footprint read {}, unavailable {}",
                        app.all_processes.len(), measured, unavailable));
                }
                _ => {} // from an older scan
            }
        }

        if app.should_quit { break; }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

fn handle_termination_key(
    app: &mut App,
    key: KeyEvent,
    send: impl FnOnce(ProcessIdentity) -> io::Result<()>,
) -> bool {
    let plain_press = key.kind == KeyEventKind::Press && key.modifiers == KeyModifiers::NONE;
    if app.pending_termination.is_some() {
        if plain_press {
            match key.code {
                KeyCode::Char('y') => app.confirm_termination(send),
                KeyCode::Esc | KeyCode::Char('n' | 'q') => app.cancel_termination(),
                _ => {}
            }
        }
        return true;
    }
    if plain_press && key.code == KeyCode::Char('x') {
        app.request_termination();
        return true;
    }
    false
}

/// Open detail popup — reads memory from already-scanned data, fetches metadata in background
fn show_detail(app: &App, tx: &mpsc::Sender<ScanEvent>) {
    let target = if let Some((gi, pi)) = app.selected_location() {
        if let Some(pidx) = pi {
            app.groups.get(gi).and_then(|g| g.processes.get(pidx))
        } else {
            app.groups.get(gi).and_then(|g| g.processes.first())
        }
    } else { None };

    if let Some(proc) = target.cloned() {
        // Memory values come from the scan.
        // Spawn a background fetch for metadata (cmd, exe, cwd, parent) only.
        let tx = tx.clone();
        thread::spawn(move || {
            let detail = fetch_detail_meta(proc);
            let _ = tx.send(ScanEvent::DetailResult(detail));
        });
    }
}

/// Fetch process metadata (cmdline, exe, cwd, parent) via sysinfo + ps fallback.
/// Memory data comes directly from the already-scanned ProcessMemory.
fn fetch_detail_meta(proc: ProcessMemory) -> ProcessDetail {
    let pid = proc.pid;
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let sys_pid = Pid::from(pid as usize);

    let (mut cmd, mut exe, mut cwd, status, start_time, cpu_usage) =
        if let Some(p) = sys.process(sys_pid) {
            let cmd: Vec<String> = p.cmd().iter().map(|s| s.to_string_lossy().to_string()).collect();
            let exe = p.exe().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
            let cwd = p.cwd().map(|c| c.to_string_lossy().to_string()).unwrap_or_default();
            (cmd, exe, cwd, p.status().to_string(), p.start_time(), p.cpu_usage())
        } else {
            (vec![], String::new(), String::new(), "Unknown".into(), 0, 0.0)
        };

    // ps fallback for cmdline
    if cmd.is_empty() || cmd.iter().all(|s| s.is_empty()) {
        cmd = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "args="])
            .output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().split_whitespace().map(|w| w.to_string()).collect())
            .unwrap_or_default();
    }

    // lsof fallback for cwd
    if cwd.is_empty() {
        cwd = std::process::Command::new("lsof")
            .args(["-p", &pid.to_string(), "-Fn"])
            .output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|out| out.lines()
                .find(|l| l.starts_with("n/") && !l.contains("(deleted)"))
                .map(|l| l[1..].to_string()))
            .unwrap_or_default();
    }

    // Parent
    let (parent_pid, parent_cmd) = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid="])
        .output().ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<i32>().ok())
        .map(|ppid| {
            let pcmd = std::process::Command::new("ps")
                .args(["-p", &ppid.to_string(), "-o", "args="])
                .output().ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string());
            (Some(ppid), pcmd)
        })
        .unwrap_or((None, None));

    ProcessDetail {
        pid,
        name: proc.name,
        cmd, exe, cwd, status, start_time, cpu_usage,
        rss: proc.rss,
        deep: proc.deep,
        parent_pid, parent_cmd,
    }
}

// ─── Background scan ────────────────────────────────────────────────────────

fn perform_scan(tx: mpsc::Sender<ScanEvent>, scan_id: u64, mode: group::GroupMode) {
    // 1. System memory
    let _ = tx.send(ScanEvent::SystemMemory(get_system_memory()));

    // 2. Collect all processes via sysinfo (fast). Later phases use this same
    //    list, so group PID sets match the rows the TUI builds from it.
    let processes = collect_processes(true);
    let _ = tx.send(ScanEvent::Processes(scan_id, processes.clone()));

    // 3. Each process alone, streamed in batches.
    let pids: Vec<i32> = processes.iter().map(|p| p.pid).collect();
    let progress = |phase, done, total| DeepProgress { phase, done, total };
    let _ = tx.send(ScanEvent::Progress(scan_id, progress(DeepPhase::Processes, 0, pids.len())));
    let mut standalone = HashMap::new();
    let mut batch = Vec::new();
    let mut last_sent = Instant::now();
    deep::measure_processes(&pids, deep::process_jobs(), &footprint::measure_set, |pid, d| {
        standalone.insert(pid, d);
        batch.push((pid, d));
        if batch.len() >= 64 || last_sent.elapsed() >= Duration::from_millis(250)
            || standalone.len() == pids.len()
        {
            let done = progress(DeepPhase::Processes, standalone.len(), pids.len());
            let _ = tx.send(ScanEvent::ProcessDeep(scan_id, std::mem::take(&mut batch), done));
            last_sent = Instant::now();
        }
    });

    // 4. One call per multi-process group: the visible mode first, then the
    //    other so Tab needs no new scan.
    let other = match mode {
        group::GroupMode::App => group::GroupMode::Project,
        group::GroupMode::Project => group::GroupMode::App,
    };
    let keys = group::multi_process_keys(&processes, &[mode, other]);
    let _ = tx.send(ScanEvent::Progress(scan_id, progress(DeepPhase::Groups, 0, keys.len())));
    let mut done = 0;
    deep::measure_groups(&keys, deep::GROUP_JOBS, &footprint::measure_set, |key, d| {
        done += 1;
        let _ = tx.send(ScanEvent::GroupDeep(scan_id, key.clone(), d,
            progress(DeepPhase::Groups, done, keys.len())));
    });

    let _ = tx.send(ScanEvent::Done(scan_id));
}

// ─── Shared helpers ─────────────────────────────────────────────────────────

/// Collect all processes via sysinfo — fast, gives RSS and cmdline.
fn collect_processes(record_termination_identities: bool) -> Vec<ProcessMemory> {
    // Bracket the sysinfo and cwd reads with audit tokens. PID reuse during
    // collection must leave the row without a termination identity.
    let identities = if record_termination_identities {
        termination::capture_identities()
    } else {
        HashMap::new()
    };
    let mut sys = System::new_all();
    sys.refresh_all();
    let cwd_map = scanner::bulk_cwd_map();

    sys.processes().iter().map(|(pid, proc)| {
        let pid_i = pid.as_u32() as i32;
        let mut p = ProcessMemory::new(
            pid_i,
            proc.name().to_string_lossy().to_string(),
            proc.memory(),
        );
        p.cmdline = proc.cmd().iter().map(|s| s.to_string_lossy().to_string()).collect::<Vec<_>>().join(" ");
        p.cwd = proc.cwd().map(|c| c.to_string_lossy().to_string())
            .unwrap_or_else(|| cwd_map.get(&pid_i).cloned().unwrap_or_default());
        p.threads = 1; // sysinfo 0.33 doesn't expose threads
        p.termination_identity = identities.get(&pid_i).copied()
            .filter(|identity| termination::process_identity(pid_i) == Some(*identity));
        p
    }).collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max { format!("{}…", &s[..max - 1]) } else { s.to_string() }
}

#[cfg(test)]
mod cli_tests {
    use super::{
        Args, GroupDeep, ProcessDeep, ProcessMemory, footprint, group, measure_cli, scanner::SystemMemory,
        write_process_table, write_system_memory,
    };
    use clap::{Parser, error::ErrorKind};
    use std::collections::HashMap;

    const MIB: u64 = 1024 * 1024;

    fn measured(footprint: u64, compressed_or_swapped: u64) -> ProcessDeep {
        ProcessDeep::Measured(footprint::Memory { footprint, compressed_or_swapped })
    }

    fn report(flags: &[&str], processes: Vec<ProcessMemory>, groups: &HashMap<Vec<i32>, GroupDeep>) -> String {
        let args = Args::try_parse_from(["memo"].iter().chain(flags)).unwrap();
        let mut out = Vec::new();
        write_process_table(&mut out, &args, args.sort_column().unwrap(), processes, groups).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn row<'a>(report: &'a str, name: &str) -> Vec<&'a str> {
        report.lines().find(|line| line.starts_with(name) || line.contains(&format!(" {name} ")))
            .unwrap_or_else(|| panic!("no row {name} in {report}"))
            .split_whitespace().collect()
    }

    #[test]
    fn cli_keeps_process_counter_separate_from_disk_swap() {
        // footprint uses the "swapped" key for compressed pages even when disk swap is zero.
        let set = footprint::parse_set(r#"{
            "unit": "byte", "bytes per unit": 1, "errors": [],
            "total footprint": 11534336,
            "summary": {"total": {"dirty": 11534336, "swapped": 3145728}},
            "processes": [{"pid": 42, "name": "memo-fixture", "footprint": 11534336, "categories": {}}]
        }"#).unwrap();
        let mut process = ProcessMemory::new(42, "memo-fixture".into(), 8 * MIB);
        process.deep = ProcessDeep::Measured(set.memory);

        for flags in [["--cli"], ["--ps"]] {
            for (swap_used, disk_label) in [(0, "0B"), (512 * MIB, "512.0M")] {
                let system = SystemMemory {
                    total_bytes: 1024 * MIB,
                    used_bytes: 600 * MIB,
                    free_bytes: 424 * MIB,
                    swap_total: 1024 * MIB,
                    swap_used,
                    compressed: 3 * MIB,
                    wired: 0,
                    app_memory: 0,
                    cache: 0,
                };
                let mut out = Vec::new();
                write_system_memory(&mut out, &system).unwrap();
                let text = String::from_utf8(out).unwrap()
                    + &report(&flags, vec![process.clone()], &HashMap::new());
                assert!(text.contains(&format!("Disk swap: {disk_label} / 1.0G")));
                assert!(text.contains("COMPRESSED OR SWAPPED FOOTPRINT"), "{text}");
                assert!(row(&text, "memo-fixture").ends_with(&["8.0M", "3.0M", "11.0M"]), "{text}");
                assert!(!text.contains("unaccounted"));
                assert!(!text.contains("(system:"));
            }
        }
    }

    #[test]
    fn grouped_rows_use_the_group_measurement_not_member_sums() {
        // Two processes that each measure 42 MiB alone because they share a
        // 32 MiB region. Apple's call over both returns 52 MiB.
        let opera = |pid, name: &str| {
            let mut p = ProcessMemory::new(pid, name.into(), 30 * MIB);
            p.deep = measured(42 * MIB, 24 * MIB);
            p
        };
        let mut daemon = ProcessMemory::new(3, "rootd".into(), 5 * MIB);
        daemon.deep = ProcessDeep::Unavailable;
        let mut helper = ProcessMemory::new(5, "Slack Helper".into(), 2 * MIB);
        helper.deep = ProcessDeep::Unavailable;
        let mut slack = ProcessMemory::new(4, "Slack".into(), 9 * MIB);
        slack.deep = measured(20 * MIB, 0);
        let processes = vec![opera(1, "Opera"), opera(2, "Opera Helper (Renderer)"), daemon, slack, helper];
        let groups = HashMap::from([
            (vec![1, 2], GroupDeep::Measured(footprint::Memory {
                footprint: 52 * MIB, compressed_or_swapped: 28 * MIB })),
            (vec![4, 5], GroupDeep::Partial {
                memory: footprint::Memory { footprint: 20 * MIB, compressed_or_swapped: 0 },
                measured: 1, members: 2 }),
        ]);

        let text = report(&["--cli"], processes.clone(), &groups);
        assert!(row(&text, "Opera").ends_with(&["60.0M", "28.0M", "52.0M"]), "{text}");
        assert!(row(&text, "Slack").ends_with(&["11.0M", "≥0B", "≥20.0M"]), "partial: {text}");
        assert!(row(&text, "rootd").ends_with(&["5.0M", "n/a", "n/a"]), "unavailable, not zero: {text}");
        let names: Vec<_> = text.lines().filter_map(|l| l.split_whitespace().next())
            .filter(|n| ["Opera", "Slack", "rootd"].contains(n)).collect();
        assert_eq!(names, ["Opera", "Slack", "rootd"], "footprint order, unreadable last");
        assert!(text.contains("RSS SUM"), "{text}");
        assert!(text.contains("footprint read 3 • unavailable 2"), "{text}");

        // A multi-process row whose measurement is missing is not shown as a sum.
        let text = report(&["--cli"], processes.clone(), &HashMap::new());
        assert!(row(&text, "Opera").ends_with(&["60.0M", "n/a", "n/a"]), "{text}");

        // Ungrouped rows show each process alone.
        let text = report(&["--ps"], processes, &groups);
        assert!(row(&text, "Opera").ends_with(&["30.0M", "24.0M", "42.0M"]), "{text}");
        assert!(row(&text, "rootd").ends_with(&["5.0M", "n/a", "n/a"]), "{text}");
    }

    #[test]
    fn cli_rows_keep_unreadable_members_and_take_coverage_from_the_group_call() {
        // PID 3 has no RSS (sysinfo cannot read it) and footprint cannot read
        // it alone. It still belongs to the Opera row and its group call.
        let procs = || {
            vec![
                ProcessMemory::new(1, "Opera".into(), 30 * MIB),
                ProcessMemory::new(2, "Opera Helper (Renderer)".into(), 30 * MIB),
                ProcessMemory::new(3, "Opera Helper (GPU)".into(), 0),
            ]
        };
        let run = |group_reads_3: bool| {
            let calls = std::sync::Mutex::new(Vec::new());
            let measure = |pids: &[i32]| -> anyhow::Result<footprint::SetMeasurement> {
                calls.lock().unwrap().push(pids.to_vec());
                let read: Vec<i32> = pids.iter().copied()
                    .filter(|p| *p != 3 || (pids.len() > 1 && group_reads_3)).collect();
                if read.is_empty() {
                    anyhow::bail!("footprint exited 66");
                }
                let n = read.len() as u64;
                Ok(footprint::SetMeasurement { pids: read, memory: footprint::Memory {
                    footprint: (10 * n + 32) * MIB, compressed_or_swapped: 4 * n * MIB } })
            };
            let mut processes = procs();
            let groups = measure_cli(&mut processes, Some(group::GroupMode::App), &measure);
            let calls = calls.into_inner().unwrap();
            assert!(calls.contains(&vec![1, 2, 3]), "the group call holds every member: {calls:?}");
            (processes, groups)
        };

        // footprint reads only 1 and 2: the row is a lower bound, not complete.
        let (processes, groups) = run(false);
        assert_eq!(processes.len(), 3, "no observed PID is dropped");
        assert_eq!(processes[2].deep, ProcessDeep::Unavailable);
        let text = report(&["--cli"], processes.clone(), &groups);
        assert!(row(&text, "Opera").starts_with(&["Opera", "3"]), "{text}");
        assert!(row(&text, "Opera").ends_with(&["60.0M", "≥8.0M", "≥52.0M"]), "{text}");
        assert!(text.contains("Rows: 0 complete • 1 lower bound (≥) • 0 n/a"), "{text}");
        let ps = report(&["--ps"], processes, &groups);
        assert!(row(&ps, "3").ends_with(&["0B", "n/a", "n/a"]), "{ps}");

        // Unavailable alone, read in the group call: the row is complete.
        let (processes, groups) = run(true);
        let text = report(&["--cli"], processes, &groups);
        assert!(row(&text, "Opera").ends_with(&["60.0M", "12.0M", "62.0M"]), "{text}");
        assert!(text.contains("footprint read 2 • unavailable 1"), "{text}");
        assert!(text.contains("Rows: 1 complete • 0 lower bound (≥) • 0 n/a"), "{text}");
    }

    #[test]
    fn grouped_cli_ranks_by_the_requested_metric() {
        struct ProjectFixture(std::path::PathBuf);
        impl Drop for ProjectFixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        // Grouping recognizes project markers under the user's home.
        // Keep the common prefix short enough for all row names to fit.
        let home = std::env::var_os("HOME").expect("project grouping needs HOME");
        let id = uuid::Uuid::new_v4().simple().to_string();
        let fixture_name = format!(".memo-{}", &id[..8]);
        let fixture_path = std::path::PathBuf::from(home).join(&fixture_name);
        std::fs::create_dir(&fixture_path).unwrap();
        let project_fixture = ProjectFixture(fixture_path);
        let fixture = [
            ("memo-alpha", 100, 0, 100),
            ("memo-beta", 60, 210, 270),
            ("memo-gamma", 90, 190, 280),
        ];
        let processes: Vec<_> = fixture
            .into_iter()
            .enumerate()
            .map(|(id, (name, rss, counter, footprint))| {
                let root = project_fixture.0.join(name);
                std::fs::create_dir(&root).unwrap();
                std::fs::write(root.join("package.json"), "{}").unwrap();
                let mut process = ProcessMemory::new(id as i32 + 1, name.into(), rss);
                process.cwd = root.to_str().unwrap().into();
                process.deep = measured(footprint, counter);
                process
            })
            .collect();
        let total = ["memo-gamma", "memo-beta", "memo-alpha"];
        let rss = ["memo-alpha", "memo-gamma", "memo-beta"];
        let counter = ["memo-beta", "memo-gamma", "memo-alpha"];
        let name = ["memo-alpha", "memo-beta", "memo-gamma"];

        for group in ["app", "project"] {
            let prefix = if group == "project" {
                format!("~/{fixture_name}/")
            } else {
                String::new()
            };
            for (flags, expected, header) in [
                (&[][..], total, "FOOTPRINT"),
                (&["--sort", "total"], total, "FOOTPRINT"),
                (&["--sort", "footprint"], total, "FOOTPRINT"),
                (&["--sort", "rss"], rss, "FOOTPRINT"),
                (&["--sort", "swap"], counter, "FOOTPRINT"),
                (&["--sort", "name"], name, "FOOTPRINT"),
                (&["--quick"], rss, "RSS SUM"),
                (&["--quick", "--sort", "rss"], rss, "RSS SUM"),
                (&["--quick", "--sort", "name"], name, "RSS SUM"),
            ] {
                let args = Args::try_parse_from(
                    ["memo", "--cli", "--group", group].into_iter().chain(flags.iter().copied()),
                ).unwrap();
                let mut out = Vec::new();
                write_process_table(&mut out, &args, args.sort_column().unwrap(), processes.clone(), &HashMap::new())
                    .unwrap();
                let report = String::from_utf8(out).unwrap();
                let columns = report.lines().next().unwrap();
                assert!(columns.trim_end().ends_with(header), "{columns}");
                let names: Vec<_> = report.lines()
                    .filter_map(|line| line.strip_prefix(&prefix))
                    .filter(|line| line.starts_with("memo-"))
                    .map(|line| line.split_whitespace().next().unwrap())
                    .collect();
                assert_eq!(names, expected, "group {group}, flags {flags:?}");
            }
        }
    }

    #[test]
    fn version_does_not_become_required_for_normal_commands() {
        let tui = Args::try_parse_from(["memo"]).expect("plain startup must parse");
        assert!(!tui.cli && !tui.ps);
        assert!(Args::try_parse_from(["memo", "--cli"]).unwrap().cli);
        assert!(Args::try_parse_from(["memo", "--ps"]).unwrap().ps);
    }

    #[test]
    fn both_version_flags_display_the_package_version() {
        for flag in ["--version", "-v"] {
            let result = Args::try_parse_from(["memo", flag]).unwrap_err();
            assert_eq!(result.kind(), ErrorKind::DisplayVersion);
            assert_eq!(result.to_string(), format!("memo {}\n", env!("CARGO_PKG_VERSION")));
        }
    }

    #[test]
    fn inspect_rejects_explicit_legacy_flags_even_with_default_values() {
        for flag in ["--sort", "--group"] {
            let value = if flag == "--sort" { "total" } else { "app" };
            assert!(Args::try_parse_from(["memo", flag, value, "inspect", "--json"]).is_err());
        }
        assert!(Args::try_parse_from(["memo", "--cli", "inspect"]).is_err());
        assert!(Args::try_parse_from(["memo", "inspect", "--ps"]).is_err());
        assert!(Args::try_parse_from(["memo", "inspect", "--json"]).is_ok());
    }
}
