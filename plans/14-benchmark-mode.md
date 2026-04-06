# Feature: Benchmark / Performance Mode

## Context
Browser automation libraries need performance measurement: how fast does a page load, how long does a complex interaction take, what's the p99 latency of a specific operation? A built-in benchmark mode provides statistical analysis without external tools, making performance regression detection part of the test workflow.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver-test` | `BenchmarkRunner` with statistical analysis |
| `ferridriver-test-macros` | `#[ferribench]` attribute macro |
| `ferridriver-cli` | `ferridriver bench` subcommand |
| `packages/ferridriver-test` | `bench()` API |

### Core Changes (ferridriver-test)
- New module `crates/ferridriver-test/src/bench.rs`:
  - `BenchmarkRunner`:
    - Runs a function N iterations (configurable, default 100).
    - Warmup phase: discard first M iterations (default 5).
    - Collects timing data per iteration.
    - Computes stats: `mean`, `median`, `p95`, `p99`, `min`, `max`, `std_dev`.
  - `BenchResult`:
    ```rust
    pub struct BenchResult {
      pub name: String,
      pub iterations: u64,
      pub mean: Duration,
      pub median: Duration,
      pub p95: Duration,
      pub p99: Duration,
      pub min: Duration,
      pub max: Duration,
      pub std_dev: Duration,
      pub ops_per_sec: f64,
    }
    ```
  - `BenchConfig`:
    ```rust
    pub struct BenchConfig {
      pub iterations: u64,      // default 100
      pub warmup: u64,          // default 5
      pub timeout: Duration,    // default 60s per benchmark
    }
    ```
  - `BenchSuite`: collection of benchmarks to run.
  - Output formatters:
    - Terminal table (aligned columns, color-coded).
    - JSON (machine-readable, for CI integration).
    - Comparison mode: load previous results from JSON, show deltas with +/- percentages.

### Rust API (ferridriver-test-macros)
- `#[ferribench]` macro:
  ```rust
  #[ferribench(iterations = 50, warmup = 3)]
  async fn page_load(pool: FixturePool) {
    let page = pool.get::<Page>().await;
    page.goto("https://example.com").await.unwrap();
  }
  ```
- Collected via `inventory` like tests.

### NAPI + TypeScript (ferridriver-napi, packages/ferridriver-test)
- `bench()` API:
  ```ts
  import { bench } from '@ferridriver/test';
  bench('page load', async ({ page }) => {
    await page.goto('https://example.com');
  }, { iterations: 50 });
  ```
- Results printed to terminal and optionally saved to JSON.

### CLI (ferridriver-cli)
- New subcommand: `ferridriver bench [OPTIONS] [FILES]`.
  - `--iterations <N>` — override iteration count.
  - `--warmup <N>` — override warmup count.
  - `--json` — output results as JSON.
  - `--compare <path>` — compare against previous results JSON.
  - `--save <path>` — save results to JSON for future comparison.
- Default: discovers all `#[ferribench]` functions or `bench()` calls, runs them sequentially.

### BDD Integration (ferridriver-bdd)
- Not applicable — benchmarks are programmatic, not BDD-style.

### Component Testing (ferridriver-ct-*)
- Benchmarks can measure component mount/render times.

## Implementation Steps
1. Create `crates/ferridriver-test/src/bench.rs` with `BenchmarkRunner`, `BenchResult`, `BenchConfig`.
2. Implement statistical analysis: mean, median, percentiles, std dev.
3. Implement terminal table output with aligned columns.
4. Implement JSON output/input for comparison.
5. Create `#[ferribench]` proc macro in `ferridriver-test-macros`.
6. Add `bench` subcommand to CLI.
7. Implement comparison mode: load baseline, compute deltas, flag regressions.
8. Add TS `bench()` API.
9. Test with real page load benchmarks.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver-test/src/bench.rs` | Create |
| `crates/ferridriver-test/src/lib.rs` | Modify — add `mod bench;` |
| `crates/ferridriver-test-macros/src/lib.rs` | Modify — add `#[ferribench]` |
| `crates/ferridriver-cli/src/cli.rs` | Modify — add `Bench` subcommand |
| `packages/ferridriver-test/src/test.ts` | Modify — add `bench()` function |

## Verification
- Unit test: `BenchmarkRunner` with a fixed-delay function produces correct stats.
- Unit test: median/p95/p99 calculations are correct for known distributions.
- Integration test: `ferridriver bench` discovers and runs benchmarks, outputs table.
- Integration test: `--json` produces valid JSON with all stats fields.
- Integration test: `--compare` shows deltas against baseline.
- Verify warmup iterations are excluded from stats.
