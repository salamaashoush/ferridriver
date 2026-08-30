"""Record what Lighthouse concludes about each fixture page.

The trace differential can be offline because a trace is a file. An
audit over a live DOM is not: both engines have to look at the page. So
the page is what gets checked in, and Lighthouse's whole verdict about
it is recorded beside it, which puts the gate back offline. One
recording serves both comparisons -- `--test lighthouse` covers
both the axe-core rules and the seven live-DOM audits, and and neither needs node, Lighthouse or the network.

    python3 scripts/perf-diff/record-lighthouse.py           # check
    python3 scripts/perf-diff/record-lighthouse.py --update  # re-record

Read the diff an update produces before committing it. Every line is a
change in what we are being measured against.

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


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


class Threaded(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def serve(directory):
    handler = functools.partial(Quiet, directory=str(directory))
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
