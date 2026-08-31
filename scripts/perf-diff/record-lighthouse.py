"""Record what Lighthouse concludes about each fixture page.

The trace differential can be offline because a trace is a file. An
audit over a live DOM is not: both engines have to look at the page. So
the page is what gets checked in, and Lighthouse's whole verdict about
it is recorded beside it, which puts the gate back offline. One
recording serves both comparisons -- `--test lighthouse` covers both
the axe-core rules and the ten live-page audits, and neither needs
node, Lighthouse or the network.

    python3 scripts/perf-diff/record-lighthouse.py           # check
    python3 scripts/perf-diff/record-lighthouse.py --update  # re-record

Read the diff an update produces before committing it. Every line is a
change in what we are being measured against.

A page can also carry a `<name>.http` sidecar giving the status and the
headers it is served with, because `http-status-code` and
`is-crawlable` read the response rather than the page. The server in
`crates/ferridriver-perf/tests/lighthouse.rs` reads the same file, so
the two cannot drift.

The pages are served here rather than by `fixture-server.py`, on an
ephemeral port, so recording needs nothing else running and two runs
cannot collide. The origin is scrubbed from the recording afterwards,
for the same reason the trace recordings carry no timestamps: it is not
part of any verdict, and leaving it in would make the recording differ
from itself on every run.
"""

import functools
import http.server
import json
import os
import socketserver
import subprocess
import sys
import threading
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
PAGES = ROOT / "crates/ferridriver-perf/tests/fixtures/lighthouse"


def chrome_path():
    if os.environ.get("CHROME_PATH"):
        return os.environ["CHROME_PATH"]
    patterns = [
        (Path.home() / "Library/Caches/ms-playwright",
         "chromium-*/chrome-mac-*/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"),
        (Path.home() / ".cache/ms-playwright", "chromium-*/chrome-linux/chrome"),
    ]
    for base, pattern in patterns:
        for candidate in sorted(base.glob(pattern), reverse=True):
            if os.access(candidate, os.X_OK):
                return str(candidate)
    sys.exit("no Chrome found; set CHROME_PATH or run: ferridriver install chromium")


CONTENT_TYPES = {".html": "text/html; charset=utf-8", ".txt": "text/plain; charset=utf-8"}


def response_head(page):
    """The status and extra headers a `<name>.http` sidecar asks for.

    Two of the audits are functions of the response rather than of the
    page, and a status or a header has nowhere to live in an HTML file.
    The sidecar is one line of status followed by headers, so it reads
    as the response head it becomes -- and `tests/lighthouse.rs` parses
    the same file the same way, which is the point of a file rather than
    a rule in each server.
    """
    sidecar = page.with_suffix(".http")
    if not sidecar.exists():
        return 200, "OK", []
    lines = sidecar.read_text().splitlines()
    status, _, reason = lines[0].partition(" ")
    headers = [line.split(":", 1) for line in lines[1:] if ":" in line]
    return int(status), reason or "OK", [(name, value.strip()) for name, value in headers]


class Fixtures(http.server.BaseHTTPRequestHandler):
    def __init__(self, *args, directory, **kwargs):
        # Set before the base constructor, which handles the request.
        self.directory = directory
        super().__init__(*args, **kwargs)

    def do_GET(self):  # noqa: N802 -- BaseHTTPRequestHandler's own name
        # A file name and nothing else: no traversal, no directories.
        name = self.path.split("?")[0].lstrip("/")
        page = self.directory / name
        if "/" in name or ".." in name or not page.is_file():
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return

        status, reason, extra = response_head(page)
        body = page.read_bytes()
        self.send_response(status, reason)
        self.send_header("Content-Type", CONTENT_TYPES.get(page.suffix, "application/octet-stream"))
        self.send_header("Content-Length", str(len(body)))
        for header, value in extra:
            self.send_header(header, value)
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class Threaded(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def serve(directory):
    handler = functools.partial(Fixtures, directory=directory)
    server = Threaded(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, server.server_address[1]


# Snapshot reads the page as it stands; navigation drives a real load,
# which is the only way Lighthouse scores `canonical` and the other
# audits that need a network log. Both are recorded because neither is a
# superset: navigation applies its own emulation, so the axe verdicts
# under it are not the ones snapshot reaches, and the comparison uses
# the snapshot recording wherever it has an answer.
MODES = {"snapshot": [], "navigation": ["--navigation"]}


def record(page, port, mode):
    url = f"http://127.0.0.1:{port}/{page.name}"
    out = subprocess.run(
        ["node", "lighthouse.mjs", url, chrome_path(), *MODES[mode]],
        cwd=HERE, capture_output=True, text=True, check=True,
    )
    report = json.loads(out.stdout)
    report["page"] = page.name
    del report["url"]
    # The port is ephemeral and leaks into every URL an audit reports
    # (`link-text` names the destinations it objected to), which would
    # make the recording differ from itself run to run. The origin is not
    # part of any verdict; the path is what a reader wants.
    text = json.dumps(report, indent=1, sort_keys=True) + "\n"
    return text.replace(f"http://127.0.0.1:{port}", "http://fixture")


def main():
    update = "--update" in sys.argv[1:]
    pages = sorted(PAGES.glob("*.html"))
    if not pages:
        sys.exit(f"no fixture pages in {PAGES}")

    server, port = serve(PAGES)
    try:
        stale = []
        for page in pages:
            for mode in MODES:
                suffix = ".lighthouse.json" if mode == "snapshot" else ".navigation.json"
                recorded = page.with_suffix(suffix)
                fresh = record(page, port, mode)
                if update:
                    recorded.write_text(fresh)
                    print(f"recorded {recorded.relative_to(ROOT)}")
                elif not recorded.exists() or recorded.read_text() != fresh:
                    stale.append(recorded)
                    print(f"{recorded.relative_to(ROOT)}: out of date with Lighthouse")
    finally:
        server.shutdown()

    if stale:
        print("\nrun: just lh-record", file=sys.stderr)
        return 1
    if not update:
        print(f"{len(pages) * len(MODES)} recording(s) still match Lighthouse")
    return 0


if __name__ == "__main__":
    sys.exit(main())
