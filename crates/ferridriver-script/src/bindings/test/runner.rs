//! Fixture validation, setup/teardown lifecycle, and test/hook body
//! invocation.

use std::future::Future;
use std::sync::{Arc, Mutex};

use rquickjs::promise::MaybePromise;
use rquickjs::{CatchResultExt, Ctx, Function, Object, Persistent, Promise, Value};

use ferridriver_test::fixture::FixtureScope;
use ferridriver_test::fixture_graph;
use ferridriver_test::host::{RunTestSpec, TestHostBridge, TestWorldData};

use crate::bindings::registry::throw_script_error;
use crate::engine::caught_to_script_error;
use crate::error::ScriptError;

use super::registry::{
  BASE_FIXTURE_SET, FixtureBox, PendingFixture, TEST_SKIP_SENTINEL, TestRegistry, WorkerFixture,
  fold_pushed_annotations, optional_bridge, truthy, with_test_registry,
};
use super::step::{fixture_slots, se, set_current_test};

/// Playwright raises these as load errors, failing every test in the
/// file; ferridriver raises them per function, so only the test that
/// asks for the name fails. Recorded in `docs/playwright-compat.md`.
pub(crate) fn validate_requested_names<'js>(
  ctx: &Ctx<'js>,
  world_obj: &Object<'js>,
  fixture_set: usize,
  functions: &[(String, Vec<String>)],
) -> Result<(), ScriptError> {
  let slots = with_test_registry(ctx, |r| fixture_slots(r, fixture_set))?;
  let is_builtin = |name: &str| {
    ferridriver_test::fixture_graph::BUILTIN_FIXTURES.contains(&name) || world_obj.contains_key(name).unwrap_or(false)
  };
  for (prefix, requested) in functions {
    fixture_graph::validate_requested(&slots, requested, &is_builtin, prefix).map_err(ScriptError::internal)?;
  }
  Ok(())
}

pub(crate) async fn resolve_custom_fixtures<'js>(
  ctx: &Ctx<'js>,
  world_obj: &Object<'js>,
  fixture_set: usize,
  requested: &[String],
  use_options: &serde_json::Value,
  source_label: &str,
) -> Result<(), ScriptError> {
  let slots = with_test_registry(ctx, |r| fixture_slots(r, fixture_set))?;
  if slots.is_empty() {
    return Ok(());
  }
  // A name the runtime already put on the fixtures object IS the base
  // implementation an override shadows (`page`, `context`, `request`,
  // `browser`, the config scalars). That is the only thing separating a
  // legitimate override from a self-reference with nothing under it.
  let is_builtin = |name: &str| world_obj.contains_key(name).unwrap_or(false);
  let ordered = fixture_graph::resolution_order(&slots, requested, &is_builtin).map_err(ScriptError::internal)?;

  for pos in ordered {
    resolve_one_fixture(ctx, world_obj, slots[pos].reg, use_options, source_label).await?;
  }
  Ok(())
}

async fn resolve_one_fixture<'js>(
  ctx: &Ctx<'js>,
  world_obj: &Object<'js>,
  reg_idx: usize,
  use_options: &serde_json::Value,
  source_label: &str,
) -> Result<(), ScriptError> {
  enum Plan {
    CachedWorker(Persistent<Value<'static>>),
    Static(Option<Persistent<Value<'static>>>),
    Factory {
      factory: Persistent<Function<'static>>,
      worker_scoped: bool,
    },
  }
  let (name, option, plan, setup) = with_test_registry(ctx, |r| {
    let f = &r.fixtures[reg_idx];
    let plan = if let Some(cached) = r.worker_fixtures.get(&reg_idx) {
      Plan::CachedWorker(cached.value.clone())
    } else if let Some(factory) = &f.factory {
      Plan::Factory {
        factory: factory.clone(),
        worker_scoped: f.scope == FixtureScope::Worker,
      }
    } else {
      Plan::Static(f.static_value.clone())
    };
    (
      f.name.clone(),
      f.option,
      plan,
      FixtureSetup {
        timeout_ms: f.timeout_ms,
        title: f.title.clone(),
        boxed: f.boxed,
      },
    )
  })?;

  // An option fixture named in a `use` bag takes that value whatever
  // its declaration was: Playwright replaces the registration's `fn`
  // with the override, and a value that came out of a config file is
  // never a factory (`common/fixtures.ts:98-101`).
  let override_v = option.then(|| use_options.get(&name)).flatten();

  match (plan, override_v) {
    (Plan::CachedWorker(value), _) => {
      let v = value.restore(ctx).map_err(se)?;
      world_obj.set(name.as_str(), v).map_err(se)?;
      Ok(())
    },
    (_, Some(json)) => {
      let v = crate::bindings::convert::json_to_js(ctx, json).map_err(se)?;
      world_obj.set(name.as_str(), v).map_err(se)?;
      Ok(())
    },
    (Plan::Static(value), None) => {
      let v: Value<'_> = match value {
        Some(saved) => saved.restore(ctx).map_err(se)?,
        None => Value::new_undefined(ctx.clone()),
      };
      world_obj.set(name.as_str(), v).map_err(se)?;
      Ok(())
    },
    (Plan::Factory { factory, worker_scoped }, None) => {
      setup
        .around(ctx, &name, || {
          run_fixture_factory(ctx, world_obj, reg_idx, &name, factory, worker_scoped, source_label)
        })
        .await
    },
  }
}

/// What a fixture's step is called: `{ title }` when it has one, the
/// fixture's own name otherwise, quoted inside `Fixture "…"`
/// (`worker/fixtureRunner.ts:48-50`).
pub(super) fn fixture_step_title(title: Option<&str>, name: &str) -> String {
  format!("Fixture \"{}\"", title.unwrap_or(name))
}

/// The `{ timeout, title, box }` half of a fixture's option bag.
struct FixtureSetup {
  timeout_ms: Option<u64>,
  title: Option<String>,
  boxed: Option<FixtureBox>,
}

impl FixtureSetup {
  /// Run a fixture's setup as Playwright runs it: inside a step titled
  /// `Fixture "name"`, under its own timeout.
  ///
  /// `box: 'self'` opens no step — the fixture still runs, it just does
  /// not appear — and `box: true` keeps the step but marks it grouped,
  /// which is what keeps a framework's own fixtures out of the way of a
  /// test's steps (`worker/fixtureRunner.ts:47-53`).
  async fn around<F, Fut>(&self, ctx: &Ctx<'_>, name: &str, body: F) -> Result<(), ScriptError>
  where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), ScriptError>>,
  {
    let run = async {
      match self.timeout_ms {
        Some(ms) if ms > 0 => match tokio::time::timeout(std::time::Duration::from_millis(ms), body()).await {
          Ok(r) => r,
          Err(_) => Err(ScriptError::internal(format!(
            "fixture `{name}` setup timed out after {ms}ms"
          ))),
        },
        _ => body().await,
      }
    };
    if self.boxed == Some(FixtureBox::SelfOnly) {
      return run.await;
    }
    let Some(bridge) = optional_bridge(ctx) else {
      // No test is running (collection, a worker-scope setup outside a
      // test): the fixture still resolves, it just has nothing to
      // attach a step to.
      return run.await;
    };
    let mut spec = ferridriver_test::step::StepSpec::new(fixture_step_title(self.title.as_deref(), name));
    spec.category = ferridriver_test::model::StepCategory::Fixture;
    if self.boxed == Some(FixtureBox::Group) {
      // Upstream marks a boxed fixture's step `group: 'configuration'`
      // for one a user registered (`worker/fixtureRunner.ts:52`).
      // ferridriver has no framework-registered boxed fixture, so that
      // is the only group reachable here.
      spec.metadata = Some(serde_json::json!({ "group": "configuration" }));
    }
    ferridriver_test::step::run(&*bridge, spec, move |_run| async move {
      run
        .await
        .map_err(|e| ferridriver_test::step::StepBodyError::new("Error", e.message))
    })
    .await
    .map_err(|f| ScriptError::internal(f.message()))
  }
}

/// A fixture function's third argument: the running test's `testInfo`
/// for a test-scoped fixture, and for a worker-scoped one the worker's
/// part of it (`workerIndex`, `parallelIndex`, `project`), which every
/// test in the worker shares. `undefined` when no test is running.
fn fixture_info<'js>(ctx: &Ctx<'js>, worker_scoped: bool) -> Result<Value<'js>, ScriptError> {
  let saved = with_test_registry(ctx, |r| r.current.as_ref().map(|c| c.test_info.clone()))?;
  let Some(info) = saved else {
    return Ok(Value::new_undefined(ctx.clone()));
  };
  let info = info.restore(ctx).map_err(se)?;
  if !worker_scoped {
    return Ok(info.into_value());
  }
  let worker = Object::new(ctx.clone()).map_err(se)?;
  for key in ["workerIndex", "parallelIndex", "project", "config"] {
    let value: Value<'js> = info.get(key).map_err(se)?;
    if !value.is_undefined() {
      worker.set(key, value).map_err(se)?;
    }
  }
  Ok(worker.into_value())
}

/// Run one `use()`-handshake factory to its suspension point: call
/// `factory(fixtures, use, info)`, drive it on the VM schedular, and wait
/// until it either calls `use(value)` (setup done, factory parked on
/// the gate promise) or settles without doing so (error).
async fn run_fixture_factory<'js>(
  ctx: &Ctx<'js>,
  world_obj: &Object<'js>,
  reg_idx: usize,
  name: &str,
  factory: Persistent<Function<'static>>,
  worker_scoped: bool,
  source_label: &str,
) -> Result<(), ScriptError> {
  let factory = factory.restore(ctx).map_err(se)?;
  let (setup_tx, mut setup_rx) = tokio::sync::oneshot::channel::<()>();
  let setup_slot: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>> = Arc::new(Mutex::new(Some(setup_tx)));

  let use_name = name.to_string();
  let use_fn = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, value: Value<'js>| -> rquickjs::Result<Promise<'js>> {
      let (gate, resolve, _reject) = Promise::new(&ctx)?;
      let resolve_saved = Persistent::save(&ctx, resolve);
      let value_saved = Persistent::save(&ctx, value.clone());
      let world = with_test_registry(&ctx, |r| {
        if worker_scoped {
          r.worker_fixtures.insert(
            reg_idx,
            WorkerFixture {
              name: use_name.clone(),
              value: value_saved,
              gate_resolve: Some(resolve_saved),
              done_rx: None,
            },
          );
        } else if let Some(c) = r.current.as_mut() {
          c.pending.push(PendingFixture {
            name: use_name.clone(),
            gate_resolve: Some(resolve_saved),
            done_rx: None,
          });
        }
        r.current.as_ref().map(|c| c.world.clone())
      })
      .map_err(|e| throw_script_error(&ctx, &e))?;
      if let Some(w) = world {
        let w = w.restore(&ctx)?;
        w.set(use_name.as_str(), value)?;
      }
      let tx = setup_slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
      if let Some(tx) = tx {
        let _ = tx.send(());
      }
      Ok(gate)
    },
  )
  .map_err(se)?;

  let info = fixture_info(ctx, worker_scoped)?;
  let called: rquickjs::Result<MaybePromise<'_>> = factory.call((world_obj.clone(), use_fn, info));
  let mp = called.catch(ctx).map_err(|e| {
    let inner = caught_to_script_error(e, source_label);
    ScriptError::internal(format!("fixture `{name}` setup failed: {}", inner.message))
  })?;
  let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
  let fut = mp.into_future::<Value<'_>>();
  let ctx2 = ctx.clone();
  let label = source_label.to_string();
  ctx.spawn(async move {
    let r = match fut.await.catch(&ctx2) {
      Ok(_) => Ok(()),
      Err(e) => Err(caught_to_script_error(e, &label).message),
    };
    let _ = done_tx.send(r);
  });

  let setup_outcome = tokio::select! {
    setup = &mut setup_rx => setup,
    done = &mut done_rx => {
      return match done {
        Ok(Err(msg)) => Err(ScriptError::internal(format!("fixture `{name}` setup failed: {msg}"))),
        _ => Err(ScriptError::internal(format!(
          "fixture `{name}` factory finished without calling use()"
        ))),
      };
    },
  };
  if setup_outcome.is_err() {
    // The setup channel closed without a send: the factory settled (or
    // was collected) without ever calling `use()` — the completion
    // channel tells which.
    return match done_rx.await {
      Ok(Err(msg)) => Err(ScriptError::internal(format!("fixture `{name}` setup failed: {msg}"))),
      _ => Err(ScriptError::internal(format!(
        "fixture `{name}` factory finished without calling use()"
      ))),
    };
  }

  // Park the completion receiver for teardown.
  with_test_registry(ctx, |r| {
    if worker_scoped {
      if let Some(w) = r.worker_fixtures.get_mut(&reg_idx) {
        w.done_rx = Some(done_rx);
      }
    } else if let Some(c) = r.current.as_mut()
      && let Some(p) = c.pending.iter_mut().rev().find(|p| p.name == name)
    {
      p.done_rx = Some(done_rx);
    }
  })
}

/// Resume suspended test-scoped fixture factories (LIFO) and await
/// their teardown halves. Returns the first teardown error.
pub(crate) async fn teardown_test_fixtures(ctx: &Ctx<'_>) -> Result<(), ScriptError> {
  let mut first_err: Option<ScriptError> = None;
  loop {
    let entry = with_test_registry(ctx, |r| r.current.as_mut().and_then(|c| c.pending.pop()))?;
    let Some(mut entry) = entry else { break };
    if let Some(resolve) = entry.gate_resolve.take() {
      let resolve = resolve.restore(ctx).map_err(se)?;
      let called: rquickjs::Result<()> = resolve.call((Value::new_undefined(ctx.clone()),));
      if let Err(e) = called.catch(ctx) {
        let msg = caught_to_script_error(e, "fixture teardown").message;
        first_err
          .get_or_insert_with(|| ScriptError::internal(format!("fixture `{}` teardown failed: {msg}", entry.name)));
        continue;
      }
    }
    if let Some(done_rx) = entry.done_rx.take()
      && let Ok(Err(msg)) = done_rx.await
    {
      first_err
        .get_or_insert_with(|| ScriptError::internal(format!("fixture `{}` teardown failed: {msg}", entry.name)));
    }
  }
  match first_err {
    Some(e) => Err(e),
    None => Ok(()),
  }
}

/// A failure caught from test code, with the `matcherResult.ariaSnapshot`
/// an `expect` failure carries, which the host needs alongside the
/// message to write `error-context.md`.
struct Thrown {
  error: ScriptError,
  aria_snapshot: Option<String>,
}

impl Thrown {
  fn caught(caught: rquickjs::CaughtError<'_>, source: &str) -> Self {
    let thrown = match &caught {
      rquickjs::CaughtError::Exception(e) => Some(e.as_object()),
      rquickjs::CaughtError::Value(v) => v.as_object(),
      rquickjs::CaughtError::Error(_) => None,
    };
    let aria_snapshot = thrown
      .and_then(|e| e.get::<_, Object<'_>>("matcherResult").ok())
      .and_then(|r| r.get::<_, Option<String>>("ariaSnapshot").ok().flatten());
    Self {
      error: caught_to_script_error(caught, source),
      aria_snapshot,
    }
  }
}

impl From<ScriptError> for Thrown {
  fn from(error: ScriptError) -> Self {
    Self {
      error,
      aria_snapshot: None,
    }
  }
}

/// Resume every suspended worker-scoped fixture factory — the glue
/// calls this once per worker session after the run completes.
///
/// # Errors
///
/// When the VM has shut down, the test registry is missing, or a fixture's
/// teardown throws (the first failure is returned once every fixture has
/// been resumed).
pub async fn teardown_worker_fixtures(vm: &ferrijs::VmHandle) -> Result<(), ScriptError> {
  crate::vm_with!(vm => |ctx| {
    let mut first_err: Option<ScriptError> = None;
    loop {
      // Highest registration index first: within an extend chain that is
      // reverse setup order, so an override is torn down before the
      // super it shadows (hash order would be arbitrary).
      let entry = with_test_registry(&ctx, |r| {
        let key = r.worker_fixtures.keys().copied().max();
        key.and_then(|k| r.worker_fixtures.remove(&k))
      })?;
      let Some(mut fixture) = entry else { break };
      let name = fixture.name.clone();
      if let Some(resolve) = fixture.gate_resolve.take() {
        let resolve = resolve.restore(&ctx).map_err(se)?;
        let called: rquickjs::Result<()> = resolve.call((Value::new_undefined(ctx.clone()),));
        if let Err(e) = called.catch(&ctx) {
          let msg = caught_to_script_error(e, "fixture teardown").message;
          first_err.get_or_insert_with(|| ScriptError::internal(format!("fixture `{name}` teardown failed: {msg}")));
          continue;
        }
      }
      if let Some(done_rx) = fixture.done_rx.take()
        && let Ok(Err(msg)) = done_rx.await
      {
        first_err.get_or_insert_with(|| ScriptError::internal(format!("fixture `{name}` teardown failed: {msg}")));
      }
    }
    match first_err {
      Some(e) => Err(e),
      None => Ok(()),
    }
  })
  .await?
}

/// Names of custom fixtures a test + its each-hooks request (their
/// requested lists intersected with the fixture set happens in
/// `resolve_custom_fixtures`).
/// What each function of this test asks for, labelled the way
/// Playwright labels it in the unknown-parameter message
/// (`common/poolBuilder.ts:66-71`: `'Test'` for the body,
/// `hook.type + ' hook'` for each hook).
fn requested_per_function(reg: &TestRegistry, spec: &RunTestSpec) -> Vec<(String, Vec<String>)> {
  let mut out = vec![(
    "Test".to_string(),
    reg.tests[spec.test_idx].requested.clone().unwrap_or_default(),
  )];
  for &h in spec.hooks_before.iter().chain(spec.hooks_after.iter()) {
    let hook = &reg.hooks[h];
    out.push((
      format!("{} hook", hook.kind),
      hook.requested.clone().unwrap_or_default(),
    ));
  }
  for &m in &spec.modifiers {
    let modifier = &reg.modifiers[m];
    out.push((
      format!("{} modifier", modifier.kind),
      modifier.requested.clone().unwrap_or_default(),
    ));
  }
  out
}

fn requested_names(reg: &TestRegistry, spec: &RunTestSpec) -> Vec<String> {
  let mut names: Vec<String> = Vec::new();
  let mut add = |req: &Option<Vec<String>>| {
    if let Some(list) = req {
      for n in list {
        if !names.contains(n) {
          names.push(n.clone());
        }
      }
    }
  };
  add(&reg.tests[spec.test_idx].requested);
  for &h in spec.hooks_before.iter().chain(spec.hooks_after.iter()) {
    add(&reg.hooks[h].requested);
  }
  for &m in &spec.modifiers {
    add(&reg.modifiers[m].requested);
  }
  names
}

/// Execute one registered test end-to-end inside the worker's VM:
/// build the fixtures object, resolve custom fixtures, run each-hooks
/// and the body, tear down, and clear the current-test slot. The
/// returned error carries the raw (bundled) stack — the glue remaps it.
///
/// # Errors
///
/// When the VM has shut down, `spec.test_idx` is out of range, setting up
/// the test fails, the body or a hook throws, or a fixture's teardown fails.
pub async fn run_test(
  vm: &ferrijs::VmHandle,
  spec: RunTestSpec,
  world: TestWorldData,
  bridge: Arc<dyn TestHostBridge>,
) -> Result<(), ScriptError> {
  let route_vm = vm.clone();
  crate::vm_with!(vm => |ctx| {
    let test_count = with_test_registry(&ctx, |r| r.tests.len())?;
    if spec.test_idx >= test_count {
      return Err(ScriptError::internal(format!(
        "test index {} out of range ({} registered)",
        spec.test_idx, test_count
      )));
    }
    set_current_test(&ctx, &route_vm, &world, Arc::clone(&bridge))?;

    let result = drive_current_test(&ctx, &spec, &world).await;

    fold_pushed_annotations(&ctx);
    let teardown = teardown_test_fixtures(&ctx).await;
    let _ = with_test_registry(&ctx, |r| r.current = None);
    match (result, teardown) {
      (Err(e), _) | (Ok(()), Err(e)) => Err(e),
      (Ok(()), Ok(())) => Ok(()),
    }
  })
  .await?
}

/// The globals a plain script sees for the length of its run: the test's
/// own fixtures, under the names a test body destructures them by.
const SCRIPT_FIXTURES: [&str; 4] = ["page", "context", "browser", "request"];

/// Execute a plain script as the body of one test: the base chain's
/// fixtures (an extension's auto fixtures and overrides included) are set
/// up as for any test, put on the global object together with the base
/// `test`, and the script's module evaluates from its own bytecode, so
/// every run starts from a fresh top level and its frames map through its
/// own source map onto the file the user wrote.
///
/// # Errors
///
/// When the VM has shut down, setting up the test fails, the script
/// throws, or a fixture's teardown fails.
pub async fn run_script_test(
  vm: &ferrijs::VmHandle,
  script: Arc<crate::bundle::CompiledBundle>,
  world: TestWorldData,
  bridge: Arc<dyn TestHostBridge>,
) -> Result<(), ScriptError> {
  let route_vm = vm.clone();
  crate::vm_with!(vm => |ctx| {
    set_current_test(&ctx, &route_vm, &world, Arc::clone(&bridge))?;
    let result = drive_script(&ctx, &script, &world).await;
    clear_script_globals(&ctx);
    fold_pushed_annotations(&ctx);
    let teardown = teardown_test_fixtures(&ctx).await;
    let _ = with_test_registry(&ctx, |r| r.current = None);
    match (result, teardown) {
      (Err(thrown), _) => {
        bridge.set_failure_context(thrown.aria_snapshot);
        Err(thrown.error)
      },
      (Ok(()), Err(e)) => Err(e),
      (Ok(()), Ok(())) => Ok(()),
    }
  })
  .await?
}

async fn drive_script(
  ctx: &Ctx<'_>,
  script: &crate::bundle::CompiledBundle,
  world: &TestWorldData,
) -> Result<(), Thrown> {
  let world_obj = with_test_registry(ctx, |r| r.current.as_ref().map(|c| c.world.clone()))?
    .ok_or_else(|| ScriptError::internal("no current test".to_string()))?
    .restore(ctx)
    .map_err(se)?;
  let requested: Vec<String> = SCRIPT_FIXTURES.iter().map(|name| (*name).to_string()).collect();
  resolve_custom_fixtures(
    ctx,
    &world_obj,
    BASE_FIXTURE_SET,
    &requested,
    &world.use_options,
    &script.module_name,
  )
  .await?;
  let globals = ctx.globals();
  for name in SCRIPT_FIXTURES {
    let value: Value<'_> = world_obj.get(name).map_err(se)?;
    globals.set(name, value).map_err(se)?;
  }
  let test = crate::bindings::runtime::ensure_ferridriver(ctx)
    .and_then(|fd| fd.get::<_, Value<'_>>("test"))
    .map_err(se)?;
  globals.set("test", test).map_err(se)?;

  crate::bindings::call_site::register_bundle(ctx, script.mapper());
  // SAFETY: `script.bytecode` was produced by `Module::write` by this
  // exact rquickjs/QuickJS build, in this process or restored from the
  // bytecode disk cache whose ABI tag guarantees an ABI-identical
  // toolchain wrote it: the precondition `Module::load` documents. QuickJS
  // creates a new module at every load, so a retry evaluates it afresh.
  #[expect(unsafe_code, reason = "rquickjs declares loading module bytecode unsafe")]
  let declared = unsafe { rquickjs::Module::load(ctx.clone(), &script.bytecode) }
    .catch(ctx)
    .map_err(|e| Thrown::caught(e, &script.module_name))?;
  let (_, promise) = declared
    .eval()
    .catch(ctx)
    .map_err(|e| Thrown::caught(e, &script.module_name))?;
  promise
    .into_future::<()>()
    .await
    .catch(ctx)
    .map_err(|e| Thrown::caught(e, &script.module_name))
}

/// A later test in the same VM must not find this run's page.
fn clear_script_globals(ctx: &Ctx<'_>) {
  let globals = ctx.globals();
  for name in SCRIPT_FIXTURES.iter().chain(&["test"]) {
    let _ = globals.remove(*name);
  }
}

async fn drive_current_test(ctx: &Ctx<'_>, spec: &RunTestSpec, world: &TestWorldData) -> Result<(), ScriptError> {
  let (world_obj, info_obj, bridge, fixture_set, each_arg, func, per_function, custom, has_custom_fixtures) =
    with_test_registry(ctx, |r| {
      let current = r
        .current
        .as_ref()
        .ok_or_else(|| ScriptError::internal("no current test".to_string()))?;
      let t = &r.tests[spec.test_idx];
      let has_custom = r.fixture_sets.get(t.fixture_set).is_some_and(|s| !s.is_empty());
      let per_fn = requested_per_function(r, spec);
      let req = if has_custom {
        requested_names(r, spec)
      } else {
        Vec::new()
      };
      Ok::<_, ScriptError>((
        current.world.clone(),
        current.test_info.clone(),
        Arc::clone(&current.bridge),
        t.fixture_set,
        t.each_arg.clone(),
        t.func.clone(),
        per_fn,
        req,
        has_custom,
      ))
    })??;
  let world_obj = world_obj.restore(ctx).map_err(se)?;
  let info_obj = info_obj.restore(ctx).map_err(se)?;

  if !per_function.is_empty() {
    validate_requested_names(ctx, &world_obj, fixture_set, &per_function)?;
  }
  if has_custom_fixtures {
    resolve_custom_fixtures(
      ctx,
      &world_obj,
      fixture_set,
      &custom,
      &world.use_options,
      &spec.source_label,
    )
    .await?;
  }

  let mut first_err: Option<Thrown> = None;

  // Modifiers first, then hooks — Playwright's order
  // (`worker/workerMain.ts:556`). A `skip` here must stop the test
  // before its beforeEach hooks run, but afterEach still runs, so the
  // failure travels the same road a hook failure does.
  for &m in &spec.modifiers {
    match apply_suite_modifier(ctx, m, &world_obj, &info_obj, &spec.source_label).await {
      Ok(()) => {},
      Err(e) => {
        first_err = Some(e.into());
        break;
      },
    }
  }

  for &h in &spec.hooks_before {
    if let Err(e) = invoke_hook_fn(ctx, h, &world_obj, &info_obj, &spec.source_label).await {
      first_err = Some(e);
      break;
    }
  }

  if first_err.is_none() {
    let func = func.restore(ctx).map_err(se)?;
    let second: Value<'_> = match &each_arg {
      Some(row) => crate::bindings::convert::json_to_js(ctx, row).map_err(se)?,
      None => info_obj.clone().into_value(),
    };
    let called: rquickjs::Result<MaybePromise<'_>> = func.call((world_obj.clone(), second));
    match called.catch(ctx) {
      Ok(mp) => {
        if let Err(e) = mp.into_future::<Value<'_>>().await.catch(ctx) {
          first_err = Some(Thrown::caught(e, &spec.source_label));
        }
      },
      Err(e) => first_err = Some(Thrown::caught(e, &spec.source_label)),
    }
  }

  // afterEach hooks always run (Playwright); the first error wins.
  for &h in &spec.hooks_after {
    if let Err(e) = invoke_hook_fn(ctx, h, &world_obj, &info_obj, &spec.source_label).await {
      first_err.get_or_insert(e);
    }
  }

  match first_err {
    Some(thrown) => {
      bridge.set_failure_context(thrown.aria_snapshot);
      Err(thrown.error)
    },
    None => Ok(()),
  }
}

/// Evaluate one suite modifier with the test's fixtures and apply it.
///
/// Playwright wraps the callback so its result becomes
/// `testInfo._modifier(type, location, [!!result, description])`
/// (`worker/workerMain.ts:545-548`) — the truthy return is the
/// condition, and the description came from the registration. A
/// `skip`/`fixme` therefore aborts the test exactly as the runtime form
/// does, through the sentinel the worker recognises.
async fn apply_suite_modifier<'js>(
  ctx: &Ctx<'js>,
  modifier_idx: usize,
  world_obj: &Object<'js>,
  info_obj: &Object<'js>,
  source_label: &str,
) -> Result<(), ScriptError> {
  let (kind, description, func) = with_test_registry(ctx, |r| {
    r.modifiers
      .get(modifier_idx)
      .map(|m| (m.kind.clone(), m.description.clone(), m.func.clone()))
      .ok_or_else(|| ScriptError::internal(format!("modifier index {modifier_idx} out of range")))
  })??;
  let func = func.restore(ctx).map_err(se)?;
  let called: rquickjs::Result<MaybePromise<'js>> = func.call((world_obj.clone(), info_obj.clone()));
  let mp = called.catch(ctx).map_err(|e| caught_to_script_error(e, source_label))?;
  let result: Value<'js> = mp
    .into_future()
    .await
    .catch(ctx)
    .map_err(|e| caught_to_script_error(e, source_label))?;
  if !truthy(&result) {
    return Ok(());
  }
  let bridge = with_test_registry(ctx, |r| r.current.as_ref().map(|c| Arc::clone(&c.bridge)))?
    .ok_or_else(|| ScriptError::internal("no current test for a suite modifier".to_string()))?;
  bridge.annotate(kind.clone(), description.clone());
  match kind.as_str() {
    "skip" | "fixme" => {
      bridge.set_skip(description.clone());
      Err(ScriptError::internal(format!(
        "{TEST_SKIP_SENTINEL}{}",
        description.unwrap_or_default()
      )))
    },
    "fail" => {
      bridge.set_expected_failure();
      Ok(())
    },
    "slow" => {
      bridge.set_slow();
      Ok(())
    },
    other => Err(ScriptError::internal(format!("unknown suite modifier `{other}`"))),
  }
}

async fn invoke_hook_fn<'js>(
  ctx: &Ctx<'js>,
  hook_idx: usize,
  world_obj: &Object<'js>,
  info_obj: &Object<'js>,
  source_label: &str,
) -> Result<(), Thrown> {
  let func = with_test_registry(ctx, |r| {
    r.hooks
      .get(hook_idx)
      .map(|h| h.func.clone())
      .ok_or_else(|| ScriptError::internal(format!("hook index {hook_idx} out of range")))
  })??;
  let func = func.restore(ctx).map_err(se)?;
  let called: rquickjs::Result<MaybePromise<'_>> = func.call((world_obj.clone(), info_obj.clone()));
  let mp = called.catch(ctx).map_err(|e| Thrown::caught(e, source_label))?;
  mp.into_future::<Value<'_>>()
    .await
    .catch(ctx)
    .map(|_| ())
    .map_err(|e| Thrown::caught(e, source_label))
}

/// Execute one `beforeAll`/`afterAll` hook with its own fixtures
/// object and current-test slot (so `test.info()`/steps work inside).
///
/// # Errors
///
/// When the VM has shut down, setting up the hook's fixtures fails, a
/// requested fixture name is unknown, the hook throws, or a fixture's
/// teardown fails.
pub async fn run_standalone_hook(
  vm: &ferrijs::VmHandle,
  hook_idx: usize,
  world: TestWorldData,
  bridge: Arc<dyn TestHostBridge>,
  source_label: String,
) -> Result<(), ScriptError> {
  let route_vm = vm.clone();
  crate::vm_with!(vm => |ctx| {
    set_current_test(&ctx, &route_vm, &world, Arc::clone(&bridge))?;
    let (world_obj, info_obj) = with_test_registry(&ctx, |r| {
      r.current
        .as_ref()
        .map(|c| (c.world.clone(), c.test_info.clone()))
        .ok_or_else(|| ScriptError::internal("no current test".to_string()))
    })??;
    let world_obj = world_obj.restore(&ctx).map_err(se)?;
    let info_obj = info_obj.restore(&ctx).map_err(se)?;

    let (requested, fixture_set, kind) = with_test_registry(&ctx, |r| {
      let hook = r.hooks.get(hook_idx);
      let req = hook.and_then(|h| h.requested.clone()).unwrap_or_default();
      // The chain the hook was registered on, so a `beforeAll` declared
      // on an extended object can ask for that chain's fixtures.
      (
        req,
        hook.map_or(0, |h| h.fixture_set),
        hook.map_or_else(String::new, |h| h.kind.clone()),
      )
    })?;
    validate_requested_names(
      &ctx,
      &world_obj,
      fixture_set,
      &[(format!("{kind} hook"), requested.clone())],
    )?;
    let result = async {
      resolve_custom_fixtures(&ctx, &world_obj, fixture_set, &requested, &world.use_options, &source_label).await?;
      invoke_hook_fn(&ctx, hook_idx, &world_obj, &info_obj, &source_label)
        .await
        .map_err(|thrown| {
          bridge.set_failure_context(thrown.aria_snapshot);
          thrown.error
        })
    }
    .await;

    fold_pushed_annotations(&ctx);
    let teardown = teardown_test_fixtures(&ctx).await;
    let _ = with_test_registry(&ctx, |r| r.current = None);
    match (result, teardown) {
      (Err(e), _) | (Ok(()), Err(e)) => Err(e),
      (Ok(()), Ok(())) => Ok(()),
    }
  })
  .await?
}
