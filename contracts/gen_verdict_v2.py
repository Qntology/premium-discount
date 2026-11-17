#!/usr/bin/env python3
# PD-Verdict v2 참조 구현. contracts/verdict_v2.json(매개변수 + 사례 + 기대값)을 만든다.
#
# 실행: python contracts/gen_verdict_v2.py   (premium-discount 폴더에서. 파일을 다시 쓴다)
#
# v2 는 v1 의 규칙(gen_verdict_v1.py 의 evidence · judge · verdict)을 그대로 쓰고 매개변수만 넓힌다.
# 1. 관측 종류 implicit (가중치 0.3): 접근성 내비게이션의 행동 로그가 에피소드마다 남기는 관측.
#    에피소드의 성공 · 마찰이 스코프 기준선에서 SDS 띠를 넘을 때만 생기고, 부호는 +1(premium 쪽) 또는 −1(discount 쪽)이다.
#    사용자가 입력하지 않아도 생기는 관측이라 표(1.0)보다 가볍게 둔다. 행동만으로는 에피소드 7개(E = 2.1)부터 판정된다
# 2. 트랙 nav: 화면 · 영역 판정. web 트랙과 같은 값으로 시작한다 (반감기 14일, min_obs secondary 8 · primary 4 · global 12)
#    스코프는 영역이면 secondary = cc + 화면 키, primary = cc 이고, 화면이면 secondary 없이 primary = cc 다
#
# v1 의 사례는 v2 에서도 결과가 같아야 한다 (그 사례가 쓰는 트랙 · 종류의 값이 같으므로). main()이 확인한다.
# 서버(cron/src/verdict.ts)는 아직 v1 로 판정한다. TypeScript 구현은 이 파일의 params 를 넘겨 v2 사례를 통과한다
# (cron/test/verdict_v2.test.ts). premium-discount 의 crates/premium-discount-verdict 는 Params::v2() 로 같은 사례를 통과한다.

import copy
import json
import sys
from pathlib import Path

sys.dont_write_bytecode = True  # contracts/ 에 __pycache__ 를 남기지 않는다
sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen_verdict_v1 as v1  # noqa: E402

RECIPE = "pd-verdict/2"
IMPLICIT = "implicit"
NAV = "nav"

PARAMS = copy.deepcopy(v1.PARAMS)
PARAMS["tracks"][NAV] = copy.deepcopy(v1.PARAMS["tracks"]["web"])
PARAMS["weights"][IMPLICIT] = 0.3


def ob(sign, days_ago=0.0, kind=IMPLICIT, credit=1.0, weight=None):
    w = PARAMS["weights"][kind] if weight is None else weight
    return {"kind": kind, "sign": sign, "weight": w, "credit": credit, "at_ms": int(v1.NOW - days_ago * v1.DAY)}


bl = v1.bl
case = v1.case

NEW_CASES = [
    case("implicit_alone_needs_seven_episodes", NAV, [ob(1)] * 6, {"secondary": bl(9, 0.0, 0.2)},
         note="implicit 6개: E = 1.8 < min_evidence. 행동만으로는 에피소드 7개부터 판정한다"),
    case("implicit_premium_on_screen", NAV, [ob(1, d) for d in range(10)], {"secondary": bl(9, 0.05, 0.2)},
         note="열흘 동안 매일 성공 에피소드. 같은 화면의 다른 영역보다 마찰이 적다"),
    case("implicit_friction_discount_primary_backoff", NAV, [ob(-1, d) for d in range(10)],
         {"secondary": bl(3, 0.0, 0.1), "primary": bl(5, 0.0, 0.15)},
         note="화면의 영역이 3개뿐이라 secondary(n < 8)를 건너뛰고 앱 전체(primary)와 비교"),
    case("votes_outweigh_implicit", NAV, [ob(1, 0, "vote"), ob(1, 0, "vote")] + [ob(-1, d) for d in range(4)],
         {"secondary": bl(9, 0.0, 0.2)},
         note="추천 표 2개(2.0)가 마찰 implicit 4개(약 1.1)보다 무겁다"),
    case("implicit_mixed_signs_stay_neutral", NAV, [ob(1, d) for d in range(5)] + [ob(-1, d) for d in range(5)],
         {"secondary": bl(9, -0.3, 0.2)},
         note="성공과 마찰이 같으면 v = 0: 순위가 높아도 floor 에 막혀 neutral"),
    case("implicit_decays_over_two_weeks", NAV, [ob(1, 14)] * 8, {"secondary": bl(9, 0.0, 0.2)},
         note="반감기(14일) 전의 implicit 8개는 근거 1.2 뿐이다"),
]

CASES = v1.CASES + NEW_CASES


def verdict(c):
    return v1.verdict(c, PARAMS)


def main():
    # v1 사례는 v2 매개변수로도 같은 결과여야 한다
    for c in v1.CASES:
        if verdict(c) != v1.verdict(c):
            raise SystemExit(f"v1 사례 {c['name']} 의 결과가 v2 에서 달라졌다")
    out = {
        "recipe": RECIPE,
        "generated_by": "contracts/gen_verdict_v2.py",
        "tolerance": 1e-9,
        "params": PARAMS,
        "cases": [dict(c, expect=verdict(c)) for c in CASES],
    }
    path = Path(__file__).with_name("verdict_v2.json")
    text = json.dumps(out, ensure_ascii=False, indent=2) + "\n"
    path.write_text(text, encoding="utf-8", newline="\n")
    for c in out["cases"][len(v1.CASES):]:
        x = c["expect"]
        rank = "null" if x["rank"] is None else f"{x['rank']:+.4f}"
        print(f"{c['name']:<44} E={x['evidence']:.4f} v={x['value']:+.4f} r={rank:>7} {x['tier']:<8} grade={x['grade']} {x['reasons']}")
    print(f"사례 {len(CASES)}개 (v1 {len(v1.CASES)}개 + v2 {len(NEW_CASES)}개)")


if __name__ == "__main__":
    main()
