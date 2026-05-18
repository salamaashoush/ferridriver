//! JavaScript step definitions driven by the shared QuickJS engine.
//!
//! `ferridriver-script` owns the VM and every binding (`page`,
//! `locator`, ...); this module loads cucumber-js-shaped `.js` step
//! files into that VM as ES modules (so shared `import './helpers.js'`
//! works) and drives them from the Rust BDD core
//! (`feature`/`scenario`/`filter`/`registry`). No matching, outline
//! expansion or tag logic lives here.
//!
//! The cucumber World is a first-class object; `ferridriver-script`'s
//! `install_*_on` helpers install `page`/`context`/`request`/`browser`
//! onto a per-scenario World (the step `this`) — the same bindings
//! scripting installs onto `globalThis`. The step registry is per-VM
//! JavaScript state, so one engine session is created per
//! `ferridriver-test` worker: scenarios run in parallel across workers,
//! each VM driving its own scenarios.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ferridriver_script::{
  AsyncContext, InMemoryVars, PathSandbox, RunContext, ScenarioWorld, ScriptEngineConfig, Session, StepOutcome,
  collect_registry, invoke_hook, invoke_step, reset_world, set_scenario_world,
};
use ferridriver_test::model::{StepCategory, TestInfo};
use tokio::sync::Mutex;

use crate::feature::FeatureSet;
use crate::filter::TagExpression;
use crate::param_type::CustomParamType;
use ferridriver_test::FixturePool;
use crate::registry::StepRegistry;
use crate::scenario::ScenarioExecution;
use crate::step::{StepError, StepHandler, StepKind, StepLocation, StepParam};
use crate::world::BrowserWorld;

const JS_STEP_LOCATION: &str = "<js-step>";

const DEFAULT_STEP_GLOBS: &[&str] = &["steps/**/*.js", "step_definitions/**/*.js"];

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
  pub line: usize,
  pub duration: Duration,
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
/// `ferridriver-test` worker).
pub struct JsBddSession {
  session: Session,
  registry: Arc<StepRegistry>,
  hooks: Vec<(usize, String, Option<TagExpression>)>,
  source_label: String,
}

/// Discover step files for the given globs (relative globs are resolved
/// against `cwd`). Empty globs fall back to the cucumber-js-style
/// defaults.
pub fn discover_step_files(globs: &[String], cwd: &Path) -> Vec<PathBuf> {
  let patterns: Vec<String> = if globs.is_empty() {
    DEFAULT_STEP_GLOBS.iter().map(|s| (*s).to_string()).collect()
  } else {
    globs.to_vec()
  };
  let mut files = Vec::new();
  for pat in patterns {
    let full = if Path::new(&pat).is_absolute() {
      pat.clone()
    } else {
      cwd.join(&pat).to_string_lossy().into_owned()
    };
    if let Ok(entries) = glob::glob(&full) {
      for entry in entries.flatten() {
        if entry.extension().and_then(|e| e.to_str()) == Some("js") {
          files.push(entry);
        }
      }
    }
  }
  files.sort();
  files.dedup();
  files
}

impl JsBddSession {
  #[must_use]
  pub fn registry(&self) -> Arc<StepRegistry> {
    Arc::clone(&self.registry)
  }

  /// Create a shared-engine session, load every step file as an ES
  /// module (trusted filesystem resolution so `import './helpers.js'`
  /// works), and build the Rust step registry from what they
  /// registered.
  pub async fn load(globs: &[String], cwd: &Path) -> anyhow::Result<Self> {
    let files = discover_step_files(globs, cwd);
    if files.is_empty() {
      let pats: Vec<&str> = if globs.is_empty() {
        DEFAULT_STEP_GLOBS.to_vec()
      } else {
        globs.iter().map(String::as_str).collect()
      };
      anyhow::bail!("no JS step files found (globs: {:?}, cwd: {})", pats, cwd.display());
    }

    let sandbox = Arc::new(
      PathSandbox::new(cwd).map_err(|e| anyhow::anyhow!("sandbox {}: {}", cwd.display(), e.message))?,
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
      trusted_modules: true,
    };

    let session = Session::create(ScriptEngineConfig::default(), &run_ctx)
      .await
      .map_err(|e| anyhow::anyhow!("session create: {}", e.message))?;

    let source_label = files
      .iter()
      .map(|f| f.display().to_string())
      .collect::<Vec<_>>()
      .join(", ");

    // Evaluate each step file as an ES module. The module name is
    // cwd-relative so a file's own `import './helpers.js'` resolves
    // through the filesystem resolver next to it.
    let actx = session.async_context();
    for f in &files {
      let name = f
        .strip_prefix(cwd)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| f.to_string_lossy().into_owned());
      let src = std::fs::read_to_string(f)
        .map_err(|e| anyhow::anyhow!("read step file {}: {e}", f.display()))?;
      ferridriver_script::evaluate_module(&actx, &name, &src)
        .await
        .map_err(|e| anyhow::anyhow!("step file {} failed to load: {}", f.display(), fmt_script_error(&e)))?;
    }
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
      let handler = js_step_handler(actx.clone(), idx, source_label.clone());
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

    let hooks = snapshot
      .hooks
      .iter()
      .enumerate()
      .map(|(i, h)| {
        let te = h.tags.as_deref().and_then(|t| TagExpression::parse(t).ok());
        (i, h.hook_type.clone(), te)
      })
      .collect();

    let session = Self {
      session,
      registry: Arc::new(registry),
      hooks,
      source_label,
    };
    session.run_hooks("BeforeAll", None).await.map_err(|e| anyhow::anyhow!(e))?;
    Ok(session)
  }

  async fn run_hooks(&self, kind: &str, tags: Option<&[String]>) -> Result<(), String> {
    let actx = self.session.async_context();
    let mut hooks: Vec<(usize, Option<&TagExpression>)> = self
      .hooks
      .iter()
      .filter(|(_, k, _)| k == kind)
      .map(|(i, _, te)| (*i, te.as_ref()))
      .collect();
    if kind == "After" || kind == "AfterAll" {
      hooks.reverse();
    }
    for (idx, te) in hooks {
      let applies = match (te, tags) {
        (Some(expr), Some(t)) => expr.matches(t),
        (Some(_), None) => false,
        (None, _) => true,
      };
      if !applies {
        continue;
      }
      if let Err(e) = invoke_hook(&actx, idx, &self.source_label).await {
        return Err(fmt_script_error(&e));
      }
    }
    Ok(())
  }

  /// Run-level `AfterAll` hooks (once per worker session).
  pub async fn after_all(&self) -> Result<(), String> {
    self.run_hooks("AfterAll", None).await
  }

  /// Execute one expanded scenario: bind its World from the fixtures,
  /// run `Before` hooks, the steps, then `After` hooks.
  pub async fn run_scenario(&self, scenario: &ScenarioExecution, world: &mut BrowserWorld) -> JsScenarioResult {
    let actx = self.session.async_context();

    let fixtures = world.fixtures();
    let sw = ScenarioWorld {
      page: Some(Arc::clone(&fixtures.page)),
      context: Some(Arc::clone(&fixtures.context)),
      request: Some(Arc::clone(&fixtures.request)),
      browser: Some(Arc::clone(&fixtures.browser)),
    };

    let _ = reset_world(&actx).await;
    if let Err(e) = set_scenario_world(&actx, &sw).await {
      return JsScenarioResult {
        name: scenario.name.clone(),
        tags: scenario.tags.clone(),
        steps: vec![JsStepResult {
          keyword: "World".into(),
          text: "bind fixtures".into(),
          line: 0,
          duration: Duration::ZERO,
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
        line: 0,
        duration: Duration::ZERO,
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
            line: step.line,
            duration: Duration::ZERO,
            status: JsStepStatus::Skipped,
          });
          continue;
        }
        let started = Instant::now();
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
          line: step.line,
          duration: started.elapsed(),
          status,
        });
      }
    }

    // After hooks always run (cleanup), even on failure.
    if let Err(msg) = self.run_hooks("After", Some(&scenario.tags)).await {
      steps.push(JsStepResult {
        keyword: "After".into(),
        text: "hook".into(),
        line: 0,
        duration: Duration::ZERO,
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

// ── Per-worker session cache + TestRunner integration ────────────────

type WorkerSessions = Mutex<HashMap<u32, Arc<JsBddSession>>>;
static WORKER_SESSIONS: OnceLock<WorkerSessions> = OnceLock::new();

async fn worker_session(
  worker_index: u32,
  globs: Arc<Vec<String>>,
  cwd: Arc<PathBuf>,
) -> Result<Arc<JsBddSession>, String> {
  let cache = WORKER_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()));
  let mut map = cache.lock().await;
  if let Some(s) = map.get(&worker_index) {
    return Ok(Arc::clone(s));
  }
  let session = JsBddSession::load(&globs, &cwd).await.map_err(|e| e.to_string())?;
  let session = Arc::new(session);
  map.insert(worker_index, Arc::clone(&session));
  Ok(session)
}

async fn record_step(test_info: &TestInfo, s: &JsStepResult) {
  use ferridriver_test::model::StepStatus as S;
  let title = format!("{}{}", s.keyword, s.text);
  let (status, error) = match &s.status {
    JsStepStatus::Passed => (S::Passed, None),
    JsStepStatus::Skipped => (S::Skipped, None),
    JsStepStatus::Pending => (S::Pending, None),
    JsStepStatus::Undefined(m) => (S::Pending, Some(m.clone())),
    JsStepStatus::Failed(m) => (S::Failed, Some(m.clone())),
  };
  let meta = serde_json::json!({
    "bdd_keyword": s.keyword.trim(),
    "bdd_text": s.text,
    "bdd_line": s.line,
  });
  test_info
    .record_step(title, StepCategory::TestStep, status, s.duration, error, Some(meta))
    .await;
}

/// Translate parsed Gherkin features into a `TestPlan` whose scenarios
/// execute through per-worker JS sessions. Reuses the core
/// `feature`/`scenario`/`filter` expansion and the shared
/// annotation/ordering helpers — only the per-scenario `test_fn`
/// differs from the Rust-step path.
pub fn translate_features_js(
  feature_set: &FeatureSet,
  config: &ferridriver_test::config::TestConfig,
  globs: Vec<String>,
  cwd: PathBuf,
) -> ferridriver_test::model::TestPlan {
  use ferridriver_test::model::{ExpectedStatus, Hooks, SuiteMode, TestCase, TestFailure, TestFn, TestId, TestSuite};

  let globs = Arc::new(globs);
  let cwd = Arc::new(cwd);
  let mut suites = Vec::new();

  for feature in &feature_set.features {
    let scenarios = crate::scenario::expand_feature(feature);
    if scenarios.is_empty() {
      continue;
    }
    let feature_name = feature.feature.name.clone();
    let feature_path = feature.path.display().to_string();
    let is_serial = scenarios.iter().any(|s| s.tags.iter().any(|t| t == "@serial"));

    let mut tests = Vec::new();
    for scenario in &scenarios {
      let scenario_clone = scenario.clone();
      let globs = Arc::clone(&globs);
      let cwd = Arc::clone(&cwd);
      let browser_config = config.browser.clone();

      let test_fn: TestFn = Arc::new(move |pool: FixturePool| {
        let scenario = scenario_clone.clone();
        let globs = Arc::clone(&globs);
        let cwd = Arc::clone(&cwd);
        let browser_config = browser_config.clone();
        Box::pin(async move {
          let browser = pool
            .get("browser")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'browser' failed", e))?;
          let page = pool
            .get("page")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'page' failed", e))?;
          let context = pool
            .get("context")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'context' failed", e))?;
          let test_info: Arc<TestInfo> = pool
            .get("test_info")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'test_info' failed", e))?;
          let request = pool
            .get("request")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'request' failed", e))?;

          let session = worker_session(test_info.worker_index, globs, cwd)
            .await
            .map_err(|e| TestFailure::from(format!("JS step load failed: {e}")))?;

          let fixtures = ferridriver_test::model::TestFixtures {
            browser,
            page,
            context,
            request,
            test_info: Arc::clone(&test_info),
            modifiers: Arc::new(ferridriver_test::model::TestModifiers::default()),
            browser_config,
            bdd_args: None,
            bdd_data_table: None,
            bdd_doc_string: None,
          };
          let mut world = BrowserWorld::new(fixtures);

          let result = session.run_scenario(&scenario, &mut world).await;
          for s in &result.steps {
            record_step(&test_info, s).await;
          }
          if result.passed {
            Ok(())
          } else {
            let msg = result
              .steps
              .iter()
              .find_map(|s| match &s.status {
                JsStepStatus::Failed(m) | JsStepStatus::Undefined(m) => Some(m.clone()),
                JsStepStatus::Pending => Some(format!("pending: {}{}", s.keyword, s.text)),
                _ => None,
              })
              .unwrap_or_else(|| "scenario failed".to_string());
            Err(TestFailure::from(msg))
          }
        })
      });

      tests.push(TestCase {
        id: TestId {
          file: scenario.feature_path.display().to_string(),
          suite: Some(scenario.feature_name.clone()),
          name: scenario.name.clone(),
          line: crate::translate::scenario_line(scenario),
        },
        test_fn,
        fixture_requests: vec![
          "browser".to_string(),
          "context".to_string(),
          "page".to_string(),
          "test_info".to_string(),
          "request".to_string(),
        ],
        annotations: crate::translate::scenario_annotations(scenario),
        timeout: None,
        retries: None,
        expected_status: ExpectedStatus::Pass,
        use_options: None,
      });
    }

    suites.push(TestSuite {
      name: feature_name,
      file: feature_path,
      tests,
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: if is_serial {
        SuiteMode::Serial
      } else {
        SuiteMode::Parallel
      },
    });
  }

  let total_tests = suites.iter().map(|s| s.tests.len()).sum();
  ferridriver_test::model::TestPlan {
    suites,
    total_tests,
    shard: None,
  }
}
