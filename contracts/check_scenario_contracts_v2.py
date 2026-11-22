#!/usr/bin/env python3
# Phase 4 ~ Phase 7 시나리오 계약(course_nav_v2.json, accessible_nav_v1.json) 소유 시험 검증기.
#
# 실행: python contracts/check_scenario_contracts_v2.py   (premium-discount 폴더에서)

import json
import os
import re
import sys
from pathlib import Path

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
PD_ROOT = HERE.parent
RZ_ROOT = Path(os.environ.get("RAON_ZENA_DIR", PD_ROOT.parent / "raon-zena"))


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
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    sys.path.insert(0, str(HERE))
    import gen_scenario_contracts_v2 as gen  # noqa: E402

    problems, skipped, checked = [], 0, 0
    for build in gen.ALL:
        body = build()
        name = body["contract"]
        on_disk = json.loads((HERE / f"{name}.json").read_text(encoding="utf-8"))
        if on_disk != body:
            problems.append(f"{name}.json is stale: run python contracts/gen_scenario_contracts_v2.py")
        for c in body["cases"]:
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
    for p in problems:
        print("FAIL", p)
    print(
        f"{checked} v2 owner tests checked, {skipped} skipped"
        if skipped
        else f"{checked} v2 owner tests checked"
    )
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
