#![cfg(target_os = "macos")]

use std::{env, fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn quick_cli_skips_footprint_and_rejects_unavailable_sorts() {
    let fixture = env::temp_dir().join(format!("memo-quick-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&fixture).unwrap();
    let marker = fixture.join("footprint-called");
    let footprint = fixture.join("footprint");
    fs::write(
        &footprint,
        "#!/bin/sh\nprintf 'called\\n' >> \"$MEMO_FOOTPRINT_MARKER\"\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&footprint, fs::Permissions::from_mode(0o700)).unwrap();
    let path = env::join_paths(
        std::iter::once(fixture.clone()).chain(env::split_paths(&env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_memo"))
            .args(args)
            .env("PATH", &path)
            .env("MEMO_FOOTPRINT_MARKER", &marker)
            .output()
            .expect("run memo")
    };

    // Keep an eligible process resident even on a host with few running processes.
    let resident = vec![1u8; 16 * 1024 * 1024];
    let full = run(&["--cli"]);
    std::hint::black_box(&resident);
    assert!(full.status.success(), "{full:?}");
    assert!(
        marker.is_file(),
        "the full scan must reach the footprint sentinel"
    );
    fs::remove_file(&marker).unwrap();

    for flags in [
        &["--cli", "--quick"][..],
        &["--cli", "--quick", "--group", "project", "--sort", "name"],
        &["--cli", "--quick", "--ps", "--sort", "rss"],
    ] {
        let output = run(flags);
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("RSS-only process scan."));
        let header = stdout
            .lines()
            .find(|line| {
                line.starts_with("APP ") || line.starts_with("PROJECT ") || line.starts_with("PID ")
            })
            .expect("process table header");
        assert!(header.trim_end().ends_with("RSS"), "{header}");
        assert!(
            !marker.exists(),
            "quick scan invoked footprint for {flags:?}"
        );
    }

    for sort in ["total", "swap", "other"] {
        let output = run(&["--cli", "--quick", "--sort", sort]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty(), "the rejected scan printed output");
        assert!(String::from_utf8_lossy(&output.stderr).contains("Use rss or name."));
        assert!(!marker.exists(), "rejected sort invoked footprint");
    }
    assert_eq!(run(&["--quick"]).status.code(), Some(2));
    fs::remove_dir_all(fixture).unwrap();
}
