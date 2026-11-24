# Architecture

## Design principles

1. **One source of truth per metric** — RSS from sysinfo, compressed or swapped pages from footprint, and disk swap from sysctl.
2. **Detail popup reads scanned data** — no re-fetching memory numbers. The popup uses the same RSS and compressed or swapped bytes as the table row.
3. **Displayed total** — RSS plus measured compressed or swapped bytes, used for ranking. Groups can count shared RSS pages more than once.
4. **Grouping is a pure function of the process list** — `group_processes(procs, mode)` has no hidden state. Switching the view (App ↔ Project) re-pivots the same scanned data; it never re-fetches.
5. **Project identity is observed, not declared** — a process's project is recovered from its cwd (+ marker walkup), with no per-service instrumentation. See [ADR 0001](docs/adr/0001-group-by-project.md).

## Agent inspection

`memo inspect` is a one-shot, RSS-only path alongside the existing TUI and
legacy CLI. `main.rs` parses and dispatches commands and reuses `collect_processes`;
`inspect.rs` resolves scope, normalizes observed cwd paths, aggregates matching
processes, and renders text or versioned JSON. It never invokes `footprint`.

Automatic scope prefers the nearest enclosing `.git` directory or worktree
file, then the nearest other project marker from the shared `group.rs` marker
list. An explicit project path selects that exact directory. Canonical cwd
containment determines membership; no command-line or ancestry hints widen it.
The inspector's own PID is excluded. Row limits affect output, not totals.

This is distinct from the TUI's nearest-marker/hybrid grouping. The process
memory model and existing grouping rules are unchanged. Missing cwd limits
attribution, and compressed or swapped pages are unmeasured. The authoritative
command, output, and error contract is [Agent inspection](docs/agent-inspection.md).

## Data flow

```
┌─────────────┐
│  sysinfo    │──── all processes (pid, name, cmdline, RSS, cwd) ─┐
└─────────────┘                                                   │
                                                                  ▼
┌─────────────┐                                          ┌──────────────────┐
│  footprint  │──── cmp/swp for PIDs with RSS≥5MB ──────► │ ProcessMemory    │
└─────────────┘    (parallel batches of 20×4)            │ rss, cmp/swp, cwd │
┌─────────────┐    (one call, ≈0.14s)                    └──────┬───────────┘
│  lsof -d cwd│──── backfills cwd where sysinfo omits ──►       │
└─────────────┘                                                 │
┌──────────────┐                                                │
│  vm_stat +   │──── system RAM breakdown ──────────────────────┤
│  sysctl      │    (App, Wired, Compressed, Cache, Disk swap)   │
└──────────────┘                                                │
                                                                ▼
                                                  ┌────────────────────────┐
                                                  │ group_processes(mode)  │
                                                  │   App │ Project        │
                                                  └────────────────────────┘
```

## Why not vmmap?

`vmmap -summary` reports "Physical footprint" which includes:
- **IOAccelerator** (GPU backing stores) — shared across all renderer tabs
- **app-specific tag 16** (GPU compositing) — same
- **Clean __TEXT** pages — the executable binary mapped into every process

For a browser with 100 renderer processes sharing 100 MB of GPU textures, a group can sum the same 100 MB allocation 100 times.

sysinfo's RSS only counts pages resident in RAM per process, which matches Activity Monitor and is a much more honest number.

## Process counters and system disk swap

The `footprint` counter includes pages compressed in RAM as well as pages
swapped out. System disk swap measures a different quantity, so the two values
are reported independently. Deep scans cover processes with RSS of at least
5 MB when `footprint` can read them.

## Process grouping rules

Grouping has two modes (`GroupMode`), cycled in the TUI with `Tab`:

- **App** — group by canonical app name (the legacy view).
- **Project** — group by project directory; processes with no project fall
  back to app-name grouping (hybrid). Default TUI mode.

### App mode

`group.rs` normalizes process names:

| Process name | Group |
|---|---|
| `Google Chrome Helper (Renderer)` | Google Chrome |
| `Slack Helper (GPU)` | Slack |
| `Electron` + cmdline contains `cursor` | Cursor |
| `node` / `bun` / `deno` | node / bun / deno (one row per runtime) |
| `python3` | Python |
| `java` + cmdline contains `idea` | IntelliJ IDEA |

Dev runtimes collapse to a single row each; the **bridge column** then shows
the distinct projects under that runtime, so the per-project split is not lost.

### Project mode

A process's project is resolved from its cwd by walking up to the nearest
project marker (`.git`, `package.json`, `pyproject.toml`, `bun.lock`,
`Cargo.toml`, `go.mod`, …). Paths under editor/CLI internals (`~/Library`,
`~/.cache`, `~/.cargo`, …) are excluded. Where sysinfo omits cwd, one bulk
`lsof -d cwd` call backfills it.

| Process cwd | Group |
|---|---|
| `~/home/self-calendar` (has `.git`) | `~/home/self-calendar` |
| `~/home/self-calendar/vendor/pkg` | `~/home/self-calendar` (nearest marker) |
| `~/Library/…/Zed/…` | (excluded) → app-name fallback |
| `/` or no project marker | app-name fallback (e.g. Slack) |

All runtimes of one project merge into one row; the bridge column shows the
runtime breakdown (`node:15 bun:3 pi:3`).

Grouping is a pure function of `(process_list, mode)` — no PID tracking, no
state. Regrouping never re-fetches.

## TUI row model

The table is a flat list of visual rows. Some rows are group headers, some are sub-processes (when a group is expanded). A `RowMap` maps visual row index → (group_index, Option<proc_index>).

Navigation with j/k moves through visual rows. The row map syncs `expanded_group` and `proc_state` so that actions (detail, kill, sort) always target the correct process.

### Group mode and the bridge column

`App.group_mode: GroupMode { App, Project }` selects the grouping key. `Tab`
toggles it and regroups from the already-scanned `all_processes` (no re-fetch).

Columns are `NAME | PROCS | BRIDGE | RSS | CMP/SWP | TOTAL`. The bridge column
carries the *other* view's key — runtime breakdown in Project mode, distinct
project count in App mode — so switching views loses no information. It
replaces the old `THREADS` column (sysinfo 0.33 doesn't expose thread counts,
so it was always 1).

## macOS system memory calculation

```
page_size     = sysctl hw.pagesize  (4096 on Intel, 16384 on Apple Silicon)
free          = vm_stat "Pages free" × page_size
active        = vm_stat "Pages active" × page_size
wired         = vm_stat "Pages wired down" × page_size
compressed    = vm_stat "Pages occupied by compressor" × page_size  (physical, not virtual)
speculative   = vm_stat "Pages speculative" × page_size
purgeable     = vm_stat "Pages purgeable" × page_size
file_backed   = vm_stat "File-backed pages" × page_size

app_memory    = active + speculative - purgeable
cache         = file_backed
used          = total - free - cache - purgeable
```

This approximates Activity Monitor's "Memory Used" within ~2%. The remaining gap is from private Apple APIs (`host_statistics64`) that we can't call from Rust without libc bindings.
