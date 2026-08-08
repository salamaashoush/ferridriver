#!/usr/bin/env bash
# Generate the synthetic workloads that BENCHMARKING.md's decomposition
# subtracts from each other. They are deliberately boring: the point is
# that D - E isolates DOM work, E - F isolates context+page creation, and
# F isolates process startup, so each one must differ from the next by
# exactly one ingredient and nothing else.
#
# Output lands in target/bench-src/<name>/ (git-ignored, and the bench
# script copies specs out of it before running).
#
# Usage: bench-workloads.sh [tests] [files]

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TESTS="${1:-96}"
FILES="${2:-8}"
PER_FILE=$((TESTS / FILES))
OUT="$REPO_ROOT/target/bench-src"

gen() {
  local name="$1" body="$2"
  rm -rf "${OUT:?}/$name"
  mkdir -p "$OUT/$name"
  for f in $(seq 1 "$FILES"); do
    {
      echo "import { test, expect } from '@playwright/test';"
      for t in $(seq 1 "$PER_FILE"); do
        printf '%s\n' "${body//__N__/$f-$t}"
      done
    } >"$OUT/$name/spec$f.spec.ts"
  done
  echo "  $name: $FILES files x $PER_FILE tests = $((FILES * PER_FILE))"
}

# F — no fixtures at all. Measures pure runner dispatch: discovery,
# bundling, worker scheduling, reporting. No browser is ever touched.
gen no-fixture "test('t__N__', () => { expect(1).toBe(1); });"

# E — requests the { page } fixture and does nothing with it. E minus F
# is therefore exactly one context create + one page create + their
# teardown, per test.
gen page-fixture "test('t__N__', async ({ page }) => { void page; });"

# D — hermetic DOM work on top of E's page. No network: setContent only,
# so D minus E is the DOM/protocol work and nothing else.
gen dom "test('t__N__', async ({ page }) => {
  await page.setContent('<h1>hello __N__</h1><ul><li>a</li><li>b</li><li>c</li></ul><input id=i>');
  await expect(page.locator('h1')).toHaveText('hello __N__');
  await page.fill('#i', 'typed');
  expect(await page.locator('li').count()).toBe(3);
  expect(await page.title()).toBe('');
});"

echo "workloads in $OUT"
