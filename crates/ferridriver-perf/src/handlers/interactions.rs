//! User interactions, for Interaction to Next Paint.
//!
//! Mirrors devtools-frontend `handlers/UserInteractionsHandler.ts`.
//!
//! Two things about `EventTiming` make this less direct than it looks.
//! Several events share one `interactionId` (a tap is `pointerdown`,
//! `pointerup` and `click`), and they describe ONE interaction that has
//! to be merged rather than overwritten. And the timing fields are
//! `performance.now()` milliseconds while `ts` is the trace clock in
//! microseconds, so the breakdown is computed entirely within the
//! millisecond fields and never by subtracting one from the other.

use rustc_hash::FxHashMap;

use crate::event::{Micro, TraceEvent};

#[derive(Debug, Clone)]
pub struct Interaction {
  pub kind: String,
  /// Trace-clock start, for ordering against everything else.
  pub ts: Micro,
  /// Total duration in milliseconds, which is what INP reports.
  pub duration_ms: f64,
  /// From the user's action to the handler starting.
  pub input_delay_ms: f64,
  /// The handlers themselves.
  pub processing_ms: f64,
  /// From the handlers finishing to the frame being shown.
  pub presentation_delay_ms: f64,
}

impl Interaction {
  #[must_use]
  pub fn duration_ms(&self) -> f64 {
    self.duration_ms
  }
}

#[derive(Default)]
struct Partial {
  kind: String,
  ts: Option<Micro>,
  /// All in `performance.now()` milliseconds.
  time_stamp: Option<f64>,
  processing_start: Option<f64>,
  processing_end: Option<f64>,
  duration: f64,
}

/// Every interaction the trace caught, longest first.
#[must_use]
pub fn from_events(events: &[TraceEvent]) -> Vec<Interaction> {
  let mut partials: FxHashMap<i64, Partial> = FxHashMap::default();

  for event in events {
    // Only the `b` half carries the payload.
    if event.name != "EventTiming" || event.ph != "b" {
      continue;
    }
    let Some(data) = event.data() else { continue };
    // A zero or absent interactionId means Chrome did not count this as
    // a distinct interaction, so it cannot contribute to INP.
    let Some(id) = data
      .get("interactionId")
      .and_then(serde_json::Value::as_i64)
      .filter(|id| *id != 0)
    else {
      continue;
    };
    let num = |k: &str| data.get(k).and_then(serde_json::Value::as_f64);

    let entry = partials.entry(id).or_default();
    // The interaction begins at the earliest of its events and ends at
    // the latest, and the longest reported duration spans the whole
    // thing.
    entry.ts = Some(entry.ts.map_or(event.ts, |t| t.min(event.ts)));
    if let Some(ts) = num("timeStamp") {
      entry.time_stamp = Some(entry.time_stamp.map_or(ts, |v: f64| v.min(ts)));
    }
    if let Some(start) = num("processingStart") {
      entry.processing_start = Some(entry.processing_start.map_or(start, |v: f64| v.min(start)));
    }
    if let Some(end) = num("processingEnd") {
      entry.processing_end = Some(entry.processing_end.map_or(end, |v: f64| v.max(end)));
    }
    entry.duration = entry.duration.max(num("duration").unwrap_or(0.0));
    // The event type worth naming is the one the user would recognise.
    let kind = data.get("type").and_then(serde_json::Value::as_str).unwrap_or_default();
    if entry.kind.is_empty() || kind == "click" {
      entry.kind = kind.to_string();
    }
  }

  let mut interactions: Vec<Interaction> = partials
    .into_values()
    .filter_map(|p| {
      let time_stamp = p.time_stamp?;
      let processing_start = p.processing_start.unwrap_or(time_stamp);
      let processing_end = p.processing_end.unwrap_or(processing_start);
      Some(Interaction {
        kind: p.kind,
        ts: p.ts?,
        duration_ms: p.duration,
        input_delay_ms: (processing_start - time_stamp).max(0.0),
        processing_ms: (processing_end - processing_start).max(0.0),
        presentation_delay_ms: (time_stamp + p.duration - processing_end).max(0.0),
      })
    })
    .filter(|i| i.duration_ms > 0.0)
    .collect();

  interactions.sort_by(|a, b| b.duration_ms.total_cmp(&a.duration_ms));
  interactions
}
