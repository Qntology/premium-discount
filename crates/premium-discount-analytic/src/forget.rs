//! 지운 사건을 다시 들이지 않는 판단: 앱(`apps/core/src/analytic.rs`)의 TOMBSTONE · DELTA GUARD 에서 저장소와 상관없는 부분
//!
//! 묘비 테이블 하나(`id`, `deleted_at`)에 두 가지를 함께 둔다. 사건 하나를 지운 id 묘비, 그리고 사이트의 어느 시각까지를 모두 지운
//! 컷오프 묘비(`id = "cutoff:<host>"`, `deleted_at = 그 시각`. 여러 번 지우면 가장 늦은 시각)다. 테이블 모양이 그대로라 예전 기기의
//! 저장소를 옮기지 않고 읽는다. 서버에서 받은 행은 ① 그 사이트의 컷오프 이전(같은 시각 포함)이거나 ② id 묘비가 있으면 들이지
//! 않고, ③ 기기에서 구조화를 마친 행(`updated_at > 0`)은 원시 사건으로 덮어쓰지 않는다.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::blob;
use crate::event::WebEvent;

/// 컷오프 묘비 id 의 머리말
pub const CUTOFF_PREFIX: &str = "cutoff:";

/// 사이트의 컷오프 묘비 id (`cutoff:<소문자 호스트>`)
pub fn cutoff_id(host: &str) -> String {
    format!("{CUTOFF_PREFIX}{}", host.trim().to_ascii_lowercase())
}

/// 컷오프 묘비 id 면 그 호스트
pub fn cutoff_host(id: &str) -> Option<&str> {
    id.strip_prefix(CUTOFF_PREFIX).filter(|h| !h.is_empty())
}

/// 묘비 묶음 (id 묘비 + 사이트별 컷오프)
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TombstoneGuard {
    pub ids: HashSet<String>,
    /// 소문자 호스트 → 이 시각(epoch ms) 이하의 사건은 지웠다
    pub cutoffs: BTreeMap<String, i64>,
}

impl TombstoneGuard {
    /// 묘비 테이블의 행 (`id`, `deleted_at`)에서
    pub fn from_rows<'a>(rows: impl IntoIterator<Item = (&'a str, i64)>) -> TombstoneGuard {
        let mut out = TombstoneGuard::default();
        for (id, deleted_at) in rows {
            match cutoff_host(id) {
                Some(host) => {
                    out.forget_before(host, deleted_at);
                }
                None => {
                    out.forget_id(id);
                }
            }
        }
        out
    }

    /// 사건 하나를 지운다. 새 묘비면 true
    pub fn forget_id(&mut self, id: &str) -> bool {
        let id = id.trim();
        !id.is_empty() && self.ids.insert(id.to_string())
    }

    /// 사이트의 `before_ms` 이하를 모두 지운다. 컷오프는 늦은 쪽으로만 움직인다. 저장할 컷오프 시각(`deleted_at`)을 돌려준다
    pub fn forget_before(&mut self, host: &str, before_ms: i64) -> i64 {
        let at = self.cutoffs.entry(host.trim().to_ascii_lowercase()).or_insert(before_ms);
        *at = (*at).max(before_ms);
        *at
    }

    pub fn cutoff(&self, host: &str) -> Option<i64> {
        self.cutoffs.get(&host.trim().to_ascii_lowercase()).copied()
    }

    /// 이 행을 다시 들이면 안 되는가 (컷오프 이전이거나 id 묘비가 있다)
    pub fn blocks(&self, host: &str, id: &str, created_at: i64) -> bool {
        self.cutoff(host).is_some_and(|c| created_at <= c) || self.ids.contains(id.trim())
    }
}

/// 받은 행 하나를 어떻게 할지
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestDecision {
    /// 새로 넣거나, 아직 구조화하지 않은 원시 행을 바꾼다
    Insert,
    /// 이미 구조화한 행이라 그대로 둔다 (DELTA GUARD: 마찰 기준선 · 선택 대조에 두 번 들어가지 않게)
    KeptStructured,
    /// 지운 사건이라 들이지 않는다 (TOMBSTONE GUARD)
    BlockedTombstoned,
}

/// `existing_updated_at` 은 기기에 같은 id 의 행이 있으면 그 `updated_at` 이다
pub fn ingest_decision(guard: &TombstoneGuard, host: &str, id: &str, created_at: i64, existing_updated_at: Option<i64>) -> IngestDecision {
    if guard.blocks(host, id, created_at) {
        IngestDecision::BlockedTombstoned
    } else if existing_updated_at.is_some_and(|u| u > 0) {
        IngestDecision::KeptStructured
    } else {
        IngestDecision::Insert
    }
}

/// 서버 행의 `data` 를 기기에 넣을 사건으로: 풀고([`blob::decode`]), 모양을 맞추고, 다듬는다([`WebEvent::sanitized`]). `sid` 는
/// 비우고(행의 `session` 이 대신한다) 받은 시각을 `at_ms` 에 둔다. 풀 수 없거나 모양이 틀리면 None: 부르는 쪽은 `broken` 으로 세고
/// 다음 행으로 간다(묶음을 멈추지 않는다)
pub fn decode_row_event(data: &Value, created_at: i64) -> Option<WebEvent> {
    let mut event = serde_json::from_value::<WebEvent>(Value::Object(blob::decode(data))).ok()?.sanitized()?;
    event.sid = String::new();
    event.at_ms = created_at;
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutoffs_only_move_later_and_ids_are_trimmed() {
        let mut g = TombstoneGuard::from_rows([("e1", 10), ("cutoff:Shop.Example.com", 5_000), ("cutoff:shop.example.com", 3_000), ("cutoff:", 9)]);
        assert_eq!(g.cutoff("shop.example.com"), Some(5_000));
        assert!(g.ids.contains("e1") && g.ids.contains("cutoff:"), "a bare prefix is an ordinary id");
        assert_eq!(g.forget_before(" SHOP.example.com ", 4_000), 5_000);
        assert_eq!(g.forget_before("shop.example.com", 6_000), 6_000);
        assert!(g.blocks("shop.example.com", "x", 6_000) && !g.blocks("shop.example.com", "x", 6_001));
        assert!(!g.blocks("other.example.com", "x", 1), "cutoffs are per site");
        assert!(g.blocks("other.example.com", " e1 ", 1));
        assert_eq!(cutoff_id(" Shop.Example.com "), "cutoff:shop.example.com");
        assert!(!g.forget_id("  "));
    }

    #[test]
    fn broken_rows_decode_to_none() {
        assert!(decode_row_event(&Value::String("!!".into()), 1).is_none());
        assert!(decode_row_event(&serde_json::json!({"type": "click"}), 1).is_none(), "no href");
        let e = decode_row_event(&serde_json::json!({"type": "click", "href": "https://shop.example.com/a?x=1", "sid": "s"}), 7).unwrap();
        assert_eq!((e.href.as_str(), e.sid.as_str(), e.at_ms), ("https://shop.example.com/a", "", 7));
    }
}
