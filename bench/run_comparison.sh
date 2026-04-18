#!/bin/bash
# Head-to-head benchmark: ferridriver vs Playwright
# Same 100 tests, same worker counts, same machine.
#
# Both runners tested in two Chrome modes:
#   - headless shell: purpose-built headless binary (default for both)
#   - regular chrome: full Chrome with --headless flag
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
    if (d == 0) print "N/A";
    else printf "%.2f", n / d;
  }'
}

# Ensure Playwright is installed
if ! command -v npx &>/dev/null || ! [ -d "$PW_DIR/node_modules" ]; then
  echo "Installing Playwright..."
  (cd "$PW_DIR" && npm install && npx playwright install chromium 2>/dev/null)
fi

# Ensure ferridriver NAPI is built
echo "Building ferridriver..."
(cd "$ROOT_DIR/crates/ferridriver-node" && bun run build:debug 2>/dev/null)

# Detect the regular Chrome binary from Playwright's CfT install
REGULAR_CHROME=""
for dir in "$HOME/Library/Caches/ms-playwright"/chromium-*/chrome-mac-arm64; do
  candidate="$dir/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"
  [ -x "$candidate" ] && REGULAR_CHROME="$candidate" && break
done
if [ -z "$REGULAR_CHROME" ]; then
  for dir in "$HOME/Library/Caches/ms-playwright"/chromium-*/chrome-mac-x64; do
    candidate="$dir/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"
    [ -x "$candidate" ] && REGULAR_CHROME="$candidate" && break
  done
fi
if [ -z "$REGULAR_CHROME" ]; then
  for dir in "$HOME/.cache/ms-playwright"/chromium-*/chrome-linux64; do
    candidate="$dir/chrome"
    [ -x "$candidate" ] && REGULAR_CHROME="$candidate" && break
  done
fi
[ -z "$REGULAR_CHROME" ] && [ -x "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" ] && \
  REGULAR_CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"

echo ""
echo "================================================================="
echo "  ferridriver vs Playwright -- $NUM_TESTS tests, avg $RUNS runs"
echo "  navigate + click + evaluate, data URLs, headless Chrome"
echo "================================================================="
if [ -n "$REGULAR_CHROME" ]; then
  echo "  Regular Chrome: $(basename "$(dirname "$(dirname "$(dirname "$REGULAR_CHROME")")")" 2>/dev/null || echo "$REGULAR_CHROME")"
fi
echo ""

# Helper: run N times in a subshell, return average ms.
run_avg() {
  local cmd="$1"
  local dir="$2"
  local env_prefix="${3:-}"
  local total=0
  local ok=0
  for ((r=1; r<=RUNS; r++)); do
    local start end elapsed
    start=$(date +%s%N)
    (cd "$dir" && eval "$env_prefix $cmd" >/dev/null 2>&1)
    end=$(date +%s%N)
    elapsed=$(( (end - start) / 1000000 ))
    # Reject suspiciously fast runs (< 500ms for 100 tests = likely crash/skip)
    if [ "$elapsed" -lt 500 ]; then
      echo "FAIL"
      return 1
    fi
    total=$((total + elapsed))
    ok=$((ok + 1))
  done
  echo $((total / ok))
}

# Playwright config: headless shell (default when headless=true, no channel)
pw_hs_config() {
  cat > "$PW_DIR/playwright.config.ts" << 'EOF'
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
EOF
}

# Playwright config: regular Chrome (channel: 'chromium' forces full browser)
pw_chrome_config() {
  cat > "$PW_DIR/playwright.config.ts" << EOF
import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: '.',
  testMatch: 'bench_compare.spec.ts',
  fullyParallel: true,
  timeout: 30000,
  retries: 0,
  reporter: 'null',
  use: { headless: true, channel: 'chromium' },
});
EOF
}

FD_CMD="node $ROOT_DIR/packages/ferridriver-test/dist/cli.js test"
FD_SPEC="$FD_DIR/bench_compare.spec.ts"

# ── Headless Shell mode ───────────────────────────────────────────
echo "--- Mode: Headless Shell (purpose-built headless binary) ---"
echo ""
printf "%-12s │ %12s │ %12s │ %8s\n" "Workers" "Playwright" "ferridriver" "Speedup"
printf "─────────────┼──────────────┼──────────────┼──────────\n"

pw_hs_config

declare -a pw_hs_results fd_hs_results

for workers in "${WORKER_COUNTS[@]}"; do
  pw_ms=$(run_avg "npx playwright test --workers=$workers" "$PW_DIR")
  pw_hs_results+=("$pw_ms")

  fd_ms=$(run_avg "$FD_CMD -j $workers $FD_SPEC" "$ROOT_DIR")
  fd_hs_results+=("$fd_ms")

  speedup=$(calc_ratio "$pw_ms" "$fd_ms")
  printf "%-12s │ %9sms │ %9sms │ %6sx\n" "$workers" "$pw_ms" "$fd_ms" "$speedup"
done

echo ""

# ── Regular Chrome mode ───────────────────────────────────────────
if [ -n "$REGULAR_CHROME" ]; then
  echo "--- Mode: Regular Chrome (full browser + --headless flag) ---"
  echo ""
  printf "%-12s │ %12s │ %12s │ %8s\n" "Workers" "Playwright" "ferridriver" "Speedup"
  printf "─────────────┼──────────────┼──────────────┼──────────\n"

  pw_chrome_config

  declare -a pw_ch_results fd_ch_results

  for workers in "${WORKER_COUNTS[@]}"; do
    pw_ms=$(run_avg "npx playwright test --workers=$workers" "$PW_DIR")
    pw_ch_results+=("$pw_ms")

    fd_ms=$(run_avg "$FD_CMD -j $workers $FD_SPEC" "$ROOT_DIR" "CHROMIUM_PATH=\"$REGULAR_CHROME\"")
    fd_ch_results+=("$fd_ms")

    speedup=$(calc_ratio "$pw_ms" "$fd_ms")
    printf "%-12s │ %9sms │ %9sms │ %6sx\n" "$workers" "$pw_ms" "$fd_ms" "$speedup"
  done

  echo ""
fi

echo "Each cell is average of $RUNS runs. $NUM_TESTS tests per run."
echo "Tests: navigate data URL, click button, evaluate JS (33/33/34 split)."
echo ""

# Save to file
{
  echo "Benchmark: $(date -Iseconds)"
  echo "Tests: $NUM_TESTS, Runs: $RUNS"
  echo ""
  echo "--- Headless Shell ---"
  printf "%-12s │ %12s │ %12s │ %8s\n" "Workers" "Playwright" "ferridriver" "Speedup"
  printf "─────────────┼──────────────┼──────────────┼──────────\n"
  for i in "${!WORKER_COUNTS[@]}"; do
    w="${WORKER_COUNTS[$i]}"
    pw="${pw_hs_results[$i]}"
    fd="${fd_hs_results[$i]}"
    sp=$(calc_ratio "$pw" "$fd")
    printf "%-12s │ %9sms │ %9sms │ %6sx\n" "$w" "$pw" "$fd" "$sp"
  done
  if [ -n "$REGULAR_CHROME" ]; then
    echo ""
    echo "--- Regular Chrome ---"
    printf "%-12s │ %12s │ %12s │ %8s\n" "Workers" "Playwright" "ferridriver" "Speedup"
    printf "─────────────┼──────────────┼──────────────┼──────────\n"
    for i in "${!WORKER_COUNTS[@]}"; do
      w="${WORKER_COUNTS[$i]}"
      pw="${pw_ch_results[$i]}"
      fd="${fd_ch_results[$i]}"
      sp=$(calc_ratio "$pw" "$fd")
      printf "%-12s │ %9sms │ %9sms │ %6sx\n" "$w" "$pw" "$fd" "$sp"
    done
  fi
} > "$RESULTS_FILE"
echo "Results saved to $RESULTS_FILE"
