#!/usr/bin/env python3
"""Shared plumbing for the MCP stress harnesses.

One MCP server speaks JSON-RPC over its stdio pipe, so concurrent
callers need their responses demultiplexed by id — that is what `Client`
does, and it is why these harnesses can put 12 workers on one server the
way a real agent session never would.

`Metrics` samples the things a leak shows up in: resident memory, open
descriptors, thread count, live browser processes, and the temp
directories a browser owns. Flat counters across a run are the actual
assertion; the latency numbers are secondary.
"""

import json
import os
import subprocess
import threading
import time

TMP = os.environ.get("TMPDIR", "/tmp")

# Enough DOM to be worth snapshotting, cheap enough to navigate in a few ms.
PAGE = (
    "data:text/html,<html><body><h1 id=t>stress</h1>"
    "<button id=b onclick=\"document.getElementById('t').textContent='clicked'\">go</button>"
    "<input id=i></body></html>"
)


def sh(cmd):
    """Run a shell command and return stdout (empty string on failure)."""
    return subprocess.run(cmd, shell=True, capture_output=True, text=True).stdout


def ps_lines(fields="pid=,ppid=,pgid=,command="):
    """`ps` output, bypassing any shell alias that may shadow it."""
    return sh(f"/bin/ps -Ao {fields}").splitlines()


def browser_procs():
    """Live browsers launched by ferridriver, keyed by pid.

    Matched on the profile-directory prefix rather than the binary name:
    a developer's own Chrome must never be counted, let alone signalled.
    """
    found = {}
    for line in ps_lines():
        parts = line.split(None, 3)
        if len(parts) < 4:
            continue
        pid, ppid, pgid, cmd = parts
        if "ferridriver-pipe-" in cmd or "ferridriver-raw-" in cmd or "ferridriver-firefox-" in cmd:
            found[int(pid)] = (int(ppid), int(pgid), cmd[:110])
    return found


def temp_dirs(prefix):
    out = sh(f"find {TMP} -maxdepth 1 -name 'ferridriver-{prefix}-*' | wc -l")
    return int(out.strip() or 0)


def kill_groups(pgids):
    """SIGKILL whole process groups. Only ever called with groups this
    harness watched into existence."""
    for pgid in pgids:
        try:
            os.killpg(pgid, 9)
        except (ProcessLookupError, PermissionError):
            pass


def default_binary():
    return os.environ.get("FERRIDRIVER_BIN", "target/debug/ferridriver")


class Client:
    """Line-delimited JSON-RPC over a spawned server's stdio."""

    def __init__(self, binary, backend="cdp-pipe", headless=True, extra_args=()):
        cmd = [binary, "mcp", "--backend", backend, *extra_args]
        if headless:
            cmd.append("--headless")
        self.proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )
        self.lock = threading.Lock()
        self.next_id = 1
        self.waiters = {}
        threading.Thread(target=self._reader, daemon=True).start()

    def _reader(self):
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            rid = msg.get("id")
            if rid is None:
                continue  # notification
            with self.lock:
                slot = self.waiters.pop(rid, None)
            if slot:
                slot[1] = msg
                slot[0].set()
        # Server gone: release everyone still waiting rather than hanging.
        with self.lock:
            for slot in self.waiters.values():
                slot[0].set()
            self.waiters.clear()

    def initialize(self, name="stress"):
        self.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": name, "version": "0"},
            },
        )
        self.notify("notifications/initialized")

    def notify(self, method, params=None):
        payload = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            payload["params"] = params
        with self.lock:
            self.proc.stdin.write(json.dumps(payload) + "\n")
            self.proc.stdin.flush()

    def request(self, method, params, timeout=120):
        slot = [threading.Event(), None]
        with self.lock:
            rid = self.next_id
            self.next_id += 1
            self.waiters[rid] = slot
            self.proc.stdin.write(
                json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}) + "\n"
            )
            self.proc.stdin.flush()
        if not slot[0].wait(timeout):
            with self.lock:
                self.waiters.pop(rid, None)
            raise TimeoutError(f"{method} timed out after {timeout}s")
        if slot[1] is None:
            raise RuntimeError("server died with a request in flight")
        return slot[1]

    def call(self, tool, arguments, timeout=120):
        """Returns (elapsed_ms, ok, message). A tool that fails reports
        `isError: true`; a JSON-RPC error means the request itself could
        not be processed."""
        started = time.time()
        msg = self.request("tools/call", {"name": tool, "arguments": arguments}, timeout)
        elapsed = (time.time() - started) * 1000
        if "error" in msg:
            return elapsed, False, json.dumps(msg["error"])[:200]
        result = msg.get("result", {})
        if result.get("isError"):
            text = " ".join(b.get("text", "") for b in result.get("content", []))
            return elapsed, False, text[:200]
        return elapsed, True, ""

    def text(self, tool, arguments):
        msg = self.request("tools/call", {"name": tool, "arguments": arguments})
        return " ".join(b.get("text", "") for b in msg.get("result", {}).get("content", []))

    def close(self, timeout=25):
        """Close stdin and wait. Returns True if the server exited on its
        own — a server that has to be killed is itself a finding."""
        try:
            self.proc.stdin.close()
        except (BrokenPipeError, ValueError):
            pass
        try:
            self.proc.wait(timeout=timeout)
            return True
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
            return False


class Metrics:
    """Periodic resource snapshots of the server process."""

    def __init__(self, pid, quiet=False):
        self.pid = pid
        self.quiet = quiet
        self.samples = []

    def sample(self, label):
        rss = sh(f"/bin/ps -o rss= -p {self.pid}").strip()
        fds = sh(f"lsof -p {self.pid} 2>/dev/null | wc -l").strip()
        threads = sh(f"/bin/ps -M -p {self.pid} 2>/dev/null | wc -l").strip()
        s = {
            "label": label,
            "rss_mb": round(int(rss or 0) / 1024, 1),
            "fds": int(fds or 0),
            # `ps -M` prints a header line before the threads.
            "threads": max(int(threads or 1) - 1, 0),
            "browsers": len(browser_procs()),
            "profiles": temp_dirs("pipe") + temp_dirs("raw") + temp_dirs("firefox"),
            "downloads": temp_dirs("downloads"),
        }
        self.samples.append(s)
        if not self.quiet:
            print(
                f"  [{label:<14}] rss={s['rss_mb']}MB fds={s['fds']} threads={s['threads']} "
                f"browsers={s['browsers']} profiles={s['profiles']} dl={s['downloads']}"
            )
        return s

    def drift(self):
        """First and last sample, for the end-of-run summary."""
        if not self.samples:
            return None, None
        return self.samples[0], self.samples[-1]


def report_drift(metrics):
    first, last = metrics.drift()
    if not first:
        return
    print("\n  resource drift (start -> end)")
    for key in ("rss_mb", "fds", "threads", "browsers", "profiles", "downloads"):
        print(f"    {key:<12} {first[key]} -> {last[key]}")


def report_latency(latencies):
    if not latencies:
        return
    print("\n  latency (ms)")
    for tool, values in sorted(latencies.items()):
        values = sorted(values)
        p50 = values[len(values) // 2]
        p95 = values[min(len(values) - 1, int(len(values) * 0.95))]
        print(f"    {tool:<24} n={len(values):<5} p50={p50:8.1f} p95={p95:8.1f} max={max(values):8.1f}")
