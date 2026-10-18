//! `memo inspect` — project-scoped memory snapshot for agents (IDEA-001).
//!
//! Scope: nearest enclosing Git worktree root (`.git` directory OR file),
//! else nearest existing project marker; without either → `project_not_found`.
//! `--project PATH` selects that exact directory. Membership is cwd-based
//! only: a process belongs when its resolvable cwd is the scope or a
//! descendant. No argv/ancestry inference; missing swap is never reported
//! (RSS-only first release).

use crate::scanner::ProcessMemory;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const REPORT_VERSION: u32 = 1;

/// Attribution caveat always attached to a report.
pub const LIMITATION: &str = "attribution is cwd-based: processes without a resolvable cwd (and background services started elsewhere) are not counted";

// ─── Scope ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeSource {
    /// Nearest enclosing `.git` (directory or worktree file)
    Git,
    /// Nearest enclosing project marker (package.json, Cargo.toml, …)
    Marker,
    /// Exact directory chosen via --project
    Explicit,
}

#[derive(Debug, Clone, Serialize)]
pub struct InspectScope {
    pub root: PathBuf,
    pub source: ScopeSource,
    /// Marker name that established the scope (".git", "package.json", …);
    /// null for explicit scopes.
    pub marker: Option<String>,
}

#[derive(Debug, Clone)]
pub enum InspectError {
    /// No enclosing .git / marker found.
    ProjectNotFound { searched_from: PathBuf },
    /// --project path does not exist or is not a directory.
    InvalidProject { path: String },
    /// Collector returned no process table at all.
    ScanFailed,
    /// Flag value or flag combination is invalid.
    BadArgument { message: String },
}

impl InspectError {
    pub fn code(&self) -> &'static str {
        match self {
            InspectError::ProjectNotFound { .. } => "project_not_found",
            InspectError::InvalidProject { .. } => "invalid_project",
            InspectError::ScanFailed => "scan_failed",
            InspectError::BadArgument { .. } => "bad_argument",
        }
    }

    pub fn message(&self) -> String {
        match self {
            InspectError::ProjectNotFound { searched_from } => format!(
                "no .git or project marker found above {}; run inside a project or pass --project .",
                searched_from.display()
            ),
            InspectError::InvalidProject { path } => {
                format!("--project {path}: not an existing directory")
            }
            InspectError::ScanFailed => "process table is empty — collector failed".into(),
            InspectError::BadArgument { message } => message.clone(),
        }
    }
}

/// Auto-detect scope: walk up from `start` (inclusive) to the nearest `.git`
/// (dir or worktree file), else nearest marker. Canonicalizes `start`.
pub fn find_scope(start: &Path) -> Result<InspectScope, InspectError> {
    let start = start
        .canonicalize()
        .map_err(|_| InspectError::ProjectNotFound {
            searched_from: start.into(),
        })?;
    let mut fallback = None;
    for dir in start.ancestors() {
        if dir.join(".git").exists() {
            return Ok(InspectScope {
                root: dir.to_path_buf(),
                source: ScopeSource::Git,
                marker: Some(".git".into()),
            });
        }
        if fallback.is_none()
            && let Some(marker) = crate::group::MARKERS
                .iter()
                .find(|marker| **marker != ".git" && dir.join(marker).exists())
        {
            fallback = Some(InspectScope {
                root: dir.to_path_buf(),
                source: ScopeSource::Marker,
                marker: Some((*marker).to_string()),
            });
        }
    }
    fallback.ok_or(InspectError::ProjectNotFound {
        searched_from: start,
    })
}

/// Explicit scope: exact directory, works anywhere on disk.
pub fn explicit_scope(path: &str) -> Result<InspectScope, InspectError> {
    let p = Path::new(path);
    if !p.is_dir() {
        return Err(InspectError::InvalidProject { path: path.into() });
    }
    Ok(InspectScope {
        root: p
            .canonicalize()
            .map_err(|_| InspectError::InvalidProject { path: path.into() })?,
        source: ScopeSource::Explicit,
        marker: None,
    })
}

/// Component-wise containment: `cwd` is `root` or a descendant.
/// Both paths must be canonical — `starts_with` never matches partial
/// components, so `/x/proj-other` is not within `/x/proj`.
pub fn is_within(cwd: &Path, root: &Path) -> bool {
    cwd.starts_with(root)
}

// ─── Report ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct InspectProc {
    pub pid: i32,
    pub name: String,
    pub rss: u64,
    /// Compact command label to tell apart multiple same-name processes.
    pub command: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InspectReport {
    pub version: u32,
    pub scope: PathBuf,
    pub source: ScopeSource,
    pub marker: Option<String>,
    /// RFC 3339 UTC observation time.
    pub observed_at: String,
    pub duration_ms: u64,
    /// All matched processes — totals are complete, `top` is a preview.
    pub total_processes: usize,
    pub total_rss: u64,
    pub top: Vec<InspectProc>,
    pub omitted: usize,
    pub limitation: &'static str,
}

/// Normalize observed cwd values before pure scope membership/aggregation.
fn normalize_cwds(processes: &mut [ProcessMemory]) {
    for process in processes {
        if !Path::new(&process.cwd).is_absolute() {
            process.cwd.clear();
            continue;
        }
        process.cwd = Path::new(&process.cwd)
            .canonicalize()
            .ok()
            .filter(|path| path.is_dir())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
}

/// Pure aggregation of processes with canonical cwd values. No I/O.
pub fn aggregate(
    processes: &[ProcessMemory],
    inspector_pid: i32,
    scope: &InspectScope,
    limit: usize,
    observed_at: SystemTime,
    duration_ms: u64,
) -> InspectReport {
    let mut members: Vec<&ProcessMemory> = processes
        .iter()
        .filter(|p| {
            p.pid != inspector_pid && !p.cwd.is_empty() && is_within(Path::new(&p.cwd), &scope.root)
        })
        .collect();
    members.sort_by(|a, b| b.rss.cmp(&a.rss).then(a.pid.cmp(&b.pid)));

    let total_rss = members.iter().map(|p| p.rss).sum();
    let top: Vec<InspectProc> = members
        .iter()
        .take(limit)
        .map(|p| InspectProc {
            pid: p.pid,
            name: p.name.clone(),
            rss: p.rss,
            command: compact_command(p),
        })
        .collect();
    InspectReport {
        version: REPORT_VERSION,
        scope: scope.root.clone(),
        source: scope.source,
        marker: scope.marker.clone(),
        observed_at: DateTime::<Utc>::from(observed_at).to_rfc3339_opts(SecondsFormat::Secs, true),
        duration_ms,
        total_processes: members.len(),
        total_rss,
        omitted: members.len().saturating_sub(top.len()),
        top,
        limitation: LIMITATION,
    }
}

fn compact_command(process: &ProcessMemory) -> String {
    let tokens: Vec<&str> = process.cmdline.split_whitespace().take(3).collect();
    let Some((executable, arguments)) = tokens.split_first() else {
        return clip(&process.name, 48);
    };
    let executable = executable.rsplit('/').next().unwrap_or(executable);
    clip(
        &std::iter::once(executable)
            .chain(arguments.iter().copied())
            .collect::<Vec<_>>()
            .join(" "),
        48,
    )
}

fn clip(text: &str, limit: usize) -> String {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.chars().count() <= limit {
        text
    } else {
        text.chars()
            .take(limit.saturating_sub(1))
            .chain(std::iter::once('…'))
            .collect()
    }
}

// ─── CLI entry ──────────────────────────────────────────────────────────────

/// Parsed `memo inspect` flags.
pub struct InspectArgs {
    pub json: bool,
    pub project: Option<String>,
    pub limit: String,
}

/// Runs the inspection and returns the process exit code.
/// 0 = usable result, 2 = scope/argument error (with `--json`, errors are
/// emitted as compact JSON on stdout), 1 = scan failure without usable data.
pub fn run(args: InspectArgs) -> i32 {
    let started = std::time::Instant::now();
    let now = SystemTime::now();

    let fail = |e: &InspectError| -> i32 {
        if args.json {
            println!(
                "{}",
                serde_json::json!({
                    "version": REPORT_VERSION,
                    "error": { "code": e.code(), "message": e.message() }
                })
            );
        } else {
            eprintln!("memo: {}", e.message());
        }
        match e {
            InspectError::ScanFailed => 1,
            _ => 2,
        }
    };

    let limit: usize = match args.limit.parse() {
        Ok(n) if n >= 1 => n,
        _ => {
            return fail(&InspectError::BadArgument {
                message: format!("--limit must be a positive integer, got {:?}", args.limit),
            });
        }
    };

    let scope = match &args.project {
        Some(p) => explicit_scope(p),
        None => std::env::current_dir()
            .map_err(|_| InspectError::ProjectNotFound {
                searched_from: PathBuf::from("."),
            })
            .and_then(|cwd| find_scope(&cwd)),
    };
    let scope = match scope {
        Ok(s) => s,
        Err(e) => return fail(&e),
    };

    let mut processes = crate::collect_processes();
    if processes.is_empty() {
        return fail(&InspectError::ScanFailed);
    }

    normalize_cwds(&mut processes);
    let mut report = aggregate(&processes, std::process::id() as i32, &scope, limit, now, 0);
    report.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    if args.json {
        match serde_json::to_string(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("memo: serialization failed: {e}");
                return fail(&InspectError::ScanFailed);
            }
        }
    } else {
        print_report(&report);
    }
    0
}

fn print_report(r: &InspectReport) {
    let marker = r.marker.as_deref().unwrap_or("-");
    println!(
        "scope     {} ({:?}, marker {marker})",
        r.scope.display(),
        r.source
    );
    println!(
        "observed  {} ({} ms) · {} processes · {} rss · {} omitted",
        r.observed_at,
        r.duration_ms,
        r.total_processes,
        crate::scanner::format_size(r.total_rss),
        r.omitted
    );
    if r.total_processes == 0 {
        println!("note      {r}", r = r.limitation);
        return;
    }
    println!("{:<8} {:<24} {:>10}  COMMAND", "PID", "NAME", "RSS");
    for p in &r.top {
        println!(
            "{:<8} {:<24} {:>10}  {}",
            p.pid,
            clip(&p.name, 24),
            crate::scanner::format_size(p.rss),
            p.command
        );
    }
    println!("note      {r}", r = r.limitation);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::UNIX_EPOCH;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("memo-inspect-{}-{name}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn proc(pid: i32, name: &str, rss: u64, cwd: &str, cmdline: &str) -> ProcessMemory {
        let mut p = ProcessMemory::new(pid, name.into(), rss);
        p.cwd = cwd.into();
        p.cmdline = cmdline.into();
        p
    }

    #[test]
    fn scope_prefers_nearest_git_including_worktree_file() {
        let base = tmpdir("gitfile");
        let proj = base.join("proj");
        fs::create_dir_all(proj.join("src")).unwrap();
        // Worktree: .git is a FILE
        fs::write(proj.join(".git"), "gitdir: /elsewhere").unwrap();

        let s = find_scope(&proj.join("src")).unwrap();
        assert_eq!(s.root, proj.canonicalize().unwrap());
        assert_eq!(s.source, ScopeSource::Git);
        assert_eq!(s.marker.as_deref(), Some(".git"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn scope_nearest_marker_wins_over_outer() {
        let base = tmpdir("nested");
        let outer = base.join("outer");
        let inner = outer.join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(outer.join("package.json"), "{}").unwrap();
        fs::write(inner.join("Cargo.toml"), "").unwrap();

        let s = find_scope(&inner).unwrap();
        assert_eq!(s.root, inner.canonicalize().unwrap());
        assert_eq!(s.source, ScopeSource::Marker);
        assert_eq!(s.marker.as_deref(), Some("Cargo.toml"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn scope_git_file_beats_outer_marker() {
        let base = tmpdir("gitovermarker");
        let proj = base.join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(base.join("package.json"), "{}").unwrap();
        fs::write(proj.join(".git"), "gitdir: x").unwrap();

        let s = find_scope(&proj).unwrap();
        assert_eq!(s.source, ScopeSource::Git);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn scope_none_reports_project_not_found() {
        let base = tmpdir("empty");
        let e = find_scope(&base).unwrap_err();
        assert_eq!(e.code(), "project_not_found");
        assert!(e.message().contains("--project"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn explicit_scope_works_outside_home_and_needs_no_marker() {
        let base = tmpdir("explicit");
        let s = explicit_scope(base.to_str().unwrap()).unwrap();
        assert_eq!(s.source, ScopeSource::Explicit);
        assert_eq!(s.marker, None);
        assert_eq!(s.root, base.canonicalize().unwrap());
        // Nonexistent path → invalid_project
        assert_eq!(
            explicit_scope("/nonexistent-memo-zz").unwrap_err().code(),
            "invalid_project"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn membership_is_componentwise() {
        let root = Path::new("/tmp/x/proj");
        assert!(is_within(Path::new("/tmp/x/proj"), root));
        assert!(is_within(Path::new("/tmp/x/proj/packages/app"), root));
        assert!(
            !is_within(Path::new("/tmp/x/proj-other"), root),
            "sibling prefix must not match"
        );
        assert!(!is_within(Path::new("/tmp/x/proj2/src"), root));
    }

    #[test]
    fn aggregate_orders_limits_and_keeps_totals() {
        let base = tmpdir("agg");
        fs::create_dir_all(base.join("pkg/inner")).unwrap();
        let root = base.canonicalize().unwrap();
        let rc = root.to_str().unwrap();
        let nested = base.join("pkg/inner").canonicalize().unwrap();
        let nc = nested.to_str().unwrap();

        let procs = vec![
            proc(1, "node", 100, rc, "node vite"),            // in scope
            proc(2, "node", 300, nc, "node inner"),           // nested descendant: in
            proc(3, "node", 200, rc, "node test"),            // in scope
            proc(9, "node", 500, "/somewhere/else", "node"),  // outside
            proc(8, "node", 999, "", "node"),                 // no cwd: not attributable
            proc(7, "inspector", 10_000, rc, "memo inspect"), // self
        ];
        let scope = explicit_scope(rc).unwrap();
        let t = UNIX_EPOCH;
        let r = aggregate(&procs, 7, &scope, 2, t, 5);

        assert_eq!(r.total_processes, 3);
        assert_eq!(r.total_rss, 600);
        assert_eq!(r.omitted, 1);
        assert_eq!(r.top.len(), 2);
        assert_eq!([r.top[0].pid, r.top[1].pid], [2, 3], "rss desc");
        assert_eq!(r.observed_at, "1970-01-01T00:00:00Z");
        // Tie-break: same rss → lower pid first
        let procs_tie = vec![
            proc(11, "node", 100, rc, "a"),
            proc(10, "node", 100, rc, "b"),
        ];
        let r2 = aggregate(&procs_tie, 0, &scope, 5, t, 0);
        assert_eq!([r2.top[0].pid, r2.top[1].pid], [10, 11]);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn aggregate_empty_is_usable() {
        let base = tmpdir("aggempty");
        let scope = explicit_scope(base.to_str().unwrap()).unwrap();
        let r = aggregate(&[], 1, &scope, 5, UNIX_EPOCH, 0);
        assert_eq!(r.total_processes, 0);
        assert_eq!(r.total_rss, 0);
        assert!(r.top.is_empty());
        assert!(!r.limitation.is_empty());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn outer_git_beats_nested_package_marker() {
        let base = tmpdir("outergit");
        let inner = base.join("packages/app/src");
        fs::create_dir_all(&inner).unwrap();
        fs::create_dir(base.join(".git")).unwrap();
        fs::write(base.join("packages/app/package.json"), "{}").unwrap();
        let scope = find_scope(&inner).unwrap();
        assert_eq!(scope.root, base.canonicalize().unwrap());
        assert_eq!(scope.source, ScopeSource::Git);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn cwd_normalization_resolves_symlinks_and_drops_missing_paths() {
        let base = tmpdir("symlink");
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        let alias = base.join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let mut processes = vec![
            proc(1, "node", 42, alias.to_str().unwrap(), "node app.js"),
            proc(2, "node", 99, "/missing-memo-process-cwd", "node app.js"),
            proc(3, "node", 99, "", root.to_str().unwrap()),
        ];
        normalize_cwds(&mut processes);
        let scope = explicit_scope(root.to_str().unwrap()).unwrap();
        assert_eq!(processes[0].cwd, scope.root.to_string_lossy());
        assert!(processes[1].cwd.is_empty());
        let report = aggregate(&processes, 0, &scope, 5, UNIX_EPOCH, 0);
        assert_eq!(report.total_processes, 1);
        assert_eq!(report.total_rss, 42);
        let _ = fs::remove_dir_all(base);
    }
}
