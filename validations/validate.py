"""One-shot validation entry point: fetch both payloads and diff them.

Runs the full check in the right order (reachability -> snapshot -> diff) and
prints a PASS/FAIL verdict with the defect count.

    python validations/validate.py
    python validations/validate.py --tosu http://127.0.0.1:24050/json/v2 \
                                   --rtosu http://127.0.0.1:24051/json/v2

Exit code 0 = schema and data match, 1 = differences found, 2 = a source was
unreachable.
"""

from __future__ import annotations

import argparse
import pathlib
import subprocess
import sys

for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass

HERE = pathlib.Path(__file__).resolve().parent


def run(script: str, *args: str) -> int:
    result = subprocess.run(
        [sys.executable, str(HERE / script), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if result.returncode == 2:
        print(result.stdout)
        print(result.stderr, file=sys.stderr)
    return result.returncode


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tosu", default="http://127.0.0.1:24050/json/v2")
    ap.add_argument("--rtosu", default="http://127.0.0.1:24051/json/v2")
    ap.add_argument("--samples", type=int, default=1)
    ap.add_argument("--interval", type=float, default=2.0)
    args = ap.parse_args()

    fetch_args = ["--samples", str(args.samples), "--interval", str(args.interval)]
    if run("fetch_snapshots.py", *fetch_args) == 1:
        print("FAILED: at least one source was unreachable; nothing was compared.")
        return 2

    diff_args = [
        "--left", str(HERE / "snapshots" / "tosu.json"),
        "--right", str(HERE / "snapshots" / "rtosu.json"),
    ]
    code = run("compare_v2.py", *diff_args)
    print(f"full report: {HERE / 'diff.txt'}")
    print(f"machine-readable: {HERE / 'snapshots' / 'diff.json'}")
    print("VERDICT:", "PASS" if code == 0 else "FAIL (schema/data differences found)")
    return code


if __name__ == "__main__":
    sys.exit(main())
