//! Native plugin surface.
//!
//! A plugin/extension file is rolldown-bundled to `QuickJS` bytecode
//! once at startup. Loading + evaluating that bytecode in a session runs
//! its top-level `defineTool(...)` (and any `Given/When/Then`) calls,
//! registering directly into the shared Rust `ExtensionRegistry`.
//! `defineTool` is the only tool-registration surface — no
//! `globalThis.exports`, no legacy shapes.
//!
//! There is **no synthesized JS and no `globalThis.__*`**: the
//! `plugins.<name>` callable is a native Rust closure that restores the
//! handler from the registry, builds `{ args, page, context, request,
//! commands }` with the Object API, applies the handler and returns its
//! promise — the exact mechanism BDD steps use (`invoke_step`). The
//! `commands` binding and the `allow.net` host guard are native Rust
//! (`PluginCommandsJs`, `APIRequestContextJs::with_net`); the allow-list
//! is checked in Rust before any shell/network I/O.

use std::collections::BTreeMap;
use std::process::Command;
use std::sync::Arc;

use rquickjs::function::{Func, Opt};
use rquickjs::{Ctx, IntoJs, JsLifetime, Module, Object, Value, class::Class, class::Trace};

use super::api_request::APIRequestContextJs;
use super::bdd::{tool_dispatch, tool_names};
use crate::bindings::convert::{serde_from_js, serde_to_js};
use crate::error::ScriptError;

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

/// The `commands` object a plugin handler receives. Holds that tool's
/// own exec allow-list; `run(name, vars)` resolves the named template,
/// substitutes `${var}` placeholders (shell-escaped), runs it and parses
/// the output. The allow-list check is native Rust — a handler cannot
/// reach a command its manifest did not declare, nor another tool's.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "PluginCommands")]
pub struct PluginCommandsJs {
  #[qjs(skip_trace)]
  allowed: Arc<BTreeMap<String, String>>,
}

#[rquickjs::methods]
impl PluginCommandsJs {
  #[qjs(rename = "run")]
  pub async fn run<'js>(&self, ctx: Ctx<'js>, name: String, vars: Opt<Value<'js>>) -> rquickjs::Result<Value<'js>> {
    let template = self.allowed.get(&name).cloned().ok_or_else(|| {
      rquickjs::Error::new_from_js_message(
        "commands.run",
        "Error",
        format!("\"{name}\" is not in the exec allow-list for this tool"),
      )
    })?;

    let vars_map: BTreeMap<String, serde_json::Value> = match vars.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => serde_from_js(&ctx, v)?,
      _ => BTreeMap::new(),
    };

    let mut cmd = template;
    for (key, value) in &vars_map {
      let placeholder = format!("${{{key}}}");
      let raw = match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
      };
      cmd = cmd.replace(&placeholder, &shell_single_quote(&raw));
    }

    let output = tokio::task::spawn_blocking(move || Command::new("sh").args(["-c", &cmd]).output())
      .await
      .map_err(|e| rquickjs::Error::new_from_js_message("commands.run", "Error", e.to_string()))?
      .map_err(|e| rquickjs::Error::new_from_js_message("commands.run", "Error", e.to_string()))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr).to_string();
      return Err(rquickjs::Error::new_from_js_message(
        "commands.run",
        "Error",
        format!("command failed (exit {}): {stderr}", output.status),
      ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    serde_to_js(&ctx, &parse_command_output(&stdout))
  }
}

fn shell_single_quote(s: &str) -> String {
  format!("'{}'", s.replace('\'', r"'\''"))
}

fn parse_command_output(s: &str) -> serde_json::Value {
  if s.is_empty() {
    return serde_json::Value::Null;
  }
  if (s.starts_with('{') || s.starts_with('['))
    && let Ok(v) = serde_json::from_str::<serde_json::Value>(s)
  {
    return v;
  }
  serde_json::Value::String(s.to_string())
}

fn rq(e: &ScriptError) -> rquickjs::Error {
  rquickjs::Error::new_from_js_message("plugins", "Error", e.message.clone())
}

/// Install loaded plugins: load+evaluate each file's bytecode (which
/// registers its tools into the shared registry, native or legacy
/// shape), then expose every registered tool as a native
/// `plugins.<name>` callable.
pub fn install_plugins(ctx: &Ctx<'_>, files: &[PluginBinding]) -> rquickjs::Result<()> {
  for file in files {
    // SAFETY: `file.bytecode` was produced by `Module::write` in THIS
    // process and this exact rquickjs/QuickJS build with native
    // endianness (see `compile_and_extract_plugins`) and is never
    // persisted — the precondition `Module::load` documents.
    #[allow(unsafe_code)]
    let module = unsafe { Module::load(ctx.clone(), &file.bytecode) }?;
    // Evaluating the module runs its top-level `defineTool(...)` /
    // `Given(...)` calls, registering directly into the extension
    // registry. No `globalThis.exports`, no post-eval ingest.
    let (_evaluated, _promise) = module.eval()?;
  }

  let names = tool_names(ctx).map_err(|e| rq(&e))?;
  let plugins_obj = Object::new(ctx.clone())?;
  for (idx, name) in names.into_iter().enumerate() {
    // The closure forwards into a generic fn so `Ctx`/`Value`/return
    // share one `'js` (an inline closure with `<'_>` would give each its
    // own lifetime and `Function::call`'s result could not be returned).
    let f = Func::from(move |ctx, call_args| dispatch_tool(ctx, idx, call_args));
    plugins_obj.set(name.as_str(), f)?;
  }
  ctx.globals().set("plugins", plugins_obj)?;
  Ok(())
}

/// Native `plugins.<name>(args)` body: restore the tool's handler from
/// the registry, build `{ args, page, context, request, commands }` via
/// the Object API (per-tool `commands` allow-list + optional net-guarded
/// `request`, both Rust-enforced) and apply the handler. Returns the
/// handler's promise; the JS caller `await`s it. No synthesized JS.
fn dispatch_tool<'js>(ctx: Ctx<'js>, idx: usize, call_args: Opt<Value<'js>>) -> rquickjs::Result<Value<'js>> {
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

  // `request`: pass through unless the tool declared `allow.net`, in
  // which case hand it a net-restricted wrapper over the SAME underlying
  // context (host check enforced natively in `APIRequestContextJs`).
  let req_val: Value<'js> = g.get("request").unwrap_or_else(|_| undef.clone());
  let request_out: Value<'js> = if d.allowed_net.is_empty() {
    req_val
  } else if let Ok(cls) = Class::<APIRequestContextJs>::from_value(&req_val) {
    let inner = cls.borrow().inner_arc();
    let net: Arc<[String]> = Arc::from(d.allowed_net);
    let guarded = Class::instance(ctx.clone(), APIRequestContextJs::with_net(inner, net))?;
    guarded.into_js(&ctx)?
  } else {
    req_val
  };
  arg.set("request", request_out)?;

  let commands = Class::instance(
    ctx.clone(),
    PluginCommandsJs {
      allowed: Arc::new(d.allowed_commands),
    },
  )?;
  arg.set("commands", commands)?;

  d.handler.call((arg,))
}
