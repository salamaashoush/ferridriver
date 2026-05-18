//! Step-file front-end: rolldown bundle + tree-shake + TypeScript ->
//! one ESM module -> compiled to `QuickJS` bytecode once.
//!
//! rolldown (built on oxc) resolves the whole import graph including
//! `node_modules`, transpiles `.ts`/`.tsx`, tree-shakes, and emits a
//! single ESM chunk. That chunk is compiled to bytecode a single time;
//! every per-worker session links the bytecode (one `Module::load`, no
//! parse, no resolver). A hidden source map is kept so a JS error in
//! the bundled output is reported at the original `.ts`/`.js` location.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rolldown::{Bundler, BundlerOptions, InputItem, OutputFormat, Platform, SourceMapType};
use rolldown_common::Output;
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Module, WriteOptions, WriteOptionsEndianness, async_with};

use crate::engine::caught_to_script_error;
use crate::error::ScriptError;

/// One bundled+tree-shaken step graph compiled to `QuickJS` bytecode,
/// plus the source map to translate bundled positions back to source.
pub struct CompiledBundle {
  pub module_name: String,
  pub bytecode: Arc<[u8]>,
  source_map: Option<sourcemap::SourceMap>,
}

/// rolldown-bundle + tree-shake + transpile the step entry files (and
/// their `node_modules`/shared imports) into a single ESM module.
/// Returns the bundled code and the (hidden) source map JSON. Exposed
/// for diagnostics/tests; production uses [`bundle_and_compile`].
pub async fn bundle_source(entry_paths: &[PathBuf], cwd: &Path) -> Result<(String, Option<String>), ScriptError> {
  if entry_paths.is_empty() {
    return Err(ScriptError::internal("no step entry files".to_string()));
  }

  let input: Vec<InputItem> = entry_paths
    .iter()
    .map(|p| InputItem {
      name: None,
      import: p.to_string_lossy().into_owned(),
    })
    .collect();

  let options = BundlerOptions {
    input: Some(input),
    cwd: Some(cwd.to_path_buf()),
    // Neutral: no Node builtins are injected (QuickJS has none); pure
    // ESM/CJS node_modules still resolve and bundle.
    platform: Some(Platform::Neutral),
    format: Some(OutputFormat::Esm),
    // Hidden: emit the map but no `//# sourceMappingURL` trailer in the
    // code we feed to QuickJS.
    sourcemap: Some(SourceMapType::Hidden),
    ..Default::default()
  };

  let mut bundler = Bundler::new(options).map_err(|e| ScriptError::internal(format!("rolldown init: {e:?}")))?;
  // rolldown's generate future is large; box it so it doesn't bloat the
  // enclosing future.
  let out = Box::pin(bundler.generate())
    .await
    .map_err(|e| ScriptError::internal(format!("rolldown bundle: {e:?}")))?;

  for asset in &out.assets {
    if let Output::Chunk(chunk) = asset {
      if chunk.is_entry {
        let code = chunk.code.clone();
        return Ok(match &chunk.map {
          Some(m) => (code, Some(m.to_json_string())),
          None => (code, None),
        });
      }
    }
  }
  Err(ScriptError::internal("rolldown produced no entry chunk".to_string()))
}

/// Bundle the step entry files (TypeScript ok; `node_modules` and
/// shared utils resolved + tree-shaken) into one ESM module and compile
/// it to bytecode. Done once, before workers spawn.
pub async fn bundle_and_compile(entry_paths: &[PathBuf], cwd: &Path) -> Result<CompiledBundle, ScriptError> {
  let (code, map_json) = Box::pin(bundle_source(entry_paths, cwd)).await?;
  let source_map = map_json.and_then(|j| sourcemap::SourceMap::from_slice(j.as_bytes()).ok());

  let module_name = "ferridriver-bdd-steps.js".to_string();
  let name = module_name.clone();
  let runtime = AsyncRuntime::new().map_err(|e| ScriptError::internal(format!("bytecode runtime: {e}")))?;
  let ctx = AsyncContext::full(&runtime)
    .await
    .map_err(|e| ScriptError::internal(format!("bytecode context: {e}")))?;
  let bytecode: Vec<u8> = async_with!(ctx => |ctx| {
    // Bundled module has no remaining imports — `declare` (parse only,
    // no execution) + `write` needs no resolver.
    let module = Module::declare(ctx.clone(), name.into_bytes(), code.into_bytes())
      .catch(&ctx)
      .map_err(|e| caught_to_script_error(e, ""))?;
    module
      .write(WriteOptions {
        endianness: WriteOptionsEndianness::Native,
        ..Default::default()
      })
      .map_err(|e| ScriptError::internal(format!("module write: {e}")))
  })
  .await?;

  Ok(CompiledBundle {
    module_name,
    bytecode: Arc::from(bytecode.into_boxed_slice()),
    source_map,
  })
}

/// Link + evaluate the bundled step module from precompiled bytecode in
/// the given session. Top-level `Given`/`When`/`Then` run here.
pub async fn eval_bundle(actx: &AsyncContext, bundle: &CompiledBundle) -> Result<(), ScriptError> {
  let bytecode = Arc::clone(&bundle.bytecode);
  let label = bundle.module_name.clone();
  async_with!(actx => |ctx| {
    // SAFETY: produced by `Module::write` in THIS process and
    // rquickjs/QuickJS build with native endianness, never persisted —
    // the precondition `Module::load` documents.
    #[allow(unsafe_code)]
    let module = match (unsafe { Module::load(ctx.clone(), &bytecode) }).catch(&ctx) {
      Ok(m) => m,
      Err(e) => return Err(caught_to_script_error(e, &label)),
    };
    let promise = match module.eval().catch(&ctx) {
      Ok((_evaluated, p)) => p,
      Err(e) => return Err(caught_to_script_error(e, &label)),
    };
    match promise.into_future::<()>().await.catch(&ctx) {
      Ok(()) => Ok(()),
      Err(e) => Err(caught_to_script_error(e, &label)),
    }
  })
  .await
}

impl CompiledBundle {
  /// Map a bundled-output `line:col` (1-based, as QuickJS reports) back
  /// to the original `.ts`/`.js` source location.
  #[must_use]
  pub fn remap(&self, line: u32, col: u32) -> Option<(String, u32, u32)> {
    let sm = self.source_map.as_ref()?;
    let token = sm.lookup_token(line.saturating_sub(1), col.saturating_sub(1))?;
    let src = token.get_source().unwrap_or("<unknown>").to_string();
    Some((src, token.get_src_line() + 1, token.get_src_col() + 1))
  }
}
