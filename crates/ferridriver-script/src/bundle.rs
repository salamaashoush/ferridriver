//! Step-file front-end: rolldown bundle + tree-shake + TypeScript ->
//! one ESM module -> compiled to `QuickJS` bytecode once.
//!
//! rolldown (built on oxc) resolves the whole import graph including
//! `node_modules`, transpiles `.ts`/`.tsx`, tree-shakes, and emits a
//! single ESM chunk. That chunk is compiled to bytecode a single time;
//! every per-worker session links the bytecode (one `Module::load`, no
//! parse, no resolver). A hidden source map is kept so a JS error in
//! the bundled output is reported at the original `.ts`/`.js` location.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rolldown::{Bundler, BundlerOptions, InputItem, OutputFormat, Platform, SourceMapType};
use rolldown_common::{ModuleType, Output};
use rolldown_plugin::{
  HookLoadArgs, HookLoadOutput, HookLoadReturn, HookResolveIdArgs, HookResolveIdOutput, HookResolveIdReturn, HookUsage,
  Plugin, PluginContext, SharedLoadPluginContext,
};
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Module, WriteOptions, WriteOptionsEndianness};

use crate::engine::caught_to_script_error;
use crate::error::ScriptError;

/// Id prefix for operator-declared virtual modules (`[bundler.virtualModules]`).
const VIRTUAL_USER_PREFIX: &str = "\0fd-virtual:";

/// Operator-facing bundler options (`[bundler]` in the unified config):
/// import-specifier aliases to shim files plus inline virtual modules.
/// Applied by `FerridriverRuntimePlugin` to EVERY bundle ferridriver
/// produces — BDD step files, extensions, `ferridriver run` scripts.
#[derive(Debug, Default, Clone)]
pub struct BundlerShims {
  /// `specifier -> absolute shim file path`. The shim is bundled and
  /// transpiled like any other source (so `.ts` works) and lands in the
  /// source map, which keeps the disk-cache freshness check covering it.
  pub alias: Vec<(String, PathBuf)>,
  /// `specifier -> inline ES-module source` (never touches the fs).
  pub virtual_modules: Vec<(String, String)>,
}

impl BundlerShims {
  /// Build from the unified config section, resolving relative alias
  /// targets against `base` (the config file's directory, or cwd).
  #[must_use]
  pub fn from_config(cfg: &ferridriver_config::BundlerConfig, base: &Path) -> Self {
    let alias = cfg
      .alias
      .iter()
      .map(|(spec, target)| {
        let p = Path::new(target);
        let abs = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
        (spec.clone(), abs)
      })
      .collect();
    let virtual_modules = cfg
      .virtual_modules
      .iter()
      .map(|(k, v)| (k.clone(), v.clone()))
      .collect();
    Self { alias, virtual_modules }
  }

  /// Stable content fingerprint, folded into every bundle cache key so
  /// editing an alias mapping or a virtual module's source invalidates
  /// cached bytecode. (Alias *target file* content is already covered by
  /// the transitive source-map input hashes; this covers the mapping
  /// itself and the inline sources, which never appear as files.)
  #[must_use]
  pub fn fingerprint(&self) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (spec, path) in &self.alias {
      spec.hash(&mut h);
      path.hash(&mut h);
    }
    for (spec, src) in &self.virtual_modules {
      spec.hash(&mut h);
      src.hash(&mut h);
    }
    h.finish()
  }
}

/// Process-global bundler shims, installed once by the host (CLI / MCP
/// server) from the loaded config before any bundling happens. A global
/// (rather than a parameter threaded through every bundle entry point)
/// because the config is process-wide and the bundle paths are reached
/// from five call sites across three crates — same pattern as
/// `set_bdd_script_caps`.
static BUNDLER_SHIMS: std::sync::RwLock<Option<Arc<BundlerShims>>> = std::sync::RwLock::new(None);

pub fn set_bundler_shims(shims: BundlerShims) {
  *BUNDLER_SHIMS.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(shims));
}

pub(crate) fn bundler_shims() -> Arc<BundlerShims> {
  BUNDLER_SHIMS
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone()
    .unwrap_or_default()
}

#[derive(Debug)]
struct FerridriverRuntimePlugin {
  shims: Arc<BundlerShims>,
}

impl Plugin for FerridriverRuntimePlugin {
  fn name(&self) -> Cow<'static, str> {
    "ferridriver-runtime".into()
  }

  #[allow(clippy::unused_async_trait_impl)] // rolldown Plugin trait requires async
  async fn resolve_id(&self, _ctx: &PluginContext, args: &HookResolveIdArgs<'_>) -> HookResolveIdReturn {
    // Native modules stay EXTERNAL: the emitted chunk keeps the bare
    // import and the bytecode re-links by name against the loading
    // runtime's ModuleDefs (`bindings::native_modules`). Checked first
    // so an operator alias can never hijack the framework surface.
    if crate::bindings::native_modules::NATIVE_MODULE_NAMES.contains(&args.specifier) {
      return Ok(Some(HookResolveIdOutput {
        id: args.specifier.into(),
        external: Some(rolldown_common::ResolvedExternal::Bool(true)),
        ..Default::default()
      }));
    }
    if self
      .shims
      .virtual_modules
      .iter()
      .any(|(spec, _)| spec == args.specifier)
    {
      return Ok(Some(HookResolveIdOutput::from_id(format!(
        "{VIRTUAL_USER_PREFIX}{}",
        args.specifier
      ))));
    }
    if let Some((_, target)) = self.shims.alias.iter().find(|(spec, _)| spec == args.specifier) {
      // Resolved to a concrete file: rolldown's default fs loader reads
      // it and transpiles by extension, so `.ts` shims work.
      return Ok(Some(HookResolveIdOutput::from_id(
        target.to_string_lossy().into_owned(),
      )));
    }
    Ok(None)
  }

  #[allow(clippy::unused_async_trait_impl)] // rolldown Plugin trait requires async
  async fn load(&self, _ctx: SharedLoadPluginContext, args: &HookLoadArgs<'_>) -> HookLoadReturn {
    let code: Option<Cow<'_, str>> = args.id.strip_prefix(VIRTUAL_USER_PREFIX).and_then(|spec| {
      self
        .shims
        .virtual_modules
        .iter()
        .find(|(s, _)| s == spec)
        .map(|(_, src)| Cow::Owned(src.clone()))
    });
    Ok(code.map(|code| HookLoadOutput {
      code: code.into_owned().into(),
      module_type: Some(ModuleType::Js),
      ..Default::default()
    }))
  }

  fn register_hook_usage(&self) -> HookUsage {
    HookUsage::ResolveId | HookUsage::Load
  }
}

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

  let mut bundler = Bundler::with_plugins(
    options,
    vec![Arc::new(FerridriverRuntimePlugin { shims: bundler_shims() })],
  )
  .map_err(|e| ScriptError::internal(format!("rolldown init: {e:?}")))?;
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
  let cache_key = crate::bytecode_cache::entry_key("bundle", entry_paths, bundler_shims().fingerprint());
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
  // QuickJS resolves the module graph EAGERLY at declare, and the
  // bundle keeps native specifiers external — so even this throwaway
  // compile runtime needs the native resolver/loader. The written
  // bytecode stores the dependency by NAME and re-links against the
  // loading runtime's own ModuleDefs (covered by
  // tests/node_compat_modules.rs end-to-end).
  runtime
    .set_loader(
      crate::bindings::native_modules::resolver(),
      crate::bindings::native_modules::loader(),
    )
    .await;
  let ctx = AsyncContext::full(&runtime)
    .await
    .map_err(|e| ScriptError::internal(format!("bytecode context: {e}")))?;
  let bytecode: Vec<u8> = ctx
    .async_with(async |ctx| {
      // The bundle's only remaining imports are the external native
      // specifiers, resolved by the loader installed above.
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
pub async fn eval_bundle(vm: &crate::vm::VmHandle, bundle: &CompiledBundle) -> Result<(), ScriptError> {
  let bytecode = Arc::clone(&bundle.bytecode);
  let label = bundle.module_name.clone();
  crate::vm_with!(vm => |ctx| {
    // SAFETY: produced by `Module::write` by this exact rquickjs/QuickJS
    // build with native endianness — either in this process or restored
    // from the bytecode disk cache, whose ABI tag (QuickJS version, arch,
    // endianness, pointer width) + transitive input hashes guarantee an
    // ABI-identical toolchain wrote it. That satisfies the precondition
    // `Module::load` documents.
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
  .await?
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

/// One extension file: rolldown-bundled (TypeScript, extension-local imports,
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
pub struct CompiledExtension {
  pub path: PathBuf,
  pub index: usize,
  pub bytecode: Arc<[u8]>,
  /// JSON array (one object per tool, source order, `handler` stripped).
  /// Deserialises into `Vec<ToolManifest>` on the MCP side without
  /// ever re-running the extension.
  pub manifests_json: String,
}

/// Process-scoped content-hash cache: `hash(canonical path + bytes)` ->
/// (bytecode, manifests JSON). A extension file whose content+path is
/// unchanged skips rolldown + compile entirely on any later
/// `compile_and_extract_extensions` call (reload, the same file discovered
/// under two roots, repeated `box-craft setup`). Bounded by the number
/// of distinct extension files a process ever loads (tiny) so no eviction
/// is needed.
///
/// This is the hot in-process tier; `compile_and_extract_extensions` also
/// consults the cross-process disk tier ([`crate::bytecode_cache`]),
/// whose ABI tag (QuickJS version, arch, endianness, pointer width) +
/// transitive input hashes are what keep the `unsafe Module::load`
/// paths sound for bytecode another process wrote.
type ExtensionCache = std::sync::Mutex<rustc_hash::FxHashMap<u64, (Arc<[u8]>, String)>>;
static EXTENSION_BYTECODE_CACHE: std::sync::OnceLock<ExtensionCache> = std::sync::OnceLock::new();

fn extension_cache() -> &'static ExtensionCache {
  EXTENSION_BYTECODE_CACHE.get_or_init(|| std::sync::Mutex::new(rustc_hash::FxHashMap::default()))
}

/// Cache key: the file's canonical path (rolldown resolution + relative
/// imports depend on it) plus its byte content, plus the bundler-shims
/// fingerprint (an alias/virtual-module edit changes the output for the
/// same input bytes). SipHash via the std default hasher — adequate for
/// an in-process content cache, no dep.
fn cache_key(path: &Path, bytes: &[u8], shims_fp: u64) -> u64 {
  use std::hash::{Hash, Hasher};
  let mut h = std::collections::hash_map::DefaultHasher::new();
  std::fs::canonicalize(path)
    .unwrap_or_else(|_| path.to_path_buf())
    .hash(&mut h);
  bytes.hash(&mut h);
  shims_fp.hash(&mut h);
  h.finish()
}

/// Bundle + compile + extract every extension file. The expensive
/// per-file rolldown bundles run concurrently; bytecode compile +
/// extraction share ONE throwaway runtime for the whole batch (the
/// pre-migration path spun one full engine per file for extraction
/// *and* one per file for bytecode). Unchanged files are served from
/// the process content-hash cache with no bundle and no compile.
///
/// Per-file failures (bundle, compile, or extraction) are returned
/// rather than aborting the batch. Output preserves input file order;
/// surviving `CompiledExtension`s carry contiguous `index` values.
pub async fn compile_and_extract_extensions(
  files: &[PathBuf],
) -> (Vec<CompiledExtension>, Vec<(PathBuf, ScriptError)>) {
  // Per original position: a cache hit (bytecode + manifests), or a
  // cache miss we must bundle, or an early failure. A miss carries both
  // the in-memory content key and the disk-cache key so the compile step
  // can populate both tiers.
  enum Slot {
    Hit(Arc<[u8]>, String),
    Miss { inmem_key: u64, disk_key: u64 },
    Failed(ScriptError),
  }

  let shims_fp = bundler_shims().fingerprint();
  let mut bytes: Vec<Vec<u8>> = Vec::with_capacity(files.len());
  let mut slots: Vec<Slot> = Vec::with_capacity(files.len());
  for path in files {
    match std::fs::read(path) {
      Ok(b) => {
        let inmem_key = cache_key(path, &b, shims_fp);
        let cached = extension_cache().lock().ok().and_then(|c| c.get(&inmem_key).cloned());
        let disk_key = crate::bytecode_cache::entry_key("extension", std::slice::from_ref(path), shims_fp);
        match cached {
          // 1. In-memory (same process).
          Some((bc, mj)) => slots.push(Slot::Hit(bc, mj)),
          // 2. Disk (cross-process), transitively validated. Promote into
          //    the in-memory tier so later same-process loads stay hot.
          None => match crate::bytecode_cache::load(disk_key) {
            Some(entry) => {
              let bc: Arc<[u8]> = Arc::from(entry.bytecode.into_boxed_slice());
              let mj = entry.aux.unwrap_or_else(|| "[]".to_string());
              if let Ok(mut cache) = extension_cache().lock() {
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
  // Native resolver/loader for the same reason as `bundle_and_compile`:
  // declare-time resolution of the external native specifiers.
  let runtime_ctx = match AsyncRuntime::new() {
    Ok(r) => {
      r.set_loader(
        crate::bindings::native_modules::resolver(),
        crate::bindings::native_modules::loader(),
      )
      .await;
      match AsyncContext::full(&r).await {
        Ok(c) => Some((r, c)),
        Err(e) => {
          let err = ScriptError::internal(format!("extension bytecode context: {e}"));
          for s in &mut slots {
            if matches!(s, Slot::Miss { .. }) {
              *s = Slot::Failed(err.clone());
            }
          }
          None
        },
      }
    },
    Err(e) => {
      let err = ScriptError::internal(format!("extension bytecode runtime: {e}"));
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
      let module_name = format!("ferri_extension_{i}.js");
      match compile_extract_one(&actx, &module_name, code).await {
        Ok((bc, mj)) => {
          let bc: Arc<[u8]> = Arc::from(bc.into_boxed_slice());
          if let Ok(mut cache) = extension_cache().lock() {
            cache.insert(inmem_key, (bc.clone(), mj.clone()));
          }
          // Persist for the next process. Inputs = this extension file plus
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

  let mut survivors: Vec<CompiledExtension> = Vec::new();
  let mut failures: Vec<(PathBuf, ScriptError)> = Vec::new();
  for (i, slot) in slots.into_iter().enumerate() {
    match slot {
      Slot::Hit(bytecode, manifests_json) => survivors.push(CompiledExtension {
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
        ScriptError::internal("extension compile produced no output".to_string()),
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
  let cfg_default = crate::engine::ScriptEngineConfig::default();
  actx
    .async_with(async |ctx| {
      // Extraction must evaluate the module in the SAME ambient
      // environment a session VM provides, or a extension whose top level
      // uses a standard global (TextEncoder, setTimeout, crypto,
      // console, expect) is rejected at startup while working fine
      // in-session. Everything context-free that `Session::create`
      // installs before `install_extensions` is installed here too; only
      // session-scoped bindings (fs/vars/artifacts/commands/page/
      // request) are absent — those are per-session by definition and
      // top-level extension code must not depend on them. All idempotent —
      // the shared extraction context installs once for the whole batch.
      crate::bindings::install_bdd(&ctx)
        .map_err(|e| ScriptError::internal(format!("install extension registry: {e}")))?;
      crate::bindings::define_classes(&ctx).map_err(|e| ScriptError::internal(format!("install classes: {e}")))?;
      crate::engine::install_runtime_shims(&ctx)
        .map_err(|e| ScriptError::internal(format!("install runtime shims: {e}")))?;
      crate::bindings::expect::install_expect(&ctx)
        .map_err(|e| ScriptError::internal(format!("install expect: {e}")))?;
      // Fresh capture per file: whatever the extension's top level logs is
      // forwarded to tracing under the module label after eval.
      let console = std::sync::Arc::new(crate::console::ConsoleCapture::new(
        cfg_default.max_console_entries,
        cfg_default.max_console_bytes,
        cfg_default.max_console_entry_bytes,
      ));
      crate::engine::install_console(&ctx, console.clone())
        .map_err(|e| ScriptError::internal(format!("install console: {e}")))?;
      // Manifest extraction is the MCP tool path: expose
      // `ferridriver.host = 'mcp'` so an extension's host-gated
      // `defineTool` runs and its manifest is captured (mirrors what the
      // mcp session does).
      crate::bindings::runtime::install_host(&ctx, "mcp")
        .map_err(|e| ScriptError::internal(format!("install ferridriver.host: {e}")))?;

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
        .map_err(|e| ScriptError::internal(format!("extension module write: {e}")))?;

      let before = crate::bindings::tools_len(&ctx)?;

      // SAFETY: `bytecode` was just produced by `Module::write` in THIS
      // process and rquickjs/QuickJS build with native endianness — the
      // precondition `Module::load` documents. (When it is later stored in
      // the disk cache, the cache's ABI tag + input hashes preserve that
      // precondition for the process that loads it back.)
      #[allow(unsafe_code)]
      let loaded = (unsafe { Module::load(ctx.clone(), &bytecode) })
        .catch(&ctx)
        .map_err(|e| caught_to_script_error(e, &label))?;
      let promise = loaded
        .eval()
        .catch(&ctx)
        .map_err(|e| caught_to_script_error(e, &label))?
        .1;
      let evaled = promise.into_future::<()>().await.catch(&ctx);
      for entry in console.drain() {
        tracing::info!(target: "ferridriver::extensions", extension = %label, "{}", entry.message);
      }
      evaled.map_err(|e| caught_to_script_error(e, &label))?;

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
