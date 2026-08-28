//! Where the time before the Largest Contentful Paint actually went.
//!
//! Mirrors devtools-frontend `insights/LCPBreakdown.ts`.
//!
//! A text LCP splits into two parts and an image LCP into four, because
//! only an image has a request to wait on:
//!
//! ```text
//! text   | ttfb |                 renderDelay                 |
//! image  | ttfb |  loadDelay  |  loadDuration  |  renderDelay |
//! ```

use crate::event::Micro;
use crate::handlers::network::NetworkRequest;
use crate::handlers::paint::LargestPaint;
use crate::insights::{Check, Insight, Item, Severity};
use crate::units::micros_to_ms;

/// Google's "good" threshold for LCP.
const GOOD_LCP_MS: f64 = 2500.0;

#[must_use]
pub fn run(
  document: Option<&NetworkRequest>,
  requests: &[NetworkRequest],
  paint: &LargestPaint,
  lcp_ts: Option<Micro>,
  navigation_ts: Micro,
) -> Option<Insight> {
  let document = document?;
  let lcp_ts = lcp_ts?;
  // Without the response-header timestamp there is no boundary between
  // ttfb and everything after it, so no breakdown is possible.
  let first_byte_ts = document.timing.first_byte_ts?;

  let mut items = Vec::new();
  let ttfb = micros_to_ms(first_byte_ts - navigation_ts);
  items.push(Item {
    label: "Time to first byte".into(),
    value: ttfb,
    unit: "ms",
  });

  match paint.request.map(|i| &requests[i]) {
    // Image LCP: the request splits the middle into waiting for the
    // fetch to start and the fetch itself.
    Some(lcp_request) => {
      items.push(Item {
        label: "Resource load delay".into(),
        value: micros_to_ms(lcp_request.start_time - first_byte_ts),
        unit: "ms",
      });
      items.push(Item {
        label: "Resource load duration".into(),
        value: micros_to_ms(lcp_request.timing.finish_time - lcp_request.start_time),
        unit: "ms",
      });
      items.push(Item {
        label: "Element render delay".into(),
        value: micros_to_ms(lcp_ts - lcp_request.timing.finish_time),
        unit: "ms",
      });
    },
    None => items.push(Item {
      label: "Element render delay".into(),
      value: micros_to_ms(lcp_ts - first_byte_ts),
      unit: "ms",
    }),
  }

  // A negative subpart means the timestamps disagree, which happens on
  // traces missing some of what this reads. Reporting the arithmetic
  // anyway would be worse than reporting nothing.
  if items.iter().any(|i| i.value < 0.0) {
    return None;
  }

  let total = micros_to_ms(lcp_ts - navigation_ts);
  // A breakdown reports where the time went; it does not pass or fail.
  // Upstream marks it informative for the same reason, and the LCP
  // number itself is already in the metrics.
  let passed = total <= GOOD_LCP_MS;
  Some(Insight {
    key: "LCPBreakdown".into(),
    title: "LCP breakdown".into(),
    description: "Each subpart has specific improvement strategies. Ideally, most of the LCP time should be \
                  spent on loading the resource, not within delays."
      .into(),
    severity: Severity::Informative,
    checks: vec![Check {
      name: "lcpIsGood".into(),
      passed,
      detail: format!(
        "{} LCP at {total:.0} ms (good is under {GOOD_LCP_MS:.0} ms)",
        if paint.is_image() { "Image" } else { "Text" }
      ),
    }],
    metrics: vec![("lcpMs".into(), total)],
    items,
  })
}
