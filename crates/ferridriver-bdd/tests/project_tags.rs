#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Tag selection, at the level a run actually uses it: `[test].tags` and
//! `--tags` choosing scenarios, and a project's own `tags` giving each
//! project its own corpus over one feature set.
//!
//! Uses the Rust-step planner, so no bundler and no browser: the claim
//! under test is which scenarios end up in the plan.

use ferridriver_bdd::build_bdd_plans;
use ferridriver_test::config::{ProjectConfig, TestConfig};

const FEATURE: &str = r"Feature: Checkout

  @smoke
  Scenario: a smoke scenario
    Given something

  @wip @smoke
  Scenario: a smoke scenario that is still in progress
    Given something

  @regression
  Scenario: a regression scenario
    Given something
";

/// A feature file on disk plus a config pointed at it.
struct Fixture {
  dir: tempfile::TempDir,
}

impl Fixture {
  fn new() -> Self {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("checkout.feature"), FEATURE).expect("write feature");
    Self { dir }
  }

  fn config(&self) -> TestConfig {
    TestConfig {
      features: vec![self.dir.path().join("*.feature").display().to_string()],
      ..Default::default()
    }
  }
}

/// Every scenario name in a plan, sorted so a comparison is stable.
fn names(plan: &ferridriver_test::model::TestPlan) -> Vec<String> {
  let mut out: Vec<String> = plan
    .suites
    .iter()
    .flat_map(|s| s.tests.iter().map(|t| t.id.name.clone()))
    .collect();
  out.sort();
  out
}

#[tokio::test(flavor = "multi_thread")]
async fn no_tag_expression_keeps_every_scenario() {
  let fixture = Fixture::new();
  let (plan, per_project) = build_bdd_plans(&fixture.config(), &[], &[], None).await.expect("plan");
  assert_eq!(plan.total_tests, 3);
  assert!(per_project.is_empty(), "no projects means no per-project plans");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tag_expression_selects_the_scenarios_it_names() {
  let fixture = Fixture::new();
  let mut config = fixture.config();
  config.tags = Some("@smoke and not @wip".to_string());
  let (plan, _) = build_bdd_plans(&config, &[], &[], None).await.expect("plan");
  assert_eq!(names(&plan), vec!["a smoke scenario"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unparseable_tag_expression_fails_the_run() {
  let fixture = Fixture::new();
  let mut config = fixture.config();
  config.tags = Some("@smoke and".to_string());
  let err = match build_bdd_plans(&config, &[], &[], None).await {
    Ok(_) => panic!("an invalid expression must not quietly select everything"),
    Err(e) => e,
  };
  assert!(err.contains("invalid tag expression"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_tag_selected_projects_over_one_feature_set_run_disjoint_scenarios() {
  let fixture = Fixture::new();
  let mut config = fixture.config();
  config.projects = vec![
    ProjectConfig {
      name: "smoke".to_string(),
      tags: Some("@smoke and not @wip".to_string()),
      ..Default::default()
    },
    ProjectConfig {
      name: "regression".to_string(),
      tags: Some("@regression".to_string()),
      ..Default::default()
    },
  ];

  let (_, per_project) = build_bdd_plans(&config, &[], &[], None).await.expect("plan");

  let smoke = names(per_project.get("smoke").expect("smoke plan"));
  let regression = names(per_project.get("regression").expect("regression plan"));
  assert_eq!(smoke, vec!["a smoke scenario"]);
  assert_eq!(regression, vec!["a regression scenario"]);
  assert!(
    smoke.iter().all(|n| !regression.contains(n)),
    "the two projects must not share a scenario: {smoke:?} vs {regression:?}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_declaring_no_bdd_keys_narrows_the_shared_plan_instead() {
  // The path every existing suite is on: nothing per-project is
  // discovered, so nothing extra is built.
  let fixture = Fixture::new();
  let mut config = fixture.config();
  config.projects = vec![
    ProjectConfig {
      name: "chromium".to_string(),
      ..Default::default()
    },
    ProjectConfig {
      name: "firefox".to_string(),
      ..Default::default()
    },
  ];

  let (plan, per_project) = build_bdd_plans(&config, &[], &[], None).await.expect("plan");
  assert_eq!(plan.total_tests, 3);
  assert!(
    per_project.is_empty(),
    "a project with no features/steps/tags of its own costs no extra discovery"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_selects_its_own_feature_files() {
  let fixture = Fixture::new();
  std::fs::write(
    fixture.dir.path().join("other.feature"),
    "Feature: Other\n\n  Scenario: elsewhere\n    Given something\n",
  )
  .expect("write");

  let mut config = fixture.config();
  config.projects = vec![ProjectConfig {
    name: "other".to_string(),
    features: Some(vec![fixture.dir.path().join("other.feature").display().to_string()]),
    ..Default::default()
  }];

  let (_, per_project) = build_bdd_plans(&config, &[], &[], None).await.expect("plan");
  assert_eq!(names(per_project.get("other").expect("other plan")), vec!["elsewhere"]);
}
