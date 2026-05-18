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

/// One plugin file: rolldown-bundled (TypeScript, plugin-local imports,
/// tree-shaking) and compiled to `QuickJS` bytecode, with its manifests
/// extracted straight from the compiled module — no separate throwaway
/// runtime per file.
///
/// `index` is the file's position among successfully compiled plugins.
/// It is baked into the bytecode epilogue so evaluating the module
/// publishes the file's tool array to `globalThis.__ferri_plugin_files[index]`;
/// the per-tool wrapper in [`install_plugins`] looks handlers up by that
/// same index. Survivors are returned with contiguous indices so a
/// broken file never shifts another file's slot.
pub struct CompiledPlugin {
  pub path: PathBuf,
  pub index: usize,
  pub bytecode: Arc<[u8]>,
  /// JSON array (one object per tool, source order, `handler` stripped).
  /// Deserialises into `Vec<PluginManifest>` on the MCP side without
  /// ever re-running the plugin.
  pub manifests_json: String,
}

/// The epilogue appended to every bundled plugin module. It captures the
/// plugin's `globalThis.exports`, normalises the three accepted shapes
/// into one tool array, and publishes it at
/// `globalThis.__ferri_plugin_files[index]`. `exports` is cleared after
/// capture so a sibling file that forgets to set it fails loudly instead
/// of inheriting the previous file's value.
///
/// This is the single source of truth for shape normalisation: both the
/// startup extraction and every session install run this exact bytecode,
/// so a file's tool indices are identical on both paths by construction.
fn plugin_epilogue(index: usize) -> String {
  format!(
    "\n;(() => {{\n\
       const __exp = globalThis.exports;\n\
       globalThis.exports = undefined;\n\
       if (typeof __exp !== 'object' || __exp === null) {{\n\
         throw new Error('plugin file index {index} did not set globalThis.exports');\n\
       }}\n\
       let __t;\n\
       if (Array.isArray(__exp)) __t = __exp;\n\
       else if (Array.isArray(__exp.tools)) __t = __exp.tools;\n\
       else __t = [__exp];\n\
       (globalThis.__ferri_plugin_files ||= [])[{index}] = __t;\n\
     }})();\n"
  )
}

/// JS expression that reads a file's normalised tool array back out and
/// serialises it with every `handler` stripped (functions are not
/// JSON-serialisable and only make sense inside a live VM).
fn manifest_extract_expr(index: usize) -> String {
  format!(
    "JSON.stringify((globalThis.__ferri_plugin_files[{index}] || []).map((t) => {{ \
       const m = {{ ...t }}; delete m.handler; return m; }}))"
  )
}

/// Bundle + compile + extract every plugin file. One throwaway runtime
/// for the whole batch (the old path span one full engine per file for
/// manifest extraction *and* one per file for bytecode). Each file is
/// bundled independently so its TypeScript/import graph and top-level
/// `const`s stay file-scoped.
///
/// Per-file failures (bundle, compile, or extraction) are returned
/// rather than aborting the batch, so one broken plugin cannot stop the
/// server. Returned `CompiledPlugin`s have contiguous `index` values
/// matching their position in the success vec.
pub async fn compile_and_extract_plugins(files: &[PathBuf]) -> (Vec<CompiledPlugin>, Vec<(PathBuf, ScriptError)>) {
  let mut survivors: Vec<CompiledPlugin> = Vec::new();
  let mut failures: Vec<(PathBuf, ScriptError)> = Vec::new();

  // rolldown each file first (async; needs no JS context). Keep the
  // index a survivor *would* get so the epilogue baked into bytecode
  // matches the file's final registry slot even if a later step fails.
  let mut bundled: Vec<(PathBuf, String)> = Vec::new();
  for path in files {
    let cwd = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    match Box::pin(bundle_source(std::slice::from_ref(path), &cwd)).await {
      Ok((code, _map)) => bundled.push((path.clone(), code)),
      Err(e) => failures.push((path.clone(), e)),
    }
  }
  if bundled.is_empty() {
    return (survivors, failures);
  }

  let runtime = match AsyncRuntime::new() {
    Ok(r) => r,
    Err(e) => {
      let err = ScriptError::internal(format!("plugin bytecode runtime: {e}"));
      for (p, _) in bundled {
        failures.push((p, err.clone()));
      }
      return (survivors, failures);
    },
  };
  let actx = match AsyncContext::full(&runtime).await {
    Ok(c) => c,
    Err(e) => {
      let err = ScriptError::internal(format!("plugin bytecode context: {e}"));
      for (p, _) in bundled {
        failures.push((p, err.clone()));
      }
      return (survivors, failures);
    },
  };

  // Init the slot array once for the shared extraction context.
  let init: Result<(), ScriptError> = async_with!(actx => |ctx| {
    ctx
      .eval::<(), _>(b"globalThis.__ferri_plugin_files = [];".as_slice())
      .map_err(|e| ScriptError::internal(format!("init plugin slots: {e}")))
  })
  .await;
  if let Err(e) = init {
    for (p, _) in bundled {
      failures.push((p, e.clone()));
    }
    return (survivors, failures);
  }

  for (path, code) in bundled {
    let index = survivors.len();
    let module_name = format!("__ferri_plugin_{index}.js");
    let wrapped = format!("{code}{}", plugin_epilogue(index));
    match compile_extract_one(&actx, index, &module_name, &wrapped).await {
      Ok((bytecode, manifests_json)) => survivors.push(CompiledPlugin {
        path,
        index,
        bytecode: Arc::from(bytecode.into_boxed_slice()),
        manifests_json,
      }),
      Err(e) => failures.push((path, e)),
    }
  }

  (survivors, failures)
}

/// Declare the bundled+epilogued module, serialise it to bytecode, then
/// load that exact bytecode and evaluate it in the shared context so the
/// manifest is read from the very bytes a session will run.
async fn compile_extract_one(
  actx: &AsyncContext,
  index: usize,
  module_name: &str,
  wrapped: &str,
) -> Result<(Vec<u8>, String), ScriptError> {
  let name = module_name.to_string();
  let code = wrapped.to_string();
  let extract = manifest_extract_expr(index);
  async_with!(actx => |ctx| {
    // Bundled module has no remaining imports — `declare` (parse only)
    // needs no resolver; mirrors `bundle_and_compile`.
    let module = Module::declare(ctx.clone(), name.clone().into_bytes(), code.into_bytes())
      .catch(&ctx)
      .map_err(|e| caught_to_script_error(e, module_name))?;
    let bytecode = module
      .write(WriteOptions {
        // Same process + interpreter that will `load` it.
        endianness: WriteOptionsEndianness::Native,
        ..Default::default()
      })
      .map_err(|e| ScriptError::internal(format!("plugin module write: {e}")))?;

    // SAFETY: `bytecode` was just produced by `Module::write` in THIS
    // process and rquickjs/QuickJS build with native endianness and is
    // not persisted — the precondition `Module::load` documents. This is
    // the same contract `eval_bundle` and `install_plugins` rely on.
    #[allow(unsafe_code)]
    let loaded = (unsafe { Module::load(ctx.clone(), &bytecode) })
      .catch(&ctx)
      .map_err(|e| caught_to_script_error(e, module_name))?;
    let promise = loaded
      .eval()
      .catch(&ctx)
      .map_err(|e| caught_to_script_error(e, module_name))?
      .1;
    promise
      .into_future::<()>()
      .await
      .catch(&ctx)
      .map_err(|e| caught_to_script_error(e, module_name))?;

    let manifests_json: String = ctx
      .eval::<String, _>(extract.as_bytes())
      .map_err(|e| caught_to_script_error(rquickjs::CaughtError::from_error(&ctx, e), module_name))?;
    Ok((bytecode, manifests_json))
  })
  .await
}
