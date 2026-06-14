use crate::group::{AppGroup, GroupMode};
use crate::scanner::{ProcessMemory, SystemMemory};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;

// ─── Visual row mapping ────────────────────────────────────────────────────

/// Maps a visual table row index to (group_index, Some(proc_index) or None for group header)
#[derive(Default)]
pub struct RowMap {
    pub entries: Vec<(usize, Option<usize>)>,
}

impl RowMap { pub fn new() -> Self { Self::default() } }

// ─── Sort ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum SortColumn { Name, Physical, Swap, Total }

// ─── View mode ──────────────────────────────────────────────────────────────
// ViewMode { Overview, Ps } removed — see ADR 0001. The TUI's old Tab toggle
// only relabelled the title; both modes rendered app-grouped data. Replaced by
// GroupMode { App, Project } which actually changes the grouping key.

// ─── Process detail (for popup) ────────────────────────────────────────────

pub struct ProcessDetail {
    pub pid: i32,
    pub name: String,
    pub cmd: Vec<String>,
    pub exe: String,
    pub cwd: String,
    pub status: String,
    pub start_time: u64,
    pub cpu_usage: f32,
    // Memory — read directly from app.all_processes, not re-fetched
    pub rss: u64,
    pub swap: u64,
    pub parent_pid: Option<i32>,
    pub parent_cmd: Option<String>,
}

// ─── App state ──────────────────────────────────────────────────────────────

pub struct App {
    pub should_quit: bool,
    pub group_mode: GroupMode,
    pub system_memory: Option<SystemMemory>,
    pub groups: Vec<AppGroup>,
    pub all_processes: Vec<ProcessMemory>,
    pub group_state: TableState,
    pub proc_state: TableState,
    pub expanded_group: Option<usize>,
    pub row_map: RowMap,
    pub table_area: Rect,
    pub sort_column: SortColumn,
    pub sort_desc: bool,
    pub is_loading: bool,
    pub scan_progress: Option<(usize, usize)>,
    pub deep_scan_progress: Option<(usize, usize)>,
    pub status_message: Option<String>,
    pub detail_view_open: bool,
    pub current_detail: Option<ProcessDetail>,
    pub spinner_idx: usize,
    pub total_phys: u64,
    pub total_swap: u64,
}

impl App {
    pub fn new() -> Self {
        Self {
            should_quit: false,
            group_mode: GroupMode::Project,
            system_memory: None,
            groups: Vec::new(),
            all_processes: Vec::new(),
            group_state: TableState::default(),
            proc_state: TableState::default(),
            expanded_group: None,
            row_map: RowMap::new(),
            table_area: Rect::default(),
            sort_column: SortColumn::Total,
            sort_desc: true,
            is_loading: false,
            scan_progress: None,
            deep_scan_progress: None,
            status_message: None,
            detail_view_open: false,
            current_detail: None,
            spinner_idx: 0,
            total_phys: 0,
            total_swap: 0,
        }
    }

    pub fn quit(&mut self) { self.should_quit = true; }

    pub fn on_tick(&mut self) { self.spinner_idx = self.spinner_idx.wrapping_add(1); }

    // ─── Navigation ─────────────────────────────────────────────────────

    pub fn next_group(&mut self) {
        let len = self.row_map.entries.len();
        if len == 0 { return; }
        let i = match self.group_state.selected() {
            Some(i) => (i + 1) % len,
            None => 0,
        };
        self.group_state.select(Some(i));
        self.sync_from_visual(i);
    }

    pub fn prev_group(&mut self) {
        let len = self.row_map.entries.len();
        if len == 0 { return; }
        let i = match self.group_state.selected() {
            Some(i) => if i == 0 { len - 1 } else { i - 1 },
            None => 0,
        };
        self.group_state.select(Some(i));
        self.sync_from_visual(i);
    }

    fn sync_from_visual(&mut self, visual_idx: usize) {
        if let Some((gi, pi)) = self.row_map.entries.get(visual_idx) {
            match pi {
                Some(pidx) => {
                    if self.expanded_group != Some(*gi) { self.expanded_group = Some(*gi); }
                    self.proc_state.select(Some(*pidx));
                }
                None => { self.proc_state.select(None); }
            }
        }
    }

    pub fn selected_process(&self) -> Option<(usize, Option<usize>)> {
        let visual_idx = self.group_state.selected()?;
        self.row_map.entries.get(visual_idx).copied()
    }

    pub fn toggle_expand(&mut self) {
        if let Some((gi, _pi)) = self.selected_process() {
            if self.expanded_group == Some(gi) {
                self.expanded_group = None;
                self.proc_state.select(None);
            } else {
                self.expanded_group = Some(gi);
                self.proc_state.select(Some(0));
            }
        }
    }

    // ─── Group mode ────────────────────────────────────────────────────

    /// Toggle App ↔ Project grouping and regroup from scanned data.
    pub fn cycle_group(&mut self) {
        self.group_mode = match self.group_mode {
            GroupMode::Project => GroupMode::App,
            GroupMode::App => GroupMode::Project,
        };
        let procs = self.all_processes.clone();
        self.recalculate_from(&procs);
        self.expanded_group = None;
        self.proc_state.select(None);
        if !self.groups.is_empty() {
            self.group_state.select(Some(0));
        }
    }

    // ─── Sort ───────────────────────────────────────────────────────────

    pub fn sort_groups(&mut self) {
        self.groups.sort_by(|a, b| {
            let ord = match self.sort_column {
                SortColumn::Name => a.name.cmp(&b.name),
                SortColumn::Physical => a.total_rss.cmp(&b.total_rss),
                SortColumn::Swap => a.total_swap.cmp(&b.total_swap),
                SortColumn::Total => a.total().cmp(&b.total()),
            };
            if self.sort_desc { ord.reverse() } else { ord }
        });
    }

    // ─── Data updates ──────────────────────────────────────────────────

    pub fn set_system_memory(&mut self, mem: SystemMemory) {
        self.system_memory = Some(mem);
    }

    pub fn set_processes(&mut self, processes: Vec<ProcessMemory>) {
        self.recalculate_from(&processes);
        self.all_processes = processes;
        if self.group_state.selected().is_none() && !self.groups.is_empty() {
            self.group_state.select(Some(0));
        }
    }

    pub fn recalculate_from(&mut self, processes: &[ProcessMemory]) {
        self.total_phys = processes.iter().map(|p| p.rss).sum();
        self.total_swap = processes.iter().map(|p| p.swap).sum();
        self.groups = crate::group::group_processes(processes, self.group_mode);
    }

    // ─── Health ─────────────────────────────────────────────────────────

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

    // ─── Actions ────────────────────────────────────────────────────────

    pub fn kill_selected(&mut self) {
        let pid = if let Some((gi, pi)) = self.selected_process() {
            if let Some(pidx) = pi {
                self.groups.get(gi).and_then(|g| g.processes.get(pidx)).map(|p| p.pid)
            } else {
                self.groups.get(gi).and_then(|g| g.processes.first()).map(|p| p.pid)
            }
        } else { None };

        if let Some(pid) = pid {
            let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).output();
        }
    }

    pub fn click_table(&mut self, row: u16, _col: u16) {
        let header_offset = self.table_area.y + 3;
        if row < header_offset { return; }
        let visual_idx = (row - header_offset) as usize;
        if visual_idx >= self.row_map.entries.len() { return; }
        self.group_state.select(Some(visual_idx));
        self.sync_from_visual(visual_idx);
    }

    pub fn scroll_table(&mut self, up: bool) {
        if up { self.prev_group(); } else { self.next_group(); }
    }
}
