use std::path::Path;

const ROOT_MARKER: &str = "PA_TEST_PRIVATE_HOME_ROOT";
const DIRECTORIES: &[(&str, &str, &str)] = &[
    ("HOME", "PA_TEST_REAL_HOME", "home"),
    ("XDG_CONFIG_HOME", "PA_TEST_REAL_XDG_CONFIG_HOME", "config"),
    ("XDG_CACHE_HOME", "PA_TEST_REAL_XDG_CACHE_HOME", "cache"),
    ("XDG_DATA_HOME", "PA_TEST_REAL_XDG_DATA_HOME", "data"),
    ("XDG_STATE_HOME", "PA_TEST_REAL_XDG_STATE_HOME", "state"),
    ("XDG_RUNTIME_DIR", "PA_TEST_REAL_XDG_RUNTIME_DIR", "runtime"),
    (
        "PRIME_AGENT_CODING_AGENT_DIR",
        "PA_TEST_REAL_CODING_AGENT_DIR",
        "agent",
    ),
];

const INHERITED_DIRECTORIES: &[(&str, &str, &str)] = &[
    ("PRIME_AGENT_AGENT_DIR", "PA_TEST_REAL_AGENT_DIR", "agent"),
    (
        "PI_CODING_AGENT_DIR",
        "PA_TEST_REAL_PI_CODING_AGENT_DIR",
        "agent",
    ),
    (
        "PRIME_AGENT_CODING_AGENT_SESSION_DIR",
        "PA_TEST_REAL_CODING_AGENT_SESSION_DIR",
        "coding-sessions",
    ),
    (
        "PRIME_AGENT_SESSION_DIR",
        "PA_TEST_REAL_SESSION_DIR",
        "sessions",
    ),
    (
        "RLM_SESSION_DIR",
        "PA_TEST_REAL_RLM_SESSION_DIR",
        "rlm-session",
    ),
    (
        "PRIME_AGENT_KERNEL_VENV",
        "PA_TEST_REAL_KERNEL_VENV",
        "kernel-venv",
    ),
    (
        "PRIME_AGENT_INTERNAL_DAEMON_SUPERVISOR_REGISTRY_DIR",
        "PA_TEST_REAL_SUPERVISOR_REGISTRY_DIR",
        "registry",
    ),
];

const SOCKET_PATHS: &[(&str, &str, &str)] = &[(
    "PRIME_AGENT_DAEMON_SOCKET",
    "PA_TEST_REAL_DAEMON_SOCKET",
    "daemon.sock",
)];
const PACKAGE_VARIABLES: &[(&str, &str)] = &[
    ("PI_PACKAGE_DIR", "PA_TEST_REAL_PI_PACKAGE_DIR"),
    (
        "PI_SKIP_VERSION_CHECK",
        "PA_TEST_REAL_PI_SKIP_VERSION_CHECK",
    ),
];

pub(crate) fn install(root: &Path) {
    // Re-executed fixtures inherit isolation and may explicitly override individual paths.
    if std::env::var_os(ROOT_MARKER).is_some() {
        return;
    }
    for &(variable, original, relative) in DIRECTORIES.iter().chain(
        INHERITED_DIRECTORIES
            .iter()
            .filter(|(variable, _, _)| std::env::var_os(variable).is_some()),
    ) {
        if let Some(value) = std::env::var_os(variable) {
            if Path::new(&value).starts_with(root) {
                continue;
            }
            std::env::set_var(original, value);
        }
        let directory = root.join(relative);
        std::fs::create_dir_all(&directory).expect("private test home directory");
        std::env::set_var(variable, directory);
    }
    for &(variable, original, relative) in SOCKET_PATHS {
        if let Some(value) = std::env::var_os(variable) {
            if Path::new(&value).starts_with(root) {
                continue;
            }
            std::env::set_var(original, value);
        }
        std::env::set_var(variable, root.join(relative));
    }
    for &(variable, original) in PACKAGE_VARIABLES {
        if let Some(value) = std::env::var_os(variable) {
            std::env::set_var(original, value);
        }
        std::env::remove_var(variable);
    }
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("scratch crate belongs to the workspace");
    if checkout
        .join("prime-agent-runtime/pyproject.toml")
        .is_file()
    {
        std::env::set_var("PI_PACKAGE_DIR", checkout);
    }
    std::env::set_var(ROOT_MARKER, root);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_owns_home_and_xdg_directories() {
        let root = std::env::var_os(ROOT_MARKER).expect("private test root");
        for &(variable, original, _) in DIRECTORIES {
            let directory = std::env::var_os(variable).expect("private test path");
            assert!(Path::new(&directory).starts_with(&root));
            assert!(Path::new(&directory).is_dir());
            if let Some(real) = std::env::var_os(original) {
                assert_ne!(directory, real, "{variable} still uses {original}");
            }
        }
    }

    #[test]
    fn startup_preserves_real_paths() {
        const ORIGINAL: &str = "PA_TEST_ORIGINAL_PATH";
        if let Some(expected) = std::env::var_os(ORIGINAL) {
            for &(variable, original, relative) in DIRECTORIES.iter().chain(INHERITED_DIRECTORIES) {
                assert_eq!(std::env::var_os(original).as_ref(), Some(&expected));
                assert_eq!(
                    std::env::var_os(variable).map(std::path::PathBuf::from),
                    Some(std::env::temp_dir().join(relative))
                );
            }
            return;
        }
        let original = std::env::temp_dir().with_extension("original-home");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.args([
            "--exact",
            "private_home::tests::startup_preserves_real_paths",
        ]);
        child.env_remove(ROOT_MARKER).env(ORIGINAL, &original);
        for &(variable, _, _) in DIRECTORIES.iter().chain(INHERITED_DIRECTORIES) {
            child.env(variable, &original);
        }
        assert!(child.status().unwrap().success());
    }

    #[test]
    fn startup_replaces_installed_package_variables() {
        const PROBE: &str = "PA_TEST_PACKAGE_PROBE";
        if std::env::var_os(PROBE).is_some() {
            assert_eq!(
                std::env::var("PA_TEST_REAL_PI_PACKAGE_DIR").unwrap(),
                "/installed-package"
            );
            assert_eq!(
                std::env::var("PA_TEST_REAL_PI_SKIP_VERSION_CHECK").unwrap(),
                "1"
            );
            assert!(std::env::var_os("PI_SKIP_VERSION_CHECK").is_none());
            let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .unwrap();
            if checkout
                .join("prime-agent-runtime/pyproject.toml")
                .is_file()
            {
                assert_eq!(
                    std::env::var_os("PI_PACKAGE_DIR").as_deref(),
                    Some(checkout.as_os_str())
                );
            } else {
                assert!(std::env::var_os("PI_PACKAGE_DIR").is_none());
            }
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "private_home::tests::startup_replaces_installed_package_variables",
            ])
            .env_remove(ROOT_MARKER)
            .env(PROBE, "1")
            .env("PI_PACKAGE_DIR", "/installed-package")
            .env("PI_SKIP_VERSION_CHECK", "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn startup_replaces_external_daemon_socket() {
        const PROBE: &str = "PA_TEST_SOCKET_PROBE";
        if std::env::var_os(PROBE).is_some() {
            assert_eq!(
                std::env::var("PA_TEST_REAL_DAEMON_SOCKET").unwrap(),
                "/external-daemon.sock"
            );
            assert_eq!(
                std::env::var_os("PRIME_AGENT_DAEMON_SOCKET").map(std::path::PathBuf::from),
                Some(std::env::temp_dir().join("daemon.sock"))
            );
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "private_home::tests::startup_replaces_external_daemon_socket",
            ])
            .env_remove(ROOT_MARKER)
            .env(PROBE, "1")
            .env("PRIME_AGENT_DAEMON_SOCKET", "/external-daemon.sock")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn startup_replaces_external_runtime_directory() {
        const PROBE: &str = "PA_TEST_RUNTIME_DIR_PROBE";
        if std::env::var_os(PROBE).is_some() {
            assert_eq!(
                std::env::var("PA_TEST_REAL_XDG_RUNTIME_DIR").unwrap(),
                "/external-runtime"
            );
            assert_eq!(
                std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from),
                Some(std::env::temp_dir().join("runtime"))
            );
            assert!(std::env::temp_dir().join("runtime").is_dir());
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "private_home::tests::startup_replaces_external_runtime_directory",
            ])
            .env_remove(ROOT_MARKER)
            .env(PROBE, "1")
            .env("XDG_RUNTIME_DIR", "/external-runtime")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn startup_keeps_env_cleared_fixture_paths() {
        const PROBE: &str = "PA_TEST_CLEARED_ENV_PROBE";
        if let Some(root) = std::env::var_os(PROBE) {
            for &(variable, _, relative) in DIRECTORIES {
                if variable != "PRIME_AGENT_CODING_AGENT_DIR" {
                    assert_eq!(
                        std::env::var_os(variable).map(std::path::PathBuf::from),
                        Some(Path::new(&root).join(relative))
                    );
                }
            }
            assert!(std::env::var_os("PRIME_AGENT_CODING_AGENT_DIR").is_none());
            assert!(std::env::var_os("PI_PACKAGE_DIR").is_none());
            return;
        }
        let root = std::env::temp_dir().join("env-cleared-fixture");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .env_clear()
            .args([
                "--exact",
                "private_home::tests::startup_keeps_env_cleared_fixture_paths",
            ])
            .env(ROOT_MARKER, &root)
            .env(PROBE, &root)
            .env("TMPDIR", &root);
        for &(variable, _, relative) in DIRECTORIES {
            if variable != "PRIME_AGENT_CODING_AGENT_DIR" {
                let directory = root.join(relative);
                std::fs::create_dir_all(&directory).unwrap();
                child.env(variable, directory);
            }
        }
        assert!(child.status().unwrap().success());
    }

    #[test]
    fn startup_keeps_explicit_child_overrides() {
        const CHILD_HOME: &str = "PA_TEST_CHILD_HOME";
        if let Some(expected) = std::env::var_os(CHILD_HOME) {
            for &(variable, _, _) in DIRECTORIES.iter().chain(INHERITED_DIRECTORIES) {
                assert_eq!(std::env::var_os(variable).as_ref(), Some(&expected));
            }
            return;
        }
        let directory = std::env::temp_dir().join("explicit-child-home");
        std::fs::create_dir_all(&directory).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.args([
            "--exact",
            "private_home::tests::startup_keeps_explicit_child_overrides",
        ]);
        child.env(CHILD_HOME, &directory);
        for &(variable, _, _) in DIRECTORIES.iter().chain(INHERITED_DIRECTORIES) {
            child.env(variable, &directory);
        }
        assert!(child.status().unwrap().success());
        child.env_remove(ROOT_MARKER);
        assert!(child.status().unwrap().success());
    }
}
