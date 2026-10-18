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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub popup_area: Option<Rect>,   // Some while the detail popup is on screen
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
            popup_area: None,
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
        for g in &mut self.groups {
            Self::sort_children(&mut g.processes, &self.sort_column, self.sort_desc);
        }
    }

    // ─── Data updates ──────────────────────────────────────────────────

/// Children share the parent view's active sort key; equal keys tie-break by
/// ascending pid so ordering is deterministic across refreshes.
fn sort_children(procs: &mut [ProcessMemory], col: &SortColumn, desc: bool) {
    procs.sort_by(|a, b| {
        let mut ord = match col {
            SortColumn::Name => a.name.cmp(&b.name),
            SortColumn::Physical => a.rss.cmp(&b.rss),
            SortColumn::Swap => a.swap.cmp(&b.swap),
            SortColumn::Total => a.total().cmp(&b.total()),
        };
        if desc { ord = ord.reverse(); }
        ord.then_with(|| a.pid.cmp(&b.pid))
    });
}

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
        self.sort_groups();
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
    /// While the detail popup is open, a left-click outside its area closes it.
    /// Clicks inside the popup are swallowed (it has no interactive widgets).
    /// Other mouse events are gated on `detail_view_open` by the caller.
    pub fn dismiss_popup_if_outside(&mut self, row: u16, col: u16) {
        let outside = self.popup_area.map_or(true, |r| {
            col < r.x || col >= r.x + r.width || row < r.y || row >= r.y + r.height
        });
        if outside {
            self.detail_view_open = false;
        }
    }

    pub fn scroll_table(&mut self, up: bool) {
        if up { self.prev_group(); } else { self.next_group(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_popup() -> App {
        let mut app = App::new();
        app.detail_view_open = true;
        // 10x10 popup with top-left at (col=5, row=5)
        app.popup_area = Some(Rect::new(5, 5, 10, 10));
        app
    }

    #[test]
    fn popup_click_outside_dismisses() {
        let mut app = app_with_popup();
        app.dismiss_popup_if_outside(0, 0); // top-left of screen — outside
        assert!(!app.detail_view_open, "outside click should close the popup");
    }

    #[test]
    fn popup_click_inside_keeps_open() {
        let mut app = app_with_popup();
        app.dismiss_popup_if_outside(9, 9); // centre — inside
        assert!(app.detail_view_open, "inside click should keep the popup open");
    }

    #[test]
    fn popup_dismiss_is_noop_when_closed() {
        let mut app = App::new();
        app.detail_view_open = false;
        app.dismiss_popup_if_outside(0, 0); // must not panic with popup_area == None
        assert!(!app.detail_view_open);
    }

    #[test]
    fn popup_boundary_is_half_open() {
        let mut app = app_with_popup();
        app.dismiss_popup_if_outside(5, 5); // top-left corner — inside
        assert!(app.detail_view_open, "corner (x,y) is inside");

        let mut app = app_with_popup();
        app.dismiss_popup_if_outside(5, 15); // col == x+width — outside
        assert!(!app.detail_view_open, "col == x+width is outside");
    }

    // ─── Sorting ────────────────────────────────────────────────────

    fn pm(pid: i32, name: &str, rss: u64, swap: u64) -> ProcessMemory {
        let mut p = ProcessMemory::new(pid, name.to_string(), rss);
        p.swap = swap;
        p
    }

    #[test]
    fn children_follow_active_sort_with_pid_tiebreak() {
        let mut procs = vec![
            pm(1, "node", 100, 0),   // total 100
            pm(3, "node", 50, 200),  // total 250
            pm(2, "node", 50, 200),  // total 250 — pid tie-break
        ];
        App::sort_children(&mut procs, &SortColumn::Total, true);
        assert_eq!([procs[0].pid, procs[1].pid, procs[2].pid], [2, 3, 1]);
        // Ascending direction flips the key but keeps the pid tie-break ascending.
        App::sort_children(&mut procs, &SortColumn::Total, false);
        assert_eq!([procs[0].pid, procs[1].pid, procs[2].pid], [1, 2, 3]);
    }

    #[test]
    fn recalculate_respects_nondefault_sort() {
        // Amendment 1: a regroup under a nondefault sort must re-sort parents
        // too, not fall back to group_processes' internal TOTAL-desc order.
        let mut app = App::new();
        app.sort_column = SortColumn::Name;
        app.sort_desc = false;
        app.recalculate_from(&[pm(1, "bb", 100, 0), pm(2, "aa", 900, 0)]);
        assert_eq!(app.groups[0].name, "aa", "parents ordered by active key");
        assert_eq!(app.groups[0].processes[0].name, "aa");
    }
}
