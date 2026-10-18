---
id: IDEA-001
status: promoted
date: 2026-10-18
resolution-date: 2026-10-18
promoted-to: ../agent-inspection.md
---

# Fast project inspection for agents

## Idea

An agent should fetch useful memory/process information with one command,
scoped to its project folder by default. The interface should be simple and
verbose enough to explain the scope, numbers, and missing information.

The first slice is implemented. The current behavior and JSON contract belong
in [Agent inspection](../agent-inspection.md); this file retains the reasoning
and future options, rather than serving as a second interface specification.

## Investigation

Memo's purpose is to explain RAM and swap usage by application or project,
without instrumenting services. A local CLI fits agents that already have shell
access and need to identify a project's largest memory consumers.

At intake, the legacy CLI had four obstacles: machine-wide output only, an
unconditional slower swap scan, human tables with progress mixed into stdout,
and no stable JSON contract. Existing project attribution also used nearest
markers plus command-line hints, which are unsuitable as strict query scope.

### Changes after challenging the first proposal

- **Narrow the release.** Swap coverage, deadlines, global views, and extra
  protocols made a quick RSS query unnecessarily dependent on broader work.
  Deliver project inspection first; defer those extensions.
- **Prefer the Git worktree.** A nested `package.json` should not unexpectedly
  hide the rest of the repository. Automatic inspection selects the nearest
  enclosing Git marker, then falls back to other project markers. Explicit
  paths select the exact folder. Legacy TUI grouping keeps its existing rules.
- **Identify processes usefully.** A list of several `node` names and PIDs is
  insufficient context. Include a compact command label alongside each PID.
- **Keep output honest.** Unmeasured swap must not look like zero; RSS-only
  inspection omits it. Folder attribution can miss services running elsewhere,
  so every report carries that limitation.
- **Separate work by files.** The existing TUI work owned app/rendering/grouping
  changes. Inspection fits a new module and a small CLI dispatch addition,
  sharing collection and the marker list without changing the memory model.

The original nearest-marker default, proposed runtime breakdown, swap-specific
JSON fields, `--deep`, and `--all` are not part of the delivered contract.

## Decision

Promoted the narrow interface on 2026-10-18 following user approval and an
explicit implementation request:

```bash
memo inspect
memo inspect --json
memo inspect --project /path/to/project
memo inspect --limit 20
```

The result contains scope, observation time/duration, total matching processes,
aggregate RSS, and the largest processes. Text supports quick inspection; JSON
supports automation. Existing TUI/legacy CLI entry points stay available.
`memo --version` and `memo -v` were also added at the user's request.

No daemon, registration, or MCP server is required. Implementation evidence and
limitations are recorded in the current [owner document](../agent-inspection.md).

## Deferred extensions

These remain proposals, without a delivery commitment:

| Option | Reason to revisit / unresolved question |
|---|---|
| Scoped swap scan (`--deep`) | Needed when RSS is insufficient; requires explicit unknown/partial coverage and subprocess deadlines |
| Machine-wide query (`--all`) | Needed for host-level diagnosis; define global grouping and system-vs-process totals separately |
| MCP wrapper | Useful for agents without a shell; reuse the CLI result contract and require explicit project context |
| Before/after snapshots | Useful for regression investigations; process identity must account for PID reuse |
| Cache or background service | Consider only if measured one-shot latency becomes a real problem |

Ports, CPU, logs, application health, repository search, and automatic process
termination were considered broader scope and excluded from this feature.

## References

- [Current agent interface and verification](../agent-inspection.md)
- [Product purpose and commands](../../README.md)
- [Architecture](../../ARCHITECTURE.md)
- [ADR 0001: legacy project grouping](../adr/0001-group-by-project.md)
- [Inspection implementation](../../src/inspect.rs)
