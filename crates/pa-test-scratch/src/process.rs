use std::ffi::OsStr;
use std::io;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::process::{Child, ExitStatus, Output, Stdio};

/// A test command whose child and descendants remain owned until the guard is dropped.
#[derive(Debug)]
pub struct Command {
    inner: std::process::Command,
    stdin_set: bool,
    stdout_set: bool,
    stderr_set: bool,
    separate_group: bool,
}

impl Command {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            inner: std::process::Command::new(program),
            stdin_set: false,
            stdout_set: false,
            stderr_set: false,
            separate_group: true,
        }
    }

    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.inner.arg(arg);
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.inner.args(args);
        self
    }

    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.inner.env(key, value);
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.inner.envs(vars);
        self
    }

    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.inner.env_remove(key);
        self
    }

    pub fn env_clear(&mut self) -> &mut Self {
        self.inner.env_clear();
        self
    }

    pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
        self.inner.current_dir(dir);
        self
    }

    pub fn stdin(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.stdin_set = true;
        self.inner.stdin(cfg);
        self
    }

    pub fn stdout(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.stdout_set = true;
        self.inner.stdout(cfg);
        self
    }

    pub fn stderr(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.stderr_set = true;
        self.inner.stderr(cfg);
        self
    }

    /// Registers a Unix pre-exec hook that establishes its own process group or session.
    ///
    /// # Safety
    /// The callback must meet `std::os::unix::process::CommandExt::pre_exec` requirements.
    #[cfg(unix)]
    pub unsafe fn pre_exec<F>(&mut self, f: F) -> &mut Self
    where
        F: FnMut() -> io::Result<()> + Send + Sync + 'static,
    {
        use std::os::unix::process::CommandExt;
        self.separate_group = false;
        unsafe {
            self.inner.pre_exec(f);
        }
        self
    }

    #[cfg(unix)]
    pub fn process_group(&mut self, group: i32) -> &mut Self {
        use std::os::unix::process::CommandExt;
        self.separate_group = false;
        self.inner.process_group(group);
        self
    }

    pub fn spawn(&mut self) -> io::Result<OwnedChild> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            if self.separate_group {
                self.inner.process_group(0);
            }
        }
        static NEXT_COMMAND: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT_COMMAND.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let local = format!("{}-{sequence}", std::process::id());
        let token = std::env::var("PA_TEST_COMMAND_OWNER")
            .map_or_else(|_| local.clone(), |parent| format!("{parent}/{local}"));
        self.inner.env("PA_TEST_COMMAND_OWNER", &token);
        self.inner.env(
            "PA_TEST_COMMAND_TEST",
            std::thread::current().name().unwrap_or("unnamed test"),
        );
        if let Some(owner) = std::env::var_os("PA_TEST_PROCESS_OWNER") {
            self.inner.env("PA_TEST_PROCESS_OWNER", owner);
        }
        self.inner
            .spawn()
            .map(|child| OwnedChild::new(child, token))
    }

    pub fn status(&mut self) -> io::Result<ExitStatus> {
        self.spawn()?.wait()
    }

    pub fn output(&mut self) -> io::Result<Output> {
        self.configure_output();
        self.spawn()?.wait_with_output()
    }

    /// Returns a guard that must remain alive while intentionally detached children run.
    pub fn output_with_detached_children(&mut self) -> io::Result<(Output, DetachedProcesses)> {
        self.configure_output();
        let mut child = self.spawn()?;
        let detached = DetachedProcesses {
            group: child.group.take().expect("owned process group"),
        };
        let output = child.wait_with_output()?;
        Ok((output, detached))
    }

    fn configure_output(&mut self) {
        if !self.stdin_set {
            self.inner.stdin(Stdio::null());
        }
        if !self.stdout_set {
            self.inner.stdout(Stdio::piped());
        }
        if !self.stderr_set {
            self.inner.stderr(Stdio::piped());
        }
    }
}

impl Deref for Command {
    type Target = std::process::Command;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Command {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Owns a test subprocess group, including when assertions unwind past the guard.
#[derive(Debug)]
pub struct OwnedChild {
    child: Option<Child>,
    pid: u32,
    group: Option<ProcessGroup>,
}

impl OwnedChild {
    fn new(child: Child, token: String) -> Self {
        let mut group = ProcessGroup::new(child.id());
        #[cfg(target_os = "linux")]
        {
            group.marker = Some(format!("PA_TEST_COMMAND_OWNER={token}").into_bytes());
        }
        #[cfg(not(target_os = "linux"))]
        let _ = token;
        Self {
            pid: child.id(),
            group: Some(group),
            child: Some(child),
        }
    }

    pub fn wait_with_output(mut self) -> io::Result<Output> {
        self.child.take().expect("owned child").wait_with_output()
    }
}

impl Deref for OwnedChild {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        self.child.as_ref().expect("owned child")
    }
}

impl DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.child.as_mut().expect("owned child")
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(group) = &self.group {
            group.kill();
        }
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    result => {
                        eprintln!("test process {} could not be reaped: {result:?}", self.pid);
                        crate::record_cleanup_failure();
                        break;
                    }
                }
            }
        }
    }
}

/// Ownership transferred from a launcher that deliberately leaves subprocesses running.
#[derive(Debug)]
#[must_use = "hold this guard until detached test subprocesses may be stopped"]
pub struct DetachedProcesses {
    group: ProcessGroup,
}

impl Drop for DetachedProcesses {
    fn drop(&mut self) {
        self.group.kill();
    }
}

/// A process-group owner for subprocess APIs other than `std::process::Child`.
#[derive(Debug)]
pub struct ProcessGroup {
    pid: u32,
    owner: String,
    #[cfg(target_os = "linux")]
    marker: Option<Vec<u8>>,
}

impl ProcessGroup {
    /// The caller must have created a dedicated process group with this ID.
    pub fn new(pid: u32) -> Self {
        Self {
            pid,
            owner: std::thread::current()
                .name()
                .unwrap_or("unnamed test")
                .to_owned(),
            #[cfg(target_os = "linux")]
            marker: std::fs::read(format!("/proc/{pid}/environ"))
                .ok()
                .and_then(|environment| {
                    environment
                        .split(|byte| *byte == 0)
                        .find(|value| value.starts_with(b"PA_TEST_COMMAND_OWNER="))
                        .map(<[u8]>::to_vec)
                }),
        }
    }

    fn kill(&self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.pid as libc::pid_t), libc::SIGKILL);
        }
        #[cfg(target_os = "linux")]
        for pid in group_processes(self.pid, self.marker.as_deref()) {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
        #[cfg(target_os = "linux")]
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                let survivors = group_processes(self.pid, self.marker.as_deref());
                if survivors.is_empty() {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    eprintln!(
                        "test {} process group {} survived cleanup: {survivors:?}",
                        self.owner, self.pid
                    );
                    crate::record_cleanup_failure();
                    break;
                }
                self.kill();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn group_processes(group: u32, marker: Option<&[u8]>) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        crate::record_cleanup_failure();
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            let fields = stat
                .rsplit_once(") ")?
                .1
                .split_whitespace()
                .collect::<Vec<_>>();
            let state = *fields.first()?;
            let process_group = fields.get(2)?.parse::<u32>().ok()?;
            let marked = marker.is_some_and(|marker| {
                std::fs::read(entry.path().join("environ")).is_ok_and(|environment| {
                    environment.split(|byte| *byte == 0).any(|value| {
                        value == marker
                            || value
                                .strip_prefix(marker)
                                .is_some_and(|suffix| suffix.starts_with(b"/"))
                    })
                })
            });
            ((process_group == group || marked) && state != "Z" && state != "X").then_some(pid)
        })
        .collect()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::Command;
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    #[test]
    fn output_and_status_retain_standard_command_behavior() {
        let output = Command::new("sh")
            .args(["-c", "printf owned"])
            .output()
            .unwrap();
        assert_eq!(output.stdout, b"owned");
        assert!(output.status.success());
        assert_eq!(
            Command::new("sh")
                .args(["-c", "exit 7"])
                .status()
                .unwrap()
                .code(),
            Some(7)
        );
    }

    #[test]
    #[allow(
        clippy::zombie_processes,
        reason = "The ownership guard under test owns the intentionally detached fixture subprocess."
    )]
    fn detached_fixture() {
        let Some(mode) = std::env::var_os("PA_TEST_DETACHED_FIXTURE") else {
            return;
        };
        use std::io::Write;
        use std::os::unix::process::CommandExt;
        let mut command = std::process::Command::new("sh");
        command
            .args(["-c", "trap '' TERM; echo $$; exec sleep 60"])
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
        println!("detached-child {}", line.trim());
        std::io::stdout().flush().unwrap();
        if mode == "exit" {
            return;
        }
        loop {
            std::thread::park();
        }
    }

    fn fixture_pid(output: &str) -> u32 {
        output
            .lines()
            .find_map(|line| {
                line.split_once("detached-child ")
                    .map(|(_, pid)| pid.trim().parse::<u32>().unwrap())
            })
            .expect("fixture readiness")
    }

    fn living(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, fields)| !fields.starts_with("Z "))
        })
    }

    #[test]
    fn token_owner_reaps_a_stubborn_detached_session() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "process::tests::detached_fixture", "--nocapture"])
            .env("PA_TEST_DETACHED_FIXTURE", "wait")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut output = String::new();
        loop {
            assert_ne!(
                reader.read_line(&mut output).unwrap(),
                0,
                "fixture exited before readiness"
            );
            if output.contains("detached-child ") {
                break;
            }
        }
        let pid = fixture_pid(&output);
        assert!(living(pid));
        drop(child);
        assert!(
            !living(pid),
            "detached session survived owned-child teardown"
        );
    }

    #[test]
    fn detached_output_transfers_ownership_to_the_returned_guard() {
        let (output, detached) = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "process::tests::detached_fixture", "--nocapture"])
            .env("PA_TEST_DETACHED_FIXTURE", "exit")
            .output_with_detached_children()
            .unwrap();
        assert!(output.status.success());
        let pid = fixture_pid(&String::from_utf8(output.stdout).unwrap());
        assert!(
            living(pid),
            "launcher exit killed deliberately transferred subprocess"
        );
        drop(detached);
        assert!(
            !living(pid),
            "transferred guard did not reap detached subprocess"
        );
    }

    #[test]
    fn assertion_unwind_reaps_the_child_and_its_descendants() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 60 & echo $!; wait"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let leader = child.id();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let descendant = line.trim().parse::<u32>().unwrap();
        let result = std::panic::catch_unwind(move || {
            let _owned = child;
            panic!("fixture assertion");
        });
        assert!(result.is_err());
        assert!(!super::group_processes(leader, None).contains(&descendant));
        assert!(!std::path::Path::new(&format!("/proc/{leader}")).exists());
    }
}
