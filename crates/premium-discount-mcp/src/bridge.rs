//! The premium-discount-app process behind the MCP tools.
//!
//! * Started on the first command that needs it, not with the server: the app loads embeddinggemma-2 while it
//!   starts (and the model is the only one it holds), so an idle Raon Zena keeps neither the model nor LanceDB
//!   in memory.
//! * Started again on the next command after it died.
//! * Stopped after `idle` without commands, unless it is still working on a request or a model download is
//!   running (stopping it would cancel the download). Its outbox is on disk, so nothing is lost: the app
//!   rebuilds the queue from the `queued` / `pending` rows when it starts again.
//!
//! Pre-warm (Scenario 2 Part 3): when Raon Zena sees early friction it may ask for the app before the user asks a
//! question ([`Backend::prewarm`], `pd_model_manage {action: "prewarm"}`):
//!
//! * the call returns at once — the process is spawned and its model load (one `cmd_model_status` round trip, which
//!   the app answers only after its core is up) is awaited on a `pd-app-warm` thread, outside the slot lock, so
//!   in-process tools such as `pd_nav_number` never wait for it;
//! * a command that arrives while the warm-up runs joins it (single flight: one process, no second spawn) and only
//!   waits for the rest of the load;
//! * a process woken only by a pre-warm holds a short [`WarmLeaseKind::Speculative`] lease ([`SPECULATIVE_IDLE`],
//!   45 s): a false alarm gives the ~300 MB back quickly. The first real command promotes it to
//!   [`WarmLeaseKind::ActiveUse`] (the usual `idle`, 300 s by default);
//! * a missing program or a missing model trips a circuit breaker: later pre-warms answer `suppressed` without
//!   spawning, until a model download is requested ([`Backend::clear_suppression`]);
//! * stopping is graceful ([`LineClient::shutdown`]): `cmd_shutdown` first (the app releases its core and the GPU
//!   pool), a kill only after [`SHUTDOWN_GRACE`].

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::lines::{CallError, LineClient};

/// How long a process woken only by a pre-warm may stay idle before it is stopped.
pub const SPECULATIVE_IDLE: Duration = Duration::from_secs(45);
/// How long a stopping app may take to exit by itself before it is killed.
pub const SHUTDOWN_GRACE: Duration = Duration::from_millis(2_000);
/// Available physical memory a pre-warm needs (Q8_0 model ~300 MB + 350 MB headroom for the OS and the game).
pub const MIN_PREWARM_AVAIL_MB: u64 = 650;

/// Why the app process is up, which decides how long it may stay idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarmLeaseKind {
    /// Woken by a friction pre-warm and not used yet: [`SPECULATIVE_IDLE`].
    Speculative,
    /// It handled a real command (search, enqueue, vote, sync …): the configured `idle`.
    ActiveUse,
}

/// What a pre-warm did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PrewarmOutcome {
    /// The app is already up.
    AlreadyWarm,
    /// A warm-up started in the background; the caller did not wait.
    StartedAsync,
    /// A warm-up is already running (no second process).
    AlreadyWarming,
    /// The circuit breaker is open (no program, no model): nothing was spawned.
    Suppressed { reason: String },
    /// Not now (too little free memory): nothing was spawned, and later pre-warms may go ahead.
    Deferred { reason: String },
}

/// What the tools need from premium-discount-app: one command at a time.
pub trait Backend {
    fn call(&mut self, cmd: &str, args: Value) -> Result<Value, CallError>;
    /// Process state for `pd_model_manage status` (`Null` when there is no process).
    fn state(&self) -> Value {
        Value::Null
    }
    /// Starts the app and its model load without waiting for them (see the module docs).
    fn prewarm(&self) -> PrewarmOutcome {
        PrewarmOutcome::AlreadyWarm
    }
    /// Stops the app now if it is idle (no request in flight, no download): true when it was stopped.
    fn evict_if_idle(&self) -> bool {
        false
    }
    /// Closes the pre-warm circuit breaker (a model download was requested).
    fn clear_suppression(&self) {}
}

/// How long a command may take. The first command after a start also waits for the model to load.
pub fn timeout_for(cmd: &str) -> Duration {
    match cmd {
        "cmd_model_status" | "cmd_model_download" | "cmd_model_download_cancel" => Duration::from_secs(30),
        _ => Duration::from_secs(180),
    }
}

/// Commands that count as using the app (they promote a speculative pre-warm to [`WarmLeaseKind::ActiveUse`]).
fn is_real_use(cmd: &str) -> bool {
    !matches!(cmd, "cmd_model_status" | "cmd_model_download_cancel" | "cmd_shutdown")
}

/// premium-discount-app's settings (`apps/core/src/main.rs`), passed as `PD_*` environment variables.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub program: PathBuf,
    pub model_dir: PathBuf,
    pub data_dir: PathBuf,
    pub server_url: String,
    /// `text` (default here: Raon Zena only sends text) | `auto` | `vision,audio`.
    pub modalities: String,
    /// `q8_0` (default here: ~300 MB resident instead of ~660 MB F32, cos 0.9999 to the reference) | `tiered` | `auto` | `off`.
    pub quant: String,
    /// Stop the app after this long without commands (`None`: keep it).
    pub idle: Option<Duration>,
}

impl AppConfig {
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("PD_MODEL_DIR".into(), self.model_dir.display().to_string()),
            ("PD_DATA_DIR".into(), self.data_dir.display().to_string()),
            ("PD_SERVER_URL".into(), self.server_url.clone()),
            ("PD_MODALITIES".into(), self.modalities.clone()),
            ("PD_QUANT".into(), self.quant.clone()),
        ]
    }
}

/// Starts one app process (the real one spawns `config.program`; tests hand in a fake on pipes).
pub type Spawner = Arc<dyn Fn() -> Result<LineClient, CallError> + Send + Sync>;

struct Slot {
    client: Option<LineClient>,
    last_used: Instant,
    started: u64,
    /// A `pd-app-warm` thread is starting the app: commands wait for it instead of spawning another.
    warming: bool,
    lease: WarmLeaseKind,
    /// The pre-warm circuit breaker: why pre-warms are refused (no program, no model).
    suppressed: Option<String>,
    speculative_idle: Duration,
    /// Graceful stops so far (for `state`).
    stops: u64,
}

impl Slot {
    /// How long the current process may stay idle.
    fn ttl(&self, idle: Duration) -> Duration {
        match self.lease {
            WarmLeaseKind::Speculative => self.speculative_idle.min(idle),
            WarmLeaseKind::ActiveUse => idle,
        }
    }
}

struct Shared {
    slot: Mutex<Slot>,
    warm: Condvar,
}

/// [`Backend`] over a premium-discount-app child process.
pub struct ProcessBackend {
    config: AppConfig,
    shared: Arc<Shared>,
    spawner: Spawner,
}

fn download_running(status: &Value) -> bool {
    status.pointer("/download/state").and_then(Value::as_str) == Some("running")
}

/// The model is missing and nothing is being downloaded: a pre-warm cannot help.
fn model_missing(status: &Value) -> bool {
    status.get("installed").and_then(Value::as_str) == Some("none") && !download_running(status)
}

/// Stops the slot's process if it has been idle past its lease, unless it is busy or downloading. Holds the lock
/// through the graceful stop (at most [`SHUTDOWN_GRACE`] plus one status call), so no command races a dying app.
fn reap_if_idle(slot: &mut Slot, idle: Duration, force: bool) -> bool {
    if slot.warming {
        return false;
    }
    let ttl = slot.ttl(idle);
    let Some(c) = slot.client.as_mut() else { return false };
    if (!force && slot.last_used.elapsed() < ttl) || c.in_flight() > 0 {
        return false;
    }
    if c.is_alive() {
        match c.call("cmd_model_status", Value::Null, Duration::from_secs(5)) {
            Ok(st) if download_running(&st) => {
                slot.last_used = Instant::now();
                return false;
            }
            // Busy past the 5 s (in_flight > 0 now): try again later.
            Err(CallError::Transport(_)) if c.in_flight() > 0 => return false,
            _ => {}
        }
    }
    eprintln!("premium-discount-mcp: premium-discount-app idle ({:?} lease), stopping it", slot.lease);
    if let Some(c) = slot.client.take() {
        if !c.shutdown(SHUTDOWN_GRACE) {
            eprintln!("premium-discount-mcp: premium-discount-app did not exit in {SHUTDOWN_GRACE:?}; killed");
        }
    }
    slot.stops += 1;
    true
}

impl ProcessBackend {
    pub fn new(config: AppConfig) -> ProcessBackend {
        let (program, env) = (config.program.clone(), config.env());
        let spawner: Spawner = Arc::new(move || LineClient::spawn(&program, &[], &env));
        ProcessBackend::with_spawner(config, spawner)
    }

    /// Like [`ProcessBackend::new`] with another way to start the app (tests: a fake app on pipes).
    pub fn with_spawner(config: AppConfig, spawner: Spawner) -> ProcessBackend {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot {
                client: None,
                last_used: Instant::now(),
                started: 0,
                warming: false,
                lease: WarmLeaseKind::ActiveUse,
                suppressed: None,
                speculative_idle: SPECULATIVE_IDLE,
                stops: 0,
            }),
            warm: Condvar::new(),
        });
        if let Some(idle) = config.idle {
            let weak = Arc::downgrade(&shared);
            let _ = std::thread::Builder::new().name("pd-app-idle".into()).spawn(move || loop {
                let tick = {
                    let Some(shared) = weak.upgrade() else { break };
                    let ttl = shared.slot.lock().map(|s| s.speculative_idle.min(idle)).unwrap_or(idle);
                    (ttl / 4).clamp(Duration::from_millis(10), Duration::from_secs(5))
                };
                std::thread::sleep(tick);
                let Some(shared) = weak.upgrade() else { break };
                // A command holds the lock for its whole call: skip this round instead of waiting for it.
                let Ok(mut slot) = shared.slot.try_lock() else { continue };
                reap_if_idle(&mut slot, idle, false);
            });
        }
        ProcessBackend { config, shared, spawner }
    }

    /// Shortens or lengthens the speculative lease (default [`SPECULATIVE_IDLE`]).
    pub fn with_speculative_idle(self, ttl: Duration) -> ProcessBackend {
        if let Ok(mut s) = self.shared.slot.lock() {
            s.speculative_idle = ttl;
        }
        self
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    fn lock(&self) -> Result<MutexGuard<'_, Slot>, CallError> {
        self.shared.slot.lock().map_err(|_| CallError::Transport("app slot poisoned".into()))
    }
}

impl Backend for ProcessBackend {
    /// Whether the app is running, how often it was started and how long it has been idle.
    fn state(&self) -> Value {
        match self.shared.slot.lock() {
            Ok(s) => json!({
                "running": s.client.as_ref().is_some_and(LineClient::is_alive),
                "starts": s.started,
                "idle_ms": s.last_used.elapsed().as_millis() as u64,
                "warming": s.warming,
                "lease": s.lease,
                "stops": s.stops,
                "suppressed": s.suppressed,
            }),
            Err(_) => json!({"running": false}),
        }
    }

    fn call(&mut self, cmd: &str, args: Value) -> Result<Value, CallError> {
        let mut s = self.lock()?;
        // Join a warm-up in flight instead of starting a second process; wait only for the rest of its load.
        let deadline = Instant::now() + timeout_for(cmd).max(Duration::from_secs(180));
        while s.warming {
            let now = Instant::now();
            if now >= deadline {
                return Err(CallError::Transport("premium-discount-app warm-up did not finish".into()));
            }
            s = self
                .shared
                .warm
                .wait_timeout(s, deadline - now)
                .map_err(|_| CallError::Transport("app slot poisoned".into()))?
                .0;
        }
        s.last_used = Instant::now();
        if s.client.as_ref().is_some_and(|c| !c.is_alive()) {
            eprintln!("premium-discount-mcp: premium-discount-app exited; starting it again");
            s.client = None;
        }
        if s.client.is_none() {
            for dir in [&self.config.model_dir, &self.config.data_dir] {
                std::fs::create_dir_all(dir).map_err(|e| CallError::Transport(format!("creating {}: {e}", dir.display())))?;
            }
            s.client = Some((self.spawner)()?);
            s.started += 1;
            s.lease = WarmLeaseKind::ActiveUse;
        }
        if is_real_use(cmd) {
            s.lease = WarmLeaseKind::ActiveUse;
        }
        if cmd == "cmd_model_download" {
            s.suppressed = None;
        }
        let client = s.client.as_mut().expect("client just started");
        let out = client.call(cmd, args, timeout_for(cmd));
        s.last_used = Instant::now();
        out
    }

    fn prewarm(&self) -> PrewarmOutcome {
        let Ok(mut s) = self.shared.slot.lock() else {
            return PrewarmOutcome::Suppressed { reason: "app slot poisoned".into() };
        };
        if let Some(reason) = &s.suppressed {
            return PrewarmOutcome::Suppressed { reason: reason.clone() };
        }
        if s.warming {
            return PrewarmOutcome::AlreadyWarming;
        }
        if s.client.as_ref().is_some_and(LineClient::is_alive) {
            return PrewarmOutcome::AlreadyWarm;
        }
        s.client = None;
        s.warming = true;
        s.lease = WarmLeaseKind::Speculative;
        s.last_used = Instant::now();
        drop(s);
        let (shared, spawner, dirs) = (self.shared.clone(), self.spawner.clone(), [self.config.model_dir.clone(), self.config.data_dir.clone()]);
        let worker = std::thread::Builder::new().name("pd-app-warm".into()).spawn(move || {
            let started = (|| {
                for dir in &dirs {
                    std::fs::create_dir_all(dir).map_err(|e| CallError::Transport(format!("creating {}: {e}", dir.display())))?;
                }
                let mut client = spawner()?;
                // The app answers its first line only after its core is up: this round trip is the model load.
                let status = client.call("cmd_model_status", Value::Null, Duration::from_secs(180));
                Ok::<_, CallError>((client, status))
            })();
            let Ok(mut s) = shared.slot.lock() else { return };
            s.warming = false;
            match started {
                Ok((client, status)) => {
                    if status.as_ref().is_ok_and(model_missing) {
                        s.suppressed = Some("embeddinggemma-2 is not installed".into());
                    }
                    s.client = Some(client);
                    s.started += 1;
                    s.last_used = Instant::now();
                }
                Err(e) => s.suppressed = Some(e.to_string()),
            }
            shared.warm.notify_all();
        });
        if let Err(e) = worker {
            if let Ok(mut s) = self.shared.slot.lock() {
                s.warming = false;
                self.shared.warm.notify_all();
            }
            return PrewarmOutcome::Suppressed { reason: format!("warm-up thread: {e}") };
        }
        PrewarmOutcome::StartedAsync
    }

    fn evict_if_idle(&self) -> bool {
        let Ok(mut s) = self.shared.slot.lock() else { return false };
        let idle = self.config.idle.unwrap_or(Duration::MAX);
        reap_if_idle(&mut s, idle, true)
    }

    fn clear_suppression(&self) {
        if let Ok(mut s) = self.shared.slot.lock() {
            s.suppressed = None;
        }
    }
}

#[cfg(test)]
pub(crate) mod fake_app {
    //! A fake premium-discount-app on OS pipes: its first reply waits `load` (the core starting), `cmd_shutdown`
    //! answers and hangs up, `cmd_model_status` reports `installed` and the download state.
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Clone)]
    pub struct FakeApp {
        pub spawns: Arc<AtomicUsize>,
        pub shutdowns: Arc<AtomicUsize>,
        pub load: Duration,
        pub installed: &'static str,
        pub downloading: Arc<AtomicBool>,
    }

    impl FakeApp {
        pub fn new(load: Duration, installed: &'static str) -> FakeApp {
            FakeApp {
                spawns: Arc::new(AtomicUsize::new(0)),
                shutdowns: Arc::new(AtomicUsize::new(0)),
                load,
                installed,
                downloading: Arc::new(AtomicBool::new(false)),
            }
        }

        pub fn spawner(&self) -> Spawner {
            let app = self.clone();
            Arc::new(move || {
                app.spawns.fetch_add(1, Ordering::SeqCst);
                let (req_r, req_w) = std::io::pipe().map_err(|e| CallError::Transport(e.to_string()))?;
                let (rep_r, mut rep_w) = std::io::pipe().map_err(|e| CallError::Transport(e.to_string()))?;
                let app = app.clone();
                std::thread::spawn(move || {
                    let mut first = true;
                    for line in BufReader::new(req_r).lines() {
                        let Ok(line) = line else { break };
                        if first {
                            std::thread::sleep(app.load);
                            first = false;
                        }
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let id = req["id"].clone();
                        let cmd = req["cmd"].as_str().unwrap_or_default().to_string();
                        let result = match cmd.as_str() {
                            "cmd_model_status" => json!({
                                "installed": app.installed,
                                "download": {"state": if app.downloading.load(Ordering::SeqCst) { "running" } else { "idle" }},
                            }),
                            "cmd_shutdown" => json!({"shutdown": true}),
                            other => json!({"echo": other}),
                        };
                        if writeln!(rep_w, "{}", json!({"id": id, "ok": true, "result": result})).is_err() {
                            break;
                        }
                        if cmd == "cmd_shutdown" {
                            app.shutdowns.fetch_add(1, Ordering::SeqCst);
                            break;
                        }
                    }
                });
                LineClient::from_io("fake-app", rep_r, req_w)
            })
        }
    }

    pub fn config(idle: Option<Duration>) -> AppConfig {
        let dir = std::env::temp_dir().join(format!("zena-pd-fake-app-{}", std::process::id()));
        AppConfig {
            program: PathBuf::from("fake-app"),
            model_dir: dir.join("model"),
            data_dir: dir.join("data"),
            server_url: "http://127.0.0.1:9".into(),
            modalities: "text".into(),
            quant: "q8_0".into(),
            idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake_app::{config, FakeApp};
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn a_missing_program_is_unavailable_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("zena-pd-bridge-test-{}", std::process::id()));
        let mut b = ProcessBackend::new(AppConfig {
            program: PathBuf::from("definitely-not-premium-discount-app-xyz"),
            model_dir: dir.join("model"),
            data_dir: dir.join("data"),
            server_url: "http://127.0.0.1:9".into(),
            modalities: "text".into(),
            quant: "q8_0".into(),
            idle: Some(Duration::from_millis(100)),
        });
        let e = b.call("cmd_model_status", Value::Null).unwrap_err();
        assert_eq!(e.kind(), "unavailable");
        assert_eq!(b.state()["running"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn download_state_is_read_from_model_status() {
        assert!(download_running(&json!({"download": {"state": "running"}})));
        assert!(!download_running(&json!({"download": {"state": "done"}})));
        assert!(!download_running(&json!({})));
        assert!(model_missing(&json!({"installed": "none", "download": {"state": "idle"}})));
        assert!(!model_missing(&json!({"installed": "none", "download": {"state": "running"}})));
        assert!(!model_missing(&json!({"installed": "text"})));
    }

    // PF-07 (bridge side): the pre-warm returns at once, a command during the load joins it (one spawn) and waits only
    // for the rest of the load.
    #[test]
    fn pf07_prewarm_returns_at_once_and_a_command_joins_the_warm_up() {
        let app = FakeApp::new(Duration::from_millis(350), "text");
        let mut b = ProcessBackend::with_spawner(config(Some(Duration::from_secs(60))), app.spawner());
        let t = Instant::now();
        assert_eq!(b.prewarm(), PrewarmOutcome::StartedAsync);
        assert!(t.elapsed() < Duration::from_millis(20), "no blocking: {:?}", t.elapsed());
        assert_eq!(b.prewarm(), PrewarmOutcome::AlreadyWarming);
        std::thread::sleep(Duration::from_millis(200));
        let t = Instant::now();
        assert_eq!(b.call("cmd_search_merged", json!({})).unwrap(), json!({"echo": "cmd_search_merged"}));
        let waited = t.elapsed();
        assert!(waited < Duration::from_millis(300), "only the rest of the load: {waited:?}");
        assert_eq!(app.spawns.load(Ordering::SeqCst), 1, "one process");
        let st = b.state();
        assert_eq!((st["starts"].as_u64(), st["lease"].as_str()), (Some(1), Some("active_use")));
        assert_eq!(b.prewarm(), PrewarmOutcome::AlreadyWarm);
    }

    // PF-08: a speculative pre-warm is stopped (gracefully) after its short lease; a used one stays for the long
    // lease; a running download is never stopped.
    #[test]
    fn pf08_speculative_leases_end_early_used_ones_stay_and_downloads_are_protected() {
        // A: pre-warm only.
        let app = FakeApp::new(Duration::from_millis(10), "text");
        let b = ProcessBackend::with_spawner(config(Some(Duration::from_secs(5))), app.spawner()).with_speculative_idle(Duration::from_millis(120));
        assert_eq!(b.prewarm(), PrewarmOutcome::StartedAsync);
        let deadline = Instant::now() + Duration::from_secs(3);
        while b.state()["running"] != json!(false) || b.state()["starts"] != json!(1) {
            assert!(Instant::now() < deadline, "not stopped: {}", b.state());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(app.shutdowns.load(Ordering::SeqCst), 1, "stopped with cmd_shutdown, not killed");
        assert_eq!(b.state()["stops"], 1);

        // B: pre-warm, then a search: promoted to the long lease.
        let app = FakeApp::new(Duration::from_millis(10), "text");
        let mut b =
            ProcessBackend::with_spawner(config(Some(Duration::from_secs(5))), app.spawner()).with_speculative_idle(Duration::from_millis(120));
        b.prewarm();
        b.call("cmd_search_merged", json!({})).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(b.state()["running"], true);
        assert_eq!(app.shutdowns.load(Ordering::SeqCst), 0);

        // C: a download keeps even a speculative process up.
        let app = FakeApp::new(Duration::from_millis(10), "text");
        app.downloading.store(true, Ordering::SeqCst);
        let b = ProcessBackend::with_spawner(config(Some(Duration::from_secs(5))), app.spawner()).with_speculative_idle(Duration::from_millis(60));
        b.prewarm();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(b.state()["running"], true, "downloading: never stopped");
        app.downloading.store(false, Ordering::SeqCst);
        assert!(b.evict_if_idle(), "an explicit evict stops it once the download is over");
        assert_eq!(b.state()["running"], false);
    }

    // PF-09 (bridge side): no model → the first warm-up trips the breaker; nine more pre-warms spawn nothing.
    // A download request closes it again.
    #[test]
    fn pf09_a_missing_model_trips_the_breaker_after_one_check() {
        let app = FakeApp::new(Duration::from_millis(10), "none");
        let mut b = ProcessBackend::with_spawner(config(Some(Duration::from_secs(5))), app.spawner());
        assert_eq!(b.prewarm(), PrewarmOutcome::StartedAsync);
        let deadline = Instant::now() + Duration::from_secs(3);
        while b.state()["warming"] == json!(true) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        b.evict_if_idle();
        for _ in 0..9 {
            assert!(matches!(b.prewarm(), PrewarmOutcome::Suppressed { .. }));
        }
        assert_eq!(app.spawns.load(Ordering::SeqCst), 1, "one check, then no spawn storm");
        b.call("cmd_model_download", json!({"variant": "text"})).unwrap();
        b.evict_if_idle();
        assert_eq!(b.prewarm(), PrewarmOutcome::StartedAsync, "a download request closes the breaker");

        // A program that cannot start trips it too.
        let failing: Spawner = Arc::new(|| Err(CallError::Transport("spawning fake: not found".into())));
        let b = ProcessBackend::with_spawner(config(None), failing);
        assert_eq!(b.prewarm(), PrewarmOutcome::StartedAsync);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !matches!(b.prewarm(), PrewarmOutcome::Suppressed { .. }) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(b.state()["starts"], 0);
    }
}
