#!/usr/bin/env python3
# 시나리오 1 ~ 6 계약 (최종 개선안의 72개 검증 벡터). 파일 여섯 개를 다시 쓴다:
#
#   course_nav_v1.json          시나리오 1  타임트라이얼 코스 내비게이션 & 고스트 코칭        CN-01 ~ CN-12   (로드맵 M1)
#   proactive_friction_v1.json  시나리오 2  실시간 마찰 감지 & 비간섭 선제 코칭                 PF-01 ~ PF-12   (M1)
#   contextual_rag_v1.json      시나리오 3  문맥 복원 투트랙 RAG & 신구 대체 로컬 격리          CR-01 ~ CR-12   (M2)
#   feedback_credit_v1.json     시나리오 4  화용 피드백 · 크레딧 & 무손실 동기화                FC-01 ~ FC-12   (M0 · M3)
#   web_analytic_v1.json        시나리오 5  웹 퍼널 분석 · 쇼핑 코칭 & 운영자 리포트            WA-01 ~ WA-12   (M4)
#   privacy_shield_v1.json      시나리오 6  민감 화면 다중 감지 · 소급 제로화 & 복귀 브리지     PS-01 ~ PS-12   (M0)
#
# 실행: python contracts/gen_scenario_contracts_v1.py   (premium-discount 폴더에서. 여섯 파일을 다시 쓴다)
#
# 사례마다 무엇을 넣고(given) 무엇이 참이어야 하는지(expect)와, 그것을 확인하는 시험(owners)을 적는다. owners 의 test 는 시험 이름의
# 앞부분(사례 id 를 소문자로: `cn01_`)이고, 그 파일에 그렇게 시작하는 시험이 하나 이상 있어야 한다.
# contracts/check_scenario_contracts.py 가 확인한다(raon-zena 는 옆 폴더에 있을 때). 값으로 바로 돌려 볼 수 있는 사례는 vector 를
# 싣고, 그 구현의 시험이 이 파일을 읽어 같은 값을 낸다. raon-zena 는 자기가 맡은 사례가 있는 파일을 바이트 그대로 복사해 SHA-256 으로
# 대조한다(verdict_v1.json 과 같은 방식).
#
# 수치(params)는 구현의 상수와 같아야 한다. 값을 바꾸면 파일 이름의 버전을 올린다.

import json
import sys
from pathlib import Path

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
PD = "premium-discount"
RZ = "raon-zena"

NAV = "crates/premium-discount-nav/src"
MCP = "crates/premium-discount-mcp"
CORE = "apps/core/src"
ANA = "crates/premium-discount-analytic"
NMS = "crates/premium-discount-nms/src"
RELAY = "crates/premium-discount-relay"
SENSOR = "crates/premium-discount-sensor/src"
VISION = "crates/premium-discount-vision/src"
WINDOW = "crates/premium-discount-window/src"
IDENTITY = "crates/premium-discount-identity/src"
TRACKER = "web/www/static/js"
ZCORE = "crates/zena-core/src"
ZVERDICT = "crates/zena-verdict/src"
ROUTER = "crates/nlp-nms-router/src"


def t(repo, path, cid):
    """`path`(파일 또는 폴더)에 `<cid 소문자>_` 로 시작하는 시험이 있다"""
    return {"repo": repo, "path": path, "test": cid.lower().replace("-", "") + "_"}


def case(cid, part, title, given, expect, owners, vector=None, pending=None):
    """`pending`: 아직 확인하는 시험이 없는(또는 일부만 있는) 사례의 남은 일. 검사기는 실패로 세지 않고 따로 알린다"""
    prefix = cid.lower().replace("-", "") + "_"
    assert owners or pending, f"{cid} has no test"
    assert all(o["test"] == prefix for o in owners), cid
    out = {"id": cid, "part": part, "title": title, "given": given, "expect": expect, "owners": owners}
    if vector is not None:
        out["vector"] = vector
    if pending is not None:
        out["pending"] = pending
    return out


def contract(name, scenario, title, roadmap, params, cases):
    ids = [c["id"] for c in cases]
    assert len(ids) == 12 and len(set(ids)) == 12, name
    return {
        "contract": name,
        "scenario": scenario,
        "title": title,
        "roadmap": roadmap,
        "source": "최종개선안: raon-zena 와 premium-discount 시나리오별 개선 개발 기획안 (24개 Part, 72개 검증 벡터)",
        "params": params,
        "cases": cases,
    }


# ---------------------------------------------------------------------------------------------------------------
# 시나리오 1: 타임트라이얼 코스 내비게이션 & 고스트 코칭
# ---------------------------------------------------------------------------------------------------------------

def course_nav():
    c = "CN"
    cases = [
        case("CN-01", 1, "비인접 서브루프 선축약 & 경과 시간 보존",
             "S0(0s) → S1(5s) → Modal(8s) → S1(15s) → S2(20s), committed 없음",
             ["정규 스텝 [S0, S1, S2]", "course_hash 가 직행 [S0, S1, S2] 와 같다", "S2.split_ms == 20_000 (루프에서 쓴 시간이 사라지지 않는다)"],
             [t(PD, f"{NAV}/trial.rs", "CN-01")]),
        case("CN-02", 1, "11스텝 초과 시 기점 · 총시간 보존 & route_marks 생존",
             "S0(0s) 출발, 경유지 13개, S_goal(95s) 결승 + 고스트 [S0, G1, G2, S_goal]",
             ["steps.len() == 10", "steps[0] == S0 이고 split_ms 0", "total_ms == 95_000 (앞을 잘라 시계가 줄지 않는다)", "route_marks(run, ghost) 가 Some"],
             [t(PD, f"{NAV}/trial.rs", "CN-02")]),
        case("CN-03", 1, "committed 순환 동선 보호 & 인접 중복 제거",
             "Hub(0s) → Shop(10s, committed) → Hub(20s) → Boss(30s), 그리고 12스텝 솎아내기 경계",
             ["Shop 스텝을 지우지 않는다", "압축 뒤 잇달아 같은 체크포인트 0건 (서버 검증 통과)"],
             [t(PD, f"{NAV}/trial.rs", "CN-03")]),
        case("CN-04", 2, "등급 변동 & 줄 경계 진동 시 번호 불변",
             "같은 screen_key 에서 A=1, B=2, C=3 배정 뒤 C 가 Premium 으로 오르고 A · B 의 cy 가 ±0.004 흔들린다",
             ["A=1, B=2, C=3 그대로 (점멸 0)", "C.tier == Premium 만 바뀐다"],
             [t(PD, f"{NAV}/order.rs", "CN-04")]),
        case("CN-05", 2, "비전 격자(cell:*) 표류 계승 & 1틱 파티클 차단",
             "비전 버튼이 cell:2,3 → cell:2,4 로 0.02 이동 + 빈 배경에 1틱 단독 이벤트(roi:0)",
             ["비전 버튼은 번호를 이어받는다", "1틱 단독 이벤트는 번호를 받지 않는다", "영역 안의 작은 이벤트는 부모를 changed 로 표시한다(포함률 ≥ 0.70)"],
             [t(PD, f"{NAV}/order.rs", "CN-05")]),
        case("CN-06", 2, "일시 가림 600ms 유예 vs 스크롤 즉시 회수",
             "정지 화면에서 2번이 300ms 안 보였다 다시 나옴 / scrolled=true 로 1 · 2번이 화면 밖으로, 아래 새 버튼 등장",
             ["가림은 원래 2번으로 돌아온다", "스크롤 이탈 번호는 바로 비고 새 버튼이 받는다", "민감 화면이 되면 비전 트랙 0건"],
             [t(PD, f"{NAV}/order.rs", "CN-06")]),
        case("CN-07", 3, "이원 3트랙 결속 & 습관 경로 대비 지름길 승격",
             "내 코스 [S0, M1, S_goal](Discount, best 30s) + 공개 A(40명, Discount, avg 28s) + 공개 B(15명, Premium, avg 12s), S0 진입",
             ["Discount 인 A 를 버리고 B 를 고른다 (next == P2)", "vs_best · vs_record · vs_average 모두 Some", "upgrade_over_habit_ms == Some(18_000)"],
             [t(PD, f"{NAV}/trial.rs", "CN-07")]),
        case("CN-08", 3, "우회 이탈(Detouring) 뒤 본선 재합류(Rejoined) & Run.ghost 보존",
             "고스트 [S0(0s), G1(5s), G2(12s), S_goal(20s)] 를 따라가다 S0 → Alt(3s) → G2(9s) → S_goal(16s)",
             ["Alt 에서 Detouring (고스트 핀 유지)", "G2 에서 Rejoined { ghost_index: 2, skipped_steps: 1 }, vs_average == Some(-3_000)",
              "완주 Run.ghost 에 고스트 해시, route_marks 가 (G1 Discount, G2 Premium)"],
             [t(PD, f"{NAV}/trial.rs", "CN-08")]),
        case("CN-09", 3, "제자리 정체 실시간 감지 & 인증 동결 차감",
             "S0(0s) → G1(4s) (고스트 G1=5s, G2=10s), t=25s 까지 다음 스텝 없음, 그 사이 [10s, 20s] 본인 인증 잠금",
             ["vs_record == Some(-1_000) 유지", "동결 10s 를 뺀 유효 경과 15s 로 live_overrun_ms == 5_000"],
             [t(PD, f"{NAV}/trial.rs", "CN-09")]),
        case("CN-10", 4, "창 오프셋 극좌표 · 시선 보정 & 자기 가림 회피",
             "보조 위치 창 (800, 200, 960×540) 안의 버튼 (0.9, 0.9) + 그 위에 오버레이 창",
             ["데스크톱 절대 좌표로 avatar_look · arrow_angle_deg 가 맞는다", "occluded_by_overlay == true"],
             [t(RZ, ZCORE, "CN-10")]),
        case("CN-11", 4, "무라벨 해시 무환각 & Unrated FactsOnly",
             "화면에 없는 다음 StepRef(0xabc…) + 코스 등급 Unrated (decided 2 < 3)",
             ["프롬프트에 \"0xabc\" 없음", "[주행 기록-판정전] 머리글", "\"추천\" 낱말 없음"],
             [t(RZ, ZCORE, "CN-11")]),
        case("CN-12", 4, "프리픽스 캐시 불변 & 발화 번호 잠금(SpokenSlotLock)",
             "CourseBrief 를 넣은 뒤 LLM 이 \"2번 성문으로 가자!\" 를 스트리밍하는 중 만석에 새 Premium 진입",
             ["system + tools + <think></think> 프리픽스 바이트 100% 같다", "locked_numbers 비트 2 로 2번 슬롯 교체 차단"],
             [t(RZ, ZCORE, "CN-12"), t(PD, f"{NAV}/order.rs", "CN-12")]),
    ]
    params = {
        "max_steps": 10, "split_unit_ms": 100, "max_detour_loop_ms": 60_000, "tier_min_decided": 3,
        "sticky": {"occlusion_grace_ms": 600, "min_survival_ratio": 0.30, "rect_ema_alpha": 0.35, "snap_move": 0.12,
                   "contain_ratio": 0.70, "transient_scans": 2, "transient_ms": 250, "row_band": 0.045, "row_overlap": 0.45, "max_numbered": 9},
        "ghost": {"tier_score": {"premium": 2.0, "neutral": 0.0, "unrated": 0.0, "discount": -3.0}, "upgrade_min_ratio": 0.15, "upgrade_min_ms": 1_500},
        "brief": {"split_deadband_ms": 1_000, "max_lines": 2, "overlay_iou": 0.05, "viewport": [350, 450]},
    }
    return contract("course_nav_v1", 1, "타임트라이얼 코스 내비게이션 & 고스트 코칭", "M1", params, cases)


# ---------------------------------------------------------------------------------------------------------------
# 시나리오 2: 실시간 마찰 감지 & 비간섭 선제 코칭
# ---------------------------------------------------------------------------------------------------------------

def proactive_friction():
    cases = [
        case("PF-01", 1, "peek_live 무오염 불변성",
             "피어 10개가 쌓인 FrictionBaselines 에 진행 중 에피소드로 peek_live 50번",
             ["Welford (n, mean, m2) 비트 단위로 그대로", "decide 직전 z 값과 오차 < 1e-12"],
             [t(PD, f"{NAV}/behavior.rs", "PF-01")]),
        case("PF-02", 1, "무선택 병목 포착 & 전투 면책",
             "#1 정상 선택 뒤 #2 에서 Retried × 2 + RageClick / combat_exempt 상태에서 3연타",
             ["앞은 bottleneck_anchor == \"#2\" 이고 CoachReady", "뒤는 RageClick 면책으로 Calm"],
             [t(PD, f"{NAV}/behavior.rs", "PF-02")]),
        case("PF-03", 1, "미행동 능동 탐색 vs AFK 동결 & 콜드스타트",
             "빈 기준선(n=0)에서 포인터가 14s 움직이며 클릭 없음 + 광클 / 포인터 정지(AFK) 60s",
             ["앞은 cold_start_fallback == true 로 경보", "뒤는 탐색 시계가 멈춰 Calm"],
             [t(PD, f"{NAV}/behavior.rs", "PF-03")]),
        case("PF-04", 2, "고긴장 전투 중 보류 & 패배 직후 복기 방출",
             "tension 0.78 교전 중 CoachReady → 8s 뒤 패배 · 교전 끝 → +1,500ms",
             ["교전 중 발화 0건", "패배 1,500ms 뒤 PostDefeatComfort 로 정확히 한 번"],
             [t(RZ, ZCORE, "PF-04")]),
        case("PF-05", 2, "채팅(typing) 우선권 & 질문 흡수 vs 취소",
             "typing 중 보류 → A: 질문 전송(ask_epoch 증가) / B: Esc 로 닫고 800ms",
             ["A 는 보류 훅을 바로 버린다 (중복 발화 0)", "B 는 AfterInputAborted 로 방출"],
             [t(RZ, ZCORE, "PF-05")]),
        case("PF-06", 2, "자가 해결 자동 폐기 & 90s 쿨다운",
             "보류 중 사용자가 스스로 Submitted 통과 / 이미 코칭한 같은 (screen, anchor) 가 30s 안에 다시",
             ["자가 해결은 뒷북 발화 0건", "같은 병목 재발은 쿨다운으로 막힌다"],
             [t(RZ, ZCORE, "PF-06")]),
        case("PF-07", 3, "비차단 예열 중 pd_nav_number 무정체 & 합류",
             "모델 로드 3,500ms 짜리 앱을 prewarm 한 직후 pd_nav_number, 2,000ms 에 검색",
             ["prewarm · pd_nav_number 응답 < 20ms", "앱 시작 starts == 1", "검색은 남은 ~1,500ms 만 기다린다"],
             [t(PD, f"{MCP}/src/bridge.rs", "PF-07")]),
        case("PF-08", 3, "투기적 예열 45s 조기 회수 vs 실사용 300s 유지",
             "A: prewarm 뒤 무명령 50s / B: prewarm 뒤 검색 1회 후 50s / C: 모델 내려받기 중 유휴",
             ["A 는 45s 에 cmd_shutdown 으로 정상 종료", "B 는 ActiveUse 로 승격되어 계속 실행", "C 는 끝내지 않는다"],
             [t(PD, f"{MCP}/src/bridge.rs", "PF-08")]),
        case("PF-09", 3, "저메모리(< 650MB) · 미설치 서킷 브레이커",
             "가용 RAM 480MB 에서 PreWarm / installed == none 에서 PreWarm 10번",
             ["저메모리는 시작 0번 (deferred)", "미설치는 한 번 확인 뒤 Suppressed 로 고정 (다시 시작 0번)"],
             [t(PD, f"{MCP}/src/bridge.rs", "PF-09")]),
        case("PF-10", 4, "근거 유무 기반 Lv2/Lv3 분기 & 질문 선점",
             "A: 검증된 Premium 우회로가 있다 / B: 모두 Unrated 초행 / C: 도중에 사용자 질문",
             ["A 는 Lv3 (FrictionBrief, 프리픽스 불변)", "B 는 LLM 없이 Lv2 캐시 대사", "C 는 선제 스트림을 바로 취소"],
             [t(RZ, ZCORE, "PF-10")]),
        case("PF-11", 4, "Dialogue 4턴 보존 & 45s 임시 제안 승격",
             "4턴이 찬 대화에서 선제 코칭 → A: 50s 무응답 / B: 10s 안에 후속 질문",
             ["A 는 기존 4턴 그대로", "B 는 선제 제안이 직전 DialogueTurn 으로 승격"],
             [t(RZ, ZCORE, "PF-11")]),
        case("PF-12", 4, "거절 피로도 히스테리시스 & 수용 engage(0.2)",
             "3번 연속 무시(MutedUntilAsk) 뒤 사용자 질문 / 제안 뒤 5s 안에 지목 번호 선택",
             ["무시 3번 뒤 선제 발화 0건, 사용자 질문으로 풀린다", "번호를 고르면 coach 트랙 engage(0.2) 와 노출 장부 등록"],
             [t(RZ, ZCORE, "PF-12")]),
    ]
    params = {
        "speculative_idle_ms": 45_000, "active_idle_ms": 300_000, "min_prewarm_avail_mb": 650, "shutdown_grace_ms": 2_000,
        "post_defeat_ms": 1_500, "after_input_aborted_ms": 800, "topic_cooldown_ms": 90_000,
        "fatigue_ignored_offers": 3, "ephemeral_offer_ms": 45_000, "accept_window_ms": 5_000, "coach_engage": 0.2,
    }
    return contract("proactive_friction_v1", 2, "실시간 마찰 감지 & 비간섭 선제 코칭", "M1", params, cases)


# ---------------------------------------------------------------------------------------------------------------
# 시나리오 3: 문맥 복원 투트랙 RAG & 신구 대체 로컬 격리
# ---------------------------------------------------------------------------------------------------------------

def contextual_rag():
    cases = [
        case("CR-01", 1, "공간 지시 vs 대화 대명사 분리",
             "\"출혈 쌍곡도\" 대화 직후 \"여기서 주의할 거?\" vs \"그거 방패로 막아?\"",
             ["앞은 DeicticScreen (지금 체크포인트의 L1 to)", "뒤는 AnaphoricDialogue (\"출혈 쌍곡도\" 복원)"],
             [t(RZ, ZCORE, "CR-01")]),
        case("CR-02", 1, "창(cc) 전환 토픽 오염 차단",
             "eldenring.exe 대화 직후 steampowered.com 에서 \"이거 할인율 어때?\"",
             ["게임 토픽 가중치 0.0", "브라우저 cc 스코프만 조회"],
             [t(RZ, ZCORE, "CR-02")]),
        case("CR-03", 1, "복수 선행사 동점 → 되묻기",
             "두 무기를 함께 말한 직후 \"그거 지력 제한 몇이야?\"",
             ["ambiguous_pair == Some(…)", "Situation.context 에 단정 금지 · 되묻기 가드"],
             [t(RZ, ZCORE, "CR-03")]),
        case("CR-04", 2, "단일 임베딩 3계층 백오프 & 구문 스팬",
             "L1 0건 · L2 2건 · L3 5건 + 긴 수식어가 붙은 바꿔 말한 질문",
             ["Role::Query 768d 인코딩 한 번", "L2(+0.06) · L1(+0.12) 가산으로 다시 줄 세움, 코사인 바닥 0.42", "구문 압축 스팬이 맞는다"],
             [t(PD, f"{CORE}/pipeline.rs", "CR-04"), t(PD, NMS, "CR-04")]),
        case("CR-05", 2, "0ms PreThink & 600ms 미러 응답 + SyncLoop 양보",
             "앱 냉기동(3.5s) 중 질문, SyncSnapshot 에 내 원문 2건(stored 1, rejected 1)",
             ["0ms 에 PreThink", "SyncLoop 대화형 홀드", "600ms 에 미러에서 stored 1건만 (rejected 0건) 으로 1차 발화", "3.5s 결과는 캐시에"],
             [t(PD, f"{MCP}/src/client.rs", "CR-05"), t(RZ, "apps/raon-zena/src/pd_bridge.rs", "CR-05")]),
        case("CR-06", 2, "연타 정정 세대 취소 & 프라이버시 캐시 격리",
             "질문 A(epoch 1) 뒤 300ms 만에 질문 B(epoch 2) / 검색 중 privacy_epoch 증가",
             ["epoch 1 검색 · 토큰 생성 폐기, 말풍선은 epoch 2 만", "privacy_epoch 가 다른 늦은 결과는 캐시에 넣지 않는다"],
             [t(RZ, "apps/raon-zena/src/pd_bridge.rs", "CR-06")]),
        case("CR-07", 3, "교차 스코프 다국어 대체 & 400 방어",
             "한글 옛 공략(to: None, \"20%\") + 영문 새 공략(to: L1, \"15%\")",
             ["로컬 Confirmed { value_diff: (\"20%\", \"15%\") }", "old.to != new.to 면 서버 pd_supersede 를 부르지 않는다"],
             [t(PD, RELAY, "CR-07")]),
        case("CR-08", 3, "조건부 병존 보호 & 부정 반전",
             "\"1페이즈 20%\" vs \"2페이즈 15%\" / \"회피 가능\" → \"회피 불가\"",
             ["앞은 둘 다 Active", "뒤는 Duplicate 로 합치지 않고 Supersedes"],
             [t(PD, RELAY, "CR-08")]),
        case("CR-09", 3, "제3자 로컬 격리 & 자가 복권",
             "남의 옛 버전을 감지(403 회피) 뒤 새 글이 전역 Discount 로 떨어진다",
             ["감지 즉시 로컬 grade −2 격리", "새 글이 떨어지면 옛 버전을 자동 복권"],
             [t(PD, RELAY, "CR-09")]),
        case("CR-10", 4, "무원문 무블로킹 3단 역해석",
             "원격 Premium 3건 (NavMap 해시 일치 1, ref_ 가 내 미러 노트와 일치 1, 미일치 1)",
             ["15초 블로킹 그래프 호출 0번", "화면 라벨 1 · 내 스레드 1 복원 + 1건은 [서버 여론] 한 줄 (지어낸 글 0)", "그래프를 묻더라도 200ms 안에 포기"],
             [t(RZ, ZCORE, "CR-10"), t(PD, f"{MCP}/src/graph.rs", "CR-10")]),
        case("CR-11", 4, "rejected 영구 배제 · missing 보존 & FactsOnly",
             "rejected 내 기록 1 + missing 개인 메모 1 + 모두 Unrated(E = 1.2) 인 후보",
             ["rejected 는 빠진다", "missing 은 로컬 메모로 남는다", "미판정 후보는 [사실기록-판정전] (\"추천\" 없음)"],
             [t(PD, f"{CORE}/pipeline.rs", "CR-11"), t(RZ, ZCORE, "CR-11")]),
        case("CR-12", 4, "송출 대사 대조 노출 장부 & 프리픽스 불변",
             "Slot 1(회피) · Slot 2(항아리) · Slot 3(대방패, 경고) 를 넣고 실제로는 회피 · 대방패만 말함",
             ["Slot 1 만 RecommendedSolution(1.0)", "Slot 3 은 WarnedTrap", "말하지 않은 Slot 2 는 등록하지 않는다", "프리픽스 바이트 그대로"],
             [t(RZ, ZCORE, "CR-12")]),
    ]
    params = {
        "soft_deadline_ms": 600, "scope_boost": {"l1": 0.12, "l2": 0.06, "mine": 0.02}, "cosine_floor": 0.42,
        "remote_top_k": 16, "hash_floor": 0.50, "graph_quick_timeout_ms": 200, "graph_min_budget_ms": 250,
        "scene_cache_ttl_ms": 120_000, "mirror_per_cc": 256, "exposure_window_ms": 300_000, "claim_max_chars": 38, "verdict_slots": 3,
    }
    return contract("contextual_rag_v1", 3, "문맥 복원 투트랙 RAG & 신구 대체 로컬 격리", "M2", params, cases)


# ---------------------------------------------------------------------------------------------------------------
# 시나리오 4: 화용 피드백 · 이중 센터링 크레딧 & 무손실 동기화
# ---------------------------------------------------------------------------------------------------------------

def feedback_credit():
    cases = [
        case("FC-01", 1, "요청문 배제 & 장형 이중 부정",
             "\"다른 공략 추천해 줘\" vs \"우회하긴 했는데 나쁘지 않았어\"",
             ["앞은 None (투표 0건)", "뒤는 라우터가 부정을 버리지 않고 (\"feedback\", +1, 0.5)"],
             [t(RZ, ZVERDICT, "FC-01"), t(PD, NMS, "FC-01")]),
        case("FC-02", 1, "역접 복합문 주절 우선",
             "\"딜은 쎈데 스태미나가 너무 딸려서 비추\"",
             ["앞절 긍정 기각", "주절 기준 (\"feedback\", -1, 1.0)"],
             [t(RZ, ZVERDICT, "FC-02"), t(PD, NMS, "FC-02")]),
        case("FC-03", 1, "자기 실수 귀인 면책 & 반어법 차단",
             "패배 직후 \"공략은 맞는데 내 손이 꼬였어\" / 고마찰 직후 \"참 좋~은 길이다 죽기 딱 좋네\"",
             ["앞은 shield_defeat_outcome == true (패배 -1 을 쌓지 않는다)", "뒤는 sarcasm_blocked == true (긍정 오탐 0)"],
             [t(RZ, ZVERDICT, "FC-03")]),
        case("FC-04", 1, "5분 안 기존 표 제자리 철회 (sign = 0)",
             "\"이거 추천!\"(+1) 60s 뒤 \"아까 말 취소, 2페 막혀서 별로야\"",
             ["앞 관측이 제자리에서 sign 0", "-1 하나만 반영 (E == 1.0, v < 0)"],
             [t(RZ, ZVERDICT, "FC-04")]),
        case("FC-05", 2, "순항 구간 연대 책임 보호 & N×4 이중 센터링",
             "체크포인트 A(무마찰 z=−0.8) → B(광클 z=+2.1) 직후 사망(outcome −1)",
             ["double_center 가 0.0 으로 퇴화하지 않는다", "병목 B 에 −1.0", "순항 A 는 credit 0.0"],
             [t(RZ, ZVERDICT, "FC-05")]),
        case("FC-06", 2, "경고 슬롯(WarnedTrap) 부호 역전 차단",
             "Slot 1(추천 해법) + Slot 3(패치 전 경고) 브리핑 뒤 클리어(+1)",
             ["경고 Slot 3 은 행렬에 넣지 않는다 (+ 크레딧 0.0)", "Slot 1 에만 +1.0"],
             [t(RZ, ZVERDICT, "FC-06")]),
        case("FC-07", 2, "N=2 시너지 0.5/0.5 분할 & 교차 시도 outperforms",
             "보완 조합 2개를 함께 써서 클리어 / 배타 대안 A 3패 뒤 B 3승",
             ["앞은 0.5 / 0.5", "뒤는 duel_for_item(B) == (3, 0), B --outperforms--> A"],
             [t(RZ, ZVERDICT, "FC-07")]),
        case("FC-08", 3, "DifferKind 4분화 · 영속 FadedRecall & 레시피 비혼합",
             "(Local Premium, Global Discount) / 감쇠 E = 1.90 상태에서 두 번 조회",
             ["WorksForMeOnly", "두 번째 조회에도 FadedRecall", "v1 · v2 원시 수치를 섞지 않는다"],
             [t(RZ, ZVERDICT, "FC-08")]),
        case("FC-09", 3, "수동 ▼ 핀 즉시 오버라이드 · 되돌리기 & 무전송",
             "5연승 Premium 기록에 ▼ 를 눌렀다 다시 눌러 해제",
             ["첫 클릭에 Discount (PinnedByUser), 서버로 보낼 것 0건", "다시 누르면 원래 Premium"],
             [t(RZ, ZVERDICT, "FC-09")]),
        case("FC-10", 4, "5초 멱등 키 · 오프라인 표 합치기 (D-03, 409 0건)",
             "5초 안에 같은 문장을 다시 저장 vs 다른 답글(ref) · 다른 대상(to)에 같은 문장 / 오프라인에서 남의 글 +1 → −1",
             ["같은 저장은 같은 request_id (\"idemp-\" + 48 hex, 54자) 로 한 항목, 임베딩 전에 0ms 에 기존 결과", "다른 부모 · 대상 · bcc 는 다른 키 (서버 409 없음)",
              "표를 바꾸면 새 request_id 로 마지막 값 하나만 보낸다"],
             [t(PD, f"{CORE}/queue.rs", "FC-10"), t(PD, f"{CORE}/pipeline.rs", "FC-10")],
             vector={"derive": "hex(SHA-256(cc \\0 to \\0 bcc \\0 parent \\0 text_hash \\0 floor(now_ms / 5000)))[0..48]", "prefix": "idemp-", "len": 54, "window_ms": 5_000}),
        case("FC-11", 4, "3단 욕설 차단 · 부모 거부 자식 연쇄 & 거부 알림 보존 (E-16)",
             "\"이 시1발\" 드롭 / 오프라인 부모+자식 등록 뒤 서버 cron 이 부모를 rejected(profanity) 로, 그 뒤 10s 주기 두 번",
             ["\"이 시1발\" 은 앱을 띄우지 않고 0ms 에 차단 (사전 필터)", "자식 답글은 reply_cascade 로 함께 거부", "두 주기 뒤에도 unacked_rejections 에 거부 알림이 남는다"],
             [t(PD, f"{MCP}/src/client.rs", "FC-11")]),
        case("FC-12", 4, "401/429 50초 유실 차단 (E-11) · 투표 단독 SyncLoop & 큐 상한 (E-17)",
             "401 · 429 상태로 10s 주기 6번(60s) 뒤 재로그인 / 평가 큐 0 + 오프라인 투표 1건",
             ["60s 뒤에도 attempts == 0, 유실 0건, 재로그인하면 바로 보낸다", "투표만 있어도 SyncLoop 가 돌고 votes_remaining == 0 에서 멈춘다",
              "상한: 대기 1,000 · 결과 대기 2,000 (7일) · 못 보낸 표 2,000 (결과 표부터 정리)"],
             [t(PD, f"{CORE}/queue.rs", "FC-12"), t(PD, f"{CORE}/votes.rs", "FC-12"), t(PD, f"{MCP}/src/client.rs", "FC-12")]),
    ]
    params = {
        "idempotent_window_ms": 5_000, "request_id_prefix": "idemp-", "request_id_len": 54, "max_server_attempts": 5,
        "max_pending_items": 1_000, "max_tracking_items": 2_000, "retain_ms": 604_800_000, "max_queued_votes": 2_000,
        "default_retry_after_ms": 60_000, "max_unacked_rejections": 32, "maintain_every_ms": 1_800_000,
        "retraction_window_ms": 300_000, "synergy_split": [0.5, 0.5], "absent": -1.0, "decisive_credit": 0.8,
        "failure": {"offline": "멈춤 · 횟수 0", "auth_hold": "401 · 403: 멈춤 · 횟수 0 · 재로그인까지", "rate_limited": "429: 멈춤 · 횟수 0 · Retry-After",
                    "transient": "408 · 5xx · 밝혀지지 않은 404: 횟수 +1, 5번이면 실패", "permanent": "400 · 409 · 422 · \"evaluation not found\": 바로 거부"},
    }
    return contract("feedback_credit_v1", 4, "화용 피드백 · 이중 센터링 크레딧 & 무손실 동기화", "M0 · M3", params, cases)


# ---------------------------------------------------------------------------------------------------------------
# 시나리오 5: 웹 퍼널 분석 · 실시간 쇼핑 코칭 & 운영자 리포트
# ---------------------------------------------------------------------------------------------------------------

def web_analytic():
    cases = [
        case("WA-01", 1, "가변 재고 · 타이머 박리 & 상품 번호 보존",
             "\"키보드 | 남은 수량 3개\" vs \"1개\" / \"상품 1\" vs \"상품 2\"",
             ["가변 재고는 같은 키", "\"상품 1\" · \"상품 2\" 는 다른 키", "analytic_pug_v1.json 13사례 그대로 통과"],
             [t(PD, ANA, "WA-01")]),
        case("WA-02", 1, "글 없는 아이콘 버튼의 aria-label 보존",
             "<button aria-label=\"장바구니\"><svg/></button> 와 input[value]",
             ["button | [장바구니] (4자리 이상 숫자는 # 로)", "input 값은 모으지 않는다"],
             [t(PD, ANA, "WA-02")]),
        case("WA-03", 1, "#/__modal/ SPA 장바구니 단계 승격",
             "URL /product/1 그대로에서 position: fixed; aria-modal=\"true\" 드로어가 열림",
             ["모달 감지", "#/__modal/cart 를 거치는 cart 퍼널 체크포인트"],
             [t(PD, ANA, "WA-03")]),
        case("WA-04", 1, "이탈 시 64KB 결말 사건 우선 & 불변 사건 id",
             "무거운 형제 30개 + 마지막 leave 를 언로드 때 보내고 다시 보냄",
             ["첫 패킷(< 8KB)에 leave", "같은 사건은 같은 id 라 다시 보내도 중복 0건"],
             [t(PD, ANA, "WA-04")]),
        case("WA-05", 2, "무한 스크롤 가시 뷰포트 분모 n_eff",
             "DOM 80개 중 화면에 보인 8개에서 1개 클릭",
             ["보이지 않은 72개는 외면 표 0건", "n_eff = 8 로 기댓값 계산"],
             [t(PD, ANA, "WA-05")]),
        case("WA-06", 2, "상단 위치 편향 제거 & 비인기 옵션 보호",
             "상단 1번의 기저율 반영 / 마찰 없는 비인기 색상 옵션(−2.2σ)",
             ["비인기 옵션은 Discount 로 내리지 않고 BenignIgnored (Neutral)"],
             [t(PD, ANA, "WA-06")]),
        case("WA-07", 2, "바로구매 단계 스킵 & 다중 탭 비교 면책",
             "상세 → 바로구매 → 완료 세션 + 같은 tab_group 비교 탭 4개 닫힘",
             ["장바구니 이탈 분모를 더럽히지 않는다 (FastTrackCourse)", "닫힌 비교 탭 Abandoned 0건"],
             [t(PD, ANA, "WA-07")]),
        case("WA-08", 2, "고클릭 × 고마찰 쿠폰 칸 FrictionTrap 거부권",
             "클릭 선호 +2.8σ + 직후 광클 · 포기 마찰 z=+1.9",
             ["GoldenPath 대신 FrictionTrap (Discount)"],
             [t(PD, ANA, "WA-08")]),
        case("WA-09", 3, "무확장 비동기 UIA 탭 cc 격리 & 번역 매칭",
             "크롬 탭 전환 (amazon.com 한글 번역 \"지금 구매\") + 시크릿 탭",
             ["LABEL_SPACE 로 \"Buy Now\" GoldenPath 와 1:1", "시크릿 탭 기록 0건"],
             [t(RZ, "apps/raon-zena/src/browser_uia.rs", "WA-09")]),
        case("WA-10", 3, "실시간 품절 강등 & 최종 결제 CommitGuard",
             "1순위 '간편결제' disabled/품절 + '결제하기' 확정 버튼 화면",
             ["다음 경로가 #1 을 이어받는다 (영구 원장은 그대로)", "확정 버튼 자동 클릭 금지, 확인 먼저 하는 말씨"],
             [t(RZ, ZCORE, "WA-10")]),
        case("WA-11", 4, "pd_analytic 노출 · +37h 대칭 비교 & 표본 가드",
             "화요일 13시에 \"이번 주 vs 지난주 전환율\" + 세션 4건 구간",
             ["TOOLS 11번째 pd_analytic 이 답한다", "지난주도 같은 +37시간 창으로 잘라 비교", "4건 구간은 InsufficientSample (단정 어휘 없음)"],
             [t(PD, ANA, "WA-11"), t(PD, f"{MCP}/tests/server.rs", "WA-11"), t(RZ, ZCORE, "WA-11")]),
        case("WA-12", 4, "통합 TOMBSTONE · DELTA GUARD & 19종 BLOB",
             "forget(host, before_ms) · forget(ids) 뒤 다시 받기 + 깨진 행 하나가 섞인 analytic_blob_v1.json 19사례",
             ["cutoff:<host> · id 묘비로 지운 사건 재삽입 0건", "updated_at > 0 행 중복 가산 0건", "19사례 복원, 불량 1건은 broken == 1 로 격리"],
             [t(PD, f"{CORE}/analytic.rs", "WA-12"), t(PD, ANA, "WA-12")]),
    ]
    params = {
        "visible_siblings": 16, "unload_budget_bytes": 65_536, "first_packet_bytes": 8_192,
        "min_funnel_sessions": 15, "min_element_exposures": 5, "choice_sigma": 2.0, "tools": 11,
        "insight_keys": ["conversion_insight", "traffic_insight", "general_insight"],
    }
    return contract("web_analytic_v1", 5, "웹 퍼널 분석 · 실시간 쇼핑 코칭 & 운영자 리포트", "M4", params, cases)


# ---------------------------------------------------------------------------------------------------------------
# 시나리오 6: 민감 화면 다중 감지 · 5초 소급 제로화 & 복귀 브리지
# ---------------------------------------------------------------------------------------------------------------

def privacy_shield():
    cases = [
        case("PS-01", 1, "비밀번호 칸 차단 하위 호환",
             "Region { sensitive: true } 가 있는 화면 (nav_screen(true))",
             ["sensitive == true, record == false", "비전 · 이벤트 후보 0건 (접근성 트리만)"],
             [t(PD, NAV, "PS-01")]),
        case("PS-02", 1, "무속성 가상 키패드 · OTP 포착 & 오탐 차단",
             "3×4 숫자 보안 키패드 · 6칸 OTP vs 3×4 게임 인벤토리 · 일반 영문 문장",
             ["키패드 · OTP 는 FullLockdown", "인벤토리 · 영문은 Clear"],
             [t(PD, NAV, "PS-02")]),
        case("PS-03", 1, "게임 결제 오버레이 · 옆 창 WDA 감지",
             "게임 위 결제 오버레이 프로세스 / 옆 창이 WDA_EXCLUDEFROMCAPTURE",
             ["게임 UIA 와 상관없이 바로 FullLockdown"],
             [t(PD, NAV, "PS-03"), t(PD, WINDOW, "PS-03")]),
        case("PS-04", 1, "천공 패스 소형 모달 Privacy 시드 보존",
             "체커보드 PerforatedCfg + 구석 4% 소형 로그인 모달",
             ["국소 휘도 깨움 + VisionKind::Privacy 시드로 구멍을 넓혀 잡는다"],
             [t(PD, VISION, "PS-04")]),
        case("PS-05", 1, "알림 토스트 패치 정렬 국소 마스킹",
             "보스전 오른쪽 아래 알림 토스트(PartialRedaction) 를 패치로 나눔",
             ["옆 패치로 번지지 않는다", "토스트에 겹친 후보만 버리고 본 화면 코칭은 그대로"],
             [t(PD, VISION, "PS-05")]),
        case("PS-06", 2, "prev_log 소거 & 무역상 복귀",
             "민감 화면 1프레임 뒤 purge_privacy_residue() → 일반 화면",
             ["prev_log · ROI 트랙 0", "복귀 첫 프레임 타일 차분 이벤트 0건"],
             [t(PD, SENSOR, "PS-06")]),
        case("PS-07", 2, "진행 중 비전 블록 조기 중단",
             "인코딩 도중 privacy_epoch 증가",
             ["다음 블록 경계에서 멈춘다", "PatchGrid · 32×18 썸네일을 지우고 넘기지 않는다"],
             [t(PD, VISION, "PS-07")]),
        case("PS-08", 2, "미완결 에피소드 5초 소급 절단 & record false",
             "키패드 감지 직전 3초 동안의 클릭 · 수정 사건이 LiveEpisodeWindow 에 있다",
             ["최근 5초 사건 폐기", "FrictionBaselines · Outbox · 장부 기록 0건"],
             [t(PD, NAV, "PS-08")]),
        case("PS-09", 3, "은행 창 장기 유지 vs 3초 로그인 고속 복귀",
             "은행 창(비전 단독 포함) 120s / 3s 로그인 뒤 게임 창",
             ["은행 창은 120s 뒤에도 Locked (캡처 0)", "로그인은 GraceProbe(3s) 뒤 ~6s 안에 복귀"],
             [t(PD, NAV, "PS-09")]),
        case("PS-10", 3, "주문 완료 영수증 ReceiptGuard 무제한 보호",
             "결제 해제 직후 /order/complete 에서 30초",
             ["정제한 page_hash 결승 체크포인트 1건만", "페이지를 떠나기 전까지 픽셀 · 라벨 수집 차단"],
             [t(PD, NAV, "PS-10")]),
        case("PS-11", 3, "다중 인증 시간 차감 course_chain 브리징",
             "S1 → S2 → [본인 인증 45s 잠금] → S3 완주 (자정을 넘김)",
             ["구간 시간 · 간격에서 45s 를 뺀 하나의 완주 런", "Run.day 는 실제 결승 날짜"],
             [t(PD, f"{NAV}/trial.rs", "PS-11")]),
        case("PS-12", 4, "최전선 차단 · 강제 클릭 투과 & 종료 0ms 캡처 절단",
             "InteractivePet 잠금 + 채팅에 Luhn 카드 번호 / BIP-39 12단어 / 0x+40자 공개 주소 + ExitApp",
             ["잠금 즉시 WS_EX_TRANSPARENT, alpha 76, 15 FPS (66,666µs)", "카드 · 시드는 지우고 LLM · 대화 · 보관함 · 사전으로 0건 (공개 주소는 통과)",
              "Farewell 0ms 에 allows_capture() == false"],
             [t(RZ, ZCORE, "PS-12"), t(PD, IDENTITY, "PS-12")]),
    ]
    params = {
        "grace_probe_ms": 3_000, "grace_probe_ticks": 3, "retro_purge_ms": 5_000, "locked_alpha": 76,
        "locked_frame_us": 66_666, "keypad": [3, 4], "otp_cells": 6, "seed_words": [12, 24],
    }
    return contract("privacy_shield_v1", 6, "민감 화면 다중 감지 · 5초 소급 제로화 & 복귀 브리지", "M0", params, cases)


ALL = [course_nav, proactive_friction, contextual_rag, feedback_credit, web_analytic, privacy_shield]


def main():
    for build in ALL:
        body = build()
        path = HERE / f"{body['contract']}.json"
        path.write_text(json.dumps(body, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n")
        print(f"wrote {path.name}: {len(body['cases'])} cases")


if __name__ == "__main__":
    main()
