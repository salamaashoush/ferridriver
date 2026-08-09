#!/usr/bin/env python3
"""Concurrent load, churn, and crash recovery against one MCP server.

An agent session drives one tool at a time; nothing in normal use puts
twelve callers on a server at once, opens and closes a hundred contexts,
or kills the browser mid-session. Every bug this harness has found lived
in exactly those gaps:

  * a browser killed from outside left its pages in the context, so the
    next call went down a dead transport and waited the full 30s CDP
    timeout instead of relaunching;
  * closing a context left its script VM, its page wrapper, and its
    per-page dispatcher state behind;
  * cold-starting an instance held the global write lock across the
    whole browser spawn, stalling every other session in the server.

Usage: stress.py [--backend cdp-pipe] [--rounds 6] [--workers 8] [--headed]
Exit code is non-zero if any call failed, a browser outlived the server,
or the server did not exit on its own.
"""

import argparse
import os
import signal
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mcp_client import (  # noqa: E402
    PAGE,
    Client,
    Metrics,
    browser_procs,
    default_binary,
    kill_groups,
    report_drift,
    report_latency,
    sh,
)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", default=default_binary())
    ap.add_argument("--backend", default="cdp-pipe")
    ap.add_argument("--rounds", type=int, default=6)
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--headed", action="store_true")
    args = ap.parse_args()

    # Process groups that existed before we started are off limits: a
    # helper of the developer's own browser shares its parent's group and
    # must never be counted as ours, let alone signalled.
    pre_existing_groups = {v[1] for v in browser_procs().values()}

    client = Client(args.binary, args.backend, headless=not args.headed)
    metrics = Metrics(client.proc.pid)
    client.initialize("stress")
    metrics.sample("start")

    sessions = ["default:a", "default:b", "alpha:one", "alpha:two", "beta:solo"]
    failures = []
    latencies = {}

    def record(tool, elapsed, ok, err, ctx=""):
        latencies.setdefault(tool, []).append(elapsed)
        if not ok:
            failures.append(f"{tool}{ctx}: {err}")

    # Cold-start every session at once. Launching used to serialize the
    # whole server behind one browser spawn, so this is the phase that
    # regresses first if the launch path goes back under the write lock.
    started = time.time()
    threads = []
    for session in sessions:
        def cold(session=session):
            record("navigate", *client.call("navigate", {"url": PAGE, "session": session}), ctx=f"[{session}]")
        thread = threading.Thread(target=cold)
        thread.start()
        threads.append(thread)
    for thread in threads:
        thread.join()
    print(f"  cold start of {len(sessions)} sessions, concurrent: {time.time() - started:.1f}s")
    metrics.sample("cold-start")

    def worker(n):
        for r in range(args.rounds):
            session = sessions[(n + r) % len(sessions)]
            record("snapshot", *client.call("snapshot", {"session": session}), ctx=f"[{session}]")
            record(
                "evaluate",
                *client.call("evaluate", {"expression": "document.title", "session": session}),
                ctx=f"[{session}]",
            )
            record(
                "run_script",
                *client.call(
                    "run_script",
                    {"source": "await page.click('#b'); return await page.textContent('#t');", "session": session},
                ),
                ctx=f"[{session}]",
            )
            record("navigate", *client.call("navigate", {"url": PAGE, "session": session}), ctx=f"[{session}]")

    started = time.time()
    threads = [threading.Thread(target=worker, args=(i,)) for i in range(args.workers)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    elapsed = time.time() - started
    calls = sum(len(v) for v in latencies.values())
    print(f"  {calls} tool calls, {args.workers} workers: {elapsed:.1f}s ({calls / elapsed:.1f}/s)")
    metrics.sample("after-load")

    # Tabs: opened and closed repeatedly. `page(close)` used to drop the
    # handle without closing the target, so the tab and its listener
    # tasks stayed behind.
    for _ in range(args.rounds):
        for session in sessions[:3]:
            record("page.new", *client.call("page", {"action": "new", "session": session}), ctx=f"[{session}]")
            record(
                "page.close",
                *client.call("page", {"action": "close", "page_index": 1, "session": session}),
                ctx=f"[{session}]",
            )
    metrics.sample("page-churn")

    # Contexts: closed and cold-started again.
    for _ in range(args.rounds):
        for session in sessions[:3]:
            record(
                "close_context",
                *client.call("page", {"action": "close_context", "session": session}),
                ctx=f"[{session}]",
            )
            record("navigate", *client.call("navigate", {"url": PAGE, "session": session}), ctx=f"[{session}]")
    metrics.sample("context-churn")

    # Instances: one browser closed while others must keep working.
    for _ in range(2):
        record("close_instance", *client.call("page", {"action": "close_instance", "session": "alpha:one"}))
        record(
            "evaluate",
            *client.call("evaluate", {"expression": "1+1", "session": "default:a"}),
            ctx="[survivor after instance close]",
        )
        record("navigate", *client.call("navigate", {"url": PAGE, "session": "alpha:one"}), ctx="[relaunch]")
    metrics.sample("instance-churn")

    # Crash recovery: kill a browser behind the server's back. Every
    # session must relaunch rather than time out against a dead pipe.
    victim = sh("/bin/ps -Ao pid=,command= | grep -E 'ferridriver-(pipe|raw|firefox)-' | grep -v grep | head -1").split()
    if victim:
        os.kill(int(victim[0]), signal.SIGKILL)
        time.sleep(1)
        before = len(failures)
        for session in sessions:
            record("navigate", *client.call("navigate", {"url": PAGE, "session": session}), ctx=f"[after kill {session}]")
        print("  browser killed externally; " + ("all sessions recovered" if len(failures) == before else "RECOVERY FAILED"))
    metrics.sample("after-crash")

    # Bad input must not wedge the session.
    _, ok, _ = client.call("page", {"action": "nonsense", "session": "default:a"})
    if ok:
        failures.append("page(nonsense) should have failed")
    record("evaluate", *client.call("evaluate", {"expression": "2+2", "session": "default:a"}), ctx="[after bad input]")

    record("close_browser", *client.call("page", {"action": "close_browser"}))
    metrics.sample("closed")
    exited = client.close()

    time.sleep(2)
    survivors = {pid: v for pid, v in browser_procs().items() if v[1] not in pre_existing_groups}

    report_latency(latencies)
    report_drift(metrics)
    print(f"\n  calls: {calls}  failures: {len(failures)}  server exited on its own: {exited}")
    print(f"  browsers outliving the server: {len(survivors)}")
    for pid, (_, _, cmd) in list(survivors.items())[:5]:
        print(f"    pid={pid} {cmd}")
    for failure in failures[:20]:
        print(f"  FAIL {failure}")

    kill_groups({v[1] for v in survivors.values()})
    return 1 if (failures or survivors or not exited) else 0


if __name__ == "__main__":
    sys.exit(main())
