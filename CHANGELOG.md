# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.3.2] - 2026-10-19

### Fixed
- **Selection and expansion now follow groups, not positions.** The selected
  row and the expanded group were stored as list indices; every scan refresh
  re-sorts groups, so the highlight stayed at the same physical row while
  groups moved under it — clicking a group or pressing Enter could act on a
  different one (the reported "com.apple collapses, bb opens" jump). Selection
  and expansion are now identity-based (group name + pid) and re-anchored on
  every rebuild: the highlight follows the clicked/selected group across
  re-sorts, and Enter toggles exactly the group under the cursor.

## [0.3.1] - 2026-10-18

### Fixed
- **Mouse clicks no longer toggle the wrong group.** The second click of a
  double-click skipped the selection update, so after rows shifted (a group
  collapsed/expanded above, or a periodic re-sort) the toggle acted on the
  previous selection and the highlight landed beside the clicked row. Every
  click now selects the clicked row first, double-clicks toggle exactly the
  clicked group, and double-click tracking resets whenever the table layout
  changes.

## [0.3.0] - 2026-10-18

### Added
- **`memo inspect` — project-scoped memory snapshot for agents.** Resolves
  the current project automatically (nearest enclosing `.git` — directory or
  worktree file — else nearest project marker), attributes processes by cwd
  (scope folder and descendants, inspector excluded), and reports total
  process count, aggregate RSS, and the top `--limit` (default 5) processes
  sorted by RSS with a compact command label. `--json` emits a stable
  versioned single-line object (integer bytes, compact error objects,
  exit 2 for scope/argument errors); `--project PATH` selects an exact
  directory anywhere on disk. Totals always cover every matched process;
  swap is omitted (not measured) in this release. Contract and rules:
  `docs/agent-inspection.md`.
- **Version flags** — `-v` / `--version` print the version without starting
  the TUI.

### Fixed
- **Expanded project groups are readable.** Child rows show process name,
  pid, and command in a tree-guide layout, follow the active sort key
  (headers show `TOTAL ↓`-style arrows), numeric columns are right-aligned,
  the RUNTIMES cell fits whole `name:count` tokens (`node:100 +2`) at the
  column's real width, and zero swap renders neutral instead of green.
- **Mouse clicks land on the displayed row after scrolling.** The click
  handler ignored the table's scroll offset, so a click selected a row
  several positions above the one clicked.
- **PageUp/PageDown (Fn+↑ / Fn+↓) page the process table**, clamped at both
  ends.

## [0.2.1] - 2026-07-28

### Fixed
- **Compressed memory now shown in the TUI.** The RAM bar segment and
  label both rendered `0`, discarding the value the scanner computed —
  the bar read "Compressed 0B" and the usage percentage was ~30 pts low.
  The scanner and `--cli` path were already correct; the bug was TUI-only.
- **Detail popup no longer lets clicks pass through.** With the popup
  open, mouse scroll and right-click acted on the table behind it and a
  left-click selected rows behind it. The popup is now modal: scroll and
  right-click are swallowed, and a left-click outside dismisses it.

## [0.2.0] - 2026-06-14

### Added
- **Project grouping mode** — group processes by project directory, resolved
  from cwd via marker walkup (`.git` / `package.json` / `pyproject.toml` / …).
  All runtimes of one project (node + bun + python + shells + agents) merge
  into one row. See `docs/adr/0001-group-by-project.md`.
- **Bridge column** — shows the other view's key (runtime breakdown in Project
  mode, distinct project count in App mode) so switching views loses no info.
- `--group app|project` CLI flag (default `app`; `--ps` unchanged).
- Bulk `lsof -d cwd` backfill for cwd where sysinfo omits it (one call,
  ~0.14s).
- TUI default mode is now **Project**; `Tab` toggles App ↔ Project.

### Changed
- `Tab` previously toggled Overview/Ps (a no-op that only relabelled the
  title); it now toggles App ↔ Project for real.
- `THREADS` column removed (sysinfo 0.33 doesn't expose threads — always 1);
  replaced by the bridge column.
- Dev runtimes (node/bun/deno/python) group to a single row each in App mode;
  the per-project split moves to the bridge column.

### Removed
- `extract_project` / `extract_python_module` hint logic (superseded by marker
  walkup + bridge column).
- `ViewMode { Overview, Ps }` enum (replaced by `GroupMode { App, Project }`).

## [0.1.0] - 2026-06-11

### Added
- Interactive TUI with ratatui — grouped process table, segmented memory bars, detail popup
- CLI mode (`--cli` for grouped, `--ps` for ungrouped)
- Process grouping by app name (Chrome, Electron, Node, Python, JetBrains, etc.)
- System memory header: RAM and Swap segmented bars with App/Wired/Compressed/Cache breakdown
- Health status indicator (OK/WARNING/CRITICAL) based on swap and RAM pressure
- Per-process detail popup showing RSS, swap, command line, CWD, exe, parent process
- Sort by Total, RSS, Swap, Name (T/P/S/N keys)
- Mouse support: scroll, click to select, double-click to open detail, right-click to expand
- btop-style segmented header bars with per-category colors
- Total column color gradient (teal → green → yellow → orange → red)
- Process kill via X key
- Parallel `footprint` invocations (batches of 20 × 4 concurrent)
- Dynamic page size detection (`hw.pagesize`) for Intel and Apple Silicon
- Swap accounting note showing unaccounted swap from tiny processes

### Fixed
- Corrected process selection in expanded groups (D key showed wrong PID)
- Fixed page size hardcoded to 4096 (4× error on Apple Silicon which uses 16384)
- Fixed system RAM calculation to exclude file-backed cache (now matches Activity Monitor within ~2%)
- Used "Pages occupied by compressor" instead of "Pages stored in compressor" (physical vs virtual)
- Removed swap normalization that inflated per-process swap by 3-6×
- Removed vmmap-based physical footprint that included shared GPU memory (showing 34 GB for Opera on 16 GB machine)

### Architecture
- Single source of truth: RSS from sysinfo, swap from footprint
- Detail popup reads from scanned data (no re-fetch for memory numbers)
- 3-step scan pipeline: system memory → process list → swap deep scan
- Removed vmmap, top.rs, swap normalization from critical path
