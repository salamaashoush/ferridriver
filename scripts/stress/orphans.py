#!/usr/bin/env python3
"""Does a browser ever outlive the process that launched it?

A headed browser that survives its server is invisible and unowned: on
macOS it is a dock tile with no window that only Force Quit removes, and
it keeps its profile directory and its port. Only the pipe-transport
backends exit by themselves when the parent dies — a websocket-transport
Chrome or a BiDi Firefox is reparented to pid 1 and runs forever — so
teardown has to be explicit on every exit path, including the ones where
no code of ours runs.

Each probe launches a server, opens a page, ends the server the given
way, and counts what is left. Expected result is zero survivors for
every combination.

  EOF      the client closes stdin, as a disconnecting MCP client does
  SIGTERM  what a supervisor or a restarting client sends
  SIGKILL  no code of ours runs at all; only the watchdog can help here

Usage: orphans.py [--backend cdp-pipe] [--signals EOF,SIGTERM,SIGKILL] [--headed]
"""

import argparse
import os
import signal
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mcp_client import PAGE, Client, browser_procs, default_binary, kill_groups, temp_dirs  # noqa: E402


def probe(binary, backend, how, headless):
    """Returns (launched, survivors, exited, leaked_dirs)."""
    # Groups that already existed are somebody else's browser.
    pre_existing_groups = {v[1] for v in browser_procs().values()}
    dirs_before = temp_dirs("pipe") + temp_dirs("raw") + temp_dirs("firefox")

    client = Client(binary, backend, headless=headless)
    client.initialize("orphans")
    _, ok, err = client.call("navigate", {"url": PAGE})
    if not ok:
        print(f"  [{backend}/{how}] navigate failed: {err}")

    launched = {pid: v for pid, v in browser_procs().items() if v[1] not in pre_existing_groups}

    exited = True
    if how == "EOF":
        exited = client.close()
    else:
        os.kill(client.proc.pid, getattr(signal, how))
        try:
            client.proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            exited = False
            client.proc.kill()
            client.proc.wait()

    # Give the watchdog and the OS a moment to finish the teardown.
    time.sleep(4)
    survivors = {pid: v for pid, v in browser_procs().items() if pid in launched}
    dirs_after = temp_dirs("pipe") + temp_dirs("raw") + temp_dirs("firefox")

    kill_groups({v[1] for v in survivors.values()})
    return len(launched), survivors, exited, dirs_after - dirs_before


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", default=default_binary())
    ap.add_argument("--backend", default="cdp-pipe")
    ap.add_argument("--signals", default="EOF,SIGTERM,SIGKILL")
    ap.add_argument("--headed", action="store_true")
    args = ap.parse_args()

    bad = 0
    for how in args.signals.split(","):
        how = how.strip()
        launched, survivors, exited, leaked = probe(args.binary, args.backend, how, not args.headed)
        verdict = "ok" if (not survivors and exited) else "FAIL"
        print(
            f"  [{args.backend}/{how:<7}] launched={launched:<3} survivors={len(survivors)} "
            f"server_exited={exited} temp_dirs_left={leaked}  {verdict}"
        )
        for pid, (_, _, cmd) in list(survivors.items())[:3]:
            print(f"      pid={pid} {cmd}")
        if survivors or not exited:
            bad += 1

    # Temp directories left by a hard kill are reclaimed by the next
    # ferridriver start, not immediately, so they are reported rather
    # than failed on.
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
