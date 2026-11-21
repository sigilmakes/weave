//! Headless e2e for the agents view's row-click grammar: a plain click on
//! a session row selects and opens it (the Enter action); a dragged
//! release never opens.
#![cfg(unix)]
// Casts: structurally bounded terminal-layout arithmetic; guarded conversions add panic paths.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
// Render routes are flat tables (one arm per route); splitting adds indirection.
#![allow(clippy::too_many_lines)]
// Widget state structs carry independent flag bits.
#![allow(clippy::struct_excessive_bools, clippy::fn_params_excessive_bools)]
// Futures are bounded by the surface's lifetime; boxing adds a steady-state allocation.
#![allow(clippy::large_futures)]
// The wrappers preserve a uniform Result-returning API surface.
#![allow(clippy::unnecessary_wraps)]

use pa_test_scratch as _; // a private TMPDIR for this test binary, deleted on exit
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use pa_tui::agents_view::{
    run_agents_view, AgentsHeadlessPlan, AgentsStep, AgentsViewOptions, AgentsViewUiMode,
};
use pa_tui::interactive::SessionSelection;
use serde_json::{json, Value};

/// Mouse tracking is process-global state, so the headless runs serialize through one lock (the
/// click dispatch gates on it; tokio's, so the guard rides the run's awaits).
static RUN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One roster row's wire summary (the mock `roster_subscribe` snapshot).
fn roster_row(id: &str, name: &str) -> Value {
    json!({
        "agentId": id,
        "status": "idle",
        "summary": {
            "sessionId": id,
            "lifecycle": "live",
            "activeSessionId": format!("{id}-live"),
            "sessionFile": format!("/tmp/{id}.jsonl"),
            "runtimeKind": "top-level",
            "sessionName": name,
            "messageCount": 2,
            "rlmDepth": 0,
        },
    })
}

struct MockSupervisor {
    listener: UnixListener,
    last_observed: std::sync::Arc<std::sync::Mutex<String>>,
    hold_catalog_response: bool,
}

impl MockSupervisor {
    fn bind(socket: &std::path::Path) -> Self {
        MockSupervisor {
            listener: UnixListener::bind(socket).expect("bind mock socket"),
            last_observed: std::sync::Arc::new(std::sync::Mutex::new("not connected".to_string())),
            hold_catalog_response: false,
        }
    }

    /// Serve the view connection: hello, then the command loop until EOF.
    fn serve(self, connected: &std::sync::mpsc::Sender<UnixStream>) {
        let (stream, _) = self.listener.accept().expect("accept view connection");
        *self.last_observed.lock().expect("mock observation") = "accepted connection".to_string();
        connected
            .send(stream.try_clone().expect("clone cancellation socket"))
            .expect("publish cancellation socket");
        let write_stream = stream.try_clone().expect("clone socket");
        let mut writer = write_stream;
        let mut reader = BufReader::new(stream);
        let hello = json!({
            "type": "daemon_hello",
            "protocol": { "name": "prime-agent.daemon", "version": 7 },
            "serverCapabilities": [],
        });
        write_line(&mut writer, &hello);
        loop {
            let Some(line) = read_line(&mut reader) else {
                return;
            };
            let Ok(envelope) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let id = envelope
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let command = envelope.get("command").cloned().unwrap_or(Value::Null);
            let command_type = command
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            self.last_observed
                .lock()
                .expect("mock observation")
                .clone_from(&command_type);
            match command_type.as_str() {
                "roster_subscribe" => {
                    respond(
                        &mut writer,
                        id,
                        "roster_subscribe",
                        &json!({
                            "roster": [
                                roster_row("s1", "first one"),
                                roster_row("s2", "second one"),
                            ]
                        }),
                    );
                }
                "list_saved_sessions" if self.hold_catalog_response => {}
                "list_saved_sessions" => {
                    respond(
                        &mut writer,
                        id,
                        "list_saved_sessions",
                        &json!({ "sessions": [] }),
                    );
                }
                "roster_unsubscribe" => {
                    respond(&mut writer, id, "roster_unsubscribe", &Value::Null);
                }
                other => {
                    respond_failure(&mut writer, id, other, "not handled by the mock");
                }
            }
        }
    }
}

fn write_line(writer: &mut UnixStream, value: &Value) {
    let mut line = serde_json::to_string(value).expect("serialize line");
    line.push('\n');
    writer.write_all(line.as_bytes()).expect("write");
    writer.flush().expect("flush");
}

fn respond(writer: &mut UnixStream, id: &str, command: &str, data: &Value) {
    write_line(
        writer,
        &json!({
            "id": id,
            "type": "response",
            "command": command,
            "success": true,
            "data": data,
        }),
    );
}

fn respond_failure(writer: &mut UnixStream, id: &str, command: &str, error: &str) {
    write_line(
        writer,
        &json!({
            "id": id,
            "type": "response",
            "command": command,
            "success": false,
            "error": error,
        }),
    );
}

fn read_line(reader: &mut BufReader<UnixStream>) -> Option<String> {
    let mut line = String::new();
    loop {
        match reader.read_line(&mut line).expect("read mock command") {
            0 => return None,
            _ if line.trim().is_empty() => line.clear(),
            _ => return Some(line),
        }
    }
}

fn view_options(socket: &std::path::Path) -> AgentsViewOptions {
    AgentsViewOptions {
        socket_path: socket.to_path_buf(),
        cwd: PathBuf::from("/tmp"),
        session_dir: Some(PathBuf::from("/tmp/sessions")),
        theme: "prime".to_string(),
        version: "0.0.0".to_string(),
        anchor_session_id: None,
        scope: None,
        query: None,
        expanded_ancestors: Vec::new(),
        selected_row_identity: None,
        selected_key: None,
        status_message: None,
        keybindings: pa_tui::keybindings::KeybindingsManager::new(),
        show_hardware_cursor: false,
        incident_notice_state: None,
        create_config: serde_json::json!({}),
    }
}

/// Run one headless plan and return the outcome. Holds the run lock: the click dispatch gates on
/// the process-global tracking state.
async fn run_plan(steps: Vec<AgentsStep>) -> pa_tui::agents_view::AgentsViewOutcome {
    let lock_wait = pa_test_scratch::timing::span("setup.lock_wait");
    let _guard = RUN_LOCK.lock().await;
    drop(lock_wait);
    let fixture = pa_test_scratch::timing::span("setup.fixture");
    let dir = tempfile::TempDir::new().expect("temp dir");
    let socket = dir.path().join("agents-view.sock");
    let mock = MockSupervisor::bind(&socket);
    let scenario = format!("{steps:?}");
    let last_observed = mock.last_observed.clone();
    let (connected_tx, connected_rx) = std::sync::mpsc::channel();
    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        mock.serve(&connected_tx);
        closed_tx.send(()).expect("acknowledge mock EOF");
    });
    let plan = AgentsHeadlessPlan {
        steps,
        width: 120,
        height: 36,
    };
    drop(fixture);
    let run_wait = pa_test_scratch::timing::span("run.wait_and_render");
    let run = run_agents_view(
        view_options(&socket),
        AgentsViewUiMode::Headless(plan),
        None,
    )
    .await;
    drop(run_wait);
    let _teardown = pa_test_scratch::timing::span("teardown.server_join");
    connected_rx
        .recv_timeout(Duration::from_millis(2500))
        .expect("mock accepted the view connection")
        .shutdown(std::net::Shutdown::Read)
        .expect("cancel mock command reads");
    closed_rx
        .recv_timeout(Duration::from_millis(2500))
        .unwrap_or_else(|error| {
            let last = last_observed.lock().expect("mock observation");
            panic!(
                "mock {scenario}: waiting for command-loop EOF acknowledgement after read cancellation; last observed={last}; completion={error}"
            );
        });
    server.join().expect("mock server thread");
    run.expect("the agents view run").outcome
}

/// The last frame holding a needle and its row.
fn locate_row(frames: &[String], needle: &str) -> Option<usize> {
    frames
        .iter()
        .filter_map(|frame| {
            frame
                .split('\n')
                .position(|row| row.contains(needle))
                .map(|row| (row, frame))
        })
        .next_back()
        .map(|(row, _)| row)
}

/// A plain click on a session row opens it: the press/release pair on the second row's line selects
/// and opens that row (the default Enter would have opened the FIRST row).
#[tokio::test]
#[pa_test_scratch::pa_test]
async fn a_click_on_a_session_row_opens_it() {
    let outcome = run_plan(vec![AgentsStep::WaitSettle { timeout_ms: 2500 }]).await;
    let row = locate_row(&outcome.frames, "second one").expect("the second roster row renders");
    let outcome = run_plan(vec![
        AgentsStep::WaitSettle { timeout_ms: 2500 },
        AgentsStep::Click { row, col: 2 },
    ])
    .await;
    assert_eq!(
        outcome.selection,
        Some(SessionSelection::Attach("s2-live".to_string())),
        "the click opened the clicked row, not the default first one"
    );
}

/// A dragged release never opens the row under it (the press-drag release is a select gesture, not
/// a click).
#[tokio::test]
#[pa_test_scratch::pa_test]
async fn a_dragged_release_never_opens_the_row() {
    let outcome = run_plan(vec![AgentsStep::WaitSettle { timeout_ms: 2500 }]).await;
    let row = locate_row(&outcome.frames, "second one").expect("the second roster row renders");
    // The raw reports a drag sends: the press, the motion report (button 0 + the motion bit), then
    // the release — the drag kills the pending click.
    let press = format!("\x1b[<0;3;{}M", row + 1);
    let drag = format!("\x1b[<32;3;{}M", row + 1);
    let release = format!("\x1b[<0;3;{}m", row + 1);
    let outcome = run_plan(vec![
        AgentsStep::WaitSettle { timeout_ms: 2500 },
        AgentsStep::Mouse(press),
        AgentsStep::Mouse(drag),
        AgentsStep::Mouse(release),
    ])
    .await;
    assert_eq!(
        outcome.selection, None,
        "the dragged release never opened the row"
    );
}

#[tokio::test]
#[pa_test_scratch::pa_test]
async fn settle_reports_the_unfinished_catalog_instead_of_releasing_the_plan() {
    assert_unfinished_catalog_fails_settle(vec![AgentsStep::WaitSettle { timeout_ms: 0 }]).await;
}

#[tokio::test]
#[pa_test_scratch::pa_test]
async fn settle_after_a_satisfied_render_still_waits_for_the_catalog() {
    assert_unfinished_catalog_fails_settle(vec![
        AgentsStep::WaitRender {
            needle: "first one".to_string(),
            timeout_ms: 0,
        },
        AgentsStep::WaitSettle { timeout_ms: 0 },
    ])
    .await;
}

async fn assert_unfinished_catalog_fails_settle(steps: Vec<AgentsStep>) {
    let _guard = RUN_LOCK.lock().await;
    let dir = tempfile::TempDir::new().expect("temp dir");
    let socket = dir.path().join("agents-view.sock");
    let mut mock = MockSupervisor::bind(&socket);
    mock.hold_catalog_response = true;
    let scenario = format!("withheld saved catalog: {steps:?}");
    let last_observed = mock.last_observed.clone();
    let (connected_tx, connected_rx) = std::sync::mpsc::channel();
    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        mock.serve(&connected_tx);
        closed_tx.send(()).expect("acknowledge mock EOF");
    });
    let result = run_agents_view(
        view_options(&socket),
        AgentsViewUiMode::Headless(AgentsHeadlessPlan {
            steps,
            width: 120,
            height: 36,
        }),
        None,
    )
    .await;
    connected_rx
        .recv_timeout(Duration::from_millis(2500))
        .expect("mock accepted the view connection")
        .shutdown(std::net::Shutdown::Read)
        .expect("cancel mock command reads");
    closed_rx
        .recv_timeout(Duration::from_millis(2500))
        .unwrap_or_else(|error| {
            let last = last_observed.lock().expect("mock observation");
            panic!(
                "mock {scenario}: waiting for command-loop EOF acknowledgement after read cancellation; last observed={last}; completion={error}"
            );
        });
    server.join().expect("mock server thread");
    let error = result
        .err()
        .expect("an unfinished catalog fails the settle");
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("WaitSettle timed out"), "{diagnostic}");
    assert!(diagnostic.contains("catalog_request=Some"), "{diagnostic}");
    assert!(diagnostic.contains("first one"), "{diagnostic}");
}
