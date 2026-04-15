//! BDD registry: encapsulates step definitions and parameter types.
//!
//! Constructed by TS registration calls on `TestRunner`, consumed during `run()`.
//! Step callbacks receive the unified `TestFixtures` — same type as E2E.

use std::sync::{Arc, Mutex as StdMutex};

// Force the linker to include the built-in BDD step definitions.
// Without this, cdylib dead code elimination strips the inventory submissions.
// The extern crate ensures the entire crate is linked, not just referenced types.
#[allow(unused_extern_crates)]
extern crate ferridriver_bdd;

use napi::Result;
use napi::Status;
use napi::threadsafe_function::ThreadsafeFunction;

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

/// Encapsulates BDD step definitions and parameter types.
/// Constructed by TS registration calls, consumed during run().
pub(crate) struct BddRegistry {
  steps: StdMutex<Vec<TsStepDef>>,
  param_types: StdMutex<Vec<(String, String)>>,
}

impl BddRegistry {
  pub fn new() -> Self {
    Self {
      steps: StdMutex::new(Vec::new()),
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

  /// Build a StepRegistry from registered TS steps + built-in Rust steps.
  pub fn build_step_registry(&self) -> Result<ferridriver_bdd::registry::StepRegistry> {
    let ts_steps = self
      .steps
      .lock()
      .map_err(|_| napi::Error::from_reason("steps lock poisoned"))?;

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
        let fixtures = crate::test_runner::fixtures_with_bdd_params(world, Some(&params), table, docstring);
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

    Ok(registry)
  }
}
