//! The Node / web modules this crate serves, and the one table a host
//! registers them from.
//!
//! Each entry carries its specifiers, the `ModuleDef` the ES loader
//! declares, and the object `require()` hands back — so a host cannot
//! wire up the import form and forget the CommonJS one, and cannot serve
//! a module this crate does not know about.

use rquickjs::module::{Declarations, Exports, ModuleDef};
use rquickjs::{Ctx, Module, Object, Value};

/// Declare `names` on a module.
fn declare_all(decl: &Declarations<'_>, names: &[&str]) -> rquickjs::Result<()> {
  for name in names {
    decl.declare(*name)?;
  }
  Ok(())
}

/// Copy `names` from a namespace object into the module's ES exports.
fn export_from<'js>(exports: &Exports<'js>, ns: &Object<'js>, names: &[&str]) -> rquickjs::Result<()> {
  for name in names {
    exports.export(*name, ns.get::<_, Value<'js>>(*name)?)?;
  }
  Ok(())
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
  let obj = crate::node::path::path_object(ctx)?;
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

/// `import { createHash } from 'node:crypto'` — the vendored llrt crypto
/// module. `require('crypto')` reads the same members off the `crypto`
/// global the runtime installs.
pub use crate::crypto::CryptoModule;

const CRYPTO_MEMBERS: &[&str] = &[
  "createHash",
  "createHmac",
  "getRandomValues",
  "randomBytes",
  "randomFill",
  "randomFillSync",
  "randomInt",
  "randomUUID",
  "subtle",
  "webcrypto",
];

fn crypto_namespace<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let global: Object<'js> = ctx.globals().get("crypto")?;
  let ns = Object::new(ctx.clone())?;
  for name in CRYPTO_MEMBERS {
    if let Ok(value) = global.get::<_, Value<'js>>(*name) {
      if !value.is_undefined() {
        ns.set(*name, value)?;
      }
    }
  }
  ns.set("webcrypto", global)?;
  Ok(ns)
}

/// `import { Buffer } from 'node:buffer'` — the vendored llrt `Buffer`,
/// which subclasses `Uint8Array`.
pub use crate::buffer::BufferModule;

/// `require('buffer')`: the same members the ES module exports, read off
/// the globals the runtime installed.
fn buffer_namespace<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let ns = Object::new(ctx.clone())?;
  for name in ["Buffer", "atob", "btoa"] {
    if let Ok(value) = ctx.globals().get::<_, Value<'js>>(name) {
      if !value.is_undefined() {
        ns.set(name, value)?;
      }
    }
  }
  let constants = Object::new(ctx.clone())?;
  constants.set("MAX_LENGTH", u32::MAX)?;
  constants.set("MAX_STRING_LENGTH", (1_u32 << 30) - 1)?;
  ns.set("constants", constants)?;
  Ok(ns)
}

/// `import os from 'node:os'` — host introspection, served by the
/// vendored `llrt_os` (see `ferridriver-jsstd`).
pub struct OsModule;

const OS_MEMBERS: &[&str] = &[
  "arch",
  "availableParallelism",
  "cpus",
  "devNull",
  "endianness",
  "EOL",
  "freemem",
  "getPriority",
  "homedir",
  "hostname",
  "loadavg",
  "machine",
  "networkInterfaces",
  "platform",
  "release",
  "setPriority",
  "tmpdir",
  "totalmem",
  "type",
  "uptime",
  "userInfo",
  "version",
];

fn os_namespace<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  crate::os::os_object(ctx)
}

impl ModuleDef for OsModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    decl.declare("default")?;
    declare_all(decl, OS_MEMBERS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let object = os_namespace(ctx)?;
    export_node_module(ctx, exports, &object, OS_MEMBERS)
  }
}

/// Export a node module: `default` is the module object, and every member
/// it actually carries is a named export. Members are read off the object
/// rather than assumed, because some depend on globals a given host does
/// not install.
fn export_node_module<'js>(
  ctx: &Ctx<'js>,
  exports: &Exports<'js>,
  object: &Object<'js>,
  members: &[&str],
) -> rquickjs::Result<()> {
  exports.export("default", object.clone())?;
  for name in members {
    let value = object
      .get::<_, Value<'js>>(*name)
      .unwrap_or_else(|_| Value::new_undefined(ctx.clone()));
    exports.export(*name, value)?;
  }
  Ok(())
}

/// `import util from 'node:util'` — formatting, the promise/callback
/// wrappers and `util.types`.
pub struct UtilModule;

impl ModuleDef for UtilModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    decl.declare("default")?;
    declare_all(decl, crate::node::util::UTIL_MEMBERS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let object = crate::node::util::util_object(ctx)?;
    export_node_module(ctx, exports, &object, crate::node::util::UTIL_MEMBERS)
  }
}

/// `import assert from 'node:assert'`, and its always-strict twin.
pub struct AssertModule;
/// `import assert from 'node:assert/strict'`.
pub struct AssertStrictModule;

impl ModuleDef for AssertModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    decl.declare("default")?;
    declare_all(decl, crate::node::assert::ASSERT_MEMBERS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let object = crate::node::assert::assert_object(ctx, false)?;
    export_node_module(ctx, exports, &object, crate::node::assert::ASSERT_MEMBERS)
  }
}

impl ModuleDef for AssertStrictModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    decl.declare("default")?;
    declare_all(decl, crate::node::assert::ASSERT_MEMBERS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let object = crate::node::assert::assert_object(ctx, true)?;
    export_node_module(ctx, exports, &object, crate::node::assert::ASSERT_MEMBERS)
  }
}

/// `import { fileURLToPath } from 'node:url'`.
pub struct UrlModule;

impl ModuleDef for UrlModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    decl.declare("default")?;
    declare_all(decl, crate::node::url::URL_MEMBERS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let object = crate::node::url::url_object(ctx)?;
    export_node_module(ctx, exports, &object, crate::node::url::URL_MEMBERS)
  }
}

/// `import process from 'node:process'` — the module form of the global.
pub struct ProcessModule;

impl ModuleDef for ProcessModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    decl.declare("default")?;
    declare_all(decl, crate::node::process::PROCESS_MEMBERS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let object = crate::node::process::process_object(ctx)?;
    export_node_module(ctx, exports, &object, crate::node::process::PROCESS_MEMBERS)
  }
}

/// `import { setTimeout } from 'node:timers'`, and the promise twin.
pub struct TimersModule;
/// `import { setTimeout } from 'node:timers/promises'`.
pub struct TimersPromisesModule;

impl ModuleDef for TimersModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    decl.declare("default")?;
    declare_all(decl, crate::node::timers::TIMERS_MEMBERS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let object = crate::node::timers::timers_object(ctx)?;
    export_node_module(ctx, exports, &object, crate::node::timers::TIMERS_MEMBERS)
  }
}

impl ModuleDef for TimersPromisesModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    decl.declare("default")?;
    declare_all(decl, crate::node::timers::TIMERS_PROMISES_MEMBERS)
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let object = crate::node::timers::timers_promises_object(ctx)?;
    export_node_module(
      ctx,
      exports,
      &object,
      crate::node::timers::TIMERS_PROMISES_MEMBERS,
    )
  }
}

/// `import { EventEmitter } from 'node:events'` — the vendored llrt
/// emitter, which the `EventTarget` globals already share.
pub struct EventsModule;

/// Key under which the per-context `EventEmitter` constructor is
/// remembered. A symbol on `globalThis`, so it stays out of
/// `Object.keys` and cannot collide with a suite's own globals.
const EVENT_EMITTER_KEY: &str = "ferridriver.node.events.EventEmitter";

fn events_namespace<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  use crate::events::{Emitter as _, EventEmitter};

  let symbol: Object<'js> = ctx.globals().get("Symbol")?;
  let symbol_for: rquickjs::Function<'js> = symbol.get("for")?;
  let key: Value<'js> = symbol_for.call((EVENT_EMITTER_KEY,))?;

  // One constructor per context, whichever path asks first: a second
  // `create_constructor` hands back a different function object, and
  // `require('events') === (await import('events')).EventEmitter` — plus
  // every `instanceof` across the two — would be false.
  if let Ok(existing) = ctx.globals().get::<_, Object<'js>>(key.clone()) {
    return Ok(existing);
  }

  let ctor = rquickjs::Class::<EventEmitter<'js>>::create_constructor(ctx)?
    .ok_or_else(|| rquickjs::Error::new_loading("events"))?;
  EventEmitter::add_event_emitter_prototype(ctx)?;
  let ctor = ctor
    .as_object()
    .cloned()
    .ok_or_else(|| rquickjs::Error::new_loading("events"))?;

  // Node's `module.exports` for this module IS the class, with the named
  // export hanging off it — so `require('events')` can be extended.
  ctor.set("EventEmitter", ctor.clone())?;
  ctx.globals().set(key, ctor.clone())?;
  Ok(ctor)
}

impl ModuleDef for EventsModule {
  fn declare(decl: &Declarations<'_>) -> rquickjs::Result<()> {
    decl.declare("default")?;
    decl.declare("EventEmitter")?;
    Ok(())
  }

  fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
    let object = events_namespace(ctx)?;
    export_node_module(ctx, exports, &object, &["EventEmitter"])
  }
}

/// How a host declares one of these modules to the ES loader.
pub type DeclareFn = for<'js> fn(Ctx<'js>, Vec<u8>) -> rquickjs::Result<Module<'js>>;

/// How a host builds the object `require('<specifier>')` returns.
pub type NamespaceFn = for<'js> fn(&Ctx<'js>) -> rquickjs::Result<Object<'js>>;

/// One module, under every specifier it answers to.
pub struct NodeModule {
  pub specifiers: &'static [&'static str],
  pub declare: DeclareFn,
  pub namespace: NamespaceFn,
}

fn declare_fn<D: ModuleDef>() -> DeclareFn {
  |ctx, name| Module::declare_def::<D, _>(ctx, name)
}

/// Every module this crate serves. A host registers all of them or none:
/// the ES loader, the `require` table and the bundler's external list all
/// read this one place.
#[must_use]
pub fn modules() -> Vec<NodeModule> {
  vec![
    NodeModule {
      specifiers: &["path", "node:path"],
      declare: declare_fn::<PathModule>(),
      namespace: path_namespace,
    },
    NodeModule {
      specifiers: &["buffer", "node:buffer"],
      declare: declare_fn::<crate::buffer::BufferModule>(),
      namespace: buffer_namespace,
    },
    NodeModule {
      specifiers: &["os", "node:os"],
      declare: declare_fn::<OsModule>(),
      namespace: os_namespace,
    },
    NodeModule {
      specifiers: &["util", "node:util"],
      declare: declare_fn::<UtilModule>(),
      namespace: |ctx| crate::node::util::util_object(ctx),
    },
    NodeModule {
      specifiers: &["events", "node:events"],
      declare: declare_fn::<EventsModule>(),
      namespace: events_namespace,
    },
    NodeModule {
      specifiers: &["assert", "node:assert"],
      declare: declare_fn::<AssertModule>(),
      namespace: |ctx| crate::node::assert::assert_object(ctx, false),
    },
    NodeModule {
      specifiers: &["assert/strict", "node:assert/strict"],
      declare: declare_fn::<AssertStrictModule>(),
      namespace: |ctx| crate::node::assert::assert_object(ctx, true),
    },
    NodeModule {
      specifiers: &["url", "node:url"],
      declare: declare_fn::<UrlModule>(),
      namespace: |ctx| crate::node::url::url_object(ctx),
    },
    NodeModule {
      specifiers: &["process", "node:process"],
      declare: declare_fn::<ProcessModule>(),
      namespace: |ctx| crate::node::process::process_object(ctx),
    },
    NodeModule {
      specifiers: &["timers", "node:timers"],
      declare: declare_fn::<TimersModule>(),
      namespace: |ctx| crate::node::timers::timers_object(ctx),
    },
    NodeModule {
      specifiers: &["timers/promises", "node:timers/promises"],
      declare: declare_fn::<TimersPromisesModule>(),
      namespace: |ctx| crate::node::timers::timers_promises_object(ctx),
    },
    NodeModule {
      specifiers: &["crypto", "node:crypto"],
      declare: declare_fn::<crate::crypto::CryptoModule>(),
      namespace: crypto_namespace,
    },
  ]
}
