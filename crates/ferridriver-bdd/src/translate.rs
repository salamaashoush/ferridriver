//! Translation layer: converts Gherkin features into ferridriver-test `TestPlan`.
//!
//! Each Feature becomes a `TestSuite`, each Scenario becomes a `TestCase`.
//! The test function for each scenario runs the BDD steps via the `StepRegistry`,
//! emitting step events through `TestInfo::begin_step()` for real-time reporting.

use std::sync::Arc;
use std::time::Duration;

use ferridriver_test::FixturePool;
use ferridriver_test::config::TestConfig;
use ferridriver_test::model::{
  ExpectedStatus, Hooks, StepCategory, SuiteMode, TestAnnotation, TestCase, TestFailure, TestFn, TestId, TestInfo,
  TestPlan, TestSuite,
};

use crate::executor::{ScenarioExecutor, StepEvent, StepObserver};
use crate::feature::FeatureSet;
use crate::hook::HookPoint;
use crate::registry::StepRegistry;
use crate::scenario::{self, ScenarioExecution, ScenarioStep, StepStatus};
use crate::step::MatchError;
use crate::world::BrowserWorld;

/// Translate parsed Gherkin features into a `TestPlan` for the core test runner.
pub fn translate_features(feature_set: &FeatureSet, registry: Arc<StepRegistry>, config: &TestConfig) -> TestPlan {
  let mut suites = Vec::new();

  for feature in &feature_set.features {
    let scenarios = scenario::expand_feature_with(
      feature,
      &scenario::ExpandOptions {
        examples_title_format: config.examples_title_format.clone(),
      },
    );
    if scenarios.is_empty() {
      continue;
    }

    let feature_name = feature.feature.name.clone();
    let feature_path = feature.path.display().to_string();
    let feature_tags = crate::feature::extract_tags(&feature.feature.tags);

    // @serial tag on any scenario means the whole feature runs serially.
    let is_serial = scenarios.iter().any(|s| s.tags.iter().any(|t| t == "@serial"));

    let test_cases: Vec<TestCase> = scenarios
      .into_iter()
      .map(|s| translate_scenario(s, Arc::clone(&registry), config))
      .collect();

    suites.push(TestSuite {
      name: feature_name,
      file: feature_path,
      tests: test_cases,
      hooks: build_feature_hooks(Arc::clone(&registry), feature_tags, config),
      annotations: Vec::new(),
      mode: if is_serial {
        SuiteMode::Serial
      } else {
        SuiteMode::Parallel
      },
    });
  }

  // Apply scenario ordering.
  if config.order.starts_with("random") {
    let seed: u64 = if let Some(seed_str) = config.order.strip_prefix("random:") {
      seed_str.parse().unwrap_or_else(|_| {
        // Hash the seed string if it's not a number.
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        seed_str.hash(&mut hasher);
        hasher.finish()
      })
    } else {
      // Use current time as seed when no explicit seed given.
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(42)
    };

    tracing::info!("shuffling scenarios with seed {seed}");

    for suite in &mut suites {
      fisher_yates_shuffle(&mut suite.tests, seed);
    }
  }

  let total_tests = suites.iter().map(|s| s.tests.len()).sum();
  TestPlan {
    suites,
    total_tests,
    shard: None,
  }
}

fn build_feature_hooks(registry: Arc<StepRegistry>, feature_tags: Vec<String>, config: &TestConfig) -> Hooks {
  let before_registry = Arc::clone(&registry);
  let before_tags = feature_tags.clone();
  let before_browser_config = config.browser.clone();

  let after_registry = Arc::clone(&registry);
  let after_tags = feature_tags;
  let after_browser_config = config.browser.clone();

  Hooks {
    before_all: vec![Arc::new(move |pool| {
      let registry = Arc::clone(&before_registry);
      let feature_tags = before_tags.clone();
      let browser_config = before_browser_config.clone();
      Box::pin(async move {
        let mut world = build_world_from_pool(pool, browser_config).await?;
        registry
          .hooks()
          .run_suite(HookPoint::BeforeAll, &mut world, &feature_tags)
          .await
          .map_err(TestFailure::from)
      })
    })],
    after_all: vec![Arc::new(move |pool| {
      let registry = Arc::clone(&after_registry);
      let feature_tags = after_tags.clone();
      let browser_config = after_browser_config.clone();
      Box::pin(async move {
        let mut world = build_world_from_pool(pool, browser_config).await?;
        registry
          .hooks()
          .run_suite(HookPoint::AfterAll, &mut world, &feature_tags)
          .await
          .map_err(TestFailure::from)
      })
    })],
    before_each: Vec::new(),
    after_each: Vec::new(),
  }
}

async fn build_world_from_pool(
  pool: FixturePool,
  browser_config: ferridriver_test::config::BrowserConfig,
) -> Result<BrowserWorld, TestFailure> {
  let browser: Arc<ferridriver::Browser> = pool
    .get("browser")
    .await
    .map_err(|e| TestFailure::wrap("fixture 'browser' failed", e))?;
  let page: Arc<ferridriver::Page> = pool
    .get("page")
    .await
    .map_err(|e| TestFailure::wrap("fixture 'page' failed", e))?;
  let context: Arc<ferridriver::context::ContextRef> = pool
    .get("context")
    .await
    .map_err(|e| TestFailure::wrap("fixture 'context' failed", e))?;
  let request: Arc<ferridriver::http_client::HttpClient> = pool
    .get("request")
    .await
    .map_err(|e| TestFailure::wrap("fixture 'request' failed", e))?;
  let test_info: Arc<TestInfo> = pool
    .get("test_info")
    .await
    .map_err(|e| TestFailure::wrap("fixture 'test_info' failed", e))?;

  let modifiers = Arc::new(ferridriver_test::model::TestModifiers::default());
  pool.inject("__test_modifiers", Arc::clone(&modifiers));

  Ok(BrowserWorld::new(ferridriver_test::model::TestFixtures {
    browser,
    page,
    context,
    request,
    test_info,
    modifiers,
    browser_config,
    bdd_args: None,
    bdd_data_table: None,
    bdd_doc_string: None,
  }))
}

/// Map `@use(key=value, ...)` tags to a Playwright-style `use` bag for
/// [`TestCase::use_options`] — the BDD analog of `test.use`. The worker
/// merges the bag over the config's context options BEFORE the
/// scenario's browser context is created, so creation-time options
/// (locale, timezoneId, userAgent, viewport scalars, ...) take effect
/// on every backend, including the ones whose web processes latch them
/// at spawn. Repeatable; later tags override earlier keys. A bare key
/// (`@use(hasTouch)`) means `true`; values parse as bool/number when
/// they look like one, else string.
pub fn scenario_use_options(scenario: &ScenarioExecution) -> Option<serde_json::Value> {
  let mut map = serde_json::Map::new();
  for tag in &scenario.tags {
    let Some(body) = tag.strip_prefix("@use(").and_then(|s| s.strip_suffix(')')) else {
      continue;
    };
    for pair in body.split(',') {
      let pair = pair.trim();
      if pair.is_empty() {
        continue;
      }
      match pair.split_once('=') {
        Some((key, value)) => {
          map.insert(key.trim().to_string(), parse_use_value(value.trim()));
        },
        None => {
          map.insert(pair.to_string(), serde_json::Value::Bool(true));
        },
      }
    }
  }
  (!map.is_empty()).then_some(serde_json::Value::Object(map))
}

fn parse_use_value(raw: &str) -> serde_json::Value {
  match raw {
    "true" => serde_json::Value::Bool(true),
    "false" => serde_json::Value::Bool(false),
    _ => {
      if let Ok(n) = raw.parse::<i64>() {
        serde_json::Value::Number(n.into())
      } else if let Some(n) = raw.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
        serde_json::Value::Number(n)
      } else {
        serde_json::Value::String(raw.to_string())
      }
    },
  }
}

/// Translate a single scenario into a `TestCase`.
/// Map a scenario's Gherkin tags to core `TestAnnotation`s
/// (`@wip`/`@only`/`@skip(...)`/`@fixme(...)`/`@fail(...)`/`@slow(...)`
/// and `@key(value)` -> `Info`). Shared by the Rust-step and JS-step
/// translation paths.
pub fn scenario_annotations(scenario: &ScenarioExecution) -> Vec<TestAnnotation> {
  let mut annotations: Vec<TestAnnotation> = Vec::with_capacity(scenario.tags.len() + 1);

  for tag in &scenario.tags {
    annotations.push(TestAnnotation::Tag(tag.clone()));

    match tag.as_str() {
      "@wip" | "@pending" => annotations.push(TestAnnotation::Skip {
        reason: Some("tagged @wip/@pending".to_string()),
        condition: None,
      }),
      "@only" => annotations.push(TestAnnotation::Only),
      "@skip" => annotations.push(TestAnnotation::Skip {
        reason: Some("tagged @skip".to_string()),
        condition: None,
      }),
      "@fixme" => annotations.push(TestAnnotation::Fixme {
        reason: Some("tagged @fixme".to_string()),
        condition: None,
      }),
      "@fail" => annotations.push(TestAnnotation::Fail {
        reason: Some("tagged @fail".to_string()),
        condition: None,
      }),
      "@slow" => annotations.push(TestAnnotation::Slow {
        reason: Some("tagged @slow".to_string()),
        condition: None,
      }),
      _ => {
        // Parameterised forms: `@kind(cond)` for the known kinds, then the
        // generic `@key(value)` -> Info for everything else.
        if let Some(cond) = tag.strip_prefix("@skip(").and_then(|s| s.strip_suffix(')')) {
          annotations.push(TestAnnotation::Skip {
            reason: Some(format!("tagged @skip({cond})")),
            condition: Some(cond.to_string()),
          });
        } else if let Some(cond) = tag.strip_prefix("@fixme(").and_then(|s| s.strip_suffix(')')) {
          annotations.push(TestAnnotation::Fixme {
            reason: Some(format!("tagged @fixme({cond})")),
            condition: Some(cond.to_string()),
          });
        } else if let Some(cond) = tag.strip_prefix("@fail(").and_then(|s| s.strip_suffix(')')) {
          annotations.push(TestAnnotation::Fail {
            reason: Some(format!("tagged @fail({cond})")),
            condition: Some(cond.to_string()),
          });
        } else if let Some(cond) = tag.strip_prefix("@slow(").and_then(|s| s.strip_suffix(')')) {
          annotations.push(TestAnnotation::Slow {
            reason: Some(format!("tagged @slow({cond})")),
            condition: Some(cond.to_string()),
          });
        } else if let Some(rest) = tag.strip_prefix('@')
          && let Some(paren_pos) = rest.find('(')
          && rest.ends_with(')')
        {
          let key = &rest[..paren_pos];
          let value = &rest[paren_pos + 1..rest.len() - 1];
          if !matches!(key, "fixme" | "skip" | "fail" | "slow" | "only" | "use") {
            annotations.push(TestAnnotation::Info {
              type_name: key.to_string(),
              description: value.to_string(),
            });
          }
        }
      },
    }
  }

  annotations
}

/// Extract the scenario's 1-based source line from its `file:line`
/// location string.
pub fn scenario_line(scenario: &ScenarioExecution) -> Option<usize> {
  scenario
    .location
    .rsplit_once(':')
    .and_then(|(_, l)| l.parse::<usize>().ok())
}

fn translate_scenario(scenario: ScenarioExecution, registry: Arc<StepRegistry>, config: &TestConfig) -> TestCase {
  let step_timeout = Duration::from_millis(config.timeout);
  let screenshot_on_failure = config.screenshot_on_failure;
  let strict = config.strict;
  let browser_config = config.browser.clone();

  // Build the immutable TestCase metadata up front (borrows `scenario`),
  // then move the scenario into an Arc so the per-invocation closure shares
  // it via a cheap refcount bump instead of deep-cloning the step Vec.
  let annotations = scenario_annotations(&scenario);
  let use_options = scenario_use_options(&scenario);
  let line = scenario_line(&scenario);
  // The Gherkin facts a cucumber-json document quotes and the run never
  // reads. Carried as opaque metadata, so the core stays domain-free.
  let metadata = serde_json::to_value(&scenario.source).ok();
  let id = TestId {
    file: scenario.feature_path.display().to_string(),
    // A `Rule` and a Scenario Outline are each a describe around what
    // they hold, the way `playwright-bdd` renders them — so a row's
    // title path is feature > [rule >] outline > row.
    suite: Some(
      std::iter::once(scenario.feature_name.clone())
        .chain(scenario.describe_path.iter().cloned())
        .collect::<Vec<_>>()
        .join("::"),
    ),
    name: scenario.name.clone(),
    line,
    // Gherkin locations are line-only; a column would be invented.
    column: None,
  };
  let scenario = Arc::new(scenario);

  let test_fn: TestFn = Arc::new(move |pool: FixturePool| {
    let scenario = Arc::clone(&scenario);
    let registry = Arc::clone(&registry);
    let browser_config = browser_config.clone();

    Box::pin(async move {
      // Get fixtures injected by the core worker.
      let browser: Arc<ferridriver::Browser> = pool
        .get("browser")
        .await
        .map_err(|e| TestFailure::wrap("fixture 'browser' failed", e))?;
      let page: Arc<ferridriver::Page> = pool
        .get("page")
        .await
        .map_err(|e| TestFailure::wrap("fixture 'page' failed", e))?;
      let context: Arc<ferridriver::context::ContextRef> = pool
        .get("context")
        .await
        .map_err(|e| TestFailure::wrap("fixture 'context' failed", e))?;
      let test_info: Arc<TestInfo> = pool
        .get("test_info")
        .await
        .map_err(|e| TestFailure::wrap("fixture 'test_info' failed", e))?;
      let request: Arc<ferridriver::http_client::HttpClient> = pool
        .get("request")
        .await
        .map_err(|e| TestFailure::wrap("fixture 'request' failed", e))?;

      // Create shared modifiers — worker reads these after callback returns.
      let modifiers = Arc::new(ferridriver_test::model::TestModifiers::default());
      pool.inject("__test_modifiers", Arc::clone(&modifiers));

      // Build unified TestFixtures and construct BrowserWorld from it.
      let fixtures = ferridriver_test::model::TestFixtures {
        browser,
        page,
        context,
        request,
        test_info: Arc::clone(&test_info),
        modifiers,
        browser_config,
        bdd_args: None,
        bdd_data_table: None,
        bdd_doc_string: None,
      };
      let mut world = BrowserWorld::new(fixtures);

      // Delegate to the single execution engine with a TestInfo observer.
      let executor = ScenarioExecutor::new(Arc::clone(&registry), step_timeout, strict, screenshot_on_failure);
      let observer = TestInfoObserver {
        test_info: Arc::clone(&test_info),
        feature_path: scenario.feature_path.display().to_string(),
        open: std::sync::Mutex::new(None),
      };
      let result = executor.run_scenario_observed(&mut world, &scenario, &observer).await;

      // Attach failure screenshot via TestInfo (for test reports).
      // Written to disk so reporters clone a path, not the PNG bytes,
      // and the UI server can serve it with a download link.
      if let Some(bytes) = result.failure_screenshot {
        let _ = std::fs::create_dir_all(&test_info.output_dir);
        let path = test_info
          .output_dir
          .join(format!("failure-screenshot-retry{}.png", test_info.retry));
        let body = match std::fs::write(&path, &bytes) {
          Ok(()) => ferridriver_test::model::AttachmentBody::Path(path),
          Err(_) => ferridriver_test::model::AttachmentBody::Bytes(bytes),
        };
        test_info
          .attach("failure-screenshot".to_string(), "image/png".to_string(), body)
          .await;
      }

      if let Some(msg) = result.error {
        Err(TestFailure::from(msg))
      } else {
        Ok(())
      }
    })
  });

  TestCase {
    id,
    test_fn,
    fixture_requests: vec![
      "browser".to_string(),
      "context".to_string(),
      "page".to_string(),
      "test_info".to_string(),
      "request".to_string(),
    ],
    annotations,
    timeout: None,
    retries: None,
    expected_status: ExpectedStatus::Pass,
    use_options,
    metadata,
  }
}

// ── TestInfo observer ───────────────────────────────────────────────────────

/// Observer that bridges `ScenarioExecutor` step events to `TestInfo` for
/// the test runner's real-time reporting pipeline. Steps are LIVE
/// boundaries: `on_step_start` opens the `TestInfo` step (streaming
/// `StepStarted` to reporters and making the step's trace span the
/// recorder's current parent so the handler's protocol actions nest
/// under it), `on_step` closes it with the outcome. Steps skipped
/// after a failure never start; they get a zero-duration boundary at
/// `on_step` time.
struct TestInfoObserver {
  test_info: Arc<TestInfo>,
  feature_path: String,
  open: std::sync::Mutex<Option<ferridriver_test::model::StepHandle>>,
}

impl TestInfoObserver {
  fn step_metadata(step: &ScenarioStep, text: &str) -> serde_json::Value {
    serde_json::json!({
      "bdd_keyword": step.keyword.trim(),
      "bdd_text": text,
      "bdd_line": step.line,
      "bdd_arguments": step.cucumber_arguments(),
    })
  }

  fn step_location(&self, step: &ScenarioStep) -> ferridriver_test::model::StepLocation {
    ferridriver_test::model::StepLocation::new(self.feature_path.clone(), u32::try_from(step.line).unwrap_or(0))
  }

  fn take_open(&self) -> Option<ferridriver_test::model::StepHandle> {
    self
      .open
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take()
  }
}

impl StepObserver for TestInfoObserver {
  fn on_step_start<'a>(
    &'a self,
    step: &'a ScenarioStep,
    text: &'a str,
  ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
      let title = format!("{}{}", step.keyword, text);
      let mut handle = self
        .test_info
        .begin_step_at(&title, StepCategory::TestStep, Some(self.step_location(step)))
        .await;
      handle.metadata = Some(Self::step_metadata(step, text));
      *self.open.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);
    })
  }

  fn on_step<'a>(
    &'a self,
    event: StepEvent<'a>,
  ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
      let handle = match self.take_open() {
        Some(handle) => handle,
        None => {
          // Skipped before starting (a previous step failed): open the
          // boundary now so the step still shows up everywhere.
          let title = format!("{}{}", event.step.keyword, event.text);
          let mut handle = self
            .test_info
            .begin_step_at(&title, StepCategory::TestStep, Some(self.step_location(event.step)))
            .await;
          handle.metadata = Some(Self::step_metadata(event.step, event.text));
          handle
        },
      };
      match event.result.status {
        StepStatus::Passed => handle.end(None).await,
        StepStatus::Failed => handle.end(event.result.error.clone()).await,
        StepStatus::Skipped => handle.skip(event.result.error.clone()).await,
        StepStatus::Pending | StepStatus::Undefined => handle.pending(event.result.error.clone()).await,
      }
    })
  }
}

/// Execute a single BDD step: match against registry, extract params, call handler.
pub async fn execute_bdd_step(
  registry: &StepRegistry,
  world: &mut BrowserWorld,
  text: &str,
  step: &ScenarioStep,
  timeout: Duration,
  strict: bool,
) -> Result<(), crate::step::StepError> {
  // Match step text against registry.
  let step_match = match registry.find_match(text) {
    Ok(m) => m,
    Err(MatchError::Undefined { text: t, suggestions }) => {
      let keyword = step.keyword.trim();
      let snippet = crate::snippet::generate_snippet(keyword, &t, step.table.is_some(), step.docstring.is_some());
      let mut msg = format!("undefined step: \"{t}\"");
      if !suggestions.is_empty() {
        msg.push_str("\n  did you mean:");
        for s in &suggestions {
          msg.push_str(&format!("\n    - {s}"));
        }
      }
      msg.push_str(&format!("\n\n  You can implement this step with:\n\n{snippet}"));

      if strict {
        return Err(crate::step::StepError::from(msg));
      }
      return Err(crate::step::StepError::pending(msg));
    },
    Err(MatchError::Ambiguous {
      text: t,
      matches,
      expressions,
    }) => {
      let mut msg = format!("ambiguous step: \"{t}\" matched {} definitions:", matches.len());
      for (i, (loc, expr)) in matches.iter().zip(expressions.iter()).enumerate() {
        msg.push_str(&format!("\n  {}. {} ({})", i + 1, expr, loc));
      }
      return Err(crate::step::StepError::from(msg));
    },
  };

  // Prepare data table and docstring.
  let table_data = step.table.as_ref();
  let docstring = step.docstring.as_deref();

  // Execute with timeout.
  let handler = &step_match.def.handler;
  let params = step_match.params;

  let result = ferridriver::pause::run_within(timeout, handler(world, params, table_data, docstring)).await;

  match result {
    Ok(Ok(())) => Ok(()),
    Ok(Err(e)) => Err(e),
    Err(_) => Err(crate::step::StepError::from(format!(
      "step timed out after {}ms",
      timeout.as_millis()
    ))),
  }
}

/// Deterministic Fisher-Yates shuffle using a simple splitmix64 PRNG.
fn fisher_yates_shuffle<T>(items: &mut [T], seed: u64) {
  let len = items.len();
  if len <= 1 {
    return;
  }

  let mut state = seed;
  for i in (1..len).rev() {
    // splitmix64 step
    state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;

    let j = (z as usize) % (i + 1);
    items.swap(i, j);
  }
}

#[cfg(test)]
mod use_options_tests {
  use super::*;

  fn scenario_with_tags(tags: &[&str]) -> ScenarioExecution {
    ScenarioExecution {
      describe_path: Vec::new(),
      source: crate::scenario::ScenarioSource::default(),
      feature_name: "f".to_string(),
      feature_path: std::path::PathBuf::from("f.feature"),
      name: "s".to_string(),
      tags: tags.iter().map(|t| (*t).to_string()).collect(),
      steps: Vec::new(),
      location: "f.feature:1".to_string(),
      example_values: None,
    }
  }

  #[test]
  fn use_tag_maps_to_use_options_bag() {
    let s = scenario_with_tags(&["@use(locale=de-DE)"]);
    assert_eq!(scenario_use_options(&s), Some(serde_json::json!({"locale": "de-DE"})));
  }

  #[test]
  fn use_tag_parses_types_and_merges_repeats() {
    let s = scenario_with_tags(&[
      "@use(locale=de-DE, hasTouch, deviceScaleFactor=2)",
      "@use(offline=false)",
      "@use(locale=fr-FR)",
    ]);
    assert_eq!(
      scenario_use_options(&s),
      Some(serde_json::json!({
        "locale": "fr-FR",
        "hasTouch": true,
        "deviceScaleFactor": 2,
        "offline": false,
      }))
    );
  }

  #[test]
  fn non_use_tags_produce_no_bag() {
    let s = scenario_with_tags(&["@smoke", "@skip(firefox)"]);
    assert_eq!(scenario_use_options(&s), None);
  }

  #[test]
  fn use_tag_is_not_an_info_annotation() {
    let s = scenario_with_tags(&["@use(locale=de-DE)"]);
    let annotations = scenario_annotations(&s);
    assert!(
      !annotations
        .iter()
        .any(|a| matches!(a, TestAnnotation::Info { type_name, .. } if type_name == "use")),
      "@use must not leak into Info annotations: {annotations:?}"
    );
  }
}
