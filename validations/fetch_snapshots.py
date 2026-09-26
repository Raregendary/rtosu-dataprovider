"""Fetch /json/v2 from both providers and store raw snapshots for offline diffing.

Exit code 2 means at least one source was unreachable, so a diff must not be
attempted against a stale snapshot on disk.

Usage:
    python validations/fetch_snapshots.py [--samples 5] [--interval 2.0]
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
import time
import urllib.error
import urllib.request

HERE = pathlib.Path(__file__).resolve().parent
SNAP_DIR = HERE / "snapshots"

SOURCES = {
    "tosu": "http://127.0.0.1:24050/json/v2",
    "rtosu": "http://127.0.0.1:24051/json/v2",
}


def fetch(url: str, timeout: float = 20.0) -> dict:
    req = urllib.request.Request(url, headers={"Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--samples", type=int, default=5)
    ap.add_argument("--interval", type=float, default=2.0)
    args = ap.parse_args()

    SNAP_DIR.mkdir(parents=True, exist_ok=True)

    failures = 0
    for i in range(args.samples):
        for name, url in SOURCES.items():
            try:
                data = fetch(url)
            except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as exc:
                print(f"[{i}] {name}: FAILED {exc}")
                failures += 1
                continue
            path = SNAP_DIR / f"{name}.json"
            path.write_text(json.dumps(data, indent=2, sort_keys=False), encoding="utf-8")
            print(f"[{i}] {name}: {len(json.dumps(data))} chars -> {path.name}")
        if i + 1 < args.samples:
            time.sleep(args.interval)

    return 2 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
