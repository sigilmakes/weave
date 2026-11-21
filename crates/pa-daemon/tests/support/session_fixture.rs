use std::io::{BufWriter, Write};
use std::path::Path;

pub struct BulkSessionFixture {
    writer: BufWriter<std::fs::File>,
    rows: u64,
    bytes: u64,
}

impl BulkSessionFixture {
    pub fn new(path: &Path) -> Self {
        let file = std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("create static session fixture");
        Self {
            writer: BufWriter::new(file),
            rows: 0,
            bytes: 0,
        }
    }

    pub fn push(&mut self, row: &serde_json::Value) {
        let bytes = serde_json::to_vec(row).expect("serialize fixture row");
        let decoded = pa_core::session_log::Row::decode(&bytes, self.rows)
            .expect("fixture row must pass the real log decoder");
        assert_eq!(&decoded.body, row);
        self.writer.write_all(&bytes).expect("write fixture row");
        self.writer.write_all(b"\n").expect("terminate fixture row");
        self.rows = decoded.seq;
        self.bytes += bytes.len() as u64 + 1;
    }

    pub fn finish(mut self) -> u64 {
        self.writer.flush().expect("flush static session fixture");
        assert_eq!(
            self.writer.get_ref().metadata().unwrap().len(),
            self.bytes,
            "fixture contains every serialized byte"
        );
        self.rows
    }
}

pub fn assert_durable_equivalent(path: &Path) {
    use pa_core::session_log::{Outcome, SessionLog};
    let bytes = std::fs::read(path).expect("read bulk fixture");
    let dir = tempfile::tempdir().expect("equivalence directory");
    let id = path.file_stem().unwrap().to_str().unwrap();
    let deadline = || std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut log = SessionLog::open(dir.path(), id, deadline()).expect("old durable fixture log");
    for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        assert_eq!(line.last(), Some(&b'\n'));
        let row = pa_core::session_log::Row::decode(&line[..line.len() - 1], index as u64)
            .expect("bulk fixture decodes in row order");
        let outcome = log.append(&row.body, deadline());
        assert!(matches!(outcome, Outcome::Committed { .. }), "{outcome:?}");
    }
    let durable =
        std::fs::read(dir.path().join(format!("{id}.jsonl"))).expect("read old durable fixture");
    assert_eq!(bytes.len(), durable.len(), "fixture file size");
    assert_eq!(bytes, durable, "bulk and old durable bytes");
    eprintln!("byte-identical fixture {id}: {} bytes", durable.len());
}

#[test]
fn bulk_serialization_matches_durable_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("representative.jsonl");
    let mut fixture = BulkSessionFixture::new(&path);
    for row in [
        serde_json::json!({"type":"session","version":3,"id":"s","cwd":"/tmp","timestamp":"2024-01-01T00:00:00.000Z"}),
        serde_json::json!({"type":"message","id":"m","parentId":null,"message":{"role":"assistant","content":"世界🚀\nquoted \" text","usage":{"input":1000,"cost":{"total":2}}}}),
        serde_json::json!({"type":"session_info","id":"i","name":"name"}),
    ] {
        fixture.push(&row);
    }
    assert_eq!(fixture.finish(), 3);
    assert_durable_equivalent(&path);
}
