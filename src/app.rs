use crate::group::AppGroup;
use crate::scanner::{ProcessMemory, SystemMemory};
use ratatui::widgets::TableState;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortColumn {
    Name,
    Physical,
    Swap,
    Total,
    Processes,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewMode {
    Overview,
    Ps,
}

pub struct App {
    pub should_quit: bool,
    pub view_mode: ViewMode,

    // Data
    pub system_memory: Option<SystemMemory>,
    pub groups: Vec<AppGroup>,
    pub all_processes: Vec<ProcessMemory>,

    // UI state
    pub group_state: TableState,
    pub proc_state: TableState,
    pub expanded_group: Option<usize>,
    pub sort_column: SortColumn,
    pub sort_desc: bool,

    // Loading
    pub is_loading: bool,
    pub scan_progress: Option<(usize, usize)>,
    pub deep_scan_progress: Option<(usize, usize)>,
    pub status_message: Option<String>,
    pub spinner_idx: usize,

    // Detail popup
    pub detail_view_open: bool,
    pub current_detail: Option<ProcessDetail>,

    // Totals
    pub total_swap: u64,
    pub total_phys: u64,
}

#[derive(Debug, Clone)]
pub struct ProcessDetail {
    pub pid: i32,
    pub name: String,
    pub cmd: Vec<String>,
    pub exe: String,
    pub cwd: String,
    pub status: String,
    pub start_time: u64,
    pub cpu_usage: f32,
    pub memory_info: ProcessMemory,
}

impl App {
    pub fn new() -> Self {
        Self {
            should_quit: false,
            view_mode: ViewMode::Overview,
            system_memory: None,
            groups: Vec::new(),
            all_processes: Vec::new(),
            group_state: TableState::default(),
            proc_state: TableState::default(),
            expanded_group: None,
            sort_column: SortColumn::Total,
            sort_desc: true,
            is_loading: false,
            scan_progress: None,
            deep_scan_progress: None,
            status_message: None,
            spinner_idx: 0,
            detail_view_open: false,
            current_detail: None,
            total_swap: 0,
            total_phys: 0,
        }
    }

    pub fn on_tick(&mut self) {
        if self.is_loading || self.deep_scan_progress.is_some() {
            self.spinner_idx = self.spinner_idx.wrapping_add(1);
        }
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    pub fn next_group(&mut self) {
        let len = self.groups.len();
        if len == 0 { return; }
        let i = match self.group_state.selected() {
            Some(i) => (i + 1) % len,
            None => 0,
        };
        self.group_state.select(Some(i));
    }

    pub fn prev_group(&mut self) {
        let len = self.groups.len();
        if len == 0 { return; }
        let i = match self.group_state.selected() {
            Some(i) => if i == 0 { len - 1 } else { i - 1 },
            None => 0,
        };
        self.group_state.select(Some(i));
    }

    pub fn toggle_expand(&mut self) {
        if let Some(i) = self.group_state.selected() {
            if self.expanded_group == Some(i) {
                self.expanded_group = None;
            } else {
                self.expanded_group = Some(i);
                self.proc_state.select(Some(0));
            }
        }
    }

    pub fn next_proc(&mut self) {
        if self.expanded_group.is_none() { return; }
        if let Some(gi) = self.expanded_group {
            if let Some(g) = self.groups.get(gi) {
                let len = g.processes.len();
                if len == 0 { return; }
                let i = match self.proc_state.selected() {
                    Some(i) => (i + 1) % len,
                    None => 0,
                };
                self.proc_state.select(Some(i));
            }
        }
    }

    pub fn prev_proc(&mut self) {
        if self.expanded_group.is_none() { return; }
        if let Some(gi) = self.expanded_group {
            if let Some(g) = self.groups.get(gi) {
                let len = g.processes.len();
                if len == 0 { return; }
                let i = match self.proc_state.selected() {
                    Some(i) => if i == 0 { len - 1 } else { i - 1 },
                    None => 0,
                };
                self.proc_state.select(Some(i));
            }
        }
    }

    pub fn sort_groups(&mut self) {
        self.groups.sort_by(|a, b| {
            let ord = match self.sort_column {
                SortColumn::Name => a.name.cmp(&b.name),
                SortColumn::Physical => a.total_footprint.cmp(&b.total_footprint),
                SortColumn::Swap => a.total_swap.cmp(&b.total_swap),
                SortColumn::Total => a.total().cmp(&b.total()),
                SortColumn::Processes => a.processes.len().cmp(&b.processes.len()),
            };
            if self.sort_desc { ord.reverse() } else { ord }
        });
    }

    pub fn set_system_memory(&mut self, mem: SystemMemory) {
        self.system_memory = Some(mem);
    }

    pub fn set_processes(&mut self, processes: Vec<ProcessMemory>) {
        self.total_phys = processes.iter().map(|p| p.physical_footprint).sum();
        self.total_swap = processes.iter().map(|p| p.swap_disk).sum();
        self.all_processes = processes.clone();
        self.groups = crate::group::group_processes(&processes);
        if self.group_state.selected().is_none() && !self.groups.is_empty() {
            self.group_state.select(Some(0));
        }
    }

    pub fn recalculate_totals(&mut self) {
        self.total_swap = self.all_processes.iter().map(|p| p.swap_disk).sum();
        self.total_phys = self.all_processes.iter().map(|p| p.physical_footprint).sum();
    }

    /// Normalize per-process swap estimates against the system-wide swap total.
    ///
    /// We NO LONGER scale per-process swap to match the system total.
    /// The old normalization redistributed "missing" swap (from processes we didn't
    /// scan with footprint) to the ones we did scan, producing inflated numbers
    /// like 7 GB swap for a uvicorn process that actually has 1.8 GB swapped.
    ///
    /// Instead, we keep raw footprint estimates. The per-process swap numbers
    /// are accurate estimates from footprint. The UI shows system swap total
    /// separately so the user can see what's unaccounted for.
    pub fn normalize_swap_to_system(&mut self) {
        // swap_disk = swap_disk_est (already set during merge_footprint)
        // Just recalculate totals and regenerate groups.
        self.recalculate_totals();
        self.groups = crate::group::group_processes(&self.all_processes);
    }

    pub fn health_status(&self) -> (&'static str, ratatui::style::Color) {
        if let Some(mem) = &self.system_memory {
            let swap_pct = mem.swap_pct();
            let used_pct = mem.used_pct();
            if swap_pct > 80.0 || used_pct > 95.0 {
                return ("CRITICAL", ratatui::style::Color::Red);
            } else if swap_pct > 50.0 || used_pct > 85.0 {
                return ("WARNING", ratatui::style::Color::Yellow);
            } else {
                return ("OK", ratatui::style::Color::Green);
            }
        }
        ("UNKNOWN", ratatui::style::Color::Gray)
    }

    pub fn kill_selected(&mut self) {
        let pid = if let Some(gi) = self.expanded_group {
            // Kill selected sub-process
            if let Some(pi) = self.proc_state.selected() {
                self.groups.get(gi).and_then(|g| g.processes.get(pi)).map(|p| p.pid)
            } else { None }
        } else {
            // Kill main process of selected group
            self.group_state.selected().and_then(|i| self.groups.get(i)).map(|g| g.processes.first().map(|p| p.pid)).flatten()
        };

        if let Some(pid) = pid {
            let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).output();
        }
    }
}
