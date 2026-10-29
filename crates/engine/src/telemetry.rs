//! Structured events for diagnosis across both Macs.
//!
//! The daemon calls [`init`]; every event then goes to `events.jsonl` in the
//! state directory (two files of at most [`FILE_LIMIT`] bytes) and to an
//! in-memory ring the peer can ask for (`Request::Diag`) without touching the
//! store – so it answers even while the store is blocked. Without `init`
//! (CLI, tests) events are dropped.
//!
//! One JSON object per line: `t` (wall clock, ms), `kind`, then the fields.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Map, Value};

/// Size of one log file; the previous one is kept as `events.1.jsonl`.
pub const FILE_LIMIT: u64 = 8 * 1024 * 1024;
/// Events kept in memory for the peer.
const RING: usize = 20_000;

struct Sink {
    path: PathBuf,
    file: Option<File>,
    written: u64,
    ring: VecDeque<(i64, String)>,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

/// Start recording into `dir/events.jsonl`. Later calls are ignored.
pub fn init(dir: &Path) {
    let path = dir.join("events.jsonl");
    let file = OpenOptions::new().create(true).append(true).open(&path).ok();
    let written = file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
    let _ = SINK.set(Mutex::new(Sink { path, file, written, ring: VecDeque::with_capacity(RING) }));
}

pub fn enabled() -> bool {
    SINK.get().is_some()
}

/// Record an event. `fields` must be a JSON object (other values are wrapped).
pub fn event(kind: &str, fields: Value) {
    let Some(sink) = SINK.get() else { return };
    let t = now_ms();
    let mut obj = Map::new();
    obj.insert("t".into(), json!(t));
    obj.insert("kind".into(), json!(kind));
    match fields {
        Value::Object(m) => obj.extend(m),
        Value::Null => {}
        other => {
            obj.insert("value".into(), other);
        }
    }
    let line = Value::Object(obj).to_string();
    let mut s = sink.lock().unwrap_or_else(|e| e.into_inner());
    if s.written + line.len() as u64 + 1 > FILE_LIMIT {
        s.rotate();
    }
    let n = line.len() as u64 + 1;
    if let Some(f) = s.file.as_mut() {
        if writeln!(f, "{line}").is_ok() {
            s.written += n;
        }
    }
    if s.ring.len() == RING {
        s.ring.pop_front();
    }
    s.ring.push_back((t, line));
}

/// Events at or after `since_ms` (wall clock), at most `max`, oldest first.
pub fn since(since_ms: i64, max: usize) -> Vec<String> {
    let Some(sink) = SINK.get() else { return Vec::new() };
    let s = sink.lock().unwrap_or_else(|e| e.into_inner());
    let from = s.ring.partition_point(|(t, _)| *t < since_ms);
    let skip = (s.ring.len() - from).saturating_sub(max);
    s.ring.iter().skip(from + skip).map(|(_, l)| l.clone()).collect()
}

/// Events from the log files in `dir` (CLI without a running daemon).
pub fn read_files(dir: &Path, since_ms: i64) -> Vec<String> {
    let mut out = Vec::new();
    for name in ["events.1.jsonl", "events.jsonl"] {
        let Ok(text) = std::fs::read_to_string(dir.join(name)) else { continue };
        for line in text.lines() {
            let t = serde_json::from_str::<Value>(line).ok().and_then(|v| v["t"].as_i64());
            if t.is_some_and(|t| t >= since_ms) {
                out.push(line.to_string());
            }
        }
    }
    out
}

pub fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl Sink {
    fn rotate(&mut self) {
        self.file = None;
        let _ = std::fs::rename(&self.path, self.path.with_file_name("events.1.jsonl"));
        self.file = OpenOptions::new().create(true).truncate(true).write(true).open(&self.path).ok();
        self.written = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_filters_and_rotates() {
        let d = tempfile::tempdir().unwrap();
        init(d.path());
        assert!(enabled());
        let t0 = now_ms();
        event("test", json!({"n": 1, "unit": "u"}));
        let got = since(t0, 10);
        let v: Value = serde_json::from_str(got.last().unwrap()).unwrap();
        assert_eq!((v["kind"].as_str(), v["n"].as_i64(), v["unit"].as_str()), (Some("test"), Some(1), Some("u")));
        assert!(since(now_ms() + 60_000, 10).is_empty());
        assert_eq!(read_files(d.path(), t0).len(), got.len());
        // Fill past the file limit: the old file becomes events.1.jsonl.
        let big = "x".repeat(64 * 1024);
        for _ in 0..(FILE_LIMIT / (64 * 1024) + 2) {
            event("fill", json!({ "pad": big }));
        }
        assert!(d.path().join("events.1.jsonl").exists());
        assert!(std::fs::metadata(d.path().join("events.jsonl")).unwrap().len() < FILE_LIMIT);
        assert_eq!(since(0, 3).len(), 3, "limited to max, newest kept");
    }
}
