//! `ScriptEngine` + `Session`: a sandboxed `QuickJS` runtime/context.
//!
//! [`ScriptEngine::run`] is the one-shot path (fresh VM, library/test
//! convenience). [`Session`] is the persistent path: one `QuickJS`
//! runtime + context reused across many [`Session::execute`] calls so
//! user `globalThis` state survives between executions REPL-style while
//! framework bindings refresh each call. The production MCP server keeps
//! a set of [`Session`]s with a retention policy in
//! [`crate::session_table::SessionTable`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rquickjs::function::{Async, Func};
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Ctx, Module, Object, Value};

use crate::vm_with;

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

/// Extra slack past the script deadline before the tokio-level backstop
/// fires. The interrupt handler is the preferred kill (it halts the
/// interpreter cleanly with the script's console output intact), but it
/// only runs while bytecode executes — a script parked on a native
/// `await` (e.g. `await new Promise(() => {})`) never re-enters the
/// interpreter, so the backstop is the only thing that frees the
/// session slot. The grace keeps the two mechanisms from racing.
const TIMEOUT_BACKSTOP_GRACE: Duration = Duration::from_secs(1);

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

/// Default idle TTL: a session VM untouched this long is reaped on the
/// next `SessionTable::acquire`, independent of cap pressure, so a
/// long-running server does not pin dead sessions' memory indefinitely.
pub const DEFAULT_SESSION_IDLE_TTL: Duration = Duration::from_secs(30 * 60);

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
  /// Idle TTL for a session VM. `None` disables time-based reaping (only
  /// the `max_session_vms` LRU cap applies).
  pub session_idle_ttl: Option<Duration>,
  /// Declared sidecar processes exposed to scripts as `sidecars.connect(name)`.
  /// Empty ⇒ `sidecars.connect` rejects every name. Connecting is by name only
  /// (no arbitrary spawn from the sandbox).
  pub sidecars: Vec<crate::sidecar::SidecarSpec>,
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
      session_idle_ttl: Some(DEFAULT_SESSION_IDLE_TTL),
      sidecars: Vec::new(),
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

/// Which host is running the extension/registry. Exposed to JS as the
/// native global `ferridriver.host` ("mcp" | "bdd" | "script") so one
/// extension file can branch its contributions — e.g. only `tool`
/// under MCP, only `Given/When/Then` under the test runner — without any
/// runtime cost (a single string set once per session).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtensionHost {
  /// MCP server (`ferridriver mcp`) — consumes `tool` registrations.
  Mcp,
  /// BDD test runner (`ferridriver bdd`) — consumes step/hook defs.
  Bdd,
  /// Ad-hoc script (`ferridriver run` / `run_script`).
  #[default]
  Script,
}

impl ExtensionHost {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Mcp => "mcp",
      Self::Bdd => "bdd",
      Self::Script => "script",
    }
  }
}

/// Per-call execution context holding session-level state the script reaches
/// via globals (`vars`, `fs`, `artifacts`, and the optional browser bindings
/// `page` / `context` / `request`). A `None` entry skips installation of
/// the matching global so pure-compute scripts don't need the extra
/// infrastructure.
///
/// # Trust contract
///
/// There is exactly ONE runtime posture: jailed. Everything a live VM
/// touches by path — `fs.*`, `artifacts`, ES module imports — goes
/// through a [`PathSandbox`]. The trusted tier is the BUNDLING step,
/// not the VM: extension files and BDD step files are read from
/// anywhere on disk by rolldown because the OPERATOR named them in
/// config/CLI; by the time they execute they are bytecode, and any
/// `import` they kept (the native `ferridriver`/node-compat set) is
/// served by Rust `ModuleDef`s. Source handed to a live session at
/// call time (MCP `run_script`) never gets that bundling privilege —
/// its imports resolve only inside the sandbox root.
#[derive(Clone)]
pub struct RunContext {
  pub vars: Arc<dyn VarsStore>,
  pub sandbox: Arc<PathSandbox>,
  /// Optional dedicated output directory, exposed to scripts as `artifacts`.
  /// Typically `.ferridriver/artifacts/` alongside `script_root`.
  pub artifacts: Option<Arc<PathSandbox>>,
  pub page: Option<Arc<ferridriver::Page>>,
  pub browser_context: Option<Arc<ferridriver::context::ContextRef>>,
  pub request: Option<Arc<ferridriver::http_client::HttpClient>>,
  /// Optional root `Browser` handle exposed as the `browser` global.
  /// Scripts use it for
  /// `browser.newContext(BrowserContextOptions)` — the natural
  /// Playwright entry point that §4.1's options bag attaches to.
  pub browser: Option<Arc<ferridriver::Browser>>,
  /// Extension bindings to install on the `tools` global.
  pub extensions: Vec<crate::bindings::ExtensionBinding>,
  /// Which host is driving this session — surfaced to JS as
  /// `ferridriver.host`. Defaults to [`ExtensionHost::Script`].
  pub host: ExtensionHost,
  /// Opt-in sandbox relaxations resolved from config (the env
  /// allow-list). Default = fully locked down.
  pub caps: ScriptCaps,
}

/// Resolved, ready-to-install sandbox relaxations. Built by the host
/// (MCP/CLI/BDD) from `ferridriver_config::ScriptingConfig`; the engine
/// only consumes it. Default is the locked-down posture: no env.
#[derive(Debug, Clone, Default)]
pub struct ScriptCaps {
  /// `process.env` contents — already filtered to the operator's
  /// allow-list intersected with the real environment. Empty ⇒
  /// `process.env` is an empty object.
  pub env: std::collections::BTreeMap<String, String>,
  /// First-party command grants exposed as `commands` /
  /// `ferridriver.commands` outside extension handlers.
  pub commands: std::collections::BTreeMap<String, crate::command_spec::CommandSpec>,
}

impl ScriptCaps {
  /// Resolve from an operator allow-list: only the named variables, and
  /// only those actually present in the process environment, are
  /// captured. A name not in the environment is silently absent (same
  /// as Node) — it is never invented.
  #[must_use]
  pub fn resolve(allow_env: &[String]) -> Self {
    let env = allow_env
      .iter()
      .filter_map(|k| std::env::var(k).ok().map(|v| (k.clone(), v)))
      .collect();
    Self {
      env,
      commands: std::collections::BTreeMap::new(),
    }
  }

  /// Resolve from env names and a pre-parsed command allow-list.
  #[must_use]
  pub fn resolve_with_commands(
    allow_env: &[String],
    commands: std::collections::BTreeMap<String, crate::command_spec::CommandSpec>,
  ) -> Self {
    let mut caps = Self::resolve(allow_env);
    caps.commands = commands;
    caps
  }
}

/// The session's VM-loop handle, stashed as rquickjs userdata at
/// [`Session::create`] so bindings that mint a `Page` from script
/// (`browser.newContext().newPage()`, `locator.page()`, `frame.page()`)
/// can thread it into `PageJs` — without it, `page.route` /
/// `page.exposeFunction` cross-task dispatch has no way back into the
/// VM event loop.
pub(crate) struct SessionVm(pub(crate) crate::vm::VmHandle);

// SAFETY: holds only an owned channel handle (`'static`; no borrowed
// JS values), so re-stating the unused `'js` lifetime is sound.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for SessionVm {
  type Changed<'to> = SessionVm;
}

/// The session's durable persistent-process registry, stashed as
/// userdata so `tools.<name>` dispatch can hand a tool's `commands`
/// binding the registry without threading it through `RunContext`.
/// Re-installed (same `Arc`) on every VM (re)build by
/// [`crate::session_table::BrowserSession::run`], so a persistent
/// process outlives a VM rebuild but dies with the session record.
pub(crate) struct SessionProcsUd(pub(crate) std::sync::Arc<crate::session_procs::SessionProcs>);

// SAFETY: holds only an owned `Arc` (`'static`; no borrowed JS values).
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for SessionProcsUd {
  type Changed<'to> = SessionProcsUd;
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

  /// Run a script once in a throwaway VM with bound args. A one-shot
  /// convenience for library consumers and tests that need no
  /// continuity; the persistent MCP path uses
  /// [`crate::session_table::SessionTable`] instead.
  ///
  /// `args` is bound as the `args` global (positional) and never
  /// interpolated into `source` — preventing prompt injection. No state
  /// survives the call.
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
/// state. Extension bindings are installed once at creation.
///
/// [`execute`]: Session::execute
pub struct Session {
  runtime: AsyncRuntime,
  /// Submission handle to the session's single VM event loop (see
  /// `crate::vm`): one persistent `async_with` owns the runtime's
  /// schedular for the VM's whole life; every execute and every
  /// cross-task dispatch runs as a job `ctx.spawn`ed by that loop.
  /// Nothing else may create an `async_with` against this runtime — a
  /// transient one steals the schedular's single wake-queue slot and
  /// dies with it, silently losing every later external wake.
  vm: crate::vm::VmHandle,
  /// Dropping this with the session ends the VM event loop, which
  /// releases the runtime on the loop's own task.
  _vm_shutdown: crate::vm::VmShutdown,
  config: ScriptEngineConfig,
  default_request: Arc<ferridriver::http_client::HttpClient>,
  caps: ScriptCaps,
  /// Last resource limits pushed to `runtime`. `set_memory_limit` /
  /// `set_max_stack_size` / `set_gc_threshold` each take the runtime's
  /// async lock; re-pushing identical values every `execute` is pure
  /// overhead on a warm persistent session that runs many small
  /// scripts (the MCP path). Skip the setter when the value is
  /// unchanged.
  applied: AppliedLimits,
  timeout: Arc<TimeoutState>,
}

/// Currently-applied runtime limits, so `execute` can skip redundant
/// `AsyncRuntime` setter calls.
struct AppliedLimits {
  memory: AtomicUsize,
  stack: AtomicUsize,
  gc: AtomicUsize,
}

/// Deadline state consulted by the session's single interrupt handler,
/// installed once at [`Session::create`]. Between calls the deadline
/// rests at [`TimeoutState::DISARMED`], so a late VM entry (route /
/// `exposeFunction` / screencast dispatch arriving after a call
/// finished) is never force-halted by a stale deadline from the
/// previous call. [`Session::execute`] / [`Session::execute_module`]
/// arm it per call; [`Session::finish`] disarms it.
struct TimeoutState {
  epoch: Instant,
  /// Deadline as milliseconds since `epoch`; `DISARMED` between calls.
  deadline_ms: AtomicU64,
  /// Set by the interrupt handler when it force-halted the interpreter.
  timed_out: AtomicBool,
}

impl TimeoutState {
  const DISARMED: u64 = u64::MAX;

  fn new() -> Self {
    Self {
      epoch: Instant::now(),
      deadline_ms: AtomicU64::new(Self::DISARMED),
      timed_out: AtomicBool::new(false),
    }
  }

  fn arm(&self, deadline: Instant) {
    let ms = deadline
      .saturating_duration_since(self.epoch)
      .as_millis()
      .min(u128::from(Self::DISARMED - 1)) as u64;
    self.timed_out.store(false, Ordering::Relaxed);
    self.deadline_ms.store(ms, Ordering::Relaxed);
  }

  fn disarm(&self) {
    self.deadline_ms.store(Self::DISARMED, Ordering::Relaxed);
  }

  fn expired(&self) -> bool {
    let deadline = self.deadline_ms.load(Ordering::Relaxed);
    deadline != Self::DISARMED && self.epoch.elapsed().as_millis() as u64 >= deadline
  }
}

impl Session {
  /// Build the persistent VM: runtime, resource limits, sandbox-rooted
  /// module loader, context, and one-time extension install. The module
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

    // One interrupt handler for the VM's lifetime, reading the shared
    // deadline cell. Installing per call and never disarming would let a
    // stale deadline force-halt route/exposeFunction/screencast dispatch
    // entering the interpreter between calls.
    let timeout = Arc::new(TimeoutState::new());
    {
      let state = Arc::clone(&timeout);
      runtime
        .set_interrupt_handler(Some(Box::new(move || {
          if state.expired() {
            state.timed_out.store(true, Ordering::Relaxed);
            true
          } else {
            false
          }
        })))
        .await;
    }

    // Module loader rooted at the sandbox — lets scripts `import './x.js'`.
    // Resolver and loader both check containment; rquickjs's built-in
    // ScriptLoader is replaced with our sandboxed pair so a rogue import
    // can't escape `script_root`. Bound once: the sandbox is stable for
    // the session's lifetime.
    // Native modules (`ferridriver`, `@cucumber/cucumber`, node-compat
    // `fs`/`path`/`buffer`) resolve first; file resolution follows.
    // Bundles mark these specifiers external, so the bytecode links
    // against THIS runtime's ModuleDefs at eval.
    runtime
      .set_loader(
        (
          crate::bindings::native_modules::resolver(),
          crate::modules::SandboxResolver::new(context.sandbox.clone()),
        ),
        (
          crate::bindings::native_modules::loader(),
          crate::modules::SandboxLoader::new(context.sandbox.clone()),
        ),
      )
      .await;

    let ctx = AsyncContext::full(&runtime)
      .await
      .map_err(|e| ScriptError::internal(format!("rquickjs context init: {e}")))?;

    let (vm, vm_shutdown) = crate::vm::spawn_vm_loop(&ctx);

    // Extension bindings are server-global and immutable post-load, so they
    // install exactly once. The per-tool wrappers dereference
    // `globalThis.page` / `context` / `request` lazily at invocation,
    // by which point `execute` has refreshed those bindings.
    let extensions = context.extensions.clone();
    // Cloned out of `context` (a `&RunContext`) so the async_with future
    // owns them rather than borrowing across the await.
    let vars = context.vars.clone();
    let sandbox = context.sandbox.clone();
    let sandbox_root = context.sandbox.root().to_string_lossy().into_owned();
    let artifacts = context.artifacts.clone();
    let host = context.host;
    let caps = context.caps.clone();
    let caps_for_session = caps.clone();
    let sidecars = config.sidecars.clone();
    let ud_vm = vm.clone();
    let install: Result<Result<(), ScriptError>, ScriptError> = vm_with!(vm => |ctx| {
      // Stash the session's VM-loop handle so script-minted pages can
      // thread it into PageJs (route/exposeFunction cross-task
      // dispatch). A failure here only degrades those to "no VM
      // handle" — never a correctness break.
      let _ = ctx.store_userdata(SessionVm(ud_vm));
      // The active-tool net allow-list cell `fetch` reads (resting state
      // = unrestricted). Stored once per VM so it survives rebuilds and
      // is present even when no tool runs; `extensions::dispatch_tool`
      // swaps it around each net-restricted handler's poll.
      let _ = ctx.store_userdata(crate::bindings::fetch::NetPolicyUd(
        crate::bindings::fetch::NetPolicy::default(),
      ));
      // Native route-handler registry (context userdata): session-once
      // so `page.route` works on ANY page (script-launched
      // `context.newPage()`, not just the MCP-prebound one whose
      // `install_page` also creates it).
      crate::bindings::page::ensure_page_callbacks(&ctx);
      install_runtime_shims(&ctx).map_err(|e| ScriptError::internal(format!("failed to install runtime shims: {e}")))?;

      // Session-stable bindings: install ONCE, not per `execute`. Class
      // prototypes are idempotent; `vars`/`fs`/`artifacts`/`browser_type`
      // back onto Arcs that never change for a session's lifetime (the
      // `SessionTable` slot owns the durable `vars`; the sandbox is
      // fixed per session). Only per-call-variant handles
      // (page/context/request/browser/console/args) refresh in `execute`.
      crate::bindings::define_classes(&ctx)
        .map_err(|e| ScriptError::internal(format!("failed to define classes: {e}")))?;
      install_vars(&ctx, vars).map_err(|e| ScriptError::internal(format!("failed to install vars: {e}")))?;
      install_fs(&ctx, sandbox).map_err(|e| ScriptError::internal(format!("failed to install fs: {e}")))?;
      crate::bindings::process::install(&ctx, &caps, &sandbox_root)
        .map_err(|e| ScriptError::internal(format!("failed to install process: {e}")))?;
      install_commands(&ctx, &caps, None)
        .map_err(|e| ScriptError::internal(format!("failed to install commands: {e}")))?;
      if let Some(artifacts) = artifacts {
        crate::bindings::install_artifacts(&ctx, artifacts)
          .map_err(|e| ScriptError::internal(format!("failed to install artifacts: {e}")))?;
      }
      crate::bindings::install_browser_type(&ctx)
        .map_err(|e| ScriptError::internal(format!("failed to install browser_type: {e}")))?;

      // expect() global (Jest value matchers, Playwright web-first
      // matchers, asymmetric matchers, expect.poll). Session-stable —
      // class prototypes + factory function are installed once and
      // survive across `execute` calls.
      crate::bindings::expect::install_expect(&ctx)
        .map_err(|e| ScriptError::internal(format!("failed to install expect: {e}")))?;

      // The unified extension registry (userdata) + native contribution
      // points (`Given`/`When`/`Then`/`defineTool`/...). Must precede
      // `install_extensions`: evaluating an extension's bytecode registers
      // its tools/steps through this native surface (`defineTool` /
      // `Given`...), so the registry must already exist.
      crate::bindings::install_bdd(&ctx)
        .map_err(|e| ScriptError::internal(format!("failed to install extension registry: {e}")))?;

      // `sidecars.connect(name)` — declared external processes driven over
      // fd 3/4. Connect is by declared name only; no arbitrary spawn.
      crate::bindings::install_sidecars(&ctx, &sidecars)
        .map_err(|e| ScriptError::internal(format!("failed to install sidecars: {e}")))?;

      crate::bindings::runtime::install_host(&ctx, host.as_str())
        .map_err(|e| ScriptError::internal(format!("install ferridriver.host: {e}")))?;

      // Extension top-level code runs during `install_extensions`, before any
      // `execute` installs its per-call capture — give it a console that
      // forwards to tracing so `console.log` at module scope works (the
      // extraction pass provides the same; see `compile_extract_one`).
      let install_console_capture = Arc::new(ConsoleCapture::new(
        DEFAULT_MAX_CONSOLE_ENTRIES,
        DEFAULT_MAX_CONSOLE_BYTES,
        DEFAULT_MAX_CONSOLE_ENTRY_BYTES,
      ));
      install_console(&ctx, install_console_capture.clone())
        .map_err(|e| ScriptError::internal(format!("failed to install console: {e}")))?;

      let installed = crate::bindings::install_extensions(&ctx, &extensions)
        .await
        .map_err(|e| ScriptError::internal(format!("failed to install extensions: {e}")));
      for entry in install_console_capture.drain() {
        tracing::info!(target: "ferridriver::extensions", "{}", entry.message);
      }
      installed
    })
    .await;
    install??;

    let applied = AppliedLimits {
      memory: AtomicUsize::new(config.default_memory_limit),
      stack: AtomicUsize::new(config.default_stack_size),
      gc: AtomicUsize::new(config.default_gc_threshold),
    };
    Ok(Self {
      runtime,
      vm,
      _vm_shutdown: vm_shutdown,
      config,
      default_request: Arc::new(ferridriver::http_client::HttpClient::new(
        ferridriver::http_client::HttpClientOptions::default(),
      )),
      caps: caps_for_session,
      applied,
      timeout,
    })
  }

  /// The session's VM-loop handle. The BDD core clones this to drive
  /// registered JS step functions back over the async bridge (same
  /// mechanism as `page.route` cross-task dispatch).
  #[must_use]
  pub fn vm_handle(&self) -> crate::vm::VmHandle {
    self.vm.clone()
  }

  /// Stash the session's persistent-process registry into VM userdata
  /// so extension `commands` start/status/stop reach it. Idempotent; the
  /// same `Arc` is re-installed on each VM rebuild (the registry is
  /// durable session state, the VM is not).
  pub async fn install_session_procs(&self, procs: std::sync::Arc<crate::session_procs::SessionProcs>) {
    let caps = self.caps.clone();
    let _ = vm_with!(self.vm => |ctx| {
      let _ = ctx.store_userdata(SessionProcsUd(procs));
      let procs = ctx.userdata::<SessionProcsUd>().map(|u| u.0.clone());
      let _ = install_commands(&ctx, &caps, procs);
    })
    .await;
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

  /// Fresh console capture sized by the session config.
  fn new_console(&self) -> Arc<ConsoleCapture> {
    Arc::new(ConsoleCapture::new(
      self.config.max_console_entries,
      self.config.max_console_bytes,
      self.config.max_console_entry_bytes,
    ))
  }

  /// Per-call framework globals (`console`, `page`, `context`, ...).
  fn globals_install(&self, context: &RunContext, console: &Arc<ConsoleCapture>) -> GlobalsInstall {
    GlobalsInstall {
      console: console.clone(),
      page: context.page.clone(),
      browser_context: context.browser_context.clone(),
      request: context.request.clone(),
      default_request: self.default_request.clone(),
      browser: context.browser.clone(),
      vm: self.vm.clone(),
    }
  }

  /// Build the `SessionRun` from an eval result, applying the poison rule:
  /// a timeout force-halt or an OOM leaves the heap untrustworthy and must
  /// rebuild the VM; a plain throw / recoverable stack overflow does not.
  fn finish(
    &self,
    eval_result: Result<serde_json::Value, ScriptError>,
    started: Instant,
    console: &Arc<ConsoleCapture>,
    timeout: Duration,
  ) -> SessionRun {
    self.timeout.disarm();
    let duration = elapsed_ms(started);
    let drained = console.drain();
    match eval_result {
      Ok(value) => SessionRun {
        result: ScriptResult::ok(value, duration, drained),
        poisoned: false,
      },
      Err(mut err) => {
        let timed_out = self.timeout.timed_out.load(Ordering::Relaxed);
        let oom = is_oom(&err);
        let poisoned = timed_out || oom;
        if timed_out {
          err = ScriptError::timeout(duration, timeout.as_millis() as u64);
        }
        SessionRun {
          result: ScriptResult::err(err, duration, drained),
          poisoned,
        }
      },
    }
  }

  /// Build the `SessionRun` for a tokio-level backstop fire: the script
  /// was parked on a native `await` past the deadline, so the interrupt
  /// handler never got a chance to halt it. The eval future was dropped
  /// mid-flight — half-driven promises may still reference VM state, so
  /// the run is always poisoned.
  fn finish_backstop(&self, started: Instant, console: &Arc<ConsoleCapture>, timeout: Duration) -> SessionRun {
    self.timeout.disarm();
    let duration = elapsed_ms(started);
    SessionRun {
      result: ScriptResult::err(
        ScriptError::timeout(duration, timeout.as_millis() as u64),
        duration,
        console.drain(),
      ),
      poisoned: true,
    }
  }

  /// Apply this call's resource overrides (falling back to session
  /// defaults), and return the resolved wall-clock timeout.
  async fn apply_call_limits(&self, options: &RunOptions) -> Duration {
    self
      .apply_limits(
        options.memory_limit.unwrap_or(self.config.default_memory_limit),
        options.stack_size.unwrap_or(self.config.default_stack_size),
        options.gc_threshold.unwrap_or(self.config.default_gc_threshold),
      )
      .await;
    options.timeout.unwrap_or(self.config.default_timeout)
  }

  /// Execute one script against the persistent VM. Framework globals are
  /// refreshed from `context` first; user `globalThis` state from prior
  /// executions is preserved.
  ///
  /// The source is wrapped in an async IIFE, so top-level `return <value>`
  /// surfaces as the run result. For ES-module sources (TypeScript,
  /// `import`/`export`) bundle them first and use [`Self::execute_module`].
  pub async fn execute(
    &self,
    source: &str,
    args: &[serde_json::Value],
    options: RunOptions,
    context: &RunContext,
  ) -> SessionRun {
    let started = Instant::now();
    let console = self.new_console();
    let timeout = self.apply_call_limits(&options).await;
    self.timeout.arm(started + timeout);
    let install = self.globals_install(context, &console);
    let source_owned = source.to_string();
    let args = args.to_vec();

    let eval_fut = vm_with!(self.vm => |ctx| {
      if let Err(e) = install_call_globals(&ctx, &args, install) {
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
    });

    let backstop = timeout.saturating_add(TIMEOUT_BACKSTOP_GRACE);
    let eval_result: Result<serde_json::Value, ScriptError> = match tokio::time::timeout(backstop, eval_fut).await {
      Ok(r) => r.and_then(|inner| inner),
      Err(_) => return self.finish_backstop(started, &console, timeout),
    };

    self.finish(eval_result, started, &console, timeout)
  }

  /// Execute a precompiled bundled ES module against the persistent VM —
  /// the TypeScript / `import` / `export` path. Framework globals
  /// (`args`, `page`, `console`, ...) are installed exactly as for
  /// [`Self::execute`]; top-level `await` is native to the module.
  ///
  /// A module cannot use top-level `return`, so the run's result value is
  /// the module's `default` export (`null` when it has none). Error
  /// locations are remapped through the bundle's source map back to the
  /// original `.ts`/`.js` position.
  pub async fn execute_module(
    &self,
    bundle: &crate::bundle::CompiledBundle,
    args: &[serde_json::Value],
    options: RunOptions,
    context: &RunContext,
  ) -> SessionRun {
    let started = Instant::now();
    let console = self.new_console();
    let timeout = self.apply_call_limits(&options).await;
    self.timeout.arm(started + timeout);
    let install = self.globals_install(context, &console);
    let bytecode = Arc::clone(&bundle.bytecode);
    let label = bundle.module_name.clone();
    let args = args.to_vec();

    let eval_fut = vm_with!(self.vm => |ctx| {
      if let Err(e) = install_call_globals(&ctx, &args, install) {
        return Err(ScriptError::internal(format!("failed to install globals: {e}")));
      }

      // SAFETY: `bytecode` was produced by `Module::write` by this exact
      // rquickjs/QuickJS build with native endianness — either in this
      // process or restored from the bytecode disk cache, whose ABI tag +
      // transitive input hashes guarantee an ABI-identical toolchain
      // wrote it. Same contract as `eval_bundle` / `install_extensions`.
      #[allow(unsafe_code)]
      let module = match (unsafe { Module::load(ctx.clone(), &bytecode) }).catch(&ctx) {
        Ok(m) => m,
        Err(e) => return Err(caught_to_script_error(e, &label)),
      };
      let (evaluated, promise) = match module.eval().catch(&ctx) {
        Ok(v) => v,
        Err(e) => return Err(caught_to_script_error(e, &label)),
      };
      if let Err(e) = promise.into_future::<()>().await.catch(&ctx) {
        return Err(caught_to_script_error(e, &label));
      }

      // Result = the module's `default` export, if any.
      let default = evaluated
        .namespace()
        .and_then(|ns| ns.get::<_, Value<'_>>("default"))
        .unwrap_or_else(|_| Value::new_undefined(ctx.clone()));
      Ok(value_to_json(&ctx, default).unwrap_or(serde_json::Value::Null))
    });

    let backstop = timeout.saturating_add(TIMEOUT_BACKSTOP_GRACE);
    let eval_result: Result<serde_json::Value, ScriptError> = match tokio::time::timeout(backstop, eval_fut).await {
      Ok(r) => r.and_then(|inner| inner),
      Err(_) => return self.finish_backstop(started, &console, timeout),
    };

    // Remap the failure location back to the original source.
    let eval_result = eval_result.map_err(|mut e| {
      if let Some(line) = e.line {
        if let Some((src, sl, sc)) = bundle.remap(line, e.column.unwrap_or(1)) {
          e.message = format!("{} (at {src}:{sl}:{sc})", e.message);
        }
      }
      e
    });

    self.finish(eval_result, started, &console, timeout)
  }

  /// Invoke a registered extension tool by manifest name against the
  /// persistent VM — the native path behind the MCP `invoke_extension_tool` /
  /// promoted-tool routes. Framework globals are refreshed exactly as
  /// for [`Self::execute`], but nothing is compiled: dispatch goes
  /// straight through the same body the `tools.<name>` binding uses, so
  /// capability wrappers, `timeoutMs`, and the net-policy bracket apply
  /// identically. `tool_args` becomes the handler's `args` value; the
  /// run's result is the handler's resolved return value.
  pub async fn execute_tool(
    &self,
    name: &str,
    tool_args: serde_json::Value,
    options: RunOptions,
    context: &RunContext,
  ) -> SessionRun {
    let started = Instant::now();
    let console = self.new_console();
    let timeout = self.apply_call_limits(&options).await;
    self.timeout.arm(started + timeout);
    let install = self.globals_install(context, &console);
    let name = name.to_string();

    let eval_fut = vm_with!(self.vm => |ctx| {
      if let Err(e) = install_call_globals(&ctx, &[], install) {
        return Err(ScriptError::internal(format!("failed to install globals: {e}")));
      }
      crate::bindings::invoke_tool_by_name(&ctx, &name, &tool_args).await
    });

    let backstop = timeout.saturating_add(TIMEOUT_BACKSTOP_GRACE);
    let eval_result: Result<serde_json::Value, ScriptError> = match tokio::time::timeout(backstop, eval_fut).await {
      Ok(r) => r.and_then(|inner| inner),
      Err(_) => return self.finish_backstop(started, &console, timeout),
    };

    self.finish(eval_result, started, &console, timeout)
  }
}

/// Wrap user source in an async IIFE so `await` works at the top level and
/// the expression evaluates to a `Promise<value>` the engine can await.
fn wrap_source(source: &str) -> String {
  format!("(async () => {{\n{source}\n}})()")
}

/// QuickJS raises an `out of memory` error when an allocation fails
/// after the runtime memory limit is hit. The allocation site is
/// arbitrary, so the heap cannot be trusted afterwards — treat it as
/// poisoning (rebuild the VM), exactly like a timeout force-halt.
fn is_oom(err: &ScriptError) -> bool {
  err.message.to_ascii_lowercase().contains("out of memory")
}

/// Everything `install_globals` needs beyond `ctx` + args JSON. Bundled into
/// a struct so the helper stays under the clippy arity limit as the binding
/// surface grows.
struct GlobalsInstall {
  console: Arc<ConsoleCapture>,
  page: Option<Arc<ferridriver::Page>>,
  browser_context: Option<Arc<ferridriver::context::ContextRef>>,
  request: Option<Arc<ferridriver::http_client::HttpClient>>,
  default_request: Arc<ferridriver::http_client::HttpClient>,
  browser: Option<Arc<ferridriver::Browser>>,
  /// VM-loop handle — passed to `install_page` so `page.route`
  /// callbacks can dispatch back into JS from a separate tokio task.
  /// Always present (cloned from the session's handle).
  vm: crate::vm::VmHandle,
}

/// Reinstall ONLY the per-call-variant globals: `args`, `console`, and
/// whichever of `page` / `context` / `request` / `browser` the run
/// context carries (their backend handles are re-resolved every call).
/// `vars` / `fs` / `artifacts` / `browser_type` / class prototypes are
/// session-stable and installed once at [`Session::create`]; extension
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
    crate::bindings::install_page(ctx, page, inst.vm.clone())?;
  }
  if let Some(bcx) = inst.browser_context {
    crate::bindings::install_browser_context(ctx, bcx)?;
  }
  if let Some(browser) = inst.browser {
    crate::bindings::install_browser(ctx, browser)?;
  }
  if let Some(req) = inst.request {
    crate::bindings::fetch::install(ctx, req.clone())?;
    crate::bindings::install_request(ctx, req)?;
  } else {
    // `fetch` is always present; with no session HTTP context it uses
    // a session-stable default one (no shared cookies). Same net posture as the
    // `request` binding when absent.
    crate::bindings::fetch::install(ctx, inst.default_request)?;
  }

  Ok(())
}

/// Node-ish console value renderer: top-level strings unquoted (quoted
/// with `'` inside containers, like `util.inspect`), arrays as
/// `[ 1, 2 ]`, objects as `{ a: 1, b: 2 }`, `Map(n) { k => v }`,
/// `Set(n) { v }`, Dates as ISO strings, RegExp as `/src/flags`,
/// `[Function: name]`, `Symbol(desc)`, `123n` bigints, `name: message`
/// (+ stack) for Error values, and `[Array]` / `[Object]` past
/// `max_depth` nesting.
#[allow(clippy::too_many_lines)]
fn format_console_value(out: &mut String, value: &Value<'_>, depth: usize, max_depth: usize) -> rquickjs::Result<()> {
  use std::fmt::Write as _;

  use rquickjs::Type;

  match value.type_of() {
    Type::String => {
      if let Some(s) = value.as_string() {
        let s = s.to_string()?;
        if depth == 0 {
          let _ = out.write_str(&s);
        } else {
          // Inside containers Node quotes strings.
          let _ = write!(out, "'{s}'");
        }
      }
    },
    Type::Int => {
      let _ = write!(out, "{}", value.as_int().unwrap_or_default());
    },
    Type::Bool => {
      let _ = write!(out, "{}", value.as_bool().unwrap_or_default());
    },
    Type::Float => {
      let _ = write!(out, "{}", value.as_float().unwrap_or_default());
    },
    Type::BigInt => {
      if let Some(b) = value.clone().into_big_int() {
        let _ = write!(out, "{}n", b.clone().to_i64()?);
      }
    },
    Type::Array => {
      let Some(array) = value.as_array() else { return Ok(()) };
      if depth > max_depth {
        let _ = out.write_str("[Array]");
        return Ok(());
      }
      if array.is_empty() {
        let _ = out.write_str("[]");
        return Ok(());
      }
      let _ = out.write_str("[ ");
      for (i, element) in array.iter::<Value<'_>>().enumerate() {
        if i > 0 {
          let _ = out.write_str(", ");
        }
        format_console_value(out, &element?, depth + 1, max_depth)?;
      }
      let _ = out.write_str(" ]");
    },
    Type::Exception => {
      if let Some(ex) = value.as_exception() {
        let name = ex.get::<_, String>("name").unwrap_or_else(|_| "Error".to_string());
        let _ = out.write_str(&name);
        if let Some(message) = ex.message() {
          let _ = write!(out, ": {message}");
        }
        // Node prints the stack under the message; keep it at top level
        // only so nested Errors don't explode container output.
        if depth == 0 {
          if let Some(stack) = ex.stack().filter(|s| !s.is_empty()) {
            let _ = write!(out, "\n{stack}");
          }
        }
      }
    },
    Type::Object => {
      if depth > max_depth {
        let _ = out.write_str("[Object]");
        return Ok(());
      }
      let Some(object) = value.as_object() else { return Ok(()) };
      if format_special_object(out, object, depth, max_depth)? {
        return Ok(());
      }
      let mut wrote_any = false;
      for (i, prop) in object.props::<String, Value<'_>>().enumerate() {
        let (key, val) = prop?;
        if i == 0 {
          let _ = out.write_str("{ ");
          wrote_any = true;
        } else {
          let _ = out.write_str(", ");
        }
        let _ = out.write_str(&key);
        let _ = out.write_str(": ");
        format_console_value(out, &val, depth + 1, max_depth)?;
      }
      let _ = out.write_str(if wrote_any { " }" } else { "{}" });
    },
    Type::Symbol => {
      if let Some(symbol) = value.as_symbol() {
        let description = symbol
          .description()?
          .as_string()
          .map(rquickjs::String::to_string)
          .transpose()?
          .unwrap_or_default();
        let _ = write!(out, "Symbol({description})");
      }
    },
    Type::Function | Type::Constructor => {
      let name = value
        .as_object()
        .and_then(|f| f.get::<_, String>("name").ok())
        .filter(|n| !n.is_empty());
      match name {
        Some(name) => {
          let _ = write!(out, "[Function: {name}]");
        },
        None => {
          let _ = out.write_str("[Function (anonymous)]");
        },
      }
    },
    Type::Null => {
      let _ = out.write_str("null");
    },
    Type::Undefined | Type::Uninitialized => {
      let _ = out.write_str("undefined");
    },
    _ => {},
  }
  Ok(())
}

/// Render Date / RegExp / Map / Set the way Node's `util.inspect` does
/// (`2026-01-01T00:00:00.000Z`, `/ab+c/i`, `Map(1) { 'a' => 1 }`,
/// `Set(2) { 1, 2 }`). Returns `false` when `object` is none of those
/// so the caller falls through to plain-object rendering. Detection is
/// by constructor name — cheap, and correct for anything built from
/// the real globals.
fn format_special_object(
  out: &mut String,
  object: &Object<'_>,
  depth: usize,
  max_depth: usize,
) -> rquickjs::Result<bool> {
  use std::fmt::Write as _;

  let ctor_name: String = object
    .get::<_, Object<'_>>("constructor")
    .and_then(|c| c.get::<_, String>("name"))
    .unwrap_or_default();
  match ctor_name.as_str() {
    "Date" => {
      // toISOString throws on Invalid Date — match Node's rendering.
      let iso = object
        .get::<_, rquickjs::Function<'_>>("toISOString")
        .and_then(|f| f.call::<_, String>((rquickjs::function::This(object.clone()),)));
      match iso {
        Ok(s) => {
          let _ = out.write_str(&s);
        },
        Err(_) => {
          let _ = out.write_str("Invalid Date");
        },
      }
      Ok(true)
    },
    "RegExp" => {
      let source: String = object.get("source").unwrap_or_default();
      let flags: String = object.get("flags").unwrap_or_default();
      let _ = write!(out, "/{source}/{flags}");
      Ok(true)
    },
    kind @ ("Map" | "Set") => {
      let size: usize = object.get("size").unwrap_or_default();
      let _ = write!(out, "{kind}({size})");
      if size == 0 {
        let _ = out.write_str(" {}");
        return Ok(true);
      }
      if depth > max_depth {
        return Ok(true);
      }
      // Drive the JS iterator so insertion order is preserved.
      let entries: rquickjs::Result<rquickjs::Function<'_>> = object.get("entries");
      let values: rquickjs::Result<rquickjs::Function<'_>> = object.get("values");
      let iter_fn = if kind == "Map" { entries } else { values };
      let Ok(iter_fn) = iter_fn else { return Ok(true) };
      let iterator: Object<'_> = iter_fn.call((rquickjs::function::This(object.clone()),))?;
      let next_fn: rquickjs::Function<'_> = iterator.get("next")?;
      let _ = out.write_str(" { ");
      let mut first = true;
      loop {
        let step: Object<'_> = next_fn.call((rquickjs::function::This(iterator.clone()),))?;
        if step.get::<_, bool>("done").unwrap_or(true) {
          break;
        }
        if !first {
          let _ = out.write_str(", ");
        }
        first = false;
        let entry: Value<'_> = step.get("value")?;
        if kind == "Map" {
          let Some(pair) = entry.as_array() else { continue };
          format_console_value(out, &pair.get::<Value<'_>>(0)?, depth + 1, max_depth)?;
          let _ = out.write_str(" => ");
          format_console_value(out, &pair.get::<Value<'_>>(1)?, depth + 1, max_depth)?;
        } else {
          format_console_value(out, &entry, depth + 1, max_depth)?;
        }
      }
      let _ = out.write_str(" }");
      Ok(true)
    },
    _ => Ok(false),
  }
}

/// Node's `util.format` core: when the first argument is a string,
/// `%s` / `%d` / `%i` / `%f` / `%j` / `%o` / `%O` / `%c` / `%%`
/// consume the following arguments; leftovers are appended
/// space-separated. Returns how many arguments were consumed
/// (including the format string itself).
fn format_console_printf(out: &mut String, fmt: &str, args: &[Value<'_>], max_depth: usize) -> rquickjs::Result<usize> {
  use std::fmt::Write as _;

  let mut consumed = 0usize;
  let mut chars = fmt.chars().peekable();
  while let Some(c) = chars.next() {
    if c != '%' {
      let _ = out.write_char(c);
      continue;
    }
    let Some(&spec) = chars.peek() else {
      let _ = out.write_char('%');
      break;
    };
    if spec == '%' {
      chars.next();
      let _ = out.write_char('%');
      continue;
    }
    if !matches!(spec, 's' | 'd' | 'i' | 'f' | 'j' | 'o' | 'O' | 'c') {
      let _ = out.write_char('%');
      continue;
    }
    let Some(arg) = args.get(consumed) else {
      // More specifiers than arguments — Node leaves them literal.
      let _ = out.write_char('%');
      continue;
    };
    chars.next();
    consumed += 1;
    match spec {
      's' => {
        if let Some(s) = arg.as_string() {
          let _ = out.write_str(&s.to_string()?);
        } else {
          format_console_value(out, arg, 1, max_depth)?;
        }
      },
      'd' | 'i' => match arg.as_number() {
        Some(n) if spec == 'i' => {
          let _ = write!(out, "{}", n.trunc());
        },
        Some(n) => {
          let _ = write!(out, "{n}");
        },
        None => {
          let _ = out.write_str("NaN");
        },
      },
      'f' => match arg.as_number() {
        Some(n) => {
          let _ = write!(out, "{n}");
        },
        None => {
          let _ = out.write_str("NaN");
        },
      },
      'j' => {
        let json: rquickjs::Result<Option<String>> = arg
          .ctx()
          .json_stringify(arg.clone())
          .map(|s| s.map(|s| s.to_string().unwrap_or_default()));
        let _ = out.write_str(&json?.unwrap_or_else(|| "undefined".into()));
      },
      'o' | 'O' => format_console_value(out, arg, 1, max_depth)?,
      // %c consumes a CSS argument and renders nothing in a terminal;
      // the guard above filters everything else out.
      _ => {},
    }
  }
  Ok(consumed + 1)
}

pub(crate) fn install_console(ctx: &Ctx<'_>, capture: Arc<ConsoleCapture>) -> rquickjs::Result<()> {
  use std::fmt::Write as _;

  use rquickjs::function::Rest;

  // Render each argument Node-style (arrays/objects structurally,
  // strings unquoted, bounded depth) into our `ConsoleCapture` sink so
  // it surfaces in `ScriptResult.console[]` for the MCP caller.
  const MAX_DEPTH: usize = 3;
  let console = Object::new(ctx.clone())?;

  for (name, level) in [
    ("log", ConsoleLevel::Log),
    ("info", ConsoleLevel::Info),
    ("warn", ConsoleLevel::Warn),
    ("error", ConsoleLevel::Error),
    ("debug", ConsoleLevel::Debug),
  ] {
    let cap = capture.clone();
    console.set(
      name,
      Func::from(move |args: Rest<Value<'_>>| -> rquickjs::Result<()> {
        let mut msg = String::new();
        // Node's util.format: a leading string with %-specifiers
        // consumes the following arguments; everything left over is
        // appended space-separated.
        let mut start = 0usize;
        if let Some(first) = args.0.first() {
          if let Some(fmt) = first.as_string() {
            let fmt = fmt.to_string()?;
            if fmt.contains('%') {
              start = format_console_printf(&mut msg, &fmt, &args.0[1..], MAX_DEPTH)?;
            }
          }
        }
        for (i, v) in args.0.iter().enumerate().skip(start) {
          if i > 0 || start > 0 {
            let _ = msg.write_char(' ');
          }
          format_console_value(&mut msg, v, 0, MAX_DEPTH)?;
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
pub(crate) fn install_runtime_shims(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  // Native timers (setTimeout/Interval, ctx.spawn-backed) and the
  // URLSearchParams class.
  crate::bindings::timers::install(ctx)?;
  crate::bindings::url_search_params::install(ctx)?;
  // Native TextEncoder/TextDecoder/URL classes + queueMicrotask/btoa/
  // atob — all real #[rquickjs::class]/Func bindings, no JS glue.
  crate::bindings::webapi::install(ctx)?;
  // Web Crypto subset: randomUUID / getRandomValues / subtle
  // digest+HMAC — native Rust, see bindings/crypto.rs for the
  // documented algorithm coverage.
  crate::bindings::crypto::install(ctx)?;
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
  crate::bindings::runtime::mirror_global(ctx, "vars")?;
  Ok(())
}

fn install_commands(
  ctx: &Ctx<'_>,
  caps: &ScriptCaps,
  procs: Option<Arc<crate::session_procs::SessionProcs>>,
) -> rquickjs::Result<()> {
  let commands = rquickjs::class::Class::instance(
    ctx.clone(),
    crate::bindings::ExtensionCommandsJs::new(Arc::new(caps.commands.clone()), procs),
  )?;
  ctx.globals().set("commands", commands)?;
  crate::bindings::runtime::mirror_global(ctx, "commands")?;
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
  crate::bindings::runtime::mirror_global(ctx, "fs")?;
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
pub(crate) fn value_to_json<'js>(_ctx: &Ctx<'js>, value: Value<'js>) -> Option<serde_json::Value> {
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
