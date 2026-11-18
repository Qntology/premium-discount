#!/usr/bin/env python3
# PD-Verdict v1 참조 구현. contracts/verdict_v1.json(매개변수 + 사례 + 기대값)을 만든다.
#
# 실행: python contracts/gen_verdict_v1.py   (premium-discount 폴더에서. 파일을 다시 쓴다)
#
# 이 파일이 계약의 기준이다. 서버(workers/cron/src/verdict.ts)와 raon-zena(crates/zena-verdict)는 이 파일이 만든
# verdict_v1.json의 사례를 그대로 통과해야 한다 (실수는 1e-9 안, 나머지는 정확히 같게).
# 매개변수를 하나라도 바꾸면 RECIPE 이름을 올리고(pd-verdict/2 …) 새 파일로 낸다. 판정 행은 recipe를 남기므로
# recipe가 다른 판정끼리는 비교하지 않는다.
#
# 규칙 (contracts/README.md에 같은 내용을 문장으로 적었다)
# 1. 관측마다 u = weight × credit × 2^(−age/H). age는 now − at_ms (미래 시각은 0). sign 0(철회)은 세지 않는다
# 2. E = Σu, v = Σ(u × sign) / (E + k)
# 3. 기준선: secondary → primary → global 중 n ≥ min_obs인 첫 단계. 있으면 r = tanh((v − mean) / (sd + eps))
# 4. 등급: 대체됨 → discount / E < min_evidence → unrated / 들어가기(premium, discount) / 머무르기(직전 등급) / neutral
# 5. grade: premium +2(r ≥ strong) 또는 +1, neutral 0, discount −2(대체됨 또는 r ≤ −strong) 또는 −1, unrated null

import json
import math
from pathlib import Path

RECIPE = "pd-verdict/1"
DAY = 86_400_000
LEVELS = ("secondary", "primary", "global")

PARAMS = {
    "k": 2.0,
    "eps": 0.05,
    "enter": 0.5,
    "exit": 0.3,
    "floor": 0.1,
    "strong": 0.8,
    "min_evidence": 2.0,
    "duel_rate": 0.65,
    "duel_min": 3,
    # 트랙: pd = 서버(여러 사용자의 투표), 나머지는 raon-zena 로컬 기록 종류
    "tracks": {
        "pd": {"half_life_ms": 90 * DAY, "min_obs": {"secondary": 5, "primary": 10, "global": 30}},
        "drop": {"half_life_ms": 14 * DAY, "min_obs": {"secondary": 6, "primary": 3, "global": 10}},
        "web": {"half_life_ms": 14 * DAY, "min_obs": {"secondary": 8, "primary": 4, "global": 12}},
        "chat": {"half_life_ms": 14 * DAY, "min_obs": {"secondary": 6, "primary": 3, "global": 10}},
        "coach": {"half_life_ms": 14 * DAY, "min_obs": {"secondary": 6, "primary": 3, "global": 10}},
    },
    # 관측 종류별 기본 가중치. 관측을 만드는 쪽이 이 값을 weight에 넣는다 (판정 함수는 weight만 본다)
    "weights": {
        "vote": 1.0,
        "vote_outcome": 0.5,
        "manual": 2.0,
        "feedback": 1.0,
        "outcome": 1.0,
        "reaction": 0.6,
        "engage": 0.2,
        "model": 0.3,
    },
}


def evidence(obs, now_ms, half_life_ms):
    """(E, v 분자)"""
    e = s = 0.0
    for o in obs:
        if o["sign"] == 0:
            continue
        age = max(0, now_ms - o["at_ms"])
        u = o["weight"] * o["credit"] * 2.0 ** (-age / half_life_ms)
        e += u
        s += u * o["sign"]
    return e, s


def judge(e, v, case, p=PARAMS):
    track = p["tracks"][case["track"]]
    level = base = None
    for lv in LEVELS:
        b = case.get("baseline", {}).get(lv)
        if b is not None and b["n"] >= track["min_obs"][lv]:
            level, base = lv, b
            break
    r = math.tanh((v - base["mean"]) / (base["sd"] + p["eps"])) if base is not None else None
    win = loss = None
    duel = case.get("duel")
    if duel is not None and duel["wins"] + duel["losses"] >= p["duel_min"]:
        meetings = duel["wins"] + duel["losses"]
        win, loss = duel["wins"] / meetings, duel["losses"] / meetings
    prev = case["prev"]["tier"]
    superseded = case.get("superseded", False)

    reasons = []
    if superseded:
        tier = "discount"
        reasons.append("superseded")
    elif e < p["min_evidence"]:
        tier = "unrated"
        reasons.append("low_evidence")
    else:
        rank_high = r is not None and r >= p["enter"]
        rank_low = r is not None and r <= -p["enter"]
        duel_win = win is not None and win >= p["duel_rate"]
        duel_loss = loss is not None and loss >= p["duel_rate"]
        if v >= p["floor"] and (rank_high or duel_win):
            tier = "premium"
            reasons.append("rank_high" if rank_high else "duel_win")
        elif v <= -p["floor"] and (rank_low or duel_loss):
            tier = "discount"
            reasons.append("rank_low" if rank_low else "duel_loss")
        elif prev == "premium" and v > 0 and ((r is not None and r >= p["exit"]) or (r is None and duel_win)):
            tier = "premium"
            reasons.append("held")
        elif prev == "discount" and v < 0 and ((r is not None and r <= -p["exit"]) or (r is None and duel_loss)):
            tier = "discount"
            reasons.append("held")
        else:
            tier = "neutral"
            if rank_high or rank_low or duel_win or duel_loss:
                reasons.append("floor")
    reasons.append(f"level:{level}" if level else "no_baseline")

    if tier == "premium":
        grade = 2 if r is not None and r >= p["strong"] else 1
    elif tier == "discount":
        grade = -2 if superseded or (r is not None and r <= -p["strong"]) else -1
    elif tier == "neutral":
        grade = 0
    else:
        grade = None
    since = case["prev"]["since_ms"] if tier == prev else case["now_ms"]
    return {"evidence": e, "value": v, "level": level, "rank": r, "tier": tier, "grade": grade, "since_ms": since, "reasons": reasons}


def verdict(case, p=PARAMS):
    track = p["tracks"][case["track"]]
    e, s = evidence(case["obs"], case["now_ms"], track["half_life_ms"])
    v = s / (e + p["k"])
    return judge(e, v, case, p)


NOW = 1763251200000


def ob(sign, days_ago=0.0, kind="vote", credit=1.0, weight=None):
    w = PARAMS["weights"][kind] if weight is None else weight
    return {"kind": kind, "sign": sign, "weight": w, "credit": credit, "at_ms": int(NOW - days_ago * DAY)}


def bl(n, mean, sd):
    return {"n": n, "mean": mean, "sd": sd}


def case(name, track, obs, baseline=None, prev=("neutral", 0), duel=None, superseded=False, note=""):
    c = {"name": name, "note": note, "now_ms": NOW, "track": track, "prev": {"tier": prev[0], "since_ms": prev[1]}, "obs": obs, "baseline": baseline or {}}
    if duel is not None:
        c["duel"] = {"wins": duel[0], "losses": duel[1]}
    if superseded:
        c["superseded"] = True
    return c


HELD_SINCE = 1762000000000

CASES = [
    case("premium_by_rank_in_secondary_scope", "pd", [ob(1, 0), ob(1, 1), ob(1, 8)], {"secondary": bl(6, 0.05, 0.2)},
         note="오늘 · 1일 전 · 8일 전 추천 3표. r ≥ strong 이라 grade +2"),
    case("unrated_with_one_vote", "pd", [ob(1)], {"secondary": bl(6, 0.05, 0.2)},
         note="E = 1 < min_evidence. 기준선이 있으므로 순위는 계산하지만 등급은 unrated"),
    case("withdrawn_vote_is_ignored", "pd", [ob(1), ob(1), ob(0)], {"secondary": bl(5, 0.1, 0.2)},
         note="sign 0 은 E 에도 v 에도 들어가지 않는다"),
    case("neutral_near_baseline", "pd", [ob(1), ob(1), ob(-1), ob(1)], {"secondary": bl(7, 0.3, 0.2)}),
    case("floor_blocks_premium", "pd", [ob(1)] * 4 + [ob(-1)] * 4, {"secondary": bl(5, -0.5, 0.2)},
         note="순위는 높지만 v = 0 < floor 라 추천이 아니다"),
    case("discount_by_rank_primary_backoff", "pd", [ob(-1)] * 3, {"secondary": bl(3, 0.0, 0.1), "primary": bl(12, 0.2, 0.3)},
         note="secondary n 3 < 5 라 primary 로 넘어감. r ≤ −strong 이라 grade −2"),
    case("global_backoff_discount", "pd", [ob(-1), ob(-1), ob(1), ob(-1)],
         {"secondary": bl(2, 0.0, 0.1), "primary": bl(4, 0.0, 0.1), "global": bl(40, 0.1, 0.4)}),
    case("no_baseline_stays_neutral", "pd", [ob(1)] * 3, note="표본이 없으면 단정하지 않는다"),
    case("duel_win_without_baseline", "drop", [ob(1, 0, "manual"), ob(1, 1, "outcome", credit=0.5)], duel=(4, 1),
         note="기준선 없이 대결 승률 0.8 로 추천. 순위가 없으므로 grade +1"),
    case("duel_needs_three_meetings", "drop", [ob(1, 0, "manual"), ob(1, 0, "feedback")], duel=(2, 0)),
    case("duel_loss_without_baseline", "drop", [ob(-1, 0, "reaction"), ob(-1, 0, "reaction"), ob(-1, 0, "outcome")],
         {"secondary": bl(2, 0.0, 0.1), "primary": bl(2, 0.0, 0.1), "global": bl(5, 0.0, 0.1)}, duel=(1, 3)),
    case("superseded_is_discount", "drop", [ob(1, 0, "engage")], superseded=True,
         note="근거가 모자라도 대체된 기록은 discount, grade −2"),
    case("hysteresis_holds_premium", "pd", [ob(1), ob(1), ob(1), ob(-1)], {"secondary": bl(6, 0.25, 0.2)}, prev=("premium", HELD_SINCE),
         note="r 이 exit 와 enter 사이: 직전 premium 을 유지하고 since 도 그대로"),
    case("hysteresis_releases_premium", "pd", [ob(1), ob(1), ob(1), ob(-1)], {"secondary": bl(6, 0.28, 0.2)}, prev=("premium", HELD_SINCE),
         note="r < exit: neutral 로 내려가고 since = now"),
    case("hysteresis_holds_discount", "pd", [ob(-1), ob(-1), ob(-1), ob(1)], {"secondary": bl(6, -0.25, 0.2)}, prev=("discount", HELD_SINCE)),
    case("decay_half_life_boundary", "pd", [ob(1, 90)] * 4, {"secondary": bl(5, 0.0, 0.3)},
         note="반감기 하나 전의 4표: E = 2.0 = min_evidence (경계는 판정 가능)"),
    case("future_timestamp_counts_as_now", "pd", [ob(1, -1), ob(1, 0)], note="시계가 앞선 관측은 나이 0"),
    case("zero_weight_counts_nothing", "pd", [ob(1, weight=0.0), ob(1, weight=0.0), ob(1)], {"secondary": bl(6, 0.0, 0.2)},
         note="게스트 · 자기 글 표(가중치 0)는 근거가 되지 않는다"),
    case("local_mixed_kinds_primary_level", "chat",
         [ob(1, 0, "feedback", credit=0.5), ob(1, 2, "reaction"), ob(1, 0, "engage"), ob(-1, 0, "model"), ob(1, 1, "manual")],
         {"secondary": bl(0, 0.0, 0.0), "primary": bl(3, 0.1, 0.15)}),
]


def main():
    out = {
        "recipe": RECIPE,
        "generated_by": "contracts/gen_verdict_v1.py",
        "tolerance": 1e-9,
        "params": PARAMS,
        "cases": [dict(c, expect=verdict(c)) for c in CASES],
    }
    path = Path(__file__).with_name("verdict_v1.json")
    text = json.dumps(out, ensure_ascii=False, indent=2) + "\n"
    path.write_text(text, encoding="utf-8", newline="\n")
    for c in out["cases"]:
        x = c["expect"]
        rank = "null" if x["rank"] is None else f"{x['rank']:+.4f}"
        print(f"{c['name']:<36} E={x['evidence']:.4f} v={x['value']:+.4f} r={rank:>7} {x['tier']:<8} grade={x['grade']} {x['reasons']}")


if __name__ == "__main__":
    main()
