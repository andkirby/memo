# ADR 0001: Group processes by project (cwd marker walkup)

- **Status:** Accepted — 2026-06-14
- **Touches:** `group.rs`, `scanner.rs`, `app.rs`, `ui.rs`, `main.rs`, README, ARCHITECTURE

## Context

memo groups multi-process applications (Chrome, Electron) into single rows by
canonical app name. For GUI apps this works. For developer runtimes — node, bun,
python, deno — it breaks down:

- **One project fans out across several runtimes.** vite runs as `node`; the dev
  shell is `zsh`; an agent is `pi`. Each becomes a separate row.
- **One runtime is shared by many projects.** "node: 2.8 GB" says nothing about
  *which* project owns it.
- **The current mitigation** (`extract_project` hint → `node (self-calendar)`)
  is lossy: it only fires when a path appears in argv, misses `bun run dev` with
  no path, and never attributes shells or agents to their project.

Result: the user cannot see how much memory a *project* eats — memo's central
unanswered question.

Separately, the TUI's `Tab` toggle between `Overview` and `Ps` only changed the
table title; both modes rendered the same app-grouped data. The toggle was a
latent no-op.

## Decision

Add a second grouping axis — **project** — resolved from the OS, with no
per-service instrumentation.

1. **cwd is the project signal.** `sysinfo`'s `Process::cwd()` (macOS:
   `proc_pidinfo`, Linux: `/proc/<pid>/cwd`) gives every process's working
   directory. Where sysinfo omits it, one bulk `lsof -d cwd` call (≈0.14 s for
   ~350 procs) backfills it. No health endpoints, no metrics libs, no edits to
   any service.
2. **Marker walkup normalizes the key.** From cwd, walk up to the nearest
   project marker (`.git`, `package.json`, `pyproject.toml`, `bun.lock`,
   `Cargo.toml`, `go.mod`, …). This collapses nested sub-projects
   (`proj/vendor/pkg` → `proj`) and normalizes `node_modules/.bin` wrappers for
   free. Paths under editor/CLI internals (`~/Library`, `~/.cache`, `~/.cargo`,
   …) are excluded.
3. **Project mode is a hybrid.** Processes that resolve to a project dir merge
   by project path (all runtimes of one project → one row). Processes that
   don't (Slack, Chrome, OS daemons) fall back to canonical-app grouping.
   Pure-project mode would hide every GUI app; pure-app mode hides the project
   answer. The hybrid keeps both.
4. **A bridge column preserves the other view's key.** In Project mode the
   bridge shows the runtime breakdown (`node:15 bun:3 pi:3`); in App mode it
   shows the distinct projects under that app. Switching views therefore
   re-pivots the same data — no information lost in either direction. The
   always-1 `THREADS` column is replaced by the bridge (sysinfo 0.33 doesn't
   expose threads, so it carried no real signal).
5. **`ViewMode{Overview,Ps}` is replaced by `GroupMode{App,Project}` on `App`.**
   Grouping stays a pure function of the process list
   (`group_processes(procs, mode)`); only the key differs. TUI `Tab` toggles
   App ↔ Project (default **Project**). The `--ps` CLI flag keeps its true
   ungrouped flat output; the broken TUI Ps toggle is removed.

## Alternatives considered

- **Argv-hint only** (status quo `extract_project`). Rejected — lossy; misses
  pathless invocations and non-runtime processes (shells, agents).
- **Per-service instrumentation** (psutil, prom-client, health endpoints).
  Rejected — violates memo's contract as pure OS observation; requires editing
  every service.
- **Unify App/Project/Ungrouped onto one three-state `Tab` cycle.** Deferred —
  a real TUI Ungrouped needs a dedicated flat (PID/NAME/RSS/SWAP) renderer;
  `--ps` already serves that need in the CLI. Tracked as future work.

## Consequences

- `ProcessMemory` gains a `cwd: String` field, populated in `collect_processes`
  (sysinfo, with `lsof` bulk fallback).
- `group_processes` takes a `GroupMode`; `AppGroup` gains a `GroupKind { App,
  Project }` and a `bridge(mode)` method.
- `Tab`'s meaning changes (was: no-op Overview/Ps relabel; now: real App ↔
  Project pivot). Documented in README + controls.
- Nested projects collapse to nearest marker by default. A future `--split`
  could expose sub-projects; not built.
- `extract_project` / `extract_python_module` are removed (superseded by marker
  walkup + bridge column).
- macOS-only caveat unchanged: the `lsof` fallback is macOS/Linux; the `cwd`
  source itself is cross-platform via sysinfo.
