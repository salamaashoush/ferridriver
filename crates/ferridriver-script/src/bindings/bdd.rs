//! Cucumber step-definition surface for the shared QuickJS engine.
//!
//! The same VM that runs `ferridriver run` scripts and MCP `run_script`
//! also loads cucumber-js-shaped `.js` step files. `Given`/`When`/
//! `Then`/`Before`/`After`/`defineParameterType`/... are native Rust
//! functions (no JS glue); registrations land in a Rust `ExtensionRegistry`
//! held as context userdata (the QuickJS context is single-threaded, so
//! a `RefCell` is the right interior mutability — no `Arc`/`Mutex`).
//! Step bodies are kept as `Persistent` functions and called back by
//! the Rust `ferridriver-bdd` core. Every body receives the
//! per-scenario World as its FIRST positional argument — arrow,
//! classic `function`, async, all the same shape — followed by the
//! cucumber-extracted parameters, an optional `DataTableJs`, and an
//! optional doc-string. The World is also bound as `this` so
//! `function (world) { this === world }` holds for callers who prefer
//! that style.
//!
//! No business logic here: matching, outline expansion, tag filtering
//! and hook ordering all stay in the `ferridriver-bdd` core.

use std::cell::RefCell;
use std::sync::Arc;

use rquickjs::class::{Class, Trace};
use rquickjs::function::{Args, Constructor, Opt, Rest};
use rquickjs::{ArrayBuffer, CatchResultExt, Ctx, Function, JsLifetime, Object, Persistent, TypedArray, Value};

use crate::bindings::convert::{serde_from_js, serde_to_js};
use crate::bindings::{install_browser_context_on, install_browser_on, install_page_on, install_request_on};
use crate::engine::caught_to_script_error;
use crate::error::ScriptError;

/// Thrown by `this.skip()`; recognised in [`invoke_step`] and mapped to
/// `StepOutcome::Skipped` (cucumber aborts the step on throw).
const SKIP_SENTINEL: &str = "__ferri_skip__";

/// Cucumber step keyword. `Step` is keyword-agnostic (`defineStep`,
/// `And`, `But`); matching in the core is keyword-agnostic anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
  Given,
  When,
  Then,
  Step,
}

impl StepKind {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Given => "Given",
      Self::When => "When",
      Self::Then => "Then",
      Self::Step => "Step",
    }
  }
}

struct StepReg {
  kind: StepKind,
  pattern: String,
  is_regex: bool,
  func: Persistent<Function<'static>>,
  /// Per-step `{ timeout }` (ms) from `Given(pat, { timeout }, fn)`.
  /// `None` ⇒ the registry default. Enforced in [`invoke_step`].
  timeout_ms: Option<u64>,
}

struct HookReg {
  kind: String,
  tags: Option<String>,
  func: Persistent<Function<'static>>,
  /// Per-hook `{ timeout }` (ms). `None` ⇒ registry default.
  timeout_ms: Option<u64>,
}

struct ParamTypeReg {
  name: String,
  regexp: String,
  /// Optional `transformer` fn from `defineParameterType`. Applied to
  /// the matched text in [`invoke_step`] so the step receives a typed
  /// value (cucumber-js parity).
  transformer: Option<Persistent<Function<'static>>>,
}

/// One MCP tool contribution. The handler is kept as a `Persistent`
/// function and called back natively by [`invoke_tool`] — exactly the
/// mechanism BDD steps use, no synthesized JS dispatch.
struct ToolReg {
  name: String,
  description: Option<String>,
  input_schema: Option<serde_json::Value>,
  expose_as_mcp_tool: bool,
  allowed_commands: std::collections::BTreeMap<String, crate::command_spec::CommandSpec>,
  allowed_net: Vec<String>,
  /// Per-tool handler timeout (ms) from the manifest `timeoutMs`. `None`
  /// ⇒ no independent bound (the session wall-clock still applies).
  /// Enforced natively in `plugins::dispatch_tool`.
  timeout_ms: Option<u64>,
  handler: Persistent<Function<'static>>,
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

/// The argument cucumber-js passes to `Before`/`After` hooks. Built by
/// the BDD layer and lowered to a JS object `{ pickle: { name, tags },
/// result: { status, message? } }` in [`invoke_hook`] — enough for the
/// screenshot-on-failure idiom (`After(s => { if (s.result.status ===
/// 'FAILED') this.attach(...) })`).
#[derive(Debug, Clone, Default)]
pub struct HookArg {
  pub name: String,
  pub tags: Vec<String>,
  /// Cucumber status: `PENDING` for `Before` (not yet run), `PASSED` /
  /// `FAILED` for `After`.
  pub status: String,
  pub message: Option<String>,
}

/// Rust-side **extension registry**: the single context-owned table that
/// every contribution kind lands in. Cucumber `Given`/`When`/`Then`/
/// hooks/param-types AND MCP `defineTool`/legacy-`exports` tools register
/// here while the user's bundled module evaluates. Hosts read back the
/// kinds they care about (`collect_registry` for BDD, [`collect_tools`]
/// for MCP) and dispatch handlers natively ([`invoke_step`],
/// [`invoke_tool`]). No `globalThis.__*`, no synthesized JS.
#[derive(Default)]
struct ExtensionRegistry {
  steps: Vec<StepReg>,
  hooks: Vec<HookReg>,
  param_types: Vec<ParamTypeReg>,
  tools: Vec<ToolReg>,
  /// Attachments queued by the running scenario's `this.attach`/`log`.
  /// Drained per scenario by the BDD layer; cleared by `reset_world`.
  attachments: Vec<ScriptAttachment>,
  default_timeout_ms: u64,
  /// `setDefinitionFunctionWrapper(fn)` — wraps every step body
  /// (cross-cut: retry/trace/log). Applied in [`invoke_step`].
  def_fn_wrapper: Option<Persistent<Function<'static>>>,
  world_ctor: Option<Persistent<Constructor<'static>>>,
  current_world: Option<Persistent<Object<'static>>>,
}

/// Read an optional `{ timeout }` (milliseconds) off an options object
/// arg, mirroring cucumber-js `Given(pat, { timeout }, fn)` /
/// `Before({ timeout }, fn)`.
fn timeout_from_opts(args: &[Value<'_>]) -> Option<u64> {
  args.iter().find_map(|v| {
    let o = v.as_object()?;
    if v.as_function().is_some() {
      return None;
    }
    o.get::<_, f64>("timeout").ok().map(|ms| ms.max(0.0) as u64)
  })
}

/// Context userdata holding the registry. Single-threaded VM ⇒
/// `RefCell`, never `Arc`/`Mutex`.
struct BddUserData(RefCell<ExtensionRegistry>);

// SAFETY: holds only `'static` data (`Persistent<…>` handles and owned
// values), so re-stating the unused `'js` lifetime is sound — same
// rationale as `SessionAsyncCtx`.
#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for BddUserData {
  type Changed<'to> = BddUserData;
}

fn with_registry<R>(ctx: &Ctx<'_>, f: impl FnOnce(&mut ExtensionRegistry) -> R) -> Result<R, ScriptError> {
  let ud = ctx
    .userdata::<BddUserData>()
    .ok_or_else(|| ScriptError::internal("bdd registry not installed".to_string()))?;
  let mut reg = ud.0.borrow_mut();
  Ok(f(&mut reg))
}

/// A cucumber data table, passed to steps as the trailing argument.
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "DataTable")]
pub struct DataTableJs {
  #[qjs(skip_trace)]
  rows: Vec<Vec<String>>,
}

#[rquickjs::methods]
impl DataTableJs {
  /// Every row including the header.
  #[qjs(rename = "raw")]
  pub fn raw(&self) -> Vec<Vec<String>> {
    self.rows.clone()
  }

  /// All rows except the header.
  #[qjs(rename = "rows")]
  pub fn data_rows(&self) -> Vec<Vec<String>> {
    self.rows.iter().skip(1).cloned().collect()
  }

  /// One object per data row keyed by the header row.
  #[qjs(rename = "hashes")]
  pub fn hashes<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let header = self.rows.first().cloned().unwrap_or_default();
    let out: Vec<serde_json::Map<String, serde_json::Value>> = self
      .rows
      .iter()
      .skip(1)
      .map(|row| {
        header
          .iter()
          .zip(row.iter())
          .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
          .collect()
      })
      .collect();
    serde_to_js(&ctx, &out)
  }

  /// First column as keys, second as values.
  #[qjs(rename = "rowsHash")]
  pub fn rows_hash<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let map: serde_json::Map<String, serde_json::Value> = self
      .rows
      .iter()
      .filter(|r| r.len() >= 2)
      .map(|r| (r[0].clone(), serde_json::Value::String(r[1].clone())))
      .collect();
    serde_to_js(&ctx, &map)
  }

  /// Rows and columns swapped.
  #[qjs(rename = "transpose")]
  pub fn transpose<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, DataTableJs>> {
    let cols = self.rows.iter().map(Vec::len).max().unwrap_or(0);
    let rows = (0..cols)
      .map(|c| {
        self
          .rows
          .iter()
          .map(|r| r.get(c).cloned().unwrap_or_default())
          .collect()
      })
      .collect();
    Class::instance(ctx, DataTableJs { rows })
  }
}

fn as_function<'js>(v: &Value<'js>) -> Option<Function<'js>> {
  v.as_function().cloned()
}

fn pattern_of(a: &Value<'_>) -> Result<(String, bool), ScriptError> {
  if let Some(s) = a.as_string() {
    return Ok((s.to_string().map_err(|e| ScriptError::internal(e.to_string()))?, false));
  }
  if let Some(o) = a.as_object() {
    if let Ok(src) = o.get::<_, String>("source") {
      return Ok((src, true));
    }
  }
  Err(ScriptError::internal(
    "step pattern must be a string or RegExp".to_string(),
  ))
}

fn rq(e: &ScriptError) -> rquickjs::Error {
  rquickjs::Error::new_from_js_message("bdd", "Error", e.message.clone())
}

fn ctx_of<'js>(args: &[Value<'js>]) -> Result<Ctx<'js>, rquickjs::Error> {
  args
    .first()
    .map(|v| v.ctx().clone())
    .ok_or_else(|| rq(&ScriptError::internal("missing arguments".to_string())))
}

fn register_step(kind: StepKind, args: &[Value<'_>]) -> rquickjs::Result<()> {
  let ctx = ctx_of(args)?;
  let pattern = args
    .first()
    .ok_or_else(|| rq(&ScriptError::internal("step pattern missing".to_string())))?;
  let (pat, is_regex) = pattern_of(pattern).map_err(|e| rq(&e))?;
  // `Given(pattern, fn)` or `Given(pattern, options, fn)`: the body is
  // the last function argument.
  let func = args
    .iter()
    .skip(1)
    .rev()
    .find_map(as_function)
    .ok_or_else(|| rq(&ScriptError::internal(format!("step `{pat}` has no function body"))))?;
  let timeout_ms = timeout_from_opts(&args[1..]);
  let saved = Persistent::save(&ctx, func);
  with_registry(&ctx, |reg| {
    reg.steps.push(StepReg {
      kind,
      pattern: pat,
      is_regex,
      func: saved,
      timeout_ms,
    });
  })
  .map_err(|e| rq(&e))
}

fn register_hook(kind: &str, args: &[Value<'_>]) -> rquickjs::Result<()> {
  let ctx = ctx_of(args)?;
  let first = args
    .first()
    .ok_or_else(|| rq(&ScriptError::internal(format!("{kind} hook missing"))))?;
  let (tags, func) = if let Some(f) = as_function(first) {
    (None, f)
  } else {
    let tags = if let Some(s) = first.as_string() {
      Some(s.to_string().map_err(|e| rq(&ScriptError::internal(e.to_string())))?)
    } else if let Some(o) = first.as_object() {
      o.get::<_, String>("tags").ok()
    } else {
      None
    };
    let f = args
      .iter()
      .skip(1)
      .find_map(as_function)
      .ok_or_else(|| rq(&ScriptError::internal(format!("{kind} hook has no function body"))))?;
    (tags, f)
  };
  let timeout_ms = timeout_from_opts(args);
  let saved = Persistent::save(&ctx, func);
  with_registry(&ctx, |reg| {
    reg.hooks.push(HookReg {
      kind: kind.to_string(),
      tags,
      func: saved,
      timeout_ms,
    });
  })
  .map_err(|e| rq(&e))
}

fn value_bytes(v: &Value<'_>) -> Option<Vec<u8>> {
  if let Ok(ta) = TypedArray::<u8>::from_value(v.clone())
    && let Some(b) = ta.as_bytes()
  {
    return Some(b.to_vec());
  }
  if let Some(obj) = v.as_object()
    && let Some(buf) = ArrayBuffer::from_object(obj.clone())
    && let Some(b) = buf.as_bytes()
  {
    return Some(b.to_vec());
  }
  None
}

/// `this.attach(data, mediaType?)` / `this.log(...)` adapter. Mirrors
/// cucumber-js: a string attaches as `text/plain` (override via
/// `mediaType`), a `Uint8Array`/`ArrayBuffer` as
/// `application/octet-stream`, anything else JSON-encoded as
/// `application/json`. `log` joins its args as
/// `text/x.cucumber.log+plain`. Same `Rest`-derived single-`'js`
/// pattern as `register_step`.
fn register_attachment(args: &[Value<'_>], is_log: bool) -> rquickjs::Result<()> {
  let ctx = ctx_of(args)?;
  let media_arg = args.get(1).and_then(Value::as_string).and_then(|s| s.to_string().ok());

  let (bytes, media): (Vec<u8>, String) = if is_log {
    let text = args
      .iter()
      .map(|v| {
        v.as_string().and_then(|s| s.to_string().ok()).unwrap_or_else(|| {
          serde_from_js::<serde_json::Value>(&ctx, v.clone())
            .map(|j| j.to_string())
            .unwrap_or_default()
        })
      })
      .collect::<Vec<_>>()
      .join(" ");
    (text.into_bytes(), "text/x.cucumber.log+plain".to_string())
  } else {
    let data = args
      .first()
      .cloned()
      .unwrap_or_else(|| Value::new_undefined(ctx.clone()));
    if let Some(s) = data.as_string() {
      let s = s.to_string().map_err(|e| rq(&ScriptError::internal(e.to_string())))?;
      (s.into_bytes(), media_arg.unwrap_or_else(|| "text/plain".to_string()))
    } else if let Some(b) = value_bytes(&data) {
      (b, media_arg.unwrap_or_else(|| "application/octet-stream".to_string()))
    } else {
      let j: serde_json::Value = serde_from_js(&ctx, data).map_err(|e| rq(&ScriptError::internal(e.to_string())))?;
      (
        serde_json::to_vec(&j).unwrap_or_default(),
        media_arg.unwrap_or_else(|| "application/json".to_string()),
      )
    }
  };

  with_registry(&ctx, |reg| {
    reg.attachments.push(ScriptAttachment {
      media_type: media,
      bytes,
    });
  })
  .map_err(|e| rq(&e))
}

/// Drain the scenario's queued attachments (and clear the queue). The
/// BDD layer calls this after each scenario and forwards them into the
/// test result so the reporters surface them.
pub async fn drain_attachments(vm: &crate::vm::VmHandle) -> Result<Vec<ScriptAttachment>, ScriptError> {
  crate::vm_with!(vm => |ctx| {
    with_registry(&ctx, |reg| std::mem::take(&mut reg.attachments))
  })
  .await?
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
      allowed_commands,
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

/// Install the native cucumber + MCP-tool surface and the shared
/// extension registry as context userdata. Idempotent; called once at
/// `Session::create`.
pub fn install_bdd(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  if ctx.userdata::<BddUserData>().is_some() {
    return Ok(());
  }
  let _ = ctx.store_userdata(BddUserData(RefCell::new(ExtensionRegistry {
    default_timeout_ms: 5000,
    ..ExtensionRegistry::default()
  })));

  let g = ctx.globals();
  let bdd = Object::new(ctx.clone())?;
  Class::<DataTableJs>::define(&g)?;

  for (name, kind) in [
    ("Given", StepKind::Given),
    ("When", StepKind::When),
    ("Then", StepKind::Then),
    ("defineStep", StepKind::Step),
    ("And", StepKind::Step),
    ("But", StepKind::Step),
  ] {
    let f = Function::new(ctx.clone(), move |args: Rest<Value<'_>>| register_step(kind, &args.0))?;
    g.set(name, f.clone())?;
    bdd.set(name, f)?;
  }

  for hook in ["Before", "After", "BeforeAll", "AfterAll", "BeforeStep", "AfterStep"] {
    let f = Function::new(ctx.clone(), move |args: Rest<Value<'_>>| register_hook(hook, &args.0))?;
    g.set(hook, f.clone())?;
    bdd.set(hook, f)?;
  }

  let define_parameter_type = Function::new(ctx.clone(), |def: Object<'_>| -> rquickjs::Result<()> {
    let ctx = def.ctx().clone();
    let name: String = def.get("name").map_err(|e| rq(&ScriptError::internal(e.to_string())))?;
    let rx_val: Value<'_> = def
      .get("regexp")
      .map_err(|e| rq(&ScriptError::internal(e.to_string())))?;
    let regexp = if let Some(s) = rx_val.as_string() {
      s.to_string().map_err(|e| rq(&ScriptError::internal(e.to_string())))?
    } else if let Some(o) = rx_val.as_object() {
      o.get::<_, String>("source")
        .map_err(|e| rq(&ScriptError::internal(e.to_string())))?
    } else {
      return Err(rq(&ScriptError::internal(
        "parameter type regexp must be string or RegExp".to_string(),
      )));
    };
    let transformer = def
      .get::<_, Value<'_>>("transformer")
      .ok()
      .and_then(|v| v.as_function().cloned())
      .map(|f| Persistent::save(&ctx, f));
    with_registry(&ctx, |reg| {
      reg.param_types.push(ParamTypeReg {
        name,
        regexp,
        transformer,
      });
    })
    .map_err(|e| rq(&e))
  })?;
  g.set("defineParameterType", define_parameter_type.clone())?;
  bdd.set("defineParameterType", define_parameter_type)?;

  let set_default_timeout = Function::new(ctx.clone(), |ctx: Ctx<'_>, ms: f64| -> rquickjs::Result<()> {
    with_registry(&ctx, |reg| reg.default_timeout_ms = ms.max(0.0) as u64).map_err(|e| rq(&e))
  })?;
  g.set("setDefaultTimeout", set_default_timeout.clone())?;
  bdd.set("setDefaultTimeout", set_default_timeout)?;

  let set_definition_function_wrapper = Function::new(ctx.clone(), |w: Function<'_>| -> rquickjs::Result<()> {
    let ctx = w.ctx().clone();
    let saved = Persistent::save(&ctx, w);
    with_registry(&ctx, |reg| reg.def_fn_wrapper = Some(saved)).map_err(|e| rq(&e))
  })?;
  g.set("setDefinitionFunctionWrapper", set_definition_function_wrapper.clone())?;
  bdd.set("setDefinitionFunctionWrapper", set_definition_function_wrapper)?;

  let set_world_constructor = Function::new(ctx.clone(), |c: Constructor<'_>| -> rquickjs::Result<()> {
    let ctx = c.ctx().clone();
    let saved = Persistent::save(&ctx, c);
    with_registry(&ctx, |reg| reg.world_ctor = Some(saved)).map_err(|e| rq(&e))
  })?;
  g.set("setWorldConstructor", set_world_constructor.clone())?;
  bdd.set("setWorldConstructor", set_world_constructor)?;
  // `setParallelCanAssign` is accepted (so cucumber-js suites that call
  // it don't break) but intentionally inert: it governs cucumber-js's
  // own pickle-level parallel scheduler, whereas ferridriver
  // parallelises at the `ferridriver-test` worker level (one VM per
  // worker) with no equivalent per-pickle assignment hook. A real
  // implementation would be a cross-worker scheduler rework with no
  // proportionate value — documented-inert, not a stub claiming to work.
  let set_parallel_can_assign = Function::new(ctx.clone(), |_: Opt<Value<'_>>| {})?;
  g.set("setParallelCanAssign", set_parallel_can_assign.clone())?;
  bdd.set("setParallelCanAssign", set_parallel_can_assign)?;

  // MCP tool contribution point — the only tool-registration surface.
  // `defineTool(tool)` (inline `handler`) or `defineTool(manifest, fn)`.
  let tool = Function::new(ctx.clone(), |args: Rest<Value<'_>>| register_tool_args(&args.0))?;
  g.set("defineTool", tool.clone())?;
  g.set("tool", tool.clone())?;

  let fd = crate::bindings::runtime::ensure_ferridriver(ctx)?;
  fd.set("bdd", bdd)?;
  fd.set("tool", tool)?;

  Ok(())
}

/// Step metadata read back by the `ferridriver-bdd` core to build its
/// Cucumber-Expression registry. Straight off the Rust registry — no JS
/// round-trip.
#[derive(Debug, Clone)]
pub struct CollectedStep {
  pub kind: String,
  pub pattern: String,
  pub is_regex: bool,
}

#[derive(Debug, Clone)]
pub struct CollectedHook {
  pub hook_type: String,
  pub tags: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CollectedParamType {
  pub name: String,
  pub regexp: String,
}

#[derive(Debug, Clone)]
pub struct CollectedRegistry {
  pub default_timeout_ms: u64,
  pub steps: Vec<CollectedStep>,
  pub hooks: Vec<CollectedHook>,
  pub param_types: Vec<CollectedParamType>,
}

/// Snapshot the registry after the step `.js` files evaluated.
pub async fn collect_registry(vm: &crate::vm::VmHandle) -> Result<CollectedRegistry, ScriptError> {
  crate::vm_with!(vm => |ctx| {
    with_registry(&ctx, |reg| CollectedRegistry {
      default_timeout_ms: reg.default_timeout_ms,
      steps: reg
        .steps
        .iter()
        .map(|s| CollectedStep {
          kind: s.kind.as_str().to_string(),
          pattern: s.pattern.clone(),
          is_regex: s.is_regex,
        })
        .collect(),
      hooks: reg
        .hooks
        .iter()
        .map(|h| CollectedHook {
          hook_type: h.kind.clone(),
          tags: h.tags.clone(),
        })
        .collect(),
      param_types: reg
        .param_types
        .iter()
        .map(|p| CollectedParamType {
          name: p.name.clone(),
          regexp: p.regexp.clone(),
        })
        .collect(),
    })
  })
  .await?
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
          commands: t.allowed_commands.clone(),
          net: t.allowed_net.clone(),
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
  pub allowed_commands: std::collections::BTreeMap<String, crate::command_spec::CommandSpec>,
  pub allowed_net: Vec<String>,
  pub timeout_ms: Option<u64>,
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

/// Per-scenario fixtures the BDD core threads onto the JS World — the
/// same handles `RunContext` carries for scripting, installed onto a
/// per-scenario World object rather than `globalThis`.
#[derive(Clone, Default)]
pub struct ScenarioWorld {
  pub page: Option<Arc<ferridriver::Page>>,
  pub context: Option<Arc<ferridriver::context::ContextRef>>,
  pub request: Option<Arc<ferridriver::http_client::HttpClient>>,
  pub browser: Option<Arc<ferridriver::Browser>>,
  /// Cucumber `--world-parameters` (top-level config / CLI). Exposed as
  /// `this.parameters` and passed to a `setWorldConstructor` ctor as
  /// `{ parameters }`. `None`/`Null` ⇒ `{}`.
  pub parameters: Option<serde_json::Value>,
}

/// Build the per-scenario World and make it the `this` steps run
/// against. If `setWorldConstructor` was used, that class is
/// constructed and the fixtures are augmented onto the instance.
pub async fn set_scenario_world(vm: &crate::vm::VmHandle, world: &ScenarioWorld) -> Result<(), ScriptError> {
  let world = world.clone();
  let route_vm = vm.clone();
  crate::vm_with!(vm => |ctx| {
    let ctor = with_registry(&ctx, |reg| reg.world_ctor.clone())?;

    // `this.parameters` (cucumber `--world-parameters`). Built once;
    // passed to a custom World ctor as `{ parameters }` and set on the
    // instance regardless (cucumber-js always populates it).
    let params_val: Value<'_> = match &world.parameters {
      Some(v) if !v.is_null() => serde_to_js(&ctx, v).map_err(|e| ScriptError::internal(e.to_string()))?,
      _ => Object::new(ctx.clone())
        .map_err(|e| ScriptError::internal(e.to_string()))?
        .into_value(),
    };

    let obj: Object<'_> = if let Some(ctor) = ctor {
      let ctor = ctor.restore(&ctx).map_err(|e| ScriptError::internal(e.to_string()))?;
      let opts = Object::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?;
      opts
        .set("parameters", params_val.clone())
        .map_err(|e| ScriptError::internal(e.to_string()))?;
      ctor
        .construct::<_, Object<'_>>((opts,))
        .map_err(|e| ScriptError::internal(format!("World constructor: {e}")))?
    } else {
      Object::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?
    };

    obj
      .set("parameters", params_val)
      .map_err(|e| ScriptError::internal(e.to_string()))?;
    // Native Cucumber `this.attach` / `this.log` — queue into the
    // registry; the BDD layer drains them into the test result.
    let attach = Function::new(ctx.clone(), |args: Rest<Value<'_>>| register_attachment(&args.0, false))
      .map_err(|e| ScriptError::internal(e.to_string()))?;
    let log = Function::new(ctx.clone(), |args: Rest<Value<'_>>| register_attachment(&args.0, true))
      .map_err(|e| ScriptError::internal(e.to_string()))?;
    obj.set("attach", attach).map_err(|e| ScriptError::internal(e.to_string()))?;
    obj.set("log", log).map_err(|e| ScriptError::internal(e.to_string()))?;
    // Cucumber `this.skip()` — throws a sentinel the step bridge maps
    // to `Skipped` (cucumber aborts the step as skipped on throw).
    let skip = Function::new(ctx.clone(), || -> rquickjs::Result<()> {
      Err(rquickjs::Error::new_from_js_message("World", "Error", SKIP_SENTINEL.to_string()))
    })
    .map_err(|e| ScriptError::internal(e.to_string()))?;
    obj.set("skip", skip).map_err(|e| ScriptError::internal(e.to_string()))?;

    if let Some(page) = world.page {
      install_page_on(&ctx, &obj, page, route_vm.clone()).map_err(|e| ScriptError::internal(e.to_string()))?;
    }
    if let Some(c) = world.context {
      install_browser_context_on(&ctx, &obj, c).map_err(|e| ScriptError::internal(e.to_string()))?;
    }
    if let Some(r) = world.request {
      install_request_on(&ctx, &obj, r).map_err(|e| ScriptError::internal(e.to_string()))?;
    }
    if let Some(b) = world.browser {
      install_browser_on(&ctx, &obj, b).map_err(|e| ScriptError::internal(e.to_string()))?;
    }

    let saved = Persistent::save(&ctx, obj);
    with_registry(&ctx, |reg| reg.current_world = Some(saved))
  })
  .await?
}

/// Drop the per-scenario World (cucumber builds a fresh one per
/// scenario). The next [`set_scenario_world`] installs a new one.
pub async fn reset_world(vm: &crate::vm::VmHandle) -> Result<(), ScriptError> {
  crate::vm_with!(vm => |ctx| {
    with_registry(&ctx, |reg| {
      reg.current_world = None;
      reg.attachments.clear();
    })
  })
  .await?
}

/// A cucumber-extracted step argument, lowered directly to a JS value
/// (never through `serde_json` — a transitive dep may enable
/// `serde_json/arbitrary_precision`, which would turn numbers into
/// objects).
#[derive(Debug, Clone)]
pub enum JsArg {
  Str(String),
  Int(i64),
  Float(f64),
  /// A custom cucumber-expression parameter. If its parameter type was
  /// defined with a `transformer`, that JS fn runs on `raw` at step
  /// invocation and the result is passed to the step; otherwise `raw`
  /// is passed as a string.
  Custom {
    type_name: String,
    raw: String,
  },
}

/// Outcome of a JS step/hook beyond plain pass (cucumber return
/// protocol: returning the string `'pending'`/`'skipped'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
  Passed,
  Pending,
  Skipped,
}

/// Invoke step `idx` with cucumber-extracted args, the optional data
/// table and doc string, against the current World. A thrown JS error
/// becomes a [`ScriptError`] carrying the `.js` location.
pub async fn invoke_step(
  vm: &crate::vm::VmHandle,
  idx: usize,
  params: &[JsArg],
  data_table: Option<&[Vec<String>]>,
  doc_string: Option<&str>,
  source: &str,
) -> Result<StepOutcome, ScriptError> {
  let params = params.to_vec();
  let data_table = data_table.map(<[Vec<String>]>::to_vec);
  let doc_string = doc_string.map(str::to_string);
  let source = source.to_string();

  crate::vm_with!(vm => |ctx| {
    let (func, world, wrapper, timeout_ms) = with_registry(&ctx, |reg| {
      let step = reg
        .steps
        .get(idx)
        .ok_or_else(|| ScriptError::internal(format!("step index {idx} out of range")))?;
      let t = step.timeout_ms.or(Some(reg.default_timeout_ms)).filter(|&v| v > 0);
      Ok::<_, ScriptError>((step.func.clone(), reg.current_world.clone(), reg.def_fn_wrapper.clone(), t))
    })??;

    let mut func = func.restore(&ctx).map_err(|e| ScriptError::internal(e.to_string()))?;
    // `setDefinitionFunctionWrapper`: replace the step body with
    // `wrapper(stepFn)` (cucumber-js cross-cut hook).
    if let Some(w) = wrapper {
      let w = w.restore(&ctx).map_err(|e| ScriptError::internal(e.to_string()))?;
      func = w
        .call::<_, Function<'_>>((func.clone(),))
        .catch(&ctx)
        .map_err(|e| caught_to_script_error(e, &source))?;
    }
    let world_obj = match world {
      Some(w) => w.restore(&ctx).map_err(|e| ScriptError::internal(e.to_string()))?,
      None => Object::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?,
    };

    // The per-scenario World is always the FIRST positional argument
    // and is also bound as `this`. Same shape for every body — arrow,
    // classic `function`, async, shorthand methods.
    //
    //   Given("I have {int} cukes", (world, n) => { world.count = n; })
    //   Given("I have {int} cukes", function (world, n) { this.count = n; })
    //
    // Both work identically; the second form just chooses to use `this`
    // instead of the first arg.
    let n = 1 + params.len() + usize::from(data_table.is_some()) + usize::from(doc_string.is_some());
    let mut args = Args::new(ctx.clone(), n);
    args
      .this(world_obj.clone())
      .map_err(|e| ScriptError::internal(e.to_string()))?;
    args
      .push_arg(world_obj)
      .map_err(|e| ScriptError::internal(e.to_string()))?;
    for p in &params {
      match p {
        JsArg::Str(s) => args.push_arg(s.as_str()).map_err(|e| ScriptError::internal(e.to_string()))?,
        JsArg::Int(i) => args.push_arg(*i).map_err(|e| ScriptError::internal(e.to_string()))?,
        JsArg::Float(f) => args.push_arg(*f).map_err(|e| ScriptError::internal(e.to_string()))?,
        JsArg::Custom { type_name, raw } => {
          // Apply the parameter type's JS `transformer` (if any) here,
          // in the live ctx, at step invocation — same place cucumber-js
          // transforms. No transformer ⇒ pass the raw string.
          let tx = with_registry(&ctx, |reg| {
            reg
              .param_types
              .iter()
              .find(|pt| &pt.name == type_name)
              .and_then(|pt| pt.transformer.clone())
          })?;
          match tx {
            Some(saved) => {
              let f = saved.restore(&ctx).map_err(|e| ScriptError::internal(e.to_string()))?;
              let call: rquickjs::Result<rquickjs::promise::MaybePromise<'_>> = f.call((raw.as_str(),));
              let mp = call.catch(&ctx).map_err(|e| caught_to_script_error(e, &source))?;
              let v: Value<'_> = mp
                .into_future::<Value<'_>>()
                .await
                .catch(&ctx)
                .map_err(|e| caught_to_script_error(e, &source))?;
              args.push_arg(v).map_err(|e| ScriptError::internal(e.to_string()))?;
            },
            None => args
              .push_arg(raw.as_str())
              .map_err(|e| ScriptError::internal(e.to_string()))?,
          }
        },
      }
    }
    if let Some(rows) = data_table {
      let dt = Class::instance(ctx.clone(), DataTableJs { rows }).map_err(|e| ScriptError::internal(e.to_string()))?;
      args.push_arg(dt).map_err(|e| ScriptError::internal(e.to_string()))?;
    }
    if let Some(s) = doc_string {
      args.push_arg(s).map_err(|e| ScriptError::internal(e.to_string()))?;
    }

    let called: rquickjs::Result<rquickjs::promise::MaybePromise<'_>> = args.apply(&func);
    let mp = match called.catch(&ctx) {
      Ok(v) => v,
      Err(e) => return Err(caught_to_script_error(e, &source)),
    };
    // Per-step (or registry-default) timeout — JS steps had none before.
    let fut = mp.into_future::<Value<'_>>();
    let awaited = match timeout_ms {
      Some(t) => match tokio::time::timeout(std::time::Duration::from_millis(t), fut).await {
        Ok(r) => r,
        Err(_) => return Err(ScriptError::timeout(t, t)),
      },
      None => fut.await,
    };
    let resolved: Value<'_> = match awaited.catch(&ctx) {
      Ok(v) => v,
      Err(e) => {
        let se = caught_to_script_error(e, &source);
        // `this.skip()` throws the sentinel → cucumber-style Skipped,
        // not a failure.
        if se.message.contains(SKIP_SENTINEL) {
          return Ok(StepOutcome::Skipped);
        }
        return Err(se);
      },
    };
    let marker = resolved.as_string().and_then(|s| s.to_string().ok());
    Ok(match marker.as_deref() {
      Some("pending") => StepOutcome::Pending,
      Some("skipped") => StepOutcome::Skipped,
      _ => StepOutcome::Passed,
    })
  })
  .await?
}

/// Invoke hook `idx`. Same bridge as [`invoke_step`].
pub async fn invoke_hook(
  vm: &crate::vm::VmHandle,
  idx: usize,
  arg: Option<&HookArg>,
  source: &str,
) -> Result<StepOutcome, ScriptError> {
  let source = source.to_string();
  let arg = arg.cloned();
  crate::vm_with!(vm => |ctx| {
    let (func, world, timeout_ms) = with_registry(&ctx, |reg| {
      let hook = reg
        .hooks
        .get(idx)
        .ok_or_else(|| ScriptError::internal(format!("hook index {idx} out of range")))?;
      let t = hook.timeout_ms.or(Some(reg.default_timeout_ms)).filter(|&v| v > 0);
      Ok::<_, ScriptError>((hook.func.clone(), reg.current_world.clone(), t))
    })??;
    let func = func.restore(&ctx).map_err(|e| ScriptError::internal(e.to_string()))?;
    let world_obj = match world {
      Some(w) => w.restore(&ctx).map_err(|e| ScriptError::internal(e.to_string()))?,
      None => Object::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?,
    };
    // Hooks: World is always arg[0] and `this`. The cucumber-shaped
    // hook parameter (`{ pickle, result }`) follows as arg[1] when
    // present (`After(world, hookInfo)`).
    let n_args = 1 + usize::from(arg.is_some());
    let mut args = Args::new(ctx.clone(), n_args);
    args
      .this(world_obj.clone())
      .map_err(|e| ScriptError::internal(e.to_string()))?;
    args
      .push_arg(world_obj)
      .map_err(|e| ScriptError::internal(e.to_string()))?;
    if let Some(a) = arg {
      let param = Object::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?;
      let pickle = Object::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?;
      pickle.set("name", a.name).map_err(|e| ScriptError::internal(e.to_string()))?;
      let tags = rquickjs::Array::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?;
      for (i, t) in a.tags.iter().enumerate() {
        let to = Object::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?;
        to.set("name", t.clone()).map_err(|e| ScriptError::internal(e.to_string()))?;
        tags.set(i, to).map_err(|e| ScriptError::internal(e.to_string()))?;
      }
      pickle.set("tags", tags).map_err(|e| ScriptError::internal(e.to_string()))?;
      let result = Object::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?;
      result.set("status", a.status).map_err(|e| ScriptError::internal(e.to_string()))?;
      if let Some(m) = a.message {
        result.set("message", m).map_err(|e| ScriptError::internal(e.to_string()))?;
      }
      param.set("pickle", pickle).map_err(|e| ScriptError::internal(e.to_string()))?;
      param.set("result", result).map_err(|e| ScriptError::internal(e.to_string()))?;
      args.push_arg(param).map_err(|e| ScriptError::internal(e.to_string()))?;
    }
    let called: rquickjs::Result<rquickjs::promise::MaybePromise<'_>> = args.apply(&func);
    let mp = match called.catch(&ctx) {
      Ok(v) => v,
      Err(e) => return Err(caught_to_script_error(e, &source)),
    };
    let fut = mp.into_future::<Value<'_>>();
    let awaited = match timeout_ms {
      Some(t) => match tokio::time::timeout(std::time::Duration::from_millis(t), fut).await {
        Ok(r) => r,
        Err(_) => return Err(ScriptError::timeout(t, t)),
      },
      None => fut.await,
    };
    if let Err(e) = awaited.catch(&ctx) {
      return Err(caught_to_script_error(e, &source));
    }
    Ok(StepOutcome::Passed)
  })
  .await?
}
