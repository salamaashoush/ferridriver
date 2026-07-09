//! The shared, context-owned **extension registry**: the single table
//! every contribution kind lands in while a bundled extension module
//! evaluates, plus the native `defineTool` / `tool` contribution point
//! and the MCP-host read-back/dispatch views. Cucumber's contribution
//! surface (`Given`/`When`/`Then`/hooks/param types) lives in
//! [`super::bdd`]; the `tools.<name>` dispatch surface lives in
//! [`super::extensions`]. Both operate on the registry defined here.

use std::cell::RefCell;

use rquickjs::function::Rest;
use rquickjs::{Ctx, Function, JsLifetime, Object, Persistent, Value, function::Constructor};

use crate::bindings::convert::serde_from_js;
use crate::error::ScriptError;

use super::bdd::StepKind;

/// One MCP tool contribution. The handler is kept as a `Persistent`
/// function and called back natively by the `tools.<name>` dispatch in
/// `extensions.rs` — exactly the mechanism BDD steps use, no synthesized
/// JS dispatch.
pub(crate) struct ToolReg {
  pub(crate) name: String,
  pub(crate) title: Option<String>,
  pub(crate) description: Option<String>,
  pub(crate) input_schema: Option<serde_json::Value>,
  /// JSON Schema for the handler's return value. Surfaced as the
  /// promoted tool's `outputSchema` and validated by the MCP layer.
  pub(crate) output_schema: Option<serde_json::Value>,
  /// MCP tool annotations (`readOnlyHint`, `destructiveHint`, ...),
  /// kept opaque here — the MCP layer types and surfaces them.
  pub(crate) annotations: Option<serde_json::Value>,
  pub(crate) expose_as_mcp_tool: bool,
  /// `Arc` so per-call dispatch is a refcount bump, not a deep clone of
  /// the command map / host list on every `tools.<name>()` invocation.
  pub(crate) allowed_commands: std::sync::Arc<std::collections::BTreeMap<String, crate::command_spec::CommandSpec>>,
  /// Effective `allow.net` after the operator ceiling: `None` ⇒
  /// unrestricted, `Some(list)` ⇒ default-deny allow-list (an empty
  /// list denies every host).
  pub(crate) allowed_net: Option<std::sync::Arc<[String]>>,
  /// Per-tool handler timeout (ms) from the manifest `timeoutMs`. `None`
  /// ⇒ no independent bound (the session wall-clock still applies).
  /// Enforced natively in `extensions::dispatch_tool`.
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
/// `tools.<name>` dispatch in `extensions.rs`). No `globalThis.__*`, no
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

/// Operator extension-policy ceiling for this VM, stored as context
/// userdata at `Session::create`. [`register_tool`] intersects every
/// declared manifest with it, so the `ToolReg` a session dispatches
/// from already carries the EFFECTIVE grants. Absent (the manifest
/// extraction runtime, plain engine tests) ⇒ manifests register
/// unclamped — extraction reports declared intent; enforcement is
/// session-scoped.
pub(crate) struct ExtensionPolicyUd(pub(crate) ferridriver_config::ExtensionPolicyConfig);

// SAFETY: holds only owned config data (no JS values), so re-stating
// the unused `'js` lifetime is sound — same rationale as
// `RegistryUserData`.
#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for ExtensionPolicyUd {
  type Changed<'to> = ExtensionPolicyUd;
}

/// The effective `allow.net` for a declared list under the operator
/// ceiling. `None` ⇒ unrestricted; `Some` ⇒ default-deny allow-list.
///
/// - no ceiling: declared semantics unchanged (empty = unrestricted).
/// - ceiling + no declaration: the tool gets exactly the ceiling
///   (operator flips undeclared tools to default-deny).
/// - ceiling + declaration: only declared entries the ceiling subsumes
///   survive (an explicit empty result denies every host).
fn effective_net(declared: Vec<String>, ceiling: Option<&[String]>) -> Option<std::sync::Arc<[String]>> {
  match (ceiling, declared.is_empty()) {
    (None, true) => None,
    (None, false) => Some(std::sync::Arc::from(declared)),
    (Some(c), true) => Some(std::sync::Arc::from(c.to_vec())),
    (Some(c), false) => Some(std::sync::Arc::from(
      declared
        .into_iter()
        .filter(|d| net_entry_subsumed(d, c))
        .collect::<Vec<_>>(),
    )),
  }
}

/// Whether one manifest `allow.net` entry lies within the ceiling. An
/// exact host is subsumed when the ceiling would allow it as a request
/// host; a wildcard `*.suffix` only when a ceiling wildcard's domain
/// space contains its whole space (an exact ceiling entry can never
/// subsume a wildcard).
pub fn net_entry_subsumed(entry: &str, ceiling: &[String]) -> bool {
  if let Some(suffix) = entry.strip_prefix("*.") {
    ceiling.iter().any(|c| {
      c.strip_prefix("*.")
        .is_some_and(|cs| suffix == cs || suffix.ends_with(&format!(".{cs}")))
    })
  } else {
    ferridriver::http_client::host_allowed(entry, ceiling)
  }
}

/// Enforce the operator commands ceiling on a tool's declared command
/// map. A violation fails the whole registration (the extension file is
/// skipped and the conflict logged) — a policy conflict is a
/// configuration error, not something to silently narrow.
fn check_commands_ceiling(
  tool: &str,
  commands: &std::collections::BTreeMap<String, crate::command_spec::CommandSpec>,
  ceiling: ferridriver_config::ExtensionCommandsCeiling,
) -> Result<(), ScriptError> {
  use ferridriver_config::ExtensionCommandsCeiling as Ceiling;
  match ceiling {
    Ceiling::Any => Ok(()),
    Ceiling::ArgvOnly => {
      for (name, spec) in commands {
        if matches!(spec.run, crate::command_spec::CommandRun::Shell(_)) {
          return Err(ScriptError::internal(format!(
            "tool `{tool}`: command `{name}` is a shell-string spec, but the operator policy \
             (`[extensions.policy] commands = \"argvOnly\"`) permits only argv-array specs"
          )));
        }
      }
      Ok(())
    },
    Ceiling::None => {
      if commands.is_empty() {
        Ok(())
      } else {
        Err(ScriptError::internal(format!(
          "tool `{tool}` declares `allow.commands`, but the operator policy \
           (`[extensions.policy] commands = \"none\"`) forbids command declarations"
        )))
      }
    },
  }
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
  let get_str = |key: &str| {
    m.get::<_, Value<'_>>(key)
      .ok()
      .and_then(|v| v.as_string().and_then(|s| s.to_string().ok()))
  };
  let title = get_str("title");
  let description = get_str("description");
  let get_json = |key: &str| -> Result<Option<serde_json::Value>, ScriptError> {
    match m.get::<_, Value<'_>>(key) {
      Ok(v) if !v.is_undefined() && !v.is_null() => {
        Ok(Some(serde_from_js::<serde_json::Value>(ctx, v).map_err(|e| {
          ScriptError::internal(format!("tool `{key}` is not JSON-serialisable: {e}"))
        })?))
      },
      _ => Ok(None),
    }
  };
  let input_schema = get_json("inputSchema")?;
  let output_schema = get_json("outputSchema")?;
  let annotations = get_json("annotations")?;
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

  // Operator ceiling (session VMs only; the extraction runtime carries
  // none so manifests keep their DECLARED capabilities for reporting).
  let policy = ctx.userdata::<ExtensionPolicyUd>().map(|u| u.0.clone());
  let allowed_net = match &policy {
    Some(p) => effective_net(allowed_net, p.net.as_deref()),
    None => effective_net(allowed_net, None),
  };
  if let Some(p) = &policy {
    check_commands_ceiling(&name, &allowed_commands, p.commands)?;
  }

  let saved = Persistent::save(ctx, handler);
  with_registry(ctx, |reg| {
    if reg.tools.iter().any(|t| t.name == name) {
      return Err(ScriptError::internal(format!(
        "tool: duplicate tool name `{name}` — names must be unique across all loaded extensions"
      )));
    }
    reg.tools.push(ToolReg {
      name,
      title,
      description,
      input_schema,
      output_schema,
      annotations,
      expose_as_mcp_tool,
      allowed_commands: std::sync::Arc::new(allowed_commands),
      allowed_net,
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
/// `ToolAllow` deserialises (`commands` + `net`, camelCase) so the
/// loader needs no JS round-trip to recover manifests.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CollectedAllow {
  pub commands: std::collections::BTreeMap<String, crate::command_spec::CommandSpec>,
  pub net: Vec<String>,
}

/// One registered tool's manifest, read straight off the Rust registry.
/// Field layout + `camelCase` match MCP `ToolManifest` so a
/// `serde_json` round-trip reconstructs it without re-running the extension.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectedTool {
  pub name: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub title: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub input_schema: Option<serde_json::Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub output_schema: Option<serde_json::Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub annotations: Option<serde_json::Value>,
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
        title: t.title.clone(),
        description: t.description.clone(),
        input_schema: t.input_schema.clone(),
        output_schema: t.output_schema.clone(),
        annotations: t.annotations.clone(),
        allow: CollectedAllow {
          commands: (*t.allowed_commands).clone(),
          net: t.allowed_net.as_ref().map(|n| n.to_vec()).unwrap_or_default(),
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
/// `extensions.rs` — the analogue of `invoke_step`'s registry lookup.
pub(crate) struct ToolDispatch<'js> {
  pub handler: Function<'js>,
  pub allowed_commands: std::sync::Arc<std::collections::BTreeMap<String, crate::command_spec::CommandSpec>>,
  /// Effective net policy (`None` = unrestricted, `Some` = default-deny
  /// allow-list, possibly empty = deny all).
  pub allowed_net: Option<std::sync::Arc<[String]>>,
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

#[cfg(test)]
mod tests {
  use super::{effective_net, net_entry_subsumed};

  fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
  }

  #[test]
  fn no_ceiling_keeps_declared_semantics() {
    assert_eq!(effective_net(Vec::new(), None), None);
    let e = effective_net(v(&["api.box.com"]), None).expect("declared list");
    assert_eq!(&*e, ["api.box.com".to_string()].as_slice());
  }

  #[test]
  fn ceiling_flips_undeclared_tools_to_the_ceiling() {
    let e = effective_net(Vec::new(), Some(&v(&["*.box.com"]))).expect("ceiling");
    assert_eq!(&*e, ["*.box.com".to_string()].as_slice());
  }

  #[test]
  fn ceiling_intersects_declared_entries() {
    let e = effective_net(
      v(&["api.box.com", "evil.example", "*.box.com"]),
      Some(&v(&["*.box.com"])),
    )
    .expect("intersection");
    assert_eq!(&*e, v(&["api.box.com", "*.box.com"]).as_slice());
  }

  #[test]
  fn empty_ceiling_denies_everything() {
    let e = effective_net(Vec::new(), Some(&[])).expect("deny-all");
    assert!(e.is_empty());
    let e = effective_net(v(&["api.box.com"]), Some(&[])).expect("deny-all");
    assert!(e.is_empty());
  }

  #[test]
  fn subsumption_covers_exact_wildcard_and_apex() {
    let ceiling = v(&["*.box.com", "localhost"]);
    assert!(net_entry_subsumed("api.box.com", &ceiling));
    assert!(net_entry_subsumed("box.com", &ceiling), "wildcard covers the apex");
    assert!(net_entry_subsumed("localhost", &ceiling));
    assert!(net_entry_subsumed("*.box.com", &ceiling), "equal wildcard");
    assert!(net_entry_subsumed("*.api.box.com", &ceiling), "narrower wildcard");
    assert!(!net_entry_subsumed("evil.example", &ceiling));
    assert!(!net_entry_subsumed("*.com", &ceiling), "wider wildcard must not pass");
    assert!(
      !net_entry_subsumed("*.localhost", &v(&["localhost"])),
      "an exact ceiling entry never subsumes a wildcard"
    );
    assert!(
      !net_entry_subsumed("evilbox.com", &v(&["*.box.com"])),
      "suffix match must be label-aligned"
    );
  }
}
