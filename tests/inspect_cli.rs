use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn json_result(args: &[&str], cwd: Option<&Path>, expected_code: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_memo"));
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command.output().expect("run memo");
    assert_eq!(output.status.code(), Some(expected_code), "{output:?}");
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    assert_eq!(stdout.trim().lines().count(), 1, "{stdout}");
    assert!(!stdout.contains('\u{1b}'));
    let report: Value = serde_json::from_str(stdout).expect("one JSON object on stdout");
    assert_eq!(report["version"], 1);
    report
}

#[test]
fn json_argument_errors_are_parseable_including_clap_failures() {
    for args in [
        vec!["inspect", "--json", "--limit", "0"],
        vec!["inspect", "--json", "--limit", "nope"],
        vec!["inspect", "--json", "--project"],
        vec!["inspect", "--json", "--unknown"],
        vec!["--cli", "inspect", "--json"],
        vec!["--sort", "total", "inspect", "--json"],
        vec!["--group", "app", "inspect", "--json"],
    ] {
        let error = json_result(&args, None, 2);
        assert_eq!(error["error"]["code"], "bad_argument", "{args:?}");
        assert!(
            error["error"]["message"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        );
    }
}

#[test]
fn explicit_empty_scope_is_usable_and_invalid_scope_is_structured() {
    let directory = std::env::temp_dir().join(format!("memo-cli-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let report = json_result(
        &["inspect", "--json", "--project", "."],
        Some(&directory),
        0,
    );
    assert_eq!(report["source"], "explicit");
    assert_eq!(
        report["scope"],
        directory.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(report["total_processes"], 0);
    assert_eq!(report["total_rss"], 0);
    assert_eq!(report["omitted"], 0);
    assert_eq!(report["top"], serde_json::json!([]));
    assert!(report["limitation"].as_str().is_some_and(|s| !s.is_empty()));
    let error = json_result(
        &["inspect", "--json", "--project", "missing"],
        Some(&directory),
        2,
    );
    assert_eq!(error["error"]["code"], "invalid_project");
    std::fs::remove_dir(directory).unwrap();
}
