//! Requests that blocked the first paint.
//!
//! Mirrors `devtools-frontend` `insights/RenderBlocking.ts`, minus the
//! Lantern simulation: `DevTools` estimates the saving by re-simulating
//! the network graph without the blocking nodes, and this reports the
//! observed duration instead. The requests identified are the same; the
//! "wasted ms" figure is measured rather than modelled, so it is stated
//! as `observedDurationMs` and not as a predicted saving.

use crate::event::Micro;
use crate::handlers::network::NetworkRequest;
use crate::insights::{Check, Insight, Item, Severity};

/// Stylesheets that finish this fast may be `link[rel=preload]` with an
/// onload handler (the loadCSS pattern) rather than genuinely blocking,
/// and they barely matter either way.
const MINIMUM_WASTED_MS: f64 = 50.0;

#[must_use]
pub fn run(requests: &[NetworkRequest], first_paint_ts: Option<Micro>) -> Option<Insight> {
  let first_paint = first_paint_ts?;

  let blocking: Vec<&NetworkRequest> = requests
    .iter()
    .filter(|r| is_render_blocking(r))
    // A request that finished after the first paint cannot have
    // delayed it.
    .filter(|r| r.timing.finish_time <= first_paint)
    .collect();

  let mut items: Vec<Item> = blocking
    .iter()
    .map(|r| Item {
      label: r.url.clone(),
      value: crate::units::micros_to_ms(r.end_time - r.start_time),
      unit: "ms",
    })
    .filter(|i| i.value >= MINIMUM_WASTED_MS)
    .collect();
  items.sort_by(|a, b| b.value.total_cmp(&a.value));

  let passed = items.is_empty();
  Some(Insight {
    key: "RenderBlocking".into(),
    title: "Render-blocking requests".into(),
    description: "Requests are blocking the page's initial render, which may delay LCP. Deferring or inlining \
                  can move these network requests out of the critical path."
      .into(),
    severity: if passed { Severity::Pass } else { Severity::Fail },
    checks: vec![Check {
      name: "noRenderBlocking".into(),
      passed,
      detail: if passed {
        "No render-blocking requests for this navigation".into()
      } else {
        format!("{} render-blocking requests before first paint", items.len())
      },
    }],
    metrics: vec![
      ("renderBlockingRequests".into(), crate::units::len_to_f64(items.len())),
      ("observedDurationMs".into(), items.iter().map(|i| i.value).sum::<f64>()),
    ],
    items,
  })
}

/// `in_body_parser_blocking` only counts when the request is important
/// enough to actually hold up the parser. Scripts fetched after a
/// non-preloaded image lose High priority without ceasing to block, so
/// a High-priority script counts alongside anything `VeryHigh`.
fn is_render_blocking(request: &NetworkRequest) -> bool {
  match request.render_blocking.as_str() {
    "blocking" | "in_body_parser_blocking" => {},
    _ => return false,
  }
  if request.render_blocking == "in_body_parser_blocking" {
    let is_blocking_script = request.resource_type == "Script" && request.priority == "High";
    if request.priority != "VeryHigh" && !is_blocking_script {
      return false;
    }
  }
  true
}
