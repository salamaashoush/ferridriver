//! Shared reporter machinery.
//!
//! Two things every reporter needs and none of them should own:
//!
//! - **Result collection.** Reporters see one event per *attempt*; a
//!   report talks about *tests*. [`ResultCollector`] folds attempts back
//!   into [`TestRecord`]s in first-seen order and answers the questions
//!   every format asks — did it pass, was it flaky, how long did it take.
//! - **Terminal rendering.** The numbered failure list, the `───` headers,
//!   the summary counts. Mirrors
//!   `/tmp/playwright/packages/playwright/src/reporters/base.ts`, so a
//!   `list`, `line`, `dot` or `github` run all print the same failure
//!   body.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rustc_hash::FxHashMap;

use super::{ReporterEvent, RunStatus};
use crate::model::{
  Attachment, AttachmentBody, ExpectedStatus, StepCategory, TestFailure, TestId, TestOutcome, TestOutcomeKind,
  TestStatus, TestStep,
};

// ── Screen ──

/// Terminal capabilities the rendering depends on. Detected once and
/// carried, so a reporter writing to a file renders the same bytes
/// regardless of who is attached to stdout.
#[derive(Debug, Clone, Copy)]
pub struct Screen {
  pub colors: bool,
  pub width: usize,
}

impl Screen {
  /// What the process is actually attached to.
  #[must_use]
  pub fn detect() -> Self {
    let term = console::Term::stdout();
    Self {
      colors: console::colors_enabled(),
      width: usize::from(term.size().1).max(1),
    }
  }

  /// A screen for output that is never a terminal (a file, an XML
  /// attribute, a CI annotation): no colors, fixed width.
  #[must_use]
  pub const fn plain() -> Self {
    Self {
      colors: false,
      width: 100,
    }
  }

  #[must_use]
  pub fn dim(self, text: &str) -> String {
    self.paint(text, console::Style::new().dim())
  }

  #[must_use]
  pub fn red(self, text: &str) -> String {
    self.paint(text, console::Style::new().red())
  }

  #[must_use]
  pub fn green(self, text: &str) -> String {
    self.paint(text, console::Style::new().green())
  }

  #[must_use]
  pub fn yellow(self, text: &str) -> String {
    self.paint(text, console::Style::new().yellow())
  }

  #[must_use]
  pub fn bold(self, text: &str) -> String {
    self.paint(text, console::Style::new().bold())
  }

  fn paint(self, text: &str, style: console::Style) -> String {
    if self.colors {
      style.force_styling(true).apply_to(text).to_string()
    } else {
      text.to_string()
    }
  }

  /// `text ─────────` padded to the screen width, capped at 100 columns
  /// the way Playwright's `separator()` is.
  #[must_use]
  pub fn separator(self, text: &str) -> String {
    let mut head = text.to_string();
    if !head.is_empty() {
      head.push(' ');
    }
    let columns = self.width.min(100);
    let visible = strip_ansi(&head).chars().count();
    let fill = columns.saturating_sub(visible);
    format!("{head}{}", self.dim(&"─".repeat(fill)))
  }
}

/// Drop ANSI SGR/CSI sequences. Reports written to a file, an XML
/// attribute or a CI annotation must not carry terminal escapes.
#[must_use]
pub fn strip_ansi(text: &str) -> Cow<'_, str> {
  if !text.contains('\u{1b}') {
    return Cow::Borrowed(text);
  }
  let mut out = String::with_capacity(text.len());
  let mut chars = text.chars();
  while let Some(c) = chars.next() {
    if c != '\u{1b}' {
      out.push(c);
      continue;
    }
    // CSI (`ESC [ … final`) and OSC (`ESC ] … BEL`) are the two forms a
    // terminal writer emits; anything else is a two-character escape.
    match chars.next() {
      Some('[') => {
        for c in chars.by_ref() {
          if c.is_ascii_alphabetic() {
            break;
          }
        }
      },
      Some(']') => {
        for c in chars.by_ref() {
          if c == '\u{7}' {
            break;
          }
        }
      },
      _ => {},
    }
  }
  Cow::Owned(out)
}

/// Playwright's `msToString`: sub-second in `ms`, above that one decimal
/// of seconds.
#[must_use]
pub fn ms_to_string(duration: Duration) -> String {
  let ms = duration.as_millis();
  if ms < 1000 {
    format!("{ms}ms")
  } else {
    format!("{:.1}s", duration.as_secs_f64())
  }
}

/// First `limit` characters, with an ellipsis when anything was cut.
#[must_use]
pub fn truncate_chars(text: &str, limit: usize) -> String {
  match text.char_indices().nth(limit) {
    Some((idx, _)) => format!("{}...", &text[..idx]),
    None => text.to_string(),
  }
}

/// Indent every non-empty line.
#[must_use]
pub fn indent(text: &str, pad: &str) -> String {
  text
    .lines()
    .map(|line| {
      if line.is_empty() {
        String::new()
      } else {
        format!("{pad}{line}")
      }
    })
    .collect::<Vec<_>>()
    .join("\n")
}

// ── Error locations ──

/// A `file:line:column` triple recovered from an error's stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorLocation {
  pub file: String,
  pub line: usize,
  pub column: usize,
}

/// The first source location named by a stack, in either the JS
/// (`at fn (file:12:5)` / `at file:12:5`) or Rust (`at file:12:5`,
/// `file:12`) spelling both runtimes produce.
#[must_use]
pub fn parse_error_location(stack: &str) -> Option<ErrorLocation> {
  for line in stack.lines() {
    let line = line.trim();
    let candidate = line
      .strip_prefix("at ")
      .map_or(line, str::trim)
      .trim_end_matches(')')
      .rsplit('(')
      .next()
      .unwrap_or(line);
    if let Some(loc) = split_location(candidate) {
      return Some(loc);
    }
  }
  None
}

/// `path:line[:column]` where `line` parses as a number. Windows drive
/// letters keep their colon because a single leading character is never
/// a valid line number position.
fn split_location(text: &str) -> Option<ErrorLocation> {
  let mut parts = text.rsplitn(3, ':');
  let last = parts.next()?;
  let middle = parts.next()?;
  let head = parts.next();
  if let (Ok(column), Ok(line)) = (last.parse::<usize>(), middle.parse::<usize>()) {
    let file = head?;
    if file.is_empty() {
      return None;
    }
    return Some(ErrorLocation {
      file: file.to_string(),
      line,
      column,
    });
  }
  if let Ok(line) = last.parse::<usize>() {
    let file = match head {
      Some(head) => format!("{head}:{middle}"),
      None => middle.to_string(),
    };
    if file.is_empty() {
      return None;
    }
    return Some(ErrorLocation { file, line, column: 0 });
  }
  None
}

/// Where a failure happened: the stack's first frame, falling back to
/// the test's own declaration site.
#[must_use]
pub fn failure_location(failure: &TestFailure, test_id: &TestId) -> ErrorLocation {
  failure
    .stack
    .as_deref()
    .and_then(parse_error_location)
    .unwrap_or_else(|| ErrorLocation {
      file: test_id.file.clone(),
      line: test_id.line.unwrap_or(0),
      column: test_id.column.unwrap_or(0),
    })
}

// ── Records ──

/// Identity a reporter groups attempts by: the project that ran it plus
/// the test's own identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TestKey {
  pub project: String,
  pub file: String,
  pub suite: Option<String>,
  pub name: String,
}

impl TestKey {
  #[must_use]
  pub fn of(outcome: &TestOutcome) -> Self {
    Self {
      project: outcome.project_name.clone(),
      file: outcome.test_id.file.clone(),
      suite: outcome.test_id.suite.clone(),
      name: outcome.test_id.name.clone(),
    }
  }
}

/// Every attempt of one test, oldest first.
#[derive(Debug, Clone)]
pub struct TestRecord {
  pub key: TestKey,
  pub attempts: Vec<Arc<TestOutcome>>,
}

impl TestRecord {
  /// The attempt that decided the result.
  #[must_use]
  pub fn last(&self) -> &Arc<TestOutcome> {
    self.attempts.last().expect("a record always has one attempt")
  }

  #[must_use]
  pub fn id(&self) -> &TestId {
    &self.last().test_id
  }

  /// How this test reads once every attempt is in.
  #[must_use]
  pub fn outcome_kind(&self) -> TestOutcomeKind {
    let statuses: Vec<TestStatus> = self.attempts.iter().map(|a| a.status).collect();
    crate::model::outcome_kind(&statuses, self.last().expected_status)
  }

  /// Playwright's `TestCase.ok()`.
  #[must_use]
  pub fn ok(&self) -> bool {
    !matches!(self.outcome_kind(), TestOutcomeKind::Unexpected)
  }

  /// Wall time across every attempt.
  #[must_use]
  pub fn total_duration(&self) -> Duration {
    self.attempts.iter().map(|a| a.duration).sum()
  }

  /// Stable per-project identity, the one a trace file on disk is named
  /// after and a UI asks to re-run by.
  #[must_use]
  pub fn stable_id(&self) -> String {
    self.id().stable_id(&self.key.project)
  }
}

/// Run-level facts a report header carries.
#[derive(Debug, Clone)]
pub struct RunInfo {
  pub total_tests: usize,
  pub num_workers: u32,
  pub metadata: serde_json::Value,
  pub start_time: SystemTime,
  pub duration: Duration,
  pub status: RunStatus,
  pub passed: usize,
  pub failed: usize,
  pub skipped: usize,
  pub flaky: usize,
  pub finished: bool,
}

impl Default for RunInfo {
  fn default() -> Self {
    Self {
      total_tests: 0,
      num_workers: 0,
      metadata: serde_json::Value::Null,
      start_time: SystemTime::UNIX_EPOCH,
      duration: Duration::ZERO,
      status: RunStatus::Passed,
      passed: 0,
      failed: 0,
      skipped: 0,
      flaky: 0,
      finished: false,
    }
  }
}

impl RunInfo {
  #[must_use]
  pub fn start_iso8601(&self) -> String {
    let ms = self
      .start_time
      .duration_since(SystemTime::UNIX_EPOCH)
      .ok()
      .and_then(|d| i64::try_from(d.as_millis()).ok())
      .unwrap_or_default();
    ferridriver::tracing::epoch_ms_to_iso8601(ms)
  }
}

/// Folds the attempt-level event stream back into per-test records.
/// Every reporter that produces a document (JSON, JUnit, HTML, CTRF,
/// markdown) and every terminal reporter that prints an epilogue drives
/// one of these instead of re-deriving the grouping.
#[derive(Debug, Clone, Default)]
pub struct ResultCollector {
  records: Vec<TestRecord>,
  index: FxHashMap<TestKey, usize>,
  /// Errors that belong to no test (config, global setup, dead worker).
  pub errors: Vec<TestFailure>,
  pub run: RunInfo,
}

impl ResultCollector {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Feed one event. Returns the record the event landed in, when it
  /// was a finished attempt — a live reporter prints from it directly.
  pub fn observe(&mut self, event: &ReporterEvent) -> Option<&TestRecord> {
    match event {
      ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata,
        start_time,
        ..
      } => {
        self.run.total_tests = *total_tests;
        self.run.num_workers = *num_workers;
        self.run.metadata = metadata.clone();
        self.run.start_time = *start_time;
        None
      },
      ReporterEvent::RunError { error } => {
        self.errors.push((**error).clone());
        None
      },
      ReporterEvent::TestFinished { outcome } => {
        let key = TestKey::of(outcome);
        let slot = if let Some(slot) = self.index.get(&key) {
          *slot
        } else {
          let slot = self.records.len();
          self.index.insert(key.clone(), slot);
          self.records.push(TestRecord {
            key,
            attempts: Vec::new(),
          });
          slot
        };
        let record = &mut self.records[slot];
        record.attempts.push(Arc::clone(outcome));
        record.attempts.sort_by_key(|a| a.attempt);
        Some(&self.records[slot])
      },
      ReporterEvent::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration,
        status,
      } => {
        self.run.total_tests = *total;
        self.run.passed = *passed;
        self.run.failed = *failed;
        self.run.skipped = *skipped;
        self.run.flaky = *flaky;
        self.run.duration = *duration;
        self.run.status = *status;
        self.run.finished = true;
        None
      },
      _ => None,
    }
  }

  #[must_use]
  pub fn records(&self) -> &[TestRecord] {
    &self.records
  }

  /// The record one attempt belongs to. Indexed, not scanned: a live
  /// reporter asks this once per finished test, and a linear scan makes
  /// that quadratic over a run.
  #[must_use]
  pub fn record_of(&self, outcome: &TestOutcome) -> Option<&TestRecord> {
    self.record_by_key(&TestKey::of(outcome))
  }

  /// [`Self::record_of`] for a key already in hand.
  #[must_use]
  pub fn record_by_key(&self, key: &TestKey) -> Option<&TestRecord> {
    self.index.get(key).map(|slot| &self.records[*slot])
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.records.is_empty()
  }

  /// Projects that produced a result, in first-seen order.
  #[must_use]
  pub fn projects(&self) -> Vec<String> {
    let mut seen = Vec::new();
    for record in &self.records {
      if !seen.contains(&record.key.project) {
        seen.push(record.key.project.clone());
      }
    }
    seen
  }

  /// Records grouped by source file, in first-seen order — the shape a
  /// JSON/JUnit suite tree and a markdown table are both built from.
  #[must_use]
  pub fn by_file(&self) -> Vec<(String, Vec<&TestRecord>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: FxHashMap<String, Vec<&TestRecord>> = FxHashMap::default();
    for record in &self.records {
      let file = record.key.file.clone();
      if !groups.contains_key(&file) {
        order.push(file.clone());
      }
      groups.entry(file).or_default().push(record);
    }
    order
      .into_iter()
      .filter_map(|file| groups.remove(&file).map(|tests| (file, tests)))
      .collect()
  }

  /// The tests a terminal epilogue lists and prints bodies for:
  /// unexpected failures first, then flaky ones.
  #[must_use]
  pub fn failures_to_print(&self) -> Vec<&TestRecord> {
    let mut out: Vec<&TestRecord> = self
      .records
      .iter()
      .filter(|r| r.outcome_kind() == TestOutcomeKind::Unexpected)
      .collect();
    out.extend(
      self
        .records
        .iter()
        .filter(|r| r.outcome_kind() == TestOutcomeKind::Flaky),
    );
    out
  }

  /// Counts recomputed from the records, for a reporter that has to
  /// stand on its own (a merge replaying blobs never sees the original
  /// run's tally).
  #[must_use]
  pub fn counts(&self) -> Counts {
    let mut counts = Counts::default();
    for record in &self.records {
      match record.outcome_kind() {
        TestOutcomeKind::Expected => counts.expected += 1,
        TestOutcomeKind::Unexpected => counts.unexpected += 1,
        TestOutcomeKind::Flaky => counts.flaky += 1,
        TestOutcomeKind::Skipped => counts.skipped += 1,
      }
    }
    counts
  }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
  pub expected: usize,
  pub unexpected: usize,
  pub flaky: usize,
  pub skipped: usize,
}

// ── Terminal rendering ──

/// `[project] › file:line:col › suite › name`, Playwright's
/// `formatTestTitle`.
#[must_use]
pub fn format_test_title(outcome: &TestOutcome) -> String {
  let id = &outcome.test_id;
  let location = format!("{}:{}:{}", id.file, id.line.unwrap_or(0), id.column.unwrap_or(0));
  let project = if outcome.project_name.is_empty() {
    String::new()
  } else {
    format!("[{}] › ", outcome.project_name)
  };
  let mut titles: Vec<String> = id.title_path();
  // `title_path` leads with the file, which `location` already names.
  titles.remove(0);
  format!("{project}{location} › {}", titles.join(" › "))
}

/// The `1) [project] › file:12:3 › suite › name ─────` header, with the
/// path to the deepest failing step appended in error mode.
#[must_use]
pub fn format_test_header(
  screen: Screen,
  record: &TestRecord,
  pad: &str,
  index: Option<usize>,
  error_mode: bool,
) -> String {
  let title = format_test_title(record.last());
  let numbered = match index {
    Some(i) => format!("{pad}{i}) {title}"),
    None => format!("{pad}{title}"),
  };
  let full = if error_mode {
    let mut steps = failing_step_path(&record.last().steps);
    if steps.is_empty() {
      for attempt in &record.attempts {
        steps = failing_step_path(&attempt.steps);
        if !steps.is_empty() {
          break;
        }
      }
    }
    if steps.is_empty() {
      numbered
    } else {
      format!("{numbered} › {}", steps.join(" › "))
    }
  } else {
    numbered
  };
  screen.separator(&full)
}

/// Titles down to the deepest failing user step. Stops at a level where
/// more than one step failed — there is no single path then.
fn failing_step_path(steps: &[TestStep]) -> Vec<String> {
  let failed: Vec<&TestStep> = steps
    .iter()
    .filter(|s| s.error.is_some() && s.category == StepCategory::TestStep)
    .collect();
  if failed.len() != 1 {
    return Vec::new();
  }
  let step = failed[0];
  let mut path = vec![step.title.clone()];
  path.extend(failing_step_path(&step.steps));
  path
}

/// One error rendered the way a terminal shows it: message, then the
/// diff (colorized), then the dimmed stack.
#[must_use]
pub fn format_error(screen: Screen, failure: &TestFailure) -> String {
  let mut out = String::new();
  out.push_str(failure.message.trim_end());
  if let Some(diff) = &failure.diff
    && !diff.trim().is_empty()
  {
    out.push('\n');
    for line in diff.lines() {
      out.push_str(&style_diff_line(screen, line));
      out.push('\n');
    }
    while out.ends_with('\n') {
      out.pop();
    }
  }
  if let Some(stack) = &failure.stack
    && !stack.trim().is_empty()
  {
    out.push('\n');
    out.push_str(&screen.dim(stack.trim_end()));
  }
  out
}

/// Colorize one line of an assertion body: `Expected:`/`Received:`/`Diff:`
/// labels, and the `-`/`+` markers of a unified diff.
#[must_use]
pub fn style_diff_line(screen: Screen, line: &str) -> String {
  let trimmed = line.trim_start();
  if trimmed.starts_with("Expected:") || trimmed.starts_with("Received:") || trimmed.starts_with("Diff:") {
    return screen.paint(line, console::Style::new().bold().cyan());
  }
  if trimmed.starts_with('-') && !trimmed.starts_with("--") {
    return screen.red(line);
  }
  if trimmed.starts_with('+') && !trimmed.starts_with("++") {
    return screen.green(line);
  }
  line.to_string()
}

/// The full failure body for one test: header, then every attempt that
/// produced errors, then its attachments and captured output.
#[must_use]
pub fn format_failure(screen: Screen, record: &TestRecord, index: Option<usize>) -> String {
  let mut lines: Vec<String> = Vec::new();
  let mut printed_header = false;

  for attempt in &record.attempts {
    let errors = attempt_errors(attempt);
    if errors.is_empty() {
      continue;
    }
    if !printed_header {
      lines.push(screen.red(&format_test_header(screen, record, "  ", index, true)));
      printed_header = true;
    }
    if attempt.attempt > 1 {
      lines.push(String::new());
      lines.push(screen.dim(&screen.separator(&format!("    Retry #{}", attempt.attempt - 1))));
    }
    for error in errors {
      lines.push(String::new());
      lines.push(indent(&format_error(screen, error), "    "));
    }
    lines.extend(attachment_lines(screen, &attempt.attachments));
    for (label, text) in [("stdout", &attempt.stdout), ("stderr", &attempt.stderr)] {
      if text.trim().is_empty() {
        continue;
      }
      lines.push(String::new());
      lines.push(screen.dim(&screen.separator(&format!("    {label}"))));
      lines.push(indent(text.trim_end(), "    "));
    }
  }

  if !printed_header {
    // A flaky test whose failing attempt carried no error still belongs
    // in the list — print the header alone.
    lines.push(screen.red(&format_test_header(screen, record, "  ", index, true)));
  }
  lines.push(String::new());
  lines.join("\n")
}

/// Errors of one attempt, hard failure first. Falls back to `error`
/// when a producer only filled the single-error field.
#[must_use]
pub fn attempt_errors(outcome: &TestOutcome) -> Vec<&TestFailure> {
  if !outcome.errors.is_empty() {
    return outcome.errors.iter().collect();
  }
  outcome.error.iter().collect()
}

/// `attachment #N: name (type)` blocks — a path for a file, the head of
/// the text for an inline text body, and the command that opens a trace.
fn attachment_lines(screen: Screen, attachments: &[Attachment]) -> Vec<String> {
  let mut lines = Vec::new();
  for (i, attachment) in attachments.iter().enumerate() {
    if attachment.name.starts_with('_') {
      continue;
    }
    let printable = attachment.content_type.starts_with("text/");
    match &attachment.body {
      AttachmentBody::Path(path) => {
        lines.push(String::new());
        lines.push(screen.dim(&screen.separator(&format!(
          "    attachment #{}: {} ({})",
          i + 1,
          screen.bold(&attachment.name),
          attachment.content_type
        ))));
        lines.push(screen.dim(&format!("    {}", path.display())));
        if attachment.name == "trace" {
          lines.push(screen.dim("    Usage:"));
          lines.push(String::new());
          lines.push(screen.dim(&format!("        ferridriver trace show {}", path.display())));
          lines.push(String::new());
        }
      },
      AttachmentBody::Bytes(bytes) if printable => {
        lines.push(String::new());
        lines.push(screen.dim(&screen.separator(&format!(
          "    attachment #{}: {} ({})",
          i + 1,
          screen.bold(&attachment.name),
          attachment.content_type
        ))));
        let text = String::from_utf8_lossy(bytes);
        lines.push(indent(&truncate_chars(&text, 300), "    "));
      },
      AttachmentBody::Bytes(_) => {},
    }
  }
  lines
}

/// Playwright's epilogue: the numbered failure bodies, the slow-test
/// warning, then the counts.
#[must_use]
pub fn epilogue(
  screen: Screen,
  collector: &ResultCollector,
  slow: Option<&crate::config::ReportSlowTestsConfig>,
  full: bool,
) -> String {
  let mut out: Vec<String> = Vec::new();
  let failures = collector.failures_to_print();

  if full && !failures.is_empty() {
    out.push(String::new());
    for (i, record) in failures.iter().enumerate() {
      out.push(format_failure(screen, record, Some(i + 1)));
    }
  }

  out.extend(slow_test_lines(screen, collector, slow));
  out.push(summary_message(screen, collector));
  out.join("\n")
}

/// `Slow test file: <file> (12.3s)` for every file over the threshold,
/// the way Playwright reports them — per file, not per test, because a
/// slow *file* is what you can split.
#[must_use]
pub fn slow_test_lines(
  screen: Screen,
  collector: &ResultCollector,
  slow: Option<&crate::config::ReportSlowTestsConfig>,
) -> Vec<String> {
  let Some(config) = slow else {
    return Vec::new();
  };
  let threshold = Duration::from_millis(config.threshold);
  let mut per_file: FxHashMap<String, (Duration, std::collections::BTreeSet<u32>)> = FxHashMap::default();
  for record in collector.records() {
    let entry = per_file.entry(record.key.file.clone()).or_default();
    for attempt in &record.attempts {
      entry.0 += attempt.duration;
      entry.1.insert(attempt.worker_index);
    }
  }
  // Only files a single worker owned: a file split across workers took
  // that long by parallelism, not by being slow.
  let mut durations: Vec<(String, Duration)> = per_file
    .into_iter()
    .filter(|(_, (duration, workers))| workers.len() == 1 && *duration > threshold)
    .map(|(file, (duration, _))| (file, duration))
    .collect();
  durations.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
  if config.max > 0 {
    durations.truncate(config.max);
  }
  if durations.is_empty() {
    return Vec::new();
  }
  let mut lines: Vec<String> = durations
    .into_iter()
    .map(|(file, duration)| {
      format!(
        "{}{file}{}",
        screen.yellow("  Slow test file: "),
        screen.yellow(&format!(" ({})", ms_to_string(duration)))
      )
    })
    .collect();
  lines.push(screen.yellow("  Consider splitting slow test files to speed up parallel execution."));
  lines
}

/// The trailing counts block: failed/interrupted/flaky listed by name,
/// then the skipped and passed tallies.
#[must_use]
pub fn summary_message(screen: Screen, collector: &ResultCollector) -> String {
  let mut tokens: Vec<String> = Vec::new();
  let unexpected: Vec<&TestRecord> = collector
    .records()
    .iter()
    .filter(|r| r.outcome_kind() == TestOutcomeKind::Unexpected)
    .collect();
  let interrupted: Vec<&TestRecord> = collector
    .records()
    .iter()
    .filter(|r| r.last().status == TestStatus::Interrupted)
    .collect();
  let flaky: Vec<&TestRecord> = collector
    .records()
    .iter()
    .filter(|r| r.outcome_kind() == TestOutcomeKind::Flaky)
    .collect();
  let counts = collector.counts();

  if !unexpected.is_empty() {
    tokens.push(screen.red(&format!("  {} failed", unexpected.len())));
    for record in &unexpected {
      tokens.push(screen.red(&format_test_header(screen, record, "    ", None, false)));
    }
  }
  if !interrupted.is_empty() {
    tokens.push(screen.yellow(&format!("  {} interrupted", interrupted.len())));
    for record in &interrupted {
      tokens.push(screen.yellow(&format_test_header(screen, record, "    ", None, false)));
    }
  }
  if !flaky.is_empty() {
    tokens.push(screen.yellow(&format!("  {} flaky", flaky.len())));
    for record in &flaky {
      tokens.push(screen.yellow(&format_test_header(screen, record, "    ", None, false)));
    }
  }
  if counts.skipped > 0 {
    tokens.push(screen.yellow(&format!("  {} skipped", counts.skipped)));
  }
  if counts.expected > 0 {
    tokens.push(format!(
      "{}{}",
      screen.green(&format!("  {} passed", counts.expected)),
      screen.dim(&format!(" ({})", ms_to_string(collector.run.duration)))
    ));
  }
  if !collector.errors.is_empty() {
    let n = collector.errors.len();
    tokens.push(screen.red(&if n == 1 {
      "  1 error was not a part of any test, see above for details".to_string()
    } else {
      format!("  {n} errors were not a part of any test, see above for details")
    }));
  }
  tokens.join("\n")
}

/// `Running N tests using M workers`, printed when a run begins.
#[must_use]
pub fn starting_message(screen: Screen, total: usize, workers: u32) -> String {
  if total == 0 {
    return String::new();
  }
  format!(
    "\n{}{total}{}{workers}{}",
    screen.dim("Running "),
    screen.dim(if total == 1 { " test using " } else { " tests using " }),
    screen.dim(if workers == 1 { " worker" } else { " workers" }),
  )
}

// ── Buffered output ──

/// A reporter's stdout. Reporters build a whole block and write it in
/// one call: a `println!` per line takes the stdout lock per line, and
/// with parallel workers finishing at once that both costs syscalls and
/// lets two reporters interleave mid-test.
/// Where a terminal reporter's bytes go. Stdout in a real run, a shared
/// buffer under test — without the second arm the only way to check what
/// `list` or `line` actually prints is to spawn a process and scrape it.
#[derive(Clone)]
pub enum Out {
  Stdout,
  Buffer(Arc<std::sync::Mutex<String>>),
}

impl Default for Out {
  fn default() -> Self {
    Self::Stdout
  }
}

impl Out {
  /// A sink plus the handle to read back what was written.
  #[must_use]
  pub fn buffer() -> (Self, Arc<std::sync::Mutex<String>>) {
    let held = Arc::new(std::sync::Mutex::new(String::new()));
    (Self::Buffer(Arc::clone(&held)), held)
  }

  /// Write a block, appending a newline unless it already ends in one.
  pub fn write(&self, text: &str) {
    if text.is_empty() {
      return;
    }
    if text.ends_with('\n') {
      self.write_raw(text);
    } else {
      self.write_raw(&format!("{text}\n"));
    }
  }

  /// Write without a trailing newline (progress glyphs, status lines).
  pub fn write_raw(&self, text: &str) {
    match self {
      Self::Stdout => {
        use std::io::Write;
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        let _ = lock.write_all(text.as_bytes());
        let _ = lock.flush();
      },
      Self::Buffer(held) => {
        if let Ok(mut held) = held.lock() {
          held.push_str(text);
        }
      },
    }
  }
}

// ── Output file resolution ──

/// Where a file reporter writes.
///
/// Precedence mirrors Playwright's `resolveOutputFile`: the
/// `FERRIDRIVER_<NAME>_OUTPUT_FILE` environment variable wins, then the
/// reporter's own `outputFile` option, then `outputDir` +
/// `FERRIDRIVER_<NAME>_OUTPUT_NAME` / the `fileName` option, and finally
/// the run's output directory with the built-in default name.
#[must_use]
pub fn resolve_output_file(
  reporter: &str,
  options: &std::collections::BTreeMap<String, serde_json::Value>,
  output_dir: &Path,
  default_name: &str,
) -> PathBuf {
  let upper = reporter.to_uppercase().replace('-', "_");
  if let Some(path) = env_path(&format!("FERRIDRIVER_{upper}_OUTPUT_FILE")) {
    return path;
  }
  if let Some(path) = str_option(options, "outputFile").or_else(|| str_option(options, "output_file")) {
    return PathBuf::from(path);
  }
  let dir = env_path(&format!("FERRIDRIVER_{upper}_OUTPUT_DIR"))
    .or_else(|| {
      str_option(options, "outputDir")
        .or_else(|| str_option(options, "output_dir"))
        .map(PathBuf::from)
    })
    .unwrap_or_else(|| output_dir.to_path_buf());
  let name = std::env::var(format!("FERRIDRIVER_{upper}_OUTPUT_NAME"))
    .ok()
    .filter(|v| !v.is_empty())
    .or_else(|| str_option(options, "fileName").or_else(|| str_option(options, "file_name")))
    .unwrap_or_else(|| default_name.to_string());
  dir.join(name)
}

fn env_path(name: &str) -> Option<PathBuf> {
  std::env::var(name).ok().filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// A string-valued reporter option under either the camelCase name the
/// config file uses or the snake_case one a programmatic caller does.
#[must_use]
pub fn str_option(options: &std::collections::BTreeMap<String, serde_json::Value>, key: &str) -> Option<String> {
  options.get(key).and_then(|v| v.as_str()).map(ToString::to_string)
}

/// A boolean reporter option, also readable from the environment as
/// `FERRIDRIVER_<REPORTER>_<KEY>` so CI can flip it without editing the
/// config — the same escape hatch Playwright's JUnit options have.
#[must_use]
pub fn bool_option(options: &std::collections::BTreeMap<String, serde_json::Value>, reporter: &str, key: &str) -> bool {
  let env_name = format!(
    "FERRIDRIVER_{}_{}",
    reporter.to_uppercase().replace('-', "_"),
    to_screaming_snake(key)
  );
  if let Ok(value) = std::env::var(&env_name) {
    return matches!(value.as_str(), "1" | "true" | "TRUE" | "True" | "yes" | "on");
  }
  options
    .get(key)
    .or_else(|| options.get(&to_snake(key)))
    .and_then(serde_json::Value::as_bool)
    .unwrap_or(false)
}

fn to_snake(camel: &str) -> String {
  let mut out = String::with_capacity(camel.len() + 4);
  for c in camel.chars() {
    if c.is_ascii_uppercase() {
      out.push('_');
      out.push(c.to_ascii_lowercase());
    } else {
      out.push(c);
    }
  }
  out
}

fn to_screaming_snake(camel: &str) -> String {
  to_snake(camel).to_uppercase()
}

/// The expected status of a test, as Playwright's JSON report spells it.
#[must_use]
pub fn expected_status_str(status: ExpectedStatus) -> &'static str {
  match status {
    ExpectedStatus::Pass => "passed",
    ExpectedStatus::Fail => "failed",
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn outcome(name: &str, attempt: u32, status: TestStatus) -> Arc<TestOutcome> {
    Arc::new(TestOutcome {
      test_id: TestId {
        file: "spec.ts".into(),
        suite: None,
        name: name.into(),
        line: Some(12),
        column: Some(3),
      },
      status,
      attempt,
      max_attempts: 3,
      ..Default::default()
    })
  }

  #[test]
  fn strip_ansi_removes_csi_sequences() {
    assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m"), "red");
    assert_eq!(strip_ansi("plain"), "plain");
  }

  #[test]
  fn a_retried_pass_reads_as_flaky() {
    let mut collector = ResultCollector::new();
    collector.observe(&ReporterEvent::TestFinished {
      outcome: outcome("t", 1, TestStatus::Failed),
    });
    collector.observe(&ReporterEvent::TestFinished {
      outcome: outcome("t", 2, TestStatus::Passed),
    });
    assert_eq!(collector.records().len(), 1);
    assert_eq!(collector.records()[0].outcome_kind(), TestOutcomeKind::Flaky);
    assert_eq!(collector.counts().flaky, 1);
  }

  #[test]
  fn an_expected_failure_is_not_a_failure() {
    let mut failing = TestOutcome {
      status: TestStatus::Failed,
      expected_status: ExpectedStatus::Fail,
      ..Default::default()
    };
    failing.test_id.name = "known bug".into();
    let mut collector = ResultCollector::new();
    collector.observe(&ReporterEvent::TestFinished {
      outcome: Arc::new(failing),
    });
    assert_eq!(collector.records()[0].outcome_kind(), TestOutcomeKind::Expected);
    assert!(collector.records()[0].ok());
  }

  #[test]
  fn error_location_comes_from_the_first_stack_frame() {
    let loc = parse_error_location("    at doThing (tests/a.spec.ts:42:9)\n    at other (b.ts:1:1)");
    assert_eq!(
      loc,
      Some(ErrorLocation {
        file: "tests/a.spec.ts".into(),
        line: 42,
        column: 9
      })
    );
  }

  #[test]
  fn error_location_accepts_a_rust_frame_without_a_column() {
    let loc = parse_error_location("at crates/x/src/lib.rs:17");
    assert_eq!(
      loc,
      Some(ErrorLocation {
        file: "crates/x/src/lib.rs".into(),
        line: 17,
        column: 0
      })
    );
  }

  #[test]
  fn a_title_names_the_project_and_the_location() {
    let mut o = TestOutcome {
      project_name: "chromium".into(),
      ..Default::default()
    };
    o.test_id = TestId {
      file: "a.spec.ts".into(),
      suite: Some("a.spec.ts::group".into()),
      name: "works".into(),
      line: Some(4),
      column: Some(2),
    };
    assert_eq!(format_test_title(&o), "[chromium] › a.spec.ts:4:2 › group › works");
  }

  #[test]
  fn output_file_prefers_the_option_over_the_default() {
    let mut options = std::collections::BTreeMap::new();
    options.insert("outputFile".to_string(), serde_json::json!("/tmp/custom.xml"));
    let path = resolve_output_file("junit", &options, Path::new("/out"), "junit.xml");
    assert_eq!(path, PathBuf::from("/tmp/custom.xml"));
    let path = resolve_output_file("junit", &Default::default(), Path::new("/out"), "junit.xml");
    assert_eq!(path, PathBuf::from("/out/junit.xml"));
  }
}
