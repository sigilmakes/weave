# pa-test-scratch

Test-only scratch ownership and timing. This crate must remain a dev-dependency;
production behavior, fixture changes, and test scheduling are out of scope.
Dependencies are `libc`, `serde_json`, and the leaf proc-macro crate `pa-test-macros`.

Linking owns the existing per-binary scratch directory lifecycle.

## Timing API

Place `#[pa_test_scratch::pa_test]` immediately before a normal `#[test]`.
For async tests, place it immediately after `#[tokio::test]`. Tokio forwards the
attribute to its generated synchronous test wrapper, so the timing guard covers
runtime setup, the async body, and runtime teardown. Putting the timing attribute
before Tokio would time only the async body.

The macro retains the function attributes and introduces `_pa_test_timing` as
the wrapper's first executable statement and first local, so it drops after
runtime and fixture locals. Leading item declarations stay in place; they do not
execute before the guard.

- `TestTiming::start(full_name)` starts a test manually.
- `_pa_test_timing.span("fixture")` returns an owned `TimingSpan`.
- `_pa_test_timing.recorder()` returns a cloneable `TimingRecorder`.
- `TimingRecorder::span(name)` starts an owned span from helpers or spawned tasks.

The recorder and spans can cross await points and threads.
`timing::span(name)` is a convenience for helpers running on the original test
thread, resolved through a thread-ID registry. It does not find test context
after task migration; use explicit recorders there. Finish all spawned work and
drop its spans before the test returns; the test guard writes its collected spans on drop.

Timing is enabled by default. `PA_TEST_TIMING=0` disables timing and file output
for overhead comparisons. `PA_TEST_TIMING_DIR` selects the destination; otherwise
it is the workspace `target/test-timings` directory. Each process appends to
`<binary>-<pid>.jsonl`, with writes serialized within that process.

Each JSON line has `kind: "test"`, `test` (libtest name without the crate prefix),
`binary`, `pid`, `start_unix_ns`, `end_unix_ns`, `elapsed_ns`, `panicking`,
`outcome` (`"panic"` or `"returned"`), and `spans`. Each span has `name`, the same
timestamps, elapsed duration, and panic flag. Durations use a monotonic clock; timestamps use the Unix clock. The panic
flag is unwind state, not the harness outcome (`should_panic` can pass).

Write failures panic during normal completion. During unwinding they are reported
to stderr without a second panic; the runner must reject the missing record.
