//! `--dry-run` and `--list`: match every step against the registry without
//! running any of them.
//!
//! A dry-run scenario is a test like any other, scheduled by the same
//! runner and reported by the same reporters, but its body only reports
//! each step: a matched step as skipped, an undefined one as pending (or a
//! failure under `--strict`), an ambiguous one as a failure. It requests no
//! browser fixture and its feature runs no hooks, so nothing launches.

use std::sync::Arc;

use rustc_hash::FxHashMap;

use ferridriver_test::config::TestConfig;
use ferridriver_test::listing::StepIssue;
use ferridriver_test::model::{
  ExpectedStatus, Hooks, StepCategory, StepLocation, SuiteMode, TestCase, TestFailure, TestFn, TestId, TestInfo,
  TestPlan, TestSuite,
};

use crate::feature::FeatureSet;
use crate::registry::StepRegistry;
use crate::scenario::{self, ScenarioExecution};
use crate::step::MatchError;

/// The undefined and ambiguous steps of every scenario a dry run covers,
/// keyed by the scenario's id before anchoring (see [`issue_key`]).
pub type StepIssues = FxHashMap<TestId, Vec<StepIssue>>;

/// The key a test's issues are filed under: its id without the parts a
/// plan adds later (the anchored path, the `repeatEach` index).
#[must_use]
pub fn issue_key(id: &TestId) -> TestId {
  TestId {
    root_path: None,
    repeat_each_index: 0,
    ..id.clone()
  }
}

/// Every step of `scenario` no definition matches, or several do.
///
/// `interpolate` substitutes the outline row's `$name` values first, as the
/// Rust step executor does before matching; the JS executor matches the
/// text as written.
#[must_use]
pub fn check_scenario(registry: &StepRegistry, scenario: &ScenarioExecution, interpolate: bool) -> Vec<StepIssue> {
  scenario
    .steps
    .iter()
    .filter_map(|step| {
      let text = if interpolate {
        substitute(&step.text, scenario.example_values.as_ref())
      } else {
        step.text.clone()
      };
      let (kind, message) = match registry.find_match(&text) {
        Ok(_) => return None,
        Err(MatchError::Undefined { text, suggestions }) => (
          "undefined",
          crate::translate::undefined_step_message(step, &text, &suggestions),
        ),
        Err(MatchError::Ambiguous {
          text,
          matches,
          expressions,
        }) => (
          "ambiguous",
          crate::translate::ambiguous_step_message(&text, &matches, &expressions),
        ),
      };
      Some(StepIssue {
        kind: kind.to_string(),
        keyword: step.keyword.clone(),
        text,
        line: step.line,
        message,
      })
    })
    .collect()
}

/// `$name` replaced by the row's value, the subset of
/// `BrowserWorld::interpolate` a scenario's own data decides.
fn substitute(text: &str, values: Option<&FxHashMap<String, String>>) -> String {
  let Some(values) = values.filter(|v| !v.is_empty()) else {
    return text.to_string();
  };
  let mut out = String::with_capacity(text.len());
  let mut chars = text.chars().peekable();
  while let Some(c) = chars.next() {
    if c != '$' {
      out.push(c);
      continue;
    }
    if chars.peek() == Some(&'$') {
      chars.next();
      out.push('$');
      continue;
    }
    let mut name = String::new();
    while let Some(&next) = chars.peek() {
      if next.is_alphanumeric() || next == '_' {
        name.push(next);
        chars.next();
      } else {
        break;
      }
    }
    match values.get(&name) {
      Some(value) => out.push_str(value),
      None => {
        out.push('$');
        out.push_str(&name);
      },
    }
  }
  out
}

/// The dry-run plan of `feature_set`: the same suites, ids, annotations
/// and serial modes a run would have, with bodies that only report steps.
#[must_use]
pub fn translate(
  feature_set: &FeatureSet,
  registry: &StepRegistry,
  config: &TestConfig,
  options: &scenario::ExpandOptions,
  interpolate: bool,
) -> (TestPlan, StepIssues) {
  let mut suites = Vec::new();
  let mut issues = StepIssues::default();
  for feature in &feature_set.features {
    let scenarios = scenario::expand_feature_with(feature, options);
    if scenarios.is_empty() {
      continue;
    }
    let is_serial = scenarios.iter().any(|s| s.tags.iter().any(|t| t == "@serial"));
    let mut tests = Vec::with_capacity(scenarios.len());
    for scenario in scenarios {
      let id = crate::translate::scenario_id(&scenario, crate::translate::scenario_line(&scenario));
      let found = check_scenario(registry, &scenario, interpolate);
      if !found.is_empty() {
        issues.insert(issue_key(&id), found.clone());
      }
      tests.push(TestCase {
        annotations: crate::translate::scenario_annotations(&scenario),
        use_options: crate::translate::scenario_use_options(&scenario),
        metadata: serde_json::to_value(&scenario.source).ok(),
        id,
        test_fn: dry_run_fn(Arc::new(scenario), Arc::new(found), config.strict),
        fixture_requests: vec!["test_info".to_string()],
        timeout: None,
        retries: None,
        expected_status: ExpectedStatus::Pass,
      });
    }
    suites.push(TestSuite {
      name: feature.feature.name.clone(),
      file: feature.path.display().to_string(),
      tests,
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: if is_serial {
        SuiteMode::Serial
      } else {
        SuiteMode::Parallel
      },
      serial_scope: None,
    });
  }
  let total_tests = suites.iter().map(|s| s.tests.len()).sum();
  (
    TestPlan {
      repetitions_expanded: false,
      suites,
      total_tests,
      shard: None,
    },
    issues,
  )
}

/// A body that reports `scenario`'s steps and runs none: matched steps as
/// skipped, the first problem as pending (undefined, not strict) or a
/// failure, and every step after a failure as skipped.
fn dry_run_fn(scenario: Arc<ScenarioExecution>, issues: Arc<Vec<StepIssue>>, strict: bool) -> TestFn {
  Arc::new(move |pool| {
    let scenario = Arc::clone(&scenario);
    let issues = Arc::clone(&issues);
    Box::pin(async move {
      let test_info: Arc<TestInfo> = pool
        .get("test_info")
        .await
        .map_err(|e| TestFailure::wrap("fixture 'test_info' failed", e))?;
      let feature_path = scenario.feature_path.display().to_string();
      let mut failure: Option<String> = None;
      for step in &scenario.steps {
        let title = format!("{}{}", step.keyword, step.text);
        let location = StepLocation::new(feature_path.clone(), u32::try_from(step.line).unwrap_or(0));
        let mut handle = test_info
          .begin_step_at(&title, StepCategory::TestStep, Some(location))
          .await;
        handle.metadata = Some(serde_json::json!({
          "bdd_keyword": step.keyword.trim(),
          "bdd_text": step.text,
          "bdd_line": step.line,
          "bdd_dry_run": true,
        }));
        let issue = issues.iter().find(|issue| issue.line == step.line);
        match issue {
          _ if failure.is_some() => handle.skip(Some("skipped due to previous failure".into())).await,
          Some(issue) if issue.kind == "undefined" && !strict => handle.pending(Some(issue.message.clone())).await,
          Some(issue) => {
            handle.end(Some(issue.message.clone())).await;
            failure = Some(issue.message.clone());
          },
          None => handle.skip(None).await,
        }
      }
      match failure {
        Some(message) => Err(TestFailure::from(message)),
        None => Ok(()),
      }
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn row_values_replace_their_names_only() {
    let values: FxHashMap<String, String> = [("user".to_string(), "sashoush".to_string())].into_iter().collect();
    assert_eq!(
      substitute("$user signs in with $password and $$5", Some(&values)),
      "sashoush signs in with $password and $5"
    );
    assert_eq!(substitute("$user", None), "$user");
  }
}
