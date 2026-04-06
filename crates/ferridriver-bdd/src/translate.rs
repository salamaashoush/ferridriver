//! Translation layer: converts Gherkin features into ferridriver-test `TestPlan`.
//!
//! Each Feature becomes a `TestSuite`, each Scenario becomes a `TestCase`.
//! The test function for each scenario runs the BDD steps via the `StepRegistry`,
//! emitting step events through `TestInfo::begin_step()` for real-time reporting.

use std::sync::Arc;
use std::time::Duration;

use ferridriver_test::config::TestConfig;
use ferridriver_test::model::{
  ExpectedStatus, Hooks, StepCategory, TestAnnotation, TestCase, TestFailure, TestFn, TestId,
  TestInfo, TestPlan, TestSuite, SuiteMode,
};
use ferridriver_test::FixturePool;

use crate::feature::FeatureSet;
use crate::hook::HookPoint;
use crate::registry::StepRegistry;
use crate::scenario::{self, ScenarioExecution, ScenarioStep};
use crate::step::MatchError;
use crate::world::BrowserWorld;

/// Translate parsed Gherkin features into a `TestPlan` for the core test runner.
pub fn translate_features(
  feature_set: &FeatureSet,
  registry: Arc<StepRegistry>,
  config: &TestConfig,
) -> TestPlan {
  let mut suites = Vec::new();

  for feature in &feature_set.features {
    let scenarios = scenario::expand_feature(feature);
    if scenarios.is_empty() {
      continue;
    }

    let feature_name = feature.feature.name.clone();
    let feature_path = feature.path.display().to_string();

    // @serial tag on any scenario means the whole feature runs serially.
    let is_serial = scenarios
      .iter()
      .any(|s| s.tags.iter().any(|t| t == "@serial"));

    let test_cases: Vec<TestCase> = scenarios
      .iter()
      .map(|s| translate_scenario(s, Arc::clone(&registry), config))
      .collect();

    suites.push(TestSuite {
      name: feature_name,
      file: feature_path,
      tests: test_cases,
      hooks: Hooks::default(),
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

/// Translate a single scenario into a `TestCase`.
fn translate_scenario(
  scenario: &ScenarioExecution,
  registry: Arc<StepRegistry>,
  config: &TestConfig,
) -> TestCase {
  let scenario_clone = scenario.clone();
  let step_timeout = Duration::from_millis(config.timeout);
  let screenshot_on_failure = config.screenshot_on_failure;
  let strict = config.strict;

  let test_fn: TestFn = Arc::new(move |pool: FixturePool| {
    let scenario = scenario_clone.clone();
    let registry = Arc::clone(&registry);

    Box::pin(async move {
      // Get fixtures injected by the core worker.
      let test_info: Arc<TestInfo> = pool
        .get("test_info")
        .await
        .map_err(|e| TestFailure::from(format!("fixture 'test_info' failed: {e}")))?;

      let page: Arc<ferridriver::Page> = pool
        .get("page")
        .await
        .map_err(|e| TestFailure::from(format!("fixture 'page' failed: {e}")))?;

      let context: Arc<ferridriver::context::ContextRef> = pool
        .get("context")
        .await
        .map_err(|e| TestFailure::from(format!("fixture 'context' failed: {e}")))?;

      // Construct BrowserWorld from the fixtures.
      let mut world = BrowserWorld::new((*page).clone(), (*context).clone());

      // Wire test_info and registry for attachments and step composition.
      world.set_test_info(Arc::clone(&test_info));
      world.set_registry(Arc::clone(&registry));

      // Inject Scenario Outline example values as variables.
      if let Some(values) = &scenario.example_values {
        for (key, val) in values {
          world.set_var(key, val);
        }
      }

      // Run BDD BeforeScenario hooks (tag-filtered).
      if let Err(e) = registry
        .hooks()
        .run_scenario(HookPoint::BeforeScenario, &mut world, &scenario.tags)
        .await
      {
        return Err(TestFailure::from(format!("BeforeScenario hook failed: {e}")));
      }

      // Execute steps.
      let mut had_failure = false;
      let mut failure_message: Option<String> = None;

      for step in &scenario.steps {
        if had_failure {
          // Record skipped step.
          let handle = test_info
            .begin_step(format!("{}{}", step.keyword, step.text), StepCategory::TestStep)
            .await;
          handle
            .skip(Some("skipped due to previous failure".to_string()))
            .await;
          continue;
        }

        // Interpolate variables.
        let text = world.interpolate(&step.text);
        let step_title = format!("{}{}", step.keyword, text);

        // Begin step (emits StepStarted event).
        let mut handle = test_info
          .begin_step(&step_title, StepCategory::TestStep)
          .await;

        // Attach BDD metadata (keyword, original text) for domain-specific reporters.
        handle.metadata = Some(serde_json::json!({
          "bdd_keyword": step.keyword.trim(),
          "bdd_text": text,
          "bdd_line": step.line,
        }));

        // Run BeforeStep hooks.
        if let Err(e) = registry
          .hooks()
          .run_step(HookPoint::BeforeStep, &mut world, &text, &scenario.tags)
          .await
        {
          tracing::warn!("BeforeStep hook failed: {e}");
        }

        // Match and execute.
        let result =
          execute_bdd_step(&registry, &mut world, &text, step, step_timeout, strict).await;

        match result {
          Ok(()) => handle.end(None).await,
          Err(e) if e.pending && !strict => {
            // Pending step in non-strict mode: mark as pending, don't fail.
            handle.pending(Some(e.to_string())).await;
          }
          Err(e) => {
            let msg = e.to_string();
            had_failure = true;
            failure_message = Some(msg.clone());
            handle.end(Some(msg)).await;
          }
        }

        // Run AfterStep hooks (always, even on failure).
        if let Err(e) = registry
          .hooks()
          .run_step(HookPoint::AfterStep, &mut world, &text, &scenario.tags)
          .await
        {
          tracing::warn!("AfterStep hook failed: {e}");
        }
      }

      // Run AfterScenario hooks (always, even on failure).
      if let Err(e) = registry
        .hooks()
        .run_scenario(HookPoint::AfterScenario, &mut world, &scenario.tags)
        .await
      {
        tracing::warn!("AfterScenario hook failed: {e}");
      }

      // Screenshot on failure.
      if had_failure && screenshot_on_failure {
        if let Ok(bytes) = world
          .page()
          .screenshot(ferridriver::options::ScreenshotOptions::default())
          .await
        {
          test_info
            .attach(
              "failure-screenshot".to_string(),
              "image/png".to_string(),
              ferridriver_test::model::AttachmentBody::Bytes(bytes),
            )
            .await;
        }
      }

      if let Some(msg) = failure_message {
        Err(TestFailure::from(msg))
      } else {
        Ok(())
      }
    })
  });

  // Map BDD tags to TestAnnotations.
  let mut annotations: Vec<TestAnnotation> = scenario
    .tags
    .iter()
    .map(|t| TestAnnotation::Tag(t.clone()))
    .collect();

  if scenario
    .tags
    .iter()
    .any(|t| t == "@skip" || t == "@wip" || t == "@pending")
  {
    annotations.push(TestAnnotation::Skip {
      reason: Some("tagged @skip/@wip/@pending".to_string()),
    });
  }

  if scenario.tags.iter().any(|t| t == "@slow") {
    annotations.push(TestAnnotation::Slow);
  }

  TestCase {
    id: TestId {
      file: scenario.feature_path.display().to_string(),
      suite: Some(scenario.feature_name.clone()),
      name: scenario.name.clone(),
      line: None,
    },
    test_fn,
    fixture_requests: vec![
      "browser".to_string(),
      "context".to_string(),
      "page".to_string(),
      "test_info".to_string(),
    ],
    annotations,
    timeout: None,
    retries: None,
    expected_status: ExpectedStatus::Pass,
  }
}

/// Execute a single BDD step: match against registry, extract params, call handler.
async fn execute_bdd_step(
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
      let snippet = crate::snippet::generate_snippet(
        keyword,
        &t,
        step.table.is_some(),
        step.docstring.is_some(),
      );
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
    }
    Err(MatchError::Ambiguous { text: t, matches, expressions }) => {
      let mut msg = format!("ambiguous step: \"{t}\" matched {} definitions:", matches.len());
      for (i, (loc, expr)) in matches.iter().zip(expressions.iter()).enumerate() {
        msg.push_str(&format!("\n  {}. {} ({})", i + 1, expr, loc));
      }
      return Err(crate::step::StepError::from(msg));
    }
  };

  // Prepare data table and docstring.
  let table_data = step.table.as_ref();
  let docstring = step.docstring.as_deref();

  // Execute with timeout.
  let handler = &step_match.def.handler;
  let params = step_match.params;

  let result =
    tokio::time::timeout(timeout, handler(world, params, table_data, docstring)).await;

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
