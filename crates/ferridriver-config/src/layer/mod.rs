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

mod discover;
mod merge;
mod paths;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::FerridriverConfig;
use discover::discover_layers;
use merge::{apply_env, camel_case_segment, merge};
use paths::{anchor_paths, normalize, resolve_against};

pub use merge::merge_documents;
pub use paths::normalize_path;

/// Array keys that concatenate across layers instead of replacing.
/// Matched on the leaf key name at any depth.
///
/// These are the additive collections: an entry added by one layer is
/// never a statement that another layer's entries should go away.
/// Everything else (globs, reporters, projects, ...) replaces, because
/// a layer that redefines them is redefining a whole policy.
pub const APPEND_KEYS: &[&str] = &["paths", "sidecars", "chromeArgs", "args", "allowEnv"];

/// Keys a document may write outside the schema, folded into the schema
/// by `normalize` before anything deserializes.
///
/// `use` at the top of `[test]` is where Playwright puts it, and it is
/// the same bag as `[test.browser].use`; a config module writing one
/// gets the other. The authoring `.d.ts` declares the alias, so the
/// contract test reads it from here rather than carrying its own copy.
pub const DOCUMENT_ALIASES: &[&str] = &["use"];

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
pub(super) struct Provenance {
  /// Dotted key -> the origin whose value won.
  winners: BTreeMap<String, Origin>,
  /// Dotted key -> every origin that added to a concatenated array.
  contributors: BTreeMap<String, Vec<Origin>>,
}

impl Provenance {
  pub(super) fn win(&mut self, path: &str, origin: &Origin) {
    self.winners.insert(path.to_string(), origin.clone());
  }

  pub(super) fn contributed(&mut self, path: &str, origin: &Origin) {
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

/// Hand the top-level `[browser]` scalars down to the sections that launch.
///
/// `[browser]` declares the browser once for every host. Its instance registry
/// is resolved at lookup time, but `backend`, `headless` and `viewport` are
/// read straight off `[mcp.browser]` / `[test.browser]`, where each carries a
/// default of its own rather than an "unset" state — so a section can never
/// say "nobody asked" and could never defer. Copying here, on the merged DOCUMENT, is what makes the
/// distinction available at all: a key absent from the document is absent,
/// whereas a deserialized `backend: "cdp-pipe"` is indistinguishable from the
/// default the same field would have taken anyway.
///
/// Only absent keys are filled, so a section that states its own value keeps
/// it, and provenance records the top-level key as the source — `ferridriver
/// config` then shows where a host's backend actually came from.
fn apply_global_browser_defaults(document: &mut Value, provenance: &mut Provenance) {
  // `viewport` is here for the same reason, one step further: both
  // sections give it a non-null default, so a deserialized 1280x720 is
  // indistinguishable from a config that never mentioned it — and an
  // explicit `viewport: null` has to reach the section as a null, not be
  // lost as "absent", because that is how Playwright spells "no fixed
  // viewport".
  const INHERITED: [&str; 5] = ["browser", "backend", "transport", "headless", "viewport"];

  let Some(global) = document.get("browser").and_then(Value::as_object).cloned() else {
    return;
  };
  for section in ["mcp", "test"] {
    for key in INHERITED {
      let Some(value) = global.get(key) else { continue };
      let target = document
        .as_object_mut()
        .and_then(|d| {
          d.entry(section)
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
        })
        .map(|s| s.entry("browser").or_insert_with(|| Value::Object(Map::new())));
      let Some(browser) = target.and_then(Value::as_object_mut) else {
        continue;
      };
      if browser.contains_key(key) {
        continue;
      }
      if matches!(key, "backend" | "transport")
        && browser
          .get("browser")
          .is_some_and(|product| Some(product) != global.get("browser"))
      {
        continue;
      }
      browser.insert(key.to_string(), value.clone());
      // Attribute it to whatever declared the top-level key, so `ferridriver
      // config` names the file an operator would edit rather than a synthetic
      // source they cannot find.
      if let Some(origin) = provenance.winners.get(&format!("browser.{key}")).cloned() {
        provenance.win(&format!("{section}.browser.{key}"), &origin);
      }
    }
  }
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
  apply_global_browser_defaults(&mut document, &mut provenance);
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

/// Read, normalize, anchor and merge one file, after recursively
/// applying whatever it `extends`.
fn apply_layer(layer: &ConfigLayer, fold: &mut Fold<'_>) -> anyhow::Result<()> {
  let canonical = std::fs::canonicalize(&layer.path).unwrap_or_else(|_| layer.path.clone());
  if !fold.seen.insert(canonical) {
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
/// A module's default export is a whole document, as a `.toml` file's
/// contents are, so each of these can be written; each is refused by
/// name rather than ignored.
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
        "config {}: a config module may not set `{key}` — {why}. Put it in a ferridriver.toml layer \
         instead.",
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
      // YAML 1.2 booleans only: `trace: on` names a mode, not `true`.
      let mut options = serde_saphyr::Options::default();
      options.strict_booleans = true;
      serde_saphyr::from_str_with_options(&content, options)
        .map_err(|e| anyhow::anyhow!("invalid YAML config {}: {e}", path.display()))?
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

/// Deserialize the merged document, collecting keys no field claims.
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
