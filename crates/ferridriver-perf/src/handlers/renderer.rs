//! Main-thread work: layout, style recalculation and forced reflow.
//!
//! Draws on devtools-frontend `handlers/RendererHandler.ts`,
//! `handlers/DOMStatsHandler.ts` and `handlers/WarningsHandler.ts`.
//!
//! Chrome emits no "forced reflow" event. A reflow is forced when a
//! `Layout` or `UpdateLayoutTree` runs *nested inside* a JS invocation,
//! which is only visible by reconstructing the nesting from timestamps.

use crate::event::{MILLIS_TO_MICROS, Micro, TraceEvent};
use crate::units::f64_to_micros;

/// Reflow inside one task under this total is not worth reporting.
/// From `WarningsHandler.FORCED_REFLOW_THRESHOLD`.
const FORCED_REFLOW_THRESHOLD_MS: f64 = 30.0;

/// Events that mean "script is running". A layout nested inside one of
/// these was forced by that script rather than scheduled by the browser.
/// From `Types.Events.isJSInvocationEvent`.
const JS_INVOCATION_EVENTS: [&str; 6] = [
  "RunMicrotasks",
  "FunctionCall",
  "EvaluateScript",
  "v8.evaluateModule",
  "EventDispatch",
  "V8.Execute",
];

/// Chrome's two reflow steps.
const REFLOW_EVENTS: [&str; 2] = ["Layout", "UpdateLayoutTree"];

#[derive(Debug, Clone, Default)]
pub struct Renderer {
  pub layouts: Vec<Update>,
  pub style_recalcs: Vec<Update>,
  pub forced_reflows: Vec<ForcedReflow>,
  pub max_dom: Option<DomStats>,
}

#[derive(Debug, Clone)]
pub struct Update {
  pub ts: Micro,
  pub dur: Micro,
  /// Layout objects, or elements for a style recalculation.
  pub size: i64,
}

#[derive(Debug, Clone)]
pub struct ForcedReflow {
  pub ts: Micro,
  pub dur: Micro,
  /// The reflow step that was forced.
  pub kind: String,
}

#[derive(Debug, Clone, Default)]
pub struct DomStats {
  pub total_elements: i64,
  pub max_depth: i64,
  pub max_children: i64,
}

impl Renderer {
  #[must_use]
  pub fn from_events(events: &[TraceEvent]) -> Self {
    let mut renderer = Self::default();

    for event in events {
      match event.name.as_str() {
        // `dirtyObjects` sits under `args.beginData`, not `args.data`,
        // because Blink splits this event's payload across the begin and
        // end halves it was built from.
        "Layout" => {
          renderer.layouts.push(Update {
            ts: event.ts,
            dur: event.dur.unwrap_or(0),
            size: nested_i64(event, &["beginData", "dirtyObjects"]).unwrap_or(0),
          });
        },
        // `elementCount` sits directly on `args`.
        "UpdateLayoutTree" => {
          renderer.style_recalcs.push(Update {
            ts: event.ts,
            dur: event.dur.unwrap_or(0),
            size: event
              .args
              .get("elementCount")
              .and_then(serde_json::Value::as_i64)
              .unwrap_or(0),
          });
        },
        "DOMStats" => {
          if let Some(data) = event.data() {
            let get = |k: &str| data.get(k).and_then(serde_json::Value::as_i64).unwrap_or(0);
            let stats = DomStats {
              total_elements: get("totalElements"),
              max_depth: get("maxDepth"),
              max_children: get("maxChildren"),
            };
            // The DOM grows through the load, so the biggest sample is
            // the one worth reporting.
            if renderer
              .max_dom
              .as_ref()
              .is_none_or(|m| stats.total_elements > m.total_elements)
            {
              renderer.max_dom = Some(stats);
            }
          }
        },
        _ => {},
      }
    }

    renderer.forced_reflows = find_forced_reflows(events);
    renderer
  }
}

/// Reflows that ran inside a script, grouped by the task that paid for
/// them and kept only when that task's total crosses the threshold.
///
/// Nesting is reconstructed from timestamps: the trace is a flat list,
/// but a complete (`X`) event contains every event that starts before it
/// ends. Sorting by start, then by descending duration, puts a parent
/// ahead of its children.
fn find_forced_reflows(events: &[TraceEvent]) -> Vec<ForcedReflow> {
  let mut ordered: Vec<&TraceEvent> = events.iter().filter(|e| e.ph == "X" || e.dur.is_some()).collect();
  ordered.sort_by_key(|e| (e.ts, std::cmp::Reverse(e.dur.unwrap_or(0))));

  let threshold = f64_to_micros(FORCED_REFLOW_THRESHOLD_MS * MILLIS_TO_MICROS);
  let mut found = Vec::new();
  // Ends of the JS invocations currently open around us.
  let mut js_stack: Vec<Micro> = Vec::new();
  let mut task_end: Option<Micro> = None;
  let mut task_reflows: Vec<ForcedReflow> = Vec::new();

  let flush = |task_reflows: &mut Vec<ForcedReflow>, found: &mut Vec<ForcedReflow>| {
    let total: Micro = task_reflows.iter().map(|r| r.dur).sum();
    if total >= threshold {
      found.append(task_reflows);
    }
    task_reflows.clear();
  };

  for event in ordered {
    // A task ends when an event starts after it; that is the point at
    // which its reflow total is final.
    if task_end.is_some_and(|end| event.ts > end) {
      flush(&mut task_reflows, &mut found);
      task_end = None;
    }
    js_stack.retain(|end| event.ts <= *end);

    if event.name == "RunTask" {
      task_end = Some(event.end());
    }
    if is_js_invocation(&event.name) {
      js_stack.push(event.end());
      continue;
    }
    if !js_stack.is_empty() && REFLOW_EVENTS.contains(&event.name.as_str()) {
      task_reflows.push(ForcedReflow {
        ts: event.ts,
        dur: event.dur.unwrap_or(0),
        kind: event.name.clone(),
      });
    }
  }
  flush(&mut task_reflows, &mut found);
  found
}

fn is_js_invocation(name: &str) -> bool {
  JS_INVOCATION_EVENTS.contains(&name) || name.starts_with("v8") || name.starts_with("V8")
}

fn nested_i64(event: &TraceEvent, path: &[&str]) -> Option<i64> {
  let mut current = &event.args;
  for key in path {
    current = current.get(key)?;
  }
  current.as_i64()
}
