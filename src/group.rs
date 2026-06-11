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
    pub total_compressed: u64,
    pub total_footprint: u64,
    pub thread_count: usize,
}

impl AppGroup {
    pub fn total(&self) -> u64 {
        self.total_footprint + self.total_swap
    }

    fn recalc(&mut self) {
        self.total_rss = self.processes.iter().map(|p| p.physical_footprint).sum();
        self.total_swap = self.processes.iter().map(|p| p.swap_disk).sum();
        self.total_compressed = self.processes.iter().map(|p| p.compressed).sum();
        self.total_footprint = self.processes.iter().map(|p| p.physical_footprint).sum();
        self.thread_count = self.processes.iter().map(|p| p.threads).sum();
    }
}

/// Normalise a process name into a canonical "app" name.
/// Maps things like:
///   "Google Chrome Helper (Renderer)" → "Google Chrome"
///   "Google Chrome Helper" → "Google Chrome"
///   "Google Chrome" → "Google Chrome"
///   "Slack Helper" → "Slack"
///   "Electron" + cmdline containing slack → "Slack"
fn canonical_app_name(name: &str, cmdline: &str) -> String {
    let name_lower = name.to_lowercase();

    // Well-known multi-process apps
    if name_lower.starts_with("google chrome") || name_lower.contains("chrome helper") {
        return "Google Chrome".into();
    }
    if name_lower.starts_with("firefox") || name_lower.contains("firefox") {
        return "Firefox".into();
    }
    if name_lower.starts_with("safari") || name_lower.contains("safariweb") {
        return "Safari".into();
    }
    if name_lower.starts_with("arc") || name_lower.contains("arc helper") {
        return "Arc".into();
    }
    if name_lower.starts_with("microsoft edge") || name_lower.contains("edge helper") {
        return "Microsoft Edge".into();
    }
    if name_lower.starts_with("brave") || name_lower.contains("brave helper") {
        return "Brave".into();
    }

    // Electron apps: "X Helper (Renderer)" → "X"
    if name_lower.ends_with(" helper (renderer)") || name_lower.ends_with(" helper (gpu)") || name_lower.ends_with(" helper (plugin)") {
        if let Some(app) = name.strip_suffix(" Helper (Renderer)")
            .or_else(|| name.strip_suffix(" Helper (GPU)"))
            .or_else(|| name.strip_suffix(" Helper (Plugin)"))
        {
            return app.to_string();
        }
    }
    if name_lower.ends_with(" helper") {
        if let Some(app) = name.strip_suffix(" Helper") {
            return app.to_string();
        }
    }

    // JetBrains IDEs via launcher
    if name_lower == "java" || name_lower == "launcher" {
        if cmdline.contains("idea") { return "IntelliJ IDEA".into(); }
        if cmdline.contains("webstorm") { return "WebStorm".into(); }
        if cmdline.contains("clion") { return "CLion".into(); }
        if cmdline.contains("pycharm") { return "PyCharm".into(); }
        if cmdline.contains("goland") { return "GoLand".into(); }
        if cmdline.contains("rustrover") { return "RustRover".into(); }
        if cmdline.contains("dataspell") { return "DataSpell".into(); }
        if cmdline.contains("jetbrains") {
            // Try to extract the product name from the path
            for ide in &["idea", "webstorm", "clion", "pycharm", "goland", "rustrover", "dataspell", "fleet"] {
                if cmdline.contains(ide) {
                    return ide_to_name(ide);
                }
            }
        }
    }

    // VS Code / Cursor / Windsurf
    if name_lower.contains("code helper") || name_lower == "electron" && cmdline.contains("vscode") {
        return "VS Code".into();
    }
    if name_lower.contains("cursor helper") || name_lower == "electron" && cmdline.contains("cursor") {
        return "Cursor".into();
    }
    if name_lower.contains("windsurf helper") || name_lower == "electron" && cmdline.contains("windsurf") {
        return "Windsurf".into();
    }

    // Docker
    if name_lower.starts_with("docker") || name_lower.contains("com.docker") {
        return "Docker".into();
    }

    // Slack
    if name_lower.contains("slack") {
        return "Slack".into();
    }

    // Discord
    if name_lower.contains("discord") {
        return "Discord".into();
    }

    // Spotify
    if name_lower.contains("spotify") {
        return "Spotify".into();
    }

    // Zoom
    if name_lower.contains("zoom") {
        return "Zoom".into();
    }

    // Telegram
    if name_lower.contains("telegram") {
        return "Telegram".into();
    }

    // Node / bun / deno
    if name_lower == "node" || name_lower == "bun" || name_lower == "deno" {
        // Try to identify the project
        if !cmdline.is_empty() {
            if let Some(proj) = extract_project_from_cmdline(cmdline) {
                return format!("{} ({})", name, proj);
            }
        }
        return name.to_string();
    }

    // Python
    if name_lower == "python" || name_lower.starts_with("python3") || name_lower == "python3" {
        if !cmdline.is_empty() {
            if let Some(proj) = extract_project_from_cmdline(cmdline) {
                return format!("Python ({})", proj);
            }
        }
        return "Python".into();
    }

    name.to_string()
}

fn ide_to_name(ide: &str) -> String {
    match ide {
        "idea" => "IntelliJ IDEA".into(),
        "webstorm" => "WebStorm".into(),
        "clion" => "CLion".into(),
        "pycharm" => "PyCharm".into(),
        "goland" => "GoLand".into(),
        "rustrover" => "RustRover".into(),
        "dataspell" => "DataSpell".into(),
        "fleet" => "Fleet".into(),
        other => other.to_string(),
    }
}

fn extract_project_from_cmdline(cmdline: &str) -> Option<String> {
    // Look for the last path-like segment that looks like a project
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
            procs.sort_by(|a, b| b.physical_footprint.cmp(&a.physical_footprint));
            let mut g = AppGroup {
                name,
                processes: procs,
                total_rss: 0,
                total_swap: 0,
                total_compressed: 0,
                total_footprint: 0,
                thread_count: 0,
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

    #[test]
    fn test_chrome_grouping() {
        let processes = vec![
            ProcessMemory::new_simple(1, "Google Chrome".into(), 100),
            ProcessMemory::new_simple(2, "Google Chrome Helper (Renderer)".into(), 200),
            ProcessMemory::new_simple(3, "Google Chrome Helper (GPU)".into(), 50),
            ProcessMemory::new_simple(4, "Google Chrome Helper".into(), 30),
        ];
        let groups = group_processes(&processes);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Google Chrome");
        assert_eq!(groups[0].processes.len(), 4);
        assert_eq!(groups[0].total_rss, 380);
    }

    #[test]
    fn test_electron_helper_grouping() {
        let processes = vec![
            ProcessMemory::new_simple(1, "Slack".into(), 100),
            ProcessMemory::new_simple(2, "Slack Helper (Renderer)".into(), 200),
            ProcessMemory::new_simple(3, "Slack Helper".into(), 30),
        ];
        let groups = group_processes(&processes);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Slack");
    }

    #[test]
    fn test_mixed_apps() {
        let processes = vec![
            ProcessMemory::new_simple(1, "Google Chrome".into(), 500),
            ProcessMemory::new_simple(2, "Slack".into(), 200),
            ProcessMemory::new_simple(3, "Google Chrome Helper (Renderer)".into(), 300),
        ];
        let groups = group_processes(&processes);
        assert_eq!(groups.len(), 2);
        // Chrome should be first (800 total > 200)
        assert_eq!(groups[0].name, "Google Chrome");
        assert_eq!(groups[1].name, "Slack");
    }
}
