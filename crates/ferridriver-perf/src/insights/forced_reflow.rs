//! Layout the page forced synchronously, mid-frame.
//!
//! Mirrors devtools-frontend `insights/ForcedReflow.ts`. Reading a
//! geometry property (`offsetHeight`, `getBoundingClientRect`) after
//! writing to the DOM makes the browser lay out immediately rather than
//! at the end of the frame. Doing it in a loop is the classic layout
//! thrash.

use rustc_hash::FxHashMap;

use crate::handlers::renderer::Renderer;
use crate::insights::{Check, Insight, Item, Severity};
use crate::units::micros_to_ms;

#[must_use]
pub fn run(renderer: &Renderer) -> Insight {
  // Grouped by reflow step: thirty layouts forced in one loop is one
  // thing to fix, not thirty findings.
  let mut by_function: FxHashMap<&str, (f64, usize)> = FxHashMap::default();
  for reflow in &renderer.forced_reflows {
    let key = reflow.kind.as_str();
    let entry = by_function.entry(key).or_insert((0.0, 0));
    entry.0 += micros_to_ms(reflow.dur);
    entry.1 += 1;
  }

  let mut items: Vec<Item> = by_function
    .into_iter()
    .map(|(kind, (total_ms, count))| Item {
      label: format!("{kind} x{count}"),
      value: total_ms,
      unit: "ms",
    })
    .collect();
  items.sort_by(|a, b| b.value.total_cmp(&a.value));

  let total: f64 = items.iter().map(|i| i.value).sum();
  let passed = renderer.forced_reflows.is_empty();
  Insight {
    key: "ForcedReflow".into(),
    title: "Forced reflow".into(),
    description: "A forced reflow occurs when JavaScript queries geometric properties after the document has \
                  been written to. This forces the browser to lay out early and can be costly in a loop."
      .into(),
    severity: if passed { Severity::Pass } else { Severity::Fail },
    checks: vec![Check {
      name: "noForcedReflow".into(),
      passed,
      detail: if passed {
        "No forced reflows".into()
      } else {
        format!("{} forced reflows costing {total:.0} ms", renderer.forced_reflows.len())
      },
    }],
    metrics: vec![("totalReflowMs".into(), total)],
    items,
  }
}
