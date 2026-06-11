mod app;
mod footprint;
mod group;
mod scanner;
mod top;
mod ui;

use std::{
    collections::HashMap,
    fs::File,
    io,
    io::Write,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use app::{App, ProcessDetail, SortColumn, ViewMode};
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use scanner::{ProcessMemory, SystemMemory, format_size, get_system_memory};
use sysinfo::{Pid, System};

#[derive(Parser, Debug)]
#[command(name = "memo", about = "Quick memory usage analysis")]
struct Args {
    /// Run in CLI mode (dump to stdout and exit)
    #[arg(long)]
    cli: bool,

    /// Show detailed process list (ungrouped)
    #[arg(long)]
    ps: bool,

    /// Sort column: total, phys, swap, name
    #[arg(long, default_value = "total")]
    sort: String,
}

enum ScanEvent {
    SysMemory(SystemMemory),
    Start(usize),
    Progress(usize, String),
    Result(ProcessMemory),
    DeepScanStart(usize),
    DeepScanProgress(usize),
    BatchUpdate(HashMap<i32, (crate::footprint::FootprintData, u64)>),
    Complete,
    DetailResult(ProcessDetail),
    SingleResult(ProcessMemory),
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.cli || args.ps {
        run_cli(args)?;
    } else {
        // Redirect stderr away
        if let Ok(file) = File::create("/dev/null") {
            use std::os::unix::io::AsRawFd;
            unsafe { libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO); }
        }

        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
            original_hook(info);
        }));

        run_tui()?;
    }
    Ok(())
}

// ─── CLI Mode ──────────────────────────────────────────────────────────────

fn run_cli(args: Args) -> Result<()> {
    let mut out = io::stdout().lock();

    // System memory
    let sys_mem = get_system_memory();
    writeln!(out, "━━━ System Memory ━━━")?;
    writeln!(out, "RAM:    {} / {} ({:.1}%)",
        format_size(sys_mem.used_bytes),
        format_size(sys_mem.total_bytes),
        sys_mem.used_pct())?;
    writeln!(out, "Swap:   {} / {} ({:.1}%)",
        format_size(sys_mem.swap_used),
        format_size(sys_mem.swap_total),
        sys_mem.swap_pct())?;
    writeln!(out, "  App: {}  Wired: {}  Compressed: {}  Cache: {}",
        format_size(sys_mem.app_memory),
        format_size(sys_mem.wired),
        format_size(sys_mem.compressed),
        format_size(sys_mem.cache))?;
    writeln!(out)?;

    // Scan processes
    writeln!(out, "Scanning processes...")?;
    let mut sys = System::new_all();
    sys.refresh_all();

    let mut processes: Vec<ProcessMemory> = sys.processes().iter().map(|(pid, proc)| {
        let mut p = ProcessMemory::new_simple(pid.as_u32() as i32, proc.name().to_string_lossy().to_string(), proc.memory());
        p.cmdline = proc.cmd().iter().map(|s| s.to_string_lossy().to_string()).collect::<Vec<_>>().join(" ");
        p
    }).collect();

    // Deep scan — processes with RSS >= 5MB (for swap accounting)
    writeln!(out, "Deep analysis (swap accounting via footprint)...")?;
    let rss_threshold = 5 * 1024 * 1024u64;
    let big_pids: Vec<i32> = processes.iter()
        .filter(|p| p.physical_footprint >= rss_threshold)
        .map(|p| p.pid)
        .collect();
    writeln!(out, "Deep-scanning {} processes (skipped {} tiny)...",
        big_pids.len(), processes.len() - big_pids.len())?;

    let compressed_map = top::get_all_processes_compressed().unwrap_or_default();

    // Parallel chunks for CLI too
    let chunks: Vec<Vec<i32>> = big_pids.chunks(20).map(|c| c.to_vec()).collect();
    let mut fp_results: HashMap<i32, crate::footprint::FootprintData> = HashMap::new();

    std::thread::scope(|s| {
        for chunk_batch in chunks.chunks(4) {
            let handles: Vec<_> = chunk_batch
                .iter()
                .map(|chunk| s.spawn(|| footprint::get_footprint_for_pids(chunk)))
                .collect();

            for (i, handle) in handles.into_iter().enumerate() {
                if let Ok(fp_map) = handle.join().unwrap() {
                    for (pid, data) in fp_map {
                        fp_results.insert(pid, data);
                    }
                }
            }
        }
    });

    for p in &mut processes {
        if let Some(data) = fp_results.get(&p.pid) {
            let compressed = *compressed_map.get(&p.pid).unwrap_or(&0);
            p.merge_footprint(data, compressed);
        }
    }

    processes.retain(|p| p.total() > 0);

    // Normalize swap
    normalize_swap(&mut processes, sys_mem.swap_used);

    if args.ps {
        // Ungrouped: just list all processes sorted
        match args.sort.as_str() {
            "phys" => processes.sort_by(|a, b| b.physical_footprint.cmp(&a.physical_footprint)),
            "swap" => processes.sort_by(|a, b| b.swap_disk.cmp(&a.swap_disk)),
            "name" => processes.sort_by(|a, b| a.name.cmp(&b.name)),
            _ => processes.sort_by(|a, b| b.total().cmp(&a.total())),
        }

        writeln!(out, "{:<8} {:<35} {:<12} {:<12} {:<12}",
            "PID", "NAME", "PHYSICAL", "SWAP", "TOTAL")?;
        writeln!(out, "{}", "─".repeat(85))?;
        for p in &processes {
            writeln!(out, "{:<8} {:<35} {:<12} {:<12} {:<12}",
                p.pid,
                truncate(&p.name, 35),
                format_size(p.physical_footprint),
                format_size(p.swap_disk),
                format_size(p.total()),
            )?;
        }
    } else {
        // Grouped view
        let groups = group::group_processes(&processes);
        let total_procs: usize = groups.iter().map(|g| g.processes.len()).sum();

        writeln!(out, "{:<30} {:<7} {:<12} {:<12} {:<12}",
            "APP", "PROCS", "PHYSICAL", "SWAP", "TOTAL")?;
        writeln!(out, "{}", "─".repeat(80))?;

        for g in &groups {
            writeln!(out, "{:<30} {:<7} {:<12} {:<12} {:<12}",
                truncate(&g.name, 30),
                g.processes.len(),
                format_size(g.total_footprint),
                format_size(g.total_swap),
                format_size(g.total()),
            )?;
        }

        writeln!(out)?;
        let sum_swap: u64 = processes.iter().map(|p| p.swap_disk).sum::<u64>();
        let sys_swap = sys_mem.swap_used;
        let delta = sys_swap.saturating_sub(sum_swap);
        writeln!(out, "{} apps • {} processes • {} physical • {} swap (system: {})",
            groups.len(),
            total_procs,
            format_size(processes.iter().map(|p| p.physical_footprint).sum::<u64>()),
            format_size(sum_swap),
            format_size(sys_swap),
        )?;
        if delta > 100 * 1024 * 1024 {
            writeln!(out, "Note: {} swap unaccounted (from processes below scan threshold)", format_size(delta))?;
        };
    }

    Ok(())
}

// ─── TUI Mode ──────────────────────────────────────────────────────────────

fn run_tui() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let (tx, rx) = mpsc::channel();

    // Kick off initial scan
    app.is_loading = true;
    let tx_scan = tx.clone();
    thread::spawn(move || perform_scan(tx_scan));

    let tick_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui::ui(f, &mut app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::from_secs(0));

        if crossterm::event::poll(timeout)?
            && let Event::Key(key) = event::read()?
        {
            if app.detail_view_open {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        app.detail_view_open = false;
                    }
                    KeyCode::Char('r') => {
                        if let Some(detail) = &app.current_detail {
                            let pid = detail.pid;
                            let name = detail.name.clone();
                            let tx = tx.clone();
                            thread::spawn(move || fetch_details(pid, name, tx));
                        }
                    }
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Char('q') => app.quit(),
                    KeyCode::Up | KeyCode::Char('k') => app.prev_group(),
                    KeyCode::Down | KeyCode::Char('j') => app.next_group(),
                    KeyCode::Enter => app.toggle_expand(),
                    KeyCode::Char(' ') => {
                        if app.expanded_group.is_some() {
                            app.next_proc();
                        }
                    }
                    KeyCode::Backspace => {
                        if app.expanded_group.is_some() {
                            app.prev_proc();
                        }
                    }
                    KeyCode::Char('d') => {
                        // Open detail for selected
                        let target = if let Some(gi) = app.expanded_group {
                            if let Some(pi) = app.proc_state.selected() {
                                app.groups.get(gi).and_then(|g| g.processes.get(pi))
                            } else { None }
                        } else {
                            app.group_state.selected().and_then(|i| app.groups.get(i))
                                .map(|g| g.processes.first()).flatten()
                        };

                        if let Some(proc) = target {
                            let pid = proc.pid;
                            let name = proc.name.clone();
                            app.detail_view_open = true;
                            app.current_detail = None;
                            let tx = tx.clone();
                            thread::spawn(move || fetch_details(pid, name, tx));
                        }
                    }
                    KeyCode::Char('r') => {
                        if !app.is_loading && app.deep_scan_progress.is_none() {
                            app.is_loading = true;
                            app.groups.clear();
                            app.all_processes.clear();
                            app.total_swap = 0;
                            app.total_phys = 0;
                            app.expanded_group = None;
                            let tx_scan = tx.clone();
                            thread::spawn(move || perform_scan(tx_scan));
                        }
                    }
                    KeyCode::Char('x') => app.kill_selected(),
                    KeyCode::Char('t') => {
                        app.sort_column = SortColumn::Total;
                        app.sort_desc = true;
                        app.sort_groups();
                    }
                    KeyCode::Char('p') => {
                        app.sort_column = SortColumn::Physical;
                        app.sort_desc = true;
                        app.sort_groups();
                    }
                    KeyCode::Char('s') => {
                        app.sort_column = SortColumn::Swap;
                        app.sort_desc = true;
                        app.sort_groups();
                    }
                    KeyCode::Char('n') => {
                        app.sort_column = SortColumn::Name;
                        app.sort_desc = false;
                        app.sort_groups();
                    }
                    KeyCode::Tab => {
                        app.view_mode = match app.view_mode {
                            ViewMode::Overview => ViewMode::Ps,
                            ViewMode::Ps => ViewMode::Overview,
                        };
                    }
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.on_tick();
            last_tick = Instant::now();
        }

        // Process events
        while let Ok(event) = rx.try_recv() {
            match event {
                ScanEvent::SysMemory(mem) => app.set_system_memory(mem),
                ScanEvent::Start(total) => {
                    app.scan_progress = Some((0, total));
                    app.status_message = Some("Quick scanning...".into());
                }
                ScanEvent::Progress(current, name) => {
                    if let Some((_, total)) = app.scan_progress {
                        app.scan_progress = Some((current, total));
                    }
                    app.status_message = Some(format!("Scanning: {}...", truncate(&name, 30)));
                }
                ScanEvent::Result(proc) => {
                    // Add to processes, will regroup at end
                    app.all_processes.push(proc);
                }
                ScanEvent::DeepScanStart(total) => {
                    app.is_loading = false;
                    app.scan_progress = None;
                    app.deep_scan_progress = Some((0, total));
                    app.status_message = Some(format!("Deep analysis 0/{}...", total));
                }
                ScanEvent::DeepScanProgress(current) => {
                    if let Some((_, total)) = app.deep_scan_progress {
                        app.deep_scan_progress = Some((current, total));
                        app.status_message = Some(format!("Deep analysis {}/{}...", current, total));
                    }
                }
                ScanEvent::BatchUpdate(updates) => {
                    for p in &mut app.all_processes {
                        if let Some((fp_data, compressed)) = updates.get(&p.pid) {
                            p.merge_footprint(fp_data, *compressed);
                        }
                    }
                    app.recalculate_totals();
                }
                ScanEvent::Complete => {
                    app.is_loading = false;
                    app.scan_progress = None;
                    app.deep_scan_progress = None;
                    app.normalize_swap_to_system();
                    app.sort_groups();
                    if app.group_state.selected().is_none() && !app.groups.is_empty() {
                        app.group_state.select(Some(0));
                    }
                    app.status_message = Some(format!(
                        "Ready — {} apps, {} processes",
                        app.groups.len(),
                        app.all_processes.len()
                    ));
                }
                ScanEvent::DetailResult(details) => {
                    app.current_detail = Some(details);
                }
                ScanEvent::SingleResult(pm) => {
                    if let Some(idx) = app.all_processes.iter().position(|p| p.pid == pm.pid) {
                        app.all_processes[idx] = pm;
                        app.recalculate_totals();
                        app.normalize_swap_to_system();
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

// ─── Background scan ──────────────────────────────────────────────────────

fn perform_scan(tx: mpsc::Sender<ScanEvent>) {
    // 1. System memory
    let sys_mem = get_system_memory();
    let _ = tx.send(ScanEvent::SysMemory(sys_mem));

    // 2. Quick scan via sysinfo
    let mut sys = System::new_all();
    sys.refresh_all();

    let entries: Vec<(i32, String, String, u64, usize)> = sys.processes().iter().map(|(pid, proc)| {
        (
            pid.as_u32() as i32,
            proc.name().to_string_lossy().to_string(),
            proc.cmd().iter().map(|s| s.to_string_lossy().to_string()).collect::<Vec<_>>().join(" "),
            proc.memory(),
            1usize,
        )
    }).collect();

    let total = entries.len();
    let _ = tx.send(ScanEvent::Start(total));

    for (i, (pid, name, cmdline, rss, threads)) in entries.iter().enumerate() {
        let _ = tx.send(ScanEvent::Progress(i + 1, name.clone()));
        let mut p = ProcessMemory::new_simple(*pid, name.clone(), *rss);
        p.cmdline = cmdline.clone();
        p.threads = *threads;
        let _ = tx.send(ScanEvent::Result(p));
    }

    // 3. Deep scan: footprint + top compressed
    // Use footprint exclusively for SWAP accounting (not physical, which we get from
    // sysinfo). We scan processes with RSS >= 5MB — below that, swap contribution is
    // negligible and sysinfo RSS is sufficient.
    let rss_threshold = 5 * 1024 * 1024u64; // 5MB
    let big_pids: Vec<i32> = entries
        .iter()
        .filter(|(_, _, _, rss, _)| *rss >= rss_threshold)
        .map(|(pid, _, _, _, _)| *pid)
        .collect();

    let _ = tx.send(ScanEvent::DeepScanStart(big_pids.len()));

    let compressed_map = top::get_all_processes_compressed().unwrap_or_default();

    // Run footprint chunks in parallel using scoped threads.
    // Smaller chunks = less cross-process shared memory analysis overhead.
    let chunk_size = 20;
    let chunks: Vec<Vec<i32>> = big_pids.chunks(chunk_size).map(|c| c.to_vec()).collect();

    let processed_count = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        let max_parallel = 4usize;
        for chunk_batch in chunks.chunks(max_parallel) {
            let handles: Vec<std::thread::ScopedJoinHandle<(Vec<i32>, Result<HashMap<i32, crate::footprint::FootprintData>>)>> = chunk_batch
                .iter()
                .map(|chunk: &Vec<i32>| {
                    let chunk = chunk.clone();
                    s.spawn(move || {
                        let result = footprint::get_footprint_for_pids(&chunk);
                        (chunk, result)
                    })
                })
                .collect();

            for handle in handles {
                let (chunk, fp_result) = handle.join().unwrap();
                if let Ok(fp_map) = fp_result {
                    let mut updates: HashMap<i32, (crate::footprint::FootprintData, u64)> = HashMap::new();
                    for pid in &chunk {
                        if let Some(data) = fp_map.get(pid) {
                            let compressed = *compressed_map.get(pid).unwrap_or(&0);
                            updates.insert(*pid, (data.clone(), compressed));
                        }
                    }
                    if !updates.is_empty() {
                        let _ = tx.send(ScanEvent::BatchUpdate(updates));
                    }
                }
                let prev = processed_count.fetch_add(chunk.len(), std::sync::atomic::Ordering::Relaxed);
                let _ = tx.send(ScanEvent::DeepScanProgress(prev + chunk.len()));
            }
        }
    });

    let _ = tx.send(ScanEvent::Complete);
}

fn fetch_details(pid: i32, name: String, tx: mpsc::Sender<ScanEvent>) {
    let mut sys = System::new();
    let sys_pid = Pid::from(pid as usize);
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    if let Some(proc) = sys.process(sys_pid) {
        let cmd: Vec<String> = proc.cmd().iter().map(|s| s.to_string_lossy().to_string()).collect();
        let exe = proc.exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let cwd = proc.cwd().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let status = proc.status().to_string();
        let start_time = proc.start_time();
        let cpu_usage = proc.cpu_usage();

        let memory_info = scanner::get_process_memory(pid, &name)
            .unwrap_or_else(|_| ProcessMemory::new_simple(pid, name.clone(), proc.memory()));

        let details = ProcessDetail {
            pid,
            name,
            cmd,
            exe,
            cwd,
            status,
            start_time,
            cpu_usage,
            memory_info,
        };
        let _ = tx.send(ScanEvent::DetailResult(details));
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}…", &s[..max - 1])
    } else {
        s.to_string()
    }
}

/// No-op: swap normalization removed.
/// The old normalization redistributed "missing" swap from unscanned processes
/// to the ones we did scan, inflating numbers (e.g. 7GB swap for a 1.8GB process).
/// We now use raw footprint swap estimates — more honest per-process numbers.
fn normalize_swap(_processes: &mut [ProcessMemory], _system_swap_bytes: u64) {
    // swap_disk = swap_disk_est (already set during merge_footprint)
}
