//! Wrapper for Apple's `footprint` tool.
//!
//! One invocation measures one set of PIDs. Within that set Apple counts
//! memory shared by several processes once (see `man footprint`), so a set
//! total can be smaller than the sum of its members. Memo therefore never
//! adds per-process values to get a group value: it measures each process
//! alone and each multi-process group as its own set.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::fs;
use std::process::{Command, Stdio};
use uuid::Uuid;

/// Memory of one measured PID set, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    /// Apple's `total footprint`: dirty memory of the set, shared memory
    /// counted once. Includes the compressed or swapped part below.
    pub footprint: u64,
    /// `summary.total.swapped`: the part of `footprint` held in the
    /// compressor or in disk swap. Apple's JSON key covers both.
    pub compressed_or_swapped: u64,
}

/// Result of one `footprint` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetMeasurement {
    /// PIDs present in the output. `footprint` exits 0 and leaves `errors`
    /// empty when it cannot read some requested PIDs (it only reports them on
    /// stderr), so callers compare this list with the request.
    pub pids: Vec<i32>,
    pub memory: Memory,
}

#[derive(Debug, Deserialize)]
struct FootprintOutput {
    #[serde(rename = "bytes per unit", default = "one")]
    bytes_per_unit: u64,
    #[serde(rename = "total footprint")]
    total_footprint: u64,
    summary: Summary,
    processes: Vec<FootprintProcess>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

fn one() -> u64 { 1 }

#[derive(Debug, Deserialize)]
struct Summary {
    total: SummaryTotal,
}

#[derive(Debug, Deserialize)]
struct SummaryTotal {
    swapped: u64,
}

#[derive(Debug, Deserialize)]
struct FootprintProcess {
    pid: i32,
}

/// Measure `pids` as one set with a single `footprint` call.
///
/// PIDs are passed with `--pid`: a bare number or `-p` is also matched as a
/// partial process name, which can select unrelated processes.
pub fn measure_set(pids: &[i32]) -> Result<SetMeasurement> {
    if pids.is_empty() {
        bail!("no PIDs to measure");
    }
    let tmp_path = std::env::temp_dir().join(format!("memo_footprint_{}.json", Uuid::new_v4()));

    let mut cmd = Command::new("footprint");
    cmd.args(["-f", "bytes", "-j"]).arg(&tmp_path);
    for pid in pids {
        cmd.arg("--pid").arg(pid.to_string());
    }

    let status = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("Failed to execute footprint")?;

    let content = fs::read_to_string(&tmp_path);
    let _ = fs::remove_file(&tmp_path);
    if !status.success() {
        bail!("footprint exited with {status}");
    }
    let content = content.context("Failed to read footprint output")?;
    let measurement = parse_set(&content)?;
    if let Some(pid) = measurement.pids.iter().find(|pid| !pids.contains(pid)) {
        bail!("footprint returned unrequested PID {pid}");
    }
    Ok(measurement)
}

pub(crate) fn parse_set(content: &str) -> Result<SetMeasurement> {
    let output: FootprintOutput =
        serde_json::from_str(content).context("Failed to parse footprint JSON")?;
    if !output.errors.is_empty() {
        bail!("footprint reported errors: {:?}", output.errors);
    }
    let unit = output.bytes_per_unit.max(1);
    let mut pids: Vec<i32> = output.processes.iter().map(|p| p.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    Ok(SetMeasurement {
        pids,
        memory: Memory {
            footprint: output.total_footprint.saturating_mul(unit),
            compressed_or_swapped: output.summary.total.swapped.saturating_mul(unit),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_totals_come_from_the_deduplicated_summary() {
        // Shape of a two-process capture. The per-process categories hold only
        // memory private to each process; the shared region appears once in
        // the summary. Adding categories would miss it, and adding the
        // per-process `footprint` ledgers would count it twice.
        let m = parse_set(r#"{
            "unit": "byte", "bytes per unit": 1,
            "total footprint": 50, "errors": [], "warnings": [],
            "summary": {"total": {"dirty": 50, "swapped": 30}},
            "shared": [{"pids": [7, 8], "categories": {}}],
            "processes": [
                {"pid": 8, "footprint": 40, "categories": {"a": {"dirty": 5, "swapped": 2}}},
                {"pid": 7, "footprint": 45, "categories": {"a": {"dirty": 10, "swapped": 3}}}
            ]
        }"#).unwrap();
        assert_eq!(m.pids, vec![7, 8]);
        assert_eq!(m.memory, Memory { footprint: 50, compressed_or_swapped: 30 });
    }

    #[test]
    fn units_scale_and_reported_errors_fail() {
        let m = parse_set(r#"{"bytes per unit": 16384, "total footprint": 2,
            "summary": {"total": {"swapped": 1}}, "processes": [{"pid": 1}]}"#).unwrap();
        assert_eq!(m.memory, Memory { footprint: 32768, compressed_or_swapped: 16384 });

        assert!(parse_set(r#"{"total footprint": 2, "errors": ["x"],
            "summary": {"total": {"swapped": 1}}, "processes": []}"#).is_err());
        assert!(parse_set("{}").is_err());
    }

    /// Real `footprint`: two child processes share one anonymous mapping.
    /// Each measured alone carries the whole region; measured together the
    /// region counts once. The parent unmaps it, so only the children hold it.
    #[cfg(target_os = "macos")]
    #[test]
    fn real_footprint_counts_a_shared_mapping_once_per_set() {
        const SHARED: usize = 64 * 1024 * 1024;
        const MIB: u64 = 1024 * 1024;
        unsafe {
            let region = libc::mmap(std::ptr::null_mut(), SHARED, libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANON, -1, 0);
            assert_ne!(region, libc::MAP_FAILED);
            let (mut ready, mut release) = ([0; 2], [0; 2]);
            assert_eq!(libc::pipe(ready.as_mut_ptr()), 0);
            assert_eq!(libc::pipe(release.as_mut_ptr()), 0);
            let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
            let mut children = Vec::new();
            for writer in [true, false] {
                let pid = libc::fork();
                assert!(pid >= 0);
                if pid == 0 {
                    // Child: async-signal-safe calls only. Drop inherited write
                    // ends so the parent's close (or exit) releases the child;
                    // the alarm bounds a stuck run.
                    libc::alarm(60);
                    libc::close(release[1]);
                    libc::close(ready[0]);
                    let bytes = region as *mut u8;
                    let mut sum = 0u8;
                    for offset in (0..SHARED).step_by(page) {
                        if writer {
                            *bytes.add(offset) = (offset / page) as u8 | 1;
                        } else {
                            sum = sum.wrapping_add(std::ptr::read_volatile(bytes.add(offset)));
                        }
                    }
                    let token = [sum];
                    libc::write(ready[1], token.as_ptr().cast(), 1);
                    libc::read(release[0], [0u8].as_mut_ptr().cast(), 1);
                    libc::_exit(0);
                }
                // The reader starts after the writer has dirtied every page.
                libc::read(ready[0], [0u8].as_mut_ptr().cast(), 1);
                children.push(pid);
            }
            libc::munmap(region, SHARED);

            let alone: Vec<Memory> = children.iter()
                .map(|pid| measure_set(&[*pid]).expect("footprint reads own child").memory)
                .collect();
            let both = measure_set(&children).expect("footprint reads both children");

            libc::close(release[1]);
            for pid in &children {
                libc::waitpid(*pid, std::ptr::null_mut(), 0);
            }

            assert_eq!(both.pids, crate::deep::group_key(children.iter().copied()));
            let sum = alone[0].footprint + alone[1].footprint;
            for m in &alone {
                assert!(m.footprint >= SHARED as u64, "each child alone holds the region: {alone:?}");
            }
            assert!(sum - both.memory.footprint >= SHARED as u64 - 8 * MIB,
                "the pair counts the region once: alone {alone:?}, both {:?}", both.memory);
            assert!(both.memory.footprint >= alone[0].footprint.max(alone[1].footprint) - 8 * MIB,
                "a set is at least its largest member: alone {alone:?}, both {:?}", both.memory);
        }
    }
}
