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

use rquickjs::loader::{BuiltinResolver, Loader};
use rquickjs::module::{Declarations, Exports, ModuleDef};
use rquickjs::{Ctx, Module, Object, Value};

/// Every specifier served natively. One list so the engine loaders, the
/// throwaway compile runtimes, and the rolldown externals can never
/// drift apart.
pub const NATIVE_MODULE_NAMES: &[&str] = &[
  "ferridriver",
  "@cucumber/cucumber",
  "fs",
  "node:fs",
  "path",
  "node:path",
  "buffer",
  "node:buffer",
];

/// Resolver accepting exactly the native specifiers (non-consuming).
#[must_use]
pub fn resolver() -> BuiltinResolver {
  let mut r = BuiltinResolver::default();
  for name in NATIVE_MODULE_NAMES {
    r.add_module(*name);
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
  fn load<'js>(&mut self, ctx: &Ctx<'js>, path: &str) -> rquickjs::Result<Module<'js>> {
    let declare = self
      .modules
      .iter()
      .find(|(name, _)| *name == path)
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

/// `import ... from 'ferridriver'` — the framework surface.
pub struct FerridriverModule;

const FERRIDRIVER_EXPORTS: &[&str] = &[
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

impl ModuleDef for FerridriverModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    decl.declare("default")?;
    for name in FERRIDRIVER_EXPORTS {
      decl.declare(*name)?;
    }
    Ok(())
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let fd: Value<'js> = global(ctx, "ferridriver")?;
    exports.export("default", fd.clone())?;
    exports.export("ferridriver", fd)?;
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
      exports.export(name, fd_prop(ctx, name)?)?;
    }
    exports.export("defineTool", fd_prop(ctx, "tool")?)?;
    for name in [
      "page", "context", "browser", "request", "expect", "chromium", "firefox", "webkit",
    ] {
      exports.export(name, global(ctx, name)?)?;
    }
    Ok(())
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

impl ModuleDef for CucumberModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    for name in CUCUMBER_EXPORTS {
      decl.declare(*name)?;
    }
    Ok(())
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let bdd: Value<'js> = fd_prop(ctx, "bdd")?;
    let bdd_obj = bdd.into_object();
    for name in CUCUMBER_EXPORTS {
      let v = match &bdd_obj {
        Some(o) => o.get(*name)?,
        None => Value::new_undefined(ctx.clone()),
      };
      exports.export(*name, v)?;
    }
    Ok(())
  }
}

/// `import fs from 'node:fs'` — re-exports the sandboxed `fs` global's
/// promise-shaped API, plus a `promises` namespace alias so both
/// `fs.readFile` and `fs.promises.readFile` work. No sync API (the
/// sandbox is async-only); packages calling `readFileSync` get a plain
/// undefined-is-not-a-function, same as any unsupported builtin.
pub struct FsModule;

const FS_EXPORTS: &[&str] = &["readFile", "readFileBytes", "writeFile", "readdir", "exists", "root"];

impl ModuleDef for FsModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    decl.declare("default")?;
    decl.declare("promises")?;
    for name in FS_EXPORTS {
      decl.declare(*name)?;
    }
    Ok(())
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let fs = global(ctx, "fs")?.into_object();
    // Fresh module object so `fs.promises.readFile` works off the
    // default export (Node shape) without mutating the `fs` global.
    let module = Object::new(ctx.clone())?;
    for name in FS_EXPORTS {
      let v = match &fs {
        Some(o) => o.get::<_, Value<'js>>(*name)?,
        None => Value::new_undefined(ctx.clone()),
      };
      module.set(*name, v.clone())?;
      exports.export(*name, v)?;
    }
    module.set("promises", module.clone())?;
    exports.export("promises", module.clone())?;
    exports.export("default", module)?;
    Ok(())
  }
}

/// `import path from 'node:path'` — pure-computation POSIX-style subset
/// (the sandbox is always a unix-style path space).
pub struct PathModule;

impl ModuleDef for PathModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    for name in [
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
    ] {
      decl.declare(name)?;
    }
    Ok(())
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let obj = crate::bindings::node_compat::path_object(ctx)?;
    exports.export("default", obj.clone())?;
    for name in [
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
    ] {
      exports.export(name, obj.get::<_, Value<'js>>(name)?)?;
    }
    Ok(())
  }
}

/// `import { Buffer } from 'node:buffer'` — the documented [`crate::bindings::node_compat::BufferJs`]
/// subset.
pub struct BufferModule;

impl ModuleDef for BufferModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    decl.declare("default")?;
    decl.declare("Buffer")?;
    Ok(())
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let ctor = crate::bindings::node_compat::buffer_constructor(ctx)?;
    let default = Object::new(ctx.clone())?;
    default.set("Buffer", ctor.clone())?;
    exports.export("default", default)?;
    exports.export("Buffer", ctor)?;
    Ok(())
  }
}
