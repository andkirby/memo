use anyhow::{Context, Result};
use std::collections::HashMap;
use std::process::Command;

pub fn get_all_processes_compressed() -> Result<HashMap<i32, u64>> {
    let output = Command::new("top")
        .arg("-l")
        .arg("1")
        .arg("-stats")
        .arg("pid,cmprs")
        .output()
        .context("Failed to execute top")?;

    if !output.status.success() {
        return Err(anyhow::anyhow!("top command failed"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_top_output(&stdout)
}

fn parse_top_output(output: &str) -> Result<HashMap<i32, u64>> {
    let mut map = HashMap::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        if let Ok(pid) = parts[0].parse::<i32>() {
            let compressed = parse_size(parts[1]);
            map.insert(pid, compressed);
        }
    }
    Ok(map)
}

fn parse_size(size_str: &str) -> u64 {
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
