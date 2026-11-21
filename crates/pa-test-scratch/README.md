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

## Developer runner

From the workspace root, use a credential-free Cargo cache already populated for
`--locked --offline`, and a kernel venv synced and readiness-qualified from this
checkout. Export `CARGO_HOME` and `CARGO_TARGET_DIR` as absolute paths. Set
`PRIME_AGENT_KERNEL_PYTHON`, `PA_CORE_KERNEL_PYTHON`, and `PA_E2E_KERNEL_PYTHON`
to that same venv interpreter. No live credentials are inherited by test targets.

```console
uv run scripts/test_timings.py run --slots 8 --audit --output /tmp/weave-timing-run
uv run scripts/test_timings.py run --slots 8 --package pa-core --audit
uv run scripts/test_timings.py run --slots 1 --package pa-daemon --target context_latency_e2e
uv run scripts/test_timings.py table /tmp/weave-timing-run
```

The runner builds the selected default test artifacts once and executes whole
binaries with their crate working directories and native libtest concurrency.
It never filters individual tests or retries failures. Each binary gets private
home, agent, socket, and temporary paths. `kernel_dispose_e2e` and
`replacement_kernel_e2e` run exclusively after all other binaries because their
fixtures inspect the machine-wide kernel process census. Workspace/crate runs
finish with native Cargo doctests; `--target` selects integration binaries only.

Slots `1`, `8`, and `16` are measurement options, not an assertion that more slots
are faster. The summary lists every target's wall, user CPU, and system CPU time.
It separately reports execution wall time and total wall time including the warm
build and inventory preparation. This Unix runner requires `wait4` for per-child
CPU accounting. Retained manifests contain commands, isolated environments,
load snapshots, harness logs, and the original sidecars.

Coverage compares the default harness's listed nonignored tests and completed
test statuses with the launched process's records. Missing or duplicate records
fail integration and `pa-core`/`pa-daemon` unit runs, including failed and
expected-panic tests. Captured test stdout is not used. `--audit` checks every
integration source test attribute and the two selected crates' unit source.
Other crates' unit tests without attributes and doctests are reported as
uncovered, with crate and unit names retained in the manifest;
`--require-all-timing` additionally makes missing unit timing fail. Ignored tests
keep the native default and are not executed.

The sorted per-test table includes named span totals and unattributed duration.
Overlapping spans are unioned when computing coverage, so nested spans do not
make unattributed time negative. Unattributed time is not classified as computation.
A directory of raw JSONL sidecars also works with `table`, but harness outcomes
and inventory coverage are unknown without a runner manifest. Target wall and
CPU time are not invented from test timestamps. CPU accounting uses `wait4` and
includes reaped child work; still-running detached descendants are not included.

Use `--disable-timing` on the same built target for the collector overhead control;
this retains the macro's disabled guard branch, so compare with a native baseline
as well when reporting total instrumentation overhead.
