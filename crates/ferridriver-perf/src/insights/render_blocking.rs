//! Requests that blocked the first paint.
//!
//! Mirrors `devtools-frontend` `insights/RenderBlocking.ts`. The
//! requests reported are every one the browser marked render-blocking
//! that finished before the first paint; the saving beside them is
//! Lantern's, from re-simulating the paint's subgraph without them.
//! `observedDurationMs` is stated separately because it is measured
//! rather than modelled, and the two answer different questions.

use crate::event::Micro;
use crate::handlers::network::NetworkRequest;
use crate::insights::{Check, Insight, Item, Severity};
use crate::lantern;

/// Below this, upstream declines to put a number on the saving: a
/// request the simulation says cost less than 50ms is inside the noise
/// of the model that produced it. It gates the estimate ONLY. The
/// requests themselves are still reported, and still fail the insight,
/// because the browser blocking on them is an observation rather than a
/// prediction.
const MINIMUM_WASTED_MS: f64 = 50.0;

#[must_use]
pub fn run(
  requests: &[NetworkRequest],
  first_paint_ts: Option<Micro>,
  lantern: Option<&lantern::Context>,
  has_image_lcp: bool,
) -> Option<Insight> {
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
    .collect();
  items.sort_by(|a, b| b.value.total_cmp(&a.value));

  // What the paint would cost without these requests in its dependency
  // graph. `None` when there was no graph to simulate, in which case
  // only the observed duration is reported.
  let blocking_urls: Vec<&str> = blocking.iter().map(|r| r.url.as_str()).collect();
  let estimate = lantern
    .and_then(|context| context.savings_from_removing(&blocking_urls))
    .filter(|e| e.removed_durations_ms.iter().any(|(_, ms)| *ms >= MINIMUM_WASTED_MS));

  let mut metrics: Vec<(String, f64)> = vec![
    ("renderBlockingRequests".into(), crate::units::len_to_f64(items.len())),
    ("observedDurationMs".into(), items.iter().map(|i| i.value).sum::<f64>()),
  ];
  if lantern.is_some() {
    // A zero here is a computed zero, not a missing one: upstream starts
    // both savings at zero and only overwrites them when something
    // cleared the reporting floor, so the absence of a number and a
    // number that came out as nothing stay distinguishable.
    let savings = estimate.as_ref().map_or(0.0, |e| e.savings);
    metrics.push(("estimatedSavingsFcpMs".into(), savings));
    // Deferring a render-blocking request only moves the largest paint
    // when the largest paint is not itself an image: an image LCP waits
    // on its own download, which deferring a stylesheet does not
    // shorten.
    metrics.push((
      "estimatedSavingsLcpMs".into(),
      if has_image_lcp { 0.0 } else { savings },
    ));
  }

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
      } else if let Some(estimate) = &estimate {
        format!(
          "{} render-blocking requests before first paint, worth about {:.0} ms",
          items.len(),
          estimate.savings
        )
      } else {
        format!("{} render-blocking requests before first paint", items.len())
      },
    }],
    metrics,
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
