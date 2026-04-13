#!/bin/bash
# Head-to-head benchmark: ferridriver vs Playwright
# Same 100 tests, same worker counts, same machine.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PW_DIR="$SCRIPT_DIR/pw-bench"
FD_DIR="$SCRIPT_DIR/fd-bench"
RESULTS_FILE="$SCRIPT_DIR/results/comparison.txt"

NUM_TESTS=100
WORKER_COUNTS=(1 2 4 8)
RUNS=3  # Average over N runs

mkdir -p "$SCRIPT_DIR/results"

calc_ratio() {
  local numerator="$1"
  local denominator="$2"
  awk -v n="$numerator" -v d="$denominator" 'BEGIN {
    if (d == 0) print "∞";
    else printf "%.2f", n / d;
  }'
}

# Ensure Playwright is installed
if ! command -v npx &>/dev/null || ! [ -d "$PW_DIR/node_modules" ]; then
  echo "Installing Playwright..."
  cd "$PW_DIR" && npm install && npx playwright install chromium 2>/dev/null
fi

# Ensure ferridriver NAPI is built
echo "Building ferridriver..."
cd "$ROOT_DIR/crates/ferridriver-napi" && bun run build:debug 2>/dev/null

echo ""
echo "╔══════════════════════════════════════════════════════════════╗"
echo "║         ferridriver vs Playwright — 100 tests               ║"
echo "║  navigate + click + evaluate, data URLs, headless Chrome    ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""

# Helper: run N times, return average ms
run_avg() {
  local cmd="$1"
  local dir="$2"
  local total=0
  for ((r=1; r<=RUNS; r++)); do
    local start end elapsed
    start=$(date +%s%N)
    cd "$dir" && eval "$cmd" >/dev/null 2>&1
    end=$(date +%s%N)
    elapsed=$(( (end - start) / 1000000 ))
    total=$((total + elapsed))
  done
  echo $((total / RUNS))
}

# Configure Playwright to use the compare spec
cat > "$PW_DIR/playwright.config.ts" << 'PWCFG'
import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: '.',
  testMatch: 'bench_compare.spec.ts',
  fullyParallel: true,
  timeout: 30000,
  retries: 0,
  reporter: 'null',
  use: { headless: true },
});
PWCFG

# Print header
printf "%-12s │ %12s │ %12s │ %8s\n" "Workers" "Playwright" "ferridriver" "Speedup"
printf "─────────────┼──────────────┼──────────────┼──────────\n"

# Results for final summary
declare -a pw_results fd_results

for workers in "${WORKER_COUNTS[@]}"; do
  # Playwright
  pw_ms=$(run_avg "npx playwright test --workers=$workers" "$PW_DIR")
  pw_results+=("$pw_ms")

  # ferridriver
  fd_ms=$(run_avg "node $ROOT_DIR/packages/ferridriver-test/dist/cli.js test -j $workers $FD_DIR/bench_compare.spec.ts" "$ROOT_DIR")
  fd_results+=("$fd_ms")

  # Speedup
  speedup=$(calc_ratio "$pw_ms" "$fd_ms")

  printf "%-12s │ %9sms │ %9sms │ %6sx\n" "$workers" "$pw_ms" "$fd_ms" "$speedup"
done

echo ""
echo "Each cell is average of $RUNS runs. $NUM_TESTS tests per run."
echo "Tests: navigate data URL, click button, evaluate JS (33/33/34 split)."
echo ""

# Save to file
{
  echo "Benchmark: $(date -Iseconds)"
  echo "Tests: $NUM_TESTS, Runs: $RUNS"
  printf "%-12s │ %12s │ %12s │ %8s\n" "Workers" "Playwright" "ferridriver" "Speedup"
  printf "─────────────┼──────────────┼──────────────┼──────────\n"
  for i in "${!WORKER_COUNTS[@]}"; do
    w="${WORKER_COUNTS[$i]}"
    pw="${pw_results[$i]}"
    fd="${fd_results[$i]}"
    sp=$(calc_ratio "$pw" "$fd")
    printf "%-12s │ %9sms │ %9sms │ %6sx\n" "$w" "$pw" "$fd" "$sp"
  done
} > "$RESULTS_FILE"
echo "Results saved to $RESULTS_FILE"
