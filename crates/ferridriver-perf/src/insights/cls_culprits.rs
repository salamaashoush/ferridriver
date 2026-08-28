//! What made the page move.
//!
//! Mirrors devtools-frontend `insights/CLSCulprits.ts`. A cause counts
//! only if it finished within half a second BEFORE the shift, which is
//! the window upstream uses to avoid attributing a shift to unrelated
//! work elsewhere in the load.
//!
//! Upstream attributes four causes. Three are covered here: images with
//! no declared size, web fonts, and iframes injected without reserved
//! space. The fourth, non-composited animations, needs the paired
//! animation events and the compositor's failure-reason bitmask, and is
//! not attempted; a page whose shifts come only from animations will
//! report the shifts with no cause named rather than a wrong one.

use crate::event::{Micro, SECONDS_TO_MICROS};
use crate::handlers::page_load::LayoutShift;
use crate::insights::{Check, Insight, Item, Severity};
use crate::units::f64_to_micros;

/// How long before a shift a cause may have finished and still be
/// blamed for it.
const ROOT_CAUSE_WINDOW_SECONDS: f64 = 0.5;

/// Something the trace saw that can move layout.
#[derive(Debug, Clone)]
pub struct Culprit {
  pub kind: CulpritKind,
  /// When it finished, which is when it could have moved anything.
  pub end_ts: Micro,
  pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CulpritKind {
  /// An image laid out before its dimensions were known.
  UnsizedImage,
  /// A font swap re-flowing the text around it.
  WebFont,
  /// A frame injected into an already-laid-out document.
  InjectedIframe,
}

impl CulpritKind {
  fn label(self) -> &'static str {
    match self {
      Self::UnsizedImage => "Unsized image",
      Self::WebFont => "Web font",
      Self::InjectedIframe => "Injected iframe",
    }
  }
}

#[must_use]
pub fn run(shifts: &[LayoutShift], culprits: &[Culprit], cls: f64) -> Insight {
  let window = f64_to_micros(ROOT_CAUSE_WINDOW_SECONDS * SECONDS_TO_MICROS);

  let mut items: Vec<Item> = Vec::new();
  for shift in shifts {
    let blamed: Vec<&Culprit> = culprits
      .iter()
      .filter(|c| c.end_ts < shift.ts && c.end_ts >= shift.ts - window)
      .collect();
    if blamed.is_empty() {
      continue;
    }
    for culprit in blamed {
      items.push(Item {
        label: format!("{}: {}", culprit.kind.label(), culprit.detail),
        value: shift.score,
        unit: "shift score",
      });
    }
  }
  items.sort_by(|a, b| b.value.total_cmp(&a.value));

  // No shifts is a pass. Shifts with nothing to blame is still a
  // failure, because the page moved.
  let passed = shifts.is_empty();
  Insight {
    key: "CLSCulprits".into(),
    title: "Layout shift culprits".into(),
    description: "Layout shifts occur when elements move absent any user interaction. Investigate the causes, \
                  such as elements being added, removed, or their fonts changing as the page loads."
      .into(),
    severity: if passed { Severity::Pass } else { Severity::Fail },
    checks: vec![Check {
      name: "noLayoutShifts".into(),
      passed,
      detail: if passed {
        "No layout shifts".into()
      } else if items.is_empty() {
        format!(
          "{} layout shifts totalling {cls:.4}, no cause identified in the trace",
          shifts.len()
        )
      } else {
        format!("{} layout shifts totalling {cls:.4}", shifts.len())
      },
    }],
    metrics: vec![("cumulativeLayoutShift".into(), cls)],
    items,
  }
}
