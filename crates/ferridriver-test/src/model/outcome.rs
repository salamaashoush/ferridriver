//! Test annotations, runtime modifiers, outcomes, failures, and attachments.

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use super::{TestId, TestStep};

// ── Annotations ──

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestAnnotation {
  /// Skip this test. Optional condition: `"firefox"`, `"chromium"`, `"linux"`, `"ci"`, `"!webkit"`.
  /// When condition is None, always skips. When condition is Some, skips only if condition matches.
  Skip {
    reason: Option<String>,
    condition: Option<String>,
  },
  /// Triple the timeout for this test (×3). Optional condition + description.
  /// Matches Playwright's `test.slow()` / `test.slow(condition, description)`.
  Slow {
    reason: Option<String>,
    condition: Option<String>,
  },
  /// Known bug — skip with intent to fix. Same condition semantics as Skip.
  /// Matches Playwright's `test.fixme()` / `test.fixme(condition, description)`.
  Fixme {
    reason: Option<String>,
    condition: Option<String>,
  },
  /// Expect this test to fail (inverts pass/fail). Optional condition + description.
  /// Matches Playwright's `test.fail()` / `test.fail(condition, description)`.
  Fail {
    reason: Option<String>,
    condition: Option<String>,
  },
  Only,
  Tag(String),
  /// A named lock: tests sharing one never run at the same time.
  /// Playwright's `test(title, { lock }, body)`.
  Lock(String),
  /// Structured metadata: type + description (e.g., issue/JIRA-1234, severity/critical).
  Info {
    type_name: String,
    description: String,
  },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExpectedStatus {
  #[default]
  Pass,
  Fail,
}

// ── Runtime Modifiers (shared between JS test body and Rust worker) ──

/// Runtime test modifiers set by `test.skip()`, `test.fail()`, `test.slow()` inside
/// a test body. The worker observes deadline updates while the body runs and
/// reads outcome modifiers after it finishes.
pub struct TestModifiers {
  /// Set by `test.skip()` / `test.fixme()` inside test body.
  pub skipped: AtomicBool,
  /// Reason for runtime skip.
  pub skip_reason: std::sync::Mutex<Option<String>>,
  /// Set by `test.fail()` inside test body — inverts pass/fail.
  pub expected_failure: AtomicBool,
  /// Set by `test.slow()` inside test body.
  pub slow: AtomicBool,
  /// Set by `testInfo.setTimeout()` inside test body.
  pub timeout_override: std::sync::Mutex<Option<u64>>,
  pub timeout_updates: tokio::sync::watch::Sender<Duration>,
}

impl Default for TestModifiers {
  fn default() -> Self {
    Self {
      skipped: AtomicBool::new(false),
      skip_reason: std::sync::Mutex::new(None),
      expected_failure: AtomicBool::new(false),
      slow: AtomicBool::new(false),
      timeout_override: std::sync::Mutex::new(None),
      timeout_updates: tokio::sync::watch::channel(Duration::ZERO).0,
    }
  }
}

impl std::fmt::Debug for TestModifiers {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TestModifiers")
      .field("skipped", &self.skipped.load(Ordering::Relaxed))
      .field("expected_failure", &self.expected_failure.load(Ordering::Relaxed))
      .field("slow", &self.slow.load(Ordering::Relaxed))
      .finish_non_exhaustive()
  }
}

// ── Outcome ──

/// Status of a completed test.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TestStatus {
  #[default]
  Passed,
  Failed,
  TimedOut,
  Skipped,
  /// Passed on retry (flaky).
  Flaky,
  /// Interrupted by signal/cancellation.
  Interrupted,
}

impl fmt::Display for TestStatus {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

impl TestStatus {
  /// Playwright's `TestStatus` spelling — the one every serialized
  /// report (JSON, blob, CTRF, `TeamCity`) and every consumer of them
  /// uses. `Display` prints the same strings so the two never drift.
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Passed => "passed",
      Self::Failed => "failed",
      Self::TimedOut => "timedOut",
      Self::Skipped => "skipped",
      Self::Flaky => "flaky",
      Self::Interrupted => "interrupted",
    }
  }

  /// Parse back from [`Self::as_str`]. Unknown input reads as `failed`
  /// rather than silently passing.
  #[must_use]
  pub fn parse(s: &str) -> Self {
    match s {
      "passed" => Self::Passed,
      // "timed out" was the pre-parity spelling; blobs written by an
      // older build still merge.
      "timedOut" | "timed out" | "timed-out" => Self::TimedOut,
      "skipped" => Self::Skipped,
      "flaky" => Self::Flaky,
      "interrupted" => Self::Interrupted,
      _ => Self::Failed,
    }
  }

  /// Whether this attempt counts as a failure for exit codes and
  /// `<failure>` elements.
  #[must_use]
  pub fn is_failure(self) -> bool {
    matches!(self, Self::Failed | Self::TimedOut | Self::Interrupted)
  }
}

/// Playwright's `TestCase.outcome()`: how a test reads once all its
/// attempts are in, as opposed to how one attempt ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TestOutcomeKind {
  #[default]
  Expected,
  Unexpected,
  Flaky,
  Skipped,
}

impl TestOutcomeKind {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Expected => "expected",
      Self::Unexpected => "unexpected",
      Self::Flaky => "flaky",
      Self::Skipped => "skipped",
    }
  }
}

impl fmt::Display for TestOutcomeKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// How a test reads once every attempt is in — Playwright's
/// `TestCase.outcome()`, and the single place that rule lives.
///
/// `expected` is measured against the test's declared status, so a
/// `test.fail()` test that fails is *expected* and one that passes is
/// *unexpected*.
#[must_use]
pub fn outcome_kind(attempts: &[TestStatus], expected: ExpectedStatus) -> TestOutcomeKind {
  let Some(last) = attempts.last().copied() else {
    return TestOutcomeKind::Skipped;
  };
  if attempts.iter().all(|s| *s == TestStatus::Skipped) {
    return TestOutcomeKind::Skipped;
  }
  let matches_expectation = |status: TestStatus| match expected {
    ExpectedStatus::Pass => matches!(status, TestStatus::Passed | TestStatus::Flaky | TestStatus::Skipped),
    ExpectedStatus::Fail => matches!(status, TestStatus::Failed | TestStatus::TimedOut),
  };
  if !matches_expectation(last) {
    return TestOutcomeKind::Unexpected;
  }
  if last == TestStatus::Flaky || attempts.iter().any(|s| !matches_expectation(*s)) {
    return TestOutcomeKind::Flaky;
  }
  TestOutcomeKind::Expected
}

/// Result of a single test attempt.
#[derive(Debug, Clone)]
pub struct TestOutcome {
  pub test_id: TestId,
  pub status: TestStatus,
  pub duration: Duration,
  pub attempt: u32,
  pub max_attempts: u32,
  pub error: Option<TestFailure>,
  /// Every error this attempt produced — the hard failure first, then
  /// the soft-assertion failures. Mirrors Playwright's
  /// `TestResult.errors`; `error` is the first entry.
  pub errors: Vec<TestFailure>,
  pub attachments: Vec<Attachment>,
  /// The snapshot comparisons the attempt made; their images are among
  /// `attachments`.
  pub snapshots: Vec<crate::snapshot_record::SnapshotComparison>,
  pub steps: Vec<TestStep>,
  pub stdout: String,
  pub stderr: String,
  /// Annotations from the test definition + runtime (tags, severity, issues, etc.).
  pub annotations: Vec<TestAnnotation>,
  /// Project/run metadata (from config). Available to reporters for JSON/HTML output.
  pub metadata: serde_json::Value,
  /// Project that ran this attempt. Empty for a single-project run.
  pub project_name: String,
  /// Worker that ran it (Playwright's `TestResult.workerIndex`).
  pub worker_index: u32,
  /// Playwright's `TestResult.parallelIndex` — the worker slot, which a
  /// restarted worker reuses.
  pub parallel_index: u32,
  /// Wall-clock start of this attempt. Reports quote it as ISO-8601.
  pub start_time: SystemTime,
  /// Status this test was declared to end in (`test.fail()` flips it).
  pub expected_status: ExpectedStatus,
  /// Effective timeout for this attempt, after `test.slow()` and
  /// `testInfo.setTimeout()`.
  pub timeout: Duration,
  /// [`super::TestCase::metadata`], carried onto the attempt so a reporter
  /// reads it from the outcome it is handed. Distinct from
  /// [`Self::metadata`], which is the RUN's (from config).
  pub case_metadata: Option<serde_json::Value>,
}

impl Default for TestOutcome {
  fn default() -> Self {
    Self {
      test_id: TestId::default(),
      status: TestStatus::default(),
      duration: Duration::ZERO,
      attempt: 1,
      max_attempts: 1,
      error: None,
      errors: Vec::new(),
      attachments: Vec::new(),
      snapshots: Vec::new(),
      steps: Vec::new(),
      stdout: String::new(),
      stderr: String::new(),
      annotations: Vec::new(),
      metadata: serde_json::Value::Null,
      project_name: String::new(),
      worker_index: 0,
      parallel_index: 0,
      start_time: SystemTime::UNIX_EPOCH,
      expected_status: ExpectedStatus::Pass,
      timeout: Duration::ZERO,
      case_metadata: None,
    }
  }
}

impl TestOutcome {
  /// Milliseconds since the Unix epoch at which this attempt started.
  #[must_use]
  pub fn start_epoch_ms(&self) -> i64 {
    self
      .start_time
      .duration_since(SystemTime::UNIX_EPOCH)
      .ok()
      .and_then(|d| i64::try_from(d.as_millis()).ok())
      .unwrap_or_default()
  }

  /// ISO-8601 rendering of [`Self::start_time`], the form Playwright's
  /// JSON report and `JUnit`'s `timestamp` attribute both use.
  #[must_use]
  pub fn start_iso8601(&self) -> String {
    ferridriver::tracing::epoch_ms_to_iso8601(self.start_epoch_ms())
  }

  /// Tags declared on the test, without the leading `@`.
  #[must_use]
  pub fn tags(&self) -> Vec<String> {
    self
      .annotations
      .iter()
      .filter_map(|a| match a {
        TestAnnotation::Tag(tag) => Some(tag.trim_start_matches('@').to_string()),
        _ => None,
      })
      .collect()
  }
}

/// A test failure with diagnostic information.
#[derive(Debug, Clone)]
pub struct TestFailure {
  pub message: String,
  pub stack: Option<String>,
  pub diff: Option<String>,
  /// Screenshot on failure (auto-captured).
  pub screenshot: Option<Vec<u8>>,
  /// The page's accessibility tree where an `expect` failed: Playwright's
  /// `TestInfoError.errorContext`, written into `error-context.md`.
  pub error_context: Option<String>,
}

impl fmt::Display for TestFailure {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.message)?;
    if let Some(diff) = &self.diff {
      write!(f, "\n{diff}")?;
    }
    Ok(())
  }
}

impl TestFailure {
  /// A failure carrying only a message: what `throw new Error(message)`
  /// is in a Playwright test.
  #[must_use]
  pub fn msg(message: impl fmt::Display) -> Self {
    Self {
      message: message.to_string(),
      stack: None,
      diff: None,
      screenshot: None,
      error_context: None,
    }
  }

  /// Wrap a [`ferridriver::FerriError`] with a contextual prefix while
  /// preserving the Playwright-style typed class name. The resulting
  /// message reads `"<prefix>: <Name>: <message>"` for distinguishable
  /// variants and `"<prefix>: <message>"` for unnamed ones, so consumers
  /// that match on `TimeoutError:` still see the marker after the prefix.
  #[must_use]
  pub fn wrap(prefix: impl fmt::Display, err: &ferridriver::FerriError) -> Self {
    Self::msg(format!("{prefix}: {}", err.display_named()))
  }

  /// Prefix the message with `context`, keeping the diff, stack and
  /// captured page state.
  #[must_use]
  pub fn context(mut self, context: impl fmt::Display) -> Self {
    self.message = format!("{context}: {}", self.message);
    self
  }

  /// The typed class name for the variants Playwright distinguishes
  /// (`TimeoutError` / `TargetClosedError`) stays in front of the
  /// message, so the TS bridge can re-hydrate a real class instance from
  /// the `<Name>: <message>` shape, the convention
  /// `ferridriver-node::error::to_napi` uses on the NAPI surface.
  fn from_ferri(err: &ferridriver::FerriError) -> Self {
    Self::msg(err.display_named())
  }

  /// Any other error: its message, then each `source()` underneath it.
  fn from_error(err: &(dyn std::error::Error + 'static)) -> Self {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
      message.push_str("\n\nCaused by: ");
      message.push_str(&cause.to_string());
      source = cause.source();
    }
    Self::msg(message)
  }
}

/// `?` on any error inside a test, hook or fixture fails it with that
/// error, the way any throw fails a Playwright test. Like `anyhow::Error`,
/// `TestFailure` is therefore not itself a `std::error::Error` (the
/// blanket conversion would otherwise overlap `From<T> for T`), and a
/// message-only failure is [`TestFailure::msg`].
///
/// [`ferridriver::FerriError`] keeps its typed class name and an
/// [`ferridriver_expect::AssertionFailure`] its diff, location and page
/// state.
impl<E> From<E> for TestFailure
where
  E: std::error::Error + Send + Sync + 'static,
{
  fn from(err: E) -> Self {
    let mut slot = Some(err);
    let any: &mut dyn std::any::Any = &mut slot;
    if let Some(ferri) = any
      .downcast_mut::<Option<ferridriver::FerriError>>()
      .and_then(Option::take)
    {
      return Self::from_ferri(&ferri);
    }
    if let Some(assertion) = any
      .downcast_mut::<Option<ferridriver_expect::AssertionFailure>>()
      .and_then(Option::take)
    {
      return crate::expect::value::assertion_failure(assertion);
    }
    match slot {
      Some(err) => Self::from_error(&err),
      None => Self::msg(""),
    }
  }
}

/// Attach context to a failure on its way out, as `anyhow::Context`:
///
/// ```ignore
/// let n: u32 = text.parse().context("todo counter")?;
/// let row = rows.first().context("no rows rendered")?;
/// ```
pub trait Context<T> {
  /// Fail with `context` in front of the underlying reason.
  ///
  /// # Errors
  ///
  /// The underlying failure, prefixed.
  fn context(self, context: impl fmt::Display) -> std::result::Result<T, TestFailure>;

  /// [`Self::context`] with the text built only on failure.
  ///
  /// # Errors
  ///
  /// The underlying failure, prefixed.
  fn with_context<C: fmt::Display>(self, context: impl FnOnce() -> C) -> std::result::Result<T, TestFailure>;
}

impl<T, E> Context<T> for std::result::Result<T, E>
where
  TestFailure: From<E>,
{
  fn context(self, context: impl fmt::Display) -> std::result::Result<T, TestFailure> {
    self.map_err(|e| TestFailure::from(e).context(context))
  }

  fn with_context<C: fmt::Display>(self, context: impl FnOnce() -> C) -> std::result::Result<T, TestFailure> {
    self.map_err(|e| TestFailure::from(e).context(context()))
  }
}

impl<T> Context<T> for Option<T> {
  fn context(self, context: impl fmt::Display) -> std::result::Result<T, TestFailure> {
    self.ok_or_else(|| TestFailure::msg(context))
  }

  fn with_context<C: fmt::Display>(self, context: impl FnOnce() -> C) -> std::result::Result<T, TestFailure> {
    self.ok_or_else(|| TestFailure::msg(context()))
  }
}

/// An artifact attached to a test result.
#[derive(Debug, Clone)]
pub struct Attachment {
  pub name: String,
  pub content_type: String,
  pub body: AttachmentBody,
  /// The step that produced it (`stepInfo.attach`), when one did.
  pub step_id: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AttachmentBody {
  Bytes(Vec<u8>),
  Path(PathBuf),
}

impl From<Vec<u8>> for AttachmentBody {
  fn from(bytes: Vec<u8>) -> Self {
    Self::Bytes(bytes)
  }
}

impl From<&[u8]> for AttachmentBody {
  fn from(bytes: &[u8]) -> Self {
    Self::Bytes(bytes.to_vec())
  }
}

impl From<String> for AttachmentBody {
  fn from(text: String) -> Self {
    Self::Bytes(text.into_bytes())
  }
}

impl From<&str> for AttachmentBody {
  fn from(text: &str) -> Self {
    Self::Bytes(text.as_bytes().to_vec())
  }
}

impl From<PathBuf> for AttachmentBody {
  fn from(path: PathBuf) -> Self {
    Self::Path(path)
  }
}

impl From<&std::path::Path> for AttachmentBody {
  fn from(path: &std::path::Path) -> Self {
    Self::Path(path.to_path_buf())
  }
}
