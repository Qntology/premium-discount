//! 원본 `index.ts` 405~1128 — 모듈 처리기 다섯이 모두 null 을 돌려준 요청을 받는 레거시 처리기(commerce 꼴).
//!
//! ## 실제로 실행되는 곳
//!
//! 이 처리기는 **어떤 요청에도 418 "I'm a teapot!"** 으로 끝난다. 원본을 차례대로 따라가면:
//!
//! - 405~463 (try 밖): `request.cf` · IP · 국가 코드. 국가별 DB 매핑(`regionBindings`)은 비어 있다.
//! - 465~ (try 안, 던지면 `console.log('err', err)` 뒤 418):
//!   1. `decodeURIComponent(req.query.origin)` — 잘못된 % 이면 URIError
//!   2. `console.log('req.query.referrer', req.query.referrer)` — 모든 요청이 남긴다
//!   3. `decodeURIComponent(req.query.href).toLowerCase()` — URIError, 이어서 `new URL(href)` — 비었거나 틀리면 TypeError
//!   4. `headers.set('Content-Security-Policy', \`frame-ancestors 'self' ${origin};\`)` — origin 이 머리글 값으로 틀리면(줄바꿈 등) TypeError
//!   5. 572 `ethers.isAddress(req.query.id) ? … : hashId(cookies.cc + pathname)` — `cookies` 는 어디에도 선언되지 않아
//!      id 가 주소가 아니면 ReferenceError("cookies is not defined")
//!   6. 630 `env[\`premium_dicount_${zoneRegion}_items\`].prepare(\`…${cookies.team}…\`)` — zoneRegion 이 '' 라 바인딩 이름은
//!      `premium_dicount__items` 이고 어느 wrangler 설정에도 없다 → TypeError("Cannot read properties of undefined (reading 'prepare')").
//!      바인딩이 있었더라도 인자의 `${cookies.team}` 이 ReferenceError 로 던진다.
//!
//! 그래서 631~1120(페이지 · 작업 트리, PUT · POST(update_from_rust · 링크 등록) · GET(목록 · renderHtml) · OPTIONS)은
//! **어떤 바인딩 · 요청으로도 실행될 수 없다**. 그 부분은 옮기지 않았다(옮겨도 시험할 길이 없다). 위의 1 ~ 6 은 같은 순서 · 같은 예외 ·
//! 같은 console 기록으로 옮겼다. 계산만 하고 던질 수 없는 줄(Accept-Language · IP 정규식 · cc · created_at · home 객체와 gzip)은
//! 결과가 630 뒤에서만 쓰이므로 건너뛰고 줄 번호를 적어 두었다.
//!
//! 머리글 검사(4)와 주소 해석(3)은 원본과 같은 런타임 객체(`Headers` · `URL`)를 불러 같은 오류 글을 낸다.

#[cfg(target_arch = "wasm32")]
pub use wasm::handle;

/// 418 응답 (1125~1128)
pub fn teapot() -> premium_discount_worker_common::web::Resp {
    use premium_discount_worker_common::web::{Body, Resp};
    let mut r = Resp::new(418, Body::Text("I'm a teapot!".into()));
    r.set("Content-Type", "text/html; charset=utf-8");
    r
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use js_sys::{Array, Function, Reflect};
    use premium_discount_worker_common::eth::is_address;
    use premium_discount_worker_common::js::{self, to_throw, Env, Js};
    use premium_discount_worker_common::jsv::{to_lower, Throw, R, V};
    use premium_discount_worker_common::web::{Req, Resp};
    use premium_discount_worker_common::console_log;
    use wasm_bindgen::{JsCast, JsValue};

    use super::teapot;

    /// `new <이름>(...args)` — 전역 생성자 (Headers · URL)
    fn construct(name: &str, args: &[JsValue]) -> R<JsValue> {
        let ctor: Function = Reflect::get(&js_sys::global(), &JsValue::from_str(name)).map_err(to_throw)?.unchecked_into();
        let list = Array::new();
        for a in args {
            list.push(a);
        }
        Reflect::construct(&ctor, &list).map_err(to_throw)
    }

    /// `decodeURIComponent(s)` — 런타임 것을 부른다. 실패는 URIError (to_throw 는 URIError 이름을 모르므로 여기서 붙인다)
    fn decode_uri_component(s: &str) -> R<String> {
        match js_sys::decode_uri_component(s) {
            Ok(v) => Ok(String::from(v)),
            Err(e) => {
                let message = Reflect::get(&e, &"message".into()).ok().and_then(|m| m.as_string()).unwrap_or_else(|| "URI malformed".into());
                Err(Throw { name: "URIError", message })
            }
        }
    }

    /// `req.query` — `queryParams.forEach((value, key) => { req.query[key] = value })`: 같은 이름은 뒤의 값,
    /// `__proto__` 에 글을 넣는 것은 아무 일도 하지 않는다(객체의 프로토타입 설정자). 읽는 이름(origin · referrer · href · id)은
    /// Object.prototype 에 없으므로 자기 속성만 본다
    fn query(url: &url::Url) -> V {
        let q = V::new_obj();
        for (k, v) in url.query_pairs() {
            if k != "__proto__" {
                q.put(&k, V::from(v.into_owned()));
            }
        }
        q
    }

    /// `x ? x : ""` 의 글
    fn or_empty(v: &V) -> String {
        if v.truthy() {
            v.to_str()
        } else {
            String::new()
        }
    }

    /// 405~1128 — 늘 418 로 끝난다 (위 설명). try 밖에서 던지면 fetch 밖으로 샌다(Err)
    pub async fn handle(req: &Req, env: &Env) -> R<Resp> {
        // 405~428: var { country, … } = request.cf || {}
        let cf = if req.cf.truthy() { req.cf.clone() } else { V::new_obj() };
        let country = cf.g("country")?;
        // 431~435: 요청자의 IP (cf-connecting-ip, 없으면 X-Real-IP) — 630 뒤에서만 쓴다
        // 453: var FLAG = (geoInfo.country || '').toLowerCase()
        if country.truthy() && !country.is_str() {
            return Err(Throw::type_error("(geoInfo.country || \"\").toLowerCase is not a function"));
        }
        // 456~463: regionBindings[FLAG] · languageCode[FLAG] — 630 뒤(작업 갈래)에서만 쓴다

        match body(req, env).await {
            Ok(()) => Ok(teapot()),
            Err(err) => {
                console_log!("err", err);
                Ok(teapot())
            }
        }
    }

    /// 465~1120 의 try 안. 630 에서 반드시 던진다 (돌려주는 Ok 는 없다 — 원본에서 그 뒤의 return 들은 실행될 수 없다)
    async fn body(req: &Req, env: &Env) -> R<()> {
        // 466: var headers = new Headers()
        let headers = V::ext(Js(construct("Headers", &[])?));
        // 472~489: new URL(request.url) · req.query
        let request_url = url::Url::parse(&req.url).map_err(|_| Throw::type_error("Invalid URL string."))?;
        let q = query(&request_url);
        // 491~505: Accept-Language (쓰지 않음)

        // 510: var origin = decodeURIComponent(req.query.origin ? req.query.origin : "")
        let origin = decode_uri_component(&or_empty(&q.get("origin")))?;
        // 512~520: User-Agent · IP 정규식 (쓰지 않음)

        // 522
        console_log!("req.query.referrer", q.get("referrer"));
        // 523: var href = decodeURIComponent(req.query.href ? req.query.href : "").toLowerCase()
        let href = to_lower(&decode_uri_component(&or_empty(&q.get("href")))?);
        // 525: var url = new URL(href)
        construct("URL", &[JsValue::from_str(&href)])?;
        // 527~548: cc (try 안에서 던질 수 없다. 630 뒤에서만 쓴다)
        // 551~553: now · current

        // 556~560
        let set = |name: &str, value: &str| js::call(&headers, "set", &[JsValue::from_str(name), JsValue::from_str(value)]).map(|_| ());
        set("Access-Control-Allow-Origin", "*")?;
        set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")?;
        set("Access-Control-Allow-Headers", "Content-Type, Content-Encoding, Authorization, X-Requested-With")?;
        set("Content-Security-Policy", &format!("frame-ancestors 'self' {origin};"))?;
        // 562~570: created_at · pathname · link

        // 572: var pageId = ethers.isAddress(req.query.id) ? req.query.id.toLowerCase() : hashId(cookies.cc+pathname)
        // 원본 그대로: `cookies` 는 선언되지 않은 이름이다
        if !is_address(&q.get("id")) {
            return Err(Throw::reference_error("cookies is not defined"));
        }
        // 581~628: homeFrom · homeId · home(gzip) · tasks … (던질 수 없다)

        // 630: env[`premium_dicount_${zoneRegion}_items`].prepare(`SELECT … "to" = '${cookies.team}' …`)
        let db = env.get("premium_dicount__items");
        if db.is_nullish() {
            return Err(Throw::type_error(format!("Cannot read properties of {} (reading 'prepare')", db.to_str())));
        }
        // 원본 그대로: 바인딩이 있어도 템플릿 인자의 `cookies` 에서 던진다
        Err(Throw::reference_error("cookies is not defined"))
    }
}
