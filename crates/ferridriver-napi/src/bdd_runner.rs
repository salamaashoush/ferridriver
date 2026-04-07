//! NAPI BDD runner: TypeScript step registration + Rust execution engine.
//!
//! Flow:
//! 1. TS registers steps via `registerStep("given", "I click {string}", callback)`
//! 2. TS registers hooks via `registerHook("before", "scenario", callback)`
//! 3. TS calls `run("features/**/*.feature")` or `run()` with config
//! 4. Rust parses .feature files, matches steps to TS callbacks, builds TestPlan
//! 5. Core TestRunner executes with worker pool, parallel dispatch, retries, reporters
//! 6. When a step matches a TS definition, TSFN calls the JS callback with Page fixture
//! 7. Returns RunSummary

use std::sync::{Arc, Mutex as StdMutex};

// Force the linker to include the built-in BDD step definitions.
// Without this, cdylib dead code elimination strips the inventory submissions.
// The extern crate ensures the entire crate is linked, not just referenced types.
#[allow(unused_extern_crates)]
extern crate ferridriver_bdd;

use napi::Result;
use napi::Status;
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;
use tokio::sync::Mutex;

/// Step context passed to TS callbacks — contains page, params, table, docstring.
///
/// Parameters are properly typed: `{int}` → JS number, `{float}` → JS number,
/// `{string}`/`{word}` → JS string.
#[napi]
pub struct StepContext {
  inner_page: ferridriver::Page,
  typed_args: Vec<serde_json::Value>,
  data_table: Option<Vec<Vec<String>>>,
  doc_string: Option<String>,
}

impl StepContext {
  fn from_params(
    page: ferridriver::Page,
    params: &[ferridriver_bdd::step::StepParam],
    table: Option<&ferridriver_bdd::step::DataTable>,
    docstring: Option<&str>,
  ) -> Self {
    use ferridriver_bdd::step::StepParam;
    let typed_args = params
      .iter()
      .map(|p| match p {
        StepParam::Int(i) => serde_json::Value::Number((*i).into()),
        StepParam::Float(f) => serde_json::json!(f),
        StepParam::String(s) | StepParam::Word(s) => serde_json::Value::String(s.clone()),
        StepParam::Custom { value, .. } => serde_json::Value::String(value.clone()),
      })
      .collect();
    Self {
      inner_page: page,
      typed_args,
      data_table: table.map(|t| t.iter().map(|r| r.clone()).collect()),
      doc_string: docstring.map(|s| s.to_string()),
    }
  }

  fn empty(page: ferridriver::Page) -> Self {
    Self {
      inner_page: page,
      typed_args: Vec::new(),
      data_table: None,
      doc_string: None,
    }
  }
}

#[napi]
impl StepContext {
  /// The browser page for this step.
  #[napi(getter)]
  pub fn page(&self) -> crate::page::Page {
    crate::page::Page::wrap(self.inner_page.clone())
  }

  /// Extracted parameters — properly typed (int/float → number, string → string).
  #[napi(getter, ts_return_type = "unknown[]")]
  pub fn args(&self) -> serde_json::Value {
    serde_json::Value::Array(self.typed_args.clone())
  }

  /// Alias for `args`.
  #[napi(getter, ts_return_type = "unknown[]")]
  pub fn params(&self) -> serde_json::Value {
    serde_json::Value::Array(self.typed_args.clone())
  }

  /// The inline data table attached to this step, if any.
  #[napi(getter)]
  pub fn data_table(&self) -> Option<Vec<Vec<String>>> {
    self.data_table.clone()
  }

  /// The doc string attached to this step, if any.
  #[napi(getter)]
  pub fn doc_string(&self) -> Option<String> {
    self.doc_string.clone()
  }
}

/// Step callback TSFN: async JS function receiving (StepContext) -> Promise<void>.
type StepCallbackFn = ThreadsafeFunction<
  StepContext,
  napi::bindgen_prelude::Promise<()>,
  StepContext,
  Status,
  false,
  true,
  0,
>;

/// BDD runner configuration from TypeScript.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct BddRunnerConfig {
  /// Feature file glob patterns.
  pub features: Option<Vec<String>>,
  /// Tag filter expression.
  pub tags: Option<String>,
  /// Number of parallel workers.
  pub workers: Option<i32>,
  /// Per-step timeout in ms.
  pub timeout: Option<f64>,
  /// Number of retries.
  pub retries: Option<i32>,
  /// Run headed.
  pub headed: Option<bool>,
  /// Browser backend.
  pub backend: Option<String>,
  /// Reporter names.
  pub reporter: Option<Vec<String>>,
  /// Output directory.
  pub output_dir: Option<String>,
  /// Screenshot on failure.
  pub screenshot_on_failure: Option<bool>,
  /// Strict mode: undefined/pending steps cause failure.
  pub strict: Option<bool>,
  /// Execution order: "defined" | "random" | "random:SEED".
  pub order: Option<String>,
  /// i18n language code for Gherkin keywords (e.g., "fr", "de", "ja").
  pub language: Option<String>,
  /// Fail if @only tag is found (CI safety net).
  pub forbid_only: Option<bool>,
  /// Re-run only previously failed scenarios (from @rerun.txt).
  pub last_failed: Option<bool>,
  /// Video recording mode: "off", "on", "retain-on-failure".
  pub video: Option<String>,
  /// Trace recording mode: "off", "on", "retain-on-failure", "on-first-retry".
  pub trace: Option<String>,
}

/// A registered TS step definition.
struct TsStepDef {
  kind: String,
  pattern: String,
  callback: Arc<StepCallbackFn>,
  is_regex: bool,
  timeout: Option<f64>,
}

/// A registered TS hook.
#[allow(dead_code)]
struct TsHook {
  point: String,
  scope: String,
  tags: Option<String>,
  name: Option<String>,
  timeout: Option<f64>,
  callback: Arc<StepCallbackFn>,
}

/// BDD step result from TypeScript.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct BddRunSummary {
  pub total: i32,
  pub passed: i32,
  pub failed: i32,
  pub skipped: i32,
  pub flaky: i32,
  pub duration_ms: f64,
}

/// The BDD runner. TS registers steps and hooks, then calls run().
#[napi]
pub struct BddRunner {
  config: ferridriver_test::TestConfig,
  last_failed: bool,
  steps: Mutex<Vec<TsStepDef>>,
  hooks: Mutex<Vec<TsHook>>,
  param_types: StdMutex<Vec<(String, String)>>,
}

#[napi]
impl BddRunner {
  /// Create a new BDD runner.
  #[napi(factory)]
  pub fn create(config: Option<BddRunnerConfig>) -> Result<Self> {
    ferridriver_test::logging::init_from_env();
    let cfg = config.unwrap_or_default();
    let mut tc = ferridriver_test::TestConfig::default();

    if let Some(t) = cfg.timeout {
      tc.timeout = t as u64;
    }
    if let Some(w) = cfg.workers {
      tc.workers = w as u32;
    }
    if let Some(r) = cfg.retries {
      tc.retries = r as u32;
    }
    if let Some(headed) = cfg.headed {
      tc.browser.headless = !headed;
    }
    if let Some(ref b) = cfg.backend {
      tc.browser.backend.clone_from(b);
    }
    if let Some(ref r) = cfg.reporter {
      tc.reporter = r
        .iter()
        .map(|name| ferridriver_test::config::ReporterConfig {
          name: name.clone(),
          options: Default::default(),
        })
        .collect();
    }
    if let Some(ref dir) = cfg.output_dir {
      tc.output_dir = dir.into();
    }
    if let Some(ref features) = cfg.features {
      tc.features.clone_from(features);
    }
    if let Some(ref tags) = cfg.tags {
      tc.tags = Some(tags.clone());
    }
    if let Some(ss) = cfg.screenshot_on_failure {
      tc.screenshot_on_failure = ss;
    }
    if let Some(strict) = cfg.strict {
      tc.strict = strict;
    }
    if let Some(ref order) = cfg.order {
      tc.order.clone_from(order);
    }
    if let Some(ref lang) = cfg.language {
      tc.language = Some(lang.clone());
    }
    if let Some(fo) = cfg.forbid_only {
      tc.forbid_only = fo;
    }
    if let Some(ref v) = cfg.video {
      tc.video.mode = match v.as_str() {
        "on" => ferridriver_test::config::VideoMode::On,
        "retain-on-failure" => ferridriver_test::config::VideoMode::RetainOnFailure,
        _ => ferridriver_test::config::VideoMode::Off,
      };
    }
    if let Some(ref t) = cfg.trace {
      tc.trace = ferridriver_test::tracing::TraceMode::from_str(t);
    }
    if tc.features.is_empty() {
      tc.features = vec!["features/**/*.feature".to_string()];
    }
    if tc.workers == 0 {
      let cpus = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4);
      tc.workers = (cpus / 2).max(1);
    }

    Ok(Self {
      config: tc,
      last_failed: cfg.last_failed.unwrap_or(false),
      steps: Mutex::new(Vec::new()),
      hooks: Mutex::new(Vec::new()),
      param_types: StdMutex::new(Vec::new()),
    })
  }

  /// Register a step definition from TypeScript.
  ///
  /// The callback receives a StepContext (with page, args, dataTable, docString)
  /// and should return Promise<void>.
  #[napi(
    ts_args_type = "kind: 'given' | 'when' | 'then' | 'step', pattern: string, callback: (ctx: StepContext) => Promise<void>, isRegex?: boolean, timeout?: number"
  )]
  pub fn register_step(
    &self,
    kind: String,
    pattern: String,
    callback: napi::bindgen_prelude::Function<
      '_,
      StepContext,
      napi::bindgen_prelude::Promise<()>,
    >,
    is_regex: Option<bool>,
    timeout: Option<f64>,
  ) -> Result<()> {
    let tsfn = callback
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;

    let mut steps = self
      .steps
      .try_lock()
      .map_err(|_| napi::Error::from_reason("steps lock contended"))?;
    steps.push(TsStepDef {
      kind,
      pattern,
      callback: Arc::new(tsfn),
      is_regex: is_regex.unwrap_or(false),
      timeout,
    });
    Ok(())
  }

  /// Register a custom parameter type for Cucumber expressions.
  /// After registering, use `{name}` in step patterns.
  #[napi(ts_args_type = "name: string, regex: string")]
  pub fn define_parameter_type(&self, name: String, regex: String) -> Result<()> {
    let mut pts = self
      .param_types
      .lock()
      .map_err(|_| napi::Error::from_reason("param_types lock poisoned"))?;
    pts.push((name, regex));
    Ok(())
  }

  /// Register a lifecycle hook from TypeScript.
  #[napi(
    ts_args_type = "point: 'before' | 'after', scope: 'scenario' | 'step' | 'all', callback: (ctx: StepContext) => Promise<void>, tags?: string, name?: string, timeout?: number"
  )]
  pub fn register_hook(
    &self,
    point: String,
    scope: String,
    callback: napi::bindgen_prelude::Function<
      '_,
      StepContext,
      napi::bindgen_prelude::Promise<()>,
    >,
    tags: Option<String>,
    name: Option<String>,
    timeout: Option<f64>,
  ) -> Result<()> {
    let tsfn = callback
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;

    let mut hooks = self
      .hooks
      .try_lock()
      .map_err(|_| napi::Error::from_reason("hooks lock contended"))?;
    hooks.push(TsHook {
      point,
      scope,
      tags,
      name,
      timeout,
      callback: Arc::new(tsfn),
    });
    Ok(())
  }

  /// Run BDD features. Parses .feature files, matches steps to registered
  /// TS callbacks + Rust built-in steps, executes via core TestRunner.
  #[napi]
  pub async fn run(&self) -> Result<BddRunSummary> {
    let ts_steps = self.steps.lock().await;

    // Build step registry: built-in Rust steps + TS steps.
    let mut registry = ferridriver_bdd::registry::StepRegistry::build();

    // Register custom parameter types before compiling step expressions.
    {
      let pts = self
        .param_types
        .lock()
        .map_err(|_| napi::Error::from_reason("param_types lock poisoned"))?;
      for (name, regex) in pts.iter() {
        registry.register_param_type(ferridriver_bdd::param_type::CustomParamType {
          name: name.clone(),
          regex: regex.clone(),
          transformer: None,
        });
      }
    }

    // Register TS steps into the Rust registry.
    for ts_step in ts_steps.iter() {
      let kind = match ts_step.kind.as_str() {
        "given" => ferridriver_bdd::step::StepKind::Given,
        "when" => ferridriver_bdd::step::StepKind::When,
        "then" => ferridriver_bdd::step::StepKind::Then,
        _ => ferridriver_bdd::step::StepKind::Step,
      };

      let cb = Arc::clone(&ts_step.callback);

      // Create a StepHandler that calls the TS callback via TSFN.
      let handler: ferridriver_bdd::step::StepHandler = Arc::new(move |world, params, table, docstring| {
        let cb = Arc::clone(&cb);
        let page = world.page().clone();
        Box::pin(async move {
          let ctx = StepContext::from_params(page, &params, table, docstring);
          match cb.call_async(ctx).await {
            Ok(promise) => promise
              .await
              .map_err(|e| ferridriver_bdd::step::StepError::from(format!("{e}"))),
            Err(e) => Err(ferridriver_bdd::step::StepError::from(format!("{e}"))),
          }
        })
      });

      let location = ferridriver_bdd::step::StepLocation {
        file: "<typescript>",
        line: 0,
      };

      let result = if ts_step.is_regex {
        registry.register_regex(kind, &ts_step.pattern, handler, location)
      } else {
        registry.register(kind, &ts_step.pattern, handler, location)
      };

      if let Err(e) = result {
        return Err(napi::Error::from_reason(format!(
          "invalid step pattern \"{}\": {e}",
          ts_step.pattern
        )));
      }
    }
    drop(ts_steps);

    // Register TS hooks into the Rust hook registry.
    let ts_hooks = self.hooks.lock().await;
    for ts_hook in ts_hooks.iter() {
      let hook_point = match (ts_hook.point.as_str(), ts_hook.scope.as_str()) {
        ("before", "scenario") => ferridriver_bdd::hook::HookPoint::BeforeScenario,
        ("after", "scenario") => ferridriver_bdd::hook::HookPoint::AfterScenario,
        ("before", "step") => ferridriver_bdd::hook::HookPoint::BeforeStep,
        ("after", "step") => ferridriver_bdd::hook::HookPoint::AfterStep,
        ("before", "all") => ferridriver_bdd::hook::HookPoint::BeforeAll,
        ("after", "all") => ferridriver_bdd::hook::HookPoint::AfterAll,
        _ => continue,
      };

      let cb = Arc::clone(&ts_hook.callback);
      let handler = match ts_hook.scope.as_str() {
        "all" => ferridriver_bdd::hook::HookHandler::Global(Arc::new(move || {
          // Global hooks don't receive a page/world; return Ok immediately.
          Box::pin(async { Ok(()) })
        })),
        "step" => {
          let cb = Arc::clone(&cb);
          ferridriver_bdd::hook::HookHandler::Step(Arc::new(move |world, _step_text| {
            let cb = Arc::clone(&cb);
            let page = world.page().clone();
            Box::pin(async move {
              let ctx = StepContext::empty(page);
              cb.call_async(ctx)
                .await
                .map_err(|e| format!("{e}"))?
                .await
                .map_err(|e| format!("{e}"))
            })
          }))
        }
        _ => {
          // scenario scope (default)
          let cb = Arc::clone(&cb);
          ferridriver_bdd::hook::HookHandler::Scenario(Arc::new(move |world| {
            let cb = Arc::clone(&cb);
            let page = world.page().clone();
            Box::pin(async move {
              let ctx = StepContext::empty(page);
              cb.call_async(ctx)
                .await
                .map_err(|e| format!("{e}"))?
                .await
                .map_err(|e| format!("{e}"))
            })
          }))
        }
      };

      let tag_filter = ts_hook
        .tags
        .as_ref()
        .and_then(|t| ferridriver_bdd::filter::TagExpression::parse(t).ok());

      registry.hooks_mut().register(ferridriver_bdd::hook::Hook {
        point: hook_point,
        tag_filter,
        order: 0,
        handler,
        location: ferridriver_bdd::step::StepLocation {
          file: "<typescript>",
          line: 0,
        },
      });
    }
    drop(ts_hooks);

    let registry = Arc::new(registry);

    // Discover and parse features (with optional i18n language).
    let files = ferridriver_bdd::feature::FeatureSet::discover(&self.config.features, &self.config.test_ignore)
      .map_err(|e| napi::Error::from_reason(format!("feature discovery: {e}")))?;
    let feature_set = ferridriver_bdd::feature::FeatureSet::parse_with_language(files, self.config.language.as_deref())
      .map_err(|e| napi::Error::from_reason(format!("feature parse: {e}")))?;

    if feature_set.features.is_empty() {
      return Ok(BddRunSummary {
        total: 0,
        passed: 0,
        failed: 0,
        skipped: 0,
        flaky: 0,
        duration_ms: 0.0,
      });
    }

    // Translate to TestPlan.
    let plan =
      ferridriver_bdd::translate::translate_features(&feature_set, registry, &self.config);

    if plan.total_tests == 0 {
      return Ok(BddRunSummary {
        total: 0,
        passed: 0,
        failed: 0,
        skipped: 0,
        flaky: 0,
        duration_ms: 0.0,
      });
    }

    // Run via core TestRunner.
    let mut overrides = ferridriver_test::config::CliOverrides::default();
    overrides.last_failed = self.last_failed;
    let mut config = self.config.clone();
    config.mode = ferridriver_test::config::RunMode::Bdd;
    let total = plan.total_tests;

    let mut runner = ferridriver_test::runner::TestRunner::new(config, overrides);
    let exit_code = runner.run(plan).await;

    // We don't have per-test results here since TestRunner reports via events.
    // Return a summary based on exit code.
    Ok(BddRunSummary {
      total: total as i32,
      passed: if exit_code == 0 { total as i32 } else { 0 },
      failed: if exit_code != 0 { 1 } else { 0 },
      skipped: 0,
      flaky: 0,
      duration_ms: 0.0, // TODO: capture from RunFinished event
    })
  }
}
