//! Tamper-evident SHA-256 hash-chain audit log ported from `noto-sds`.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::persist::write_atomic;

pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const ANCHORS_FILE: &str = "anchors.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogLine {
    pub seq: u64,
    pub ts: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick: Option<u64>,
    pub cat: String,
    pub level: Level,
    pub msg: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileLine {
    #[serde(flatten)]
    line: LogLine,
    prev: String,
    hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorEntry {
    pub session: String,
    pub file: String,
    pub seq: u64,
    pub hash: String,
    pub updated_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AnchorFile {
    #[serde(default)]
    last_session: Option<String>,
    #[serde(default)]
    sessions: BTreeMap<String, AnchorEntry>,
}

fn load_anchors(dir: &Path) -> AnchorFile {
    let p = dir.join(ANCHORS_FILE);
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str::<AnchorFile>(&t).ok())
        .unwrap_or_default()
}

fn save_anchor(dir: &Path, entry: AnchorEntry) -> std::io::Result<()> {
    let mut af = load_anchors(dir);
    af.last_session = Some(entry.session.clone());
    af.sessions.insert(entry.session.clone(), entry);
    let txt = serde_json::to_string_pretty(&af).unwrap_or_default();
    write_atomic(&dir.join(ANCHORS_FILE), txt.as_bytes())
}

fn chain_hash(prev: &str, line: &LogLine) -> String {
    let body = serde_json::to_string(line).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(b"|");
    h.update(body.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifyOk {
    pub lines: usize,
    pub last_hash: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifyError {
    pub line_no: usize,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionVerifyReport {
    pub session: String,
    pub file: String,
    pub lines: usize,
    pub chain_ok: bool,
    pub broken_at: Option<usize>,
    pub reason: Option<String>,
    pub anchor_ok: Option<bool>,
    pub seal_ok: Option<bool>,
    pub linked_from: Option<String>,
}

pub fn verify_file(path: &Path) -> Result<VerifyOk, VerifyError> {
    let f = File::open(path).map_err(|e| VerifyError { line_no: 0, reason: format!("open: {e}") })?;
    let mut prev = GENESIS.to_string();
    let mut count = 0usize;
    let mut last_seq = 0u64;
    for (i, raw) in BufReader::new(f).lines().enumerate() {
        let line_no = i + 1;
        let raw = raw.map_err(|e| VerifyError { line_no, reason: format!("read: {e}") })?;
        if raw.trim().is_empty() {
            continue;
        }
        let fl: FileLine = serde_json::from_str(&raw).map_err(|e| VerifyError { line_no, reason: format!("parse: {e}") })?;
        if fl.prev != prev {
            return Err(VerifyError {
                line_no,
                reason: "previous-hash mismatch (a line was removed, inserted or reordered)".into(),
            });
        }
        if fl.line.seq != last_seq + 1 {
            return Err(VerifyError {
                line_no,
                reason: format!("sequence jumps from {} to {}", last_seq, fl.line.seq),
            });
        }
        let expect = chain_hash(&prev, &fl.line);
        if fl.hash != expect {
            return Err(VerifyError {
                line_no,
                reason: "hash mismatch (the line content was edited)".into(),
            });
        }
        prev = fl.hash;
        last_seq = fl.line.seq;
        count += 1;
    }
    Ok(VerifyOk { lines: count, last_hash: prev })
}

pub fn verify_dir(log_dir: &Path, sds_digests: Option<&BTreeMap<String, String>>) -> Vec<SessionVerifyReport> {
    let anchors = load_anchors(log_dir);
    let mut files: Vec<PathBuf> = std::fs::read_dir(log_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect();
    files.sort();

    let mut out = Vec::with_capacity(files.len());
    let total = files.len();
    for (idx, path) in files.iter().enumerate() {
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let session = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let mut linked_from = None;
        let mut last_seal: Option<BTreeMap<String, String>> = None;
        if let Ok(txt) = std::fs::read_to_string(path) {
            for raw in txt.lines() {
                if let Ok(fl) = serde_json::from_str::<FileLine>(raw) {
                    if fl.line.cat == "session_link" && linked_from.is_none() {
                        linked_from = fl
                            .line
                            .data
                            .as_ref()
                            .and_then(|d| d.get("prev_file"))
                            .and_then(|v| v.as_str())
                            .map(String::from);
                    }
                    if fl.line.cat == "sds_seal" {
                        if let Some(map) = fl.line.data.as_ref().and_then(|d| d.get("digests")).and_then(|v| v.as_object()) {
                            let mut parsed = BTreeMap::new();
                            for (k, v) in map {
                                if let Some(s) = v.as_str() {
                                    parsed.insert(k.clone(), s.to_string());
                                }
                            }
                            last_seal = Some(parsed);
                        }
                    }
                }
            }
        }
        match verify_file(path) {
            Ok(ok) => {
                let anchor_ok = anchors
                    .sessions
                    .get(&session)
                    .map(|a| ok.lines as u64 == a.seq && ok.last_hash == a.hash);
                let seal_ok = if idx + 1 == total {
                    match (last_seal.as_ref(), sds_digests) {
                        (Some(sealed), Some(cur)) => Some(sealed.iter().all(|(k, v)| cur.get(k) == Some(v))),
                        _ => None,
                    }
                } else {
                    last_seal.as_ref().map(|_| true)
                };
                out.push(SessionVerifyReport {
                    session,
                    file: fname,
                    lines: ok.lines,
                    chain_ok: true,
                    broken_at: None,
                    reason: None,
                    anchor_ok,
                    seal_ok,
                    linked_from,
                });
            }
            Err(err) => {
                out.push(SessionVerifyReport {
                    session,
                    file: fname,
                    lines: err.line_no.saturating_sub(1),
                    chain_ok: false,
                    broken_at: Some(err.line_no),
                    reason: Some(err.reason),
                    anchor_ok: Some(false),
                    seal_ok: Some(false),
                    linked_from,
                });
            }
        }
    }
    out
}

pub struct AuditLog {
    ring: VecDeque<LogLine>,
    cap: usize,
    next_seq: u64,
    prev_hash: String,
    session: String,
    file: Option<(PathBuf, BufWriter<File>)>,
    pub write_error: Option<String>,
    anchor_dirty: bool,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl AuditLog {
    pub fn memory(cap: usize) -> Self {
        Self {
            ring: VecDeque::new(),
            cap: cap.max(16),
            next_seq: 1,
            prev_hash: GENESIS.into(),
            session: "memory".into(),
            file: None,
            write_error: None,
            anchor_dirty: false,
        }
    }

    pub fn open(dir: &Path, session: &str, cap: usize) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let safe: String = session
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        let path = dir.join(format!("{safe}.jsonl"));
        let prev_anchor = {
            let af = load_anchors(dir);
            af.last_session.as_deref().and_then(|s| af.sessions.get(s).cloned())
        };
        let f = OpenOptions::new().create_new(true).append(true).open(&path)?;
        let mut log = Self::memory(cap);
        log.session = safe;
        log.file = Some((path, BufWriter::new(f)));
        if let Some(prev) = prev_anchor {
            log.log(
                "session_link",
                Level::Info,
                format!("linked to {} (seq {})", prev.file, prev.seq),
                Some(serde_json::json!({
                    "prev_session": prev.session,
                    "prev_file": prev.file,
                    "prev_seq": prev.seq,
                    "prev_hash": prev.hash,
                })),
            );
        }
        Ok(log)
    }

    pub fn path(&self) -> Option<&Path> {
        self.file.as_ref().map(|(p, _)| p.as_path())
    }

    pub fn dir(&self) -> Option<&Path> {
        self.file.as_ref().and_then(|(p, _)| p.parent())
    }

    pub fn sync_anchor(&mut self) {
        if self.last_seq() == 0 {
            return;
        }
        let Some((path, _)) = &self.file else { return };
        let Some(dir) = path.parent() else { return };
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let entry = AnchorEntry {
            session: self.session.clone(),
            file: fname,
            seq: self.last_seq(),
            hash: self.prev_hash.clone(),
            updated_ms: now_ms(),
        };
        if let Err(e) = save_anchor(dir, entry) {
            self.write_error = Some(format!("anchor: {e}"));
        } else {
            self.anchor_dirty = false;
        }
    }

    pub fn log_with_tick(&mut self, cat: &str, level: Level, msg: impl Into<String>, data: Option<serde_json::Value>, tick: Option<u64>) -> u64 {
        let line = LogLine { seq: self.next_seq, ts: now_ms(), tick, cat: cat.to_string(), level, msg: msg.into(), data };
        let hash = chain_hash(&self.prev_hash, &line);
        if let Some((_, w)) = self.file.as_mut() {
            let fl = FileLine { line: line.clone(), prev: self.prev_hash.clone(), hash: hash.clone() };
            let res = serde_json::to_string(&fl).map_err(|e| e.to_string()).and_then(|s| {
                writeln!(w, "{s}").and_then(|_| w.flush()).map_err(|e| e.to_string())
            });
            if let Err(e) = res {
                let err_msg = e.clone();
                self.write_error = Some(e);
                self.ring.push_back(LogLine {
                    seq: self.next_seq,
                    ts: now_ms(),
                    tick,
                    cat: "audit".into(),
                    level: Level::Warn,
                    msg: format!("audit log write error: {err_msg}"),
                    data: None,
                });
            }
        }
        self.prev_hash = hash;
        self.next_seq += 1;
        let seq = line.seq;
        self.ring.push_back(line);
        while self.ring.len() > self.cap {
            self.ring.pop_front();
        }
        self.anchor_dirty = true;
        if self.file.is_some()
            && (seq == 1 || seq % 16 == 0 || matches!(cat, "session_link" | "sds_seal" | "sds_purge" | "sds_prune" | "replay"))
        {
            self.sync_anchor();
        }
        seq
    }

    pub fn log(&mut self, cat: &str, level: Level, msg: impl Into<String>, data: Option<serde_json::Value>) -> u64 {
        self.log_with_tick(cat, level, msg, data, None)
    }

    pub fn info(&mut self, cat: &str, msg: impl Into<String>) -> u64 {
        self.log(cat, Level::Info, msg, None)
    }

    pub fn warn(&mut self, cat: &str, msg: impl Into<String>) -> u64 {
        self.log(cat, Level::Warn, msg, None)
    }

    pub fn last_seq(&self) -> u64 {
        self.next_seq - 1
    }

    pub fn last_hash(&self) -> &str {
        &self.prev_hash
    }

    pub fn tail(&self, after_seq: u64, limit: usize, cat: Option<&str>) -> (Vec<LogLine>, u64) {
        let limit = limit.max(1);
        let mut out = Vec::new();
        let mut next = after_seq;
        for l in self.ring.iter().filter(|l| l.seq > after_seq) {
            if out.len() >= limit {
                break;
            }
            next = l.seq;
            if cat.map_or(true, |c| c == l.cat) {
                out.push(l.clone());
            }
        }
        (out, next)
    }
}

impl Drop for AuditLog {
    fn drop(&mut self) {
        if self.anchor_dirty && self.file.is_some() {
            self.sync_anchor();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pd_audit_{tag}_{}_{}", std::process::id(), now_ms()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn audit_log_hash_chain_detects_tampering() {
        let dir = tmpdir("tamper");
        let mut log = AuditLog::open(&dir, "s1", 64).unwrap();
        log.info("sds", "boot");
        log.log("gate", Level::Warn, "blocked", Some(serde_json::json!({"severity": 72})));
        log.info("sds", "close");
        let path = log.path().unwrap().to_path_buf();
        let expected_hash = log.last_hash().to_string();
        drop(log);

        let ok = verify_file(&path).unwrap();
        assert_eq!((ok.lines, ok.last_hash), (3, expected_hash));

        let txt = std::fs::read_to_string(&path).unwrap();
        let edited = txt.replace("blocked", "allowed");
        assert_ne!(txt, edited);
        std::fs::write(&path, edited).unwrap();
        let err = verify_file(&path).unwrap_err();
        assert_eq!(err.line_no, 2);
        assert!(err.reason.contains("edited"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
