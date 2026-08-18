//! Running a JavaScript reporter.
//!
//! `reporter = [{ name = "./reporters/my-reporter.ts" }]` names a module
//! whose default export is a reporter class. This is Playwright's
//! `loadReporter` + `wrapReporterAsV2` + `Multiplexer` isolation, over
//! ferridriver's own event stream: the module is bundled and compiled
//! through the same rolldown -> bytecode pipeline every other script
//! takes, instantiated with `new`, and driven from the core
//! [`Reporter`] trait.
//!
//! Nothing here decides what a suite, a title path, an outcome or an
//! attempt's errors ARE — [`ferridriver_test::reporter::api`] does, and
//! this module only lifts those structs into JS objects and calls the
//! reporter's hooks. Playwright ref:
//! `packages/playwright/src/reporters/reporterV2.ts`,
//! `packages/playwright/src/reporters/multiplexer.ts`,
//! `packages/playwright/src/runner/reporters.ts`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rquickjs::function::This;
use rquickjs::{CatchResultExt, Ctx, Function, IntoJs, JsLifetime, Object, Persistent, TypedArray, Value};

use ferridriver_test::config::{ReporterConfig, TestConfig};
use ferridriver_test::reporter::{Reporter, ReporterEvent, RunStatus, api};

use crate::bundle::{CompiledBundle, bundle_and_compile_named, eval_bundle_with};
use crate::engine::{ExtensionHost, RunContext, ScriptCaps, ScriptEngineConfig, Session};
use crate::error::ScriptError;
use crate::vm::VmHandle;

/// Which reporter interface a module's default export implements.
///
/// Playwright's `wrapReporterAsV2`: a `version()` that answers `'v2'`
/// is the V2 interface and takes `onBegin(suite)`; anything else is V1
/// and takes `onBegin(config, suite)` and never sees `onConfigure`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Protocol {
  #[default]
  V1,
  V2,
}

/// What a V1 reporter is told only after `onBegin`. Playwright's
/// `ReporterV2Wrapper._deferred`: a V1 reporter has no `onConfigure`,
/// so an error or a chunk that arrives before the suite exists would
/// reach it before it knows the config.
enum Deferred {
  Error(api::ReportedError),
  Stdio {
    stderr: bool,
    text: String,
    case: String,
    attempt: u32,
  },
}

#[derive(Default)]
struct ReporterState {
  instance: Option<Persistent<Object<'static>>>,
  config: Option<Persistent<Value<'static>>>,
  suite: Option<Persistent<Object<'static>>>,
  /// Everything the run has built so far, keyed by [`case_key`],
  /// [`result_key`] and [`step_key`]. One JS object, so the whole graph
  /// is reachable from a single `Persistent` and the collector traces
  /// it without any Rust-side handle to a JS value.
  index: Option<Persistent<Object<'static>>>,
  protocol: Protocol,
  began: bool,
  deferred: Vec<Deferred>,
  /// `onEnd` returning `{ status }` — Playwright lets a reporter change
  /// how the run is reported to have ended.
  status_override: Option<String>,
  /// What the reporter asked for during `preprocess`, and whether it is
  /// still allowed to ask. Playwright refuses every `TestRun` method
  /// once `preprocess` has returned.
  edits: ferridriver_test::reporter::TestRunEdits,
  preprocessing: bool,
}

struct ReporterUd(RefCell<ReporterState>);

// SAFETY: holds only `'static` data (`Persistent` handles and owned
// values), the same rationale as the extension registry's userdata.
#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for ReporterUd {
  type Changed<'to> = ReporterUd;
}

fn state<R>(ctx: &Ctx<'_>, f: impl FnOnce(&mut ReporterState) -> R) -> Result<R, ScriptError> {
  let ud = ctx
    .userdata::<ReporterUd>()
    .ok_or_else(|| ScriptError::internal("reporter state not installed".to_string()))?;
  let mut held = ud.0.borrow_mut();
  Ok(f(&mut held))
}

fn case_key(id: &str) -> String {
  format!("c:{id}")
}

fn result_key(id: &str, attempt: u32) -> String {
  format!("r:{id}:{attempt}")
}

fn step_key(id: &str, attempt: u32, step_id: &str) -> String {
  format!("s:{id}:{attempt}:{step_id}")
}

// ── Loading ──

/// A reporter module, compiled once for the run.
///
/// Compilation happens where it can fail loudly — before the first test
/// — exactly as Playwright's `loadReporter` runs before the run starts.
/// The VM itself is created lazily, so a set of reporters built per run
/// (watch mode, the test server) each get their own instance.
pub struct ReporterModule {
  /// The name as configured, for diagnostics.
  pub label: String,
  options: serde_json::Value,
  bundle: Arc<CompiledBundle>,
  cwd: PathBuf,
  caps: ScriptCaps,
  /// `printsToStdio()` as the module's own instance answered it, probed
  /// at load time — the default-terminal decision is made before any
  /// reporter instance exists.
  prints_to_stdio: bool,
  /// The instance [`load`]'s probe built, handed to the first reporter
  /// this module produces rather than torn down and rebuilt.
  probe: std::sync::Mutex<Option<Live>>,
}

/// Whether `name` looks like a module path rather than a built-in
/// reporter name. Playwright treats anything outside its built-in table
/// as a path; ferridriver keeps that but refuses to guess about a bare
/// word, which is far more likely a typo than a file.
#[must_use]
pub fn looks_like_module(name: &str) -> bool {
  name.starts_with('.') || name.starts_with('/') || crate::discover::is_source_file(Path::new(name))
}

/// Resolve a reporter name to a file. Playwright resolves against
/// `config.rootDir`; a name typed on the command line is relative to
/// the working directory, so both are tried, in that order.
fn resolve(name: &str, config: &TestConfig, cwd: &Path) -> Option<PathBuf> {
  let candidate = Path::new(name);
  if candidate.is_absolute() {
    return candidate.is_file().then(|| candidate.to_path_buf());
  }
  let mut roots = vec![cwd.to_path_buf()];
  if let Some(dir) = &config.test_dir {
    let root = Path::new(dir);
    roots.push(if root.is_absolute() {
      root.to_path_buf()
    } else {
      cwd.join(root)
    });
  }
  roots
    .into_iter()
    .map(|root| root.join(candidate))
    .find(|path| path.is_file())
}

/// Compile one reporter module and probe the two things the reporter
/// set has to know before the run: which interface it implements and
/// whether it writes to the terminal.
///
/// # Errors
///
/// Fails when the name resolves to no file, the module does not bundle,
/// its top level throws, or its default export is not a constructor.
pub async fn load(
  entry: &ReporterConfig,
  config: &TestConfig,
  cwd: &Path,
  caps: ScriptCaps,
) -> Result<ReporterModule, ScriptError> {
  let path = resolve(&entry.name, config, cwd).ok_or_else(|| {
    ScriptError::internal(format!(
      "reporter '{}' is neither a known reporter name nor a file that exists",
      entry.name
    ))
  })?;
  let bundle = bundle_and_compile_named(
    std::slice::from_ref(&path),
    cwd,
    &format!("reporter:{}", path.display()),
  )
  .await?;

  let options = options_value(entry, config, cwd);
  let mut module = ReporterModule {
    label: entry.name.clone(),
    options,
    bundle: Arc::new(bundle),
    cwd: cwd.to_path_buf(),
    caps,
    prints_to_stdio: true,
    probe: std::sync::Mutex::new(None),
  };
  // The probe instance is the one the first reporter of this module
  // uses, so loading costs one VM, not two.
  let live = module.start().await?;
  module.prints_to_stdio = live.prints_to_stdio;
  *module.probe.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(live);
  Ok(module)
}

/// The options bag the reporter class is constructed with: the entry's
/// own options plus `configDir`, which every Playwright reporter that
/// writes a file resolves its output path against.
fn options_value(entry: &ReporterConfig, config: &TestConfig, cwd: &Path) -> serde_json::Value {
  let mut map = serde_json::Map::new();
  for (key, value) in &entry.options {
    map.insert(key.clone(), value.clone());
  }
  let config_dir = config
    .test_dir
    .as_ref()
    .map_or_else(|| cwd.display().to_string(), Clone::clone);
  map.entry("configDir".to_string()).or_insert(config_dir.into());
  map
    .entry("outputDir".to_string())
    .or_insert(config.output_dir.display().to_string().into());
  serde_json::Value::Object(map)
}

/// A reporter module's live VM and the instance in it.
struct Live {
  /// Held for its `Drop`: dropping the session ends the VM event loop.
  _session: Session,
  vm: VmHandle,
  prints_to_stdio: bool,
}

impl ReporterModule {
  /// Create the VM, evaluate the module, `new` its default export and
  /// probe its interface.
  async fn start(&self) -> Result<Live, ScriptError> {
    let sandbox = Arc::new(crate::fs::PathSandbox::new(&self.cwd)?);
    let run_ctx = RunContext {
      vars: Arc::new(crate::vars::InMemoryVars::new()),
      sandbox,
      artifacts: None,
      page: None,
      browser_context: None,
      request: None,
      browser: None,
      extensions: Vec::new(),
      host: ExtensionHost::Test,
      caps: self.caps.clone(),
      session: None,
    };
    let session = Session::create(ScriptEngineConfig::default(), &run_ctx).await?;
    let vm = session.vm_handle();
    let options = self.options.clone();
    let label = self.label.clone();

    crate::vm_with!(vm => |ctx| {
      let _ = ctx.store_userdata(ReporterUd(RefCell::new(ReporterState::default())));
      Ok::<(), ScriptError>(())
    })
    .await??;

    eval_bundle_with(&vm, &self.bundle, move |ctx, namespace| {
      let default: Value<'_> = namespace
        .get("default")
        .map_err(|e| ScriptError::internal(format!("reporter '{label}': reading its default export: {e}")))?;
      let ctor = default.clone().try_into_constructor().map_err(|_| {
        ScriptError::internal(format!(
          "reporter '{label}': its default export is not a class — a reporter module exports the reporter \
           class as `export default`"
        ))
      })?;
      let options = crate::bindings::convert::json_to_js(ctx, &options)
        .map_err(|e| ScriptError::internal(format!("reporter '{label}': building its options bag: {e}")))?;
      let instance: Object<'_> = ctor
        .construct((options,))
        .catch(ctx)
        .map_err(|e| crate::engine::caught_to_script_error(e, &label))?;
      let protocol = probe_protocol(&instance);
      state(ctx, |s| {
        s.protocol = protocol;
        s.instance = Some(Persistent::save(ctx, instance));
      })
    })
    .await?;

    let prints_to_stdio = crate::vm_with!(vm => |ctx| {
      let instance = state(&ctx, |s| s.instance.clone())?;
      let Some(instance) = instance else {
        return Ok::<bool, ScriptError>(true);
      };
      let instance = instance.restore(&ctx).map_err(|e| ScriptError::internal(e.to_string()))?;
      // Playwright defaults a MISSING `printsToStdio` to true, in both
      // the V1 wrapper and the multiplexer.
      Ok(match method(&instance, "printsToStdio") {
        Some(f) => f.call::<_, bool>((This(instance.clone()),)).unwrap_or(true),
        None => true,
      })
    })
    .await??;

    Ok(Live {
      _session: session,
      vm,
      prints_to_stdio,
    })
  }

  /// A reporter driven by this module. The instance is built on the
  /// first event, unless [`load`]'s probe instance is still unclaimed.
  #[must_use]
  pub fn reporter(self: &Arc<Self>) -> JsReporter {
    let live = self
      .probe
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take();
    JsReporter {
      module: Arc::clone(self),
      live,
      attempts: rustc_hash::FxHashMap::default(),
      run_start: std::time::SystemTime::UNIX_EPOCH,
      status_override: None,
    }
  }

  #[must_use]
  pub fn prints_to_stdio(&self) -> bool {
    self.prints_to_stdio
  }
}

fn probe_protocol(instance: &Object<'_>) -> Protocol {
  // Playwright reads `version()` inside a try/catch and treats a throw
  // as V1.
  match method(instance, "version") {
    Some(f) => match f.call::<_, String>((This(instance.clone()),)) {
      Ok(v) if v == "v2" => Protocol::V2,
      _ => Protocol::V1,
    },
    None => Protocol::V1,
  }
}

fn method<'js>(instance: &Object<'js>, name: &str) -> Option<Function<'js>> {
  instance.get::<_, Value<'js>>(name).ok().and_then(Value::into_function)
}

// ── The reporter ──

/// One JS reporter, driven from the core event stream.
pub struct JsReporter {
  module: Arc<ReporterModule>,
  live: Option<Live>,
  /// The attempt each case is currently on. Step and output events name
  /// the test but not the attempt, exactly as Playwright's own worker
  /// protocol does — the attempt is the one `onTestBegin` opened.
  attempts: rustc_hash::FxHashMap<String, u32>,
  run_start: std::time::SystemTime,
  status_override: Option<RunStatus>,
}

impl JsReporter {
  async fn vm(&mut self) -> Option<VmHandle> {
    if self.live.is_none() {
      match self.module.start().await {
        Ok(live) => self.live = Some(live),
        Err(e) => {
          tracing::error!(
            target: "ferridriver::reporter",
            reporter = %self.module.label,
            error = %e.message,
            "reporter failed to start; its hooks will not run",
          );
          return None;
        },
      }
    }
    self.live.as_ref().map(|live| live.vm.clone())
  }

  fn failed(&self, hook: &str, error: &ScriptError) {
    // Playwright's multiplexer catches every reporter callback and
    // reports it as a run error rather than failing the run.
    tracing::error!(
      target: "ferridriver::reporter",
      reporter = %self.module.label,
      hook,
      error = %error.message,
      "reporter hook threw",
    );
  }
}

/// Hand `args` to the instance's `name` hook, if it has one. Every
/// throw is caught here: a reporter must never take the run down.
fn dispatch<'js>(ctx: &Ctx<'js>, name: &str, args: Vec<Value<'js>>) -> Result<(), ScriptError> {
  let instance = state(ctx, |s| s.instance.clone())?;
  let Some(instance) = instance else {
    return Ok(());
  };
  let instance = instance
    .restore(ctx)
    .map_err(|e| ScriptError::internal(e.to_string()))?;
  let Some(func) = method(&instance, name) else {
    return Ok(());
  };
  let mut call = rquickjs::function::Args::new(ctx.clone(), args.len() + 1);
  call.this(instance).map_err(|e| ScriptError::internal(e.to_string()))?;
  call.push_args(args).map_err(|e| ScriptError::internal(e.to_string()))?;
  func
    .call_arg::<Value<'js>>(call)
    .catch(ctx)
    .map(|_| ())
    .map_err(|e| crate::engine::caught_to_script_error(e, name))
}

// ── `preprocess`: the reporter edits the corpus ──

/// Playwright's `TestRun`, the object `preprocess` is handed. Every
/// method records into the reporter's Rust-side edits, which the runner
/// then applies to the plan.
fn test_run_obj<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  obj.set(
    "exclude",
    Function::new(ctx.clone(), |ctx: Ctx<'_>, target: Value<'_>| -> rquickjs::Result<()> {
      let ids = target_ids(&target);
      guard(&ctx, "exclude")?;
      let _ = state(&ctx, |s| s.edits.excluded.extend(ids));
      Ok(())
    })?,
  )?;
  obj.set(
    "skipSharding",
    Function::new(ctx.clone(), |ctx: Ctx<'_>| -> rquickjs::Result<()> {
      guard(&ctx, "skipSharding")?;
      let already = state(&ctx, |s| std::mem::replace(&mut s.edits.skip_sharding, true)).unwrap_or(false);
      if already {
        return Err(
          ctx.throw("Multiple reporters called 'skipSharding'. Only one reporter may handle sharding.".into_js(&ctx)?),
        );
      }
      Ok(())
    })?,
  )?;
  for kind in ["skip", "fixme", "fail"] {
    obj.set(
      kind,
      Function::new(
        ctx.clone(),
        move |ctx: Ctx<'_>, target: Value<'_>, reason: rquickjs::function::Opt<String>| -> rquickjs::Result<()> {
          guard(&ctx, kind)?;
          let ids = target_ids(&target);
          let reason = reason.0;
          let _ = state(&ctx, |s| {
            for id in ids {
              s.edits.annotations.push((id, annotation_for(kind, reason.clone())));
            }
          });
          Ok(())
        },
      )?,
    )?;
  }
  Ok(obj)
}

fn guard(ctx: &Ctx<'_>, method: &str) -> rquickjs::Result<()> {
  if state(ctx, |s| s.preprocessing).unwrap_or(false) {
    return Ok(());
  }
  let message = format!("TestRun.{method}() can only be called from Reporter.preprocess().");
  Err(ctx.throw(message.into_js(ctx)?))
}

fn annotation_for(kind: &str, reason: Option<String>) -> ferridriver_test::model::TestAnnotation {
  use ferridriver_test::model::TestAnnotation;
  match kind {
    "fixme" => TestAnnotation::Fixme {
      reason,
      condition: None,
    },
    "fail" => TestAnnotation::Fail {
      reason,
      condition: None,
    },
    _ => TestAnnotation::Skip {
      reason,
      condition: None,
    },
  }
}

fn collect_case_ids(suite: &Object<'_>, out: &mut Vec<String>) {
  for child in suite.get::<_, Vec<Object<'_>>>("suites").unwrap_or_default() {
    collect_case_ids(&child, out);
  }
  for case in suite.get::<_, Vec<Object<'_>>>("tests").unwrap_or_default() {
    if let Ok(id) = case.get::<_, String>("id") {
      out.push(id);
    }
  }
}

/// The case ids a `TestRun` target names: its own for a test, every one
/// underneath for a suite.
fn target_ids(target: &Value<'_>) -> Vec<String> {
  let Some(obj) = target.as_object() else {
    return Vec::new();
  };
  if let Ok(id) = obj.get::<_, String>("id") {
    return vec![id];
  }
  let mut out = Vec::new();
  collect_case_ids(obj, &mut out);
  out
}

// ── Lifting the API structs into JS objects ──

fn js_date<'js>(ctx: &Ctx<'js>, epoch_ms: i64) -> rquickjs::Result<Value<'js>> {
  let raw: Value<'js> = ctx.globals().get("Date")?;
  let ctor = raw
    .try_into_constructor()
    .map_err(|_| rquickjs::Error::new_from_js_message("reporter", "Date", "global Date is not a constructor"))?;
  ctor.construct((epoch_ms,))
}

fn loc_obj<'js>(ctx: &Ctx<'js>, loc: &api::Location) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  obj.set("file", loc.file.clone())?;
  obj.set("line", loc.line)?;
  obj.set("column", loc.column)?;
  Ok(obj)
}

fn annotation_objs<'js>(ctx: &Ctx<'js>, annotations: &[api::Annotation]) -> rquickjs::Result<Vec<Object<'js>>> {
  annotations
    .iter()
    .map(|a| {
      let obj = Object::new(ctx.clone())?;
      obj.set("type", a.kind.clone())?;
      if let Some(description) = &a.description {
        obj.set("description", description.clone())?;
      }
      Ok(obj)
    })
    .collect()
}

fn error_obj<'js>(ctx: &Ctx<'js>, error: &api::ReportedError) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  obj.set("message", error.message.clone())?;
  if let Some(stack) = &error.stack {
    obj.set("stack", stack.clone())?;
  }
  if let Some(location) = &error.location {
    obj.set("location", loc_obj(ctx, location)?)?;
  }
  if let Some(snippet) = &error.snippet {
    obj.set("snippet", snippet.clone())?;
  }
  Ok(obj)
}

fn attachment_obj<'js>(ctx: &Ctx<'js>, attachment: &api::Attachment) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  obj.set("name", attachment.name.clone())?;
  obj.set("contentType", attachment.content_type.clone())?;
  if let Some(path) = &attachment.path {
    obj.set("path", path.clone())?;
  }
  if let Some(body) = &attachment.body {
    // Playwright hands a Buffer, which the runtime has as a real
    // Uint8Array subclass — a reporter calling `body.toString('base64')`
    // needs the subclass, not the view.
    let bytes = TypedArray::<u8>::new(ctx.clone(), body.clone())?;
    let buffer: Option<Function<'js>> = ctx
      .globals()
      .get::<_, Value<'js>>("Buffer")
      .ok()
      .and_then(Value::into_object)
      .and_then(|b| b.get::<_, Value<'js>>("from").ok())
      .and_then(Value::into_function);
    match buffer {
      Some(from) => obj.set("body", from.call::<_, Value<'js>>((bytes,))?)?,
      None => obj.set("body", bytes)?,
    }
  }
  Ok(obj)
}

fn step_obj<'js>(ctx: &Ctx<'js>, step: &api::Step, parent: Option<&Object<'js>>) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  obj.set("title", step.title.clone())?;
  obj.set("category", step.category.clone())?;
  obj.set("duration", step.duration)?;
  obj.set("startTime", js_date(ctx, step.start_time)?)?;
  if let Some(error) = &step.error {
    obj.set("error", error_obj(ctx, error)?)?;
  }
  if let Some(location) = &step.location {
    obj.set("location", loc_obj(ctx, location)?)?;
  }
  obj.set("annotations", annotation_objs(ctx, &step.annotations)?)?;
  obj.set(
    "attachments",
    step
      .attachments
      .iter()
      .map(|a| attachment_obj(ctx, a))
      .collect::<rquickjs::Result<Vec<_>>>()?,
  )?;
  if let Some(parent) = parent {
    obj.set("parent", parent.clone())?;
  }
  let children: Vec<Object<'js>> = step
    .steps
    .iter()
    .map(|child| step_obj(ctx, child, Some(&obj)))
    .collect::<rquickjs::Result<_>>()?;
  obj.set("steps", children)?;
  obj.set("titlePath", Function::new(ctx.clone(), step_title_path)?)?;
  Ok(obj)
}

/// Playwright's `TestStep.titlePath()`: the enclosing steps' titles,
/// outermost first, then this step's own.
fn step_title_path(this: This<Object<'_>>) -> Vec<String> {
  let mut path = Vec::new();
  let mut node = Some(this.0);
  while let Some(current) = node {
    if let Ok(title) = current.get::<_, String>("title") {
      path.push(title);
    }
    node = current.get::<_, Value<'_>>("parent").ok().and_then(Value::into_object);
  }
  path.reverse();
  path
}

fn attempt_obj<'js>(ctx: &Ctx<'js>, attempt: &api::Attempt) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  fill_attempt(ctx, &obj, attempt)?;
  Ok(obj)
}

/// Write an attempt's facts onto a result object. `onTestBegin` hands a
/// reporter the object and `onTestEnd` hands it the SAME one, filled —
/// which is why this mutates rather than rebuilding.
fn fill_attempt<'js>(ctx: &Ctx<'js>, obj: &Object<'js>, attempt: &api::Attempt) -> rquickjs::Result<()> {
  obj.set("retry", attempt.retry)?;
  obj.set("workerIndex", attempt.worker_index)?;
  obj.set("parallelIndex", attempt.parallel_index)?;
  obj.set("duration", attempt.duration)?;
  obj.set("startTime", js_date(ctx, attempt.start_time)?)?;
  match &attempt.status {
    Some(status) => obj.set("status", status.clone())?,
    None => obj.set("status", Value::new_undefined(ctx.clone()))?,
  }
  if let Some(error) = &attempt.error {
    obj.set("error", error_obj(ctx, error)?)?;
  }
  obj.set(
    "errors",
    attempt
      .errors
      .iter()
      .map(|e| error_obj(ctx, e))
      .collect::<rquickjs::Result<Vec<_>>>()?,
  )?;
  obj.set("stdout", attempt.stdout.clone())?;
  obj.set("stderr", attempt.stderr.clone())?;
  obj.set(
    "attachments",
    attempt
      .attachments
      .iter()
      .map(|a| attachment_obj(ctx, a))
      .collect::<rquickjs::Result<Vec<_>>>()?,
  )?;
  obj.set("annotations", annotation_objs(ctx, &attempt.annotations)?)?;
  // Live steps are appended by `onStepBegin` as they happen; the
  // finished attempt's tree replaces them only when the live stream
  // produced none (a replayed blob, a harness that emits no step
  // events).
  let live: Vec<Value<'js>> = obj.get("steps").unwrap_or_default();
  if live.is_empty() {
    let steps: Vec<Object<'js>> = attempt
      .steps
      .iter()
      .map(|s| step_obj(ctx, s, None))
      .collect::<rquickjs::Result<_>>()?;
    obj.set("steps", steps)?;
  }
  Ok(())
}

fn case_obj<'js>(ctx: &Ctx<'js>, case: &api::Case) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  obj.set("id", case.id.clone())?;
  obj.set("title", case.title.clone())?;
  obj.set("type", "test")?;
  obj.set("location", loc_obj(ctx, &case.location)?)?;
  obj.set("expectedStatus", case.expected_status.clone())?;
  obj.set("timeout", case.timeout)?;
  obj.set("retries", case.retries)?;
  obj.set("repeatEachIndex", case.repeat_each_index)?;
  obj.set("tags", case.tags.clone())?;
  obj.set("annotations", annotation_objs(ctx, &case.annotations)?)?;
  obj.set("results", Vec::<Value<'js>>::new())?;
  let title_path = case.title_path.clone();
  obj.set("titlePath", Function::new(ctx.clone(), move || title_path.clone())?)?;
  obj.set("outcome", Function::new(ctx.clone(), case_outcome)?)?;
  obj.set("ok", Function::new(ctx.clone(), case_ok)?)?;
  Ok(obj)
}

/// The statuses a case has produced so far, and the status it was
/// declared to end in. Read from the live object, because a reporter
/// asks after every attempt.
fn case_verdict(this: &Object<'_>) -> (Vec<String>, String) {
  let expected = this
    .get::<_, String>("expectedStatus")
    .unwrap_or_else(|_| "passed".to_string());
  let results: Vec<Object<'_>> = this.get("results").unwrap_or_default();
  let statuses = results
    .iter()
    .filter_map(|result| result.get::<_, String>("status").ok())
    .collect();
  (statuses, expected)
}

fn case_outcome(this: This<Object<'_>>) -> String {
  let (statuses, expected) = case_verdict(&this.0);
  api::outcome_of(&statuses, &expected).to_string()
}

fn case_ok(this: This<Object<'_>>) -> bool {
  let (statuses, expected) = case_verdict(&this.0);
  api::ok_of(&statuses, &expected)
}

fn suite_obj<'js>(ctx: &Ctx<'js>, suite: &api::Suite, index: &Object<'js>) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  obj.set("title", suite.title.clone())?;
  obj.set("type", suite.kind.as_str())?;
  if let Some(location) = &suite.location {
    obj.set("location", loc_obj(ctx, location)?)?;
  }
  if let Some(project) = &suite.project {
    obj.set("_project", crate::bindings::convert::json_to_js(ctx, project)?)?;
  }
  let title_path = suite.title_path.clone();
  obj.set("titlePath", Function::new(ctx.clone(), move || title_path.clone())?)?;
  obj.set("entries", Function::new(ctx.clone(), suite_entries)?)?;
  obj.set("allTests", Function::new(ctx.clone(), suite_all_tests)?)?;
  obj.set("project", Function::new(ctx.clone(), suite_project)?)?;

  let children: Vec<Object<'js>> = suite
    .suites
    .iter()
    .map(|child| {
      let child = suite_obj(ctx, child, index)?;
      child.set("parent", obj.clone())?;
      Ok(child)
    })
    .collect::<rquickjs::Result<_>>()?;
  obj.set("suites", children)?;

  let cases: Vec<Object<'js>> = suite
    .tests
    .iter()
    .map(|case| {
      let built = case_obj(ctx, case)?;
      built.set("parent", obj.clone())?;
      index.set(case_key(&case.id), built.clone())?;
      Ok(built)
    })
    .collect::<rquickjs::Result<_>>()?;
  obj.set("tests", cases)?;
  Ok(obj)
}

/// Playwright's `Suite.entries()`: child suites, then loose tests.
fn suite_entries<'js>(this: This<Object<'js>>) -> Vec<Value<'js>> {
  let mut entries: Vec<Value<'js>> = this.0.get("suites").unwrap_or_default();
  entries.extend(this.0.get::<_, Vec<Value<'js>>>("tests").unwrap_or_default());
  entries
}

fn collect_cases<'js>(suite: &Object<'js>, out: &mut Vec<Value<'js>>) {
  for child in suite.get::<_, Vec<Object<'js>>>("suites").unwrap_or_default() {
    collect_cases(&child, out);
  }
  out.extend(suite.get::<_, Vec<Value<'js>>>("tests").unwrap_or_default());
}

/// Playwright's `Suite.allTests()`: every case in the subtree, in
/// `entries()` order.
fn suite_all_tests(this: This<Object<'_>>) -> Vec<Value<'_>> {
  let mut out = Vec::new();
  collect_cases(&this.0, &mut out);
  out
}

/// Playwright's `Suite.project()`: the `FullProject` of the enclosing
/// project-level suite, if there is one.
fn suite_project<'js>(this: This<Object<'js>>) -> Value<'js> {
  let mut node = Some(this.0.clone());
  while let Some(current) = node {
    if let Ok(project) = current.get::<_, Value<'js>>("_project")
      && !project.is_undefined()
    {
      return project;
    }
    node = current.get::<_, Value<'js>>("parent").ok().and_then(Value::into_object);
  }
  Value::new_undefined(this.0.ctx().clone())
}

// ── Driving the hooks ──

fn js<T>(result: rquickjs::Result<T>) -> Result<T, ScriptError> {
  result.map_err(|e| ScriptError::internal(e.to_string()))
}

/// The case object for `id`, when the run's tree carries it.
fn case_of<'js>(ctx: &Ctx<'js>, id: &str) -> Result<Option<Object<'js>>, ScriptError> {
  let index = state(ctx, |s| s.index.clone())?;
  let Some(index) = index else {
    return Ok(None);
  };
  let index = index.restore(ctx).map_err(|e| ScriptError::internal(e.to_string()))?;
  Ok(
    index
      .get::<_, Value<'js>>(case_key(id))
      .ok()
      .and_then(Value::into_object),
  )
}

fn indexed<'js>(ctx: &Ctx<'js>, key: &str) -> Result<Option<Object<'js>>, ScriptError> {
  let index = state(ctx, |s| s.index.clone())?;
  let Some(index) = index else {
    return Ok(None);
  };
  let index = index.restore(ctx).map_err(|e| ScriptError::internal(e.to_string()))?;
  Ok(index.get::<_, Value<'js>>(key).ok().and_then(Value::into_object))
}

fn store<'js>(ctx: &Ctx<'js>, key: &str, value: &Object<'js>) -> Result<(), ScriptError> {
  let index = state(ctx, |s| s.index.clone())?;
  let Some(index) = index else {
    return Ok(());
  };
  let index = index.restore(ctx).map_err(|e| ScriptError::internal(e.to_string()))?;
  js(index.set(key, value.clone()))
}

fn push<'js>(owner: &Object<'js>, key: &str, value: &Object<'js>) -> Result<(), ScriptError> {
  let array: rquickjs::Array<'js> = js(owner.get(key))?;
  js(array.set(array.len(), value.clone()))
}

/// Hand `args` to `name`, awaiting whatever it returns. Playwright
/// awaits `onEnd`, `onExit` and `onTestPaused`; the rest are called
/// synchronously.
async fn dispatch_async<'js>(
  ctx: &Ctx<'js>,
  name: &str,
  args: Vec<Value<'js>>,
) -> Result<Option<Value<'js>>, ScriptError> {
  let instance = state(ctx, |s| s.instance.clone())?;
  let Some(instance) = instance else {
    return Ok(None);
  };
  let instance = instance
    .restore(ctx)
    .map_err(|e| ScriptError::internal(e.to_string()))?;
  let Some(func) = method(&instance, name) else {
    return Ok(None);
  };
  let mut call = rquickjs::function::Args::new(ctx.clone(), args.len() + 1);
  call.this(instance).map_err(|e| ScriptError::internal(e.to_string()))?;
  call.push_args(args).map_err(|e| ScriptError::internal(e.to_string()))?;
  let returned: Value<'js> = func
    .call_arg::<Value<'js>>(call)
    .catch(ctx)
    .map_err(|e| crate::engine::caught_to_script_error(e, name))?;
  match returned.as_promise() {
    Some(promise) => promise
      .clone()
      .into_future::<Value<'js>>()
      .await
      .catch(ctx)
      .map(Some)
      .map_err(|e| crate::engine::caught_to_script_error(e, name)),
    None => Ok(Some(returned)),
  }
}

/// Replay what a V1 reporter could not be told before `onBegin`.
fn flush_deferred(ctx: &Ctx<'_>) -> Result<(), ScriptError> {
  let deferred = state(ctx, |s| std::mem::take(&mut s.deferred))?;
  for item in deferred {
    match item {
      Deferred::Error(error) => {
        let value = js(error_obj(ctx, &error))?.into_value();
        dispatch(ctx, "onError", vec![value])?;
      },
      Deferred::Stdio {
        stderr,
        text,
        case,
        attempt,
      } => {
        let chunk = js(text.into_js(ctx))?;
        let test = case_of(ctx, &case)?.map_or_else(|| Value::new_undefined(ctx.clone()), Object::into_value);
        let result = indexed(ctx, &result_key(&case, attempt))?
          .map_or_else(|| Value::new_undefined(ctx.clone()), Object::into_value);
        dispatch(
          ctx,
          if stderr { "onStdErr" } else { "onStdOut" },
          vec![chunk, test, result],
        )?;
      },
    }
  }
  Ok(())
}

impl JsReporter {
  async fn begin(&mut self, preamble: &Arc<api::RunPreamble>) {
    let Some(vm) = self.vm().await else { return };
    let preamble = Arc::clone(preamble);
    let outcome = crate::vm_with!(vm => |ctx| {
      let index = js(Object::new(ctx.clone()))?;
      let suite = js(suite_obj(&ctx, &preamble.suite, &index))?;
      let config = js(crate::bindings::convert::json_to_js(&ctx, &preamble.config))?;
      state(&ctx, |s| {
        s.index = Some(Persistent::save(&ctx, index));
        s.suite = Some(Persistent::save(&ctx, suite.clone()));
        s.config = Some(Persistent::save(&ctx, config.clone()));
      })?;
      // Playwright's `wrapReporterAsV2`: a V2 reporter is configured
      // and then begun with the suite alone; a V1 reporter never sees
      // `onConfigure` and takes the config as `onBegin`'s first
      // argument.
      match state(&ctx, |s| s.protocol)? {
        Protocol::V2 => {
          dispatch(&ctx, "onConfigure", vec![config])?;
          dispatch(&ctx, "onBegin", vec![suite.into_value()])?;
        },
        Protocol::V1 => dispatch(&ctx, "onBegin", vec![config, suite.into_value()])?,
      }
      state(&ctx, |s| s.began = true)?;
      flush_deferred(&ctx)
    })
    .await;
    self.report("onBegin", outcome);
  }

  async fn test_begin(&mut self, id: String, attempt: u32, worker_id: u32) {
    let Some(vm) = self.vm().await else { return };
    let started = api::started_attempt(attempt, worker_id, std::time::SystemTime::now());
    let outcome = crate::vm_with!(vm => |ctx| {
      let Some(case) = case_of(&ctx, &id)? else {
        return Ok(());
      };
      let result = js(attempt_obj(&ctx, &started))?;
      push(&case, "results", &result)?;
      store(&ctx, &result_key(&id, attempt), &result)?;
      dispatch(&ctx, "onTestBegin", vec![case.into_value(), result.into_value()])
    })
    .await;
    self.report("onTestBegin", outcome);
  }

  async fn step_begin(&mut self, id: String, attempt: u32, step: api::Step, parent_step_id: Option<String>) {
    let Some(vm) = self.vm().await else { return };
    let outcome = crate::vm_with!(vm => |ctx| {
      let (Some(case), Some(result)) = (case_of(&ctx, &id)?, indexed(&ctx, &result_key(&id, attempt))?) else {
        return Ok(());
      };
      let parent = match &parent_step_id {
        Some(parent_id) => indexed(&ctx, &step_key(&id, attempt, parent_id))?,
        None => None,
      };
      let built = js(step_obj(&ctx, &step, parent.as_ref()))?;
      match &parent {
        Some(parent) => push(parent, "steps", &built)?,
        None => push(&result, "steps", &built)?,
      }
      store(&ctx, &step_key(&id, attempt, &step.id), &built)?;
      dispatch(&ctx, "onStepBegin", vec![
        case.into_value(),
        result.into_value(),
        built.into_value(),
      ])
    })
    .await;
    self.report("onStepBegin", outcome);
  }

  async fn step_end(&mut self, id: String, attempt: u32, step: api::Step) {
    let Some(vm) = self.vm().await else { return };
    let outcome = crate::vm_with!(vm => |ctx| {
      let (Some(case), Some(result)) = (case_of(&ctx, &id)?, indexed(&ctx, &result_key(&id, attempt))?) else {
        return Ok(());
      };
      let Some(built) = indexed(&ctx, &step_key(&id, attempt, &step.id))? else {
        return Ok(());
      };
      js(built.set("duration", step.duration))?;
      if let Some(error) = &step.error {
        js(built.set("error", js(error_obj(&ctx, error))?))?;
      }
      js(built.set("annotations", js(annotation_objs(&ctx, &step.annotations))?))?;
      dispatch(&ctx, "onStepEnd", vec![
        case.into_value(),
        result.into_value(),
        built.into_value(),
      ])
    })
    .await;
    self.report("onStepEnd", outcome);
  }

  async fn output(&mut self, id: String, attempt: u32, stderr: bool, text: String) {
    let Some(vm) = self.vm().await else { return };
    let outcome = crate::vm_with!(vm => |ctx| {
      // Playwright's V1 wrapper queues output that arrives before the
      // suite exists and replays it after `onBegin`.
      if !state(&ctx, |s| s.began)? && state(&ctx, |s| s.protocol)? == Protocol::V1 {
        return state(&ctx, |s| {
          s.deferred.push(Deferred::Stdio {
            stderr,
            text,
            case: id,
            attempt,
          });
        });
      }
      let chunk = js(text.into_js(&ctx))?;
      let owner = case_of(&ctx, &id)?.map_or_else(|| Value::new_undefined(ctx.clone()), Object::into_value);
      let result = indexed(&ctx, &result_key(&id, attempt))?
        .map_or_else(|| Value::new_undefined(ctx.clone()), Object::into_value);
      dispatch(&ctx, if stderr { "onStdErr" } else { "onStdOut" }, vec![chunk, owner, result])
    })
    .await;
    self.report("onStdOut", outcome);
  }

  async fn test_end(&mut self, id: String, attempt: u32, filled: api::Attempt) {
    let Some(vm) = self.vm().await else { return };
    let outcome = crate::vm_with!(vm => |ctx| {
      let Some(case) = case_of(&ctx, &id)? else {
        return Ok(());
      };
      // A run replayed from a blob has no `onTestBegin`, so the result
      // object may not exist yet.
      let result = if let Some(existing) = indexed(&ctx, &result_key(&id, attempt))? {
        existing
      } else {
        let built = js(Object::new(ctx.clone()))?;
        js(built.set("steps", Vec::<Value<'_>>::new()))?;
        push(&case, "results", &built)?;
        store(&ctx, &result_key(&id, attempt), &built)?;
        built
      };
      js(fill_attempt(&ctx, &result, &filled))?;
      dispatch(&ctx, "onTestEnd", vec![case.into_value(), result.into_value()])
    })
    .await;
    self.report("onTestEnd", outcome);
  }

  async fn run_error(&mut self, error: api::ReportedError) {
    let Some(vm) = self.vm().await else { return };
    let outcome = crate::vm_with!(vm => |ctx| {
      if !state(&ctx, |s| s.began)? && state(&ctx, |s| s.protocol)? == Protocol::V1 {
        return state(&ctx, |s| s.deferred.push(Deferred::Error(error)));
      }
      let value = js(error_obj(&ctx, &error))?.into_value();
      dispatch(&ctx, "onError", vec![value])
    })
    .await;
    self.report("onError", outcome);
  }

  async fn end(&mut self, result: api::FullResult) {
    let Some(vm) = self.vm().await else { return };
    let outcome = crate::vm_with!(vm => |ctx| {
      let full = js(Object::new(ctx.clone()))?;
      js(full.set("status", result.status.clone()))?;
      js(full.set("startTime", js(js_date(&ctx, result.start_time))?))?;
      js(full.set("duration", result.duration))?;
      let returned = dispatch_async(&ctx, "onEnd", vec![full.into_value()]).await?;
      // Playwright: `onEnd` may return `{ status }`, and the
      // multiplexer lets it overwrite how the run is reported to have
      // ended.
      let status = returned
        .and_then(Value::into_object)
        .and_then(|o| o.get::<_, String>("status").ok());
      state(&ctx, |s| s.status_override = status)
    })
    .await;
    self.report("onEnd", outcome);
    if let Some(live) = &self.live {
      let vm = live.vm.clone();
      if let Ok(Ok(status)) = crate::vm_with!(vm => |ctx| { state(&ctx, |s| s.status_override.clone()) }).await {
        self.status_override = status.as_deref().map(RunStatus::parse);
      }
    }
  }

  async fn exit(&mut self) {
    let Some(vm) = self.vm().await else { return };
    let outcome = crate::vm_with!(vm => |ctx| {
      dispatch_async(&ctx, "onExit", Vec::new()).await.map(|_| ())
    })
    .await;
    self.report("onExit", outcome);
  }

  fn report(&self, hook: &str, outcome: Result<Result<(), ScriptError>, ScriptError>) {
    match outcome {
      Ok(Ok(())) => {},
      Ok(Err(e)) | Err(e) => self.failed(hook, &e),
    }
  }
}

#[async_trait::async_trait]
impl Reporter for JsReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::RunStarted {
        preamble, start_time, ..
      } => {
        self.run_start = *start_time;
        self.begin(preamble).await;
      },
      ReporterEvent::TestStarted {
        test_id,
        project,
        attempt,
        worker_id,
      } => {
        let id = test_id.stable_id(project);
        self.attempts.insert(id.clone(), *attempt);
        self.test_begin(id, *attempt, *worker_id).await;
      },
      ReporterEvent::StepStarted(started) => {
        let id = started.test_id.stable_id(&started.project);
        let attempt = self.attempts.get(&id).copied().unwrap_or(1);
        let step = api::Step {
          id: started.step_id.clone(),
          title: started.title.clone(),
          category: started.category.to_string(),
          start_time: api::epoch_ms(std::time::SystemTime::now()),
          location: started.location.as_ref().map(|l| api::Location {
            file: l.file.clone(),
            line: usize::try_from(l.line).unwrap_or(0),
            column: usize::try_from(l.column).unwrap_or(0),
          }),
          ..api::Step::default()
        };
        self.step_begin(id, attempt, step, started.parent_step_id.clone()).await;
      },
      ReporterEvent::StepFinished(finished) => {
        let id = finished.test_id.stable_id(&finished.project);
        let attempt = self.attempts.get(&id).copied().unwrap_or(1);
        let step = api::Step {
          id: finished.step_id.clone(),
          title: finished.title.clone(),
          category: finished.category.to_string(),
          duration: api::ms(finished.duration),
          error: finished.error.as_ref().map(|message| api::ReportedError {
            message: message.clone(),
            ..api::ReportedError::default()
          }),
          annotations: api::annotations(&finished.annotations),
          ..api::Step::default()
        };
        self.step_end(id, attempt, step).await;
      },
      ReporterEvent::TestOutput(output) => {
        let id = output.test_id.stable_id(&output.project);
        let attempt = self.attempts.get(&id).copied().unwrap_or(1);
        self.output(id, attempt, output.stderr, output.text.clone()).await;
      },
      ReporterEvent::TestFinished { outcome } => {
        let id = outcome.test_id.stable_id(&outcome.project_name);
        self.test_end(id, outcome.attempt, api::attempt(outcome)).await;
      },
      ReporterEvent::RunError { error } => {
        self.run_error(api::error(error, None)).await;
      },
      ReporterEvent::RunFinished { status, duration, .. } => {
        let result = api::FullResult {
          status: status.as_str().to_string(),
          start_time: api::epoch_ms(self.run_start),
          duration: api::ms(*duration),
        };
        self.end(result).await;
      },
      ReporterEvent::WorkerStarted { .. } | ReporterEvent::WorkerFinished { .. } => {},
    }
  }

  async fn preprocess(
    &mut self,
    preamble: &api::RunPreamble,
    edits: &mut ferridriver_test::reporter::TestRunEdits,
  ) -> Result<(), String> {
    let Some(vm) = self.vm().await else {
      return Ok(());
    };
    let preamble = preamble.clone();
    let collected = crate::vm_with!(vm => |ctx| {
      let index = js(Object::new(ctx.clone()))?;
      let suite = js(suite_obj(&ctx, &preamble.suite, &index))?;
      let config = js(crate::bindings::convert::json_to_js(&ctx, &preamble.config))?;
      state(&ctx, |s| {
        s.index = Some(Persistent::save(&ctx, index));
        s.suite = Some(Persistent::save(&ctx, suite.clone()));
        s.config = Some(Persistent::save(&ctx, config.clone()));
        s.preprocessing = true;
      })?;
      let params = js(Object::new(ctx.clone()))?;
      js(params.set("config", config))?;
      js(params.set("suite", suite))?;
      js(params.set("testRun", js(test_run_obj(&ctx))?))?;
      let outcome = dispatch_async(&ctx, "preprocess", vec![params.into_value()]).await;
      // The `TestRun` handle is dead the moment `preprocess` returns,
      // whether it returned or threw.
      state(&ctx, |s| s.preprocessing = false)?;
      outcome?;
      state(&ctx, |s| s.edits.clone())
    })
    .await;
    let collected = match collected {
      Ok(Ok(collected)) => collected,
      Ok(Err(e)) | Err(e) => return Err(e.message),
    };
    edits.excluded.extend(collected.excluded);
    edits.annotations.extend(collected.annotations);
    edits.skip_sharding |= collected.skip_sharding;
    Ok(())
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    self.exit().await;
    Ok(())
  }

  fn prints_to_stdio(&self) -> bool {
    self.module.prints_to_stdio
  }

  fn status_override(&self) -> Option<RunStatus> {
    self.status_override
  }
}

// ── The factory the core reporter set consults ──

/// Every reporter module a config names, compiled once for the run.
pub struct JsReporterFactory {
  modules: Vec<Arc<ReporterModule>>,
}

impl JsReporterFactory {
  /// Compile every reporter entry whose name is not a built-in.
  ///
  /// # Errors
  ///
  /// Fails on the first module that does not resolve, bundle,
  /// evaluate, or export a class — before the run, which is where a
  /// broken reporter has to be reported.
  pub async fn load(config: &TestConfig, cwd: &Path, caps: &ScriptCaps) -> Result<Self, ScriptError> {
    let mut modules = Vec::new();
    for entry in &config.reporter {
      if ferridriver_test::reporter::REPORTER_NAMES.contains(&entry.name.as_str())
        || matches!(
          entry.name.as_str(),
          "terminal" | "bdd" | "default" | "" | "none" | "empty" | "cucumber"
        )
        || !looks_like_module(&entry.name)
      {
        continue;
      }
      if modules
        .iter()
        .any(|module: &Arc<ReporterModule>| module.label == entry.name)
      {
        continue;
      }
      modules.push(Arc::new(load(entry, config, cwd, caps.clone()).await?));
    }
    Ok(Self { modules })
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.modules.is_empty()
  }
}

impl ferridriver_test::reporter::ReporterFactory for JsReporterFactory {
  fn create(&self, entry: &ReporterConfig, _config: &TestConfig) -> Option<Box<dyn Reporter>> {
    self
      .modules
      .iter()
      .find(|module| module.label == entry.name)
      .map(|module| Box::new(module.reporter()) as Box<dyn Reporter>)
  }
}

/// Compile the config's JS reporters and install them as the factory
/// `create_reporters` consults. A config that names none is a no-op.
///
/// # Errors
///
/// Propagates [`JsReporterFactory::load`]'s failure, so a run refuses
/// to start with a reporter that cannot load.
pub async fn install(config: &TestConfig, cwd: &Path, caps: &ScriptCaps) -> Result<(), ScriptError> {
  let factory = JsReporterFactory::load(config, cwd, caps).await?;
  if factory.is_empty() {
    return Ok(());
  }
  ferridriver_test::reporter::set_reporter_factory(Arc::new(factory));
  Ok(())
}
