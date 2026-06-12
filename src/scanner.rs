use std::process::Command;

// ─── Process memory — single source of truth ──────────────────────────────
//
// Fields are named by WHERE the data comes from, not vague terms:
//   rss      — sysinfo: pages currently resident in RAM (Activity Monitor style)
//   swap     — footprint: pages swapped to disk (the one thing footprint is uniquely good at)
//   threads  — sysinfo: thread count
//
// "total" = rss + swap. That's what you'd free if you killed this process.

#[derive(Debug, Clone)]
pub struct ProcessMemory {
    pub pid: i32,
    pub name: String,
    pub cmdline: String,
    pub rss: u64,       // from sysinfo — honest per-process resident memory
    pub swap: u64,      // from footprint — pages swapped to disk
    pub threads: usize,
}

impl ProcessMemory {
    pub fn total(&self) -> u64 {
        self.rss.saturating_add(self.swap)
    }

    pub fn new(pid: i32, name: String, rss: u64) -> Self {
        Self {
            pid,
            name,
            cmdline: String::new(),
            rss,
            swap: 0,
            threads: 1,
        }
    }
}

// ─── Formatting ────────────────────────────────────────────────────────────

pub fn format_size(bytes: u64) -> String {
    const K: f64 = 1024.0;
    const M: f64 = 1024.0 * 1024.0;
    const G: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if bytes >= G as u64 {
        format!("{:.1}G", b / G)
    } else if bytes >= M as u64 {
        format!("{:.1}M", b / M)
    } else if bytes >= K as u64 {
        format!("{:.1}K", b / K)
    } else {
        format!("{}B", bytes)
    }
}

pub fn parse_size(size_str: &str) -> u64 {
    let size_str = size_str.trim().to_uppercase();
    let (num_str, multiplier) = if size_str.ends_with('G') {
        (size_str.trim_end_matches('G'), 1024 * 1024 * 1024)
    } else if size_str.ends_with('M') {
        (size_str.trim_end_matches('M'), 1024 * 1024)
    } else if size_str.ends_with('K') {
        (size_str.trim_end_matches('K'), 1024)
    } else {
        (size_str.trim_end_matches('B'), 1)
    };
    let num: f64 = num_str.parse().unwrap_or(0.0);
    (num * multiplier as f64) as u64
}

// ─── System memory — from vm_stat + sysctl ─────────────────────────────────

pub struct SystemMemory {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub compressed: u64,   // Pages occupied by compressor × page_size (physical)
    pub wired: u64,
    pub app_memory: u64,
    pub cache: u64,        // File-backed pages (≈ Activity Monitor "Cached Files")
}

impl SystemMemory {
    pub fn used_pct(&self) -> f64 {
        if self.total_bytes == 0 { return 0.0; }
        (self.used_bytes as f64 / self.total_bytes as f64) * 100.0
    }

    pub fn swap_pct(&self) -> f64 {
        if self.swap_total == 0 { return 0.0; }
        (self.swap_used as f64 / self.swap_total as f64) * 100.0
    }
}

pub fn get_system_memory() -> SystemMemory {
    let total = get_total_ram();
    let vm_stats = get_vm_stats();
    let swap = get_swap_usage();

    let page_size = get_page_size();
    let free = vm_stats.free_count as u64 * page_size;
    let active = vm_stats.active_count as u64 * page_size;
    let speculative = vm_stats.speculative_count as u64 * page_size;
    let wired = vm_stats.wire_count as u64 * page_size;
    let compressed = vm_stats.compressor_page_count as u64 * page_size;
    let purgeable = vm_stats.purgeable_count as u64 * page_size;

    let app_memory = active.saturating_add(speculative).saturating_sub(purgeable);
    let cache = vm_stats.file_backed_count as u64 * page_size;
    let used = total.saturating_sub(free).saturating_sub(cache).saturating_sub(purgeable);

    SystemMemory {
        total_bytes: total,
        used_bytes: used,
        free_bytes: free,
        swap_total: swap.total,
        swap_used: swap.used,
        compressed,
        wired,
        app_memory,
        cache,
    }
}

fn get_page_size() -> u64 {
    Command::new("sysctl").arg("-n").arg("hw.pagesize").output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(16384)
}

fn get_total_ram() -> u64 {
    Command::new("sysctl").arg("-n").arg("hw.memsize").output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0)
}

struct VmStats {
    free_count: u64,
    active_count: u64,
    wire_count: u64,
    compressor_page_count: u64,
    purgeable_count: u64,
    speculative_count: u64,
    file_backed_count: u64,
}

fn get_vm_stats() -> VmStats {
    let mut stats = VmStats {
        free_count: 0, active_count: 0, wire_count: 0,
        compressor_page_count: 0, purgeable_count: 0,
        speculative_count: 0, file_backed_count: 0,
    };

    let Ok(o) = Command::new("vm_stat").output() else { return stats };
    if !o.status.success() { return stats; }

    let text = String::from_utf8_lossy(&o.stdout);
    for line in text.lines() {
        let line = line.trim();
        parse_vm_stat(line, "Pages free:", &mut stats.free_count);
        parse_vm_stat(line, "Pages active:", &mut stats.active_count);
        parse_vm_stat(line, "Pages wired down:", &mut stats.wire_count);
        parse_vm_stat(line, "Pages speculative:", &mut stats.speculative_count);
        parse_vm_stat(line, "Pages occupied by compressor:", &mut stats.compressor_page_count);
        parse_vm_stat(line, "Pages purgeable:", &mut stats.purgeable_count);
        parse_vm_stat(line, "File-backed pages:", &mut stats.file_backed_count);
    }
    stats
}

fn parse_vm_stat(line: &str, prefix: &str, out: &mut u64) {
    if line.starts_with(prefix) {
        let num_part = line.trim_start_matches(prefix)
            .trim().trim_end_matches('.').replace(",", "");
        if let Ok(val) = num_part.parse::<u64>() {
            *out = val;
        }
    }
}

struct SwapInfo { total: u64, used: u64 }

fn get_swap_usage() -> SwapInfo {
    let Ok(o) = Command::new("sysctl").arg("vm.swapusage").output() else {
        return SwapInfo { total: 0, used: 0 };
    };
    let text = String::from_utf8_lossy(&o.stdout);
    SwapInfo {
        total: extract_swap_field(&text, "total = "),
        used: extract_swap_field(&text, "used = "),
    }
}

fn extract_swap_field(text: &str, prefix: &str) -> u64 {
    let Some(idx) = text.find(prefix) else { return 0 };
    let rest = &text[idx + prefix.len()..];
    let val = rest.split_whitespace().next().unwrap_or("0");
    parse_size(val)
}
