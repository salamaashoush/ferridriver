#!/usr/bin/env bash
# Does closing a browser actually release everything it held?
#
# Runs N launch/close cycles, then holds the process open on ONE final
# browser and samples its open descriptors. That final browser is constant
# across every N, so the constant drops out and the SLOPE is the leak:
# flat means teardown is clean, +1 per cycle means each closed browser
# leaves its connection behind.
#
# The leak this was written for reached the default 1024 soft limit after
# roughly a thousand sessions ever, which is a hard EMFILE crash rather
# than a slowdown, and it only showed up in a process that opens and
# closes browsers repeatedly -- exactly the cloud shape.
#
# Usage: probe-fd-leak.sh [binary] [cycle-counts...]
#   e.g. probe-fd-leak.sh target/debug/ferridriver 1 5 9

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${1:-$REPO_ROOT/target/debug/ferridriver}"
shift || true
COUNTS=("$@")
[ ${#COUNTS[@]} -gt 0 ] || COUNTS=(1 5 9)
[ -x "$BIN" ] || { echo "no ferridriver at $BIN (cargo build --bin ferridriver)" >&2; exit 2; }

SCRIPT="$(mktemp -t fdprobe.XXXXXX).js"
trap 'rm -f "$SCRIPT"' EXIT
cat >"$SCRIPT" <<'JS'
const n = Number(args[0]);
for (let i = 0; i < n; i++) {
  const b = await chromium().launch({ headless: true });
  const p = await (await b.newContext()).newPage();
  await p.setContent('<h1>x</h1>');
  await b.close();
}
// Hold the process open on one final browser so descriptors can be sampled.
const hold = await chromium().launch({ headless: true });
const hp = await (await hold.newContext()).newPage();
await hp.waitForTimeout(8000);
await hold.close();
return 'done';
JS

# Linux exposes them directly; macOS needs lsof. Count only IPC-ish entries
# so unrelated files (the binary, shared libs) do not drown the signal.
count_fds() {
  local pid="$1"
  if [ -d "/proc/$pid/fd" ]; then
    find "/proc/$pid/fd" -type l -printf '%l\n' 2>/dev/null | grep -cE 'socket:|pipe:|anon_inode:' || echo 0
  else
    lsof -p "$pid" 2>/dev/null | grep -cE 'unix|PIPE|KQUEUE' || echo 0
  fi
}

echo "binary   $BIN"
echo "platform $(uname -s)"
echo
printf '%-10s %s\n' "cycles" "ipc descriptors held"
for n in "${COUNTS[@]}"; do
  "$BIN" run "$SCRIPT" -- "$n" >/dev/null 2>&1 &
  pid=$!
  best=0
  while kill -0 "$pid" 2>/dev/null; do
    c=$(count_fds "$pid"); c=${c:-0}
    [ "$c" -gt "$best" ] && best=$c
    sleep 0.4
  done
  wait "$pid" 2>/dev/null || true
  printf '%-10s %s\n' "$n" "$best"
done

echo
echo "Flat across cycle counts means teardown releases the connection."
echo "A slope of about +1 per cycle means each closed browser leaks one."
