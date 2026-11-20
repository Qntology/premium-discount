//! 원본 `index.ts` 의 판정 절 (`processVerdicts`, PD-Verdict v1).
//!
//! www 가 투표를 eval_votes 에 더하고 verdict_dirty 에 표시한다. 한 번의 실행에서
//! 1. 표시된 평가와, 가장 오래전에 계산한 판정(sweep)의 근거량 E 와 값 v 를 다시 계산한다
//! 2. 그 평가들이 속한 범위(secondary: cc+to, primary: cc, global: 전체)의 기준선을 다시 계산한다
//! 3. 그 평가들과, 건드린 secondary·primary 범위의 피어를 다시 판정한다
//!
//! JS 의 `Map` · `Set` 은 처음 넣은 자리를 지키며 값만 바꾼다 — `IndexMap::insert` 가 같다.

use indexmap::{IndexMap, IndexSet};
use premium_discount_verdict::{baseline_of, evidence_in, judge_in, Baseline, Baselines, Context, Level, Obs, Prev, Tier};
use premium_discount_worker_common::js::{self, Env};
use premium_discount_worker_common::jsv::{strict_eq, R, V};
use premium_discount_worker_common::stringify_str;

use crate::queue::stmt;
use crate::verdict::{math, params, tier_of, track, weight_of, RECIPE};

const VERDICT_BATCH: f64 = 50.0; // 한 번의 실행에서 처리할 투표 표시 최대 건수 (임시값)
const VERDICT_SWEEP: f64 = 25.0; // 실행마다 다시 계산할 가장 오래된 판정 수 (임시값)
const JUDGE_LIMIT: f64 = 500.0; // 건드린 범위마다 다시 판정할 피어 최대 수 (임시값)
const JUDGE_CHUNK: usize = 200; // 판정 결과를 JSON 배열 하나로 묶는 행 수
const GLOBAL_SCOPE: &str = "*";

const JUDGE_COLS: &str = "\"eval_id\", \"cc\", \"to\", \"evidence\", \"value\", \"tier\", \"tier_since\",\n\tEXISTS (SELECT 1 FROM eval_supersedes s WHERE s.\"old_id\" = eval_verdicts.\"eval_id\") AS \"superseded\"";

const SQL_DIRTY: &str = "SELECT \"eval_id\", \"at\" FROM verdict_dirty ORDER BY \"at\" LIMIT ?1";
const SQL_SWEEP: &str = "SELECT \"eval_id\" FROM eval_verdicts ORDER BY \"updated_at\" LIMIT ?1";
const SQL_EVALUATION: &str = "SELECT \"cc\", \"to\", \"from\" FROM evaluations WHERE \"id\" = ?1";
const SQL_CLEAR_MARK: &str = "DELETE FROM verdict_dirty WHERE \"eval_id\" = ?1 AND \"at\" <= ?2";
const SQL_VOTES: &str = "SELECT \"voter\", \"voter_kind\", \"kind\", \"value\", \"created_at\" FROM eval_votes WHERE \"eval_id\" = ?1 ORDER BY \"created_at\", \"id\"";
const SQL_UPSERT_VERDICT: &str = "INSERT INTO eval_verdicts (\"eval_id\", \"cc\", \"to\", \"evidence\", \"value\", \"up\", \"down\", \"level\", \"rank\", \"tier\", \"grade\", \"tier_since\", \"reasons\", \"recipe\", \"updated_at\", \"judged_at\")\n\t\t VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, 'unrated', NULL, ?8, '[]', ?9, ?8, ?8)\n\t\t ON CONFLICT (\"eval_id\") DO UPDATE SET \"evidence\" = excluded.\"evidence\", \"value\" = excluded.\"value\",\n\t\t \"up\" = excluded.\"up\", \"down\" = excluded.\"down\", \"updated_at\" = excluded.\"updated_at\"";
const SQL_DROP_MARKS: &str = "DELETE FROM verdict_dirty WHERE \"eval_id\" = ?1";
const SQL_STALE: &str = "SELECT \"cc\", \"to\" FROM eval_verdicts WHERE \"eval_id\" = ?1";
const SQL_DROP_VERDICT: &str = "DELETE FROM eval_verdicts WHERE \"eval_id\" = ?1";
const SQL_VALUES_SECONDARY: &str = "SELECT \"value\" FROM eval_verdicts WHERE \"cc\" = ?1 AND \"to\" = ?2 AND \"evidence\" >= ?3";
const SQL_VALUES_PRIMARY: &str = "SELECT \"value\" FROM eval_verdicts WHERE \"cc\" = ?1 AND \"evidence\" >= ?2";
const SQL_VALUES_GLOBAL: &str = "SELECT \"value\" FROM eval_verdicts WHERE \"evidence\" >= ?1";
const SQL_SAVE_SCOPE: &str = "INSERT OR REPLACE INTO verdict_scopes (\"scope_key\", \"n\", \"mean\", \"sd\", \"updated_at\") VALUES (?1, ?2, ?3, ?4, ?5)";
const SQL_READ_SCOPES: &str = "SELECT \"scope_key\", \"n\", \"mean\", \"sd\" FROM verdict_scopes WHERE \"scope_key\" IN (SELECT \"value\" FROM json_each(?1))";
const SQL_WRITE: &str = "UPDATE eval_verdicts SET\n\t\t\t \"tier\" = json_extract(j.\"value\", '$.tier'), \"grade\" = json_extract(j.\"value\", '$.grade'),\n\t\t\t \"rank\" = json_extract(j.\"value\", '$.rank'), \"level\" = json_extract(j.\"value\", '$.level'),\n\t\t\t \"reasons\" = json_extract(j.\"value\", '$.reasons'), \"tier_since\" = json_extract(j.\"value\", '$.since'),\n\t\t\t \"recipe\" = ?2, \"judged_at\" = ?3\n\t\t\t FROM json_each(?1) AS j\n\t\t\t WHERE eval_verdicts.\"eval_id\" = json_extract(j.\"value\", '$.id')";

fn sql_peers_secondary() -> String {
    format!("SELECT {JUDGE_COLS} FROM eval_verdicts WHERE \"cc\" = ?1 AND \"to\" = ?2 ORDER BY \"judged_at\" LIMIT ?3")
}
fn sql_peers_primary() -> String {
    format!("SELECT {JUDGE_COLS} FROM eval_verdicts WHERE \"cc\" = ?1 ORDER BY \"judged_at\" LIMIT ?2")
}
fn sql_judge_rows() -> String {
    format!("SELECT {JUDGE_COLS} FROM eval_verdicts WHERE \"eval_id\" IN (SELECT \"value\" FROM json_each(?1))")
}

/// `Scope` — verdict_scopes.scope_key · 단계 · cc · to
#[derive(Clone)]
struct Scope {
    key: String,
    level: Level,
    cc: V,
    to: V,
}

/// `scopesOf(cc, to)` — secondary 는 to 가 있을 때만
fn scopes_of(cc: &V, to: &V) -> Vec<Scope> {
    let mut scopes = Vec::new();
    if !strict_eq(to, &V::Null) {
        scopes.push(Scope { key: format!("cc:{}|to:{}", cc.to_str(), to.to_str()), level: Level::Secondary, cc: cc.clone(), to: to.clone() });
    }
    scopes.push(Scope { key: format!("cc:{}", cc.to_str()), level: Level::Primary, cc: cc.clone(), to: V::Null });
    scopes.push(Scope { key: GLOBAL_SCOPE.into(), level: Level::Global, cc: V::Null, to: V::Null });
    scopes
}

/// `processVerdicts(env, now)` → (computed, judged)
pub async fn process_verdicts(env: &Env, now: f64) -> R<(f64, f64)> {
    let dirty = js::all(&stmt(env, SQL_DIRTY, &[V::Num(VERDICT_BATCH)])?).await?;
    let swept = js::all(&stmt(env, SQL_SWEEP, &[V::Num(VERDICT_SWEEP)])?).await?;
    // 읽어 둔 표시 시각. 계산 뒤에는 이 값 이하인 표시만 지운다
    let mut marks: IndexMap<String, V> = IndexMap::new();
    for d in dirty.g("results")?.items() {
        marks.insert(d.g("eval_id")?.to_key(), d.g("at")?);
    }
    let mut work: IndexSet<String> = marks.keys().cloned().collect();
    for s in swept.g("results")?.items() {
        work.insert(s.g("eval_id")?.to_key());
    }

    let mut touched: IndexMap<String, Scope> = IndexMap::new();
    let mut computed: Vec<String> = Vec::new();
    for id in &work {
        let idv = V::from(id.as_str());
        let ev = js::first(&stmt(env, SQL_EVALUATION, &[idv.clone()])?).await?;
        if !ev.truthy() {
            drop_verdict(env, &idv, &mut touched).await?;
            continue;
        }
        compute_verdict(env, &idv, &ev, now).await?;
        if let Some(at) = marks.get(id) {
            if !at.is_undef() {
                js::run(&stmt(env, SQL_CLEAR_MARK, &[idv.clone(), at.clone()])?).await?;
            }
        }
        for s in scopes_of(&ev.g("cc")?, &ev.g("to")?) {
            touched.insert(s.key.clone(), s);
        }
        computed.push(id.clone());
    }
    if touched.is_empty() {
        return Ok((0.0, 0.0));
    }

    // 건드린 범위의 기준선은 다시 계산해 쓰고, 나머지 범위는 저장된 값을 읽는다
    let mut baselines: IndexMap<String, Baseline> = IndexMap::new();
    for s in touched.values() {
        let b = refresh_scope(env, s, now).await?;
        baselines.insert(s.key.clone(), b);
    }

    // 다시 판정할 행: 계산한 평가 + 건드린 secondary·primary 범위의 피어 (범위마다 JUDGE_LIMIT 까지)
    let mut rows: IndexMap<String, V> = IndexMap::new();
    for r in read_judge_rows(env, &computed).await? {
        rows.insert(r.g("eval_id")?.to_key(), r);
    }
    for s in touched.values() {
        if s.level == Level::Global {
            continue;
        }
        for r in scope_peers(env, s).await? {
            rows.insert(r.g("eval_id")?.to_key(), r);
        }
    }
    let mut missing: IndexSet<String> = IndexSet::new();
    for r in rows.values() {
        for s in scopes_of(&r.g("cc")?, &r.g("to")?) {
            if !baselines.contains_key(&s.key) {
                missing.insert(s.key);
            }
        }
    }
    let missing: Vec<String> = missing.into_iter().collect();
    for (key, b) in read_scopes(env, &missing).await? {
        baselines.insert(key, b);
    }

    let mut judged: Vec<V> = Vec::new();
    for r in rows.values() {
        let mut bs = Baselines::default();
        for s in scopes_of(&r.g("cc")?, &r.g("to")?) {
            if let Some(b) = baselines.get(&s.key) {
                match s.level {
                    Level::Secondary => bs.secondary = Some(*b),
                    Level::Primary => bs.primary = Some(*b),
                    Level::Global => bs.global = Some(*b),
                }
            }
        }
        let prev_tier = r.g("tier")?;
        let prev_since = r.g("tier_since")?;
        let known = tier_of(&prev_tier);
        let cx = Context {
            track: track(),
            baselines: &bs,
            prev: Prev { tier: known.unwrap_or(Tier::Unrated), since_ms: prev_since.to_num() as i64 },
            now_ms: now as i64,
            duel: None,
            superseded: strict_eq(&r.g("superseded")?, &V::Num(1.0)),
        };
        let v = judge_in(math(), r.g("evidence")?.to_num(), r.g("value")?.to_num(), &cx, params());
        // since: 등급이 그대로면 직전 값(D1 의 tier_since 그대로), 바뀌면 now. 모르는 글의 직전 등급과는 늘 다르다
        let since = if known == Some(v.tier) { prev_since } else { V::Num(now) };
        let item = V::new_obj();
        item.put("id", r.g("eval_id")?);
        item.put("tier", V::from(v.tier.name()));
        item.put("grade", v.grade.map_or(V::Null, |g| V::Num(g as f64)));
        item.put("rank", v.rank.map_or(V::Null, V::Num));
        item.put("level", v.level.map_or(V::Null, |l| V::from(l.name())));
        item.put("reasons", V::from(v.reasons.iter().map(|x| V::from(x.as_str())).collect::<Vec<_>>()));
        item.put("since", since);
        judged.push(item);
    }
    write_judgements(env, &judged, now).await?;
    Ok((computed.len() as f64, judged.len() as f64))
}

/// `computeVerdict(env, id, ev, now)` — E·v 를 다시 계산해 저장한다. (투표자, 종류)마다 가장 최근 표만 센다
async fn compute_verdict(env: &Env, id: &V, ev: &V, now: f64) -> R<()> {
    let votes = js::all(&stmt(env, SQL_VOTES, &[id.clone()])?).await?;
    // 시간순으로 읽으므로 같은 (투표자, 종류)는 뒤의 행이 앞의 행을 덮는다
    let mut latest: IndexMap<String, V> = IndexMap::new();
    for v in votes.g("results")?.items() {
        latest.insert(format!("{}|{}", v.g("voter")?.to_str(), v.g("kind")?.to_str()), v);
    }

    let from = ev.g("from")?;
    let mut obs: Vec<Obs> = Vec::new();
    let (mut up, mut down) = (0.0, 0.0);
    for v in latest.values() {
        let value = v.g("value")?;
        if strict_eq(&value, &V::Num(0.0)) {
            continue; // 철회
        }
        // 확인된 로그인의 표만 센다. 게스트 주소는 IP 만 바꾸면 늘릴 수 있고, 작성자는 자기 평가를 올릴 수 없어야 한다
        let counted = strict_eq(&v.g("voter_kind")?, &V::from("session")) && !strict_eq(&v.g("voter")?, &from);
        let weight = if counted { weight_of(&v.g("kind")?) } else { 0.0 };
        obs.push(Obs {
            kind: v.g("kind")?.to_str(),
            // www 는 1 · -1 · 0 만 받는다 (그 밖의 값은 docs/workers-rs.md "알려진 차이")
            sign: value.to_num() as i32,
            weight,
            credit: 1.0,
            at_ms: v.g("created_at")?.to_num() as i64,
        });
        if weight > 0.0 && strict_eq(&value, &V::Num(1.0)) {
            up += 1.0;
        }
        if weight > 0.0 && strict_eq(&value, &V::Num(-1.0)) {
            down += 1.0;
        }
    }
    let (e, value) = evidence_in(math(), &obs, now as i64, track(), params());

    // 새 행은 unrated 로 시작하고 같은 실행의 판정 단계에서 등급을 받는다. 있던 행은 E·v 와 표 수만 바꾼다
    js::run(&stmt(
        env,
        SQL_UPSERT_VERDICT,
        &[id.clone(), ev.g("cc")?, ev.g("to")?, V::Num(e), V::Num(value), V::Num(up), V::Num(down), V::Num(now), V::from(RECIPE)],
    )?)
    .await?;
    Ok(())
}

/// `dropVerdict` — 평가가 없다. 표시를 지우고, 남은 판정 행도 지운 뒤 그 범위를 다시 계산하게 한다
async fn drop_verdict(env: &Env, id: &V, touched: &mut IndexMap<String, Scope>) -> R<()> {
    js::run(&stmt(env, SQL_DROP_MARKS, &[id.clone()])?).await?;
    let stale = js::first(&stmt(env, SQL_STALE, &[id.clone()])?).await?;
    if !stale.truthy() {
        return Ok(());
    }
    js::run(&stmt(env, SQL_DROP_VERDICT, &[id.clone()])?).await?;
    for s in scopes_of(&stale.g("cc")?, &stale.g("to")?) {
        touched.insert(s.key.clone(), s);
    }
    Ok(())
}

/// `refreshScope` — 판정 가능한(E ≥ min_evidence) 평가들의 v 로 기준선을 다시 계산해 쓴다
async fn refresh_scope(env: &Env, s: &Scope, now: f64) -> R<Baseline> {
    let min = V::Num(params().min_evidence);
    let rs = match s.level {
        Level::Secondary => js::all(&stmt(env, SQL_VALUES_SECONDARY, &[s.cc.clone(), s.to.clone(), min])?).await?,
        Level::Primary => js::all(&stmt(env, SQL_VALUES_PRIMARY, &[s.cc.clone(), min])?).await?,
        Level::Global => js::all(&stmt(env, SQL_VALUES_GLOBAL, &[min])?).await?,
    };
    let mut values = Vec::new();
    for r in rs.g("results")?.items() {
        values.push(r.g("value")?.to_num());
    }
    let b = baseline_of(&values);
    js::run(&stmt(env, SQL_SAVE_SCOPE, &[V::from(s.key.as_str()), V::Num(b.n as f64), V::Num(b.mean), V::Num(b.sd), V::Num(now)])?).await?;
    Ok(b)
}

/// `scopePeers` — 가장 오래전에 판정한 행부터
async fn scope_peers(env: &Env, s: &Scope) -> R<Vec<V>> {
    let rs = if s.level == Level::Secondary {
        js::all(&stmt(env, &sql_peers_secondary(), &[s.cc.clone(), s.to.clone(), V::Num(JUDGE_LIMIT)])?).await?
    } else {
        js::all(&stmt(env, &sql_peers_primary(), &[s.cc.clone(), V::Num(JUDGE_LIMIT)])?).await?
    };
    Ok(rs.g("results")?.items())
}

/// `readJudgeRows` — id 목록은 JSON 배열 하나로 넘긴다 (D1 은 문장당 바인딩이 100개까지)
async fn read_judge_rows(env: &Env, ids: &[String]) -> R<Vec<V>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let list = V::from(ids.iter().map(|x| V::from(x.as_str())).collect::<Vec<_>>());
    let rs = js::all(&stmt(env, &sql_judge_rows(), &[V::from(stringify_str(&list))])?).await?;
    Ok(rs.g("results")?.items())
}

/// `readScopes` — 덩이(JUDGE_CHUNK)마다 한 문장
async fn read_scopes(env: &Env, keys: &[String]) -> R<Vec<(String, Baseline)>> {
    let mut out: IndexMap<String, Baseline> = IndexMap::new();
    for chunk in keys.chunks(JUDGE_CHUNK) {
        let list = V::from(chunk.iter().map(|x| V::from(x.as_str())).collect::<Vec<_>>());
        let rs = js::all(&stmt(env, SQL_READ_SCOPES, &[V::from(stringify_str(&list))])?).await?;
        for r in rs.g("results")?.items() {
            let b = Baseline { n: r.g("n")?.to_num() as u64, mean: r.g("mean")?.to_num(), sd: r.g("sd")?.to_num() };
            out.insert(r.g("scope_key")?.to_key(), b);
        }
    }
    Ok(out.into_iter().collect())
}

/// `writeJudgements` — 덩이마다 UPDATE 한 문장. reasons 는 JSON 배열 글로 저장된다
async fn write_judgements(env: &Env, items: &[V], now: f64) -> R<()> {
    for chunk in items.chunks(JUDGE_CHUNK) {
        let list = V::from(chunk.to_vec());
        js::run(&stmt(env, SQL_WRITE, &[V::from(stringify_str(&list)), V::from(RECIPE), V::Num(now)])?).await?;
    }
    Ok(())
}
