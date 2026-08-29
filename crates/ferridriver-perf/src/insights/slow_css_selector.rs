//! CSS selectors that cost real time to match.
//!
//! Mirrors devtools-frontend `insights/SlowCSSSelector.ts`.
//!
//! Chrome only emits `SelectorStats` when selector profiling is turned
//! on, which is off in a normal trace. Absent the events this reports
//! "not measured" rather than "no slow selectors", because those are
//! very different claims.

use crate::insights::{Check, Insight, Item, Severity};
use crate::units::{count_to_f64, micros_to_ms};

/// One selector's cost, as Blink measured it.
#[derive(Debug, Clone)]
pub struct SelectorTiming {
  pub selector: String,
  /// Microseconds spent matching.
  pub elapsed_us: i64,
  pub match_attempts: i64,
  pub match_count: i64,
}

/// Selectors under this are not worth anyone's attention.
const SLOW_SELECTOR_THRESHOLD_US: i64 = 500;

#[must_use]
pub fn run(timings: &[SelectorTiming]) -> Insight {
  if timings.is_empty() {
    return Insight {
      key: "SlowCSSSelector".into(),
      title: "CSS selector costs".into(),
      description: "Optimize the selectors with both high elapsed time and high slow-path percentage.".into(),
      severity: Severity::Informative,
      checks: vec![Check {
        name: "selectorsAreFast".into(),
        passed: true,
        detail: "Not measured: the trace carries no selector statistics".into(),
      }],
      // Zeros rather than nothing: they are the honest sum over no
      // timings, and a consumer reading the numbers should not have to
      // handle two shapes. What the trace could not tell us is in the
      // detail above, not in a missing key.
      metrics: vec![
        ("totalElapsedMs".into(), 0.0),
        ("totalMatchAttempts".into(), 0.0),
        ("totalMatchCount".into(), 0.0),
      ],
      items: Vec::new(),
    };
  }

  let total_elapsed_us: i64 = timings.iter().map(|t| t.elapsed_us).sum();
  let total_attempts: i64 = timings.iter().map(|t| t.match_attempts).sum();
  let total_matches: i64 = timings.iter().map(|t| t.match_count).sum();

  let mut items: Vec<Item> = timings
    .iter()
    .filter(|t| t.elapsed_us >= SLOW_SELECTOR_THRESHOLD_US)
    .map(|t| Item {
      label: format!(
        "{} ({} attempts, {} matches)",
        t.selector, t.match_attempts, t.match_count
      ),
      value: micros_to_ms(t.elapsed_us),
      unit: "ms",
    })
    .collect();
  items.sort_by(|a, b| b.value.total_cmp(&a.value));

  let passed = items.is_empty();
  Insight {
    key: "SlowCSSSelector".into(),
    title: "CSS selector costs".into(),
    description: "If recalculate style costs remain high, selector optimization can reduce them. Simpler \
                  selectors, fewer selectors, a smaller DOM and a shallower DOM all reduce matching costs."
      .into(),
    // Never a failure. There is no cost at which a selector is wrong,
    // only one at which it is worth looking at, so upstream reports
    // this as information and so does this.
    severity: Severity::Informative,
    checks: vec![Check {
      name: "selectorsAreFast".into(),
      passed,
      detail: if passed {
        format!("No selector cost over {SLOW_SELECTOR_THRESHOLD_US} us")
      } else {
        format!(
          "{} selectors over {SLOW_SELECTOR_THRESHOLD_US} us, {:.1} ms total",
          items.len(),
          micros_to_ms(total_elapsed_us)
        )
      },
    }],
    metrics: vec![
      ("totalElapsedMs".into(), micros_to_ms(total_elapsed_us)),
      ("totalMatchAttempts".into(), count_to_f64(total_attempts)),
      ("totalMatchCount".into(), count_to_f64(total_matches)),
    ],
    items,
  }
}
