//! JavaScript step definitions driven by the shared QuickJS engine.
//!
//! This is the BDD half of the "one engine, two entry points" model.
//! `ferridriver-script` owns the VM and every binding (`page`,
//! `locator`, ...); this module reuses that VM to load cucumber-js-shaped
//! `.js` step files and drive them from the Rust BDD core
//! (`feature`/`scenario`/`filter`/`registry`). It contains no matching,
//! outline-expansion or tag logic — that all stays in the core.
//!
//! The cucumber World is a first-class object: `ferridriver-script`'s
//! `install_*_on` helpers install `page`/`context`/`request`/`browser`
//! onto a per-scenario World object (the step `this`) — the same
//! bindings scripting installs onto `globalThis`. The step registry is
//! per-VM JavaScript state (as plugins are, per session VM), so each
//! engine session builds its own [`StepRegistry`] from that VM's
//! `__fdBdd.snapshot()`; one session per ferridriver-test worker gives
//! parallel scenarios isolated VMs.

use std::path::Path;
use std::sync::Arc;

use ferridriver_script::{
  AsyncContext, CollectedRegistry, InMemoryVars, PathSandbox, RunContext, RunOptions, ScenarioWorld, ScriptEngineConfig,
  Session, StepOutcome, collect_registry, invoke_hook, invoke_step, set_scenario_world,
};

use crate::filter::TagExpression;
use crate::param_type::CustomParamType;
use crate::registry::StepRegistry;
use crate::scenario::ScenarioExecution;
use crate::step::{StepError, StepHandler, StepKind, StepLocation, StepParam};
use crate::world::BrowserWorld;

const JS_STEP_LOCATION: &str = "<js-step>";

/// Status of one step in a JS-driven scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsStepStatus {
  Passed,
  Failed(String),
  Skipped,
  Undefined(String),
  Pending,
}

/// Result of one step.
#[derive(Debug, Clone)]
pub struct JsStepResult {
  pub keyword: String,
  pub text: String,
  pub status: JsStepStatus,
}

/// Result of one scenario.
#[derive(Debug, Clone)]
pub struct JsScenarioResult {
  pub name: String,
  pub tags: Vec<String>,
  pub steps: Vec<JsStepResult>,
  pub passed: bool,
}

/// A loaded JS step suite bound to one shared-engine session (one per
/// ferridriver-test worker in the integrated path).
pub struct JsBddSession {
  session: Session,
  registry: Arc<StepRegistry>,
  snapshot: CollectedRegistry,
  source: String,
}

impl JsBddSession {
  /// The Cucumber-Expression / built-in step registry built from this
  /// VM's JS registrations plus the inventory-collected Rust steps.
  #[must_use]
  pub fn registry(&self) -> Arc<StepRegistry> {
    Arc::clone(&self.registry)
  }

  /// Evaluate the step `.js` source in a fresh shared-engine session and
  /// build the Rust step registry from what it registered.
  ///
  /// `step_dir` roots the script sandbox (so `import './helpers.js'`
  /// resolves). No `page` is bound at the session level — page/context
  /// are bound per scenario onto the World ([`set_scenario_world`]).
  pub async fn load(step_source: &str, step_dir: &Path) -> anyhow::Result<Self> {
    let sandbox = Arc::new(
      PathSandbox::new(step_dir).map_err(|e| anyhow::anyhow!("sandbox {}: {}", step_dir.display(), e.message))?,
    );
    let run_ctx = RunContext {
      vars: Arc::new(InMemoryVars::new()),
      sandbox,
      artifacts: None,
      page: None,
      browser_context: None,
      request: None,
      browser: None,
      plugins: Vec::new(),
    };

    let session = Session::create(ScriptEngineConfig::default(), &run_ctx)
      .await
      .map_err(|e| anyhow::anyhow!("session create: {}", e.message))?;

    let run = session.execute(step_source, &[], RunOptions::default(), &run_ctx).await;
    if run.result.is_err() {
      if let ferridriver_script::Outcome::Error { error } = &run.result.outcome {
        anyhow::bail!("step file failed to load: {}", fmt_script_error(error));
      }
      anyhow::bail!("step file failed to load");
    }

    let actx = session.async_context();
    let snapshot = collect_registry(&actx)
      .await
      .map_err(|e| anyhow::anyhow!("collect registry: {}", e.message))?;

    let mut registry = StepRegistry::build();
    for pt in &snapshot.param_types {
      registry.register_param_type(CustomParamType {
        name: pt.name.clone(),
        regex: pt.regexp.clone(),
        transformer: None,
      });
    }
    for (idx, step) in snapshot.steps.iter().enumerate() {
      let kind = match step.kind.as_str() {
        "Given" => StepKind::Given,
        "When" => StepKind::When,
        "Then" => StepKind::Then,
        _ => StepKind::Step,
      };
      let handler = js_step_handler(actx.clone(), idx, step_source.to_string());
      let loc = StepLocation {
        file: JS_STEP_LOCATION,
        line: 0,
      };
      let res = if step.is_regex {
        registry.register_regex(kind, &step.pattern, handler, loc)
      } else {
        registry.register(kind, &step.pattern, handler, loc)
      };
      res.map_err(|e| anyhow::anyhow!("register step `{}`: {}", step.pattern, e))?;
    }

    Ok(Self {
      session,
      registry: Arc::new(registry),
      snapshot,
      source: step_source.to_string(),
    })
  }

  fn hook_indices(&self, kind: &str) -> Vec<(usize, Option<TagExpression>)> {
    self
      .snapshot
      .hooks
      .iter()
      .enumerate()
      .filter(|(_, h)| h.hook_type == kind)
      .map(|(i, h)| {
        let te = h.tags.as_deref().and_then(|t| TagExpression::parse(t).ok());
        (i, te)
      })
      .collect()
  }

  async fn run_hooks(&self, kind: &str, tags: Option<&[String]>) -> Result<(), String> {
    let actx = self.session.async_context();
    let mut hooks = self.hook_indices(kind);
    if kind == "After" || kind == "AfterAll" {
      hooks.reverse();
    }
    for (idx, te) in hooks {
      let applies = match (&te, tags) {
        (Some(expr), Some(t)) => expr.matches(t),
        (Some(_), None) => false,
        (None, _) => true,
      };
      if !applies {
        continue;
      }
      if let Err(e) = invoke_hook(&actx, idx, &self.source).await {
        return Err(fmt_script_error(&e));
      }
    }
    Ok(())
  }

  /// Run run-level `BeforeAll` hooks (once per session/worker).
  pub async fn before_all(&self) -> Result<(), String> {
    self.run_hooks("BeforeAll", None).await
  }

  /// Run run-level `AfterAll` hooks (once per session/worker).
  pub async fn after_all(&self) -> Result<(), String> {
    self.run_hooks("AfterAll", None).await
  }

  /// Execute one expanded scenario end-to-end: build its World from the
  /// fixtures, run `Before` hooks, the steps, then `After` hooks.
  pub async fn run_scenario(&self, scenario: &ScenarioExecution, world: &mut BrowserWorld) -> JsScenarioResult {
    let actx = self.session.async_context();

    // Bind this scenario's fixtures onto a fresh World object via the
    // shared install_* helpers (the same bindings as scripting).
    let fixtures = world.fixtures();
    let sw = ScenarioWorld {
      page: Some(Arc::clone(&fixtures.page)),
      context: Some(Arc::clone(&fixtures.context)),
      request: Some(Arc::clone(&fixtures.request)),
      browser: Some(Arc::clone(&fixtures.browser)),
    };
    if let Err(e) = set_scenario_world(&actx, &sw).await {
      return JsScenarioResult {
        name: scenario.name.clone(),
        tags: scenario.tags.clone(),
        steps: vec![JsStepResult {
          keyword: "World".into(),
          text: "bind fixtures".into(),
          status: JsStepStatus::Failed(format!("set_scenario_world: {}", e.message)),
        }],
        passed: false,
      };
    }

    let mut steps = Vec::with_capacity(scenario.steps.len());
    let mut failed = false;

    if let Err(msg) = self.run_hooks("Before", Some(&scenario.tags)).await {
      steps.push(JsStepResult {
        keyword: "Before".into(),
        text: "hook".into(),
        status: JsStepStatus::Failed(msg),
      });
      failed = true;
    }

    if !failed {
      for step in &scenario.steps {
        if failed {
          steps.push(JsStepResult {
            keyword: step.keyword.clone(),
            text: step.text.clone(),
            status: JsStepStatus::Skipped,
          });
          continue;
        }
        let status = match self.registry.find_match(&step.text) {
          Err(e) => {
            failed = true;
            JsStepStatus::Undefined(e.to_string())
          },
          Ok(m) => {
            let fut = (m.def.handler)(world, m.params, step.table.as_ref(), step.docstring.as_deref());
            match fut.await {
              Ok(()) => JsStepStatus::Passed,
              Err(e) if e.pending => {
                failed = true;
                JsStepStatus::Pending
              },
              Err(e) => {
                failed = true;
                JsStepStatus::Failed(e.to_string())
              },
            }
          },
        };
        steps.push(JsStepResult {
          keyword: step.keyword.clone(),
          text: step.text.clone(),
          status,
        });
      }
    }

    // After hooks always run (cleanup), even on failure.
    if let Err(msg) = self.run_hooks("After", Some(&scenario.tags)).await {
      steps.push(JsStepResult {
        keyword: "After".into(),
        text: "hook".into(),
        status: JsStepStatus::Failed(msg),
      });
      failed = true;
    }

    JsScenarioResult {
      name: scenario.name.clone(),
      tags: scenario.tags.clone(),
      passed: !failed,
      steps,
    }
  }
}

/// Build a [`StepHandler`] that dispatches step `idx` back into the
/// shared VM. The World (with this scenario's `page`/`context`) was
/// already installed by [`set_scenario_world`]; the closure is
/// fixture-agnostic — it only forwards cucumber-extracted args.
fn js_step_handler(actx: AsyncContext, idx: usize, source: String) -> StepHandler {
  Arc::new(move |_world, params, table, doc| {
    let actx = actx.clone();
    let source = source.clone();
    let params_json: Vec<serde_json::Value> = params.iter().map(step_param_to_json).collect();
    let data_table: Option<Vec<Vec<String>>> = table.map(|t| t.raw().to_vec());
    let doc_string: Option<String> = doc.map(str::to_string);
    Box::pin(async move {
      match invoke_step(
        &actx,
        idx,
        &params_json,
        data_table.as_deref(),
        doc_string.as_deref(),
        &source,
      )
      .await
      {
        Ok(StepOutcome::Passed | StepOutcome::Skipped) => Ok(()),
        Ok(StepOutcome::Pending) => Err(StepError::pending("step returned 'pending'")),
        Err(e) => Err(StepError::from(fmt_script_error(&e))),
      }
    })
  })
}

fn step_param_to_json(p: &StepParam) -> serde_json::Value {
  match p {
    StepParam::String(s) | StepParam::Word(s) => serde_json::Value::String(s.clone()),
    StepParam::Int(i) => serde_json::Value::Number((*i).into()),
    StepParam::Float(f) => serde_json::Number::from_f64(*f)
      .map(serde_json::Value::Number)
      .unwrap_or(serde_json::Value::Null),
    StepParam::Custom { value, .. } => serde_json::Value::String(value.clone()),
  }
}

fn fmt_script_error(e: &ferridriver_script::ScriptError) -> String {
  let mut m = e.message.clone();
  if let Some(line) = e.line {
    m.push_str(&format!(" (at {JS_STEP_LOCATION}:{line}"));
    if let Some(col) = e.column {
      m.push_str(&format!(":{col}"));
    }
    m.push(')');
  }
  if let Some(snippet) = &e.source_snippet {
    m.push('\n');
    m.push_str(snippet);
  }
  // QuickJS does not expose `lineNumber` as an own property on a plain
  // `throw new Error(...)`, so the precise location lives in the JS
  // stack — surface it so a failing step points at its `.js` source.
  if let Some(stack) = &e.stack {
    if !stack.trim().is_empty() {
      m.push('\n');
      m.push_str(stack.trim_end());
    }
  }
  m
}
