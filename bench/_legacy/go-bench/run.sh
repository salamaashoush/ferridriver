#!/bin/bash
# ferridriver vs Go-ecosystem competitors (chromedp, go-rod).
# Same 100-test workload as the main bench. Each tool gets fresh
# Chrome processes per run to keep per-mode resource state clean.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
RESULTS_FILE="$SCRIPT_DIR/results.txt"

WORKER_COUNTS=(1 2 4 8)
RUNS=3

HS="$HOME/Library/Caches/ms-playwright/chromium_headless_shell-1217/chrome-headless-shell-mac-arm64/chrome-headless-shell"
RC="$HOME/Library/Caches/ms-playwright/chromium-1217/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"

[ -x "$HS" ] || { echo "missing headless_shell at $HS"; exit 1; }
[ -x "$RC" ] || { echo "missing regular Chrome at $RC"; exit 1; }

echo "Building Go competitors..."
(cd "$SCRIPT_DIR" && go build -tags chromedp -o chromedp-bench chromedp_bench.go)
(cd "$SCRIPT_DIR" && go build -tags rod -o rod-bench rod_bench.go)

run_avg() {
  local cmd="$1"
  local total=0 ok=0
  for ((r=1; r<=RUNS; r++)); do
    local elapsed
    elapsed=$(eval "$cmd" 2>/dev/null | head -1)
    if [[ "$elapsed" =~ ^[0-9]+$ ]] && [ "$elapsed" -ge 100 ]; then
      total=$((total + elapsed)); ok=$((ok + 1))
    else
      echo "FAIL"; return 1
    fi
  done
  echo $((total / ok))
}

# ferridriver via the bench's own runner — same code path as main bench.
fd_run() {
  local workers="$1"
  local chrome="$2"
  CHROMIUM_PATH="$chrome" run_avg "(time -p bun $ROOT_DIR/packages/ferridriver-test/dist/cli.js test -j $workers $ROOT_DIR/bench/fd-bench/bench_compare.spec.ts) 2>&1 | awk '/real/ {printf \"%d\\n\", \$2*1000}'"
}

run_table() {
  local label="$1" chrome="$2"
  echo ""
  echo "--- $label ---"
  printf "%-12s | %12s | %12s | %12s | %s\n" "Workers" "ferridriver" "chromedp" "rod" "best comp / fd"
  printf "─────────────┼──────────────┼──────────────┼──────────────┼──────────────\n"
  local i=0
  for w in "${WORKER_COUNTS[@]}"; do
    fd_ms=$(fd_run "$w" "$chrome")
    cdp_ms=$(run_avg "$SCRIPT_DIR/chromedp-bench --workers $w --chrome '$chrome'")
    rod_ms=$(run_avg "$SCRIPT_DIR/rod-bench --workers $w --chrome '$chrome'")
    best_comp=$(awk -v a="$cdp_ms" -v b="$rod_ms" 'BEGIN { print (a<b)?a:b; }')
    speedup=$(awk -v a="$best_comp" -v b="$fd_ms" 'BEGIN { if (b<=0) print "N/A"; else printf "%.2fx", a/b; }')
    printf "%-12s | %9sms | %9sms | %9sms | %12s\n" "$w" "$fd_ms" "$cdp_ms" "$rod_ms" "$speedup"
    eval "${label// /_}_fd_$i='$fd_ms'"
    eval "${label// /_}_cdp_$i='$cdp_ms'"
    eval "${label// /_}_rod_$i='$rod_ms'"
    i=$((i+1))
  done
}

echo "=== ferridriver vs Go ecosystem (chromedp, go-rod) ==="
echo "Tests: 100, Runs: $RUNS, Workers: ${WORKER_COUNTS[*]}"

run_table "Headless Shell" "$HS"
run_table "Regular Chrome" "$RC"

# Save raw stdout to file (last bench output).
{
  echo "Bench: $(date -Iseconds)"
  echo "ferridriver vs chromedp vs go-rod — 100 tests, $RUNS runs avg"
  echo ""
  echo "Headless Shell ($HS)"
  printf "%-8s | %12s | %12s | %12s\n" "W" "ferridriver" "chromedp" "rod"
  for i in "${!WORKER_COUNTS[@]}"; do
    w="${WORKER_COUNTS[$i]}"
    eval "fd=\$Headless_Shell_fd_$i"; eval "cdp=\$Headless_Shell_cdp_$i"; eval "rod=\$Headless_Shell_rod_$i"
    printf "%-8s | %9sms | %9sms | %9sms\n" "$w" "$fd" "$cdp" "$rod"
  done
  echo ""
  echo "Regular Chrome ($RC)"
  printf "%-8s | %12s | %12s | %12s\n" "W" "ferridriver" "chromedp" "rod"
  for i in "${!WORKER_COUNTS[@]}"; do
    w="${WORKER_COUNTS[$i]}"
    eval "fd=\$Regular_Chrome_fd_$i"; eval "cdp=\$Regular_Chrome_cdp_$i"; eval "rod=\$Regular_Chrome_rod_$i"
    printf "%-8s | %9sms | %9sms | %9sms\n" "$w" "$fd" "$cdp" "$rod"
  done
} > "$RESULTS_FILE"
echo ""
echo "Saved: $RESULTS_FILE"
