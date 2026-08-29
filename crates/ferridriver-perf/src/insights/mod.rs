//! Insights: the judgements drawn from the handler output.
//!
//! Each insight is a pure function over the parsed model, so it is
//! testable against a recorded trace without a browser anywhere near it.

pub mod cache;
pub mod character_set;
pub mod cls_culprits;
pub mod document_latency;
pub mod dom_size;
pub mod duplicated_javascript;
pub mod font_display;
pub mod forced_reflow;
pub mod image_delivery;
pub mod inp_breakdown;
pub mod lcp_breakdown;
pub mod lcp_discovery;
pub mod legacy_javascript;
pub mod modern_http;
pub mod network_dependency_tree;
pub mod polyfills;
pub mod render_blocking;
pub mod slow_css_selector;
pub mod third_parties;
pub mod viewport;

use rustc_hash::FxHashMap;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
  Pass,
  /// Worth reporting but not a failure on its own.
  Informative,
  Fail,
}

/// One pass/fail condition inside an insight, mirroring `DevTools`'
/// `Checklist` so the rendered output reads the same.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
  pub name: String,
  pub passed: bool,
  pub detail: String,
}

/// A specific offender an insight found, such as a render-blocking
/// stylesheet or a third-party origin.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
  pub label: String,
  /// Milliseconds, bytes or count, depending on the insight.
  pub value: f64,
  pub unit: &'static str,
}

/// Record what each paint would gain if the named requests transferred
/// that many fewer bytes.
///
/// `metricSavingsForWastedBytes` in devtools-frontend, which four
/// insights share. Nothing is recorded when the page had no paint to
/// simulate: upstream leaves `metricSavings` off the model entirely
/// rather than reporting a zero it did not compute.
pub(crate) fn push_byte_savings(
  metrics: &mut Vec<(String, f64)>,
  lantern: Option<&crate::lantern::Context>,
  wasted_by_url: &FxHashMap<&str, f64>,
) {
  let Some(context) = lantern else { return };
  let savings = context.savings_from_wasted_bytes(wasted_by_url);
  metrics.push(("estimatedSavingsFcpMs".into(), savings.fcp_ms));
  metrics.push(("estimatedSavingsLcpMs".into(), savings.lcp_ms));
}

#[derive(Debug, Clone, Serialize)]
pub struct Insight {
  pub key: String,
  pub title: String,
  pub description: String,
  pub severity: Severity,
  pub checks: Vec<Check>,
  /// Named scalars the insight computed, for machine consumers.
  pub metrics: Vec<(String, f64)>,
  pub items: Vec<Item>,
}
