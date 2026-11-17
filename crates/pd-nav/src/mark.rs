//! 결정 기록 (`navmark/1`): 해시 · 벡터 · 종류(premium / discount)
//!
//! SDS 가 결정한 에피소드만 기록이 된다([`crate::FrictionBaselines::decide`]). 기록에는 계정 · 기기 식별자 · IP ·
//! 원시 이벤트 · 입력값 · 화면 글자 · URL 원문 · 시각이 없고, 날짜만 일 단위로 남는다.
//!
//! 기록은 기기에 쌓는다(로그인 없이, 게스트도). 기기 안의 판정(`to_obs` → PD-Verdict v2 `implicit`)과 번호 순서에 쓴다.
//! 서버로 올리는 단위는 이 기록이 아니라 타임트라이얼의 런([`crate::Run`], `trial/1`)이다: 런은 결승까지의 체크포인트
//! 해시와 구간 시간, 그 에피소드의 종류를 담고, 체크포인트마다 이 기록과 같은 벡터([`MarkVector`])를 붙일 수 있다.

use std::fmt;

use pd_verdict::{Obs, Params, IMPLICIT, RECIPE_V2};
use serde::{Deserialize, Serialize};

use crate::privacy::scrub_name;
use crate::region::Region;

pub const MARK_SCHEMA: &str = "navmark/1";

/// 라벨 글 벡터의 공간: granite-embedding-97m-multilingual-r2 의 CLS (raon-zena `zena-nlp` 의 SPACE_TAG 와 같다).
/// 두 앱이 같은 모델이라 서버가 한 색인으로 함께 찾는다
pub const LABEL_SPACE: &str = "granite-97m-r2-cls-v1";

const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkType {
    Premium,
    Discount,
}

impl MarkType {
    pub fn sign(self) -> i32 {
        match self {
            MarkType::Premium => 1,
            MarkType::Discount => -1,
        }
    }
}

/// 공간 이름을 붙인 벡터 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkVector {
    /// 벡터 공간 이름. 공간이 다른 벡터끼리는 견주지 않는다
    pub space: String,
    /// L2 정규화
    pub vector: Vec<f32>,
}

/// 결정 기록 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavMark {
    pub schema: String,
    /// 화면 또는 영역의 `hashId` ([`crate::screen_hash`], [`crate::region_hash`])
    pub hash: String,
    /// 라벨 글([`region_label`] · [`screen_label`])의 granite 벡터([`LABEL_SPACE`], 두 앱 공용)와, 있으면 영역 조각의
    /// 비전 벡터(premium-discount 는 SigLIP2, raon-zena 는 Raon-VE: 앱마다 공간이 다르다)
    pub vectors: Vec<MarkVector>,
    #[serde(rename = "type")]
    pub kind: MarkType,
    /// UTC 날짜 (1970-01-01 부터 센 날). 시각은 남기지 않는다
    pub day: i64,
    /// 이 기록을 판정에 넣는 규칙 (`pd-verdict/2`)
    pub recipe: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkError {
    /// `0x` + hex 40자가 아니다
    Hash,
    /// 벡터가 없거나, 비었거나, 유한하지 않거나, 길이가 0 이다
    Vector,
    /// 공간 이름이 비었거나 겹친다
    Space,
}

impl fmt::Display for MarkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MarkError::Hash => "hash must be an address (0x + 40 hex)",
            MarkError::Vector => "every vector must be non-empty, finite and non-zero, and there must be one at least",
            MarkError::Space => "spaces must be named and distinct",
        })
    }
}

impl std::error::Error for MarkError {}

pub(crate) fn normalized(vector: &[f32]) -> Option<Vec<f32>> {
    let norm = vector.iter().map(|v| *v as f64 * *v as f64).sum::<f64>().sqrt();
    if vector.is_empty() || !vector.iter().all(|v| v.is_finite()) || norm == 0.0 {
        return None;
    }
    Some(vector.iter().map(|v| (*v as f64 / norm) as f32).collect())
}

impl NavMark {
    /// 기록을 만든다. `vectors` 는 (공간 이름, 벡터) 들이다. 해시는 소문자로 고치고, 벡터는 L2 정규화해 담고,
    /// 시각은 UTC 날짜로 내린다
    pub fn new(hash: &str, kind: MarkType, at_ms: i64, vectors: &[(&str, &[f32])]) -> Result<NavMark, MarkError> {
        let hash = hash.trim().to_ascii_lowercase();
        if !(hash.len() == 42 && hash.starts_with("0x") && hash[2..].chars().all(|c| c.is_ascii_hexdigit())) {
            return Err(MarkError::Hash);
        }
        if vectors.is_empty() {
            return Err(MarkError::Vector);
        }
        let mut out: Vec<MarkVector> = Vec::with_capacity(vectors.len());
        for (space, vector) in vectors {
            let space = space.trim();
            if space.is_empty() || out.iter().any(|v| v.space == space) {
                return Err(MarkError::Space);
            }
            out.push(MarkVector {
                space: space.to_string(),
                vector: normalized(vector).ok_or(MarkError::Vector)?,
            });
        }
        Ok(NavMark {
            schema: MARK_SCHEMA.to_string(),
            hash,
            vectors: out,
            kind,
            day: at_ms.div_euclid(DAY_MS),
            recipe: RECIPE_V2.to_string(),
        })
    }

    /// 공간 이름으로 벡터 찾기
    pub fn vector(&self, space: &str) -> Option<&[f32]> {
        self.vectors.iter().find(|v| v.space == space).map(|v| v.vector.as_slice())
    }

    /// 서버가 판정에 넣는 관측: 종류의 부호, `implicit` 가중치, 그날 정오(시각은 모른다)
    pub fn to_obs(&self, params: &Params) -> Obs {
        Obs {
            kind: IMPLICIT.to_string(),
            sign: self.kind.sign(),
            weight: params.weight(IMPLICIT),
            credit: 1.0,
            at_ms: self.day * DAY_MS + DAY_MS / 2,
        }
    }
}

/// 영역 기록의 라벨 벡터가 될 글: 종류 + 개인정보를 거른 이름 (이름이 없으면 종류만). 위치 단어는 넣지 않는다
pub fn region_label(region: &Region) -> String {
    let name = scrub_name(&region.name);
    if name.is_empty() {
        region.role.label().to_string()
    } else {
        format!("{} {}", region.role.label(), name)
    }
}

/// 화면 기록의 라벨 벡터가 될 글: 앱 · 사이트와 화면 키의 틀
pub fn screen_label(cc: &str, template: &str) -> String {
    format!("{} {}", cc.trim().to_ascii_lowercase(), template.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::{BehaviorParams, Episode, Event, FrictionBaselines, Signal};
    use crate::hash::{screen_hash, url_template};
    use crate::testing::region;
    use crate::{Role, Source};
    use pd_verdict::{verdict, Baseline, Baselines, Case, Prev, Tier, NAV};
    use serde_json::Value;

    const HASH: &str = "0xA0328CD123A2B578BA1A6A211A29AF590FEC75C7";

    #[test]
    fn a_mark_is_hash_vectors_and_type_only() {
        let m = NavMark::new(
            HASH,
            MarkType::Premium,
            1_763_251_200_000 + 3_600_000,
            &[(LABEL_SPACE, &[3.0, 4.0]), ("siglip2-so400m-p16-naflex", &[0.0, 2.0, 0.0])],
        )
        .unwrap();
        assert_eq!(m.hash, HASH.to_ascii_lowercase());
        assert_eq!(m.vector(LABEL_SPACE), Some(&[0.6f32, 0.8][..]));
        assert_eq!(m.vector("siglip2-so400m-p16-naflex"), Some(&[0.0f32, 1.0, 0.0][..]));
        assert_eq!(m.day, 20_408);
        let v: Value = serde_json::to_value(&m).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["day", "hash", "recipe", "schema", "type", "vectors"]);
        assert_eq!(v["type"], "premium");
        assert_eq!(v["vectors"][0]["space"], LABEL_SPACE);
        assert_eq!(v["recipe"], "pd-verdict/2");
    }

    #[test]
    fn bad_marks_are_refused() {
        let ok: &[f32] = &[1.0];
        assert_eq!(NavMark::new("0x1234", MarkType::Discount, 0, &[("s", ok)]), Err(MarkError::Hash));
        assert_eq!(NavMark::new(HASH, MarkType::Discount, 0, &[]), Err(MarkError::Vector));
        assert_eq!(NavMark::new(HASH, MarkType::Discount, 0, &[("s", &[])]), Err(MarkError::Vector));
        assert_eq!(NavMark::new(HASH, MarkType::Discount, 0, &[("s", &[0.0, 0.0])]), Err(MarkError::Vector));
        assert_eq!(NavMark::new(HASH, MarkType::Discount, 0, &[("s", &[f32::NAN])]), Err(MarkError::Vector));
        assert_eq!(NavMark::new(HASH, MarkType::Discount, 0, &[(" ", ok)]), Err(MarkError::Space));
        assert_eq!(NavMark::new(HASH, MarkType::Discount, 0, &[("s", ok), ("s", ok)]), Err(MarkError::Space));
    }

    #[test]
    fn labels_carry_no_private_words() {
        let r = region("main>button", Role::Button, "홍길동 hong@example.com 송금", (0.1, 0.1, 0.1, 0.1), Source::Tree);
        assert_eq!(region_label(&r), "버튼 홍길동 송금");
        let unnamed = region("cell:1,1", Role::Image, "", (0.1, 0.1, 0.1, 0.1), Source::Vision);
        assert_eq!(region_label(&unnamed), "그림");
        assert_eq!(screen_label("Example.com", "example.com/cart/:id"), "example.com example.com/cart/:id");
    }

    /// 행동 기록 → SDS 결정 → 기록(해시 · 벡터 · 종류) → 서버 판정까지 한 줄로
    #[test]
    fn decided_episodes_become_marks_and_marks_become_a_verdict() {
        let p = BehaviorParams::default();
        let mut baselines = FrictionBaselines::default();
        for i in 0..12 {
            let rough = Episode {
                events: vec![
                    Event { at_ms: 0, signal: Signal::Retried, anchor: None },
                    Event { at_ms: 1000, signal: if i % 2 == 0 { Signal::Backtracked } else { Signal::Hovered }, anchor: None },
                ],
            };
            baselines.decide(&format!("app{i}.example"), &format!("0xother{i}"), None, &rough, &p);
        }
        let screen = screen_hash("shop.example", &url_template("https://shop.example/cart/991234").unwrap()).unwrap();
        let smooth = Episode {
            events: vec![
                Event { at_ms: 0, signal: Signal::Picked, anchor: Some("main>button#pay".into()) },
                Event { at_ms: 900, signal: Signal::Submitted, anchor: Some("main>button#pay".into()) },
            ],
        };
        // 열흘 동안 하루 한 번: 감쇠(반감기 14일)를 거쳐 근거 E = 2.37. 이레치는 1.78 로 min_evidence(2.0)에 못 미친다
        let mut marks = Vec::new();
        let day0 = 1_763_251_200_000_i64;
        let label: &[f32] = &[0.1, 0.2, 0.3];
        for d in 0..10 {
            let decision = baselines.decide("shop.example", &screen, None, &smooth, &p);
            let kind = decision.screen.mark.expect("마찰이 다른 화면보다 낮은 성공 에피소드는 premium");
            marks.push(NavMark::new(&screen, kind, day0 + d * DAY_MS, &[(LABEL_SPACE, label)]).unwrap());
        }
        assert!(marks.iter().all(|m| m.kind == MarkType::Premium && m.hash == screen));

        // 서버: 같은 해시의 기록 10개 → implicit 관측 10개 → nav 트랙 판정
        let params = Params::v2();
        let case = Case {
            now_ms: day0 + 10 * DAY_MS,
            track: NAV.to_string(),
            prev: Prev { tier: Tier::Unrated, since_ms: 0 },
            obs: marks.iter().map(|m| m.to_obs(&params)).collect(),
            baseline: Baselines {
                secondary: None,
                primary: Some(Baseline { n: 6, mean: 0.0, sd: 0.2 }),
                global: None,
            },
            duel: None,
            superseded: false,
        };
        let v = verdict(&case, &params).unwrap();
        assert_eq!(v.tier, Tier::Premium, "{v:?}");
        // 하루치(1개)만으로는 근거가 모자라 판정하지 않는다
        let one = Case { obs: case.obs[..1].to_vec(), ..case.clone() };
        assert_eq!(verdict(&one, &params).unwrap().tier, Tier::Unrated);
    }
}
