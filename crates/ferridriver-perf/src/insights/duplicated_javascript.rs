//! The same JavaScript shipped more than once.
//!
//! Mirrors devtools-frontend `insights/DuplicatedJavaScript.ts` in
//! intent, not in method. Upstream reads each bundle's source map and
//! attributes duplicated MODULES inside different bundles, which needs
//! the map; maps are not in the trace and fetching them would make this
//! crate do network I/O.
//!
//! What is computable from the trace alone is the coarser case: byte
//! identical script bodies served from more than one URL. That catches
//! a library vendored into several bundles, or the same file served
//! from two paths, and never reports a false duplicate. It will miss a
//! module duplicated inside two otherwise-different bundles, which is
//! the case the source map is needed for.

use rustc_hash::FxHashMap;

use crate::handlers::scripts::Script;
use crate::insights::{Check, Insight, Item, Severity};
use crate::units::{count_to_f64, len_to_f64};

/// Duplicates smaller than this are not worth acting on.
const MIN_DUPLICATE_BYTES: usize = 1024;

#[must_use]
pub fn run(scripts: &[Script]) -> Insight {
  // Grouped by content. Hashing the body is enough: two scripts with the
  // same bytes are the same script whatever they are called.
  let mut by_content: FxHashMap<&str, Vec<&Script>> = FxHashMap::default();
  for script in scripts {
    if script.content.len() >= MIN_DUPLICATE_BYTES {
      by_content.entry(script.content.as_str()).or_default().push(script);
    }
  }

  let mut wasted_bytes = 0i64;
  let mut items: Vec<Item> = Vec::new();
  for (content, group) in by_content {
    // Distinct URLs, because one script fetched twice is a caching
    // question, not a duplication one.
    let mut urls: Vec<&str> = group
      .iter()
      .map(|s| if s.url.is_empty() { "(inline)" } else { s.url.as_str() })
      .collect();
    urls.sort_unstable();
    urls.dedup();
    if urls.len() < 2 {
      continue;
    }
    // Every copy after the first is waste.
    let copies = urls.len() - 1;
    let wasted = i64::try_from(content.len() * copies).unwrap_or(i64::MAX);
    wasted_bytes += wasted;
    items.push(Item {
      label: format!(
        "{} bytes shipped {} times: {}",
        content.len(),
        urls.len(),
        urls.join(", ")
      ),
      value: count_to_f64(wasted),
      unit: "bytes",
    });
  }

  items.sort_by(|a, b| b.value.total_cmp(&a.value));
  let passed = items.is_empty();
  Insight {
    key: "DuplicatedJavaScript".into(),
    title: "Duplicated JavaScript".into(),
    description: "Removing large, duplicate JavaScript from bundles can reduce bytes consumed by network \
                  activity."
      .into(),
    severity: if passed { Severity::Pass } else { Severity::Fail },
    checks: vec![Check {
      name: "noDuplicatedJavaScript".into(),
      passed,
      detail: if passed {
        if scripts.is_empty() {
          "Not evaluated: the trace carries no script sources".into()
        } else {
          format!("No identical scripts among {} sources", scripts.len())
        }
      } else {
        format!(
          "{} scripts shipped more than once, wasting {:.0} KB",
          len_to_f64(items.len()),
          count_to_f64(wasted_bytes) / 1024.0
        )
      },
    }],
    metrics: vec![("wastedBytes".into(), count_to_f64(wasted_bytes))],
    items,
  }
}
