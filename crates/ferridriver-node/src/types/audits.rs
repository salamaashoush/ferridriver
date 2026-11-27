//! Accessibility, page-tool, and page-quality audit NAPI object types.

use napi_derive::napi;

/// Options for `page.checkAccessibility`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct AccessibilityAuditOptions {
  /// CSS selectors to audit. Omitted or empty audits the whole document.
  pub include: Option<Vec<String>>,
  /// Subtrees to leave out, for regions the page does not own.
  pub exclude: Option<Vec<String>>,
  /// Rule tags to run, such as `wcag2a` or `best-practice`. Omitted or
  /// empty runs every rule axe-core has.
  pub tags: Option<Vec<String>>,
}

impl From<AccessibilityAuditOptions> for ferridriver::accessibility::AccessibilityOptions {
  fn from(options: AccessibilityAuditOptions) -> Self {
    Self {
      include: options.include.unwrap_or_default(),
      exclude: options.exclude.unwrap_or_default(),
      tags: options.tags.unwrap_or_default(),
    }
  }
}

/// One element a rule had something to say about.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct AccessibilityNode {
  /// CSS selectors locating the element, outermost frame first.
  pub target: Vec<String>,
  pub html: String,
  /// Why the rule fired here, in axe-core's words.
  pub failure_summary: String,
}

/// One rule's verdict, with the elements behind it.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct AccessibilityRule {
  pub id: String,
  /// `minor`, `moderate`, `serious` or `critical`.
  pub impact: Option<String>,
  pub help: String,
  pub help_url: String,
  /// Which standards the rule belongs to (`wcag2a`, `best-practice`).
  pub tags: Vec<String>,
  pub nodes: Vec<AccessibilityNode>,
}

/// What axe-core found. Four outcomes, not two: `incomplete` is a
/// question axe could not settle without a human, which is not a pass.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct AccessibilityReport {
  pub violations: Vec<AccessibilityRule>,
  pub passes: Vec<AccessibilityRule>,
  pub incomplete: Vec<AccessibilityRule>,
  pub inapplicable: Vec<AccessibilityRule>,
  /// The engine that produced this, e.g. `axe-core/4.12.1`.
  pub engine: String,
}

impl From<ferridriver::accessibility::AccessibilityNode> for AccessibilityNode {
  fn from(node: ferridriver::accessibility::AccessibilityNode) -> Self {
    Self {
      target: node.target,
      html: node.html,
      failure_summary: node.failure_summary,
    }
  }
}

impl From<ferridriver::accessibility::AccessibilityRule> for AccessibilityRule {
  fn from(rule: ferridriver::accessibility::AccessibilityRule) -> Self {
    Self {
      id: rule.id,
      impact: rule.impact,
      help: rule.help,
      help_url: rule.help_url,
      tags: rule.tags,
      nodes: rule.nodes.into_iter().map(Into::into).collect(),
    }
  }
}

impl From<ferridriver::accessibility::AccessibilityReport> for AccessibilityReport {
  fn from(report: ferridriver::accessibility::AccessibilityReport) -> Self {
    let rules = |list: Vec<ferridriver::accessibility::AccessibilityRule>| -> Vec<AccessibilityRule> {
      list.into_iter().map(Into::into).collect()
    };
    Self {
      violations: rules(report.violations),
      passes: rules(report.passes),
      incomplete: rules(report.incomplete),
      inapplicable: rules(report.inapplicable),
      engine: report.engine,
    }
  }
}

/// One group of tools a page announced about itself.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct PageToolGroup {
  pub name: String,
  pub description: Option<String>,
  pub tools: Vec<PageTool>,
}

/// One callable the page exposes.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct PageTool {
  pub name: String,
  pub description: String,
  /// The JSON schema its input has to satisfy, as the page wrote it.
  #[napi(ts_type = "Record<string, unknown>")]
  pub input_schema: serde_json::Value,
  #[napi(ts_type = "Record<string, unknown>")]
  pub annotations: Option<serde_json::Value>,
}

impl From<ferridriver::page_tools::PageToolGroup> for PageToolGroup {
  fn from(group: ferridriver::page_tools::PageToolGroup) -> Self {
    Self {
      name: group.name,
      description: group.description,
      tools: group
        .tools
        .into_iter()
        .map(|tool| PageTool {
          name: tool.name,
          description: tool.description,
          input_schema: tool.input_schema,
          annotations: tool.annotations,
        })
        .collect(),
    }
  }
}

/// Options for `page.checkPageQuality`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct PageQualityOptions {
  /// Lighthouse audit ids to run, e.g. `meta-description`. Omitted or
  /// empty runs all ten.
  pub only: Option<Vec<String>>,
}

impl From<PageQualityOptions> for ferridriver::audits::PageQualityOptions {
  fn from(options: PageQualityOptions) -> Self {
    Self {
      only: options.only.unwrap_or_default(),
    }
  }
}

/// One element an audit objected to.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct PageAuditItem {
  /// A CSS selector locating the element.
  pub selector: String,
  /// The element's opening tag, truncated.
  pub snippet: String,
  /// Whatever the audit has to say about this element: the offending
  /// link text, the two aspect ratios, the size it should have been.
  pub detail: String,
}

/// One audit's verdict.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct PageAuditResult {
  /// Lighthouse's own audit id, e.g. `image-aspect-ratio`.
  pub id: String,
  /// `seo` or `best-practices`.
  pub category: String,
  /// Lighthouse's title for the state this audit is in.
  pub title: String,
  pub passed: bool,
  /// Why it failed, where the audit has more to say than its title.
  pub explanation: Option<String>,
  /// The elements behind a failure, which is the part anyone acts on.
  pub items: Vec<PageAuditItem>,
}

/// What the live-page Lighthouse audits concluded.
///
/// Eight of the ten read nothing but the DOM. `http-status-code` and
/// `is-crawlable` also read the main document's own response, and are
/// absent for a document that arrived without one.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct PageQualityReport {
  pub audits: Vec<PageAuditResult>,
}

impl From<ferridriver::audits::AuditItem> for PageAuditItem {
  fn from(item: ferridriver::audits::AuditItem) -> Self {
    Self {
      selector: item.selector,
      snippet: item.snippet,
      detail: item.detail,
    }
  }
}

impl From<ferridriver::audits::AuditResult> for PageAuditResult {
  fn from(audit: ferridriver::audits::AuditResult) -> Self {
    Self {
      id: audit.id,
      category: match audit.category {
        ferridriver::audits::AuditCategory::Seo => "seo".into(),
        ferridriver::audits::AuditCategory::BestPractices => "best-practices".into(),
      },
      title: audit.title,
      passed: audit.passed,
      explanation: audit.explanation,
      items: audit.items.into_iter().map(Into::into).collect(),
    }
  }
}

impl From<ferridriver::audits::PageQualityReport> for PageQualityReport {
  fn from(report: ferridriver::audits::PageQualityReport) -> Self {
    Self {
      audits: report.audits.into_iter().map(Into::into).collect(),
    }
  }
}
