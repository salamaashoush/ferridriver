//! Structured test steps, source locations, and parent-child step tree nesting.

use std::fmt;
use std::time::{Duration, SystemTime};

use super::TestAnnotation;

/// Source location of a step (`file` + 1-based `line`), e.g. the BDD
/// step's `.feature` line or a `test.step` call site.
///
/// The file is the step's own, not its test's: an explicit
/// `test.step(title, body, { location })` and every BDD step name a
/// file the spec does not.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepLocation {
  pub file: String,
  pub line: u32,
  #[serde(default)]
  pub column: u32,
}

impl StepLocation {
  #[must_use]
  pub fn new(file: impl Into<String>, line: u32) -> Self {
    Self {
      file: file.into(),
      line,
      column: 0,
    }
  }

  /// Read back the `file:line` form older blob reports stored, so a
  /// merge across a version boundary keeps the location it has.
  #[must_use]
  pub fn parse(text: &str) -> Option<Self> {
    let (file, line) = text.rsplit_once(':')?;
    Some(Self::new(file, line.parse().ok()?))
  }
}

impl fmt::Display for StepLocation {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if self.column > 0 {
      write!(f, "{}:{}:{}", self.file, self.line, self.column)
    } else {
      write!(f, "{}:{}", self.file, self.line)
    }
  }
}

// ── Test Step ──

/// A structured test step (maps to Playwright's `test.step()`).
#[derive(Debug, Clone)]
pub struct TestStep {
  /// Unique step identifier (for parent/child tracking and reporter correlation).
  pub step_id: String,
  pub title: String,
  pub category: StepCategory,
  pub duration: Duration,
  /// Playwright's `TestStep.subtitle`: what complements the title, as
  /// `test.step(…, { subtitle })` gave it.
  pub subtitle: Option<String>,
  /// Playwright's `TestStep.params`: the parameters the test author
  /// passed as `test.step(…, { params })`.
  pub params: Option<serde_json::Value>,
  /// Wall-clock start, which places the step on a timeline. The epoch
  /// when it was never recorded (a report merged from an older blob).
  pub start_time: SystemTime,
  /// Step completion status.
  pub status: StepStatus,
  pub error: Option<String>,
  /// Where the step happened — its own file, which need not be the
  /// test's (`test.step(…, { location })`, a `.feature` line).
  pub location: Option<StepLocation>,
  /// Annotations the step recorded while it ran (`step.skip()`).
  pub annotations: Vec<TestAnnotation>,
  /// Parent step ID for nesting.
  pub parent_step_id: Option<String>,
  /// Arbitrary metadata for domain-specific extensions (e.g., BDD keyword, tags).
  /// Reporters can use this for custom rendering without the core needing domain knowledge.
  pub metadata: Option<serde_json::Value>,
  pub steps: Vec<TestStep>,
}

/// The steps of an attempt as the tree its reporters walk.
///
/// The runner records each step as it finishes, in one list, with the
/// id of the step it ran inside: a child therefore precedes its parent.
/// Every report reads [`TestStep::steps`] instead, in the order the
/// steps started, which is Playwright's `TestResult.steps`.
#[must_use]
pub fn nest_steps(finished: Vec<TestStep>) -> Vec<TestStep> {
  let known: rustc_hash::FxHashSet<String> = finished.iter().map(|step| step.step_id.clone()).collect();
  let mut waiting: rustc_hash::FxHashMap<String, Vec<TestStep>> = rustc_hash::FxHashMap::default();
  let mut roots = Vec::new();
  for mut step in finished {
    if let Some(mut children) = waiting.remove(&step.step_id) {
      children.sort_by_key(|child| child.start_time);
      step.steps.extend(children);
    }
    match step.parent_step_id.clone().filter(|parent| known.contains(parent)) {
      Some(parent) => waiting.entry(parent).or_default().push(step),
      None => roots.push(step),
    }
  }
  // A step still waiting names a parent that finished before it did,
  // which only a step outliving its own body does: it is kept, at the top.
  roots.extend(waiting.into_values().flatten());
  roots.sort_by_key(|step| step.start_time);
  roots
}

/// Status of a completed test step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
  Passed,
  Failed,
  Skipped,
  /// Step exists but is not yet implemented.
  Pending,
}

/// Category of a test step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepCategory {
  /// User-defined step via `test.step()`.
  TestStep,
  /// Expect assertion.
  Expect,
  /// Fixture setup/teardown.
  Fixture,
  /// Hook execution.
  Hook,
  /// Playwright API call.
  PwApi,
}

impl StepCategory {
  /// Whether this step category is visible in standard reporter output.
  /// `TestStep` and Hook are always shown. Expect, Fixture, `PwApi` are hidden
  /// unless verbose mode is enabled.
  #[must_use]
  pub fn is_visible(&self) -> bool {
    matches!(self, Self::TestStep | Self::Hook)
  }
}

impl fmt::Display for StepCategory {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::TestStep => write!(f, "test.step"),
      Self::Expect => write!(f, "expect"),
      Self::Fixture => write!(f, "fixture"),
      Self::Hook => write!(f, "hook"),
      Self::PwApi => write!(f, "pw:api"),
    }
  }
}
