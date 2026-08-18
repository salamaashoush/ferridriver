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
use rolldown_common::{CodeSplittingMode, ModuleType, Output, ResolveOptions, TsConfig};
use rolldown_plugin::{
  HookLoadArgs, HookLoadOutput, HookLoadReturn, HookResolveIdArgs, HookResolveIdOutput, HookResolveIdReturn, HookUsage,
  Plugin, PluginContext, SharedLoadPluginContext,
};
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Module, WriteOptions, WriteOptionsEndianness};

use crate::engine::{caught_to_script_error, caught_to_script_error_in};
use crate::error::ScriptError;

/// Id prefix for operator-declared virtual modules (`[bundler.virtualModules]`).
const VIRTUAL_USER_PREFIX: &str = "\0fd-virtual:";

/// Operator-facing bundler options: the `[bundler]` section of the
/// unified config (shim aliases, inline virtual modules, and the module
/// resolution controls) plus the `[test].tsconfig` selection. Applied to
/// EVERY bundle ferridriver produces — BDD step files, extensions,
/// `ferridriver run` scripts.
#[derive(Debug, Default, Clone)]
pub struct BundlerEnv {
  /// `specifier -> absolute shim file path`. The shim is bundled and
  /// transpiled like any other source (so `.ts` works) and lands in the
  /// source map, which keeps the disk-cache freshness check covering it.
  pub alias: Vec<(String, PathBuf)>,
  /// `specifier -> inline ES-module source` (never touches the fs).
  pub virtual_modules: Vec<(String, String)>,
  /// Extra `exports`/`imports` condition names. The resolver appends
  /// these to its own base set, so an empty list resolves exactly as it
  /// did before any were configured.
  pub conditions: Vec<String>,
  /// `package.json` fields consulted when no `exports` entry matches.
  /// rolldown's own default for a neutral platform is EMPTY, which
  /// leaves a plain `"main": "index.js"` package unresolvable; the
  /// config's default (`["module", "main"]`) is what ships.
  pub main_fields: Vec<String>,
  /// `package.json` field paths holding a legacy path-remapping object.
  pub alias_fields: Vec<Vec<String>>,
  /// The tsconfig whose `paths` / `baseUrl` govern resolution. `None`
  /// leaves rolldown's per-module upward discovery in place; a value
  /// pins one file for the whole graph, which is the only way to select
  /// a config discovery would not find (`tsconfig.test.json`).
  pub tsconfig: Option<PathBuf>,
}

impl BundlerEnv {
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
    Self {
      alias,
      virtual_modules,
      conditions: cfg.conditions.clone(),
      main_fields: cfg.main_fields.clone(),
      alias_fields: cfg.alias_fields.clone(),
      tsconfig: None,
    }
  }

  /// Pin the tsconfig governing resolution, resolved against `base` when
  /// relative.
  #[must_use]
  pub fn with_tsconfig(mut self, tsconfig: Option<&str>, base: &Path) -> Self {
    self.tsconfig = tsconfig.map(|t| {
      let p = Path::new(t);
      if p.is_absolute() { p.to_path_buf() } else { base.join(p) }
    });
    self
  }

  /// Stable content fingerprint, folded into every bundle cache key so
  /// editing an alias mapping, a virtual module's source or a resolution
  /// control invalidates cached bytecode. (Alias *target file* content is
  /// already covered by the transitive source-map input hashes; this
  /// covers the mapping itself, the inline sources, and every knob that
  /// changes output without changing a source byte. The tsconfig's
  /// CONTENT is covered separately, through the bundle's input set.)
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
    self.conditions.hash(&mut h);
    self.main_fields.hash(&mut h);
    self.alias_fields.hash(&mut h);
    self.tsconfig.hash(&mut h);
    h.finish()
  }
}

/// Process-global bundler environment, installed once by the host (CLI /
/// MCP server) from the loaded config before any bundling happens. A
/// global (rather than a parameter threaded through every bundle entry
/// point) because the config is process-wide and the bundle paths are
/// reached from five call sites across three crates — same pattern as
/// `set_bdd_script_caps`.
static BUNDLER_ENV: std::sync::RwLock<Option<Arc<BundlerEnv>>> = std::sync::RwLock::new(None);

pub fn set_bundler_env(env: BundlerEnv) {
  *BUNDLER_ENV.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(env));
}

pub(crate) fn bundler_env() -> Arc<BundlerEnv> {
  BUNDLER_ENV
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone()
    .unwrap_or_default()
}

/// Everything outside the entry files that can change a bundle's output
/// for byte-identical sources: the `[bundler]` shims and resolution
/// controls, the pinned tsconfig, and the native module aliases. Every
/// cache key folds this in.
fn bundle_env_fingerprint() -> u64 {
  use std::hash::{Hash, Hasher};
  let mut h = std::collections::hash_map::DefaultHasher::new();
  bundler_env().fingerprint().hash(&mut h);
  crate::bindings::native_modules::alias_fingerprint().hash(&mut h);
  crate::provided_modules::provided_fingerprint().hash(&mut h);
  h.finish()
}

/// Virtual id of the synthetic entry that fans out to every requested
/// entry file. rolldown emits ONE entry chunk per input; feeding it N
/// step/extension files as N inputs produces N entry chunks, of which
/// [`bundle_source`] can only return one — every other file's
/// registrations would be silently dropped. The synthetic entry
/// side-effect-imports each file instead, so one chunk carries them all.
const MULTI_ENTRY_ID: &str = "\0ferridriver-multi-entry.js";

#[derive(Debug)]
struct FerridriverRuntimePlugin {
  env: Arc<BundlerEnv>,
  /// Source of the synthetic multi-entry module, when the bundle has
  /// more than one entry file.
  multi_entry: Option<String>,
}

impl Plugin for FerridriverRuntimePlugin {
  fn name(&self) -> Cow<'static, str> {
    "ferridriver-runtime".into()
  }

  async fn resolve_id(&self, _ctx: &PluginContext, args: &HookResolveIdArgs<'_>) -> HookResolveIdReturn {
    if args.specifier == MULTI_ENTRY_ID && self.multi_entry.is_some() {
      return Ok(Some(HookResolveIdOutput::from_id(MULTI_ENTRY_ID)));
    }
    // Native modules stay EXTERNAL: the emitted chunk keeps the bare
    // import and the bytecode re-links by name against the loading
    // runtime's ModuleDefs (`bindings::native_modules`). Checked first
    // so an operator alias can never hijack the framework surface.
    // A specifier a package serves stays external too: the emitted
    // chunk keeps the bare import and links, at load, against the one
    // module the provider's bytecode already is. Inlining it would give
    // every consumer its own copy of the provider's state.
    if crate::bindings::native_modules::is_native_specifier(args.specifier)
      || crate::provided_modules::is_provided_specifier(args.specifier)
    {
      return Ok(Some(HookResolveIdOutput {
        id: args.specifier.into(),
        external: Some(rolldown_common::ResolvedExternal::Bool(true)),
        ..Default::default()
      }));
    }
    if self.env.virtual_modules.iter().any(|(spec, _)| spec == args.specifier) {
      return Ok(Some(HookResolveIdOutput::from_id(format!(
        "{VIRTUAL_USER_PREFIX}{}",
        args.specifier
      ))));
    }
    if let Some((_, target)) = self.env.alias.iter().find(|(spec, _)| spec == args.specifier) {
      // Resolved to a concrete file: rolldown's default fs loader reads
      // it and transpiles by extension, so `.ts` shims work.
      return Ok(Some(HookResolveIdOutput::from_id(
        target.to_string_lossy().into_owned(),
      )));
    }
    Ok(None)
  }

  async fn load(&self, _ctx: SharedLoadPluginContext, args: &HookLoadArgs<'_>) -> HookLoadReturn {
    if args.id == MULTI_ENTRY_ID
      && let Some(src) = &self.multi_entry
    {
      return Ok(Some(HookLoadOutput {
        code: src.clone().into(),
        module_type: Some(ModuleType::Js),
        ..Default::default()
      }));
    }
    let code: Option<Cow<'_, str>> = args.id.strip_prefix(VIRTUAL_USER_PREFIX).and_then(|spec| {
      self
        .env
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
  source_map: Option<Arc<sourcemap::SourceMap>>,
}

/// A bundle's position mapping on its own.
///
/// A VM has to keep translating positions for as long as the module it
/// loaded can run — long after the [`CompiledBundle`] that produced the
/// bytecode has been dropped by whoever compiled it.
#[derive(Clone)]
pub struct SourceMapper {
  /// Module name QuickJS knows the bundle by, which is what its stack
  /// frames are labelled with.
  pub module_name: String,
  map: Option<Arc<sourcemap::SourceMap>>,
}

impl std::fmt::Debug for SourceMapper {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SourceMapper")
      .field("module_name", &self.module_name)
      .field("mapped", &self.map.is_some())
      .finish()
  }
}

/// Resolve a source-map `sources` entry to a real file.
///
/// The entries are relative to the bundle chunk's virtual location (a
/// level below the bundling cwd), so a literal join produces paths like
/// `<cwd>/../tests/a.test.ts` — peel leading `../` segments until the
/// candidate exists under `cwd`.
#[must_use]
pub fn resolve_source(cwd: &Path, src: &str) -> PathBuf {
  let p = Path::new(src);
  if p.is_absolute() {
    return p.to_path_buf();
  }
  // Normalized, because the joined form keeps every `../` verbatim: a
  // spec outside the working directory would then be reported as
  // `<cwd>/../../../tmp/specs/a.ts`, which names the right file but is
  // not under `cwd` and is not under `testDir` either.
  let mut rest = src;
  loop {
    let candidate = ferridriver_config::layer::normalize_path(&cwd.join(rest));
    if candidate.exists() {
      return candidate;
    }
    match rest.strip_prefix("../") {
      Some(stripped) => rest = stripped,
      None => return ferridriver_config::layer::normalize_path(&cwd.join(src)),
    }
  }
}

impl SourceMapper {
  /// Map a bundled-output `line:col` (1-based, as QuickJS reports) back
  /// to the original `.ts`/`.js` source location.
  #[must_use]
  pub fn remap(&self, line: u32, col: u32) -> Option<(String, u32, u32)> {
    let sm = self.map.as_ref()?;
    let token = sm.lookup_token(line.saturating_sub(1), col.saturating_sub(1))?;
    let src = token.get_source().unwrap_or("<unknown>").to_string();
    Some((src, token.get_src_line() + 1, token.get_src_col() + 1))
  }
}

/// A compiled bundle as the runner's [`ferridriver_test::host::SourceMap`]:
/// a position in the code QuickJS executed, answered as the file the
/// author wrote, resolved against the directory the bundle was built
/// from.
pub struct BundleSourceMap {
  bundle: std::sync::Arc<CompiledBundle>,
  cwd: std::sync::Arc<std::path::PathBuf>,
}

impl BundleSourceMap {
  #[must_use]
  pub fn new(bundle: std::sync::Arc<CompiledBundle>, cwd: std::sync::Arc<std::path::PathBuf>) -> Self {
    Self { bundle, cwd }
  }
}

impl ferridriver_test::host::SourceMap for BundleSourceMap {
  fn remap(&self, line: u32, column: u32) -> Option<(String, u32, u32)> {
    let (src, src_line, src_col) = self.bundle.remap(line, column)?;
    Some((resolve_source(&self.cwd, &src).display().to_string(), src_line, src_col))
  }
}

/// The result of one rolldown bundle.
pub struct BundledSource {
  pub code: String,
  /// Hidden source map JSON, for translating bundled positions back to
  /// source in stack traces.
  pub source_map_json: Option<String>,
  /// Every module the entry chunk was built from, straight out of
  /// rolldown's module graph.
  ///
  /// NOT derived from the source map: a module whose every binding is
  /// inlined leaves no mapping tokens and vanishes from the map's
  /// `sources`, so a source-map-derived input set silently omitted
  /// exactly the small helper modules extensions are made of — and the
  /// bytecode caches then treated an edited helper as unchanged.
  pub modules: Vec<PathBuf>,
  /// Non-module files the resolver read that can change the output —
  /// the tsconfigs rolldown discovered or was pointed at. They are not
  /// in `modules` (nothing imports them) but editing a `paths` mapping
  /// changes what the same sources resolve to, so they belong in the
  /// cache's input set.
  pub config_inputs: Vec<PathBuf>,
}

/// rolldown-bundle + tree-shake + transpile the step entry files (and
/// their `node_modules`/shared imports) into a single ESM module.
/// Exposed for diagnostics/tests; production uses [`bundle_and_compile`].
pub async fn bundle_source(entry_paths: &[PathBuf], cwd: &Path) -> Result<BundledSource, ScriptError> {
  if entry_paths.is_empty() {
    return Err(ScriptError::internal("no step entry files".to_string()));
  }

  let env = bundler_env();
  if let Some(ts) = &env.tsconfig
    && !ts.is_file()
  {
    return Err(ScriptError::internal(format!(
      "[test].tsconfig points at {}, which is not a file",
      ts.display()
    )));
  }

  // ONE rolldown input, always. Each input produces its own entry
  // chunk and only one chunk's code can be returned, so multiple entry
  // files must be fanned out from a single synthetic entry module that
  // side-effect-imports each of them (top-level `Given`/`defineTool`
  // registrations are side effects, so nothing tree-shakes away).
  let multi_entry = (entry_paths.len() > 1).then(|| {
    use std::fmt::Write as _;
    entry_paths.iter().fold(String::new(), |mut acc, p| {
      let _ = writeln!(
        acc,
        "import {};",
        serde_json::to_string(&p.to_string_lossy()).unwrap_or_else(|_| String::from("\"\""))
      );
      acc
    })
  });
  let input: Vec<InputItem> = vec![InputItem {
    name: None,
    import: if multi_entry.is_some() {
      MULTI_ENTRY_ID.to_string()
    } else {
      entry_paths[0].to_string_lossy().into_owned()
    },
  }];

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
    // One chunk, always. Only the entry chunk is returned and compiled,
    // so a split chunk would be a reference to code nobody wrote — and
    // its modules would be missing from the cache's input set, making an
    // edit to them invalidate nothing. Legal because there is exactly
    // one input (MULTI_ENTRY fans the rest out).
    code_splitting: Some(CodeSplittingMode::Bool(false)),
    resolve: Some(ResolveOptions {
      // `None` and an empty list are NOT the same to rolldown for main
      // fields: `None` means "platform default", which is empty for
      // Platform::Neutral. Always pass ours.
      main_fields: Some(env.main_fields.clone()),
      condition_names: (!env.conditions.is_empty()).then(|| env.conditions.clone()),
      alias_fields: (!env.alias_fields.is_empty()).then(|| env.alias_fields.clone()),
      ..Default::default()
    }),
    // Unset leaves rolldown's per-module upward discovery (its default).
    tsconfig: env.tsconfig.clone().map(TsConfig::Manual),
    ..Default::default()
  };

  let mut bundler = Bundler::with_plugins(
    options,
    vec![Arc::new(FerridriverRuntimePlugin {
      env: Arc::clone(&env),
      multi_entry,
    })],
  )
  .map_err(|e| ScriptError::internal(format!("rolldown init: {e:?}")))?;
  // rolldown's generate future is large; box it so it doesn't bloat the
  // enclosing future.
  let out = Box::pin(bundler.generate())
    .await
    .map_err(|e| ScriptError::internal(format!("rolldown bundle: {e:?}")))?;

  // Every tsconfig the resolver consulted, whether pinned or discovered
  // per module. rolldown reports them alongside the modules it read.
  let config_inputs: Vec<PathBuf> = bundler
    .watch_files()
    .iter()
    .map(|f| PathBuf::from(f.as_str()))
    .filter(|p| {
      let named_tsconfig = p
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("tsconfig"));
      named_tsconfig && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"))
    })
    .collect();

  for asset in &out.assets {
    if let Output::Chunk(chunk) = asset
      && chunk.is_entry
    {
      let modules = chunk
        .module_ids
        .iter()
        .map(|id| PathBuf::from(id.to_string()))
        .filter(|p| p.is_file())
        .collect();
      // Assigned imperatively rather than through `Option::map`: the
      // map's type lives in a transitive crate this one does not depend on
      // directly, so it cannot be named for a method-path closure.
      let mut source_map_json = None;
      if let Some(m) = chunk.map.as_ref() {
        source_map_json = Some(m.to_json_string());
      }
      return Ok(BundledSource {
        code: chunk.code.clone(),
        source_map_json,
        modules,
        config_inputs,
      });
    }
  }
  Err(ScriptError::internal("rolldown produced no entry chunk".to_string()))
}

/// Bundle the step entry files (TypeScript ok; `node_modules` and
/// shared utils resolved + tree-shaken) into one ESM module and compile
/// it to bytecode. Done once, before workers spawn.
pub async fn bundle_and_compile(entry_paths: &[PathBuf], cwd: &Path) -> Result<CompiledBundle, ScriptError> {
  bundle_and_compile_named(entry_paths, cwd, "ferridriver-bdd-steps.js").await
}

/// [`bundle_and_compile`] with a caller-chosen bundle module name, so
/// error locations and stack frames carry a label matching the host
/// (e.g. `ferridriver-tests.js` for the test runner).
pub async fn bundle_and_compile_named(
  entry_paths: &[PathBuf],
  cwd: &Path,
  module_name: &str,
) -> Result<CompiledBundle, ScriptError> {
  let module_name = module_name.to_string();

  // Disk cache: an unchanged source tree skips rolldown AND the QuickJS
  // compile. Validated against every transitive input's content hash.
  // The module name participates in the key: it is baked into the
  // written bytecode (QuickJS stores the module name), so two hosts
  // bundling the same files under different labels must not share an
  // entry.
  let cache_key = crate::bytecode_cache::entry_key(
    &format!("bundle:{module_name}"),
    entry_paths,
    cwd,
    bundle_env_fingerprint(),
  );
  if let Some(hit) = crate::bytecode_cache::load(cache_key) {
    let source_map = hit
      .source_map_json
      .and_then(|j| sourcemap::SourceMap::from_slice(j.as_bytes()).ok())
      .map(Arc::new);
    return Ok(CompiledBundle {
      module_name,
      bytecode: Arc::from(hit.bytecode.into_boxed_slice()),
      source_map,
    });
  }

  let bundled = Box::pin(bundle_source(entry_paths, cwd)).await?;
  let (code, map_json, mut modules) = (bundled.code, bundled.source_map_json, bundled.modules);
  modules.extend(bundled.config_inputs);

  let compiled = compile_bundled_source(&code, &module_name, map_json.as_deref()).await?;

  let inputs = crate::bytecode_cache::input_set(entry_paths, &modules);
  crate::bytecode_cache::store(
    cache_key,
    &compiled.bytecode,
    &module_name,
    map_json.as_deref(),
    None,
    &inputs,
  );

  Ok(compiled)
}

/// Compile already-bundled ESM `code` to `QuickJS` bytecode.
///
/// Split out of [`bundle_and_compile_named`] because bundling and compiling
/// can happen in different processes: a session client bundles (its working
/// directory is the one relative imports resolve against) and the session host
/// compiles (its `QuickJS` build is the one that will load the bytecode), so
/// bytecode never crosses the wire between differently-built binaries.
///
/// Does not touch the disk cache — the caller owns the key, because only it
/// knows which inputs the code was built from.
///
/// # Errors
///
/// Returns [`ScriptError`] if the module fails to declare (a syntax error, or
/// an import the native loader cannot resolve) or to serialize.
pub async fn compile_bundled_source(
  code: &str,
  module_name: &str,
  source_map_json: Option<&str>,
) -> Result<CompiledBundle, ScriptError> {
  let name = module_name.to_string();
  let code = code.to_string();
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

  Ok(CompiledBundle {
    module_name: module_name.to_string(),
    bytecode: Arc::from(bytecode.into_boxed_slice()),
    source_map: source_map_json
      .and_then(|j| sourcemap::SourceMap::from_slice(j.as_bytes()).ok())
      .map(Arc::new),
  })
}

/// Link + evaluate the bundled step module from precompiled bytecode in
/// the given session. Top-level `Given`/`When`/`Then` run here.
pub async fn eval_bundle(vm: &crate::vm::VmHandle, bundle: &CompiledBundle) -> Result<(), ScriptError> {
  eval_bundle_with(vm, bundle, |_, _| Ok(())).await
}

/// [`eval_bundle`], plus a look at the evaluated module's namespace.
///
/// A host that consumes a module's EXPORTS rather than its
/// registrations — a reporter module, whose default export is the
/// class to instantiate — needs the namespace, which `eval_bundle`
/// drops. `after` runs on the VM loop with the namespace object, right
/// after the module's top level has settled.
pub async fn eval_bundle_with<F>(vm: &crate::vm::VmHandle, bundle: &CompiledBundle, after: F) -> Result<(), ScriptError>
where
  F: for<'js> FnOnce(&rquickjs::Ctx<'js>, rquickjs::Object<'js>) -> Result<(), ScriptError> + Send + 'static,
{
  let bytecode = Arc::clone(&bundle.bytecode);
  let label = bundle.module_name.clone();
  let mapper = bundle.mapper();
  crate::vm_with!(vm => |ctx| {
    crate::bindings::call_site::register_bundle(&ctx, mapper);
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
    let (evaluated, promise) = match module.eval().catch(&ctx) {
      Ok(pair) => pair,
      Err(e) => return Err(caught_to_script_error(e, &label)),
    };
    match promise.into_future::<()>().await.catch(&ctx) {
      // A bundle's top level may register tools of its own; the
      // callables are built from the registry, so they only exist after
      // a rebuild.
      Ok(()) => {
        crate::bindings::rebuild_tool_bindings(&ctx)
          .map_err(|e| ScriptError::internal(format!("rebuild tool bindings: {e}")))?;
        let namespace = match evaluated.namespace().catch(&ctx) {
          Ok(ns) => ns,
          Err(e) => return Err(caught_to_script_error(e, &label)),
        };
        after(&ctx, namespace)
      },
      Err(e) => Err(caught_to_script_error_in(&ctx, e, &label)),
    }
  })
  .await?
}

impl CompiledBundle {
  /// This bundle's position mapping, detached so a VM can keep it.
  #[must_use]
  pub fn mapper(&self) -> SourceMapper {
    SourceMapper {
      module_name: self.module_name.clone(),
      map: self.source_map.clone(),
    }
  }

  /// Map a bundled-output `line:col` (1-based, as QuickJS reports) back
  /// to the original `.ts`/`.js` source location.
  #[must_use]
  pub fn remap(&self, line: u32, col: u32) -> Option<(String, u32, u32)> {
    let sm = self.source_map.as_ref()?;
    let token = sm.lookup_token(line.saturating_sub(1), col.saturating_sub(1))?;
    let src = token.get_source().unwrap_or("<unknown>").to_string();
    Some((src, token.get_src_line() + 1, token.get_src_col() + 1))
  }

  /// Render a [`ScriptError`] with every bundled-output position
  /// translated back to the original `.ts`/`.js` source: the primary
  /// `line:col`, the source snippet, and each stack frame.
  #[must_use]
  pub fn format_error(&self, e: &ScriptError) -> String {
    use std::fmt::Write as _;

    let mut m = e.message.clone();
    if let Some(line) = e.line {
      let col = e.column.unwrap_or(1);
      if let Some((src, sl, sc)) = self.remap(line, col) {
        let _ = write!(m, " (at {src}:{sl}:{sc})");
      } else {
        let _ = write!(m, " (at {}:{line}:{col})", self.module_name);
      }
    }
    if let Some(snippet) = &e.source_snippet {
      m.push('\n');
      m.push_str(snippet);
    }
    // QuickJS does not expose `lineNumber` as an own property on a plain
    // `throw new Error(...)`; the location lives in the stack. Remap each
    // `<bundle>:line:col` frame back to the original .ts/.js source.
    if let Some(stack) = &e.stack {
      let stack = stack.trim_end();
      if !stack.is_empty() {
        m.push('\n');
        m.push_str(&self.remap_stack(stack));
      }
    }
    m
  }

  /// Rewrite `<bundle module>:LINE:COL` occurrences in a JS stack to the
  /// original source location via the rolldown source map.
  #[must_use]
  pub fn remap_stack(&self, stack: &str) -> String {
    use std::sync::OnceLock;

    use regex::Regex;
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let Some(re) = RE.get_or_init(|| Regex::new(r"([^\s()]+):(\d+):(\d+)").ok()) else {
      return stack.to_string();
    };
    re.replace_all(stack, |caps: &regex::Captures<'_>| {
      let (Ok(line), Ok(col)) = (caps[2].parse::<u32>(), caps[3].parse::<u32>()) else {
        return caps[0].to_string();
      };
      match self.remap(line, col) {
        Some((src, sl, sc)) => format!("{src}:{sl}:{sc}"),
        None => caps[0].to_string(),
      }
    })
    .into_owned()
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
/// What one extension file registered under one host, sliced out of the
/// shared registries by the difference its evaluation made.
///
/// Tools were the only thing extraction ever reported, so a file whose
/// contribution is steps, hooks, parameter types or fixtures showed up
/// as "declares no tools" — indistinguishable from a `defineTool` that
/// never ran.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct HostRegistrations {
  /// Tool manifests, verbatim as the registry serialises them (the
  /// manifest type belongs to the MCP crate, so this stays raw JSON).
  pub tools: Vec<serde_json::Value>,
  /// `"Given a cart with 2 items"` — keyword and expression.
  pub steps: Vec<String>,
  /// `"Before"`, `"AfterAll"`, …
  pub hooks: Vec<String>,
  pub param_types: Vec<String>,
  /// `test(...)` titles.
  pub tests: Vec<String>,
  /// `test.extend` fixture names.
  pub fixtures: Vec<String>,
  /// `defineDefaults(defaults)` payloads, in call order. Folded under
  /// every config layer by the two-pass startup.
  pub defaults: Vec<serde_json::Value>,
  /// Why this host's evaluation failed, when it did. A file is entitled
  /// to throw under one host and work under another — a session
  /// isolates per file per host, so extraction records the throw where
  /// it happened instead of condemning the file everywhere.
  pub error: Option<String>,
  /// The thrown error's `name`, kept beside the message because one
  /// name decides whether the failure may be skipped:
  /// `ExtensionPolicyError` never can. A cache HIT replays the recorded
  /// throw rather than re-evaluating, so the marker has to survive in
  /// the snapshot.
  pub error_name: Option<String>,
}

impl HostRegistrations {
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.tools.is_empty()
      && self.steps.is_empty()
      && self.hooks.is_empty()
      && self.param_types.is_empty()
      && self.tests.is_empty()
      && self.fixtures.is_empty()
      && self.defaults.is_empty()
  }
}

/// One extension file's contribution, per host.
///
/// A file branches on `ferridriver.host`, so what it registers is a
/// function of the host — extracting under one host and reporting that
/// as "the manifest" hid every contribution the other three make.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ExtensionSnapshot {
  pub hosts: std::collections::BTreeMap<String, HostRegistrations>,
}

impl ExtensionSnapshot {
  #[must_use]
  pub fn for_host(&self, host: &str) -> Option<&HostRegistrations> {
    self.hosts.get(host)
  }

  /// The tool manifests this file declares under `host`, as the JSON
  /// array its consumer deserialises.
  #[must_use]
  pub fn tools_json(&self, host: &str) -> String {
    let tools = self.hosts.get(host).map(|h| h.tools.as_slice()).unwrap_or_default();
    serde_json::to_string(tools).unwrap_or_else(|_| "[]".to_string())
  }

  /// The first `[extensions.policy]` refusal any host recorded, as
  /// `(host, message)`. Never skippable, so its consumer fails rather
  /// than dropping the package.
  #[must_use]
  pub fn policy_refusal(&self) -> Option<(&str, &str)> {
    self.hosts.iter().find_map(|(host, registrations)| {
      let message = registrations.error.as_deref()?;
      (registrations.error_name.as_deref() == Some(crate::error::EXTENSION_POLICY_ERROR))
        .then_some((host.as_str(), message))
    })
  }

  /// The config defaults this file contributes under `host`, in call
  /// order.
  #[must_use]
  pub fn defaults_for(&self, host: &str) -> &[serde_json::Value] {
    self.hosts.get(host).map(|h| h.defaults.as_slice()).unwrap_or_default()
  }

  /// Whether the file registered anything at all, under any host.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.hosts.values().all(HostRegistrations::is_empty)
  }

  /// Why the file failed to evaluate under `host`, if it did.
  #[must_use]
  pub fn host_error(&self, host: &str) -> Option<&str> {
    self.hosts.get(host).and_then(|h| h.error.as_deref())
  }
}

/// Cache payload envelope. The snapshot's shape is going to grow, and a
/// reader that met an older one used to deserialise it as an empty
/// manifest list rather than as the miss it is.
#[derive(serde::Serialize, serde::Deserialize)]
struct AuxEnvelope {
  v: u32,
  snapshot: ExtensionSnapshot,
}

/// Bump on any change to [`ExtensionSnapshot`] that a reader cannot
/// absorb; an entry at another version is a miss.
const AUX_VERSION: u32 = 2;

fn encode_aux(snapshot: &ExtensionSnapshot) -> String {
  serde_json::to_string(&AuxEnvelope {
    v: AUX_VERSION,
    snapshot: snapshot.clone(),
  })
  .unwrap_or_else(|_| String::new())
}

fn decode_aux(aux: Option<&str>) -> Option<ExtensionSnapshot> {
  let envelope: AuxEnvelope = serde_json::from_str(aux?).ok()?;
  (envelope.v == AUX_VERSION).then_some(envelope.snapshot)
}

pub struct CompiledExtension {
  /// The group's first file — what a report names.
  pub path: PathBuf,
  /// Every file bundled into this module. A package's entries share one
  /// bundle, so a helper both of them import is evaluated once.
  pub files: Vec<PathBuf>,
  pub index: usize,
  pub bytecode: Arc<[u8]>,
  /// The module name baked into `bytecode`, which is what QuickJS
  /// labels this extension's stack frames with — the key a session's
  /// source-map registry is looked up by.
  pub module_name: String,
  /// Source-map JSON for that bundle, so a frame from this extension
  /// reports the author's `.ts` line rather than a bundled offset.
  /// `None` when the bundle produced no map.
  pub source_map_json: Option<String>,
  /// Everything the file registered, per host — read straight off the
  /// registries its evaluation changed, never by re-running it.
  pub snapshot: ExtensionSnapshot,
}

impl CompiledExtension {
  /// The tool manifests this file declares under the MCP host, as the
  /// JSON array `ferridriver-mcp` deserialises.
  #[must_use]
  pub fn manifests_json(&self) -> String {
    self.snapshot.tools_json(crate::ExtensionHost::Mcp.as_str())
  }
}

impl CompiledExtension {
  /// The mapper a session registers so this extension's frames report
  /// the author's source. Parsed here rather than carried as a live map,
  /// because the JSON is what both cache tiers store.
  #[must_use]
  pub fn mapper(&self) -> SourceMapper {
    SourceMapper {
      module_name: self.module_name.clone(),
      map: self
        .source_map_json
        .as_deref()
        .and_then(|j| sourcemap::SourceMap::from_slice(j.as_bytes()).ok())
        .map(Arc::new),
    }
  }
}

/// One in-process cache entry: the compiled bytecode, its manifests, and
/// the transitive input set the bundle was built from (with that set's
/// content fingerprint).
///
/// The inputs are what make the entry safe to reuse. Keying on the ENTRY
/// file's own bytes alone served stale bytecode the moment an imported
/// helper changed — the entry's bytes were identical, so a reload
/// (`ferridriver_extensions action: "reload"`, `ext dev --watch`) kept
/// handing out code compiled from the old helper.
struct CachedExtension {
  bytecode: Arc<[u8]>,
  snapshot: ExtensionSnapshot,
  module_name: String,
  source_map_json: Option<String>,
  inputs: Vec<PathBuf>,
  inputs_fingerprint: u64,
}

/// Process-scoped content-hash cache: `hash(canonical path + bytes)` ->
/// [`CachedExtension`]. A extension file whose whole transitive input set
/// is unchanged skips rolldown + compile entirely on any later
/// `compile_and_extract_extensions` call (reload, the same file discovered
/// under two roots, a repeated host setup). Bounded by the number
/// of distinct extension files a process ever loads (tiny) so no eviction
/// is needed.
///
/// This is the hot in-process tier; `compile_and_extract_extensions` also
/// consults the cross-process disk tier ([`crate::bytecode_cache`]),
/// whose ABI tag (QuickJS version, arch, endianness, pointer width) +
/// transitive input hashes are what keep the `unsafe Module::load`
/// paths sound for bytecode another process wrote.
type ExtensionCache = std::sync::Mutex<rustc_hash::FxHashMap<u64, CachedExtension>>;
static EXTENSION_BYTECODE_CACHE: std::sync::OnceLock<ExtensionCache> = std::sync::OnceLock::new();

fn extension_cache() -> &'static ExtensionCache {
  EXTENSION_BYTECODE_CACHE.get_or_init(|| std::sync::Mutex::new(rustc_hash::FxHashMap::default()))
}

/// Record a compile in the in-process tier together with the input set
/// its freshness depends on. An unreadable input means "cannot vouch for
/// this" — the entry is simply not cached rather than cached as stale.
fn remember_extension(
  key: u64,
  bytecode: &Arc<[u8]>,
  snapshot: &ExtensionSnapshot,
  module_name: &str,
  source_map_json: Option<&str>,
  inputs: Vec<PathBuf>,
) {
  let Some(fingerprint) = crate::bytecode_cache::inputs_fingerprint(&inputs) else {
    return;
  };
  if let Ok(mut cache) = extension_cache().lock() {
    cache.insert(
      key,
      CachedExtension {
        bytecode: bytecode.clone(),
        snapshot: snapshot.clone(),
        module_name: module_name.to_string(),
        source_map_json: source_map_json.map(str::to_string),
        inputs,
        inputs_fingerprint: fingerprint,
      },
    );
  }
}

/// Cache key: the file's canonical path (rolldown resolution + relative
/// imports depend on it) plus its byte content, plus
/// [`bundle_env_fingerprint`] (a shim/alias edit changes the output for
/// the same input bytes). SipHash via the std default hasher — adequate
/// for an in-process content cache, no dep.
fn cache_key(group: &[PathBuf], bytes: &[u8], shims_fp: u64) -> u64 {
  use std::hash::{Hash, Hasher};
  let mut h = std::collections::hash_map::DefaultHasher::new();
  for path in group {
    std::fs::canonicalize(path)
      .unwrap_or_else(|_| path.clone())
      .hash(&mut h);
  }
  bytes.hash(&mut h);
  shims_fp.hash(&mut h);
  h.finish()
}

/// Every file of a group, concatenated, as the content half of its
/// cache key.
fn group_bytes(group: &[PathBuf]) -> Result<Vec<u8>, ScriptError> {
  let mut out = Vec::new();
  for path in group {
    let bytes = std::fs::read(path).map_err(|e| ScriptError::internal(format!("read {}: {e}", path.display())))?;
    out.extend_from_slice(&bytes);
  }
  Ok(out)
}

/// The directory a group bundles from: its first entry's.
fn group_cwd(group: &[PathBuf]) -> PathBuf {
  group
    .first()
    .and_then(|p| p.parent())
    .unwrap_or_else(|| Path::new("."))
    .to_path_buf()
}

/// Bundle + compile + extract every extension file. The expensive
/// per-file rolldown bundles run concurrently; bytecode compile +
/// extraction share ONE throwaway runtime for the whole batch (the
/// pre-migration path spun one full engine per file for extraction
/// *and* one per file for bytecode). Unchanged files are served from
/// A module name derived from the file's own identity rather than its
/// position in the batch.
///
/// Two bytecode modules can share a name — QuickJS creates a module at
/// `Module::load` and never looks one up by name — but the SOURCE MAP
/// registry keys on it (`call_site::register_bundle` keeps one mapper
/// per name), and every load path compiles whatever is cold in ITS
/// batch, so two extensions routinely reach one VM having been compiled
/// apart. Under a position-derived name they collide there, and the
/// second one's frames are mapped through the first one's map.
fn extension_module_name(group: &[PathBuf]) -> String {
  use std::hash::{Hash, Hasher};
  // A provider is loaded under the SPECIFIER it serves, so an importer
  // links straight to it: QuickJS looks a module up by the name the
  // resolver returned, and that name is already in `loaded_modules`.
  // No facade, no re-export list, and exactly one instance per run.
  if let [only] = group
    && let Some(specifier) = crate::provided_modules::provider_module_name(only)
  {
    return specifier;
  }
  let mut h = std::collections::hash_map::DefaultHasher::new();
  for path in group {
    std::fs::canonicalize(path)
      .unwrap_or_else(|_| path.clone())
      .hash(&mut h);
  }
  format!("ferri_extension_{:016x}.js", h.finish())
}

/// Cache namespace for one extension file. A provider's bytecode is
/// compiled under a different module name from a plain entry's, and the
/// name is baked into the bytecode, so the two must not share a slot.
fn extension_cache_kind(group: &[PathBuf]) -> String {
  match group {
    [only] => crate::provided_modules::provider_module_name(only)
      .map_or_else(|| "extension".to_string(), |s| format!("extension:provide:{s}")),
    _ => "extension".to_string(),
  }
}

/// the process content-hash cache with no bundle and no compile.
///
/// Per-file failures (bundle, compile, or extraction) are returned
/// rather than aborting the batch. Output preserves input file order;
/// surviving `CompiledExtension`s carry contiguous `index` values.
pub async fn compile_and_extract_extensions(
  groups: &[Vec<PathBuf>],
  policy: &ferridriver_config::ExtensionPolicyConfig,
) -> (Vec<CompiledExtension>, Vec<(PathBuf, ScriptError)>) {
  // Per original position: a cache hit (bytecode + manifests), or a
  // cache miss we must bundle, or an early failure. A miss carries both
  // the in-memory content key and the disk-cache key so the compile step
  // can populate both tiers.

  let shims_fp = bundle_env_fingerprint();
  let mut slots: Vec<Slot> = Vec::with_capacity(groups.len());
  for group in groups {
    match group_bytes(group) {
      Ok(b) => {
        let inmem_key = cache_key(group, &b, shims_fp);
        let cached = extension_cache().lock().ok().and_then(|c| {
          let hit = c.get(&inmem_key)?;
          // Same question the disk tier asks: did ANY input change?
          if crate::bytecode_cache::inputs_fingerprint(&hit.inputs) != Some(hit.inputs_fingerprint) {
            return None;
          }
          Some(Loaded {
            bytecode: hit.bytecode.clone(),
            snapshot: hit.snapshot.clone(),
            module_name: hit.module_name.clone(),
            source_map_json: hit.source_map_json.clone(),
          })
        });
        // A group bundles from its first entry's directory: a
        // package's entries live together, and a loose file is its own
        // group.
        let ext_cwd = group_cwd(group);
        let disk_key = crate::bytecode_cache::entry_key(&extension_cache_kind(group), group, &ext_cwd, shims_fp);
        match cached {
          // 1. In-memory (same process).
          Some(hit) => slots.push(Slot::Hit(hit)),
          // 2. Disk (cross-process), transitively validated. Promote into
          //    the in-memory tier so later same-process loads stay hot.
          // An entry whose payload this build cannot read is a MISS,
          // not an empty snapshot.
          None => match crate::bytecode_cache::load(disk_key).and_then(|e| {
            let snapshot = decode_aux(e.aux.as_deref())?;
            Some((e.bytecode, e.module_name, e.source_map_json, e.inputs, snapshot))
          }) {
            Some((bytecode, module_name, source_map_json, inputs, snapshot)) => {
              let bc: Arc<[u8]> = Arc::from(bytecode.into_boxed_slice());
              // The map and the module name come back with the entry:
              // dropping them here is what made a cache hit report
              // bundled offsets where a cold compile reported the
              // author's `.ts` line.
              //
              // Reuse the input set the disk manifest recorded rather
              // than re-deriving it; it is what that bytecode's freshness
              // was just validated against.
              remember_extension(
                inmem_key,
                &bc,
                &snapshot,
                &module_name,
                source_map_json.as_deref(),
                inputs,
              );
              slots.push(Slot::Hit(Loaded {
                bytecode: bc,
                snapshot,
                module_name,
                source_map_json,
              }));
            },
            // 3. Cold: bundle + compile below.
            None => slots.push(Slot::Miss { inmem_key, disk_key }),
          },
        }
      },
      Err(e) => slots.push(Slot::Failed(e)),
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
    let group = groups[i].clone();
    async move {
      let cwd = group_cwd(&group);
      // One graph per GROUP: a package's entries share their helpers,
      // and bundling them apart would inline a shared helper into each
      // and give one module two states.
      (i, Box::pin(bundle_source(&group, &cwd)).await)
    }
  }))
  .await;

  // Compiled code (+ source map for stack traces, + the module graph for
  // the caches' transitive input set) per missed position. Absent = the
  // bundle failed.
  let mut bundled_code: rustc_hash::FxHashMap<usize, String> = rustc_hash::FxHashMap::default();
  let mut bundled_map: rustc_hash::FxHashMap<usize, Option<String>> = rustc_hash::FxHashMap::default();
  let mut bundled_modules: rustc_hash::FxHashMap<usize, Vec<PathBuf>> = rustc_hash::FxHashMap::default();
  for (i, res) in bundles {
    match res {
      Ok(b) => {
        bundled_code.insert(i, b.code);
        bundled_map.insert(i, b.source_map_json);
        let mut modules = b.modules;
        modules.extend(b.config_inputs);
        bundled_modules.insert(i, modules);
      },
      Err(e) => slots[i] = Slot::Failed(e),
    }
  }

  // One throwaway runtime/context compiles + extracts every missed file.
  // Native resolver/loader for the same reason as `bundle_and_compile`:
  // declare-time resolution of the external native specifiers.
  //
  // With nothing missed there is nothing to read back, so no file has to
  // evaluate at all — the whole batch's manifests came from a cache.
  // Nothing to extract: every file's manifests came from a cache, and no
  // file needs a context to be read back in.
  if !miss_idx.is_empty() {
    match extraction_hosts(policy).await {
      Ok(contexts) => {
        // Compile every missed file ONCE, in a context of its own.
        //
        // `Module::declare` parses and resolves — so a consumer of a
        // package-provided specifier resolves it HERE, against the
        // loader's stub, before any provider has evaluated. Doing that
        // in a host context would leave the stub registered under the
        // specifier's name, and the entries would link to it instead of
        // to the provider that evaluates in the pass below.
        let Ok(compile) = compile_context(policy).await else {
          for s in &mut slots {
            if matches!(s, Slot::Miss { .. }) {
              *s = Slot::Failed(ScriptError::internal("extension compile context".to_string()));
            }
          }
          return finish(slots, groups);
        };
        let compile_ctx = &compile.1;
        let mut compiled: rustc_hash::FxHashMap<usize, Arc<[u8]>> = rustc_hash::FxHashMap::default();
        for &i in &miss_idx {
          let Some(code) = bundled_code.get(&i) else { continue };
          let module_name = extension_module_name(&groups[i]);
          match compile_one(compile_ctx, &module_name, code).await {
            Ok(bc) => {
              compiled.insert(i, Arc::from(bc.into_boxed_slice()));
            },
            Err(e) => slots[i] = Slot::Failed(e),
          }
        }

        // One pass per host. What a file registers is a function of
        // `ferridriver.host` — an extension that only calls `defineTool`
        // under `mcp` and `Given` under `bdd` reported, under a
        // single-host extraction, exactly half of itself.
        //
        // Within a pass: file order, hits included. A session evaluates
        // every extension it was given into one VM, so extraction has to
        // reach each file in a context where the earlier ones have
        // already run. Stopping at the LAST miss costs nothing — a hit
        // after it has no cold file left to observe what it would leave
        // behind.
        let last_miss = miss_idx.last().copied().unwrap_or(0);
        let mut snapshots: rustc_hash::FxHashMap<usize, ExtensionSnapshot> = rustc_hash::FxHashMap::default();
        for (host, _runtime, actx) in &contexts {
          for i in 0..=last_miss {
            let (bytecode, label, is_miss) = match &slots[i] {
              Slot::Hit(hit) => (Arc::clone(&hit.bytecode), hit.module_name.clone(), false),
              Slot::Miss { .. } => match compiled.get(&i) {
                Some(bc) => (Arc::clone(bc), extension_module_name(&groups[i]), true),
                None => continue,
              },
              Slot::Failed(_) => continue,
            };
            match eval_and_slice(actx, &bytecode, &label).await {
              Ok(registrations) => {
                if is_miss {
                  snapshots
                    .entry(i)
                    .or_default()
                    .hosts
                    .insert(host.as_str().to_string(), registrations);
                }
              },
              // Recorded against THIS host, not against the file: a
              // session installs per file per host and skips the pairing
              // that throws, so condemning the file everywhere would
              // report a package as broken that three hosts run fine.
              Err(e) => {
                tracing::warn!(
                  target: "ferridriver::extensions",
                  path = %group_label(&groups[i]),
                  host = host.as_str(),
                  error = %e.message,
                  "extension.extract.host_failed: the file threw under this host"
                );
                if is_miss {
                  snapshots.entry(i).or_default().hosts.insert(
                    host.as_str().to_string(),
                    HostRegistrations {
                      error: Some(e.message.clone()),
                      error_name: e.name.clone(),
                      ..HostRegistrations::default()
                    },
                  );
                }
              },
            }
          }
        }

        // Persist what the passes found, in both cache tiers.
        for &i in &miss_idx {
          let Slot::Miss { inmem_key, disk_key } = slots[i] else {
            continue;
          };
          let Some(bytecode) = compiled.get(&i) else { continue };
          let snapshot = snapshots.remove(&i).unwrap_or_default();
          // Throwing under one host is the file's business; throwing
          // under EVERY host is a file that cannot work anywhere — a
          // registration the operator ceiling refuses, say — and that is
          // a failure, not a snapshot full of errors.
          if !snapshot.hosts.is_empty() && snapshot.hosts.values().all(|h| h.error.is_some()) {
            let (first, name) = snapshot
              .hosts
              .values()
              .find_map(|h| h.error.clone().map(|message| (message, h.error_name.clone())))
              .unwrap_or_else(|| ("extension failed under every host".to_string(), None));
            // The thrown error's NAME decides whether the failure may be
            // skipped, so rebuilding it as a plain internal error is how
            // an `[extensions.policy]` refusal used to reach the loader
            // as a skippable compile failure.
            slots[i] = Slot::Failed(if name.as_deref() == Some(crate::error::EXTENSION_POLICY_ERROR) {
              ScriptError::policy(first)
            } else {
              ScriptError::internal(first)
            });
            continue;
          }
          let module_name = extension_module_name(&groups[i]);
          // Inputs = this group's files plus their transitive imports
          // (from the module graph), so an edited helper invalidates the
          // entry in BOTH tiers.
          let map = bundled_map.get(&i).cloned().flatten();
          let modules = bundled_modules.get(&i).cloned().unwrap_or_default();
          let inputs = crate::bytecode_cache::input_set(&groups[i], &modules);
          let aux = encode_aux(&snapshot);
          crate::bytecode_cache::store(disk_key, bytecode, &module_name, map.as_deref(), Some(&aux), &inputs);
          remember_extension(inmem_key, bytecode, &snapshot, &module_name, map.as_deref(), inputs);
          slots[i] = Slot::Hit(Loaded {
            bytecode: Arc::clone(bytecode),
            snapshot,
            module_name,
            source_map_json: map,
          });
        }
      },
      Err(err) => {
        for s in &mut slots {
          if matches!(s, Slot::Miss { .. }) {
            *s = Slot::Failed(err.clone());
          }
        }
      },
    }
  }

  finish(slots, groups)
}

/// What a position ended up with. `Hit` carries everything a caller
/// needs to install the file AND map its frames — the module name and
/// source map travel with the bytecode through both cache tiers,
/// because a cached extension's stack traces have to read the same as
/// a freshly compiled one's.
struct Loaded {
  bytecode: Arc<[u8]>,
  snapshot: ExtensionSnapshot,
  module_name: String,
  source_map_json: Option<String>,
}
enum Slot {
  Hit(Loaded),
  Miss { inmem_key: u64, disk_key: u64 },
  Failed(ScriptError),
}

/// A group's files as one label, for a diagnostic that must name what
/// failed without printing a paragraph.
fn group_label(group: &[PathBuf]) -> String {
  match group {
    [only] => only.display().to_string(),
    many => many
      .iter()
      .map(|p| p.display().to_string())
      .collect::<Vec<_>>()
      .join(", "),
  }
}

/// Turn the per-position outcomes into the batch's result.
fn finish(slots: Vec<Slot>, groups: &[Vec<PathBuf>]) -> (Vec<CompiledExtension>, Vec<(PathBuf, ScriptError)>) {
  let mut survivors: Vec<CompiledExtension> = Vec::new();
  let mut failures: Vec<(PathBuf, ScriptError)> = Vec::new();
  for (i, slot) in slots.into_iter().enumerate() {
    match slot {
      Slot::Hit(hit) => survivors.push(CompiledExtension {
        path: groups[i].first().cloned().unwrap_or_default(),
        files: groups[i].clone(),
        index: survivors.len(),
        bytecode: hit.bytecode,
        module_name: hit.module_name,
        source_map_json: hit.source_map_json,
        snapshot: hit.snapshot,
      }),
      Slot::Failed(e) => failures.push((groups[i].first().cloned().unwrap_or_default(), e)),
      // A Miss with no compiled output never reached Hit/Failed only if
      // its bundle was dropped — already recorded as Failed above; this
      // arm is unreachable but keeps the match total without a panic.
      Slot::Miss { .. } => failures.push((
        groups[i].first().cloned().unwrap_or_default(),
        ScriptError::internal("extension compile produced no output".to_string()),
      )),
    }
  }
  (survivors, failures)
}

/// Install everything the extraction context shares with a session VM,
/// once for the whole batch.
///
/// A session installs all of this before `install_extensions`, so an
/// extension whose top level uses a standard global (`TextEncoder`,
/// `setTimeout`, `crypto`, `console`, `expect`) or the Playwright test
/// surface must find it here too — otherwise the file throws during
/// extraction and is skipped with a warning, never reaching the session
/// that would have run it fine. Only session-scoped bindings
/// (fs/vars/artifacts/commands/page/request) are absent: those are
/// per-session by definition and top-level extension code must not
/// depend on them.
///
/// The operator ceiling is installed for the same reason: `defineTool`
/// clamps `allow.*` at REGISTRATION time, so an extraction that carries
/// no ceiling accepts a package the session then refuses.
async fn install_extraction_env(
  actx: &AsyncContext,
  policy: &ferridriver_config::ExtensionPolicyConfig,
  host: crate::ExtensionHost,
) -> Result<(), ScriptError> {
  let policy = policy.clone();
  actx
    .async_with(async |ctx| {
      crate::bindings::install_bdd(&ctx)
        .map_err(|e| ScriptError::internal(format!("install extension registry: {e}")))?;
      crate::bindings::define_classes(&ctx).map_err(|e| ScriptError::internal(format!("install classes: {e}")))?;
      crate::engine::install_runtime_shims(&ctx)
        .map_err(|e| ScriptError::internal(format!("install runtime shims: {e}")))?;
      crate::bindings::expect::install_expect(&ctx)
        .map_err(|e| ScriptError::internal(format!("install expect: {e}")))?;
      crate::bindings::test::install_test(&ctx)
        .map_err(|e| ScriptError::internal(format!("install test surface: {e}")))?;
      // The host this context extracts for: an extension branches on
      // `ferridriver.host`, so each host needs its own context to
      // register what that host would have seen.
      crate::bindings::runtime::install_host(&ctx, host.as_str())
        .map_err(|e| ScriptError::internal(format!("install ferridriver.host: {e}")))?;
      let _ = ctx.store_userdata(crate::bindings::registry::ExtensionPolicyUd(policy));
      Ok(())
    })
    .await
}

/// One extraction runtime + context per host.
///
/// A runtime, not just a context: `store_userdata` is keyed on the
/// RUNTIME (`Ctx::get_opaque` reads `JS_GetRuntime`), and the registries
/// every contribution point writes into are userdata. Four contexts on
/// one runtime would share one registry — the second context would not
/// even get its `defineTool` global, because `registry::install` returns
/// early when the userdata is already there — so the per-host slices
/// would be each other's.
async fn extraction_hosts(
  policy: &ferridriver_config::ExtensionPolicyConfig,
) -> Result<Vec<(crate::ExtensionHost, AsyncRuntime, AsyncContext)>, ScriptError> {
  use crate::ExtensionHost as H;
  let mut out = Vec::new();
  for host in [H::Mcp, H::Bdd, H::Test, H::Script] {
    let runtime = AsyncRuntime::new().map_err(|e| ScriptError::internal(format!("extension bytecode runtime: {e}")))?;
    runtime
      .set_loader(
        crate::bindings::native_modules::resolver(),
        crate::bindings::native_modules::loader(),
      )
      .await;
    let ctx = AsyncContext::full(&runtime)
      .await
      .map_err(|e| ScriptError::internal(format!("extension bytecode context: {e}")))?;
    install_extraction_env(&ctx, policy, host).await?;
    out.push((host, runtime, ctx));
  }
  Ok(out)
}

/// A runtime + context used ONLY to parse and serialise, never to
/// evaluate — kept apart from the host passes so what `Module::declare`
/// resolves here cannot become what an entry links to there.
async fn compile_context(
  policy: &ferridriver_config::ExtensionPolicyConfig,
) -> Result<(AsyncRuntime, AsyncContext), ScriptError> {
  let runtime = AsyncRuntime::new().map_err(|e| ScriptError::internal(format!("extension bytecode runtime: {e}")))?;
  runtime
    .set_loader(
      crate::bindings::native_modules::resolver(),
      crate::bindings::native_modules::loader(),
    )
    .await;
  let ctx = AsyncContext::full(&runtime)
    .await
    .map_err(|e| ScriptError::internal(format!("extension bytecode context: {e}")))?;
  install_extraction_env(&ctx, policy, crate::ExtensionHost::Script).await?;
  Ok((runtime, ctx))
}

/// Parse the bundled module and serialise it to bytecode. Parsing only
/// — nothing evaluates, so every per-host pass starts from the same
/// bytes and from registries this call did not touch.
async fn compile_one(actx: &AsyncContext, module_name: &str, code: &str) -> Result<Vec<u8>, ScriptError> {
  let name = module_name.to_string();
  let code = code.to_string();
  let label = module_name.to_string();
  actx
    .async_with(async |ctx| {
      // Bundled module has no remaining imports — `declare` (parse only)
      // needs no resolver; mirrors `bundle_and_compile`.
      let module = Module::declare(ctx.clone(), name.into_bytes(), code.into_bytes())
        .catch(&ctx)
        .map_err(|e| caught_to_script_error(e, &label))?;
      module
        .write(WriteOptions {
          // Same process + interpreter that will `load` it.
          endianness: WriteOptionsEndianness::Native,
          ..Default::default()
        })
        .map_err(|e| ScriptError::internal(format!("extension module write: {e}")))
    })
    .await
}

/// Load + evaluate one extension's bytecode in a host context and slice
/// off everything it registered.
///
/// The evaluation matters even for a file whose registrations are
/// already cached: the files after it must see the world a session
/// would have given them.
async fn eval_and_slice(actx: &AsyncContext, bytecode: &[u8], label: &str) -> Result<HostRegistrations, ScriptError> {
  let bytecode = bytecode.to_vec();
  let label = label.to_string();
  let cfg_default = crate::engine::ScriptEngineConfig::default();
  actx
    .async_with(async |ctx| {
      // Fresh capture per file: whatever the extension's top level logs
      // is forwarded to tracing under the module label after eval.
      let console = std::sync::Arc::new(crate::console::ConsoleCapture::new(
        cfg_default.max_console_entries,
        cfg_default.max_console_bytes,
        cfg_default.max_console_entry_bytes,
      ));
      crate::console_fmt::install_console(&ctx, console.clone())
        .map_err(|e| ScriptError::internal(format!("install console: {e}")))?;

      let marks = crate::bindings::registry::registry_marks(&ctx)?;

      // SAFETY: same-interpreter precondition as `install_one_extension`
      // — the bytes came from `Module::write` in this process or from the
      // disk cache, whose ABI tag and input hashes guarantee an
      // ABI-identical toolchain wrote them.
      #[allow(unsafe_code)]
      let module = (unsafe { Module::load(ctx.clone(), &bytecode) })
        .catch(&ctx)
        .map_err(|e| caught_to_script_error(e, &label))?;
      let promise = module
        .eval()
        .catch(&ctx)
        .map_err(|e| caught_to_script_error(e, &label))?
        .1;
      let evaled = promise.into_future::<()>().await.catch(&ctx);
      for entry in console.drain() {
        tracing::info!(target: "ferridriver::extensions", extension = %label, "{}", entry.message);
      }
      evaled.map_err(|e| caught_to_script_error(e, &label))?;

      crate::bindings::registry::registrations_since(&ctx, marks)
    })
    .await
}
