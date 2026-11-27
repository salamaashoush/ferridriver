//! The Lighthouse audits that read the page as it stands.
//!
//! Lighthouse's non-performance audits fall into three groups. Sixty-
//! seven are axe-core wrappers and are covered by running the engine
//! instead (`accessibility`). The performance ones wrap the
//! `DevTools` trace insights, which `ferridriver-perf` has ported. What is
//! left needs artifacts gathered from a live DOM, and ten of those are
//! here.
//!
//! Each is a pure function over the artifact struct below, ported from
//! Lighthouse's own source and checked against it: `just lh-audit <url>`
//! prints what Lighthouse concludes about a page, and
//! `cargo test -p ferridriver-perf --test lighthouse` compares these
//! verdicts against recorded ones for the fixture pages.
//!
//! Eight of the ten read nothing but the DOM. [`http_status_code`] and
//! [`is_crawlable`] also need the main document's own response, and
//! `is-crawlable` needs `/robots.txt` as well, so both return `None`
//! when [`Artifacts::main_document`] was not filled in --
//! `check_page_quality` leaves the audit out of the report rather than
//! guessing, which is what Lighthouse's `notApplicable` amounts to.
//!
//! Still missing from the group: `is-on-https`, `csp-xss` and
//! `has-hsts`. The last two score `informative` in Lighthouse, so there
//! is no verdict for the recorded gate to compare against.
//!
//! # The one place this cannot see what Lighthouse sees
//!
//! `crawlable-anchors` asks whether an anchor carrying no `href` and no
//! href-associated attribute has an event listener; Lighthouse answers
//! with CDP's `DOMDebugger.getEventListeners`. Nothing page-side can
//! enumerate listeners, and no `BiDi` command exposes them at all, so
//! [`Anchor::has_inline_handler`] answers the part that IS visible: an
//! `onclick` attribute. Chrome reports inline handlers as listeners too,
//! so the two agree on `<a onclick="...">`; they part company on an
//! anchor whose only handler was registered with `addEventListener`,
//! which Lighthouse fails and this passes. Stated rather than papered
//! over, and the same on all four backends.

use serde::{Deserialize, Serialize};

pub mod robots;

mod artifacts;
mod best_practices;
mod seo;
#[cfg(test)]
mod tests;

pub use artifacts::{
  Anchor, Artifacts, Dimensions, Doctype, Image, Input, Link, MainDocument, Meta, Rect, RobotsTxt, Viewport,
  gather_source, parse_artifacts,
};
pub use best_practices::{doctype, image_aspect_ratio, image_size_responsive, paste_preventing_inputs};
pub use seo::{
  canonical, crawlable_anchors, http_status_code, is_crawlable, link_text, meta_description, robots_txt_url,
};

/// Which audits to run. Empty runs all ten.
#[derive(Debug, Clone, Default)]
pub struct PageQualityOptions {
  /// Audit ids to run, e.g. `meta-description`. Empty runs every one.
  pub only: Vec<String>,
}

impl PageQualityOptions {
  /// Whether `id` is in the run. Also decides what
  /// `check_page_quality` has to collect beyond the DOM: narrowing to
  /// audits that need neither the main document nor `/robots.txt`
  /// leaves both uncollected.
  #[must_use]
  pub fn wants(&self, id: &str) -> bool {
    self.only.is_empty() || self.only.iter().any(|want| want == id)
  }
}

/// Which Lighthouse category an audit belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditCategory {
  Seo,
  BestPractices,
}

/// One element an audit objected to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditItem {
  /// A CSS selector locating the element.
  pub selector: String,
  /// The element's opening tag, truncated.
  pub snippet: String,
  /// Whatever the audit has to say about this element: the offending
  /// link text, the two aspect ratios, the size it should have been.
  #[serde(default)]
  pub detail: String,
}

/// One audit's verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditResult {
  /// Lighthouse's own audit id, e.g. `image-aspect-ratio`.
  pub id: String,
  pub category: AuditCategory,
  /// Lighthouse's title for the state this audit is in.
  pub title: String,
  pub passed: bool,
  /// Why it failed, where the audit has more to say than its title.
  #[serde(default)]
  pub explanation: Option<String>,
  /// The elements behind a failure, which is the part anyone acts on.
  #[serde(default)]
  pub items: Vec<AuditItem>,
}

/// What the seven audits concluded.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageQualityReport {
  pub audits: Vec<AuditResult>,
}

impl PageQualityReport {
  /// The audits that failed, which is usually all anyone wants.
  #[must_use]
  pub fn failures(&self) -> Vec<&AuditResult> {
    self.audits.iter().filter(|audit| !audit.passed).collect()
  }
}

// ── The audits ──────────────────────────────────────────────────────────

/// Every audit id this module implements, in the order they are
/// reported. The last two come last because they are the two that need
/// more than the DOM, and so the two a report can be missing.
pub const AUDIT_IDS: [&str; 10] = [
  "doctype",
  "meta-description",
  "canonical",
  "crawlable-anchors",
  "link-text",
  "image-aspect-ratio",
  "image-size-responsive",
  "paste-preventing-inputs",
  "http-status-code",
  "is-crawlable",
];

/// Run the audits over gathered artifacts.
#[must_use]
pub fn run(artifacts: &Artifacts, options: &PageQualityOptions) -> PageQualityReport {
  let mut audits = Vec::new();
  if options.wants("doctype") {
    audits.push(doctype(artifacts));
  }
  if options.wants("meta-description") {
    audits.push(meta_description(artifacts));
  }
  if options.wants("canonical") {
    audits.push(canonical(artifacts));
  }
  if options.wants("crawlable-anchors") {
    audits.push(crawlable_anchors(artifacts));
  }
  if options.wants("link-text") {
    audits.push(link_text(artifacts));
  }
  if options.wants("image-aspect-ratio") {
    audits.push(image_aspect_ratio(artifacts));
  }
  if options.wants("image-size-responsive") {
    audits.push(image_size_responsive(artifacts));
  }
  if options.wants("paste-preventing-inputs") {
    audits.push(paste_preventing_inputs(artifacts));
  }
  if options.wants("http-status-code") {
    audits.extend(http_status_code(artifacts));
  }
  if options.wants("is-crawlable") {
    audits.extend(is_crawlable(artifacts));
  }
  PageQualityReport { audits }
}

pub(crate) fn passed(id: &str, category: AuditCategory, title: &str) -> AuditResult {
  AuditResult {
    id: id.into(),
    category,
    title: title.into(),
    passed: true,
    explanation: None,
    items: Vec::new(),
  }
}

pub(crate) fn failed(
  id: &str,
  category: AuditCategory,
  title: &str,
  explanation: Option<&str>,
  items: Vec<AuditItem>,
) -> AuditResult {
  AuditResult {
    id: id.into(),
    category,
    title: title.into(),
    passed: false,
    explanation: explanation.map(str::to_string),
    items,
  }
}
