#!/usr/bin/env python3
"""Many session lifecycles through one server, watching for drift.

Every distinct session name touches the per-context registries, the
snapshot ref-map cache, the page-wrapper cache, the script session table
and the per-context lock table. Anything that is only ever inserted into
shows up here as a slope; a short run cannot see it.

What the numbers mean: fds, threads and temp directories must be FLAT —
any growth is a leak. Resident memory rises early (allocator arenas,
lazily built caches) and should flatten; a slope that holds across four
checkpoints is worth chasing.

Usage: soak.py [--cycles 1000] [--backend cdp-pipe]
"""

import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mcp_client import Client, Metrics, default_binary, report_drift  # noqa: E402

PAGE = "data:text/html,<html><body><h1 id=t>soak</h1></body></html>"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", default=default_binary())
    ap.add_argument("--backend", default="cdp-pipe")
    ap.add_argument("--cycles", type=int, default=1000)
    args = ap.parse_args()

    client = Client(args.binary, args.backend)
    metrics = Metrics(client.proc.pid)
    client.initialize("soak")

    # Warm up first: the first browser launch and the first script VM are
    # one-off costs that would otherwise be attributed to the soak.
    client.call("navigate", {"url": PAGE, "session": "default:warm"})
    metrics.sample("warm")

    failures = []
    started = time.time()
    checkpoint = max(args.cycles // 4, 1)
    for i in range(args.cycles):
        session = f"default:soak{i}"
        for tool, arguments in (
            ("navigate", {"url": PAGE, "session": session}),
            ("snapshot", {"session": session}),
            ("run_script", {"source": "vars.set('k','v'); return await page.title();", "session": session}),
            ("page", {"action": "close_context", "session": session}),
        ):
            _, ok, err = client.call(tool, arguments)
            if not ok:
                failures.append(f"{session} {tool}: {err}")
        if (i + 1) % checkpoint == 0:
            metrics.sample(f"after {i + 1}")

    elapsed = time.time() - started
    print(f"  {args.cycles} session lifecycles in {elapsed:.1f}s ({args.cycles / elapsed:.1f}/s)")

    client.call("page", {"action": "close_browser"})
    metrics.sample("closed")
    exited = client.close()

    report_drift(metrics)
    first, last = metrics.drift()
    overall_kb = (last["rss_mb"] - first["rss_mb"]) * 1024 / args.cycles
    print(f"\n  rss growth, whole run: {overall_kb:.1f} KB per lifecycle")
    # The overall figure is dominated by warm-up at small cycle counts.
    # The last checkpoint interval is the steady-state slope, and the one
    # to watch: it should keep falling, not hold.
    checkpoints = [s for s in metrics.samples if s["label"].startswith("after ")]
    if len(checkpoints) >= 2:
        span = args.cycles // len(checkpoints)
        tail_kb = (checkpoints[-1]["rss_mb"] - checkpoints[-2]["rss_mb"]) * 1024 / max(span, 1)
        print(f"  rss growth, last {span} cycles: {tail_kb:.1f} KB per lifecycle (this is the one to watch)")
    print(f"  failures: {len(failures)}  server exited on its own: {exited}")
    for failure in failures[:10]:
        print(f"  FAIL {failure}")

    return 1 if (failures or not exited) else 0


if __name__ == "__main__":
    sys.exit(main())
