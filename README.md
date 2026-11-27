# memo — macOS memory usage analyzer

A terminal tool that shows **where process memory goes** — grouped by
**app** (100 Chrome processes → one row) or by **project** (every runtime a
project spins up → one row).

<img width="896" height="287" alt="image" src="https://github.com/user-attachments/assets/0ace5bf9-e729-43dd-afcd-16901e280df3" />

```
PROJECT                        PROCS   BREAKDOWN                RSS SUM      COMPRESSED OR SWAPPED FOOTPRINT
────────────────────────────────────────────────────────────────────────────────────────────────────────────────
Opera                          59                               5.9G         13.6G                 14.5G
~/home/self-calendar           23      node:15 bun:3 pi:3 zsh:2 1.2G         1.8G                  2.6G
~/home/markdown-ticket         31      bash:8 sleep:8 node:2 …  412.0M       610.3M                842.9M
Telegram                       1                                191.0M       171.5M                362.5M
```

FOOTPRINT is Apple's `footprint` for the row's processes as one set: memory
they share counts once. COMPRESSED OR SWAPPED is the part of it that is
compressed in RAM or swapped to disk. RSS SUM adds each process's resident
memory, so shared pages count once per process.

## Why

macOS Activity Monitor doesn't group processes. If Opera runs 37 processes,
you see 37 separate entries. `memo` combines them into one row.

For developer machines there's a second blind spot: one project fans out
across node, bun, python, your shell, and your editor's agent — each shows up
as a separate runtime, and one runtime is shared by many projects. "node: 2.8
GB" tells you nothing about *which* project owns it. `memo`'s **project mode**
recovers each process's project from its working directory (no per-service
instrumentation) and merges all runtimes of a project into a single row.

## Install

**Option A — prebuilt binary (recommended)**

Download from [releases](https://github.com/andkirby/memo/releases), or one-liner:

```bash
# Pick the slice for your Mac, then drop it on your PATH.
arch=$(uname -m)
case "$arch" in
  arm64)  asset=memo-macos-arm64  ;;   # Apple Silicon (M1/M2/M3…)
  x86_64) asset=memo-macos-x86_64 ;;   # Intel
  *) echo "unsupported arch: $arch" >&2; exit 1 ;;
esac
curl -L "https://github.com/andkirby/memo/releases/latest/download/$asset" \
  -o /usr/local/bin/memo && chmod +x /usr/local/bin/memo
```

**Option B — build from source**

```bash
cargo build --release
cp target/release/memo /usr/local/bin/memo   # or ~/bin/memo
```

**Required on macOS either way** — a freshly downloaded/built binary is killed
on exec (`com.apple.provenance`). Clear the xattr and re-sign ad-hoc:

```bash
xattr -cr /usr/local/bin/memo && codesign --force -s - /usr/local/bin/memo
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

# CLI — RSS-only scan with no footprint subprocess
memo --cli --quick
memo --cli --quick --group project --sort name

# Full-scan sort options (also apply to app and project groups)
memo --cli --sort rss     # by resident memory (RSS SUM for groups)
memo --cli --sort swap    # by compressed or swapped memory
memo --cli --sort total   # by footprint (default; `footprint` also works)
memo --cli --sort name    # alphabetically

# Agent queries — current project, RSS only
memo inspect
memo inspect --json
memo inspect --project /path/to/project --limit 20

# Executable version
memo --version           # also: memo -v
```

`--quick` defaults to RSS descending and also accepts `--sort name`.
The process table omits the footprint columns. The system memory header
still reports disk swap. Combine `--cli --quick` with `--ps`
for an ungrouped RSS-only list.

See [Agent inspection](docs/agent-inspection.md) for scope rules, JSON fields,
and error handling. Automatic scope prefers the enclosing Git worktree root;
project processes are matched by their observed working directories.

## TUI Controls

| Key | Action |
|---|---|
| `↑↓` / `jk` | Navigate rows |
| `Tab` | Toggle App ↔ Project grouping (default: Project) |
| `Enter` | Expand group / open detail for sub-process |
| `D` | Open detail popup for selected process |
| `x` | Expand selected group or open a confirmation for the selected process |
| `y` in confirmation | Send SIGKILL to the displayed process |
| `n` or `Esc` in confirmation | Cancel |
| `R` | Refresh (full rescan, once the current deep scan has finished) |
| `T` / `P` / `S` / `N` | Sort by Footprint / RSS / Compressed or swapped / Name |
| `q` | Quit from the table or close a dialog |
| `Esc` in details | Close details |
| Mouse scroll | Navigate |
| Mouse click | Select row |
| Double-click | Open detail (sub-process) or expand (group) |
| Right-click | Toggle expand |

The confirmation shows the process name and PID. macOS verifies the process
identity as part of sending SIGKILL. The status panel reports stale targets
and signaling errors. Termination requires the macOS audit-token signal API.
On older versions without it, Memo reports that termination is unavailable.

## How it works

`memo` uses these OS signals, each for what it's best at:

| Data | Source | Why |
|---|---|---|
| **RSS** (resident memory) | `sysinfo` crate | Resident bytes per process |
| **Footprint** and **compressed or swapped** (`CMP/SWP` in the TUI) | macOS `footprint` tool | Dirty memory of a PID set, with memory shared inside the set counted once |
| **cwd** (working dir) | `sysinfo` + one bulk `lsof -d cwd` | Resolves each process to its project (marker walkup) |
| **System RAM** | `vm_stat` + `sysctl` | Breakdown: App / Wired / Compressed / Cache |
| **Disk swap** | `sysctl vm.swapusage` | System-wide disk swap usage |

### Footprint per row

One `footprint` call measures one set of PIDs. Within that set Apple counts
memory shared by several processes once (see `man footprint`). Memo reads the
set's `total footprint` and its compressed or swapped part
(`summary.total.swapped`). Apple's JSON names that key `swapped`, but it
covers pages in the compressor as well as pages on disk.

- **Process rows** (`--ps`, expanded TUI rows, the detail popup) use one call
  for that PID alone. Memory the process shares with others is included in
  full.
- **Group rows** with several processes use one call for exactly the group's
  PIDs, including processes `footprint` could not read alone. Shared memory
  counts once, so a group can be smaller than the sum of its process rows.
- **One-process groups** reuse the process call.

Adding per-process values would count shared memory once per process. On a
16 GB Mac, Opera's 56 processes added up to 35.1 GiB this way, while one
call over the same PIDs returned 12.9 GiB. Memo never adds footprint values
across processes, and the result does not depend on how PIDs are batched.

Memory a group shares with processes outside the group, such as graphics
buffers shared with WindowServer, still counts in the group.

A group row's coverage comes from its own call. `footprint` leaves PIDs it
cannot read (root processes, exited processes) out of its output and still
exits successfully, so Memo compares the PIDs in the output with the row's
members. All of them: the value is complete. Some: the cell shows `≥`, the
footprint of the readable subset, a lower bound. None: `n/a`. A process that
fails alone but is read in the group call does not make the row partial.
`0B` is a real measurement. In the TUI, `…` marks a value still being
measured. The CLI summary also counts complete, `≥` and `n/a` rows;
"footprint read" counts processes measured alone.

### Scan pipeline

1. **System memory** — `vm_stat` + `sysctl` (instant)
2. **Process list** — `sysinfo` for all processes with RSS + cmdline + cwd (fast); one bulk `lsof -d cwd` call backfills cwd where sysinfo omits it
3. **Each process alone** — one `footprint --pid` call per process, one call per CPU core at a time
4. **Each multi-process group** — one `footprint` call over the group's PIDs, two at a time. The CLI measures the requested grouping. The TUI measures the visible grouping first, then the other one so `Tab` needs no rescan.

Steps 1-2 take under 1 s. On a 10-core Mac with about 580 processes, a full
CLI scan took 16-18 s; the 60-process Opera group call took most of it.
`--ps` skips step 4, and `--quick` skips steps 3 and 4.

PIDs are passed with `--pid`. `footprint` also reads a bare number, or `-p`,
as a partial process name, which can select unrelated processes. It exits
successfully when it cannot read some requested PIDs, so Memo compares the
PIDs in the output with the request.

## Architecture

Agent interface: [current contract](docs/agent-inspection.md) and
[original idea and deferred extensions](docs/ideas/IDEA-001-agent-project-inspection.md).

```
src/
├── main.rs        CLI parsing, TUI event loop, scan orchestration
├── scanner.rs     ProcessMemory, SystemMemory, vm_stat/sysctl parsing
├── footprint.rs   macOS footprint wrapper: one call per PID set
├── deep.rs        Deep measurement states, scheduling, and cell text
├── group.rs       Process grouping — App or Project mode (marker walkup on cwd)
├── inspect.rs     Project-scoped RSS query: scope, aggregation, text/JSON output
├── app.rs         TUI state: navigation, selection, sort, row mapping
├── termination.rs macOS process identity capture and signaling
├── ui.rs          ratatui rendering: header bars, table, detail popup
└── top.rs         (unused — kept for potential future use)
```

### Data model

```rust
struct ProcessMemory {
    pid: i32,
    name: String,
    termination_identity: Option<ProcessIdentity>, // recorded for the TUI
    cmdline: String,
    cwd: String,   // from sysinfo (+ lsof fallback) — used for project grouping
    rss: u64,      // from sysinfo — pages in RAM right now
    deep: ProcessDeep, // Pending | Unavailable | Measured { footprint, compressed_or_swapped }
    threads: usize,
}

struct AppGroup {
    processes: Vec<ProcessMemory>,
    total_rss: u64, // sum of member RSS
    deep: GroupDeep, // Pending | Unavailable | Measured | Partial (lower bound)
    // ...
}
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
| Disk swap | 6.70 GB | matches | Same `sysctl vm.swapusage` source |

The per-process RSS column uses resident memory, as in `ps aux`. A process's
footprint matched `top`'s MEM column in our checks. Activity Monitor shows
one value per process and no group total, so adding its Memory column counts
shared memory more than once.

Footprint is dirty memory: it leaves out clean pages, such as program code
read from disk, that macOS can drop and reload. Compressed or swapped memory
is counted at its uncompressed size, so it can be larger than the RAM it
occupies, and it can be nonzero when system disk swap is zero. `footprint`
does not report how much of a process's or group's memory is in the
compressor and how much is on disk, nor how much physical RAM the compressed
part takes. Memory freed when a group exits depends on what remains shared
with other processes.

## Known limitations

- **macOS only** — no Linux/Windows support, and not planned. Depends on `footprint`, `vm_stat`, `sysctl`, and `lsof`.
- **Build targets: `aarch64-apple-darwin` (Apple Silicon) and `x86_64-apple-darwin` (Intel).** No universal binary — grab the slice matching your CPU.
- **Tested on macOS Sonoma (14) only.** Expected to run on macOS 11 Big Sur and later (the arm64 slice requires Apple Silicon, i.e. 11+ anyway). The Intel slice is **cross-compiled in CI and not run-tested on Intel hardware** — it should work, but treat it as best-effort.
- **Intel is end-of-life.** macOS 26 Tahoe (Sept 2025) is the last release supporting Intel Macs; its successor (Golden Gate, fall 2026) is Apple-Silicon-only. The `memo-macos-x86_64` build targets that shrinking tail.
- **Compressed memory gap** — Activity Monitor shows more compressed memory than our `vm_stat` reading. This is because AM uses private `host_statistics64()` API.
- **Unreadable processes** — without root, `footprint` cannot read some processes (mostly root daemons). They show `n/a`, and groups whose call cannot read them show a `≥` lower bound. Every observed process stays in its row and in its group's call, including processes with no RSS reading, so `--ps` lists them as `0B` RSS and `n/a`. `--quick` still lists only processes with RSS.
- **Scan time** — group calls on large multi-process apps take seconds each (about 12 s for 60 Opera processes on a 10-core Mac).
- **Point in time** — each `footprint` call is a separate sample. Values in different rows, and a group and its process rows, come from slightly different moments.

## License

MIT
