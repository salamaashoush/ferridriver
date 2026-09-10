use std::sync::Arc;

use anyhow::Result;
use ferridriver_test::config::{ProjectConfig, ReporterConfig, TestConfig};
use ferridriver_test::model::{ExpectedStatus, Hooks, TestAnnotation, TestCase, TestId, TestPlan, TestSuite};
use ferridriver_test::reporter::{ReporterMode, api, create_reporters_mode, create_reporters_pub};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Request {
  Preamble {
    config: Box<TestConfig>,
    project: Option<Box<ProjectConfig>>,
    project_name: String,
    groups: Vec<Vec<Case>>,
  },
  Factory {
    reporters: Vec<ReporterConfig>,
    merge: bool,
  },
}

#[derive(Deserialize)]
pub struct Case {
  file: String,
  describe: Option<String>,
  name: String,
  line: usize,
  column: usize,
  tags: Vec<String>,
}

fn case(spec: Case) -> TestCase {
  TestCase {
    metadata: None,
    id: TestId {
      suite: spec.describe.map(|describe| format!("{}::{describe}", spec.file)),
      file: spec.file,
      name: spec.name,
      line: Some(spec.line),
      column: Some(spec.column),
    },
    test_fn: Arc::new(|_| Box::pin(async { Ok(()) })),
    fixture_requests: Vec::new(),
    annotations: spec.tags.into_iter().map(TestAnnotation::Tag).collect(),
    timeout: None,
    retries: None,
    expected_status: ExpectedStatus::Pass,
    use_options: None,
  }
}

fn plan(specs: Vec<Case>) -> TestPlan {
  let total_tests = specs.len();
  let suites = specs
    .into_iter()
    .map(|spec| {
      let name = spec.describe.clone().unwrap_or_else(|| spec.file.clone());
      TestSuite {
        name,
        file: spec.file.clone(),
        tests: vec![case(spec)],
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: ferridriver_test::model::SuiteMode::default(),
      }
    })
    .collect();
  TestPlan {
    suites,
    total_tests,
    shard: None,
  }
}

fn preamble(config: &TestConfig, project: Option<&ProjectConfig>, name: &str, groups: Vec<Vec<Case>>) -> Result<Value> {
  let mut merged: Option<api::RunPreamble> = None;
  let mut ids = Vec::new();
  for specs in groups {
    let plan = plan(specs);
    ids.extend(
      plan
        .suites
        .iter()
        .flat_map(|suite| suite.tests.iter().map(|case| case.id.stable_id(name))),
    );
    let next = api::RunPreamble::build(
      config,
      &[api::ProjectPlan {
        name,
        config,
        project,
        plan: &plan,
      }],
    );
    match &mut merged {
      Some(first) => first.merge_from(next),
      None => merged = Some(next),
    }
  }
  let merged = merged.unwrap_or_else(api::RunPreamble::empty);
  let text = serde_json::to_string(&merged)?;
  let back: api::RunPreamble = serde_json::from_str(&text)?;
  Ok(
    json!({ "preamble": merged, "text": text, "roundTripCases": back.suite.all_cases().len(),
    "caseIds": ids, "totalCases": merged.suite.all_cases().len() }),
  )
}

pub fn run(request: Request) -> Result<Value> {
  match request {
    Request::Preamble {
      config,
      project,
      project_name,
      groups,
    } => preamble(&config, project.as_deref(), &project_name, groups),
    Request::Factory { reporters, merge } => {
      let config = TestConfig::default();
      let reporters = if merge {
        create_reporters_mode(&reporters, &config, ReporterMode::Merge)
      } else {
        create_reporters_pub(&reporters, &config)
      };
      Ok(json!({ "printsToStdio": reporters.prints_to_stdio() }))
    },
  }
}
