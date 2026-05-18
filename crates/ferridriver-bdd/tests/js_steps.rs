//! `.js` step definitions + a `.feature` executed through the
//! `ferridriver-bdd` core on the shared `ferridriver-script` QuickJS
//! engine: passing steps, a failing step that reports its JavaScript
//! source location, a data table, a scenario outline, and tag
//! filtering.
//!
//! Requires a Chromium binary (as the rest of the suite does) to
//! construct the per-scenario World fixtures; these step bodies are
//! pure and do not touch the page.

use std::path::PathBuf;
use std::sync::Arc;

use ferridriver_bdd::feature::FeatureSet;
use ferridriver_bdd::filter::{TagExpression, filter_scenarios};
use ferridriver_bdd::js::{JsBddSession, JsScenarioResult, JsStepStatus};
use ferridriver_bdd::scenario::expand_feature;
use ferridriver_bdd::world::BrowserWorld;

async fn build_world() -> BrowserWorld {
  let browser = Arc::new(
    ferridriver::chromium()
      .launch(ferridriver::options::LaunchOptions {
        headless: Some(true),
        ..Default::default()
      })
      .await
      .expect("launch chromium"),
  );
  let context = Arc::new(browser.new_context(None));
  let page = context.new_page().await.expect("new page");
  let request = Arc::new(ferridriver::api_request::APIRequestContext::new(Default::default()));
  let fixtures = ferridriver_test::model::TestFixtures {
    browser,
    page,
    context,
    request,
    test_info: Arc::new(ferridriver_test::model::TestInfo::new_anonymous()),
    modifiers: Arc::new(ferridriver_test::model::TestModifiers::default()),
    browser_config: ferridriver_test::config::BrowserConfig::default(),
    bdd_args: None,
    bdd_data_table: None,
    bdd_doc_string: None,
  };
  BrowserWorld::new(fixtures)
}

#[tokio::test]
async fn js_steps_pass_fail_and_tag_filter() {
  let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
  let feature_src = std::fs::read_to_string(fixtures_dir.join("cukes.feature")).expect("read feature");

  // 1. Load the JS step files (as ES modules) into the shared QuickJS
  //    engine and build the Rust step registry from what they
  //    registered.
  let session = JsBddSession::from_globs(&["cukes.steps.js".to_string()], &fixtures_dir)
    .await
    .expect("load js bdd session");

  // 2. Parse + expand + tag-filter via the real ferridriver-bdd core.
  let feature_set = FeatureSet::parse_text(&feature_src).expect("parse feature");
  let parsed = &feature_set.features[0];
  let mut scenarios = expand_feature(parsed);
  let tag_expr = TagExpression::parse("@smoke and not @wip").expect("tag expr");
  filter_scenarios(&mut scenarios, &tag_expr);

  // 3. Drive each scenario through the QuickJS step functions.
  //    (BeforeAll hooks ran during JsBddSession::load.)
  let mut world = build_world().await;

  let mut results: Vec<JsScenarioResult> = Vec::new();
  for scenario in &scenarios {
    world.reset_scenario_state();
    results.push(session.run_scenario(scenario, &mut world).await);
  }
  session.after_all().await.expect("afterAll");

  eprintln!("\n=== JS step results ===");
  for r in &results {
    eprintln!("Scenario: {}  [{}]", r.name, if r.passed { "PASS" } else { "FAIL" });
    for s in &r.steps {
      eprintln!("  {} {}  -> {:?}", s.keyword, s.text, s.status);
    }
  }

  // ---- Assertions ----

  // tag-filter: the @wip scenario must not have run.
  assert!(
    !results.iter().any(|r| r.name.contains("excluded")),
    "tag filter must exclude @wip scenario"
  );
  // 4 scenarios survive the filter (eat / data table / 2 outline rows / failing).
  assert_eq!(results.len(), 5, "expected 5 @smoke scenarios, got {}", results.len());

  let pass = |name: &str| {
    results
      .iter()
      .find(|r| r.name == name || r.name.starts_with(name))
      .unwrap_or_else(|| panic!("scenario {name} missing"))
  };

  // pass: plain scenario
  assert!(pass("eat some cukes").passed, "eat some cukes should pass");
  // pass: data table reached JS as a cucumber DataTable
  assert!(pass("data table sum").passed, "data table scenario should pass");
  // pass: both scenario-outline rows
  let outline: Vec<&JsScenarioResult> = results.iter().filter(|r| r.name.starts_with("outline math")).collect();
  assert_eq!(outline.len(), 2, "outline expands to 2 rows");
  assert!(outline.iter().all(|r| r.passed), "both outline rows should pass");

  // fail: error message carries the JS source location.
  let failing = pass("deliberately failing");
  assert!(!failing.passed, "failing scenario must fail");
  let failed_step = failing
    .steps
    .iter()
    .find(|s| matches!(s.status, JsStepStatus::Failed(_)))
    .expect("a failed step");
  let JsStepStatus::Failed(msg) = &failed_step.status else {
    unreachable!()
  };
  assert!(msg.contains("boom from js step"), "JS throw message propagated: {msg}");
  assert!(
    msg.contains("<js-step>:") || msg.contains("    at "),
    "failure carries the JS source location: {msg}"
  );
}
