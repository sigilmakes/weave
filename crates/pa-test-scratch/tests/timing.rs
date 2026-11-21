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
