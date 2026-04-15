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
    let scenarios = scenario::expand_feature(feature);
    if scenarios.is_empty() {
      continue;
    }

    let feature_name = feature.feature.name.clone();
    let feature_path = feature.path.display().to_string();
    let feature_tags = crate::feature::extract_tags(&feature.feature.tags);

    // @serial tag on any scenario means the whole feature runs serially.
    let is_serial = scenarios.iter().any(|s| s.tags.iter().any(|t| t == "@serial"));

    let test_cases: Vec<TestCase> = scenarios
      .iter()
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
    .map_err(|e| TestFailure::from(format!("fixture 'browser' failed: {e}")))?;
  let page: Arc<ferridriver::Page> = pool
    .get("page")
    .await
    .map_err(|e| TestFailure::from(format!("fixture 'page' failed: {e}")))?;
  let context: Arc<ferridriver::context::ContextRef> = pool
    .get("context")
    .await
    .map_err(|e| TestFailure::from(format!("fixture 'context' failed: {e}")))?;
  let request: Arc<ferridriver::api_request::APIRequestContext> = pool
    .get("request")
    .await
    .map_err(|e| TestFailure::from(format!("fixture 'request' failed: {e}")))?;
  let test_info: Arc<TestInfo> = pool
    .get("test_info")
    .await
    .map_err(|e| TestFailure::from(format!("fixture 'test_info' failed: {e}")))?;

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

/// Translate a single scenario into a `TestCase`.
fn translate_scenario(scenario: &ScenarioExecution, registry: Arc<StepRegistry>, config: &TestConfig) -> TestCase {
  let scenario_clone = scenario.clone();
  let step_timeout = Duration::from_millis(config.timeout);
  let screenshot_on_failure = config.screenshot_on_failure;
  let strict = config.strict;
  let browser_config = config.browser.clone();

  let test_fn: TestFn = Arc::new(move |pool: FixturePool| {
    let scenario = scenario_clone.clone();
    let registry = Arc::clone(&registry);
    let browser_config = browser_config.clone();

    Box::pin(async move {
      // Get fixtures injected by the core worker.
      let browser: Arc<ferridriver::Browser> = pool
        .get("browser")
        .await
        .map_err(|e| TestFailure::from(format!("fixture 'browser' failed: {e}")))?;
      let page: Arc<ferridriver::Page> = pool
        .get("page")
        .await
        .map_err(|e| TestFailure::from(format!("fixture 'page' failed: {e}")))?;
      let context: Arc<ferridriver::context::ContextRef> = pool
        .get("context")
        .await
        .map_err(|e| TestFailure::from(format!("fixture 'context' failed: {e}")))?;
      let test_info: Arc<TestInfo> = pool
        .get("test_info")
        .await
        .map_err(|e| TestFailure::from(format!("fixture 'test_info' failed: {e}")))?;
      let request: Arc<ferridriver::api_request::APIRequestContext> = pool
        .get("request")
        .await
        .map_err(|e| TestFailure::from(format!("fixture 'request' failed: {e}")))?;

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
      };
      let result = executor.run_scenario_observed(&mut world, &scenario, &observer).await;

      // Attach failure screenshot via TestInfo (for test reports).
      if let Some(bytes) = result.failure_screenshot {
        test_info
          .attach(
            "failure-screenshot".to_string(),
            "image/png".to_string(),
            ferridriver_test::model::AttachmentBody::Bytes(bytes),
          )
          .await;
      }

      if let Some(msg) = result.error {
        Err(TestFailure::from(msg))
      } else {
        Ok(())
      }
    })
  });

  // Map BDD tags to TestAnnotations.
  let mut annotations: Vec<TestAnnotation> = scenario.tags.iter().map(|t| TestAnnotation::Tag(t.clone())).collect();

  if scenario.tags.iter().any(|t| t == "@wip" || t == "@pending") {
    annotations.push(TestAnnotation::Skip {
      reason: Some("tagged @wip/@pending".to_string()),
      condition: None,
    });
  }

  if scenario.tags.iter().any(|t| t == "@only") {
    annotations.push(TestAnnotation::Only);
  }

  // @skip, @skip(condition), @fixme, @fixme(condition), @fail, @fail(condition),
  // @slow, @slow(condition)
  for tag in &scenario.tags {
    if tag == "@skip" {
      annotations.push(TestAnnotation::Skip {
        reason: Some("tagged @skip".to_string()),
        condition: None,
      });
    } else if let Some(cond) = tag.strip_prefix("@skip(").and_then(|s| s.strip_suffix(')')) {
      annotations.push(TestAnnotation::Skip {
        reason: Some(format!("tagged @skip({cond})")),
        condition: Some(cond.to_string()),
      });
    } else if tag == "@fixme" {
      annotations.push(TestAnnotation::Fixme {
        reason: Some("tagged @fixme".to_string()),
        condition: None,
      });
    } else if let Some(cond) = tag.strip_prefix("@fixme(").and_then(|s| s.strip_suffix(')')) {
      annotations.push(TestAnnotation::Fixme {
        reason: Some(format!("tagged @fixme({cond})")),
        condition: Some(cond.to_string()),
      });
    } else if tag == "@fail" {
      annotations.push(TestAnnotation::Fail {
        reason: Some("tagged @fail".to_string()),
        condition: None,
      });
    } else if let Some(cond) = tag.strip_prefix("@fail(").and_then(|s| s.strip_suffix(')')) {
      annotations.push(TestAnnotation::Fail {
        reason: Some(format!("tagged @fail({cond})")),
        condition: Some(cond.to_string()),
      });
    } else if tag == "@slow" {
      annotations.push(TestAnnotation::Slow {
        reason: Some("tagged @slow".to_string()),
        condition: None,
      });
    } else if let Some(cond) = tag.strip_prefix("@slow(").and_then(|s| s.strip_suffix(')')) {
      annotations.push(TestAnnotation::Slow {
        reason: Some(format!("tagged @slow({cond})")),
        condition: Some(cond.to_string()),
      });
    }
  }

  // @key(value) → Info annotations (e.g., @issue(JIRA-1234), @severity(critical), @owner(team-auth))
  for tag in &scenario.tags {
    if let Some(rest) = tag.strip_prefix('@') {
      if let Some(paren_pos) = rest.find('(') {
        if rest.ends_with(')') {
          let key = &rest[..paren_pos];
          let value = &rest[paren_pos + 1..rest.len() - 1];
          // Skip annotation tags handled above.
          if !matches!(key, "fixme" | "skip" | "fail" | "slow" | "only") {
            annotations.push(TestAnnotation::Info {
              type_name: key.to_string(),
              description: value.to_string(),
            });
          }
        }
      }
    }
  }

  // Extract line number from location "file:line".
  let line = scenario
    .location
    .rsplit_once(':')
    .and_then(|(_, l)| l.parse::<usize>().ok());

  TestCase {
    id: TestId {
      file: scenario.feature_path.display().to_string(),
      suite: Some(scenario.feature_name.clone()),
      name: scenario.name.clone(),
      line,
    },
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
    use_options: None,
  }
}

// ── TestInfo observer ───────────────────────────────────────────────────────

/// Observer that bridges `ScenarioExecutor` step events to `TestInfo` for
/// the test runner's real-time reporting pipeline.
struct TestInfoObserver {
  test_info: Arc<TestInfo>,
}

impl StepObserver for TestInfoObserver {
  fn on_step<'a>(
    &'a self,
    event: StepEvent<'a>,
  ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
      let step_title = format!("{}{}", event.step.keyword, event.text);
      self
        .test_info
        .record_step(
          step_title,
          StepCategory::TestStep,
          match event.result.status {
            StepStatus::Passed => ferridriver_test::model::StepStatus::Passed,
            StepStatus::Failed => ferridriver_test::model::StepStatus::Failed,
            StepStatus::Skipped => ferridriver_test::model::StepStatus::Skipped,
            StepStatus::Pending => ferridriver_test::model::StepStatus::Pending,
            StepStatus::Undefined => ferridriver_test::model::StepStatus::Pending,
          },
          event.result.duration,
          event.result.error.clone(),
          Some(serde_json::json!({
            "bdd_keyword": event.step.keyword.trim(),
            "bdd_text": event.text,
            "bdd_line": event.step.line,
          })),
        )
        .await;
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

  let result = tokio::time::timeout(timeout, handler(world, params, table_data, docstring)).await;

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
