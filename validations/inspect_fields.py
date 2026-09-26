"""Targeted field inspector for the tosu vs rtosu v2 payload comparison.

Prints both payloads side by side for a hand-picked set of JSON paths, so a
specific difference can be inspected without dumping two 250 KB documents.

Usage:
    python validations/inspect_fields.py [path ...]
    python validations/inspect_fields.py --dump-tourney
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass

HERE = pathlib.Path(__file__).resolve().parent
SNAP_DIR = HERE / "snapshots"

DEFAULT_PATHS = [
    "play.mods",
    "beatmap.stats.hitWindow",
    "beatmap.stats.ar",
    "beatmap.stats.od",
    "beatmap.stats.cs",
    "beatmap.stats.hp",
    "beatmap.stats.stars",
    "resultsScreen",
    "performance.graph.series",
    "performance.accuracy",
]


def get(doc, path: str):
    cur = doc
    for part in path.split("."):
        if isinstance(cur, dict) and part in cur:
            cur = cur[part]
        else:
            return None
    return cur


def compact(value, limit=260):
    if isinstance(value, (dict, list)):
        text = json.dumps(value, ensure_ascii=False)
    else:
        text = repr(value)
    return text if len(text) <= limit else text[: limit - 3] + "..."


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("paths", nargs="*", default=None)
    ap.add_argument("--left", default=str(SNAP_DIR / "tosu.json"))
    ap.add_argument("--right", default=str(SNAP_DIR / "rtosu.json"))
    ap.add_argument("--keys", action="store_true", help="show top-level keys and order")
    args = ap.parse_args()

    left = json.loads(pathlib.Path(args.left).read_text(encoding="utf-8"))
    right = json.loads(pathlib.Path(args.right).read_text(encoding="utf-8"))

    if args.keys:
        print("top-level key order")
        print("  tosu :", list(left.keys()))
        print("  rtosu:", list(right.keys()))
        for section in ("resultsScreen", "performance", "tourney", "profile", "play"):
            lv, rv = left.get(section), right.get(section)
            if isinstance(lv, dict) and isinstance(rv, dict):
                lk, rk = list(lv.keys()), list(rv.keys())
                if lk != rk and set(lk) == set(rk):
                    print(f"\n{section} key order")
                    print("  tosu :", lk)
                    print("  rtosu:", rk)
                elif set(lk) != set(rk):
                    print(f"\n{section} key SET")
                    print("  only tosu :", sorted(set(lk) - set(rk)))
                    print("  only rtosu:", sorted(set(rk) - set(lk)))
        print()

    for path in args.paths or DEFAULT_PATHS:
        print(f"=== {path} ===")
        print(f"  tosu : {compact(get(left, path))}")
        print(f"  rtosu: {compact(get(right, path))}")
        print()

    return 0


if __name__ == "__main__":
    sys.exit(main())
