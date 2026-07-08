//! The shared, context-owned **extension registry**: the single table
//! every contribution kind lands in while a bundled extension module
//! evaluates, plus the native `defineTool` / `tool` contribution point
//! and the MCP-host read-back/dispatch views. Cucumber's contribution
//! surface (`Given`/`When`/`Then`/hooks/param types) lives in
//! [`super::bdd`]; the `tools.<name>` dispatch surface lives in
//! [`super::plugins`]. Both operate on the registry defined here.

use std::cell::RefCell;

use rquickjs::function::Rest;
use rquickjs::{Ctx, Function, JsLifetime, Object, Persistent, Value, function::Constructor};

use crate::bindings::convert::serde_from_js;
use crate::error::ScriptError;

use super::bdd::StepKind;

/// One MCP tool contribution. The handler is kept as a `Persistent`
/// function and called back natively by the `tools.<name>` dispatch in
/// `plugins.rs` — exactly the mechanism BDD steps use, no synthesized
/// JS dispatch.
pub(crate) struct ToolReg {
  pub(crate) name: String,
  pub(crate) description: Option<String>,
  pub(crate) input_schema: Option<serde_json::Value>,
  pub(crate) expose_as_mcp_tool: bool,
  /// `Arc` so per-call dispatch is a refcount bump, not a deep clone of
  /// the command map / host list on every `tools.<name>()` invocation.
  pub(crate) allowed_commands: std::sync::Arc<std::collections::BTreeMap<String, crate::command_spec::CommandSpec>>,
  pub(crate) allowed_net: std::sync::Arc<[String]>,
  /// Per-tool handler timeout (ms) from the manifest `timeoutMs`. `None`
  /// ⇒ no independent bound (the session wall-clock still applies).
  /// Enforced natively in `plugins::dispatch_tool`.
  pub(crate) timeout_ms: Option<u64>,
  pub(crate) handler: Persistent<Function<'static>>,
}

/// One Cucumber attachment produced by `this.attach(...)` /
/// `this.log(...)` during a scenario. Drained by the BDD layer into the
/// test result so the messages / HTML / Allure reporters surface it
/// (screenshot- and text-on-failure).
#[derive(Debug, Clone)]
pub struct ScriptAttachment {
  pub media_type: String,
  pub bytes: Vec<u8>,
}

/// Rust-side **extension registry**: the single context-owned table that
/// every contribution kind lands in. Cucumber `Given`/`When`/`Then`/
/// hooks/param-types AND MCP `defineTool`/legacy-`exports` tools register
/// here while the user's bundled module evaluates. Hosts read back the
/// kinds they care about (`collect_registry` for BDD, [`tools_snapshot`]
/// for MCP) and dispatch handlers natively ([`invoke_step`], the
/// `tools.<name>` dispatch in `plugins.rs`). No `globalThis.__*`, no
/// synthesized JS.
#[derive(Default)]
pub(crate) struct ExtensionRegistry {
  pub(crate) steps: Vec<StepReg>,
  pub(crate) hooks: Vec<HookReg>,
  pub(crate) param_types: Vec<ParamTypeReg>,
  pub(crate) tools: Vec<ToolReg>,
  /// Attachments queued by the running scenario's `this.attach`/`log`.
  /// Drained per scenario by the BDD layer; cleared by `reset_world`.
  pub(crate) attachments: Vec<ScriptAttachment>,
  pub(crate) default_timeout_ms: u64,
  /// `setDefinitionFunctionWrapper(fn)` — wraps every step body
  /// (cross-cut: retry/trace/log). Applied in [`invoke_step`].
  pub(crate) def_fn_wrapper: Option<Persistent<Function<'static>>>,
  pub(crate) world_ctor: Option<Persistent<Constructor<'static>>>,
  pub(crate) current_world: Option<Persistent<Object<'static>>>,
}

pub(crate) struct StepReg {
  pub(crate) kind: StepKind,
  pub(crate) pattern: String,
  pub(crate) is_regex: bool,
  pub(crate) func: Persistent<Function<'static>>,
  /// Per-step `{ timeout }` (ms) from `Given(pat, { timeout }, fn)`.
  /// `None` ⇒ the registry default. Enforced in [`invoke_step`].
  pub(crate) timeout_ms: Option<u64>,
}

pub(crate) struct HookReg {
  pub(crate) kind: String,
  pub(crate) tags: Option<String>,
  pub(crate) func: Persistent<Function<'static>>,
  /// Per-hook `{ timeout }` (ms). `None` ⇒ registry default.
  pub(crate) timeout_ms: Option<u64>,
}

pub(crate) struct ParamTypeReg {
  pub(crate) name: String,
  pub(crate) regexp: String,
  /// Optional `transformer` fn from `defineParameterType`. Applied to
  /// the matched text in [`invoke_step`] so the step receives a typed
  /// value (cucumber-js parity).
  pub(crate) transformer: Option<Persistent<Function<'static>>>,
}

/// Context userdata holding the registry. Single-threaded VM ⇒
/// `RefCell`, never `Arc`/`Mutex`.
struct RegistryUserData(RefCell<ExtensionRegistry>);

// SAFETY: holds only `'static` data (`Persistent<…>` handles and owned
// values), so re-stating the unused `'js` lifetime is sound — same
// rationale as `SessionAsyncCtx`.
#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for RegistryUserData {
  type Changed<'to> = RegistryUserData;
}

pub(crate) fn with_registry<R>(ctx: &Ctx<'_>, f: impl FnOnce(&mut ExtensionRegistry) -> R) -> Result<R, ScriptError> {
  let ud = ctx
    .userdata::<RegistryUserData>()
    .ok_or_else(|| ScriptError::internal("bdd registry not installed".to_string()))?;
  let mut reg = ud.0.borrow_mut();
  Ok(f(&mut reg))
}

/// Register one MCP tool from a manifest object + handler function.
/// The single tool-registration path, behind the native `tool` /
/// `defineTool` contribution point.
fn register_tool<'js>(ctx: &Ctx<'js>, m: &Object<'js>, handler: Function<'js>) -> Result<(), ScriptError> {
  let name: String = m
    .get("name")
    .map_err(|e| ScriptError::internal(format!("tool manifest missing string `name`: {e}")))?;
  if name.trim().is_empty() {
    return Err(ScriptError::internal(
      "tool: `name` must be a non-empty string".to_string(),
    ));
  }
  let description = m
    .get::<_, Value<'_>>("description")
    .ok()
    .and_then(|v| v.as_string().and_then(|s| s.to_string().ok()));
  let input_schema = match m.get::<_, Value<'_>>("inputSchema") {
    Ok(v) if !v.is_undefined() && !v.is_null() => {
      Some(serde_from_js::<serde_json::Value>(ctx, v).map_err(|e| ScriptError::internal(e.to_string()))?)
    },
    _ => None,
  };
  let expose_as_mcp_tool = m
    .get::<_, bool>("exposeAsMcpTool")
    .or_else(|_| m.get::<_, bool>("exposeAsTool"))
    .unwrap_or(false);
  let timeout_ms = m
    .get::<_, f64>("timeoutMs")
    .ok()
    .map(|ms| ms.max(0.0) as u64)
    .filter(|&v| v > 0);

  let (allowed_commands, allowed_net) = match m.get::<_, Value<'_>>("allow") {
    Ok(v) => {
      if let Some(allow) = v.as_object() {
        // `commands` is the canonical capability name; `exec` is a
        // compatibility alias. Either populates the command allow-list.
        let commands = ["commands", "exec"]
          .into_iter()
          .find_map(|k| match allow.get::<_, Value<'_>>(k) {
            Ok(c) if c.is_object() => serde_from_js(ctx, c).ok(),
            _ => None,
          })
          .unwrap_or_default();
        let net = match allow.get::<_, Value<'_>>("net") {
          Ok(n) if !n.is_undefined() && !n.is_null() => serde_from_js(ctx, n).unwrap_or_default(),
          _ => Vec::new(),
        };
        (commands, net)
      } else {
        (std::collections::BTreeMap::new(), Vec::new())
      }
    },
    Err(_) => (std::collections::BTreeMap::new(), Vec::new()),
  };

  let saved = Persistent::save(ctx, handler);
  with_registry(ctx, |reg| {
    if reg.tools.iter().any(|t| t.name == name) {
      return Err(ScriptError::internal(format!(
        "tool: duplicate tool name `{name}` — names must be unique across all loaded extensions"
      )));
    }
    reg.tools.push(ToolReg {
      name,
      description,
      input_schema,
      expose_as_mcp_tool,
      allowed_commands: std::sync::Arc::new(allowed_commands),
      allowed_net: std::sync::Arc::from(allowed_net),
      timeout_ms,
      handler: saved,
    });
    Ok(())
  })?
}

/// `tool(...)` / `defineTool(...)` argument adapter. Two equivalent
/// native forms: `tool(tool)` where `tool` carries an inline `handler`,
/// or `tool(manifest, handlerFn)`. Uses the same `Rest`-derived
/// single-`'js` pattern `register_step`/`register_hook` use so the
/// `Persistent::save` lifetimes unify. There is no `globalThis.exports`
/// path.
fn register_tool_args(args: &[Value<'_>]) -> rquickjs::Result<()> {
  let ctx = ctx_of(args)?;
  let manifest = args.first().and_then(Value::as_object).ok_or_else(|| {
    rq(&ScriptError::internal(
      "tool: first arg must be a tool/manifest object".to_string(),
    ))
  })?;
  // Handler: an explicit 2nd-arg function wins; otherwise the tool
  // object's own `handler` method.
  let handler = args
    .iter()
    .skip(1)
    .find_map(as_function)
    .or_else(|| {
      manifest
        .get::<_, Value<'_>>("handler")
        .ok()
        .and_then(|v| v.as_function().cloned())
    })
    .ok_or_else(|| {
      rq(&ScriptError::internal(
        "tool: no handler — pass tool(manifest) with a `handler` method or tool(manifest, fn)".to_string(),
      ))
    })?;
  register_tool(&ctx, manifest, handler).map_err(|e| rq(&e))
}

/// Capability allow-list snapshot. Serialises to the exact JSON the MCP
/// `PluginAllow` deserialises (`commands` + `net`, camelCase) so the
/// loader needs no JS round-trip to recover manifests.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CollectedAllow {
  pub commands: std::collections::BTreeMap<String, crate::command_spec::CommandSpec>,
  pub net: Vec<String>,
}

/// One registered tool's manifest, read straight off the Rust registry.
/// Field layout + `camelCase` match MCP `PluginManifest` so a
/// `serde_json` round-trip reconstructs it without re-running the plugin.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectedTool {
  pub name: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub input_schema: Option<serde_json::Value>,
  pub allow: CollectedAllow,
  pub expose_as_mcp_tool: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub timeout_ms: Option<u64>,
}

/// Snapshot every registered tool manifest, in registration order.
/// Synchronous (`&Ctx`) so the bundle/extraction path can call it inside
/// its own `async_with!` without a second context hop.
pub fn tools_snapshot(ctx: &Ctx<'_>) -> Result<Vec<CollectedTool>, ScriptError> {
  with_registry(ctx, |reg| {
    reg
      .tools
      .iter()
      .map(|t| CollectedTool {
        name: t.name.clone(),
        description: t.description.clone(),
        input_schema: t.input_schema.clone(),
        allow: CollectedAllow {
          commands: (*t.allowed_commands).clone(),
          net: t.allowed_net.to_vec(),
        },
        expose_as_mcp_tool: t.expose_as_mcp_tool,
        timeout_ms: t.timeout_ms,
      })
      .collect()
  })
}

/// Number of tools registered so far — lets the loader slice each
/// bundled file's contributions out of the shared registry.
pub fn tools_len(ctx: &Ctx<'_>) -> Result<usize, ScriptError> {
  with_registry(ctx, |reg| reg.tools.len())
}

/// The ordered tool names — drives building the native `tools.<name>`
/// surface.
pub fn tool_names(ctx: &Ctx<'_>) -> Result<Vec<String>, ScriptError> {
  with_registry(ctx, |reg| reg.tools.iter().map(|t| t.name.clone()).collect())
}

/// A tool's restored handler + its capability allow-lists, looked up by
/// registration index. Used by the native `tools.<name>` dispatch in
/// `plugins.rs` — the analogue of `invoke_step`'s registry lookup.
pub(crate) struct ToolDispatch<'js> {
  pub handler: Function<'js>,
  pub allowed_commands: std::sync::Arc<std::collections::BTreeMap<String, crate::command_spec::CommandSpec>>,
  pub allowed_net: std::sync::Arc<[String]>,
  pub timeout_ms: Option<u64>,
}

/// Registration index of the tool named `name`, if that tool was
/// registered in THIS session (a file that failed session install has
/// no entry even when the startup manifest advertises it).
pub(crate) fn tool_index_by_name(ctx: &Ctx<'_>, name: &str) -> Result<Option<usize>, ScriptError> {
  with_registry(ctx, |reg| reg.tools.iter().position(|t| t.name == name))
}

pub(crate) fn tool_dispatch<'js>(ctx: &Ctx<'js>, idx: usize) -> Result<ToolDispatch<'js>, ScriptError> {
  let (saved, allowed_commands, allowed_net, timeout_ms) = with_registry(ctx, |reg| {
    reg
      .tools
      .get(idx)
      .map(|t| {
        (
          t.handler.clone(),
          t.allowed_commands.clone(),
          t.allowed_net.clone(),
          t.timeout_ms,
        )
      })
      .ok_or_else(|| ScriptError::internal(format!("tool index {idx} out of range")))
  })??;
  let handler = saved.restore(ctx).map_err(|e| ScriptError::internal(e.to_string()))?;
  Ok(ToolDispatch {
    handler,
    allowed_commands,
    allowed_net,
    timeout_ms,
  })
}

/// Install the registry userdata (idempotent) and the native `tool` /
/// `defineTool` contribution point, returning the contribution
/// function so the caller can mirror it (`ferridriver.tool`).
pub(crate) fn install<'js>(ctx: &Ctx<'js>, default_timeout_ms: u64) -> rquickjs::Result<Option<Function<'js>>> {
  if ctx.userdata::<RegistryUserData>().is_some() {
    return Ok(None);
  }
  let _ = ctx.store_userdata(RegistryUserData(RefCell::new(ExtensionRegistry {
    default_timeout_ms,
    ..ExtensionRegistry::default()
  })));
  let tool = Function::new(ctx.clone(), |args: Rest<Value<'_>>| register_tool_args(&args.0))?;
  let g = ctx.globals();
  g.set("defineTool", tool.clone())?;
  g.set("tool", tool.clone())?;
  Ok(Some(tool))
}

pub(crate) fn rq(e: &ScriptError) -> rquickjs::Error {
  rquickjs::Error::new_from_js_message("bdd", "Error", e.message.clone())
}

pub(crate) fn as_function<'js>(v: &Value<'js>) -> Option<Function<'js>> {
  v.as_function().cloned()
}

fn ctx_of<'js>(args: &[Value<'js>]) -> Result<Ctx<'js>, rquickjs::Error> {
  args
    .first()
    .map(|v| v.ctx().clone())
    .ok_or_else(|| rq(&ScriptError::internal("missing arguments".to_string())))
}
