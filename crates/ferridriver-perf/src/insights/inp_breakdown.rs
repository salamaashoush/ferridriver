//! Where the time in the page's worst interaction went.
//!
//! Mirrors devtools-frontend `insights/INPBreakdown.ts`. The longest
//! interaction is the one INP reports, and it splits into the wait
//! before the handler ran, the handler itself, and the wait for the
//! next frame.

use crate::handlers::interactions::Interaction;
use crate::insights::{Check, Insight, Item, Severity};

/// Google's "good" threshold for INP.
const GOOD_INP_MS: f64 = 200.0;

#[must_use]
pub fn run(interactions: &[Interaction]) -> Option<Insight> {
  // Sorted longest-first by the handler, so the worst is the first.
  let worst = interactions.first()?;

  let items = vec![
    Item {
      label: "Input delay".into(),
      value: worst.input_delay_ms,
      unit: "ms",
    },
    Item {
      label: "Processing duration".into(),
      value: worst.processing_ms,
      unit: "ms",
    },
    Item {
      label: "Presentation delay".into(),
      value: worst.presentation_delay_ms,
      unit: "ms",
    },
  ];

  let total = worst.duration_ms();
  let passed = total <= GOOD_INP_MS;
  Some(Insight {
    key: "INPBreakdown".into(),
    title: "INP breakdown".into(),
    description: "Start investigating with the longest subpart. Delays can be minimized. To reduce processing \
                  duration, optimize the main-thread costs, often JS."
      .into(),
    severity: if passed { Severity::Pass } else { Severity::Fail },
    checks: vec![Check {
      name: "inpIsGood".into(),
      passed,
      detail: format!(
        "Longest interaction ({}) took {total:.0} ms (good is under {GOOD_INP_MS:.0} ms)",
        if worst.kind.is_empty() { "unknown" } else { &worst.kind }
      ),
    }],
    metrics: vec![("inpMs".into(), total)],
    items,
  })
}
