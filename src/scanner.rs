use std::process::Command;
use std::sync::OnceLock;
use regex::Regex;

#[derive(Debug, Clone)]
pub struct ProcessMemory {
    pub pid: i32,
    pub name: String,
    pub cmdline: String,
    pub physical_footprint: u64,
    pub compressed: u64,
    pub swapped_total: u64,
    pub swap_disk_est: u64,
    pub swap_disk: u64,
    pub threads: usize,
    pub oom_score: i32,
}

impl ProcessMemory {
    pub fn total(&self) -> u64 {
        self.physical_footprint + self.swap_disk
    }

    pub fn new_simple(pid: i32, name: String, rss: u64) -> Self {
        Self {
            pid,
            name,
            cmdline: String::new(),
            physical_footprint: rss,
            compressed: 0,
            swapped_total: 0,
            swap_disk_est: 0,
            swap_disk: 0,
            threads: 1,
            oom_score: 0,
        }
    }

    /// Merges precise swap data from footprint into this struct.
    ///
    /// We do NOT use footprint's phys_footprint for the "physical" column because it
    /// includes GPU backing stores (IOSurface, tag 14/16) and clean __TEXT pages that are
    /// SHARED across processes. Summing these across grouped processes produces impossible
    /// numbers (e.g. 35 GB for Opera on a 16 GB machine). We keep sysinfo's RSS instead,
    /// which matches Activity Monitor and only counts pages resident in RAM.
    ///
    /// Footprint is used exclusively for swap accounting — the one thing it provides
    /// that sysinfo cannot.
    pub fn merge_footprint(&mut self, fp: &crate::footprint::FootprintData, compressed: u64) {
        // Keep sysinfo RSS for physical — DO NOT overwrite with fp.physical_footprint
        self.swapped_total = fp.swapped_total;
        self.compressed = compressed;
        self.swap_disk_est = self.swapped_total.saturating_sub(self.compressed);
        self.swap_disk = self.swap_disk_est;
    }
}

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

#[allow(dead_code)]
pub fn get_process_memory(pid: i32, name: &str) -> anyhow::Result<ProcessMemory> {
    let output = Command::new("vmmap")
        .arg("-summary")
        .arg(pid.to_string())
        .output()?;

    if !output.status.success() {
        return Ok(ProcessMemory::new_simple(pid, name.to_string(), 0));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_vmmap_output(pid, name, &stdout)
}

fn parse_vmmap_output(pid: i32, name: &str, output: &str) -> anyhow::Result<ProcessMemory> {
    static RE_PHYSICAL: OnceLock<Regex> = OnceLock::new();
    static RE_COMPRESSED: OnceLock<Regex> = OnceLock::new();
    static RE_SWAP_USED: OnceLock<Regex> = OnceLock::new();
    static RE_WRITABLE: OnceLock<Regex> = OnceLock::new();
    static RE_TOTAL_TABLE: OnceLock<Regex> = OnceLock::new();

    let re_physical =
        RE_PHYSICAL.get_or_init(|| Regex::new(r"Physical footprint:\s+([\d\.]+)([KMG]?)").unwrap());
    let re_compressed =
        RE_COMPRESSED.get_or_init(|| Regex::new(r"Compressed:\s+([\d\.]+)([KMG]?)").unwrap());
    let re_swap_used =
        RE_SWAP_USED.get_or_init(|| Regex::new(r"Swap used:\s+([\d\.]+)([KMG]?)").unwrap());
    let re_writable = RE_WRITABLE
        .get_or_init(|| Regex::new(r"swapped_out=([\d\.]+)([KMG]?)").unwrap());
    let re_total_table = RE_TOTAL_TABLE
        .get_or_init(|| Regex::new(r"^TOTAL\s+(\S+)\s+(\S+)\s+(\S+)\s+(\S+)").unwrap());

    let mut phys = 0u64;
    let mut compressed = 0u64;
    let mut swap_used = 0u64;
    let mut resident_from_table = 0u64;
    let mut swap_from_table = 0u64;
    let mut writable_swapped_out = 0u64;
    let mut found_total_table = false;
    let mut in_region_type_table = false;

    for line in output.lines() {
        let line = line.trim();
        if let Some(caps) = re_physical.captures(line) {
            phys = parse_size(&format!("{}{}", &caps[1], &caps[2]));
        }
        if let Some(caps) = re_compressed.captures(line) {
            compressed = parse_size(&format!("{}{}", &caps[1], &caps[2]));
        }
        if let Some(caps) = re_swap_used.captures(line) {
            swap_used = parse_size(&format!("{}{}", &caps[1], &caps[2]));
        }
        if let Some(caps) = re_writable.captures(line) {
            writable_swapped_out = parse_size(&format!("{}{}", &caps[1], &caps[2]));
        }
        if line.starts_with("REGION TYPE") || (line.contains("VIRTUAL") && line.contains("RESIDENT")) {
            in_region_type_table = true;
        }
        if in_region_type_table {
            if let Some(caps) = re_total_table.captures(line) {
                resident_from_table = parse_size(&caps[2]);
                swap_from_table = parse_size(&caps[4]);
                found_total_table = true;
            }
        }
    }

    let swapped_total = if swap_from_table > 0 {
        swap_from_table
    } else if writable_swapped_out > 0 {
        writable_swapped_out
    } else if phys > 0 && resident_from_table > 0 {
        phys.saturating_sub(resident_from_table)
    } else {
        0
    };

    let disk_from_swapped = swapped_total.saturating_sub(compressed);
    let swap_disk_est = if swapped_total > 0 {
        if swap_used > 0 { disk_from_swapped.min(swap_used) } else { disk_from_swapped }
    } else if swap_used > 0 {
        swap_used
    } else {
        0
    };

    Ok(ProcessMemory {
        pid,
        name: name.to_string(),
        cmdline: String::new(),
        physical_footprint: phys,
        compressed,
        swapped_total,
        swap_disk_est,
        swap_disk: swap_disk_est,
        threads: 1,
        oom_score: 0,
    })
}

// --- System memory info ---

pub struct SystemMemory {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub compressed: u64,
    pub wired: u64,
    pub app_memory: u64,
    pub cache: u64,
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

    let page_size = 4096u64;
    let free = vm_stats.free_count as u64 * page_size;
    let active = vm_stats.active_count as u64 * page_size;
    let inactive = vm_stats.inactive_count as u64 * page_size;
    let wired = vm_stats.wire_count as u64 * page_size;
    let compressed = vm_stats.compressor_page_count as u64 * page_size;
    let purgeable = vm_stats.purgeable_count as u64 * page_size;
    let speculative = vm_stats.speculative_count as u64 * page_size;

    // macOS "used" = wired + active + compressed + speculative (internal)
    // "app memory" ≈ active + speculative - purgeable
    let app_memory = active.saturating_add(speculative).saturating_sub(purgeable);
    let cache = inactive;
    let used = total.saturating_sub(free).saturating_sub(purgeable);

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

fn get_total_ram() -> u64 {
    let output = Command::new("sysctl").arg("-n").arg("hw.memsize").output();
    match output {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            s.parse::<u64>().unwrap_or(0)
        }
        _ => 0,
    }
}

struct VmStats {
    free_count: u64,
    active_count: u64,
    inactive_count: u64,
    wire_count: u64,
    compressor_page_count: u64,
    purgeable_count: u64,
    speculative_count: u64,
}

fn get_vm_stats() -> VmStats {
    let output = Command::new("vm_stat").output();
    let mut stats = VmStats {
        free_count: 0, active_count: 0, inactive_count: 0,
        wire_count: 0, compressor_page_count: 0,
        purgeable_count: 0, speculative_count: 0,
    };

    if let Ok(o) = output {
        if o.status.success() {
            let text = String::from_utf8_lossy(&o.stdout);
            for line in text.lines() {
                let line = line.trim();
                if let Some(val) = parse_vm_stat_line(line, "Pages free:") {
                    stats.free_count = val;
                } else if let Some(val) = parse_vm_stat_line(line, "Pages active:") {
                    stats.active_count = val;
                } else if let Some(val) = parse_vm_stat_line(line, "Pages inactive:") {
                    stats.inactive_count = val;
                } else if let Some(val) = parse_vm_stat_line(line, "Pages wired down:") {
                    stats.wire_count = val;
                } else if let Some(val) = parse_vm_stat_line(line, "Pages speculative:") {
                    stats.speculative_count = val;
                } else if let Some(val) = parse_vm_stat_line(line, "Compressor pages:") {
                    stats.compressor_page_count = val;
                } else if let Some(val) = parse_vm_stat_line(line, "Pages stored in compressor:") {
                    stats.compressor_page_count = val;
                } else if let Some(val) = parse_vm_stat_line(line, "Purgeable pages:") {
                    stats.purgeable_count = val;
                } else if let Some(val) = parse_vm_stat_line(line, "Pages purgeable:") {
                    stats.purgeable_count = val;
                }
            }
        }
    }
    stats
}

fn parse_vm_stat_line(line: &str, prefix: &str) -> Option<u64> {
    if line.starts_with(prefix) {
        let num_part = line.trim_start_matches(prefix)
            .trim()
            .trim_end_matches('.')
            .replace(",", "");
        num_part.parse::<u64>().ok()
    } else {
        None
    }
}

struct SwapInfo {
    total: u64,
    used: u64,
}

fn get_swap_usage() -> SwapInfo {
    let output = Command::new("sysctl").arg("vm.swapusage").output();
    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let total = extract_swap_field(&text, "total = ");
            let used = extract_swap_field(&text, "used = ");
            SwapInfo { total, used }
        }
        _ => SwapInfo { total: 0, used: 0 },
    }
}

fn extract_swap_field(text: &str, prefix: &str) -> u64 {
    if let Some(idx) = text.find(prefix) {
        let rest = &text[idx + prefix.len()..];
        let val = rest.split_whitespace().next().unwrap_or("0");
        parse_size(val)
    } else {
        0
    }
}
