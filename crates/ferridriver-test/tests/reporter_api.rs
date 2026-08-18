#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The reporter API lowering: the tree `onBegin` hands a reporter, and
//! the `printsToStdio` rule that decides whether a run is silent.

use std::sync::Arc;

use ferridriver_test::config::{ProjectConfig, ReporterConfig, TestConfig};
use ferridriver_test::model::{ExpectedStatus, Hooks, TestAnnotation, TestCase, TestId, TestPlan, TestSuite};
use ferridriver_test::reporter::{ReporterMode, api, create_reporters_mode, create_reporters_pub};

fn case(file: &str, describe: Option<&str>, name: &str) -> TestCase {
  TestCase {
    id: TestId {
      file: file.to_string(),
      suite: describe.map(|d| format!("{file}::{d}")),
      name: name.to_string(),
      line: Some(7),
      column: Some(1),
    },
    test_fn: Arc::new(|_| Box::pin(async { Ok(()) })),
    fixture_requests: Vec::new(),
    annotations: vec![TestAnnotation::Tag("smoke".to_string())],
    timeout: None,
    retries: None,
    expected_status: ExpectedStatus::Pass,
    use_options: None,
  }
}

fn plan() -> TestPlan {
  TestPlan {
    suites: vec![
      TestSuite {
        name: "Checkout".to_string(),
        file: "tests/pay.spec.ts".to_string(),
        tests: vec![case("tests/pay.spec.ts", Some("Checkout"), "adds a row")],
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: ferridriver_test::model::SuiteMode::default(),
      },
      TestSuite {
        name: "tests/pay.spec.ts".to_string(),
        file: "tests/pay.spec.ts".to_string(),
        tests: vec![case("tests/pay.spec.ts", None, "loads")],
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: ferridriver_test::model::SuiteMode::default(),
      },
    ],
    total_tests: 2,
    shard: None,
  }
}

#[test]
fn the_tree_is_root_project_file_describe() {
  let config = TestConfig {
    test_dir: Some("tests".to_string()),
    ..TestConfig::default()
  };
  let project = ProjectConfig {
    name: "chromium".to_string(),
    ..ProjectConfig::default()
  };
  let plan = plan();
  let preamble = api::RunPreamble::build(
    &config,
    &[api::ProjectPlan {
      name: "chromium",
      config: &config,
      project: Some(&project),
      plan: &plan,
    }],
  );

  let root = &preamble.suite;
  assert_eq!(root.kind, api::SuiteKind::Root);
  assert_eq!(root.title, "");
  assert_eq!(root.suites.len(), 1, "one project");

  let project_suite = &root.suites[0];
  assert_eq!(project_suite.kind, api::SuiteKind::Project);
  assert_eq!(project_suite.title, "chromium");
  assert_eq!(project_suite.title_path, vec![String::new(), "chromium".to_string()]);
  assert_eq!(
    project_suite.project.as_ref().and_then(|p| p["name"].as_str()),
    Some("chromium"),
    "a project suite answers `project()` with its FullProject",
  );

  let file = &project_suite.suites[0];
  assert_eq!(file.kind, api::SuiteKind::File);
  assert_eq!(file.title, "tests/pay.spec.ts");
  assert_eq!(
    file.location.as_ref().map(|l| l.file.as_str()),
    Some("pay.spec.ts"),
    "locations are relative to the root dir",
  );

  // A `describe` is a child of its file, not a sibling — the plan
  // carries it as a flat suite, which is not the shape a reporter wants.
  let describe = &file.suites[0];
  assert_eq!(describe.kind, api::SuiteKind::Describe);
  assert_eq!(describe.title, "Checkout");
  assert_eq!(describe.tests.len(), 1);
  assert_eq!(file.tests.len(), 1, "the loose test stays on the file");

  let nested = &describe.tests[0];
  assert_eq!(nested.title, "adds a row");
  assert_eq!(
    nested.title_path,
    vec![
      String::new(),
      "chromium".to_string(),
      "tests/pay.spec.ts".to_string(),
      "Checkout".to_string(),
      "adds a row".to_string(),
    ],
  );
  assert_eq!(nested.tags, vec!["@smoke".to_string()], "tags carry their `@`");
  assert_eq!(
    nested.id,
    nested_id(),
    "the case id is the one every reporter and the HTML report key by",
  );

  assert_eq!(root.all_cases().len(), 2);
}

fn nested_id() -> String {
  TestId {
    file: "tests/pay.spec.ts".to_string(),
    suite: Some("tests/pay.spec.ts::Checkout".to_string()),
    name: "adds a row".to_string(),
    line: Some(7),
    column: Some(1),
  }
  .stable_id("chromium")
}

#[test]
fn merging_shards_unions_their_trees() {
  let config = TestConfig {
    test_dir: Some("tests".to_string()),
    ..TestConfig::default()
  };
  let build = |name: &str| {
    let plan = TestPlan {
      suites: vec![TestSuite {
        name: "tests/pay.spec.ts".to_string(),
        file: "tests/pay.spec.ts".to_string(),
        tests: vec![case("tests/pay.spec.ts", None, name)],
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: ferridriver_test::model::SuiteMode::default(),
      }],
      total_tests: 1,
      shard: None,
    };
    api::RunPreamble::build(
      &config,
      &[api::ProjectPlan {
        name: "chromium",
        config: &config,
        project: None,
        plan: &plan,
      }],
    )
  };
  let mut first = build("loads");
  first.merge_from(build("saves"));
  first.merge_from(build("loads"));

  let file = &first.suite.suites[0].suites[0];
  let titles: Vec<&str> = file.tests.iter().map(|c| c.title.as_str()).collect();
  assert_eq!(titles, vec!["loads", "saves"], "one entry per case, no duplicates");
}

#[test]
fn a_run_that_would_be_silent_gains_a_terminal_reporter() {
  let config = TestConfig::default();
  let file_only = [ReporterConfig {
    name: "json".to_string(),
    options: std::collections::BTreeMap::new(),
  }];
  let reporters = create_reporters_pub(&file_only, &config);
  assert!(
    reporters.prints_to_stdio(),
    "nothing in `json` + `rerun` writes to the terminal, so one is put in front",
  );

  // Merging shards prints its own summary; Playwright does not add one
  // there and neither do we.
  let merged = create_reporters_mode(&file_only, &config, ReporterMode::Merge);
  assert!(!merged.prints_to_stdio(), "merge mode gains no terminal reporter");

  let terminal = [ReporterConfig {
    name: "list".to_string(),
    options: std::collections::BTreeMap::new(),
  }];
  let already = create_reporters_pub(&terminal, &config);
  assert!(already.prints_to_stdio());
}

#[test]
fn the_preamble_round_trips_through_json() {
  let config = TestConfig {
    test_dir: Some("tests".to_string()),
    ..TestConfig::default()
  };
  let plan = plan();
  let preamble = api::RunPreamble::build(
    &config,
    &[api::ProjectPlan {
      name: "chromium",
      config: &config,
      project: None,
      plan: &plan,
    }],
  );
  let text = serde_json::to_string(&preamble).expect("serialize");
  let back: api::RunPreamble = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
  assert_eq!(back.suite.all_cases().len(), 2);
}

/// The preamble travels on the blob wire inside an internally-tagged
/// enum, and serde buffers such a map through `deserialize_any`. Under
/// `serde_json/arbitrary_precision` — which a transitive dependency
/// force-enables workspace-wide — that turns a FLOAT into a map with a
/// private key, and a float field then fails to read back with
/// `invalid type: map, expected f64`. Integers are unaffected, so the
/// rule is simply that the preamble carries no float.
#[test]
fn the_preamble_carries_no_float() {
  fn walk(value: &serde_json::Value, path: &str, floats: &mut Vec<String>) {
    match value {
      serde_json::Value::Number(number) if number.as_i64().is_none() && number.as_u64().is_none() => {
        floats.push(format!("{path} = {number}"));
      },
      serde_json::Value::Array(items) => {
        for (i, item) in items.iter().enumerate() {
          walk(item, &format!("{path}[{i}]"), floats);
        }
      },
      serde_json::Value::Object(map) => {
        for (key, item) in map {
          walk(item, &format!("{path}.{key}"), floats);
        }
      },
      _ => {},
    }
  }

  let config = TestConfig {
    test_dir: Some("tests".to_string()),
    timeout: 30_000,
    ..TestConfig::default()
  };
  let plan = plan();
  let preamble = api::RunPreamble::build(
    &config,
    &[api::ProjectPlan {
      name: "chromium",
      config: &config,
      project: None,
      plan: &plan,
    }],
  );
  let value = serde_json::to_value(&preamble).expect("to_value");
  let mut floats = Vec::new();
  walk(&value, "preamble", &mut floats);
  assert!(floats.is_empty(), "the blob wire cannot read these back: {floats:?}");
}
