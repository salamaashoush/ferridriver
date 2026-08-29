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
  /// The outermost script frame that forced it.
  pub function: String,
}

#[derive(Debug, Clone, Default)]
pub struct DomStats {
  pub total_elements: i64,
  pub max_depth: i64,
  pub max_children: i64,
}

impl Renderer {
  #[must_use]
  pub fn from_events(events: &[TraceEvent<'_>]) -> Self {
    let mut renderer = Self::default();
    // Layout and style work is attributed to the renderer's main
    // thread only. A trace carries several processes, and counting a
    // worker's or another frame's layout against this page reports
    // updates the page never did.
    let main = crate::handlers::main_thread(events);

    for event in events {
      if let Some((pid, tid)) = main
        && (event.pid != pid || event.tid != tid)
        && matches!(event.name.as_ref(), "Layout" | "UpdateLayoutTree")
      {
        continue;
      }
      match event.name.as_ref() {
        // These two are more than half the events in a layout-heavy
        // trace, and each needs one integer, so both read through a
        // targeted struct rather than materialising `args`.
        //
        // `dirtyObjects` sits under `args.beginData`, not `args.data`,
        // because Blink splits this event's payload across the begin and
        // end halves it was built from. `elementCount` sits directly on
        // `args`.
        "Layout" => {
          renderer.layouts.push(Update {
            ts: event.ts,
            dur: event.dur.unwrap_or(0),
            size: event
              .args_as::<LayoutArgs>()
              .and_then(|a| a.begin_data)
              .map_or(0, |b| b.dirty_objects),
          });
        },
        "UpdateLayoutTree" => {
          renderer.style_recalcs.push(Update {
            ts: event.ts,
            dur: event.dur.unwrap_or(0),
            size: event.args_as::<StyleRecalcArgs>().map_or(0, |a| a.element_count),
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

/// Reflows that ran inside a script.
///
/// Mirrors `WarningsHandler.processForcedReflowWarning` exactly, and the
/// exactness matters. Events are walked in TRACE ORDER, not sorted:
/// sorting by timestamp interleaves events from different threads and
/// processes, which corrupts the containment stack and made this find
/// nothing at all on a page that really was thrashing layout.
///
/// Two stacks are maintained. `all_events` holds whatever encloses the
/// current event, and returning to depth one means a new top-level task
/// began, which is when the previous task's reflow total is final.
/// `js_invocations` holds only script frames, so a non-empty one means
/// the layout below it was forced rather than scheduled.
fn find_forced_reflows(events: &[TraceEvent<'_>]) -> Vec<ForcedReflow> {
  let threshold = f64_to_micros(FORCED_REFLOW_THRESHOLD_MS * MILLIS_TO_MICROS);
  let mut found = Vec::new();
  let mut all_events: Vec<Micro> = Vec::new();
  let mut js_invocations: Vec<(Micro, String)> = Vec::new();
  let mut task_reflows: Vec<ForcedReflow> = Vec::new();

  for event in events {
    let end = event.end();
    // Anything that finished before this event started no longer
    // encloses it.
    all_events.retain(|open_end| event.ts <= *open_end);
    all_events.push(end);
    js_invocations.retain(|(open_end, _)| event.ts <= *open_end);

    if is_js_invocation(&event.name) {
      js_invocations.push((end, script_frame(event)));
      continue;
    }
    if !js_invocations.is_empty() && REFLOW_EVENTS.contains(&event.name.as_ref()) {
      task_reflows.push(ForcedReflow {
        ts: event.ts,
        dur: event.dur.unwrap_or(0),
        // The INNERMOST frame that names a function. Upstream walks up
        // from the reflow to the first enclosing `FunctionCall`, and
        // that is what a developer would go and change; the outermost
        // frame is usually `EventDispatch`, which names only the event
        // type and attributes every reflow on the page to "click".
        function: js_invocations
          .iter()
          .rev()
          .find(|(_, name)| !name.is_empty())
          .map(|(_, name)| name.clone())
          .unwrap_or_default(),
      });
      continue;
    }
    if all_events.len() == 1 {
      let total: Micro = task_reflows.iter().map(|r| r.dur).sum();
      if total >= threshold {
        found.append(&mut task_reflows);
      }
      task_reflows.clear();
    }
  }
  let total: Micro = task_reflows.iter().map(|r| r.dur).sum();
  if total >= threshold {
    found.append(&mut task_reflows);
  }
  found
}

/// The function a JS invocation event names, for attribution.
fn script_frame(event: &TraceEvent<'_>) -> String {
  let Some(data) = event.data() else {
    return String::new();
  };
  let name = data
    .get("functionName")
    .and_then(serde_json::Value::as_str)
    .unwrap_or_default();
  let url = data.get("url").and_then(serde_json::Value::as_str).unwrap_or_default();
  match (name.is_empty(), url.is_empty()) {
    (true, true) => String::new(),
    (false, true) => name.to_string(),
    (true, false) => url.to_string(),
    (false, false) => format!("{name} ({url})"),
  }
}

fn is_js_invocation(name: &str) -> bool {
  JS_INVOCATION_EVENTS.contains(&name) || name.starts_with("v8") || name.starts_with("V8")
}

#[derive(serde::Deserialize)]
struct LayoutArgs {
  #[serde(default, rename = "beginData")]
  begin_data: Option<LayoutBeginData>,
}

#[derive(serde::Deserialize)]
struct LayoutBeginData {
  #[serde(default, rename = "dirtyObjects")]
  dirty_objects: i64,
}

#[derive(serde::Deserialize)]
struct StyleRecalcArgs {
  #[serde(default, rename = "elementCount")]
  element_count: i64,
}
