//! TypeScript BDD steps exercising the `expect()` global end-to-end —
//! Jest value matchers, asymmetric matchers, `toThrow`, and Playwright
//! web-first matchers, all dispatched through QuickJS into the same
//! Rust `ferridriver_expect` core that the test runner uses.

use std::path::PathBuf;
use std::sync::Arc;

use ferridriver_bdd::feature::FeatureSet;
use ferridriver_bdd::js::JsBddSession;
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
  let context = Arc::new(browser.new_context().await.expect("new context"));
  let page = context.new_page().await.expect("new page");
  let request = Arc::new(ferridriver::http_client::HttpClient::new(Default::default()));
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
async fn ts_expect_steps_run_end_to_end() {
  let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ts_expect");

  let session = JsBddSession::from_globs(&["steps.ts".to_string()], &dir)
    .await
    .expect("load ts_expect bdd session");

  let feature_src = std::fs::read_to_string(dir.join("assert.feature")).expect("read assert.feature");
  let feature_set = FeatureSet::parse_text(&feature_src).expect("parse feature");
  let scenarios = expand_feature(&feature_set.features[0]);
  assert_eq!(scenarios.len(), 3, "expected three scenarios in assert.feature");

  for scenario in &scenarios {
    let mut world = build_world().await;
    let result = session.run_scenario(scenario, &mut world).await;
    eprintln!(
      "TS expect scenario: {} [{}]",
      result.name,
      if result.passed { "PASS" } else { "FAIL" }
    );
    for s in &result.steps {
      eprintln!("  {} {} -> {:?}", s.keyword, s.text, s.status);
    }
    assert!(
      result.passed,
      "scenario {:?} must pass end-to-end: {:?}",
      result.name, result.steps
    );
  }
}
