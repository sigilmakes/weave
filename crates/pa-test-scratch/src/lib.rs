//! Test-only subprocess and temporary-directory ownership.
//!
//! Linking installs a private, short `TMPDIR` before the test binary starts. Unix exit hooks
//! remove owned roots only after successful cleanup; failures exit nonzero and retain scratch.
//! Linux additionally starts a guardian that
//! watches a close-on-exec liveness pipe, cleans inherited subprocess ownership markers after
//! abrupt binary termination, and preserves scratch roots when leaks are found.
//!
//! Use [`Command`] rather than `std::process::Command` for test subprocesses. Its [`OwnedChild`]
//! guard kills the dedicated process group and, on Linux, uniquely marked descendants even if
//! they create their own sessions. [`ProcessGroup`] adopts an externally created dedicated group.
//! Launchers deliberately leaving children running return a [`DetachedProcesses`] guard through
//! [`Command::output_with_detached_children`]; keep that guard alive until the owning test ends.
//!
//! The workspace gate can supply `PA_TEST_SCRATCH_ROOT`. Otherwise short temporary paths use
//! `<temp_dir>/pa-t-<pid>` and deep temporary paths use `/tmp/pa-t-<pid>`, preserving Unix socket
//! path capacity. Child test-binary re-execs inherit the binary owner rather than minting roots.

#[cfg(target_os = "linux")]
mod guardian;
mod process;
pub use process::{Command, DetachedProcesses, OwnedChild, ProcessGroup};

static CLEANUP_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn record_cleanup_failure() {
    CLEANUP_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(unix)]
mod hook {
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    /// Environment variable carrying a root the runner already owns, such as the workspace
    /// gate's per-target directory. Used verbatim so the runner's own cleanup still applies.
    const RUNNER_ROOT: &str = "PA_TEST_SCRATCH_ROOT";

    /// The longest `temp_dir()` that can host a scratch root without eating the `sun_path`
    /// budget: `/tmp`, `/var/tmp`, and short `/run/user/<uid>` paths qualify.
    const SHORT_TEMP_DIR: usize = 16;

    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    static OWNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// The private root, created on first use with `TMPDIR` pointing at it. Idempotent: the hook
    /// and any later call share one root.
    pub(crate) fn root() -> &'static PathBuf {
        ROOT.get_or_init(|| {
            let (root, owned) = select();
            let _ = std::fs::create_dir_all(&root);
            // The scratch root is the new temporary directory for this process and every child it
            // spawns, so a test that writes a fixture without naming a path still lands here.
            std::env::set_var("TMPDIR", &root);
            let process_owner = std::env::var_os("PA_TEST_PROCESS_OWNER").is_none();
            #[cfg(target_os = "linux")]
            if process_owner {
                crate::guardian::install(&root, owned);
            }
            if process_owner {
                OWNED.store(owned, std::sync::atomic::Ordering::Relaxed);
            }
            unsafe {
                libc::atexit(remove);
            }
            root
        })
    }

    /// The root this process uses, and whether this process owns its removal.
    fn select() -> (PathBuf, bool) {
        if let Some(runner) = std::env::var_os(RUNNER_ROOT).filter(|value| !value.is_empty()) {
            return (PathBuf::from(runner), false);
        }
        // A child re-execs its own test binary for a fixture helper, and some of those children
        // are killed rather than exiting. They inherit the parent's root and use it as-is instead
        // of minting one of their own that no one is left to remove.
        let temp = std::env::temp_dir();
        if temp
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("pa-t-"))
        {
            return (temp, false);
        }
        let name = format!("pa-t-{}", std::process::id());
        let root = if temp.as_os_str().as_encoded_bytes().len() <= SHORT_TEMP_DIR {
            temp.join(name)
        } else {
            Path::new("/tmp").join(name)
        };
        (root, true)
    }

    extern "C" fn remove() {
        if ROOT
            .get()
            .is_some_and(|root| root.join(".pa-test-leak").exists())
        {
            crate::record_cleanup_failure();
        }
        #[cfg(target_os = "linux")]
        if !crate::guardian::cleanup(std::process::id()) {
            crate::record_cleanup_failure();
        }
        if crate::CLEANUP_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(root) = ROOT.get() {
                let _ = std::fs::write(root.join(".pa-test-leak"), "subprocess cleanup failed\n");
            }
        }
        #[cfg(target_os = "linux")]
        crate::guardian::finish();
        if crate::CLEANUP_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(root) = ROOT.get() {
                let _ = std::fs::write(root.join(".pa-test-leak"), "subprocess cleanup failed\n");
            }
            unsafe {
                libc::_exit(1);
            }
        }
        if let Some(root) = ROOT
            .get()
            .filter(|_| OWNED.load(std::sync::atomic::Ordering::Relaxed))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }

    extern "C" fn install() {
        let _ = root();
    }

    #[used]
    #[cfg_attr(
        any(target_os = "linux", target_os = "android"),
        link_section = ".init_array"
    )]
    static INSTALL: extern "C" fn() = install;
}

#[cfg(test)]
mod tests {
    #[test]
    fn reexec_cleanup_failure_fixture() {
        if std::env::var_os("PA_TEST_CLEANUP_FAILURE_FIXTURE").is_some() {
            crate::record_cleanup_failure();
        }
    }

    #[cfg(unix)]
    #[test]
    fn inherited_owner_reexec_reports_cleanup_failure_in_its_exit_status() {
        let scratch = std::env::temp_dir().join(format!("cleanup-failure-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        let output = crate::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::reexec_cleanup_failure_fixture",
                "--nocapture",
            ])
            .env("PA_TEST_CLEANUP_FAILURE_FIXTURE", "1")
            .env("PA_TEST_SCRATCH_ROOT", &scratch)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "inherited-owner fixture must fail at exit"
        );
        assert!(scratch.join(".pa-test-leak").exists());
        std::fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn the_test_binary_runs_with_its_own_temporary_directory() {
        let root = std::env::temp_dir();
        let own = format!("pa-t-{}", std::process::id());
        let runner = std::env::var_os("PA_TEST_SCRATCH_ROOT").map(std::path::PathBuf::from);
        assert!(
            root.ends_with(&own) || runner.as_deref() == Some(root.as_path()),
            "TMPDIR is {root:?}, neither a {own} root nor the runner's scratch root"
        );
    }
}
