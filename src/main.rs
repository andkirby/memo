mod app;
mod footprint;
mod group;
mod inspect;
mod scanner;
mod ui;

use std::{
    collections::HashMap,
    fs::File,
    io, io::Write,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use app::{App, ProcessDetail, SortColumn};
use clap::{CommandFactory, Parser, Subcommand, error::ErrorKind};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseEvent, MouseEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use scanner::{ProcessMemory, format_size, get_system_memory};
use sysinfo::{Pid, System};

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

    /// Sort by total (default), rss, swap (compressed or swapped), or name.
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
            (false, _) => Ok(SortColumn::Total),
            (true, Some(value)) => Err(Self::command().error(
                ErrorKind::InvalidValue,
                format!("--sort {value} is unavailable with --quick. Use rss or name."),
            )),
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

enum ScanEvent {
    SystemMemory(scanner::SystemMemory),
    Processes(Vec<ProcessMemory>),
    CompressedOrSwappedBatch(HashMap<i32, u64>), // pid → compressed or swapped bytes
    DetailResult(ProcessDetail),
    Done,
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
    let mut processes = collect_processes();

    if args.quick {
        writeln!(out, "RSS-only process scan.")?;
    } else {
        writeln!(out, "Deep analysis (compressed or swapped via footprint)...")?;
        let rss_threshold = 5 * 1024 * 1024u64;
        let big_pids: Vec<i32> = processes.iter()
            .filter(|p| p.rss >= rss_threshold).map(|p| p.pid).collect();
        writeln!(out, "Deep-scanning {} processes (skipped {} tiny)...",
            big_pids.len(), processes.len() - big_pids.len())?;

        let compressed_or_swapped = scan_compressed_or_swapped(&big_pids);
        for p in &mut processes {
            if let Some(bytes) = compressed_or_swapped.get(&p.pid) {
                p.compressed_or_swapped = *bytes;
            }
        }
    }

    processes.retain(|p| p.total() > 0);

    write_process_table(&mut out, &args, sort, processes)
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
) -> io::Result<()> {
    let descending = sort != SortColumn::Name;
    let extra_columns = if args.quick { "" } else { " COMPRESSED OR SWAPPED TOTAL" };
    if args.ps {
        processes.sort_by(|a, b| sort.compare_processes(a, b, descending));
        writeln!(out, "{:<8} {:<35} {:<12}{}", "PID", "NAME", "RSS", extra_columns)?;
        writeln!(out, "{}", "─".repeat(if args.quick { 57 } else { 92 }))?;
        for p in &processes {
            write!(out, "{:<8} {:<35} {:<12}", p.pid, truncate(&p.name, 35), format_size(p.rss))?;
            write_deep_scan_columns(out, args.quick, p.compressed_or_swapped, p.total())?;
        }
    } else {
        let mode = match args.group.as_str() {
            "project" => group::GroupMode::Project,
            _ => group::GroupMode::App,
        };
        let mut groups = group::group_processes(&processes, mode);
        if sort != SortColumn::Total {
            groups.sort_by(|a, b| sort.compare_groups(a, b, descending));
        }
        let total_procs: usize = groups.iter().map(|g| g.processes.len()).sum();
        let (noun, noun_pl) = match mode {
            group::GroupMode::Project => ("PROJECT", "projects"),
            group::GroupMode::App => ("APP", "apps"),
        };

        writeln!(out, "{:<30} {:<7} {:<24} {:<12}{}",
            noun, "PROCS", "BREAKDOWN", "RSS", extra_columns)?;
        writeln!(out, "{}", "─".repeat(if args.quick { 76 } else { 112 }))?;
        for g in &groups {
            write!(out, "{:<30} {:<7} {:<24} {:<12}",
                truncate(&g.name, 30), g.processes.len(), truncate(&g.bridge(mode), 24),
                format_size(g.total_rss))?;
            write_deep_scan_columns(out, args.quick, g.total_compressed_or_swapped, g.total())?;
        }

        writeln!(out)?;
        let sum_rss: u64 = groups.iter().map(|g| g.total_rss).sum();
        write!(
            out,
            "{} {} • {} processes • {} RSS",
            groups.len(), noun_pl, total_procs, format_size(sum_rss),
        )?;
        if !args.quick {
            let bytes = groups.iter().map(|g| g.total_compressed_or_swapped).sum();
            write!(out, " • {} compressed or swapped", format_size(bytes))?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn write_deep_scan_columns(
    out: &mut impl Write,
    quick: bool,
    compressed_or_swapped: u64,
    total: u64,
) -> io::Result<()> {
    if !quick {
        write!(out, " {:<21} {:<12}", format_size(compressed_or_swapped), format_size(total))?;
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

    app.is_loading = true;
    let tx_scan = tx.clone();
    thread::spawn(move || perform_scan(tx_scan));

    let tick_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui::ui(f, &mut app))?;

        let timeout = tick_rate.checked_sub(last_tick.elapsed()).unwrap_or(Duration::ZERO);

        if crossterm::event::poll(timeout)? && let Ok(ev) = event::read() {
            match ev {
                Event::Mouse(mouse) => match mouse.kind {
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
                                if !app.is_loading {
                                    app.is_loading = true;
                                    app.groups.clear();
                                    app.all_processes.clear();
                                    app.expanded_group = None;
                                    let tx_scan = tx.clone();
                                    thread::spawn(move || perform_scan(tx_scan));
                                }
                            }
                            KeyCode::Char('x') => app.kill_selected(),
                            KeyCode::Char('t') => { app.sort_column = SortColumn::Total; app.sort_desc = true; app.sort_groups(); }
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
            match event {
                ScanEvent::SystemMemory(mem) => app.set_system_memory(mem),
                ScanEvent::Processes(procs) => {
                    app.set_processes(procs);
                    app.is_loading = false;
                    app.status_message = Some(format!("Quick scan done — {} procs", app.all_processes.len()));
                }
                ScanEvent::CompressedOrSwappedBatch(compressed_or_swapped) => {
                    for p in &mut app.all_processes {
                        if let Some(bytes) = compressed_or_swapped.get(&p.pid) {
                            p.compressed_or_swapped = *bytes;
                        }
                    }
                    app.recalculate_from(&app.all_processes.clone());
                    app.sort_groups();
                }
                ScanEvent::DetailResult(details) => app.current_detail = Some(details),
                ScanEvent::Done => {
                    app.is_loading = false;
                    app.status_message = Some(format!("Ready — {} apps, {} processes",
                        app.groups.len(), app.all_processes.len()));
                }
            }
        }

        if app.should_quit { break; }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
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
        compressed_or_swapped: proc.compressed_or_swapped,
        parent_pid, parent_cmd,
    }
}

// ─── Background scan ────────────────────────────────────────────────────────

fn perform_scan(tx: mpsc::Sender<ScanEvent>) {
    // 1. System memory
    let sys_mem = get_system_memory();
    let _ = tx.send(ScanEvent::SystemMemory(sys_mem));

    // 2. Collect all processes via sysinfo (fast)
    let processes = collect_processes();
    let _ = tx.send(ScanEvent::Processes(processes));

    // 3. Deep scan: measure compressed or swapped pages for processes with RSS >= 5 MB
    let rss_threshold = 5 * 1024 * 1024u64;
    let big_pids: Vec<i32> = {
        // Read from the processes we just sent (re-derive from sysinfo since we can't borrow tx data)
        let mut sys = System::new_all();
        sys.refresh_all();
        sys.processes().iter()
            .filter(|(_, p)| p.memory() >= rss_threshold)
            .map(|(pid, _)| pid.as_u32() as i32)
            .collect()
    };

    let compressed_or_swapped = scan_compressed_or_swapped(&big_pids);
    if !compressed_or_swapped.is_empty() {
        let _ = tx.send(ScanEvent::CompressedOrSwappedBatch(compressed_or_swapped));
    }

    let _ = tx.send(ScanEvent::Done);
}

// ─── Shared helpers ─────────────────────────────────────────────────────────

/// Collect all processes via sysinfo — fast, gives RSS and cmdline.
fn collect_processes() -> Vec<ProcessMemory> {
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
        p
    }).collect()
}

/// Run footprint in parallel batches and return compressed or swapped bytes by PID.
fn scan_compressed_or_swapped(pids: &[i32]) -> HashMap<i32, u64> {
    let mut result = HashMap::new();
    if pids.is_empty() { return result; }

    let chunks: Vec<Vec<i32>> = pids.chunks(20).map(|c| c.to_vec()).collect();

    std::thread::scope(|s| {
        for batch in chunks.chunks(4) {
            let handles: Vec<_> = batch.iter()
                .map(|chunk| s.spawn(|| footprint::get_compressed_or_swapped_for_pids(chunk)))
                .collect();

            for handle in handles {
                if let Ok(fp_map) = handle.join().unwrap() {
                    result.extend(fp_map);
                }
            }
        }
    });

    result
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max { format!("{}…", &s[..max - 1]) } else { s.to_string() }
}

#[cfg(test)]
mod cli_tests {
    use super::{
        Args, ProcessMemory, footprint, scanner::SystemMemory, write_process_table,
        write_system_memory,
    };
    use clap::{Parser, error::ErrorKind};

    #[test]
    fn cli_keeps_process_counter_separate_from_disk_swap() {
        // footprint uses the "swapped" key for compressed pages even when disk swap is zero.
        let counters = footprint::parse_compressed_or_swapped(r#"{
            "processes": [{
                "pid": 42, "name": "memo-fixture",
                "categories": {
                    "MALLOC": {"swapped": 2097152},
                    "STACK": {"swapped": 1048576}
                }
            }]
        }"#).unwrap();
        let mib = 1024 * 1024;
        let mut process = ProcessMemory::new(42, "memo-fixture".into(), 8 * mib);
        process.compressed_or_swapped = counters[&42];

        for args in [["memo", "--cli"], ["memo", "--ps"]] {
            let args = Args::try_parse_from(args).unwrap();
            for (swap_used, disk_label) in [(0, "0B"), (512 * mib, "512.0M")] {
                let system = SystemMemory {
                    total_bytes: 1024 * mib,
                    used_bytes: 600 * mib,
                    free_bytes: 424 * mib,
                    swap_total: 1024 * mib,
                    swap_used,
                    compressed: 3 * mib,
                    wired: 0,
                    app_memory: 0,
                    cache: 0,
                };
                let mut out = Vec::new();
                write_system_memory(&mut out, &system).unwrap();
                write_process_table(&mut out, &args, args.sort_column().unwrap(), vec![process.clone()])
                    .unwrap();
                let report = String::from_utf8(out).unwrap();
                assert!(report.contains(&format!("Disk swap: {disk_label} / 1.0G")));
                assert!(report.contains("RSS          COMPRESSED OR SWAPPED TOTAL"));
                let row = report.lines().find(|line| line.contains("memo-fixture")).unwrap();
                let columns: Vec<_> = row.split_whitespace().collect();
                assert!(columns.ends_with(&["8.0M", "3.0M", "11.0M"]));
                assert!(!report.contains("unaccounted"));
                assert!(!report.contains("(system:"));
            }
        }
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
            ("memo-alpha", 100, 0),
            ("memo-beta", 60, 210),
            ("memo-gamma", 90, 190),
        ];
        let processes: Vec<_> = fixture
            .into_iter()
            .enumerate()
            .map(|(id, (name, rss, counter))| {
                let root = project_fixture.0.join(name);
                std::fs::create_dir(&root).unwrap();
                std::fs::write(root.join("package.json"), "{}").unwrap();
                let mut process = ProcessMemory::new(id as i32 + 1, name.into(), rss);
                process.cwd = root.to_str().unwrap().into();
                process.compressed_or_swapped = counter;
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
                (&[][..], total, "TOTAL"),
                (&["--sort", "total"], total, "TOTAL"),
                (&["--sort", "rss"], rss, "TOTAL"),
                (&["--sort", "swap"], counter, "TOTAL"),
                (&["--sort", "name"], name, "TOTAL"),
                (&["--quick"], rss, "RSS"),
                (&["--quick", "--sort", "rss"], rss, "RSS"),
                (&["--quick", "--sort", "name"], name, "RSS"),
            ] {
                let args = Args::try_parse_from(
                    ["memo", "--cli", "--group", group].into_iter().chain(flags.iter().copied()),
                ).unwrap();
                let mut out = Vec::new();
                write_process_table(&mut out, &args, args.sort_column().unwrap(), processes.clone())
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
