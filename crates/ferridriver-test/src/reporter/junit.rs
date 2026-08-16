//! JUnit XML reporter for CI integration.
//!
//! Mirrors `/tmp/playwright/packages/playwright/src/reporters/junit.ts`:
//! one `<testsuite>` per file per project, `<properties>` carrying the
//! test's annotations (the Xray convention), errors classified into
//! `<failure>` (an assertion) vs `<error>` (a thrown exception), and
//! attachments announced as `[[ATTACHMENT|path]]` inside `<system-out>`
//! — the marker Jenkins, Xray and Bamboo all look for.

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::model::{TestOutcome, TestOutcomeKind, TestStatus};
use crate::reporter::base::{self, ResultCollector, Screen, TestRecord};
use crate::reporter::{Reporter, ReporterEvent};

pub struct JUnitReporter {
  output_path: PathBuf,
  collector: ResultCollector,
  timestamp: String,
  include_project_in_test_name: bool,
  include_retries: bool,
  strip_ansi: bool,
  omit_tags: bool,
  suite_id: String,
  suite_name: String,
}

impl JUnitReporter {
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      collector: ResultCollector::new(),
      timestamp: ferridriver::tracing::now_iso8601(),
      include_project_in_test_name: false,
      include_retries: false,
      strip_ansi: false,
      omit_tags: false,
      suite_id: String::new(),
      suite_name: String::new(),
    }
  }

  /// Prefix every `<testcase name>` with `[project]`. Without it, two
  /// projects running the same file produce indistinguishable cases.
  #[must_use]
  pub fn with_include_project_in_test_name(mut self, include: bool) -> Self {
    self.include_project_in_test_name = include;
    self
  }

  /// Report each retry as its own `<flakyFailure>` / `<rerunFailure>`
  /// child instead of collapsing a test to its final attempt.
  #[must_use]
  pub fn with_include_retries(mut self, include: bool) -> Self {
    self.include_retries = include;
    self
  }

  /// Drop ANSI escapes from every attribute and text node. Some XML
  /// consumers render them literally.
  #[must_use]
  pub fn with_strip_ansi(mut self, strip: bool) -> Self {
    self.strip_ansi = strip;
    self
  }

  /// Leave tag annotations out of the `<properties>` block.
  #[must_use]
  pub fn with_omit_tags(mut self, omit: bool) -> Self {
    self.omit_tags = omit;
    self
  }

  #[must_use]
  pub fn with_suite_id(mut self, id: String) -> Self {
    self.suite_id = id;
    self
  }

  #[must_use]
  pub fn with_suite_name(mut self, name: String) -> Self {
    self.suite_name = name;
    self
  }

  fn render(&self) -> String {
    let mut totals = Totals::default();
    let mut body = String::new();
    for (project, file, records) in self.groups() {
      body.push_str(&self.render_suite(&project, &file, &records, &mut totals));
    }

    let mut xml = String::with_capacity(body.len() + 512);
    let _ = writeln!(xml, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
      xml,
      r#"<testsuites id="{}" name="{}" tests="{}" failures="{}" skipped="{}" errors="{}" time="{:.3}">"#,
      self.attr(&self.suite_id),
      self.attr(&self.suite_name),
      totals.tests,
      totals.failures,
      totals.skipped,
      totals.errors,
      self.collector.run.duration.as_secs_f64(),
    );
    xml.push_str(&body);
    let _ = writeln!(xml, "</testsuites>");
    xml
  }

  /// `(project, file, records)` triples, in first-seen order. Playwright
  /// nests project over file, so a multi-project run yields one
  /// `<testsuite>` per pair rather than one merged suite.
  fn groups(&self) -> Vec<(String, String, Vec<&TestRecord>)> {
    let mut order: Vec<(String, String)> = Vec::new();
    let mut grouped: rustc_hash::FxHashMap<(String, String), Vec<&TestRecord>> = rustc_hash::FxHashMap::default();
    for record in self.collector.records() {
      let key = (record.key.project.clone(), record.key.file.clone());
      if !grouped.contains_key(&key) {
        order.push(key.clone());
      }
      grouped.entry(key).or_default().push(record);
    }
    order
      .into_iter()
      .filter_map(|key| {
        grouped
          .remove(&key)
          .map(|records| (key.0.clone(), key.1.clone(), records))
      })
      .collect()
  }

  fn render_suite(&self, project: &str, file: &str, records: &[&TestRecord], totals: &mut Totals) -> String {
    let mut cases = String::new();
    let mut suite = Totals::default();
    for record in records {
      suite.tests += 1;
      if record.outcome_kind() == TestOutcomeKind::Skipped {
        suite.skipped += 1;
      }
      cases.push_str(&self.render_case(project, file, record, &mut suite));
    }
    let duration: f64 = records
      .iter()
      .flat_map(|r| r.attempts.iter())
      .map(|a| a.duration.as_secs_f64())
      .sum();

    totals.tests += suite.tests;
    totals.skipped += suite.skipped;
    totals.failures += suite.failures;
    totals.errors += suite.errors;

    let mut xml = String::new();
    let _ = writeln!(
      xml,
      r#"  <testsuite name="{}" timestamp="{}" hostname="{}" tests="{}" failures="{}" skipped="{}" time="{duration:.3}" errors="{}">"#,
      self.attr(file),
      self.attr(&self.timestamp),
      self.attr(project),
      suite.tests,
      suite.failures,
      suite.skipped,
      suite.errors,
    );
    xml.push_str(&cases);
    let _ = writeln!(xml, "  </testsuite>");
    xml
  }

  fn render_case(&self, project: &str, file: &str, record: &TestRecord, suite: &mut Totals) -> String {
    let last = record.last();
    let prefix = if self.include_project_in_test_name && !project.is_empty() {
      format!("[{project}] ")
    } else {
      String::new()
    };
    // Playwright drops root/project/file from the title path; ours leads
    // with the file, so the same slice is everything after it.
    let titles = record.id().title_path();
    let name = format!("{prefix}{}", titles[1..].join(" \u{203a} "));

    let time = if self.include_retries {
      record.attempts.first().map_or(0.0, |a| a.duration.as_secs_f64())
    } else {
      record.total_duration().as_secs_f64()
    };

    let mut children = String::new();
    children.push_str(&self.render_properties(last));

    let skipped = record.outcome_kind() == TestOutcomeKind::Skipped;
    if skipped {
      children.push_str("      <skipped/>\n");
    } else if self.include_retries {
      let reported: Vec<&std::sync::Arc<TestOutcome>> = record.attempts.iter().take(1).collect();
      children.push_str(&self.render_stdio(&reported));
      let retry_kind = if record.ok() { "flaky" } else { "rerun" };
      for attempt in record.attempts.iter().skip(1) {
        if matches!(attempt.status, TestStatus::Passed | TestStatus::Skipped) {
          continue;
        }
        children.push_str(&self.render_retry_entry(attempt, retry_kind));
      }
      // A test that recovered on retry counts as passing: no <failure>.
      if !record.ok()
        && let Some(entry) = self.render_failure(record, suite)
      {
        children.push_str(&entry);
      }
    } else {
      let all: Vec<&std::sync::Arc<TestOutcome>> = record.attempts.iter().collect();
      children.push_str(&self.render_stdio(&all));
      if !record.ok()
        && let Some(entry) = self.render_failure(record, suite)
      {
        children.push_str(&entry);
      }
    }

    let mut xml = String::new();
    let _ = writeln!(
      xml,
      r#"    <testcase name="{}" classname="{}" time="{time:.3}">"#,
      self.attr(&name),
      self.attr(file),
    );
    xml.push_str(&children);
    let _ = writeln!(xml, "    </testcase>");
    xml
  }

  /// Annotations as `<properties>` — the Xray JUnit extension, and the
  /// only standard slot a JUnit consumer has for test metadata.
  fn render_properties(&self, outcome: &TestOutcome) -> String {
    let mut rows = String::new();
    for annotation in &outcome.annotations {
      let (kind, description) = annotation_pair(annotation);
      if self.omit_tags && kind == "tag" {
        continue;
      }
      let _ = writeln!(
        rows,
        r#"        <property name="{}" value="{}"/>"#,
        self.attr(&kind),
        self.attr(&description),
      );
    }
    if rows.is_empty() {
      return String::new();
    }
    format!("      <properties>\n{rows}      </properties>\n")
  }

  fn render_failure(&self, record: &TestRecord, suite: &mut Totals) -> Option<String> {
    let info = record
      .attempts
      .iter()
      .find_map(|attempt| classify_error(attempt))
      .unwrap_or_else(|| {
        let id = record.id();
        ErrorInfo {
          element: "failure",
          kind: "FAILURE".to_string(),
          message: format!(
            "{}:{}:{} {}",
            std::path::Path::new(&id.file)
              .file_name()
              .map_or_else(|| id.file.clone(), |n| n.to_string_lossy().into_owned()),
            id.line.unwrap_or(0),
            id.column.unwrap_or(0),
            id.name
          ),
        }
      });
    if info.element == "error" {
      suite.errors += 1;
    } else {
      suite.failures += 1;
    }
    let body = base::format_failure(Screen::plain(), record, None);
    Some(format!(
      "      <{element} message=\"{message}\" type=\"{kind}\">{body}</{element}>\n",
      element = info.element,
      message = self.attr(&info.message),
      kind = self.attr(&info.kind),
      body = self.cdata(&body),
    ))
  }

  fn render_retry_entry(&self, attempt: &TestOutcome, prefix: &str) -> String {
    let info = classify_error(attempt);
    let element = format!(
      "{prefix}{}",
      match info.as_ref().map(|i| i.element) {
        Some("error") => "Error",
        _ => "Failure",
      }
    );
    let stack = attempt
      .error
      .as_ref()
      .map(|e| e.stack.clone().unwrap_or_else(|| e.message.clone()))
      .unwrap_or_default();
    let mut xml = String::new();
    let _ = writeln!(
      xml,
      r#"      <{element} message="{}" type="{}" time="{:.3}">"#,
      self.attr(&info.as_ref().map(|i| i.message.clone()).unwrap_or_default()),
      self.attr(info.as_ref().map_or("FAILURE", |i| i.kind.as_str())),
      attempt.duration.as_secs_f64(),
    );
    let _ = writeln!(xml, "        <stackTrace>{}</stackTrace>", self.cdata(&stack));
    let _ = writeln!(xml, "      </{element}>");
    xml
  }

  /// A single `<system-out>` / `<system-err>` per case — parsers in the
  /// wild only read the first of each.
  fn render_stdio(&self, attempts: &[&std::sync::Arc<TestOutcome>]) -> String {
    let mut out = String::new();
    let mut err = String::new();
    for attempt in attempts {
      out.push_str(&attempt.stdout);
      err.push_str(&attempt.stderr);
      for attachment in &attempt.attachments {
        let crate::model::AttachmentBody::Path(path) = &attachment.body else {
          continue;
        };
        // Relative to the report, so a CI server that resolves the
        // marker finds the file next to the XML it just parsed.
        let relative = self
          .output_path
          .parent()
          .and_then(|dir| pathdiff(path, dir))
          .unwrap_or_else(|| path.clone());
        if path.exists() {
          let _ = write!(out, "\n[[ATTACHMENT|{}]]\n", relative.display());
        } else {
          let _ = write!(err, "\nWarning: attachment {} is missing", relative.display());
        }
      }
    }
    let mut xml = String::new();
    if !out.is_empty() {
      let _ = writeln!(xml, "      <system-out>{}</system-out>", self.cdata(&out));
    }
    if !err.is_empty() {
      let _ = writeln!(xml, "      <system-err>{}</system-err>", self.cdata(&err));
    }
    xml
  }

  fn attr(&self, text: &str) -> String {
    let text = if self.strip_ansi {
      base::strip_ansi(text).into_owned()
    } else {
      text.to_string()
    };
    drop_discouraged(
      &text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"),
    )
  }

  fn cdata(&self, text: &str) -> String {
    let text = if self.strip_ansi {
      base::strip_ansi(text).into_owned()
    } else {
      text.to_string()
    };
    // `]]>` would end the section early; XML has no escape inside CDATA,
    // so the sequence is broken with an entity the way Playwright does.
    format!("<![CDATA[{}]]>", drop_discouraged(&text.replace("]]>", "]]&gt;")))
  }
}

#[derive(Default)]
struct Totals {
  tests: usize,
  failures: usize,
  errors: usize,
  skipped: usize,
}

struct ErrorInfo {
  element: &'static str,
  kind: String,
  message: String,
}

/// Split an error into JUnit's two buckets: a failed assertion is a
/// `<failure>` typed by the matcher, anything else is an `<error>`
/// typed by the exception class.
fn classify_error(outcome: &TestOutcome) -> Option<ErrorInfo> {
  let failure = base::attempt_errors(outcome).into_iter().next()?;
  let raw = base::strip_ansi(&failure.message).into_owned();
  let (error_name, body) = match raw.split_once(": ") {
    Some((name, rest)) if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') => (name, rest),
    _ => ("", raw.as_str()),
  };
  let first_line = body.lines().next().unwrap_or_default().trim().to_string();

  if let Some(matcher) = matcher_name(&raw) {
    return Some(ErrorInfo {
      element: "failure",
      kind: matcher,
      message: first_line,
    });
  }
  Some(ErrorInfo {
    element: "error",
    kind: if error_name.is_empty() {
      "Error".to_string()
    } else {
      error_name.to_string()
    },
    message: first_line,
  })
}

/// `expect(...).toHaveText` → `expect.toHaveText`, `expect(...).not.toBe`
/// → `expect.not.toBe`. Names the assertion that failed as the JUnit
/// `type`, which is how test-management tools group failures.
fn matcher_name(message: &str) -> Option<String> {
  let start = message.find("expect(")?;
  let rest = &message[start..];
  let close = rest.find(").")?;
  let after = &rest[close + 2..];
  let (negated, after) = match after.strip_prefix("not.") {
    Some(tail) => ("not.", tail),
    None => ("", after),
  };
  let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
  if name.is_empty() {
    return None;
  }
  Some(format!("expect.{negated}{name}"))
}

fn annotation_pair(annotation: &crate::model::TestAnnotation) -> (String, String) {
  use crate::model::TestAnnotation as A;
  match annotation {
    A::Skip { reason, .. } => ("skip".into(), reason.clone().unwrap_or_default()),
    A::Slow { reason, .. } => ("slow".into(), reason.clone().unwrap_or_default()),
    A::Fixme { reason, .. } => ("fixme".into(), reason.clone().unwrap_or_default()),
    A::Fail { reason, .. } => ("fail".into(), reason.clone().unwrap_or_default()),
    A::Only => ("only".into(), String::new()),
    A::Tag(tag) => ("tag".into(), tag.clone()),
    A::Info { type_name, description } => (type_name.clone(), description.clone()),
  }
}

/// Characters XML 1.0 discourages; some parsers reject the document
/// outright when a control byte from a test's stdout survives into it.
fn drop_discouraged(text: &str) -> String {
  text
    .chars()
    .filter(|c| {
      let n = *c as u32;
      !matches!(n, 0x0..=0x8 | 0xb..=0xc | 0xe..=0x1f | 0x7f..=0x84 | 0x86..=0x9f)
    })
    .collect()
}

/// `path` expressed relative to `base`, when both are absolute and share
/// a prefix. Falls back to `None` so the caller keeps the original.
fn pathdiff(path: &std::path::Path, base: &std::path::Path) -> Option<PathBuf> {
  let path = path.canonicalize().ok()?;
  let base = base.canonicalize().ok()?;
  let mut path_parts = path.components().peekable();
  let mut base_parts = base.components().peekable();
  while path_parts.peek().is_some() && path_parts.peek() == base_parts.peek() {
    path_parts.next();
    base_parts.next();
  }
  let mut out = PathBuf::new();
  for _ in base_parts {
    out.push("..");
  }
  out.extend(path_parts);
  Some(out)
}

#[async_trait::async_trait]
impl Reporter for JUnitReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    if let ReporterEvent::RunStarted { .. } = event {
      self.timestamp = ferridriver::tracing::now_iso8601();
    }
    self.collector.observe(event);
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    let xml = self.render();
    if let Some(parent) = self.output_path.parent() {
      std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&self.output_path, xml)?;
    tracing::info!("JUnit report written to {}", self.output_path.display());
    Ok(())
  }
}
