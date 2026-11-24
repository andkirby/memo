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
struct FootprintCategory {
    // Apple's JSON key covers both compressed and swapped pages.
    #[serde(rename = "swapped")]
    compressed_or_swapped: u64,
}

/// Run `footprint` on a batch of PIDs and return compressed or swapped bytes by PID.
pub fn get_compressed_or_swapped_for_pids(pids: &[i32]) -> Result<HashMap<i32, u64>> {
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

    parse_compressed_or_swapped(&content)
}

pub(crate) fn parse_compressed_or_swapped(content: &str) -> Result<HashMap<i32, u64>> {
    let output: FootprintOutput =
        serde_json::from_str(content).context("Failed to parse footprint JSON")?;

    Ok(output.processes.into_iter().map(|p| {
        let bytes = p.categories.values().map(|c| c.compressed_or_swapped).sum();
        (p.pid, bytes)
    }).collect())
}
