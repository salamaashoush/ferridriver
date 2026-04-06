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

use std::sync::Arc;

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

/// Step callback TSFN: async JS function receiving (Page, ...args) -> Promise<void>.
type StepCallbackFn = ThreadsafeFunction<
  crate::page::Page,
  napi::bindgen_prelude::Promise<()>,
  crate::page::Page,
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
}

/// A registered TS step definition.
struct TsStepDef {
  kind: String,
  pattern: String,
  callback: Arc<StepCallbackFn>,
}

/// A registered TS hook.
#[allow(dead_code)]
struct TsHook {
  point: String,
  tags: Option<String>,
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
  steps: Mutex<Vec<TsStepDef>>,
  hooks: Mutex<Vec<TsHook>>,
}

#[napi]
impl BddRunner {
  /// Create a new BDD runner.
  #[napi(factory)]
  pub fn create(config: Option<BddRunnerConfig>) -> Result<Self> {
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
      steps: Mutex::new(Vec::new()),
      hooks: Mutex::new(Vec::new()),
    })
  }

  /// Register a step definition from TypeScript.
  ///
  /// The callback receives a Page and should return Promise<void>.
  /// Step matching uses cucumber expressions.
  #[napi(
    ts_args_type = "kind: 'given' | 'when' | 'then' | 'step', pattern: string, callback: (page: Page) => Promise<void>"
  )]
  pub fn register_step(
    &self,
    kind: String,
    pattern: String,
    callback: napi::bindgen_prelude::Function<
      '_,
      crate::page::Page,
      napi::bindgen_prelude::Promise<()>,
    >,
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
    });
    Ok(())
  }

  /// Register a lifecycle hook from TypeScript.
  #[napi(
    ts_args_type = "point: 'before' | 'after', scope: 'scenario' | 'all', callback: (page: Page) => Promise<void>, tags?: string"
  )]
  pub fn register_hook(
    &self,
    point: String,
    _scope: String,
    callback: napi::bindgen_prelude::Function<
      '_,
      crate::page::Page,
      napi::bindgen_prelude::Promise<()>,
    >,
    tags: Option<String>,
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
      tags,
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
      let handler: ferridriver_bdd::step::StepHandler = Arc::new(move |world, _params, _table, _docstring| {
        let cb = Arc::clone(&cb);
        // We need a Page to pass to JS. Clone it from the world.
        let page = world.page().clone();
        Box::pin(async move {
          let napi_page = crate::page::Page::wrap(page);
          match cb.call_async(napi_page).await {
            Ok(promise) => promise
              .await
              .map_err(|e| ferridriver_bdd::step::StepError::from(format!("{e}"))),
            Err(e) => Err(ferridriver_bdd::step::StepError::from(format!("{e}"))),
          }
        })
      });

      if let Err(e) = registry.register(
        kind,
        &ts_step.pattern,
        handler,
        ferridriver_bdd::step::StepLocation {
          file: "<typescript>",
          line: 0,
        },
      ) {
        return Err(napi::Error::from_reason(format!(
          "invalid step pattern \"{}\": {e}",
          ts_step.pattern
        )));
      }
    }
    drop(ts_steps);

    let registry = Arc::new(registry);

    // Discover and parse features.
    let feature_set = ferridriver_bdd::feature::FeatureSet::discover_and_parse(
      &self.config.features,
      &self.config.test_ignore,
    )
    .map_err(|e| napi::Error::from_reason(format!("feature discovery: {e}")))?;

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

    // Create reporters.
    let reporters = {
      let mut reps: Vec<Box<dyn ferridriver_test::reporter::Reporter>> = Vec::new();
      reps.push(Box::new(
        ferridriver_bdd::reporter::terminal::BddTerminalReporter::new(),
      ));
      ferridriver_test::reporter::ReporterSet::new(reps)
    };

    // Run via core TestRunner.
    let overrides = ferridriver_test::config::CliOverrides::default();
    let config = self.config.clone();
    let total = plan.total_tests;

    let mut runner = ferridriver_test::runner::TestRunner::new(config, reporters, overrides);
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
