//! Single BDD execution engine for all consumers (TestRunner, MCP, standalone).
//!
//! `ScenarioExecutor` handles step matching, hooks, variable interpolation,
//! timeouts, and result collection.  An optional `StepObserver` allows callers
//! to receive per-step events in real-time (TestInfo reporting in the test
//! runner, progress notifications in MCP, etc.).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::hook::HookPoint;
use crate::registry::StepRegistry;
use crate::scenario::{ScenarioExecution, ScenarioResult, ScenarioStatus, ScenarioStep, StepResult, StepStatus};
use crate::translate::execute_bdd_step;
use crate::world::BrowserWorld;

// ── Step observer ───────────────────────────────────────────────────────────

/// Per-step event emitted during scenario execution.
pub struct StepEvent<'a> {
  /// The Gherkin step definition (keyword, original text, line number).
  pub step: &'a ScenarioStep,
  /// The interpolated step text (after `$variable` substitution).
  pub text: &'a str,
  /// The execution result for this step.
  pub result: &'a StepResult,
}

/// Callback for observing step execution in real-time.
///
/// Implement this to receive `StepEvent`s as each step completes (or is
/// skipped).  The test runner uses this for `TestInfo` step events; MCP
/// could use it for progress notifications.
pub trait StepObserver: Send + Sync {
  fn on_step<'a>(&'a self, event: StepEvent<'a>) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// No-op observer used when no step reporting is needed.
pub(crate) struct NoopObserver;

impl StepObserver for NoopObserver {
  fn on_step<'a>(&'a self, _event: StepEvent<'a>) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(std::future::ready(()))
  }
}

/// Executes BDD scenarios and individual steps without requiring the
/// `TestRunner` or `FixturePool`.
///
/// The caller is responsible for constructing a `BrowserWorld` with the
/// desired `Page` and `ContextRef`.  The executor handles step matching,
/// hooks, variable interpolation, and result collection.
#[derive(Clone)]
pub struct ScenarioExecutor {
  registry: Arc<StepRegistry>,
  step_timeout: Duration,
  strict: bool,
  screenshot_on_failure: bool,
}

impl ScenarioExecutor {
  /// Create a new executor.
  pub fn new(registry: Arc<StepRegistry>, step_timeout: Duration, strict: bool, screenshot_on_failure: bool) -> Self {
    Self {
      registry,
      step_timeout,
      strict,
      screenshot_on_failure,
    }
  }

  /// Run a full scenario with hooks against the given world.
  ///
  /// Equivalent to `run_scenario_observed` with a no-op observer.
  pub async fn run_scenario(&self, world: &mut BrowserWorld, scenario: &ScenarioExecution) -> ScenarioResult {
    self.run_scenario_observed(world, scenario, &NoopObserver).await
  }

  /// Run a full scenario with hooks and a per-step observer.
  ///
  /// Executes: `BeforeScenario` hooks -> steps (with `BeforeStep`/`AfterStep`
  /// hooks around each) -> `AfterScenario` hooks.  The observer is notified
  /// after each step completes or is skipped, enabling real-time reporting.
  pub async fn run_scenario_observed(
    &self,
    world: &mut BrowserWorld,
    scenario: &ScenarioExecution,
    observer: &dyn StepObserver,
  ) -> ScenarioResult {
    let start = Instant::now();

    // Ensure the world has the registry for step composition (world.run_step()).
    // Only set if not already pointing to the same registry (avoids Arc clone).
    if world.registry_arc().as_ref().map(Arc::as_ptr) != Some(Arc::as_ptr(&self.registry)) {
      world.set_registry(Arc::clone(&self.registry));
    }

    // Set feature directory for fixture path resolution.
    if let Some(dir) = scenario.feature_path.parent() {
      world.set_feature_dir(dir.to_path_buf());
    }

    // Inject Scenario Outline example values as variables.
    if let Some(values) = &scenario.example_values {
      for (key, val) in values {
        world.set_var(key, val);
      }
    }

    let mut step_results: Vec<StepResult> = Vec::with_capacity(scenario.steps.len());
    let mut had_failure = false;
    let mut failure_message: Option<String> = None;

    // BeforeScenario hooks.
    if let Err(e) = self
      .registry
      .hooks()
      .run_scenario(HookPoint::BeforeScenario, world, &scenario.tags)
      .await
    {
      return ScenarioResult {
        feature_name: scenario.feature_name.clone(),
        feature_path: scenario.feature_path.display().to_string(),
        scenario_name: scenario.name.clone(),
        status: ScenarioStatus::Failed,
        steps: Vec::new(),
        duration: start.elapsed(),
        attempt: 1,
        tags: scenario.tags.clone(),
        error: Some(format!("BeforeScenario hook failed: {e}")),
        failure_screenshot: None,
      };
    }

    // Execute steps.
    for step in &scenario.steps {
      if had_failure {
        let sr = StepResult {
          keyword: step.keyword.clone(),
          text: step.text.clone(),
          status: StepStatus::Skipped,
          duration: Duration::ZERO,
          error: Some("skipped due to previous failure".to_string()),
        };
        observer
          .on_step(StepEvent {
            step,
            text: &step.text,
            result: &sr,
          })
          .await;
        step_results.push(sr);
        continue;
      }

      let text = world.interpolate(&step.text);
      let step_start = Instant::now();

      // BeforeStep hooks.
      if let Err(e) = self
        .registry
        .hooks()
        .run_step(HookPoint::BeforeStep, world, &text, &scenario.tags)
        .await
      {
        tracing::warn!("BeforeStep hook failed: {e}");
      }

      // Match and execute.
      let result = execute_bdd_step(&self.registry, world, &text, step, self.step_timeout, self.strict).await;

      let step_duration = step_start.elapsed();

      let sr = match result {
        Ok(()) => StepResult {
          keyword: step.keyword.clone(),
          text: text.clone(),
          status: StepStatus::Passed,
          duration: step_duration,
          error: None,
        },
        Err(e) if e.pending && !self.strict => StepResult {
          keyword: step.keyword.clone(),
          text: text.clone(),
          status: StepStatus::Pending,
          duration: step_duration,
          error: Some(e.to_string()),
        },
        Err(e) => {
          let msg = e.to_string();
          had_failure = true;
          failure_message = Some(msg.clone());
          StepResult {
            keyword: step.keyword.clone(),
            text: text.clone(),
            status: StepStatus::Failed,
            duration: step_duration,
            error: Some(msg),
          }
        },
      };

      observer
        .on_step(StepEvent {
          step,
          text: &text,
          result: &sr,
        })
        .await;
      step_results.push(sr);

      // AfterStep hooks (always, even on failure).
      if let Err(e) = self
        .registry
        .hooks()
        .run_step(HookPoint::AfterStep, world, &text, &scenario.tags)
        .await
      {
        tracing::warn!("AfterStep hook failed: {e}");
      }
    }

    // AfterScenario hooks (always, even on failure).
    if let Err(e) = self
      .registry
      .hooks()
      .run_scenario(HookPoint::AfterScenario, world, &scenario.tags)
      .await
    {
      tracing::warn!("AfterScenario hook failed: {e}");
    }

    // Screenshot on failure.
    let failure_screenshot = if had_failure && self.screenshot_on_failure {
      world
        .page()
        .screenshot(ferridriver::options::ScreenshotOptions::default())
        .await
        .ok()
    } else {
      None
    };

    let status = if had_failure {
      ScenarioStatus::Failed
    } else if step_results.iter().any(|s| s.status == StepStatus::Pending) {
      ScenarioStatus::Undefined
    } else {
      ScenarioStatus::Passed
    };

    ScenarioResult {
      feature_name: scenario.feature_name.clone(),
      feature_path: scenario.feature_path.display().to_string(),
      scenario_name: scenario.name.clone(),
      status,
      steps: step_results,
      duration: start.elapsed(),
      attempt: 1,
      tags: scenario.tags.clone(),
      error: failure_message,
      failure_screenshot,
    }
  }

  /// Execute a single BDD step (for interactive / REPL use).
  ///
  /// Matches the step text against the registry and invokes the handler
  /// directly.  No hooks are executed -- use `run_scenario` for full
  /// lifecycle.
  pub async fn run_step(
    &self,
    world: &mut BrowserWorld,
    text: &str,
    table: Option<&crate::data_table::DataTable>,
    docstring: Option<&str>,
  ) -> StepResult {
    // Ensure registry is set for step composition (skip if already set to same).
    if world.registry_arc().as_ref().map(Arc::as_ptr) != Some(Arc::as_ptr(&self.registry)) {
      world.set_registry(Arc::clone(&self.registry));
    }

    let interpolated = world.interpolate(text);
    let start = Instant::now();

    // Match and execute directly -- no ScenarioStep allocation needed.
    let result = match self.registry.find_match(&interpolated) {
      Ok(step_match) => {
        let handler = &step_match.def.handler;
        match tokio::time::timeout(self.step_timeout, handler(world, step_match.params, table, docstring)).await {
          Ok(r) => r,
          Err(_) => Err(crate::step::StepError::from(format!(
            "step timed out after {}ms",
            self.step_timeout.as_millis()
          ))),
        }
      },
      Err(crate::step::MatchError::Undefined { text: t, suggestions }) => {
        let mut msg = format!("undefined step: \"{t}\"");
        if !suggestions.is_empty() {
          msg.push_str("\n  did you mean:");
          for s in &suggestions {
            msg.push_str(&format!("\n    - {s}"));
          }
        }
        if self.strict {
          Err(crate::step::StepError::from(msg))
        } else {
          Err(crate::step::StepError::pending(msg))
        }
      },
      Err(crate::step::MatchError::Ambiguous {
        text: t,
        matches,
        expressions,
      }) => {
        let mut msg = format!("ambiguous step: \"{t}\" matched {} definitions:", matches.len());
        for (i, (loc, expr)) in matches.iter().zip(expressions.iter()).enumerate() {
          msg.push_str(&format!("\n  {}. {} ({})", i + 1, expr, loc));
        }
        Err(crate::step::StepError::from(msg))
      },
    };

    let duration = start.elapsed();

    match result {
      Ok(()) => StepResult {
        keyword: String::new(),
        text: interpolated,
        status: StepStatus::Passed,
        duration,
        error: None,
      },
      Err(e) if e.pending && !self.strict => StepResult {
        keyword: String::new(),
        text: interpolated,
        status: StepStatus::Pending,
        duration,
        error: Some(e.to_string()),
      },
      Err(e) => StepResult {
        keyword: String::new(),
        text: interpolated,
        status: StepStatus::Failed,
        duration,
        error: Some(e.to_string()),
      },
    }
  }

  /// Access the step registry.
  pub fn registry(&self) -> &StepRegistry {
    &self.registry
  }
}
