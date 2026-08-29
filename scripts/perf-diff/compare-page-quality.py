"""Check ferridriver's seven live-DOM audits against Lighthouse's.

Unlike the accessibility comparison there is no shared engine underneath:
`ferridriver::audits` is a port, so nothing here agrees by construction.
Each verdict is arithmetic this repo wrote against arithmetic Lighthouse
wrote, over the same page.

Both the verdict and the number of elements behind it are compared.
Agreeing that a page fails while disagreeing about which elements is the
failure worth catching -- an audit can reach the right answer from the
wrong set.

    python3 scripts/perf-diff/compare-page-quality.py http://127.0.0.1:8732/rich/

Exits non-zero on a disagreement.
"""

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent

AUDIT_IDS = [
    "doctype",
    "meta-description",
    "crawlable-anchors",
    "link-text",
    "image-aspect-ratio",
    "image-size-responsive",
    "paste-preventing-inputs",
]


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


def lighthouse(url):
    out = subprocess.run(
        ["node", "lighthouse.mjs", url, chrome_path()],
        cwd=HERE, capture_output=True, text=True, check=True,
    )
    return json.loads(out.stdout)["audits"]


def ferridriver(url):
    out = subprocess.run(
        ["cargo", "run", "-q", "-p", "ferridriver-perf", "--example", "page-quality", "--", url],
        cwd=ROOT, capture_output=True, text=True, check=True,
    )
    return {audit["id"]: audit for audit in json.loads(out.stdout)["audits"]}


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    url = sys.argv[1]

    theirs = lighthouse(url)
    ours = ferridriver(url)
    print(f"the seven live-DOM audits against Lighthouse, on {url}\n")

    differences, compared, failures = [], 0, []
    for audit_id in AUDIT_IDS:
        their = theirs.get(audit_id)
        # `notApplicable` means the page had nothing for the audit to
        # look at, which is not a verdict and nothing to compare.
        if not their or their["mode"] != "binary":
            print(f"  {audit_id}: Lighthouse did not score it on this page")
            continue
        our = ours.get(audit_id)
        if our is None:
            differences.append(f"{audit_id}: Lighthouse scored it, we did not run it")
            continue
        compared += 1
        their_passed = their["score"] == 1
        their_items = their.get("itemCount", 0) if not their_passed else 0
        if not their_passed:
            failures.append(audit_id)
        if their_passed != our["passed"]:
            differences.append(
                f"{audit_id}: Lighthouse {'passed' if their_passed else 'failed'} it, "
                f"we {'passed' if our['passed'] else 'failed'} it")
        elif their_items != len(our["items"]):
            differences.append(
                f"{audit_id}: Lighthouse found {their_items} element(s), we found {len(our['items'])}")

    print(f"\ncompared {compared} of {len(AUDIT_IDS)} audits; "
          f"{len(failures)} failed on both sides"
          + (f" ({', '.join(failures)})" if failures else ""))
    if differences:
        print("\ndisagreements:")
        for line in differences:
            print(f"  {line}")
        return 1
    print("\nno disagreements")
    return 0


if __name__ == "__main__":
    sys.exit(main())
