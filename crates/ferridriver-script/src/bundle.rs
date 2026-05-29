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
  let module_name = "ferridriver-bdd-steps.js".to_string();

  // Disk cache: an unchanged source tree skips rolldown AND the QuickJS
  // compile. Validated against every transitive input's content hash.
  let cache_key = crate::bytecode_cache::entry_key(entry_paths);
  if let Some(hit) = crate::bytecode_cache::load(cache_key) {
    let source_map = hit
      .source_map_json
      .and_then(|j| sourcemap::SourceMap::from_slice(j.as_bytes()).ok());
    return Ok(CompiledBundle {
      module_name,
      bytecode: Arc::from(hit.bytecode.into_boxed_slice()),
      source_map,
    });
  }

  let (code, map_json) = Box::pin(bundle_source(entry_paths, cwd)).await?;

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

  let inputs = crate::bytecode_cache::collect_inputs(entry_paths, map_json.as_deref(), cwd);
  crate::bytecode_cache::store(cache_key, &bytecode, &module_name, map_json.as_deref(), None, &inputs);

  let source_map = map_json.and_then(|j| sourcemap::SourceMap::from_slice(j.as_bytes()).ok());
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

  /// Every source file that went into this bundle (entry + transitive
  /// imports), resolved to absolute paths against `cwd`. Read from the
  /// source map's `sources`; synthetic (non-file) sources are skipped.
  ///
  /// Callers running untrusted bundles use this to enforce a sandbox
  /// jail (every input must live under an allowed root).
  #[must_use]
  pub fn source_files(&self, cwd: &Path) -> Vec<PathBuf> {
    let Some(sm) = self.source_map.as_ref() else {
      return Vec::new();
    };
    sm.sources()
      .map(|src| {
        let p = Path::new(src);
        if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }
      })
      .collect()
  }
}

/// True when a path's extension marks it as TypeScript (`.ts`/`.tsx`/
/// `.mts`/`.cts`) and so must be transpiled through the bundler.
#[must_use]
pub fn is_typescript_path(path: &Path) -> bool {
  matches!(
    path.extension().and_then(|e| e.to_str()),
    Some("ts" | "tsx" | "mts" | "cts")
  )
}

/// Heuristic: the source begins a line with a static `import`/`export`
/// and so must run as an ES module (bundled). Dynamic `import(...)` is
/// intentionally NOT matched — it is valid in a plain script, so such a
/// script keeps top-level `return`. A false positive only costs an
/// unnecessary bundle, never wrong output.
#[must_use]
pub fn source_is_es_module(source: &str) -> bool {
  source.lines().any(|line| {
    let t = line.trim_start();
    let static_import = t
      .strip_prefix("import")
      .is_some_and(|rest| matches!(rest.as_bytes().first(), Some(b' ' | b'\t' | b'{' | b'\'' | b'"')));
    static_import
      || t.starts_with("export ")
      || t.starts_with("export\t")
      || t.starts_with("export{")
      || t.starts_with("export*")
  })
}

/// One plugin file: rolldown-bundled (TypeScript, plugin-local imports,
/// tree-shaking) and compiled to `QuickJS` bytecode, with its manifests
/// extracted straight from the compiled module — no separate throwaway
/// runtime per file.
///
/// The bytecode is pure rolldown output — no appended epilogue, no
/// transfer global. Evaluating it runs the file's top-level
/// `defineTool(...)` calls, registering into the Rust
/// `ExtensionRegistry`. `manifests_json` is read straight off that
/// registry — no JS extraction expression. `index` is the file's
/// position in the returned (file-order, contiguous over successes) vec.
pub struct CompiledPlugin {
  pub path: PathBuf,
  pub index: usize,
  pub bytecode: Arc<[u8]>,
  /// JSON array (one object per tool, source order, `handler` stripped).
  /// Deserialises into `Vec<PluginManifest>` on the MCP side without
  /// ever re-running the plugin.
  pub manifests_json: String,
}

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
  // cache miss we must bundle, or an early failure. A miss carries both
  // the in-memory content key and the disk-cache key so the compile step
  // can populate both tiers.
  enum Slot {
    Hit(Arc<[u8]>, String),
    Miss { inmem_key: u64, disk_key: u64 },
    Failed(ScriptError),
  }

  let mut bytes: Vec<Vec<u8>> = Vec::with_capacity(files.len());
  let mut slots: Vec<Slot> = Vec::with_capacity(files.len());
  for path in files {
    match std::fs::read(path) {
      Ok(b) => {
        let inmem_key = cache_key(path, &b);
        let cached = plugin_cache().lock().ok().and_then(|c| c.get(&inmem_key).cloned());
        let disk_key = crate::bytecode_cache::entry_key(std::slice::from_ref(path));
        match cached {
          // 1. In-memory (same process).
          Some((bc, mj)) => slots.push(Slot::Hit(bc, mj)),
          // 2. Disk (cross-process), transitively validated. Promote into
          //    the in-memory tier so later same-process loads stay hot.
          None => match crate::bytecode_cache::load(disk_key) {
            Some(entry) => {
              let bc: Arc<[u8]> = Arc::from(entry.bytecode.into_boxed_slice());
              let mj = entry.aux.unwrap_or_else(|| "[]".to_string());
              if let Ok(mut cache) = plugin_cache().lock() {
                cache.insert(inmem_key, (bc.clone(), mj.clone()));
              }
              slots.push(Slot::Hit(bc, mj));
            },
            // 3. Cold: bundle + compile below.
            None => slots.push(Slot::Miss { inmem_key, disk_key }),
          },
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
    .filter_map(|(i, s)| matches!(s, Slot::Miss { .. }).then_some(i))
    .collect();
  let bundles = futures::future::join_all(miss_idx.iter().map(|&i| {
    let path = files[i].clone();
    async move {
      let cwd = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
      (i, Box::pin(bundle_source(std::slice::from_ref(&path), &cwd)).await)
    }
  }))
  .await;

  // Compiled code (+ source map, for the disk cache's transitive input
  // set) per missed position. None = bundle failed.
  let mut bundled_code: rustc_hash::FxHashMap<usize, String> = rustc_hash::FxHashMap::default();
  let mut bundled_map: rustc_hash::FxHashMap<usize, Option<String>> = rustc_hash::FxHashMap::default();
  for (i, res) in bundles {
    match res {
      Ok((code, map)) => {
        bundled_code.insert(i, code);
        bundled_map.insert(i, map);
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
          if matches!(s, Slot::Miss { .. }) {
            *s = Slot::Failed(err.clone());
          }
        }
        None
      },
    },
    Err(e) => {
      let err = ScriptError::internal(format!("plugin bytecode runtime: {e}"));
      for s in &mut slots {
        if matches!(s, Slot::Miss { .. }) {
          *s = Slot::Failed(err.clone());
        }
      }
      None
    },
  };

  if let Some((_runtime, actx)) = runtime_ctx {
    for i in &miss_idx {
      let i = *i;
      let Slot::Miss { inmem_key, disk_key } = slots[i] else {
        continue;
      };
      let Some(code) = bundled_code.get(&i) else { continue };
      let module_name = format!("ferri_plugin_{i}.js");
      match compile_extract_one(&actx, &module_name, code).await {
        Ok((bc, mj)) => {
          let bc: Arc<[u8]> = Arc::from(bc.into_boxed_slice());
          if let Ok(mut cache) = plugin_cache().lock() {
            cache.insert(inmem_key, (bc.clone(), mj.clone()));
          }
          // Persist for the next process. Inputs = this plugin file plus
          // its transitive imports (from the source map), so an edited
          // helper invalidates the entry on the next load.
          let cwd = files[i].parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
          let map = bundled_map.get(&i).cloned().flatten();
          let inputs = crate::bytecode_cache::collect_inputs(std::slice::from_ref(&files[i]), map.as_deref(), &cwd);
          crate::bytecode_cache::store(disk_key, &bc, &module_name, map.as_deref(), Some(&mj), &inputs);
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
      Slot::Miss { .. } => failures.push((
        files[i].clone(),
        ScriptError::internal("plugin compile produced no output".to_string()),
      )),
    }
  }
  (survivors, failures)
}

/// Declare the bundled module, serialise it to bytecode, then load +
/// evaluate that exact bytecode in the shared context (which has the
/// extension registry installed) so the manifest is read straight off
/// the Rust registry — the very bytes, and the very registration path,
/// a session will run. Returns the file's bytecode and the JSON of just
/// the tools THIS file registered (registry slice `[before, after)`).
async fn compile_extract_one(
  actx: &AsyncContext,
  module_name: &str,
  code: &str,
) -> Result<(Vec<u8>, String), ScriptError> {
  let name = module_name.to_string();
  let code = code.to_string();
  let label = module_name.to_string();
  async_with!(actx => |ctx| {
    // Registry + native `defineTool`/cucumber surface. Idempotent — the
    // shared extraction context installs it once for the whole batch.
    crate::bindings::install_bdd(&ctx)
      .map_err(|e| ScriptError::internal(format!("install extension registry: {e}")))?;
    // Manifest extraction is the MCP tool path: expose
    // `ferridriver.host = 'mcp'` so an extension's host-gated
    // `defineTool` runs and its manifest is captured (mirrors what the
    // mcp session does).
    {
      let fd = rquickjs::Object::new(ctx.clone())
        .map_err(|e| ScriptError::internal(format!("ferridriver global: {e}")))?;
      fd.set("host", "mcp")
        .map_err(|e| ScriptError::internal(format!("ferridriver.host: {e}")))?;
      ctx
        .globals()
        .set("ferridriver", fd)
        .map_err(|e| ScriptError::internal(format!("install ferridriver global: {e}")))?;
    }

    // Bundled module has no remaining imports — `declare` (parse only)
    // needs no resolver; mirrors `bundle_and_compile`.
    let module = Module::declare(ctx.clone(), name.clone().into_bytes(), code.into_bytes())
      .catch(&ctx)
      .map_err(|e| caught_to_script_error(e, &label))?;
    let bytecode = module
      .write(WriteOptions {
        // Same process + interpreter that will `load` it.
        endianness: WriteOptionsEndianness::Native,
        ..Default::default()
      })
      .map_err(|e| ScriptError::internal(format!("plugin module write: {e}")))?;

    let before = crate::bindings::tools_len(&ctx)?;

    // SAFETY: `bytecode` was just produced by `Module::write` in THIS
    // process and rquickjs/QuickJS build with native endianness and is
    // not persisted — the precondition `Module::load` documents. This is
    // the same contract `eval_bundle` and `install_plugins` rely on.
    #[allow(unsafe_code)]
    let loaded = (unsafe { Module::load(ctx.clone(), &bytecode) })
      .catch(&ctx)
      .map_err(|e| caught_to_script_error(e, &label))?;
    let promise = loaded
      .eval()
      .catch(&ctx)
      .map_err(|e| caught_to_script_error(e, &label))?
      .1;
    promise
      .into_future::<()>()
      .await
      .catch(&ctx)
      .map_err(|e| caught_to_script_error(e, &label))?;

    // Tools registered via the file's top-level `defineTool(...)` calls
    // during eval — slice off the ones THIS file added.
    let all = crate::bindings::tools_snapshot(&ctx)?;
    let slice = all.get(before..).unwrap_or(&[]);
    let manifests_json =
      serde_json::to_string(slice).map_err(|e| ScriptError::internal(format!("serialise manifests: {e}")))?;
    Ok((bytecode, manifests_json))
  })
  .await
}
