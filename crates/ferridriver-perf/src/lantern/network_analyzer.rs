//! What the observed trace says about the network it was recorded on.
//!
//! From devtools-frontend `lantern/core/NetworkAnalyzer.ts`. The
//! simulator supplies its own connection latency, so what it needs from
//! the observation is how much EXTRA latency each origin adds beyond the
//! fastest one, plus how long each origin's server took to think.
//!
//! The round-trip time is estimated four ways, in order of how much the
//! trace has to say. A connection the trace watched being opened gives
//! the answer directly; where every connection was reused there is
//! nothing to watch, and three coarse estimators infer it from how long
//! the response took instead. Using only the first one and giving up
//! otherwise reads as tidier and is wrong: a page whose connections were
//! all warm comes out with a round trip of zero.

use rustc_hash::FxHashMap;

use crate::event::ResourceTiming;
use crate::handlers::network::NetworkRequest;
use crate::lantern::graph::RequestFacts;
use crate::units::micros_to_ms;

/// Used for an origin whose timings the trace did not carry.
pub const DEFAULT_SERVER_RESPONSE_TIME: f64 = 30.0;

/// TCP's initial congestion window, in bytes. A response no larger than
/// this arrives in one round trip, so it says nothing about how long a
/// round trip takes.
const INITIAL_CWD_BYTES: f64 = 14.0 * 1024.0;

/// How much of the time to first byte is the server thinking rather than
/// the network moving, when nothing better is known.
const DEFAULT_SERVER_RESPONSE_PERCENTAGE: f64 = 0.4;

/// The three resource types whose time to first byte is nearly all
/// server: a document or an API call is generated, not read off disk.
const SERVER_RESPONSE_PERCENTAGE_OF_TTFB: [(&str, f64); 3] = [("Document", 0.9), ("XHR", 0.9), ("Fetch", 0.9)];

/// Coarse estimates are scaled down because they measure several round
/// trips at once and attribute the whole of it to one.
const COARSE_ESTIMATE_MULTIPLIER: f64 = 0.3;

/// No round trip is credibly faster than this once inferred from a
/// response time rather than watched directly.
const MINIMUM_COARSE_RTT_MS: f64 = 3.0;

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
    let reused = connection_reuse(requests, facts);
    let rtt_by_origin = minimum_rtt_by_origin(requests, facts, &reused);
    let minimum_rtt = rtt_by_origin.values().copied().fold(f64::INFINITY, f64::min);
    let minimum_rtt = if minimum_rtt.is_finite() { minimum_rtt } else { 0.0 };

    let mut additional_rtt_by_origin = FxHashMap::default();
    let mut server_response_time_by_origin = FxHashMap::default();
    for (origin, mut responses) in response_times_by_origin(requests, facts, &rtt_by_origin) {
      let origin_rtt = rtt_by_origin.get(&origin).copied().unwrap_or(minimum_rtt);
      additional_rtt_by_origin.insert(origin.clone(), origin_rtt - minimum_rtt);
      server_response_time_by_origin.insert(origin, median(&mut responses));
    }

    Self {
      rtt: minimum_rtt,
      additional_rtt_by_origin,
      server_response_time_by_origin,
      throughput: estimate_throughput(requests, facts),
    }
  }
}

/// Whether each request rode a connection that already existed.
///
/// Chrome's own `connectionReused` flag is only believed when every
/// connection in the trace was also seen being opened; otherwise the
/// trace started mid-session and the flag describes a history it did not
/// record. The fallback infers reuse from the timings: a request that
/// started after something on the same origin had already finished could
/// have reused that connection, and HTTP/2 always can.
fn connection_reuse(requests: &[NetworkRequest], facts: &[RequestFacts]) -> Vec<bool> {
  let mut started_by_connection: FxHashMap<i64, bool> = FxHashMap::default();
  for request in requests {
    let entry = started_by_connection.entry(request.connection_id).or_insert(false);
    *entry = *entry || !request.connection_reused;
  }
  let trustworthy = started_by_connection.len() > 1 && started_by_connection.values().all(|started| *started);
  if trustworthy {
    return requests.iter().map(|r| r.connection_reused).collect();
  }

  let mut reused = vec![false; requests.len()];
  for (origin, members) in group_by_origin(requests, facts) {
    let _ = origin;
    let earliest_finish = members
      .iter()
      .map(|index| requests[*index].timing.finish_time)
      .min()
      .unwrap_or(i64::MAX);
    for index in &members {
      let request = &requests[*index];
      reused[*index] = request.timing.send_start_time >= earliest_finish || request.protocol == "h2";
    }
    // Whatever went first cannot have reused anything.
    if let Some(first) = members
      .iter()
      .min_by_key(|index| requests[**index].timing.send_start_time)
    {
      reused[*first] = false;
    }
  }
  reused
}

fn group_by_origin(requests: &[NetworkRequest], facts: &[RequestFacts]) -> FxHashMap<String, Vec<usize>> {
  let mut grouped: FxHashMap<String, Vec<usize>> = FxHashMap::default();
  for (index, fact) in facts.iter().enumerate() {
    if fact.origin.starts_with("chrome-extension:") || index >= requests.len() {
      continue;
    }
    grouped.entry(fact.origin.clone()).or_default().push(index);
  }
  grouped
}

/// The fastest round trip each origin showed, by whichever estimator the
/// trace supports.
fn minimum_rtt_by_origin(
  requests: &[NetworkRequest],
  facts: &[RequestFacts],
  reused: &[bool],
) -> FxHashMap<String, f64> {
  let mut by_origin = FxHashMap::default();
  for (origin, members) in group_by_origin(requests, facts) {
    let mut estimates: Vec<f64> = Vec::new();
    for index in &members {
      let request = &requests[*index];
      let Some(timing) = request.resource_timing.as_ref() else {
        continue;
      };
      if request.encoded_data_length <= 0 {
        continue;
      }
      estimates.extend(via_connection_timing(request, timing, reused[*index]));
    }
    // Only when nothing watched a connection open. The three coarse
    // estimators all measure a whole response and divide it down, so
    // mixing them with a directly observed handshake would drag the
    // answer towards the slowest thing on the page.
    if estimates.is_empty() {
      for index in &members {
        let request = &requests[*index];
        let Some(timing) = request.resource_timing.as_ref() else {
          continue;
        };
        if request.encoded_data_length <= 0 {
          continue;
        }
        let coarse = via_download_timing(request, timing, reused[*index])
          .into_iter()
          .chain(via_send_start_timing(request, timing, reused[*index]))
          .chain(via_headers_end_timing(request, timing, reused[*index]));
        estimates.extend(coarse.map(|estimate| estimate * COARSE_ESTIMATE_MULTIPLIER));
      }
    }
    if let Some(min) = estimates.iter().copied().reduce(f64::min) {
      by_origin.insert(origin, min);
    }
  }
  by_origin
}

/// The handshake itself, which is one round trip, or two when TLS
/// negotiated separately.
fn via_connection_timing(request: &NetworkRequest, timing: &ResourceTiming, reused: bool) -> Vec<f64> {
  if reused {
    return Vec::new();
  }
  let (start, end) = (timing.connect_start, timing.connect_end);
  if end >= 0.0 && start >= 0.0 && request.protocol.starts_with("h3") {
    return vec![end - start];
  }
  if timing.ssl_start >= 0.0 && timing.ssl_end >= 0.0 && (timing.ssl_start - start).abs() > f64::EPSILON {
    return vec![end - timing.ssl_start, timing.ssl_start - start];
  }
  if start >= 0.0 && end >= 0.0 {
    return vec![end - start];
  }
  Vec::new()
}

/// How long the body took after the first byte, divided by the number of
/// congestion-window doublings it needed.
fn via_download_timing(request: &NetworkRequest, timing: &ResourceTiming, reused: bool) -> Option<f64> {
  if reused {
    return None;
  }
  let transfer = crate::units::count_to_f64(request.encoded_data_length);
  if transfer <= INITIAL_CWD_BYTES || !timing.receive_headers_end.is_finite() || timing.receive_headers_end < 0.0 {
    return None;
  }
  let total = micros_to_ms(request.timing.finish_time - request.timing.send_start_time);
  let round_trips = (transfer / INITIAL_CWD_BYTES).log2();
  // Past five doublings the download is bandwidth-bound, and dividing
  // by the count stops saying anything about latency.
  if round_trips > 5.0 || round_trips <= 0.0 {
    return None;
  }
  Some((total - timing.receive_headers_end) / round_trips)
}

/// How long before the request could be sent, over the number of round
/// trips the connection setup needed.
fn via_send_start_timing(request: &NetworkRequest, timing: &ResourceTiming, reused: bool) -> Option<f64> {
  if reused || !timing.send_start.is_finite() || timing.send_start < 0.0 {
    return None;
  }
  Some(timing.send_start / setup_round_trips(request))
}

/// The time to first byte with the server's own share taken out.
fn via_headers_end_timing(request: &NetworkRequest, timing: &ResourceTiming, reused: bool) -> Option<f64> {
  if !timing.receive_headers_end.is_finite() || timing.receive_headers_end < 0.0 || request.resource_type.is_empty() {
    return None;
  }
  let server_share = SERVER_RESPONSE_PERCENTAGE_OF_TTFB
    .iter()
    .find(|(kind, _)| *kind == request.resource_type)
    .map_or(DEFAULT_SERVER_RESPONSE_PERCENTAGE, |(_, share)| *share);
  let server_time = timing.receive_headers_end * server_share;
  let round_trips = if reused { 1.0 } else { setup_round_trips(request) };
  Some(((timing.receive_headers_end - server_time) / round_trips).max(MINIMUM_COARSE_RTT_MS))
}

/// Round trips a fresh connection costs: the request itself, plus TCP
/// unless the protocol folds it in, plus TLS when there is any.
fn setup_round_trips(request: &NetworkRequest) -> f64 {
  let mut trips = 1.0;
  if !request.protocol.starts_with("h3") {
    trips += 1.0;
  }
  if request.url.starts_with("https:") {
    trips += 1.0;
  }
  trips
}

/// Time each origin's server spent between receiving the request and
/// starting to answer, with the network round trip taken out.
fn response_times_by_origin(
  requests: &[NetworkRequest],
  facts: &[RequestFacts],
  rtt_by_origin: &FxHashMap<String, f64>,
) -> FxHashMap<String, Vec<f64>> {
  let mut by_origin: FxHashMap<String, Vec<f64>> = FxHashMap::default();
  for (origin, members) in group_by_origin(requests, facts) {
    let rtt = rtt_by_origin.get(&origin).copied().unwrap_or(0.0);
    let mut estimates = Vec::new();
    for index in &members {
      let request = &requests[*index];
      let Some(timing) = request.resource_timing.as_ref() else {
        continue;
      };
      if !timing.receive_headers_end.is_finite()
        || timing.receive_headers_end < 0.0
        || !timing.send_end.is_finite()
        || timing.send_end < 0.0
      {
        continue;
      }
      let ttfb = timing.receive_headers_end - timing.send_end;
      estimates.push((ttfb - rtt).max(0.0));
    }
    if !estimates.is_empty() {
      by_origin.insert(origin, estimates);
    }
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
  // `(time, is_start)`, so overlapping downloads collapse into one
  // window rather than being counted twice.
  let mut boundaries: Vec<(i64, bool)> = Vec::new();

  for (request, fact) in requests.iter().zip(facts) {
    if fact.delivery.is_connectionless()
      || request.flags.outcome == crate::handlers::network::Outcome::Failed
      || !request.flags.finished()
      || request.status_code > 300
      || request.encoded_data_length <= 0
    {
      continue;
    }
    total_bytes += crate::units::count_to_f64(request.encoded_data_length);
    boundaries.push((request.timing.download_start, true));
    boundaries.push((request.timing.finish_time, false));
  }
  if boundaries.is_empty() {
    return 0.0;
  }
  boundaries.sort_unstable_by_key(|(time, is_start)| (*time, !*is_start));

  let mut inflight = 0i32;
  let mut window_start = 0i64;
  let mut total_us = 0i64;
  for (time, is_start) in boundaries {
    if is_start {
      if inflight == 0 {
        window_start = time;
      }
      inflight += 1;
    } else {
      inflight -= 1;
      if inflight == 0 {
        total_us += time - window_start;
      }
    }
  }

  let seconds = micros_to_ms(total_us) / 1000.0;
  if seconds <= 0.0 {
    0.0
  } else {
    total_bytes * 8.0 / seconds
  }
}

/// Upstream's median, which on an even count averages the two middle
/// values of the sorted list.
fn median(values: &mut [f64]) -> f64 {
  if values.is_empty() {
    return DEFAULT_SERVER_RESPONSE_TIME;
  }
  values.sort_by(f64::total_cmp);
  let middle = (values.len() - 1) / 2;
  if values.len().is_multiple_of(2) {
    f64::midpoint(values[middle], values[middle + 1])
  } else {
    values[middle]
  }
}
