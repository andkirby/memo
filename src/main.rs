mod app;
mod footprint;
mod group;
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
use app::{App, ProcessDetail, SortColumn, ViewMode};
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseEvent, MouseEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use scanner::{ProcessMemory, format_size, get_system_memory};
use sysinfo::{Pid, System};

#[derive(Parser, Debug)]
#[command(name = "memo", about = "Quick memory usage analysis")]
struct Args {
    /// CLI mode (dump to stdout and exit)
    #[arg(long)]
    cli: bool,

    /// Show ungrouped process list
    #[arg(long)]
    ps: bool,

    /// Sort: total, rss, swap, name
    #[arg(long, default_value = "total")]
    sort: String,
}

// ─── Scan events (background → main thread) ────────────────────────────────

enum ScanEvent {
    SystemMemory(scanner::SystemMemory),
    Processes(Vec<ProcessMemory>),
    SwapBatch(HashMap<i32, u64>),              // pid → swap bytes
    DetailResult(ProcessDetail),
    Done,
}

// ─── Entry point ────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args = Args::parse();

    if args.cli || args.ps {
        run_cli(args)?;
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

fn run_cli(args: Args) -> Result<()> {
    let mut out = io::stdout().lock();

    let sys_mem = get_system_memory();
    writeln!(out, "━━━ System Memory ━━━")?;
    writeln!(out, "RAM:    {} / {} ({:.1}%)",
        format_size(sys_mem.used_bytes), format_size(sys_mem.total_bytes), sys_mem.used_pct())?;
    writeln!(out, "Swap:   {} / {} ({:.1}%)",
        format_size(sys_mem.swap_used), format_size(sys_mem.swap_total), sys_mem.swap_pct())?;
    writeln!(out, "  App: {}  Wired: {}  Compressed: {}  Cache: {}",
        format_size(sys_mem.app_memory), format_size(sys_mem.wired),
        format_size(sys_mem.compressed), format_size(sys_mem.cache))?;
    writeln!(out)?;

    // Collect processes via sysinfo
    writeln!(out, "Scanning processes...")?;
    let mut processes = collect_processes();

    // Deep scan for swap
    writeln!(out, "Deep analysis (swap via footprint)...")?;
    let rss_threshold = 5 * 1024 * 1024u64;
    let big_pids: Vec<i32> = processes.iter()
        .filter(|p| p.rss >= rss_threshold).map(|p| p.pid).collect();
    writeln!(out, "Deep-scanning {} processes (skipped {} tiny)...",
        big_pids.len(), processes.len() - big_pids.len())?;

    let swap_map = parallel_swap_scan(&big_pids);
    for p in &mut processes {
        if let Some(swap) = swap_map.get(&p.pid) {
            p.swap = *swap;
        }
    }

    processes.retain(|p| p.total() > 0);

    if args.ps {
        match args.sort.as_str() {
            "rss" => processes.sort_by(|a, b| b.rss.cmp(&a.rss)),
            "swap" => processes.sort_by(|a, b| b.swap.cmp(&a.swap)),
            "name" => processes.sort_by(|a, b| a.name.cmp(&b.name)),
            _ => processes.sort_by(|a, b| b.total().cmp(&a.total())),
        }
        writeln!(out, "{:<8} {:<35} {:<12} {:<12} {:<12}",
            "PID", "NAME", "RSS", "SWAP", "TOTAL")?;
        writeln!(out, "{}", "─".repeat(85))?;
        for p in &processes {
            writeln!(out, "{:<8} {:<35} {:<12} {:<12} {:<12}",
                p.pid, truncate(&p.name, 35), format_size(p.rss), format_size(p.swap), format_size(p.total()))?;
        }
    } else {
        let groups = group::group_processes(&processes);
        let total_procs: usize = groups.iter().map(|g| g.processes.len()).sum();

        writeln!(out, "{:<30} {:<7} {:<12} {:<12} {:<12}",
            "APP", "PROCS", "RSS", "SWAP", "TOTAL")?;
        writeln!(out, "{}", "─".repeat(80))?;
        for g in &groups {
            writeln!(out, "{:<30} {:<7} {:<12} {:<12} {:<12}",
                truncate(&g.name, 30), g.processes.len(),
                format_size(g.total_rss), format_size(g.total_swap), format_size(g.total()))?;
        }

        writeln!(out)?;
        let sum_swap: u64 = processes.iter().map(|p| p.swap).sum();
        let delta = sys_mem.swap_used.saturating_sub(sum_swap);
        writeln!(out, "{} apps • {} processes • {} rss • {} swap (system: {})",
            groups.len(), total_procs,
            format_size(processes.iter().map(|p| p.rss).sum::<u64>()),
            format_size(sum_swap), format_size(sys_mem.swap_used))?;
        if delta > 100 * 1024 * 1024 {
            writeln!(out, "Note: {} swap unaccounted (from processes below scan threshold)", format_size(delta))?;
        }
    }
    Ok(())
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
                    MouseEventKind::ScrollUp => app.scroll_table(true),
                    MouseEventKind::ScrollDown => app.scroll_table(false),
                    MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                        app.click_table(mouse.row, mouse.column);
                    }
                    MouseEventKind::Down(crossterm::event::MouseButton::Right) => app.toggle_expand(),
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
                            KeyCode::Enter => app.toggle_expand(),
                            KeyCode::Char('d') => show_detail(&app, &tx),
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
                            KeyCode::Char('s') => { app.sort_column = SortColumn::Swap; app.sort_desc = true; app.sort_groups(); }
                            KeyCode::Char('n') => { app.sort_column = SortColumn::Name; app.sort_desc = false; app.sort_groups(); }
                            KeyCode::Tab => app.view_mode = match app.view_mode {
                                ViewMode::Overview => ViewMode::Ps,
                                ViewMode::Ps => ViewMode::Overview,
                            },
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
                ScanEvent::SwapBatch(swap_map) => {
                    for p in &mut app.all_processes {
                        if let Some(swap) = swap_map.get(&p.pid) {
                            p.swap = *swap;
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
    let target = if let Some((gi, pi)) = app.selected_process() {
        if let Some(pidx) = pi {
            app.groups.get(gi).and_then(|g| g.processes.get(pidx))
        } else {
            app.groups.get(gi).and_then(|g| g.processes.first())
        }
    } else { None };

    if let Some(proc) = target.cloned() {
        // We already have rss + swap from the scan — no re-fetch needed for memory.
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
        swap: proc.swap,
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

    // 3. Deep scan: get swap from footprint for processes with RSS >= 5MB
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

    let swap_map = parallel_swap_scan(&big_pids);
    if !swap_map.is_empty() {
        let _ = tx.send(ScanEvent::SwapBatch(swap_map));
    }

    let _ = tx.send(ScanEvent::Done);
}

// ─── Shared helpers ─────────────────────────────────────────────────────────

/// Collect all processes via sysinfo — fast, gives RSS and cmdline.
fn collect_processes() -> Vec<ProcessMemory> {
    let mut sys = System::new_all();
    sys.refresh_all();

    sys.processes().iter().map(|(pid, proc)| {
        let mut p = ProcessMemory::new(
            pid.as_u32() as i32,
            proc.name().to_string_lossy().to_string(),
            proc.memory(),
        );
        p.cmdline = proc.cmd().iter().map(|s| s.to_string_lossy().to_string()).collect::<Vec<_>>().join(" ");
        p.threads = 1; // sysinfo 0.33 doesn't expose threads
        p
    }).collect()
}

/// Run footprint in parallel batches and return pid → swap bytes.
fn parallel_swap_scan(pids: &[i32]) -> HashMap<i32, u64> {
    let mut result = HashMap::new();
    if pids.is_empty() { return result; }

    let chunks: Vec<Vec<i32>> = pids.chunks(20).map(|c| c.to_vec()).collect();

    std::thread::scope(|s| {
        for batch in chunks.chunks(4) {
            let handles: Vec<_> = batch.iter()
                .map(|chunk| s.spawn(|| footprint::get_swap_for_pids(chunk)))
                .collect();

            for handle in handles {
                if let Ok(fp_map) = handle.join().unwrap() {
                    for (pid, data) in fp_map {
                        result.insert(pid, data.swapped_total);
                    }
                }
            }
        }
    });

    result
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max { format!("{}…", &s[..max - 1]) } else { s.to_string() }
}
