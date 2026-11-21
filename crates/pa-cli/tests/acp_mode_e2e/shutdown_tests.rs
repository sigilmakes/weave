use super::shutdown_sandboxed_daemon;
use pa_test_scratch::{Command, OwnedChild};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

fn stubborn_child() -> OwnedChild {
    let mut child = Command::new("sh")
        .args(["-c", "trap '' TERM; echo ready; exec sleep 60"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn stubborn shutdown fixture");
    let mut ready = String::new();
    BufReader::new(child.stdout.take().expect("readiness pipe"))
        .read_line(&mut ready)
        .expect("stubborn child readiness");
    assert_eq!(ready.trim(), "ready");
    child
}

fn withhold_shutdown_acknowledgement(socket: &Path, pid: u32) -> std::thread::JoinHandle<()> {
    let listener = UnixListener::bind(socket).expect("bind shutdown fixture");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept shutdown connection");
        writeln!(
            stream,
            "{}",
            json!({"type": "daemon_hello", "supervisorPid": pid})
        )
        .expect("send supervisor hello");
        stream.flush().expect("flush supervisor hello");
        let mut reader = BufReader::new(stream);
        let mut request = String::new();
        reader
            .read_line(&mut request)
            .expect("receive shutdown request");
        let request: Value = serde_json::from_str(&request).expect("shutdown JSON");
        assert_eq!(request["id"], "shutdown-test");
        std::io::copy(&mut reader, &mut std::io::sink()).expect("wait for teardown disconnect");
    })
}

#[pa_test_scratch::pa_test]
#[test]
fn missing_acknowledgement_passes_after_stubborn_child_is_confirmed_absent() {
    let root = tempfile::tempdir().expect("shutdown fixture root");
    let socket = root.path().join("daemon.sock");
    let mut child = stubborn_child();
    let pid = child.id();
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) }, 0);
    let server = withhold_shutdown_acknowledgement(&socket, pid);
    let started = Instant::now();
    shutdown_sandboxed_daemon(&socket, &mut child);
    assert!(started.elapsed() >= Duration::from_millis(300));
    server.join().expect("shutdown fixture server");
    assert!(!child.wait().expect("reap stubborn child").success());
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "shutdown child {pid} survived teardown"
    );
}

#[pa_test_scratch::pa_test]
#[test]
fn missing_hello_cleanup_fixture() {
    let Some(root) = std::env::var_os("PA_TEST_MISSING_HELLO_ROOT") else {
        return;
    };
    let socket = Path::new(&root).join("daemon.sock");
    let listener = UnixListener::bind(&socket).expect("bind missing hello fixture");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept missing hello connection");
        std::io::copy(&mut stream, &mut std::io::sink())
            .expect("wait for missing hello disconnect");
    });
    let mut child = stubborn_child();
    let pid = child.id();
    println!("missing-hello-child-pid:{pid}");
    let started = Instant::now();
    shutdown_sandboxed_daemon(&socket, &mut child);
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(
        child
            .try_wait()
            .expect("observe terminated child")
            .is_some(),
        "missing hello did not terminate child {pid}"
    );
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "missing hello left child {pid} alive"
    );
    server.join().expect("missing hello fixture server");
}

#[pa_test_scratch::pa_test]
#[test]
fn missing_hello_confirms_owned_tree_absence_and_names_handshake_failure() {
    let root = tempfile::tempdir().expect("missing hello fixture root");
    let fixture = "shutdown_tests::missing_hello_cleanup_fixture";
    let mut child = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", fixture, "--nocapture"])
        .env("PA_TEST_MISSING_HELLO_ROOT", root.path())
        .env("PA_TEST_SCRATCH_ROOT", root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run missing hello cleanup fixture");
    let deadline = Instant::now() + Duration::from_secs(10);
    while child
        .try_wait()
        .expect("observe missing hello fixture exit")
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "missing hello fixture exceeded its bounded lifetime"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child
        .wait_with_output()
        .expect("capture missing hello fixture");
    let stdout = String::from_utf8(output.stdout).expect("missing hello stdout");
    let stderr = String::from_utf8(output.stderr).expect("missing hello stderr");
    assert!(output.status.success(), "{stdout}\n{stderr}");
    let pid = stdout
        .lines()
        .find_map(|line| {
            line.split_once("missing-hello-child-pid:")
                .map(|(_, pid)| pid.trim())
        })
        .expect("missing hello child pid")
        .parse::<u32>()
        .expect("numeric child pid");
    let diagnostic = format!("supervisor at {} did not send a shutdown hello within 300 ms; terminating owned process tree", root.path().join("daemon.sock").display());
    assert!(
        stderr.lines().any(|line| line == diagnostic),
        "expected diagnostic {diagnostic:?} in {stderr:?}"
    );
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "missing hello fixture child {pid} survived"
    );
}
