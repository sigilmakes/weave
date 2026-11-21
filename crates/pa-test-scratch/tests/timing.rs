use pa_test_scratch::timing;

#[pa_test_scratch::pa_test]
#[test]
fn timing_keeps_owned_spans_when_work_moves_threads() {
    let recorder = _pa_test_timing.recorder();
    let span = recorder.span("across_threads");
    std::thread::spawn(move || {
        let _span = span;
        let _nested = recorder.span("worker");
    })
    .join()
    .expect("timed worker returns normally");
}

#[pa_test_scratch::pa_test]
#[test]
#[should_panic(expected = "timed test panic")]
fn timing_retains_should_panic_behavior() {
    let _span = timing::span("unwind");
    panic!("timed test panic");
}

#[pa_test_scratch::pa_test]
#[test]
fn timing_configuration_survives_environment_changes() {
    if let Ok(mode) = std::env::var("PA_TIMING_CONFIG_CHILD") {
        let late_directory =
            std::env::var_os("PA_TIMING_CONFIG_LATE_DIR").expect("late timing destination");
        let keys: Vec<_> = std::env::vars_os().map(|(key, _)| key).collect();
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("PA_TEST_TIMING", if mode == "enabled" { "0" } else { "1" });
        std::env::set_var("PA_TEST_TIMING_DIR", late_directory);
        assert_eq!(
            pa_test_scratch::TestTiming::is_enabled(),
            mode == "enabled" || mode == "invalid"
        );
        let timing = pa_test_scratch::TestTiming::start("timing::after_environment_change");
        let _span = timing.span("after_environment_change");
        return;
    }

    let executable = std::env::current_exe().expect("test executable");
    let binary = executable
        .file_name()
        .expect("test binary name")
        .to_string_lossy();
    let root = std::env::temp_dir().join(format!("timing-config-{}", std::process::id()));
    for mode in ["enabled", "disabled", "unset", "empty", "invalid"] {
        let directory = root.join(mode);
        let output = directory.join("timings");
        std::fs::create_dir_all(&directory).expect("isolated child directory");
        let invalid = directory.join("not-a-directory");
        std::fs::write(&invalid, "blocking file").expect("invalid output destination");
        let mut command = std::process::Command::new(&executable);
        command
            .args([
                "--exact",
                "timing_configuration_survives_environment_changes",
                "--test-threads=1",
            ])
            .env_clear()
            .env("PA_TIMING_CONFIG_CHILD", mode)
            .env("PA_TIMING_CONFIG_LATE_DIR", &invalid)
            .env("PA_TEST_TIMING", if mode == "disabled" { "0" } else { "1" })
            .env("PA_TEST_SCRATCH_ROOT", directory.join("scratch"))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        match mode {
            "enabled" | "disabled" => {
                command.env("PA_TEST_TIMING_DIR", &output);
            }
            "unset" => {}
            "empty" => {
                command.env("PA_TEST_TIMING_DIR", "");
            }
            "invalid" => {
                command.env("PA_TEST_TIMING_DIR", &invalid);
            }
            _ => unreachable!("known timing configuration mode"),
        }
        let child = command.spawn().expect("run isolated timing child");
        let pid = child.id();
        let result = child.wait_with_output().expect("wait for timing child");
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        if mode == "invalid" {
            assert!(
                !result.status.success(),
                "invalid timing destination succeeded"
            );
            assert!(
                log.contains("test timing write failed:"),
                "missing loud failure: {log}"
            );
        } else {
            assert!(result.status.success(), "{mode} timing child failed: {log}");
        }
        if mode == "enabled" {
            let files: Vec<_> = std::fs::read_dir(&output)
                .expect("original timing destination")
                .map(|entry| entry.expect("timing output entry").path())
                .collect();
            assert_eq!(files.len(), 1);
            let contents = std::fs::read_to_string(&files[0]).expect("timing records");
            let records: Vec<serde_json::Value> = contents
                .lines()
                .map(|line| serde_json::from_str(line).expect("timing JSON"))
                .collect();
            assert_eq!(records.len(), 2);
            assert!(records.iter().all(|record| {
                record["test"] == "timing_configuration_survives_environment_changes"
                    && record["outcome"] == "returned"
            }));
        } else {
            assert!(!output.exists(), "inactive timing created output");
        }
        let legacy_output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-timings")
            .join(format!("{binary}-{pid}.jsonl"));
        assert!(
            !legacy_output.exists(),
            "timing used the removed default destination"
        );
    }
}
