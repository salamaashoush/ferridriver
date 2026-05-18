//! Cucumber step-definition surface for the shared QuickJS engine.
//!
//! The same VM that runs `ferridriver run` scripts and MCP `run_script`
//! also loads cucumber-js-shaped `.js` step files. `Given`/`When`/
//! `Then`/`Before`/`After`/`defineParameterType`/... are native Rust
//! functions (no JS glue); registrations land in a Rust [`BddRegistry`]
//! held as context userdata (the QuickJS context is single-threaded, so
//! a `RefCell` is the right interior mutability — no `Arc`/`Mutex`).
//! Step bodies are kept as [`Persistent`] functions and called back by
//! the Rust `ferridriver-bdd` core with cucumber-extracted arguments, a
//! real [`DataTableJs`] and the per-scenario World as `this`.
//!
//! No business logic here: matching, outline expansion, tag filtering
//! and hook ordering all stay in the `ferridriver-bdd` core.

use std::cell::RefCell;
use std::sync::Arc;

use rquickjs::class::{Class, Trace};
use rquickjs::function::{Args, Constructor, Func, Opt, Rest};
use rquickjs::{AsyncContext, CatchResultExt, Ctx, Function, JsLifetime, Object, Persistent, Value, async_with};

use crate::bindings::convert::serde_to_js;
use crate::bindings::{install_browser_context_on, install_browser_on, install_page_on, install_request_on};
use crate::engine::caught_to_script_error;
use crate::error::ScriptError;

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
}

struct HookReg {
  kind: String,
  tags: Option<String>,
  func: Persistent<Function<'static>>,
}

struct ParamTypeReg {
  name: String,
  regexp: String,
}

/// Rust-side cucumber registry, populated by the native `Given`/`When`/
/// ... functions while the user's step `.js` evaluates.
#[derive(Default)]
struct BddRegistry {
  steps: Vec<StepReg>,
  hooks: Vec<HookReg>,
  param_types: Vec<ParamTypeReg>,
  default_timeout_ms: u64,
  world_ctor: Option<Persistent<Constructor<'static>>>,
  current_world: Option<Persistent<Object<'static>>>,
}

/// Context userdata holding the registry. Single-threaded VM ⇒
/// `RefCell`, never `Arc`/`Mutex`.
struct BddUserData(RefCell<BddRegistry>);

// SAFETY: holds only `'static` data (`Persistent<…>` handles and owned
// values), so re-stating the unused `'js` lifetime is sound — same
// rationale as `SessionAsyncCtx`.
#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for BddUserData {
  type Changed<'to> = BddUserData;
}

fn with_registry<R>(ctx: &Ctx<'_>, f: impl FnOnce(&mut BddRegistry) -> R) -> Result<R, ScriptError> {
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
  let saved = Persistent::save(&ctx, func);
  with_registry(&ctx, |reg| {
    reg.steps.push(StepReg {
      kind,
      pattern: pat,
      is_regex,
      func: saved,
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
  let saved = Persistent::save(&ctx, func);
  with_registry(&ctx, |reg| {
    reg.hooks.push(HookReg {
      kind: kind.to_string(),
      tags,
      func: saved,
    });
  })
  .map_err(|e| rq(&e))
}

/// Install the native cucumber surface and the shared registry as
/// context userdata. Idempotent; called once at `Session::create`
/// next to `install_plugins`.
pub fn install_bdd(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  if ctx.userdata::<BddUserData>().is_some() {
    return Ok(());
  }
  let _ = ctx.store_userdata(BddUserData(RefCell::new(BddRegistry {
    default_timeout_ms: 5000,
    ..BddRegistry::default()
  })));

  let g = ctx.globals();
  Class::<DataTableJs>::define(&g)?;

  for (name, kind) in [
    ("Given", StepKind::Given),
    ("When", StepKind::When),
    ("Then", StepKind::Then),
    ("defineStep", StepKind::Step),
    ("And", StepKind::Step),
    ("But", StepKind::Step),
  ] {
    g.set(
      name,
      Func::from(move |args: Rest<Value<'_>>| register_step(kind, &args.0)),
    )?;
  }

  for hook in ["Before", "After", "BeforeAll", "AfterAll", "BeforeStep", "AfterStep"] {
    g.set(
      hook,
      Func::from(move |args: Rest<Value<'_>>| register_hook(hook, &args.0)),
    )?;
  }

  g.set(
    "defineParameterType",
    Func::from(|def: Object<'_>| -> rquickjs::Result<()> {
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
      with_registry(&ctx, |reg| reg.param_types.push(ParamTypeReg { name, regexp })).map_err(|e| rq(&e))
    }),
  )?;

  g.set(
    "setDefaultTimeout",
    Func::from(|ctx: Ctx<'_>, ms: f64| -> rquickjs::Result<()> {
      with_registry(&ctx, |reg| reg.default_timeout_ms = ms.max(0.0) as u64).map_err(|e| rq(&e))
    }),
  )?;
  g.set(
    "setWorldConstructor",
    Func::from(|c: Constructor<'_>| -> rquickjs::Result<()> {
      let ctx = c.ctx().clone();
      let saved = Persistent::save(&ctx, c);
      with_registry(&ctx, |reg| reg.world_ctor = Some(saved)).map_err(|e| rq(&e))
    }),
  )?;
  g.set("setParallelCanAssign", Func::from(|_: Opt<Value<'_>>| {}))?;

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
pub async fn collect_registry(actx: &AsyncContext) -> Result<CollectedRegistry, ScriptError> {
  async_with!(actx => |ctx| {
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
  .await
}

/// Per-scenario fixtures the BDD core threads onto the JS World — the
/// same handles `RunContext` carries for scripting, installed onto a
/// per-scenario World object rather than `globalThis`.
#[derive(Clone, Default)]
pub struct ScenarioWorld {
  pub page: Option<Arc<ferridriver::Page>>,
  pub context: Option<Arc<ferridriver::context::ContextRef>>,
  pub request: Option<Arc<ferridriver::api_request::APIRequestContext>>,
  pub browser: Option<Arc<ferridriver::Browser>>,
}

/// Build the per-scenario World and make it the `this` steps run
/// against. If `setWorldConstructor` was used, that class is
/// constructed and the fixtures are augmented onto the instance.
pub async fn set_scenario_world(actx: &AsyncContext, world: &ScenarioWorld) -> Result<(), ScriptError> {
  let world = world.clone();
  let route_ctx = actx.clone();
  async_with!(actx => |ctx| {
    let ctor = with_registry(&ctx, |reg| reg.world_ctor.clone())?;

    let obj: Object<'_> = if let Some(ctor) = ctor {
      let ctor = ctor.restore(&ctx).map_err(|e| ScriptError::internal(e.to_string()))?;
      let opts = Object::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?;
      ctor
        .construct::<_, Object<'_>>((opts,))
        .map_err(|e| ScriptError::internal(format!("World constructor: {e}")))?
    } else {
      Object::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?
    };

    if obj.get::<_, Value<'_>>("parameters").map_or(true, |v| v.is_undefined()) {
      let params = Object::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?;
      obj.set("parameters", params).map_err(|e| ScriptError::internal(e.to_string()))?;
    }
    let noop = Function::new(ctx.clone(), |_: Rest<Value<'_>>| {}).map_err(|e| ScriptError::internal(e.to_string()))?;
    if obj.get::<_, Value<'_>>("attach").map_or(true, |v| v.is_undefined()) {
      obj.set("attach", noop.clone()).map_err(|e| ScriptError::internal(e.to_string()))?;
    }
    if obj.get::<_, Value<'_>>("log").map_or(true, |v| v.is_undefined()) {
      obj.set("log", noop).map_err(|e| ScriptError::internal(e.to_string()))?;
    }

    if let Some(page) = world.page {
      install_page_on(&ctx, &obj, page, route_ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?;
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
  .await
}

/// Drop the per-scenario World (cucumber builds a fresh one per
/// scenario). The next [`set_scenario_world`] installs a new one.
pub async fn reset_world(actx: &AsyncContext) -> Result<(), ScriptError> {
  async_with!(actx => |ctx| {
    with_registry(&ctx, |reg| reg.current_world = None)
  })
  .await
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
  actx: &AsyncContext,
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

  async_with!(actx => |ctx| {
    let (func, world) = with_registry(&ctx, |reg| {
      let step = reg
        .steps
        .get(idx)
        .ok_or_else(|| ScriptError::internal(format!("step index {idx} out of range")))?;
      Ok::<_, ScriptError>((step.func.clone(), reg.current_world.clone()))
    })??;

    let func = func.restore(&ctx).map_err(|e| ScriptError::internal(e.to_string()))?;
    let this = match world {
      Some(w) => w.restore(&ctx).map_err(|e| ScriptError::internal(e.to_string()))?,
      None => Object::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?,
    };

    let n = params.len() + usize::from(data_table.is_some()) + usize::from(doc_string.is_some());
    let mut args = Args::new(ctx.clone(), n);
    args.this(this).map_err(|e| ScriptError::internal(e.to_string()))?;
    for p in &params {
      match p {
        JsArg::Str(s) => args.push_arg(s.as_str()),
        JsArg::Int(i) => args.push_arg(*i),
        JsArg::Float(f) => args.push_arg(*f),
      }
      .map_err(|e| ScriptError::internal(e.to_string()))?;
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
    let resolved: Value<'_> = match mp.into_future::<Value<'_>>().await.catch(&ctx) {
      Ok(v) => v,
      Err(e) => return Err(caught_to_script_error(e, &source)),
    };
    let marker = resolved.as_string().and_then(|s| s.to_string().ok());
    Ok(match marker.as_deref() {
      Some("pending") => StepOutcome::Pending,
      Some("skipped") => StepOutcome::Skipped,
      _ => StepOutcome::Passed,
    })
  })
  .await
}

/// Invoke hook `idx`. Same bridge as [`invoke_step`].
pub async fn invoke_hook(actx: &AsyncContext, idx: usize, source: &str) -> Result<StepOutcome, ScriptError> {
  let source = source.to_string();
  async_with!(actx => |ctx| {
    let (func, world) = with_registry(&ctx, |reg| {
      let hook = reg
        .hooks
        .get(idx)
        .ok_or_else(|| ScriptError::internal(format!("hook index {idx} out of range")))?;
      Ok::<_, ScriptError>((hook.func.clone(), reg.current_world.clone()))
    })??;
    let func = func.restore(&ctx).map_err(|e| ScriptError::internal(e.to_string()))?;
    let this = match world {
      Some(w) => w.restore(&ctx).map_err(|e| ScriptError::internal(e.to_string()))?,
      None => Object::new(ctx.clone()).map_err(|e| ScriptError::internal(e.to_string()))?,
    };
    let mut args = Args::new(ctx.clone(), 0);
    args.this(this).map_err(|e| ScriptError::internal(e.to_string()))?;
    let called: rquickjs::Result<rquickjs::promise::MaybePromise<'_>> = args.apply(&func);
    let mp = match called.catch(&ctx) {
      Ok(v) => v,
      Err(e) => return Err(caught_to_script_error(e, &source)),
    };
    if let Err(e) = mp.into_future::<Value<'_>>().await.catch(&ctx) {
      return Err(caught_to_script_error(e, &source));
    }
    Ok(StepOutcome::Passed)
  })
  .await
}
