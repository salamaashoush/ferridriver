//! Native extension surface.
//!
//! A extension/extension file is rolldown-bundled to `QuickJS` bytecode
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
//! (`ExtensionCommandsJs`, `HttpClientJs::with_net`); the allow-list
//! is checked in Rust before any shell/network I/O.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use rquickjs::function::Opt;
use rquickjs::promise::{MaybePromise, Promised};
use rquickjs::{Ctx, Function, IntoJs, JsLifetime, Module, Object, Value, class::Class, class::Trace};

use super::http_client::HttpClientJs;
use super::registry::{tool_dispatch, tool_names};
use crate::bindings::convert::{json_to_js, serde_from_js};
use crate::command_spec::CommandSpec;
use crate::engine::SessionProcsUd;
use crate::error::ScriptError;
use crate::session_procs::{self, SessionProcs};

/// One extension file handed to the engine at `install_extensions` time:
/// its precompiled bytecode plus a display name for diagnostics. Tool
/// names + capabilities are read from the manifest the module
/// registers, not carried here.
#[derive(Debug, Clone)]
pub struct ExtensionBinding {
  /// Precompiled `QuickJS` bytecode of the rolldown-bundled module,
  /// produced once at startup by
  /// [`crate::bundle::compile_and_extract_extensions`]. `Module::load`ed
  /// per session — no per-session parse, no source retained.
  pub bytecode: Arc<[u8]>,
  /// Source identity (file path) used only in install-failure logs.
  pub name: String,
}

/// The `commands` object a extension handler receives. Holds this tool's
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
#[rquickjs::class(rename = "ExtensionCommands")]
pub struct ExtensionCommandsJs {
  #[qjs(skip_trace)]
  allowed: Arc<BTreeMap<String, CommandSpec>>,
  #[qjs(skip_trace)]
  procs: Option<Arc<SessionProcs>>,
}

impl ExtensionCommandsJs {
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
impl ExtensionCommandsJs {
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
  rquickjs::Error::new_from_js_message("extensions", "Error", e.message.clone())
}

/// Install loaded extensions: load+evaluate each file's bytecode (which
/// registers its tools into the shared registry, native or legacy
/// shape), then expose every registered tool as a native
/// `tools.<name>` callable.
///
/// Per-file isolation mirrors startup (`load_all`): one extension whose
/// top-level throws is logged and skipped — it must not take down the
/// whole session VM (and with it every `run_script` for the session).
pub async fn install_extensions(ctx: &Ctx<'_>, files: &[ExtensionBinding]) -> rquickjs::Result<()> {
  for file in files {
    if let Err(e) = install_one_extension(ctx, file).await {
      let detail = match e {
        rquickjs::Error::Exception => ctx.catch().try_into_exception().map_or_else(
          |v| format!("{v:?}"),
          |ex| ex.message().unwrap_or_else(|| "exception".into()),
        ),
        other => other.to_string(),
      };
      tracing::warn!(extension = %file.name, error = %detail, "extension install failed; skipping file");
    }
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

/// Load + fully evaluate one extension module, including its top-level
/// `await` (the extraction pass awaits the eval promise the same way —
/// `compile_extract_one` — so a tool registered after an async setup
/// step is visible here too, not only in the manifest).
async fn install_one_extension(ctx: &Ctx<'_>, file: &ExtensionBinding) -> rquickjs::Result<()> {
  // SAFETY: `file.bytecode` was produced by `Module::write` by this
  // exact rquickjs/QuickJS build with native endianness — either in
  // this process (`compile_and_extract_extensions`) or restored from the
  // bytecode disk cache, whose ABI tag (QuickJS version, arch,
  // endianness, pointer width) plus transitive input hashes guarantee
  // an ABI-identical toolchain wrote it. That satisfies the
  // same-interpreter precondition `Module::load` documents.
  #[allow(unsafe_code)]
  let module = unsafe { Module::load(ctx.clone(), &file.bytecode) }?;
  // Evaluating the module runs its top-level `tool(...)` /
  // `Given(...)` calls, registering directly into the extension
  // registry. No `globalThis.exports`, no post-eval ingest.
  let (_evaluated, promise) = module.eval()?;
  promise.into_future::<()>().await
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
  let Some((last, ancestors)) = parts.split_last() else {
    return Ok(());
  };

  let mut current = root.clone();
  for part in ancestors {
    let next = if let Ok(obj) = current.get::<_, Object<'js>>(*part) {
      obj
    } else {
      let obj = Object::new(ctx.clone())?;
      current.set(*part, obj.clone())?;
      obj
    };
    current = next;
  }
  current.set(*last, value)
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
/// every caller — promoted MCP tool, `invoke_extension_tool`, or another
/// extension calling `tools.<name>` — is covered, not just the MCP
/// entry point. Returns a JS promise; the caller `await`s it. No
/// synthesized JS.
fn dispatch_tool<'js>(
  ctx: Ctx<'js>,
  idx: usize,
  call_args: Opt<Value<'js>>,
) -> Promised<impl std::future::Future<Output = rquickjs::Result<Value<'js>>> + 'js> {
  Promised::from(run_tool(ctx, idx, call_args.0))
}

/// Shared tool-invocation body behind both entry points: the JS-visible
/// `tools.<name>` callable ([`dispatch_tool`]) and the host-side native
/// invoke ([`invoke_tool_by_name`], which backs the MCP `invoke_extension_tool`
/// path without synthesizing a script).
async fn run_tool<'js>(ctx: Ctx<'js>, idx: usize, call_args: Option<Value<'js>>) -> rquickjs::Result<Value<'js>> {
  let d = tool_dispatch(&ctx, idx).map_err(|e| rq(&e))?;

  let arg = Object::new(ctx.clone())?;
  let undef = Value::new_undefined(ctx.clone());
  arg.set("args", call_args.unwrap_or_else(|| undef.clone()))?;

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
    Some(d.allowed_net.clone())
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
  let commands = Class::instance(ctx.clone(), ExtensionCommandsJs::new(d.allowed_commands, procs))?;
  arg.set("commands", commands)?;

  // The same `allow.net` must also bind the global `fetch` and the
  // global `request` (facades over the same core). Both read the
  // active policy from VM userdata; `bracket_net` swaps the cell
  // around every poll of THIS handler's future so the list in effect
  // is whichever tool's continuation is running — correct under
  // nesting (a tool calling `tools.other`) and concurrent
  // interleaving (`Promise.all([tools.a(), tools.b()])`).
  let policy_cell = crate::bindings::fetch::policy_cell(&ctx);

  let handler = d.handler;
  let timeout_ms = d.timeout_ms;
  let inner = async move {
    let mp: MaybePromise<'js> = handler.call((arg,))?;
    let fut = mp.into_future::<Value<'js>>();
    match timeout_ms {
      Some(t) => match tokio::time::timeout(Duration::from_millis(t), fut).await {
        Ok(r) => r,
        Err(_) => Err(rquickjs::Error::new_from_js_message(
          "extensions",
          "Error",
          format!("tool timed out after {t}ms"),
        )),
      },
      None => fut.await,
    }
  };
  crate::bindings::fetch::bracket_net(policy_cell, net_policy, inner).await
}

/// Host-side native invocation of a registered tool by manifest name —
/// what the MCP server's `invoke_extension_tool` / promoted-tool routes call.
/// Skips the script pipeline entirely (no synthesized one-liner, no
/// compile): the exact `run_tool` body behind `tools.<name>` runs, so
/// capability wrappers, `timeoutMs`, and the net-policy bracket apply
/// identically for every caller.
pub async fn invoke_tool_by_name(
  ctx: &Ctx<'_>,
  name: &str,
  args: &serde_json::Value,
) -> Result<serde_json::Value, ScriptError> {
  let Some(idx) = crate::bindings::registry::tool_index_by_name(ctx, name)? else {
    return Err(ScriptError::internal(format!(
      "tool `{name}` is not installed in this session (its extension file may have failed to load — check the server log)"
    )));
  };
  let arg = json_to_js(ctx, args).map_err(|e| convert_caught(ctx, e, name))?;
  let value = run_tool(ctx.clone(), idx, Some(arg))
    .await
    .map_err(|e| convert_caught(ctx, e, name))?;
  Ok(crate::engine::value_to_json(ctx, value).unwrap_or(serde_json::Value::Null))
}

fn convert_caught(ctx: &Ctx<'_>, e: rquickjs::Error, name: &str) -> ScriptError {
  crate::engine::caught_to_script_error(rquickjs::CaughtError::from_error(ctx, e), name)
}
