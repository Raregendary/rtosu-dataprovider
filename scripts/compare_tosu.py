from __future__ import annotations

import argparse
import json
import subprocess
import threading
import time
import urllib.request
from pathlib import Path
from typing import Any


def fetch_json(url: str, timeout: float) -> dict[str, Any]:
    request = urllib.request.Request(url, headers={"Cache-Control": "no-cache"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.load(response)


def run_snapshot(binary: Path, profile: str, scan_megabytes: int, timeout: float) -> list[dict[str, Any]]:
    command = [
        str(binary),
        "snapshot-all",
        profile,
        "--scan-megabytes",
        str(scan_megabytes),
    ]
    completed = subprocess.run(
        command,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(completed.stderr.strip() or "snapshot command failed")
    value = json.loads(completed.stdout)
    if not isinstance(value, list):
        raise RuntimeError("snapshot output was not a list")
    return value


def client_index(payload: dict[str, Any]) -> dict[tuple[int, str], dict[str, Any]]:
    tourney = payload.get("tourney") or {}
    clients = tourney.get("clients") or []
    result: dict[tuple[int, str], dict[str, Any]] = {}
    for client in clients:
        user = client.get("user") or {}
        key = (int(user.get("id") or 0), str(user.get("name") or ""))
        if key[0] or key[1]:
            result[key] = client
    return result


def playable(client: dict[str, Any]) -> bool:
    play = client.get("play") or {}
    return int(play.get("score") or 0) != 0 or int((play.get("combo") or {}).get("current") or 0) != 0


def nearest_sample(samples: list[tuple[float, dict[str, Any]]], captured_at_ms: int) -> tuple[float, dict[str, Any]] | None:
    if not samples:
        return None
    target = captured_at_ms / 1000.0
    return min(samples, key=lambda sample: abs(sample[0] - target))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", default="http://127.0.0.1:24050/json/v2")
    parser.add_argument("--profile", default="tournament")
    parser.add_argument("--scan-megabytes", type=int, default=32)
    parser.add_argument("--http-timeout", type=float, default=2.0)
    parser.add_argument("--snapshot-timeout", type=float, default=30.0)
    parser.add_argument(
        "--binary",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "target" / "debug" / "osumemoryreading.exe",
    )
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    started = time.perf_counter()
    samples: list[tuple[float, dict[str, Any]]] = []
    stop = threading.Event()

    def poll() -> None:
        while not stop.is_set():
            try:
                payload = fetch_json(args.url, min(args.http_timeout, 1.0))
                samples.append((time.time(), payload))
            except Exception:
                pass
            stop.wait(0.1)

    poller = threading.Thread(target=poll, daemon=True)
    poller.start()
    try:
        before = fetch_json(args.url, args.http_timeout)
        snapshots = run_snapshot(args.binary, args.profile, args.scan_megabytes, args.snapshot_timeout)
        after = fetch_json(args.url, args.http_timeout)
    finally:
        stop.set()
        poller.join(timeout=1.0)
    elapsed = time.perf_counter() - started

    if args.json:
        print(json.dumps({"before": before, "after": after, "snapshots": snapshots}, indent=2))
        return 0

    before_index = client_index(before)
    after_index = client_index(after)
    print(f"elapsed={elapsed:.2f}s endpoint_samples={len(samples)} tosu_before={len(before_index)} tosu_after={len(after_index)} rust_processes={len(snapshots)}")
    matched = 0
    active_matches = 0
    errors = 0
    for item in snapshots:
        pid = item.get("pid")
        snapshot = item.get("snapshot")
        if not snapshot:
            errors += 1
            print(f"pid={pid} error={item.get('error', 'snapshot failed')}")
            continue
        user = snapshot.get("user") or {}
        key = (int(user.get("id") or 0), str(user.get("name") or ""))
        sample = nearest_sample(samples, int(snapshot.get("captured_at_ms") or 0))
        sample_payload = sample[1] if sample else before
        sample_time = sample[0] if sample else 0.0
        sample_client = client_index(sample_payload).get(key)
        if sample_client or key in before_index or key in after_index:
            matched += 1
        gameplay = snapshot.get("gameplay") or {}
        sample_play = (sample_client or {}).get("play") or {}
        sample_active = playable(sample_client or {})
        active_matches += int(sample_active)
        print(
            f"pid={pid} name={user.get('name', '')!r} ipc={(sample_client or {}).get('ipcId')} "
            f"sample_age_ms={int(abs(time.time() - sample_time) * 1000) if sample else -1} "
            f"rust_score={gameplay.get('score')} tosu_score={sample_play.get('score')} "
            f"rust_accuracy={gameplay.get('accuracy')} tosu_accuracy={sample_play.get('accuracy')} "
            f"rust_combo={gameplay.get('combo')} tosu_combo={(sample_play.get('combo') or {}).get('current')} "
            f"errors={list((snapshot.get('errors') or {}).keys())}"
        )
    print(f"identity_matches={matched} active_matches={active_matches} process_errors={errors}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
