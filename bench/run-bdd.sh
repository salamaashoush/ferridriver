#!/usr/bin/env bash
# BDD head-to-head bench: ferridriver bdd vs playwright-bdd.
#
# 1000 scenarios across 7 .feature files (todos, forms-valid,
# forms-invalid, blog-list, blog-detail, dashboard, wizard) — same
# .feature inputs run via:
#   - ferridriver bdd       (Rust step impls in ferridriver-bdd)
#   - playwright-bdd        (TS step impls in bench/pw-bdd/steps)
#
# Both target the bench/app webServer (Bun/React/Tailwind kitchen
# sink) and Chrome from ~/.cache/ms-playwright.
#
# Usage:
#   bench/run-bdd.sh                # full matrix, both runners
#   bench/run-bdd.sh fd-only        # ferridriver only
#   bench/run-bdd.sh pw-only        # playwright only
#   bench/run-bdd.sh smoke          # 4 workers x 1 run (sanity)
#   bench/run-bdd.sh scaling        # 1/2/4/8 workers x 1 run
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RESULTS_FILE="$SCRIPT_DIR/results/bdd.txt"
mkdir -p "$SCRIPT_DIR/results"

WORKER_COUNTS=(2 4 8)
RUNS=2
MODE="${1:-all}"
case "$MODE" in
  smoke)   WORKER_COUNTS=(4); RUNS=1 ;;
  scaling) WORKER_COUNTS=(1 2 4 8); RUNS=1 ;;
esac

# 1. Generate .feature files (idempotent).
echo "Generating .feature files…"
"$SCRIPT_DIR/bdd-features/generate.sh" >/dev/null

# 2. Build ferridriver release binary.
echo "Building ferridriver release…"
(cd "$ROOT_DIR" && cargo build --release --bin ferridriver >/dev/null 2>&1)
FD_BIN="$ROOT_DIR/target/release/ferridriver"

# 3. Build app dist.
echo "Building app dist…"
(cd "$SCRIPT_DIR/app" && bun install >/dev/null 2>&1 && bun run build >/dev/null 2>&1)

# 4. Install pw-bdd deps if missing.
if [ "$MODE" != "fd-only" ] && [ ! -d "$SCRIPT_DIR/pw-bdd/node_modules/playwright-bdd" ]; then
  echo "Installing pw-bdd deps…"
  (cd "$SCRIPT_DIR/pw-bdd" && bun install >/dev/null 2>&1)
fi

# 5. Pre-generate playwright-bdd specs (one-shot).
if [ "$MODE" != "fd-only" ]; then
  echo "Generating playwright-bdd specs…"
  (cd "$SCRIPT_DIR/pw-bdd" && bunx bddgen >/dev/null 2>&1)
fi

cleanup() {
  pkill -f "ferridriver-pipe-" 2>/dev/null || true
  pkill -f "playwright_chromiumdev_profile-" 2>/dev/null || true
}
trap cleanup EXIT

# Locate Chrome (prefer headless shell for headless bench).
HS=""
for cand in \
  "$HOME/.cache/ms-playwright/chromium_headless_shell-1217/chrome-headless-shell-linux64/chrome-headless-shell" \
  "$HOME/Library/Caches/ms-playwright/chromium_headless_shell-1217/chrome-headless-shell-mac-arm64/chrome-headless-shell"; do
  [ -x "$cand" ] && HS="$cand" && break
done
RC=""
for cand in \
  "$HOME/.cache/ms-playwright/chromium-1217/chrome-linux64/chrome" \
  "$HOME/Library/Caches/ms-playwright/chromium-1217/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"; do
  [ -x "$cand" ] && RC="$cand" && break
done

run_one() {
  # $1 = command; $2 = cwd; prints elapsed ms.
  local cmd="$1" cwd="$2"
  local t0=$(date +%s%N)
  if (cd "$cwd" && eval "$cmd" >/dev/null 2>&1); then
    local t1=$(date +%s%N)
    echo $(( (t1 - t0) / 1000000 ))
  else
    echo "FAIL"
  fi
}

run_avg() {
  local cmd="$1" cwd="$2"
  local total=0 ok=0 t
  for _ in $(seq 1 "$RUNS"); do
    t=$(run_one "$cmd" "$cwd")
    if [[ "$t" =~ ^[0-9]+$ ]]; then total=$((total + t)); ok=$((ok + 1)); fi
  done
  if [ "$ok" -eq 0 ]; then echo "FAIL"; else echo $((total / ok)); fi
}

echo ""
echo "================================================================="
echo "  ferridriver bdd vs playwright-bdd — 1000 scenarios"
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
      pw=$(CHROMIUM_PATH="$HS" run_avg "bunx playwright test --workers=$w" "$SCRIPT_DIR/pw-bdd")
    fi
    if [ "$MODE" != "pw-only" ]; then
      fd=$(run_avg "$FD_BIN bdd --workers $w --headless --executable-path '$HS'" "$SCRIPT_DIR/fd-bdd")
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
      pw=$(CHROMIUM_PATH="$RC" run_avg "bunx playwright test --workers=$w" "$SCRIPT_DIR/pw-bdd")
    fi
    if [ "$MODE" != "pw-only" ]; then
      fd=$(run_avg "$FD_BIN bdd --workers $w --headless --executable-path '$RC'" "$SCRIPT_DIR/fd-bdd")
    fi
    fd_rc+=("$fd"); pw_rc+=("$pw")
    sp=$(awk -v a="$pw" -v b="$fd" 'BEGIN { if (a !~ /^[0-9]+$/ || b !~ /^[0-9]+$/ || b<=0) print "N/A"; else printf "%.2fx", a/b; }')
    printf "%-12s | %9sms | %9sms | %8s\n" "$w" "$pw" "$fd" "$sp"
  done
fi

# Save results.
{
  echo "ferridriver bdd vs playwright-bdd — 1000 scenarios"
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
