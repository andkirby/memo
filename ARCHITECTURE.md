# Architecture

## Design principles

1. **One source of truth per metric** — RSS from sysinfo; footprint and its compressed or swapped part from Apple's `footprint`; disk swap from sysctl.
2. **A row's footprint is one measurement of exactly that row's PIDs** — process rows measure one PID; multi-process group rows measure the group's PID set, where Apple counts shared memory once. Footprint values are never added across processes, and never depend on PID batching.
3. **Missing is not zero** — deep values are `Pending`, `Unavailable`, `Measured`, or (groups) `Partial`, a lower bound over the readable members. Sorting puts rows without a value last.
4. **Detail popup reads scanned data** — no re-fetching memory numbers. The popup shows the same process measurement as the expanded row.
5. **Grouping is a pure function of the process list** — `group_processes(procs, mode)` has no hidden state. Switching the view (App ↔ Project) re-pivots the same scanned data and looks up group measurements by PID set; it never re-fetches.
6. **Project identity is observed, not declared** — a process's project is recovered from its cwd (+ marker walkup), with no per-service instrumentation. See [ADR 0001](docs/adr/0001-group-by-project.md).

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

`memo --cli --quick` uses the same app or project grouping as the full CLI
scan and returns RSS without invoking `footprint`. It accepts RSS and name
sorting. The system memory header is shared with the full CLI.

## Data flow

```
┌─────────────┐
│  sysinfo    │──── all processes (pid, name, cmdline, RSS, cwd) ─┐
└─────────────┘                                                   │
┌─────────────┐                                                   │
│  lsof -d cwd│──── backfills cwd where sysinfo omits ──►         │
└─────────────┘    (one call, ≈0.14s)                             ▼
┌─────────────┐                                          ┌──────────────────┐
│  footprint  │──── one call per PID ───────────────────► │ ProcessMemory    │
│  --pid N    │    (one call per core at a time)          │ rss, deep, cwd   │
└─────────────┘                                          └──────┬───────────┘
                                                                ▼
                                                  ┌────────────────────────┐
                                                  │ group_processes(mode)  │
                                                  │   App │ Project        │
                                                  └──────┬─────────────────┘
┌─────────────┐                                          │ multi-process PID sets
│  footprint  │◄──── one call per group PID set ─────────┘ (deduplicated across modes)
│  --pid A..Z │──── GroupDeep by PID set ──► AppGroup.deep
└─────────────┘    (two calls at a time)
┌──────────────┐
│  vm_stat +   │──── system RAM breakdown (App, Wired, Compressed, Cache, Disk swap)
│  sysctl      │
└──────────────┘
```

`footprint.rs` runs one invocation for one PID set and returns the set's
`total footprint`, `summary.total.swapped`, and the PIDs present in the
output. `deep.rs` owns the measurement states, the worker pools, and group
resolution:

- `measure_processes` — every PID alone. A call whose output is not exactly
  that PID is `Unavailable`.
- `measure_group` — one call over every member, whatever the per-process
  calls returned. `footprint` exits 0 and omits PIDs it cannot read, so the
  PIDs in this call's output decide coverage: all members `Measured`, some
  `Partial` with the measured count, none or a failed call `Unavailable`.
- `AppGroup::resolve_deep` — one-process groups take their process value;
  multi-process groups look up their PID set, staying `Pending` until the
  scan settles and `Unavailable` afterwards.

The CLI (`measure_cli`) measures processes, then the multi-process groups of
the requested mode from that same list. It drops no observed PID, so a row's
membership, its group call and its coverage agree with the TUI. `--quick`
keeps its RSS-only list. The TUI background scan streams process results in batches, then group
results for the visible mode and the other mode. Each event carries a scan id
so results from an older scan are ignored.

## Why one call per set

Apple's tool deduplicates within the PIDs of one invocation (`man footprint`).
Its JSON lists per-process `categories` with memory private to that process
inside the set, a `shared` list, a per-process `footprint` ledger, and the
set's `summary` and `total footprint`. Only the set totals count shared memory
once:

- Adding per-process ledgers counts shared memory once per process. For 56
  Opera processes this gave 35.1 GiB; the set total was 12.9 GiB.
- Adding per-process categories leaves out memory shared inside the set, and
  the result depends on which other PIDs were in the same call. Memo 0.4.0
  did this over machine-wide batches of 20 PIDs.
- The `shared` entries are not additive: added to the categories, they come
  close to the ledger total again.

A controlled check mapped one 32 MiB dirty region into two processes. Summed
RSS rose by 64 MiB and the two-PID footprint by 32 MiB. The real-footprint
unit test in `footprint.rs` repeats this with a 64 MiB region.

Footprint values are dirty memory; clean file-backed pages such as shared
`__TEXT` are not included. Memory a group shares with processes outside the
group (for example IOSurface buffers shared with WindowServer) still counts in
the group.

## Process counters and system disk swap

The `footprint` compressed or swapped value includes pages compressed in RAM
as well as pages swapped out, at their uncompressed size. System disk swap
measures a different quantity, so the two values are reported independently.
`footprint` gives no per-process or per-group split between the compressor
and disk, and no physical size of the compressed pages.

The CLI and TUI summaries report RSS as a sum and give footprint coverage as
counts (read / unavailable). They do not add footprint values across rows:
memory shared between rows would count more than once.

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

Navigation with j/k moves through visual rows. The row map syncs
`expanded_group` and `proc_state` with the highlighted row.

Pressing `x` on a group expands it. On a child row, the confirmation stores
the displayed name and process identity. The TUI scan binds a row only when
its Mach audit token matches before and after collecting the process data.
Background regrouping and sorting leave the pending target intact.

Memo resolves `proc_signal_with_audittoken` at runtime. The kernel validates
the token and holds the target process reference through signal delivery.
Memo reports an error when that operation is unavailable.
[Apple XNU signal implementation](https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.121.6/bsd/kern/proc_info.c#L3564).

### Group mode and the bridge column

`App.group_mode: GroupMode { App, Project }` selects the grouping key. `Tab`
toggles it and regroups from the already-scanned `all_processes` (no re-fetch).

Columns are `NAME | PROCS | BRIDGE | ΣRSS | CMP/SWP | FOOTPRINT`. The bridge column
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
