//! 원본 `moderation.ts`: Alibaba Cloud Model Studio Decision Model API 로 텍스트 유해성을 판정한다.
//!
//! 요청: POST `env.DECISION_ENDPOINT`, `Authorization: Bearer <DASHSCOPE_API_KEY>`, 본문
//! `{"model":"decision-model-preview","state":<글>,"questions":{"profanity":{"type":"noul","instructions":…},…}}`.
//! 응답 `answers.<이름>.noul`(P(yes)) 중 하나라도 0.5 이상이면 profane. 모양이 다르거나 답이 빠지면 던진다(fail-closed).

use premium_discount_worker_common::jsv::{Throw, R, V};

pub const DECISION_MODEL: &str = "decision-model-preview";
pub const THRESHOLD: f64 = 0.5;

/// `QUESTIONS` — 이름(거부 사유로 그대로 쓰인다) · 질문. 순서가 `Object.keys(QUESTIONS)` 다
pub const QUESTIONS: [(&str, &str); 3] = [
    ("profanity", "이 텍스트에 욕설이나 모욕적인 비속어가 포함되어 있는가? 포함되어 있으면 yes."),
    (
        "nsfw",
        "이 텍스트에 성적으로 노골적이거나 음란한 표현, 또는 폭력을 노골적으로 묘사하거나 조장하는 표현이 포함되어 있는가? 포함되어 있으면 yes.",
    ),
    (
        "illegal",
        "이 텍스트가 불법 촬영물, 범죄나 자해를 조장하는 내용, 또는 타인의 개인정보(전화번호·주소·주민등록번호 등) 노출을 포함하는가? 포함되어 있으면 yes.",
    ),
];

/// `Moderation` — `verdict` 는 flagged 가 비었는지로 정해진다
#[derive(Debug, Clone, PartialEq)]
pub struct Moderation {
    pub flagged: Vec<&'static str>,
}

impl Moderation {
    /// `verdict === 'profane'`
    pub fn profane(&self) -> bool {
        !self.flagged.is_empty()
    }
}

fn shape_error() -> Throw {
    Throw::error("unrecognized decision response shape")
}

/// `parseModeration(body)`
pub fn parse_moderation(body: &V) -> R<Moderation> {
    // (body as …)?.answers — 원시값의 속성도 undefined 다
    let answers = if body.is_nullish() { V::Undef } else { body.get("answers") };
    if !answers.truthy() || answers.type_of() != "object" {
        return Err(shape_error());
    }
    let mut flagged = Vec::new();
    for (c, _) in QUESTIONS {
        let answer = answers.get(c);
        if !answer.truthy() {
            return Err(shape_error());
        }
        let noul = answer.get("noul");
        let ok_type = matches!(answer.get("type"), V::Str(ref t) if t == "noul");
        let finite = matches!(noul, V::Num(n) if n.is_finite());
        if !ok_type || !finite {
            return Err(shape_error());
        }
        if noul.to_num() >= THRESHOLD {
            flagged.push(c);
        }
    }
    Ok(Moderation { flagged })
}

/// 요청 본문 `JSON.stringify({ model, state: text, questions })`
pub fn request_body(text: &V) -> String {
    let questions = V::new_obj();
    for (c, q) in QUESTIONS {
        let item = V::new_obj();
        item.put("type", V::from("noul"));
        item.put("instructions", V::from(q));
        questions.put(c, item);
    }
    let body = V::new_obj();
    body.put("model", V::from(DECISION_MODEL));
    body.put("state", text.clone());
    body.put("questions", questions);
    premium_discount_worker_common::stringify_str(&body)
}

/// `moderate(env, text)` — fetch 는 전역 `fetch` 를 원본과 같은 인자로 부른다(주소 · 키가 없으면 undefined 그대로)
#[cfg(target_arch = "wasm32")]
pub async fn moderate(env: &premium_discount_worker_common::js::Env, text: &V) -> R<Moderation> {
    use js_sys::{Function, Object, Reflect};
    use premium_discount_worker_common::js::{self, to_js, to_throw};
    use premium_discount_worker_common::jsv::num_to_string;
    use wasm_bindgen::{JsCast, JsValue};

    let endpoint = env.get("DECISION_ENDPOINT");
    let init = Object::new();
    let _ = Reflect::set(&init, &"method".into(), &"POST".into());
    let headers = Object::new();
    let _ = Reflect::set(&headers, &"Content-Type".into(), &"application/json".into());
    let auth = format!("Bearer {}", env.get("DASHSCOPE_API_KEY").to_str());
    let _ = Reflect::set(&headers, &"Authorization".into(), &JsValue::from_str(&auth));
    let _ = Reflect::set(&init, &"headers".into(), &headers);
    let _ = Reflect::set(&init, &"body".into(), &JsValue::from_str(&request_body(text)));

    let global = js_sys::global();
    let f: Function = Reflect::get(&global, &"fetch".into()).map_err(to_throw)?.unchecked_into();
    let p = f.call2(&global, &to_js(&endpoint), &init).map_err(to_throw)?;
    let res = js::wait(p).await?;
    let ok = Reflect::get(&res, &"ok".into()).map_err(to_throw)?;
    if !js::to_v(&ok).truthy() {
        // 응답 본문에는 원문이 들어가지 않으므로 상태 코드만 남긴다
        let status = js::to_v(&Reflect::get(&res, &"status".into()).map_err(to_throw)?);
        let shown = match status {
            V::Num(n) => num_to_string(n),
            other => other.to_str(),
        };
        return Err(Throw::error(format!("decision api responded {shown}")));
    }
    let body = js::response_json(&res).await?;
    parse_moderation(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use premium_discount_worker_common::jsv::{from_json, parse_str};
    use serde_json::json;

    fn answers(p: serde_json::Value) -> V {
        let mut out = serde_json::Map::new();
        for k in ["profanity", "nsfw", "illegal"] {
            out.insert(k.into(), json!({ "type": "noul", "noul": p.get(k).cloned().unwrap_or(json!(0)) }));
        }
        from_json(json!({ "answers": out }))
    }

    #[test]
    fn clean_below_the_threshold() {
        assert_eq!(parse_moderation(&answers(json!({}))).unwrap(), Moderation { flagged: vec![] });
        assert!(!parse_moderation(&answers(json!({ "profanity": THRESHOLD - 0.001 }))).unwrap().profane());
    }

    #[test]
    fn profane_at_or_above_the_threshold_with_every_flagged_question() {
        assert_eq!(parse_moderation(&answers(json!({ "nsfw": THRESHOLD }))).unwrap().flagged, vec!["nsfw"]);
        assert_eq!(parse_moderation(&answers(json!({ "illegal": 0.9 }))).unwrap().flagged, vec!["illegal"]);
        assert_eq!(parse_moderation(&answers(json!({ "profanity": 0.6, "illegal": 0.7 }))).unwrap().flagged, vec!["profanity", "illegal"]);
    }

    #[test]
    fn a_missing_answer_is_an_error() {
        for missing in ["profanity", "nsfw", "illegal"] {
            let body = answers(json!({}));
            body.get("answers").del(missing).unwrap();
            let err = parse_moderation(&body).unwrap_err();
            assert!(err.message.contains("unrecognized"), "{missing}");
        }
    }

    #[test]
    fn other_shapes_are_errors() {
        for bad in ["null", "{}", r#"{"answers":null}"#, r#"{"answers":"x"}"#, r#"{"answers":{"profanity":{"type":"noul","noul":"high"}}}"#, "5", r#""s""#, "[]"] {
            assert!(parse_moderation(&parse_str(bad).unwrap()).is_err(), "{bad}");
        }
        let nan = answers(json!({}));
        nan.get("answers").get("nsfw").put("noul", V::Num(f64::NAN));
        assert!(parse_moderation(&nan).is_err());
        let wrong = answers(json!({}));
        wrong.get("answers").get("illegal").put("type", V::from("text"));
        assert!(parse_moderation(&wrong).is_err());
    }

    #[test]
    fn one_request_carries_all_three_questions_in_key_order() {
        let body = request_body(&V::from("안녕하세요"));
        assert!(body.starts_with(r#"{"model":"decision-model-preview","state":"안녕하세요","questions":{"profanity":{"type":"noul","instructions":"#));
        let v = parse_str(&body).unwrap();
        assert_eq!(v.get("questions").keys(), vec!["profanity", "nsfw", "illegal"]);
    }
}
