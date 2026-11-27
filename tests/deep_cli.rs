#![cfg(target_os = "macos")]

//! End-to-end deep scan against a fake `footprint` on PATH. The fake applies
//! the shared-memory model Apple documents: a PID set's footprint counts a
//! shared region once, so a group is less than the sum of its members.

use std::{env, fs, os::unix::fs::PermissionsExt, process::Command, time::Duration};

const MIB: u64 = 1024 * 1024;

// One PID alone: 42 MiB (24 MiB compressed or swapped). A set of n PIDs:
// 10 MiB private each plus one 32 MiB shared region (4 MiB / 20 MiB).
// Like Apple's tool, unreadable PIDs are left out of the output with exit 0,
// and a call that reads nothing exits 66. MEMO_FAKE_HIDDEN is never readable;
// MEMO_FAKE_ALONE_FAILS fails only when measured alone.
const FAKE_FOOTPRINT: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$MEMO_FOOTPRINT_LOG"
out=""; requested=0; n=0; pids=""; alone=""
while [ $# -gt 0 ]; do
  case "$1" in
    -j) out="$2"; shift 2 ;;
    --pid) requested=$((requested + 1)); alone="$2"
      if [ "$2" != "${MEMO_FAKE_HIDDEN:-}" ]; then n=$((n + 1)); pids="$pids{\"pid\": $2},"; fi
      shift 2 ;;
    *) shift ;;
  esac
done
if [ "$requested" -eq 1 ] && [ "$alone" = "${MEMO_FAKE_ALONE_FAILS:-}" ]; then exit 66; fi
if [ "$n" -eq 0 ]; then exit 66; fi
M=1048576
if [ "$n" -eq 1 ]; then fp=$((42 * M)); sw=$((24 * M)); else fp=$(((10 * n + 32) * M)); sw=$(((4 * n + 20) * M)); fi
printf '{"unit":"byte","bytes per unit":1,"errors":[],"warnings":[],"total footprint":%s,"summary":{"total":{"dirty":%s,"swapped":%s}},"processes":[%s]}' \
  "$fp" "$fp" "$sw" "${pids%,}" > "$out"
"#;

fn size(bytes: u64) -> String {
    format!("{:.1}M", bytes as f64 / MIB as f64)
}

/// Child processes share this test binary's name, so App mode groups them
/// with the test process itself.
#[test]
#[ignore = "fixture process started by grouped_scan_measures_each_row_as_one_pid_set"]
fn fixture_sleeper() {
    if env::var_os("MEMO_FIXTURE_SLEEP").is_some() {
        std::thread::sleep(Duration::from_secs(120));
    }
}

#[test]
fn grouped_scan_measures_each_row_as_one_pid_set() {
    let fixture = env::temp_dir().join(format!("memo-deep-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&fixture).unwrap();
    let footprint = fixture.join("footprint");
    fs::write(&footprint, FAKE_FOOTPRINT).unwrap();
    fs::set_permissions(&footprint, fs::Permissions::from_mode(0o700)).unwrap();
    let log = fixture.join("calls.log");
    let path = env::join_paths(
        std::iter::once(fixture.clone()).chain(env::split_paths(&env::var_os("PATH").unwrap())),
    )
    .unwrap();

    let me = env::current_exe().unwrap();
    let mut children: Vec<_> = (0..2)
        .map(|_| {
            Command::new(&me)
                .args(["--ignored", "--exact", "fixture_sleeper", "--test-threads=1"])
                .env("MEMO_FIXTURE_SLEEP", "1")
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    std::thread::sleep(Duration::from_millis(300));
    let ours: Vec<i32> = std::iter::once(std::process::id() as i32)
        .chain(children.iter().map(|c| c.id() as i32))
        .collect();
    let name = me.file_name().unwrap().to_string_lossy().to_string();

    let run = |args: &[&str], fake_env: &[(&str, String)]| {
        let _ = fs::remove_file(&log);
        let output = Command::new(env!("CARGO_BIN_EXE_memo"))
            .args(args)
            .env("PATH", &path)
            .env("MEMO_FOOTPRINT_LOG", &log)
            .envs(fake_env.iter().map(|(k, v)| (k, v)))
            .output()
            .expect("run memo");
        assert!(output.status.success(), "{output:?}");
        (String::from_utf8(output.stdout).unwrap(), fs::read_to_string(&log).unwrap())
    };
    let our_row = |report: &str| -> Vec<String> {
        report
            .lines()
            .find(|line| {
                let first = line.split_whitespace().next().unwrap_or("");
                first.len() >= 8 && name.starts_with(first.trim_end_matches('…'))
            })
            .unwrap_or_else(|| panic!("no row for {name}:\n{report}"))
            .split_whitespace()
            .map(str::to_string)
            .collect()
    };
    let group_calls = |calls: &str| -> Vec<Vec<i32>> {
        calls
            .lines()
            .map(|call| call.split_whitespace().skip(4).filter_map(|a| a.parse().ok()).collect())
            .filter(|pids: &Vec<i32>| pids.len() > 1 && ours.iter().all(|p| pids.contains(p)))
            .collect()
    };

    let (grouped, calls) = run(&["--cli", "--group", "app"], &[]);
    let (ps, _) = run(&["--cli", "--ps"], &[]);
    // A child footprint cannot read alone but reads in the group call, and a
    // child footprint never reads.
    let child = children[0].id().to_string();
    let (alone_fails, alone_fails_calls) =
        run(&["--cli", "--group", "app"], &[("MEMO_FAKE_ALONE_FAILS", child.clone())]);
    let (hidden, hidden_calls) = run(&["--cli", "--group", "app"], &[("MEMO_FAKE_HIDDEN", child.clone())]);
    let (hidden_ps, _) = run(&["--cli", "--ps"], &[("MEMO_FAKE_HIDDEN", child.clone())]);
    for child in &mut children {
        let _ = child.kill();
        let _ = child.wait();
    }

    // Every call passes PIDs with --pid; a bare number is a name pattern.
    for call in calls.lines() {
        let args: Vec<_> = call.split_whitespace().collect();
        assert_eq!(&args[..3], ["-f", "bytes", "-j"], "{call}");
        assert!(args[4..].chunks(2).all(|pair| pair[0] == "--pid" && pair[1].parse::<i32>().is_ok()), "{call}");
    }

    // Our row: one call over exactly its PIDs, and the row shows that call.
    let row = our_row(&grouped);
    let members: u64 = row[1].parse().unwrap();
    assert!(members >= 3, "test process and two children: {row:?}");
    let complete = [size((4 * members + 20) * MIB), size((10 * members + 32) * MIB)];
    assert!(row.ends_with(&complete), "row shows the group call, not {members} × 42 MiB: {row:?}");
    let calls_for_row = group_calls(&calls);
    assert_eq!(calls_for_row.len(), 1, "one call for the row: {calls_for_row:?}");
    assert_eq!(calls_for_row[0].len() as u64, members, "the call holds exactly the row's PIDs");
    assert!(calls.lines().filter(|c| c.split_whitespace().count() == 6).count() > 0, "per-process calls");
    assert!(grouped.contains("RSS SUM"), "{grouped}");

    // Unreadable alone, readable in the group call: the row is complete and
    // the group call still held the child.
    let recovered = our_row(&alone_fails);
    assert_eq!(recovered[1], row[1], "{alone_fails}");
    assert!(recovered.ends_with(&complete), "complete, not ≥: {recovered:?}");
    assert!(group_calls(&alone_fails_calls).iter().any(|c| c.len() as u64 == members));

    // Never readable: the child stays in the row and in the group call, and
    // the row is a lower bound from the readable subset.
    let partial = our_row(&hidden);
    assert_eq!(partial[1], row[1], "the unreadable child is still a member: {hidden}");
    let lower = [
        format!("≥{}", size((4 * (members - 1) + 20) * MIB)),
        format!("≥{}", size((10 * (members - 1) + 32) * MIB)),
    ];
    assert!(partial.ends_with(&lower), "lower bound, not complete: {partial:?}");
    assert!(group_calls(&hidden_calls).iter().any(|c| c.len() as u64 == members), "{hidden_calls}");
    let hidden_line = hidden_ps
        .lines()
        .find(|line| line.split_whitespace().next() == Some(child.as_str()))
        .unwrap_or_else(|| panic!("unreadable child is listed in --ps:\n{hidden_ps}"));
    assert!(hidden_line.split_whitespace().rev().take(2).all(|c| c == "n/a"), "{hidden_line}");

    // Ungrouped rows are each process alone.
    for pid in &ours {
        let line = ps
            .lines()
            .find(|line| line.split_whitespace().next() == Some(&pid.to_string()))
            .unwrap_or_else(|| panic!("no --ps row for {pid}:\n{ps}"));
        let cols: Vec<_> = line.split_whitespace().collect();
        assert!(cols.ends_with(&[&size(24 * MIB), &size(42 * MIB)]), "{line}");
    }
    fs::remove_dir_all(fixture).unwrap();
}
