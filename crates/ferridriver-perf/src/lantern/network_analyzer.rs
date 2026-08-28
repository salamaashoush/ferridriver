//! What the observed trace says about the network it was recorded on.
//!
//! From devtools-frontend `lantern/core/NetworkAnalyzer.ts`. The
//! simulator supplies its own connection latency, so what it needs from
//! the observation is how much EXTRA latency each origin adds beyond the
//! fastest one, plus how long each origin's server took to think.

use rustc_hash::FxHashMap;

use crate::handlers::network::NetworkRequest;
use crate::lantern::graph::RequestFacts;
use crate::units::micros_to_ms;

/// Used for an origin whose timings the trace did not carry.
pub const DEFAULT_SERVER_RESPONSE_TIME: f64 = 30.0;

#[derive(Debug, Clone, Default)]
pub struct NetworkAnalysis {
  /// Milliseconds, the fastest round trip observed anywhere.
  pub rtt: f64,
  /// Per origin, how much slower than `rtt` that origin was.
  pub additional_rtt_by_origin: FxHashMap<String, f64>,
  /// Per origin, the median time the server spent before responding.
  pub server_response_time_by_origin: FxHashMap<String, f64>,
  /// Bits per second, measured across the observed download windows.
  pub throughput: f64,
}

impl NetworkAnalysis {
  #[must_use]
  pub fn analyze(requests: &[NetworkRequest], facts: &[RequestFacts]) -> Self {
    let rtt_by_origin = estimate_rtt_by_origin(requests, facts);
    // The minimum is taken as the connection latency because the
    // simulator models that itself; what it wants to know is the excess.
    let minimum_rtt = rtt_by_origin.values().copied().fold(f64::INFINITY, f64::min);
    let minimum_rtt = if minimum_rtt.is_finite() { minimum_rtt } else { 0.0 };

    let mut additional_rtt_by_origin = FxHashMap::default();
    let mut server_response_time_by_origin = FxHashMap::default();
    for (origin, responses) in group_response_times(requests, facts, &rtt_by_origin) {
      let origin_rtt = rtt_by_origin.get(&origin).copied().unwrap_or(minimum_rtt);
      additional_rtt_by_origin.insert(origin.clone(), (origin_rtt - minimum_rtt).max(0.0));
      server_response_time_by_origin.insert(origin, median(responses));
    }

    Self {
      rtt: minimum_rtt,
      additional_rtt_by_origin,
      server_response_time_by_origin,
      throughput: estimate_throughput(requests, facts),
    }
  }
}

/// The fastest connection setup seen per origin, which is the closest
/// thing the trace has to that origin's round-trip time.
fn estimate_rtt_by_origin(requests: &[NetworkRequest], facts: &[RequestFacts]) -> FxHashMap<String, f64> {
  let mut by_origin: FxHashMap<String, f64> = FxHashMap::default();

  for (request, fact) in requests.iter().zip(facts) {
    if fact.delivery.is_connectionless() || fact.delivery.is_cached() {
      continue;
    }
    // The TCP handshake is one round trip; TLS adds more, so the
    // connect phase alone is the cleanest single-RTT observation.
    let connect_ms = micros_to_ms(request.timing.initial_connection);
    if connect_ms <= 0.0 {
      continue;
    }
    // TLS rides on the same connect window, so subtract it back out.
    let ssl_ms = micros_to_ms(request.timing.ssl);
    let rtt = (connect_ms - ssl_ms).max(connect_ms / 2.0);
    by_origin
      .entry(fact.origin.clone())
      .and_modify(|current| *current = current.min(rtt))
      .or_insert(rtt);
  }
  by_origin
}

/// Time each origin's server spent between receiving the request and
/// starting to answer, with the network round trip taken out.
fn group_response_times(
  requests: &[NetworkRequest],
  facts: &[RequestFacts],
  rtt_by_origin: &FxHashMap<String, f64>,
) -> FxHashMap<String, Vec<f64>> {
  let mut by_origin: FxHashMap<String, Vec<f64>> = FxHashMap::default();
  for (request, fact) in requests.iter().zip(facts) {
    if fact.delivery.is_connectionless() || fact.delivery.is_cached() {
      continue;
    }
    let observed = micros_to_ms(request.timing.server_response_time);
    if observed <= 0.0 {
      continue;
    }
    // What the server actually cost is the wait minus the round trip
    // getting there and back.
    let rtt = rtt_by_origin.get(&fact.origin).copied().unwrap_or(0.0);
    by_origin
      .entry(fact.origin.clone())
      .or_default()
      .push((observed - rtt).max(0.0));
  }
  by_origin
}

/// Average throughput in bits per second over the windows in which
/// bytes were actually arriving.
///
/// Summing bytes over wall time would under-report badly, because a page
/// load is mostly gaps.
fn estimate_throughput(requests: &[NetworkRequest], facts: &[RequestFacts]) -> f64 {
  let mut total_bytes = 0f64;
  let mut windows: Vec<(i64, i64)> = Vec::new();

  for (request, fact) in requests.iter().zip(facts) {
    if fact.delivery.is_connectionless() || fact.delivery.is_cached() || fact.transfer_size <= 0 {
      continue;
    }
    let start = request.timing.download_start;
    let end = request.timing.finish_time;
    if end <= start {
      continue;
    }
    total_bytes += crate::units::count_to_f64(fact.transfer_size);
    windows.push((start, end));
  }
  if windows.is_empty() {
    return 0.0;
  }

  // Overlapping downloads share the link, so their windows are merged
  // rather than counted twice.
  windows.sort_unstable();
  let mut merged_us = 0i64;
  let (mut current_start, mut current_end) = windows[0];
  for (start, end) in windows.into_iter().skip(1) {
    if start > current_end {
      merged_us += current_end - current_start;
      current_start = start;
      current_end = end;
    } else {
      current_end = current_end.max(end);
    }
  }
  merged_us += current_end - current_start;

  let seconds = micros_to_ms(merged_us) / 1000.0;
  if seconds <= 0.0 {
    0.0
  } else {
    total_bytes * 8.0 / seconds
  }
}

fn median(mut values: Vec<f64>) -> f64 {
  if values.is_empty() {
    return DEFAULT_SERVER_RESPONSE_TIME;
  }
  values.sort_by(f64::total_cmp);
  let middle = values.len() / 2;
  if values.len().is_multiple_of(2) {
    f64::midpoint(values[middle - 1], values[middle])
  } else {
    values[middle]
  }
}
