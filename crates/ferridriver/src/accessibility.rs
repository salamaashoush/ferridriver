//! Accessibility auditing, by running axe-core inside the page.
//!
//! Lighthouse's accessibility category is 67 audits, and every one of
//! them is a wrapper that looks up a rule id in what axe-core already
//! decided. Porting the wrappers would port nothing; running the engine
//! they wrap covers all 67 at once and stays current with it.
//!
//! axe-core is not vendored here. It is a third-party artifact under a
//! different licence, fetched to the same cache as the browsers by
//! [`crate::install::BrowserInstaller::install_axe_core`] and pinned to
//! the version Lighthouse bundles so the two can be compared rule for
//! rule.
//!
//! The result shape below is axe's own. Reducing it to a pass/fail
//! count would throw away the part anyone acts on: which element, and
//! why.

use serde::{Deserialize, Serialize};

use crate::error::{FerriError, Result};

/// What to run, and over what.
#[derive(Debug, Clone, Default)]
pub struct AccessibilityOptions {
  /// Restrict the audit to these CSS selectors. Empty audits the whole
  /// document, which is what axe does by default.
  pub include: Vec<String>,
  /// Subtrees to leave out, for regions a page does not own.
  pub exclude: Vec<String>,
  /// Rule tags to run, such as `wcag2a` or `best-practice`. Empty runs
  /// every rule axe has.
  pub tags: Vec<String>,
}

/// One element a rule had something to say about.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessibilityNode {
  /// CSS selectors locating the element, outermost frame first.
  #[serde(default)]
  pub target: Vec<String>,
  #[serde(default)]
  pub html: String,
  /// Why the rule fired here, in axe's words.
  #[serde(default, rename = "failureSummary")]
  pub failure_summary: String,
}

/// One rule's verdict, with the elements behind it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessibilityRule {
  pub id: String,
  /// `minor`, `moderate`, `serious` or `critical`. Absent on rules that
  /// passed, and on some that could not be decided.
  #[serde(default)]
  pub impact: Option<String>,
  #[serde(default)]
  pub help: String,
  #[serde(default, rename = "helpUrl")]
  pub help_url: String,
  /// Which standards the rule belongs to (`wcag2a`, `best-practice`).
  #[serde(default)]
  pub tags: Vec<String>,
  #[serde(default)]
  pub nodes: Vec<AccessibilityNode>,
}

/// What axe found.
///
/// Four outcomes, not two. `incomplete` is the one that matters and the
/// one a pass/fail summary loses: axe reached a question it cannot
/// answer without a human, and reporting that as a pass would be a
/// claim nobody checked.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessibilityReport {
  #[serde(default)]
  pub violations: Vec<AccessibilityRule>,
  #[serde(default)]
  pub passes: Vec<AccessibilityRule>,
  #[serde(default)]
  pub incomplete: Vec<AccessibilityRule>,
  #[serde(default)]
  pub inapplicable: Vec<AccessibilityRule>,
  /// The engine that produced this, so a report carries the version its
  /// verdicts came from.
  #[serde(default)]
  pub engine: String,
}

impl AccessibilityReport {
  /// Every element that failed a rule, across all violations.
  #[must_use]
  pub fn violation_count(&self) -> usize {
    self.violations.iter().map(|rule| rule.nodes.len()).sum()
  }

  /// Violations ordered worst first, which is the order anyone reading
  /// them wants.
  #[must_use]
  pub fn violations_by_impact(&self) -> Vec<&AccessibilityRule> {
    let mut rules: Vec<&AccessibilityRule> = self.violations.iter().collect();
    rules.sort_by_key(|rule| {
      let rank = match rule.impact.as_deref() {
        Some("critical") => 0,
        Some("serious") => 1,
        Some("moderate") => 2,
        Some("minor") => 3,
        _ => 4,
      };
      (rank, std::cmp::Reverse(rule.nodes.len()))
    });
    rules
  }
}

/// Everything that happens in the page, in one evaluate.
///
/// One call and not two, deliberately. A frame-scoped evaluate can land
/// in the utility world, and axe defined by one evaluate is then
/// invisible to the next; running the load and the audit together means
/// they share a world whatever the backend chose. The load goes through
/// indirect eval so axe's UMD wrapper sees global scope and attaches to
/// `window` as it expects, rather than defining itself inside a
/// function and vanishing.
const INJECT_AND_RUN: &str = r"
(async (arg) => {
  if (typeof window.axe === 'undefined') {
    (0, eval)(arg.source);
  }
  return await (SCRIPT)(arg.options);
})
";

/// The call that runs axe once it is on the page.
///
/// `axe.run` resolves to an object far larger than the verdicts: every
/// rule carries its full check tree, and a page of any size produces
/// megabytes of it. This keeps the fields anyone reads and stringifies
/// the result, because a value this size crossing the protocol as a
/// structured object is re-serialised by each backend's own remote-value
/// format on the way.
const RUN_AXE: &str = r"
(async (options) => {
  if (typeof axe === 'undefined') return JSON.stringify({ error: 'axe-core did not load' });
  const context = {};
  if (options.include.length) context.include = options.include.map(s => [s]);
  if (options.exclude.length) context.exclude = options.exclude.map(s => [s]);
  const runOptions = { resultTypes: ['violations', 'incomplete'] };
  if (options.tags.length) runOptions.runOnly = { type: 'tag', values: options.tags };
  try {
    const result = await axe.run(Object.keys(context).length ? context : document, runOptions);
    const rule = r => ({
      id: r.id, impact: r.impact ?? null, help: r.help ?? '', helpUrl: r.helpUrl ?? '',
      tags: r.tags ?? [],
      nodes: (r.nodes ?? []).map(n => ({
        target: (n.target ?? []).map(String), html: n.html ?? '', failureSummary: n.failureSummary ?? '',
      })),
    });
    return JSON.stringify({
      violations: result.violations.map(rule),
      passes: result.passes.map(rule),
      incomplete: result.incomplete.map(rule),
      inapplicable: result.inapplicable.map(rule),
      engine: 'axe-core/' + (result.testEngine?.version ?? 'unknown'),
    });
  } catch (e) {
    // A throw does not survive the trip out of the page: the host sees
    // `Uncaught` and nothing else. Hand the message back as data.
    return JSON.stringify({ error: String(e && e.message ? e.message : e) });
  }
})
";

/// Turn what the page returned into a report.
///
/// # Errors
///
/// [`FerriError::Backend`] when axe reported a failure of its own, or
/// when the payload is not the shape this asked for.
pub fn parse_report(payload: &str) -> Result<AccessibilityReport> {
  #[derive(Deserialize)]
  struct Envelope {
    #[serde(default)]
    error: Option<String>,
  }
  let envelope: Envelope =
    serde_json::from_str(payload).map_err(|e| FerriError::backend(format!("axe-core returned no result: {e}")))?;
  if let Some(error) = envelope.error {
    return Err(FerriError::backend(format!("axe-core failed: {error}")));
  }
  serde_json::from_str(payload).map_err(|e| FerriError::backend(format!("axe-core result was unreadable: {e}")))
}

/// The single expression that loads axe and runs it.
#[must_use]
pub fn inject_and_run_source() -> String {
  INJECT_AND_RUN.replace("SCRIPT", RUN_AXE)
}

/// What the page-side call needs, as one argument.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditArgument<'a> {
  pub source: &'a str,
  pub options: AuditOptions,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditOptions {
  pub include: Vec<String>,
  pub exclude: Vec<String>,
  pub tags: Vec<String>,
}

impl From<AccessibilityOptions> for AuditOptions {
  fn from(options: AccessibilityOptions) -> Self {
    Self {
      include: options.include,
      exclude: options.exclude,
      tags: options.tags,
    }
  }
}

/// Read the installed axe-core, or say how to get it.
///
/// # Errors
///
/// [`FerriError::Unsupported`] when it has not been installed, because
/// this is a missing prerequisite rather than a failure of the page.
pub fn load_axe_source(path: &std::path::Path) -> Result<String> {
  std::fs::read_to_string(path).map_err(|e| {
    FerriError::unsupported(format!(
      "axe-core is not installed ({e}). Run `ferridriver install axe` to fetch it into {}",
      path.display()
    ))
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_error_from_the_page_becomes_a_typed_error_not_a_report() {
    let error = parse_report(r#"{"error":"axe-core did not load"}"#).unwrap_err();
    assert!(error.to_string().contains("did not load"), "{error}");
  }

  #[test]
  fn violations_are_ordered_worst_first_then_by_how_many_elements_failed() {
    let rule = |id: &str, impact: Option<&str>, nodes: usize| AccessibilityRule {
      id: id.into(),
      impact: impact.map(str::to_string),
      help: String::new(),
      help_url: String::new(),
      tags: Vec::new(),
      nodes: vec![
        AccessibilityNode {
          target: vec!["html".into()],
          html: String::new(),
          failure_summary: String::new()
        };
        nodes
      ],
    };
    let report = AccessibilityReport {
      violations: vec![
        rule("minor-one", Some("minor"), 9),
        rule("critical-one", Some("critical"), 1),
        rule("serious-few", Some("serious"), 1),
        rule("serious-many", Some("serious"), 4),
      ],
      ..Default::default()
    };
    let order: Vec<&str> = report.violations_by_impact().iter().map(|r| r.id.as_str()).collect();
    assert_eq!(order, ["critical-one", "serious-many", "serious-few", "minor-one"]);
    assert_eq!(report.violation_count(), 15);
  }

  #[test]
  fn a_real_axe_payload_keeps_the_element_and_the_reason() {
    let payload = r#"{
      "violations": [{
        "id": "image-alt", "impact": "critical", "help": "Images must have alternate text",
        "helpUrl": "https://dequeuniversity.com/rules/axe/4.12/image-alt", "tags": ["wcag2a"],
        "nodes": [{"target": ["body > img.thumb"], "html": "<img class=\"thumb\">",
                   "failureSummary": "Fix any of the following: Element has no alt attribute"}]
      }],
      "passes": [], "incomplete": [], "inapplicable": [],
      "engine": "axe-core/4.12.1"
    }"#;
    let report = parse_report(payload).unwrap();
    assert_eq!(report.engine, "axe-core/4.12.1");
    let violation = &report.violations[0];
    assert_eq!(violation.id, "image-alt");
    assert_eq!(violation.nodes[0].target, ["body > img.thumb"]);
    assert!(violation.nodes[0].failure_summary.contains("no alt attribute"));
  }
}
