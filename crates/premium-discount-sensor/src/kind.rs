//! 화면 종류: 화면 감시기가 읽은 화면을 어떻게 다루는지. raon-zena 의 단어 사전(`zena_lexicon`)과 비전 앵커 은행
//! (premium-discount-vision)이 같은 값을 쓴다.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisionKind {
    /// 화면이 무엇인지(게임 전투, 차트, 코드 …): 바뀔 때 반응하고, 그다음은 드물게.
    Scene,
    /// 순간(승리, 게임 오버, 로딩): 그때마다 반응, 짧은 대기.
    Event,
    /// 개인 화면: 반응 · 말 없음, 감시기도 물러난다.
    Privacy,
}

impl VisionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VisionKind::Scene => "scene",
            VisionKind::Event => "event",
            VisionKind::Privacy => "privacy",
        }
    }

    pub fn parse(s: &str) -> Result<VisionKind, String> {
        Ok(match s {
            "scene" => VisionKind::Scene,
            "event" => VisionKind::Event,
            "privacy" => VisionKind::Privacy,
            _ => return Err(format!("unknown vision kind '{s}' (scene, event, privacy)")),
        })
    }

    /// 같은 도메인에 다시 반응하기까지의 초 (노드가 `cooldown_s` 를 정하지 않았을 때)
    pub fn default_cooldown_s(self) -> u32 {
        match self {
            VisionKind::Scene => 600,
            VisionKind::Event => 20,
            VisionKind::Privacy => 60,
        }
    }
}
