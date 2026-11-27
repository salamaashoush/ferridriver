//! Unit tests for the test model types.

use std::time::{Duration, SystemTime};

use super::*;
use ferridriver::FerriError;

fn finished(id: &str, parent: Option<&str>, start_ms: u64) -> TestStep {
  TestStep {
    step_id: id.into(),
    title: id.into(),
    category: StepCategory::TestStep,
    duration: Duration::from_millis(1),
    subtitle: None,
    params: None,
    start_time: SystemTime::UNIX_EPOCH + Duration::from_millis(start_ms),
    status: StepStatus::Passed,
    error: None,
    location: None,
    annotations: Vec::new(),
    parent_step_id: parent.map(Into::into),
    metadata: None,
    steps: Vec::new(),
  }
}

fn outline(steps: &[TestStep]) -> String {
  steps
    .iter()
    .map(|step| {
      if step.steps.is_empty() {
        step.title.clone()
      } else {
        format!("{}({})", step.title, outline(&step.steps))
      }
    })
    .collect::<Vec<_>>()
    .join(" ")
}

#[test]
fn steps_nest_under_the_step_they_ran_inside_in_start_order() {
  // Finish order, as the runner records them: every child before its
  // parent.
  let tree = nest_steps(vec![
    finished("charge", Some("pay"), 3),
    finished("pay", Some("checkout"), 2),
    finished("receipt", Some("checkout"), 4),
    finished("checkout", None, 1),
    finished("plain", None, 5),
  ]);
  assert_eq!(outline(&tree), "checkout(pay(charge) receipt) plain");
}

#[test]
fn steps_that_overlap_keep_their_start_order() {
  // Two concurrent roots: the one that started first finishes last.
  let tree = nest_steps(vec![
    finished("b-child", Some("b"), 4),
    finished("b", None, 2),
    finished("a-child", Some("a"), 3),
    finished("a", None, 1),
  ]);
  assert_eq!(outline(&tree), "a(a-child) b(b-child)");
}

#[test]
fn a_step_without_its_parent_stays_visible() {
  // The parent never finished (the test timed out inside it), or
  // finished before a child that outlived its body.
  let tree = nest_steps(vec![
    finished("orphan", Some("never-finished"), 2),
    finished("early-parent", None, 1),
    finished("late-child", Some("early-parent"), 3),
  ]);
  assert_eq!(outline(&tree), "early-parent orphan late-child");
}

#[test]
fn a_plain_pass_is_expected() {
  assert_eq!(
    outcome_kind(&[TestStatus::Passed], ExpectedStatus::Pass),
    TestOutcomeKind::Expected
  );
}

#[test]
fn a_declared_failure_that_fails_is_expected() {
  assert_eq!(
    outcome_kind(&[TestStatus::Failed], ExpectedStatus::Fail),
    TestOutcomeKind::Expected
  );
  assert_eq!(
    outcome_kind(&[TestStatus::TimedOut], ExpectedStatus::Fail),
    TestOutcomeKind::Expected
  );
}

#[test]
fn a_declared_failure_that_passes_is_unexpected() {
  assert_eq!(
    outcome_kind(&[TestStatus::Passed], ExpectedStatus::Fail),
    TestOutcomeKind::Unexpected
  );
}

#[test]
fn failing_then_passing_is_flaky() {
  assert_eq!(
    outcome_kind(&[TestStatus::Failed, TestStatus::Passed], ExpectedStatus::Pass),
    TestOutcomeKind::Flaky
  );
}

#[test]
fn failing_every_attempt_stays_unexpected() {
  assert_eq!(
    outcome_kind(&[TestStatus::Failed, TestStatus::Failed], ExpectedStatus::Pass),
    TestOutcomeKind::Unexpected
  );
}

#[test]
fn only_skips_read_as_skipped() {
  assert_eq!(
    outcome_kind(&[TestStatus::Skipped], ExpectedStatus::Pass),
    TestOutcomeKind::Skipped
  );
  assert_eq!(outcome_kind(&[], ExpectedStatus::Pass), TestOutcomeKind::Skipped);
}

#[test]
fn status_strings_round_trip_and_accept_the_old_spelling() {
  for status in [
    TestStatus::Passed,
    TestStatus::Failed,
    TestStatus::TimedOut,
    TestStatus::Skipped,
    TestStatus::Flaky,
    TestStatus::Interrupted,
  ] {
    assert_eq!(TestStatus::parse(status.as_str()), status);
  }
  assert_eq!(TestStatus::parse("timed out"), TestStatus::TimedOut);
  assert_eq!(TestStatus::parse("timed-out"), TestStatus::TimedOut);
  assert_eq!(
    TestStatus::parse("nonsense"),
    TestStatus::Failed,
    "unknown never passes"
  );
}

#[test]
fn testfailure_from_timeout_keeps_class_prefix() {
  let tf = TestFailure::from(FerriError::timeout("navigating", 30_000));
  assert_eq!(tf.message, "TimeoutError: Timeout 30000ms exceeded while navigating");
}

#[test]
fn testfailure_from_target_closed_keeps_class_prefix() {
  let tf = TestFailure::from(FerriError::target_closed(Some("crashed".into())));
  assert_eq!(
    tf.message,
    "TargetClosedError: Target page, context or browser has been closed: crashed"
  );
}

#[test]
fn testfailure_from_backend_has_no_prefix() {
  let tf = TestFailure::from(FerriError::backend("launch failed"));
  assert_eq!(tf.message, "backend error: launch failed");
}

#[test]
fn testfailure_wrap_preserves_timeout_class_after_prefix() {
  let tf = TestFailure::wrap("fixture 'browser' failed", &FerriError::timeout("launch", 30_000));
  assert_eq!(
    tf.message,
    "fixture 'browser' failed: TimeoutError: Timeout 30000ms exceeded while launch"
  );
}

#[test]
fn testfailure_wrap_unnamed_keeps_message_only() {
  let tf = TestFailure::wrap("fixture 'page' failed", &FerriError::backend("oops"));
  assert_eq!(tf.message, "fixture 'page' failed: backend error: oops");
}

#[derive(Debug)]
struct Outer(std::io::Error);

impl std::fmt::Display for Outer {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("loading fixtures")
  }
}

impl std::error::Error for Outer {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    Some(&self.0)
  }
}

fn parse_count(text: &str) -> crate::Result<u32> {
  let re = regex::Regex::new(r"(\d+)")?;
  let digits = re.captures(text).and_then(|c| c.get(1)).context("no number rendered")?;
  Ok(digits.as_str().parse::<u32>()?)
}

#[test]
fn question_mark_accepts_any_error() {
  assert_eq!(parse_count("Count: 42").unwrap(), 42);
  assert_eq!(parse_count("Count: none").unwrap_err().message, "no number rendered");
  let overflow = parse_count("Count: 99999999999").unwrap_err();
  assert_eq!(overflow.message, "number too large to fit in target type");

  let json: std::result::Result<u32, _> = serde_json::from_str("nope");
  let failure = TestFailure::from(json.unwrap_err());
  assert!(failure.message.starts_with("expected ident"), "{}", failure.message);
}

#[test]
fn an_error_source_chain_is_kept() {
  let failure = TestFailure::from(Outer(std::io::Error::other("disk full")));
  assert_eq!(failure.message, "loading fixtures\n\nCaused by: disk full");
}

#[test]
fn typed_errors_keep_their_rendering() {
  let ferri = TestFailure::from(FerriError::timeout("navigating", 30_000));
  assert!(ferri.message.starts_with("TimeoutError: "), "{}", ferri.message);

  let mut assertion =
    ferridriver_expect::AssertionFailure::new("expect(value).toBe() failed", Some("Expected: 1".into()));
  assertion.error_context = Some("- button".into());
  let failure = TestFailure::from(assertion);
  assert_eq!(failure.message, "expect(value).toBe() failed");
  assert_eq!(failure.diff.as_deref(), Some("Expected: 1"));
  assert_eq!(failure.error_context.as_deref(), Some("- button"));
}

#[test]
fn context_prefixes_without_losing_detail() {
  let mut inner = TestFailure::msg("boom");
  inner.diff = Some("diff".into());
  let failure = Err::<(), _>(inner).context("saving the cart").unwrap_err();
  assert_eq!(failure.message, "saving the cart: boom");
  assert_eq!(failure.diff.as_deref(), Some("diff"));
  let lazy = None::<u8>.with_context(|| format!("row {}", 3)).unwrap_err();
  assert_eq!(lazy.message, "row 3");
}

#[test]
fn a_plan_keeps_its_suites_in_the_order_their_tests_were_added() {
  let mut builder = TestPlanBuilder::new();
  let added: Vec<String> = (0..16).map(|n| format!("{n:02}.spec.ts")).collect();
  for file in &added {
    builder.add_test(TestCase {
      id: TestId {
        file: file.clone(),
        suite: Some(format!("{file}::root")),
        name: "works".into(),
        ..TestId::default()
      },
      test_fn: std::sync::Arc::new(|_| Box::pin(async { Ok(()) })),
      fixture_requests: Vec::new(),
      annotations: Vec::new(),
      timeout: None,
      retries: None,
      expected_status: ExpectedStatus::Pass,
      use_options: None,
      metadata: None,
    });
  }
  let files: Vec<String> = builder
    .build()
    .suites
    .iter()
    .map(|suite| suite.tests[0].id.file.clone())
    .collect();
  assert_eq!(files, added);
}
