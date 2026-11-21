use std::path::Path;
use std::time::{Duration, Instant};

const OWNER: &str = "PA_TEST_PROCESS_OWNER";
static GUARDIAN_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
static LIVENESS_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

pub(crate) fn install(root: &Path, remove_root: bool) {
    if std::env::var_os(OWNER).is_some() {
        return;
    }
    let owner = std::process::id();
    std::env::set_var(OWNER, owner.to_string());
    let mut pipe = [-1; 2];
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        eprintln!(
            "cannot create test process guardian pipe: {}",
            std::io::Error::last_os_error()
        );
        crate::record_cleanup_failure();
        return;
    }
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe {
            libc::close(pipe[1]);
            libc::setsid();
        }
        let mut byte = 0_u8;
        loop {
            let result = unsafe { libc::read(pipe[0], (&mut byte as *mut u8).cast(), 1) };
            if result >= 0 {
                break;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break;
            }
        }
        let clean = cleanup(owner);
        if clean && remove_root && !root.join(".pa-test-leak").exists() {
            let _ = std::fs::remove_dir_all(root);
        }
        unsafe {
            libc::_exit(i32::from(!clean));
        }
    }
    unsafe {
        libc::close(pipe[0]);
    }
    if pid < 0 {
        unsafe {
            libc::close(pipe[1]);
        }
        eprintln!(
            "cannot fork test process guardian: {}",
            std::io::Error::last_os_error()
        );
        crate::record_cleanup_failure();
    } else {
        GUARDIAN_PID.store(pid, std::sync::atomic::Ordering::Relaxed);
        LIVENESS_FD.store(pipe[1], std::sync::atomic::Ordering::Relaxed);
    }
}

pub(crate) fn finish() {
    let fd = LIVENESS_FD.swap(-1, std::sync::atomic::Ordering::Relaxed);
    let pid = GUARDIAN_PID.swap(-1, std::sync::atomic::Ordering::Relaxed);
    if fd < 0 || pid < 0 {
        return;
    }
    unsafe {
        libc::close(fd);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let mut status = 0;
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if result == pid {
            if status != 0 {
                crate::record_cleanup_failure();
            }
            return;
        }
        if result < 0 || Instant::now() >= deadline {
            eprintln!("test process guardian {pid} did not finish cleanly");
            crate::record_cleanup_failure();
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn cleanup(owner: u32) -> bool {
    let mut alive = processes(owner);
    if alive.is_empty() {
        return true;
    }
    let grace_deadline = Instant::now() + Duration::from_millis(50);
    while Instant::now() < grace_deadline {
        std::thread::sleep(Duration::from_millis(10));
        alive = processes(owner);
        if alive.is_empty() {
            return true;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        for pid in &alive {
            unsafe {
                libc::kill(*pid, libc::SIGKILL);
            }
        }
        alive = processes(owner);
        if alive.is_empty() {
            return true;
        }
        if Instant::now() >= deadline {
            let details = alive
                .iter()
                .map(|pid| {
                    let argv = std::fs::read(format!("/proc/{pid}/cmdline"))
                        .map(|bytes| {
                            bytes
                                .split(|byte| *byte == 0)
                                .filter(|arg| !arg.is_empty())
                                .map(|arg| String::from_utf8_lossy(arg).into_owned())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let test = std::fs::read(format!("/proc/{pid}/environ")).ok().and_then(
                        |environment| {
                            environment
                                .split(|byte| *byte == 0)
                                .find_map(|value| value.strip_prefix(b"PA_TEST_COMMAND_TEST="))
                                .map(|label| String::from_utf8_lossy(label).into_owned())
                        },
                    );
                    format!("pid={pid} argv={argv:?} test={test:?}")
                })
                .collect::<Vec<_>>();
            eprintln!("test binary {owner} subprocess cleanup failed: {details:?}; retaining scratch root");
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
        alive = processes(owner);
        if alive.is_empty() {
            return true;
        }
    }
}

fn processes(owner: u32) -> Vec<libc::pid_t> {
    let marker = format!("{OWNER}={owner}");
    let Ok(entries) = std::fs::read_dir("/proc") else {
        eprintln!("cannot inspect /proc for leaked test subprocesses");
        crate::record_cleanup_failure();
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<libc::pid_t>().ok()?;
            if pid == owner as libc::pid_t || pid == std::process::id() as libc::pid_t {
                return None;
            }
            let environment = std::fs::read(entry.path().join("environ")).ok()?;
            if !environment
                .split(|byte| *byte == 0)
                .any(|value| value == marker.as_bytes())
            {
                return None;
            }
            Some(pid)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    #[allow(
        clippy::zombie_processes,
        reason = "The guardian owns the intentionally orphaned fixture subprocess."
    )]
    fn guardian_fixture() {
        if std::env::var_os("PA_TEST_GUARDIAN_FIXTURE").is_none() {
            return;
        }
        let mut command = Command::new("sh");
        command
            .args(["-c", "echo $$; exec sleep 60"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        println!("guardian-child {}", line.trim());
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }

    #[test]
    fn cleanup_success_fixture() {
        let Ok(mode) = std::env::var("PA_TEST_GUARDIAN_CLEANUP_FIXTURE") else {
            return;
        };
        use std::os::unix::process::ExitStatusExt;
        let script = match mode.as_str() {
            "natural" => "echo ready; read release; exec sleep 0.01",
            "stubborn" => "trap '' TERM; echo ready; read release; exec sleep 60",
            other => panic!("unknown cleanup fixture {other}"),
        };
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let mut readiness = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut readiness)
            .unwrap();
        assert_eq!(readiness, "ready\n");
        assert!(super::processes(std::process::id()).contains(&(child.id() as libc::pid_t)));
        child.stdin.take().unwrap().write_all(b"release\n").unwrap();
        assert!(
            super::cleanup(std::process::id()),
            "cleanup must confirm the fixture is gone"
        );
        let status = child.wait().unwrap();
        match mode.as_str() {
            "natural" => assert!(status.success(), "natural exit was killed: {status}"),
            "stubborn" => assert_eq!(status.signal(), Some(libc::SIGKILL)),
            other => panic!("unknown cleanup fixture {other}"),
        }
        assert!(super::processes(std::process::id()).is_empty());
    }

    fn run_cleanup_fixture(mode: &str) {
        let scratch =
            std::env::temp_dir().join(format!("guardian-cleanup-{}-{mode}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "guardian::tests::cleanup_success_fixture",
                "--nocapture",
            ])
            .env("PA_TEST_GUARDIAN_CLEANUP_FIXTURE", mode)
            .env("PA_TEST_SCRATCH_ROOT", &scratch)
            .env_remove("PA_TEST_PROCESS_OWNER")
            .env_remove("PA_TEST_COMMAND_OWNER")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let _group = crate::ProcessGroup::new(child.id());
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "isolated {mode} fixture failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!scratch.join(".pa-test-leak").exists());
        std::fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn cleanup_accepts_delayed_natural_exit() {
        run_cleanup_fixture("natural");
    }

    #[test]
    fn cleanup_accepts_confirmed_kill_of_a_long_lived_child() {
        run_cleanup_fixture("stubborn");
    }

    #[test]
    fn guardian_reaps_detached_descendants_after_binary_is_killed() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "guardian::tests::guardian_fixture",
                "--nocapture",
            ])
            .env("PA_TEST_GUARDIAN_FIXTURE", "1")
            .env_remove("PA_TEST_PROCESS_OWNER")
            .env_remove("PA_TEST_COMMAND_OWNER")
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let _group = crate::ProcessGroup::new(child.id());
        let pid = BufReader::new(child.stdout.take().unwrap())
            .lines()
            .map(Result::unwrap)
            .find_map(|line| {
                line.split_once("guardian-child ")
                    .map(|(_, pid)| pid.to_owned())
            })
            .expect("fixture readiness")
            .parse::<u32>()
            .unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let living = std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, fields)| !fields.starts_with("Z "))
            });
            if !living {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "detached fixture {pid} survived guardian cleanup"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
