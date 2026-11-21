#!/usr/bin/env python3
# 시나리오 계약(gen_scenario_contracts_v1.py 가 만든 여섯 파일)의 사례마다, 그것을 확인하는 시험이 정말 있는지 본다.
#
# 실행: python contracts/check_scenario_contracts.py   (premium-discount 폴더에서)
#
# - owners 의 path(파일 또는 폴더) 안에 `fn <test>`(Rust) 또는 `<test>`(JS 시험 이름)로 시작하는 시험이 하나 이상 있어야 한다.
# - raon-zena 의 owners 는 옆 폴더(../raon-zena, 또는 RAON_ZENA_DIR)가 있을 때만 본다. 없으면 건너뛰었다고 알린다.
# - 파일이 생성기와 다르면(생성기를 고치고 다시 돌리지 않았으면) 실패한다.
# - `pending` 이 있는 사례(남은 일이 적힌 사례)는 실패로 세지 않고 목록으로 알린다.
# 하나라도 없으면 종료 코드 1.

import json
import os
import re
import sys
from pathlib import Path

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
PD_ROOT = HERE.parent
RZ_ROOT = Path(os.environ.get("RAON_ZENA_DIR", PD_ROOT.parent / "raon-zena"))
FILES = ["course_nav_v1", "proactive_friction_v1", "contextual_rag_v1", "feedback_credit_v1", "web_analytic_v1", "privacy_shield_v1"]


def sources(path: Path):
    if path.is_file():
        yield path
    elif path.is_dir():
        for p in sorted(path.rglob("*")):
            if p.suffix in (".rs", ".js", ".mjs", ".ts") and "target" not in p.parts and "node_modules" not in p.parts:
                yield p


def has_test(path: Path, prefix: str) -> bool:
    rust = re.compile(r"\bfn\s+" + re.escape(prefix) + r"\w*\s*\(")
    js = re.compile(r"""\b(?:test|it)\s*\(\s*['"`]""" + re.escape(prefix))
    for p in sources(path):
        text = p.read_text(encoding="utf-8", errors="replace")
        if rust.search(text) or js.search(text):
            return True
    return False


def main():
    # Windows 콘솔(cp949)에서도 한글 사유가 깨지지 않게
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    # 생성기가 쓰는 내용과 파일이 같은가
    sys.path.insert(0, str(HERE))
    import gen_scenario_contracts_v1 as gen  # noqa: E402

    problems, skipped, checked, pending = [], 0, 0, []
    for build in gen.ALL:
        body = build()
        name = body["contract"]
        on_disk = json.loads((HERE / f"{name}.json").read_text(encoding="utf-8"))
        if on_disk != body:
            problems.append(f"{name}.json is stale: run python contracts/gen_scenario_contracts_v1.py")
        for c in body["cases"]:
            if c.get("pending"):
                pending.append(f"{c['id']}: {c['pending']}")
            for o in c["owners"]:
                root = PD_ROOT if o["repo"] == "premium-discount" else RZ_ROOT
                if o["repo"] == "raon-zena" and not RZ_ROOT.exists():
                    skipped += 1
                    continue
                checked += 1
                target = root / o["path"]
                if not target.exists():
                    problems.append(f"{c['id']}: {o['repo']}/{o['path']} does not exist")
                elif not has_test(target, o["test"]):
                    problems.append(f"{c['id']}: no test starting with {o['test']} in {o['repo']}/{o['path']}")
    for p in pending:
        print("PENDING", p)
    for p in problems:
        print("FAIL", p)
    print(f"{checked} owner tests checked, {skipped} skipped (raon-zena not found at {RZ_ROOT})" if skipped else f"{checked} owner tests checked")
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
