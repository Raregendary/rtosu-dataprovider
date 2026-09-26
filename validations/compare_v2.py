"""Structural + value diff between the tosu v2 payload and the rtosu payload.

Walks both JSON trees in parallel and classifies every difference:

  MISSING_IN_RTOSU  - key present in tosu, absent in rtosu (schema gap)
  MISSING_IN_TOSU   - key present in rtosu, absent in tosu (extra field)
  TYPE_MISMATCH     - same key, different JSON type
  KEY_ORDER         - same key set, different emission order
  ARRAY_LEN         - same key, different array length
  VALUE_MISMATCH    - same key, materially different value
  ROUNDING          - values agree within 0.005; only the decimal rounding
                      differs (tosu applies fixDecimals = toFixed(2))

Arrays of different length are still compared over their overlapping prefix, so
a length drift cannot hide a value bug inside the common part.

Live gameplay fields (score, combo, timings, pp, ...) are polled at different
instants by the two readers, so they are bucketed separately from structural
problems. Every leaf path is classified by an explicit rule table rather than a
guess, so nothing is silently dropped.

Usage:
    python validations/compare_v2.py [--left a.json] [--right b.json]
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

# Leaf paths that drift between two independent poll instants of a live game.
# Matched as an exact path, or as a `path.` prefix.
VOLATILE = (
    "session.",
    "state.",
    "game.",
    "client",
    "server",
    "profile.",
    "beatmap.time.",
    "beatmap.stats.stars.live",
    "beatmap.stats.bpm.realtime",
    "play.score",
    "play.accuracy",
    "play.combo.",
    "play.hits.",
    "play.healthBar.",
    "play.rank.",
    "play.unstableRate",
    "play.mods.number",
    "play.hitErrorArray",
    "play.pp.",
    "resultsScreen.",
    "folders.",
    "files.",
    "directPath.",
    "leaderboard",
    "tourney.",
    "performance.graph",
)

# Paths where tosu rounds to 2 decimals (fixDecimals) and a raw f64 is
# acceptable in spirit but not byte-identical.
ROUNDING_TOLERANCE = 0.005

# Guard against a 250 KB array being reported thousands of times over.
MAX_ROWS = 60
MAX_TOTAL = 4000


def is_volatile(path: str) -> bool:
    return any(path == p or path.startswith(p) for p in VOLATILE)


def jtype(value) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, (int, float)):
        return "number"
    if isinstance(value, str):
        return "string"
    if isinstance(value, list):
        return "array"
    if isinstance(value, dict):
        return "object"
    return "unknown"


def rounded(value: float, digits: int) -> float:
    return float(f"{value:.{digits}f}")


def walk(left, right, path, out):
    lt, rt = jtype(left), jtype(right)
    p = ".".join(str(x) for x in path) or "<root>"

    if lt == "object" and rt == "object":
        lkeys, rkeys = list(left.keys()), list(right.keys())
        for k in lkeys:
            if k not in right:
                out.append(("MISSING_IN_RTOSU", p + "." + k, jtype(left[k]), None, None))
            else:
                walk(left[k], right[k], path + [k], out)
        for k in rkeys:
            if k not in left:
                out.append(("MISSING_IN_TOSU", p + "." + k, None, jtype(right[k]), None))
        if set(lkeys) == set(rkeys) and lkeys != rkeys:
            out.append(("KEY_ORDER", p, lkeys, rkeys, None))
        return

    if lt == "array" and rt == "array":
        if len(left) != len(right):
            out.append(("ARRAY_LEN", p, len(left), len(right), None))
        for i, (a, b) in enumerate(zip(left, right)):
            child = f"{p}[{i}]"
            if isinstance(a, (int, float)) and isinstance(b, (int, float)) \
                    and not isinstance(a, bool) and not isinstance(b, bool):
                # Bulk compare numeric series so a 4778-long array does not
                # produce 4778 identical rows.
                first_bad = next(
                    (j for j, (x, y) in enumerate(zip(left, right))
                     if isinstance(x, (int, float)) and isinstance(y, (int, float))
                     and not isinstance(x, bool) and not isinstance(y, bool)
                     and abs(x - y) > 1e-6),
                    None,
                )
                if first_bad is not None:
                    out.append(("VALUE_MISMATCH", child, a, b, len(left)))
                    return
                continue
            walk(a, b, path + [f"[{i}]"], out)
        return

    if lt != rt:
        out.append(("TYPE_MISMATCH", p, lt, rt, None))
        return

    if isinstance(left, (int, float)) and not isinstance(left, bool):
        delta = abs(float(left) - float(right))
        if delta <= 1e-6:
            return
        if delta <= ROUNDING_TOLERANCE and (
            rounded(float(left), 2) == rounded(float(right), 2)
            or rounded(float(left), 3) == rounded(float(right), 3)
        ):
            out.append(("ROUNDING", p, left, right, None))
        else:
            out.append(("VALUE_MISMATCH", p, left, right, None))
        return

    if left != right:
        out.append(("VALUE_MISMATCH", p, left, right, None))


def render(value, limit=90):
    text = repr(value)
    return text if len(text) <= limit else text[: limit - 3] + "..."


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--left", default=str(SNAP_DIR / "tosu.json"))
    ap.add_argument("--right", default=str(SNAP_DIR / "rtosu.json"))
    ap.add_argument("--left-label", default="tosu")
    ap.add_argument("--right-label", default="rtosu")
    ap.add_argument("--json-out", default=str(SNAP_DIR / "diff.json"))
    ap.add_argument("--out", default=str(HERE / "diff.txt"))
    args = ap.parse_args()

    left = json.loads(pathlib.Path(args.left).read_text(encoding="utf-8"))
    right = json.loads(pathlib.Path(args.right).read_text(encoding="utf-8"))

    out: list[tuple] = []
    walk(left, right, [], out)

    struct = [d for d in out if d[0] in ("MISSING_IN_RTOSU", "MISSING_IN_TOSU",
                                         "TYPE_MISMATCH", "KEY_ORDER")]
    arrays = [d for d in out if d[0] == "ARRAY_LEN"]
    rounding = [d for d in out if d[0] == "ROUNDING"]
    values = [d for d in out if d[0] == "VALUE_MISMATCH"]
    hard = [d for d in values if not is_volatile(d[1])]

    lines = [
        f"left  = {args.left_label} ({pathlib.Path(args.left).stat().st_size} bytes)",
        f"right = {args.right_label} ({pathlib.Path(args.right).stat().st_size} bytes)",
        "",
        f"STRUCTURAL (schema gaps / type / key order) : {len(struct)}",
        f"ARRAY LEN  (element count differences)     : {len(arrays)}",
        f"VALUE      (materially different)          : {len(values)}",
        f"VALUE      (non-volatile -> real defect)   : {len(hard)}",
        f"ROUNDING   (agrees within {ROUNDING_TOLERANCE})              : {len(rounding)}",
        "",
    ]

    def dump(title, rows):
        if not rows:
            return
        lines.append(f"--- {title} ({len(rows)}) ---")
        for kind, path, l, r, _ in rows[:MAX_ROWS]:
            lines.append(f"  {kind:<17} {path}")
            lines.append(f"      left : {render(l)}")
            lines.append(f"      right: {render(r)}")
        if len(rows) > MAX_ROWS:
            lines.append(f"  ... {len(rows) - MAX_ROWS} more")
        lines.append("")

    dump("STRUCTURAL", struct)
    dump("ARRAY LEN", arrays)
    dump("VALUE (non-volatile -> real defect)", hard)
    dump("VALUE (volatile / expected live drift)", [d for d in values if d not in hard])
    dump("ROUNDING", rounding)

    report = "\n".join(lines)
    print(report)
    pathlib.Path(args.out).write_text(report, encoding="utf-8")
    pathlib.Path(args.json_out).write_text(
        json.dumps(
            [{"kind": k, "path": p, "left": l, "right": r} for k, p, l, r, _ in out],
            indent=2,
            default=str,
        ),
        encoding="utf-8",
    )
    return 1 if (struct or hard) else 0


if __name__ == "__main__":
    sys.exit(main())
