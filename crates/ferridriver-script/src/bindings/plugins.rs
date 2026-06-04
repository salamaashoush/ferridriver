//! Native plugin surface.
//!
//! A plugin/extension file is rolldown-bundled to `QuickJS` bytecode
//! once at startup. Loading + evaluating that bytecode in a session runs
//! its top-level `tool(...)` / `defineTool(...)` (and any `Given/When/Then`) calls,
//! registering directly into the shared Rust `ExtensionRegistry`.
//! `tool` is the canonical tool-registration surface — no
//! `globalThis.exports`, no legacy shapes.
//!
//! There is **no synthesized JS and no `globalThis.__*`**: the
//! `tools.<name>` callable is a native Rust closure that restores the
//! handler from the registry, builds `{ args, page, context, request,
//! commands }` with the Object API, applies the handler and returns its
//! promise — the exact mechanism BDD steps use (`invoke_step`). The
//! `commands` binding and the `allow.net` host guard are native Rust
//! (`PluginCommandsJs`, `HttpClientJs::with_net`); the allow-list
//! is checked in Rust before any shell/network I/O.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use rquickjs::function::Opt;
use rquickjs::promise::{MaybePromise, Promised};
use rquickjs::{Ctx, Function, IntoJs, JsLifetime, Module, Object, Value, class::Class, class::Trace};

use super::bdd::{tool_dispatch, tool_names};
use super::http_client::HttpClientJs;
use crate::bindings::convert::{json_to_js, serde_from_js};
use crate::command_spec::CommandSpec;
use crate::engine::SessionProcsUd;
use crate::error::ScriptError;
use crate::session_procs::{self, SessionProcs};

/// One plugin file handed to the engine at `install_plugins` time:
/// just its precompiled bytecode. Tool names + capabilities are read
/// from the manifest the module registers, not carried here.
#[derive(Debug, Clone)]
pub struct PluginBinding {
  /// Precompiled `QuickJS` bytecode of the rolldown-bundled module,
  /// produced once at startup by
  /// [`crate::bundle::compile_and_extract_plugins`]. `Module::load`ed
  /// per session — no per-session parse, no source retained.
  pub bytecode: Arc<[u8]>,
}

/// The `commands` object a plugin handler receives. Holds this tool's
/// declared command set (default-deny — a handler cannot reach a name
/// its manifest did not declare, nor another tool's) plus the session's
/// durable persistent-process registry.
///
/// - `run(name, vars?)` — one-shot: resolve `${vars}` strictly, execute
///   (argv or `sh -c` per the spec), bounded by timeout + output cap,
///   shaped per the declared output mode.
/// - `start(name, vars?)` / `status(name)` / `stop(name)` — persistent:
///   manage a long-running process whose lifetime is the session's.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "PluginCommands")]
pub struct PluginCommandsJs {
  #[qjs(skip_trace)]
  allowed: Arc<BTreeMap<String, CommandSpec>>,
  #[qjs(skip_trace)]
  procs: Option<Arc<SessionProcs>>,
}

impl PluginCommandsJs {
  pub(crate) fn new(allowed: Arc<BTreeMap<String, CommandSpec>>, procs: Option<Arc<SessionProcs>>) -> Self {
    Self { allowed, procs }
  }

  fn cmd_err(verb: &'static str, msg: impl std::fmt::Display) -> rquickjs::Error {
    rquickjs::Error::new_from_js_message(verb, "Error", msg.to_string())
  }

  fn spec(&self, verb: &'static str, name: &str) -> rquickjs::Result<CommandSpec> {
    self.allowed.get(name).cloned().ok_or_else(|| {
      Self::cmd_err(
        verb,
        format!("\"{name}\" is not in the commands allow-list for this tool"),
      )
    })
  }

  fn vars_of<'js>(ctx: &Ctx<'js>, vars: Opt<Value<'js>>) -> rquickjs::Result<BTreeMap<String, serde_json::Value>> {
    match vars.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => serde_from_js(ctx, v),
      _ => Ok(BTreeMap::new()),
    }
  }

  fn registry(&self, verb: &'static str) -> rquickjs::Result<&Arc<SessionProcs>> {
    self
      .procs
      .as_ref()
      .ok_or_else(|| Self::cmd_err(verb, "persistent commands are unavailable in this context"))
  }
}

#[rquickjs::methods]
impl PluginCommandsJs {
  /// One-shot: run to completion and return shaped stdout.
  #[qjs(rename = "run")]
  pub async fn run<'js>(&self, ctx: Ctx<'js>, name: String, vars: Opt<Value<'js>>) -> rquickjs::Result<Value<'js>> {
    let spec = self.spec("commands.run", &name)?;
    let vars_map = Self::vars_of(&ctx, vars)?;
    let resolved = spec
      .resolve(&vars_map)
      .map_err(|m| Self::cmd_err("commands.run", format!("{name}: {m}")))?;
    let value = Box::pin(session_procs::run_oneshot(&resolved))
      .await
      .map_err(|m| Self::cmd_err("commands.run", format!("{name}: {m}")))?;
    json_to_js(&ctx, &value)
  }

  /// Persistent: start (idempotent if already running). Returns
  /// `{ name, pid }`.
  #[qjs(rename = "start")]
  pub fn start<'js>(&self, ctx: Ctx<'js>, name: String, vars: Opt<Value<'js>>) -> rquickjs::Result<Value<'js>> {
    let spec = self.spec("commands.start", &name)?;
    let vars_map = Self::vars_of(&ctx, vars)?;
    let resolved = spec
      .resolve(&vars_map)
      .map_err(|m| Self::cmd_err("commands.start", format!("{name}: {m}")))?;
    let pid = self
      .registry("commands.start")?
      .start(&name, &resolved)
      .map_err(|m| Self::cmd_err("commands.start", format!("{name}: {m}")))?;
    json_to_js(&ctx, &serde_json::json!({ "name": name, "pid": pid }))
  }

  /// Persistent: running?/exit code + the buffered stdout/stderr tail.
  #[qjs(rename = "status")]
  pub fn status<'js>(&self, ctx: Ctx<'js>, name: String) -> rquickjs::Result<Value<'js>> {
    let value = self
      .registry("commands.status")?
      .status(&name)
      .map_err(|m| Self::cmd_err("commands.status", m))?;
    json_to_js(&ctx, &value)
  }

  /// Persistent: kill the process group.
  #[qjs(rename = "stop")]
  pub fn stop(&self, name: String) -> rquickjs::Result<()> {
    self
      .registry("commands.stop")?
      .stop(&name)
      .map_err(|m| Self::cmd_err("commands.stop", m))
  }
}

fn rq(e: &ScriptError) -> rquickjs::Error {
  rquickjs::Error::new_from_js_message("plugins", "Error", e.message.clone())
}

/// Install loaded plugins: load+evaluate each file's bytecode (which
/// registers its tools into the shared registry, native or legacy
/// shape), then expose every registered tool as a native
/// `tools.<name>` callable.
pub fn install_plugins(ctx: &Ctx<'_>, files: &[PluginBinding]) -> rquickjs::Result<()> {
  for file in files {
    // SAFETY: `file.bytecode` was produced by `Module::write` in THIS
    // process and this exact rquickjs/QuickJS build with native
    // endianness (see `compile_and_extract_plugins`) and is never
    // persisted — the precondition `Module::load` documents.
    #[allow(unsafe_code)]
    let module = unsafe { Module::load(ctx.clone(), &file.bytecode) }?;
    // Evaluating the module runs its top-level `tool(...)` /
    // `Given(...)` calls, registering directly into the extension
    // registry. No `globalThis.exports`, no post-eval ingest.
    let (_evaluated, _promise) = module.eval()?;
  }

  let names = tool_names(ctx).map_err(|e| rq(&e))?;
  let tools_obj = Object::new(ctx.clone())?;
  let mut created_global_roots = BTreeSet::new();
  for (idx, name) in names.into_iter().enumerate() {
    // The closure forwards into a generic fn so `Ctx`/`Value`/return
    // share one `'js` (an inline closure with `<'_>` would give each its
    // own lifetime and `Function::call`'s result could not be returned).
    let f = Function::new(ctx.clone(), move |ctx, call_args| dispatch_tool(ctx, idx, call_args))?;
    tools_obj.set(name.as_str(), f.clone())?;
    install_tool_namespace(ctx, &tools_obj, &name, f, &mut created_global_roots)?;
  }
  ctx.globals().set("tools", tools_obj)?;
  crate::bindings::runtime::mirror_global(ctx, "tools")?;
  Ok(())
}

fn install_tool_namespace<'js>(
  ctx: &Ctx<'js>,
  tools_obj: &Object<'js>,
  name: &str,
  f: Function<'js>,
  created_global_roots: &mut BTreeSet<String>,
) -> rquickjs::Result<()> {
  let parts: Vec<&str> = name.split('.').collect();
  if parts.len() < 2 || parts.iter().any(|p| !is_js_identifier(p)) {
    return Ok(());
  }

  set_nested(ctx, tools_obj, &parts, f.clone().into_value())?;

  let globals = ctx.globals();
  let root_name = parts[0];
  if globals.contains_key(root_name)? {
    if created_global_roots.contains(root_name)
      && let Ok(root) = globals.get::<_, Object<'js>>(root_name)
    {
      set_nested(ctx, &root, &parts[1..], f)?;
    }
    return Ok(());
  }

  let root = Object::new(ctx.clone())?;
  globals.set(root_name, root.clone())?;
  created_global_roots.insert(root_name.to_string());
  set_nested(ctx, &root, &parts[1..], f)
}

fn set_nested<'js, V>(ctx: &Ctx<'js>, root: &Object<'js>, parts: &[&str], value: V) -> rquickjs::Result<()>
where
  V: IntoJs<'js>,
{
  if parts.is_empty() {
    return Ok(());
  }

  let mut current = root.clone();
  for part in &parts[..parts.len() - 1] {
    let next = match current.get::<_, Object<'js>>(*part) {
      Ok(obj) => obj,
      Err(_) => {
        let obj = Object::new(ctx.clone())?;
        current.set(*part, obj.clone())?;
        obj
      },
    };
    current = next;
  }
  current.set(*parts.last().expect("non-empty parts"), value)
}

fn is_js_identifier(part: &str) -> bool {
  let mut chars = part.chars();
  let Some(first) = chars.next() else {
    return false;
  };
  if !(first == '_' || first == '$' || first.is_ascii_alphabetic()) {
    return false;
  }
  chars.all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
}

/// Native `tools.<name>(args)` body: restore the tool's handler from
/// the registry, build `{ args, page, context, request, commands }` via
/// the Object API (per-tool `commands` allow-list + optional net-guarded
/// `request`, both Rust-enforced), apply the handler and await its
/// result. When the manifest declared `timeoutMs`, the handler is raced
/// against that bound natively (same mechanism `invoke_step` uses) so
/// every caller — promoted MCP tool, `invoke_plugin`, or another
/// extension calling `tools.<name>` — is covered, not just the MCP
/// entry point. Returns a JS promise; the caller `await`s it. No
/// synthesized JS.
fn dispatch_tool<'js>(
  ctx: Ctx<'js>,
  idx: usize,
  call_args: Opt<Value<'js>>,
) -> Promised<impl std::future::Future<Output = rquickjs::Result<Value<'js>>> + 'js> {
  Promised::from(async move {
    let d = tool_dispatch(&ctx, idx).map_err(|e| rq(&e))?;

    let arg = Object::new(ctx.clone())?;
    let undef = Value::new_undefined(ctx.clone());
    arg.set("args", call_args.0.unwrap_or_else(|| undef.clone()))?;

    let g = ctx.globals();
    arg.set("page", g.get::<_, Value<'js>>("page").unwrap_or_else(|_| undef.clone()))?;
    arg.set(
      "context",
      g.get::<_, Value<'js>>("context").unwrap_or_else(|_| undef.clone()),
    )?;

    // The tool's declared `allow.net` (empty ⇒ unrestricted). Used for
    // BOTH the net-guarded `request` wrapper AND the `fetch` policy
    // bracket below — one allow-list, both HTTP entry points.
    let net_policy: Option<Arc<[String]>> = if d.allowed_net.is_empty() {
      None
    } else {
      Some(Arc::from(d.allowed_net.as_slice()))
    };

    // `request`: pass through unless the tool declared `allow.net`, in
    // which case hand it a net-restricted wrapper over the SAME underlying
    // context (host check enforced natively in `HttpClientJs`).
    let req_val: Value<'js> = g.get("request").unwrap_or_else(|_| undef.clone());
    let request_out: Value<'js> = match net_policy.clone() {
      Some(net) => match Class::<HttpClientJs>::from_value(&req_val) {
        Ok(cls) => {
          let inner = cls.borrow().inner_arc();
          let guarded = Class::instance(ctx.clone(), HttpClientJs::with_net(inner, net))?;
          guarded.into_js(&ctx)?
        },
        Err(_) => req_val,
      },
      None => req_val,
    };
    arg.set("request", request_out)?;

    let procs = ctx.userdata::<SessionProcsUd>().map(|u| u.0.clone());
    let commands = Class::instance(ctx.clone(), PluginCommandsJs::new(Arc::new(d.allowed_commands), procs))?;
    arg.set("commands", commands)?;

    // The same `allow.net` must also bind the global `fetch` (a facade
    // over the same core). `fetch` reads the active policy from VM
    // userdata; bracket every poll of THIS handler's future so the cell
    // holds this tool's list whenever its continuation runs and is
    // restored to the caller's value otherwise — correct under nesting
    // (a tool calling `tools.other`) and concurrent interleaving
    // (`Promise.all([tools.a(), tools.b()])`) because the swap and
    // the synchronous `fetch` guard both run within a single poll on the
    // single QuickJS thread.
    let policy_cell = ctx
      .userdata::<crate::bindings::fetch::NetPolicyUd>()
      .map(|u| u.0.clone());

    let handler = d.handler;
    let timeout_ms = d.timeout_ms;
    let inner = async move {
      let mp: MaybePromise<'js> = handler.call((arg,))?;
      let fut = mp.into_future::<Value<'js>>();
      match timeout_ms {
        Some(t) => match tokio::time::timeout(Duration::from_millis(t), fut).await {
          Ok(r) => r,
          Err(_) => Err(rquickjs::Error::new_from_js_message(
            "plugins",
            "Error",
            format!("tool timed out after {t}ms"),
          )),
        },
        None => fut.await,
      }
    };

    match policy_cell {
      None => inner.await,
      Some(cell) => {
        let mut inner = std::pin::pin!(inner);
        std::future::poll_fn(move |cx2| {
          let prev = cell.swap(net_policy.clone());
          let r = inner.as_mut().poll(cx2);
          cell.swap(prev);
          r
        })
        .await
      },
    }
  })
}
