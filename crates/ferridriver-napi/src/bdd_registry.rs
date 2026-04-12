//! BDD registry: encapsulates step definitions, hooks, and parameter types.
//!
//! Constructed by TS registration calls on `TestRunner`, consumed during `run()`.
//! Step/hook callbacks receive the unified `TestFixtures` — same type as E2E.

use std::sync::{Arc, Mutex as StdMutex};

// Force the linker to include the built-in BDD step definitions.
// Without this, cdylib dead code elimination strips the inventory submissions.
// The extern crate ensures the entire crate is linked, not just referenced types.
#[allow(unused_extern_crates)]
extern crate ferridriver_bdd;

use napi::Result;
use napi::Status;
use napi::threadsafe_function::ThreadsafeFunction;
use tokio::sync::Mutex;

/// Step/hook callback TSFN: async JS function receiving TestFixtures -> Promise<void>.
/// Same callback type as E2E tests — unified fixture bag.
type StepCallbackFn = ThreadsafeFunction<
  crate::test_fixtures::TestFixtures,
  napi::bindgen_prelude::Promise<()>,
  crate::test_fixtures::TestFixtures,
  Status,
  false,
  true,
  0,
>;

/// A registered TS step definition.
struct TsStepDef {
  kind: String,
  pattern: String,
  callback: Arc<StepCallbackFn>,
  is_regex: bool,
  #[allow(dead_code)]
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

/// Encapsulates BDD step definitions, hooks, and parameter types.
/// Constructed by TS registration calls, consumed during run().
pub(crate) struct BddRegistry {
  steps: Mutex<Vec<TsStepDef>>,
  hooks: Mutex<Vec<TsHook>>,
  param_types: StdMutex<Vec<(String, String)>>,
}

impl BddRegistry {
  pub fn new() -> Self {
    Self {
      steps: Mutex::new(Vec::new()),
      hooks: Mutex::new(Vec::new()),
      param_types: StdMutex::new(Vec::new()),
    }
  }

  pub fn register_step(
    &self,
    kind: String,
    pattern: String,
    callback: napi::bindgen_prelude::Function<
      '_,
      crate::test_fixtures::TestFixtures,
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

  pub fn define_parameter_type(&self, name: String, regex: String) -> Result<()> {
    let mut pts = self
      .param_types
      .lock()
      .map_err(|_| napi::Error::from_reason("param_types lock poisoned"))?;
    pts.push((name, regex));
    Ok(())
  }

  pub fn register_hook(
    &self,
    point: String,
    scope: String,
    callback: napi::bindgen_prelude::Function<
      '_,
      crate::test_fixtures::TestFixtures,
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

  /// Build a StepRegistry from registered TS steps + built-in Rust steps.
  pub async fn build_step_registry(&self) -> Result<Arc<ferridriver_bdd::registry::StepRegistry>> {
    let ts_steps = self.steps.lock().await;

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

      // Step handler clones world's fixtures and sets BDD params for this step.
      let handler: ferridriver_bdd::step::StepHandler = Arc::new(move |world, params, table, docstring| {
        let cb = Arc::clone(&cb);
        let fixtures = fixtures_with_bdd_params(world, Some(&params), table, docstring);
        Box::pin(async move {
          let napi_fixtures = crate::test_fixtures::TestFixtures::from_resolved(fixtures);
          match cb.call_async(napi_fixtures).await {
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
          Box::pin(async { Ok(()) })
        })),
        "step" => {
          let cb = Arc::clone(&cb);
          ferridriver_bdd::hook::HookHandler::Step(Arc::new(move |world, _step_text| {
            let cb = Arc::clone(&cb);
            // Hook gets full fixtures but no BDD args.
            let fixtures = fixtures_with_bdd_params(world, None, None, None);
            Box::pin(async move {
              let napi_fixtures = crate::test_fixtures::TestFixtures::from_resolved(fixtures);
              cb.call_async(napi_fixtures)
                .await
                .map_err(|e| format!("{e}"))?
                .await
                .map_err(|e| format!("{e}"))
            })
          }))
        },
        _ => {
          // scenario scope (default)
          let cb = Arc::clone(&cb);
          ferridriver_bdd::hook::HookHandler::Scenario(Arc::new(move |world| {
            let cb = Arc::clone(&cb);
            let fixtures = fixtures_with_bdd_params(world, None, None, None);
            Box::pin(async move {
              let napi_fixtures = crate::test_fixtures::TestFixtures::from_resolved(fixtures);
              cb.call_async(napi_fixtures)
                .await
                .map_err(|e| format!("{e}"))?
                .await
                .map_err(|e| format!("{e}"))
            })
          }))
        },
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

    Ok(Arc::new(registry))
  }
}

/// Clone the world's fixtures and set BDD-specific fields for this step.
/// For hooks, pass `None` for all BDD params — just clones the base fixtures.
fn fixtures_with_bdd_params(
  world: &ferridriver_bdd::world::BrowserWorld,
  params: Option<&[ferridriver_bdd::step::StepParam]>,
  table: Option<&ferridriver_bdd::step::DataTable>,
  docstring: Option<&str>,
) -> ferridriver_test::model::TestFixtures {
  use ferridriver_bdd::step::StepParam;

  let mut fixtures = world.fixtures().clone();

  fixtures.bdd_args = params.map(|p| {
    p.iter()
      .map(|param| match param {
        StepParam::Int(i) => serde_json::Value::Number((*i).into()),
        StepParam::Float(f) => serde_json::json!(f),
        StepParam::String(s) | StepParam::Word(s) => serde_json::Value::String(s.clone()),
        StepParam::Custom { value, .. } => serde_json::Value::String(value.clone()),
      })
      .collect()
  });
  fixtures.bdd_data_table = table.map(|t| t.iter().map(|r| r.clone()).collect());
  fixtures.bdd_doc_string = docstring.map(|s| s.to_string());

  fixtures
}
