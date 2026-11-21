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
        let keys: Vec<_> = std::env::vars_os().map(|(key, _)| key).collect();
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("PA_TEST_TIMING", if mode == "enabled" { "0" } else { "1" });
        let timing = pa_test_scratch::TestTiming::start("timing::after_environment_change");
        let _span = timing.span("after_environment_change");
        return;
    }

    let executable = std::env::current_exe().expect("test executable");
    let root = std::env::temp_dir().join(format!("timing-config-{}", std::process::id()));
    for mode in ["enabled", "disabled"] {
        let directory = root.join(mode);
        let output = directory.join("timings");
        std::fs::create_dir_all(&directory).expect("isolated child directory");
        let status = std::process::Command::new(&executable)
            .args([
                "--exact",
                "timing_configuration_survives_environment_changes",
                "--test-threads=1",
            ])
            .env_clear()
            .env("PA_TIMING_CONFIG_CHILD", mode)
            .env("PA_TEST_TIMING", if mode == "enabled" { "1" } else { "0" })
            .env("PA_TEST_TIMING_DIR", &output)
            .env("PA_TEST_SCRATCH_ROOT", directory.join("scratch"))
            .status()
            .expect("run isolated timing child");
        assert!(status.success(), "{mode} timing child failed");
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
            assert!(!output.exists(), "disabled timing created output");
        }
    }
}
