//! Playwright's `selectors` — custom selector engines and the
//! `getByTestId` attribute.
//!
//! Both rules are core's (`ferridriver::selectors`): the registry, the
//! duplicate-name message, and the
//! `Function | string | { path, content }` lowering. This decides only
//! which of those shapes JS passed.

// `#[napi]` exports these to JS but clippy's reachability check only
// follows Rust call graphs, so it flags the entry points as dead. Same
// module-level exemption every other NAPI entry-point module carries.
#![allow(dead_code)]

use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::error::to_napi;

/// `selectors.register(name, script, options)`.
///
/// The script is evaluated in the page, so it arrives as source: a
/// function (stringified by the caller's engine), an expression, or a
/// file to read.
///
/// # Errors
///
/// When the name is already registered, or a `path` cannot be read.
/// The script is read on the JS thread — an `Unknown` cannot cross into
/// a future — and the registration itself runs inside the returned
/// `AsyncBlock`, so a duplicate name rejects the promise rather than
/// throwing synchronously. Playwright's `register` returns
/// `Promise<void>`.
#[napi(
  namespace = "selectors",
  js_name = "register",
  ts_args_type = "name: string, script: Function | string | { path?: string, content?: string }, options?: { contentScript?: boolean }",
  ts_return_type = "Promise<void>"
)]
pub fn register_selector_engine(
  env: Env,
  name: String,
  script: Unknown<'_>,
  options: Option<SelectorEngineOptions>,
) -> Result<AsyncBlock<()>> {
  let lowered = lower_script(&script)?;
  let content_script = options.and_then(|o| o.content_script).unwrap_or(false);
  AsyncBlockBuilder::new(async move {
    let source = ferridriver::selectors::evaluation_script(&lowered).map_err(to_napi)?;
    ferridriver::selectors::register_selector_engine(&name, &source, content_script).map_err(to_napi)
  })
  .build(&env)
}

#[napi(object)]
pub struct SelectorEngineOptions {
  /// Playwright's `contentScript`: run the engine in the page's own
  /// world rather than an isolated one. ferridriver evaluates
  /// everything in the page's world, so `true` is honoured exactly and
  /// `false` — Playwright's default — runs there too. The engine works
  /// either way; what it does not get is isolation from page globals.
  pub content_script: Option<bool>,
}

/// `selectors.setTestIdAttribute(attributeName)` — the process default
/// every context starts from. A comma-separated list matches any of the
/// named attributes.
#[napi(namespace = "selectors", js_name = "setTestIdAttribute")]
pub fn set_test_id_attribute(attribute_name: String) {
  ferridriver::selectors::set_default_test_id_attribute(&attribute_name);
}

/// Which of Playwright's three shapes JS passed. A function is
/// stringified here (`Function.prototype.toString`) because the engine
/// is evaluated in the page, not called in this process.
fn lower_script(script: &Unknown<'_>) -> Result<ferridriver::selectors::SelectorScript> {
  match script.get_type()? {
    ValueType::Function => {
      let object = script.coerce_to_object()?;
      let to_string: Function<'_, (), String> = object.get_named_property("toString")?;
      Ok(ferridriver::selectors::SelectorScript::Function(
        to_string.apply(object, ())?,
      ))
    },
    ValueType::String => Ok(ferridriver::selectors::SelectorScript::Source(
      script.coerce_to_string()?.into_utf8()?.as_str()?.to_string(),
    )),
    ValueType::Object => {
      let bag = script.coerce_to_object()?;
      if let Some(content) = bag.get::<String>("content")? {
        return Ok(ferridriver::selectors::SelectorScript::Source(content));
      }
      if let Some(path) = bag.get::<String>("path")? {
        return Ok(ferridriver::selectors::SelectorScript::Path(path.into()));
      }
      Err(Error::new(
        Status::InvalidArg,
        "Either path or content property must be present",
      ))
    },
    other => Err(Error::new(
      Status::InvalidArg,
      format!("selectors.register: script must be a function, a string, or {{ path }} / {{ content }}, got {other:?}"),
    )),
  }
}
