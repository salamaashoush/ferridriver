#!/usr/bin/env python3
"""Two things happening to one session at once, and calls that are
deliberately expensive.

Where `stress.py` measures throughput, this looks for a wedged server: a
lock dropped while someone holds it, a cached page handed out after its
context went away, a call that never returns. Failures here are
correctness bugs, not slow numbers.

Usage: adversarial.py [--backend cdp-pipe] [--headed]
"""

import argparse
import os
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mcp_client import (  # noqa: E402
    Client,
    Metrics,
    browser_procs,
    default_binary,
    kill_groups,
    report_drift,
)

SMALL = "data:text/html,<html><body><h1 id=t>x</h1><button id=b>go</button></body></html>"
BIG = (
    "data:text/html,<html><body><script>"
    "for(let i=0;i<4000;i++){const d=document.createElement('div');"
    "d.setAttribute('role','listitem');d.textContent='row '+i;document.body.appendChild(d);}"
    "</script></body></html>"
)
STORM = (
    "data:text/html,<html><body><script>"
    "for(let i=0;i<3000;i++)console.log('storm line '+i+' '+'x'.repeat(200));"
    "</script><h1 id=t>storm</h1></body></html>"
)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", default=default_binary())
    ap.add_argument("--backend", default="cdp-pipe")
    ap.add_argument("--headed", action="store_true")
    args = ap.parse_args()

    pre_existing_groups = {v[1] for v in browser_procs().values()}
    client = Client(args.binary, args.backend, headless=not args.headed)
    metrics = Metrics(client.proc.pid)
    client.initialize("adversarial")
    failures = []

    def expect(label, tool, arguments, want_ok=True, timeout=90):
        _, ok, err = client.call(tool, arguments, timeout)
        if ok != want_ok:
            failures.append(f"{label}: expected ok={want_ok}, got ok={ok} {err}")

    def phase(label, fn):
        started = time.time()
        try:
            fn()
            ok, detail = True, ""
        except Exception as exc:  # noqa: BLE001 - the harness reports whatever escapes
            ok, detail = False, f"{type(exc).__name__}: {exc}"
            failures.append(f"{label}: {detail}")
        print(f"  {label:<34} {'ok ' if ok else 'FAIL'} {time.time() - started:6.1f}s")

    def pileup():
        """32 callers on ONE session at once. The per-context lock has to
        serialize them without deadlocking or losing one — it used to be
        dropped mid-flight by a cache invalidation."""
        client.call("navigate", {"url": SMALL, "session": "pile:one"})
        errors = []

        def hit(i):
            for tool, arguments in (
                ("snapshot", {"session": "pile:one"}),
                ("evaluate", {"expression": f"{i}+1", "session": "pile:one"}),
                ("run_script", {"source": "return await page.title();", "session": "pile:one"}),
            ):
                _, ok, err = client.call(tool, arguments)
                if not ok:
                    errors.append(f"{tool}: {err}")

        threads = [threading.Thread(target=hit, args=(i,)) for i in range(32)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        if errors:
            raise RuntimeError(f"{len(errors)} failures, first: {errors[0]}")

    def teardown_race():
        """Close a context while calls on it are in flight, then use it
        again. Losing the race is fine; hanging or staying broken is not."""
        for round_ in range(5):
            client.call("navigate", {"url": SMALL, "session": "race:one"})
            errors = []

            def hit():
                for _ in range(4):
                    _, ok, err = client.call("snapshot", {"session": "race:one"})
                    if not ok and "closed" not in err.lower() and "not found" not in err.lower():
                        errors.append(err)

            threads = [threading.Thread(target=hit) for _ in range(6)]
            for thread in threads:
                thread.start()
            time.sleep(0.05)
            client.call("page", {"action": "close_context", "session": "race:one"})
            for thread in threads:
                thread.join()
            if errors:
                raise RuntimeError(f"{len(errors)} unexpected failures, first: {errors[0]}")
            _, ok, err = client.call("navigate", {"url": SMALL, "session": "race:one"})
            if not ok:
                raise RuntimeError(f"session unusable after close (round {round_}): {err}")

    def instance_race():
        """Close a whole browser under concurrent load on its sessions."""
        for session in ("kill:a", "kill:b"):
            client.call("navigate", {"url": SMALL, "session": session})
        errors = []

        def hit(session):
            for _ in range(6):
                _, ok, err = client.call("evaluate", {"expression": "1+1", "session": session})
                if not ok and "closed" not in err.lower() and "gone" not in err.lower():
                    errors.append(f"{session}: {err}")

        threads = [threading.Thread(target=hit, args=(s,)) for s in ("kill:a", "kill:b")]
        for thread in threads:
            thread.start()
        time.sleep(0.05)
        client.call("page", {"action": "close_instance", "session": "kill:a"})
        for thread in threads:
            thread.join()
        if errors:
            raise RuntimeError(f"{len(errors)} unexpected failures, first: {errors[0]}")
        for session in ("kill:a", "kill:b"):
            _, ok, err = client.call("navigate", {"url": SMALL, "session": session})
            if not ok:
                raise RuntimeError(f"{session} unusable after instance close: {err}")

    def big_dom():
        expect("big dom navigate", "navigate", {"url": BIG, "session": "big:one"})
        for _ in range(3):
            expect("big dom snapshot", "snapshot", {"session": "big:one"})

    def console_storm():
        """3000 console lines. The browser's stderr pipe must stay
        drained: at 64KB unread the browser blocks in write(2) on the
        thread that routes protocol traffic and every command freezes."""
        expect("storm navigate", "navigate", {"url": STORM, "session": "storm:one"})
        expect("storm evaluate", "evaluate", {"expression": "document.title", "session": "storm:one"})
        expect("storm snapshot", "snapshot", {"session": "storm:one"})

    def script_reports(label, source, marker):
        """`run_script` reports a thrown script as a tool error AND keeps
        the structured payload; assert the failure is visible in both."""
        msg = client.request(
            "tools/call", {"name": "run_script", "arguments": {"source": source, "session": "err:one"}}
        )
        result = msg.get("result", {})
        text = " ".join(b.get("text", "") for b in result.get("content", []))
        if not result.get("isError"):
            failures.append(f"{label}: a failed script must set isError")
        if marker not in text:
            failures.append(f"{label}: payload does not report the failure: {text[:160]}")

    def error_paths():
        script_reports("throwing script", "throw new Error('boom');", '"status": "error"')
        expect("still alive after throw", "evaluate", {"expression": "1+1", "session": "err:one"})
        script_reports("bad selector", "await page.click('#nope', {timeout: 300});", '"status": "error"')
        expect("still alive after bad selector", "snapshot", {"session": "err:one"})
        expect("unknown action", "page", {"action": "bogus", "session": "err:one"}, want_ok=False)
        expect("bad page index", "page", {"action": "select", "page_index": 99, "session": "err:one"}, want_ok=False)
        expect("still alive after bad index", "evaluate", {"expression": "2+2", "session": "err:one"})

    metrics.sample("start")
    phase("same-session pileup (96 calls)", pileup)
    phase("close_context under load x5", teardown_race)
    phase("close_instance under load", instance_race)
    phase("4000-node dom snapshot x3", big_dom)
    phase("3000-line console storm", console_storm)
    phase("error paths stay recoverable", error_paths)
    client.call("page", {"action": "close_browser"})
    # Sampled AFTER teardown: taken before it, the drift line reports the
    # browsers still legitimately open mid-run and reads like a leak.
    metrics.sample("closed")
    exited = client.close()
    time.sleep(2)
    survivors = {pid: v for pid, v in browser_procs().items() if v[1] not in pre_existing_groups}

    report_drift(metrics)
    print(f"\n  failures: {len(failures)}  server exited on its own: {exited}  survivors: {len(survivors)}")
    for failure in failures[:15]:
        print(f"  FAIL {failure}")

    kill_groups({v[1] for v in survivors.values()})
    return 1 if (failures or survivors or not exited) else 0


if __name__ == "__main__":
    sys.exit(main())
