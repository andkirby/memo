# memo — macOS memory usage analyzer

A terminal tool that shows **what is eating your RAM and swap** — grouped by
**app** (100 Chrome processes → one row) or by **project** (every runtime a
project spins up → one row).

<img width="896" height="287" alt="image" src="https://github.com/user-attachments/assets/0ace5bf9-e729-43dd-afcd-16901e280df3" />

```
PROJECT                         PROCS  RUNTIMES                 RSS       SWAP      TOTAL
──────────────────────────────────────────────────────────────────────────────────────────
~/home/self-calendar            23     node:15 bun:3 pi:3 zsh:2  1.2G      1.8G      3.0G
~/home/markdown-ticket          31     bash:8 sleep:8 node:2 …  0.4G      0.6G      1.0G
Opera                           37     (app)                     3.4G      4.9G      8.3G
Telegram                        1      (app)                     191.0M    171.5M    362.5M
```

## Why

macOS Activity Monitor doesn't group processes. If Opera runs 37 processes
using 8 GB total, you see 37 separate entries with no way to understand the
total cost. `memo` fixes this.

For developer machines there's a second blind spot: one project fans out
across node, bun, python, your shell, and your editor's agent — each shows up
as a separate runtime, and one runtime is shared by many projects. "node: 2.8
GB" tells you nothing about *which* project owns it. `memo`'s **project mode**
recovers each process's project from its working directory (no per-service
instrumentation) and merges all runtimes of a project into a single row.

## Install

```bash
cargo build --release
cp target/release/memo ~/bin/memo   # or /usr/local/bin/memo
# On Apple Silicon, a freshly copied binary is killed on exec (com.apple.provenance).
# Clear the xattr and re-sign ad-hoc:
xattr -cr ~/bin/memo && codesign --force -s - ~/bin/memo
```

Requires macOS with Xcode command line tools (`footprint` and `lsof` must be available).

## Usage

```bash
# Interactive TUI — the main experience
memo

# CLI — grouped by app (default)
memo --cli

# CLI — grouped by project
memo --cli --group project

# CLI — all processes ungrouped
memo --ps

# Sort options (CLI and TUI)
memo --cli --sort rss     # by resident memory
memo --cli --sort swap    # by swap usage
memo --cli --sort total   # by total (default)
memo --cli --sort name    # alphabetically
```

## TUI Controls

| Key | Action |
|---|---|
| `↑↓` / `jk` | Navigate rows |
| `Tab` | Toggle App ↔ Project grouping (default: Project) |
| `Enter` | Expand group / open detail for sub-process |
| `D` | Open detail popup for selected process |
| `X` | Kill selected process (`kill -9`) |
| `R` | Refresh (full rescan) |
| `T` / `P` / `S` / `N` | Sort by Total / Physical / Swap / Name |
| `Q` / `Esc` | Quit (Esc also closes the detail popup) |
| Mouse scroll | Navigate |
| Mouse click | Select row |
| Double-click | Open detail (sub-process) or expand (group) |
| Right-click | Toggle expand |

## How it works

`memo` uses these OS signals, each for what it's best at:

| Data | Source | Why |
|---|---|---|
| **RSS** (resident memory) | `sysinfo` crate | Honest per-process number, matches Activity Monitor |
| **Swap** | macOS `footprint` tool | Only reliable per-process swap source on macOS |
| **cwd** (working dir) | `sysinfo` + one bulk `lsof -d cwd` | Resolves each process to its project (marker walkup) |
| **System RAM** | `vm_stat` + `sysctl` | Breakdown: App / Wired / Compressed / Cache |

It does **not** use `vmmap`'s "Physical Footprint" because that metric includes GPU backing stores (IOSurface) and shared `__TEXT` pages that are shared across processes. Summing those across grouped processes produces impossible numbers (e.g., 35 GB for Opera on a 16 GB machine).

### Scan pipeline

1. **System memory** — `vm_stat` + `sysctl` (instant)
2. **Process list** — `sysinfo` for all processes with RSS + cmdline + cwd (fast); one bulk `lsof -d cwd` call backfills cwd where sysinfo omits it
3. **Swap accounting** — `footprint` in parallel batches for processes with RSS ≥ 5 MB

Steps 1-2 take <1s. Step 3 takes ~8s for ~350 processes (parallel batches of 20 PIDs × 4 concurrent).

## Architecture

```
src/
├── main.rs        CLI parsing, TUI event loop, scan orchestration
├── scanner.rs     ProcessMemory, SystemMemory, vm_stat/sysctl parsing
├── footprint.rs   macOS footprint CLI wrapper (swap data only)
├── group.rs       Process grouping — App or Project mode (marker walkup on cwd)
├── app.rs         TUI state: navigation, selection, sort, row mapping
├── ui.rs          ratatui rendering: header bars, table, detail popup
└── top.rs         (unused — kept for potential future use)
```

### Data model

```rust
struct ProcessMemory {
    pid: i32,
    name: String,
    cmdline: String,
    cwd: String,   // from sysinfo (+ lsof fallback) — used for project grouping
    rss: u64,      // from sysinfo — pages in RAM right now
    swap: u64,     // from footprint — pages swapped to disk
    threads: usize,
}
// total() = rss + swap — that's what you'd free if you killed this process
```

### Process grouping

Two modes, cycled with `Tab` in the TUI or `--group app|project` on the CLI:

- **App mode** — group by canonical app name (Chrome, Electron, Slack, …). Dev
  runtimes (node/bun/deno/python) collapse to one row each; a bridge column
  shows the distinct projects under that runtime.
- **Project mode** (TUI default) — group by project directory, resolved from
each process's cwd by walking up to the nearest project marker (`.git`,
`package.json`, `pyproject.toml`, …). All runtimes of one project merge into
one row; a bridge column shows the runtime breakdown (`node:15 bun:3`).
Processes with no project (Slack, Chrome, OS daemons) fall back to app-name
grouping.

Rules (App mode):
- `Google Chrome Helper (Renderer)` → **Google Chrome**
- `Slack Helper (GPU)` → **Slack**
- Electron apps stripped of `Helper (Renderer/GPU/Service)` suffixes
- JetBrains IDEs detected from Java launcher + classpath
- VS Code, Cursor, Windsurf, Codex detected by helper names

## Accuracy vs Activity Monitor

| Metric | Activity Monitor | memo | Notes |
|---|---|---|---|
| Memory Used | 14.10 GB | ~14.0 GB | Within ~1% |
| App Memory | 3.28 GB | ~3.0 GB | sysinfo reports slightly less |
| Wired | 3.65 GB | ~3.6 GB | Near exact |
| Compressed | 6.43 GB | ~5.4 GB | We use "occupied by compressor" (physical), AM may include overhead |
| Cached Files | 1.83 GB | ~1.5 GB | File-backed pages (close) |
| Swap | 6.70 GB | matches | Same `sysctl vm.swapusage` source |

The per-process "RSS" column matches what `ps aux` and Activity Monitor show. The per-process "swap" comes from Apple's `footprint` tool which is the only reliable way to get per-process swap on macOS.

## Known limitations

- **macOS only** — depends on `footprint`, `vm_stat`, `sysctl`
- **Compressed memory gap** — Activity Monitor shows more compressed memory than our `vm_stat` reading. This is because AM uses private `host_statistics64()` API.
- **Swap unaccounted** — processes with RSS < 5 MB are skipped during deep scan. Their swap contribution is shown as "unaccounted" in CLI mode. This is a deliberate speed/accuracy tradeoff.
- **Shared memory not deducted** — if two processes share 100 MB of memory, each shows 100 MB RSS. The "total if killed" is approximate. This is a fundamental limitation of per-process accounting.

## License

MIT
