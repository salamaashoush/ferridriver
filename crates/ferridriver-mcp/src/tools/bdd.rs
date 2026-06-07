//! BDD tool -- run Gherkin features (inline text or files) on the SAME
//! live MCP session as `run_script` / `snapshot` / `click`.
//!
//! This is NOT the test runner. The core `TestRunner` exists to isolate
//! each scenario in its own browser/context with parallel workers,
//! retries and sharding -- the wrong tool for an interactive session,
//! where the agent has already navigated/logged-in on a live page and
//! wants the scenario to run against THAT page.
//!
//! What is reused is the actual BDD step engine -- the exact primitives
//! the CLI's per-scenario execution calls: feature parsing
//! (`feature::FeatureSet`), scenario expansion + tag filtering
//! (`scenario`/`filter`), the step registry + cucumber-expression
//! matching, hooks, the `BrowserWorld`, and JS/TS step bodies via the
//! rolldown -> `QuickJS` bundle (`js::JsBddSession`). Built-in Rust steps
//! run through `executor::ScenarioExecutor`; JS/TS steps through
//! `js::JsBddSession`. Both drive a single `BrowserWorld` built from the
//! session's live page/context/request/browser, scenario after scenario.

use std::sync::Arc;
use std::time::Duration;

use rmcp::{
  ErrorData,
  handler::server::wrapper::Parameters,
  model::{CallToolResult, Content},
  tool, tool_router,
};
use serde::{Deserialize, Serialize};

use ferridriver_bdd::executor::ScenarioExecutor;
use ferridriver_bdd::feature::FeatureSet;
use ferridriver_bdd::filter::TagExpression;
use ferridriver_bdd::js::{JsScenarioResult, JsStepStatus, set_bdd_script_caps, set_bdd_sidecars};
use ferridriver_bdd::scenario::{ScenarioExecution, ScenarioResult, ScenarioStatus, expand_feature};
use ferridriver_bdd::world::BrowserWorld;
use ferridriver_test::model::{TestFixtures, TestInfo, TestModifiers};

use crate::bdd_engine::logical_key;
use crate::server::{McpServer, sess};

// ── Parameter type ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunBddParams {
  #[schemars(description = "Inline Gherkin feature text to run. Mutually exclusive with `features`. \
    Example: 'Feature: Login\\n  Scenario: Valid login\\n    Given I navigate to \"https://example.com\"\\n    \
    Then \"h1\" should contain text \"Example\"'. Parsed as a single feature.")]
  pub gherkin: Option<String>,

  #[schemars(description = "Feature file globs or paths, e.g. ['tests/features/**/*.feature']. A directory \
    expands to '<dir>/**/*.feature'. Mutually exclusive with `gherkin`. When both are omitted, the configured \
    [test].features globs are used. Globs resolve against the MCP server's working directory.")]
  pub features: Option<Vec<String>>,

  #[schemars(description = "JavaScript/TypeScript step-definition file globs (cucumber-js style), e.g. \
    ['steps/**/*.ts']. Overrides [test].steps. These are ADDED ON TOP of the always-available built-in Rust \
    steps, so a scenario can mix both. Resolved against the server's working directory.")]
  pub steps: Option<Vec<String>>,

  #[schemars(description = "Session identifier (same as other tools, e.g. 'default' or 'instance:context'). \
    The scenario runs on this session's CURRENT live page, with its existing cookies, storage and navigation \
    state -- the same session `run_script` / `snapshot` / `click` use. Default: 'default'.")]
  pub session: Option<String>,

  #[schemars(description = "Tag filter expression, e.g. '@smoke and not @wip'. Omit to run all scenarios.")]
  pub tags: Option<String>,

  #[schemars(description = "Run only scenarios whose name contains this substring (case-insensitive).")]
  pub grep: Option<String>,

  #[schemars(description = "Parse and report scenarios + steps without executing them. Default false.")]
  pub dry_run: Option<bool>,

  #[schemars(description = "Stop after the first failing scenario. Default false.")]
  pub fail_fast: Option<bool>,

  #[schemars(description = "Treat undefined or pending steps as failures. Default false (or the [test] value).")]
  pub strict: Option<bool>,

  #[schemars(description = "Per-step timeout in milliseconds (built-in Rust steps). Defaults to [test].timeout.")]
  pub step_timeout: Option<u64>,

  #[schemars(description = "Gherkin keyword language for parsing feature FILES (e.g. 'en', 'de', 'fr'). \
    Inline gherkin uses a '# language: xx' directive instead.")]
  pub language: Option<String>,

  #[schemars(description = "Cucumber world parameters as a JSON object, exposed to JS steps as `this.parameters`. \
    Overrides [test].worldParameters.")]
  pub world_parameters: Option<serde_json::Value>,
}

// ── Unified result shape (Rust + JS paths map into this) ────────────────────

#[derive(Serialize)]
struct StepJson {
  keyword: String,
  text: String,
  status: String,
  duration_ms: u128,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<String>,
}

#[derive(Serialize)]
struct ScenarioJson {
  name: String,
  status: String,
  duration_ms: u128,
  tags: Vec<String>,
  steps: Vec<StepJson>,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<String>,
}

#[derive(Serialize)]
struct BddRunResult {
  status: &'static str,
  total: usize,
  passed: usize,
  failed: usize,
  skipped: usize,
  duration_ms: u128,
  scenarios: Vec<ScenarioJson>,
}

impl From<ScenarioResult> for ScenarioJson {
  fn from(r: ScenarioResult) -> Self {
    let status = match r.status {
      ScenarioStatus::Passed => "passed",
      ScenarioStatus::Failed => "failed",
      ScenarioStatus::Skipped => "skipped",
      ScenarioStatus::Undefined => "undefined",
    };
    let steps = r
      .steps
      .into_iter()
      .map(|s| StepJson {
        keyword: s.keyword,
        text: s.text,
        status: format!("{:?}", s.status).to_lowercase(),
        duration_ms: s.duration.as_millis(),
        error: s.error,
      })
      .collect();
    ScenarioJson {
      name: r.scenario_name,
      status: status.to_string(),
      duration_ms: r.duration.as_millis(),
      tags: r.tags,
      steps,
      error: r.error,
    }
  }
}

impl From<JsScenarioResult> for ScenarioJson {
  fn from(r: JsScenarioResult) -> Self {
    let mut scenario_error = None;
    let steps = r
      .steps
      .into_iter()
      .map(|s| {
        let (status, error) = match s.status {
          JsStepStatus::Passed => ("passed", None),
          JsStepStatus::Skipped => ("skipped", None),
          JsStepStatus::Pending => ("pending", None),
          JsStepStatus::Failed(e) => ("failed", Some(e)),
          JsStepStatus::Undefined(e) => ("undefined", Some(e)),
        };
        if let Some(ref e) = error {
          if scenario_error.is_none() {
            scenario_error = Some(format!("{}{}: {e}", s.keyword, s.text));
          }
        }
        StepJson {
          keyword: s.keyword,
          text: s.text,
          status: status.to_string(),
          duration_ms: s.duration.as_millis(),
          error,
        }
      })
      .collect();
    ScenarioJson {
      name: r.name,
      status: if r.passed { "passed".into() } else { "failed".into() },
      duration_ms: 0,
      tags: r.tags,
      steps,
      error: scenario_error,
    }
  }
}

// ── Tool implementation ─────────────────────────────────────────────────────

#[tool_router(router = bdd_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "run_bdd",
    description = "Run BDD/Gherkin features on the CURRENT live browser session (the same session as \
    run_script / snapshot / click) -- not in an isolated test-runner browser. The scenario executes against \
    the session's existing page, cookies and navigation state, so you can navigate/log in first and then run \
    a scenario against that page, then snapshot the result. \
    Provide `gherkin` (inline feature text) OR `features` (file globs); omit both to use the configured \
    [test].features. The 109 built-in Rust steps (navigation, clicks, fills, assertions, cookies, network, ...) \
    are ALWAYS available; JS/TS step files passed via `steps` (rolldown -> QuickJS, cucumber-js style, \
    disk-cached) are added on top, so a single scenario can freely mix built-in and custom JS/TS steps. With no \
    steps or extensions, only the built-in library is used. Uses the same step engine, cucumber expression \
    matching and hooks as the `ferridriver bdd` CLI. \
    Supports tags, grep (scenario-name filter), dry_run, fail_fast, strict, step_timeout, language, and \
    world_parameters. Scenarios run sequentially on the shared page (state is reset between scenarios). \
    Returns { status, total, passed, failed, skipped, duration_ms, scenarios[] } with per-step results; a \
    short human summary is prepended."
  )]
  async fn run_bdd(&self, Parameters(p): Parameters<RunBddParams>) -> Result<CallToolResult, ErrorData> {
    let session = sess(p.session.as_ref()).to_string();
    // Serialize with run_script / navigation on the same session so the
    // scenario doesn't interleave with another mutation of the live page.
    let _guard = self.session_guard(&session).await;

    // ── 1. Parse features (inline text OR file globs). ──
    let feature_set = if let Some(text) = p.gherkin.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
      if p.features.as_ref().is_some_and(|f| !f.is_empty()) {
        return Err(McpServer::err("run_bdd accepts `gherkin` OR `features`, not both"));
      }
      FeatureSet::parse_text(text).map_err(|e| McpServer::err(format!("invalid Gherkin: {e}")))?
    } else {
      let globs = match p.features {
        Some(f) if !f.is_empty() => f,
        _ => self.test_config.features.clone(),
      };
      if globs.is_empty() {
        return Err(McpServer::err(
          "run_bdd needs `gherkin`, `features`, or a configured [test].features list",
        ));
      }
      let files = FeatureSet::discover(&globs, &self.test_config.test_ignore)
        .map_err(|e| McpServer::err(format!("feature discovery: {e}")))?;
      if files.is_empty() {
        return Ok(CallToolResult::success(vec![Content::text(format!(
          "run_bdd: no .feature files matched {globs:?}"
        ))]));
      }
      FeatureSet::parse_with_language(files, p.language.as_deref())
        .map_err(|e| McpServer::err(format!("feature parse: {e}")))?
    };

    // ── 2. Expand + filter scenarios. ──
    let mut scenarios: Vec<ScenarioExecution> = feature_set.features.iter().flat_map(expand_feature).collect();
    if let Some(g) = p.grep.as_deref() {
      let lower = g.to_lowercase();
      scenarios.retain(|s| s.name.to_lowercase().contains(&lower));
    }
    if let Some(expr) = p.tags.as_deref() {
      let parsed = TagExpression::parse(expr).map_err(|e| McpServer::err(format!("invalid tag expression: {e}")))?;
      scenarios.retain(|s| parsed.matches(&s.tags));
    }
    if scenarios.is_empty() {
      return Ok(CallToolResult::success(vec![Content::text(
        "run_bdd: no scenarios matched the given filters",
      )]));
    }

    // ── 3. Dry run: report the plan without executing or touching the browser. ──
    if p.dry_run.unwrap_or(false) {
      let plan: Vec<ScenarioJson> = scenarios
        .iter()
        .map(|s| ScenarioJson {
          name: s.name.clone(),
          status: "skipped".into(),
          duration_ms: 0,
          tags: s.tags.clone(),
          steps: s
            .steps
            .iter()
            .map(|st| StepJson {
              keyword: st.keyword.clone(),
              text: st.text.clone(),
              status: "skipped".into(),
              duration_ms: 0,
              error: None,
            })
            .collect(),
          error: None,
        })
        .collect();
      let total = plan.len();
      let result = BddRunResult {
        status: "passed",
        total,
        passed: 0,
        failed: 0,
        skipped: total,
        duration_ms: 0,
        scenarios: plan,
      };
      return Ok(finish(format!("BDD dry-run: {total} scenario(s) parsed"), &result));
    }

    // ── 4. Build a BrowserWorld from the session's live handles. ──
    let (page, ctx_ref) = Box::pin(self.page_and_context(&session)).await?;
    let request = Arc::new(ferridriver::http_client::HttpClient::new(
      ferridriver::http_client::HttpClientOptions::default(),
    ));
    let browser = Arc::new(ferridriver::Browser::from_shared_state(self.state.state_arc()));
    let fixtures = TestFixtures {
      browser,
      page,
      context: Arc::new(ctx_ref),
      request,
      test_info: Arc::new(TestInfo::new_anonymous()),
      modifiers: Arc::new(TestModifiers::default()),
      browser_config: self.test_config.browser.clone(),
      bdd_args: None,
      bdd_data_table: None,
      bdd_doc_string: None,
    };
    let mut world = BrowserWorld::new(fixtures);

    let strict = p.strict.unwrap_or(self.test_config.strict);
    let step_timeout = Duration::from_millis(p.step_timeout.unwrap_or(self.test_config.timeout));
    let fail_fast = p.fail_fast.unwrap_or(false);
    let world_params = p
      .world_parameters
      .unwrap_or_else(|| self.test_config.world_parameters.clone());

    // ── 5. Choose the step engine: JS/TS bundle, or built-in Rust steps. ──
    let js_globs = match p.steps {
      Some(s) if !s.is_empty() => s,
      _ => self.test_config.steps.clone(),
    };
    let extensions = self.extension_specs.clone();
    let use_js = !js_globs.is_empty() || !extensions.is_empty();

    let mut out: Vec<ScenarioJson> = Vec::with_capacity(scenarios.len());

    if use_js {
      // Thread scripting caps + declared sidecars into the BDD step VM
      // (idempotent OnceLock; identical values every call).
      set_bdd_script_caps(self.script_caps.clone());
      set_bdd_sidecars(self.script_engine.config().sidecars.clone());
      let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

      // Lock the single cached engine. The lock is the build+run guard: a
      // single VM (one global world binding) can't run scenarios
      // concurrently, so all run_bdd JS calls serialize here. `ensure`
      // reuses the warm engine when the step-set + sources are unchanged
      // (mtime fast-path) and reloads only on a step-set or source change.
      let key = logical_key(&js_globs, &extensions, &world_params);
      let mut engine = self.bdd_engine.lock().await;
      let session = engine
        .ensure(key, &js_globs, &extensions, world_params, &cwd)
        .await
        .map_err(|e| McpServer::err(format!("step engine: {e}")))?;
      for sc in &scenarios {
        world.reset_scenario_state();
        let r = session.run_scenario(sc, &mut world).await;
        let failed = !r.passed;
        out.push(r.into());
        if fail_fast && failed {
          break;
        }
      }
      // Hold `engine` until here: the lock is the run guard.
      drop(engine);
    } else {
      // Built-in Rust steps: reuse the process-wide immutable registry.
      let executor = ScenarioExecutor::new(
        self.builtin_registry(),
        step_timeout,
        strict,
        self.test_config.screenshot_on_failure,
      );
      for sc in &scenarios {
        world.reset_scenario_state();
        let r = executor.run_scenario(&mut world, sc).await;
        let failed = matches!(r.status, ScenarioStatus::Failed | ScenarioStatus::Undefined);
        out.push(r.into());
        if fail_fast && failed {
          break;
        }
      }
    }

    // ── 6. Aggregate + return. ──
    let passed = out.iter().filter(|s| s.status == "passed").count();
    let failed = out.iter().filter(|s| s.status == "failed" || s.status == "undefined").count();
    let total = out.len();
    let skipped = total - passed - failed;
    let duration_ms: u128 = out.iter().map(|s| s.duration_ms).sum();
    let result = BddRunResult {
      status: if failed == 0 { "passed" } else { "failed" },
      total,
      passed,
      failed,
      skipped,
      duration_ms,
      scenarios: out,
    };
    let summary = bdd_summary(&result);
    Ok(finish(summary, &result))
  }
}

/// Build the tool result: human summary block + machine-readable JSON block.
fn finish(summary: String, result: &BddRunResult) -> CallToolResult {
  let json = serde_json::to_string_pretty(result).unwrap_or_else(|e| format!("{{\"error\":\"serialize: {e}\"}}"));
  CallToolResult::success(vec![Content::text(summary), Content::text(json)])
}

fn bdd_summary(r: &BddRunResult) -> String {
  use std::fmt::Write as _;
  let mut out = format!(
    "BDD {}: {} passed, {} failed, {} skipped of {} ({} ms)",
    if r.failed == 0 { "PASS" } else { "FAIL" },
    r.passed,
    r.failed,
    r.skipped,
    r.total,
    r.duration_ms,
  );
  for s in &r.scenarios {
    if s.status == "failed" || s.status == "undefined" {
      let err = s.error.as_deref().and_then(|e| e.lines().next()).unwrap_or("");
      let _ = write!(out, "\n  [FAIL] {}: {err}", s.name);
    }
  }
  out
}
