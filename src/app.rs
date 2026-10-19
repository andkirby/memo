use crate::group::{AppGroup, GroupMode};
use crate::scanner::{ProcessMemory, SystemMemory};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;
use std::time::{Duration, Instant};

/// Two clicks on the same spot within this window count as a double-click.
const DOUBLE_CLICK_MS: u64 = 400;

/// What a left-click asks the caller to do after App updated its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickAction {
    /// Selection moved to the clicked row; nothing further to do.
    Select,
    /// Double-click on a group header — expansion was toggled.
    ToggleExpand,
    /// Double-click on a process row — open the detail popup.
    OpenDetail,
}

// ─── Visual row mapping ────────────────────────────────────────────

/// Maps a visual table row index to its identity: (group name, Some(pid))
/// for a process row or (group name, None) for a group header. Group names
/// are unique keys; positions shift whenever groups re-sort or expansion
/// changes — identities do not.
#[derive(Default)]
pub struct RowMap {
    pub entries: Vec<(String, Option<i32>)>,
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
    pub expanded_group: Option<String>,
    /// Identity of the highlighted row: (group name, Some(pid) | None).
    pub selected_key: Option<(String, Option<i32>)>,
    pub row_map: RowMap,
    pub table_area: Rect,
    pub sort_column: SortColumn,
    pub sort_desc: bool,
    pub last_click: Option<(u16, u16, Instant, usize)>,
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
            selected_key: None,
            row_map: RowMap::new(),
            table_area: Rect::default(),
            sort_column: SortColumn::Total,
            sort_desc: true,
            last_click: None,
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
        if let Some((name, pid)) = self.row_map.entries.get(visual_idx).cloned() {
            self.selected_key = Some((name.clone(), pid));
            match pid {
                Some(pid) => {
                    if self.expanded_group.as_deref() != Some(name.as_str())
                        && let Some(gi) = self.groups.iter().position(|g| g.name == name)
                        && let Some(pi) = self.groups[gi].processes.iter().position(|p| p.pid == pid)
                    {
                        self.proc_state.select(Some(pi));
                    }
                }
                None => { self.proc_state.select(None); }
            }
        }
    }

    pub fn selected_process(&self) -> Option<(String, Option<i32>)> {
        let visual_idx = self.group_state.selected()?;
        self.row_map.entries.get(visual_idx).cloned()
    }

    /// Resolve the selected identity to current positions
    /// (group_index, Option<proc_index>). Positions shift on re-sort;
    /// identities do not.
    pub fn selected_location(&self) -> Option<(usize, Option<usize>)> {
        let (name, pid) = self.selected_key.as_ref()?;
        let gi = self.groups.iter().position(|g| &g.name == name)?;
        let pi = pid.and_then(|pid| self.groups[gi].processes.iter().position(|p| p.pid == pid));
        Some((gi, pi))
    }

    /// Re-position the visual selection on the selected identity's row after
    /// the row list was rebuilt (re-sort, expansion change, scan refresh).
    /// Falls back to the current index when the identity no longer exists.
    pub fn resync_selection(&mut self) {
        if let Some((name, pid)) = &self.selected_key
            && let Some(idx) = self
                .row_map
                .entries
                .iter()
                .position(|e| &e.0 == name && e.1 == *pid)
        {
            self.group_state.select(Some(idx));
        }
    }

    pub fn toggle_expand(&mut self) {
        if let Some((name, _pid)) = self.selected_process() {
            if self.expanded_group.as_deref() == Some(name.as_str()) {
                self.expanded_group = None;
                self.proc_state.select(None);
            } else {
                self.expanded_group = Some(name);
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
        let pid = match self.selected_location() {
            Some((gi, Some(pi))) => self.groups.get(gi).and_then(|g| g.processes.get(pi)).map(|p| p.pid),
            Some((gi, None)) => self.groups.get(gi).and_then(|g| g.processes.first()).map(|p| p.pid),
            None => None,
        };

        if let Some(pid) = pid {
            let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).output();
        }
    }

    pub fn click_table(&mut self, row: u16, _col: u16) {
        let header_offset = self.table_area.y + 3;
        if row < header_offset { return; }
        // The table can be scrolled: a screen row shows item `offset + k`, not
        // item `k`. Screen row 0 under the header is item `offset`.
        let screen_idx = (row - header_offset) as usize;
        let visual_idx = screen_idx + self.group_state.offset();
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

    // ─── Mouse clicks ───────────────────────────────────────────────────

    /// Handle one left-click at the given terminal position. The clicked row
    /// ALWAYS becomes the selected row — even on the second click of a
    /// double — so the toggle below acts on exactly the group under the
    /// cursor, never on a stale selection.
    pub fn click(&mut self, row: u16, col: u16, now: Instant) -> ClickAction {
        self.click_table(row, col);

        // The table is re-rendered between clicks, so any layout change
        // (expansion toggle, periodic re-sort) changes the row count. A
        // stored click from a different layout is stale: the same screen
        // position now shows a different row, and it must not read as double.
        let rows = self.row_map.entries.len();
        let is_double = self
            .last_click
            .is_some_and(|(r, c, t, rows_at_click)| {
                r == row
                    && c == col
                    && rows_at_click == rows
                    && now.duration_since(t) < Duration::from_millis(DOUBLE_CLICK_MS)
            });

        let action = if is_double {
            match self.selected_process() {
                Some((_gi, Some(_pid))) => ClickAction::OpenDetail,
                Some((_gi, None)) => {
                    self.toggle_expand();
                    ClickAction::ToggleExpand
                }
                None => ClickAction::Select,
            }
        } else {
            ClickAction::Select
        };

        self.last_click = Some((row, col, now, rows));
        action
    }

    pub fn scroll_table(&mut self, up: bool) {
        if up { self.prev_group(); } else { self.next_group(); }
    }

    // ─── Paging ─────────────────────────────────────────────────────────

    /// Visible data rows: area height minus borders (2), header (1) and its
    /// bottom margin (1). Falls back to 1 before the first render.
    fn page_size(&self) -> usize {
        self.table_area.height.saturating_sub(5).max(1) as usize
    }

    pub fn page_up(&mut self) {
        let len = self.row_map.entries.len();
        if len == 0 { return; }
        let i = self.group_state.selected().unwrap_or(0).saturating_sub(self.page_size());
        self.group_state.select(Some(i));
        self.sync_from_visual(i);
    }

    pub fn page_down(&mut self) {
        let len = self.row_map.entries.len();
        if len == 0 { return; }
        let i = (self.group_state.selected().unwrap_or(0) + self.page_size()).min(len - 1);
        self.group_state.select(Some(i));
        self.sync_from_visual(i);
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

    // ─── Mouse + paging ─────────────────────────────────────────────

    fn app_with_rows(n: usize) -> App {
        let mut app = App::new();
        app.table_area = Rect::new(0, 6, 100, 24);
        app.row_map.entries = (0..n).map(|i| (format!("g{i}"), None)).collect();
        app
    }

    #[test]
    fn click_accounts_for_scroll_offset() {
        let mut app = app_with_rows(30);
        // Scrolled so item 7 is the first visible data row.
        *app.group_state.offset_mut() = 7;
        app.click_table(6 + 3, 10); // first data row on screen
        assert_eq!(app.selected_process(), Some(("g7".into(), None)), "click at first visible row selects the displayed item, not item 0");
        app.click_table(6 + 3 + 2, 10);
        assert_eq!(app.selected_process(), Some(("g9".into(), None)));
    }

    #[test]
    fn click_ignores_header_and_past_end() {
        let mut app = app_with_rows(5);
        app.group_state.select(Some(2));
        app.click_table(6 + 2, 10); // header/margin rows
        assert_eq!(app.selected_process(), Some(("g2".into(), None)), "click above data rows is a no-op");
        app.click_table(6 + 3 + 50, 10);
        assert_eq!(app.selected_process(), Some(("g2".into(), None)), "click past the list is a no-op");
    }

    #[test]
    fn paging_moves_by_page_and_clamps() {
        let mut app = app_with_rows(30);
        app.group_state.select(Some(0));
        app.page_down();
        assert_eq!(app.selected_process(), Some(("g19".into(), None)), "page_size = 24-5 = 19");
        app.page_down();
        assert_eq!(app.selected_process(), Some(("g29".into(), None)), "clamped to last row");
        app.page_up();
        assert_eq!(app.selected_process(), Some(("g10".into(), None)));
        app.page_up();
        assert_eq!(app.selected_process(), Some(("g0".into(), None)), "clamped to first row");
        app.page_up();
        assert_eq!(app.selected_process(), Some(("g0".into(), None)));
    }

    // ─── Double-click handling ──────────────────────────────────────

    fn t(ms: u64) -> Instant {
        Instant::now() - Duration::from_millis(10_000) + Duration::from_millis(ms)
    }

    #[test]
    fn double_click_expands_the_clicked_group_not_a_stale_selection() {
        // Regression: with group 0 expanded, double-clicking group 1's header
        // must expand group 1 — the old code skipped the selection update on
        // the second click and toggled the previous selection instead.
        let mut app = app_with_rows(3);
        app.expanded_group = Some("g0".into());
        let row1 = 6 + 3 + 1; // group 1's header row (group 0 header + 1 child)
        assert_eq!(app.click(row1, 10, t(0)), ClickAction::Select);
        assert_eq!(app.selected_process(), Some(("g1".into(), None)));
        assert_eq!(app.click(row1, 10, t(100)), ClickAction::ToggleExpand);
        assert_eq!(app.expanded_group.as_deref(), Some("g1"), "clicked group toggled");
        assert_eq!(app.selected_process(), Some(("g1".into(), None)), "clicked row stays active");
    }

    #[test]
    fn expansion_change_invalidates_double_click_tracking() {
        // Regression: after rows shift (expansion change + re-render), a fast
        // click at the same SCREEN position is a different row — it must not
        // toggle.
        let mut app = app_with_rows(5);
        app.expanded_group = Some("g0".into()); // rows below shift by group 0's children
        app.click(6 + 3 + 3, 10, t(0)); // select row 3 (group 3)
        assert_eq!(app.selected_process(), Some(("g3".into(), None)));
        // Group 0 collapses and the table re-renders: one row disappears.
        app.expanded_group = None;
        app.row_map.entries.remove(1);
        let action = app.click(6 + 3 + 3, 10, t(100));
        assert_eq!(action, ClickAction::Select, "shifted row must not read as double");
        assert_eq!(app.expanded_group, None, "no accidental toggle");
    }

    #[test]
    fn double_click_on_child_row_requests_detail() {
        let mut app = app_with_rows(3);
        app.expanded_group = Some("g0".into());
        app.row_map.entries.insert(1, ("g0".into(), Some(0))); // child under group 0
        let child_row = 6 + 3 + 1;
        app.click(child_row, 10, t(0));
        assert_eq!(app.click(child_row, 10, t(100)), ClickAction::OpenDetail);
        assert_eq!(app.expanded_group.as_deref(), Some("g0"), "detail double-click must not collapse");
    }

    #[test]
    fn rapid_third_click_does_not_oscillate() {
        let mut app = app_with_rows(3);
        let row0 = 6 + 3;
        app.click(row0, 10, t(0));
        assert_eq!(app.click(row0, 10, t(50)), ClickAction::ToggleExpand);
        // The re-render inserts group 0's child row (layout changed).
        app.row_map.entries.insert(1, ("g0".into(), Some(0)));
        // Third fast click is a plain select, not another toggle.
        assert_eq!(app.click(row0, 10, t(100)), ClickAction::Select);
        assert_eq!(app.expanded_group.as_deref(), Some("g0"));
    }

    #[test]
    fn selection_follows_group_after_reshuffle() {
        // The whole point of identity-based selection: groups re-sort on every
        // scan; after the row list is rebuilt, the highlight must stay on the
        // same GROUP, not the same physical row.
        let mut app = app_with_rows(3);
        app.selected_key = Some(("g2".into(), None));
        app.group_state.select(Some(2)); // g2's current physical row
        app.resync_selection();
        // Refresh re-sorts: g2 moves to the front.
        app.row_map.entries = vec!["g2".into(), "g0".into(), "g1".into()]
            .into_iter()
            .map(|n| (n, None))
            .collect();
        app.resync_selection();
        assert_eq!(app.group_state.selected(), Some(0), "highlight follows g2 to its new row");
        assert_eq!(app.selected_process().map(|(n, _)| n), Some("g2".into()));
    }

    #[test]
    fn enter_toggles_clicked_group_identity_only() {
        // Regression for the reported bug: with "com" expanded, Enter/click on
        // "bb" must toggle bb and never touch com.
        let mut app = app_with_rows(3);
        app.expanded_group = Some("com".into());
        app.selected_key = Some(("bb".into(), None));
        app.group_state.select(Some(1)); // bb's header row
        app.row_map.entries = vec![("com".into(), None), ("bb".into(), None), ("x".into(), None)];
        app.toggle_expand();
        assert_eq!(app.expanded_group.as_deref(), Some("bb"));
        app.toggle_expand();
        assert_eq!(app.expanded_group, None);
    }
}
