#!/bin/bash
# Benchmark Playwright Test with different worker counts
set -e

cd "$(dirname "$0")"

echo ""
echo "=== Playwright Test benchmark ==="
echo ""

for workers in 1 2 4; do
  echo -n "  $workers worker(s): "
  start=$(date +%s%N)
  npx playwright test --workers=$workers 2>/dev/null
  end=$(date +%s%N)
  elapsed_ms=$(( (end - start) / 1000000 ))
  per_test=$(echo "scale=1; $elapsed_ms / 50" | bc)
  tps=$(echo "scale=1; 50000 / $elapsed_ms" | bc)
  echo "50 tests => ${elapsed_ms}ms total, ${per_test}ms/test, ${tps} tests/sec"
done

echo ""
echo "=== benchmark complete ==="
echo ""
