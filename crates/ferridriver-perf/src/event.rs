//! Raw Chrome trace events, and the typed views the handlers read.
//!
//! Chrome emits one flat array of objects in the Trace Event Format. Each
//! carries `name`, `cat`, `ph` (phase), `ts`, `pid`/`tid` and a free-form
//! `args`. Everything downstream is derived from that array, so this
//! module keeps the parse cheap and total: an event whose `args` do not
//! match what its name implies is skipped, never a hard error, because a
//! single malformed event must not cost the whole trace.

use std::borrow::Cow;
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::value::RawValue;

/// Microseconds since an arbitrary trace-local origin.
pub type Micro = i64;

/// One event as Chrome wrote it.
///
/// `args` is kept as an unparsed slice. The union across event names is
/// far too wide to model, and building a `Value` for every event was
/// measurably three quarters of the cost of loading a trace: 51ms
/// against 14ms on the same 13MB file. Most events are never looked at
/// (a layout-heavy trace is over half `ScheduleStyleRecalculation` and
/// `InvalidateLayout`, which no handler reads), so the parse is deferred
/// to whoever actually wants it. A faster tokenizer is not the lever;
/// simd-json measured at 7%.
#[derive(Debug, Clone, Deserialize)]
pub struct TraceEvent<'a> {
  #[serde(default, borrow)]
  pub name: Cow<'a, str>,
  #[serde(default, borrow)]
  pub cat: Cow<'a, str>,
  #[serde(default, borrow)]
  pub ph: Cow<'a, str>,
  #[serde(default)]
  pub ts: Micro,
  #[serde(default)]
  pub dur: Option<Micro>,
  #[serde(default)]
  pub pid: i64,
  #[serde(default)]
  pub tid: i64,
  /// Unparsed, borrowed from the trace buffer. Reach it through
  /// [`TraceEvent::data`] or [`TraceEvent::args_get`], never directly.
  #[serde(rename = "args", default, borrow)]
  raw_args: Option<&'a RawValue>,
  /// `args` a caller had already parsed, borrowed from their tree.
  ///
  /// Never deserialized into; [`from_values`] sets it. Kept as a second
  /// field rather than an untagged enum with the raw form, because an
  /// untagged enum makes serde buffer the whole document to decide which
  /// variant it is looking at, which is the exact cost this borrows to
  /// avoid.
  #[serde(skip)]
  value_args: Option<&'a serde_json::Value>,
  /// `args` once someone has asked for it. Parsed at most once per
  /// event, and not at all for an event nobody reads.
  #[serde(skip)]
  parsed: OnceLock<Option<serde_json::Value>>,
}

impl TraceEvent<'_> {
  /// `args`, parsed on first use and cached.
  ///
  /// A caller who already had a `Value` pays nothing here at all.
  fn args(&self) -> Option<&serde_json::Value> {
    if let Some(value) = self.value_args {
      return Some(value);
    }
    let raw = self.raw_args?;
    self
      .parsed
      .get_or_init(|| serde_json::from_str(raw.get()).ok())
      .as_ref()
  }

  /// A top-level key of `args`.
  #[must_use]
  pub fn args_get(&self, key: &str) -> Option<&serde_json::Value> {
    self.args()?.get(key)
  }

  /// `args.data`, where nearly every `DevTools` payload lives.
  #[must_use]
  pub fn data(&self) -> Option<&serde_json::Value> {
    self.args_get("data")
  }

  /// Deserialize `args` into `T`.
  ///
  /// Straight from the unparsed slice when there is one, which builds
  /// only the fields `T` declares instead of a whole `Value` tree. An
  /// event built from an already-parsed value has no slice, so that path
  /// deserializes from the cached value instead: both callers get the
  /// same answer, and neither pays to convert into the other's form.
  #[must_use]
  pub fn args_as<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
    match (self.raw_args, self.value_args) {
      (Some(raw), _) => serde_json::from_str(raw.get()).ok(),
      (None, Some(value)) => T::deserialize(value).ok(),
      (None, None) => None,
    }
  }

  /// Deserialize `args.data` into `T`, or `None` when it does not match.
  #[must_use]
  pub fn data_as<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
    #[derive(Deserialize)]
    struct Wrapper<T> {
      data: T,
    }
    self.args_as::<Wrapper<T>>().map(|w| w.data)
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
pub fn parse(bytes: &[u8]) -> Result<Vec<TraceEvent<'_>>, crate::Error> {
  #[derive(Deserialize)]
  struct Wrapped<'a> {
    #[serde(rename = "traceEvents", borrow)]
    trace_events: Vec<TraceEvent<'a>>,
  }

  // The shape is decided by the first non-space byte rather than by
  // `#[serde(untagged)]`. Untagged buffers the whole document into an
  // intermediate that `RawValue` cannot be rebuilt from, so the fast
  // path silently failed and every trace was parsed twice: 129ms where
  // this takes 16ms.
  match bytes.iter().find(|b| !b.is_ascii_whitespace()) {
    Some(b'[') => Ok(serde_json::from_slice::<Vec<TraceEvent<'_>>>(bytes)?),
    Some(b'{') => Ok(serde_json::from_slice::<Wrapped<'_>>(bytes)?.trace_events),
    _ => Err(crate::Error::NotATrace),
  }
}

/// Build the typed event list from already-parsed values, as
/// `Page::stop_tracing` returns them.
///
/// Built field by field rather than through serde. The `args` value is
/// moved straight into the parse cache, because a caller holding a
/// `Value` has already paid for it: round-tripping it back through the
/// raw form to satisfy the deserializer meant re-serialising every
/// event, which measured four times slower than this.
///
/// An event missing `name` or `ts` is kept with defaults rather than
/// dropped; Chrome mixes in metadata records that carry neither, and a
/// trace is still analysable without them.
#[must_use]
pub fn from_values(values: &[serde_json::Value]) -> Vec<TraceEvent<'_>> {
  values
    .iter()
    .map(|value| {
      let string = |key: &str| Cow::Borrowed(value.get(key).and_then(serde_json::Value::as_str).unwrap_or_default());
      let int = |key: &str| value.get(key).and_then(serde_json::Value::as_i64).unwrap_or(0);
      TraceEvent {
        name: string("name"),
        cat: string("cat"),
        ph: string("ph"),
        ts: int("ts"),
        dur: value.get("dur").and_then(serde_json::Value::as_i64),
        pid: int("pid"),
        tid: int("tid"),
        raw_args: None,
        value_args: value.get("args"),
        parsed: OnceLock::new(),
      }
    })
    .collect()
}
