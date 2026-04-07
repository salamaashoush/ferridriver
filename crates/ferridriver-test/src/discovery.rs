//! Test discovery: inventory-based collection for Rust, glob-based file scanning.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::config::TestConfig;
use crate::fixture::FixturePool;
use std::fmt;
use std::path::Path;

use crate::model::{
  ExpectedStatus, Hooks, TestAnnotation, TestCase, TestFailure, TestId, TestPlan, TestSuite,
};

// ── Inventory-based registration (populated by #[ferritest] macro) ──

/// What the `#[ferritest]` proc macro submits via `inventory::submit!`.
pub struct TestRegistration {
  pub file: &'static str,
  pub line: u32,
  pub name: &'static str,
  pub suite: Option<&'static str>,
  pub fixture_requests: &'static [&'static str],
  pub annotations: &'static [TestAnnotation],
  pub timeout_ms: Option<u64>,
  pub retries: Option<u32>,
  pub test_fn: fn(FixturePool) -> Pin<Box<dyn Future<Output = Result<(), TestFailure>> + Send>>,
}

inventory::collect!(TestRegistration);

// ── Discovery ──

/// Collect all registered tests and build a `TestPlan`.
pub fn collect_rust_tests(config: &TestConfig) -> TestPlan {
  let mut suites: rustc_hash::FxHashMap<String, TestSuite> = rustc_hash::FxHashMap::default();

  for reg in inventory::iter::<TestRegistration> {
    let file = reg.file.to_string();
    let suite_name = reg.suite.map(String::from);
    let suite_key = format!("{}::{}", file, suite_name.as_deref().unwrap_or(""));

    let test_fn_ptr = reg.test_fn;
    let test_case: TestCase = TestCase {
      id: TestId {
        file: file.clone(),
        suite: suite_name.clone(),
        name: reg.name.to_string(),
        line: None,
      },
      test_fn: Arc::new(move |pool| test_fn_ptr(pool)),
      fixture_requests: reg.fixture_requests.iter().map(|s| (*s).to_string()).collect(),
      annotations: reg.annotations.to_vec(),
      timeout: reg.timeout_ms.map(std::time::Duration::from_millis),
      retries: reg.retries,
      expected_status: if reg.annotations.iter().any(|a| matches!(a, TestAnnotation::Fail)) {
        ExpectedStatus::Fail
      } else {
        ExpectedStatus::Pass
      },
    };

    let suite = suites.entry(suite_key).or_insert_with(|| TestSuite {
      name: suite_name.unwrap_or_else(|| file.clone()),
      file: file.clone(),
      tests: Vec::new(),
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: crate::model::SuiteMode::default(),
    });
    suite.tests.push(test_case);
  }

  let suites: Vec<TestSuite> = suites.into_values().collect();
  let total_tests = suites.iter().map(|s| s.tests.len()).sum();

  apply_filters(
    TestPlan {
      suites,
      total_tests,
      shard: None,
    },
    config,
  )
}

/// Discover test files on disk using glob patterns.
///
/// # Errors
///
/// Returns an error if glob pattern is invalid.
pub fn find_test_files(root: &str, patterns: &[String], ignore: &[String]) -> Result<Vec<String>, String> {
  let mut files = Vec::new();

  for pattern in patterns {
    let full_pattern = if pattern.starts_with('/') || pattern.starts_with('.') {
      pattern.clone()
    } else {
      format!("{root}/{pattern}")
    };

    let entries =
      glob::glob(&full_pattern).map_err(|e| format!("invalid glob pattern '{full_pattern}': {e}"))?;

    for entry in entries {
      let path = entry.map_err(|e| format!("glob error: {e}"))?;
      let path_str = path.display().to_string();

      // Check ignore patterns.
      let ignored = ignore.iter().any(|ig| {
        glob::Pattern::new(ig)
          .map(|p| p.matches(&path_str))
          .unwrap_or(false)
      });

      if !ignored {
        files.push(path_str);
      }
    }
  }

  files.sort();
  files.dedup();
  Ok(files)
}

/// Apply grep, tag, and other filters to a test plan.
fn apply_filters(mut plan: TestPlan, _config: &TestConfig) -> TestPlan {
  // Grep is applied at runtime via CLI, not from config file typically.
  // This is a placeholder for the runner to call with CliOverrides.
  plan.total_tests = plan.suites.iter().map(|s| s.tests.len()).sum();
  plan
}

/// Filter a test plan by grep pattern.
pub fn filter_by_grep(plan: &mut TestPlan, pattern: &str, invert: bool) {
  // Build a case-insensitive regex. If the pattern has invalid regex syntax,
  // fall back to case-insensitive literal substring match.
  let re = regex::RegexBuilder::new(pattern)
    .case_insensitive(true)
    .build()
    .ok();
  let pattern_lower = pattern.to_lowercase();

  for suite in &mut plan.suites {
    suite.tests.retain(|test| {
      let full_name = test.id.full_name();
      let matches = if let Some(ref r) = re {
        r.is_match(&full_name)
      } else {
        // Fallback: case-insensitive substring search.
        full_name.to_lowercase().contains(&pattern_lower)
      };
      if invert { !matches } else { matches }
    });
  }
  plan.suites.retain(|s| !s.tests.is_empty());
  plan.total_tests = plan.suites.iter().map(|s| s.tests.len()).sum();
}

/// Error returned when `--forbid-only` is set and `.only()` markers are found.
pub struct ForbidOnlyError {
  pub tests: Vec<String>,
}

impl fmt::Display for ForbidOnlyError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    writeln!(f, "Error: test.only() found in {} test(s):", self.tests.len())?;
    for name in &self.tests {
      writeln!(f, "  {name}")?;
    }
    Ok(())
  }
}

/// Check that no tests or suites have `Only` annotations.
/// Returns `Err` listing all offending tests if any are found.
pub fn check_forbid_only(plan: &TestPlan) -> Result<(), ForbidOnlyError> {
  let mut only_tests: Vec<String> = Vec::new();

  for suite in &plan.suites {
    let suite_is_only = suite.annotations.iter().any(|a| matches!(a, TestAnnotation::Only));
    for test in &suite.tests {
      let test_is_only = test.annotations.iter().any(|a| matches!(a, TestAnnotation::Only));
      if suite_is_only || test_is_only {
        only_tests.push(test.id.full_name());
      }
    }
  }

  if only_tests.is_empty() {
    Ok(())
  } else {
    Err(ForbidOnlyError { tests: only_tests })
  }
}

/// Filter a test plan to only `Only`-marked tests/suites.
/// If no `Only` annotations exist, the plan is unchanged.
pub fn filter_by_only(plan: &mut TestPlan) {
  let has_only = plan.suites.iter().any(|suite| {
    suite.annotations.iter().any(|a| matches!(a, TestAnnotation::Only))
      || suite.tests.iter().any(|t| t.annotations.iter().any(|a| matches!(a, TestAnnotation::Only)))
  });

  if !has_only {
    return;
  }

  for suite in &mut plan.suites {
    let suite_is_only = suite.annotations.iter().any(|a| matches!(a, TestAnnotation::Only));
    if !suite_is_only {
      suite.tests.retain(|t| t.annotations.iter().any(|a| matches!(a, TestAnnotation::Only)));
    }
  }
  plan.suites.retain(|s| !s.tests.is_empty());
  plan.total_tests = plan.suites.iter().map(|s| s.tests.len()).sum();
}

/// Filter a test plan to only tests listed in a rerun file.
/// The rerun file contains one `file:line` or `file > suite > name` entry per line.
/// If the file doesn't exist or is empty, logs a warning and runs all tests.
pub fn filter_by_rerun(plan: &mut TestPlan, rerun_path: &Path) {
  let content = match std::fs::read_to_string(rerun_path) {
    Ok(c) if !c.trim().is_empty() => c,
    Ok(_) => {
      tracing::warn!("rerun file {} is empty, running all tests", rerun_path.display());
      return;
    }
    Err(_) => {
      tracing::warn!("rerun file {} not found, running all tests", rerun_path.display());
      return;
    }
  };

  let rerun_set: rustc_hash::FxHashSet<String> = content
    .lines()
    .map(|l| l.trim().to_string())
    .filter(|l| !l.is_empty())
    .collect();

  for suite in &mut plan.suites {
    suite.tests.retain(|test| {
      rerun_set.contains(&test.id.file_location())
        || rerun_set.contains(&test.id.full_name())
    });
  }
  plan.suites.retain(|s| !s.tests.is_empty());
  plan.total_tests = plan.suites.iter().map(|s| s.tests.len()).sum();
}

/// Filter a test plan by tag.
pub fn filter_by_tag(plan: &mut TestPlan, tag: &str) {
  for suite in &mut plan.suites {
    suite.tests.retain(|test| {
      test
        .annotations
        .iter()
        .any(|a| matches!(a, TestAnnotation::Tag(t) if t == tag))
    });
  }
  plan.suites.retain(|s| !s.tests.is_empty());
  plan.total_tests = plan.suites.iter().map(|s| s.tests.len()).sum();
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{ExpectedStatus, Hooks, TestCase, TestPlan, TestSuite};

  fn dummy_test(name: &str, annotations: Vec<TestAnnotation>) -> TestCase {
    TestCase {
      id: TestId {
        file: "test.rs".into(),
        suite: Some("suite".into()),
        name: name.into(),
        line: None,
      },
      test_fn: Arc::new(|_| Box::pin(async { Ok(()) })),
      fixture_requests: vec![],
      annotations,
      timeout: None,
      retries: None,
      expected_status: ExpectedStatus::Pass,
    }
  }

  fn make_plan(tests: Vec<TestCase>, suite_annotations: Vec<TestAnnotation>) -> TestPlan {
    let total = tests.len();
    TestPlan {
      suites: vec![TestSuite {
        name: "suite".into(),
        file: "test.rs".into(),
        tests,
        hooks: Hooks::default(),
        annotations: suite_annotations,
        mode: crate::model::SuiteMode::default(),
      }],
      total_tests: total,
      shard: None,
    }
  }

  #[test]
  fn forbid_only_no_only_markers() {
    let plan = make_plan(
      vec![dummy_test("test1", vec![]), dummy_test("test2", vec![])],
      vec![],
    );
    assert!(check_forbid_only(&plan).is_ok());
  }

  #[test]
  fn forbid_only_detects_test_level_only() {
    let plan = make_plan(
      vec![
        dummy_test("normal", vec![]),
        dummy_test("focused", vec![TestAnnotation::Only]),
      ],
      vec![],
    );
    let err = check_forbid_only(&plan).unwrap_err();
    assert_eq!(err.tests.len(), 1);
    assert!(err.tests[0].contains("focused"));
  }

  #[test]
  fn forbid_only_detects_suite_level_only() {
    let plan = make_plan(
      vec![dummy_test("test1", vec![]), dummy_test("test2", vec![])],
      vec![TestAnnotation::Only],
    );
    let err = check_forbid_only(&plan).unwrap_err();
    assert_eq!(err.tests.len(), 2);
  }

  #[test]
  fn filter_by_only_keeps_only_marked_tests() {
    let mut plan = make_plan(
      vec![
        dummy_test("normal1", vec![]),
        dummy_test("focused", vec![TestAnnotation::Only]),
        dummy_test("normal2", vec![]),
      ],
      vec![],
    );
    filter_by_only(&mut plan);
    assert_eq!(plan.total_tests, 1);
    assert_eq!(plan.suites[0].tests[0].id.name, "focused");
  }

  #[test]
  fn filter_by_only_no_only_keeps_all() {
    let mut plan = make_plan(
      vec![dummy_test("test1", vec![]), dummy_test("test2", vec![])],
      vec![],
    );
    filter_by_only(&mut plan);
    assert_eq!(plan.total_tests, 2);
  }

  #[test]
  fn filter_by_only_suite_level_keeps_all_in_suite() {
    let mut plan = make_plan(
      vec![dummy_test("test1", vec![]), dummy_test("test2", vec![])],
      vec![TestAnnotation::Only],
    );
    filter_by_only(&mut plan);
    assert_eq!(plan.total_tests, 2);
  }

  #[test]
  fn forbid_only_error_message_format() {
    let plan = make_plan(
      vec![dummy_test("focused", vec![TestAnnotation::Only])],
      vec![],
    );
    let err = check_forbid_only(&plan).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("test.only() found in 1 test(s)"));
    assert!(msg.contains("focused"));
  }

  #[test]
  fn filter_by_rerun_keeps_matching_tests() {
    let dir = std::env::temp_dir().join("ferritest_rerun_test");
    std::fs::create_dir_all(&dir).unwrap();
    let rerun_path = dir.join("@rerun.txt");
    std::fs::write(&rerun_path, "test.rs:10\n").unwrap();

    let mut plan = make_plan(
      vec![
        {
          let mut t = dummy_test("match", vec![]);
          t.id.line = Some(10);
          t
        },
        dummy_test("nomatch", vec![]),
      ],
      vec![],
    );
    filter_by_rerun(&mut plan, &rerun_path);
    assert_eq!(plan.total_tests, 1);
    assert_eq!(plan.suites[0].tests[0].id.name, "match");

    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn filter_by_rerun_missing_file_keeps_all() {
    let mut plan = make_plan(
      vec![dummy_test("test1", vec![]), dummy_test("test2", vec![])],
      vec![],
    );
    filter_by_rerun(&mut plan, Path::new("/nonexistent/@rerun.txt"));
    assert_eq!(plan.total_tests, 2);
  }

  #[test]
  fn filter_by_rerun_empty_file_keeps_all() {
    let dir = std::env::temp_dir().join("ferritest_rerun_empty");
    std::fs::create_dir_all(&dir).unwrap();
    let rerun_path = dir.join("@rerun.txt");
    std::fs::write(&rerun_path, "  \n").unwrap();

    let mut plan = make_plan(
      vec![dummy_test("test1", vec![]), dummy_test("test2", vec![])],
      vec![],
    );
    filter_by_rerun(&mut plan, &rerun_path);
    assert_eq!(plan.total_tests, 2);

    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn filter_by_rerun_matches_full_name() {
    let dir = std::env::temp_dir().join("ferritest_rerun_fullname");
    std::fs::create_dir_all(&dir).unwrap();
    let rerun_path = dir.join("@rerun.txt");
    std::fs::write(&rerun_path, "test.rs > suite > focused\n").unwrap();

    let mut plan = make_plan(
      vec![
        dummy_test("focused", vec![]),
        dummy_test("other", vec![]),
      ],
      vec![],
    );
    filter_by_rerun(&mut plan, &rerun_path);
    assert_eq!(plan.total_tests, 1);
    assert_eq!(plan.suites[0].tests[0].id.name, "focused");

    std::fs::remove_dir_all(&dir).ok();
  }
}
