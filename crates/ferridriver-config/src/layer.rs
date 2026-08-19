//! Layered configuration resolution.
//!
//! A ferridriver setup is rarely one file. An operator installs
//! machine- or user-wide defaults (browser instances, extensions,
//! server instructions), a repository pins project settings, and a
//! developer keeps personal overrides out of git. Before this module
//! the loader took the FIRST file it found and ignored the rest, so a
//! project file silently deleted every user-level setting — the only
//! way to combine them was to copy the whole document into every
//! repository, where it then drifted.
//!
//! # Layer order
//!
//! Lowest precedence first; a later layer overrides an earlier one:
//!
//! 1. `/etc/ferridriver/config.{toml,yaml,yml,json}` (machine)
//! 2. `<user config dir>/ferridriver/config.*` (user)
//! 3. `<git root>/ferridriver.*` (project, when the git root is not the cwd)
//! 4. `./ferridriver.*` (cwd)
//! 5. `./ferridriver.local.*` and `<git root>/ferridriver.local.*` (personal, gitignored)
//! 6. the file passed to `-c/--config`, if any (explicit)
//! 7. `FERRIDRIVER_<SECTION>__<KEY>` environment overrides
//!
//! Every file may name others in `extends`; each extended file is
//! applied immediately BELOW the file that names it (so the extending
//! file wins), recursively, with cycle detection.
//!
//! `-c/--config` does not disable inheritance — that is the point of
//! the stack: a project can pin two settings and still get the user's
//! browser instances. Pass `inherit: false` (CLI `--no-inherit`, env
//! `FERRIDRIVER_NO_INHERIT=1`) for a reproducible single-file load.
//!
//! # Merge rules
//!
//! Objects merge key by key. Scalars replace. Arrays replace, EXCEPT
//! the additive keys in [`APPEND_KEYS`], which concatenate (earlier
//! layers first) and de-duplicate — an operator adding an extension or
//! a Chrome flag at the user level must not have it deleted by a
//! project that adds a different one.
//!
//! # Path anchoring
//!
//! A relative path inside a config file is resolved against THAT
//! FILE's directory, before merging. `extensions = ["./acme.ts"]` in
//! `~/.config/ferridriver/config.yaml` therefore means
//! `~/.config/ferridriver/acme.ts` no matter which repository the
//! process runs in. Globs (`testMatch`, `steps`, `features`) are left
//! alone: they stay relative to the run's `testDir`/cwd, matching
//! Playwright.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::FerridriverConfig;

/// Config file basenames searched for a project/cwd layer, in format
/// precedence order.
const PROJECT_BASENAMES: &[&str] = &[
  "ferridriver.toml",
  "ferridriver.yaml",
  "ferridriver.yml",
  "ferridriver.json",
  "ferridriver.config.ts",
  "ferridriver.config.mts",
  "ferridriver.config.js",
  "ferridriver.config.mjs",
];

/// Personal-override basenames, searched alongside [`PROJECT_BASENAMES`].
const LOCAL_BASENAMES: &[&str] = &[
  "ferridriver.local.toml",
  "ferridriver.local.yaml",
  "ferridriver.local.yml",
  "ferridriver.local.json",
  "ferridriver.local.ts",
  "ferridriver.local.js",
];

/// Basenames searched inside a machine/user config DIRECTORY.
const DIR_BASENAMES: &[&str] = &[
  "config.toml",
  "config.yaml",
  "config.yml",
  "config.json",
  "config.ts",
  "config.js",
];

/// Array keys that concatenate across layers instead of replacing.
/// Matched on the leaf key name at any depth.
///
/// These are the additive collections: an entry added by one layer is
/// never a statement that another layer's entries should go away.
/// Everything else (globs, reporters, projects, ...) replaces, because
/// a layer that redefines them is redefining a whole policy.
pub const APPEND_KEYS: &[&str] = &["paths", "sidecars", "chromeArgs", "args", "allowEnv"];

/// Leaf key names whose string value is a filesystem path and is
/// therefore anchored to its own config file's directory.
const PATH_KEYS: &[&str] = &[
  "scriptRoot",
  "artifactsRoot",
  "executablePath",
  "userDataDir",
  "discoverProfile",
  "testDir",
  "outputDir",
  "snapshotDir",
  "tsconfig",
  "storageState",
  "cwd",
];

/// Leaf key names holding an ARRAY of filesystem paths.
const PATH_ARRAY_KEYS: &[&str] = &["globalSetup", "globalTeardown"];

/// Keys Playwright spells `string | string[]`, both forms holding paths.
const PATH_STRING_OR_ARRAY_KEYS: &[&str] = &["stylePath"];

/// Keys whose object value is keyed by the OPERATOR, not by the schema.
/// Their contents are data, so no key inside them may be read as a path
/// name.
const OPAQUE_MAP_KEYS: &[&str] = &["env", "headers", "extraHTTPHeaders", "settings", "worldParameters"];

/// Which slot in the stack a layer occupied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LayerKind {
  Machine,
  User,
  Project,
  Cwd,
  Local,
  /// Pulled in by another layer's `extends`.
  Extends,
  /// Named by `-c/--config`.
  Explicit,
}

impl LayerKind {
  /// Short label for `ferridriver config` output.
  #[must_use]
  pub fn label(self) -> &'static str {
    match self {
      Self::Machine => "machine",
      Self::User => "user",
      Self::Project => "project",
      Self::Cwd => "cwd",
      Self::Local => "local",
      Self::Extends => "extends",
      Self::Explicit => "explicit",
    }
  }
}

/// One config file that contributed to the resolved document.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigLayer {
  pub kind: LayerKind,
  pub path: PathBuf,
}

/// Where a resolved value came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "from", content = "source")]
pub enum Origin {
  /// A config file at this path.
  File(PathBuf),
  /// An environment variable of this name.
  Env(String),
  /// `defineDefaults` in the named extension package.
  Extension(String),
}

impl Origin {
  /// Human-readable source for diagnostics.
  #[must_use]
  pub fn describe(&self) -> String {
    match self {
      Self::File(p) => p.display().to_string(),
      Self::Env(name) => format!("${name}"),
      Self::Extension(name) => format!("extension {name}"),
    }
  }
}

/// A non-fatal problem found while resolving: a key nobody reads, a
/// deprecated spelling, an `extends` that could not be followed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigWarning {
  /// File (or `$VAR`) the problem came from.
  pub source: String,
  pub message: String,
}

/// Where each resolved key came from, accumulated while merging.
#[derive(Debug, Default)]
struct Provenance {
  /// Dotted key -> the origin whose value won.
  winners: BTreeMap<String, Origin>,
  /// Dotted key -> every origin that added to a concatenated array.
  contributors: BTreeMap<String, Vec<Origin>>,
}

impl Provenance {
  fn win(&mut self, path: &str, origin: &Origin) {
    self.winners.insert(path.to_string(), origin.clone());
  }

  fn contributed(&mut self, path: &str, origin: &Origin) {
    let entries = self.contributors.entry(path.to_string()).or_default();
    if entries.last() != Some(origin) {
      entries.push(origin.clone());
    }
  }
}

/// Turns a config MODULE into a document.
///
/// `.ts` / `.js` are config formats like any other from where an author
/// sits, but parsing one means bundling and evaluating it, which lives
/// in `ferridriver-script` — a crate that depends on THIS one. So the
/// dependency inverts: the host installs a loader, and this crate calls
/// it for exactly the paths whose extension needs one.
///
/// That inversion is also what makes a module cost nothing to a stack
/// that has none. There is no guard to forget: a run whose layers are
/// all documents never reaches a call site that could construct a
/// bundler or a VM.
pub type ModuleLoader = Arc<dyn Fn(&Path) -> anyhow::Result<Value> + Send + Sync>;

/// Extensions that name a config MODULE rather than a document.
const MODULE_EXTENSIONS: &[&str] = &["ts", "mts", "cts", "tsx", "js", "mjs", "cjs", "jsx"];

/// Whether `path` needs a [`ModuleLoader`] rather than [`parse_file`].
#[must_use]
pub fn is_script_config(path: &Path) -> bool {
  path
    .extension()
    .and_then(|e| e.to_str())
    .is_some_and(|e| MODULE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Layer documents already read from disk, shared across the passes of
/// one startup.
///
/// Startup resolves the stack more than once when an extension
/// contributes defaults or a `--config` module has to be evaluated. The
/// FOLD has to be redone each time — a contribution is the lowest layer,
/// and that is where append-keys concatenate and relative paths anchor —
/// but the files have not changed, so reading and parsing them again is
/// waste. Worse than waste: a file edited between two passes would
/// produce a merged document that no single state of the disk ever had.
///
/// Cheap to clone (one `Arc`), and empty by default, so a caller that
/// resolves once pays nothing for it.
#[derive(Debug, Clone, Default)]
pub struct LayerCache(Arc<Mutex<BTreeMap<PathBuf, Value>>>);

impl LayerCache {
  /// The document at `path`, read (or evaluated) only the first time.
  ///
  /// A module goes through `loader`; every other format is parsed here.
  /// Evaluating a module is expensive enough that caching it matters for
  /// its own sake, not only to spare a second read.
  ///
  /// # Errors
  ///
  /// As [`parse_file`], or whatever the loader reports — on the pass
  /// that actually reads it. A module with no loader installed is an
  /// error naming the file: a config the author wrote and the run
  /// silently ignored is the worst of the available answers.
  pub fn parse(&self, path: &Path, loader: Option<&ModuleLoader>) -> anyhow::Result<Value> {
    if let Some(hit) = self
      .0
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .get(path)
    {
      return Ok(hit.clone());
    }
    let value = if is_script_config(path) {
      let Some(loader) = loader else {
        anyhow::bail!(
          "config {}: this is a `.ts` / `.js` config, which has to be compiled and evaluated — \
           this host has no JavaScript runtime. Run it through the `ferridriver` CLI, or write \
           the config as `.toml` / `.yaml` / `.json`.",
          path.display()
        );
      };
      loader(path)?
    } else {
      parse_file(path)?
    };
    self
      .0
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(path.to_path_buf(), value.clone());
    Ok(value)
  }
}

/// Inputs to a resolve. Every ambient dependency (cwd, user config
/// directory, environment) is a field so a test can resolve a stack
/// without mutating process state.
#[derive(Clone)]
pub struct LoadOptions {
  /// File named by `-c/--config`, applied above the discovered files.
  pub explicit: Option<PathBuf>,
  /// Directory the run is anchored at (normally the process cwd).
  pub cwd: PathBuf,
  /// User config directory holding `ferridriver/config.*`.
  pub user_config_dir: Option<PathBuf>,
  /// Machine config directory (normally `/etc`).
  pub machine_config_dir: Option<PathBuf>,
  /// Environment the `FERRIDRIVER_*__*` overrides are read from.
  pub env: BTreeMap<String, String>,
  /// When false, only `explicit` (or, without it, the single
  /// highest-precedence discovered file) is loaded.
  pub inherit: bool,
  /// `defineDefaults` contributions, `(package, payload)` in load
  /// order. They are applied BENEATH every discovered file, so a config
  /// file, an environment override and a CLI flag all still win.
  ///
  /// Empty on the first pass: the list of extensions is itself read
  /// from the config, so the defaults can only be learnt after one
  /// resolution has happened. See [`Resolved`] and the CLI's two-pass
  /// startup.
  pub extension_defaults: Vec<(String, Value)>,
  /// Layer documents already read, so the passes of one startup read
  /// each file once between them. Default-empty; a single resolve never
  /// notices it.
  pub cache: LayerCache,
  /// How a `.ts` / `.js` layer becomes a document. `None` on a host with
  /// no JavaScript runtime, where meeting one is an error rather than a
  /// silent skip.
  pub module_loader: Option<ModuleLoader>,
  /// Fold only the layers this crate can read by itself.
  ///
  /// The first phase of a startup that has a module layer: a module
  /// cannot be compiled until `extensions`, `[bundler]`, `[scripting]`
  /// and `[test].moduleAliases` are known, and those are precisely the
  /// sections a module may not set. Folding the documents alone settles
  /// them, the host installs what it needs, and the second phase folds
  /// the whole stack with every layer in its own slot.
  pub documents_only: bool,
}

impl std::fmt::Debug for LoadOptions {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("LoadOptions")
      .field("explicit", &self.explicit)
      .field("cwd", &self.cwd)
      .field("user_config_dir", &self.user_config_dir)
      .field("machine_config_dir", &self.machine_config_dir)
      .field("env", &self.env)
      .field("inherit", &self.inherit)
      .field("extension_defaults", &self.extension_defaults)
      .field("cache", &self.cache)
      .field("module_loader", &self.module_loader.is_some())
      .field("documents_only", &self.documents_only)
      .finish()
  }
}

impl LoadOptions {
  /// Options reading the real process environment.
  #[must_use]
  pub fn from_process(explicit: Option<&Path>) -> Self {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let explicit = explicit
      .map(Path::to_path_buf)
      .or_else(|| env.get("FERRIDRIVER_CONFIG").map(PathBuf::from));
    let inherit = !env
      .get("FERRIDRIVER_NO_INHERIT")
      .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    Self {
      explicit,
      cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
      user_config_dir: dirs::config_dir(),
      machine_config_dir: Some(PathBuf::from("/etc")),
      env,
      inherit,
      extension_defaults: Vec::new(),
      cache: LayerCache::default(),
      module_loader: None,
      documents_only: false,
    }
  }

  /// Options that touch nothing outside `cwd`: no machine or user
  /// layer, no environment overrides. The base for tests and for
  /// reproducible runs.
  #[must_use]
  pub fn isolated(cwd: impl Into<PathBuf>) -> Self {
    Self {
      explicit: None,
      cwd: cwd.into(),
      user_config_dir: None,
      machine_config_dir: None,
      env: BTreeMap::new(),
      inherit: true,
      extension_defaults: Vec::new(),
      cache: LayerCache::default(),
      module_loader: None,
      documents_only: false,
    }
  }
}

/// The outcome of resolving the layer stack.
#[derive(Debug)]
pub struct Resolved {
  /// The typed configuration every host consumes.
  pub config: FerridriverConfig,
  /// Files that contributed, in application order (lowest first).
  pub layers: Vec<ConfigLayer>,
  /// Non-fatal problems worth showing the operator.
  pub warnings: Vec<ConfigWarning>,
  /// Dotted key -> where its winning value came from.
  pub provenance: BTreeMap<String, Origin>,
  /// Dotted key -> every layer that CONTRIBUTED to it, in application
  /// order, for the [`APPEND_KEYS`] whose value is concatenated rather
  /// than replaced.
  ///
  /// [`Self::provenance`] can only name one origin, which for an appended
  /// array is the last one to add an entry — so a `chromeArgs` built from
  /// three layers was reported as belonging to whichever file happened to
  /// go last. That is precisely the question `ferridriver config` exists
  /// to answer.
  pub contributors: BTreeMap<String, Vec<Origin>>,
  /// The merged document, for `ferridriver config --resolved`.
  pub document: Value,
}

/// The files this stack would apply, without reading any of them.
///
/// Lets a host ask whether it needs a [`ModuleLoader`] before building
/// one — the whole point being that a stack of documents never
/// constructs a JavaScript runtime to read its configuration.
#[must_use]
pub fn discovered_paths(opts: &LoadOptions) -> Vec<PathBuf> {
  let mut warnings = Vec::new();
  discover_layers(opts, &mut warnings)
    .into_iter()
    .map(|l| l.path)
    .collect()
}

/// Resolve the full layer stack.
///
/// # Errors
///
/// Returns an error when a file that was explicitly named (by
/// `-c/--config` or by an `extends` entry) cannot be read or parsed,
/// or when the merged document does not satisfy the schema. A
/// DISCOVERED file that fails to parse is also an error: silently
/// ignoring a malformed config is how a setup ends up running with
/// defaults nobody chose.
pub fn resolve(opts: &LoadOptions) -> anyhow::Result<Resolved> {
  let mut warnings = Vec::new();
  let discovered = discover_layers(opts, &mut warnings);

  let mut document = Value::Object(Map::new());
  let mut provenance = Provenance::default();
  let mut layers = Vec::new();
  let mut seen = BTreeSet::new();
  let mut extension_bases = BTreeMap::new();

  // Beneath everything: what the loaded packages contribute. A file, an
  // environment override and a CLI flag all overwrite these.
  for (package, defaults) in &opts.extension_defaults {
    let mut value = defaults.clone();
    check_contributed_defaults(package, &value)?;
    normalize(&mut value);
    merge(
      &mut document,
      &value,
      "",
      &Origin::Extension(package.clone()),
      &mut provenance,
    );
  }

  for layer in discovered {
    // Phase one folds the documents alone, to learn how a module would
    // be compiled. A module layer joins on the second pass, in this same
    // slot — its position in the stack is its position in the stack,
    // whichever phase reads it.
    if opts.documents_only && is_script_config(&layer.path) {
      continue;
    }
    apply_layer(
      &layer,
      &mut Fold {
        document: &mut document,
        provenance: &mut provenance,
        layers: &mut layers,
        seen: &mut seen,
        extension_bases: &mut extension_bases,
        warnings: &mut warnings,
        cache: &opts.cache,
        loader: opts.module_loader.as_ref(),
      },
    )?;
  }

  apply_env(&opts.env, &mut document, &mut provenance, &mut warnings);
  let Provenance { winners, contributors } = provenance;

  let mut config = deserialize_document(&document, &mut warnings)?;
  config.validate()?;
  config.source_dir = layers.last().and_then(|l| l.path.parent()).map(Path::to_path_buf);
  // Playwright's `configDir`: what a relative `snapshotPathTemplate`
  // resolves against, carried on the test config so the runner does not
  // have to be handed the layer stack.
  config.test.config_dir.clone_from(&config.source_dir);
  config.extension_bases = extension_bases;

  Ok(Resolved {
    config,
    layers,
    warnings,
    provenance: winners,
    contributors,
    document,
  })
}

/// Build the ordered list of files to apply, before `extends`
/// expansion.
fn discover_layers(opts: &LoadOptions, warnings: &mut Vec<ConfigWarning>) -> Vec<ConfigLayer> {
  if !opts.inherit {
    let single = match opts.explicit.clone() {
      Some(path) => Some(ConfigLayer {
        kind: LayerKind::Explicit,
        path,
      }),
      None => highest_precedence_file(opts, warnings),
    };
    return single.into_iter().collect();
  }

  let mut layers = Vec::new();

  if let Some(dir) = &opts.machine_config_dir
    && let Some(path) = first_existing_reported(&dir.join("ferridriver"), DIR_BASENAMES, warnings)
  {
    layers.push(ConfigLayer {
      kind: LayerKind::Machine,
      path,
    });
  }

  if let Some(dir) = &opts.user_config_dir
    && let Some(path) = first_existing_reported(&dir.join("ferridriver"), DIR_BASENAMES, warnings)
  {
    layers.push(ConfigLayer {
      kind: LayerKind::User,
      path,
    });
  }

  // Every ancestor between the repository root and the cwd may carry a
  // config, applied outermost first so the NEAREST one wins. That is
  // what makes a monorepo work: the root sets shared defaults, a
  // package overrides just its own keys.
  let git_root = find_git_root(&opts.cwd);
  for dir in ancestor_chain(git_root.as_deref(), &opts.cwd) {
    if let Some(path) = first_existing_reported(&dir, PROJECT_BASENAMES, warnings) {
      layers.push(ConfigLayer {
        kind: LayerKind::Project,
        path,
      });
    }
  }

  if let Some(path) = first_existing_reported(&opts.cwd, PROJECT_BASENAMES, warnings) {
    layers.push(ConfigLayer {
      kind: LayerKind::Cwd,
      path,
    });
  }

  for dir in ancestor_chain(git_root.as_deref(), &opts.cwd) {
    if let Some(path) = first_existing_reported(&dir, LOCAL_BASENAMES, warnings) {
      layers.push(ConfigLayer {
        kind: LayerKind::Local,
        path,
      });
    }
  }
  if let Some(path) = first_existing_reported(&opts.cwd, LOCAL_BASENAMES, warnings) {
    layers.push(ConfigLayer {
      kind: LayerKind::Local,
      path,
    });
  }

  if let Some(path) = &opts.explicit {
    if path.exists() {
      layers.push(ConfigLayer {
        kind: LayerKind::Explicit,
        path: path.clone(),
      });
    } else {
      warnings.push(ConfigWarning {
        source: path.display().to_string(),
        message: "config file named by --config does not exist".to_string(),
      });
    }
  }

  layers
}

/// The single file a non-inheriting load would use: the cwd's own
/// config, and nothing else.
///
/// Deliberately does NOT fall back to the user or machine directory —
/// "no inherit" has to mean it, or a reproducible run would still pick
/// up whatever an operator installed under `~/.config`.
fn highest_precedence_file(opts: &LoadOptions, warnings: &mut Vec<ConfigWarning>) -> Option<ConfigLayer> {
  if let Some(path) = first_existing_reported(&opts.cwd, LOCAL_BASENAMES, warnings) {
    return Some(ConfigLayer {
      kind: LayerKind::Local,
      path,
    });
  }
  first_existing_reported(&opts.cwd, PROJECT_BASENAMES, warnings).map(|path| ConfigLayer {
    kind: LayerKind::Cwd,
    path,
  })
}

/// The highest-precedence config in `dir`, warning when a lower-precedence
/// sibling is being shadowed.
///
/// Silently taking `ferridriver.toml` while a `ferridriver.yaml` sits
/// beside it makes every edit to the yaml look like it does nothing.
fn first_existing_reported(dir: &Path, basenames: &[&str], warnings: &mut Vec<ConfigWarning>) -> Option<PathBuf> {
  // One `stat` per candidate, NOT one `read_dir`: the cost has to scale
  // with the number of config formats, which is a fixed handful, and not
  // with the number of files in the directory, which is unbounded.
  // Listing the directory instead measured 29ms slower in a tree with
  // 5000 files beside the config — and a repository root is exactly
  // where a config lives.
  let mut matched = basenames.iter().map(|name| dir.join(name)).filter(|c| c.is_file());
  let winner = matched.next()?;
  let shadowed: Vec<String> = matched.map(|p| p.display().to_string()).collect();
  if !shadowed.is_empty() {
    warnings.push(ConfigWarning {
      source: winner.display().to_string(),
      message: format!("also present and ignored: {}", shadowed.join(", ")),
    });
  }
  Some(winner)
}

/// Directories from `root` down to (but excluding) `cwd`, outermost
/// first. Empty when there is no repository root, or when `cwd` IS the
/// root — the cwd layer covers that case.
fn ancestor_chain(root: Option<&Path>, cwd: &Path) -> Vec<PathBuf> {
  let Some(root) = root else { return Vec::new() };
  let mut chain = Vec::new();
  let mut current = cwd.parent();
  while let Some(dir) = current {
    if !dir.starts_with(root) {
      break;
    }
    chain.push(dir.to_path_buf());
    if dir == root {
      break;
    }
    current = dir.parent();
  }
  chain.reverse();
  chain
}

/// Nearest ancestor of `from` (inclusive) containing a `.git` entry.
fn find_git_root(from: &Path) -> Option<PathBuf> {
  let mut dir = Some(from);
  while let Some(current) = dir {
    if current.join(".git").exists() {
      return Some(current.to_path_buf());
    }
    dir = current.parent();
  }
  None
}

/// Read, normalize, anchor and merge one file, after recursively
/// applying whatever it `extends`.
/// The state one fold accumulates into, so `extends` recursion carries
/// it in one place rather than as eight parallel arguments.
struct Fold<'a> {
  document: &'a mut Value,
  provenance: &'a mut Provenance,
  layers: &'a mut Vec<ConfigLayer>,
  seen: &'a mut BTreeSet<PathBuf>,
  extension_bases: &'a mut BTreeMap<String, PathBuf>,
  warnings: &'a mut Vec<ConfigWarning>,
  cache: &'a LayerCache,
  loader: Option<&'a ModuleLoader>,
}

fn apply_layer(layer: &ConfigLayer, fold: &mut Fold<'_>) -> anyhow::Result<()> {
  let canonical = std::fs::canonicalize(&layer.path).unwrap_or_else(|_| layer.path.clone());
  if !fold.seen.insert(canonical.clone()) {
    // Already applied (a diamond in `extends`, or the same file
    // discovered twice). Re-applying would only duplicate append-key
    // entries.
    return Ok(());
  }

  // Through the cache: a startup that resolves more than once reads each
  // file on the first pass only.
  let mut value = fold.cache.parse(&layer.path, fold.loader)?;
  if is_script_config(&layer.path) {
    check_module_document(&layer.path, &value)?;
  }
  let dir = layer
    .path
    .parent()
    .map_or_else(|| PathBuf::from("."), Path::to_path_buf);

  for parent in take_extends(&mut value, &dir, fold.warnings) {
    apply_layer(
      &ConfigLayer {
        kind: LayerKind::Extends,
        path: parent,
      },
      fold,
    )?;
  }

  normalize(&mut value);
  anchor_paths(&mut value, &dir);
  record_extension_bases(&value, &dir, fold.extension_bases);
  merge(
    fold.document,
    &value,
    "",
    &Origin::File(layer.path.clone()),
    fold.provenance,
  );
  fold.layers.push(layer.clone());
  Ok(())
}

/// Sections a config MODULE may not set, and why. Each one had to be
/// read before the module could be bundled at all, so a value here
/// would be advice arriving after the decision it advises on.
///
/// Only `test.moduleAliases` is reachable in practice — the export IS
/// the `[test]` section — but the check is written against the whole
/// document so it still holds if that ever widens.
const REFUSED_SCRIPT_CONFIG_KEYS: &[(&str, &str)] = &[
  (
    "extensions",
    "the set of extensions is resolved before the config module is compiled",
  ),
  (
    "bundler",
    "the bundler compiled this config module before it could ask for a different one",
  ),
  (
    "scripting",
    "the sandbox a config module runs under is the operator's to set, never the module's",
  ),
  (
    "test.moduleAliases",
    "the alias table is sealed by the first bundle, which is the one that compiled this config",
  ),
];

/// Refuse what a config module cannot decide, naming the key.
///
/// Applies to a module WHEREVER it sits in the stack. Every one of these
/// had to be read before any module could be compiled — they are what
/// the documents-only phase exists to settle — so a module setting one
/// would be advice arriving after the decision it advises on.
fn check_module_document(path: &Path, value: &Value) -> anyhow::Result<()> {
  if !value.is_object() {
    anyhow::bail!(
      "config {}: its default export must be a configuration object, the same document a \
       `.toml` layer holds",
      path.display()
    );
  }
  for (key, why) in REFUSED_SCRIPT_CONFIG_KEYS {
    if key
      .split('.')
      .try_fold(value, |node, segment| node.get(segment))
      .is_some()
    {
      anyhow::bail!(
        "config {}: a config module may not set `{key}` — {why}. Put it in a ferridriver.toml          layer instead.",
        path.display()
      );
    }
  }
  Ok(())
}

/// Parse a config file into a generic document. Format comes from the
/// extension; an unknown extension is an error rather than a guess.
///
/// # Errors
///
/// Returns an error when the file cannot be read, its extension is not
/// a supported format, or its contents do not parse.
pub fn parse_file(path: &Path) -> anyhow::Result<Value> {
  let content =
    std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("failed to read config {}: {e}", path.display()))?;
  let ext = path
    .extension()
    .and_then(|e| e.to_str())
    .unwrap_or_default()
    .to_ascii_lowercase();

  let value: Value = match ext.as_str() {
    "toml" => toml::from_str(&content).map_err(|e| anyhow::anyhow!("invalid TOML config {}: {e}", path.display()))?,
    "yaml" | "yml" => {
      serde_yaml::from_str(&content).map_err(|e| anyhow::anyhow!("invalid YAML config {}: {e}", path.display()))?
    },
    "json" => {
      serde_json::from_str(&content).map_err(|e| anyhow::anyhow!("invalid JSON config {}: {e}", path.display()))?
    },
    other => anyhow::bail!(
      "unsupported config format {other:?} for {} (expected toml/yaml/yml/json)",
      path.display()
    ),
  };

  // An empty YAML file deserialises to null; treat it as an empty
  // document so it can still participate in the stack.
  Ok(if value.is_null() {
    Value::Object(Map::new())
  } else {
    value
  })
}

/// Remove the `extends` key and return the files it names, resolved
/// against `dir`.
fn take_extends(value: &mut Value, dir: &Path, warnings: &mut Vec<ConfigWarning>) -> Vec<PathBuf> {
  let Some(map) = value.as_object_mut() else {
    return Vec::new();
  };
  let Some(raw) = map.remove("extends") else {
    return Vec::new();
  };

  let entries: Vec<String> = match raw {
    Value::String(s) => vec![s],
    Value::Array(items) => items
      .into_iter()
      .filter_map(|i| i.as_str().map(str::to_string))
      .collect(),
    other => {
      warnings.push(ConfigWarning {
        source: dir.display().to_string(),
        message: format!("`extends` must be a string or array of strings, got {other}"),
      });
      Vec::new()
    },
  };

  entries.into_iter().map(|entry| resolve_against(dir, &entry)).collect()
}

/// Remember which directory each `extensions` entry was declared in.
/// Path-like entries are already absolute by now; a package specifier
/// still needs its declaring directory as the `node_modules` walk root.
fn record_extension_bases(value: &Value, dir: &Path, bases: &mut BTreeMap<String, PathBuf>) {
  let Some(paths) = value.pointer("/extensions/paths").and_then(Value::as_array) else {
    return;
  };
  for entry in paths.iter().filter_map(Value::as_str) {
    bases.insert(entry.to_string(), dir.to_path_buf());
  }
}

/// Normalize shorthand shapes so merging sees one canonical structure.
///
/// `extensions = [..]` becomes `extensions = { paths = [..] }`, so a
/// user layer using the shorthand and a project layer setting a policy
/// combine instead of one clobbering the other.
fn normalize(value: &mut Value) {
  let Some(map) = value.as_object_mut() else { return };
  if let Some(existing) = map.get_mut("extensions")
    && existing.is_array()
  {
    let paths = std::mem::replace(existing, Value::Object(Map::new()));
    if let Some(obj) = existing.as_object_mut() {
      obj.insert("paths".to_string(), paths);
    }
  }
  if let Some(test) = map.get_mut("test") {
    normalize_test_section(test);
  }
}

/// Keys a document may write outside the schema, folded into the schema
/// by [`normalize`] before anything deserializes.
///
/// `use` at the top of `[test]` is where Playwright puts it, and it is
/// the same bag as `[test.browser].use`; a config module writing one
/// gets the other. The authoring `.d.ts` declares the alias, so the
/// contract test reads it from here rather than carrying its own copy.
pub const DOCUMENT_ALIASES: &[&str] = &["use"];

/// Expand every `use` block in the `[test]` section, then fold the
/// top-level one into the browser's.
fn normalize_test_section(test: &mut Value) {
  let Some(map) = test.as_object_mut() else { return };
  expand_device(map.get_mut("use"));
  if let Some(browser) = map.get_mut("browser").and_then(Value::as_object_mut) {
    expand_device(browser.get_mut("use"));
  }
  if let Some(projects) = map.get_mut("projects").and_then(Value::as_array_mut) {
    for project in projects {
      let Some(project) = project.as_object_mut() else {
        continue;
      };
      expand_device(project.get_mut("use"));
      if let Some(browser) = project.get_mut("browser").and_then(Value::as_object_mut) {
        expand_device(browser.get_mut("use"));
      }
    }
  }
  hoist_use(map);
}

/// Move `[test].use` into `[test.browser].use`, the one place the bag is
/// stored. A key written in both spellings keeps the browser's, which is
/// the more specific of the two.
fn hoist_use(map: &mut Map<String, Value>) {
  let Some(top) = map.remove("use") else { return };
  let Some(top) = top.as_object().cloned() else {
    // Not an object: put it back so deserialization reports it rather
    // than dropping it here.
    map.insert("use".to_string(), top);
    return;
  };
  let browser = map
    .entry("browser".to_string())
    .or_insert_with(|| Value::Object(Map::new()));
  let Some(browser) = browser.as_object_mut() else { return };
  let target = browser
    .entry("use".to_string())
    .or_insert_with(|| Value::Object(Map::new()));
  let Some(target) = target.as_object_mut() else { return };
  for (key, value) in top {
    target.entry(key).or_insert(value);
  }
}

fn expand_device(block: Option<&mut Value>) {
  if let Some(block) = block.and_then(Value::as_object_mut) {
    crate::test::expand_device_keys(block);
  }
}

/// Rewrite every relative filesystem path in `value` to be absolute
/// against `dir`.
fn anchor_paths(value: &mut Value, dir: &Path) {
  match value {
    Value::Object(map) => {
      for (key, child) in map.iter_mut() {
        match key.as_str() {
          // Alias targets are shim files; virtual modules are inline
          // source and must not be touched.
          "alias" => anchor_map_values(child, dir),
          "paths" => anchor_extension_specs(child, dir),
          "virtualModules" | "virtual_modules" => {},
          // `file` is too common a leaf name to anchor globally, so the
          // secrets table anchors its own member.
          "secrets" => anchor_member(child, "file", dir),
          // Anchoring matches a LEAF KEY NAME at any depth, so a
          // caller-keyed map must be skipped whole: an instance's env
          // variable (or a request header) literally named `cwd` /
          // `testDir` / `outputDir` would otherwise be rewritten into an
          // absolute path behind the operator's back.
          k if OPAQUE_MAP_KEYS.contains(&k) => {},
          k if PATH_KEYS.contains(&k) => anchor_in_place(child, dir),
          k if PATH_ARRAY_KEYS.contains(&k) => anchor_array(child, dir),
          k if PATH_STRING_OR_ARRAY_KEYS.contains(&k) => {
            if child.is_array() {
              anchor_array(child, dir);
            } else {
              anchor_in_place(child, dir);
            }
          },
          _ => anchor_paths(child, dir),
        }
      }
    },
    Value::Array(items) => {
      for item in items.iter_mut() {
        anchor_paths(item, dir);
      }
    },
    _ => {},
  }
}

/// Anchor exactly one named member of an object, leaving its siblings alone.
fn anchor_member(value: &mut Value, member: &str, dir: &Path) {
  if let Some(child) = value.as_object_mut().and_then(|map| map.get_mut(member)) {
    anchor_in_place(child, dir);
  }
}

fn anchor_map_values(value: &mut Value, dir: &Path) {
  if let Some(map) = value.as_object_mut() {
    for child in map.values_mut() {
      anchor_in_place(child, dir);
    }
  }
}

fn anchor_array(value: &mut Value, dir: &Path) {
  if let Some(items) = value.as_array_mut() {
    for item in items.iter_mut() {
      anchor_in_place(item, dir);
    }
  }
}

/// Anchor only the entries an extension spec resolver would treat as
/// paths. A bare or scoped package specifier (`@acme/ext`) must stay a
/// specifier so `node_modules` resolution still applies.
///
/// `~`-relative entries are expanded HERE rather than left for a later
/// consumer: the extension resolver classifies anything that is not
/// `./`, `../` or absolute as a package name, so an unexpanded
/// `~/.config/...` entry failed with "extension package not found"
/// instead of a missing-file error.
fn anchor_extension_specs(value: &mut Value, dir: &Path) {
  if let Some(items) = value.as_array_mut() {
    for item in items.iter_mut() {
      let Some(text) = item.as_str() else { continue };
      if text.starts_with('~') {
        *item = Value::String(shellexpand::tilde(text).into_owned());
        continue;
      }
      if spec_looks_like_path(text) {
        anchor_in_place(item, dir);
      }
    }
  }
}

/// Mirrors `ferridriver_script::discover`'s path-vs-package rule.
fn spec_looks_like_path(spec: &str) -> bool {
  spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') || spec == "." || spec == ".."
}

fn anchor_in_place(value: &mut Value, dir: &Path) {
  if let Some(text) = value.as_str()
    && let Some(resolved) = anchored(dir, text)
  {
    *value = Value::String(resolved);
  }
}

/// The absolute form of `text` against `dir`, or `None` when it must be
/// left alone: already absolute, `~`-relative (expanded later, by the
/// consumer that knows about `~`), or carrying a `${VAR}` template a
/// caller substitutes before use.
fn anchored(dir: &Path, text: &str) -> Option<String> {
  if text.is_empty() || text.starts_with('~') || text.contains("${") || Path::new(text).is_absolute() {
    return None;
  }
  Some(normalize_path(&dir.join(text)).to_string_lossy().into_owned())
}

fn resolve_against(dir: &Path, entry: &str) -> PathBuf {
  let expanded = shellexpand::tilde(entry);
  let path = Path::new(expanded.as_ref());
  if path.is_absolute() {
    path.to_path_buf()
  } else {
    normalize_path(&dir.join(path))
  }
}

/// Lexically remove `.` and `..` components. Unlike `canonicalize`
/// this does not require the path to exist, which matters for output
/// directories a run has yet to create.
///
/// A `..` that would climb past the root is DROPPED, as the OS drops it
/// (`/..` is `/`). Keeping it produced paths like `/../private/tmp/x`,
/// which name the same file but compare as a prefix of nothing.
#[must_use]
pub fn normalize_path(path: &Path) -> PathBuf {
  let mut out = PathBuf::new();
  let mut rooted = false;
  for component in path.components() {
    match component {
      std::path::Component::CurDir => {},
      std::path::Component::ParentDir => {
        if !out.pop() && !rooted {
          out.push("..");
        }
      },
      other => {
        rooted |= matches!(other, std::path::Component::RootDir | std::path::Component::Prefix(_));
        out.push(other.as_os_str());
      },
    }
  }
  out
}

/// Deep-merge `overlay` into `base`, recording provenance for every
/// leaf the overlay wins.
fn merge(base: &mut Value, overlay: &Value, prefix: &str, origin: &Origin, provenance: &mut Provenance) {
  match (base, overlay) {
    (Value::Object(base_map), Value::Object(overlay_map)) => {
      for (key, value) in overlay_map {
        let path = if prefix.is_empty() {
          key.clone()
        } else {
          format!("{prefix}.{key}")
        };
        let appends = value.is_array() && APPEND_KEYS.contains(&key.as_str());
        match base_map.get_mut(key) {
          Some(existing) if appends => {
            let added = append_unique(existing, value);
            if added {
              provenance.contributed(&path, origin);
            }
            provenance.win(&path, origin);
          },
          Some(existing) => merge(existing, value, &path, origin, provenance),
          None => {
            if appends {
              provenance.contributed(&path, origin);
            }
            base_map.insert(key.clone(), value.clone());
            record_leaves(value, &path, origin, provenance);
          },
        }
      }
    },
    (base, _) => {
      *base = overlay.clone();
      provenance.win(prefix, origin);
    },
  }
}

/// Concatenate `overlay` onto `existing`, dropping entries already
/// present so a repeated layer (or a diamond `extends`) cannot
/// duplicate a Chrome flag or an extension path.
/// Returns whether this overlay actually added anything, so a layer that
/// only repeats entries an earlier one already supplied is not reported
/// as a contributor.
fn append_unique(existing: &mut Value, overlay: &Value) -> bool {
  let (Some(target), Some(items)) = (existing.as_array_mut(), overlay.as_array()) else {
    *existing = overlay.clone();
    return true;
  };
  let mut added = false;
  for item in items {
    if !target.contains(item) {
      target.push(item.clone());
      added = true;
    }
  }
  added
}

/// Record provenance for every leaf under a freshly inserted subtree.
fn record_leaves(value: &Value, prefix: &str, origin: &Origin, provenance: &mut Provenance) {
  match value {
    Value::Object(map) if !map.is_empty() => {
      for (key, child) in map {
        let path = if prefix.is_empty() {
          key.clone()
        } else {
          format!("{prefix}.{key}")
        };
        record_leaves(child, &path, origin, provenance);
      }
    },
    _ => {
      // The FIRST layer to supply an additive array is a contributor too.
      // Recording only the later layers that appended to it left a
      // two-layer `chromeArgs` looking like it came from one file.
      let leaf = prefix.rsplit('.').next().unwrap_or(prefix);
      if value.is_array() && APPEND_KEYS.contains(&leaf) {
        provenance.contributed(prefix, origin);
      }
      provenance.win(prefix, origin);
    },
  }
}

/// Apply `FERRIDRIVER_<SECTION>__<KEY>` overrides.
///
/// `__` separates nesting levels and a single `_` inside a segment is
/// a camelCase boundary, so
/// `FERRIDRIVER_MCP__BROWSER__INSTANCE_ARGS_COMMAND` sets
/// `mcp.browser.instanceArgsCommand`. Values parse as JSON when they
/// can (`true`, `7`, `["--flag"]`) and stay strings otherwise.
///
/// A name with no `__` is NOT a config key: the legacy single-segment
/// variables (`FERRIDRIVER_WORKERS`, `FERRIDRIVER_BACKEND`, ...) keep
/// their existing meaning in the test runner, and `FERRIDRIVER_CONFIG`
/// / `FERRIDRIVER_NO_INHERIT` / `FERRIDRIVER_DEBUG` are loader
/// controls. The two top-level path keys are the documented exception.
fn apply_env(
  env: &BTreeMap<String, String>,
  document: &mut Value,
  provenance: &mut Provenance,
  warnings: &mut Vec<ConfigWarning>,
) {
  const TOP_LEVEL: &[(&str, &str)] = &[
    ("FERRIDRIVER_SCRIPT_ROOT", "scriptRoot"),
    ("FERRIDRIVER_ARTIFACTS_ROOT", "artifactsRoot"),
  ];

  for (name, raw) in env {
    let Some(rest) = name.strip_prefix("FERRIDRIVER_") else {
      continue;
    };
    let dotted = if rest.contains("__") {
      rest.split("__").map(camel_case_segment).collect::<Vec<_>>().join(".")
    } else if let Some((_, key)) = TOP_LEVEL.iter().find(|(var, _)| var == name) {
      (*key).to_string()
    } else {
      continue;
    };

    if dotted.split('.').any(str::is_empty) {
      warnings.push(ConfigWarning {
        source: format!("${name}"),
        message: "empty key segment (check for a stray `__`)".to_string(),
      });
      continue;
    }

    let value = parse_env_value(raw);
    let overlay = nest(&dotted, value);
    merge(document, &overlay, "", &Origin::Env(name.clone()), provenance);
  }
}

/// `INSTANCE_ARGS_COMMAND` -> `instanceArgsCommand`.
fn camel_case_segment(segment: &str) -> String {
  let mut out = String::with_capacity(segment.len());
  for (idx, word) in segment.split('_').filter(|w| !w.is_empty()).enumerate() {
    let lower = word.to_ascii_lowercase();
    if idx == 0 {
      out.push_str(&lower);
      continue;
    }
    let mut chars = lower.chars();
    if let Some(first) = chars.next() {
      out.extend(first.to_uppercase());
      out.push_str(chars.as_str());
    }
  }
  out
}

fn parse_env_value(raw: &str) -> Value {
  serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// Build `{a: {b: value}}` from `"a.b"`.
fn nest(dotted: &str, value: Value) -> Value {
  let mut segments: Vec<&str> = dotted.split('.').collect();
  let mut current = value;
  while let Some(key) = segments.pop() {
    let mut map = Map::new();
    map.insert(key.to_string(), current);
    current = Value::Object(map);
  }
  current
}

/// Deserialize the merged document, collecting keys no field claims.
///
/// # Errors
///
/// Returns an error when the document does not match the schema (a
/// wrong type, or an invalid enum variant such as an unknown backend).
/// Sections an extension may not contribute defaults for, and why.
///
/// Every one of them decides how the contributing package itself was
/// found, compiled or trusted — a default that changed them would have
/// had to take effect before the package that set it was read.
const REFUSED_DEFAULT_SECTIONS: &[(&str, &str)] = &[
  (
    "extensions",
    "the set of extensions is resolved before any of them runs",
  ),
  (
    "bundler",
    "the bundler compiled this package before it could ask for a different one",
  ),
  (
    "scripting",
    "the sandbox an extension runs under is the operator's to set, never the package's",
  ),
];

/// The one key inside a permitted section that is refused for the same
/// reason: the module-alias table is read by the first bundle, which is
/// the extraction that produced this contribution.
const REFUSED_DEFAULT_KEYS: &[(&str, &str)] = &[(
  "test.moduleAliases",
  "the alias table is sealed by the first bundle, which is the one that read this package",
)];

/// Check one `defineDefaults` payload before it is merged.
///
/// Strict where a config FILE is lenient: an unknown key in a file is a
/// warning the author can see, while a contributed one comes from a
/// dependency and would silently do nothing. It is checked against the
/// same schema every layer is checked against rather than against a
/// hand-written mirror, which is the only way the two cannot drift.
fn check_contributed_defaults(package: &str, value: &Value) -> anyhow::Result<()> {
  let Some(map) = value.as_object() else {
    anyhow::bail!("extension {package}: defineDefaults() takes a configuration object");
  };
  for (section, why) in REFUSED_DEFAULT_SECTIONS {
    if map.contains_key(*section) {
      anyhow::bail!(
        "extension {package}: defineDefaults() may not set `{section}` — {why}. Ask the operator to \
         put it in the config file instead."
      );
    }
  }
  for (key, why) in REFUSED_DEFAULT_KEYS {
    let mut node = value;
    let mut present = true;
    for segment in key.split('.') {
      if let Some(next) = node.get(segment) {
        node = next;
      } else {
        present = false;
        break;
      }
    }
    if present {
      anyhow::bail!(
        "extension {package}: defineDefaults() may not set `{key}` — {why}. Ask the operator to put \
         it in the config file instead."
      );
    }
  }

  let mut normalized = value.clone();
  normalize(&mut normalized);
  let mut ignored = Vec::new();
  let _: FerridriverConfig = serde_ignored::deserialize(&normalized, |path| ignored.push(path.to_string()))
    .map_err(|e| anyhow::anyhow!("extension {package}: defineDefaults() payload is invalid: {e}"))?;
  if let Some(key) = ignored.first() {
    anyhow::bail!(
      "extension {package}: defineDefaults() sets unknown key `{key}`{}",
      suggestion_for(key)
    );
  }
  Ok(())
}

fn deserialize_document(document: &Value, warnings: &mut Vec<ConfigWarning>) -> anyhow::Result<FerridriverConfig> {
  let mut ignored = Vec::new();
  let config: FerridriverConfig = serde_ignored::deserialize(document, |path| ignored.push(path.to_string()))
    .map_err(|e| anyhow::anyhow!("invalid config: {e}"))?;

  for key in ignored {
    warnings.push(ConfigWarning {
      source: "merged config".to_string(),
      message: format!("unknown key `{key}` is ignored{}", suggestion_for(&key)),
    });
  }

  Ok(config)
}

/// Point a misspelled key at the closest known spelling, when there is
/// an obvious one. Only the `snake_case`/`camelCase` confusion is worth
/// naming — it is the mistake the old `[mcp]` section actively invited.
fn suggestion_for(key: &str) -> String {
  let leaf = key.rsplit('.').next().unwrap_or(key);
  if !leaf.contains('_') {
    return String::new();
  }
  let camel = camel_case_segment(leaf);
  if camel == leaf {
    return String::new();
  }
  format!(" (did you mean `{camel}`?)")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn camel_case_segment_joins_words() {
    assert_eq!(camel_case_segment("BROWSER"), "browser");
    assert_eq!(camel_case_segment("INSTANCE_ARGS_COMMAND"), "instanceArgsCommand");
    assert_eq!(camel_case_segment("MCP"), "mcp");
  }

  #[test]
  fn nest_builds_nested_objects() {
    let v = nest("a.b.c", Value::Bool(true));
    assert_eq!(v["a"]["b"]["c"], Value::Bool(true));
  }

  #[test]
  fn parse_env_value_prefers_json() {
    assert_eq!(parse_env_value("true"), Value::Bool(true));
    assert_eq!(parse_env_value("7"), Value::from(7));
    assert_eq!(parse_env_value(r#"["--a"]"#), Value::from(vec!["--a"]));
    assert_eq!(parse_env_value("cdp-raw"), Value::String("cdp-raw".into()));
  }

  #[test]
  fn normalize_path_removes_dot_segments() {
    assert_eq!(normalize_path(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
  }

  #[test]
  fn normalize_path_cannot_climb_past_the_root() {
    // A source map's `../`-heavy entry joined onto a shallow cwd
    // produces more `..` than there are components. The OS reads `/..`
    // as `/`; keeping the `..` made the result compare as a prefix of
    // nothing, which is how a multi-project run came to report "no
    // tests matched" for a testDir outside the working directory.
    assert_eq!(
      normalize_path(Path::new("/one/two/../../../../tmp/specs/a.ts")),
      PathBuf::from("/tmp/specs/a.ts")
    );
    // A relative path still keeps the `..` it needs.
    assert_eq!(normalize_path(Path::new("a/../../b")), PathBuf::from("../b"));
  }

  #[test]
  fn anchored_leaves_absolute_tilde_and_templates_alone() {
    let dir = Path::new("/base");
    assert_eq!(anchored(dir, "./x.ts").as_deref(), Some("/base/x.ts"));
    assert_eq!(anchored(dir, "x.ts").as_deref(), Some("/base/x.ts"));
    assert!(anchored(dir, "/abs/x.ts").is_none());
    assert!(anchored(dir, "~/x.ts").is_none());
    assert!(anchored(dir, "/tmp/${INSTANCE}/profile").is_none());
    assert!(anchored(dir, "profiles/${INSTANCE}").is_none());
  }

  #[test]
  fn append_unique_concatenates_without_duplicates() {
    let mut base = Value::from(vec!["--a", "--b"]);
    append_unique(&mut base, &Value::from(vec!["--b", "--c"]));
    assert_eq!(base, Value::from(vec!["--a", "--b", "--c"]));
  }

  #[test]
  fn normalize_promotes_extensions_shorthand() {
    let mut v = serde_json::json!({ "extensions": ["./a.ts"] });
    normalize(&mut v);
    assert_eq!(v, serde_json::json!({ "extensions": { "paths": ["./a.ts"] } }));
  }

  #[test]
  fn suggestion_names_the_camel_case_spelling() {
    assert!(suggestion_for("mcp.browser.chrome_args").contains("chromeArgs"));
    assert_eq!(suggestion_for("mcp.browser.nonsense"), "");
  }
}
