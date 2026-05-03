#!/bin/bash
# Real-app head-to-head bench: ferridriver vs Playwright.
#
# 1000 tests against a React + Tailwind kitchen-sink app served by
# Bun. Both runners spawn the app via webServer config, then run all
# 1000 tests at -j {1,4,8,16}. Three runs per cell, averaged.
#
# Usage:
#   bench/run.sh                    # full matrix, both runners
#   bench/run.sh fd-only            # ferridriver only
#   bench/run.sh pw-only            # playwright only
#   bench/run.sh smoke              # 4 workers × 1 run × 50 tests (sanity)
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RESULTS_FILE="$SCRIPT_DIR/results/realapp.txt"
mkdir -p "$SCRIPT_DIR/results"

WORKER_COUNTS=(2 4 8)
RUNS=2
MODE="${1:-all}"
if [ "$MODE" = "smoke" ]; then
  WORKER_COUNTS=(4); RUNS=1
fi

# Ensure release NAPI is built (debug NAPI is 30-60% slower).
echo "Building ferridriver NAPI release…"
(cd "$ROOT_DIR/crates/ferridriver-node" && bun run build >/dev/null 2>&1)

# Ensure dist/cli.js is fresh.
echo "Building ferridriver-test bundle…"
(cd "$ROOT_DIR/packages/ferridriver-test" && bun run build:cli >/dev/null 2>&1)

# Ensure app dist exists.
echo "Building app dist…"
(cd "$SCRIPT_DIR/app" && bun install >/dev/null 2>&1 && bun run build >/dev/null 2>&1)

# Install playwright + chromium browser if not present.
if [ "$MODE" != "fd-only" ] && [ ! -d "$SCRIPT_DIR/pw-tests/node_modules/@playwright/test" ]; then
  echo "Installing Playwright…"
  (cd "$SCRIPT_DIR/pw-tests" && bun install >/dev/null 2>&1)
fi

cleanup() {
  # ferridriver chromes — match the temp user-data-dir prefix.
  pkill -f "ferridriver-pipe-" 2>/dev/null || true
  # Playwright chromes — match the temp profile dir prefix used by
  # `chromium.launch` (`/var/folders/.../playwright_chromiumdev_*`)
  # AND the headless-shell binary path Playwright launches.
  pkill -f "playwright_chromiumdev_" 2>/dev/null || true
  pkill -f "chromium_headless_shell-1217/chrome-headless-shell" 2>/dev/null || true
  # PW worker process.
  pkill -f "playwright/lib/common/process" 2>/dev/null || true
  # The mock app server.
  pkill -f "bench/app/server.ts" 2>/dev/null || true
  sleep 0.8
}

cleanup

run_avg() {
  local cmd="$1" dir="$2"
  local total=0 ok=0
  for ((r=1; r<=RUNS; r++)); do
    cleanup
    local start end elapsed
    start=$(date +%s%N)
    (cd "$dir" && eval "$cmd" >/dev/null 2>&1) || true
    end=$(date +%s%N)
    elapsed=$(( (end - start) / 1000000 ))
    if [ "$elapsed" -ge 500 ]; then
      total=$((total + elapsed)); ok=$((ok + 1))
    fi
  done
  if [ "$ok" -eq 0 ]; then echo "FAIL"; return 1; fi
  echo $((total / ok))
}

# Need both /healthz reachable + Chrome available. Use Playwright's
# downloaded chromium so both runners use the same binary.
HS="$HOME/Library/Caches/ms-playwright/chromium_headless_shell-1217/chrome-headless-shell-mac-arm64/chrome-headless-shell"
[ -x "$HS" ] || HS=""
RC="$HOME/Library/Caches/ms-playwright/chromium-1217/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"
[ -x "$RC" ] || RC=""

echo ""
echo "================================================================="
echo "  ferridriver vs Playwright — real app, 1000 tests"
echo "================================================================="
echo "  Workers: ${WORKER_COUNTS[*]}    Runs/cell: $RUNS"
[ -n "$HS" ] && echo "  Headless Shell: $HS"
[ -n "$RC" ] && echo "  Regular Chrome: $RC"
echo ""

declare -a fd_hs pw_hs

if [ -n "$HS" ]; then
  echo "--- Headless Shell ---"
  printf "%-12s | %12s | %12s | %8s\n" "Workers" "Playwright" "ferridriver" "Speedup"
  printf "─────────────┼──────────────┼──────────────┼──────────\n"
  for w in "${WORKER_COUNTS[@]}"; do
    pw="N/A"; fd="N/A"
    if [ "$MODE" != "fd-only" ]; then
      pw=$(CHROMIUM_PATH="$HS" run_avg "bun playwright test --workers=$w" "$SCRIPT_DIR/pw-tests")
    fi
    if [ "$MODE" != "pw-only" ]; then
      fd=$(CHROMIUM_PATH="$HS" run_avg "bun ../../packages/ferridriver-test/dist/cli.js test -j $w" "$SCRIPT_DIR/fd-tests")
    fi
    fd_hs+=("$fd"); pw_hs+=("$pw")
    sp=$(awk -v a="$pw" -v b="$fd" 'BEGIN { if (a !~ /^[0-9]+$/ || b !~ /^[0-9]+$/ || b<=0) print "N/A"; else printf "%.2fx", a/b; }')
    printf "%-12s | %9sms | %9sms | %8s\n" "$w" "$pw" "$fd" "$sp"
  done
  echo ""
fi

declare -a fd_rc pw_rc

if [ -n "$RC" ] && [ "$MODE" != "smoke" ]; then
  echo "--- Regular Chrome ---"
  printf "%-12s | %12s | %12s | %8s\n" "Workers" "Playwright" "ferridriver" "Speedup"
  printf "─────────────┼──────────────┼──────────────┼──────────\n"
  for w in "${WORKER_COUNTS[@]}"; do
    pw="N/A"; fd="N/A"
    if [ "$MODE" != "fd-only" ]; then
      pw=$(CHROMIUM_PATH="$RC" run_avg "bun playwright test --workers=$w" "$SCRIPT_DIR/pw-tests")
    fi
    if [ "$MODE" != "pw-only" ]; then
      fd=$(CHROMIUM_PATH="$RC" run_avg "bun ../../packages/ferridriver-test/dist/cli.js test -j $w" "$SCRIPT_DIR/fd-tests")
    fi
    fd_rc+=("$fd"); pw_rc+=("$pw")
    sp=$(awk -v a="$pw" -v b="$fd" 'BEGIN { if (a !~ /^[0-9]+$/ || b !~ /^[0-9]+$/ || b<=0) print "N/A"; else printf "%.2fx", a/b; }')
    printf "%-12s | %9sms | %9sms | %8s\n" "$w" "$pw" "$fd" "$sp"
  done
  echo ""
fi

# Save final table.
{
  echo "ferridriver vs Playwright — real app, 1000 tests"
  echo "Date: $(date -Iseconds)"
  echo "Mode: $MODE   Runs/cell: $RUNS   Workers: ${WORKER_COUNTS[*]}"
  echo ""
  if [ -n "$HS" ]; then
    echo "--- Headless Shell ---"
    printf "%-12s | %12s | %12s | %8s\n" "Workers" "Playwright" "ferridriver" "Speedup"
    printf "─────────────┼──────────────┼──────────────┼──────────\n"
    for i in "${!WORKER_COUNTS[@]}"; do
      w="${WORKER_COUNTS[$i]}"; fd="${fd_hs[$i]}"; pw="${pw_hs[$i]}"
      sp=$(awk -v a="$pw" -v b="$fd" 'BEGIN { if (a !~ /^[0-9]+$/ || b !~ /^[0-9]+$/ || b<=0) print "N/A"; else printf "%.2fx", a/b; }')
      printf "%-12s | %9sms | %9sms | %8s\n" "$w" "$pw" "$fd" "$sp"
    done
    echo ""
  fi
  if [ -n "$RC" ] && [ "$MODE" != "smoke" ]; then
    echo "--- Regular Chrome ---"
    printf "%-12s | %12s | %12s | %8s\n" "Workers" "Playwright" "ferridriver" "Speedup"
    printf "─────────────┼──────────────┼──────────────┼──────────\n"
    for i in "${!WORKER_COUNTS[@]}"; do
      w="${WORKER_COUNTS[$i]}"; fd="${fd_rc[$i]}"; pw="${pw_rc[$i]}"
      sp=$(awk -v a="$pw" -v b="$fd" 'BEGIN { if (a !~ /^[0-9]+$/ || b !~ /^[0-9]+$/ || b<=0) print "N/A"; else printf "%.2fx", a/b; }')
      printf "%-12s | %9sms | %9sms | %8s\n" "$w" "$pw" "$fd" "$sp"
    done
  fi
} > "$RESULTS_FILE"
echo "Saved: $RESULTS_FILE"
