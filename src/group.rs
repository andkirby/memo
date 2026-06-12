use crate::scanner::ProcessMemory;
use std::collections::HashMap;

/// A group of processes belonging to the same "app".
/// e.g. all "Google Chrome Helper (Renderer)" + "Google Chrome" → one Chrome group.
#[derive(Debug, Clone)]
pub struct AppGroup {
    pub name: String,
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
}

/// Normalise a process name into a canonical "app" name.
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

    // Node / bun / deno — identify by project
    if n == "node" || n == "bun" || n == "deno" {
        if let Some(proj) = extract_project(cmdline) {
            return format!("{} ({})", name, proj);
        }
        return name.to_string();
    }

    // Python — identify by project or module
    if n == "python" || n.starts_with("python3") || n == "python3.10" || n == "python3.11" || n == "python3.12" {
        if let Some(proj) = extract_python_module(cmdline) {
            return format!("Python ({})", proj);
        }
        return "Python".into();
    }

    name.to_string()
}

fn extract_project(cmdline: &str) -> Option<String> {
    let parts: Vec<&str> = cmdline.split_whitespace().collect();
    for part in parts.iter().rev() {
        if part.starts_with('/') || part.starts_with("~/") || part.starts_with("./") {
            if let Some(name) = part.split('/').last() {
                let name = name.trim_end_matches('/');
                if !name.is_empty() && name != "." && name != ".." {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// Extract a recognizable module name from python command line
fn extract_python_module(cmdline: &str) -> Option<String> {
    let parts: Vec<&str> = cmdline.split_whitespace().collect();
    for (i, part) in parts.iter().enumerate() {
        if part.ends_with("/python3") || part.ends_with("/python") || part.ends_with("/python3.10")
            || part.ends_with("/python3.11") || part.ends_with("/python3.12") {
            // Look for -m module or the script name after python
            if let Some(next) = parts.get(i + 1) {
                if *next == "-m" {
                    if let Some(mod_name) = parts.get(i + 2) {
                        return Some(mod_name.to_string());
                    }
                } else if next == &"-u" {
                    if let Some(mod_name) = parts.get(i + 2) {
                        // Could be a script path or module
                        return Some(mod_name.split('/').last().unwrap_or(mod_name).to_string());
                    }
                } else if !next.starts_with('-') {
                    return Some(next.split('/').last().unwrap_or(next).to_string());
                }
            }
        }
        // uvicorn is a common python server
        if part.contains("uvicorn") {
            return Some("uvicorn".to_string());
        }
    }
    extract_project(cmdline)
}

/// Group a flat list of processes into AppGroups, sorted by total memory descending.
pub fn group_processes(processes: &[ProcessMemory]) -> Vec<AppGroup> {
    let mut groups: HashMap<String, Vec<ProcessMemory>> = HashMap::new();

    for p in processes {
        let app_name = canonical_app_name(&p.name, &p.cmdline);
        groups.entry(app_name).or_default().push(p.clone());
    }

    let mut result: Vec<AppGroup> = groups
        .into_iter()
        .map(|(name, mut procs)| {
            procs.sort_by(|a, b| b.rss.cmp(&a.rss));
            let mut g = AppGroup { name, processes: procs, total_rss: 0, total_swap: 0, thread_count: 0 };
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

    fn pm(pid: i32, name: &str, rss: u64) -> ProcessMemory {
        ProcessMemory::new(pid, name.into(), rss)
    }

    #[test]
    fn test_chrome_grouping() {
        let groups = group_processes(&[
            pm(1, "Google Chrome", 100), pm(2, "Google Chrome Helper (Renderer)", 200),
            pm(3, "Google Chrome Helper (GPU)", 50), pm(4, "Google Chrome Helper", 30),
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Google Chrome");
        assert_eq!(groups[0].processes.len(), 4);
        assert_eq!(groups[0].total_rss, 380);
    }

    #[test]
    fn test_mixed_apps() {
        let groups = group_processes(&[
            pm(1, "Google Chrome", 500), pm(2, "Slack", 200), pm(3, "Google Chrome Helper (Renderer)", 300),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "Google Chrome");
        assert_eq!(groups[1].name, "Slack");
    }
}
