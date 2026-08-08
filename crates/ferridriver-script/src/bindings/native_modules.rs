//! Native ES modules: the `ferridriver` / `@cucumber/cucumber` runtime
//! surface (and the node-compat modules) as Rust [`ModuleDef`]s, served
//! by the QuickJS module loader — no generated JS glue, no bundled
//! source. Bundles (rolldown) mark these specifiers EXTERNAL, so the
//! emitted chunk keeps the bare `import ... from 'ferridriver'` and the
//! written bytecode re-links by NAME against whatever runtime loads it
//! (covered end-to-end by `tests/node_compat_modules.rs`). QuickJS
//! resolves the module graph EAGERLY at declare time, so the throwaway
//! compile runtimes must register the same names.
//!
//! Export semantics intentionally mirror the deleted JS glue: values
//! are read from the installed globals ONCE at module evaluation
//! (per-session), so `import { page } from 'ferridriver'` observes the
//! session-initial binding exactly as before.

use std::sync::{Arc, RwLock};

use rquickjs::loader::{BuiltinResolver, Loader};
use rquickjs::module::{Declarations, Exports, ModuleDef};
use rquickjs::{Ctx, Module, Object, Value};

/// Every specifier served natively. One list so the engine loaders, the
/// throwaway compile runtimes, and the rolldown externals can never
/// drift apart.
pub const NATIVE_MODULE_NAMES: &[&str] = &[
  "ferridriver",
  "@ferridriver/test",
  "@cucumber/cucumber",
  "fs",
  "node:fs",
  "path",
  "node:path",
  "buffer",
  "node:buffer",
];

/// Extra specifiers the native loader answers, each mapped onto one of
/// [`NATIVE_MODULE_NAMES`]. Configured via `[test].moduleAliases`, this
/// is what lets an UNMODIFIED upstream suite keep its own framework
/// import (`@playwright/test`) and still link against the native test
/// surface.
///
/// Process-global for the same reason as
/// [`crate::bundle::set_bundler_shims`]: the resolver, the throwaway
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
    if NATIVE_MODULE_NAMES.contains(&from.as_str()) {
      return Err(format!(
        "module alias `{from}`: cannot alias a specifier the runtime already serves natively"
      ));
    }
    if !NATIVE_MODULE_NAMES.contains(&to.as_str()) {
      return Err(format!(
        "module alias `{from}` -> `{to}`: `{to}` is not a native module (expected one of {})",
        NATIVE_MODULE_NAMES.join(", ")
      ));
    }
  }
  *MODULE_ALIASES
    .write()
    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(list));
  Ok(())
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
  if NATIVE_MODULE_NAMES.contains(&specifier) {
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
  let mut h = std::collections::hash_map::DefaultHasher::new();
  for (from, to) in module_aliases().iter() {
    from.hash(&mut h);
    to.hash(&mut h);
  }
  h.finish()
}

/// Resolver accepting exactly the native specifiers plus the configured
/// aliases (non-consuming).
#[must_use]
pub fn resolver() -> BuiltinResolver {
  let mut r = BuiltinResolver::default();
  for name in NATIVE_MODULE_NAMES {
    r.add_module(*name);
  }
  for (from, _) in module_aliases().iter() {
    r.add_module(from.as_str());
  }
  r
}

type DeclareFn = for<'js> fn(Ctx<'js>, Vec<u8>) -> rquickjs::Result<Module<'js>>;

/// Non-consuming native module loader. `rquickjs::loader::ModuleLoader`
/// REMOVES an entry on first load, which breaks the second context on a
/// shared runtime (and any future re-link); QuickJS only calls the
/// loader once per name per context, but the loader itself should not
/// be single-shot.
pub struct NativeModuleLoader {
  modules: Vec<(&'static str, DeclareFn)>,
}

impl NativeModuleLoader {
  fn declare_fn<D: ModuleDef>() -> DeclareFn {
    |ctx, name| Module::declare_def::<D, _>(ctx, name)
  }
}

#[must_use]
pub fn loader() -> NativeModuleLoader {
  NativeModuleLoader {
    modules: vec![
      ("ferridriver", NativeModuleLoader::declare_fn::<FerridriverModule>()),
      (
        "@ferridriver/test",
        NativeModuleLoader::declare_fn::<FerridriverTestModule>(),
      ),
      ("@cucumber/cucumber", NativeModuleLoader::declare_fn::<CucumberModule>()),
      ("fs", NativeModuleLoader::declare_fn::<FsModule>()),
      ("node:fs", NativeModuleLoader::declare_fn::<FsModule>()),
      ("path", NativeModuleLoader::declare_fn::<PathModule>()),
      ("node:path", NativeModuleLoader::declare_fn::<PathModule>()),
      ("buffer", NativeModuleLoader::declare_fn::<BufferModule>()),
      ("node:buffer", NativeModuleLoader::declare_fn::<BufferModule>()),
    ],
  }
}

impl Loader for NativeModuleLoader {
  fn load<'js>(
    &mut self,
    ctx: &Ctx<'js>,
    path: &str,
    _attributes: Option<rquickjs::loader::ImportAttributes<'js>>,
  ) -> rquickjs::Result<Module<'js>> {
    // An aliased specifier declares the SAME `ModuleDef` under its own
    // name, so `import ... from '@playwright/test'` links to exactly the
    // module `import ... from '@ferridriver/test'` would.
    let canonical = canonical_native_name(path).ok_or_else(|| rquickjs::Error::new_loading(path))?;
    let declare = self
      .modules
      .iter()
      .find(|(name, _)| *name == canonical)
      .map(|(_, f)| *f)
      .ok_or_else(|| rquickjs::Error::new_loading(path))?;
    declare(ctx.clone(), Vec::from(path))
  }
}

/// Read a property off `globalThis` (undefined when not installed —
/// same as the old glue's `globalThis.page`).
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
pub fn namespace<'js>(ctx: &Ctx<'js>, specifier: &str) -> rquickjs::Result<Option<Object<'js>>> {
  let Some(canonical) = canonical_native_name(specifier) else {
    return Ok(None);
  };
  let ns = match canonical.as_str() {
    "ferridriver" => ferridriver_namespace(ctx)?,
    "@ferridriver/test" => test_namespace(ctx)?,
    "@cucumber/cucumber" => cucumber_namespace(ctx)?,
    "fs" | "node:fs" => fs_namespace(ctx)?,
    "path" | "node:path" => path_namespace(ctx)?,
    "buffer" | "node:buffer" => buffer_namespace(ctx)?,
    _ => return Ok(None),
  };
  Ok(Some(ns))
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

/// Install `globalThis.require` for the native specifiers only.
///
/// A `.spec.js` written as CommonJS (`const { test } = require('…')`)
/// is bundled by rolldown into an `__require("…")` call for any EXTERNAL
/// specifier, and rolldown's helper defers to a global `require` when
/// one exists (`rolldown/src/runtime/runtime-tail.js`). Without this the
/// spec dies at load with "in an environment that doesn't expose the
/// `require` function". Anything the runtime does not serve natively
/// throws — this is a bridge for the framework surface, not a general
/// CommonJS loader.
///
/// # Errors
///
/// When the global cannot be installed.
pub fn install_require<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<()> {
  let require = rquickjs::Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, specifier: String| -> rquickjs::Result<Object<'js>> {
      match namespace(&ctx, &specifier)? {
        Some(ns) => Ok(ns),
        None => Err(rquickjs::Exception::throw_type(
          &ctx,
          &format!(
            "require('{specifier}') is not available: only the runtime's native modules ({}) can be require()d",
            NATIVE_MODULE_NAMES.join(", ")
          ),
        )),
      }
    },
  )?;
  ctx.globals().set("require", require)
}

/// `import ... from 'ferridriver'` — the framework surface.
pub struct FerridriverModule;

const FERRIDRIVER_EXPORTS: &[&str] = &[
  "default",
  "ferridriver",
  "host",
  "tool",
  "defineTool",
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
  for name in [
    "host",
    "tool",
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
/// Playwright-shaped test-runner surface. `test`/`describe` live on the
/// `ferridriver` global object (installed only for
/// `ExtensionHost::Test` sessions); under any other host they evaluate
/// to `undefined`, so importing is harmless and calling gives a plain
/// TypeError.
pub struct FerridriverTestModule;

const TEST_EXPORTS: &[&str] = &["default", "test", "describe", "expect"];

fn test_namespace<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let test = fd_prop(ctx, "test")?;
  let describe = fd_prop(ctx, "describe")?;
  let expect = global(ctx, "expect")?;
  let default = Object::new(ctx.clone())?;
  default.set("test", test.clone())?;
  default.set("describe", describe.clone())?;
  default.set("expect", expect.clone())?;
  let ns = Object::new(ctx.clone())?;
  ns.set("default", default)?;
  ns.set("test", test)?;
  ns.set("describe", describe)?;
  ns.set("expect", expect)?;
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

/// `import fs from 'node:fs'` — re-exports the sandboxed `fs` global's
/// API, plus a `promises` namespace alias so both `fs.readFile` and
/// `fs.promises.readFile` work. Reads come in both shapes (Node's
/// `readFileSync` alongside the promise form); writes and directory
/// listing stay async-only.
pub struct FsModule;

const FS_MEMBERS: &[&str] = &[
  "readFile",
  "readFileBytes",
  "readFileSync",
  "readFileBytesSync",
  "existsSync",
  "writeFile",
  "readdir",
  "exists",
  "root",
];
const FS_EXPORTS: &[&str] = &[
  "default",
  "promises",
  "readFile",
  "readFileBytes",
  "readFileSync",
  "readFileBytesSync",
  "existsSync",
  "writeFile",
  "readdir",
  "exists",
  "root",
];

fn fs_namespace<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let fs = global(ctx, "fs")?.into_object();
  // Fresh module object so `fs.promises.readFile` works off the default
  // export (Node shape) without mutating the `fs` global.
  let module = Object::new(ctx.clone())?;
  let ns = Object::new(ctx.clone())?;
  for name in FS_MEMBERS {
    let v: Value<'js> = match &fs {
      Some(o) => o.get(*name)?,
      None => Value::new_undefined(ctx.clone()),
    };
    module.set(*name, v.clone())?;
    ns.set(*name, v)?;
  }
  module.set("promises", module.clone())?;
  ns.set("promises", module.clone())?;
  ns.set("default", module)?;
  Ok(ns)
}

impl ModuleDef for FsModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    declare_all(decl, FS_EXPORTS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    export_from(exports, &fs_namespace(ctx)?, FS_EXPORTS)
  }
}

/// `import path from 'node:path'` — pure-computation POSIX-style subset
/// (the sandbox is always a unix-style path space).
pub struct PathModule;

const PATH_MEMBERS: &[&str] = &[
  "join",
  "resolve",
  "dirname",
  "basename",
  "extname",
  "normalize",
  "relative",
  "isAbsolute",
  "sep",
  "delimiter",
];
const PATH_EXPORTS: &[&str] = &[
  "default",
  "join",
  "resolve",
  "dirname",
  "basename",
  "extname",
  "normalize",
  "relative",
  "isAbsolute",
  "sep",
  "delimiter",
];

fn path_namespace<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let obj = crate::bindings::node_compat::path_object(ctx)?;
  let ns = Object::new(ctx.clone())?;
  ns.set("default", obj.clone())?;
  for name in PATH_MEMBERS {
    ns.set(*name, obj.get::<_, Value<'js>>(*name)?)?;
  }
  Ok(ns)
}

impl ModuleDef for PathModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    declare_all(decl, PATH_EXPORTS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    export_from(exports, &path_namespace(ctx)?, PATH_EXPORTS)
  }
}

/// `import { Buffer } from 'node:buffer'` — the documented [`crate::bindings::node_compat::BufferJs`]
/// subset.
pub struct BufferModule;

const BUFFER_EXPORTS: &[&str] = &["default", "Buffer"];

fn buffer_namespace<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let ctor = crate::bindings::node_compat::buffer_constructor(ctx)?;
  let default = Object::new(ctx.clone())?;
  default.set("Buffer", ctor.clone())?;
  let ns = Object::new(ctx.clone())?;
  ns.set("default", default)?;
  ns.set("Buffer", ctor)?;
  Ok(ns)
}

impl ModuleDef for BufferModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    declare_all(decl, BUFFER_EXPORTS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    export_from(exports, &buffer_namespace(ctx)?, BUFFER_EXPORTS)
  }
}
