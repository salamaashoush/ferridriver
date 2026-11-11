#!/usr/bin/env bash
# Publish every publishable workspace crate to crates.io, dependencies first.
#
# The order comes from `cargo metadata`, so a crate added to the workspace is
# published without anyone editing a list. Dev-dependencies count as edges:
# crates.io rejects a manifest naming a crate it does not have, dev or not.
# A version already on the registry is skipped, so a failed release can be
# re-run from the top. `cargo publish` waits for each upload to reach the
# index before returning, so the next crate resolves against it.
set -euo pipefail

order=$(cargo metadata --format-version 1 --no-deps | python3 -c '
import json, sys
meta = json.load(sys.stdin)
pkgs = {p["name"]: p for p in meta["packages"] if p["publish"] is None}
deps = {n: {d["name"] for d in p["dependencies"] if d["name"] in pkgs and d["name"] != n} for n, p in pkgs.items()}
done, out = set(), []
while len(done) < len(pkgs):
  ready = sorted(n for n in pkgs if n not in done and deps[n] <= done)
  if not ready:
    sys.exit("dependency cycle among: " + " ".join(sorted(set(pkgs) - done)))
  done.update(ready)
  out.extend(ready)
print("\n".join(out))
')

if [ "${1:-}" = "--list" ]; then
  echo "$order"
  exit 0
fi

for crate in $order; do
  if output=$(cargo publish --locked -p "$crate" 2>&1); then
    echo "published $crate"
  elif echo "$output" | grep -qE "already exists|already uploaded"; then
    echo "skipped $crate: this version is already on crates.io"
  else
    echo "$output"
    exit 1
  fi
done
