use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::{self, ThreadId};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

type Spans = Mutex<Vec<Value>>;

static RECORDERS: OnceLock<Mutex<HashMap<ThreadId, Weak<Spans>>>> = OnceLock::new();
static OUTPUT: OnceLock<Result<Mutex<File>, String>> = OnceLock::new();
static CONFIG: OnceLock<TimingConfig> = OnceLock::new();

struct TimingConfig {
    enabled: bool,
    directory: PathBuf,
}

pub(crate) fn capture_config() {
    let _ = config();
}

fn config() -> &'static TimingConfig {
    CONFIG.get_or_init(|| TimingConfig {
        enabled: !std::env::var_os("PA_TEST_TIMING").is_some_and(|value| value == "0"),
        directory: std::env::var_os("PA_TEST_TIMING_DIR").map_or_else(
            || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-timings"),
            PathBuf::from,
        ),
    })
}

pub struct TestTiming {
    state: Option<TestState>,
}

struct TestState {
    name: String,
    started: Instant,
    start_unix_ns: u128,
    thread: ThreadId,
    recorder: TimingRecorder,
}

#[derive(Clone, Default)]
pub struct TimingRecorder {
    spans: Option<Arc<Spans>>,
}

pub struct TimingSpan {
    state: Option<SpanState>,
}

struct SpanState {
    name: String,
    started: Instant,
    start_unix_ns: u128,
    spans: Arc<Spans>,
}

impl TestTiming {
    #[must_use]
    pub fn start(full_name: &str) -> Self {
        if !config().enabled {
            return Self { state: None };
        }
        let started = Instant::now();
        let start_unix_ns = unix_ns();
        let thread = thread::current();
        let normalized = full_name
            .split_once("::")
            .map_or(full_name, |(_, name)| name);
        let name = thread.name().unwrap_or(normalized).to_owned();
        let spans = Arc::new(Mutex::new(Vec::new()));
        RECORDERS
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(thread.id(), Arc::downgrade(&spans));
        Self {
            state: Some(TestState {
                name,
                started,
                start_unix_ns,
                thread: thread.id(),
                recorder: TimingRecorder { spans: Some(spans) },
            }),
        }
    }

    #[must_use]
    pub fn recorder(&self) -> TimingRecorder {
        self.state
            .as_ref()
            .map_or_else(TimingRecorder::default, |state| state.recorder.clone())
    }

    #[must_use]
    pub fn span(&self, name: &str) -> TimingSpan {
        self.recorder().span(name)
    }
}

impl TimingRecorder {
    #[must_use]
    pub fn span(&self, name: &str) -> TimingSpan {
        TimingSpan {
            state: self.spans.as_ref().map(|spans| SpanState {
                name: name.to_owned(),
                started: Instant::now(),
                start_unix_ns: unix_ns(),
                spans: Arc::clone(spans),
            }),
        }
    }
}

#[must_use]
pub fn span(name: &str) -> TimingSpan {
    let spans = RECORDERS.get().and_then(|recorders| {
        recorders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&thread::current().id())
            .and_then(Weak::upgrade)
    });
    TimingRecorder { spans }.span(name)
}

impl Drop for TimingSpan {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            let elapsed_ns = state.started.elapsed().as_nanos();
            let end_unix_ns = unix_ns();
            state
                .spans
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(json!({
                    "name": state.name,
                    "start_unix_ns": state.start_unix_ns,
                    "end_unix_ns": end_unix_ns,
                    "elapsed_ns": elapsed_ns,
                    "panicking": thread::panicking(),
                }));
        }
    }
}

impl Drop for TestTiming {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            let elapsed_ns = state.started.elapsed().as_nanos();
            let end_unix_ns = unix_ns();
            let panicking = thread::panicking();
            RECORDERS
                .get_or_init(Mutex::default)
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&state.thread);
            let spans = state
                .recorder
                .spans
                .as_ref()
                .expect("enabled timing has a recorder")
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let record = json!({
                "kind": "test",
                "test": state.name,
                "binary": binary_name(),
                "pid": std::process::id(),
                "start_unix_ns": state.start_unix_ns,
                "end_unix_ns": end_unix_ns,
                "elapsed_ns": elapsed_ns,
                "panicking": panicking,
                "outcome": if panicking { "panic" } else { "returned" },
                "spans": &*spans,
            });
            if let Err(error) = write_record(&record) {
                if panicking {
                    let _ = writeln!(io::stderr().lock(), "test timing write failed: {error}");
                } else {
                    panic!("test timing write failed: {error}");
                }
            }
        }
    }
}

fn unix_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn binary_name() -> String {
    std::env::current_exe()
        .expect("test executable path")
        .file_name()
        .expect("test executable file name")
        .to_string_lossy()
        .into_owned()
}

fn write_record(record: &Value) -> Result<(), String> {
    let output = OUTPUT.get_or_init(|| {
        let directory = &config().directory;
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        let binary = binary_name();
        let pid = std::process::id();
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join(format!("{binary}-{pid}.jsonl")))
            .map(Mutex::new)
            .map_err(|error| error.to_string())
    });
    let output = output.as_ref().map_err(Clone::clone)?;
    let mut file = output
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    serde_json::to_writer(&mut *file, record).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{TimingRecorder, TimingSpan};
    use std::sync::{Arc, Mutex};

    struct TeardownSpan(TimingRecorder);

    impl Drop for TeardownSpan {
        fn drop(&mut self) {
            let _span = self.0.span("teardown");
        }
    }

    #[crate::pa_test]
    #[test]
    fn spans_include_local_teardown_before_the_enclosing_scope_ends() {
        let spans = Arc::new(Mutex::new(Vec::new()));
        let recorder = TimingRecorder {
            spans: Some(Arc::clone(&spans)),
        };
        {
            let _scope = recorder.span("scope");
            let _local = TeardownSpan(recorder);
        }
        let recorded = spans.lock().expect("span lock");
        let names: Vec<_> = recorded.iter().map(|span| span["name"].as_str()).collect();
        assert_eq!(names, vec![Some("teardown"), Some("scope")]);
        assert!(recorded.iter().all(|span| span["panicking"] == false));
    }

    #[crate::pa_test]
    #[test]
    fn spans_capture_unwinding_without_requiring_a_test_thread_context() {
        let spans = Arc::new(Mutex::new(Vec::new()));
        let recorder = TimingRecorder {
            spans: Some(Arc::clone(&spans)),
        };
        let result = std::panic::catch_unwind(|| {
            let _span: TimingSpan = recorder.span("unwind");
            panic!("span unwind");
        });
        assert!(result.is_err());
        let recorded = spans.lock().expect("span lock");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0]["name"], "unwind");
        assert!(recorded[0]["panicking"].as_bool().expect("panic flag"));
    }
}
