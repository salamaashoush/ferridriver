//! Layout and style work made slow by the size of the DOM.
//!
//! Mirrors devtools-frontend `insights/DOMSize.ts`. The thresholds come
//! from that file, chosen to separate updates that ran long from ones
//! that did not.

use crate::event::MILLIS_TO_MICROS;
use crate::handlers::renderer::Renderer;
use crate::insights::{Check, Insight, Item, Severity};
use crate::units::{count_to_f64, f64_to_micros, micros_to_ms};

/// Only updates that actually took this long are worth attributing to
/// DOM size.
const DURATION_THRESHOLD_MS: f64 = 40.0;

/// A layout touching fewer objects than this was not slow because of
/// the DOM.
const LAYOUT_OBJECTS_THRESHOLD: i64 = 100;

/// The same idea for style recalculation, which is cheaper per element.
const STYLE_RECALC_ELEMENTS_THRESHOLD: i64 = 300;

#[must_use]
pub fn run(renderer: &Renderer) -> Insight {
  let threshold_us = f64_to_micros(DURATION_THRESHOLD_MS * MILLIS_TO_MICROS);
  let mut items = Vec::new();

  for layout in &renderer.layouts {
    if layout.dur >= threshold_us && layout.size >= LAYOUT_OBJECTS_THRESHOLD {
      items.push(Item {
        label: format!("Layout ({} objects)", layout.size),
        value: micros_to_ms(layout.dur),
        unit: "ms",
      });
    }
  }
  for recalc in &renderer.style_recalcs {
    if recalc.dur >= threshold_us && recalc.size >= STYLE_RECALC_ELEMENTS_THRESHOLD {
      items.push(Item {
        label: format!("Style recalculation ({} elements)", recalc.size),
        value: micros_to_ms(recalc.dur),
        unit: "ms",
      });
    }
  }
  items.sort_by(|a, b| b.value.total_cmp(&a.value));

  let mut metrics = Vec::new();
  if let Some(dom) = &renderer.max_dom {
    metrics.push(("totalElements".into(), count_to_f64(dom.total_elements)));
    metrics.push(("maxDepth".into(), count_to_f64(dom.max_depth)));
    metrics.push(("maxChildren".into(), count_to_f64(dom.max_children)));
  }

  let passed = items.is_empty();
  Insight {
    key: "DOMSize".into(),
    title: "Optimize DOM size".into(),
    description: "A large DOM can increase the duration of style calculations and layout reflows, impacting \
                  responsiveness."
      .into(),
    severity: if passed { Severity::Pass } else { Severity::Fail },
    checks: vec![Check {
      name: "domIsNotTooLarge".into(),
      passed,
      detail: if passed {
        match &renderer.max_dom {
          Some(dom) => format!("No large layout or style updates ({} elements)", dom.total_elements),
          None => "No large layout or style updates".into(),
        }
      } else {
        format!(
          "{} layout or style updates over {DURATION_THRESHOLD_MS:.0} ms",
          items.len()
        )
      },
    }],
    metrics,
    items,
  }
}
