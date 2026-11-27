//! Playwright-shaped test registration surface for the shared `QuickJS`
//! engine (`import { test, describe, expect } from '@ferridriver/test'`).
//!
//! `test`/`describe` and every modifier (`skip`/`fixme`/`fail`/`slow`/
//! `only`/`each`/`use`/`extend`/hooks) are native Rust functions — no JS
//! glue. Registrations land in a Rust `TestRegistry` held as context
//! userdata (single-threaded VM, `RefCell`). Test bodies are kept as
//! `Persistent` functions; the test-runner glue crate snapshots the
//! registry after the bundled test module evaluates ([`collect_tests`])
//! and invokes bodies by registration index — the same architecture the
//! BDD step surface uses ([`super::bdd`]).
//!
//! No business logic here: filtering, retries, worker scheduling and
//! reporting all stay in the `ferridriver-test` core.

mod builders;
mod collect;
mod registry;
mod runner;
mod step;

use std::cell::RefCell;

use rquickjs::{Ctx, Function, Object, Value};

use crate::bindings::convert::ferri_throw;
use crate::bindings::registry::throw_script_error;
use crate::error::ScriptError;

pub use collect::{
  CollectedFileAnnotation, CollectedFileConfigure, CollectedFileUse, CollectedFixture, CollectedModifier,
  CollectedSuite, CollectedTest, CollectedTestHook, CollectedTests, collect_tests,
};
pub use registry::{CollectedAnnotation, CollectedSuiteMode, TEST_SKIP_SENTINEL, base_is_sealed, seal_base_fixtures};
pub(crate) use registry::{
  TestRegistry, current_bridge, destructured_keys, fixture_set_of, optional_bridge, with_test_registry,
};
pub(crate) use runner::{resolve_custom_fixtures, teardown_test_fixtures};
pub use runner::{run_script_test, run_standalone_hook, run_test, teardown_worker_fixtures};
pub(crate) use step::{
  clear_current_test, current_world_object, fixture_set_table, fixture_table, registration_count, registrations_since,
  registry_marks, set_current_test,
};

use builders::{make_describe_object, make_test_object};
use registry::{BASE_FIXTURE_SET, TestRegistryUserData, define_fixtures, merge_tests};

/// Install the Playwright-shaped test surface: the `TestRegistry`
/// userdata plus the base `test` / `describe` objects and `mergeTests`
/// on the `ferridriver` global (consumed by the `@ferridriver/test`
/// native module — they are NOT bare globals). Idempotent; called once
/// at `Session::create`, for EVERY host: a fixture chain an extension
/// or a step file builds must exist wherever it is imported, and only
/// the Test host consumes what registering leaves behind.
///
/// # Errors
///
/// When building the `test`/`describe` objects, installing `defineConfig`,
/// or setting a property on `ferridriver` or the globals fails.
pub fn install_test(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  if ctx.userdata::<TestRegistryUserData>().is_some() {
    return Ok(());
  }
  let _ = ctx.store_userdata(TestRegistryUserData(RefCell::new(TestRegistry::new())));

  let test = make_test_object(ctx, BASE_FIXTURE_SET)?;
  let describe = make_describe_object(ctx)?;
  let fd = crate::bindings::runtime::ensure_ferridriver(ctx)?;
  // Playwright exports the unextended root as `_baseTest`; it is this
  // very object, not a copy, so `mergeTests(_baseTest, x)` sees the
  // shared ancestor it has to dedup against.
  //
  // Both are read-only: they ARE the base chain's identity, and an
  // importer links to the object it found here. Letting a package
  // reassign either would give it a way to look like it had replaced
  // the base chain while every module that already imported kept the
  // original — the failure `defineFixtures` exists to avoid.
  fd.prop("baseTest", rquickjs::object::Property::from(test.clone()).enumerable())?;
  fd.prop("test", rquickjs::object::Property::from(test).enumerable())?;
  fd.set("describe", describe)?;
  fd.set("mergeTests", Function::new(ctx.clone(), merge_tests)?)?;
  crate::bindings::define_config::install(ctx)?;
  fd.set("selectors", make_selectors_object(ctx)?)?;
  let define_fixtures_fn = Function::new(ctx.clone(), define_fixtures)?;
  define_fixtures_fn.set_name("defineFixtures")?;
  ctx.globals().set("defineFixtures", define_fixtures_fn.clone())?;
  fd.set("defineFixtures", define_fixtures_fn)?;
  Ok(())
}

/// Playwright's `selectors`
/// (`packages/playwright-core/src/client/selectors.ts`): `register(name,
/// script, options)` and `setTestIdAttribute(attributeName)`.
///
/// Both rules are core's — the registry, the duplicate-name message, the
/// `Function | string | { path, content }` lowering — so this only
/// decides which of those shapes the caller passed.
fn make_selectors_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;

  let register = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, name: String, script: Value<'js>, options: rquickjs::function::Opt<Object<'js>>| {
      let lowered = if let Some(func) = script.as_function() {
        let source: String = func
          .as_object()
          .ok_or_else(|| {
            throw_script_error(
              &ctx,
              &ScriptError::internal("selectors.register: script must be a function".to_string()),
            )
          })?
          .get::<_, Function<'js>>("toString")?
          .call((rquickjs::function::This(func.clone()),))?;
        ferridriver::selectors::SelectorScript::Function(source)
      } else if let Some(source) = script.as_string() {
        ferridriver::selectors::SelectorScript::Source(source.to_string()?)
      } else if let Some(bag) = script.as_object() {
        match (
          bag.get::<_, Option<String>>("content")?,
          bag.get::<_, Option<String>>("path")?,
        ) {
          (Some(content), _) => ferridriver::selectors::SelectorScript::Source(content),
          (None, Some(path)) => {
            crate::bindings::convert::readable(&ctx, std::path::Path::new(&path))?;
            ferridriver::selectors::SelectorScript::Path(std::path::PathBuf::from(path))
          },
          (None, None) => {
            return Err(throw_script_error(
              &ctx,
              &ScriptError::internal("Either path or content property must be present".to_string()),
            ));
          },
        }
      } else {
        return Err(throw_script_error(
          &ctx,
          &ScriptError::internal(
            "selectors.register: script must be a function, a string, or { path } / { content }".to_string(),
          ),
        ));
      };

      let content_script = options
        .0
        .map(|o| o.get::<_, Option<bool>>("contentScript"))
        .transpose()?
        .flatten()
        .unwrap_or(false);
      let source = ferridriver::selectors::evaluation_script(&lowered).map_err(|e| ferri_throw(&ctx, &e))?;
      ferridriver::selectors::register_selector_engine(&name, &source, content_script)
        .map_err(|e| ferri_throw(&ctx, &e))?;
      // Playwright's `register` is `Promise<void>`; a suite may `.then`
      // it, so hand back a real promise rather than `undefined`.
      let (promise, resolve, _reject) = rquickjs::Promise::new(&ctx)?;
      resolve.call::<_, ()>(())?;
      Ok::<_, rquickjs::Error>(promise)
    },
  )?;
  obj.set("register", register)?;

  let set_test_id_attribute = Function::new(ctx.clone(), |name: String| {
    ferridriver::selectors::set_default_test_id_attribute(&name);
  })?;
  obj.set("setTestIdAttribute", set_test_id_attribute)?;
  Ok(obj)
}
