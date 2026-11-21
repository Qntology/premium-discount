//! The app side: a typed [`PdSyncClient`] over `zena_avatar_mcp::protocol::McpClient`, and [`SyncLoop`], the
//! coalesced background sync of premium-discount's `web/desktop/coalesce.ts` + `main.ts`:
//!
//! * a request while idle runs at once;
//! * requests while a run is in progress are not dropped: they share exactly one rerun after it, and the rerun
//!   flushes if any of them asked to;
//! * while any row is `queued` or `pending`, a run with flush is due every `interval` (10 s in the app).
//!
//! The drop handler, opening the history, the "check now" button and the timer can all call
//! [`SyncLoop::request`]; the server sees one `pd_flush_and_sync` at a time. Offline, the flush stops (the
//! outbox keeps its rows) and the local rows still come back, so the history keeps working.
//!
//! Lossless rules (Scenario 4 Part 4):
//!
//! * a vote queued while nothing else waits still starts the loop ([`SyncLoop::notify_vote_queued`]) and keeps the
//!   timer running until `votes_remaining` is 0 — the flush summary, or `queued_votes` on a run without flush;
//! * 401 / 403 (`auth_required`) pause the timer instead of hammering the server; the next explicit request (after
//!   signing in again) resumes it. 429 (`retry_after_ms`) delays the next timer run. Neither burns an item;
//! * rejections (dictionary, model, the server's cron, a rejected parent, a refused vote) are kept in
//!   [`SyncSnapshot::unacked_rejections`] until the overlay acknowledges them, so the next run cannot overwrite them.
//!
//! While an interactive search runs (Scenario 3 Part 2), [`SyncLoop::hold_for_interactive`] keeps timer and requested
//! runs from taking the shared client; [`try_lock_for`] bounds how long the search waits for a run already started.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use premium_discount_mcp_protocol::{result_json, McpClient};

use crate::server::error_kind;

/// Longer than the server's own wait for premium-discount-app (180 s), so its error arrives first.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(200);
/// `main.ts`: the refresh period while something waits to be sent or checked.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(10);

pub struct PdSyncClient {
    mcp: McpClient,
}

impl PdSyncClient {
    /// Spawns `premium-discount-mcp args…` and runs the MCP handshake.
    pub fn spawn(program: &Path, args: &[String]) -> Result<PdSyncClient> {
        Ok(PdSyncClient {
            mcp: McpClient::spawn(program, args)?,
        })
    }

    pub fn from_client(mcp: McpClient) -> PdSyncClient {
        PdSyncClient { mcp }
    }

    /// Calls `tool` and returns its structured result. A tool error is `Err("<tool>: <kind>: <message>")`.
    pub fn call(&mut self, tool: &str, args: Value) -> Result<Value> {
        Ok(result_json(&self.mcp.call_tool(tool, args, TOOL_TIMEOUT)?))
    }

    pub fn enqueue(&mut self, node: Value) -> Result<Value> {
        self.call("pd_enqueue_node", node)
    }

    pub fn flush_and_sync(&mut self, since_ms: i64, flush: bool) -> Result<Value> {
        self.call("pd_flush_and_sync", json!({"since_ms": since_ms, "flush": flush}))
    }

    pub fn search_merged(&mut self, query: Value) -> Result<Value> {
        self.call("pd_search_merged", query)
    }
}

/// One sync run: flush (when asked) and read the rows changed after `since_ms`.
pub trait SyncRunner: Send + 'static {
    fn run(&mut self, since_ms: i64, flush: bool) -> Result<Value>;
}

impl SyncRunner for PdSyncClient {
    fn run(&mut self, since_ms: i64, flush: bool) -> Result<Value> {
        self.flush_and_sync(since_ms, flush)
    }
}

/// Shares the client with the app's other calls (enqueue, search).
impl<R: SyncRunner> SyncRunner for Arc<Mutex<R>> {
    fn run(&mut self, since_ms: i64, flush: bool) -> Result<Value> {
        self.lock()
            .map_err(|_| anyhow::anyhow!("sync client lock poisoned"))?
            .run(since_ms, flush)
    }
}

/// How many unacknowledged rejections [`SyncSnapshot`] keeps (the oldest goes first).
pub const MAX_UNACKED_REJECTIONS: usize = 32;

/// A rejection the user has not seen yet (E-16). Kept across runs until [`SyncSnapshot::drain_unacked_rejections`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectionNotice {
    /// The evaluation's `local_id`, or the vote's `request_id`.
    pub item_key: String,
    /// The evaluation's server id (votes only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval_id: Option<String>,
    /// `evaluation` | `reply_cascade` (its parent was rejected) | `vote`.
    pub kind: String,
    /// `profanity` | `nsfw` | `illegal` | `parent_rejected` | `parent_missing` | `server_failed` | `rejected` | `http_<status>` | `other`.
    pub reason_code: String,
    /// The same Korean label as premium-discount's web UI (`web/desktop/view.ts` `reasonLabel`).
    pub label_ko: String,
}

const REASONS: [(&str, &str); 5] = [
    ("profanity", "욕설"),
    ("nsfw", "음란·폭력"),
    ("illegal", "불법·개인정보"),
    ("parent rejected", "답글을 단 글이 거부됨"),
    ("parent missing", "답글을 단 글을 찾을 수 없음"),
];

/// `server status <rejected|failed>[: detail]` → (status, detail).
fn server_status(reason: &str) -> Option<(&str, Option<&str>)> {
    let rest = reason.strip_prefix("server status ")?;
    let (status, detail) = match rest.split_once(':') {
        Some((s, d)) => (s, Some(d.trim()).filter(|d| !d.is_empty())),
        None => (rest, None),
    };
    matches!(status, "rejected" | "failed").then_some((status, detail))
}

/// `parent <id> was rejected`.
fn names_rejected_parent(reason: &str) -> bool {
    reason.starts_with("parent ") && reason.ends_with(" was rejected") && reason.len() > "parent  was rejected".len()
}

/// `server returned <3 digits>…` → the status.
fn http_status(reason: &str) -> Option<&str> {
    let code = reason.strip_prefix("server returned ")?.get(..3)?;
    code.bytes().all(|b| b.is_ascii_digit()).then_some(code)
}

/// premium-discount's `reasonLabel` (`web/desktop/view.ts`): the Korean label the web UI shows for a rejection reason.
pub fn reason_label(reason: &str) -> String {
    let mut r = reason.trim();
    if r.is_empty() {
        return String::new();
    }
    if let Some((status, detail)) = server_status(r) {
        match detail {
            None if status == "failed" => return "서버가 처리하지 못했습니다 (재시도 한도 초과)".into(),
            None => return "서버가 거부했습니다".into(),
            Some(d) => r = d,
        }
    }
    if names_rejected_parent(r) {
        return "답글을 단 글이 거부됨".into();
    }
    if let Some(code) = http_status(r) {
        return format!("서버가 요청을 받아들이지 않았습니다 ({code})");
    }
    r.split(',')
        .map(|x| {
            let x = x.trim();
            REASONS.iter().find(|(k, _)| *k == x).map_or(x, |(_, v)| v)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A short machine code for a rejection reason (the first reason when the server lists several).
pub fn reason_code(reason: &str) -> String {
    let r = reason.trim();
    let detail = match server_status(r) {
        Some(("failed", None)) => return "server_failed".into(),
        Some((_, None)) => return "rejected".into(),
        Some((_, Some(d))) => d,
        None => r,
    };
    if names_rejected_parent(detail) || detail == "parent rejected" {
        return "parent_rejected".into();
    }
    if detail == "parent missing" {
        return "parent_missing".into();
    }
    if let Some(code) = http_status(detail) {
        return format!("http_{code}");
    }
    let first = detail.split(',').next().unwrap_or("").trim();
    match first {
        "profanity" | "nsfw" | "illegal" => first.into(),
        _ => "other".into(),
    }
}

fn is_hold(stopped: &str) -> bool {
    stopped.starts_with("auth_required") || stopped.starts_with("rate_limited")
}

/// What the overlay shows: the local rows (by `request_id`, like the web UI's Dexie cache) and the last run.
#[derive(Debug, Clone, Default)]
pub struct SyncSnapshot {
    pub rows: BTreeMap<String, Value>,
    pub since_ms: i64,
    /// The last flush summary (`sent`, `remaining`, `stopped`, `stored`, `still_pending` …).
    pub last_flush: Option<Value>,
    pub last_error: Option<String>,
    /// The last run could not reach the server (or premium-discount-app).
    pub offline: bool,
    pub runs: u64,
    /// Votes still waiting to be sent (the last flush's `votes_remaining`, or `queued_votes` on a run without flush).
    pub votes_remaining: usize,
    /// A vote was queued after the last run ([`SyncLoop::notify_vote_queued`]): run even if no row waits.
    pub vote_hint: bool,
    /// The server answered 401 / 403: the timer pauses until a run gets through (sign in, then request a run).
    pub auth_required: bool,
    /// The server answered 429: no timer run before this.
    pub retry_at: Option<Instant>,
    /// Rejections not acknowledged yet (newest last, at most [`MAX_UNACKED_REJECTIONS`]).
    pub unacked_rejections: VecDeque<RejectionNotice>,
}

impl SyncSnapshot {
    pub fn count(&self, state: &str) -> usize {
        self.rows
            .values()
            .filter(|r| r.get("state").and_then(Value::as_str) == Some(state))
            .count()
    }

    /// Something waits to be sent (`queued`) or checked (`pending`), or a vote waits — unless the server wants a
    /// new sign-in first (then only an explicit request runs).
    pub fn has_work(&self) -> bool {
        if self.auth_required {
            return false;
        }
        self.count("queued") + self.count("pending") > 0 || self.votes_remaining > 0 || self.vote_hint
    }

    /// The Stage 0 status dot: `attention` (unseen rejections or sign-in needed), then the least settled state present.
    pub fn status_dot(&self) -> Option<&'static str> {
        if self.auth_required || !self.unacked_rejections.is_empty() {
            return Some("attention");
        }
        ["queued", "pending", "stored"].into_iter().find(|s| self.count(s) > 0)
    }

    /// Hands the unseen rejections to the overlay (Stage 1 chip / Stage 2 drawer opened) and forgets them.
    pub fn drain_unacked_rejections(&mut self) -> Vec<RejectionNotice> {
        self.unacked_rejections.drain(..).collect()
    }

    fn note_rejection(&mut self, notice: RejectionNotice) {
        if self.unacked_rejections.iter().any(|n| n.item_key == notice.item_key) {
            return;
        }
        if self.unacked_rejections.len() >= MAX_UNACKED_REJECTIONS {
            self.unacked_rejections.pop_front();
        }
        self.unacked_rejections.push_back(notice);
    }

    /// Collects `rejected`, `sync_rejected` and `votes_rejected` of a flush summary before it replaces `last_flush`.
    fn collect_rejections(&mut self, flush: &Value) {
        for key in ["rejected", "sync_rejected"] {
            for r in flush.get(key).and_then(Value::as_array).into_iter().flatten() {
                let (Some(local_id), Some(reason)) = (r.get("local_id").and_then(Value::as_str), r.get("reason").and_then(Value::as_str)) else {
                    continue;
                };
                let code = reason_code(reason);
                self.note_rejection(RejectionNotice {
                    item_key: local_id.to_string(),
                    eval_id: None,
                    kind: if code == "parent_rejected" { "reply_cascade" } else { "evaluation" }.into(),
                    label_ko: reason_label(reason),
                    reason_code: code,
                });
            }
        }
        for r in flush.get("votes_rejected").and_then(Value::as_array).into_iter().flatten() {
            let (Some(request_id), Some(reason)) = (r.get("request_id").and_then(Value::as_str), r.get("reason").and_then(Value::as_str)) else {
                continue;
            };
            self.note_rejection(RejectionNotice {
                item_key: request_id.to_string(),
                eval_id: r.get("eval_id").and_then(Value::as_str).map(str::to_string),
                kind: "vote".into(),
                reason_code: reason_code(reason),
                label_ko: reason_label(reason),
            });
        }
    }

    fn apply(&mut self, result: Result<Value>) {
        self.runs += 1;
        match result {
            Ok(v) => {
                for row in v.get("rows").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(id) = row.get("request_id").and_then(Value::as_str) {
                        self.rows.insert(id.to_string(), row.clone());
                    }
                }
                if let Some(next) = v.get("next_since_ms").and_then(Value::as_i64) {
                    self.since_ms = self.since_ms.max(next);
                }
                let flush = v.get("flush").filter(|f| !f.is_null()).cloned();
                let stopped = |f: &Value, key: &str| f.get(key).and_then(Value::as_str).map(str::to_string);
                self.offline = flush.as_ref().is_some_and(|f| {
                    ["stopped", "sync_stopped"].iter().any(|k| f.get(*k).is_some_and(|s| !s.is_null() && !s.as_str().is_some_and(is_hold)))
                        || stopped(f, "votes_stopped").is_some_and(|s| s.starts_with("offline"))
                });
                if let Some(f) = &flush {
                    self.collect_rejections(f);
                    self.auth_required = f.get("auth_required").and_then(Value::as_bool).unwrap_or(false)
                        || ["stopped", "sync_stopped", "votes_stopped"]
                            .iter()
                            .any(|k| stopped(f, k).is_some_and(|s| s.starts_with("auth_required")));
                    self.retry_at = f
                        .get("retry_after_ms")
                        .and_then(Value::as_u64)
                        .map(|ms| Instant::now() + Duration::from_millis(ms));
                    self.votes_remaining = f.get("votes_remaining").and_then(Value::as_u64).unwrap_or(0) as usize;
                    self.vote_hint = false;
                    self.last_flush = flush;
                } else if let Some(n) = v.get("queued_votes").and_then(Value::as_u64) {
                    // A run without flush still says how many votes wait, so a vote queued offline keeps the timer on.
                    self.votes_remaining = n as usize;
                    self.vote_hint = false;
                }
                self.last_error = None;
            }
            Err(e) => {
                let message = format!("{e:#}");
                // `pd_flush_and_sync: <kind>: …` from the server, or a transport failure of the MCP client itself.
                let kind = message.split_once(": ").and_then(|(_, rest)| error_kind(rest));
                self.offline = matches!(kind, Some("offline" | "unavailable") | None);
                self.last_error = Some(message);
            }
        }
    }
}

#[derive(Default)]
struct LoopState {
    requested: bool,
    flush: bool,
    running: bool,
    stop: bool,
    /// Interactive searches in progress ([`SyncLoop::hold_for_interactive`]): no run starts while > 0.
    interactive: usize,
    snapshot: SyncSnapshot,
}

/// Waits up to `budget` for `mutex` (std has no timed lock): the interactive search gives up on a sync run that
/// already holds the shared client instead of blocking past its deadline, and answers from its fast track.
pub fn try_lock_for<T>(mutex: &Mutex<T>, budget: Duration) -> Option<MutexGuard<'_, T>> {
    let deadline = Instant::now() + budget;
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::Poisoned(_)) => return None,
            Err(std::sync::TryLockError::WouldBlock) => {
                let now = Instant::now();
                if now >= deadline {
                    return None;
                }
                std::thread::sleep((deadline - now).min(Duration::from_millis(2)));
            }
        }
    }
}

/// Held while an interactive search runs ([`SyncLoop::hold_for_interactive`]); dropping it lets runs go again.
pub struct InteractiveHoldGuard<'a> {
    shared: &'a Shared,
}

impl Drop for InteractiveHoldGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut st) = self.shared.state.lock() {
            st.interactive = st.interactive.saturating_sub(1);
            self.shared.cv.notify_all();
        }
    }
}

struct Shared {
    state: Mutex<LoopState>,
    cv: Condvar,
}

/// Background sync with coalesced requests (see the module docs).
pub struct SyncLoop {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

type OnUpdate = Box<dyn Fn(&SyncSnapshot) + Send>;

impl SyncLoop {
    /// Starts the loop. Nothing runs until the first [`request`](Self::request); afterwards the timer keeps
    /// running while [`SyncSnapshot::has_work`]. `on_update` is called on the loop's thread after every run.
    pub fn start(mut runner: impl SyncRunner, interval: Duration, on_update: Option<OnUpdate>) -> SyncLoop {
        let shared = Arc::new(Shared {
            state: Mutex::new(LoopState::default()),
            cv: Condvar::new(),
        });
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("pd-sync".into())
            .spawn(move || loop {
                let (since, flush) = {
                    let Ok(mut st) = worker.state.lock() else { return };
                    let mut due: Option<Instant> = None;
                    loop {
                        if st.stop {
                            return;
                        }
                        // An interactive search owns the client: requested and timer runs wait for it.
                        if st.interactive > 0 {
                            st = match worker.cv.wait(st) {
                                Ok(g) => g,
                                Err(_) => return,
                            };
                            continue;
                        }
                        if st.requested {
                            break;
                        }
                        if st.snapshot.has_work() {
                            let mut deadline = *due.get_or_insert_with(|| Instant::now() + interval);
                            // 429: not before the server's Retry-After.
                            if let Some(retry_at) = st.snapshot.retry_at {
                                deadline = deadline.max(retry_at);
                            }
                            let now = Instant::now();
                            if now >= deadline {
                                st.requested = true;
                                st.flush = true;
                                break;
                            }
                            st = match worker.cv.wait_timeout(st, deadline - now) {
                                Ok((g, _)) => g,
                                Err(_) => return,
                            };
                        } else {
                            due = None;
                            st = match worker.cv.wait(st) {
                                Ok(g) => g,
                                Err(_) => return,
                            };
                        }
                    }
                    st.requested = false;
                    st.running = true;
                    (st.snapshot.since_ms, std::mem::take(&mut st.flush))
                };
                let result = runner.run(since, flush);
                let snapshot = {
                    let Ok(mut st) = worker.state.lock() else { return };
                    st.snapshot.apply(result);
                    st.running = false;
                    worker.cv.notify_all();
                    st.snapshot.clone()
                };
                if let Some(f) = &on_update {
                    f(&snapshot);
                }
            })
            .expect("spawning the pd-sync thread");
        SyncLoop {
            shared,
            thread: Some(thread),
        }
    }

    /// Asks for a run (coalesced). `flush` sends queued evaluations and checks pending ones first.
    pub fn request(&self, flush: bool) {
        if let Ok(mut st) = self.shared.state.lock() {
            st.requested = true;
            st.flush |= flush;
            self.shared.cv.notify_all();
        }
    }

    /// `pd_vote` answered `queued` (offline): run a flushing sync now and keep the timer on until the votes are
    /// out, even when no evaluation row waits (FC-12).
    pub fn notify_vote_queued(&self) {
        if let Ok(mut st) = self.shared.state.lock() {
            st.snapshot.vote_hint = true;
            st.requested = true;
            st.flush = true;
            self.shared.cv.notify_all();
        }
    }

    /// Defers background runs while an interactive search uses the shared client (Scenario 3 Part 2): the timer and
    /// requested runs wait until every guard is dropped. A run already in progress finishes; bound the wait for it
    /// with [`try_lock_for`].
    pub fn hold_for_interactive(&self) -> InteractiveHoldGuard<'_> {
        if let Ok(mut st) = self.shared.state.lock() {
            st.interactive += 1;
        }
        InteractiveHoldGuard { shared: &self.shared }
    }

    /// Marks the unseen rejections seen and returns them (the overlay opened the Stage 1 chip or the drawer).
    pub fn ack_rejections(&self) -> Vec<RejectionNotice> {
        self.shared
            .state
            .lock()
            .map(|mut s| s.snapshot.drain_unacked_rejections())
            .unwrap_or_default()
    }

    pub fn snapshot(&self) -> SyncSnapshot {
        self.shared.state.lock().map(|s| s.snapshot.clone()).unwrap_or_default()
    }

    /// Waits until no run is in progress or requested (true), or `timeout` passes (false).
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let Ok(mut st) = self.shared.state.lock() else { return false };
        while st.requested || st.running {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            st = match self.shared.cv.wait_timeout(st, deadline - now) {
                Ok((g, _)) => g,
                Err(_) => return false,
            };
        }
        true
    }
}

impl Drop for SyncLoop {
    fn drop(&mut self) {
        if let Ok(mut st) = self.shared.state.lock() {
            st.stop = true;
            self.shared.cv.notify_all();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Records every run; each run waits for a go signal so a test can pile up requests during it.
    struct Scripted {
        log: Arc<Mutex<Vec<(i64, bool)>>>,
        go: mpsc::Receiver<Result<Value, String>>,
    }

    impl SyncRunner for Scripted {
        fn run(&mut self, since_ms: i64, flush: bool) -> Result<Value> {
            self.log.lock().unwrap().push((since_ms, flush));
            match self.go.recv() {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(anyhow::anyhow!(e)),
                Err(_) => Err(anyhow::anyhow!("test over")),
            }
        }
    }

    fn rows(states: &[(&str, &str, i64)]) -> Value {
        json!({
            "rows": states.iter().map(|(id, st, at)| json!({"request_id": id, "state": st, "updated_at": at})).collect::<Vec<_>>(),
            "next_since_ms": states.iter().map(|s| s.2).max().unwrap_or(0),
            "flush": {"sent": 0, "remaining": 0, "stopped": null, "still_pending": 0, "sync_stopped": null},
        })
    }

    type RunLog = Arc<Mutex<Vec<(i64, bool)>>>;

    fn started(interval: Duration) -> (SyncLoop, RunLog, mpsc::Sender<Result<Value, String>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::channel();
        let l = SyncLoop::start(Scripted { log: log.clone(), go: rx }, interval, None);
        (l, log, tx)
    }

    fn wait_for_runs(log: &Mutex<Vec<(i64, bool)>>, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while log.lock().unwrap().len() < n {
            assert!(Instant::now() < deadline, "expected {n} runs, saw {:?}", log.lock().unwrap());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn requests_during_a_run_share_one_rerun_with_flush_ored() {
        let (l, log, go) = started(Duration::from_secs(3600));
        l.request(false);
        wait_for_runs(&log, 1);
        // Three calls while the first run is in progress, one of them with flush.
        l.request(false);
        l.request(true);
        l.request(false);
        go.send(Ok(rows(&[("a", "stored", 5)]))).unwrap();
        wait_for_runs(&log, 2);
        go.send(Ok(rows(&[("b", "stored", 9)]))).unwrap();
        assert!(l.wait_idle(Duration::from_secs(5)));
        assert_eq!(
            *log.lock().unwrap(),
            vec![(0, false), (5, true)],
            "one rerun, flushing, from the new since"
        );
        let snap = l.snapshot();
        assert_eq!((snap.runs, snap.since_ms, snap.rows.len()), (2, 9, 2));
        assert!(!snap.has_work());
        assert_eq!(snap.status_dot(), Some("stored"));
    }

    #[test]
    fn the_timer_runs_while_rows_wait_and_stops_after() {
        let (l, log, go) = started(Duration::from_millis(30));
        l.request(true);
        wait_for_runs(&log, 1);
        go.send(Ok(rows(&[("a", "queued", 1)]))).unwrap();
        // has_work: the timer fires a flushing run by itself.
        wait_for_runs(&log, 2);
        assert_eq!(log.lock().unwrap()[1], (1, true));
        go.send(Ok(rows(&[("a", "pending", 2)]))).unwrap();
        wait_for_runs(&log, 3);
        go.send(Ok(rows(&[("a", "stored", 3)]))).unwrap();
        assert!(l.wait_idle(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(log.lock().unwrap().len(), 3, "nothing left: no more timer runs");
        assert_eq!(l.snapshot().count("stored"), 1);
    }

    #[test]
    fn errors_keep_the_rows_and_mark_offline() {
        let (l, log, go) = started(Duration::from_secs(3600));
        l.request(true);
        wait_for_runs(&log, 1);
        go.send(Ok(rows(&[("a", "queued", 1)]))).unwrap();
        assert!(l.wait_idle(Duration::from_secs(5)));
        l.request(true);
        wait_for_runs(&log, 2);
        go.send(Err(
            "pd_flush_and_sync: unavailable: spawning premium-discount-app: not found".into()
        ))
        .unwrap();
        assert!(l.wait_idle(Duration::from_secs(5)));
        let snap = l.snapshot();
        assert!(snap.offline);
        assert_eq!(snap.rows.len(), 1, "the cache survives a failed run");
        assert!(snap.last_error.as_deref().unwrap().contains("unavailable"));
        assert!(snap.has_work());

        let mut s = SyncSnapshot::default();
        s.apply(Err(anyhow::anyhow!("pd_flush_and_sync: invalid_input: bad since_ms")));
        assert!(!s.offline, "an argument error is not a connection problem");
        let mut flush = rows(&[]);
        flush["flush"]["stopped"] = json!("offline: no network");
        s.apply(Ok(flush));
        assert!(s.offline);
    }

    // The web UI's reasonLabel cases (web/desktop/view.test.ts), so the overlay and the web history say the same thing.
    #[test]
    fn rejection_reasons_read_like_the_web_ui() {
        assert_eq!(reason_label("server status rejected: profanity"), "욕설");
        assert_eq!(reason_label("server status rejected: nsfw,illegal"), "음란·폭력, 불법·개인정보");
        assert_eq!(reason_label("server status rejected"), "서버가 거부했습니다");
        assert!(reason_label("server status failed").contains("재시도 한도 초과"));
        assert_eq!(reason_label("parent local-3 was rejected"), "답글을 단 글이 거부됨");
        assert_eq!(reason_label("parent rejected"), "답글을 단 글이 거부됨");
        assert_eq!(reason_label(r#"server returned 400: {"error":"x"}"#), "서버가 요청을 받아들이지 않았습니다 (400)");
        assert_eq!(reason_label("nsfw,illegal"), "음란·폭력, 불법·개인정보");
        assert_eq!(reason_label("something new"), "something new");
        assert_eq!(reason_label(""), "");
        assert_eq!(reason_code("server status rejected: nsfw,illegal"), "nsfw");
        assert_eq!(reason_code("server status failed"), "server_failed");
        assert_eq!(reason_code("parent local-3 was rejected"), "parent_rejected");
        assert_eq!(reason_code("server status rejected: parent missing"), "parent_missing");
        assert_eq!(reason_code("server returned 409: conflict"), "http_409");
        assert_eq!(reason_code("something new"), "other");
    }

    // FC-11 / E-16: a rejection stays until the overlay acknowledges it, however many runs come after.
    #[test]
    fn fc11_rejections_survive_later_runs_until_acknowledged() {
        let mut s = SyncSnapshot::default();
        let mut first = rows(&[("p", "rejected", 1), ("c", "rejected", 1)]);
        first["flush"]["sync_rejected"] = json!([
            {"local_id": "local-1", "reason": "server status rejected: profanity"},
            {"local_id": "local-2", "reason": "parent local-1 was rejected"},
        ]);
        first["flush"]["votes_rejected"] = json!([{"request_id": "v-1", "eval_id": "e-9", "reason": "server returned 422: bad"}]);
        s.apply(Ok(first.clone()));
        // Two more 10 s runs with nothing new: the notices must not be overwritten.
        s.apply(Ok(rows(&[])));
        s.apply(Ok(first));
        assert_eq!(s.unacked_rejections.len(), 3, "no duplicates either");
        assert_eq!(s.status_dot(), Some("attention"));
        let notices = s.drain_unacked_rejections();
        assert_eq!(notices[0].kind, "evaluation");
        assert_eq!((notices[0].reason_code.as_str(), notices[0].label_ko.as_str()), ("profanity", "욕설"));
        assert_eq!((notices[1].kind.as_str(), notices[1].reason_code.as_str()), ("reply_cascade", "parent_rejected"));
        assert_eq!((notices[2].kind.as_str(), notices[2].eval_id.as_deref()), ("vote", Some("e-9")));
        assert_eq!(s.status_dot(), None, "acknowledged, and only rejected rows are left");
        // The queue of notices is bounded.
        for i in 0..(MAX_UNACKED_REJECTIONS + 5) {
            s.note_rejection(RejectionNotice {
                item_key: format!("local-{i}"),
                eval_id: None,
                kind: "evaluation".into(),
                reason_code: "profanity".into(),
                label_ko: "욕설".into(),
            });
        }
        assert_eq!(s.unacked_rejections.len(), MAX_UNACKED_REJECTIONS);
        assert_eq!(s.unacked_rejections.front().unwrap().item_key, "local-5", "the oldest went first");
    }

    // FC-12: a vote queued offline with no evaluation waiting still runs the loop, and the timer keeps going until
    // votes_remaining is 0. A run without flush learns the count from queued_votes.
    #[test]
    fn fc12_a_queued_vote_alone_keeps_the_loop_running_until_it_is_out() {
        let (l, log, go) = started(Duration::from_millis(30));
        l.notify_vote_queued();
        wait_for_runs(&log, 1);
        assert_eq!(log.lock().unwrap()[0], (0, true), "a flushing run at once");
        let mut v = rows(&[]);
        v["flush"]["votes_remaining"] = json!(1);
        v["flush"]["votes_stopped"] = json!("offline: no network");
        go.send(Ok(v)).unwrap();
        // Still one vote: the timer fires by itself.
        wait_for_runs(&log, 2);
        let mut v = rows(&[]);
        v["flush"]["votes_remaining"] = json!(0);
        v["flush"]["votes_sent"] = json!(1);
        go.send(Ok(v)).unwrap();
        assert!(l.wait_idle(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(log.lock().unwrap().len(), 2, "votes out: the timer stops");

        let mut s = SyncSnapshot::default();
        s.apply(Ok(json!({"flush": null, "rows": [], "next_since_ms": 0, "queued_votes": 2})));
        assert_eq!(s.votes_remaining, 2);
        assert!(s.has_work());
    }

    // E-11: 401 / 403 pause the timer (no hammering) without dropping anything; an explicit request after signing in
    // resumes it. 429 pushes the next timer run past Retry-After.
    #[test]
    fn auth_holds_pause_the_timer_and_rate_limits_delay_it() {
        let (l, log, go) = started(Duration::from_millis(20));
        l.request(true);
        wait_for_runs(&log, 1);
        let mut v = rows(&[("a", "queued", 1)]);
        v["flush"]["stopped"] = json!("auth_required: server returned 401");
        v["flush"]["auth_required"] = json!(true);
        go.send(Ok(v)).unwrap();
        assert!(l.wait_idle(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(log.lock().unwrap().len(), 1, "no timer runs while sign-in is needed");
        let snap = l.snapshot();
        assert!(snap.auth_required && !snap.offline && !snap.has_work());
        assert_eq!(snap.status_dot(), Some("attention"));
        assert_eq!(snap.count("queued"), 1, "the row is kept");

        // Signed in again: the host asks for a run; the row goes out.
        l.request(true);
        wait_for_runs(&log, 2);
        let mut v = rows(&[("a", "queued", 2)]);
        v["flush"]["stopped"] = json!("rate_limited: server returned 429");
        v["flush"]["retry_after_ms"] = json!(150);
        go.send(Ok(v)).unwrap();
        assert!(l.wait_idle(Duration::from_secs(5)));
        let snap = l.snapshot();
        assert!(!snap.auth_required && !snap.offline && snap.retry_at.is_some());
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(log.lock().unwrap().len(), 2, "not before Retry-After even though the interval is 20 ms");
        wait_for_runs(&log, 3);
        go.send(Ok(rows(&[("a", "stored", 3)]))).unwrap();
        assert!(l.wait_idle(Duration::from_secs(5)));
    }

    // CR-05 (Scenario 3 Part 2): no background run starts while an interactive search holds the loop.
    #[test]
    fn cr05_interactive_holds_defer_runs_and_try_lock_for_gives_up_in_time() {
        let (l, log, go) = started(Duration::from_secs(3600));
        {
            let _hold = l.hold_for_interactive();
            l.request(true);
            std::thread::sleep(Duration::from_millis(80));
            assert!(log.lock().unwrap().is_empty(), "held: the requested run waits");
        }
        wait_for_runs(&log, 1);
        go.send(Ok(rows(&[]))).unwrap();
        assert!(l.wait_idle(Duration::from_secs(5)));

        let shared = Arc::new(Mutex::new(0u32));
        let held = shared.lock().unwrap();
        let started_at = Instant::now();
        assert!(try_lock_for(&shared, Duration::from_millis(40)).is_none());
        let waited = started_at.elapsed();
        assert!(waited >= Duration::from_millis(40) && waited < Duration::from_millis(400), "{waited:?}");
        drop(held);
        assert!(try_lock_for(&shared, Duration::from_millis(40)).is_some());
    }
}
