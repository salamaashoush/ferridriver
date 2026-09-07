//! Native ES modules: the `ferridriver` / `@cucumber/cucumber` runtime
//! surface as Rust [`ModuleDef`]s, registered into the runtime's module
//! table beside the Node modules the standard library serves. Bundles
//! mark these specifiers EXTERNAL, so the emitted chunk keeps the bare
//! `import ... from 'ferridriver'` and the written bytecode re-links by
//! NAME against whatever realm loads it. QuickJS resolves the module
//! graph EAGERLY at declare time, so the compile realms read the same
//! table.
//!
//! Export semantics intentionally mirror the deleted JS glue: values
//! are read from the installed globals ONCE at module evaluation
//! (per-session), so `import { page } from 'ferridriver'` observes the
//! session-initial binding exactly as before.
//!
//! The alias table (`[test].moduleAliases`) stays process-global: the
//! bundler, the config validation and every session read it, and a
//! session created before an alias arrived could never honour it, so
//! the table seals on first read.

use std::sync::{Arc, RwLock};

use ferrijs::modules::{ModuleRegistry, NativeModule};
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::{Declarations, Exports, ModuleDef};
use rquickjs::{Ctx, Module, Object, Value};

/// The specifiers ferridriver itself serves. Everything else native comes
/// from the standard library's own table.
const FERRIDRIVER_MODULE_NAMES: &[&str] = &[
  "ferridriver",
  "@ferridriver/test",
  // The canonical Playwright import specifiers, served by the same
  // module: a suite that imports `@playwright/test` links against the
  // native test surface with no config, no alias entry and no edit to
  // its own source. Parity belongs to the binary, not to a setting.
  "@playwright/test",
  "playwright/test",
  "@cucumber/cucumber",
];

/// Specifiers ferridriver serves that are ONE module under several
/// names. The standard library's own table carries the same information for the Node
/// modules (`url` / `node:url`, ...).
const FERRIDRIVER_SPECIFIER_GROUPS: &[&[&str]] = &[&["@ferridriver/test", "@playwright/test", "playwright/test"]];

/// Specifier namespaces no package may claim, beyond the ones the
/// runtime already serves.
///
/// A claim on any of these would let whichever extension happened to be
/// loaded — and in whatever order — decide what `@playwright/test` or
/// `node:fs` means. The bare twin of every reserved `node:` name is
/// reserved with it, because a package claiming `fs` while the runtime
/// serves `node:fs` is the same hijack spelled differently.
const RESERVED_PREFIXES: &[&str] = &["node:", "@ferridriver/", "@playwright/", "@cucumber/"];
const RESERVED_NAMES: &[&str] = &["playwright", "playwright-core", "ferridriver"];

/// Whether `specifier` is off-limits to a package's `provides` claims.
#[must_use]
pub fn is_reserved_specifier(specifier: &str) -> bool {
  if native_module_names().contains(&specifier) {
    return true;
  }
  if RESERVED_NAMES.contains(&specifier) || RESERVED_PREFIXES.iter().any(|p| specifier.starts_with(p)) {
    return true;
  }
  // `fs` when the runtime serves `node:fs`.
  native_module_names()
    .iter()
    .any(|name| name.strip_prefix("node:") == Some(specifier))
}

/// The canonical name of the module a specifier resolves to, so two
/// spellings of one module compare equal.
fn namespace_group(specifier: &str) -> String {
  if let Some(group) = FERRIDRIVER_SPECIFIER_GROUPS
    .iter()
    .find(|group| group.contains(&specifier))
  {
    return group[0].to_string();
  }
  ferrijs::std::modules::modules()
    .into_iter()
    .find(|module| module.specifiers.contains(&specifier))
    .map_or_else(|| specifier.to_string(), |module| module.specifiers[0].to_string())
}

/// Every specifier served natively, ferridriver's own plus the standard
/// library's.
pub fn native_module_names() -> Vec<&'static str> {
  let mut names: Vec<&'static str> = FERRIDRIVER_MODULE_NAMES.to_vec();
  for module in ferrijs::std::modules::modules() {
    names.extend_from_slice(module.specifiers);
  }
  names
}

/// Extra specifiers the native loader answers, each mapped onto one of
/// [`native_module_names`]. Configured via `[test].moduleAliases`, this
/// is what lets an UNMODIFIED upstream suite keep its own framework
/// import (`@playwright/test`) and still link against the native test
/// surface.
///
/// Process-global for the same reason as
/// [`crate::bundle::set_bundler_env`]: the resolver, the throwaway
/// compile runtimes and the rolldown externals all consult it from call
/// sites spread across three crates.
/// Import specifier -> native module name, as configured by
/// `[test.moduleAliases]`.
type AliasTable = Arc<Vec<(String, String)>>;

static MODULE_ALIASES: RwLock<Option<AliasTable>> = RwLock::new(None);

/// Install the alias map (replacing any previous one). Must be called
/// before anything bundles or creates a session.
///
/// # Errors
///
/// When an alias would shadow a native specifier, or its target is not
/// a native module.
pub fn set_module_aliases(aliases: impl IntoIterator<Item = (String, String)>) -> Result<(), String> {
  let list: Vec<(String, String)> = aliases.into_iter().collect();
  for (from, to) in &list {
    if native_module_names().contains(&from.as_str()) {
      // Redundant rather than wrong: `@playwright/test` was an alias
      // target before the runtime served it natively, and a config that
      // still spells it out must keep working. Only an alias that would
      // REDIRECT a native specifier somewhere else is an error.
      if canonical_native_name(from).as_deref() == Some(from.as_str()) && namespace_group(from) == namespace_group(to) {
        continue;
      }
      return Err(format!(
        "module alias `{from}`: cannot alias a specifier the runtime already serves natively"
      ));
    }
    if !native_module_names().contains(&to.as_str()) {
      return Err(format!(
        "module alias `{from}` -> `{to}`: `{to}` is not a native module (expected one of {})",
        native_module_names().join(", ")
      ));
    }
  }
  if ALIASES_SEALED.load(std::sync::atomic::Ordering::Acquire) {
    // Re-stating what the table already says is a no-op, and the hosts
    // do it (startup installs the operator's table, a subcommand later
    // re-installs the same one after resolving its own config). Only a
    // genuine addition is refused.
    let table = module_aliases();
    let late: Vec<&str> = list
      .iter()
      .filter(|(from, to)| !table.iter().any(|(f, t)| f == from && t == to))
      .map(|(from, _)| from.as_str())
      .collect();
    if late.is_empty() {
      return Ok(());
    }
    return Err(format!(
      "module aliases are sealed: `{}` arrived after the first resolver was built, \
       so a session created earlier would keep resolving without it",
      late.join("`, `")
    ));
  }

  // MERGE, never replace: the table is fed from more than one place —
  // the operator's `[test].moduleAliases`, the `--module-alias` flag,
  // and (later) the specifiers a package provides. A whole-table write
  // silently dropped whichever arrived first.
  let mut guard = MODULE_ALIASES
    .write()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  let mut merged: Vec<(String, String)> = guard.as_ref().map(|a| (**a).clone()).unwrap_or_default();
  for (from, to) in list {
    match merged.iter_mut().find(|(existing, _)| *existing == from) {
      Some(entry) if entry.1 == to => {},
      Some(entry) => {
        tracing::warn!(
          target: "ferridriver::script",
          specifier = %from,
          previous = %entry.1,
          replacement = %to,
          "module.alias.replaced: two sources claim the same specifier; the later one wins"
        );
        entry.1 = to;
      },
      None => merged.push((from, to)),
    }
  }
  *guard = Some(Arc::new(merged));
  Ok(())
}

/// One-way latch: set the first time anything reads the table to build a
/// resolver or a cache key. After that a new alias cannot be honoured —
/// a session created before it would keep resolving without it, and a
/// bundle keyed before it would be served from the wrong slot — so the
/// attempt is an error rather than a table that means two things.
static ALIASES_SEALED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn seal_aliases() {
  ALIASES_SEALED.store(true, std::sync::atomic::Ordering::Release);
}

/// Whether the alias table has been read by a resolver / cache key yet.
#[must_use]
pub fn aliases_sealed() -> bool {
  ALIASES_SEALED.load(std::sync::atomic::Ordering::Acquire)
}

#[must_use]
pub fn module_aliases() -> Arc<Vec<(String, String)>> {
  MODULE_ALIASES
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone()
    .unwrap_or_default()
}

/// Canonical native name for a specifier: itself when it is native, the
/// alias target when it is aliased, otherwise `None`.
#[must_use]
pub fn canonical_native_name(specifier: &str) -> Option<String> {
  if native_module_names().contains(&specifier) {
    return Some(specifier.to_string());
  }
  module_aliases()
    .iter()
    .find(|(from, _)| from == specifier)
    .map(|(_, to)| to.clone())
}

/// True when the specifier is served natively (directly or via alias) —
/// the single predicate the rolldown externals check.
#[must_use]
pub fn is_native_specifier(specifier: &str) -> bool {
  canonical_native_name(specifier).is_some()
}

/// Stable fingerprint of the alias map, folded into every bundle cache
/// key: adding or removing an alias flips a specifier between "external
/// bare import" and "resolved into the chunk", which changes the output
/// for byte-identical inputs.
#[must_use]
pub fn alias_fingerprint() -> u64 {
  use std::hash::{Hash, Hasher};
  // Sorted: the table is merged from several sources, so declaration
  // order varies between runs that mean exactly the same thing — and an
  // order-sensitive fingerprint would invalidate every cached bundle.
  // Reading it seals the table (a bundle keyed now must not be served
  // under a table that changes afterwards).
  seal_aliases();
  let mut entries: Vec<(String, String)> = (*module_aliases()).clone();
  entries.sort();
  let mut h = std::collections::hash_map::DefaultHasher::new();
  for (from, to) in &entries {
    from.hash(&mut h);
    to.hash(&mut h);
  }
  h.finish()
}

/// Register ferridriver's modules and the configured aliases into the
/// runtime's table. Reading the alias table seals it: a realm built now
/// must not see a table that changes afterwards.
///
/// # Errors
///
/// A specifier already served, which cannot happen for the fixed names
/// here and is reported rather than ignored for an alias.
pub fn register(registry: &mut ModuleRegistry) -> Result<(), String> {
  seal_aliases();
  registry.register(NativeModule::new::<FerridriverModule, _>(
    ["ferridriver"],
    ferridriver_namespace,
  ))?;
  registry.register(NativeModule::new::<FerridriverTestModule, _>(
    ["@ferridriver/test", "@playwright/test", "playwright/test"],
    test_namespace,
  ))?;
  registry.register(NativeModule::new::<CucumberModule, _>(
    ["@cucumber/cucumber"],
    cucumber_namespace,
  ))?;
  for prefix in RESERVED_PREFIXES {
    registry.reserve_prefix(*prefix);
  }
  for name in RESERVED_NAMES {
    registry.reserve_name(*name);
  }
  for (from, to) in module_aliases().iter() {
    // An alias that re-states a native name is redundant rather than
    // wrong (see `set_module_aliases`), and the table already refused
    // anything else.
    if registry.serves(from) {
      continue;
    }
    registry.alias(from.clone(), to.clone())?;
  }
  Ok(())
}

/// The module table a compile realm needs: the standard library plus
/// ferridriver's modules and aliases, exactly what a session realm is
/// built over.
///
/// # Errors
///
/// See [`register`].
pub fn registry() -> Result<Arc<ModuleRegistry>, String> {
  let mut registry = ModuleRegistry::with_std();
  register(&mut registry)?;
  Ok(Arc::new(registry))
}

/// The specifiers packages serve, as the bundler's externals.
#[must_use]
pub fn provided_externals() -> Vec<String> {
  crate::provided_modules::provided_modules()
    .specifiers()
    .into_iter()
    .map(ToString::to_string)
    .collect()
}

/// The loader pair for package-provided specifiers, chained after the
/// runtime's native table.
///
/// The provider's own bytecode is loaded under the specifier's name, so
/// accepting it here is all that stands between an importer and the one
/// module instance QuickJS already holds -- no facade, no re-export. An
/// alias answers with its target's name, which is what keeps `x` and
/// `x/sub` one module rather than two copies of the provider's state.
/// The loader IS reached in the throwaway compile realms, which only
/// ever `declare` -- linking happens at eval -- so an empty module is
/// enough to let a consumer's import resolve while it is being compiled.
#[must_use]
pub fn provided_loaders() -> Vec<(ferrijs::modules::BoxResolver, ferrijs::modules::BoxLoader)> {
  vec![(Box::new(ProvidedResolver), Box::new(ProvidedLoader))]
}

struct ProvidedResolver;

impl Resolver for ProvidedResolver {
  fn resolve<'js>(
    &mut self,
    _ctx: &Ctx<'js>,
    base: &str,
    name: &str,
    _attributes: Option<ImportAttributes<'js>>,
  ) -> rquickjs::Result<String> {
    crate::provided_modules::canonical_provided_name(name).ok_or_else(|| rquickjs::Error::new_resolving(base, name))
  }
}

struct ProvidedLoader;

impl Loader for ProvidedLoader {
  fn load<'js>(
    &mut self,
    ctx: &Ctx<'js>,
    path: &str,
    _attributes: Option<ImportAttributes<'js>>,
  ) -> rquickjs::Result<Module<'js>> {
    if crate::provided_modules::is_provided_specifier(path) {
      return Module::declare(ctx.clone(), path, "export {};\n");
    }
    Err(rquickjs::Error::new_loading(path))
  }
}

/// What `require()` answers ahead of the runtime's table: a
/// package-provided specifier answers with the very module `import`
/// links to, so `require` and `import` cannot see different objects;
/// an alias resolves as a builtin.
pub struct FerridriverRequire;

impl ferrijs::RequireHook for FerridriverRequire {
  fn namespace<'js>(&self, ctx: &Ctx<'js>, specifier: &str) -> rquickjs::Result<Option<Object<'js>>> {
    Ok(provided_namespace(ctx, specifier))
  }

  fn is_builtin(&self, specifier: &str) -> bool {
    module_aliases().iter().any(|(from, _)| from == specifier)
  }
}

/// Read a property off `globalThis` (undefined when not installed —
/// same as the old glue's `globalThis.page`).
/// The `devices` object for this VM, built once.
///
/// Playwright's `devices` is ONE object shared by `playwright` and
/// `@playwright/test` (`test.mjs` re-exports `playwright.devices`), so
/// `require('playwright').devices === require('@playwright/test').devices`
/// holds there and has to hold here.
struct DevicesObject(rquickjs::Persistent<Object<'static>>);

// SAFETY: holds only a `Persistent`, which is lifetime-erased by
// construction — the same rationale as the registry userdata.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for DevicesObject {
  type Changed<'to> = DevicesObject;
}

/// `devices['iPhone 15']` — Playwright's device registry.
///
/// Parsed from the vendored source with the engine's own JSON parser
/// rather than assembled property by property: 207 descriptors is ~2000
/// property writes across the boundary, and the parse gives an object
/// with exactly upstream's keys, `screen` included.
fn devices_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
  if let Some(ud) = ctx.userdata::<DevicesObject>()
    && let Ok(obj) = ud.0.clone().restore(ctx)
  {
    return Ok(obj.into_value());
  }
  let parsed: Value<'js> = ctx.json_parse(ferridriver::devices::SOURCE)?;
  if let Some(obj) = parsed.as_object() {
    let _ = ctx.store_userdata(DevicesObject(rquickjs::Persistent::save(ctx, obj.clone())));
  }
  Ok(parsed)
}

fn global<'js>(ctx: &Ctx<'js>, name: &str) -> rquickjs::Result<Value<'js>> {
  ctx.globals().get(name)
}

/// Read a property off the `ferridriver` global object; undefined when
/// either level is missing.
fn fd_prop<'js>(ctx: &Ctx<'js>, name: &str) -> rquickjs::Result<Value<'js>> {
  match ctx.globals().get::<_, Option<Object<'js>>>("ferridriver")? {
    Some(fd) => fd.get(name),
    None => Ok(Value::new_undefined(ctx.clone())),
  }
}

/// A native module's exports as one plain object. Single source of
/// truth for BOTH the ESM `evaluate` path and the synchronous
/// CommonJS [`install_require`] path, so `require('…')` can never see a
/// different surface from `import … from '…'`.
///
/// # Errors
///
/// Propagates the underlying property reads; `None` for a specifier no
/// native module serves.
/// Namespaces of the package-provided modules this VM has evaluated,
/// keyed by specifier. What `require('<specifier>')` answers, since it
/// is synchronous and cannot await a dynamic import.
struct ProvidedNamespaces(
  std::cell::RefCell<std::collections::BTreeMap<String, rquickjs::Persistent<Object<'static>>>>,
);

// SAFETY: holds only `Persistent` values, which are lifetime-erased by
// construction — the same rationale as the registry userdata.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for ProvidedNamespaces {
  type Changed<'to> = ProvidedNamespaces;
}

/// Record a provider's namespace after its module evaluated.
pub fn remember_provided_namespace<'js>(ctx: &Ctx<'js>, specifier: &str, namespace: &Object<'js>) {
  if ctx.userdata::<ProvidedNamespaces>().is_none() {
    let _ = ctx.store_userdata(ProvidedNamespaces(std::cell::RefCell::new(
      std::collections::BTreeMap::new(),
    )));
  }
  if let Some(ud) = ctx.userdata::<ProvidedNamespaces>() {
    ud.0.borrow_mut().insert(
      specifier.to_string(),
      rquickjs::Persistent::save(ctx, namespace.clone()),
    );
  }
}

/// The namespace of an evaluated package-provided module, if this VM has
/// one. Aliases normalise to their target first, so `x` and `x/sub`
/// answer with the same object `import` would give.
fn provided_namespace<'js>(ctx: &Ctx<'js>, specifier: &str) -> Option<Object<'js>> {
  let canonical = crate::provided_modules::canonical_provided_name(specifier)?;
  let ud = ctx.userdata::<ProvidedNamespaces>()?;
  let saved = ud.0.borrow().get(&canonical).cloned()?;
  saved.restore(ctx).ok()
}

/// Copy `names` from a namespace object into the module's ES exports.
fn export_from<'js>(exports: &Exports<'js>, ns: &Object<'js>, names: &[&str]) -> rquickjs::Result<()> {
  for name in names {
    exports.export(*name, ns.get::<_, Value<'js>>(*name)?)?;
  }
  Ok(())
}

/// Declare `names` on a module.
fn declare_all(decl: &Declarations<'_>, names: &[&str]) -> rquickjs::Result<()> {
  for name in names {
    decl.declare(*name)?;
  }
  Ok(())
}

/// `import ... from 'ferridriver'` — the framework surface.
pub struct FerridriverModule;

const FERRIDRIVER_EXPORTS: &[&str] = &[
  "default",
  "ferridriver",
  "devices",
  "host",
  "tool",
  "defineTool",
  "defineFixtures",
  "defineDefaults",
  "bindSteps",
  "test",
  "describe",
  "mergeTests",
  "bdd",
  "commands",
  "tools",
  "fs",
  "vars",
  "sidecars",
  "artifacts",
  "page",
  "context",
  "browser",
  "request",
  "expect",
  "chromium",
  "firefox",
  "webkit",
];

fn ferridriver_namespace<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let ns = Object::new(ctx.clone())?;
  let fd: Value<'js> = global(ctx, "ferridriver")?;
  ns.set("default", fd.clone())?;
  ns.set("ferridriver", fd)?;
  ns.set("devices", devices_object(ctx)?)?;
  for name in [
    "host",
    "tool",
    "defineFixtures",
    "defineDefaults",
    "test",
    "describe",
    "mergeTests",
    "bdd",
    "commands",
    "tools",
    "fs",
    "vars",
    "sidecars",
    "artifacts",
  ] {
    ns.set(name, fd_prop(ctx, name)?)?;
  }
  // `bindSteps` hangs off `ferridriver.bdd` beside the cucumber
  // registrars; the module exports it flat because that is how a
  // package writes `import { bindSteps } from 'ferridriver'`.
  ns.set(
    "bindSteps",
    fd_prop(ctx, "bdd")?
      .into_object()
      .map_or_else(|| Ok(Value::new_undefined(ctx.clone())), |o| o.get("bindSteps"))?,
  )?;
  ns.set("defineTool", fd_prop(ctx, "tool")?)?;
  for name in [
    "page", "context", "browser", "request", "expect", "chromium", "firefox", "webkit",
  ] {
    ns.set(name, global(ctx, name)?)?;
  }
  Ok(ns)
}

impl ModuleDef for FerridriverModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    declare_all(decl, FERRIDRIVER_EXPORTS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    export_from(exports, &ferridriver_namespace(ctx)?, FERRIDRIVER_EXPORTS)
  }
}

/// `import { test, describe, expect } from '@ferridriver/test'` — the
/// Playwright-shaped test-runner surface, served under `@playwright/test`
/// and `playwright/test` too so an unmodified suite links against it.
/// The surface exists under every host; only `ferridriver test` collects
/// what registering leaves behind.
pub struct FerridriverTestModule;

/// Mirrors `@playwright/test`'s own export list (`packages/playwright/
/// test.mjs`), minus what ferridriver has not implemented yet:
/// `errors`, `by` and the
/// `_electron` / `_android` / `_utilityTest` internals.
/// Exporting a name that resolves to `undefined` would be worse than not
/// exporting it — the import succeeds and the call site fails somewhere
/// else.
const TEST_EXPORTS: &[&str] = &[
  "default",
  "test",
  "describe",
  "expect",
  "mergeExpects",
  "mergeTests",
  "defineConfig",
  "devices",
  "selectors",
  "_baseTest",
  "chromium",
  "firefox",
  "webkit",
  "request",
];

/// The module object IS the `test` function, carrying every named export
/// as a property — Playwright's `module.exports = Object.assign(test,
/// exports)` — so `import test, { expect } from …`,
/// `import { test } from …` and `const { test } = require(…)` all work,
/// as does `require('@playwright/test')(…)`.
fn test_namespace<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let test = fd_prop(ctx, "test")?;
  let members: Vec<(&str, Value<'js>)> = vec![
    ("test", test.clone()),
    ("describe", fd_prop(ctx, "describe")?),
    ("expect", global(ctx, "expect")?),
    ("mergeExpects", global(ctx, "mergeExpects")?),
    ("mergeTests", fd_prop(ctx, "mergeTests")?),
    ("defineConfig", fd_prop(ctx, "defineConfig")?),
    ("devices", devices_object(ctx)?),
    ("selectors", fd_prop(ctx, "selectors")?),
    ("_baseTest", fd_prop(ctx, "baseTest")?),
    ("chromium", global(ctx, "chromium")?),
    ("firefox", global(ctx, "firefox")?),
    ("webkit", global(ctx, "webkit")?),
    ("request", global(ctx, "request")?),
  ];
  let ns = test.as_object().cloned().unwrap_or(Object::new(ctx.clone())?);
  for (name, value) in members {
    ns.set(name, value)?;
  }
  ns.set("default", test)?;
  Ok(ns)
}

impl ModuleDef for FerridriverTestModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    declare_all(decl, TEST_EXPORTS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    export_from(exports, &test_namespace(ctx)?, TEST_EXPORTS)
  }
}

/// `import { Given } from '@cucumber/cucumber'` — the registration
/// surface, read off `ferridriver.bdd` (the same native functions the
/// globals expose).
pub struct CucumberModule;

const CUCUMBER_EXPORTS: &[&str] = &[
  "Given",
  "When",
  "Then",
  "defineStep",
  "And",
  "But",
  "Before",
  "After",
  "BeforeAll",
  "AfterAll",
  "BeforeStep",
  "AfterStep",
  "defineParameterType",
  "setDefaultTimeout",
  "setDefinitionFunctionWrapper",
  "setWorldConstructor",
  "setParallelCanAssign",
];

fn cucumber_namespace<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let bdd_obj = fd_prop(ctx, "bdd")?.into_object();
  let ns = Object::new(ctx.clone())?;
  for name in CUCUMBER_EXPORTS {
    let v: Value<'js> = match &bdd_obj {
      Some(o) => o.get(*name)?,
      None => Value::new_undefined(ctx.clone()),
    };
    ns.set(*name, v)?;
  }
  Ok(ns)
}

impl ModuleDef for CucumberModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    declare_all(decl, CUCUMBER_EXPORTS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    export_from(exports, &cucumber_namespace(ctx)?, CUCUMBER_EXPORTS)
  }
}
