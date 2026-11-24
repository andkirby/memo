use super::{App, ProcessMemory, handle_termination_key, ui};
use crate::group::GroupMode;
use crate::termination::ProcessIdentity;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::io;

const TARGET_PID: i32 = 700_041;
const OTHER_PID: i32 = 700_043;
fn process(pid: i32, name: &str, rss: u64) -> ProcessMemory {
    let mut process = ProcessMemory::new(pid, name.into(), rss);
    process.termination_identity = Some(ProcessIdentity::fixture(pid, 1));
    process
}

fn fixture() -> (App, Terminal<TestBackend>) {
    let mut app = App::new();
    app.group_mode = GroupMode::App;
    app.set_processes(vec![
        process(TARGET_PID, "Worker", 500),
        process(OTHER_PID, "Worker", 400),
        process(700_044, "Background", 50),
    ]);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| ui::ui(f, &mut app)).unwrap();
    (app, terminal)
}

fn press_without_signal(app: &mut App, key: impl Into<KeyEvent>) -> bool {
    handle_termination_key(app, key.into(), |_| panic!("unexpected signal"))
}

fn open_confirmation(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    assert!(press_without_signal(app, KeyCode::Char('x')));
    terminal.draw(|f| ui::ui(f, app)).unwrap();
    app.next_group();
    assert_eq!(
        app.selected_process(),
        Some(("Worker".into(), Some(TARGET_PID)))
    );
    assert!(press_without_signal(app, KeyCode::Char('x')));
    terminal.draw(|f| ui::ui(f, app)).unwrap();
    let target = app.pending_termination.as_ref().unwrap();
    assert_eq!(target.name, "Worker");
    assert_eq!(target.pid(), TARGET_PID);
}

fn screen_text(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

#[test]
fn group_termination_key_expands_without_opening_a_confirmation() {
    let (mut app, mut terminal) = fixture();
    assert!(press_without_signal(&mut app, KeyCode::Char('x')));
    assert_eq!(app.expanded_group.as_deref(), Some("Worker"));
    terminal.draw(|f| ui::ui(f, &mut app)).unwrap();

    assert!(press_without_signal(&mut app, KeyCode::Char('x')));
    assert_eq!(app.expanded_group.as_deref(), Some("Worker"));
    assert!(app.pending_termination.is_none());
    assert!(!press_without_signal(&mut app, KeyCode::Char('y')));
}

#[test]
fn cancel_closes_the_displayed_confirmation_without_signaling() {
    for cancel in [KeyCode::Esc, KeyCode::Char('n'), KeyCode::Char('q')] {
        let (mut app, mut terminal) = fixture();
        open_confirmation(&mut app, &mut terminal);
        let screen = screen_text(&terminal);
        assert!(screen.contains("Send SIGKILL?"));
        assert!(screen.contains(&format!("PID {TARGET_PID}")));
        assert!(screen.contains("y confirm"));

        assert!(press_without_signal(&mut app, cancel));
        assert!(app.pending_termination.is_none());
        assert!(!press_without_signal(&mut app, KeyCode::Char('y')));
    }
}

#[test]
fn confirm_signals_only_the_original_pid_after_refresh_and_reorder() {
    let (mut app, mut terminal) = fixture();
    open_confirmation(&mut app, &mut terminal);

    let control_y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert!(press_without_signal(&mut app, control_y));
    assert!(app.pending_termination.is_some());

    app.set_processes(vec![
        ProcessMemory {
            termination_identity: Some(ProcessIdentity::fixture(TARGET_PID, 2)),
            ..process(TARGET_PID, "Worker", 30)
        },
        process(OTHER_PID, "Worker", 800),
        process(700_044, "Background", 900),
    ]);
    terminal.draw(|f| ui::ui(f, &mut app)).unwrap();
    app.prev_group();
    assert_eq!(
        app.selected_process(),
        Some(("Worker".into(), Some(OTHER_PID)))
    );
    assert!(screen_text(&terminal).contains(&format!("PID {TARGET_PID}")));

    let mut signaled = Vec::new();
    assert!(handle_termination_key(
        &mut app,
        KeyCode::Char('y').into(),
        |identity| {
            signaled.push(identity);
            Ok(())
        },
    ));
    assert_eq!(signaled, [ProcessIdentity::fixture(TARGET_PID, 1)]);
    assert!(app.pending_termination.is_none());
    assert!(!press_without_signal(&mut app, KeyCode::Char('y')));
}

#[test]
fn exited_or_reused_pid_is_rejected_by_the_identity_bound_sender() {
    for live in [None, Some(ProcessIdentity::fixture(TARGET_PID, 2))] {
        let (mut app, mut terminal) = fixture();
        open_confirmation(&mut app, &mut terminal);
        let mut delivered = Vec::new();
        assert!(handle_termination_key(
            &mut app,
            KeyCode::Char('y').into(),
            |identity| {
                assert_eq!(identity, ProcessIdentity::fixture(TARGET_PID, 1));
                if live == Some(identity) {
                    delivered.push(identity.pid());
                    Ok(())
                } else {
                    Err(io::Error::from_raw_os_error(libc::ESRCH))
                }
            },
        ));
        assert!(delivered.is_empty());
        assert!(app.pending_termination.is_none());
        assert!(
            app.termination_status
                .unwrap()
                .contains("exited or changed identity")
        );
    }
}

#[test]
fn signaling_failure_stays_visible_when_the_scan_finishes() {
    let (mut app, mut terminal) = fixture();
    open_confirmation(&mut app, &mut terminal);
    assert!(handle_termination_key(
        &mut app,
        KeyCode::Char('y').into(),
        |identity| {
            assert_eq!(identity, ProcessIdentity::fixture(TARGET_PID, 1));
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "permission denied",
            ))
        },
    ));
    app.status_message = Some("Ready".into());
    terminal.draw(|f| ui::ui(f, &mut app)).unwrap();
    let screen = screen_text(&terminal);
    assert!(screen.contains(&format!("SIGKILL to PID {TARGET_PID} failed:")));
    assert!(screen.contains("permission denied"));
}

#[test]
fn unverified_process_never_opens_a_confirmation() {
    let (mut app, mut terminal) = fixture();
    let mut processes = app.all_processes.clone();
    processes[0].termination_identity = None;
    app.set_processes(processes);
    assert!(press_without_signal(&mut app, KeyCode::Char('x')));
    terminal.draw(|f| ui::ui(f, &mut app)).unwrap();
    app.next_group();
    assert!(press_without_signal(&mut app, KeyCode::Char('x')));
    assert!(app.pending_termination.is_none());
    assert!(
        app.termination_status
            .unwrap()
            .contains("Cannot verify PID")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn native_scan_binds_the_current_process() {
    let pid = std::process::id() as i32;
    let identity = crate::termination::process_identity(pid).unwrap();
    let processes = crate::collect_processes(true);
    assert_eq!(
        processes
            .iter()
            .find(|process| process.pid == pid)
            .unwrap()
            .termination_identity,
        Some(identity),
    );
}
