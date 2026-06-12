use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::process::{Command, Stdio};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
struct FootprintOutput {
    processes: Vec<FootprintProcess>,
}

#[derive(Debug, Deserialize)]
struct FootprintProcess {
    pid: i32,
    #[allow(dead_code)]
    name: String,
    #[serde(default)]
    footprint: u64,
    auxiliary: Option<FootprintAuxiliary>,
    #[serde(default)]
    categories: HashMap<String, FootprintCategory>,
}

#[derive(Debug, Deserialize)]
struct FootprintAuxiliary { #[allow(dead_code)] phys_footprint: u64 }

#[derive(Debug, Deserialize)]
struct FootprintCategory { #[allow(dead_code)] swapped: u64 }

/// Swap data extracted from `footprint` for a single process.
#[derive(Debug, Clone)]
pub struct SwapData {
    pub swapped_total: u64,
}

/// Run `footprint` on a batch of PIDs and extract per-process swap data.
pub fn get_swap_for_pids(pids: &[i32]) -> Result<HashMap<i32, SwapData>> {
    if pids.is_empty() {
        return Ok(HashMap::new());
    }

    let tmp_path = std::env::temp_dir().join(format!("memo_footprint_{}.json", Uuid::new_v4()));

    let mut cmd = Command::new("footprint");
    cmd.arg("-j").arg(&tmp_path);
    for pid in pids {
        cmd.arg(pid.to_string());
    }

    let status = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("Failed to execute footprint")?;

    if !status.success() {
        let _ = fs::remove_file(&tmp_path);
        return Err(anyhow::anyhow!("footprint command failed"));
    }

    let content = fs::read_to_string(&tmp_path).context("Failed to read footprint output")?;
    let _ = fs::remove_file(&tmp_path);

    let output: FootprintOutput =
        serde_json::from_str(&content).context("Failed to parse footprint JSON")?;

    let mut map = HashMap::new();
    for p in output.processes {
        let swapped = p.categories.values().map(|c| c.swapped).sum();
        map.insert(p.pid, SwapData { swapped_total: swapped });
    }

    Ok(map)
}
