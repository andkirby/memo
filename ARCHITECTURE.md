# Architecture

## Design principles

1. **One source of truth per metric** — RSS from sysinfo, swap from footprint. Never mix.
2. **Detail popup reads scanned data** — no re-fetching memory numbers. The popup's RSS/swap are identical to the table row.
3. **Honest numbers** — we show raw estimates, not normalized/fitted totals. If per-process swap sums to less than system swap, we say so ("unaccounted").

## Data flow

```
┌─────────────┐
│  sysinfo     │──── all processes (pid, name, cmdline, RSS) ────┐
└─────────────┘                                                │
                                                               ▼
┌─────────────┐                                         ┌─────────────┐
│  footprint  │──── swap data for PIDs with RSS≥5MB ──► │ ProcessMemory│
└─────────────┘    (parallel batches of 20×4)           │  {rss, swap} │
                                                         └──────┬──────┘
┌─────────────┐                                                │
│  vm_stat +   │──── system RAM breakdown ──────────────────────┤
│  sysctl      │    (App, Wired, Compressed, Cache, Swap)       │
└─────────────┘                                                │
                                                               ▼
                                                        ┌─────────────┐
                                                        │  AppGroup    │
                                                        │  (grouped)   │
                                                        └─────────────┘
```

## Why not vmmap?

`vmmap -summary` reports "Physical footprint" which includes:
- **IOAccelerator** (GPU backing stores) — shared across all renderer tabs
- **app-specific tag 16** (GPU compositing) — same
- **Clean __TEXT** pages — the executable binary mapped into every process

For a browser with 100 renderer processes sharing 100 MB of GPU textures, vmmap reports 100 × 100 MB = 10 GB. The real additional cost of killing those processes is near zero (the GPU textures are freed once, not 100 times).

sysinfo's RSS only counts pages resident in RAM per process, which matches Activity Monitor and is a much more honest number.

## Why not normalize swap?

The old code redistributed "missing" swap (system total − sum of per-process) proportionally across scanned processes. This inflated a 1.8 GB swap process to 7 GB because hundreds of tiny swapped processes weren't scanned. The per-process numbers became lies.

Instead, we show raw per-process swap estimates and note the unaccounted delta.

## Process grouping rules

The grouping logic (`group.rs`) normalizes process names:

| Process name | Group |
|---|---|
| `Google Chrome Helper (Renderer)` | Google Chrome |
| `Slack Helper (GPU)` | Slack |
| `Electron` + cmdline contains `cursor` | Cursor |
| `python3` + cmdline contains `uvicorn` | Python (uvicorn) |
| `node` + cmdline path `/some/project` | node (project) |
| `java` + cmdline contains `idea` | IntelliJ IDEA |

Grouping is by canonical name only — no PID tracking, no state. This means regrouping is always a pure function of the process list.

## TUI row model

The table is a flat list of visual rows. Some rows are group headers, some are sub-processes (when a group is expanded). A `RowMap` maps visual row index → (group_index, Option<proc_index>).

Navigation with j/k moves through visual rows. The row map syncs `expanded_group` and `proc_state` so that actions (detail, kill, sort) always target the correct process.

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
