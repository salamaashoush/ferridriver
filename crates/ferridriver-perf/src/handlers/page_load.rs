//! Page-load markers and Cumulative Layout Shift.
//!
//! Mirrors devtools-frontend `handlers/PageLoadMetricsHandler.ts` and
//! `handlers/LayoutShiftsHandler.ts`.

use crate::event::{Micro, TraceEvent};
use crate::handlers::meta::Meta;

/// Core Web Vitals and load markers for the main frame, in
/// milliseconds from the time origin.
#[derive(Debug, Clone, Default)]
pub struct PageLoadMetrics {
  pub first_paint: Option<f64>,
  pub first_contentful_paint: Option<f64>,
  pub largest_contentful_paint: Option<f64>,
  pub dom_content_loaded: Option<f64>,
  pub load: Option<f64>,
  /// Cumulative Layout Shift: the worst session window, which is the
  /// score the Web Vitals definition reports.
  pub cumulative_layout_shift: f64,
  /// The individual shifts making up the score, worst first.
  pub layout_shifts: Vec<LayoutShift>,
  /// Size in bytes of the element that produced the LCP, when the trace
  /// recorded a candidate carrying one.
  pub lcp_size: Option<f64>,
  /// Whether the winning LCP candidate was an image.
  pub lcp_is_image: bool,
}

#[derive(Debug, Clone)]
pub struct LayoutShift {
  pub ts: Micro,
  pub score: f64,
}

/// A shift more than 1s after the previous one, or more than 5s after
/// the window opened, starts a new session window. These are the
/// constants the Web Vitals CLS definition fixes.
const SESSION_WINDOW_GAP_US: Micro = 1_000_000;
const SESSION_WINDOW_MAX_US: Micro = 5_000_000;

impl PageLoadMetrics {
  #[must_use]
  pub fn from_events(events: &[TraceEvent], meta: &Meta) -> Self {
    let origin = meta.time_origin();
    let mut metrics = Self::default();
    let to_ms = |ts: Micro| crate::units::micros_to_ms(ts - origin);

    let mut shifts: Vec<LayoutShift> = Vec::new();

    for event in events {
      // Markers for other frames describe an iframe's load, not the
      // page's, and would silently overwrite the main frame's numbers.
      if !meta.main_frame_id.is_empty() && !belongs_to_main_frame(event, &meta.main_frame_id) {
        continue;
      }
      match event.name.as_str() {
        "firstPaint" => metrics.first_paint = Some(to_ms(event.ts)),
        "firstContentfulPaint" => metrics.first_contentful_paint = Some(to_ms(event.ts)),
        // Candidates supersede one another; the last before load wins,
        // which is exactly what taking every candidate in order gives.
        "largestContentfulPaint::Candidate" => {
          metrics.largest_contentful_paint = Some(to_ms(event.ts));
          if let Some(data) = event.data() {
            metrics.lcp_size = data.get("size").and_then(serde_json::Value::as_f64);
            metrics.lcp_is_image = data
              .get("type")
              .and_then(serde_json::Value::as_str)
              .is_some_and(|t| t == "image");
          }
        },
        "MarkDOMContent" => metrics.dom_content_loaded = Some(to_ms(event.ts)),
        "MarkLoad" => metrics.load = Some(to_ms(event.ts)),
        "LayoutShift" => {
          if let Some(data) = event.data() {
            // A shift within 500ms of a user interaction is excluded
            // from CLS by definition: the user asked for it.
            if data
              .get("had_recent_input")
              .and_then(serde_json::Value::as_bool)
              .unwrap_or(false)
            {
              continue;
            }
            if let Some(score) = data.get("weighted_score_delta").and_then(serde_json::Value::as_f64) {
              shifts.push(LayoutShift { ts: event.ts, score });
            }
          }
        },
        _ => {},
      }
    }

    metrics.cumulative_layout_shift = worst_session_window(&shifts);
    shifts.sort_by(|a, b| b.score.total_cmp(&a.score));
    metrics.layout_shifts = shifts;
    metrics
  }
}

/// A marker belongs to the main frame when it says so. Markers that name
/// no frame at all are kept: `firstPaint` on some Chrome versions
/// carries only a `frame` inside `args`, and dropping those would lose
/// the metric entirely.
fn belongs_to_main_frame(event: &TraceEvent, main_frame: &str) -> bool {
  let frame = event
    .data()
    .and_then(|d| d.get("frame"))
    .or_else(|| event.args_get("frame"))
    .and_then(serde_json::Value::as_str);
  match frame {
    Some(f) => f == main_frame,
    None => true,
  }
}

/// CLS is the largest sum over any session window, not the total of
/// every shift. Summing them all is the common wrong answer and
/// over-reports badly on long pages.
fn worst_session_window(shifts: &[LayoutShift]) -> f64 {
  let mut ordered: Vec<&LayoutShift> = shifts.iter().collect();
  ordered.sort_by_key(|s| s.ts);

  let mut worst: f64 = 0.0;
  let mut current: f64 = 0.0;
  let mut window_start: Option<Micro> = None;
  let mut previous: Option<Micro> = None;

  for shift in ordered {
    let starts_new = match (window_start, previous) {
      (Some(start), Some(prev)) => shift.ts - prev > SESSION_WINDOW_GAP_US || shift.ts - start > SESSION_WINDOW_MAX_US,
      _ => true,
    };
    if starts_new {
      worst = worst.max(current);
      current = 0.0;
      window_start = Some(shift.ts);
    }
    current += shift.score;
    previous = Some(shift.ts);
  }
  worst.max(current)
}
