//! Raw Chrome trace events, and the typed views the handlers read.
//!
//! Chrome emits one flat array of objects in the Trace Event Format. Each
//! carries `name`, `cat`, `ph` (phase), `ts`, `pid`/`tid` and a free-form
//! `args`. Everything downstream is derived from that array, so this
//! module keeps the parse cheap and total: an event whose `args` do not
//! match what its name implies is skipped, never a hard error, because a
//! single malformed event must not cost the whole trace.

use serde::Deserialize;

/// Microseconds since an arbitrary trace-local origin.
pub type Micro = i64;

/// One event as Chrome wrote it.
///
/// `args` stays a `Value`: the union across event names is far too wide
/// to model, and each handler deserializes only the shape it needs.
#[derive(Debug, Clone, Deserialize)]
pub struct TraceEvent {
  #[serde(default)]
  pub name: String,
  #[serde(default)]
  pub cat: String,
  #[serde(default)]
  pub ph: String,
  #[serde(default)]
  pub ts: Micro,
  #[serde(default)]
  pub dur: Option<Micro>,
  #[serde(default)]
  pub pid: i64,
  #[serde(default)]
  pub tid: i64,
  #[serde(default)]
  pub args: serde_json::Value,
}

impl TraceEvent {
  /// `args.data`, where nearly every `DevTools` payload lives.
  #[must_use]
  pub fn data(&self) -> Option<&serde_json::Value> {
    self.args.get("data")
  }

  /// Deserialize `args.data` into `T`, or `None` when it does not match.
  #[must_use]
  pub fn data_as<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
    serde_json::from_value(self.data()?.clone()).ok()
  }

  /// End timestamp for a complete (`X`) event; `ts` for anything without
  /// a duration.
  #[must_use]
  pub fn end(&self) -> Micro {
    self.ts + self.dur.unwrap_or(0)
  }
}

/// The `timing` block on `ResourceReceiveResponse`.
///
/// Every field except `requestTime` is milliseconds RELATIVE to
/// `requestTime`, which is itself seconds since the epoch. Mixing those
/// two units up is the classic way to get timings that look plausible
/// and are wrong by orders of magnitude, so the conversions live in one
/// place: [`crate::handlers::network`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTiming {
  #[serde(default)]
  pub request_time: f64,
  #[serde(default)]
  pub proxy_start: f64,
  #[serde(default)]
  pub proxy_end: f64,
  #[serde(default)]
  pub dns_start: f64,
  #[serde(default)]
  pub dns_end: f64,
  #[serde(default)]
  pub connect_start: f64,
  #[serde(default)]
  pub connect_end: f64,
  #[serde(default)]
  pub ssl_start: f64,
  #[serde(default)]
  pub ssl_end: f64,
  #[serde(default)]
  pub send_start: f64,
  #[serde(default)]
  pub send_end: f64,
  /// Absent on older traces, where `receive_headers_end` is the only
  /// header timestamp available.
  #[serde(default)]
  pub receive_headers_start: Option<f64>,
  #[serde(default)]
  pub receive_headers_end: f64,
}

pub const SECONDS_TO_MICROS: f64 = 1_000_000.0;
pub const MILLIS_TO_MICROS: f64 = 1_000.0;

/// Parse the `traceEvents` of a trace, accepting either the bare array
/// Chrome's `Tracing.dataCollected` delivers or the `{traceEvents: [...]}`
/// object a saved `.json` trace file wraps it in.
///
/// # Errors
///
/// When the input is not JSON, or is neither of those two shapes.
pub fn parse(bytes: &[u8]) -> Result<Vec<TraceEvent>, crate::Error> {
  /// Either shape, deserialized straight into the target type.
  ///
  /// Going via `serde_json::Value` first builds a whole intermediate
  /// tree and then clones every event out of it, which measured about
  /// 50x the cost of the analysis that follows.
  #[derive(Deserialize)]
  #[serde(untagged)]
  enum TraceFile {
    Bare(Vec<TraceEvent>),
    Wrapped {
      #[serde(rename = "traceEvents")]
      trace_events: Vec<TraceEvent>,
    },
  }

  if let Ok(TraceFile::Bare(events) | TraceFile::Wrapped { trace_events: events }) =
    serde_json::from_slice::<TraceFile>(bytes)
  {
    return Ok(events);
  }

  // The fast path is all-or-nothing: one event whose field types differ
  // from the declarations above fails the whole document. Chrome's own
  // traces vary between versions, so fall back to per-event decoding,
  // which drops only what it cannot read.
  let value: serde_json::Value = serde_json::from_slice(bytes)?;
  let array = match &value {
    serde_json::Value::Array(a) => a,
    serde_json::Value::Object(o) => o
      .get("traceEvents")
      .and_then(serde_json::Value::as_array)
      .ok_or(crate::Error::NotATrace)?,
    _ => return Err(crate::Error::NotATrace),
  };
  Ok(from_values(array))
}

/// Build the typed event list from already-parsed values, as
/// `Page::stop_tracing` returns them.
///
/// Events that do not deserialize are dropped rather than failing the
/// batch: Chrome mixes in metadata records that carry no `ts`, and a
/// trace is still analysable without them.
#[must_use]
pub fn from_values(values: &[serde_json::Value]) -> Vec<TraceEvent> {
  values
    .iter()
    .filter_map(|v| serde_json::from_value::<TraceEvent>(v.clone()).ok())
    .collect()
}
