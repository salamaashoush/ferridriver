"""Check ferridriver's accessibility audit against Lighthouse's.

Both run axe-core, so where they overlap they must agree exactly: the
same rules failing, on the same number of elements. What they do NOT
share is scope. Lighthouse wraps 67 of axe's rules and reports only
those; ferridriver runs the engine, so it reports every rule axe has.
Extra rules on our side are the point, not a disagreement, and this
compares the intersection rather than the union.

Serve the fixtures first, then:

    python3 scripts/perf-diff/compare-accessibility.py http://127.0.0.1:8732/rich/

Exits non-zero on a real disagreement.
"""

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent

def chrome_path():
    if os.environ.get("CHROME_PATH"):
        return os.environ["CHROME_PATH"]
    patterns = [
        (Path.home() / "Library/Caches/ms-playwright", "chromium-*/chrome-mac-*/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"),
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
    report = json.loads(out.stdout)
    return report["audits"], set(report["axeRules"])


def ferridriver(url):
    out = subprocess.run(
        ["cargo", "run", "-q", "-p", "ferridriver-perf", "--example", "accessibility", "--", url],
        cwd=ROOT, capture_output=True, text=True, check=True,
    )
    return json.loads(out.stdout)


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    url = sys.argv[1]

    theirs, axe_rules = lighthouse(url)
    ours = ferridriver(url)
    print(f"{ours['engine']} against Lighthouse, on {url}\n")

    # Lighthouse scores each wrapped rule 0 or 1; we report the rule only
    # when it failed, so a missing rule on our side means it passed.
    ours_failing = {rule["id"]: len(rule["nodes"]) for rule in ours["violations"]}
    # Every rule the engine reached, whatever it concluded. `inapplicable`
    # belongs here: a rule that found nothing to look at still ran, and
    # leaving it out would read as a coverage gap.
    ours_known = {r["id"] for key in ("violations", "passes", "incomplete", "inapplicable")
                  for r in ours[key]}

    differences, compared = [], 0
    for audit_id, audit in sorted(theirs.items()):
        if audit_id not in axe_rules or audit["mode"] != "binary":
            continue
        if audit_id not in ours_known:
            # Lighthouse reached a verdict on an axe rule and we never ran
            # it. A gap whichever way their verdict went: ours is meant to
            # be the superset.
            differences.append(f"{audit_id}: Lighthouse evaluated the rule, we never ran it")
            continue
        compared += 1
        their_nodes = audit.get("itemCount", 0) if audit["score"] == 0 else 0
        our_nodes = ours_failing.get(audit_id, 0)
        if (their_nodes > 0) != (our_nodes > 0):
            differences.append(
                f"{audit_id}: Lighthouse {'failed' if their_nodes else 'passed'} it, "
                f"we {'failed' if our_nodes else 'passed'} it")
        elif their_nodes != our_nodes:
            differences.append(f"{audit_id}: Lighthouse found {their_nodes} element(s), we found {our_nodes}")

    only_ours = sorted(set(ours_failing) - set(theirs))
    print(f"compared {compared} shared rules")
    if only_ours:
        print(f"we also report {len(only_ours)} rule(s) Lighthouse does not wrap: {', '.join(only_ours)}")
    if differences:
        print("\ndisagreements:")
        for line in differences:
            print(f"  {line}")
        return 1
    print("\nno disagreements")
    return 0


if __name__ == "__main__":
    sys.exit(main())
