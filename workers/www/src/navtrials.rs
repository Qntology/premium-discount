//! 원본 `navtrials.ts` (698줄) — 접근성 내비게이션 타임트라이얼(trial/1). 보관 규칙은 cron 의 `navtrials.ts`(TRIAL_POLICY),
//! 표는 workers/migrations/0007_nav_trials.sql, 기기 쪽은 crates/premium-discount-nav trial.rs 다.
//!
//! ```text
//! POST   /nav/trials              본문 { "runs": [trial/1, ...] } (최대 10개). 런마다 결과를 보낸 순서대로 돌려준다
//! GET    /nav/trials?app=0x…      그 앱 · 웹의 내 콜렉션(인증했을 때)과 다른 사용자의 공개 코스(고스트)
//! DELETE /nav/trials[?app=0x…]    내 콜렉션을 지운다(앱 하나 또는 전부). 계정 없는 합계 · 개수 · 대표 벡터는 남는다
//! ```
//!
//! 인증은 두 갈래 중 하나다 (auth.rs). 지갑 헤더(x-pd-address · x-pd-time · x-pd-signature)가 하나라도 있으면 지갑으로만,
//! 없으면 세션(QR 메일 로그인)으로 확인한다. POST · DELETE 는 인증이 있어야 하고, GET 은 인증이 없으면 다른 사용자의 기록만 준다.
//! 런마다의 결과(status)는 stored · repeat · replaced · expired · invalid 이고, created(코스 id 가 서버에 처음 생김) ·
//! duplicate(이 사용자가 이미 등록함) · best · evicted · route(고스트 따라가기 판정) · error 가 붙는다 (원본 머리말 그대로).
//!
//! ## 고침: 동시에 들어온 쓰기 (fix-nav-lost-update)
//!
//! 원본은 nav_trials · nav_courses 를 읽은 값으로 계산한 절대값으로 upsert 했다(`"runs" = excluded."runs"`, `"sums" = excluded."sums"` …).
//! 그래서 같은 코스의 첫 등록 둘이 동시에 오면 runs 가 2 가 아니라 1 이 되고(하나를 잃음), 같은 사용자의 같은 요청 둘은 둘 다 등록을
//! 읽지 못해 합계 · 판정 수에 두 번 들어갔다. 고친 쓰기(낙관적 동시성):
//! - **등록**: batch 앞에서 `INSERT … ON CONFLICT DO NOTHING` 을 batch 하나로 보내 바뀐 행이 1 인 것만 처음 등록이다(원본의 SELECT 자리).
//!   등록이 합계 · 판정 수를 쓸지 정하므로 batch 밖 앞으로 옮겼다. 그 뒤에서 실패하면(D1 오류 · 원본처럼 던지는 TypeError)
//!   잡은 등록을 지워 되돌린다 — 원본은 등록도 같은 batch 라 실패하면 남지 않았다.
//! - **수**(runs · premium · discount)는 상대 갱신(`"runs" = "runs" + ?`), last_day 는 `MAX("last_day", ?)` — 다시 시도가 필요 없다.
//! - **읽은 값에서 계산한 JSON**(내 콜렉션의 best, 합계의 record · sums)은 읽은 글에 조건을 건 UPDATE 이고, 처음 넣는 행은
//!   `INSERT … ON CONFLICT DO NOTHING`, 밀어내기는 읽은 touched 에 조건을 건 DELETE 다. 이 쓰기들은 원본처럼 batch 하나에 그대로 두고
//!   (충돌이 없으면 D1 왕복 수가 원본과 같다), batch 의 결과에서 바뀐 행이 0 인 것만 batch 뒤에 다시 읽어 계산해 `NAV_RETRIES`(3) 번까지
//!   다시 쓴다(첫 등록이 졌으면 갱신 갈래로, created 는 false 로). 다 쓰면 던져 500 — 그때 batch 의 나머지는 이미 쓰였다.
//! - 충돌이 없는 요청의 응답과 D1 끝 상태는 원본과 같다. 다른 것은 D1 에 직접 넣은 숫자 아닌 수뿐이다(원본은 JS `"x" + 1` = "x1",
//!   SQL 의 `'x' + 1` = 1 — 시험대 `stored_rows_odd`).
//!
//! ## 옮긴 방식
//!
//! - 같은 SQL 글(템플릿 리터럴의 탭 · 줄바꿈까지), 같은 D1 호출 차례(지갑 기한 지우기 → 앱마다 콜렉션 → 등록 → 합계 → 고스트 → batch),
//!   같은 JSON(키 차례 · 수 표기), 같은 상태 · 머리글. try 안의 예외는 `nav trials error` 기록과 500 이고, try 밖에서 새는 예외는 없다.
//! - D1 행 · 저장된 JSON 은 원본처럼 동적인 값([`V`])으로 다룬다(`row.runs += 1` 은 JS `+`, 비교는 JS 관계 비교, `Math.max` · `Math.round`).
//!   저장된 글(steps · best · last · record · sums)과 요청 본문은 런타임의 `JSON.parse` 를 그대로 불러 푼다(같은 수 · 같은 오류 글).
//!   요청 본문은 원본이 읽는 깊이(본문 → runs → 런 → steps → 체크포인트 → vectors → 벡터 → 수)까지만 V 로 옮기고, 그보다 깊은
//!   객체 · 배열은 빈 것으로 둔다(원본은 그 깊이에서 객체인지만 본다). 아주 깊은 본문이 wasm 스택을 넘지 않게 하려는 것이다.
//! - 원본 모듈의 `HASH_IDS` 캐시(hashId 값을 isolate 에 담아 둠)는 밖에서 보이지 않아 옮기지 않았다.
//! - **cron 사본**: 원본은 `../../cron/src/navtrials.ts` 의 NAV_FIRST_DAY · NAV_RECIPE · TRIAL_POLICY · TRIAL_SCHEMA · dayOf · tier ·
//!   walletOldestDay 를 가져다 쓴다. cron crate 를 의존하면 그 `scheduled` wasm 내보내기까지 www 에 들어오므로 여기 사본을 둔다
//!   (cron 의 `navtrials.rs` 에는 DAY_MS · day_of · wallet_oldest_day 가 있다 — 두 곳이 같은 값이어야 한다).
//!   crates/premium-discount-nav 는 쓰지 않았다: identity 말고도 sds · verdict 를 끌어오고, 수를 u32 로 받아 D1 에서 읽은 값의
//!   JS 의미(문자열 더하기 · NaN 비교)와 다르다.

use hmac::{Hmac, Mac};
use premium_discount_worker_common::jsv::{add, js_trim, less_than, num_to_string, strict_eq, to_lower};
use premium_discount_worker_common::util::hash_str;
use premium_discount_worker_common::{obj, Throw, R, V};
use sha2::Sha256;

pub const NAV_TRIALS_PATH: &str = "/nav/trials";

// ───────────── cron/src/navtrials.ts 의 사본 (workers/cron/src/navtrials.rs 와 같은 값) ─────────────

/// `TRIAL_SCHEMA`
pub const TRIAL_SCHEMA: &str = "trial/1";
/// `NAV_RECIPE` — 런의 종류(SDS 결정)를 판정에 넣는 규칙. contracts/verdict_v2.json 의 recipe 와 같다
pub const NAV_RECIPE: &str = "pd-verdict/2";
/// `DAY_MS`
pub const DAY_MS: f64 = 86_400_000.0;
/// `NAV_FIRST_DAY` — 받는 날짜의 하한: 2026-01-01
pub const NAV_FIRST_DAY: f64 = 20454.0;
/// `TRIAL_POLICY.maxSteps`
pub const MAX_STEPS: usize = 10;
/// `TRIAL_POLICY.maxCourses`
pub const MAX_COURSES: usize = 10;
/// `TRIAL_POLICY.walletMaxAgeDays`
pub const WALLET_MAX_AGE_DAYS: f64 = 30.0;
/// `TRIAL_POLICY.publishMinRuns`
pub const PUBLISH_MIN_RUNS: f64 = 11.0;
/// `TRIAL_POLICY.splitUnitMs`
pub const SPLIT_UNIT_MS: f64 = 100.0;
/// `TRIAL_POLICY.maxRunMs`
pub const MAX_RUN_MS: f64 = 3_600_000.0;
/// `TRIAL_POLICY.tierMinDecided`
pub const TIER_MIN_DECIDED: f64 = 3.0;

/// `dayOf(ms)`
pub fn day_of(ms: f64) -> f64 {
    (ms / DAY_MS).floor()
}

/// `walletOldestDay(nowMs)` — 마지막으로 달린 날이 이보다 앞선 지갑 코스는 지운다
pub fn wallet_oldest_day(now_ms: f64) -> f64 {
    day_of(now_ms) - WALLET_MAX_AGE_DAYS + 1.0
}

/// `tier(premium, discount)` — D1 에서 읽은 값 그대로 (원본은 JS `+` · `<` · `*` · `>=`)
pub fn tier(premium: &V, discount: &V) -> &'static str {
    let decided = add(premium, discount);
    if less_than(&decided, &V::Num(TIER_MIN_DECIDED)) == Some(true) {
        return "unrated";
    }
    let two_thirds = decided.to_num() * 2.0;
    if premium.to_num() * 3.0 >= two_thirds {
        "premium"
    } else if discount.to_num() * 3.0 >= two_thirds {
        "discount"
    } else {
        "neutral"
    }
}

// ───────────── navtrials.ts ─────────────

/// 한 요청에 담는 런 수 (콜렉션 하나 크기)
pub const MAX_RUNS: usize = MAX_COURSES;
/// 본문 크기 상한
pub const MAX_BODY_BYTES: f64 = 4.0 * 1024.0 * 1024.0;
const MAX_VECTORS: usize = 4;
const MAX_DIMS: usize = 2048;
/// 기기는 L2 정규화해 보낸다. float32 반올림만큼만 허용한다
const NORM_TOLERANCE: f64 = 1e-3;
pub const WALLET_HEADERS: [&str; 3] = ["x-pd-address", "x-pd-time", "x-pd-signature"];
const STEP_KINDS: [&str; 4] = ["screen", "region", "page", "element"];
/// crates/premium-discount-nav gesture.rs 의 Gesture 이름. press 는 체크포인트에 남기지 않는다
const GESTURES: [&str; 8] = ["swipe-left", "swipe-right", "swipe-up", "swipe-down", "scroll-up", "scroll-down", "scroll-left", "scroll-right"];

/// `/^0x[0-9a-f]{40}$/`
pub fn is_hash(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 42 && b.starts_with(b"0x") && b[2..].iter().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}

/// `/^[a-z0-9][a-z0-9._-]{0,63}$/`
fn is_space(s: &str) -> bool {
    let b = s.as_bytes();
    let head = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    (1..=64).contains(&b.len()) && head(&b[0]) && b[1..].iter().all(|c| head(c) || matches!(c, b'.' | b'_' | b'-'))
}

/// `Number.isSafeInteger(x)` (수일 때만)
fn safe_integer(v: &V) -> Option<f64> {
    match v {
        V::Num(n) if n.is_finite() && n.trunc() == *n && n.abs() <= 9_007_199_254_740_991.0 => Some(*n),
        _ => None,
    }
}

/// `Math.round(x)`
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn js_round(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    let f = x.floor();
    let r = if x - f >= 0.5 { f + 1.0 } else { f };
    if r == 0.0 && x < 0.0 {
        -0.0
    } else {
        r
    }
}

/// 체크포인트 하나 (원본 `StepRef` — 저장하는 steps 에는 id · course 가 늘 들어 있다)
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub kind: &'static str,
    pub hash: String,
    pub gesture: Option<&'static str>,
    pub id: String,
    pub course: String,
}

impl Step {
    /// `stepKey(s)` — `<종류>:<해시>` 또는 `<종류>:<해시>@<제스처>`
    pub fn key(&self) -> String {
        match self.gesture {
            Some(g) => format!("{}:{}@{g}", self.kind, self.hash),
            None => format!("{}:{}", self.kind, self.hash),
        }
    }

    /// 저장 · 판정에 쓰는 JS 객체: `{ kind, hash, gesture?, id, course }` (원본의 펼침 · 대입 차례)
    pub fn to_v(&self) -> V {
        let o = obj! { "kind" => self.kind, "hash" => self.hash.as_str() };
        if let Some(g) = self.gesture {
            o.put("gesture", V::from(g));
        }
        o.put("id", V::from(self.id.as_str()));
        o.put("course", V::from(self.course.as_str()));
        o
    }
}

/// `checkpointId(s)` = hashId(<종류> + <해시> + <제스처>)
pub fn checkpoint_id(kind: &str, hash: &str, gesture: Option<&str>) -> String {
    hash_str(&format!("{kind}{hash}{}", gesture.unwrap_or("")))
}

/// 저장된 체크포인트(아무 JS 값)의 `checkpointId(s)` — `${s.kind}${s.hash}${s.gesture ?? ''}`
pub fn checkpoint_id_v(s: &V) -> R<String> {
    let kind = s.g("kind")?.to_str();
    let hash = s.g("hash")?.to_str();
    let gesture = s.g("gesture")?;
    let gesture = if gesture.is_nullish() { String::new() } else { gesture.to_str() };
    Ok(hash_str(&format!("{kind}{hash}{gesture}")))
}

/// `courseChain(app, steps)` — 첫 값은 hashId(<앱 id> + <체크포인트 id>), 다음부터 hashId(<이전 코스 id> + <체크포인트 id>)
pub fn course_chain(app: &str, ids: &[String]) -> Vec<String> {
    let mut previous = app.to_string();
    ids.iter()
        .map(|id| {
            previous = hash_str(&format!("{previous}{id}"));
            previous.clone()
        })
        .collect()
}

/// 저장된 체크포인트(아무 JS 값)의 `stepKey(s)` — `s.gesture ? \`${s.kind}:${s.hash}@${s.gesture}\` : \`${s.kind}:${s.hash}\``
fn step_key_v(s: &V) -> R<String> {
    let gesture = s.g("gesture")?;
    let (kind, hash) = (s.get("kind").to_str(), s.get("hash").to_str());
    Ok(if gesture.truthy() { format!("{kind}:{hash}@{}", gesture.to_str()) } else { format!("{kind}:{hash}") })
}

/// `routeMarks(run, ghost)` — 고스트 따라가기 판정 (crates/premium-discount-nav route_marks 와 같은 규칙).
/// run 은 이 런의 체크포인트([`Step::to_v`]), ghost 는 저장된 고스트 코스의 steps 를 푼 값 그대로다.
/// 시작 · 결승이 다르면 None, 같으면 고스트의 중간 체크포인트마다 (체크포인트, 1 따라감 | -1 비켜 감)
pub fn route_marks(run: &[V], ghost: &V) -> R<Option<Vec<(V, i32)>>> {
    // if (run.length === 0 || ghost.length === 0) return null
    let ghost_len = ghost.g("length")?;
    if run.is_empty() || strict_eq(&ghost_len, &V::Num(0.0)) {
        return Ok(None);
    }
    let same = |a: &V, b: &V| -> R<bool> { Ok(step_key_v(a)? == step_key_v(b)?) };
    let ghost_first = ghost.get("0");
    let ghost_last = ghost.get(&num_to_string(ghost_len.to_num() - 1.0));
    if !same(&run[0], &ghost_first)? || !same(&run[run.len() - 1], &ghost_last)? {
        return Ok(None);
    }
    // ghost.slice(1, -1)
    let middle: Vec<V> = match ghost {
        V::Arr(_) => {
            let items = ghost.items();
            if items.len() > 2 {
                items[1..items.len() - 1].to_vec()
            } else {
                Vec::new()
            }
        }
        _ => return Err(Throw::type_error("ghost.slice is not a function")),
    };
    let mut out: Vec<(V, i32)> = Vec::new();
    for step in middle {
        let mut seen = false;
        for (m, _) in &out {
            if same(m, &step)? {
                seen = true;
                break;
            }
        }
        if seen {
            continue;
        }
        let mut passed = false;
        for s in run {
            if same(s, &step)? {
                passed = true;
                break;
            }
        }
        out.push((step, if passed { 1 } else { -1 }));
    }
    Ok(Some(out))
}

/// 체크포인트의 벡터 하나 (`{ hash, space, vector: Float32Array }`)
#[derive(Clone, Debug, PartialEq)]
pub struct Vector {
    pub hash: String,
    pub space: String,
    pub vector: Vec<f32>,
}

impl Vector {
    /// `vector.buffer` — float32 리틀 엔디언
    pub fn bytes(&self) -> Vec<u8> {
        self.vector.iter().flat_map(|x| x.to_le_bytes()).collect()
    }
}

/// 확인한 런 (`ParsedRun`)
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedRun {
    pub app: String,
    pub course: String,
    pub steps: Vec<Step>,
    pub splits: Vec<f64>,
    pub total: f64,
    /// 1 premium · -1 discount · 0 결정 없음
    pub kind: i32,
    pub day: f64,
    pub vectors: Vec<Vector>,
    /// 따라간 다른 사용자의 코스 (없으면 None)
    pub ghost: Option<String>,
    /// Phase 4 Part 1 (FC-05/FC-06): 스텝별 사용자·환경 귀책 비트마스크 (생략 시 0)
    pub fault_mask: u16,
}

impl ParsedRun {
    /// `run.steps` 를 JS 객체 배열로
    pub fn steps_v(&self) -> Vec<V> {
        self.steps.iter().map(Step::to_v).collect()
    }
    /// `run.splits` (새 배열 — 원본은 같은 배열을 여러 칸에 넣지만 고치지 않으므로 같다)
    pub fn splits_v(&self) -> V {
        V::from(self.splits.iter().map(|s| V::Num(*s)).collect::<Vec<_>>())
    }
}

/// `parseHash(value, field)` — 글이면 trim · 소문자, 0x + 40 hex 여야 한다
fn parse_hash(value: &V, field: &str) -> Result<String, String> {
    let hash = match value {
        V::Str(s) => to_lower(js_trim(s)),
        _ => String::new(),
    };
    if !is_hash(&hash) {
        return Err(format!("{field} must be 0x + 40 hex"));
    }
    Ok(hash)
}

/// `entry && typeof entry === 'object' ? entry : {}`
fn object_or_empty(entry: &V) -> V {
    if entry.truthy() && entry.type_of() == "object" {
        entry.clone()
    } else {
        V::new_obj()
    }
}

/// `parseVectors(value, hash)` — Err 는 RunInvalid 의 글
fn parse_vectors(value: &V, hash: &str) -> Result<Vec<Vector>, String> {
    if value.is_undef() {
        return Ok(Vec::new());
    }
    if !value.is_arr() || value.len() > MAX_VECTORS {
        return Err(format!("vectors must hold up to {MAX_VECTORS} entries"));
    }
    let mut out: Vec<Vector> = Vec::new();
    for entry in value.items() {
        let v = object_or_empty(&entry);
        let space = match v.get("space") {
            V::Str(s) => s,
            _ => String::new(),
        };
        if !is_space(&space)
            || !crate::navtrials_v2::is_allowed_upload_space(&space)
            || out.iter().any(|x| x.space == space)
        {
            return Err("spaces must be named and distinct".to_string());
        }
        let raw = v.get("vector");
        let items = raw.items();
        if !raw.is_arr() || items.is_empty() || items.len() > MAX_DIMS || !items.iter().all(|x| matches!(x, V::Num(n) if n.is_finite())) {
            return Err(format!("vector must hold 1 to {MAX_DIMS} finite numbers"));
        }
        // Float32Array.from(raw): 가장 가까운 float32 로 (넘치면 Infinity)
        let vector: Vec<f32> = items.iter().map(|x| x.to_num() as f32).collect();
        let mut norm = 0.0f64;
        for x in &vector {
            let x = *x as f64;
            norm += x * x;
        }
        if (norm.sqrt() - 1.0).abs() > NORM_TOLERANCE {
            return Err("vector must be L2-normalized".to_string());
        }
        out.push(Vector { hash: hash.to_string(), space, vector });
    }
    Ok(out)
}

/// `parseRun(value, today)` — trial/1 런 하나를 확인한다. Err 는 RunInvalid 의 글 (원본은 그 밖의 예외를 던질 자리가 없다)
pub fn parse_run(value: &V, today: f64) -> Result<ParsedRun, String> {
    if !matches!(value, V::Obj(_)) {
        return Err("run must be an object".to_string());
    }
    let r = value;
    if r.get("schema").as_str() != Some(TRIAL_SCHEMA) {
        return Err(format!("schema must be {TRIAL_SCHEMA}"));
    }
    if r.get("recipe").as_str() != Some(NAV_RECIPE) {
        return Err(format!("recipe must be {NAV_RECIPE}"));
    }
    let app = parse_hash(&r.get("app"), "app")?;
    let day = match safe_integer(&r.get("day")) {
        Some(d) if d >= NAV_FIRST_DAY && d <= today + 1.0 => d,
        _ => return Err("day must be a UTC day number between 2026-01-01 and tomorrow".to_string()),
    };
    let kind = match r.get("type") {
        V::Str(s) if s == "premium" => 1,
        V::Str(s) if s == "discount" => -1,
        V::Null => 0,
        _ => return Err("type must be premium, discount or null".to_string()),
    };
    let raw_steps = r.get("steps");
    if !raw_steps.is_arr() || raw_steps.len() < 2 || raw_steps.len() > MAX_STEPS {
        return Err(format!("steps must hold 2 to {MAX_STEPS} checkpoints (the device keeps the last {MAX_STEPS})"));
    }
    let mut steps: Vec<Step> = Vec::new();
    let mut splits: Vec<f64> = Vec::new();
    let mut vectors: Vec<Vector> = Vec::new();
    // 기기가 함께 보낸 체크포인트마다의 코스 id (있으면 다시 계산해 맞춰 본다)
    let mut sent_courses: Vec<V> = Vec::new();
    for entry in raw_steps.items() {
        let s = object_or_empty(&entry);
        let step_kind = s.get("kind");
        let Some(kind) = STEP_KINDS.iter().copied().find(|k| step_kind.as_str() == Some(*k)) else {
            return Err(format!("step kind must be one of {}", STEP_KINDS.join(", ")));
        };
        let hash = parse_hash(&s.get("hash"), "step hash")?;
        let raw_gesture = s.get("gesture");
        let mut gesture = None;
        if !raw_gesture.is_nullish() && raw_gesture.as_str() != Some("press") {
            gesture = GESTURES.iter().copied().find(|g| raw_gesture.as_str() == Some(*g));
            if gesture.is_none() {
                return Err(format!("gesture must be press or one of {}", GESTURES.join(", ")));
            }
        }
        let id = checkpoint_id(kind, &hash, gesture);
        let sent_id = s.get("id");
        if !sent_id.is_undef() && sent_id.as_str() != Some(id.as_str()) {
            return Err("step id does not match the checkpoint".to_string());
        }
        let step = Step { kind, hash: hash.clone(), gesture, id, course: String::new() };
        let split = match safe_integer(&s.get("split_ms")) {
            Some(x) if x >= 0.0 && x % SPLIT_UNIT_MS == 0.0 => x,
            _ => return Err(format!("split_ms must be a multiple of {}", num_to_string(SPLIT_UNIT_MS))),
        };
        if steps.last().is_some_and(|prev| prev.key() == step.key()) {
            return Err("the same checkpoint must not repeat in a row".to_string());
        }
        let bad_split = match splits.last() {
            None => split != 0.0,
            Some(last) => split < *last,
        };
        if bad_split {
            return Err("splits must start at 0 and never go down".to_string());
        }
        steps.push(step);
        splits.push(split);
        sent_courses.push(s.get("course"));
        vectors.extend(parse_vectors(&s.get("vectors"), &hash)?);
    }
    let total = splits[splits.len() - 1];
    if !matches!(r.get("total_ms"), V::Num(t) if t == total) {
        return Err("total_ms must equal the last split".to_string());
    }
    if total > MAX_RUN_MS {
        return Err("a run must finish within an hour".to_string());
    }
    let course = parse_hash(&r.get("course"), "course")?;
    let ids: Vec<String> = steps.iter().map(|s| s.id.clone()).collect();
    let chain = course_chain(&app, &ids);
    if Some(&course) != chain.last() {
        return Err("course does not match the checkpoints".to_string());
    }
    for (i, step) in steps.iter_mut().enumerate() {
        if !sent_courses[i].is_undef() && sent_courses[i].as_str() != Some(chain[i].as_str()) {
            return Err("step course does not match the checkpoints so far".to_string());
        }
        step.course = chain[i].clone();
    }
    let raw_ghost = r.get("ghost");
    let ghost = if raw_ghost.is_nullish() { None } else { Some(parse_hash(&raw_ghost, "ghost")?) };
    let fault_mask = match r.get("fault_mask") {
        V::Undef | V::Null => 0u16,
        V::Num(n) if n.is_finite() && n.trunc() == n && (0.0..=65535.0).contains(&n) => n as u16,
        _ => return Err("fault_mask must be an integer between 0 and 65535".to_string()),
    };
    Ok(ParsedRun { app, course, steps, splits, total, kind, day, vectors, ghost, fault_mask })
}

/// `accountKey(secret, kind, address)` = HMAC-SHA256(AUTHOR_KEY_SECRET, "nav-account|<종류>|<주소>") hex.
/// 비밀이 비었으면(빈 글 · 없음) 원본처럼 던진다
pub fn account_key(secret: &V, kind: &str, address: &str) -> R<String> {
    if !secret.truthy() {
        return Err(Throw::error("AUTHOR_KEY_SECRET is not configured"));
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.to_str().as_bytes()).expect("HMAC 는 어떤 길이의 키도 받는다");
    mac.update(format!("nav-account|{kind}|{}", to_lower(address)).as_bytes());
    Ok(crate::util::hex(&mac.finalize().into_bytes()))
}

/// `navCors(origin)` — corsHeaders 위에 메서드 · 머리글을 덮는다
pub fn nav_cors(origin: &str) -> Vec<(String, String)> {
    let mut h = crate::util::cors_headers(origin);
    for (k, v) in [
        ("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS".to_string()),
        ("Access-Control-Allow-Headers", format!("Content-Type, x-oauth-hash, x-oauth-token, {}", WALLET_HEADERS.join(", "))),
    ] {
        match h.iter_mut().find(|(name, _)| name == k) {
            Some(slot) => slot.1 = v,
            None => h.push((k.to_string(), v)),
        }
    }
    h
}

#[cfg(target_arch = "wasm32")]
pub use wasm::handle;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use indexmap::{IndexMap, IndexSet};
    use js_sys::{Array, Object, Reflect};
    use premium_discount_worker_common::js::{self, Env};
    use premium_discount_worker_common::jsv::string_to_number;
    use premium_discount_worker_common::web::{Req, Resp};
    use premium_discount_worker_common::{console_log, gzip, platform, stringify};
    use wasm_bindgen::{JsCast, JsValue};

    use super::*;
    use crate::auth::{read_session_credentials, verified_address, verify_wallet_request_once};
    use crate::navtrials_v2::{
        apply_ballot_delta, ballot_id, compute_course_traits, dead_end_ballot_id, duel_reg_id,
        encode_unit_i8_b64, epoch_pulse_id, evaluate_vector_consensus, extract_server_shortcut_duels,
        half_life_epoch, parse_dead_end_marks, plan_wallet_claim_registrations,
        representative_best_swap, route_marks_v2, select_stratified_portfolio,
        should_pioneer_promote, update_ema_splits, valid_batch_id, BallotDelta, ParsedDeadEnd,
        PortfolioCourse, VectorConsensusAction, CLAIM_WALLET_PATH,
    };
    use crate::ratelimit::{is_rate_limited, RETRY_AFTER_SECONDS};
    use crate::util::{changes_of, empty_response, header_or_empty, hv, json_response, pathname};

    /// 본문을 V 로 옮기는 깊이 (본문 0 → runs 1 → 런 2 → steps 3 → 체크포인트 4 → vectors 5 → 벡터 6 → vector 7 → 수 8).
    /// 이보다 깊은 객체 · 배열은 빈 것으로 둔다 — 원본은 그 깊이에서 객체인지(형)만 본다
    const BODY_DEPTH: u32 = 10;

    /// IN 바인딩 한 번의 수 (D1 문장당 바인딩 100 이하)
    const IN_CHUNK: usize = 90;

    /// `Math.max(a, b)` — b 는 늘 수 (날짜)
    fn js_max(a: &V, b: f64) -> V {
        let x = a.to_num();
        V::Num(if x.is_nan() || b.is_nan() { f64::NAN } else { x.max(b) })
    }

    /// JS `a >= b` (b 는 수)
    fn js_ge(a: &V, b: f64) -> bool {
        less_than(a, &V::Num(b)) == Some(false)
    }

    /// Map · Set 의 키 (SameValueZero 와 같은 뜻: 형이 다르면 다른 키)
    fn mk(v: &V) -> String {
        match v {
            V::Str(s) => format!("s{s}"),
            V::Num(n) => format!("n{}", num_to_string(*n)),
            other => format!("{}{}", other.type_of(), other.to_str()),
        }
    }

    /// `JSON.stringify(x)` 를 묶을 값으로 (undefined 면 undefined)
    fn json(v: &V) -> V {
        stringify(v).map(V::Str).unwrap_or(V::Undef)
    }

    // ───────────── SQL (0007 + 0011..0015 Phase 4/5/7 확장) ─────────────

    const SQL_EXPIRE_MINE: &str = "DELETE FROM nav_trials WHERE \"account\" = ?1 AND \"last_day\" < ?2";
    const SQL_COLLECTION: &str = "SELECT * FROM nav_trials WHERE \"account\" = ?1 AND \"app\" = ?2";
    const SQL_ALL_MINE: &str = "SELECT * FROM nav_trials WHERE \"account\" = ?1";
    const SQL_EVICT: &str = "DELETE FROM nav_trials WHERE \"account\" = ?1 AND \"app\" = ?2 AND \"course\" = ?3";
    // 고침(fix-nav-lost-update) + Phase 5 Part 2/3 (best_total_ms 단조 병합 · pinned 보호)
    const SQL_EVICT_IF: &str = "DELETE FROM nav_trials WHERE \"account\" = ?1 AND \"app\" = ?2 AND \"course\" = ?3 AND \"touched\" = ?4";
    const SQL_TRIAL_ROW: &str = "SELECT * FROM nav_trials WHERE \"account\" = ?1 AND \"app\" = ?2 AND \"course\" = ?3";
    const SQL_INSERT_TRIAL: &str = "INSERT INTO nav_trials (\"account\", \"kind\", \"app\", \"course\", \"steps\", \"runs\", \"premium\", \"discount\", \"best\", \"last\", \"last_day\", \"touched\", \"best_total_ms\") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) ON CONFLICT (\"account\", \"app\", \"course\") DO NOTHING";
    const SQL_UPDATE_TRIAL: &str = "UPDATE nav_trials SET \"runs\" = \"runs\" + ?5, \"premium\" = \"premium\" + ?6, \"discount\" = \"discount\" + ?7, \"best\" = CASE WHEN ?12 > 0 AND (\"best_total_ms\" = 0 OR ?12 < \"best_total_ms\") THEN ?8 ELSE \"best\" END, \"best_total_ms\" = CASE WHEN ?12 > 0 AND (\"best_total_ms\" = 0 OR ?12 < \"best_total_ms\") THEN ?12 ELSE \"best_total_ms\" END, \"last\" = ?9, \"last_day\" = MAX(\"last_day\", ?10), \"touched\" = ?11 WHERE \"account\" = ?1 AND \"app\" = ?2 AND \"course\" = ?3 AND \"best\" = ?4";
    const SQL_MERGE_CLAIM_TRIAL: &str = "INSERT INTO nav_trials (\"account\", \"kind\", \"app\", \"course\", \"steps\", \"runs\", \"premium\", \"discount\", \"best\", \"last\", \"last_day\", \"touched\", \"best_total_ms\", \"pinned\") VALUES (?1, 'session', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) ON CONFLICT (\"account\", \"app\", \"course\") DO UPDATE SET \"runs\" = \"runs\" + excluded.\"runs\", \"premium\" = \"premium\" + excluded.\"premium\", \"discount\" = \"discount\" + excluded.\"discount\", \"best\" = CASE WHEN excluded.\"best_total_ms\" > 0 AND (nav_trials.\"best_total_ms\" = 0 OR excluded.\"best_total_ms\" < nav_trials.\"best_total_ms\") THEN excluded.\"best\" ELSE nav_trials.\"best\" END, \"best_total_ms\" = CASE WHEN excluded.\"best_total_ms\" > 0 AND (nav_trials.\"best_total_ms\" = 0 OR excluded.\"best_total_ms\" < nav_trials.\"best_total_ms\") THEN excluded.\"best_total_ms\" ELSE nav_trials.\"best_total_ms\" END, \"last_day\" = MAX(nav_trials.\"last_day\", excluded.\"last_day\"), \"touched\" = MAX(nav_trials.\"touched\", excluded.\"touched\"), \"pinned\" = MAX(nav_trials.\"pinned\", excluded.\"pinned\")";
    const SQL_COURSE_ROW: &str = "SELECT * FROM nav_courses WHERE \"course\" = ?1";
    const SQL_INSERT_COURSE: &str = "INSERT INTO nav_courses (\"course\", \"app\", \"steps\", \"runs\", \"premium\", \"discount\", \"record\", \"sums\", \"last_day\", \"start_cp\", \"goal_cp\", \"pace_ms\", \"interaction_traits\", \"ema_splits\", \"era_record\", \"era_record_day\") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) ON CONFLICT (\"course\") DO NOTHING";
    const SQL_UPDATE_COURSE: &str = "UPDATE nav_courses SET \"runs\" = \"runs\" + 1, \"premium\" = \"premium\" + ?4, \"discount\" = \"discount\" + ?5, \"record\" = ?6, \"sums\" = ?7, \"last_day\" = MAX(\"last_day\", ?8), \"start_cp\" = CASE WHEN \"start_cp\" = '' THEN ?9 ELSE \"start_cp\" END, \"goal_cp\" = CASE WHEN \"goal_cp\" = '' THEN ?10 ELSE \"goal_cp\" END, \"pace_ms\" = ?11, \"interaction_traits\" = ?12, \"ema_splits\" = ?13, \"era_record\" = ?14, \"era_record_day\" = ?15 WHERE \"course\" = ?1 AND \"record\" = ?2 AND \"sums\" = ?3";
    const SQL_SWAP_COURSE_REP: &str = "UPDATE nav_courses SET \"record\" = ?4, \"sums\" = ?5, \"pace_ms\" = ?6, \"ema_splits\" = ?7, \"last_day\" = MAX(\"last_day\", ?8) WHERE \"course\" = ?1 AND \"record\" = ?2 AND \"sums\" = ?3";
    const SQL_COUNT_UP: &str = "INSERT INTO nav_counts (\"hash\", \"day\", \"type\", \"recipe\", \"n\") VALUES (?1, ?2, ?3, ?4, 1)\n\t\t\t\t ON CONFLICT (\"hash\", \"day\", \"type\", \"recipe\") DO UPDATE SET \"n\" = \"n\" + 1";
    const SQL_COUNT_DEC: &str = "UPDATE nav_counts SET \"n\" = MAX(0, \"n\" - 1) WHERE \"hash\" = ?1 AND \"type\" = ?2 AND \"recipe\" = ?3 AND \"n\" > 0 AND \"day\" = (SELECT MAX(\"day\") FROM nav_counts WHERE \"hash\" = ?1 AND \"type\" = ?2 AND \"recipe\" = ?3 AND \"n\" > 0)";
    const SQL_ROUTE_FIT_UP: &str = "INSERT INTO nav_route_fit_counts (\"hash\", \"goal\", \"day\", \"type\", \"recipe\", \"n\") VALUES (?1, ?2, ?3, ?4, ?5, 1) ON CONFLICT (\"hash\", \"goal\", \"day\", \"type\", \"recipe\") DO UPDATE SET \"n\" = \"n\" + 1";
    const SQL_ROUTE_FIT_DEC: &str = "UPDATE nav_route_fit_counts SET \"n\" = MAX(0, \"n\" - 1) WHERE \"hash\" = ?1 AND \"goal\" = ?2 AND \"type\" = ?3 AND \"recipe\" = ?4 AND \"n\" > 0 AND \"day\" = (SELECT MAX(\"day\") FROM nav_route_fit_counts WHERE \"hash\" = ?1 AND \"goal\" = ?2 AND \"type\" = ?3 AND \"recipe\" = ?4 AND \"n\" > 0)";
    const SQL_BALLOT_GET: &str = "SELECT \"sign\" FROM nav_route_ballots WHERE \"id\" = ?1";
    const SQL_BALLOT_UPSERT: &str = "INSERT INTO nav_route_ballots (\"id\", \"sign\", \"day\") VALUES (?1, ?2, ?3) ON CONFLICT (\"id\") DO UPDATE SET \"sign\" = excluded.\"sign\", \"day\" = excluded.\"day\"";
    const SQL_DUEL_REG: &str = "INSERT INTO nav_shortcut_duel_regs (\"id\", \"day\") VALUES (?1, ?2) ON CONFLICT DO NOTHING";
    const SQL_DUEL_UPSERT: &str = "INSERT INTO nav_shortcut_duels (\"fork_course\", \"skipped_cp\", \"taken_cp\", \"app\", \"wins\", \"saved_ms_sum\", \"margin_sum\", \"promoted\", \"last_day\") VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, 0, ?7) ON CONFLICT (\"fork_course\", \"skipped_cp\", \"taken_cp\") DO UPDATE SET \"wins\" = \"wins\" + 1, \"saved_ms_sum\" = \"saved_ms_sum\" + excluded.\"saved_ms_sum\", \"margin_sum\" = \"margin_sum\" + excluded.\"margin_sum\", \"last_day\" = MAX(\"last_day\", excluded.\"last_day\")";
    const SQL_DUEL_GET: &str = "SELECT \"wins\", \"saved_ms_sum\", \"margin_sum\", \"promoted\" FROM nav_shortcut_duels WHERE \"fork_course\" = ?1 AND \"skipped_cp\" = ?2 AND \"taken_cp\" = ?3";
    const SQL_DUEL_PROMOTE: &str = "UPDATE nav_shortcut_duels SET \"promoted\" = 1 WHERE \"fork_course\" = ?1 AND \"skipped_cp\" = ?2 AND \"taken_cp\" = ?3";
    const SQL_DUELS_PROMOTED: &str = "SELECT \"fork_course\", \"skipped_cp\", \"taken_cp\", \"wins\", \"saved_ms_sum\", \"margin_sum\" FROM nav_shortcut_duels WHERE \"app\" = ?1 AND \"promoted\" = 1 ORDER BY \"wins\" DESC LIMIT 32";
    const SQL_REGISTER: &str = "INSERT INTO nav_registrations (\"id\", \"day\", \"rep_splits\", \"rep_type\") VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING";
    const SQL_REGISTER_PRESEED: &str = "INSERT INTO nav_registrations (\"id\", \"day\") VALUES (?1, ?2) ON CONFLICT DO NOTHING";
    const SQL_REGISTER_GET: &str = "SELECT \"rep_splits\", \"rep_type\" FROM nav_registrations WHERE \"id\" = ?1";
    const SQL_REGISTER_UPDATE_REP: &str = "UPDATE nav_registrations SET \"rep_splits\" = ?2, \"rep_type\" = ?3, \"day\" = MAX(\"day\", ?4) WHERE \"id\" = ?1";
    const SQL_EPOCH_PULSE: &str = "INSERT INTO nav_epoch_pulses (\"id\", \"epoch\", \"day\", \"sign\") VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING";
    const SQL_DIRTY_MARK: &str = "INSERT INTO nav_verdict_dirty (\"hash\", \"kind\", \"app\", \"scope_to\", \"at\") VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (\"hash\") DO UPDATE SET \"at\" = excluded.\"at\"";
    const SQL_BATCH_GET: &str = "SELECT \"response_json\" FROM nav_batch_receipts WHERE \"batch_id\" = ?1 AND \"account\" = ?2 AND \"created_ms\" >= ?3";
    const SQL_BATCH_PUT: &str = "INSERT INTO nav_batch_receipts (\"batch_id\", \"account\", \"response_json\", \"created_ms\") VALUES (?1, ?2, ?3, ?4) ON CONFLICT (\"batch_id\") DO NOTHING";
    const SQL_VECTOR_GET: &str = "SELECT \"dims\", \"vector\", \"confirmations\", \"locked\", \"candidate_vector\", \"candidate_hits\" FROM nav_vectors WHERE \"hash\" = ?1 AND \"space\" = ?2";
    const SQL_VECTOR_INSERT: &str = "INSERT INTO nav_vectors (\"hash\", \"space\", \"dims\", \"vector\", \"day\", \"confirmations\", \"locked\") VALUES (?1, ?2, ?3, ?4, ?5, 1, 0) ON CONFLICT DO NOTHING";
    const SQL_VECTOR_CONFIRM: &str = "UPDATE nav_vectors SET \"vector\" = ?3, \"confirmations\" = ?4, \"locked\" = ?5, \"day\" = MAX(\"day\", ?6) WHERE \"hash\" = ?1 AND \"space\" = ?2 AND \"locked\" = 0";
    const SQL_VECTOR_CANDIDATE: &str = "UPDATE nav_vectors SET \"candidate_vector\" = ?3, \"candidate_hits\" = ?4 WHERE \"hash\" = ?1 AND \"space\" = ?2 AND \"locked\" = 0";
    const SQL_VECTOR_REPLACE: &str = "UPDATE nav_vectors SET \"vector\" = ?3, \"confirmations\" = 2, \"locked\" = 0, \"candidate_vector\" = NULL, \"candidate_hits\" = 0, \"day\" = MAX(\"day\", ?4) WHERE \"hash\" = ?1 AND \"space\" = ?2 AND \"locked\" = 0";
    const SQL_MINE: &str = "SELECT * FROM nav_trials WHERE \"account\" = ?1 AND \"app\" = ?2 ORDER BY \"touched\" DESC";
    const SQL_TOP: &str = "SELECT * FROM nav_courses WHERE \"app\" = ?1 AND \"runs\" >= ?2 ORDER BY \"runs\" DESC, \"course\" LIMIT ?3";
    const SQL_TOP_ENTRY: &str = "SELECT * FROM nav_courses WHERE \"app\" = ?1 AND \"runs\" >= ?2 AND \"start_cp\" = ?3 ORDER BY \"pace_ms\" ASC, \"runs\" DESC LIMIT 16";
    const SQL_TOP_ACCESSIBLE: &str = "SELECT * FROM nav_courses WHERE \"app\" = ?1 AND \"runs\" >= ?2 AND (\"interaction_traits\" & 3) = 3 ORDER BY \"pace_ms\" ASC, \"runs\" DESC LIMIT 12";
    const SQL_DELETE_APP: &str = "DELETE FROM nav_trials WHERE \"account\" = ?1 AND \"app\" = ?2";
    const SQL_DELETE_ALL: &str = "DELETE FROM nav_trials WHERE \"account\" = ?1";

    fn sql_courses(n: usize) -> String {
        format!("SELECT * FROM nav_courses WHERE \"course\" IN ({})", crate::util::binds(n))
    }
    fn sql_counts(n: usize) -> String {
        let marks = (0..n).map(|i| format!("?{}", i + 2)).collect::<Vec<_>>().join(", ");
        format!("SELECT \"hash\", \"type\", SUM(\"n\") AS \"n\" FROM nav_counts WHERE \"recipe\" = ?1 AND \"hash\" IN ({marks}) GROUP BY \"hash\", \"type\"")
    }
    fn sql_route_fit_counts(n: usize) -> String {
        let marks = (0..n).map(|i| format!("?{}", i + 2)).collect::<Vec<_>>().join(", ");
        format!("SELECT \"hash\", \"goal\", \"type\", SUM(\"n\") AS \"n\" FROM nav_route_fit_counts WHERE \"recipe\" = ?1 AND \"hash\" IN ({marks}) GROUP BY \"hash\", \"goal\", \"type\"")
    }
    fn sql_verdicts(n: usize) -> String {
        format!("SELECT * FROM nav_verdicts WHERE \"hash\" IN ({})", crate::util::binds(n))
    }
    fn sql_confirmed_vectors(n: usize) -> String {
        format!("SELECT \"hash\", \"space\", \"dims\", \"vector\" FROM nav_vectors WHERE \"confirmations\" >= 2 AND \"hash\" IN ({})", crate::util::binds(n))
    }

    fn v_to_f32_vec(v: &V) -> Option<Vec<f32>> {
        let bytes: Vec<u8> = match v {
            V::Bytes(b) => b.borrow().clone(),
            V::Arr(_) => v.items().into_iter().map(|x| x.to_num() as u8).collect(),
            _ => return None,
        };
        if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
            return None;
        }
        Some(
            bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
        )
    }

    fn f32_vec_to_bytes(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn parse_f64_array(v: &V) -> Option<Vec<f64>> {
        if !v.is_arr() {
            return None;
        }
        let items = v.items();
        if items.is_empty() {
            return None;
        }
        Some(items.into_iter().map(|x| x.to_num()).collect())
    }

    fn splits_v_from_f64(xs: &[f64]) -> V {
        V::from(xs.iter().map(|&x| V::Num(x)).collect::<Vec<_>>())
    }

    fn verdict_row_to_v(r: &V) -> V {
        obj! {
            "tier" => r.get("tier"),
            "grade" => r.get("grade"),
            "evidence" => r.get("evidence"),
            "value" => r.get("value"),
            "level" => r.get("level"),
            "rank" => r.get("rank"),
            "superseded" => r.get("superseded").to_num() != 0.0,
        }
    }

    /// 계정 (`NavAccount`)
    struct Account {
        kind: &'static str,
        key: String,
    }

    /// `reply(body, status, origin, extra)`
    fn reply(body: V, status: u16, origin: &str, extra: &[(&str, &str)]) -> Resp {
        let mut headers = hv(&[("Content-Type", "application/json")]);
        headers.extend(nav_cors(origin));
        headers.extend(hv(extra));
        json_response(&body, status, headers)
    }

    fn error(message: &str, status: u16, origin: &str) -> Resp {
        reply(obj! { "error" => message }, status, origin, &[])
    }

    /// `JSON.parse(x)` — 런타임의 JSON.parse 그대로 (글이 아니면 String(x) 를 푼다. 같은 수 · 같은 오류 글)
    fn js_parse(text: &V) -> R<V> {
        js_sys::JSON::parse(&text.to_str()).map(|j| js::to_v(&j)).map_err(js::to_throw)
    }

    /// 요청 본문의 JS 값 → V ([`BODY_DEPTH`] 까지)
    fn body_v(j: &JsValue, depth: u32) -> V {
        if Array::is_array(j) {
            if depth >= BODY_DEPTH {
                return V::new_arr();
            }
            let a: &Array = j.unchecked_ref();
            return V::from(a.iter().map(|x| body_v(&x, depth + 1)).collect::<Vec<_>>());
        }
        if j.is_object() {
            if depth >= BODY_DEPTH {
                return V::new_obj();
            }
            let out = V::new_obj();
            let o: &Object = j.unchecked_ref();
            for k in Object::keys(o).iter() {
                if let Some(key) = k.as_string() {
                    out.put(&key, body_v(&Reflect::get(j, &k).unwrap_or(JsValue::UNDEFINED), depth + 1));
                }
            }
            return out;
        }
        js::to_v(j)
    }

    fn stmt(env: &Env, sql: &str, args: &[V]) -> R<V> {
        js::bind_exact(&env.prepare("DB", sql)?, args)
    }

    /// `(await db.prepare(sql).bind(...args).all()).results`
    async fn rows(env: &Env, sql: &str, args: &[V]) -> R<Vec<V>> {
        Ok(js::all(&stmt(env, sql, args)?).await?.g("results")?.items())
    }

    /// `Number(res.meta?.changes ?? 0)`
    fn changes(res: &V) -> R<f64> {
        let meta = res.g("meta")?;
        let changes = if meta.is_nullish() { V::Undef } else { meta.get("changes") };
        Ok(if changes.is_nullish() { 0.0 } else { changes.to_num() })
    }

    /// `[...new Set(xs)]` (SameValueZero)
    fn unique(xs: impl IntoIterator<Item = V>) -> Vec<V> {
        let mut seen = IndexSet::new();
        let mut out = Vec::new();
        for x in xs {
            if seen.insert(mk(&x)) {
                out.push(x);
            }
        }
        out
    }

    /// `readCourses(db, courses)` — 코스 id → 합계 행 (record · sums 는 푼 값)
    async fn read_courses(env: &Env, courses: &[V]) -> R<IndexMap<String, V>> {
        let mut out = IndexMap::new();
        for part in courses.chunks(IN_CHUNK) {
            for r in rows(env, &sql_courses(part.len()), part).await? {
                // { ...r, record: JSON.parse(r.record), sums: JSON.parse(r.sums), stored: { record: r.record, sums: r.sums } }
                // stored 는 읽은 글 그대로 — 합계의 조건 건 갱신(fix-nav-lost-update)이 그 글과 견준다
                let stored = obj! { "record" => r.get("record"), "sums" => r.get("sums") };
                let record = js_parse(&r.g("record")?)?;
                let sums = js_parse(&r.g("sums")?)?;
                r.put("record", record);
                r.put("sums", sums);
                r.put("stored", stored);
                out.insert(mk(&r.get("course")), r);
            }
        }
        Ok(out)
    }

    /// `row.best[row.best.length - 1]` 처럼 배열 끝 값 (row.best 가 null · undefined 면 원본처럼 TypeError)
    fn last_of(xs: &V) -> R<V> {
        let len = xs.g("length")?;
        Ok(xs.get(&num_to_string(len.to_num() - 1.0)))
    }

    /// 고침(fix-nav-lost-update): 조건 건 쓰기가 맞지 않을 때(다른 요청이 그 사이 그 행을 바꿈) 다시 읽어 계산해 다시 쓰는 횟수
    const NAV_RETRIES: usize = 3;
    /// 다시 시도를 다 쓰면 던지는 글 (처리기의 catch → console 뒤 500)
    const NAV_CONFLICT: &str = "nav trials: concurrent updates did not settle";

    /// 이 요청이 내 콜렉션의 한 행에 더한 런들 (`RunAdds`): 수(상대 갱신) · 그 런들 중 최고 기록(처음으로 가장 빠른 것) · 가장 늦은 날짜
    struct Adds {
        runs: f64,
        premium: f64,
        discount: f64,
        best: V,
        best_total: f64,
        last_day: f64,
    }

    /// `note(key, run, premium, discount)` — 런 하나를 그 행의 adds 에 더한다
    fn note(adds: &mut IndexMap<String, Adds>, key: &str, run: &ParsedRun, premium: f64, discount: f64) {
        let Some(a) = adds.get_mut(key) else {
            adds.insert(key.to_string(), Adds { runs: 1.0, premium, discount, best: run.splits_v(), best_total: run.total, last_day: run.day });
            return;
        };
        a.runs += 1.0;
        a.premium += premium;
        a.discount += discount;
        if run.total < a.best_total {
            a.best = run.splits_v();
            a.best_total = run.total;
        }
        a.last_day = a.last_day.max(run.day);
    }

    /// batch 의 조건 건 쓰기 (`WriteCheck`): 바뀐 행이 0 이면 무엇을 다시 할지
    enum Check {
        Evict { app: String, index: usize },
        Trial { key: String },
        Course { course: String, insert: bool },
    }

    /// `sums.map((s, k) => s + (run.splits[k] ?? 0))` — 배열이 아니면 원본처럼 TypeError (`<expr>.map is not a function`)
    fn sums_plus(sums: &V, run: &ParsedRun, expr: &str) -> R<Vec<V>> {
        if !sums.is_arr() {
            return Err(Throw::type_error(format!("{expr}.map is not a function")));
        }
        Ok(sums.items().iter().enumerate().map(|(k, s)| add(s, &V::Num(run.splits.get(k).copied().unwrap_or(0.0)))).collect())
    }

    /// `storeRuns(db, account, items, nowMs)` — 확인 → (지갑) 기한 지난 내 코스 지우기 → 앱마다 콜렉션 읽기 → 보낸 순서대로 적용 →
    /// 등록 잡기 → 계정 없는 합계 · 개수 · 대표 벡터와 함께 한 번에 쓰기 → 조건이 맞지 않은 쓰기만 다시 (fix-nav-lost-update, 머리말)
    async fn store_runs(env: &Env, account: &Account, items: &[V], now_ms: f64) -> R<Vec<V>> {
        let today = day_of(now_ms);
        let oldest = if account.kind == "wallet" { wallet_oldest_day(now_ms) } else { NAV_FIRST_DAY };
        let mut results: Vec<V> = Vec::new();
        let mut runs: Vec<Option<ParsedRun>> = Vec::new();
        for item in items {
            match parse_run(item, today) {
                Ok(run) => {
                    let expired = run.day < oldest;
                    results.push(obj! { "course" => run.course.as_str(), "status" => if expired { "expired" } else { "stored" } });
                    runs.push(if expired { None } else { Some(run) });
                }
                Err(message) => {
                    runs.push(None);
                    results.push(obj! { "course" => V::Null, "status" => "invalid", "error" => message });
                }
            }
        }
        if runs.iter().all(Option::is_none) {
            return Ok(results);
        }
        let key = V::from(account.key.as_str());

        // 지갑: 기한이 지난 내 코스부터 지운다 (모든 지갑은 cron 도 1분마다 지운다)
        if account.kind == "wallet" {
            js::run(&stmt(env, SQL_EXPIRE_MINE, &[key.clone(), V::Num(oldest)])?).await?;
        }

        // 앱마다 내 콜렉션 (최대 10행). base 는 읽은 행 그대로 — 조건 건 쓰기(fix-nav-lost-update)가 그 값(best · touched)을 본다
        let mut collections: IndexMap<String, IndexMap<String, V>> = IndexMap::new();
        let apps: IndexSet<String> = runs.iter().flatten().map(|r| r.app.clone()).collect();
        for app in &apps {
            let mut collection = IndexMap::new();
            for r in rows(env, SQL_COLLECTION, &[key.clone(), V::from(app.as_str())]).await? {
                // { ...r, best: JSON.parse(r.best), last: JSON.parse(r.last), base: r }
                let base = r.deep_clone();
                let best = js_parse(&r.g("best")?)?;
                let last = js_parse(&r.g("last")?)?;
                r.put("best", best);
                r.put("last", last);
                r.put("base", base);
                collection.insert(mk(&r.get("course")), r);
            }
            collections.insert(app.clone(), collection);
        }

        // 보낸 순서대로 적용한다. touched 는 받은 시각(ms) × 1000 + 요청 안의 순서라, 같은 요청 안에서도 뒤의 런이 더 최근이다.
        // adds 는 이 요청이 그 행에 더한 것 (상대 갱신 · 다시 시도에 쓴다)
        let mut dirty: IndexMap<String, (String, V)> = IndexMap::new();
        let mut adds: IndexMap<String, Adds> = IndexMap::new();
        // (앱, 코스, 읽은 행(없으면 undefined), 밀어낸 런의 차례)
        let mut evicted: Vec<(String, V, V, usize)> = Vec::new();
        for (i, run) in runs.iter().enumerate() {
            let Some(run) = run else { continue };
            let collection = collections.get_mut(&run.app).expect("앱마다 읽어 두었다");
            let touched = V::Num(now_ms * 1000.0 + i as f64);
            let p = if run.kind == 1 { 1.0 } else { 0.0 };
            let d = if run.kind == -1 { 1.0 } else { 0.0 };
            let premium = V::Num(p);
            let discount = V::Num(d);
            let course = V::from(run.course.as_str());
            let dirty_key = format!("{}|{}", run.app, run.course);
            if let Some(row) = collection.get(&mk(&course)).cloned() {
                let best = less_than(&V::Num(run.total), &last_of(&row.g("best")?)?) == Some(true);
                row.put("runs", add(&row.get("runs"), &V::Num(1.0)));
                row.put("premium", add(&row.get("premium"), &premium));
                row.put("discount", add(&row.get("discount"), &discount));
                if best {
                    row.put("best", run.splits_v());
                }
                row.put("last", run.splits_v());
                row.put("last_day", js_max(&row.get("last_day"), run.day));
                row.put("touched", touched);
                results[i] = obj! { "course" => course.clone(), "status" => "repeat", "best" => best };
                dirty.insert(dirty_key.clone(), (run.app.clone(), row));
                note(&mut adds, &dirty_key, run, p, d);
            } else {
                let mut status = "stored";
                let mut gone = V::Undef;
                if collection.len() >= MAX_COURSES {
                    // Phase 5 Part 2 (Pinned-SLRU): 수동 고정(pinned != 0) 코스를 우선 보호하고 비고정 중 가장 오래된 코스를 밀어낸다
                    let unpinned: Vec<&V> = collection.values().filter(|r| r.get("pinned").to_num() == 0.0).collect();
                    let pool: Vec<&V> = if unpinned.is_empty() { collection.values().collect() } else { unpinned };
                    let mut oldest_row = pool[0].clone();
                    for b in &pool[1..] {
                        if less_than(&b.get("touched"), &oldest_row.get("touched")) == Some(true) {
                            oldest_row = (*b).clone();
                        }
                    }
                    let oldest_course = oldest_row.get("course");
                    let gone_key = format!("{}|{}", run.app, oldest_course.to_str());
                    collection.shift_remove(&mk(&oldest_course));
                    dirty.shift_remove(&gone_key);
                    adds.shift_remove(&gone_key);
                    evicted.push((run.app.clone(), oldest_course.clone(), oldest_row.get("base"), i));
                    status = "replaced";
                    gone = oldest_course;
                }
                let fresh = obj! {
                    "course" => course.clone(),
                    "steps" => json(&V::from(run.steps_v())),
                    "runs" => 1,
                    "premium" => premium,
                    "discount" => discount,
                    "best" => run.splits_v(),
                    "last" => run.splits_v(),
                    "last_day" => run.day,
                    "touched" => touched,
                    "best_total_ms" => run.total,
                };
                collection.insert(mk(&course), fresh.clone());
                dirty.insert(dirty_key.clone(), (run.app.clone(), fresh));
                adds.shift_remove(&dirty_key);
                note(&mut adds, &dirty_key, run, p, d);
                let result = obj! { "course" => course.clone(), "status" => status };
                if gone.truthy() {
                    result.put("evicted", gone);
                }
                results[i] = result;
            }
        }

        // 사용자마다 코스 등록은 한 번: 사용자 코스 id = hashId(<계정 키> + <코스 id>).
        // Phase 5 Part 2: 대표 기록(rep_splits, rep_type)을 함께 저장해 duplicate 재주행 시 차분 치환을 지원한다.
        let registration_of = |run: &ParsedRun| hash_str(&format!("{}{}", account.key, run.course));
        let mut first_reg: IndexMap<String, (f64, String, i32)> = IndexMap::new();
        for run in runs.iter().flatten() {
            first_reg.entry(registration_of(run)).or_insert_with(|| {
                let splits_json = stringify(&run.splits_v()).unwrap_or_else(|| "[]".to_string());
                (run.day, splits_json, run.kind)
            });
        }
        let mut claims = Vec::new();
        for (id, (day, rep_splits, rep_type)) in &first_reg {
            claims.push(stmt(
                env,
                SQL_REGISTER,
                &[V::from(id.as_str()), V::Num(*day), V::from(rep_splits.as_str()), V::Num(*rep_type as f64)],
            )?);
        }
        let claim_out = env.batch("DB", &V::from(claims)).await?;
        let claimed: Vec<String> = first_reg.keys().enumerate().filter(|(k, _)| changes_of(&claim_out.at(*k)) > 0.0).map(|(_, id)| id.clone()).collect();
        let mut fresh = vec![false; runs.len()];
        let mut seen: IndexSet<String> = IndexSet::new();
        for (i, run) in runs.iter().enumerate() {
            let Some(run) = run else { continue };
            let id = registration_of(run);
            let first = seen.insert(id.clone());
            let is_fresh = first && claimed.contains(&id);
            results[i].put("duplicate", V::from(!is_fresh));
            fresh[i] = is_fresh;
        }

        // 코스 id(mk) → 처음 등록하는 런의 차례
        let mut changed: IndexMap<String, usize> = IndexMap::new();
        let mut checks: Vec<Option<Check>> = Vec::new();
        let written: R<Vec<V>> = async {
            // 계정 없는 코스 합계: 코스 id 가 없으면 새로 만들고(created), 있으면 처음 등록하는 런만 더한다 (원본과 같은 계산)
            let courses = unique(runs.iter().flatten().map(|r| V::from(r.course.as_str())));
            let mut totals = read_courses(env, &courses).await?;
            for (i, run) in runs.iter().enumerate() {
                let Some(run) = run else { continue };
                let ck = mk(&V::from(run.course.as_str()));
                let prev = totals.get(&ck).cloned();
                results[i].put("created", V::from(prev.is_none()));
                if !fresh[i] {
                    continue;
                }
                let premium = if run.kind == 1 { 1.0 } else { 0.0 };
                let discount = if run.kind == -1 { 1.0 } else { 0.0 };
                let start_cp = run.steps.first().map(|s| s.id.as_str()).unwrap_or("");
                let goal_cp = run.steps.last().map(|s| s.id.as_str()).unwrap_or("");
                let Some(prev) = prev else {
                    let traits = compute_course_traits(&run.steps, &run.splits, discount, 1.0);
                    totals.insert(
                        ck.clone(),
                        obj! {
                            "course" => run.course.as_str(),
                            "app" => run.app.as_str(),
                            "steps" => json(&V::from(run.steps_v())),
                            "runs" => 1,
                            "premium" => premium,
                            "discount" => discount,
                            "record" => run.splits_v(),
                            "sums" => run.splits_v(),
                            "last_day" => run.day,
                            "start_cp" => start_cp,
                            "goal_cp" => goal_cp,
                            "pace_ms" => run.total,
                            "interaction_traits" => traits as f64,
                            "ema_splits" => run.splits_v(),
                            "era_record" => run.splits_v(),
                            "era_record_day" => run.day,
                        },
                    );
                    changed.insert(ck, i);
                    continue;
                };
                if less_than(&V::Num(run.total), &last_of(&prev.g("record")?)?) == Some(true) {
                    prev.put("record", run.splits_v());
                }
                let next_runs = prev.get("runs").to_num() + 1.0;
                let next_discount = prev.get("discount").to_num() + discount;
                prev.put("runs", add(&prev.get("runs"), &V::Num(1.0)));
                prev.put("premium", add(&prev.get("premium"), &V::Num(premium)));
                prev.put("discount", add(&prev.get("discount"), &V::Num(discount)));
                let next_sums = sums_plus(&prev.g("sums")?, run, "prev.sums")?;
                let next_total_sum = next_sums.last().map(V::to_num).unwrap_or(run.total);
                let next_pace = if next_runs > 0.0 { js_round(next_total_sum / next_runs) } else { run.total };
                let prev_ema_vec = prev
                    .get("ema_splits")
                    .as_str()
                    .and_then(|s| js_parse(&V::from(s)).ok())
                    .and_then(|v| parse_f64_array(&v));
                let next_ema = update_ema_splits(prev_ema_vec.as_deref(), &run.splits, 0.15);
                let traits = compute_course_traits(&run.steps, &next_ema, next_discount, next_runs);
                // 30일 에라 최고기록 회전
                let prev_era_day = prev.get("era_record_day").to_num();
                let mut era_rec = prev
                    .get("era_record")
                    .as_str()
                    .and_then(|s| js_parse(&V::from(s)).ok())
                    .unwrap_or_else(|| prev.get("record"));
                let mut era_day = prev_era_day;
                if prev_era_day <= 0.0 || run.day - prev_era_day >= 30.0 || less_than(&V::Num(run.total), &last_of(&era_rec).unwrap_or(V::Num(f64::INFINITY))) == Some(true) {
                    era_rec = run.splits_v();
                    era_day = run.day;
                }
                prev.put("sums", V::from(next_sums));
                prev.put("last_day", js_max(&prev.get("last_day"), run.day));
                prev.put("start_cp", V::from(start_cp));
                prev.put("goal_cp", V::from(goal_cp));
                prev.put("pace_ms", V::Num(next_pace));
                prev.put("interaction_traits", V::Num(traits as f64));
                prev.put("ema_splits", splits_v_from_f64(&next_ema));
                prev.put("era_record", era_rec);
                prev.put("era_record_day", V::Num(era_day));
                changed.insert(ck, i);
            }

            let mut statements: Vec<V> = Vec::new();
            for (app, course, base, index) in &evicted {
                if base.is_undef() {
                    statements.push(stmt(env, SQL_EVICT, &[key.clone(), V::from(app.as_str()), course.clone()])?);
                    checks.push(None);
                } else {
                    statements.push(stmt(env, SQL_EVICT_IF, &[key.clone(), V::from(app.as_str()), course.clone(), base.get("touched")])?);
                    checks.push(Some(Check::Evict { app: app.clone(), index: *index }));
                }
            }
            for (dirty_key, (app, row)) in &dirty {
                let a = &adds[dirty_key];
                let base = row.get("base");
                if base.is_undef() {
                    statements.push(stmt(
                        env,
                        SQL_INSERT_TRIAL,
                        &[
                            key.clone(),
                            V::from(account.kind),
                            V::from(app.as_str()),
                            row.get("course"),
                            row.get("steps"),
                            row.get("runs"),
                            row.get("premium"),
                            row.get("discount"),
                            json(&row.get("best")),
                            json(&row.get("last")),
                            row.get("last_day"),
                            row.get("touched"),
                            V::Num(a.best_total),
                        ],
                    )?);
                } else {
                    statements.push(stmt(
                        env,
                        SQL_UPDATE_TRIAL,
                        &[
                            key.clone(),
                            V::from(app.as_str()),
                            row.get("course"),
                            base.get("best"),
                            V::Num(a.runs),
                            V::Num(a.premium),
                            V::Num(a.discount),
                            json(&row.get("best")),
                            json(&row.get("last")),
                            V::Num(a.last_day),
                            row.get("touched"),
                            V::Num(a.best_total),
                        ],
                    )?);
                }
                checks.push(Some(Check::Trial { key: dirty_key.clone() }));
            }
            for t in totals.values() {
                let Some(&ri) = changed.get(&mk(&t.get("course"))) else { continue };
                let run = runs[ri].as_ref().expect("처음 등록하는 런");
                let stored = t.get("stored");
                let course = t.get("course");
                let start_cp = run.steps.first().map(|s| s.id.as_str()).unwrap_or("");
                let goal_cp = run.steps.last().map(|s| s.id.as_str()).unwrap_or("");
                if stored.is_undef() {
                    statements.push(stmt(
                        env,
                        SQL_INSERT_COURSE,
                        &[
                            course.clone(),
                            t.get("app"),
                            t.get("steps"),
                            t.get("runs"),
                            t.get("premium"),
                            t.get("discount"),
                            json(&t.get("record")),
                            json(&t.get("sums")),
                            t.get("last_day"),
                            V::from(start_cp),
                            V::from(goal_cp),
                            t.get("pace_ms"),
                            t.get("interaction_traits"),
                            json(&t.get("ema_splits")),
                            json(&t.get("era_record")),
                            t.get("era_record_day"),
                        ],
                    )?);
                    checks.push(Some(Check::Course { course: course.to_str(), insert: true }));
                } else {
                    statements.push(stmt(
                        env,
                        SQL_UPDATE_COURSE,
                        &[
                            course.clone(),
                            stored.get("record"),
                            stored.get("sums"),
                            V::Num(if run.kind == 1 { 1.0 } else { 0.0 }),
                            V::Num(if run.kind == -1 { 1.0 } else { 0.0 }),
                            json(&t.get("record")),
                            json(&t.get("sums")),
                            V::Num(run.day),
                            V::from(start_cp),
                            V::from(goal_cp),
                            t.get("pace_ms"),
                            t.get("interaction_traits"),
                            json(&t.get("ema_splits")),
                            json(&t.get("era_record")),
                            t.get("era_record_day"),
                        ],
                    )?);
                    checks.push(Some(Check::Course { course: course.to_str(), insert: false }));
                }
            }
            let count_up = |hash: V, day: f64, kind: i32| stmt(env, SQL_COUNT_UP, &[hash, V::Num(day), V::Num(kind as f64), V::from(NAV_RECIPE)]);
            for (i, run) in runs.iter().enumerate() {
                let Some(run) = run else { continue };
                let goal_cp = run.steps.last().map(|s| s.id.as_str()).unwrap_or("");
                if fresh[i] && run.kind != 0 {
                    statements.push(count_up(V::from(run.course.as_str()), run.day, run.kind)?);
                    checks.push(None);
                    statements.push(stmt(
                        env,
                        SQL_DIRTY_MARK,
                        &[V::from(run.course.as_str()), V::from("course"), V::from(run.app.as_str()), V::from(goal_cp), V::Num(now_ms)],
                    )?);
                    checks.push(None);
                }
            }

            // Phase 4 Part 1 & 2: 고스트 따라가기 판정(route_marks_v2 + 1인1표 ballot + 목적지별 route_fit + 갈림길 대결 선행 승격)
            let ghost_ids = unique(runs.iter().flatten().filter_map(|r| r.ghost.as_deref().map(V::from)));
            let ghosts = read_courses(env, &ghost_ids).await?;
            for (i, run) in runs.iter().enumerate() {
                let Some(run) = run else { continue };
                let Some(ghost_id) = &run.ghost else { continue };
                let ghost = ghosts.get(&mk(&V::from(ghost_id.as_str())));
                let (marks, ghost_steps_vec, ghost_avg_splits) = match ghost {
                    Some(g) if fresh[i] && strict_eq(&g.get("app"), &V::from(run.app.as_str())) && js_ge(&g.get("runs"), PUBLISH_MIN_RUNS) => {
                        let steps = run.steps_v();
                        let ghost_steps = js_parse(&g.get("steps"))?;
                        let g_runs = g.get("runs").to_num().max(1.0);
                        let g_sums = parse_f64_array(&g.get("sums")).unwrap_or_default();
                        let g_avg: Vec<f64> = g_sums.iter().map(|s| s / g_runs).collect();
                        let g_avg_total = g_avg.last().copied().unwrap_or(0.0);
                        let m = route_marks_v2(&steps, &ghost_steps, run.total, run.kind, g_avg_total, run.fault_mask)?;
                        (m, ghost_steps.items(), g_avg)
                    }
                    _ => (None, Vec::new(), Vec::new()),
                };
                let Some(marks) = marks else {
                    results[i].put("route", V::Null);
                    continue;
                };
                let goal_cp = run.steps.last().map(|s| s.id.as_str()).unwrap_or("");
                let mut prem_cnt = 0usize;
                let mut disc_cnt = 0usize;
                for (step, outcome) in &marks {
                    let Some(sign) = outcome.sign() else { continue };
                    let cp_id = checkpoint_id_v(step)?;
                    let bid = ballot_id(&account.key, &run.app, goal_cp, &cp_id);
                    let prev_row = js::first(&stmt(env, SQL_BALLOT_GET, &[V::from(bid.as_str())])?).await?;
                    let prev_sign = if prev_row.is_nullish() { None } else { Some(prev_row.get("sign").to_num() as i32) };
                    match apply_ballot_delta(prev_sign, sign) {
                        BallotDelta::Duplicate => continue,
                        BallotDelta::First(new_sign) => {
                            statements.push(stmt(env, SQL_BALLOT_UPSERT, &[V::from(bid.as_str()), V::Num(new_sign as f64), V::Num(run.day)])?);
                            checks.push(None);
                            statements.push(count_up(V::from(cp_id.as_str()), run.day, new_sign)?);
                            checks.push(None);
                            statements.push(stmt(
                                env,
                                SQL_ROUTE_FIT_UP,
                                &[V::from(cp_id.as_str()), V::from(goal_cp), V::Num(run.day), V::Num(new_sign as f64), V::from(NAV_RECIPE)],
                            )?);
                            checks.push(None);
                            statements.push(stmt(
                                env,
                                SQL_DIRTY_MARK,
                                &[V::from(cp_id.as_str()), V::from("checkpoint"), V::from(run.app.as_str()), V::from(goal_cp), V::Num(now_ms)],
                            )?);
                            checks.push(None);
                        }
                        BallotDelta::Flipped { old, new } => {
                            statements.push(stmt(env, SQL_BALLOT_UPSERT, &[V::from(bid.as_str()), V::Num(new as f64), V::Num(run.day)])?);
                            checks.push(None);
                            statements.push(stmt(env, SQL_COUNT_DEC, &[V::from(cp_id.as_str()), V::Num(old as f64), V::from(NAV_RECIPE)])?);
                            checks.push(None);
                            statements.push(stmt(
                                env,
                                SQL_ROUTE_FIT_DEC,
                                &[V::from(cp_id.as_str()), V::from(goal_cp), V::Num(old as f64), V::from(NAV_RECIPE)],
                            )?);
                            checks.push(None);
                            statements.push(count_up(V::from(cp_id.as_str()), run.day, new)?);
                            checks.push(None);
                            statements.push(stmt(
                                env,
                                SQL_ROUTE_FIT_UP,
                                &[V::from(cp_id.as_str()), V::from(goal_cp), V::Num(run.day), V::Num(new as f64), V::from(NAV_RECIPE)],
                            )?);
                            checks.push(None);
                            statements.push(stmt(
                                env,
                                SQL_DIRTY_MARK,
                                &[V::from(cp_id.as_str()), V::from("checkpoint"), V::from(run.app.as_str()), V::from(goal_cp), V::Num(now_ms)],
                            )?);
                            checks.push(None);
                        }
                    }
                    if sign == 1 {
                        prem_cnt += 1;
                    } else {
                        disc_cnt += 1;
                    }
                }
                results[i].put("route", obj! { "ghost" => ghost_id.as_str(), "premium" => prem_cnt, "discount" => disc_cnt });

                // Phase 4 Part 2: 서버 갈림길 대결 추출 및 3인 합의 선행 승격
                if let Ok(duels) = extract_server_shortcut_duels(run, &ghost_steps_vec, &ghost_avg_splits) {
                    for duel in duels {
                        let drid = duel_reg_id(&account.key, &duel.fork_course, &duel.taken_cp, &duel.skipped_cp);
                        let reg_res = js::run(&stmt(env, SQL_DUEL_REG, &[V::from(drid.as_str()), V::Num(run.day)])?).await?;
                        if changes_of(&reg_res) <= 0.0 {
                            continue;
                        }
                        js::run(&stmt(
                            env,
                            SQL_DUEL_UPSERT,
                            &[
                                V::from(duel.fork_course.as_str()),
                                V::from(duel.skipped_cp.as_str()),
                                V::from(duel.taken_cp.as_str()),
                                V::from(run.app.as_str()),
                                V::Num(duel.saved_ms.round()),
                                V::Num(duel.margin),
                                V::Num(run.day),
                            ],
                        )?)
                        .await?;
                        let drow = js::first(&stmt(
                            env,
                            SQL_DUEL_GET,
                            &[V::from(duel.fork_course.as_str()), V::from(duel.skipped_cp.as_str()), V::from(duel.taken_cp.as_str())],
                        )?)
                        .await?;
                        if !drow.is_nullish() && drow.get("promoted").to_num() == 0.0 {
                            let wins = drow.get("wins").to_num() as u32;
                            let saved_sum = drow.get("saved_ms_sum").to_num();
                            let margin_sum = drow.get("margin_sum").to_num();
                            let mut taken_p = 0u32;
                            let mut taken_d = 0u32;
                            for c in rows(env, &sql_counts(1), &[V::from(NAV_RECIPE), V::from(duel.taken_cp.as_str())]).await? {
                                if strict_eq(&c.get("type"), &V::Num(1.0)) {
                                    taken_p += c.get("n").to_num() as u32;
                                } else {
                                    taken_d += c.get("n").to_num() as u32;
                                }
                            }
                            if should_pioneer_promote(wins, saved_sum, margin_sum, taken_p, taken_d) {
                                statements.push(stmt(
                                    env,
                                    SQL_DUEL_PROMOTE,
                                    &[V::from(duel.fork_course.as_str()), V::from(duel.skipped_cp.as_str()), V::from(duel.taken_cp.as_str())],
                                )?);
                                checks.push(None);
                                statements.push(count_up(V::from(duel.taken_cp.as_str()), run.day, 1)?);
                                checks.push(None);
                                statements.push(stmt(
                                    env,
                                    SQL_DIRTY_MARK,
                                    &[V::from(duel.taken_cp.as_str()), V::from("checkpoint"), V::from(run.app.as_str()), V::from(goal_cp), V::Num(now_ms)],
                                )?);
                                checks.push(None);
                            }
                        }
                    }
                }
            }

            // Phase 5 Part 3: 2-단계 신뢰 합의 대표 벡터(Consensus EMA Medoid)
            for (i, run) in runs.iter().enumerate() {
                let Some(run) = run else { continue };
                if !fresh[i] {
                    continue;
                }
                for v in &run.vectors {
                    let existing = js::first(&stmt(env, SQL_VECTOR_GET, &[V::from(v.hash.as_str()), V::from(v.space.as_str())])?).await?;
                    if existing.is_nullish() {
                        statements.push(stmt(
                            env,
                            SQL_VECTOR_INSERT,
                            &[V::from(v.hash.as_str()), V::from(v.space.as_str()), V::from(v.vector.len()), V::bytes(v.bytes()), V::Num(run.day)],
                        )?);
                        checks.push(None);
                    } else if existing.get("locked").to_num() == 0.0 {
                        if let Some(cur) = v_to_f32_vec(&existing.get("vector")) {
                            let conf = existing.get("confirmations").to_num().max(1.0) as u32;
                            let cand = v_to_f32_vec(&existing.get("candidate_vector"));
                            let cand_hits = existing.get("candidate_hits").to_num().max(0.0) as u32;
                            match evaluate_vector_consensus(&cur, conf, false, cand.as_deref(), cand_hits, &v.vector) {
                                VectorConsensusAction::ConfirmMerge { merged, confirmations, lock } => {
                                    statements.push(stmt(
                                        env,
                                        SQL_VECTOR_CONFIRM,
                                        &[
                                            V::from(v.hash.as_str()),
                                            V::from(v.space.as_str()),
                                            V::bytes(f32_vec_to_bytes(&merged)),
                                            V::Num(confirmations as f64),
                                            V::Num(if lock { 1.0 } else { 0.0 }),
                                            V::Num(run.day),
                                        ],
                                    )?);
                                    checks.push(None);
                                }
                                VectorConsensusAction::TrackCandidate { candidate, hits } => {
                                    statements.push(stmt(
                                        env,
                                        SQL_VECTOR_CANDIDATE,
                                        &[
                                            V::from(v.hash.as_str()),
                                            V::from(v.space.as_str()),
                                            V::bytes(f32_vec_to_bytes(&candidate)),
                                            V::Num(hits as f64),
                                        ],
                                    )?);
                                    checks.push(None);
                                }
                                VectorConsensusAction::ReplaceWithCandidate { replacement } => {
                                    statements.push(stmt(
                                        env,
                                        SQL_VECTOR_REPLACE,
                                        &[
                                            V::from(v.hash.as_str()),
                                            V::from(v.space.as_str()),
                                            V::bytes(f32_vec_to_bytes(&replacement)),
                                            V::Num(run.day),
                                        ],
                                    )?);
                                    checks.push(None);
                                }
                                VectorConsensusAction::KeepExisting => {}
                            }
                        }
                    }
                }
            }
            if statements.is_empty() {
                return Ok(Vec::new());
            }
            Ok(env.batch("DB", &V::from(statements)).await?.items())
        }
        .await;
        let out = match written {
            Ok(out) => out,
            Err(err) => {
                if !claimed.is_empty() {
                    let ids: Vec<V> = claimed.iter().map(|id| V::from(id.as_str())).collect();
                    let undo: R<()> = async {
                        for part in ids.chunks(IN_CHUNK) {
                            let sql = format!("DELETE FROM nav_registrations WHERE \"id\" IN ({})", crate::util::binds(part.len()));
                            js::run(&stmt(env, &sql, part)?).await?;
                        }
                        Ok(())
                    }
                    .await;
                    let _ = undo;
                }
                return Err(err);
            }
        };

        // 조건이 맞지 않은 쓰기 (다른 요청이 그 사이 그 행을 바꿈): 다시 읽어 계산해 다시 쓴다
        let mut retry_trials: Vec<String> = Vec::new();
        let mut retry_courses: Vec<String> = Vec::new();
        let mut retry_evictions: Vec<(String, usize)> = Vec::new();
        for (res, check) in out.iter().zip(&checks) {
            let Some(check) = check else { continue };
            if changes_of(res) > 0.0 {
                continue;
            }
            match check {
                Check::Trial { key } => retry_trials.push(key.clone()),
                Check::Course { course, insert } => {
                    if *insert {
                        for (i, run) in runs.iter().enumerate() {
                            if run.as_ref().is_some_and(|r| &r.course == course) {
                                results[i].put("created", V::from(false));
                            }
                        }
                    }
                    retry_courses.push(course.clone());
                }
                Check::Evict { app, index } => retry_evictions.push((app.clone(), *index)),
            }
        }
        for dirty_key in &retry_trials {
            let (app, row) = &dirty[dirty_key];
            let a = &adds[dirty_key];
            let mut done = false;
            let mut attempt = 0;
            while attempt < NAV_RETRIES && !done {
                attempt += 1;
                let cur = js::first(&stmt(env, SQL_TRIAL_ROW, &[key.clone(), V::from(app.as_str()), row.get("course")])?).await?;
                if cur.is_nullish() {
                    let res = js::run(&stmt(
                        env,
                        SQL_INSERT_TRIAL,
                        &[
                            key.clone(),
                            V::from(account.kind),
                            V::from(app.as_str()),
                            row.get("course"),
                            row.get("steps"),
                            V::Num(a.runs),
                            V::Num(a.premium),
                            V::Num(a.discount),
                            json(&a.best),
                            json(&row.get("last")),
                            V::Num(a.last_day),
                            row.get("touched"),
                            V::Num(a.best_total),
                        ],
                    )?)
                    .await?;
                    done = changes_of(&res) > 0.0;
                    continue;
                }
                let stored = js_parse(&cur.g("best")?)?;
                let best = if less_than(&V::Num(a.best_total), &last_of(&stored)?) == Some(true) { a.best.clone() } else { stored };
                let res = js::run(&stmt(
                    env,
                    SQL_UPDATE_TRIAL,
                    &[
                        key.clone(),
                        V::from(app.as_str()),
                        row.get("course"),
                        cur.get("best"),
                        V::Num(a.runs),
                        V::Num(a.premium),
                        V::Num(a.discount),
                        json(&best),
                        json(&row.get("last")),
                        V::Num(a.last_day),
                        row.get("touched"),
                        V::Num(a.best_total),
                    ],
                )?)
                .await?;
                done = changes_of(&res) > 0.0;
            }
            if !done {
                return Err(Throw::error(NAV_CONFLICT));
            }
        }
        for course in &retry_courses {
            let ri = changed[&mk(&V::from(course.as_str()))];
            let run = runs[ri].as_ref().expect("처음 등록하는 런");
            let premium = V::Num(if run.kind == 1 { 1.0 } else { 0.0 });
            let discount = V::Num(if run.kind == -1 { 1.0 } else { 0.0 });
            let start_cp = run.steps.first().map(|s| s.id.as_str()).unwrap_or("");
            let goal_cp = run.steps.last().map(|s| s.id.as_str()).unwrap_or("");
            let mut done = false;
            let mut attempt = 0;
            while attempt < NAV_RETRIES && !done {
                attempt += 1;
                let cur = js::first(&stmt(env, SQL_COURSE_ROW, &[V::from(course.as_str())])?).await?;
                if cur.is_nullish() {
                    let traits = compute_course_traits(&run.steps, &run.splits, discount.to_num(), 1.0);
                    let res = js::run(&stmt(
                        env,
                        SQL_INSERT_COURSE,
                        &[
                            V::from(course.as_str()),
                            V::from(run.app.as_str()),
                            json(&V::from(run.steps_v())),
                            V::Num(1.0),
                            premium.clone(),
                            discount.clone(),
                            json(&run.splits_v()),
                            json(&run.splits_v()),
                            V::Num(run.day),
                            V::from(start_cp),
                            V::from(goal_cp),
                            V::Num(run.total),
                            V::Num(traits as f64),
                            json(&run.splits_v()),
                            json(&run.splits_v()),
                            V::Num(run.day),
                        ],
                    )?)
                    .await?;
                    done = changes_of(&res) > 0.0;
                    continue;
                }
                let record = js_parse(&cur.g("record")?)?;
                let sums = js_parse(&cur.g("sums")?)?;
                let next_record = if less_than(&V::Num(run.total), &last_of(&record)?) == Some(true) { run.splits_v() } else { record };
                let next_sums_vec = sums_plus(&sums, run, "sums")?;
                let next_runs = cur.get("runs").to_num() + 1.0;
                let next_discount = cur.get("discount").to_num() + discount.to_num();
                let next_pace = js_round(next_sums_vec.last().map(V::to_num).unwrap_or(run.total) / next_runs.max(1.0));
                let next_ema = update_ema_splits(None, &run.splits, 0.15);
                let traits = compute_course_traits(&run.steps, &next_ema, next_discount, next_runs);
                let next_sums = V::from(next_sums_vec);
                let res = js::run(&stmt(
                    env,
                    SQL_UPDATE_COURSE,
                    &[
                        V::from(course.as_str()),
                        cur.get("record"),
                        cur.get("sums"),
                        premium.clone(),
                        discount.clone(),
                        json(&next_record),
                        json(&next_sums),
                        V::Num(run.day),
                        V::from(start_cp),
                        V::from(goal_cp),
                        V::Num(next_pace),
                        V::Num(traits as f64),
                        json(&splits_v_from_f64(&next_ema)),
                        json(&next_record),
                        V::Num(run.day),
                    ],
                )?)
                .await?;
                done = changes_of(&res) > 0.0;
            }
            if !done {
                return Err(Throw::error(NAV_CONFLICT));
            }
        }
        for (app, index) in &retry_evictions {
            let mine: IndexSet<String> = dirty.values().filter(|(a, _)| a == app).map(|(_, row)| mk(&row.get("course"))).collect();
            let mut done = false;
            let mut attempt = 0;
            while attempt < NAV_RETRIES && !done {
                attempt += 1;
                let all = rows(env, SQL_COLLECTION, &[key.clone(), V::from(app.as_str())]).await?;
                let candidates: Vec<&V> = all
                    .iter()
                    .filter(|r| !mine.contains(&mk(&r.get("course"))) && r.get("pinned").to_num() == 0.0)
                    .collect();
                if all.len() <= MAX_COURSES || candidates.is_empty() {
                    done = true;
                    break;
                }
                let mut oldest_row = candidates[0];
                for b in &candidates[1..] {
                    if less_than(&b.get("touched"), &oldest_row.get("touched")) == Some(true) {
                        oldest_row = b;
                    }
                }
                let res = js::run(&stmt(env, SQL_EVICT_IF, &[key.clone(), V::from(app.as_str()), oldest_row.get("course"), oldest_row.get("touched")])?).await?;
                if changes_of(&res) > 0.0 {
                    results[*index].put("evicted", oldest_row.get("course"));
                    done = true;
                }
            }
            if !done {
                return Err(Throw::error(NAV_CONFLICT));
            }
        }

        // Phase 5 Part 1 & 2: duplicate 재주행자 대표 기록 차분 치환(runs 불변) + 14일 에폭 하트비트 갱신
        for (i, run) in runs.iter().enumerate() {
            let Some(run) = run else { continue };
            if fresh[i] {
                continue;
            }
            let reg_id = registration_of(run);
            if run.kind != -1 {
                let reg_row = js::first(&stmt(env, SQL_REGISTER_GET, &[V::from(reg_id.as_str())])?).await?;
                let cur_course = js::first(&stmt(env, SQL_COURSE_ROW, &[V::from(run.course.as_str())])?).await?;
                if !reg_row.is_nullish() && !cur_course.is_nullish() {
                    let old_rep = reg_row
                        .get("rep_splits")
                        .as_str()
                        .and_then(|s| js_parse(&V::from(s)).ok())
                        .and_then(|v| parse_f64_array(&v));
                    let old_sums = js_parse(&cur_course.get("sums")).ok().and_then(|v| parse_f64_array(&v));
                    let old_rec = js_parse(&cur_course.get("record")).ok().and_then(|v| parse_f64_array(&v));
                    if let (Some(rep), Some(sums), Some(rec)) = (old_rep, old_sums, old_rec) {
                        if let Some((next_sums, next_rec)) = representative_best_swap(&sums, &rep, &run.splits, &rec, run.kind) {
                            let c_runs = cur_course.get("runs").to_num().max(1.0);
                            let next_pace = js_round(next_sums.last().copied().unwrap_or(run.total) / c_runs);
                            let prev_ema = cur_course
                                .get("ema_splits")
                                .as_str()
                                .and_then(|s| js_parse(&V::from(s)).ok())
                                .and_then(|v| parse_f64_array(&v));
                            let next_ema = update_ema_splits(prev_ema.as_deref(), &run.splits, 0.15);
                            let swap_res = js::run(&stmt(
                                env,
                                SQL_SWAP_COURSE_REP,
                                &[
                                    V::from(run.course.as_str()),
                                    cur_course.get("record"),
                                    cur_course.get("sums"),
                                    json(&splits_v_from_f64(&next_rec)),
                                    json(&splits_v_from_f64(&next_sums)),
                                    V::Num(next_pace),
                                    json(&splits_v_from_f64(&next_ema)),
                                    V::Num(run.day),
                                ],
                            )?)
                            .await?;
                            if changes_of(&swap_res) > 0.0 {
                                let _ = js::run(&stmt(
                                    env,
                                    SQL_REGISTER_UPDATE_REP,
                                    &[V::from(reg_id.as_str()), json(&run.splits_v()), V::Num(run.kind as f64), V::Num(run.day)],
                                )?)
                                .await;
                            }
                        }
                    }
                }
            }
            if run.kind != 0 {
                let pid = epoch_pulse_id(&account.key, &run.course, run.day);
                let ep = half_life_epoch(run.day);
                let pulse_res = js::run(&stmt(
                    env,
                    SQL_EPOCH_PULSE,
                    &[V::from(pid.as_str()), V::Num(ep as f64), V::Num(run.day), V::Num(run.kind as f64)],
                )?)
                .await?;
                if changes_of(&pulse_res) > 0.0 {
                    let goal_cp = run.steps.last().map(|s| s.id.as_str()).unwrap_or("");
                    let _ = js::run(&stmt(env, SQL_COUNT_UP, &[V::from(run.course.as_str()), V::Num(run.day), V::Num(run.kind as f64), V::from(NAV_RECIPE)])?).await;
                    let _ = js::run(&stmt(
                        env,
                        SQL_DIRTY_MARK,
                        &[V::from(run.course.as_str()), V::from("course"), V::from(run.app.as_str()), V::from(goal_cp), V::Num(now_ms)],
                    )?)
                    .await;
                }
            }
        }
        Ok(results)
    }

    /// Phase 4 Part 3: 생존자 편향 해소용 단일 체크포인트 이탈(`dead_end_marks`) 기록.
    async fn store_dead_end_marks(env: &Env, account: &Account, marks: &[ParsedDeadEnd], now_ms: f64) -> R<usize> {
        let mut recorded = 0usize;
        for m in marks {
            let bid = dead_end_ballot_id(&account.key, &m.app, &m.checkpoint_id);
            let prev = js::first(&stmt(env, SQL_BALLOT_GET, &[V::from(bid.as_str())])?).await?;
            if !prev.is_nullish() {
                continue;
            }
            js::run(&stmt(env, SQL_BALLOT_UPSERT, &[V::from(bid.as_str()), V::Num(-1.0), V::Num(m.day)])?).await?;
            js::run(&stmt(env, SQL_COUNT_UP, &[V::from(m.checkpoint_id.as_str()), V::Num(m.day), V::Num(-1.0), V::from(NAV_RECIPE)])?).await?;
            js::run(&stmt(
                env,
                SQL_DIRTY_MARK,
                &[V::from(m.checkpoint_id.as_str()), V::from("checkpoint"), V::from(m.app.as_str()), V::Null, V::Num(now_ms)],
            )?)
            .await?;
            recorded += 1;
        }
        Ok(recorded)
    }

    /// `readMine(db, account, app)` — 앱의 내 콜렉션 (최근에 달린 순)
    async fn read_mine(env: &Env, account: &Account, app: &str) -> R<Vec<V>> {
        let mut out = Vec::new();
        for r in rows(env, SQL_MINE, &[V::from(account.key.as_str()), V::from(app)]).await? {
            let steps = js_parse(&r.get("steps"))?;
            let t = tier(&r.get("premium"), &r.get("discount"));
            let best = js_parse(&r.get("best"))?;
            let last = js_parse(&r.get("last"))?;
            out.push(obj! {
                "course" => r.get("course"),
                "steps" => steps,
                "runs" => r.get("runs"),
                "premium" => r.get("premium"),
                "discount" => r.get("discount"),
                "tier" => t,
                "best" => best,
                "last" => last,
                "last_day" => r.get("last_day"),
            });
        }
        Ok(out)
    }

    /// `s.id ?? checkpointId(s)`
    fn step_hash(s: &V) -> R<V> {
        let id = s.g("id")?;
        Ok(if id.is_nullish() { V::from(checkpoint_id_v(s)?) } else { id })
    }

    /// `readOthers(db, app, mine, entry_cp)` — Phase 5 Part 2 + Phase 7 Part 4 4-쿼터 다양성 포트폴리오 공개 조회.
    async fn read_others(env: &Env, app: &str, mine: &[V], entry_cp: Option<&str>) -> R<Vec<V>> {
        // 후보 풀 수집: 상위 인기 코스(최대 40) + ?entry= 진입 일치(최대 16) + 접근성 코스(최대 12) + 내 콜렉션 공개 코스
        let top = rows(env, SQL_TOP, &[V::from(app), V::Num(PUBLISH_MIN_RUNS), V::Num(40.0)]).await?;
        let mut table: IndexMap<String, V> = top.into_iter().map(|r| (mk(&r.get("course")), r)).collect();
        if let Some(entry) = entry_cp {
            for r in rows(env, SQL_TOP_ENTRY, &[V::from(app), V::Num(PUBLISH_MIN_RUNS), V::from(entry)]).await? {
                table.entry(mk(&r.get("course"))).or_insert(r);
            }
        }
        for r in rows(env, SQL_TOP_ACCESSIBLE, &[V::from(app), V::Num(PUBLISH_MIN_RUNS)]).await? {
            table.entry(mk(&r.get("course"))).or_insert(r);
        }
        let missing: Vec<V> = mine.iter().filter(|c| !table.contains_key(&mk(c))).cloned().collect();
        for (course, row) in read_courses(env, &missing).await? {
            if strict_eq(&row.get("app"), &V::from(app)) && js_ge(&row.get("runs"), PUBLISH_MIN_RUNS) {
                row.put("record", json(&row.get("record")));
                row.put("sums", json(&row.get("sums")));
                table.insert(course, row);
            }
        }

        // 코스 판정(nav_verdicts)을 조회해 4-쿼터 포트폴리오 선발에 반영한다.
        let course_hashes: Vec<V> = table.values().map(|r| r.get("course")).collect();
        let mut verdicts: IndexMap<String, V> = IndexMap::new();
        for part in course_hashes.chunks(IN_CHUNK) {
            if part.is_empty() {
                continue;
            }
            for vr in rows(env, &sql_verdicts(part.len()), part).await? {
                verdicts.insert(mk(&vr.get("hash")), vr);
            }
        }
        let mut portfolio_cands = Vec::with_capacity(table.len());
        for r in table.values() {
            let c_id = r.get("course").to_str();
            let v_row = verdicts.get(&mk(&r.get("course")));
            let c_tier = v_row
                .map(|vr| vr.get("tier").to_str())
                .unwrap_or_else(|| tier(&r.get("premium"), &r.get("discount")).to_string());
            let grade = v_row.map(|vr| vr.get("grade").to_num() as i32).unwrap_or(0);
            let superseded = v_row.is_some_and(|vr| vr.get("superseded").to_num() != 0.0);
            let pace_ms = {
                let p = r.get("pace_ms").to_num();
                if p > 0.0 {
                    p
                } else {
                    let sums_last = js_parse(&r.get("sums")).ok().and_then(|v| parse_f64_array(&v)).and_then(|a| a.last().copied()).unwrap_or(0.0);
                    let runs_n = r.get("runs").to_num().max(1.0);
                    sums_last / runs_n
                }
            };
            portfolio_cands.push(PortfolioCourse {
                course: c_id,
                start_cp: r.get("start_cp").to_str(),
                goal_cp: r.get("goal_cp").to_str(),
                runs: r.get("runs").to_num(),
                tier: c_tier,
                grade,
                pace_ms,
                interaction_traits: r.get("interaction_traits").to_num().max(0.0) as u32,
                superseded,
            });
        }
        let mine_ids: Vec<String> = mine.iter().map(V::to_str).collect();
        let selected_ids = select_stratified_portfolio(&portfolio_cands, entry_cp, &mine_ids);
        let selected_rows: Vec<V> = selected_ids
            .iter()
            .filter_map(|cid| table.get(&mk(&V::from(cid.as_str()))).cloned())
            .collect();

        // 체크포인트 판정 수(nav_counts + 목적지별 nav_route_fit_counts) · 판정(nav_verdicts) · 합의 벡터(nav_vectors) · 선행 승격 갈림길(nav_shortcut_duels)
        let mut hashes: Vec<V> = Vec::new();
        let mut raw_step_hashes: Vec<V> = Vec::new();
        for r in &selected_rows {
            let steps = js_parse(&r.get("steps"))?;
            if !steps.is_arr() {
                return Err(Throw::type_error("JSON.parse(...).map is not a function"));
            }
            for s in steps.items() {
                hashes.push(step_hash(&s)?);
                raw_step_hashes.push(s.get("hash"));
            }
        }
        let hashes = unique(hashes);
        let raw_step_hashes = unique(raw_step_hashes);

        let mut tallies: IndexMap<String, (f64, f64)> = IndexMap::new();
        let mut route_fit: IndexMap<(String, String), (f64, f64)> = IndexMap::new();
        for part in hashes.chunks(IN_CHUNK) {
            if part.is_empty() {
                continue;
            }
            let mut args = vec![V::from(NAV_RECIPE)];
            args.extend(part.iter().cloned());
            for c in rows(env, &sql_counts(part.len()), &args).await? {
                let t = tallies.entry(mk(&c.get("hash"))).or_insert((0.0, 0.0));
                if strict_eq(&c.get("type"), &V::Num(1.0)) {
                    t.0 += c.get("n").to_num();
                } else {
                    t.1 += c.get("n").to_num();
                }
            }
            for c in rows(env, &sql_route_fit_counts(part.len()), &args).await? {
                let key = (c.get("hash").to_str(), c.get("goal").to_str());
                let t = route_fit.entry(key).or_insert((0.0, 0.0));
                if strict_eq(&c.get("type"), &V::Num(1.0)) {
                    t.0 += c.get("n").to_num();
                } else {
                    t.1 += c.get("n").to_num();
                }
            }
            for vr in rows(env, &sql_verdicts(part.len()), part).await? {
                verdicts.insert(mk(&vr.get("hash")), vr);
            }
        }

        // 합의 2회 이상 확인된 대표 벡터(Int8 Base64 압축)
        let mut confirmed_vecs: IndexMap<String, Vec<V>> = IndexMap::new();
        for part in raw_step_hashes.chunks(IN_CHUNK) {
            if part.is_empty() {
                continue;
            }
            for vr in rows(env, &sql_confirmed_vectors(part.len()), part).await? {
                if let Some(f32s) = v_to_f32_vec(&vr.get("vector")) {
                    let h = vr.get("hash").to_str();
                    confirmed_vecs.entry(h).or_default().push(obj! {
                        "space" => vr.get("space"),
                        "dims" => vr.get("dims"),
                        "q8_b64" => encode_unit_i8_b64(&f32s),
                    });
                }
            }
        }

        // 앱의 선행 승격 갈림길 목록
        let mut forks_by_course: IndexMap<String, Vec<V>> = IndexMap::new();
        for fr in rows(env, SQL_DUELS_PROMOTED, &[V::from(app)]).await? {
            let fc = fr.get("fork_course").to_str();
            let wins = fr.get("wins").to_num().max(1.0);
            forks_by_course.entry(fc).or_default().push(obj! {
                "fork_course" => fr.get("fork_course"),
                "skipped_cp" => fr.get("skipped_cp"),
                "taken_cp" => fr.get("taken_cp"),
                "wins" => fr.get("wins"),
                "mean_saved_ms" => js_round(fr.get("saved_ms_sum").to_num() / wins),
            });
        }

        let mut out = Vec::new();
        for r in &selected_rows {
            let sums = js_parse(&r.get("sums"))?;
            let steps = js_parse(&r.get("steps"))?;
            let shown_steps = js_parse(&r.get("steps"))?;
            let t = tier(&r.get("premium"), &r.get("discount"));
            let record = js_parse(&r.get("record"))?;
            if !sums.is_arr() {
                return Err(Throw::type_error("sums.map is not a function"));
            }
            let runs = r.get("runs").to_num();
            let average: Vec<V> = sums.items().iter().map(|s| V::Num(js_round(s.to_num() / runs / SPLIT_UNIT_MS) * SPLIT_UNIT_MS)).collect();
            let step_items = steps.items();
            let goal_cp = step_items
                .last()
                .and_then(|s| step_hash(s).ok())
                .map(|v| v.to_str())
                .unwrap_or_default();
            let mut checkpoints = Vec::new();
            let mut course_forks = Vec::new();
            let course_vecs = V::new_obj();
            for s in &step_items {
                let sh = step_hash(s)?;
                let sh_str = sh.to_str();
                // 목적지별 적합도 집계(nav_route_fit_counts)가 3표 이상이면 우선 사용, 부족 시 전역 nav_counts 폴백
                let (p, d) = match route_fit.get(&(sh_str.clone(), goal_cp.clone())).copied() {
                    Some((rp, rd)) if rp + rd >= TIER_MIN_DECIDED => (rp, rd),
                    _ => tallies.get(&mk(&sh)).copied().unwrap_or((0.0, 0.0)),
                };
                let cp_obj = obj! { "premium" => p, "discount" => d, "tier" => tier(&V::Num(p), &V::Num(d)) };
                if let Some(vr) = verdicts.get(&mk(&sh)) {
                    cp_obj.put("verdict", verdict_row_to_v(vr));
                }
                checkpoints.push(cp_obj);
                if let Some(fk_list) = forks_by_course.get(&s.get("course").to_str()) {
                    course_forks.extend(fk_list.iter().cloned());
                }
                let raw_h = s.get("hash").to_str();
                if let Some(vs) = confirmed_vecs.get(&raw_h) {
                    course_vecs.put(&raw_h, V::from(vs.clone()));
                }
            }
            let item = obj! {
                "course" => r.get("course"),
                "steps" => shown_steps,
                "runs" => r.get("runs"),
                "premium" => r.get("premium"),
                "discount" => r.get("discount"),
                "tier" => t,
                "record" => record,
                "average" => V::from(average),
                "checkpoints" => V::from(checkpoints),
                "interaction_traits" => r.get("interaction_traits").to_num().max(0.0),
            };
            if let Some(vr) = verdicts.get(&mk(&r.get("course"))) {
                item.put("verdict", verdict_row_to_v(vr));
            }
            if !course_forks.is_empty() {
                item.put("verified_forks", V::from(course_forks));
            }
            if !course_vecs.keys().is_empty() {
                item.put("vectors", course_vecs);
            }
            out.push(item);
        }
        Ok(out)
    }

    /// Phase 5 Part 3: `POST /nav/trials/claim-wallet` — 세션 + 지갑 일회성 서명 이중 검증 후 무중복 승격.
    async fn handle_claim_wallet(req: &Req, env: &Env, origin: &str, now_ms: f64) -> R<Resp> {
        if req.method != "POST" {
            return Ok(error("method not allowed", 405, origin));
        }
        let Some(session_addr) = verified_address(req, env).await else {
            return Ok(error("valid session required", 401, origin));
        };
        let Some(wallet_addr) = verify_wallet_request_once(req, env, &req.body, now_ms).await? else {
            return Ok(error("valid wallet signature required", 401, origin));
        };
        let secret = env.get("AUTHOR_KEY_SECRET");
        let session_key = account_key(&secret, "session", &session_addr)?;
        let wallet_key = account_key(&secret, "wallet", &wallet_addr)?;
        let wallet_rows = rows(env, SQL_ALL_MINE, &[V::from(wallet_key.as_str())]).await?;
        let mut course_ids = Vec::with_capacity(wallet_rows.len());
        let mut stmts = Vec::new();
        let today = day_of(now_ms);
        for wr in &wallet_rows {
            let cid = wr.get("course").to_str();
            course_ids.push(cid);
            stmts.push(stmt(
                env,
                SQL_MERGE_CLAIM_TRIAL,
                &[
                    V::from(session_key.as_str()),
                    wr.get("app"),
                    wr.get("course"),
                    wr.get("steps"),
                    wr.get("runs"),
                    wr.get("premium"),
                    wr.get("discount"),
                    wr.get("best"),
                    wr.get("last"),
                    wr.get("last_day"),
                    wr.get("touched"),
                    wr.get("best_total_ms"),
                    wr.get("pinned"),
                ],
            )?);
        }
        let preseeded = plan_wallet_claim_registrations(&session_key, &course_ids);
        for reg_id in &preseeded {
            stmts.push(stmt(env, SQL_REGISTER_PRESEED, &[V::from(reg_id.as_str()), V::Num(today)])?);
        }
        stmts.push(stmt(env, SQL_DELETE_ALL, &[V::from(wallet_key.as_str())])?);
        if !stmts.is_empty() {
            env.batch("DB", &V::from(stmts)).await?;
        }
        Ok(reply(
            obj! { "account" => "session", "claimed" => wallet_rows.len(), "preseeded" => preseeded.len() },
            200,
            origin,
            &[],
        ))
    }

    fn has_wallet_headers(req: &Req) -> bool {
        WALLET_HEADERS.iter().any(|h| !req.header(h).is_null())
    }

    /// `authenticate(request, body, env, nowMs)` — 지갑 헤더가 있으면 지갑으로만, 없으면 세션으로 확인한다. 어느 쪽도 아니면 None
    async fn authenticate(req: &Req, env: &Env, now_ms: f64) -> R<Option<Account>> {
        if has_wallet_headers(req) {
            return Ok(match verify_wallet_request_once(req, env, &req.body, now_ms).await? {
                Some(address) => Some(Account { kind: "wallet", key: account_key(&env.get("AUTHOR_KEY_SECRET"), "wallet", &address)? }),
                None => None,
            });
        }
        Ok(match verified_address(req, env).await {
            Some(address) => Some(Account { kind: "session", key: account_key(&env.get("AUTHOR_KEY_SECRET"), "session", &address)? }),
            None => None,
        })
    }

    /// `url.searchParams.get(name)`
    fn search_param(url: &str, name: &str) -> Option<String> {
        let u = url::Url::parse(url).ok()?;
        u.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned())
    }

    /// try 안 (`handleNavTrials` 643~693 + Phase 4/5/7 확장)
    async fn handle_inner(req: &Req, env: &Env, origin: &str, now_ms: f64) -> R<Resp> {
        if req.body.len() as f64 > MAX_BODY_BYTES {
            return Ok(error("body too large", 413, origin));
        }
        if pathname(req) == CLAIM_WALLET_PATH {
            return handle_claim_wallet(req, env, origin, now_ms).await;
        }
        let app = search_param(&req.url, "app").map(|a| to_lower(js_trim(&a)));
        if let Some(a) = &app {
            if !is_hash(a) {
                return Ok(error("app must be 0x + 40 hex", 400, origin));
            }
        }
        let entry = search_param(&req.url, "entry").map(|e| to_lower(js_trim(&e)));
        if let Some(e) = &entry {
            if !is_hash(e) {
                return Ok(error("entry must be 0x + 40 hex", 400, origin));
            }
        }
        let method = req.method.as_str();

        if method == "GET" {
            let Some(app) = app else { return Ok(error("app is required", 400, origin)) };
            let signed_in = has_wallet_headers(req) || read_session_credentials(req).is_some();
            let account = if signed_in { authenticate(req, env, now_ms).await? } else { None };
            if signed_in && account.is_none() {
                return Ok(error("sign in, or sign the request with a wallet", 401, origin));
            }
            let mine = match &account {
                Some(a) => Some(read_mine(env, a, &app).await?),
                None => None,
            };
            let mine_courses: Vec<V> = mine.iter().flatten().map(|m| m.get("course")).collect();
            let others = read_others(env, &app, &mine_courses, entry.as_deref()).await?;
            let body = obj! {
                "app" => app.as_str(),
                "account" => account.as_ref().map_or(V::Null, |a| V::from(a.kind)),
                "mine" => mine.map_or(V::Null, V::from),
                "others" => V::from(others),
                "policy" => obj! { "max_steps" => MAX_STEPS, "max_courses" => MAX_COURSES, "publish_min_runs" => PUBLISH_MIN_RUNS },
            };
            return Ok(reply(body, 200, origin, &[]));
        }

        let Some(account) = authenticate(req, env, now_ms).await? else {
            return Ok(error("sign in, or sign the request with a wallet", 401, origin));
        };

        if method == "DELETE" {
            let key = V::from(account.key.as_str());
            let res = match &app {
                Some(a) => js::run(&stmt(env, SQL_DELETE_APP, &[key, V::from(a.as_str())])?).await?,
                _ => js::run(&stmt(env, SQL_DELETE_ALL, &[key])?).await?,
            };
            return Ok(reply(obj! { "account" => account.kind, "deleted" => changes(&res)? }, 200, origin, &[]));
        }

        let Ok(parsed) = js_sys::JSON::parse(&gzip::decode_utf8(&req.body)) else {
            return Ok(error("invalid JSON body", 400, origin));
        };
        let is_obj = parsed.is_object() && !Array::is_array(&parsed);

        // Phase 5 Part 2: 선택적 batch_id 멱등 재전송 보호(24시간 TTL)
        let batch_id = if is_obj {
            Reflect::get(&parsed, &JsValue::from_str("batch_id"))
                .ok()
                .and_then(|v| v.as_string())
        } else {
            None
        };
        if let Some(bid) = &batch_id {
            if !valid_batch_id(bid) {
                return Ok(error("batch_id must be 16 to 64 alphanumeric/-/_ chars", 400, origin));
            }
            let cutoff = now_ms - DAY_MS;
            let cached = js::first(&stmt(env, SQL_BATCH_GET, &[V::from(bid.as_str()), V::from(account.key.as_str()), V::Num(cutoff)])?).await?;
            if !cached.is_nullish() {
                if let Ok(resp_v) = js_parse(&cached.get("response_json")) {
                    return Ok(reply(resp_v, 200, origin, &[]));
                }
            }
        }

        let runs = if is_obj { Reflect::get(&parsed, &JsValue::from_str("runs")).unwrap_or(JsValue::UNDEFINED) } else { JsValue::UNDEFINED };
        let count = if Array::is_array(&runs) { runs.unchecked_ref::<Array>().length() as usize } else { 0 };

        let raw_dead_ends = if is_obj {
            Reflect::get(&parsed, &JsValue::from_str("dead_end_marks")).unwrap_or(JsValue::UNDEFINED)
        } else {
            JsValue::UNDEFINED
        };
        let dead_ends = match parse_dead_end_marks(&body_v(&raw_dead_ends, 2), day_of(now_ms)) {
            Ok(d) => d,
            Err(msg) => return Ok(error(&msg, 400, origin)),
        };

        if (!Array::is_array(&runs) || count == 0 || count > MAX_RUNS) && dead_ends.is_empty() {
            return Ok(error(&format!("runs must hold 1 to {MAX_RUNS} records"), 400, origin));
        }
        let results = if Array::is_array(&runs) && count > 0 {
            if count > MAX_RUNS {
                return Ok(error(&format!("runs must hold 1 to {MAX_RUNS} records"), 400, origin));
            }
            let items: Vec<V> = runs.unchecked_ref::<Array>().iter().map(|x| body_v(&x, 2)).collect();
            store_runs(env, &account, &items, now_ms).await?
        } else {
            Vec::new()
        };
        let dead_end_recorded = if !dead_ends.is_empty() {
            store_dead_end_marks(env, &account, &dead_ends, now_ms).await?
        } else {
            0
        };
        let resp_body = obj! { "account" => account.kind, "results" => V::from(results) };
        if dead_end_recorded > 0 {
            resp_body.put("dead_end_recorded", V::from(dead_end_recorded));
        }
        if let Some(bid) = &batch_id {
            if let Some(resp_json) = stringify(&resp_body) {
                let _ = js::run(&stmt(env, SQL_BATCH_PUT, &[V::from(bid.as_str()), V::from(account.key.as_str()), V::from(resp_json), V::Num(now_ms)])?).await;
            }
        }
        Ok(reply(resp_body, 200, origin, &[]))
    }

    /// 원본 `handleNavTrials(request, env, nowMs = Date.now())` + `/nav/trials/claim-wallet`
    pub async fn handle(req: &Req, env: &Env) -> R<Option<Resp>> {
        let now_ms = platform::now();
        let path = pathname(req);
        if path != NAV_TRIALS_PATH && path != CLAIM_WALLET_PATH {
            return Ok(None);
        }
        let origin = header_or_empty(req, "Origin");
        let method = req.method.as_str();
        if method == "OPTIONS" {
            return Ok(Some(empty_response(204, nav_cors(&origin))));
        }
        if method != "GET" && method != "POST" && method != "DELETE" {
            return Ok(Some(error("method not allowed", 405, &origin)));
        }
        let limiter = env.get(if method == "GET" { "EVAL_READ_LIMITER" } else { "EVAL_WRITE_LIMITER" });
        if is_rate_limited(req, &limiter).await {
            let retry = RETRY_AFTER_SECONDS.to_string();
            return Ok(Some(reply(obj! { "error" => "rate limit exceeded" }, 429, &origin, &[("Retry-After", retry.as_str())])));
        }
        let content_length = match req.header("Content-Length") {
            V::Str(s) => string_to_number(&s),
            _ => 0.0,
        };
        if content_length > MAX_BODY_BYTES {
            return Ok(Some(error("body too large", 413, &origin)));
        }
        Ok(Some(match handle_inner(req, env, &origin, now_ms).await {
            Ok(resp) => resp,
            Err(err) => {
                console_log!("nav trials error", err);
                error("internal error", 500, &origin)
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use premium_discount_worker_common::parse_str;

    // navtrials.test.ts 의 기준값 (ethers 6.17 · crates/premium-discount-nav trial.rs 시험과 같다)
    const APP: &str = "0x39206c971297059b89e4e6fc35844ee87feb0407";
    const CHAIN: [&str; 3] = ["0x826797e39016c80c7db39259352ad3af843f0acf", "0x517cbe9e479a79a6ef66e403072e9776000d90fc", "0xa9c45e19240962f6cb2dfb7aa8381e3ec3d80340"];
    const CP_R2: &str = "0x4e9fa56d3569702eb1ec03106e8bd1896a716af5";
    const WEB_COURSE: &str = "0xe8c5279f506af6687f43eee19360173c51f44cce";

    fn fill(c: char) -> String {
        format!("0x{}", c.to_string().repeat(40))
    }

    #[test]
    fn ids_match_the_device_baselines() {
        assert_eq!(hash_str("nav-app/1|shop.example.com"), APP);
        let (s1, r2, s3) = (fill('1'), fill('2'), fill('3'));
        assert_eq!(checkpoint_id("region", &r2, None), CP_R2);
        let ids = vec![checkpoint_id("screen", &s1, None), checkpoint_id("region", &r2, None), checkpoint_id("screen", &s3, None)];
        assert_eq!(course_chain(APP, &ids), CHAIN.to_vec());
        let web = vec![
            checkpoint_id("page", &fill('a'), None),
            checkpoint_id("element", &fill('b'), None),
            checkpoint_id("page", &fill('a'), Some("scroll-down")),
            checkpoint_id("element", &fill('c'), Some("swipe-left")),
            checkpoint_id("page", &fill('d'), None),
        ];
        assert_eq!(course_chain(APP, &web).last().unwrap(), WEB_COURSE);
    }

    #[test]
    fn tier_table() {
        let t = |p: f64, d: f64| tier(&V::Num(p), &V::Num(d));
        assert_eq!([t(2.0, 0.0), t(2.0, 1.0), t(1.0, 2.0), t(3.0, 2.0), t(4.0, 2.0), t(0.0, 3.0)], ["unrated", "premium", "discount", "neutral", "premium", "discount"]);
        // JS 의미: null 은 0, 글은 이어 붙인다 ("1" + 2 = "12" ≥ 3)
        assert_eq!(tier(&V::Null, &V::Num(3.0)), "discount");
        assert_eq!(tier(&V::from("1"), &V::Num(2.0)), "neutral");
    }

    #[test]
    fn route_marks_table() {
        let st = |kind: &str, hash: &str| obj! { "kind" => kind, "hash" => hash };
        let (start, a, b, c, finish) = (st("screen", &fill('1')), st("region", &fill('2')), st("region", &fill('4')), st("region", &fill('5')), st("screen", &fill('3')));
        let ghost = V::from(vec![start.clone(), a.clone(), b.clone(), finish.clone()]);
        let marks = |run: Vec<V>| route_marks(&run, &ghost).unwrap().map(|m| m.into_iter().map(|(s, k)| (s.get("hash").to_str(), k)).collect::<Vec<_>>());
        assert_eq!(marks(vec![start.clone(), a.clone(), c.clone(), finish.clone()]), Some(vec![(fill('2'), 1), (fill('4'), -1)]));
        assert_eq!(marks(vec![start.clone(), b.clone(), a.clone(), finish.clone()]), Some(vec![(fill('2'), 1), (fill('4'), 1)]));
        assert_eq!(marks(vec![start.clone(), finish.clone()]), Some(vec![(fill('2'), -1), (fill('4'), -1)]));
        assert_eq!(marks(vec![start.clone(), a.clone(), c.clone()]), None);
        assert_eq!(marks(vec![c.clone(), a.clone(), finish.clone()]), None);
        assert_eq!(marks(vec![]), None);
        let short = V::from(vec![start.clone(), finish.clone()]);
        assert_eq!(route_marks(&[start.clone(), a, c, finish.clone()], &short).unwrap().unwrap().len(), 0);
        assert_eq!(route_marks(&[start.clone()], &V::Null).unwrap_err().message, "Cannot read properties of null (reading 'length')");
        assert_eq!(route_marks(&[start], &V::from(vec![V::Null])).unwrap_err().message, "Cannot read properties of null (reading 'gesture')");
    }

    #[test]
    fn parse_run_messages() {
        let today = 20774.0;
        let ids = [checkpoint_id("screen", &fill('1'), None), checkpoint_id("screen", &fill('3'), None)];
        let chain = course_chain(APP, &ids);
        let run = format!(
            r#"{{"schema":"trial/1","app":" {} ","course":"{}","steps":[{{"kind":"screen","hash":"{}","split_ms":0,"gesture":"press"}},{{"kind":"screen","hash":"{}","split_ms":1000,"vectors":[{{"space":"s","vector":[0.6,0.8]}}]}}],"total_ms":1000,"type":null,"day":{today},"recipe":"pd-verdict/2"}}"#,
            APP.to_uppercase().replace("0X", "0x"),
            chain[1],
            fill('1'),
            fill('3')
        );
        let ok = parse_run(&parse_str(&run).unwrap(), today).unwrap();
        assert_eq!(ok.app, APP);
        assert_eq!(ok.steps[1].course, chain[1]);
        assert_eq!(ok.vectors[0].bytes().len(), 8);
        assert_eq!(premium_discount_worker_common::stringify(&ok.steps[0].to_v()).unwrap(), format!(r#"{{"kind":"screen","hash":"{}","id":"{}","course":"{}"}}"#, fill('1'), ids[0], chain[0]));
        let bad = |patch: &str, want: &str| {
            let v = parse_str(&run).unwrap();
            let p = parse_str(patch).unwrap();
            for (k, x) in p.entries() {
                v.put(&k, x);
            }
            assert_eq!(parse_run(&v, today).unwrap_err(), want, "{patch}");
        };
        bad(r#"{"schema":"trial/2"}"#, "schema must be trial/1");
        bad(r#"{"day":20775.5}"#, "day must be a UTC day number between 2026-01-01 and tomorrow");
        bad(r#"{"type":"neutral"}"#, "type must be premium, discount or null");
        bad(r#"{"total_ms":900}"#, "total_ms must equal the last split");
        bad(r#"{"ghost":"nope"}"#, "ghost must be 0x + 40 hex");
        bad(r#"{"course":"0x0000000000000000000000000000000000000007"}"#, "course does not match the checkpoints");
        assert_eq!(parse_run(&V::from(Vec::<V>::new()), today).unwrap_err(), "run must be an object");
        assert_eq!(js_round(-0.4).to_string(), "-0");
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert!(is_space("granite-97m-r2-cls-v1") && !is_space("Granite") && !is_space(""));
    }

    #[test]
    fn account_keys_differ_by_kind() {
        let k = account_key(&V::from("secret"), "session", "0xABC").unwrap();
        assert_eq!(k, account_key(&V::from("secret"), "session", "0xabc").unwrap());
        assert_eq!(k.len(), 64);
        assert_ne!(k, account_key(&V::from("secret"), "wallet", "0xabc").unwrap());
        assert_eq!(account_key(&V::from(""), "wallet", "0xabc").unwrap_err().message, "AUTHOR_KEY_SECRET is not configured");
        let cors = nav_cors("tauri://localhost");
        assert_eq!(cors.len(), 4);
        assert_eq!(cors[0].1, "GET, POST, DELETE, OPTIONS");
        assert_eq!(cors[1].1, "Content-Type, x-oauth-hash, x-oauth-token, x-pd-address, x-pd-time, x-pd-signature");
    }
}
