//! Evaluating a `--config <file.ts|.js>`.
//!
//! `.ts` / `.js` is a config FORMAT, not a special case: a module layer
//! occupies whatever slot its file occupies, and its default export is a
//! whole configuration document, exactly as a `.toml` file's contents
//! are. The only thing that makes it different is that parsing it needs
//! a bundler and a JavaScript runtime — which is why the config crate
//! calls in here through a loader rather than owning this itself.
//!
//! The module goes through the same rolldown -> bytecode pipeline every
//! other script takes, so a config can `import` helpers, share types
//! with the suite and be written in TypeScript without a build step.
//! Nothing here decides what the document MEANS, which slot it occupies
//! or which keys it may not set.
//!
//! Reached only for a stack that actually HAS a module layer. A
//! configuration written entirely in `.toml` / `.yaml` / `.json` never
//! constructs any of this.

use std::path::Path;
use std::sync::Arc;

use rquickjs::Value;

use crate::bundle::{bundle_and_compile_named, eval_bundle_with};
use crate::engine::{ExtensionHost, RunContext, ScriptCaps, ScriptEngineConfig, Session};
use crate::error::ScriptError;

/// Bundle, evaluate and read the default export of a config module.
///
/// # Errors
///
/// Fails when the module does not bundle, its top level throws, it has
/// no default export, or that export is not an object.
pub async fn evaluate(path: &Path, cwd: &Path, caps: ScriptCaps) -> Result<serde_json::Value, ScriptError> {
  let entry = path.to_path_buf();
  // Its own cache kind: a config module and a spec file with the same
  // path would otherwise share an entry, and they are compiled under
  // different module names.
  let bundle =
    bundle_and_compile_named(std::slice::from_ref(&entry), cwd, &format!("config:{}", path.display())).await?;

  let sandbox = Arc::new(crate::fs::PathSandbox::new(cwd)?);
  let run_ctx = RunContext {
    vars: Arc::new(crate::vars::InMemoryVars::new()),
    sandbox,
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    // The config is what SELECTS a host, so it is evaluated under none
    // of them: a module that branches on `ferridriver.host` while
    // deciding which projects exist would be deciding it from a value
    // that does not exist yet.
    host: ExtensionHost::Script,
    caps,
    session: None,
  };
  let session = Session::create(ScriptEngineConfig::default(), &run_ctx).await?;
  let vm = session.vm_handle();

  let label = path.display().to_string();
  let (tx, rx) = std::sync::mpsc::channel();
  eval_bundle_with(&vm, &bundle, move |ctx, namespace| {
    let exported: Value<'_> = namespace
      .get("default")
      .map_err(|e| ScriptError::internal(format!("config '{label}': reading its default export: {e}")))?;
    if exported.is_undefined() {
      return Err(ScriptError::internal(format!(
        "config '{label}': has no default export — a config module exports its configuration as \
         `export default defineConfig({{ … }})`"
      )));
    }
    let document: serde_json::Value = crate::bindings::convert::serde_from_js(ctx, exported)
      .map_err(|e| ScriptError::internal(format!("config '{label}': reading its default export: {e}")))?;
    if !document.is_object() {
      return Err(ScriptError::internal(format!(
        "config '{label}': its default export must be a configuration object"
      )));
    }
    let _ = tx.send(document);
    Ok(())
  })
  .await?;

  rx.try_recv()
    .map_err(|_| ScriptError::internal(format!("config '{}': produced no document", path.display())))
}
