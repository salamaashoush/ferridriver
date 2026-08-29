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
  /// `{pid}.{id}` from the load event, which is the request's own id.
  pub request_id: String,
}

/// Blocking time is reported in whole multiples of this, floored rather
/// than rounded: the claim is "the text was hidden at least this long",
/// not "about this long".
///
/// Upstream writes it as `floor(ms, 1/5)`, whose helper INVERTS a
/// precision below one — `1/5` there means multiples of five, not
/// fifths. Reading it as fifths puts every font a few milliseconds out.
const WASTED_GRANULARITY_MS: f64 = 5.0;

#[must_use]
pub fn run(fonts: &[RemoteFont], requests: &[NetworkRequest]) -> Insight {
  let mut items = Vec::new();

  for font in fonts {
    if !BLOCKING_DISPLAYS.contains(&font.display.as_str()) {
      continue;
    }
    let Some(request) = requests
      .iter()
      .find(|r| r.request_id == font.request_id)
      .or_else(|| requests.iter().find(|r| r.url == font.url))
    else {
      continue;
    };
    // From the moment the request went out to the moment the bytes
    // landed: the whole time the text could not be painted.
    let wasted = micros_to_ms(request.timing.finish_time - request.timing.send_start_time);
    let wasted = (wasted / WASTED_GRANULARITY_MS).floor() * WASTED_GRANULARITY_MS;
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
    // Upstream states the saving as the WORST font, not the total: the
    // fonts load in parallel, so the text appears when the slowest one
    // arrives and summing them would count the same wait twice.
    metrics: vec![
      ("wastedMs".into(), items.iter().map(|i| i.value).sum::<f64>()),
      (
        "estimatedSavingsFcpMs".into(),
        items.iter().map(|i| i.value).fold(0.0, f64::max),
      ),
    ],
    items,
  }
}
