//! `--list`: what a run would execute, without executing it.
//!
//! The JSON form is one document, `{ "schema", "tests", "errors" }`, with a
//! record per test as it would be scheduled (one per project, one per
//! `repeatEach` index). It is meant for a program that plans the run
//! elsewhere: every field it needs to select (`stableId`), place
//! (`groupKey`) and budget (`timeout`, `retries`) a test is here, and every
//! path is relative to the config root, so two machines list the same
//! suite identically.

use serde::{Deserialize, Serialize};

use crate::config::TestConfig;
use crate::model::{ExpectedStatus, TestAnnotation, TestId, TestPlan};

/// Bumped when a field changes meaning or disappears; new fields do not
/// bump it.
pub const LIST_SCHEMA: u32 = 1;

/// One test as `--list --format json` reports it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ListRecord {
  /// What `--test-id`, `--test-list` and `--shard-assignment` select by.
  pub stable_id: String,
  /// The project the test runs in; empty when the config declares none.
  pub project: String,
  /// The spec or feature file, relative to the config root.
  pub file: String,
  pub line: Option<usize>,
  pub column: Option<usize>,
  /// The file, each enclosing `describe` (or feature, rule and outline),
  /// then the test's own title.
  pub title_path: Vec<String>,
  pub tags: Vec<String>,
  pub annotations: Vec<ListAnnotation>,
  /// Retries this test gets after a failure.
  pub retries: u32,
  /// Per-attempt timeout in milliseconds, before `test.slow()` triples it.
  pub timeout: u64,
  /// The serial group that has to run whole, in order, on one worker;
  /// `null` for a test that schedules on its own.
  pub group_key: Option<String>,
  pub repeat_each_index: u32,
  /// Undefined or ambiguous steps of a BDD scenario.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub step_issues: Vec<StepIssue>,
}

/// A static annotation: `skip`, `fixme`, `fail`, `slow`, `only`, `lock`, or
/// an `{ type, description }` pair the test declared.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListAnnotation {
  #[serde(rename = "type")]
  pub kind: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
  /// The platform or browser condition a conditional annotation applies
  /// under (`@skip(firefox)`).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub condition: Option<String>,
}

/// A BDD step no definition matches, or more than one does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepIssue {
  /// `undefined` or `ambiguous`.
  pub kind: String,
  pub keyword: String,
  pub text: String,
  pub line: usize,
  pub message: String,
}

/// A problem that belongs to the listing rather than one test.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ListError {
  pub message: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub stable_id: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub file: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub line: Option<usize>,
}

/// The whole `--list --format json` document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListReport {
  pub schema: u32,
  pub tests: Vec<ListRecord>,
  pub errors: Vec<ListError>,
}

/// A listed test, with the id it was built from so a front-end can attach
/// what only it knows (a scenario's step issues).
#[derive(Debug, Clone)]
pub struct ListedTest {
  pub id: TestId,
  pub record: ListRecord,
}

/// Every test of `plan` (already narrowed and filtered for `project`) as
/// a listing record.
#[must_use]
pub fn list_plan(plan: &TestPlan, project: &str, config: &TestConfig) -> Vec<ListedTest> {
  let mut listed = Vec::with_capacity(plan.total_tests);
  for suite in &plan.suites {
    let group_key = suite.group_key(project);
    for test in &suite.tests {
      let mut title_path = test.id.title_path();
      if let Some(first) = title_path.first_mut() {
        *first = test.id.anchored_file().to_string();
      }
      let tags = test
        .annotations
        .iter()
        .filter_map(|a| match a {
          TestAnnotation::Tag(tag) => Some(tag.clone()),
          _ => None,
        })
        .collect();
      let mut annotations: Vec<ListAnnotation> = test.annotations.iter().filter_map(list_annotation).collect();
      if test.expected_status == ExpectedStatus::Fail && !annotations.iter().any(|a| a.kind == "fail") {
        annotations.push(ListAnnotation {
          kind: "fail".into(),
          description: None,
          condition: None,
        });
      }
      let timeout = test
        .timeout
        .map_or(config.timeout, |t| u64::try_from(t.as_millis()).unwrap_or(u64::MAX));
      listed.push(ListedTest {
        id: test.id.clone(),
        record: ListRecord {
          stable_id: test.id.stable_id(project),
          project: project.to_string(),
          file: test.id.anchored_file().to_string(),
          line: test.id.line,
          column: test.id.column,
          title_path,
          tags,
          annotations,
          retries: test.retries.unwrap_or(config.retries),
          timeout,
          group_key: group_key.clone(),
          repeat_each_index: test.id.repeat_each_index,
          step_issues: Vec::new(),
        },
      });
    }
  }
  // Plan order is a scheduling detail; a reader (and a diff of two
  // listings) wants declaration order.
  listed.sort_by(|a, b| {
    (&a.record.file, a.record.line, a.record.repeat_each_index).cmp(&(
      &b.record.file,
      b.record.line,
      b.record.repeat_each_index,
    ))
  });
  listed
}

fn list_annotation(annotation: &TestAnnotation) -> Option<ListAnnotation> {
  let plain = |kind: &str, reason: &Option<String>, condition: &Option<String>| ListAnnotation {
    kind: kind.to_string(),
    description: reason.clone(),
    condition: condition.clone(),
  };
  Some(match annotation {
    TestAnnotation::Tag(_) => return None,
    TestAnnotation::Skip { reason, condition } => plain("skip", reason, condition),
    TestAnnotation::Fixme { reason, condition } => plain("fixme", reason, condition),
    TestAnnotation::Fail { reason, condition } => plain("fail", reason, condition),
    TestAnnotation::Slow { reason, condition } => plain("slow", reason, condition),
    TestAnnotation::Only => plain("only", &None, &None),
    TestAnnotation::Lock(name) => plain("lock", &Some(name.clone()), &None),
    TestAnnotation::Info { type_name, description } => ListAnnotation {
      kind: type_name.clone(),
      description: Some(description.clone()).filter(|d| !d.is_empty()),
      condition: None,
    },
  })
}

/// The listing document for `listed` plus problems that belong to no one
/// test.
#[must_use]
pub fn report(listed: &[ListedTest], errors: &[ListError]) -> ListReport {
  ListReport {
    schema: LIST_SCHEMA,
    tests: listed.iter().map(|test| test.record.clone()).collect(),
    errors: errors.to_vec(),
  }
}

/// Print the listing: the JSON document on stdout, or the text form, one
/// test per line (`[project] ` in front when there is one) and a count.
pub fn print(listed: &[ListedTest], errors: &[ListError], json: bool) {
  if json {
    match serde_json::to_string_pretty(&report(listed, errors)) {
      Ok(text) => println!("{text}"),
      Err(e) => eprintln!("Error: could not serialize the listing: {e}"),
    }
    return;
  }
  for test in listed {
    let project = &test.record.project;
    if project.is_empty() {
      println!("  {}", test.id.full_name());
    } else {
      println!("  [{project}] {}", test.id.full_name());
    }
    for issue in &test.record.step_issues {
      println!(
        "      {} step at line {}: {}{}",
        issue.kind, issue.line, issue.keyword, issue.text
      );
    }
  }
  println!("\n  {} test(s) found", listed.len());
  for error in errors {
    eprintln!("Error: {}", error.message);
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{Hooks, SuiteMode, TestCase, TestSuite};

  fn case(name: &str, line: usize, annotations: Vec<TestAnnotation>) -> TestCase {
    TestCase {
      id: TestId {
        file: "tests/cart.spec.ts".into(),
        suite: Some("tests/cart.spec.ts::checkout".into()),
        name: name.into(),
        line: Some(line),
        column: Some(3),
        root_path: Some("tests/cart.spec.ts".into()),
        ..TestId::default()
      },
      test_fn: std::sync::Arc::new(|_| Box::pin(async { Ok(()) })),
      fixture_requests: Vec::new(),
      annotations,
      timeout: None,
      retries: Some(2),
      expected_status: ExpectedStatus::Fail,
      use_options: None,
      metadata: None,
    }
  }

  #[test]
  fn a_serial_test_lists_its_group_tags_and_annotations() {
    let plan = TestPlan {
      suites: vec![TestSuite {
        name: "checkout".into(),
        file: "tests/cart.spec.ts".into(),
        tests: vec![case(
          "pays",
          12,
          vec![
            TestAnnotation::Tag("@smoke".into()),
            TestAnnotation::Slow {
              reason: None,
              condition: None,
            },
            TestAnnotation::Info {
              type_name: "issue".into(),
              description: "https://example.com/issues/1".into(),
            },
          ],
        )],
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: SuiteMode::Serial,
        serial_scope: None,
      }],
      total_tests: 1,
      ..TestPlan::default()
    };
    let config = TestConfig::default();
    let listed = list_plan(&plan, "chromium", &config);
    assert_eq!(listed.len(), 1);
    let record = &listed[0].record;
    assert_eq!(record.stable_id, plan.suites[0].tests[0].id.stable_id("chromium"));
    assert_eq!(record.title_path, ["tests/cart.spec.ts", "checkout", "pays"]);
    assert_eq!(
      record.group_key.as_deref(),
      Some("[chromium] tests/cart.spec.ts > checkout")
    );
    assert_eq!(record.tags, ["@smoke"]);
    assert_eq!(record.retries, 2);
    assert_eq!(record.timeout, config.timeout);
    let kinds: Vec<&str> = record.annotations.iter().map(|a| a.kind.as_str()).collect();
    assert_eq!(kinds, ["slow", "issue", "fail"]);

    let json = serde_json::to_value(report(&listed, &[])).expect("json");
    assert_eq!(json["schema"], 1);
    assert_eq!(json["tests"][0]["groupKey"], "[chromium] tests/cart.spec.ts > checkout");
    assert_eq!(json["tests"][0]["line"], 12);
    assert!(json["tests"][0].get("stepIssues").is_none());
  }
}
