//! Web fonts that hide their text while loading.
//!
//! Mirrors devtools-frontend `insights/FontDisplay.ts`.
//!
//! `block`, `fallback` and `auto` all start with an invisible-text
//! period; `swap` and `optional` do not. The time the font spent
//! loading under one of those three is time the user saw nothing.

use crate::handlers::network::NetworkRequest;
use crate::insights::{Check, Insight, Item, Severity};
use crate::units::micros_to_ms;

/// No browser blocks text for longer than this, so a slower font costs
/// no more than three seconds of invisible text.
const MAX_BLOCKING_MS: f64 = 3000.0;

/// The `font-display` values that begin with an invisible-text period.
const BLOCKING_DISPLAYS: [&str; 3] = ["block", "fallback", "auto"];

/// One font the trace saw start loading, with the `display` it declared.
#[derive(Debug, Clone)]
pub struct RemoteFont {
  pub url: String,
  pub display: String,
}

#[must_use]
pub fn run(fonts: &[RemoteFont], requests: &[NetworkRequest]) -> Insight {
  let mut items = Vec::new();

  for font in fonts {
    if !BLOCKING_DISPLAYS.contains(&font.display.as_str()) {
      continue;
    }
    let Some(request) = requests.iter().find(|r| r.url == font.url) else {
      continue;
    };
    // From the moment the request went out to the moment the bytes
    // landed: the whole time the text could not be painted.
    let wasted = micros_to_ms(request.timing.finish_time - request.timing.send_start_time);
    if wasted <= 0.0 {
      continue;
    }
    items.push(Item {
      label: format!("{} ({})", font.url, font.display),
      value: wasted.min(MAX_BLOCKING_MS),
      unit: "ms",
    });
  }

  items.sort_by(|a, b| b.value.total_cmp(&a.value));
  let passed = items.is_empty();
  Insight {
    key: "FontDisplay".into(),
    title: "Font display".into(),
    description: "Consider setting font-display to swap or optional to ensure text is consistently visible.".into(),
    severity: if passed { Severity::Pass } else { Severity::Fail },
    checks: vec![Check {
      name: "usesNonBlockingFontDisplay".into(),
      passed,
      detail: if passed {
        "No fonts with suboptimal font-display found".into()
      } else {
        format!(
          "{} fonts hid text for a total of {:.0} ms",
          items.len(),
          items.iter().map(|i| i.value).sum::<f64>()
        )
      },
    }],
    metrics: vec![("wastedMs".into(), items.iter().map(|i| i.value).sum::<f64>())],
    items,
  }
}
