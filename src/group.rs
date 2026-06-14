use crate::scanner::ProcessMemory;
use std::collections::HashMap;
use std::path::PathBuf;

// ─── Grouping mode (see ADR 0001) ──────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupMode {
    /// Group by canonical app name (Chrome, Slack, node, …) — the legacy view.
    App,
    /// Group by project directory (cwd marker walkup). Processes with no
    /// project fall back to app-name grouping (hybrid). TUI default.
    Project,
}

/// Whether a group was keyed by a project path or by an app name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    App,
    Project,
}

#[derive(Debug, Clone)]
pub struct AppGroup {
    pub name: String,
    pub kind: GroupKind,
    pub processes: Vec<ProcessMemory>,
    pub total_rss: u64,
    pub total_swap: u64,
    pub thread_count: usize,
}

impl AppGroup {
    pub fn total(&self) -> u64 {
        self.total_rss.saturating_add(self.total_swap)
    }

    fn recalc(&mut self) {
        self.total_rss = self.processes.iter().map(|p| p.rss).sum();
        self.total_swap = self.processes.iter().map(|p| p.swap).sum();
        self.thread_count = self.processes.iter().map(|p| p.threads).sum();
    }

    /// The bridge column — the *other* view's key, so switching views loses no
    /// information. Public entrypoint uses $HOME.
    pub fn bridge(&self, mode: GroupMode) -> String {
        self.bridge_in(mode, &home_dir())
    }

    fn bridge_in(&self, mode: GroupMode, home: &str) -> String {
        match (mode, self.kind) {
            // Project group → runtime breakdown, e.g. "node:15 bun:3"
            (GroupMode::Project, GroupKind::Project) => {
                let mut counts: Vec<(String, usize)> = {
                    let mut m: HashMap<String, usize> = HashMap::new();
                    for p in &self.processes {
                        *m.entry(runtime_family(&p.name)).or_insert(0) += 1;
                    }
                    m.into_iter().collect()
                };
                counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                counts.iter()
                    .map(|(f, c)| format!("{}:{}", f, c))
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            // App group (in either mode) → distinct projects under that app
            (GroupMode::App, GroupKind::App) => {
                let mut projs: Vec<String> = Vec::new();
                for p in &self.processes {
                    if let Some(r) = project_root_in(&p.cwd, &p.cmdline, home) {
                        let s = shorten_in(&r, home);
                        if !projs.contains(&s) {
                            projs.push(s);
                        }
                    }
                }
                match projs.len() {
                    0 => String::new(),
                    1 => projs[0].clone(),
                    n => format!("{} projects", n),
                }
            }
            // App-fallback group inside Project mode, or any other combo: none.
            _ => String::new(),
        }
    }
}

// ─── App-name canonicalisation ─────────────────────────────────────────────

fn canonical_app_name(name: &str, cmdline: &str) -> String {
    let n = name.to_lowercase();

    // Well-known multi-process browsers
    if n.starts_with("google chrome") || n.contains("chrome helper") { return "Google Chrome".into(); }
    if n.starts_with("firefox") || n.contains("firefox") { return "Firefox".into(); }
    if n.starts_with("safari") || n.contains("safariweb") { return "Safari".into(); }
    if n.starts_with("arc") || n.contains("arc helper") { return "Arc".into(); }
    if n.starts_with("microsoft edge") || n.contains("edge helper") { return "Microsoft Edge".into(); }
    if n.starts_with("brave") || n.contains("brave helper") { return "Brave".into(); }
    if n.contains("opera") { return "Opera".into(); }

    // Electron apps: "X Helper (Renderer)" → "X"
    for suffix in &[" Helper (Renderer)", " Helper (GPU)", " Helper (Plugin)", " Helper (Service)", " Helper"] {
        if let Some(app) = name.strip_suffix(suffix) {
            return app.to_string();
        }
    }

    // JetBrains IDEs
    if n == "java" || n == "launcher" {
        for (kw, label) in &[("idea","IntelliJ IDEA"),("webstorm","WebStorm"),("clion","CLion"),
            ("pycharm","PyCharm"),("goland","GoLand"),("rustrover","RustRover"),("dataspell","DataSpell")] {
            if cmdline.contains(kw) { return (*label).into(); }
        }
    }

    // VS Code / Cursor / Windsurf / Codex
    if n.contains("code helper") || n == "electron" && cmdline.contains("vscode") { return "VS Code".into(); }
    if n.contains("cursor helper") || n == "electron" && cmdline.contains("cursor") { return "Cursor".into(); }
    if n.contains("windsurf helper") || n == "electron" && cmdline.contains("windsurf") { return "Windsurf".into(); }
    if n.contains("codex") { return "Codex".into(); }

    // Docker
    if n.starts_with("docker") || n.contains("com.docker") { return "Docker".into(); }
    // Chat apps
    if n.contains("slack") { return "Slack".into(); }
    if n.contains("discord") { return "Discord".into(); }
    if n.contains("telegram") { return "Telegram".into(); }
    if n.contains("zoom") { return "Zoom".into(); }
    if n.contains("spotify") { return "Spotify".into(); }

    // Dev runtimes: one row per runtime. The bridge column carries projects.
    if n == "node" || n == "node-repl" { return "node".into(); }
    if n == "bun" { return "bun".into(); }
    if n == "deno" { return "deno".into(); }
    if n == "python" || n == "python2" || n.starts_with("python3") { return "Python".into(); }

    name.to_string()
}

// ─── Project resolution: cwd → nearest marker ──────────────────────────────

const MARKERS: &[&str] = &[
    ".git", "package.json", "pyproject.toml", "bun.lockb", "bun.lock",
    "Cargo.toml", "go.mod", "pom.xml", "Makefile", "composer.json",
];

/// Paths under home that are NOT projects (editor/CLI internals). Home-relative.
const EXCLUDE_SUFFIXES: &[&str] = &[
    "/Library", "/.cache", "/.local", "/.bun/install", "/.npm",
    "/.cargo", "/.config", "/.rustup", "/.volta", "/.nvm",
];

fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn excluded_in(path: &str, home: &str) -> bool {
    for suf in EXCLUDE_SUFFIXES {
        if path.starts_with(&format!("{}{}", home, suf)) {
            return true;
        }
    }
    false
}

/// Walk up from `dir` to the nearest project marker. Only considers paths under
/// `home`; excludes editor/CLI internals. None if no marker found.
fn find_marker_up_in(dir: &str, home: &str) -> Option<String> {
    if dir.is_empty() || !dir.starts_with('/') { return None; }
    if excluded_in(dir, home) { return None; }
    let home_prefix = format!("{}/", home);
    if dir != home && !dir.starts_with(&home_prefix) { return None; }

    let mut d = PathBuf::from(dir);
    loop {
        for m in MARKERS {
            if d.join(m).exists() {
                return Some(d.to_string_lossy().to_string());
            }
        }
        if !d.pop() { break; }
        if d == PathBuf::from(home) {
            // Check home itself once, then stop — never walk above $HOME.
            for m in MARKERS {
                if d.join(m).exists() {
                    return Some(d.to_string_lossy().to_string());
                }
            }
            break;
        }
    }
    None
}

/// Resolve a project root: prefer cwd, fall back to the first home-relative
/// path in cmdline. None if the process isn't under a project.
fn project_root_in(cwd: &str, cmdline: &str, home: &str) -> Option<String> {
    if let Some(r) = find_marker_up_in(cwd, home) { return Some(r); }
    let home_prefix = format!("{}/", home);
    for tok in cmdline.split_whitespace() {
        if tok.starts_with(&home_prefix) {
            if let Some(r) = find_marker_up_in(tok, home) { return Some(r); }
        }
    }
    None
}

/// Map a process name to a runtime family label for the bridge column.
fn runtime_family(name: &str) -> String {
    let n = name.trim_start_matches('-').to_lowercase();
    if matches!(n.as_str(), "node" | "node-repl" | "npm" | "npx" | "yarn" | "pnpm") {
        return "node".into();
    }
    if n == "bun" { return "bun".into(); }
    if n == "deno" { return "deno".into(); }
    if n == "python" || n == "python2" || n.starts_with("python3") || n == "uv" || n == "poetry" {
        return "python".into();
    }
    if matches!(n.as_str(), "ruby" | "rails" | "bundle" | "rake") {
        return "ruby".into();
    }
    if n == "go" { return "go".into(); }
    n
}

fn shorten_in(path: &str, home: &str) -> String {
    if path == home { return "~".into(); }
    if let Some(rest) = path.strip_prefix(&format!("{}/", home)) {
        return format!("~/{}", rest);
    }
    path.to_string()
}

// ─── Grouping ──────────────────────────────────────────────────────────────

/// Group processes by `mode`, sorted by total memory (rss + swap) descending.
/// Grouping is a pure function of the process list — no hidden state.
pub fn group_processes(processes: &[ProcessMemory], mode: GroupMode) -> Vec<AppGroup> {
    group_processes_in(processes, mode, &home_dir())
}

fn group_processes_in(processes: &[ProcessMemory], mode: GroupMode, home: &str) -> Vec<AppGroup> {
    let mut buckets: HashMap<String, (GroupKind, Vec<ProcessMemory>)> = HashMap::new();

    for p in processes {
        let (kind, key) = match mode {
            GroupMode::Project => match project_root_in(&p.cwd, &p.cmdline, home) {
                Some(root) => (GroupKind::Project, root),
                None => (GroupKind::App, canonical_app_name(&p.name, &p.cmdline)),
            },
            GroupMode::App => (GroupKind::App, canonical_app_name(&p.name, &p.cmdline)),
        };
        buckets.entry(key).or_insert_with(|| (kind, Vec::new())).1.push(p.clone());
    }

    let mut result: Vec<AppGroup> = buckets
        .into_iter()
        .map(|(key, (kind, mut procs))| {
            procs.sort_by(|a, b| b.rss.cmp(&a.rss));
            let name = if kind == GroupKind::Project { shorten_in(&key, home) } else { key };
            let mut g = AppGroup {
                name, kind, processes: procs,
                total_rss: 0, total_swap: 0, thread_count: 0,
            };
            g.recalc();
            g
        })
        .collect();

    result.sort_by(|a, b| b.total().cmp(&a.total()));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::ProcessMemory;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique temp dir used as a fake $HOME. NOT under the real $HOME, so
    /// tests don't pollute the user's tree and don't race on the env var.
    fn fake_home() -> String {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut p = std::env::temp_dir();
        p.push(format!("memo-test-{}/{}", std::process::id(), n));
        fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().to_string()
    }

    fn pm(pid: i32, name: &str, rss: u64) -> ProcessMemory {
        ProcessMemory::new(pid, name.into(), rss)
    }

    fn pm_cwd(pid: i32, name: &str, rss: u64, cwd: &str, cmdline: &str) -> ProcessMemory {
        ProcessMemory {
            pid, name: name.into(), cmdline: cmdline.into(), cwd: cwd.into(),
            rss, swap: 0, threads: 1,
        }
    }

    #[test]
    fn app_mode_groups_chrome_helpers() {
        let groups = group_processes(&[
            pm(1, "Google Chrome", 100),
            pm(2, "Google Chrome Helper (Renderer)", 200),
            pm(3, "Google Chrome Helper (GPU)", 50),
        ], GroupMode::App);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Google Chrome");
        assert_eq!(groups[0].kind, GroupKind::App);
        assert_eq!(groups[0].processes.len(), 3);
        assert_eq!(groups[0].total_rss, 350);
    }

    #[test]
    fn app_mode_merges_node_into_one_row() {
        // Two node procs from different projects collapse to one "node" group;
        // the bridge carries the per-project split.
        let home = fake_home();
        fs::create_dir_all(format!("{}/projA", home)).unwrap();
        fs::create_dir_all(format!("{}/projB", home)).unwrap();
        fs::write(format!("{}/projA/package.json", home), "{}").unwrap();
        fs::write(format!("{}/projB/package.json", home), "{}").unwrap();
        let groups = group_processes_in(&[
            pm_cwd(10, "node", 300, &format!("{}/projA", home), "node vite"),
            pm_cwd(11, "node", 200, &format!("{}/projB", home), "node vite"),
        ], GroupMode::App, &home);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "node");
        assert_eq!(groups[0].bridge_in(GroupMode::App, &home), "2 projects");
    }

    #[test]
    fn project_mode_merges_all_runtimes_of_one_project() {
        let home = fake_home();
        fs::create_dir_all(format!("{}/self-calendar", home)).unwrap();
        fs::write(format!("{}/self-calendar/.git", home), "").unwrap();
        let groups = group_processes_in(&[
            pm_cwd(1, "node", 500, &format!("{}/self-calendar", home), "node vite"),
            pm_cwd(2, "bun", 300, &format!("{}/self-calendar", home), "bun start"),
            pm_cwd(3, "-zsh", 10, &format!("{}/self-calendar", home), "-zsh"),
        ], GroupMode::Project, &home);
        assert_eq!(groups.len(), 1, "all three collapse to one project group");
        assert_eq!(groups[0].kind, GroupKind::Project);
        assert!(groups[0].name.ends_with("self-calendar"));
        assert_eq!(groups[0].processes.len(), 3);
        // runtime breakdown, count-sorted then name-sorted: bun:1 node:1 zsh:1
        assert_eq!(groups[0].bridge_in(GroupMode::Project, &home), "bun:1 node:1 zsh:1");
    }

    #[test]
    fn project_mode_app_fallback_for_slack() {
        let home = fake_home();
        let groups = group_processes_in(&[
            pm_cwd(1, "Slack Helper (Renderer)", 200, "/nonexistent", ""),
        ], GroupMode::Project, &home);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Slack");
        assert_eq!(groups[0].kind, GroupKind::App);
    }

    #[test]
    fn marker_walkup_collapses_nested_subdir() {
        let home = fake_home();
        fs::create_dir_all(format!("{}/proj/vendor/pkg/deep", home)).unwrap();
        fs::write(format!("{}/proj/.git", home), "").unwrap();
        let root = find_marker_up_in(
            &format!("{}/proj/vendor/pkg/deep", home), &home);
        assert_eq!(root.as_deref(), Some(format!("{}/proj", home).as_str()));
    }

    #[test]
    fn marker_walkup_excludes_editor_internals() {
        let home = fake_home();
        let zed = format!("{}/Library/Application Support/Zed/x", home);
        fs::create_dir_all(&zed).unwrap();
        fs::write(format!("{}/package.json", zed), "{}").unwrap();
        // Even though a marker exists, the path is excluded.
        let root = find_marker_up_in(&zed, &home);
        assert!(root.is_none(), "editor internal paths must not become projects");
    }

    #[test]
    fn runtime_family_maps_known_runtimes() {
        assert_eq!(runtime_family("node"), "node");
        assert_eq!(runtime_family("node-repl"), "node");
        assert_eq!(runtime_family("python3.11"), "python");
        assert_eq!(runtime_family("-zsh"), "zsh"); // login shell → family zsh
        assert_eq!(runtime_family("vite"), "vite"); // unknown passthrough
    }

    #[test]
    fn shorten_replaces_home_prefix() {
        let home = "/Users/test";
        assert_eq!(shorten_in("/Users/test/proj", home), "~/proj");
        assert_eq!(shorten_in("/Users/test", home), "~");
        assert_eq!(shorten_in("/opt/other", home), "/opt/other");
    }
}
