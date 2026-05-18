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
/// The bytecode is **registry-position-independent**: its epilogue
/// publishes the file's tool array to a single transfer global
/// (`globalThis.__ferri_plugin_pending`); the consumer assigns it to the
/// correct `__ferri_plugin_files[i]` slot from Rust. That decoupling is
/// what lets the content-hash cache reuse a file's bytecode regardless
/// of where it lands in the registry. `index` is the file's position in
/// the returned (file-order, contiguous over successes) vec — registry
/// ordering only, never baked into the bytes.
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
/// into one tool array, and parks it on the single transfer global
/// `globalThis.__ferri_plugin_pending`. `exports` is cleared after
/// capture so a sibling file that forgets to set it fails loudly.
///
/// Single source of truth for shape normalisation: startup extraction
/// and every session install run this exact bytecode, so a file's tool
/// list is identical on both paths by construction. It bakes in NO
/// registry index — the consumer moves `__ferri_plugin_pending` into the
/// right slot — so the bytes are position-independent and cacheable.
const PLUGIN_EPILOGUE: &str = "\n;(() => {\n\
   const __exp = globalThis.exports;\n\
   globalThis.exports = undefined;\n\
   if (typeof __exp !== 'object' || __exp === null) {\n\
     throw new Error('plugin did not set globalThis.exports');\n\
   }\n\
   let __t;\n\
   if (Array.isArray(__exp)) __t = __exp;\n\
   else if (Array.isArray(__exp.tools)) __t = __exp.tools;\n\
   else __t = [__exp];\n\
   globalThis.__ferri_plugin_pending = __t;\n\
 })();\n";

/// JS that serialises the just-evaluated module's parked tool array with
/// every `handler` stripped (functions are not JSON-serialisable and
/// only make sense inside a live VM).
const MANIFEST_EXTRACT_EXPR: &str = "JSON.stringify((globalThis.__ferri_plugin_pending || []).map((t) => { \
   const m = { ...t }; delete m.handler; return m; }))";

/// Process-scoped content-hash cache: `hash(canonical path + bytes)` ->
/// (bytecode, manifests JSON). A plugin file whose content+path is
/// unchanged skips rolldown + compile entirely on any later
/// `compile_and_extract_plugins` call (reload, the same file discovered
/// under two roots, repeated `box-craft setup`). Bounded by the number
/// of distinct plugin files a process ever loads (tiny) so no eviction
/// is needed.
///
/// In-memory only and never serialised: the cached bytecode never
/// crosses a process or interpreter boundary, which is exactly the
/// precondition the `unsafe Module::load` paths rely on (a disk cache
/// would violate it — see `docs/plugin-architecture.md`).
type PluginCache = std::sync::Mutex<rustc_hash::FxHashMap<u64, (Arc<[u8]>, String)>>;
static PLUGIN_BYTECODE_CACHE: std::sync::OnceLock<PluginCache> = std::sync::OnceLock::new();

fn plugin_cache() -> &'static PluginCache {
  PLUGIN_BYTECODE_CACHE.get_or_init(|| std::sync::Mutex::new(rustc_hash::FxHashMap::default()))
}

/// Cache key: the file's canonical path (rolldown resolution + relative
/// imports depend on it) plus its byte content. SipHash via the std
/// default hasher — adequate for an in-process content cache, no dep.
fn cache_key(path: &Path, bytes: &[u8]) -> u64 {
  use std::hash::{Hash, Hasher};
  let mut h = std::collections::hash_map::DefaultHasher::new();
  std::fs::canonicalize(path)
    .unwrap_or_else(|_| path.to_path_buf())
    .hash(&mut h);
  bytes.hash(&mut h);
  h.finish()
}

/// Bundle + compile + extract every plugin file. The expensive
/// per-file rolldown bundles run concurrently; bytecode compile +
/// extraction share ONE throwaway runtime for the whole batch (the
/// pre-migration path spun one full engine per file for extraction
/// *and* one per file for bytecode). Unchanged files are served from
/// the process content-hash cache with no bundle and no compile.
///
/// Per-file failures (bundle, compile, or extraction) are returned
/// rather than aborting the batch. Output preserves input file order;
/// surviving `CompiledPlugin`s carry contiguous `index` values.
pub async fn compile_and_extract_plugins(files: &[PathBuf]) -> (Vec<CompiledPlugin>, Vec<(PathBuf, ScriptError)>) {
  // Per original position: a cache hit (bytecode + manifests), or a
  // cache miss we must bundle, or an early failure.
  enum Slot {
    Hit(Arc<[u8]>, String),
    Miss(u64),
    Failed(ScriptError),
  }

  let mut bytes: Vec<Vec<u8>> = Vec::with_capacity(files.len());
  let mut slots: Vec<Slot> = Vec::with_capacity(files.len());
  for path in files {
    match std::fs::read(path) {
      Ok(b) => {
        let key = cache_key(path, &b);
        let cached = plugin_cache().lock().ok().and_then(|c| c.get(&key).cloned());
        match cached {
          Some((bc, mj)) => slots.push(Slot::Hit(bc, mj)),
          None => slots.push(Slot::Miss(key)),
        }
        bytes.push(b);
      },
      Err(e) => {
        slots.push(Slot::Failed(ScriptError::internal(format!(
          "read {}: {e}",
          path.display()
        ))));
        bytes.push(Vec::new());
      },
    }
  }

  // Bundle every cache-miss file concurrently (independent rolldown
  // graphs; this is the dominant cold-start cost).
  let miss_idx: Vec<usize> = slots
    .iter()
    .enumerate()
    .filter_map(|(i, s)| matches!(s, Slot::Miss(_)).then_some(i))
    .collect();
  let bundles = futures::future::join_all(miss_idx.iter().map(|&i| {
    let path = files[i].clone();
    async move {
      let cwd = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
      (i, Box::pin(bundle_source(std::slice::from_ref(&path), &cwd)).await)
    }
  }))
  .await;

  // Compiled code per missed position (None = bundle failed).
  let mut bundled_code: rustc_hash::FxHashMap<usize, String> = rustc_hash::FxHashMap::default();
  for (i, res) in bundles {
    match res {
      Ok((code, _map)) => {
        bundled_code.insert(i, code);
      },
      Err(e) => slots[i] = Slot::Failed(e),
    }
  }

  // One throwaway runtime/context compiles + extracts every missed file.
  let runtime_ctx = match AsyncRuntime::new() {
    Ok(r) => match AsyncContext::full(&r).await {
      Ok(c) => Some((r, c)),
      Err(e) => {
        let err = ScriptError::internal(format!("plugin bytecode context: {e}"));
        for s in &mut slots {
          if matches!(s, Slot::Miss(_)) {
            *s = Slot::Failed(err.clone());
          }
        }
        None
      },
    },
    Err(e) => {
      let err = ScriptError::internal(format!("plugin bytecode runtime: {e}"));
      for s in &mut slots {
        if matches!(s, Slot::Miss(_)) {
          *s = Slot::Failed(err.clone());
        }
      }
      None
    },
  };

  if let Some((_runtime, actx)) = runtime_ctx {
    for i in &miss_idx {
      let i = *i;
      let Slot::Miss(key) = slots[i] else { continue };
      let Some(code) = bundled_code.get(&i) else { continue };
      let wrapped = format!("{code}{PLUGIN_EPILOGUE}");
      let module_name = format!("__ferri_plugin_{i}.js");
      match compile_extract_one(&actx, &module_name, &wrapped).await {
        Ok((bc, mj)) => {
          let bc: Arc<[u8]> = Arc::from(bc.into_boxed_slice());
          if let Ok(mut cache) = plugin_cache().lock() {
            cache.insert(key, (bc.clone(), mj.clone()));
          }
          slots[i] = Slot::Hit(bc, mj);
        },
        Err(e) => slots[i] = Slot::Failed(e),
      }
    }
  }

  let mut survivors: Vec<CompiledPlugin> = Vec::new();
  let mut failures: Vec<(PathBuf, ScriptError)> = Vec::new();
  for (i, slot) in slots.into_iter().enumerate() {
    match slot {
      Slot::Hit(bytecode, manifests_json) => survivors.push(CompiledPlugin {
        path: files[i].clone(),
        index: survivors.len(),
        bytecode,
        manifests_json,
      }),
      Slot::Failed(e) => failures.push((files[i].clone(), e)),
      // A Miss with no compiled output never reached Hit/Failed only if
      // its bundle was dropped — already recorded as Failed above; this
      // arm is unreachable but keeps the match total without a panic.
      Slot::Miss(_) => failures.push((
        files[i].clone(),
        ScriptError::internal("plugin compile produced no output".to_string()),
      )),
    }
  }
  (survivors, failures)
}

/// Declare the bundled+epilogued module, serialise it to bytecode, then
/// load that exact bytecode and evaluate it in the shared context so the
/// manifest is read from the very bytes a session will run.
async fn compile_extract_one(
  actx: &AsyncContext,
  module_name: &str,
  wrapped: &str,
) -> Result<(Vec<u8>, String), ScriptError> {
  let name = module_name.to_string();
  let code = wrapped.to_string();
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
      .eval::<String, _>(MANIFEST_EXTRACT_EXPR.as_bytes())
      .map_err(|e| caught_to_script_error(rquickjs::CaughtError::from_error(&ctx, e), module_name))?;
    Ok((bytecode, manifests_json))
  })
  .await
}
