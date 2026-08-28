//! Whether the browser could find the LCP image early enough.
//!
//! Mirrors devtools-frontend `insights/LCPDiscovery.ts`. Only applies to
//! an image LCP; a text LCP has nothing to discover.

use crate::handlers::network::NetworkRequest;
use crate::handlers::paint::LargestPaint;
use crate::insights::{Check, Insight, Severity};

#[must_use]
pub fn run(document: Option<&NetworkRequest>, requests: &[NetworkRequest], paint: &LargestPaint) -> Option<Insight> {
  let lcp_request = requests.get(paint.request?)?;
  let document = document?;

  // Discoverable means the preload scanner could reach it from the HTML
  // itself: either the parser found it in the main document, or a
  // `<link rel=preload>` announced it. Anything a script injected later
  // is by definition found late.
  let initiated_by_main_doc = lcp_request.initiator_type == "parser" && lcp_request.initiator_url == document.url;
  let discoverable = lcp_request.flags.is_link_preload || initiated_by_main_doc;

  // `auto` and `low` both leave the image behind other work; only an
  // explicit `high` moves it up.
  let priority_hinted = lcp_request.fetch_priority_hint == "high";

  // A preloaded image is fetched regardless of the attribute, so lazy
  // loading only actually hurts when there is no preload.
  let eagerly_loaded = paint.loading_attr != "lazy" || lcp_request.flags.is_link_preload;

  let checks = vec![
    Check {
      name: "priorityHinted".into(),
      passed: priority_hinted,
      detail: if priority_hinted {
        "fetchpriority=high applied".into()
      } else {
        format!(
          "fetchpriority=high should be applied (currently {})",
          if lcp_request.fetch_priority_hint.is_empty() {
            "unset"
          } else {
            &lcp_request.fetch_priority_hint
          }
        )
      },
    },
    Check {
      name: "requestDiscoverable".into(),
      passed: discoverable,
      detail: if discoverable {
        "Request is discoverable in initial document".into()
      } else {
        "Request is not discoverable in the initial document; preload it".into()
      },
    },
    Check {
      name: "eagerlyLoaded".into(),
      passed: eagerly_loaded,
      detail: if eagerly_loaded {
        "lazy load not applied".into()
      } else {
        "lazy loading delays the LCP image; remove loading=lazy".into()
      },
    },
  ];

  let failed = checks.iter().any(|c| !c.passed);
  Some(Insight {
    key: "LCPDiscovery".into(),
    title: "LCP request discovery".into(),
    description: "Optimize LCP by making the LCP image discoverable from the HTML immediately, and avoiding \
                  lazy-loading."
      .into(),
    severity: if failed { Severity::Fail } else { Severity::Pass },
    checks,
    metrics: Vec::new(),
    items: Vec::new(),
  })
}
