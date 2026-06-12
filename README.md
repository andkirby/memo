# memo — macOS memory usage analyzer

A terminal tool that shows **which apps consume your RAM and swap**, grouping multi-process applications into a single entry. 100 Chrome processes? One row.

<img width="896" height="287" alt="image" src="https://github.com/user-attachments/assets/0ace5bf9-e729-43dd-afcd-16901e280df3" />

```
APP                            PROCS   RSS          SWAP         TOTAL
────────────────────────────────────────────────────────────────────────
Opera                          37      3.4G         4.9G         8.3G
node                           16      1.2G         2.8G         4.0G
Python (uvicorn)               2       24.0M        1.2G         1.2G
Telegram                       1       191.0M       171.5M       362.5M
```

## Why

macOS Activity Monitor doesn't group processes. If Opera runs 37 processes using 8 GB total, you see 37 separate entries with no way to understand the total cost. `memo` fixes this.

## Install

```bash
cargo build --release
cp target/release/memo ~/bin/memo   # or /usr/local/bin/memo
```

Requires macOS with Xcode command line tools (`footprint` must be available).

## Usage

```bash
# Interactive TUI — the main experience
memo

# CLI — grouped by app
memo --cli

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
| `Enter` | Expand group / open detail for sub-process |
| `D` | Open detail popup for selected process |
| `X` | Kill selected process (`kill -9`) |
| `R` | Refresh (full rescan) |
| `T` / `P` / `S` / `N` | Sort by Total / Physical / Swap / Name |
| `Tab` | Toggle grouped / ungrouped view |
| `Q` | Quit |
| Mouse scroll | Navigate |
| Mouse click | Select row |
| Double-click | Open detail (sub-process) or expand (group) |
| Right-click | Toggle expand |

## How it works

`memo` uses two data sources, each for what it's best at:

| Data | Source | Why |
|---|---|---|
| **RSS** (resident memory) | `sysinfo` crate | Honest per-process number, matches Activity Monitor |
| **Swap** | macOS `footprint` tool | Only reliable per-process swap source on macOS |
| **System RAM** | `vm_stat` + `sysctl` | Breakdown: App / Wired / Compressed / Cache |

It does **not** use `vmmap`'s "Physical Footprint" because that metric includes GPU backing stores (IOSurface) and shared `__TEXT` pages that are shared across processes. Summing those across grouped processes produces impossible numbers (e.g., 35 GB for Opera on a 16 GB machine).

### Scan pipeline

1. **System memory** — `vm_stat` + `sysctl` (instant)
2. **Process list** — `sysinfo` for all processes with RSS + cmdline (fast)
3. **Swap accounting** — `footprint` in parallel batches for processes with RSS ≥ 5 MB

Steps 1-2 take <1s. Step 3 takes ~8s for ~350 processes (parallel batches of 20 PIDs × 4 concurrent).

## Architecture

```
src/
├── main.rs        CLI parsing, TUI event loop, scan orchestration
├── scanner.rs     ProcessMemory, SystemMemory, vm_stat/sysctl parsing
├── footprint.rs   macOS footprint CLI wrapper (swap data only)
├── group.rs       Process grouping by app name (Chrome, Electron, etc.)
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
    rss: u64,      // from sysinfo — pages in RAM right now
    swap: u64,     // from footprint — pages swapped to disk
    threads: usize,
}
// total() = rss + swap — that's what you'd free if you killed this process
```

### Process grouping

Processes are grouped by canonical app name. Rules:
- `Google Chrome Helper (Renderer)` → **Google Chrome**
- `Slack Helper (GPU)` → **Slack**
- Electron apps stripped of `Helper (Renderer/GPU/Service)` suffixes
- `node` / `python` identified by project from command line
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
