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

const DESCRIPTION: &str = "Start investigating with the longest subpart. Delays can be minimized. To reduce \
                           processing duration, optimize the main-thread costs, often JS.";

#[must_use]
pub fn run(interactions: &[Interaction]) -> Insight {
  // Sorted longest-first by the handler, so the worst is the first. A
  // page nobody interacted with has no INP, which is a pass rather than
  // an absent insight: "no slow interaction" is a real answer.
  let Some(worst) = interactions.first() else {
    return Insight {
      key: "INPBreakdown".into(),
      title: "INP breakdown".into(),
      description: DESCRIPTION.into(),
      severity: Severity::Pass,
      checks: vec![Check {
        name: "inpIsGood".into(),
        passed: true,
        detail: "No interactions were recorded".into(),
      }],
      metrics: Vec::new(),
      items: Vec::new(),
    };
  };

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
  // Like the LCP breakdown, this reports where the time went rather than
  // rendering a verdict, so it stays informative whenever there is an
  // interaction to break down.
  Insight {
    key: "INPBreakdown".into(),
    title: "INP breakdown".into(),
    description: DESCRIPTION.into(),
    severity: Severity::Informative,
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
  }
}
