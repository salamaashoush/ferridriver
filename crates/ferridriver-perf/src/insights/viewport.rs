//! Whether the page declared a mobile-optimized viewport.
//!
//! Mirrors devtools-frontend `insights/Viewport.ts`. Without
//! `<meta name=viewport content="width=device-width">` a browser
//! assumes a desktop layout and adds up to 300ms to every tap while it
//! waits to see whether the tap was a double-tap zoom.

use crate::insights::{Check, Insight, Severity};

/// What the compositor reported about the frame's viewport.
#[derive(Debug, Clone, Default)]
pub struct ViewportState {
  /// Whether the trace saw a `ParseMetaViewport` at all.
  pub saw_meta_viewport: bool,
  /// `None` when the trace carried no compositor frames to judge from.
  pub mobile_optimized: Option<bool>,
}

#[must_use]
pub fn run(state: &ViewportState) -> Insight {
  // A trace with no committed compositor frame simply does not say, and
  // guessing from the meta tag alone would report a verdict the trace
  // does not support.
  let Some(optimized) = state.mobile_optimized else {
    return Insight {
      key: "Viewport".into(),
      title: "Optimize viewport for mobile".into(),
      description: "Tap interactions may be delayed by up to 300 ms if the viewport isn't optimized for mobile.".into(),
      severity: Severity::Informative,
      checks: vec![Check {
        name: "mobileOptimized".into(),
        passed: true,
        detail: "Not evaluated: the trace recorded no compositor frame".into(),
      }],
      metrics: Vec::new(),
      items: Vec::new(),
    };
  };

  Insight {
    key: "Viewport".into(),
    title: "Optimize viewport for mobile".into(),
    description: "Tap interactions may be delayed by up to 300 ms if the viewport isn't optimized for mobile.".into(),
    severity: if optimized { Severity::Pass } else { Severity::Fail },
    checks: vec![Check {
      name: "mobileOptimized".into(),
      passed: optimized,
      detail: if optimized {
        "Viewport is optimized for mobile".into()
      } else if state.saw_meta_viewport {
        "A meta viewport tag was parsed but the frame is still not mobile optimized".into()
      } else {
        "No mobile-optimized viewport; add <meta name=viewport content=\"width=device-width\">".into()
      },
    }],
    metrics: Vec::new(),
    items: Vec::new(),
  }
}
