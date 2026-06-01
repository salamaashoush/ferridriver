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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use ferridriver_script::{
  AsyncContext, CompiledBundle, HookArg, InMemoryVars, JsArg, PathSandbox, RunContext, ScenarioWorld, ScriptAttachment,
  ScriptEngineConfig, Session, StepOutcome, bundle_and_compile, collect_registry, drain_attachments, eval_bundle,
  invoke_hook, invoke_step, is_source_file, reset_world, set_scenario_world, walk_source_files,
};
use ferridriver_test::FixturePool;
use ferridriver_test::model::{AttachmentBody, StepCategory, TestInfo};
use tokio::sync::OnceCell;

use crate::feature::FeatureSet;
use crate::filter::TagExpression;
use crate::param_type::CustomParamType;
use crate::registry::StepRegistry;
use crate::scenario::ScenarioExecution;
use crate::step::{StepError, StepHandler, StepKind, StepLocation, StepParam};
use crate::world::BrowserWorld;

const JS_STEP_LOCATION: &str = "<js-step>";

const DEFAULT_STEP_GLOBS: &[&str] = &[
  "steps/**/*.js",
  "steps/**/*.ts",
  "step_definitions/**/*.js",
  "step_definitions/**/*.ts",
];

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
  bundle: Arc<CompiledBundle>,
  /// Cucumber `--world-parameters` exposed to every scenario as
  /// `this.parameters`.
  world_parameters: serde_json::Value,
}

/// Discover step entry files for the given globs (relative globs are
/// resolved against `cwd`). Empty globs fall back to the cucumber-js
/// defaults. `.ts`/`.tsx` are included — rolldown transpiles them.
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
        if is_source_file(&entry) {
          files.push(entry);
        }
      }
    }
  }
  files.sort();
  files.dedup();
  files
}

/// Discover extension entry files. Each path is a single source file or
/// a directory scanned **recursively** for
/// [`ferridriver_script::SOURCE_EXTENSIONS`] files (same rule the MCP
/// plugin loader uses, so one extension serves both hosts). A file the
/// user named explicitly is taken as-is regardless of extension.
pub fn discover_extension_files(paths: &[String], cwd: &Path) -> Vec<PathBuf> {
  let mut files = Vec::new();
  for raw in paths {
    let p = if Path::new(raw).is_absolute() {
      PathBuf::from(raw)
    } else {
      cwd.join(raw)
    };
    match std::fs::metadata(&p) {
      Ok(m) if m.is_file() => files.push(p),
      Ok(m) if m.is_dir() => files.extend(walk_source_files(&p)),
      _ => {},
    }
  }
  files.sort();
  files.dedup();
  files
}

/// Discover the step entry files and rolldown-bundle + tree-shake +
/// transpile them (plus `node_modules` / shared utils) into one module
/// compiled to bytecode, once, before workers spawn.
pub async fn bundle_steps(globs: &[String], cwd: &Path) -> anyhow::Result<Arc<CompiledBundle>> {
  bundle_steps_with(globs, &[], cwd).await
}

/// Like [`bundle_steps`] but also bundles the configured `extensions`
/// (top-level config) into the same module, so an extension's BDD steps
/// are available to the test runner exactly like a step file's.
pub async fn bundle_steps_with(
  globs: &[String],
  extensions: &[String],
  cwd: &Path,
) -> anyhow::Result<Arc<CompiledBundle>> {
  let mut files = discover_step_files(globs, cwd);
  files.extend(discover_extension_files(extensions, cwd));
  files.sort();
  files.dedup();
  if files.is_empty() {
    let pats: Vec<&str> = if globs.is_empty() {
      DEFAULT_STEP_GLOBS.to_vec()
    } else {
      globs.iter().map(String::as_str).collect()
    };
    anyhow::bail!(
      "no step or extension files found (globs: {:?}, extensions: {:?}, cwd: {})",
      pats,
      extensions,
      cwd.display()
    );
  }
  let bundle = bundle_and_compile(&files, cwd)
    .await
    .map_err(|e| anyhow::anyhow!("bundle step/extension files: {}", e.message))?;
  Ok(Arc::new(bundle))
}

/// Forward the scenario's queued `this.attach`/`this.log` attachments
/// into the test result so the messages / HTML / Allure reporters
/// surface them (the Cucumber screenshot-/text-on-failure idiom). The
/// name is derived from the media type (Cucumber attachments are
/// unnamed).
async fn forward_attachments(test_info: &TestInfo, atts: Vec<ScriptAttachment>) {
  for a in atts {
    let name = if a.media_type.starts_with("image/") {
      "screenshot"
    } else if a.media_type.starts_with("text/x.cucumber.log") {
      "log"
    } else {
      "attachment"
    };
    test_info
      .attach(name.to_string(), a.media_type, AttachmentBody::Bytes(a.bytes))
      .await;
  }
}

impl JsBddSession {
  #[must_use]
  pub fn registry(&self) -> Arc<StepRegistry> {
    Arc::clone(&self.registry)
  }

  /// Drain attachments queued by `this.attach`/`this.log` during the
  /// just-run scenario (clears the queue for the next scenario).
  pub async fn drain_attachments(&self) -> Vec<ScriptAttachment> {
    drain_attachments(&self.session.async_context())
      .await
      .unwrap_or_default()
  }

  /// Discover, bundle and load step files in one call (convenience for
  /// single-session callers / tests). Production uses [`bundle_steps`]
  /// once + [`JsBddSession::load`] per worker.
  pub async fn from_globs(globs: &[String], cwd: &Path) -> anyhow::Result<Self> {
    let bundle = bundle_steps(globs, cwd).await?;
    Self::load(bundle, cwd, serde_json::Value::Null).await
  }

  /// Create a shared-engine session and link the precompiled bundled
  /// step module (one `Module::load`, no parse, no resolver — imports
  /// are inlined by rolldown). Builds the Rust step registry from what
  /// the module registered.
  pub async fn load(
    bundle: Arc<CompiledBundle>,
    cwd: &Path,
    world_parameters: serde_json::Value,
  ) -> anyhow::Result<Self> {
    let sandbox =
      Arc::new(PathSandbox::new(cwd).map_err(|e| anyhow::anyhow!("sandbox {}: {}", cwd.display(), e.message))?);
    let run_ctx = RunContext {
      vars: Arc::new(InMemoryVars::new()),
      sandbox,
      artifacts: None,
      page: None,
      browser_context: None,
      request: None,
      browser: None,
      plugins: Vec::new(),
      trusted_modules: false,
      host: ferridriver_script::ExtensionHost::Bdd,
      // `[scripting]` caps threaded from resolved config by the
      // `ferridriver bdd` entry (`set_bdd_script_caps`), exactly like
      // the MCP server. Unset (macro/harness path with no config) ⇒
      // locked down — the safe default.
      caps: BDD_SCRIPT_CAPS.get().cloned().unwrap_or_default(),
    };

    let engine_config = ScriptEngineConfig {
      sidecars: BDD_SIDECARS.get().cloned().unwrap_or_default(),
      ..Default::default()
    };
    let session = Session::create(engine_config, &run_ctx)
      .await
      .map_err(|e| anyhow::anyhow!("session create: {}", e.message))?;

    // Link the single bundled module (top-level Given/When/Then run).
    let actx = session.async_context();
    eval_bundle(&actx, &bundle)
      .await
      .map_err(|e| anyhow::anyhow!("step bundle failed to load: {}", fmt_script_error(&bundle, &e)))?;
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
      let handler = js_step_handler(actx.clone(), idx, Arc::clone(&bundle));
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
      bundle,
      world_parameters,
    };
    session
      .run_hooks("BeforeAll", None, None)
      .await
      .map_err(|e| anyhow::anyhow!(e))?;
    Ok(session)
  }

  async fn run_hooks(&self, kind: &str, tags: Option<&[String]>, arg: Option<&HookArg>) -> Result<(), String> {
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
      if let Err(e) = invoke_hook(&actx, idx, arg, &self.bundle.module_name).await {
        return Err(fmt_script_error(&self.bundle, &e));
      }
    }
    Ok(())
  }

  /// Run-level `AfterAll` hooks (once per worker session).
  pub async fn after_all(&self) -> Result<(), String> {
    self.run_hooks("AfterAll", None, None).await
  }

  /// Execute one expanded scenario: bind its World from the fixtures,
  /// run `Before` hooks, the steps, then `After` hooks.
  pub async fn run_scenario(&self, scenario: &ScenarioExecution, world: &mut BrowserWorld) -> JsScenarioResult {
    let actx = self.session.async_context();

    // Mirror the Rust executor: scope `world.resolve_fixture_path(...)`
    // to the feature file's directory so steps like
    // `I mock requests to "..." with fixture "mocks/page.html"` resolve
    // relative to the feature, not the process cwd. Without this the
    // JS-driven path resolves against cwd and every fixture-file step
    // errors with `No such file or directory`.
    if let Some(dir) = scenario.feature_path.parent() {
      world.set_feature_dir(dir.to_path_buf());
    }

    let fixtures = world.fixtures();
    let sw = ScenarioWorld {
      page: Some(Arc::clone(&fixtures.page)),
      context: Some(Arc::clone(&fixtures.context)),
      request: Some(Arc::clone(&fixtures.request)),
      browser: Some(Arc::clone(&fixtures.browser)),
      parameters: Some(self.world_parameters.clone()),
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

    let before_arg = HookArg {
      name: scenario.name.clone(),
      tags: scenario.tags.clone(),
      status: "PENDING".to_string(),
      message: None,
    };
    if let Err(msg) = self.run_hooks("Before", Some(&scenario.tags), Some(&before_arg)).await {
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
            // JS step authors need a JS snippet, not a Rust skeleton.
            let snip = crate::snippet::generate_js_snippet(
              &step.keyword,
              &step.text,
              step.table.is_some(),
              step.docstring.is_some(),
            );
            JsStepStatus::Undefined(format!("{e}\n\nImplement with:\n\n{snip}"))
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

    // After hooks always run (cleanup), even on failure. Pass the
    // scenario result so `After(s => { if (s.result.status === 'FAILED')
    // ... })` works (the screenshot-on-failure idiom).
    let after_msg = steps.iter().find_map(|s| match &s.status {
      JsStepStatus::Failed(m) | JsStepStatus::Undefined(m) => Some(m.clone()),
      JsStepStatus::Pending => Some(format!("pending: {}{}", s.keyword, s.text)),
      _ => None,
    });
    let after_arg = HookArg {
      name: scenario.name.clone(),
      tags: scenario.tags.clone(),
      status: if failed { "FAILED" } else { "PASSED" }.to_string(),
      message: after_msg,
    };
    if let Err(msg) = self.run_hooks("After", Some(&scenario.tags), Some(&after_arg)).await {
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

fn js_step_handler(actx: AsyncContext, idx: usize, bundle: Arc<CompiledBundle>) -> StepHandler {
  Arc::new(move |_world, params, table, doc| {
    let actx = actx.clone();
    let bundle = Arc::clone(&bundle);
    let params_json: Vec<JsArg> = params.iter().map(step_param_to_jsarg).collect();
    let data_table: Option<Vec<Vec<String>>> = table.map(|t| t.raw().to_vec());
    let doc_string: Option<String> = doc.map(str::to_string);
    Box::pin(async move {
      match invoke_step(
        &actx,
        idx,
        &params_json,
        data_table.as_deref(),
        doc_string.as_deref(),
        &bundle.module_name,
      )
      .await
      {
        Ok(StepOutcome::Passed | StepOutcome::Skipped) => Ok(()),
        Ok(StepOutcome::Pending) => Err(StepError::pending("step returned 'pending'")),
        Err(e) => Err(StepError::from(fmt_script_error(&bundle, &e))),
      }
    })
  })
}

fn step_param_to_jsarg(p: &StepParam) -> JsArg {
  match p {
    StepParam::String(s) | StepParam::Word(s) => JsArg::Str(s.clone()),
    StepParam::Int(i) => JsArg::Int(*i),
    StepParam::Float(f) => JsArg::Float(*f),
    StepParam::Custom { type_name, value } => JsArg::Custom {
      type_name: type_name.clone(),
      raw: value.clone(),
    },
  }
}

fn fmt_script_error(bundle: &CompiledBundle, e: &ferridriver_script::ScriptError) -> String {
  let mut m = e.message.clone();
  // Remap the bundled-output position back to the original .ts/.js
  // source via the rolldown source map.
  if let Some(line) = e.line {
    let col = e.column.unwrap_or(1);
    if let Some((src, sl, sc)) = bundle.remap(line, col) {
      m.push_str(&format!(" (at {src}:{sl}:{sc})"));
    } else {
      m.push_str(&format!(" (at {}:{line}:{col})", bundle.module_name));
    }
  }
  if let Some(snippet) = &e.source_snippet {
    m.push('\n');
    m.push_str(snippet);
  }
  // QuickJS does not expose `lineNumber` as an own property on a plain
  // `throw new Error(...)`; the location lives in the stack. Remap each
  // `<bundle>:line:col` frame back to the original .ts/.js source.
  if let Some(stack) = &e.stack {
    let stack = stack.trim_end();
    if !stack.is_empty() {
      m.push('\n');
      m.push_str(&remap_stack(bundle, stack));
    }
  }
  m
}

/// Rewrite `ferridriver-bdd-steps.js:LINE:COL` occurrences in a JS stack
/// to the original source location via the rolldown source map.
fn remap_stack(bundle: &CompiledBundle, stack: &str) -> String {
  use std::sync::OnceLock;

  use regex::Regex;
  static RE: OnceLock<Regex> = OnceLock::new();
  let re = RE.get_or_init(|| Regex::new(r"([^\s()]+):(\d+):(\d+)").expect("valid stack regex"));
  re.replace_all(stack, |caps: &regex::Captures<'_>| {
    let (Ok(line), Ok(col)) = (caps[2].parse::<u32>(), caps[3].parse::<u32>()) else {
      return caps[0].to_string();
    };
    match bundle.remap(line, col) {
      Some((src, sl, sc)) => format!("{src}:{sl}:{sc}"),
      None => caps[0].to_string(),
    }
  })
  .into_owned()
}

// ── Per-worker session cache + TestRunner integration ────────────────
//
// Per-worker `OnceCell`s keyed by `worker_index`. The outer `DashMap`
// is sharded so concurrent workers fetching their own slot don't
// contend on a single mutex. The inner `OnceCell` is per-worker, so
// `JsBddSession::load(...)` for worker N proceeds in parallel with
// worker M's load — only the same worker's repeated calls collapse to
// one init (the desired behaviour). Previous design used
// `Mutex<HashMap>` and held the lock across `load().await`, which
// serialised the first-scenario session load across every worker.

type WorkerSessionCell = OnceCell<Arc<JsBddSession>>;
type WorkerSessions = DashMap<u32, Arc<WorkerSessionCell>>;
static WORKER_SESSIONS: OnceLock<WorkerSessions> = OnceLock::new();

/// The `[scripting]` sandbox caps the BDD step VM runs with. Set once
/// by the `ferridriver bdd` entry point from resolved config (mirrors
/// how the MCP server threads them); unset ⇒ locked down
/// ([`ScriptCaps::default`]), the safe default for the macro/harness
/// path that has no config.
static BDD_SCRIPT_CAPS: OnceLock<ferridriver_script::ScriptCaps> = OnceLock::new();

/// Install the BDD step VM sandbox caps (the env allow-list). Call
/// before the run; idempotent (first set wins).
pub fn set_bdd_script_caps(caps: ferridriver_script::ScriptCaps) {
  let _ = BDD_SCRIPT_CAPS.set(caps);
}

/// Declared sidecar specs the BDD step VM exposes as
/// `sidecars.connect(name)`. Set once by the `ferridriver bdd` entry from
/// resolved config (same threading as [`BDD_SCRIPT_CAPS`]); unset ⇒ no
/// declared sidecars (`sidecars.connect` rejects every name).
static BDD_SIDECARS: OnceLock<Vec<ferridriver_script::sidecar::SidecarSpec>> = OnceLock::new();

/// Install the declared sidecar specs for the BDD step VM. Call before the
/// run; idempotent (first set wins).
pub fn set_bdd_sidecars(sidecars: Vec<ferridriver_script::sidecar::SidecarSpec>) {
  let _ = BDD_SIDECARS.set(sidecars);
}

async fn worker_session(
  worker_index: u32,
  bundle: Arc<CompiledBundle>,
  cwd: Arc<PathBuf>,
  world_parameters: serde_json::Value,
) -> Result<Arc<JsBddSession>, String> {
  let map = WORKER_SESSIONS.get_or_init(DashMap::new);
  let cell = map
    .entry(worker_index)
    .or_insert_with(|| Arc::new(OnceCell::new()))
    .clone();
  cell
    .get_or_try_init(|| async move {
      JsBddSession::load(bundle, &cwd, world_parameters)
        .await
        .map(Arc::new)
        .map_err(|e| e.to_string())
    })
    .await
    .cloned()
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
  bundle: Arc<CompiledBundle>,
  cwd: PathBuf,
) -> ferridriver_test::model::TestPlan {
  use ferridriver_test::model::{ExpectedStatus, Hooks, SuiteMode, TestCase, TestFailure, TestFn, TestId, TestSuite};

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
    for scenario in scenarios {
      // Build the immutable TestCase metadata up front, then move the
      // scenario into an Arc so the per-invocation closure shares it via a
      // refcount bump instead of deep-cloning the step Vec twice (mirrors
      // the Rust-step path in `translate::translate_scenario`).
      let id = TestId {
        file: scenario.feature_path.display().to_string(),
        suite: Some(scenario.feature_name.clone()),
        name: scenario.name.clone(),
        line: crate::translate::scenario_line(&scenario),
      };
      let annotations = crate::translate::scenario_annotations(&scenario);
      let scenario = Arc::new(scenario);

      let bundle = Arc::clone(&bundle);
      let cwd = Arc::clone(&cwd);
      let browser_config = config.browser.clone();
      let world_parameters = config.world_parameters.clone();
      let bdd_strict = config.strict;

      let test_fn: TestFn = Arc::new(move |pool: FixturePool| {
        let scenario = Arc::clone(&scenario);
        let bundle = Arc::clone(&bundle);
        let cwd = Arc::clone(&cwd);
        let browser_config = browser_config.clone();
        let world_parameters = world_parameters.clone();
        let bdd_strict = bdd_strict;
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

          let session = worker_session(test_info.worker_index, bundle, cwd, world_parameters)
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
          forward_attachments(&test_info, session.drain_attachments().await).await;
          for s in &result.steps {
            record_step(&test_info, s).await;
          }
          if result.passed {
            return Ok(());
          }
          // Non-strict mode: undefined / pending steps don't fail the
          // test — they're reported as `StepStatus::Pending` and the
          // scenario passes overall (mirrors the Rust executor's
          // `Err(e) if e.pending && !self.strict` branch).
          let only_pending = !bdd_strict
            && result.steps.iter().all(|s| {
              matches!(
                s.status,
                JsStepStatus::Passed | JsStepStatus::Skipped | JsStepStatus::Pending | JsStepStatus::Undefined(_)
              )
            });
          if only_pending {
            return Ok(());
          }
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
        })
      });

      tests.push(TestCase {
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
