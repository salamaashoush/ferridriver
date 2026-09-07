//! ferridriver's bundle front-end over `ferrijs-bundle`: the operator's
//! `[bundler]` options as the bundler's, the session module table as
//! the externals, the disk cache where the operator said, and the
//! extension extraction that runs an extension's bytecode once to learn
//! what it contributes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferrijs::rquickjs;
use ferrijs_bundle::{BundlerOptions, BytecodeCache};
use rquickjs::CatchResultExt;

use crate::error::ScriptError;

pub use ferrijs::source_map::{LazyMap, SourceMapper, resolve_source};
pub use ferrijs_bundle::{BundledSource, is_typescript_path, source_is_es_module};

/// One bundled+tree-shaken graph compiled to `QuickJS` bytecode, plus the
/// source map to translate bundled positions back to source.
pub type CompiledBundle = ferrijs::CompiledModule;

/// Operator-facing bundler options: the `[bundler]` section of the
/// unified config (shim aliases, inline virtual modules, and the module
/// resolution controls) plus the `[test].tsconfig` selection. Applied to
/// EVERY bundle ferridriver produces -- BDD step files, extensions,
/// `ferridriver run` scripts.
#[derive(Debug, Default, Clone)]
pub struct BundlerEnv {
  /// `specifier -> absolute shim file path`. The shim is bundled and
  /// transpiled like any other source (so `.ts` works) and lands in the
  /// source map, which keeps the disk-cache freshness check covering it.
  pub alias: Vec<(String, PathBuf)>,
  /// `specifier -> inline ES-module source` (never touches the fs).
  pub virtual_modules: Vec<(String, String)>,
  /// Extra `exports`/`imports` condition names.
  pub conditions: Vec<String>,
  /// `package.json` fields consulted when no `exports` entry matches.
  pub main_fields: Vec<String>,
  /// `package.json` field paths holding a legacy path-remapping object.
  pub alias_fields: Vec<Vec<String>>,
  /// The tsconfig whose `paths` / `baseUrl` govern resolution. `None`
  /// leaves rolldown's per-module upward discovery in place.
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

  /// Stable content fingerprint of everything here, folded into every
  /// bundle cache key so editing a mapping, a virtual module or a
  /// resolution control invalidates cached bytecode.
  #[must_use]
  pub fn fingerprint(&self) -> u64 {
    self.options().fingerprint()
  }

  /// The bundler's own options, with the package-provided specifiers as
  /// externals.
  fn options(&self) -> BundlerOptions {
    BundlerOptions {
      alias: self.alias.clone(),
      virtual_modules: self.virtual_modules.clone(),
      conditions: self.conditions.clone(),
      main_fields: self.main_fields.clone(),
      alias_fields: self.alias_fields.clone(),
      tsconfig: self.tsconfig.clone(),
      externals: crate::bindings::native_modules::provided_externals(),
    }
  }
}

/// Process-global bundler environment, installed once by the host (CLI /
/// MCP server) from the loaded config before any bundling happens. A
/// global (rather than a parameter threaded through every bundle entry
/// point) because the config is process-wide and the bundle paths are
/// reached from five call sites across three crates.
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

/// Where compiled bytecode is kept between processes:
/// `$FERRIDRIVER_CACHE_DIR/ferridriver`, else the platform user cache
/// under `ferridriver`; nowhere when `FERRIDRIVER_NO_BYTECODE_CACHE` is
/// set.
#[must_use]
pub fn bytecode_cache() -> BytecodeCache {
  static CACHE: std::sync::OnceLock<BytecodeCache> = std::sync::OnceLock::new();
  CACHE
    .get_or_init(|| {
      if std::env::var_os("FERRIDRIVER_NO_BYTECODE_CACHE").is_some() {
        return BytecodeCache::disabled();
      }
      match std::env::var_os("FERRIDRIVER_CACHE_DIR") {
        Some(dir) => BytecodeCache::at(PathBuf::from(dir).join("ferridriver")),
        None => BytecodeCache::for_app("ferridriver"),
      }
    })
    .clone()
}

/// The bundler for this process: the operator's options over the
/// session module table, with the disk cache.
///
/// # Errors
///
/// When the module table cannot be built (an alias naming nothing).
pub fn bundler() -> Result<ferrijs_bundle::Bundler, ScriptError> {
  let registry = crate::bindings::native_modules::registry().map_err(ScriptError::internal)?;
  Ok(ferrijs_bundle::Bundler::new(
    bundler_env().options(),
    registry,
    bytecode_cache(),
  ))
}

/// Everything outside the entry files that can change a bundle's output
/// for byte-identical sources: the `[bundler]` shims and resolution
/// controls, the pinned tsconfig, the native module aliases and the
/// package-provided specifiers. Every extension cache key folds this in.
fn bundle_env_fingerprint() -> Result<u64, ScriptError> {
  use std::hash::{Hash, Hasher};
  let mut h = std::collections::hash_map::DefaultHasher::new();
  bundler()?.env_fingerprint().hash(&mut h);
  crate::provided_modules::provided_fingerprint().hash(&mut h);
  Ok(h.finish())
}

/// rolldown-bundle + tree-shake + transpile the entry files (and their
/// `node_modules`/shared imports) into a single ESM module. Exposed for
/// diagnostics/tests; production uses [`bundle_and_compile`].
pub async fn bundle_source(entry_paths: &[PathBuf], cwd: &Path) -> Result<BundledSource, ScriptError> {
  bundler()?.bundle(entry_paths, cwd).await
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
  bundler()?.compile(entry_paths, cwd, module_name).await
}

/// Compile already-bundled ESM `code` to `QuickJS` bytecode.
///
/// Split out of [`bundle_and_compile_named`] because bundling and compiling
/// can happen in different processes: a session client bundles (its working
/// directory is the one relative imports resolve against) and the session host
/// compiles (its `QuickJS` build is the one that will load the bytecode), so
/// bytecode never crosses the wire between differently-built binaries.
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
  bundler()?.compile_source(code, module_name, source_map_json).await
}

/// Link + evaluate the bundled step module from precompiled bytecode in
/// the given session. Top-level `Given`/`When`/`Then` run here.
pub async fn eval_bundle(vm: &ferrijs::VmHandle, bundle: &CompiledBundle) -> Result<(), ScriptError> {
  eval_bundle_with(vm, bundle, |_, _| Ok(())).await
}

/// [`eval_bundle`], plus a look at the evaluated module's namespace.
///
/// A host that consumes a module's EXPORTS rather than its
/// registrations -- a reporter module, whose default export is the
/// class to instantiate -- needs the namespace, which `eval_bundle`
/// drops. `after` runs on the VM loop with the namespace object, right
/// after the module's top level has settled.
pub async fn eval_bundle_with<F>(vm: &ferrijs::VmHandle, bundle: &CompiledBundle, after: F) -> Result<(), ScriptError>
where
  F: for<'js> FnOnce(&rquickjs::Ctx<'js>, rquickjs::Object<'js>) -> Result<(), ScriptError> + Send + 'static,
{
  let bytecode = Arc::clone(&bundle.bytecode);
  let label = bundle.module_name.clone();
  let mapper = bundle.mapper();
  ferrijs::vm_with!(vm => |ctx| {
    ferrijs::source_map::register_bundle(&ctx, mapper);
    let evaluated = ferrijs::eval_bytecode(&ctx, &bytecode, &label).await?;
    // A bundle's top level may register tools of its own; the callables
    // are built from the registry, so they only exist after a rebuild.
    crate::bindings::rebuild_tool_bindings(&ctx)
      .map_err(|e| ScriptError::internal(format!("rebuild tool bindings: {e}")))?;
    let namespace = match evaluated.namespace().catch(&ctx) {
      Ok(ns) => ns,
      Err(e) => return Err(ScriptError::from_caught(&ctx, e, &label)),
    };
    after(&ctx, namespace)
  })
  .await?
}

/// What a compiled bundle can say about a failure and about its inputs.
pub trait CompiledBundleExt {
  /// Render a [`ScriptError`] with every bundled-output position
  /// translated back to the original `.ts`/`.js` source: the primary
  /// `line:col`, the source snippet, and each stack frame.
  fn format_error(&self, e: &ScriptError) -> String;

  /// Rewrite `<bundle module>:LINE:COL` occurrences in a JS stack to the
  /// original source location via the source map.
  fn remap_stack(&self, stack: &str) -> String;

  /// Every source file that went into this bundle (entry + transitive
  /// imports), resolved to absolute paths against `cwd`. Read from the
  /// source map's `sources`; synthetic (non-file) sources are skipped.
  fn source_files(&self, cwd: &Path) -> Vec<PathBuf>;
}

impl CompiledBundleExt for CompiledBundle {
  fn format_error(&self, e: &ScriptError) -> String {
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
    // `throw new Error(...)`; the location lives in the stack.
    if let Some(stack) = &e.stack {
      let stack = stack.trim_end();
      if !stack.is_empty() {
        m.push('\n');
        m.push_str(&self.remap_stack(stack));
      }
    }
    m
  }

  fn remap_stack(&self, stack: &str) -> String {
    stack
      .split('\n')
      .map(|line| {
        let Some((file, l, c)) = ferrijs::source_map::innermost_frame(line) else {
          return line.to_string();
        };
        if file != self.module_name {
          return line.to_string();
        }
        match self.remap(l, c) {
          Some((src, sl, sc)) => line.replace(&format!("{file}:{l}:{c}"), &format!("{src}:{sl}:{sc}")),
          None => line.to_string(),
        }
      })
      .collect::<Vec<_>>()
      .join("\n")
  }

  fn source_files(&self, cwd: &Path) -> Vec<PathBuf> {
    self
      .source_map
      .sources()
      .iter()
      .map(|src| {
        let p = Path::new(src);
        if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }
      })
      .collect()
  }
}

/// A compiled bundle as the runner's [`ferridriver_test::host::SourceMap`]:
/// a position in the code QuickJS executed, answered as the file the
/// author wrote, resolved against the directory the bundle was built
/// from.
pub struct BundleSourceMap {
  bundle: Arc<CompiledBundle>,
  cwd: Arc<PathBuf>,
}

impl BundleSourceMap {
  #[must_use]
  pub fn new(bundle: Arc<CompiledBundle>, cwd: Arc<PathBuf>) -> Self {
    Self { bundle, cwd }
  }
}

impl ferridriver_test::host::SourceMap for BundleSourceMap {
  fn remap(&self, line: u32, column: u32) -> Option<(String, u32, u32)> {
    let (src, src_line, src_col) = self.bundle.remap(line, column)?;
    Some((resolve_source(&self.cwd, &src).display().to_string(), src_line, src_col))
  }
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
    SourceMapper::new(
      self.module_name.clone(),
      LazyMap::from_json(self.source_map_json.as_deref()),
    )
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
/// consults the cross-process disk tier (the disk cache),
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
  let Some(fingerprint) = ferrijs_bundle::cache::inputs_fingerprint(&inputs) else {
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

  let shims_fp = match bundle_env_fingerprint() {
    Ok(fp) => fp,
    Err(e) => {
      return (
        Vec::new(),
        groups
          .iter()
          .map(|g| (g.first().cloned().unwrap_or_default(), e.clone()))
          .collect(),
      );
    },
  };
  let mut slots: Vec<Slot> = Vec::with_capacity(groups.len());
  for group in groups {
    match group_bytes(group) {
      Ok(b) => {
        let inmem_key = cache_key(group, &b, shims_fp);
        let cached = extension_cache().lock().ok().and_then(|c| {
          let hit = c.get(&inmem_key)?;
          // Same question the disk tier asks: did ANY input change?
          if ferrijs_bundle::cache::inputs_fingerprint(&hit.inputs) != Some(hit.inputs_fingerprint) {
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
        let disk_key = ferrijs_bundle::cache::entry_key(&extension_cache_kind(group), group, &ext_cwd, shims_fp);
        match cached {
          // 1. In-memory (same process).
          Some(hit) => slots.push(Slot::Hit(hit)),
          // 2. Disk (cross-process), transitively validated. Promote into
          //    the in-memory tier so later same-process loads stay hot.
          // An entry whose payload this build cannot read is a MISS,
          // not an empty snapshot.
          None => match bytecode_cache().load(disk_key).and_then(|e| {
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
        let Ok(compiler) = bundler() else {
          for s in &mut slots {
            if matches!(s, Slot::Miss { .. }) {
              *s = Slot::Failed(ScriptError::internal("extension compile context".to_string()));
            }
          }
          return finish(slots, groups);
        };
        let mut emitted: rustc_hash::FxHashMap<usize, Arc<[u8]>> = rustc_hash::FxHashMap::default();
        for &i in &miss_idx {
          let Some(code) = bundled_code.get(&i) else { continue };
          let module_name = extension_module_name(&groups[i]);
          match compiler.compile_source(code, &module_name, None).await {
            Ok(module) => {
              emitted.insert(i, module.bytecode);
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
        for (host, rt) in &contexts {
          for i in 0..=last_miss {
            let (bytecode, label, is_miss) = match &slots[i] {
              Slot::Hit(hit) => (Arc::clone(&hit.bytecode), hit.module_name.clone(), false),
              Slot::Miss { .. } => match emitted.get(&i) {
                Some(bc) => (Arc::clone(bc), extension_module_name(&groups[i]), true),
                None => continue,
              },
              Slot::Failed(_) => continue,
            };
            match eval_and_slice(rt, &bytecode, &label).await {
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
          let Some(bytecode) = emitted.get(&i) else { continue };
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
              crate::error::policy_error(first)
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
          let inputs = ferrijs_bundle::cache::input_set(&groups[i], &modules);
          let aux = encode_aux(&snapshot);
          bytecode_cache().store(disk_key, bytecode, &module_name, map.as_deref(), Some(&aux), &inputs);
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

/// What an extraction realm installs beyond the runtime's own surface:
/// the registry and every contribution point, the class prototypes,
/// the `expect` and `test` surfaces, `ferridriver.host`, and the
/// operator ceiling. The per-session bindings (vars, artifacts,
/// commands, page, request) are absent: those are per-session by
/// definition and top-level extension code must not depend on them.
///
/// The operator ceiling is installed for the same reason: `defineTool`
/// clamps `allow.*` at REGISTRATION time, so an extraction that carries
/// no ceiling accepts a package the session then refuses.
///
/// `fetch` is present but refusing: a module-scope request would put a
/// network call inside `ferridriver ext check` and make the pass depend
/// on a live host. The function still has to be THERE, because
/// `typeof fetch === 'function'` is how a library picks between the
/// native client and a bundled polyfill.
struct ExtractionExtension {
  policy: ferridriver_config::ExtensionPolicyConfig,
  host: crate::ExtensionHost,
}

impl ferrijs::Extension for ExtractionExtension {
  fn name(&self) -> &'static str {
    "ferridriver-extraction"
  }

  fn modules(&self, registry: &mut ferrijs::ModuleRegistry) -> Result<(), String> {
    crate::bindings::native_modules::register(registry)
  }

  fn loaders(&self) -> Vec<(ferrijs::modules::BoxResolver, ferrijs::modules::BoxLoader)> {
    crate::bindings::native_modules::provided_loaders()
  }

  fn require_hook(&self) -> Option<Arc<dyn ferrijs::RequireHook>> {
    Some(Arc::new(crate::bindings::native_modules::FerridriverRequire))
  }

  fn install(&self, ctx: &rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    crate::bindings::install_bdd(ctx)?;
    crate::bindings::define_classes(ctx)?;
    crate::bindings::runtime::mirror_global(ctx, "process")?;
    let refusing = rquickjs::Function::new(ctx.clone(), || -> rquickjs::Result<()> {
      Err(rquickjs::Error::new_from_js_message(
        "fetch",
        "extraction",
        "no HTTP during extension extraction: move the call into a handler or a hook, \
         where the session's client and its `allow.net` grant exist",
      ))
    })?;
    ctx.globals().set("fetch", refusing)?;
    crate::bindings::expect::install_expect(ctx)?;
    crate::bindings::test::install_test(ctx)?;
    // The host this realm extracts for: an extension branches on
    // `ferridriver.host`, so each host needs its own realm to register
    // what that host would have seen.
    crate::bindings::runtime::install_host(ctx, self.host.as_str())?;
    let _ = ctx.store_userdata(crate::bindings::registry::ExtensionPolicyUd(self.policy.clone()));
    Ok(())
  }
}

/// One extraction realm per host.
///
/// A realm, not just a context: `store_userdata` is keyed on the
/// RUNTIME, and the registries every contribution point writes into are
/// userdata. Four contexts on one runtime would share one registry, so
/// the per-host slices would be each other's.
async fn extraction_hosts(
  policy: &ferridriver_config::ExtensionPolicyConfig,
) -> Result<Vec<(crate::ExtensionHost, ferrijs::Runtime)>, ScriptError> {
  use crate::ExtensionHost as H;
  let mut out = Vec::new();
  for host in [H::Mcp, H::Bdd, H::Test, H::Script] {
    let rt = ferrijs::Runtime::builder()
      .permissions(ferrijs::Permissions::none())
      .without_fetch()
      .extension(ExtractionExtension {
        policy: policy.clone(),
        host,
      })
      .build()
      .await?;
    out.push((host, rt));
  }
  Ok(out)
}

/// Load + evaluate one extension's bytecode in a host realm and slice
/// off everything it registered.
///
/// The evaluation matters even for a file whose registrations are
/// already cached: the files after it must see the world a session
/// would have given them.
async fn eval_and_slice(rt: &ferrijs::Runtime, bytecode: &[u8], label: &str) -> Result<HostRegistrations, ScriptError> {
  let bytecode = bytecode.to_vec();
  let label = label.to_string();
  let cfg_default = crate::engine::ScriptEngineConfig::default();
  ferrijs::vm_with!(rt.handle() => |ctx| {
    // Fresh capture per file: whatever the extension's top level logs
    // is forwarded to tracing under the module label after eval.
    let console = std::sync::Arc::new(ferrijs::ConsoleCapture::new(
      cfg_default.max_console_entries,
      cfg_default.max_console_bytes,
      cfg_default.max_console_entry_bytes,
    ));
    ferrijs::console_fmt::install_console(&ctx, console.clone())
      .map_err(|e| ScriptError::internal(format!("install console: {e}")))?;

    let marks = crate::bindings::registry::registry_marks(&ctx)?;
    let evaled = ferrijs::eval_bytecode(&ctx, &bytecode, &label).await;
    for entry in console.drain() {
      tracing::info!(target: "ferridriver::extensions", extension = %label, "{}", entry.message);
    }
    evaled?;
    crate::bindings::registry::registrations_since(&ctx, marks)
  })
  .await?
}
