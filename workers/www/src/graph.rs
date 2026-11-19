//! 원본 `graph.ts` (463줄) — 그래프 API (공개). 경로: /graph, /graph/local, /graph/similar, /graph/view
//!
//! - `GET /graph?cc=example.com` : 도메인 범위 그래프 (최신 200건, created_at DESC)
//! - `GET /graph/local?id=…` : 선택한 평가가 속한 스레드(같은 root) 그래프 (200건, created_at ASC)
//! - `GET /graph/similar?id=…&topK=10` : 선택한 평가와 벡터가 비슷한 평가 (같은 cc 안에서, Vectorize getByIds → query)
//! - `GET /graph/view` : 옵시디언식 그래프 뷰 페이지 ([`view::VIEW_HTML`], 요청 제한 대상이 아니다)
//!
//! 인증은 없고, JSON 셋은 읽기 제한기(EVAL_READ_LIMITER, IP 기준)를 쓴다. 응답에는 원문과 주소(from, to 원값)가 들어가지 않는다.
//! 작성자는 표시용 키(`projection::author_key`)로만 나온다. 평가 노드에는 판정 등급(tier, grade)을 붙이고, 응답에는 평가 노드의
//! 등급별 개수(summary)를 붙인다. 벡터는 서버 안에서만 쓰고 응답에 넣지 않는다.
//!
//! 이음매: `handle(req, env) -> R<Option<Resp>>` — `Ok(None)` = 원본의 `return null`. 원본의 try 밖에서 던질 수 있는 것은 없어
//! (`new URL` · `isRateLimited` 는 던지지 않는다) `Err` 를 돌려주는 일은 없다. try 안의 예외는 ValidationError 면 400, 그 밖은
//! console `graph error <err>` 와 500 이다.
//!
//! 순수한 부분(검사 · 노드/엣지 만들기 · summary · 배열 꺼내기)은 이 파일 위쪽에 두어 호스트에서 단위 시험하고,
//! D1 · Vectorize 를 부르는 부분은 `wasm` 모듈에 있다.

pub mod view;

use indexmap::IndexMap;
use premium_discount_worker_common::jsv::{slice, strict_eq};
use premium_discount_worker_common::{obj, Throw, R, V};

use crate::util::validation_error;

/// `GRAPH_NODE_LIMIT_TEMP` — TODO(결정 보류): 그래프 노드 수 상한. 확정 전까지 임시값
pub const GRAPH_NODE_LIMIT_TEMP: f64 = 200.0;
/// `SIMILAR_TOP_K_DEFAULT`
pub const SIMILAR_TOP_K_DEFAULT: f64 = 10.0;
/// `SIMILAR_TOP_K_MAX` — 자기 자신을 제외하려고 topK + 1을 요청하므로 최대 49 (Vectorize 상한 50)
pub const SIMILAR_TOP_K_MAX: f64 = 49.0;

/// `SELECT_COLS` — 판정은 eval_verdicts에 있다. 행이 없으면 아직 판정 전이다 (unrated, grade null)
pub const SELECT_COLS: &str = r#"e."id", e."from", e."cc", e."ref", e."root", e."created_at", v."tier", v."grade""#;
/// `FROM_JOINED`
pub const FROM_JOINED: &str = r#"evaluations e LEFT JOIN eval_verdicts v ON v."eval_id" = e."id""#;
/// `GRAPH_PATHS`
pub const GRAPH_PATHS: [&str; 4] = ["/graph", "/graph/local", "/graph/similar", "/graph/view"];

/// `url.searchParams.get(name)` — 처음 것 (form-urlencoded 풀이: `+` 는 공백, 잘못된 UTF-8 은 U+FFFD), 없으면 null
pub fn search_param(url: &url::Url, name: &str) -> V {
    url.query_pairs().find(|(k, _)| k == name).map_or(V::Null, |(_, v)| V::from(v.into_owned()))
}

/// `parseTopK(raw)` — null 이면 10, `Number(raw)` 가 1~49 의 정수가 아니면 ValidationError
pub fn parse_top_k(raw: &V) -> R<f64> {
    if raw.is_null() {
        return Ok(SIMILAR_TOP_K_DEFAULT);
    }
    let n = raw.to_num();
    if !(n.is_finite() && n.fract() == 0.0) || n < 1.0 || n > SIMILAR_TOP_K_MAX {
        return Err(validation_error(format!("topK must be an integer between 1 and {}", SIMILAR_TOP_K_MAX)));
    }
    Ok(n)
}

/// `summarize(nodes)` — 응답에 들어간 평가 노드의 등급별 개수 `{ premium, neutral, discount, unrated }`.
/// 원본 그대로: `summary[n.tier ?? 'unrated']++` 라 네 등급 밖의 tier(cron 만 쓰므로 생기지 않음)는 NaN(JSON null) 칸이 되고,
/// `__proto__` 는 아무 일도 하지 않는다 (Object.prototype 의 다른 이름도 NaN 칸)
pub fn summarize(nodes: &[V]) -> V {
    let summary = obj! { "premium" => 0.0, "neutral" => 0.0, "discount" => 0.0, "unrated" => 0.0 };
    for n in nodes {
        if !strict_eq(&n.get("type"), &V::from("evaluation")) {
            continue;
        }
        let tier = n.get("tier");
        let key = if tier.is_nullish() { "unrated".to_string() } else { tier.to_key() };
        // summary[key]++ : 자기 속성이면 +1, 없으면 undefined(또는 Object.prototype 의 함수) + 1 = NaN
        if key == "__proto__" {
            continue; // 원본 그대로: __proto__ 에 NaN 을 넣는 것은 무시된다
        }
        let next = if summary.has(&key) { summary.get(&key).to_num() + 1.0 } else { f64::NAN };
        summary.put(&key, V::Num(next));
    }
    summary
}

/// `authorKey(r.from, env.AUTHOR_KEY_SECRET)` 를 행마다 (`Promise.all` — 처음 실패한 행의 예외).
/// 비밀이 거짓 값이면 `AUTHOR_KEY_SECRET is not configured`. 행이 없으면 비밀을 보지 않는다
pub fn author_keys(rows: &[V], secret: &V) -> R<Vec<String>> {
    rows.iter()
        .map(|r| {
            if !secret.truthy() {
                return Err(Throw::error("AUTHOR_KEY_SECRET is not configured"));
            }
            let address = r.get("from");
            match &address {
                V::Str(s) => crate::projection::author_key(s, &secret.to_str()),
                // address.toLowerCase() — from 은 NOT NULL TEXT 라 생기지 않는다
                V::Undef | V::Null => Err(Throw::type_error(format!("Cannot read properties of {} (reading 'toLowerCase')", address.to_str()))),
                _ => Err(Throw::type_error("address.toLowerCase is not a function")),
            }
        })
        .collect()
}

/// `r.id.slice(0, n)` 꼴 — 글이 아니면 원본처럼 TypeError (`what` 은 원본 식의 이름)
fn label_of(v: &V, n: f64, what: &str) -> R<V> {
    match v {
        V::Str(s) => Ok(V::from(slice(s, 0.0, Some(n)))),
        V::Undef | V::Null => Err(Throw::type_error(format!("Cannot read properties of {} (reading 'slice')", v.to_str()))),
        _ => Err(Throw::type_error(format!("{what}.slice is not a function"))),
    }
}

/// `buildGraph(rows, env)` 의 본문 — rows -> 노드/엣지. 평가 노드, 도메인 허브, 작성자 허브(표시용 키)를 만든다.
/// 노드는 Map(id → 노드)의 차례(처음 넣은 자리, 같은 id 는 값만 바뀜), 엣지는 넣은 차례
pub fn build_graph(rows: &[V], keys: &[String]) -> R<V> {
    let ids: Vec<V> = rows.iter().map(|r| r.get("id")).collect();
    let mut nodes: IndexMap<String, V> = IndexMap::new();
    let edges = V::new_arr();
    for (r, key) in rows.iter().zip(keys) {
        let id = r.get("id");
        let cc = r.get("cc");
        let domain_id = format!("domain:{}", cc.to_str());
        let author_id = format!("author:{key}");
        let tier = r.get("tier");
        nodes.insert(
            id.to_key(),
            obj! {
                "id" => id.clone(),
                "type" => "evaluation",
                "label" => label_of(&id, 8.0, "r.id")?,
                "cc" => cc.clone(),
                "root" => r.get("root"),
                "created_at" => r.get("created_at"),
                "tier" => if tier.is_nullish() { V::from("unrated") } else { tier },
                "grade" => r.get("grade"),
            },
        );
        if !nodes.contains_key(&domain_id) {
            nodes.insert(domain_id.clone(), obj! { "id" => domain_id.as_str(), "type" => "domain", "label" => cc.clone() });
        }
        if !nodes.contains_key(&author_id) {
            let label = format!("작성자 {}", slice(key, 0.0, Some(6.0)));
            nodes.insert(author_id.clone(), obj! { "id" => author_id.as_str(), "type" => "author", "label" => label });
        }
        edges.push(obj! { "source" => id.clone(), "target" => domain_id.as_str(), "type" => "belongs" })?;
        edges.push(obj! { "source" => id.clone(), "target" => author_id.as_str(), "type" => "authored" })?;
        // 답글 관계: 부모가 같은 응답 안에 있을 때만 연결
        let parent = r.get("ref");
        if parent.truthy() && ids.iter().any(|x| strict_eq(x, &parent)) {
            edges.push(obj! { "source" => id.clone(), "target" => parent, "type" => "reply" })?;
        }
    }
    let all: Vec<V> = nodes.into_values().collect();
    let summary = summarize(&all);
    Ok(obj! { "nodes" => all, "edges" => edges, "summary" => summary })
}

/// `const [vec] = await env.EVAL_VECTORIZE.getByIds([id])` — 배열 · 글(첫 글자)은 첫 원소, 그 밖은 원본처럼 TypeError
pub fn first_of(v: &V) -> R<V> {
    match v {
        V::Arr(_) => Ok(v.at(0)),
        V::Str(s) => Ok(s.chars().next().map_or(V::Undef, |c| V::from(c.to_string()))),
        V::Bytes(_) => Ok(v.get("0")),
        _ => Err(Throw::type_error("(intermediate value) is not iterable")),
    }
}

/// `vec.values` 를 원본의 `if (!vec || !vec.values)` 로 거른 것: None 이면 404 'vector not found'.
/// 원본 그대로: vec 이 배열(또는 TypedArray)이면 `vec.values` 는 `Array.prototype.values` 함수라 참이고, `Array.from(함수)` 는 빈 배열이다
pub fn vector_values(vec: &V) -> Option<V> {
    if !vec.truthy() {
        return None;
    }
    let values = match vec {
        V::Arr(_) | V::Bytes(_) => return Some(V::new_arr()),
        _ => vec.get("values"),
    };
    values.truthy().then_some(values)
}

/// `Array.from(values)` — 배열 · TypedArray 는 원소 그대로(사본), 글은 코드 포인트마다, 길이(length)가 있는 객체는 그만큼,
/// 그 밖(수 · 참거짓 · 길이 없는 객체)은 빈 배열. Vectorize 는 숫자 배열을 준다
pub fn array_from(values: &V) -> V {
    match values {
        V::Arr(_) => V::from(values.items()),
        V::Bytes(b) => V::from(b.borrow().iter().map(|x| V::Num(f64::from(*x))).collect::<Vec<_>>()),
        V::Str(s) => V::from(s.chars().map(|c| V::from(c.to_string())).collect::<Vec<_>>()),
        V::Obj(_) => {
            // ToLength(values.length)
            let n = values.get("length").to_num();
            let n = if n.is_nan() || n <= 0.0 { 0 } else { n.min(9_007_199_254_740_991.0).trunc() as usize };
            V::from((0..n).map(|i| values.get(&i.to_string())).collect::<Vec<_>>())
        }
        _ => V::new_arr(),
    }
}

/// `res.matches.filter((m) => m.id !== id).slice(0, topK)`
pub fn other_matches(res: &V, id: &str, top_k: f64) -> R<Vec<V>> {
    let matches = res.g("matches")?;
    let list = match &matches {
        V::Arr(_) => matches.items(),
        V::Undef | V::Null => return Err(Throw::type_error(format!("Cannot read properties of {} (reading 'filter')", matches.to_str()))),
        _ => return Err(Throw::type_error("res.matches.filter is not a function")),
    };
    let me = V::from(id);
    let mut out = Vec::new();
    for m in list {
        if !strict_eq(&m.g("id")?, &me) {
            out.push(m);
        }
    }
    out.truncate(top_k as usize);
    Ok(out)
}

/// `similarGraph` 의 응답 본문 — 유사 노드(같은 cc)와 `similar` 엣지(score), summary 는 유사 노드만 센다
pub fn similar_body(id: &str, cc: &V, matches: &[V], verdicts: &IndexMap<String, V>) -> R<V> {
    let mut nodes = Vec::new();
    for m in matches {
        let mid = m.get("id");
        let verdict = verdicts.get(&mid.to_key());
        let tier = verdict.map(|v| v.get("tier")).filter(|t| !t.is_nullish()).unwrap_or_else(|| V::from("unrated"));
        let grade = verdict.map(|v| v.get("grade")).filter(|g| !g.is_nullish()).unwrap_or(V::Null);
        nodes.push(obj! {
            "id" => mid.clone(),
            "type" => "evaluation",
            "label" => label_of(&mid, 8.0, "m.id")?,
            "cc" => cc.clone(),
            "tier" => tier,
            "grade" => grade,
        });
    }
    let edges: Vec<V> = matches
        .iter()
        .map(|m| obj! { "source" => id, "target" => m.get("id"), "type" => "similar", "score" => m.get("score") })
        .collect();
    let summary = summarize(&nodes);
    Ok(obj! { "nodes" => nodes, "edges" => edges, "summary" => summary })
}

#[cfg(target_arch = "wasm32")]
pub use wasm::handle;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use premium_discount_worker_common::js::{self, Env};
    use premium_discount_worker_common::web::{Body, Req, Resp};
    use premium_discount_worker_common::{arr, console_log, obj, R, V};

    use super::*;
    use crate::ratelimit::{is_rate_limited, RETRY_AFTER_SECONDS};
    use crate::util::{hv, is_validation, json_response, parse_domain, parse_uuid, verdict_summaries};

    /// `json(body, status)` — Content-Type · Cache-Control
    fn json(body: V, status: u16) -> Resp {
        json_response(&body, status, hv(&[("Content-Type", "application/json; charset=utf-8"), ("Cache-Control", "no-store")]))
    }

    fn not_found(what: &str) -> Resp {
        json(obj! { "error" => what }, 404)
    }

    /// `buildGraph(rows, env)` — 작성자 키를 모두 구한 뒤 노드/엣지
    fn build(results: &V, env: &Env) -> R<Resp> {
        let rows = results.items();
        let keys = author_keys(&rows, &env.get("AUTHOR_KEY_SECRET"))?;
        Ok(json(build_graph(&rows, &keys)?, 200))
    }

    /// `env.DB.prepare(sql).bind(...args)`
    fn stmt(env: &Env, sql: &str, args: &[V]) -> R<V> {
        js::bind_exact(&env.prepare("DB", sql)?, args)
    }

    /// GET /graph?cc=example.com : 도메인 범위 그래프
    async fn domain_graph(url: &url::Url, env: &Env) -> R<Resp> {
        let cc = parse_domain(&search_param(url, "cc"))?;
        let sql = format!(r#"SELECT {SELECT_COLS} FROM {FROM_JOINED} WHERE e."cc" = ?1 ORDER BY e."created_at" DESC LIMIT ?2"#);
        let rs = js::all(&stmt(env, &sql, &[V::from(cc), V::Num(GRAPH_NODE_LIMIT_TEMP)])?).await?;
        build(&rs.g("results")?, env)
    }

    /// GET /graph/local?id=... : 선택한 평가가 속한 스레드(같은 root) 그래프
    async fn local_graph(url: &url::Url, env: &Env) -> R<Resp> {
        let id = parse_uuid(&search_param(url, "id"), "id")?;
        let base = js::first(&stmt(env, r#"SELECT "root" FROM evaluations WHERE "id" = ?1"#, &[V::from(id)])?).await?;
        if !base.truthy() {
            return Ok(not_found("not found"));
        }
        let sql = format!(r#"SELECT {SELECT_COLS} FROM {FROM_JOINED} WHERE e."root" = ?1 ORDER BY e."created_at" ASC LIMIT ?2"#);
        let rs = js::all(&stmt(env, &sql, &[base.g("root")?, V::Num(GRAPH_NODE_LIMIT_TEMP)])?).await?;
        build(&rs.g("results")?, env)
    }

    /// GET /graph/similar?id=...&topK=10 : 선택한 평가와 벡터가 비슷한 평가 (같은 cc 안에서)
    async fn similar_graph(url: &url::Url, env: &Env) -> R<Resp> {
        let id = parse_uuid(&search_param(url, "id"), "id")?;
        let top_k = parse_top_k(&search_param(url, "topK"))?;
        let base = js::first(&stmt(env, r#"SELECT "cc" FROM evaluations WHERE "id" = ?1"#, &[V::from(id.as_str())])?).await?;
        if !base.truthy() {
            return Ok(not_found("not found"));
        }

        let index = env.get("EVAL_VECTORIZE");
        let got = js::call_async(&index, "getByIds", &[js::to_js(&arr![id.as_str()])]).await?;
        let Some(values) = vector_values(&first_of(&got)?) else {
            return Ok(not_found("vector not found"));
        };

        let cc = base.get("cc");
        let options = obj! { "topK" => top_k + 1.0, "filter" => obj! { "cc" => cc.clone() } };
        let res = js::call_async(&index, "query", &[js::to_js(&array_from(&values)), js::to_js(&options)]).await?;
        let matches = other_matches(&res, &id, top_k)?;
        // 최대 49건이라 IN 한 번으로 읽는다. 알려진 차이: id 가 글이 아니면(Vectorize 는 늘 글) 원본은 값 그대로 묶는다
        let ids: Vec<String> = matches.iter().map(|m| m.get("id").to_str()).collect();
        let verdicts = verdict_summaries(env, &ids).await?;
        Ok(json(similar_body(&id, &cc, &matches, &verdicts)?, 200))
    }

    /// 원본 `handleGraph(request, env)`
    pub async fn handle(req: &Req, env: &Env) -> R<Option<Resp>> {
        // 들어온 요청의 주소는 늘 올바르다 (런타임이 만든 것)
        let Ok(url) = url::Url::parse(&req.url) else { return Ok(None) };
        let path = url.path();
        if !GRAPH_PATHS.contains(&path) {
            return Ok(None);
        }
        if req.method != "GET" {
            return Ok(Some(json(obj! { "error" => "method not allowed" }, 405)));
        }

        if path == "/graph/view" {
            let mut r = Resp::new(200, Body::Text(super::view::VIEW_HTML.to_string()));
            r.set("Content-Type", "text/html; charset=utf-8");
            return Ok(Some(r));
        }

        // /graph, /graph/local은 D1 조회, /graph/similar는 Vectorize 조회가 따른다
        if is_rate_limited(req, &env.get("EVAL_READ_LIMITER")).await {
            let retry = RETRY_AFTER_SECONDS.to_string();
            let headers = hv(&[("Content-Type", "application/json; charset=utf-8"), ("Cache-Control", "no-store"), ("Retry-After", retry.as_str())]);
            return Ok(Some(json_response(&obj! { "error" => "rate limit exceeded" }, 429, headers)));
        }

        let r = match path {
            "/graph" => domain_graph(&url, env).await,
            "/graph/local" => local_graph(&url, env).await,
            _ => similar_graph(&url, env).await,
        };
        Ok(Some(match r {
            Ok(resp) => resp,
            Err(err) if is_validation(&err) => json(obj! { "error" => err.message }, 400),
            Err(err) => {
                console_log!("graph error", err);
                json(obj! { "error" => "internal error" }, 500)
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use premium_discount_worker_common::{arr, stringify_str};

    fn row(id: &str, from: &str, r#ref: Option<&str>, root: &str, created_at: f64, tier: Option<&str>, grade: Option<f64>) -> V {
        obj! {
            "id" => id, "from" => from, "cc" => "example.com", "ref" => r#ref.map_or(V::Null, V::from), "root" => root,
            "created_at" => created_at, "tier" => tier.map_or(V::Null, V::from), "grade" => grade.map_or(V::Null, V::from),
        }
    }

    #[test]
    fn view_html_is_the_original_template_value() {
        // 원본 템플릿 리터럴의 값: LF 줄 끝 10437 바이트, 인라인 스크립트 하나에 로컬 원문 오버레이 (graph.test.ts 의 단언)
        // (core.autocrlf 로 view.rs 가 CRLF 로 풀려도 rustc 가 원시 문자열의 CRLF 를 LF 로 읽는다)
        let html = view::VIEW_HTML;
        assert_eq!(html.len(), 10437);
        assert!(!html.contains('\r'));
        assert_eq!(html.matches("<script>").count(), 1);
        assert!(html.contains("'PremiumDiscountLocal'") && html.contains("textContent") && !html.contains("innerHTML"));
    }

    #[test]
    fn top_k_is_an_integer_between_1_and_49() {
        assert_eq!(parse_top_k(&V::Null).unwrap(), 10.0);
        for (raw, n) in [("1", 1.0), ("49", 49.0), (" 7 ", 7.0), ("5.0", 5.0), ("0x10", 16.0), ("1e1", 10.0), ("+3", 3.0)] {
            assert_eq!(parse_top_k(&V::from(raw)).unwrap(), n, "{raw}");
        }
        for raw in ["", "0", "50", "1.5", "-1", "abc", "Infinity", "NaN"] {
            let e = parse_top_k(&V::from(raw)).unwrap_err();
            assert!(crate::util::is_validation(&e), "{raw}");
            assert_eq!(e.message, "topK must be an integer between 1 and 49");
        }
    }

    #[test]
    fn search_params_decode_like_url_search_params() {
        let u = url::Url::parse("https://premium.discount/graph?cc=Ex%61mple.com&cc=second&topK=+5&x").unwrap();
        assert_eq!(search_param(&u, "cc").as_str(), Some("Example.com"));
        assert_eq!(search_param(&u, "topK").as_str(), Some(" 5"));
        assert_eq!(search_param(&u, "x").as_str(), Some(""));
        assert!(search_param(&u, "id").is_null());
    }

    #[test]
    fn graph_keeps_map_order_and_reply_edges_inside_the_response() {
        let rows = vec![
            row("r0000000-0", "0xA", None, "r0000000-0", 1.0, None, None),
            row("a0000000-1", "0xa", Some("r0000000-0"), "r0000000-0", 2.0, Some("premium"), Some(2.0)),
            row("b0000000-2", "0xb", Some("missing"), "missing", 3.0, Some("discount"), Some(-1.0)),
        ];
        let keys = vec!["aaaaaaaaaaaa".to_string(), "aaaaaaaaaaaa".to_string(), "bbbbbbbbbbbb".to_string()];
        let body = build_graph(&rows, &keys).unwrap();
        let text = stringify_str(&body);
        assert!(text.starts_with(r#"{"nodes":[{"id":"r0000000-0","type":"evaluation","label":"r0000000","cc":"example.com","root":"r0000000-0","created_at":1,"tier":"unrated","grade":null},{"id":"domain:example.com","type":"domain","label":"example.com"},{"id":"author:aaaaaaaaaaaa","type":"author","label":"작성자 aaaaaa"},"#), "{text}");
        assert_eq!(body.get("nodes").len(), 6);
        let edges = body.get("edges").items();
        assert_eq!(edges.len(), 7, "belongs · authored × 3 + reply 1 (부모가 응답 밖이면 없음)");
        assert_eq!(stringify_str(&edges[4]), r#"{"source":"a0000000-1","target":"r0000000-0","type":"reply"}"#);
        assert_eq!(stringify_str(&body.get("summary")), r#"{"premium":1,"neutral":0,"discount":1,"unrated":1}"#);
        assert_eq!(stringify_str(&build_graph(&[], &[]).unwrap()), r#"{"nodes":[],"edges":[],"summary":{"premium":0,"neutral":0,"discount":0,"unrated":0}}"#);
    }

    #[test]
    fn summary_keeps_the_original_increment_on_odd_tiers() {
        let n = |t: &str| obj! { "type" => "evaluation", "tier" => t };
        let nodes = vec![n("gold"), n("premium"), n("__proto__"), n("constructor"), n("1"), n("gold"), obj! { "type" => "domain", "tier" => "premium" }];
        assert_eq!(stringify_str(&summarize(&nodes)), r#"{"1":null,"premium":1,"neutral":0,"discount":0,"unrated":0,"gold":null,"constructor":null}"#);
    }

    #[test]
    fn author_keys_need_the_secret_only_when_there_are_rows() {
        assert!(author_keys(&[], &V::Undef).unwrap().is_empty());
        let rows = vec![obj! { "from" => "0xABC" }];
        assert_eq!(author_keys(&rows, &V::from("k")).unwrap(), vec!["95ebc8f2605c".to_string()]);
        for secret in [V::Undef, V::from("")] {
            assert_eq!(author_keys(&rows, &secret).unwrap_err().to_string(), "Error: AUTHOR_KEY_SECRET is not configured");
        }
        assert_eq!(author_keys(&[obj! { "from" => V::Null }], &V::from("k")).unwrap_err().to_string(), "TypeError: Cannot read properties of null (reading 'toLowerCase')");
    }

    #[test]
    fn vector_shapes_follow_destructuring_and_array_from() {
        assert!(first_of(&arr![]).unwrap().is_undef());
        assert_eq!(first_of(&V::Null).unwrap_err().to_string(), "TypeError: (intermediate value) is not iterable");
        assert_eq!(first_of(&obj! {}).unwrap_err().to_string(), "TypeError: (intermediate value) is not iterable");
        assert!(vector_values(&V::Undef).is_none());
        assert!(vector_values(&obj! { "id" => "x" }).is_none());
        assert!(vector_values(&obj! { "values" => V::Null }).is_none());
        assert_eq!(stringify_str(&vector_values(&obj! { "values" => arr![] }).unwrap()), "[]", "빈 배열은 참");
        assert_eq!(stringify_str(&vector_values(&arr![1.0]).unwrap()), "[]", "배열의 values 는 함수 → Array.from 은 빈 배열");
        assert!(vector_values(&V::from("a")).is_none());
        assert_eq!(stringify_str(&array_from(&arr![0.5, -0.25])), "[0.5,-0.25]");
        assert_eq!(stringify_str(&array_from(&V::from("a😀"))), r#"["a","😀"]"#);
        assert_eq!(stringify_str(&array_from(&obj! { "length" => 2.0, "0" => 1.0 })), "[1,null]");
        assert_eq!(stringify_str(&array_from(&V::Num(3.0))), "[]");
    }

    #[test]
    fn similar_matches_skip_self_and_keep_top_k() {
        let m = |id: &str, s: f64| obj! { "id" => id, "score" => s };
        let res = obj! { "matches" => vec![m("a", 1.0), m("b", 0.9), m("c", 0.8), m("d", 0.7)] };
        let out = other_matches(&res, "a", 2.0).unwrap();
        assert_eq!(out.iter().map(|x| x.get("id").to_str()).collect::<Vec<_>>(), ["b", "c"]);
        assert_eq!(other_matches(&V::Undef, "a", 1.0).unwrap_err().to_string(), "TypeError: Cannot read properties of undefined (reading 'matches')");
        assert_eq!(other_matches(&obj! {}, "a", 1.0).unwrap_err().to_string(), "TypeError: Cannot read properties of undefined (reading 'filter')");
        assert_eq!(other_matches(&obj! { "matches" => 5.0 }, "a", 1.0).unwrap_err().to_string(), "TypeError: res.matches.filter is not a function");
        assert_eq!(other_matches(&obj! { "matches" => vec![V::Null] }, "a", 1.0).unwrap_err().to_string(), "TypeError: Cannot read properties of null (reading 'id')");

        let mut verdicts = IndexMap::new();
        verdicts.insert("b".to_string(), obj! { "tier" => "premium", "grade" => 1.0, "rank" => 0.5, "evidence" => 3.0 });
        verdicts.insert("c".to_string(), obj! { "tier" => "unrated", "grade" => V::Null, "rank" => V::Null, "evidence" => 0.0 });
        let body = similar_body("a", &V::from("example.com"), &[m("b", 0.9), obj! { "id" => "c" }, m("e", 0.1)], &verdicts).unwrap();
        assert_eq!(
            stringify_str(&body),
            concat!(
                r#"{"nodes":[{"id":"b","type":"evaluation","label":"b","cc":"example.com","tier":"premium","grade":1},"#,
                r#"{"id":"c","type":"evaluation","label":"c","cc":"example.com","tier":"unrated","grade":null},"#,
                r#"{"id":"e","type":"evaluation","label":"e","cc":"example.com","tier":"unrated","grade":null}],"#,
                r#""edges":[{"source":"a","target":"b","type":"similar","score":0.9},{"source":"a","target":"c","type":"similar"},{"source":"a","target":"e","type":"similar","score":0.1}],"#,
                r#""summary":{"premium":1,"neutral":0,"discount":0,"unrated":2}}"#
            )
        );
    }
}
