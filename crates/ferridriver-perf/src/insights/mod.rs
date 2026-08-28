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
