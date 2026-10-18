# Agent inspection

`memo inspect` returns a fresh, project-scoped process/RSS snapshot without
opening the TUI or running swap analysis. It is intended for agents that need a
quick answer to “how much memory is this project using, and which processes
account for it?”

## Commands

```bash
memo inspect
memo inspect --json
memo inspect --project /path/to/project
memo inspect --limit 20
memo --version
memo -v
```

Text is the default. `--json` returns one JSON object on stdout, with integer
byte values and no progress messages or ANSI formatting. `--limit` must be a
positive integer; it defaults to 5 and affects displayed processes only.

Existing `memo`, `memo --cli`, and `memo --ps` entry points remain available.
Do not combine legacy CLI flags with `inspect`. This first release has no
`--deep`, `--all`, watch mode, or process termination action.

## Scope

Without `--project`, Memo canonicalizes the caller's cwd and selects the nearest
enclosing `.git` directory or worktree `.git` file. If there is no Git marker,
it selects the nearest project marker from Memo's existing marker list.
Nested package markers do not override an enclosing Git worktree root.

`--project PATH` selects that exact canonical directory. Relative paths use the
caller's cwd. The directory needs no project marker and may be outside home.
When automatic discovery finds no project, Memo suggests `--project .` instead
of silently inspecting the whole machine.

A process is included when its observed canonical cwd is the scope directory
or a descendant. Nested packages are included. Sibling names such as
`/repo-other` do not match `/repo`. Symlink aliases resolve to the same path.
The inspector's own PID is excluded; other agents and shells in the folder
count. Arguments and process ancestry do not establish membership.

Memo may enumerate OS-wide process metadata to find matches. Scope limits the
returned process data; it is not an OS access-control boundary.

## Result

A successful report includes scope and its source, UTC observation time,
inspection duration, total matching process count, aggregate RSS, and the
largest processes. Each process includes PID, name, RSS, and a compact command
label. Rows sort by RSS descending, then PID ascending. Totals include omitted
rows; `--limit` never changes aggregate membership.

JSON schema version 1 uses these fields:

| Field | Meaning |
|---|---|
| `version` | Inspection schema version, independent of the executable version |
| `scope` | Absolute canonical directory |
| `source` | `git`, `marker`, or `explicit` |
| `marker` | Marker name; null for an explicit scope |
| `observed_at` | UTC timestamp at the start of inspection |
| `duration_ms` | Collection, normalization, and aggregation time, excluding rendering |
| `total_processes` | Count of all matched processes |
| `total_rss` | Sum of matched RSS, in bytes |
| `top` | Up to N rows with `pid`, `name`, `rss` (bytes), and `command` |
| `omitted` | Matched processes not returned in `top` |
| `limitation` | Explanation of cwd attribution limits |

Treat this as an observation over an interval, not an atomic OS snapshot. RSS
may count shared pages more than once. It does not predict memory reclaimed by
stopping a process. Swap is unmeasured and omitted, not reported as zero.

Missing or unresolvable cwd prevents attribution. Processes serving a project
from `/tmp`, containers, or a shared external daemon can be missed. An empty
result means no matching processes were observed, not that no project service
is running. Collection uses the existing scanner and does not guarantee full
OS visibility.

## Errors and exits

- `0`: usable report, including an empty project result.
- `1`: scan failure without a usable result.
- `2`: invalid arguments or project scope.

With `--json`, errors use the same schema version plus an `error` object with
`code` and `message`. Error codes are `bad_argument`, `project_not_found`,
`invalid_project`, and `scan_failed`. Help and version requests are informational
output, not JSON reports.

## Verification snapshot — 2026-10-18

- `cargo test`: 43 unit tests and 2 CLI integration tests passed. Inspection
  coverage includes Git-first scope, worktree marker files, marker fallback,
  cwd normalization/symlinks, sibling boundaries, self exclusion, sorting,
  row limits with complete totals, empty results, and JSON argument errors.
- `cargo build --release`: succeeded. Release-binary smoke checks used real
  processes in temporary nested/sibling/symlink folders and checked 10 error
  cases. Both version flags and plain TUI startup/quit also passed.
- `cargo clippy --message-format short`: completed with 12 existing warnings
  in legacy code; no warnings in the new inspection module.
- Ten sequential release queries on this Apple Silicon Mac (macOS 15.7.9)
  measured median wall time **234.3 ms** and nearest-rank p95 **259.4 ms**.
  This small local sample is not a latency guarantee for other machines.

Repeat the durable checks with `cargo test` and `cargo build --release`.
The CLI integration tests live in [tests/inspect_cli.rs](../tests/inspect_cli.rs).

## Ownership

[src/inspect.rs](../src/inspect.rs) owns inspection scope, result construction,
and rendering. [src/main.rs](../src/main.rs) owns argument parsing and dispatch;
existing collection and metric sources remain shared. The TUI's hybrid grouping
is separate from inspection membership.

The original brainstorm is retained in
[IDEA-001](ideas/IDEA-001-agent-project-inspection.md). Swap scans, machine-wide
inspection, MCP, and snapshot comparisons remain future proposals.
