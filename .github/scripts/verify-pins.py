#!/usr/bin/env python3
"""Verify 3-Tier pin consistency, single candle-rocm-vulkan commit, zero floating branches,
zero cross-repo local path patches, and Tier 0 vision purity. Exits 2 on any violation."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXPECTED_CANDLE_REV = "f73415d4d149d7949e8a6a94911af670e4e10f7f"
EXPECTED_PD_REV = "b024dbb5e12bdb4fc1fcf83d0d62e5a645baccb5"
EXPECTED_LV_REV = "54b161b3f182ccef867ff9fafa1c684946be2611"
FORBIDDEN_CANDLE_REVS = ("ebaf93d", "3abdfab7")


def iter_cargo_manifests(base: Path) -> list[Path]:
    out: list[Path] = []
    for p in sorted(base.rglob("Cargo.toml")):
        if any(part in ("target", ".git", "node_modules", "vendor") for part in p.parts):
            continue
        out.append(p)
    return out


def strip_comment(line: str) -> str:
    in_quote = False
    escape = False
    for idx, ch in enumerate(line):
        if escape:
            escape = False
            continue
        if ch == "\\" and in_quote:
            escape = True
        elif ch == '"':
            in_quote = not in_quote
        elif ch == "#" and not in_quote:
            return line[:idx]
    return line


def check_manifest_rules(manifest: Path, repo_root: Path, errs: list[str], candle_revs: set[str]) -> None:
    rel = manifest.relative_to(repo_root).as_posix()
    if repo_root.name == "logis-center" and rel == "tools/cpu-embed-bench/Cargo.toml":
        return
    allow_parent_crates = (
        (rel == "apps/Cargo.toml" and repo_root.name == "premium-discount")
        or (rel.startswith("apps/") and repo_root.name == "louis-vision")
        or ((rel.startswith("apps/") or rel.startswith("crates/")) and repo_root.name == "logis-center")
    )
    text = manifest.read_text(encoding="utf-8", errors="replace")
    for lineno, raw in enumerate(text.splitlines(), 1):
        code = strip_comment(raw).strip()
        if not code:
            continue
        if re.search(r"\bbranch\s*=", code):
            errs.append(f"{repo_root.name}/{rel}:{lineno}: forbidden floating 'branch =' reference")
        for m in re.finditer(r'\bpath\s*=\s*"([^"]+)"', code):
            target = m.group(1)
            if target.startswith(".."):
                resolved = (manifest.parent / target).resolve()
                try:
                    resolved.relative_to(repo_root.resolve())
                except ValueError:
                    errs.append(f"{repo_root.name}/{rel}:{lineno}: cross-repo local path '{target}'")
                if not allow_parent_crates and "../.." in target:
                    errs.append(f"{repo_root.name}/{rel}:{lineno}: disallowed multi-parent path '{target}'")
        if "candle-rocm-vulkan" in code:
            for bad in FORBIDDEN_CANDLE_REVS:
                if bad in code:
                    errs.append(f"{repo_root.name}/{rel}:{lineno}: legacy candle rev '{bad}' present")
            rm = re.search(r'\brev\s*=\s*"([0-9a-f]{7,40})"', code)
            if rm:
                candle_revs.add(rm.group(1))
            else:
                errs.append(f"{repo_root.name}/{rel}:{lineno}: candle-rocm-vulkan missing exact rev")


def check_tier0(pd_root: Path, errs: list[str]) -> None:
    for forbidden_dir in ("crates/premium-discount-sensor", "crates/premium-discount-vision"):
        if (pd_root / forbidden_dir).exists():
            errs.append(f"Tier 0 violation: {forbidden_dir} still present in premium-discount")
    manifests = [pd_root / "Cargo.toml"] + sorted((pd_root / "crates").glob("premium-discount-*/Cargo.toml"))
    for m in manifests:
        if not m.exists():
            continue
        rel = m.relative_to(pd_root).as_posix()
        text = m.read_text(encoding="utf-8", errors="replace")
        for lineno, raw in enumerate(text.splitlines(), 1):
            code = strip_comment(raw)
            for sym in (
                "louis-vision",
                "lv-kernels",
                "lv-sds",
                "lv-core",
                "lv-screen",
                "lv-doc",
                "lv-spatial",
                "lv-planar",
                "lv-domain",
                "lv-router",
                "lv-agent",
                "premium-discount-sensor",
                "premium-discount-vision",
            ):
                if sym in code:
                    errs.append(f"Tier 0 violation in {rel}:{lineno}: forbidden dependency '{sym}'")


def check_cross_repo_pins(roots: list[Path], errs: list[str]) -> tuple[set[str], set[str]]:
    pd_revs: set[str] = set()
    lv_revs: set[str] = set()
    for r in roots:
        if not r.exists():
            continue
        for m in iter_cargo_manifests(r):
            text = m.read_text(encoding="utf-8", errors="replace")
            for lineno, raw in enumerate(text.splitlines(), 1):
                code = strip_comment(raw).strip()
                if re.match(r"^repository\s*=", code):
                    continue
                if "Qntology/premium-discount" in code:
                    rm = re.search(r'\brev\s*=\s*"([0-9a-f]{7,40})"', code)
                    if rm:
                        pd_revs.add(rm.group(1))
                    elif 'path = "' not in code and '[patch.' not in code:
                        errs.append(f"{m}:{lineno}: unpinned Qntology/premium-discount ref")
                if "Qntology/louis-vision" in code:
                    rm = re.search(r'\brev\s*=\s*"([0-9a-f]{7,40})"', code)
                    if rm:
                        lv_revs.add(rm.group(1))
                    elif 'path = "' not in code and '[patch.' not in code:
                        errs.append(f"{m}:{lineno}: unpinned Qntology/louis-vision ref")
    if len(pd_revs) > 1:
        errs.append(f"Mismatched Qntology/premium-discount revs across manifests: {sorted(pd_revs)}")
    if pd_revs and pd_revs != {EXPECTED_PD_REV}:
        errs.append(f"Qntology/premium-discount rev mismatch: found {sorted(pd_revs)}, expected {EXPECTED_PD_REV}")
    if len(lv_revs) > 1:
        errs.append(f"Mismatched Qntology/louis-vision revs across manifests: {sorted(lv_revs)}")
    return pd_revs, lv_revs


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tier", choices=["0", "1", "2a", "2b", "all"], default="0")
    ap.add_argument("--all", action="store_true")
    args = ap.parse_args()
    mode = "all" if args.all else args.tier

    errs: list[str] = []
    candle_revs: set[str] = set()
    ws_parent = ROOT.parent
    pd_root = ws_parent / "premium-discount" if (ws_parent / "premium-discount").exists() else ROOT
    lv_root = ws_parent / "louis-vision"
    lc_root = ws_parent / "logis-center"
    ns_root = ws_parent / "noto-sandbox"

    if mode in ("0", "2a", "all") and pd_root.exists():
        for m in iter_cargo_manifests(pd_root):
            check_manifest_rules(m, pd_root, errs, candle_revs)
        if mode in ("0", "all"):
            check_tier0(pd_root, errs)

    if mode in ("1", "2b", "all") and lv_root.exists() and (lv_root / "Cargo.toml").exists():
        for m in iter_cargo_manifests(lv_root):
            check_manifest_rules(m, lv_root, errs, candle_revs)

    if mode == "all":
        for extra in (lc_root, ns_root):
            if extra.exists():
                for m in iter_cargo_manifests(extra):
                    check_manifest_rules(m, extra, errs, candle_revs)

    if not candle_revs:
        errs.append("No candle-rocm-vulkan rev found")
    elif candle_revs != {EXPECTED_CANDLE_REV}:
        errs.append(f"candle-rocm-vulkan rev mismatch: found {sorted(candle_revs)}, expected {EXPECTED_CANDLE_REV}")

    pin_roots = [r for r in (pd_root, lv_root) if r.exists()] if mode in ("2a", "2b") else (
        [r for r in (pd_root, lv_root, lc_root, ns_root) if r.exists()] if mode == "all" else [pd_root]
    )
    pd_revs, lv_revs = check_cross_repo_pins(pin_roots, errs)
    if mode in ("2a", "2b", "all"):
        if not lv_revs:
            errs.append("No Qntology/louis-vision rev found")
        elif lv_revs != {EXPECTED_LV_REV}:
            errs.append(f"Qntology/louis-vision rev mismatch: found {sorted(lv_revs)}, expected {EXPECTED_LV_REV}")

    if errs:
        for e in errs:
            print(f"[VERIFY-PINS] FAIL: {e}", file=sys.stderr)
        return 2

    print(
        f"[VERIFY-PINS] PASS tier={mode} candle={EXPECTED_CANDLE_REV[:8]} "
        f"pd_revs={[x[:8] for x in sorted(pd_revs)]} lv_revs={[x[:8] for x in sorted(lv_revs)]}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
