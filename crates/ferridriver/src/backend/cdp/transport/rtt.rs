//! Per-CDP-method round-trip latency aggregation (`FERRIDRIVER_RTT_STATS=1`).

use rustc_hash::FxHashMap;

/// Aggregated per-CDP-method round-trip statistics. Updated when a
/// response lands in [`CdpDispatcher::dispatch_message`] and the
/// matching `PendingEntry` is removed. Dumped to stderr on
/// [`CdpDispatcher::drop`] when `FERRIDRIVER_RTT_STATS=1` is set.
#[derive(Default)]
pub(super) struct RttBucket {
  count: u64,
  total_ns: u128,
  max_ns: u128,
}

/// Format nanoseconds as fixed-point milliseconds with 2 decimals,
/// using integer arithmetic to dodge the `u128 -> f64` precision-loss
/// lint without suppressions.
fn fmt_ms2(ns: u128) -> String {
  let ms = ns / 1_000_000;
  let dec = (ns % 1_000_000) / 10_000;
  format!("{ms}.{dec:02}")
}

/// Format nanoseconds as fixed-point microseconds with 1 decimal.
fn fmt_us1(ns: u128) -> String {
  let us = ns / 1_000;
  let dec = (ns % 1_000) / 100;
  format!("{us}.{dec}")
}

/// Average microseconds with 1 decimal — `total_ns / count / 1000`
/// in integer space to dodge precision-loss lint.
fn fmt_avg_us1(total_ns: u128, count: u64) -> String {
  if count == 0 {
    return "0.0".to_string();
  }
  let total_us10 = total_ns * 10 / 1_000;
  let avg_us10 = total_us10 / u128::from(count);
  let us = avg_us10 / 10;
  let dec = avg_us10 % 10;
  format!("{us}.{dec}")
}

#[derive(Default)]
pub(crate) struct RttStats {
  pub(super) buckets: FxHashMap<String, RttBucket>,
}

impl RttStats {
  pub(super) fn record(&mut self, method: &str, elapsed_ns: u128) {
    let entry = self.buckets.entry(method.to_string()).or_default();
    entry.count += 1;
    entry.total_ns += elapsed_ns;
    if elapsed_ns > entry.max_ns {
      entry.max_ns = elapsed_ns;
    }
  }

  pub(super) fn dump(&self) {
    use std::fmt::Write as _;
    if self.buckets.is_empty() {
      return;
    }
    let mut rows: Vec<(&String, &RttBucket)> = self.buckets.iter().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1.total_ns));
    let total_count: u64 = self.buckets.values().map(|b| b.count).sum();
    let total_ns: u128 = self.buckets.values().map(|b| b.total_ns).sum();
    let mut report = format!(
      "─── ferridriver CDP RTT stats ─── total_calls={total_count}  total_time={}ms\n",
      fmt_ms2(total_ns)
    );
    let _ = writeln!(
      report,
      "  {:<48}  {:>7}  {:>10}  {:>10}  {:>10}",
      "method", "count", "total_ms", "avg_us", "max_us"
    );
    for (method, bucket) in rows {
      let _ = writeln!(
        report,
        "  {:<48}  {:>7}  {:>10}  {:>10}  {:>10}",
        method,
        bucket.count,
        fmt_ms2(bucket.total_ns),
        fmt_avg_us1(bucket.total_ns, bucket.count),
        fmt_us1(bucket.max_ns),
      );
    }
    // From an atexit hook too, where stderr may already be closed: a
    // failed write is dropped rather than panicking as `eprintln!` would.
    let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), report.as_bytes());
  }
}

/// Returns true when the `FERRIDRIVER_RTT_STATS` env var is set to
/// any truthy value (`1`, `true`, `yes`). Cached via [`std::sync::OnceLock`]
/// so the env-var lookup happens once per process. First truthy
/// observation also registers a libc-level `atexit` hook that dumps
/// the aggregated [`global_rtt_stats`] — covers process-exit paths
/// (NAPI / cargo-test harness) where individual dispatcher Drops do
/// not run because reader/writer tokio tasks still hold Arc clones.
pub(super) fn rtt_stats_enabled() -> bool {
  static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  *ENABLED.get_or_init(|| {
    let on = std::env::var("FERRIDRIVER_RTT_STATS").is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"));
    if on {
      // SAFETY: atexit takes a `extern "C" fn()` and stores it in a
      // process-global list. The handler reads from a `Mutex<RttStats>`
      // owned by a `OnceLock` so its lifetime spans process exit.
      #[expect(unsafe_code, reason = "libc::atexit has no safe wrapper in std or rustix")]
      unsafe {
        libc::atexit(rtt_atexit_dump);
      }
    }
    on
  })
}

/// Process-global aggregate of every [`CdpDispatcher`]'s RTT buckets.
/// Each dispatcher merges its local stats here on drop; the libc
/// atexit hook registered by [`rtt_stats_enabled`] dumps the global
/// table on process exit (covers `process::exit` / NAPI paths where
/// per-task dispatcher drops never run).
pub(super) fn global_rtt_stats() -> &'static std::sync::Mutex<RttStats> {
  static GLOBAL: std::sync::OnceLock<std::sync::Mutex<RttStats>> = std::sync::OnceLock::new();
  GLOBAL.get_or_init(|| std::sync::Mutex::new(RttStats::default()))
}

extern "C" fn rtt_atexit_dump() {
  if let Ok(stats) = global_rtt_stats().lock()
    && !stats.buckets.is_empty()
  {
    stats.dump();
  }
}

/// Explicit dump entry-point for runtimes whose process-exit path
/// doesn't trigger libc `atexit` (Bun + some Node configurations).
/// CLI bridges call this just before `process.exit` so the
/// `FERRIDRIVER_RTT_STATS=1` table prints reliably.
pub fn dump_global_rtt_stats() {
  if !rtt_stats_enabled() {
    return;
  }
  if let Ok(stats) = global_rtt_stats().lock()
    && !stats.buckets.is_empty()
  {
    stats.dump();
  }
}
