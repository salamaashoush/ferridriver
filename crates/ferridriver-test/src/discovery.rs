//! Test discovery: inventory-based collection for Rust, glob-based file scanning.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::config::TestConfig;
use crate::fixture::FixturePool;
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
  let re = regex::Regex::new(pattern).ok();
  for suite in &mut plan.suites {
    suite.tests.retain(|test| {
      let full_name = test.id.full_name();
      let matches = re.as_ref().is_some_and(|r| r.is_match(&full_name));
      if invert { !matches } else { matches }
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
