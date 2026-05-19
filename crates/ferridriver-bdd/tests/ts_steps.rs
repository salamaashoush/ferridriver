//! TypeScript step files end-to-end on the shared QuickJS engine:
//! rolldown transpiles `.ts` (types stripped), resolves a sibling `.ts`
//! import, and tree-shakes an unused export out of the bundle.

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
  let context = Arc::new(browser.new_context(None));
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
async fn typescript_steps_run_and_unused_export_is_tree_shaken() {
  let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ts");

  // 1. Tree-shaking: bundle the .ts entry; the used helper survives,
  //    the never-imported export (+ its marker) is dropped.
  let entry = dir.join("steps.ts");
  let (code, _map) = ferridriver_script::bundle_source(&[entry], &dir)
    .await
    .expect("rolldown bundle .ts");
  assert!(
    !code.contains("TREE_SHAKE_ME_AWAY_MARKER_9F3A"),
    "unused export must be tree-shaken out of the bundle:\n{code}"
  );
  assert!(code.contains("add"), "used helper must remain in the bundle");
  // Types must be stripped (no `: number` / `interface` in output JS).
  assert!(
    !code.contains("interface Wallet"),
    "TypeScript types must be transpiled away:\n{code}"
  );

  // 2. End-to-end: the .ts steps run through the core via QuickJS.
  let session = JsBddSession::from_globs(&["steps.ts".to_string()], &dir)
    .await
    .expect("load ts bdd session");

  let feature_src = std::fs::read_to_string(dir.join("calc.feature")).expect("read feature");
  let feature_set = FeatureSet::parse_text(&feature_src).expect("parse feature");
  let scenarios = expand_feature(&feature_set.features[0]);
  assert_eq!(scenarios.len(), 1);

  let mut world = build_world().await;
  let result = session.run_scenario(&scenarios[0], &mut world).await;

  eprintln!(
    "TS scenario: {} [{}]",
    result.name,
    if result.passed { "PASS" } else { "FAIL" }
  );
  for s in &result.steps {
    eprintln!("  {} {} -> {:?}", s.keyword, s.text, s.status);
  }
  assert!(
    result.passed,
    "TypeScript steps must pass end-to-end: {:?}",
    result.steps
  );
}
