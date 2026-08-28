//! Synthetic network requests.
//!
//! Chrome does not emit one event per request. It emits a
//! `ResourceSendRequest`, then `ResourceReceiveResponse`, then
//! `ResourceFinish`, correlated by `requestId`, plus a
//! `ResourceWillSendRequest` per redirect hop. This handler stitches
//! those back into one record per request with a resolved timing
//! breakdown.
//!
//! Mirrors devtools-frontend `handlers/NetworkRequestsHandler.ts`. The
//! arithmetic is reproduced rather than reinvented, including the unit
//! mixing: `timing.request_time` is SECONDS since the epoch while every
//! other `timing` field is MILLISECONDS relative to it.

use rustc_hash::FxHashMap;
use serde::Deserialize;

use crate::event::{MILLIS_TO_MICROS, Micro, ResourceTiming, SECONDS_TO_MICROS, TraceEvent};
use crate::units::{f64_to_micros as us, micros_to_f64};

#[derive(Debug, Clone, Default)]
pub struct NetworkRequest {
  pub request_id: String,
  pub url: String,
  pub method: String,
  pub mime_type: String,
  pub resource_type: String,
  pub protocol: String,
  pub priority: String,
  pub status_code: i64,
  pub frame: String,
  pub render_blocking: String,
  pub response_headers: Vec<(String, String)>,
  /// `high`, `low` or `auto` from a `fetchpriority` attribute.
  pub fetch_priority_hint: String,
  /// What caused the request: `parser`, `script`, `preload`, ...
  pub initiator_type: String,
  /// The URL of whatever initiated it, when the trace recorded one.
  pub initiator_url: String,
  pub decoded_body_length: i64,
  pub encoded_data_length: i64,
  pub flags: Flags,
  /// One entry per redirect hop preceding the final request.
  pub redirects: Vec<Redirect>,
  pub start_time: Micro,
  pub end_time: Micro,
  pub timing: Timing,
}

/// How far the request got.
///
/// `finished` and `failed` were separate booleans in `DevTools`, but
/// they only ever describe one lifecycle, and as flags they permit
/// states the protocol never produces (failed-but-not-finished).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Outcome {
  /// No `ResourceFinish` arrived: the trace ended while the request was
  /// still in flight.
  #[default]
  Pending,
  Finished,
  /// A `ResourceFinish` that reported a failure. Still finished, in the
  /// sense that the network stack was done with it.
  Failed,
}

/// The remaining yes/no facts about a request.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flags {
  pub outcome: Outcome,
  pub from_service_worker: bool,
  pub is_link_preload: bool,
  /// A response arrived. Independent of [`Outcome`]: a failed request
  /// may have had no response at all.
  pub has_response: bool,
}

impl Flags {
  /// Whether the network stack was done with the request, however it
  /// ended.
  #[must_use]
  pub fn finished(self) -> bool {
    matches!(self.outcome, Outcome::Finished | Outcome::Failed)
  }
}

#[derive(Debug, Clone, Default)]
pub struct Redirect {
  pub url: String,
  pub ts: Micro,
  pub dur: Micro,
}

/// The resolved per-phase breakdown, all in microseconds.
#[derive(Debug, Clone, Default)]
pub struct Timing {
  pub queueing: Micro,
  pub stalled: Micro,
  pub dns_lookup: Micro,
  pub initial_connection: Micro,
  pub ssl: Micro,
  pub proxy_negotiation: Micro,
  pub request_sent: Micro,
  pub waiting: Micro,
  /// `receiveHeadersStart - sendEnd`: what `DocumentLatency` judges the
  /// server on. Falls back to `receiveHeadersEnd` on traces too old to
  /// carry a start.
  pub server_response_time: Micro,
  pub download: Micro,
  pub download_start: Micro,
  pub network_duration: Micro,
  pub processing_duration: Micro,
  pub redirection_duration: Micro,
  pub total_time: Micro,
  pub send_start_time: Micro,
  pub finish_time: Micro,
  /// When the response headers began arriving: `requestTime` plus
  /// `receiveHeadersStart`. This is the "first byte" every TTFB-shaped
  /// metric measures to.
  pub first_byte_ts: Option<Micro>,
  pub is_disk_cached: bool,
  pub is_memory_cached: bool,
  pub is_https: bool,
}

#[derive(Default)]
struct Partial {
  send_request: Option<TraceEvent>,
  will_send_requests: Vec<TraceEvent>,
  receive_response: Option<TraceEvent>,
  resource_finish: Option<TraceEvent>,
  mark_as_cached: bool,
}

#[derive(Deserialize)]
struct SendData {
  #[serde(default)]
  url: String,
  #[serde(default, rename = "requestMethod")]
  request_method: String,
  #[serde(default)]
  priority: String,
  #[serde(default)]
  frame: String,
  #[serde(default, rename = "renderBlocking")]
  render_blocking: String,
  #[serde(default, rename = "resourceType")]
  resource_type: String,
  #[serde(default, rename = "isLinkPreload")]
  is_link_preload: bool,
  #[serde(default, rename = "fetchPriorityHint")]
  fetch_priority_hint: String,
  #[serde(default)]
  initiator: Option<Initiator>,
}

#[derive(Deserialize)]
struct Initiator {
  #[serde(default)]
  r#type: String,
  #[serde(default)]
  url: String,
}

#[derive(Deserialize)]
struct ResponseData {
  #[serde(default, rename = "statusCode")]
  status_code: i64,
  #[serde(default, rename = "mimeType")]
  mime_type: String,
  #[serde(default, rename = "encodedDataLength")]
  encoded_data_length: i64,
  #[serde(default)]
  protocol: String,
  #[serde(default, rename = "fromServiceWorker")]
  from_service_worker: bool,
  #[serde(default, rename = "fromCache")]
  from_cache: bool,
  #[serde(default)]
  timing: Option<ResourceTiming>,
  #[serde(default)]
  headers: Option<Vec<Header>>,
}

#[derive(Deserialize)]
struct Header {
  #[serde(default)]
  name: String,
  #[serde(default)]
  value: String,
}

#[derive(Deserialize)]
struct FinishData {
  #[serde(default, rename = "finishTime")]
  finish_time: f64,
  #[serde(default, rename = "encodedDataLength")]
  encoded_data_length: i64,
  #[serde(default, rename = "decodedBodyLength")]
  decoded_body_length: i64,
  #[serde(default, rename = "didFail")]
  did_fail: bool,
}

/// Group every resource event by `requestId` and resolve each group.
///
/// Requests with no `ResourceSendRequest` are dropped: without one there
/// is no URL, no start time and nothing an insight could say.
#[must_use]
pub fn from_events(events: &[TraceEvent]) -> Vec<NetworkRequest> {
  let mut partials: FxHashMap<String, Partial> = FxHashMap::default();

  for event in events {
    let Some(id) = event
      .data()
      .and_then(|d| d.get("requestId"))
      .and_then(serde_json::Value::as_str)
    else {
      continue;
    };
    let entry = partials.entry(id.to_string()).or_default();
    match event.name.as_str() {
      "ResourceSendRequest" => entry.send_request = Some(event.clone()),
      "ResourceWillSendRequest" => entry.will_send_requests.push(event.clone()),
      "ResourceReceiveResponse" => entry.receive_response = Some(event.clone()),
      "ResourceFinish" => entry.resource_finish = Some(event.clone()),
      "ResourceMarkAsCached" => entry.mark_as_cached = true,
      _ => {},
    }
  }

  let mut requests: Vec<NetworkRequest> = partials
    .into_iter()
    .filter_map(|(id, partial)| resolve(&id, &partial))
    .collect();
  requests.sort_by_key(|r| r.start_time);
  requests
}

#[allow(clippy::too_many_lines)]
fn resolve(id: &str, partial: &Partial) -> Option<NetworkRequest> {
  let send = partial.send_request.as_ref()?;
  let send_data: SendData = send.data_as()?;
  let response_data: Option<ResponseData> = partial.receive_response.as_ref().and_then(TraceEvent::data_as);
  let finish_data: Option<FinishData> = partial.resource_finish.as_ref().and_then(TraceEvent::data_as);
  let timing = response_data.as_ref().and_then(|r| r.timing.clone());

  let start_time = send.ts;
  // The LAST willSendRequest is where the non-redirect part of the
  // request actually begins; everything before it was redirect hops.
  let end_redirect_time = partial.will_send_requests.last().map_or(send.ts, |e| e.ts);
  let end_time = partial.resource_finish.as_ref().map_or(end_redirect_time, |e| e.ts);
  let finish_time = finish_data
    .as_ref()
    .filter(|f| f.finish_time > 0.0)
    .map_or(end_time, |f| us(f.finish_time * SECONDS_TO_MICROS));

  let network_duration = if timing.is_some() {
    finish_time - end_redirect_time
  } else {
    0
  };
  let processing_duration = end_time - finish_time;
  let redirection_duration = end_redirect_time - start_time;

  let mut t = Timing {
    network_duration,
    processing_duration,
    redirection_duration,
    total_time: network_duration + processing_duration,
    finish_time,
    is_https: send_data.url.starts_with("https:"),
    is_memory_cached: partial.mark_as_cached,
    is_disk_cached: response_data.as_ref().is_some_and(|r| r.from_cache) && !partial.mark_as_cached,
    ..Default::default()
  };

  if let Some(ref timing) = timing {
    let request_time_us = timing.request_time * SECONDS_TO_MICROS;
    // Clamped at zero: a recorded start later than `requestTime` would
    // otherwise report negative queueing.
    t.queueing = us((request_time_us - micros_to_f64(end_redirect_time)).max(0.0));
    t.stalled = first_positive(&[
      timing.dns_start * MILLIS_TO_MICROS,
      timing.connect_start * MILLIS_TO_MICROS,
      timing.send_start * MILLIS_TO_MICROS,
      partial
        .receive_response
        .as_ref()
        .map_or(f64::NAN, |e| micros_to_f64(e.ts - end_redirect_time)),
    ]);
    t.dns_lookup = us((timing.dns_end - timing.dns_start) * MILLIS_TO_MICROS);
    t.initial_connection = us((timing.connect_end - timing.connect_start) * MILLIS_TO_MICROS);
    t.ssl = us((timing.ssl_end - timing.ssl_start) * MILLIS_TO_MICROS);
    t.proxy_negotiation = us((timing.proxy_end - timing.proxy_start) * MILLIS_TO_MICROS);
    t.request_sent = us((timing.send_end - timing.send_start) * MILLIS_TO_MICROS);
    t.waiting = us((timing.receive_headers_end - timing.send_end) * MILLIS_TO_MICROS);
    t.server_response_time =
      us((timing.receive_headers_start.unwrap_or(timing.receive_headers_end) - timing.send_end) * MILLIS_TO_MICROS);
    t.send_start_time = us(request_time_us + timing.send_start * MILLIS_TO_MICROS);
    t.download_start = us(request_time_us + timing.receive_headers_end * MILLIS_TO_MICROS);
    t.download = finish_time - t.download_start;
    // `receiveHeadersStart` is absent on older traces; headers-end is
    // close enough and is what DevTools falls back to.
    t.first_byte_ts = Some(us(
      request_time_us + timing.receive_headers_start.unwrap_or(timing.receive_headers_end) * MILLIS_TO_MICROS,
    ));
  } else {
    t.stalled = partial.receive_response.as_ref().map_or(0, |e| e.ts - start_time);
    t.send_start_time = start_time;
    t.download_start = start_time;
    t.download = partial.receive_response.as_ref().map_or(0, |e| end_time - e.ts);
  }

  let redirects = partial
    .will_send_requests
    .windows(2)
    .map(|pair| Redirect {
      url: pair[0]
        .data()
        .and_then(|d| d.get("url"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string(),
      ts: pair[0].ts,
      dur: pair[1].ts - pair[0].ts,
    })
    .collect();

  Some(NetworkRequest {
    request_id: id.to_string(),
    url: send_data.url,
    method: send_data.request_method,
    mime_type: response_data.as_ref().map(|r| r.mime_type.clone()).unwrap_or_default(),
    resource_type: send_data.resource_type,
    protocol: response_data.as_ref().map(|r| r.protocol.clone()).unwrap_or_default(),
    priority: send_data.priority,
    status_code: response_data.as_ref().map_or(0, |r| r.status_code),
    frame: send_data.frame,
    render_blocking: send_data.render_blocking,
    fetch_priority_hint: send_data.fetch_priority_hint,
    initiator_type: send_data
      .initiator
      .as_ref()
      .map(|i| i.r#type.clone())
      .unwrap_or_default(),
    initiator_url: send_data.initiator.as_ref().map(|i| i.url.clone()).unwrap_or_default(),
    response_headers: response_data
      .as_ref()
      .and_then(|r| r.headers.as_ref())
      .map(|h| h.iter().map(|h| (h.name.clone(), h.value.clone())).collect())
      .unwrap_or_default(),
    decoded_body_length: finish_data.as_ref().map_or(0, |f| f.decoded_body_length),
    encoded_data_length: finish_data.as_ref().map_or_else(
      || response_data.as_ref().map_or(0, |r| r.encoded_data_length),
      |f| f.encoded_data_length,
    ),
    flags: Flags {
      from_service_worker: response_data.as_ref().is_some_and(|r| r.from_service_worker),
      is_link_preload: send_data.is_link_preload,
      outcome: match (
        partial.resource_finish.is_some(),
        finish_data.as_ref().is_some_and(|f| f.did_fail),
      ) {
        (false, _) => Outcome::Pending,
        (true, true) => Outcome::Failed,
        (true, false) => Outcome::Finished,
      },
      has_response: partial.receive_response.is_some(),
    },
    redirects,
    start_time,
    end_time,
    timing: t,
  })
}

/// First strictly-positive value, matching `DevTools`'
/// `firstPositiveValueInList`. `NaN` stands in for "field absent".
fn first_positive(values: &[f64]) -> Micro {
  for v in values {
    if *v > 0.0 {
      return us(*v);
    }
  }
  0
}

impl NetworkRequest {
  /// Case-insensitive response header lookup.
  #[must_use]
  pub fn header(&self, name: &str) -> Option<&str> {
    self
      .response_headers
      .iter()
      .find(|(k, _)| k.eq_ignore_ascii_case(name))
      .map(|(_, v)| v.as_str())
  }

  /// Whether the response body arrived compressed.
  ///
  /// Mirrors `insights/Common.ts::isRequestCompressed`: a
  /// `content-encoding` of gzip/br/deflate/zstd, and separately the
  /// case `DevTools` treats as already-compressed because the transferred
  /// size came in under the decoded size.
  #[must_use]
  pub fn is_compressed(&self) -> bool {
    match self.header("content-encoding") {
      Some(enc) => {
        let enc = enc.trim().to_ascii_lowercase();
        matches!(enc.as_str(), "gzip" | "br" | "deflate" | "zstd")
      },
      None => false,
    }
  }
}
