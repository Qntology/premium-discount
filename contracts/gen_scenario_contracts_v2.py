#!/usr/bin/env python3
# Phase 4 ~ Phase 7 코스·체크포인트 N:N 고도화 및 포용적 접근성 계약 생성기.
#
#   course_nav_v2.json       Phase 4 ~ 6  코스·체크포인트 N:N 심층 고도화   CN2-45 ~ CN2-80 (36개 검증 벡터)
#   accessible_nav_v1.json   Phase 7      포용적 접근성 내비게이션          AC-01 ~ AC-06   (6개 검증 벡터)
#
# 실행: python contracts/gen_scenario_contracts_v2.py  (premium-discount 폴더에서)

import json
import sys
from pathlib import Path

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
PD = "premium-discount"
RZ = "raon-zena"

NAV = "crates/premium-discount-nav/src"
ANA = "crates/premium-discount-analytic"
NMS = "crates/premium-discount-nms/src"
RELAY = "crates/premium-discount-relay/src"
WWW = "workers/www/src"
CRON = "workers/cron/src"
ZCORE = "crates/zena-core/src"
ZVERDICT = "crates/zena-verdict/src"
ZLEX = "crates/zena-lexicon/src"
ZAPP = "apps/raon-zena/src"


def t(repo, path, cid):
    return {"repo": repo, "path": path, "test": cid.lower().replace("-", "") + "_"}


def case(cid, part, title, given, expect, owners):
    prefix = cid.lower().replace("-", "") + "_"
    assert owners, f"{cid} has no owner test"
    assert all(o["test"] == prefix for o in owners), cid
    return {
        "id": cid,
        "part": part,
        "title": title,
        "given": given,
        "expect": expect,
        "owners": owners,
    }


def course_nav_v2():
    cases = [
        # Phase 4 Part 1: route_marks 2.0
        case("CN2-45", "P4.1", "품질·속도 인지형 route_marks_v2 오염 차단",
             "고스트(15s) 대비 둔속 고마찰 우회 런(45s, discount) 및 고속 무마찰 우회 런(11s, premium)",
             ["둔속·마찰 우회 런에서 미방문 스텝이 Abstain(0)으로 보호된다", "고속 무마찰 우회 런에서만 -1이 부과된다", "결함 스텝은 +1 세탁이 차단된다"],
             [t(PD, f"{NAV}/trial.rs", "CN2-45")]),
        case("CN2-46", "P4.1", "1인 1목적지 투표권(nav_route_ballots) 다중 우회 증폭 차단 및 목적지별 적합도 분리",
             "단일 지갑이 서로 다른 우회 경유지 X1..X5로 같은 고스트의 CP_R2를 5회 우회 완주",
             ["nav_route_ballots 중복 차단으로 CP_R2 discount가 1을 넘지 않는다", "의견 번복 시 이전 부호가 상쇄된다", "route_fit_hash가 결승 목적지별로 분리된다"],
             [t(PD, f"{WWW}/navtrials_v2.rs", "CN2-46")]),
        case("CN2-47", "P4.1", "크로스 모달리티(탭 vs 스와이프) EquivalentPass 보호",
             "WEB_COURSE의 elem:c@swipe-left를 탭(elem:c)으로 정속 완주",
             ["동일 (kind, hash) 상이 제스처 통과 시 Discount(-1) 대신 EquivalentPass(0)로 판정된다"],
             [t(PD, f"{NAV}/trial.rs", "CN2-47"), t(PD, f"{WWW}/navtrials_v2.rs", "CN2-47")]),
        case("CN2-48", "P4.1", "FC-05 fault_mask 병목 격리 · FC-06 WarnedTrap 보호 · 승리 직후 부정 발화 억제",
             "순항 A(z=-0.4) + WarnedTrap B + 병목 C(z=2.1) 및 자책 패배 / 승리 직후 '깨긴 했는데 비추' 발화",
             ["자책 방패는 fault_mask=u16::MAX, run_kind=0으로 오염을 차단한다", "승리 후 부정 발화는 SuppressedByFeedback로 +1을 억제한다", "순항 스텝 비트는 0으로 보호된다"],
             [t(RZ, f"{ZVERDICT}/credit.rs", "CN2-48")]),
        # Phase 4 Part 2: shortcut_wins 2.0 & Pioneer Promotion
        case("CN2-49", "P4.2", "LCS 창 내부 구간 시간 대조 shortcut_wins_v2 무임승차 차단 및 후반 지름길 구제",
             "1차 분기 지연(+3s)+2차 분기 단축(-10s) 런 및 초반 지연(+8s)+후반 단축(-6s) 런",
             ["지연된 1차 우회는 무임승차 차단된다", "창 내부 구간 단축을 입증한 후반 Alt_2만 승리로 추출된다"],
             [t(PD, f"{NAV}/trial.rs", "CN2-49")]),
        case("CN2-50", "P4.2", "서버 갈림길 대결(nav_shortcut_duels) 독립 3인 선행 승격(Pioneer Promotion)",
             "후반 경로가 서로 다른 3명의 독립 계정이 동일 갈림길에서 Alt로 6초 단축(각 코스 runs=1 < 11)",
             ["동일 계정 반복 전송은 duel_reg_id로 1회만 인정된다", "독립 3인 합의(saved>=1.5s, margin>=15%) 시 Alt가 선행 승격된다"],
             [t(PD, f"{WWW}/navtrials_v2.rs", "CN2-50")]),
        case("CN2-51", "P4.2", "로컬 지름길 3연승 즉시 승격 및 Stage 2 드로어 가역적 핀",
             "서버 미공개 개인 지름길 3연승 직후 LocalVerdictStore nav 트랙 및 Stage 2 드로어",
             ["3연승 시 outperforms 성립으로 로컬 Premium 승격", "Stage 2 드로어 [▼] 핀 클릭 시 즉시 Discount로 전환, 재클릭 시 복원된다"],
             [t(RZ, f"{ZAPP}/pd_bridge.rs", "CN2-51")]),
        case("CN2-52", "P4.2", "WorksForMeOnly 개인 숙련 지름길 브리핑 분기 및 로컬 핀 격리",
             "서버 Discount · 로컬 3승 0패(Premium) 체크포인트",
             ["로컬/글로벌을 혼합하지 않고 WorksForMeOnly 나만의 비기 브리핑을 생성한다", "수동 핀은 outbound_votes에 포함되지 않는다"],
             [t(RZ, f"{ZVERDICT}/speak.rs", "CN2-52")]),
        # Phase 4 Part 3: Survival bias & 1-Hop & Live UIA wiring
        case("CN2-53", "P4.3", "1-Hop 경계 횡단 post_pick_windows_v2 다크패턴 함정 포착",
             "랜딩 버튼 클릭 직후 새 페이지 전환 후 8초 내 역류/이탈하는 다크패턴 세션",
             ["1-Hop 가중 감쇠(0.85) 귀속으로 랜딩 버튼이 FrictionTrap으로 분류된다"],
             [t(PD, f"{ANA}/tests/funnel.rs", "CN2-53")]),
        case("CN2-54", "P4.3", "생존자 편향 해소용 단일 체크포인트 dead_end_marks 3인 함정 격리",
             "완주율 0% 막다른 길에서 독립 3계정이 dead_end_marks 보고",
             ["경로 노출 없이 단일 체크포인트만 계정당 1회 중복 차단되어 3계정 합산 시 함정으로 격리된다"],
             [t(PD, f"{WWW}/navtrials_v2.rs", "CN2-54")]),
        case("CN2-55", "P4.3", "3-소스 라이브 web_rows 병합 및 진전 안전 2-단계 CommitGuard",
             "로컬/공개/오너 3-소스 병합 및 300ms 내 #3 더블탭 vs '결제 진행' 명시 발화",
             ["LABEL_SPACE_ID가 premium_discount_nav::LABEL_SPACE와 일치한다", "로컬 우선 병합과 2-단계 CommitGuard 더블탭 차단이 동작한다"],
             [t(RZ, f"{ZAPP}/browser_uia.rs", "CN2-55")]),
        case("CN2-56", "P4.3", "인게임 DefeatTrap 전투 침묵 → 패배 1.5s 뒤 PostDefeatComfort 함정 경고",
             "고긴장 전투 중 DefeatTrap 포착 후 패배 이벤트 발생",
             ["전투 중 무발화 유지", "패배 1500ms 후 위로+함정 경고 Lv3 PostDefeatComfort로 전달된다"],
             [t(RZ, f"{ZCORE}/coach.rs", "CN2-56")]),
        # Phase 5 Part 1: PD-Verdict v2 nav track
        case("CN2-57", "P5.1", "PD-Verdict v2 nav 트랙 14일 반감기 감쇠 및 drift_z 적응 반감기 급전환",
             "과거 Premium 코스에 최근 패치 마찰(discount) 급증 및 drift_z >= 1.5 발생",
             ["적응형 반감기 단축으로 낡은 Premium이 신속히 Discount로 전환된다"],
             [t(PD, f"{CRON}/nav_verdicts.rs", "CN2-57")]),
        case("CN2-58", "P5.1", "14일 에폭 하트비트(nav_epoch_pulses) 동일 에폭 폭주 차단 및 다음 에폭 갱신",
             "동일 계정의 5일차 재주행 vs 15일차(다음 14일 에폭) 재주행",
             ["같은 14일 에폭 내 반복은 동일 키로 충돌 차단되고, 다음 에폭 재주행은 생존 증거를 갱신한다"],
             [t(PD, f"{WWW}/navtrials_v2.rs", "CN2-58")]),
        case("CN2-59", "P5.1", "ghost_utility_v3 등급·순위 가중치 및 superseded 구코스 강등",
             "grade/rank가 다른 두 공개 코스 및 신규 코스에 패배해 superseded된 구코스",
             ["grade+rank가 반영된 ghost_utility_v3가 순위를 결정하고 superseded 코스는 명확히 강등된다"],
             [t(PD, f"{NAV}/trial.rs", "CN2-59")]),
        case("CN2-60", "P5.1", "FadedRecall 과거 경로 회상 및 신코스 붕괴 시 구코스 자가 복권",
             "14일 nav 트랙 18일 경과(1.0<=E<2.0) 및 신코스 Discount(grade<=-1, E>=3.0)",
             ["FadedRecall이 과거 Premium 경로를 회상한다", "신코스 붕괴 시 should_reinstate_standard_course가 true를 반환한다"],
             [t(RZ, f"{ZVERDICT}/speak.rs", "CN2-60")]),
        # Phase 5 Part 2: 4-Quota Stratified Portfolio & Representative Swap & Pinned-SLRU
        case("CN2-61", "P5.2", "4-쿼터 다양성 포트폴리오(Quota A/B/C/D) 고속 지름길·진입점·접근성 코스 구출",
             "인기 1~12위가 느린 함정 코스일 때 13위 정예 지름길 + ?entry= 일치 + 접근성 코스",
             ["Quota A(?entry=), Quota B(Elite), Quota D(Accessible)가 모두 탈락 없이 반환된다"],
             [t(PD, f"{WWW}/navtrials_v2.rs", "CN2-61")]),
        case("CN2-62", "P5.2", "대표 기록 차분 치환(representative_best_swap) runs 불변 보장",
             "duplicate:true 재주행자가 무마찰로 종전 대표 기록보다 1500ms 단축",
             ["nav_courses.runs 증가 없이 sums 차분 치환 및 record 갱신이 정확히 이뤄진다"],
             [t(PD, f"{WWW}/navtrials_v2.rs", "CN2-62")]),
        case("CN2-63", "P5.2", "3-계층 Pinned-SLRU 숙련·고정 코스 보호",
             "고정 코스 및 다회 숙련 코스 존재 상태에서 일회성 탐색 코스 대량 유입",
             ["고정 코스와 보호 계층 숙련 코스는 일회성 탐색 코스에 의해 축출되지 않는다"],
             [t(PD, f"{NAV}/trial.rs", "CN2-63")]),
        case("CN2-64", "P5.2", "온디바이스 64-슬롯 ZoneVault 및 batch_id 멱등 재전송 보호",
             "70개 코스 적재(고정 코스 포함) 및 미응답 배치 재전송",
             ["64슬롯 상한 내 고정 코스가 보존되고 ack 전까지 동일 batch_id가 유지된다"],
             [t(RZ, f"{ZAPP}/pd_bridge.rs", "CN2-64")]),
        # Phase 5 Part 3: Canonical Spaces, Consensus Medoid, Wallet Claim, Soft Bridge
        case("CN2-65", "P5.3", "캐노니컬 공간 레지스트리 양자화 통합 및 hash-ngram-v1 업로드 차단",
             "raon-ve-so400m-v1 / siglip2-so400m-hf-v1 정규화 및 hash-ngram-v1 / 콜론(:) 입력",
             ["동일 아키텍처 양자화 변형이 단일 공간 ID로 통일되고 hash-ngram-v1은 업로드 차단된다"],
             [t(PD, f"{NAV}/mark.rs", "CN2-65")]),
        case("CN2-66", "P5.3", "2-단계 합의 대표 벡터(Consensus Medoid) 초기 오염 축출 및 3인 잠금 + Int8 Base64",
             "최초 1인 오염 벡터 등록 후 정상 독립 주행자 3인의 벡터 도착",
             ["충돌 후보 2인 합의로 오염 초기 벡터가 자동 교체되고 3인 합의 시 잠금 및 Int8 Base64 왕복 cos>0.999 유지"],
             [t(PD, f"{WWW}/navtrials_v2.rs", "CN2-66")]),
        case("CN2-67", "P5.3", "지갑→세션 무중복 승격(claim-wallet) 세션 등록 선점",
             "지갑 계정 2개 코스를 세션 계정으로 claim-wallet 이관",
             ["plan_wallet_claim_registrations가 세션 등록 키를 사전 생성해 재주행 시 공용 runs 부풀림 0건 보장"],
             [t(PD, f"{WWW}/navtrials_v2.rs", "CN2-67")]),
        case("CN2-68", "P5.3", "이중 중심화 soft_checkpoint_bridge DOM 리팩터링·인게임 ROI 복구",
             "해시가 바뀐 DOM/ROI 벡터 쌍에 대한 1:1 소프트 매칭",
             ["이중 중심화 + 배타 1:1 할당으로 변경된 체크포인트가 오매칭 없이 복구된다"],
             [t(PD, f"{NMS}/spatial.rs", "CN2-68")]),
        # Phase 6 Part 1: Cross-App Meta-Course & Relay Federation
        case("CN2-69", "P6.1", "크로스-앱 릴레이 연합(curate_relay_federation) 다국어 명칭 연결 및 서버 supersede 0건",
             "영문 위키 노트(Malenia)와 한글 인게임 노트(말레니아) 간 공유 RelayKey 연합",
             ["서로 다른 cc 간 연합 링크가 생성되되 서버 supersede 패킷은 0건을 유지한다"],
             [t(PD, f"{RELAY}/lib.rs", "CN2-69")]),
        case("CN2-70", "P6.1", "TopicTracker 선택적 크로스-앱 브리지 및 CR-02 PLINKO 단절 보존",
             "공통 RelayKey 없는 메모장 전환 vs 공통 RelayKey 있는 위키→게임 전환",
             ["무관한 앱 전환은 salience 0.0 단절을 유지하고, 공유 RelayKey 전환에서만 토픽이 복원된다"],
             [t(RZ, f"{ZCORE}/reference.rs", "CN2-70")]),
        case("CN2-71", "P6.1", "LocalMirrorIndex 크로스-앱 브리지 노트 인게임 검색 및 타 앱 격리",
             "fextralife.com 노트를 eldenring.exe로만 브리지한 뒤 게임·메모장에서 각각 검색",
             ["게임에서는 브리지된 위키 노트가 검색되고 무관한 앱에서는 0건으로 격리된다"],
             [t(RZ, f"{ZAPP}/pd_bridge.rs", "CN2-71")]),
        case("CN2-72", "P6.1", "크로스-앱 메타 궤적 승리 크레딧 역전파(게임 승리 → 위키 노트 귀속)",
             "fextralife.com → eldenring.exe 궤적 간선 기록 후 인게임 승리 이벤트 발생",
             ["active_in_meta_trajectory가 위키 노트를 회수해 인게임 승리가 위키 노트 전적(wins+=1)에 귀속된다"],
             [t(RZ, f"{ZAPP}/pd_bridge.rs", "CN2-72")]),
        # Phase 6 Part 2: zena-lexicon 2-Tier & Safe --learn & Reflex Distillation
        case("CN2-73", "P6.2", "닫힌 nav.* 아키타입 5종 및 Tier 2 scoped_bindings 재시딩 보존",
             "data/zena_lexicon.json nav.* 5종 로드 및 eldenring.exe 은어 3세션 승격 후 import 재실행",
             ["nav.* 5종은 text_nodes()에서 제외된다", "앱 은어는 글로벌을 오염시키지 않고 재시딩 후에도 보존된다"],
             [t(RZ, f"{ZLEX}/store.rs", "CN2-73")]),
        case("CN2-74", "P6.2", "Lv3 → 0ms Lv2 반사 신경 증류(ReflexCompiler)",
             "동일 (cc, screen, anchor) Lv3 코칭 2회 연속 검증 수락 후 재발생 및 철회",
             ["2회 수락 뒤 LLM을 깨우지 않고 0ms Lv2 캐시 응답으로 전환되며 revoke 시 Lv3로 복귀한다"],
             [t(RZ, f"{ZCORE}/coach.rs", "CN2-74")]),
        case("CN2-75", "P6.2", "TextTower 없는 시각 프로토타입 축적 및 전투/저RAM 재임베딩 지연 가드",
             "전투 중·저RAM(<650MiB) 가드 체크 및 3회 일관된 비프라이버시 이미지 벡터 축적",
             ["전투/주행/저RAM에서는 재임베딩이 지연되고, 프라이버시 버퍼(±10s) 밖 3회 일치 시 단위 프로토타입이 생성된다"],
             [t(RZ, f"{ZAPP}/lexicon.rs", "CN2-75")]),
        case("CN2-76", "P6.2", "4-게이트 안전 --learn(Command Air-Gap · PII 스크럽 · 2회 거부 자동 철회 · Privacy 불변)",
             "system.exit_app 자동학습 시도, 이메일/0x해시, 저마진, 3세션 승격 후 2회 거부, vision.private 덮어쓰기 시도",
             ["Command Air-Gap·PII·저마진은 모두 거부되고 2회 거부 시 자동 철회+감사 로그 기록, vision.private는 불변 유지"],
             [t(RZ, f"{ZLEX}/store.rs", "CN2-76")]),
        # Phase 6 Part 3: End-to-End Hybrid RAG x Telemetry Closure
        case("CN2-77", "P6.3", "다계층 어뷰징 방어 통합(1인 5변종 우회 + 지갑→세션 승격 + batch_id 멱등성)",
             "단일 계정 5변종 우회, 지갑→세션 승격, 배치 재전송 검증",
             ["표 증폭 0건, 런 부풀림 0건, 배치 키 검증 통과"],
             [t(PD, f"{WWW}/navtrials_v2.rs", "CN2-77")]),
        case("CN2-78", "P6.3", "파이프라인 라이브 web_rows · checkpoint_to · focused · CourseBrief 종단 배선",
             "라이브 NavBadgeOverlay 활성 상태에서 ScreenAnchorSnapshot 및 CourseBrief 생성",
             ["checkpoint_to와 focused가 실제 활성 배지로 채워지고 CourseBrief가 프롬프트에 연결된다"],
             [t(RZ, f"{ZAPP}/pipeline.rs", "CN2-78")]),
        case("CN2-79", "P6.3", "하이브리드 RAG(90일) × 실측 텔레메트리(14일) 패치 모순 4사분면 중재기",
             "구버전 공략 Premium vs 최근 14일 실측 Discount 충돌 및 원본 해시 포함 라벨",
             ["RecentPatchCaution 경고로 자동 전환되고 원본 0x 해시는 완전히 제거된다"],
             [t(RZ, f"{ZCORE}/brief.rs", "CN2-79")]),
        case("CN2-80", "P6.3", "종단 간 하이브리드 폐루프 0.5 / 0.5 시너지 크레딧 귀속",
             "동일 턴에서 RAG 공략('tactics')과 코스 체크포인트('route')를 동시 노출 후 승리",
             ["RagRecord와 CourseCheckpoint가 상호보완 0.5/0.5 크레딧으로 동시 승리 기록된다"],
             [t(RZ, f"{ZAPP}/pipeline.rs", "CN2-80")]),
    ]
    assert len(cases) == 36
    return {
        "contract": "course_nav_v2",
        "scenario": "phases_4_to_6",
        "title": "코스·체크포인트 N:N 심층 고도화 (Phase 4 ~ Phase 6)",
        "source": "최종_코스와 체크포인트 N_N 시나리오 목차.docx (CN2-45 ~ CN2-80)",
        "params": {
            "pace_eps": 0.05,
            "pace_deadband_ms": 500,
            "pioneer_min_wins": 3,
            "pioneer_min_saved_ms": 1500,
            "pioneer_min_margin": 0.15,
            "nav_half_life_days": 14,
            "drift_half_life_trigger_z": 1.5,
            "zone_vault_max_slots": 64,
            "consensus_lock_confirmations": 3,
            "safe_learn_promote_sessions": 3,
            "safe_learn_revoke_rejects": 2,
            "reflex_distill_min_accepts": 2,
        },
        "cases": cases,
    }


def accessible_nav_v1():
    cases = [
        case("AC-01", "P7.1", "진전·지연 릴리스 탄성 획 Press 수렴 및 80ms 경직 바운스 광클 면책",
             "KinematicProfile(tremor_px=30, release_ms=900) 하에서 wander=28px, dur=820ms 입력 및 60ms 바운스 3회",
             ["@hold 분열 없이 표준 Gesture::Press로 수렴한다", "80ms 미만 경직 바운스는 RageClick에서 면책된다"],
             [t(PD, f"{NAV}/gesture.rs", "AC-01")]),
        case("AC-02", "P7.1", "#0 배지 뱅크 페이징 발화 잠금 보존 및 10~15번 요소 도달 + 3x3 재귀 GridInvoke",
             "15개 요소 화면에서 발화 잠금(#2) 유지 중 뱅크 1 전환 및 3x3 격자 분할",
             ["잠긴 #2 번호가 뱅크 전환 시 보존된다", "10~15번 초과 요소에 도달 가능하며 3x3 grid_subrect가 정확히 분할된다"],
             [t(PD, f"{NAV}/order.rs", "AC-02")]),
        case("AC-03", "P7.4", "개인 속도비(ρ_u) 정규화 shortcut_wins_v2 및 Zero-Health-Telemetry 물리 특성 비트",
             "개인 기저 속도비 ρ_u=3.0 사용자의 무마찰 33s 우회 완주(고스트 15s) 및 코스 TRAIT 비트 산출",
             ["정규화 시간 11s < 15s로 지름길 승리가 인정된다", "장애 정보 전송 0건으로 물리 특성 비트 기반 접근성 라우팅이 동작한다"],
             [t(PD, f"{NAV}/trial.rs", "AC-03")]),
        case("AC-04", "P7.2", "스크린리더 20s 잠금 연장 · 3D 공간 오디오 비콘 · 확대경 뷰포트 추종 도킹",
             "스크린리더 활성 시 SpokenSlotLock(20s), 우상단 극좌표 음향화, 3배 확대경 내 비가림 도킹",
             ["발화 잠금이 20초간 유지된다", "우상단 극좌표가 우측 패닝·고피치로 변환되고 확대경 내 대상을 가리지 않는 모서리에 도킹된다"],
             [t(RZ, f"{ZCORE}/nav_target.rs", "AC-04")]),
        case("AC-05", "P7.3", "구음장애·말더듬 분할 발화 병합기(CR-06 명시 취소 보존)",
             "900ms 간격으로 끊긴 '그...' → '어... 2번' → '눌러줘' 및 '아니 취소 1번으로'",
             ["미완결 조각이 단일 질의 '그 2번 눌러줘'로 병합된다", "CR-06 명시적 취소/정정 시 기존 버퍼가 즉시 폐기·교체된다"],
             [t(RZ, f"{ZCORE}/reference.rs", "AC-05")]),
        case("AC-06", "P7.3", "차분한 나침반(Calm Compass) 스톱워치 문구 제거 및 프리픽스 캐시 불변",
             "calm_compass=true 상태에서 vs_best=+4.5s, vs_public=+8.2s CourseBrief 렌더링",
             ["초·느림·빠름 등 스톱워치 압박 문구 0건", "시스템+도구 프리픽스 캐시가 바이트 단위로 100% 동일하다"],
             [t(RZ, f"{ZCORE}/brief.rs", "AC-06")]),
    ]
    assert len(cases) == 6
    return {
        "contract": "accessible_nav_v1",
        "scenario": "phase_7_accessibility",
        "title": "포용적 접근성 내비게이션 계약 (Phase 7)",
        "source": "최종_코스와 체크포인트 N_N 시나리오 목차.docx (AC-01 ~ AC-06)",
        "params": {
            "spastic_bounce_ms": 80,
            "hold_shift_margin_ms": 450,
            "spoken_lock_hold_sr_ms": 20000,
            "screen_reader_accept_window_ms": 35000,
            "speech_coalesce_window_ms": 2500,
            "reading_ms_per_char": 120,
            "quota_accessible": 4,
        },
        "cases": cases,
    }


ALL = [course_nav_v2, accessible_nav_v1]


def main():
    for build in ALL:
        body = build()
        out = HERE / f"{body['contract']}.json"
        out.write_text(json.dumps(body, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(f"wrote {out.name} ({len(body['cases'])} cases)")


if __name__ == "__main__":
    main()
