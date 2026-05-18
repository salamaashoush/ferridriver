//! `ScriptEngine` + `Session`: a sandboxed `QuickJS` runtime/context.
//!
//! [`ScriptEngine::run`] is the one-shot path (fresh VM, used at plugin
//! load and by callers that don't need continuity). [`Session`] is the
//! persistent path: one `QuickJS` runtime + context reused across many
//! [`Session::execute`] calls so user `globalThis` state survives between
//! executions REPL-style while framework bindings refresh each call.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rquickjs::function::{Async, Func};
use rquickjs::{AsyncContext, AsyncRuntime, Ctx, Object, Value, async_with};

use crate::console::{ConsoleCapture, strip_ansi};
use crate::error::{ScriptError, ScriptErrorKind};
use crate::fs::PathSandbox;
use crate::result::{ConsoleLevel, ScriptResult};
use crate::vars::VarsStore;

/// Default console-capture limits.
pub const DEFAULT_MAX_CONSOLE_ENTRIES: usize = 1_000;
pub const DEFAULT_MAX_CONSOLE_BYTES: usize = 1_048_576;
pub const DEFAULT_MAX_CONSOLE_ENTRY_BYTES: usize = 8_192;

/// Default per-script wall-clock timeout (5 minutes).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Default per-script memory quota (256 MiB).
pub const DEFAULT_MEMORY_LIMIT: usize = 256 * 1024 * 1024;

/// Default per-script JS stack size (1 MiB).
pub const DEFAULT_STACK_SIZE: usize = 1024 * 1024;

/// Default GC trigger threshold (64 MiB). QuickJS is reference-counted;
/// the cycle GC otherwise fires adaptively at ~1.5x live size, so an
/// object-churny automation script (big `evaluate` results, repeated
/// `ariaSnapshot`/snapshot trees, locator chains) pays recurring
/// mark-sweep stalls mid-run. Raising the floor lets a typical
/// short-lived script finish with few/zero cycle-GC passes — the same
/// lever Amazon LLRT exposes (`LLRT_GC_THRESHOLD_MB`, 20 MiB default).
/// `default_memory_limit` (256 MiB) remains the hard backstop, and
/// acyclic garbage is still freed immediately by refcounting, so this
/// only defers *cycle* collection, not normal frees.
pub const DEFAULT_GC_THRESHOLD: usize = 64 * 1024 * 1024;

/// Default cap on concurrently-retained persistent session VMs. When a
/// new session would exceed this, the least-recently-used idle VM is
/// evicted (its `globalThis` state is discarded; a later call rebuilds).
pub const DEFAULT_MAX_SESSION_VMS: usize = 64;

/// Configuration for the script engine.
#[derive(Debug, Clone)]
pub struct ScriptEngineConfig {
  pub default_timeout: Duration,
  pub default_memory_limit: usize,
  pub default_stack_size: usize,
  /// Cycle-GC trigger threshold in bytes. See [`DEFAULT_GC_THRESHOLD`].
  pub default_gc_threshold: usize,
  pub max_console_entries: usize,
  pub max_console_bytes: usize,
  pub max_console_entry_bytes: usize,
  /// Upper bound on persistent session VMs kept warm at once.
  pub max_session_vms: usize,
}

impl Default for ScriptEngineConfig {
  fn default() -> Self {
    Self {
      default_timeout: DEFAULT_TIMEOUT,
      default_memory_limit: DEFAULT_MEMORY_LIMIT,
      default_stack_size: DEFAULT_STACK_SIZE,
      default_gc_threshold: DEFAULT_GC_THRESHOLD,
      max_console_entries: DEFAULT_MAX_CONSOLE_ENTRIES,
      max_console_bytes: DEFAULT_MAX_CONSOLE_BYTES,
      max_console_entry_bytes: DEFAULT_MAX_CONSOLE_ENTRY_BYTES,
      max_session_vms: DEFAULT_MAX_SESSION_VMS,
    }
  }
}

/// Per-call overrides for a single `run` invocation.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
  pub timeout: Option<Duration>,
  pub memory_limit: Option<usize>,
  pub stack_size: Option<usize>,
  pub gc_threshold: Option<usize>,
}

/// Per-call execution context holding session-level state the script reaches
/// via globals (`vars`, `fs`, `artifacts`, and the optional browser bindings
/// `page` / `context` / `request`). A `None` entry skips installation of
/// the matching global so pure-compute scripts don't need the extra
/// infrastructure.
#[derive(Clone)]
pub struct RunContext {
  pub vars: Arc<dyn VarsStore>,
  pub sandbox: Arc<PathSandbox>,
  /// Optional dedicated output directory, exposed to scripts as `artifacts`.
  /// Typically `.ferridriver/artifacts/` alongside `script_root`.
  pub artifacts: Option<Arc<PathSandbox>>,
  pub page: Option<Arc<ferridriver::Page>>,
  pub browser_context: Option<Arc<ferridriver::context::ContextRef>>,
  pub request: Option<Arc<ferridriver::api_request::APIRequestContext>>,
  /// Optional root `Browser` handle exposed as the `browser` global.
  /// Scripts use it for
  /// `browser.newContext(BrowserContextOptions)` — the natural
  /// Playwright entry point that §4.1's options bag attaches to.
  pub browser: Option<Arc<ferridriver::Browser>>,
  /// Plugin bindings to install on the `plugins` global. Empty means no
  /// `plugins` global is exposed beyond the singleton commands runner.
  pub plugins: Vec<crate::bindings::PluginBinding>,
  /// When true, ES module imports use normal filesystem resolution
  /// instead of the `PathSandbox`-rooted loader. Intended for trusted
  /// first-party code (BDD step files run from the user's own CLI), so
  /// step files can `import './helpers.js'` from anywhere on disk. The
  /// MCP / `run_script` path leaves this `false` and stays sandboxed.
  pub trusted_modules: bool,
}

/// The session's owning [`AsyncContext`], stashed as rquickjs userdata
/// at [`Session::create`] so bindings that mint a `Page` from script
/// (`browser.newContext().newPage()`, `locator.page()`, `frame.page()`)
/// can thread it into `PageJs` — without it, `page.route` /
/// `page.exposeFunction` cross-task dispatch has no context to re-enter.
pub(crate) struct SessionAsyncCtx(pub(crate) AsyncContext);

// SAFETY: holds only an owned `AsyncContext` (`'static`; no borrowed
// JS values), so re-stating the unused `'js` lifetime is sound.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for SessionAsyncCtx {
  type Changed<'to> = SessionAsyncCtx;
}

/// Sandboxed `QuickJS` scripting engine.
pub struct ScriptEngine {
  config: ScriptEngineConfig,
}

impl ScriptEngine {
  #[must_use]
  pub fn new(config: ScriptEngineConfig) -> Self {
    Self { config }
  }

  #[must_use]
  pub fn config(&self) -> &ScriptEngineConfig {
    &self.config
  }

  /// Run a script once in a throwaway VM with bound args.
  ///
  /// `source` is the JS text. `args` is an array of values made available
  /// inside the script as the `args` global (positional). Args are never
  /// interpolated into `source` — preventing prompt injection.
  ///
  /// No state survives the call. Callers that need REPL-style continuity
  /// across executions should hold a [`Session`] and call
  /// [`Session::execute`] instead.
  pub async fn run(
    &self,
    source: &str,
    args: &[serde_json::Value],
    options: RunOptions,
    context: RunContext,
  ) -> ScriptResult {
    match Session::create(self.config.clone(), &context).await {
      Ok(session) => session.execute(source, args, options, &context).await.result,
      Err(e) => ScriptResult::err(e, 0, Vec::new()),
    }
  }
}

/// Outcome of one [`Session::execute`]: the script result plus whether
/// the VM was left in a state the caller must discard before the next
/// execution. Poisoning means the interpreter was force-halted mid-run
/// (timeout interrupt) or hit an allocation fault — a plain JS `throw`
/// is NOT poisoning and leaves session state intact.
#[derive(Debug)]
pub struct SessionRun {
  pub result: ScriptResult,
  pub poisoned: bool,
}

/// A persistent `QuickJS` runtime + context reused across many script
/// executions for one logical session.
///
/// User state on `globalThis` (and `var` / `function` declarations,
/// which hoist to the global object) survives across [`execute`] calls
/// REPL-style. Top-level `let` / `const` inside a script are scoped to
/// the async wrapper of that single call and do NOT persist — assign to
/// `globalThis` for continuity. Framework bindings (`page`, `context`,
/// `request`, `browser`, `vars`, `fs`, `artifacts`, `console`, `args`)
/// are reinstalled every call so they always reflect current session
/// state. Plugin bindings are installed once at creation.
///
/// [`execute`]: Session::execute
pub struct Session {
  runtime: AsyncRuntime,
  ctx: AsyncContext,
  config: ScriptEngineConfig,
  /// Last resource limits pushed to `runtime`. `set_memory_limit` /
  /// `set_max_stack_size` / `set_gc_threshold` each take the runtime's
  /// async lock; re-pushing identical values every `execute` is pure
  /// overhead on a warm persistent session that runs many small
  /// scripts (the MCP path). Skip the setter when the value is
  /// unchanged.
  applied: AppliedLimits,
}

/// Currently-applied runtime limits, so `execute` can skip redundant
/// `AsyncRuntime` setter calls.
struct AppliedLimits {
  memory: AtomicUsize,
  stack: AtomicUsize,
  gc: AtomicUsize,
}

impl Session {
  /// Build the persistent VM: runtime, resource limits, sandbox-rooted
  /// module loader, context, and one-time plugin install. The module
  /// loader is bound to `context.sandbox` for the VM's lifetime, so a
  /// session must always be driven with the same `script_root`.
  pub async fn create(config: ScriptEngineConfig, context: &RunContext) -> Result<Self, ScriptError> {
    let runtime = AsyncRuntime::new().map_err(|e| ScriptError::internal(format!("rquickjs runtime init: {e}")))?;

    runtime.set_memory_limit(config.default_memory_limit).await;
    runtime.set_max_stack_size(config.default_stack_size).await;
    // Defer cycle-GC so short automation scripts don't mark-sweep
    // mid-run (LLRT-style). Refcounting still frees acyclic garbage
    // immediately; memory_limit is the hard cap.
    runtime.set_gc_threshold(config.default_gc_threshold).await;

    // Module loader rooted at the sandbox — lets scripts `import './x.js'`.
    // Resolver and loader both check containment; rquickjs's built-in
    // ScriptLoader is replaced with our sandboxed pair so a rogue import
    // can't escape `script_root`. Bound once: the sandbox is stable for
    // the session's lifetime.
    if context.trusted_modules {
      // Trusted first-party code (BDD step files): normal filesystem
      // ESM resolution so shared `import './helpers.js'` works from
      // anywhere, not only under the sandbox root.
      let mut resolver = rquickjs::loader::FileResolver::default();
      resolver.add_path(".");
      resolver.add_path(context.sandbox.root().to_string_lossy().as_ref());
      runtime
        .set_loader(resolver, rquickjs::loader::ScriptLoader::default())
        .await;
    } else {
      runtime
        .set_loader(
          crate::modules::SandboxResolver::new(context.sandbox.clone()),
          crate::modules::SandboxLoader::new(context.sandbox.clone()),
        )
        .await;
    }

    let ctx = AsyncContext::full(&runtime)
      .await
      .map_err(|e| ScriptError::internal(format!("rquickjs context init: {e}")))?;

    // Plugin bindings are server-global and immutable post-load, so they
    // install exactly once. The per-tool wrappers dereference
    // `globalThis.page` / `context` / `request` lazily at invocation,
    // by which point `execute` has refreshed those bindings.
    let plugins = context.plugins.clone();
    // Cloned out of `context` (a `&RunContext`) so the async_with future
    // owns them rather than borrowing across the await.
    let vars = context.vars.clone();
    let sandbox = context.sandbox.clone();
    let artifacts = context.artifacts.clone();
    let ud_ctx = ctx.clone();
    let install: Result<(), ScriptError> = async_with!(ctx => |ctx| {
      // Stash the session's AsyncContext so script-minted pages can
      // thread it into PageJs (route/exposeFunction cross-task
      // dispatch). A failure here only degrades those to "no async
      // ctx" (same as before this fix) — never a correctness break.
      let _ = ctx.store_userdata(SessionAsyncCtx(ud_ctx));
      // Native route-handler registry (context userdata): session-once
      // so `page.route` works on ANY page (script-launched
      // `context.newPage()`, not just the MCP-prebound one whose
      // `install_page` also creates it).
      crate::bindings::page::ensure_route_registry(&ctx);
      // (No `__ferriJSON` intrinsic capture any more: result
      // serialisation walks the JS value directly in Rust and never
      // touches the JS `JSON` global, so a script reassigning
      // `globalThis.JSON` in a persistent VM cannot affect it.)
      install_runtime_shims(&ctx).map_err(|e| ScriptError::internal(format!("failed to install runtime shims: {e}")))?;

      // Session-stable bindings: install ONCE, not per `execute`. Class
      // prototypes are idempotent; `vars`/`fs`/`artifacts`/`browser_type`
      // back onto Arcs that never change for a session's lifetime
      // (server.rs keys `session_vars` + `script_sandbox` per session).
      // Only per-call-variant handles (page/context/request/browser/
      // console/args) refresh in `execute`.
      crate::bindings::define_classes(&ctx)
        .map_err(|e| ScriptError::internal(format!("failed to define classes: {e}")))?;
      install_vars(&ctx, vars).map_err(|e| ScriptError::internal(format!("failed to install vars: {e}")))?;
      install_fs(&ctx, sandbox).map_err(|e| ScriptError::internal(format!("failed to install fs: {e}")))?;
      if let Some(artifacts) = artifacts {
        crate::bindings::install_artifacts(&ctx, artifacts)
          .map_err(|e| ScriptError::internal(format!("failed to install artifacts: {e}")))?;
      }
      crate::bindings::install_browser_type(&ctx)
        .map_err(|e| ScriptError::internal(format!("failed to install browser_type: {e}")))?;

      // The unified extension registry (userdata) + native contribution
      // points (`Given`/`When`/`Then`/`defineTool`/...). Must precede
      // `install_plugins`: evaluating a plugin's bytecode registers its
      // tools through this surface (native `defineTool` or the legacy
      // `globalThis.exports` ingest), and the registry must already exist.
      crate::bindings::install_bdd(&ctx)
        .map_err(|e| ScriptError::internal(format!("failed to install extension registry: {e}")))?;

      crate::bindings::install_plugins(&ctx, &plugins)
        .map_err(|e| ScriptError::internal(format!("failed to install plugins: {e}")))
    })
    .await;
    install?;

    let applied = AppliedLimits {
      memory: AtomicUsize::new(config.default_memory_limit),
      stack: AtomicUsize::new(config.default_stack_size),
      gc: AtomicUsize::new(config.default_gc_threshold),
    };
    Ok(Self {
      runtime,
      ctx,
      config,
      applied,
    })
  }

  /// The session's owning [`AsyncContext`]. The BDD core clones this to
  /// drive registered JS step functions back over the async bridge
  /// (same mechanism as `page.route` cross-task dispatch).
  #[must_use]
  pub fn async_context(&self) -> AsyncContext {
    self.ctx.clone()
  }

  /// Push resource limits to the runtime, skipping any setter whose
  /// value is unchanged since the last call (avoids the runtime's async
  /// lock on the warm-session hot path).
  async fn apply_limits(&self, memory: usize, stack: usize, gc: usize) {
    if self.applied.memory.swap(memory, Ordering::Relaxed) != memory {
      self.runtime.set_memory_limit(memory).await;
    }
    if self.applied.stack.swap(stack, Ordering::Relaxed) != stack {
      self.runtime.set_max_stack_size(stack).await;
    }
    if self.applied.gc.swap(gc, Ordering::Relaxed) != gc {
      self.runtime.set_gc_threshold(gc).await;
    }
  }

  /// Execute one script against the persistent VM. Framework globals are
  /// refreshed from `context` first; user `globalThis` state from prior
  /// executions is preserved.
  pub async fn execute(
    &self,
    source: &str,
    args: &[serde_json::Value],
    options: RunOptions,
    context: &RunContext,
  ) -> SessionRun {
    let started = Instant::now();
    let timeout = options.timeout.unwrap_or(self.config.default_timeout);
    let memory_limit = options.memory_limit.unwrap_or(self.config.default_memory_limit);
    let stack_size = options.stack_size.unwrap_or(self.config.default_stack_size);
    let gc_threshold = options.gc_threshold.unwrap_or(self.config.default_gc_threshold);

    let console = Arc::new(ConsoleCapture::new(
      self.config.max_console_entries,
      self.config.max_console_bytes,
      self.config.max_console_entry_bytes,
    ));

    // Per-call resource overrides may differ from the session defaults;
    // re-apply only the ones that actually changed (skips the runtime
    // async lock on the warm-session hot path, where they never do).
    self.apply_limits(memory_limit, stack_size, gc_threshold).await;

    // Timeout enforcement via interrupt handler. The handler fires
    // regularly during script execution; once the deadline passes we
    // signal `true` to halt the interpreter. Reset each call with this
    // call's deadline (overwrites any prior call's handler).
    let deadline = started + timeout;
    let timed_out = Arc::new(AtomicBool::new(false));
    {
      let timed_out = timed_out.clone();
      self
        .runtime
        .set_interrupt_handler(Some(Box::new(move || {
          if Instant::now() >= deadline {
            timed_out.store(true, Ordering::Relaxed);
            true
          } else {
            false
          }
        })))
        .await;
    }

    let install = GlobalsInstall {
      console: console.clone(),
      page: context.page.clone(),
      browser_context: context.browser_context.clone(),
      request: context.request.clone(),
      browser: context.browser.clone(),
      async_ctx: self.ctx.clone(),
    };
    let source_owned = source.to_string();

    let eval_result: Result<serde_json::Value, ScriptError> = async_with!(self.ctx => |ctx| {
      if let Err(e) = install_call_globals(&ctx, args, install) {
        return Err(ScriptError::internal(format!("failed to install globals: {e}")));
      }

      let wrapped = wrap_source(&source_owned);

      let promise: rquickjs::Promise<'_> = match ctx.eval(wrapped.as_bytes()) {
        Ok(v) => v,
        Err(e) => return Err(caught_to_script_error(rquickjs::CaughtError::from_error(&ctx, e), &source_owned)),
      };

      let result: Value<'_> = match promise.into_future::<Value<'_>>().await {
        Ok(v) => v,
        Err(e) => return Err(caught_to_script_error(rquickjs::CaughtError::from_error(&ctx, e), &source_owned)),
      };

      Ok(value_to_json(&ctx, result).unwrap_or(serde_json::Value::Null))
    })
    .await;

    let duration = elapsed_ms(started);
    let drained = console.drain();

    match eval_result {
      Ok(value) => SessionRun {
        result: ScriptResult::ok(value, duration, drained),
        poisoned: false,
      },
      Err(mut err) => {
        // A fired timeout interrupt force-halted the interpreter at an
        // arbitrary point — the VM is no longer trustworthy and must be
        // rebuilt. A plain JS throw does not poison.
        let poisoned = timed_out.load(Ordering::Relaxed);
        if poisoned {
          err = ScriptError::timeout(duration, timeout.as_millis() as u64);
        }
        SessionRun {
          result: ScriptResult::err(err, duration, drained),
          poisoned,
        }
      },
    }
  }
}

/// Wrap user source in an async IIFE so `await` works at the top level and
/// the expression evaluates to a `Promise<value>` the engine can await.
fn wrap_source(source: &str) -> String {
  format!("(async () => {{\n{source}\n}})()")
}

/// Everything `install_globals` needs beyond `ctx` + args JSON. Bundled into
/// a struct so the helper stays under the clippy arity limit as the binding
/// surface grows.
struct GlobalsInstall {
  console: Arc<ConsoleCapture>,
  page: Option<Arc<ferridriver::Page>>,
  browser_context: Option<Arc<ferridriver::context::ContextRef>>,
  request: Option<Arc<ferridriver::api_request::APIRequestContext>>,
  browser: Option<Arc<ferridriver::Browser>>,
  /// `AsyncContext` driving the script — passed to `install_page` so
  /// `page.route` callbacks can dispatch back into JS from a separate
  /// tokio task. Always present (cloned from the session's context).
  async_ctx: AsyncContext,
}

/// Reinstall ONLY the per-call-variant globals: `args`, `console`, and
/// whichever of `page` / `context` / `request` / `browser` the run
/// context carries (their backend handles are re-resolved every call).
/// `vars` / `fs` / `artifacts` / `browser_type` / class prototypes are
/// session-stable and installed once at [`Session::create`]; plugin
/// bindings likewise.
fn install_call_globals(ctx: &Ctx<'_>, args: &[serde_json::Value], inst: GlobalsInstall) -> rquickjs::Result<()> {
  let globals = ctx.globals();

  // args: build the JS array directly from the serde values — no JSON
  // string, no JS-side `JSON.parse`, and immune to a script reassigning
  // `globalThis.JSON` in a persistent VM.
  let args_arr = rquickjs::Array::new(ctx.clone())?;
  for (i, a) in args.iter().enumerate() {
    args_arr.set(i, crate::bindings::convert::json_to_js(ctx, a)?)?;
  }
  globals.set("args", args_arr)?;

  install_console(ctx, inst.console)?;

  if let Some(page) = inst.page {
    crate::bindings::install_page(ctx, page, inst.async_ctx.clone())?;
  }
  if let Some(bcx) = inst.browser_context {
    crate::bindings::install_browser_context(ctx, bcx)?;
  }
  if let Some(browser) = inst.browser {
    crate::bindings::install_browser(ctx, browser)?;
  }
  if let Some(req) = inst.request {
    crate::bindings::install_request(ctx, req)?;
  }

  Ok(())
}

fn install_console(ctx: &Ctx<'_>, capture: Arc<ConsoleCapture>) -> rquickjs::Result<()> {
  use std::fmt::Write as _;

  use rquickjs::function::Rest;

  // Reuse rquickjs-extra-console's Node-style value renderer (handles
  // `%s`/`%d` substitution, arrays, objects, `[Function: name]`, Symbol,
  // bounded depth) — but route the rendered line into our
  // `ConsoleCapture` sink instead of the `log` crate, so it still
  // surfaces in `ScriptResult.console[]` for the MCP caller. The
  // formatter is stateless (`max_depth` only), cheap to clone per level.
  let formatter = rquickjs_extra_console::Formatter::builder().max_depth(3).build();
  let console = Object::new(ctx.clone())?;

  for (name, level) in [
    ("log", ConsoleLevel::Log),
    ("info", ConsoleLevel::Info),
    ("warn", ConsoleLevel::Warn),
    ("error", ConsoleLevel::Error),
    ("debug", ConsoleLevel::Debug),
  ] {
    let cap = capture.clone();
    let fmt = formatter.clone();
    console.set(
      name,
      Func::from(move |args: Rest<Value<'_>>| -> rquickjs::Result<()> {
        let mut msg = String::new();
        for (i, v) in args.0.into_iter().enumerate() {
          if i > 0 {
            let _ = msg.write_char(' ');
          }
          fmt.format(&mut msg, v)?;
        }
        cap.push(level, strip_ansi(&msg));
        Ok(())
      }),
    )?;
  }

  ctx.globals().set("console", console)?;
  Ok(())
}

/// Install the session-lifetime runtime shims: timers, URL, and a few
/// hand-rolled web globals. Called once at [`Session::create`]; these
/// PERSIST across executions (browser/REPL-like) and are cancelled only
/// when the session VM is dropped (poison / eviction / session end) —
/// dropping the `AsyncRuntime` aborts every `setInterval`/`setTimeout`
/// task `ctx.spawn`ed by the timers module, so no per-call teardown is
/// needed. Sandbox-safe surface only — `os` / `sqlite` are deliberately
/// excluded so scripts cannot escape the filesystem/db sandbox.
fn install_runtime_shims(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  // Native timers (setTimeout/Interval, ctx.spawn-backed) and the
  // URLSearchParams class.
  rquickjs_extra_timers::init(ctx)?;
  rquickjs_extra_url::init(ctx)?;
  // Native TextEncoder/TextDecoder/URL classes + queueMicrotask/btoa/
  // atob — all real #[rquickjs::class]/Func bindings, no JS glue.
  crate::bindings::webapi::install(ctx)?;
  Ok(())
}

fn install_vars(ctx: &Ctx<'_>, vars: Arc<dyn VarsStore>) -> rquickjs::Result<()> {
  let obj = Object::new(ctx.clone())?;

  {
    let v = vars.clone();
    obj.set("get", Func::from(move |name: String| v.get(&name)))?;
  }
  {
    let v = vars.clone();
    obj.set(
      "set",
      Func::from(move |name: String, value: String| {
        v.set(&name, value);
      }),
    )?;
  }
  {
    let v = vars.clone();
    obj.set("has", Func::from(move |name: String| v.has(&name)))?;
  }
  {
    let v = vars.clone();
    obj.set(
      "delete",
      Func::from(move |name: String| {
        v.delete(&name);
      }),
    )?;
  }
  {
    let v = vars.clone();
    obj.set("keys", Func::from(move || v.keys()))?;
  }

  ctx.globals().set("vars", obj)?;
  Ok(())
}

fn install_fs(ctx: &Ctx<'_>, sandbox: Arc<PathSandbox>) -> rquickjs::Result<()> {
  let obj = Object::new(ctx.clone())?;

  {
    let sb = sandbox.clone();
    obj.set(
      "readFile",
      Func::from(Async(move |path: String| {
        let sb = sb.clone();
        async move {
          let resolved = sb.resolve_read(&path).map_err(|e| to_rq_error(&e))?;
          tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| rquickjs::Error::new_from_js_message("fs", "readFile", e.to_string()))
        }
      })),
    )?;
  }
  {
    let sb = sandbox.clone();
    obj.set(
      "readFileBytes",
      Func::from(Async(move |path: String| {
        let sb = sb.clone();
        async move {
          let resolved = sb.resolve_read(&path).map_err(|e| to_rq_error(&e))?;
          tokio::fs::read(&resolved)
            .await
            .map_err(|e| rquickjs::Error::new_from_js_message("fs", "readFileBytes", e.to_string()))
        }
      })),
    )?;
  }
  {
    let sb = sandbox.clone();
    obj.set(
      "writeFile",
      Func::from(Async(move |path: String, contents: String| {
        let sb = sb.clone();
        async move {
          let resolved = sb.resolve_write(&path).map_err(|e| to_rq_error(&e))?;
          tokio::fs::write(&resolved, contents)
            .await
            .map_err(|e| rquickjs::Error::new_from_js_message("fs", "writeFile", e.to_string()))
        }
      })),
    )?;
  }
  {
    let sb = sandbox.clone();
    obj.set(
      "readdir",
      Func::from(Async(move |path: String| {
        let sb = sb.clone();
        async move {
          let resolved = sb.resolve_read(&path).map_err(|e| to_rq_error(&e))?;
          let mut entries = tokio::fs::read_dir(&resolved)
            .await
            .map_err(|e| rquickjs::Error::new_from_js_message("fs", "readdir", e.to_string()))?;
          let mut names: Vec<String> = Vec::new();
          while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| rquickjs::Error::new_from_js_message("fs", "readdir", e.to_string()))?
          {
            names.push(entry.file_name().to_string_lossy().into_owned());
          }
          Ok::<_, rquickjs::Error>(names)
        }
      })),
    )?;
  }
  {
    let sb = sandbox.clone();
    obj.set(
      "exists",
      Func::from(Async(move |path: String| {
        let sb = sb.clone();
        async move {
          // Syntactic checks still apply; absence or sandbox-escape returns false.
          match sb.resolve_read(&path) {
            Ok(resolved) => Ok::<bool, rquickjs::Error>(tokio::fs::try_exists(&resolved).await.unwrap_or(false)),
            Err(_) => Ok(false),
          }
        }
      })),
    )?;
  }

  // Expose the sandbox root so scripts can build relative paths confidently.
  obj.set("root", sandbox.root().to_string_lossy().into_owned())?;

  ctx.globals().set("fs", obj)?;
  Ok(())
}

fn to_rq_error(err: &ScriptError) -> rquickjs::Error {
  // The `from`/`to` static labels are used only in rquickjs's Display impl.
  // We route sandbox-rejection errors through the FromJs variant so the
  // message propagates to JS as a thrown exception with our reason string.
  rquickjs::Error::new_from_js_message("fs", "sandbox", err.message.clone())
}

/// Convert the script's return value to `serde_json::Value`.
///
/// `rquickjs-serde` (`from_value`) drives the deserializer: it invokes
/// `toJSON()` / `valueOf()` (a returned `Date` still serialises as its
/// ISO string), coerces whole f64 in the safe-integer range to `i64`,
/// drops `undefined` / function / symbol, and renders non-finite as
/// null. We deserialize into a small AP-immune intermediate rather than
/// straight into `serde_json::Value`: a transitive dep force-enables
/// `serde_json/arbitrary_precision` workspace-wide, and under that
/// feature `serde_json::Value`'s own `Deserialize` demands a private
/// number representation that a non-`serde_json` deserializer (here,
/// `rquickjs-serde`) cannot provide — every numeric/array result would
/// otherwise fail to convert and collapse to `null`. The intermediate's
/// `Deserialize` is plain serde; the `serde_json::Value` is then built
/// with explicit constructors, which are AP-correct.
fn value_to_json<'js>(_ctx: &Ctx<'js>, value: Value<'js>) -> Option<serde_json::Value> {
  rquickjs_serde::from_value::<JsonInter>(value)
    .ok()
    .map(JsonInter::into_json)
}

/// AP-immune mirror of a JSON value. Its `Deserialize` is plain serde
/// (no `serde_json` number coupling); `into_json` rebuilds a
/// `serde_json::Value` via explicit constructors.
enum JsonInter {
  Null,
  Bool(bool),
  I64(i64),
  U64(u64),
  F64(f64),
  Str(String),
  Arr(Vec<JsonInter>),
  Obj(Vec<(String, JsonInter)>),
}

impl JsonInter {
  fn into_json(self) -> serde_json::Value {
    use serde_json::Value;
    match self {
      Self::Null => Value::Null,
      Self::Bool(b) => Value::Bool(b),
      Self::I64(n) => Value::Number(n.into()),
      Self::U64(n) => Value::Number(n.into()),
      Self::F64(f) => serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number),
      Self::Str(s) => Value::String(s),
      Self::Arr(a) => Value::Array(a.into_iter().map(Self::into_json).collect()),
      Self::Obj(o) => Value::Object(o.into_iter().map(|(k, v)| (k, v.into_json())).collect()),
    }
  }
}

impl<'de> serde::Deserialize<'de> for JsonInter {
  fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
      type Value = JsonInter;
      fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any JSON value")
      }
      fn visit_unit<E>(self) -> Result<JsonInter, E> {
        Ok(JsonInter::Null)
      }
      fn visit_none<E>(self) -> Result<JsonInter, E> {
        Ok(JsonInter::Null)
      }
      fn visit_bool<E>(self, v: bool) -> Result<JsonInter, E> {
        Ok(JsonInter::Bool(v))
      }
      fn visit_i64<E>(self, v: i64) -> Result<JsonInter, E> {
        Ok(JsonInter::I64(v))
      }
      fn visit_u64<E>(self, v: u64) -> Result<JsonInter, E> {
        Ok(JsonInter::U64(v))
      }
      fn visit_f64<E>(self, v: f64) -> Result<JsonInter, E> {
        Ok(JsonInter::F64(v))
      }
      fn visit_str<E>(self, v: &str) -> Result<JsonInter, E> {
        Ok(JsonInter::Str(v.to_owned()))
      }
      fn visit_string<E>(self, v: String) -> Result<JsonInter, E> {
        Ok(JsonInter::Str(v))
      }
      fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<JsonInter, A::Error> {
        let mut out = Vec::new();
        while let Some(e) = a.next_element()? {
          out.push(e);
        }
        Ok(JsonInter::Arr(out))
      }
      fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<JsonInter, A::Error> {
        let mut out = Vec::new();
        while let Some((k, v)) = m.next_entry()? {
          out.push((k, v));
        }
        Ok(JsonInter::Obj(out))
      }
    }
    d.deserialize_any(V)
  }
}

pub(crate) fn caught_to_script_error(caught: rquickjs::CaughtError<'_>, source: &str) -> ScriptError {
  let (message, stack, line, column) = match caught {
    rquickjs::CaughtError::Exception(ex) => {
      let message = ex.message().unwrap_or_else(|| "exception".to_string());
      let stack = ex.stack();
      // Playwright-style: lineNumber/columnNumber are present on most QuickJS
      // exceptions; read them directly off the exception object.
      let obj = ex.as_object();
      let line = obj.get::<_, u32>("lineNumber").ok();
      let column = obj.get::<_, u32>("columnNumber").ok();
      (message, stack, line, column)
    },
    rquickjs::CaughtError::Value(v) => (format!("{v:?}"), None, None, None),
    rquickjs::CaughtError::Error(e) => (format!("{e}"), None, None, None),
  };

  ScriptError {
    kind: ScriptErrorKind::Runtime,
    message,
    stack,
    line,
    column,
    source_snippet: line.and_then(|l| snippet_around_line(source, l, 2)),
  }
}

/// Build a 1-indexed source snippet with `context_lines` around the target
/// line, used in error reporting so the LLM can see where the script failed.
fn snippet_around_line(source: &str, line_1based: u32, context_lines: u32) -> Option<String> {
  use std::fmt::Write as _;
  let lines: Vec<&str> = source.lines().collect();
  if lines.is_empty() {
    return None;
  }
  let target = line_1based.saturating_sub(1) as usize;
  let start = target.saturating_sub(context_lines as usize);
  let end = (target + context_lines as usize + 1).min(lines.len());
  let mut out = String::new();
  for (i, text) in lines[start..end].iter().enumerate() {
    let ln = start + i + 1;
    let marker = if ln == line_1based as usize { ">>>" } else { "   " };
    let _ = writeln!(out, "{marker} {ln:>4}: {text}");
  }
  Some(out)
}

fn elapsed_ms(started: Instant) -> u64 {
  started.elapsed().as_millis() as u64
}
