//! `ScriptEngine` + `Session`: ferridriver's realm over the `ferrijs`
//! runtime.
//!
//! [`ScriptEngine::run`] is the one-shot path (fresh VM, library/test
//! convenience). [`Session`] is the persistent path: one runtime reused
//! across many [`Session::execute`] calls so user `globalThis` state
//! survives between executions REPL-style while framework bindings
//! refresh each call. The production MCP server keeps a set of
//! [`Session`]s with a retention policy in
//! [`crate::session_table::SessionTable`].
//!
//! Everything generic about running JavaScript -- the event loop, the
//! limits, the deadline and its backstop, poisoning, the console, the
//! module loader, the standard library, `fetch`, the sandbox -- is the
//! runtime's. What lives here is ferridriver's: the `page` / `context` /
//! `browser` / `request` bindings, the extension registry, the BDD and
//! test surfaces, `vars`, `artifacts`, `commands`, `sidecars`, and the
//! policy the operator's config resolves to.

use std::sync::Arc;
use std::time::Duration;

use ferrijs::rquickjs;
use ferrijs::{ModulePolicy, Permissions, Runtime};
use rquickjs::function::Func;
use rquickjs::{Ctx, Object};

use crate::error::ScriptError;
use crate::result::ScriptResult;
use crate::vars::VarsStore;
use std::path::PathBuf;

use crate::output_dir::OutputDir;

pub use ferrijs::RunOptions;
pub use ferrijs::runtime::{DEFAULT_MAX_CONSOLE_BYTES, DEFAULT_MAX_CONSOLE_ENTRIES, DEFAULT_MAX_CONSOLE_ENTRY_BYTES};

/// Default per-script wall-clock timeout (5 minutes).
pub const DEFAULT_TIMEOUT: Duration = ferrijs::limits::DEFAULT_TIMEOUT;

/// Default per-script memory quota (256 MiB).
pub const DEFAULT_MEMORY_LIMIT: usize = ferrijs::limits::DEFAULT_MEMORY_LIMIT;

/// Default per-script JS stack size (1 MiB).
pub const DEFAULT_STACK_SIZE: usize = ferrijs::limits::DEFAULT_STACK_SIZE;

/// Default GC trigger threshold (64 MiB). See
/// [`ferrijs::limits::DEFAULT_GC_THRESHOLD`].
pub const DEFAULT_GC_THRESHOLD: usize = ferrijs::limits::DEFAULT_GC_THRESHOLD;

/// Default cap on concurrently-retained persistent session VMs. When a
/// new session would exceed this, the least-recently-used idle VM is
/// evicted (its `globalThis` state is discarded; a later call rebuilds).
pub const DEFAULT_MAX_SESSION_VMS: usize = 64;

/// Default idle TTL: a session VM untouched this long is reaped on the
/// next `SessionTable::acquire`, independent of cap pressure, so a
/// long-running server does not pin dead sessions' memory indefinitely.
pub const DEFAULT_SESSION_IDLE_TTL: Duration = Duration::from_mins(30);

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
  /// When set, `console.*` calls stream to this sink as they happen and
  /// `ScriptResult.console` stays empty. `None` (the default) keeps the
  /// buffered form every machine consumer reads.
  pub console_sink: Option<Arc<dyn ferrijs::ConsoleSink>>,
  /// Values redacted from everything a run hands back: console entries,
  /// the returned value, and the failure. Redacting at the runtime means
  /// a host cannot forget to do it on one of the three paths.
  pub secrets: ferridriver::response::Secrets,
  /// Ceiling on the artifacts root, enforced by whichever host owns the
  /// output directory. Carried here so a session published by
  /// `browser.bind()` inherits the ceiling of the VM that bound it.
  pub artifacts_budget: Option<ferridriver::response::OutputBudget>,
  /// Set only for a session published by a paused test: scripts get a
  /// `testDebug` global that inspects the pause and releases it.
  pub test_debug: Option<Arc<dyn crate::bindings::TestDebugControl>>,
  /// Identity this VM's actions are attributed with
  /// ([`ferridriver::trace::CallOrigin::script`]). An action gate reads it
  /// to tell a paused test's own calls from those of the client inspecting
  /// it -- pausing the inspector would leave nobody to resume.
  pub script_id: Option<String>,
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
      console_sink: None,
      secrets: ferridriver::response::Secrets::default(),
      artifacts_budget: None,
      test_debug: None,
      script_id: None,
    }
  }
}

impl ScriptEngineConfig {
  fn limits(&self) -> ferrijs::Limits {
    ferrijs::Limits {
      memory: self.default_memory_limit,
      stack: self.default_stack_size,
      gc_threshold: self.default_gc_threshold,
      timeout: self.default_timeout,
      ..ferrijs::Limits::default()
    }
  }

  fn console(&self) -> ferrijs::ConsoleOptions {
    ferrijs::ConsoleOptions {
      max_entries: self.max_console_entries,
      max_bytes: self.max_console_bytes,
      max_entry_bytes: self.max_console_entry_bytes,
      sink: self.console_sink.clone(),
    }
  }
}

/// The operator's declared secrets as the runtime's redactor.
#[derive(Debug)]
struct SecretsRedactor(ferridriver::response::Secrets);

impl ferrijs::Redactor for SecretsRedactor {
  fn redact<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
    self.0.redact(text)
  }

  fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
}

/// The debugger's parked time, so a run held at a breakpoint is not a
/// run that timed out.
struct PauseClock;

impl ferrijs::PauseClock for PauseClock {
  fn parked_now(&self) -> Duration {
    ferridriver::pause::pause_clock().parked_now()
  }
}

/// Which host is running the extension/registry. Exposed to JS as the
/// native global `ferridriver.host` ("mcp" | "bdd" | "test" | "script") so one
/// extension file can branch its contributions -- e.g. only `tool`
/// under MCP, only `Given/When/Then` under the test runner -- without any
/// runtime cost (a single string set once per session).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtensionHost {
  /// MCP server (`ferridriver mcp`) -- consumes `tool` registrations.
  Mcp,
  /// BDD test runner (`ferridriver bdd`) -- consumes step/hook defs.
  Bdd,
  /// Playwright-shaped test runner (`ferridriver test`) -- consumes
  /// `test`/`describe` registrations from `@ferridriver/test`.
  Test,
  /// Ad-hoc script (`ferridriver run` / `run_script`).
  #[default]
  Script,
}

impl ExtensionHost {
  /// Every host, in the order a report lists them. Pinned against
  /// `ferridriver_config::extension_manifest::EXTENSION_HOSTS`, which a
  /// manifest's `hosts` filter is validated against -- the two spellings
  /// of the same set must not drift.
  pub const ALL: &'static [Self] = &[Self::Mcp, Self::Bdd, Self::Test, Self::Script];

  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Mcp => "mcp",
      Self::Bdd => "bdd",
      Self::Test => "test",
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
/// # Filesystem posture
///
/// A script reaches the filesystem the way Node does: `fs` is the
/// runtime's `node:fs`, and a relative path resolves against the process
/// cwd. `script_root` is where a relative ES module import resolves from,
/// and `artifacts` is where outputs are written by default -- anchors,
/// not boundaries. What confines a script is the realm's permissions
/// ([`ScriptCaps::permissions`]), which the operator's config resolves.
#[derive(Clone)]
pub struct RunContext {
  pub vars: Arc<dyn VarsStore>,
  /// Directory a relative ES module import resolves against.
  pub script_root: PathBuf,
  /// Optional dedicated output directory, exposed to scripts as `artifacts`.
  /// Typically `.ferridriver/artifacts/` alongside `script_root`.
  pub artifacts: Option<Arc<OutputDir>>,
  pub page: Option<Arc<ferridriver::Page>>,
  pub browser_context: Option<Arc<ferridriver::context::ContextRef>>,
  pub request: Option<Arc<ferridriver::http_client::HttpClient>>,
  /// Optional root `Browser` handle exposed as the `browser` global.
  pub browser: Option<Arc<ferridriver::Browser>>,
  /// Extension bindings to install on the `tools` global.
  pub extensions: Vec<crate::bindings::ExtensionBinding>,
  /// Which host is driving this session -- surfaced to JS as
  /// `ferridriver.host`. Defaults to [`ExtensionHost::Script`].
  pub host: ExtensionHost,
  /// The realm's policy and the operator's extension ceiling, resolved
  /// from config.
  pub caps: ScriptCaps,
  /// The session key this VM serves (`"<instance>:<context>"`), when the
  /// host has one. Surfaced to scripts and to extension handlers as
  /// `session`, so a tool can tell WHICH environment it is driving
  /// instead of taking that as an argument and silently disagreeing with
  /// the browser it was handed.
  pub session: Option<String>,
}

/// What the operator's config resolves to: the realm's permissions, the
/// command grants, and the extension ceiling. Built by the host
/// (MCP/CLI/BDD) from `ferridriver_config::ScriptingConfig`; the engine
/// only consumes it. Default is a script trusted as much as the host,
/// with an empty `process.env`.
#[derive(Debug, Clone)]
pub struct ScriptCaps {
  /// `process.env` contents -- already filtered to the operator's
  /// allow-list intersected with the real environment. Empty ⇒
  /// `process.env` is an empty object.
  pub env: std::collections::BTreeMap<String, String>,
  /// The names the operator allow-listed (`[scripting].allowEnv`),
  /// before intersecting with the real environment. Kept alongside
  /// [`Self::env`] because "not allow-listed" and "allow-listed but
  /// unset" are different diagnoses for an extension that declares an
  /// environment requirement, and `env` alone cannot tell them apart.
  pub allow_env: Vec<String>,
  /// The realm's grants for everything but `env`. Defaults to
  /// everything: a ferridriver script drives a browser it was handed,
  /// which is authority enough that a filesystem jail on top would be
  /// theatre. An operator narrows it through config.
  pub permissions: Permissions,
  /// First-party command grants exposed as `commands` /
  /// `ferridriver.commands` outside extension handlers.
  pub commands: std::collections::BTreeMap<String, crate::command_spec::CommandSpec>,
  /// Operator ceiling over extension capability manifests
  /// (`[extensions.policy]`). Enforced at tool registration: a tool's
  /// effective grants are its declared `allow` intersected with this.
  /// Default = fully open (manifests keep their declared authority).
  pub extension_policy: ferridriver_config::ExtensionPolicyConfig,
  /// Per-extension settings (`[extensions.settings.<name>]`), keyed by
  /// namespace or full tool name. Surfaced to a handler as `settings`.
  pub extension_settings: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Default for ScriptCaps {
  fn default() -> Self {
    Self {
      env: std::collections::BTreeMap::new(),
      allow_env: Vec::new(),
      permissions: Permissions::all(),
      commands: std::collections::BTreeMap::new(),
      extension_policy: ferridriver_config::ExtensionPolicyConfig::default(),
      extension_settings: std::collections::BTreeMap::new(),
    }
  }
}

impl ScriptCaps {
  /// Attach the operator's per-extension settings.
  #[must_use]
  pub fn with_extension_settings(mut self, settings: std::collections::BTreeMap<String, serde_json::Value>) -> Self {
    self.extension_settings = settings;
    self
  }

  /// Resolve from an operator allow-list: only the named variables, and
  /// only those actually present in the process environment, are
  /// captured. A name not in the environment is silently absent (same
  /// as Node) -- it is never invented.
  #[must_use]
  pub fn resolve(allow_env: &[String]) -> Self {
    let env = allow_env
      .iter()
      .filter_map(|k| std::env::var(k).ok().map(|v| (k.clone(), v)))
      .collect();
    Self {
      env,
      allow_env: allow_env.to_vec(),
      ..Self::default()
    }
  }

  /// Resolve from the operator's `[scripting]` section: the env
  /// allow-list, the command grants, and the realm permissions (or
  /// everything, when the section names none).
  #[must_use]
  pub fn from_scripting(cfg: &ferridriver_config::ScriptingConfig) -> Self {
    let mut caps = Self::resolve_with_commands(&cfg.allow_env, cfg.allow.commands.clone());
    if let Some(permissions) = &cfg.permissions {
      caps.permissions = permissions.clone();
    }
    caps
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

  /// Attach the operator extension-policy ceiling (`[extensions.policy]`).
  #[must_use]
  pub fn with_extension_policy(mut self, policy: ferridriver_config::ExtensionPolicyConfig) -> Self {
    self.extension_policy = policy;
    self
  }

  /// Narrow the realm's grants.
  #[must_use]
  pub fn with_permissions(mut self, permissions: Permissions) -> Self {
    self.permissions = permissions;
    self
  }

  /// The realm's policy: the grants, with `env` reduced to the
  /// allow-list.
  #[must_use]
  pub fn realm_permissions(&self) -> Permissions {
    let mut p = self.permissions.clone();
    p.env = ferrijs::permissions::Allow::Only(self.allow_env.clone());
    p
  }
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

/// The session key this VM serves plus the operator's per-extension
/// settings, stashed as userdata so `tools.<name>` dispatch can build a
/// handler's `session` / `settings` without threading them through every
/// call.
pub(crate) struct ExtensionEnvUd {
  pub(crate) session: Option<String>,
  pub(crate) settings: std::collections::BTreeMap<String, serde_json::Value>,
}

// SAFETY: owned data only; no borrowed JS values.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for ExtensionEnvUd {
  type Changed<'to> = ExtensionEnvUd;
}

/// The session's `fetch` backend, stashed as userdata so a tool
/// dispatch can hand its handler an attenuated copy.
pub(crate) struct SessionFetchUd(pub(crate) Arc<crate::bindings::net_policy::SessionFetch>);

// SAFETY: owned `Arc` only.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for SessionFetchUd {
  type Changed<'to> = SessionFetchUd;
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
  /// interpolated into `source` -- preventing prompt injection. No state
  /// survives the call.
  ///
  /// Boxed at the definition for the same reason as [`Session::create`],
  /// which it awaits: the future carries the engine config, and every
  /// caller would otherwise carry it too (`clippy::large_futures`).
  pub fn run<'a>(
    &'a self,
    source: &'a str,
    args: &'a [serde_json::Value],
    options: RunOptions,
    context: RunContext,
  ) -> impl std::future::Future<Output = ScriptResult> + Send + 'a {
    Box::pin(async move {
      match Session::create(self.config.clone(), &context).await {
        Ok(session) => session.execute(source, args, options, &context).await.result,
        Err(e) => ScriptResult::err(e, 0, Vec::new()),
      }
    })
  }
}

/// Outcome of one [`Session::execute`]: the script result plus whether
/// the VM was left in a state the caller must discard before the next
/// execution. Poisoning means the interpreter was force-halted mid-run
/// (timeout interrupt) or hit an allocation fault -- a plain JS `throw`
/// is NOT poisoning and leaves session state intact.
#[derive(Debug)]
pub struct SessionRun {
  pub result: ScriptResult,
  pub poisoned: bool,
}

impl From<ferrijs::Run<serde_json::Value>> for SessionRun {
  fn from(run: ferrijs::Run<serde_json::Value>) -> Self {
    let poisoned = run.poisoned;
    Self {
      result: run.into_result(),
      poisoned,
    }
  }
}

/// Cloneable handle to a session's interrupt deadline. Held by hosts
/// that re-arm it from outside the session (the test-runner bridge, for
/// `test.slow()` / `testInfo.setTimeout()`).
#[derive(Clone)]
pub struct Deadline(ferrijs::Deadline);

impl Deadline {
  /// Whether the interrupt handler force-halted the interpreter for this
  /// deadline. See [`ferrijs::Deadline::force_halted`].
  #[must_use]
  pub fn force_halted(&self) -> bool {
    self.0.force_halted()
  }
}

impl ferridriver_test::host::DeadlineControl for Deadline {
  fn arm(&self, timeout: Duration) {
    self.0.arm(timeout);
  }

  fn disarm(&self) {
    self.0.disarm();
  }
}

/// Everything ferridriver installs once per realm, as the runtime's
/// extension: userdata the bindings read, the class prototypes, the
/// session-stable globals, the extension registry and every loaded
/// extension's contributions.
struct FerridriverExtension {
  vars: Arc<dyn VarsStore>,
  artifacts: Option<Arc<OutputDir>>,
  host: ExtensionHost,
  caps: ScriptCaps,
  session: Option<String>,
  extensions: Vec<crate::bindings::ExtensionBinding>,
  sidecars: Vec<crate::sidecar::SidecarSpec>,
  test_debug: Option<Arc<dyn crate::bindings::TestDebugControl>>,
  script_id: Option<String>,
  fetch: Arc<crate::bindings::net_policy::SessionFetch>,
  env: Arc<crate::session_host::SessionScriptConfig>,
}

impl ferrijs::Extension for FerridriverExtension {
  fn name(&self) -> &'static str {
    "ferridriver"
  }

  fn modules(&self, registry: &mut ferrijs::ModuleRegistry) -> Result<(), String> {
    crate::bindings::native_modules::register(registry)
  }

  fn loaders(&self) -> Vec<(ferrijs::modules::BoxResolver, ferrijs::modules::BoxLoader)> {
    crate::bindings::native_modules::provided_loaders()
  }

  fn require_hook(&self) -> Option<Arc<dyn ferrijs::RequireHook>> {
    Some(Arc::new(crate::bindings::native_modules::FerridriverRequire))
  }

  fn install_async<'js>(
    &self,
    ctx: Ctx<'js>,
  ) -> std::pin::Pin<Box<dyn std::future::Future<Output = rquickjs::Result<()>> + 'js>> {
    let vars = self.vars.clone();
    let artifacts = self.artifacts.clone();
    let host = self.host;
    let caps = self.caps.clone();
    let session = self.session.clone();
    let extensions = self.extensions.clone();
    let sidecars = self.sidecars.clone();
    let test_debug = self.test_debug.clone();
    let script_id = self.script_id.clone();
    let fetch = Arc::clone(&self.fetch);
    let env = Arc::clone(&self.env);
    Box::pin(async move {
      if let Some(id) = &script_id {
        crate::bindings::call_site::set_script_id(&ctx, id);
      }
      // The operator extension-policy ceiling. Must precede
      // `install_extensions`: `defineTool` reads it to clamp each
      // manifest's `allow` down to the effective grants.
      let _ = ctx.store_userdata(crate::bindings::registry::ExtensionPolicyUd(
        caps.extension_policy.clone(),
      ));
      // Session identity + per-extension settings for tool dispatch.
      let _ = ctx.store_userdata(ExtensionEnvUd {
        session: session.clone(),
        settings: caps.extension_settings.clone(),
      });
      let _ = ctx.store_userdata(SessionFetchUd(fetch));
      // The scripting environment itself, so `browser.bind()` can publish a
      // session that runs scripts with the SAME sandboxes, caps and
      // extensions this VM has -- a bound browser nobody can script is not a
      // session, it is a registry entry.
      let _ = ctx.store_userdata(crate::session_host::ScriptEnvUd(env));
      // Native route-handler registry (context userdata): session-once
      // so `page.route` works on ANY page (script-launched
      // `context.newPage()`, not just the MCP-prebound one whose
      // `install_page` also creates it).
      crate::bindings::page::ensure_page_callbacks(&ctx);

      // `testDebug`, only for a session a paused test published. Absent
      // otherwise, so a script can feature-detect the pause rather than
      // calling into a control that would have nothing to release.
      if let Some(control) = test_debug {
        crate::bindings::test_debug::install(&ctx, control)?;
      }

      // Session-stable bindings: install ONCE, not per `execute`. Class
      // prototypes are idempotent; `vars`/`artifacts`/`browser_type`
      // back onto Arcs that never change for a session's lifetime. Only
      // per-call-variant handles (page/context/request/browser/args)
      // refresh in `execute`.
      crate::bindings::define_classes(&ctx)?;
      install_vars(&ctx, vars)?;
      crate::bindings::runtime::mirror_global(&ctx, "fs")?;
      crate::bindings::runtime::mirror_global(&ctx, "process")?;
      install_commands(&ctx, &caps, None)?;
      if let Some(artifacts) = artifacts {
        crate::bindings::install_artifacts(&ctx, artifacts)?;
      }
      crate::bindings::install_browser_type(&ctx)?;

      // expect() global (Jest value matchers, Playwright web-first
      // matchers, asymmetric matchers, expect.poll).
      crate::bindings::expect::install_expect(&ctx)?;

      // The unified extension registry (userdata) + native contribution
      // points (`Given`/`When`/`Then`/`defineTool`/...). Must precede
      // `install_extensions`: evaluating an extension's bytecode registers
      // its tools/steps through this native surface.
      crate::bindings::install_bdd(&ctx)?;

      // Playwright-shaped `test`/`describe` registration surface. Every
      // host gets it: an extension or a step file that builds a fixture
      // chain with `test.extend` / `mergeTests` must be able to do so
      // wherever it is loaded. Only `ferridriver test` CONSUMES what
      // registering leaves behind.
      crate::bindings::test::install_test(&ctx)?;

      // `sidecars.connect(name)` -- declared external processes driven over
      // fd 3/4. Connect is by declared name only; no arbitrary spawn.
      crate::bindings::install_sidecars(&ctx, &sidecars)?;

      crate::bindings::runtime::install_host(&ctx, host.as_str())?;

      crate::bindings::install_extensions(&ctx, &extensions).await?;
      // Every extension has now contributed. `defineFixtures` appends to
      // the base fixture chain in place, and every `test.extend` from
      // here on COPIES that chain -- so the base has to stop moving
      // before the first bundle links against it.
      crate::bindings::test::seal_base_fixtures(&ctx)
        .map_err(|e| rquickjs::Error::new_from_js_message("test", "seal", e.message))?;
      Ok(())
    })
  }
}

/// A persistent runtime reused across many script executions for one
/// logical session.
///
/// User state on `globalThis` survives across [`execute`] calls
/// REPL-style; a script's own top-level declarations are scoped to that
/// run. Framework bindings (`page`, `context`, `request`, `browser`,
/// `console`, `args`) are reinstalled every call so they always reflect
/// current session state. Extension bindings are installed once at
/// creation.
///
/// [`execute`]: Session::execute
pub struct Session {
  rt: Runtime,
  config: ScriptEngineConfig,
  default_request: Arc<ferridriver::http_client::HttpClient>,
  fetch: Arc<crate::bindings::net_policy::SessionFetch>,
  caps: ScriptCaps,
}

impl Session {
  /// Build the persistent VM: runtime, resource limits, sandbox-rooted
  /// module loader, context, and one-time extension install. The module
  /// loader is bound to `context.script_root` for the VM's lifetime, so a
  /// session must always be driven with the same `script_root`.
  ///
  /// Boxed at the definition: the future carries the whole engine
  /// config, and every caller that awaits it would otherwise carry it
  /// too (`clippy::large_futures`). One allocation next to building a
  /// QuickJS runtime is noise.
  pub fn create(
    config: ScriptEngineConfig,
    context: &RunContext,
  ) -> impl std::future::Future<Output = Result<Self, ScriptError>> + Send + '_ {
    Box::pin(async move {
      let default_request = Arc::new(ferridriver::http_client::HttpClient::new(
        ferridriver::http_client::HttpClientOptions::default(),
      ));
      let fetch = Arc::new(crate::bindings::net_policy::SessionFetch::new(
        context.request.clone().unwrap_or_else(|| Arc::clone(&default_request)),
      ));
      // Snapshot for the `browser.bind()` script host. The engine config
      // goes without its console sink: that sink belongs to THIS process's
      // stdout, and a session host routes each run's output to whichever
      // client asked for it.
      let env = Arc::new(crate::session_host::SessionScriptConfig {
        script_root: context.script_root.clone(),
        artifacts: context.artifacts.clone(),
        caps: context.caps.clone(),
        extensions: context.extensions.clone(),
        engine: ScriptEngineConfig {
          console_sink: None,
          ..config.clone()
        },
      });
      let extension = FerridriverExtension {
        vars: context.vars.clone(),
        artifacts: context.artifacts.clone(),
        host: context.host,
        caps: context.caps.clone(),
        session: context.session.clone(),
        extensions: context.extensions.clone(),
        sidecars: config.sidecars.clone(),
        test_debug: config.test_debug.clone(),
        script_id: config.script_id.clone(),
        fetch: Arc::clone(&fetch),
        env,
      };
      let script_root = context.script_root.clone();
      let mut builder = Runtime::builder()
        .limits(config.limits())
        .console(config.console())
        .modules(ModulePolicy::new(&script_root))
        .process(ferrijs::ProcessOptions {
          cwd: Some(script_root.to_string_lossy().into_owned()),
          argv: vec!["script".to_string()],
        })
        .identity(ferrijs::Identity::new("ferridriver", env!("CARGO_PKG_VERSION")))
        .permissions(context.caps.realm_permissions())
        .pause_clock(Arc::new(PauseClock))
        .fs_global(true)
        .fetch(Arc::clone(&fetch) as Arc<dyn ferrijs::fetch::FetchBackend>)
        .extension(extension);
      if !config.secrets.is_empty() {
        builder = builder.redactor(Arc::new(SecretsRedactor(config.secrets.clone())));
      }
      let rt = builder.build().await?;
      Ok(Self {
        rt,
        config,
        default_request,
        fetch,
        caps: context.caps.clone(),
      })
    })
  }

  /// Arm the session's interrupt deadline so a busy-looping test body
  /// is force-halted when its per-test budget expires. The test-runner
  /// glue arms this around each `run_test`; [`Self::disarm_deadline`]
  /// must follow, or the stale deadline would halt later VM entries.
  pub fn arm_deadline(&self, timeout: Duration) {
    self.rt.deadline().arm(timeout);
  }

  /// Clear a deadline armed with [`Self::arm_deadline`].
  pub fn disarm_deadline(&self) {
    self.rt.deadline().disarm();
  }

  /// A cloneable handle to the same deadline [`Self::arm_deadline`]
  /// drives, for a host that re-arms from somewhere the session itself
  /// cannot be held (`testInfo.setTimeout()` reaching in from a bridge
  /// the runner owns).
  #[must_use]
  pub fn deadline(&self) -> Deadline {
    Deadline(self.rt.deadline())
  }

  /// The session's VM-loop handle. The BDD core clones this to drive
  /// registered JS step functions back over the async bridge (same
  /// mechanism as `page.route` cross-task dispatch).
  #[must_use]
  pub fn vm_handle(&self) -> ferrijs::VmHandle {
    self.rt.handle()
  }

  /// The runtime underneath, for a host that needs the realm's own
  /// surface (its container, its registry).
  #[must_use]
  pub fn runtime(&self) -> &Runtime {
    &self.rt
  }

  /// Whether a run left the heap untrustworthy.
  #[must_use]
  pub fn poisoned(&self) -> bool {
    self.rt.poisoned()
  }

  /// Stash the session's persistent-process registry into VM userdata
  /// so extension `commands` start/status/stop reach it. Idempotent; the
  /// same `Arc` is re-installed on each VM rebuild (the registry is
  /// durable session state, the VM is not).
  pub async fn install_session_procs(&self, procs: std::sync::Arc<crate::session_procs::SessionProcs>) {
    let caps = self.caps.clone();
    let _ = ferrijs::vm_with!(self.rt.handle() => |ctx| {
      let _ = ctx.store_userdata(SessionProcsUd(procs));
      let procs = ctx.userdata::<SessionProcsUd>().map(|u| u.0.clone());
      let _ = install_commands(&ctx, &caps, procs);
    })
    .await;
  }

  /// Per-call framework globals (`page`, `context`, ...).
  fn globals_install(&self, context: &RunContext) -> GlobalsInstall {
    GlobalsInstall {
      page: context.page.clone(),
      browser_context: context.browser_context.clone(),
      request: context.request.clone(),
      default_request: self.default_request.clone(),
      browser: context.browser.clone(),
      vm: self.rt.handle(),
      fetch: Arc::clone(&self.fetch),
    }
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
    let install = self.globals_install(context);
    let source = source.to_string();
    let args = args.to_vec();
    let run = self
      .rt
      .run(
        options,
        Box::new(move |ctx| {
          Box::pin(async move {
            install_call_globals(&ctx, install)?;
            ferrijs::script_body(&ctx, &source, &args).await
          })
        }),
      )
      .await;
    self.finish(run)
  }

  /// Execute a precompiled bundled ES module against the persistent VM --
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
    let install = self.globals_install(context);
    let bytecode = Arc::clone(&bundle.bytecode);
    let mapper = bundle.mapper();
    let args = args.to_vec();
    let run = self
      .rt
      .run(
        options,
        Box::new(move |ctx| {
          Box::pin(async move {
            install_call_globals(&ctx, install)?;
            let value = ferrijs::module_body(&ctx, &bytecode, mapper, &args).await?;
            // A module that registered a tool at its top level has no
            // callable until the bindings are rebuilt.
            crate::bindings::rebuild_tool_bindings(&ctx)
              .map_err(|e| ScriptError::internal(format!("rebuild tool bindings: {e}")))?;
            Ok(value)
          })
        }),
      )
      .await;
    let run = match run.result {
      Err(mut e) => {
        if let Some(line) = e.line
          && let Some((src, sl, sc)) = bundle.remap(line, e.column.unwrap_or(1))
        {
          e.message = format!("{} (at {src}:{sl}:{sc})", e.message);
        }
        ferrijs::Run { result: Err(e), ..run }
      },
      ok => ferrijs::Run { result: ok, ..run },
    };
    self.finish(run)
  }

  /// Invoke a registered extension tool by manifest name against the
  /// persistent VM -- the native path behind the MCP `invoke_extension_tool` /
  /// promoted-tool routes. Framework globals are refreshed exactly as
  /// for [`Self::execute`], but nothing is compiled: dispatch goes
  /// straight through the same body the `tools.<name>` binding uses, so
  /// capability wrappers and `timeoutMs` apply identically. `tool_args`
  /// becomes the handler's `args` value; the run's result is the
  /// handler's resolved return value.
  pub async fn execute_tool(
    &self,
    name: &str,
    tool_args: serde_json::Value,
    options: RunOptions,
    context: &RunContext,
  ) -> SessionRun {
    let install = self.globals_install(context);
    let name = name.to_string();
    let run = self
      .rt
      .run(
        options,
        Box::new(move |ctx| {
          Box::pin(async move {
            install_call_globals(&ctx, install)?;
            crate::bindings::invoke_tool_by_name(&ctx, &name, &tool_args).await
          })
        }),
      )
      .await;
    self.finish(run)
  }

  fn finish(&self, mut run: ferrijs::Run<serde_json::Value>) -> SessionRun {
    if let Ok(value) = &mut run.result {
      self.rt.redact_value(value);
    }
    let _ = &self.config;
    run.into()
  }
}

/// A caught JS failure as a [`ScriptError`], with a snippet of `source`
/// when the failure carries a line.
#[must_use]
pub(crate) fn caught_to_script_error(caught: rquickjs::CaughtError<'_>, source: &str) -> ScriptError {
  ScriptError::from_caught_unmapped(caught, source, 0)
}

pub(crate) use ferrijs::value::value_to_json;

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

/// Everything `install_call_globals` needs. Bundled into a struct so the
/// helper stays under the clippy arity limit as the binding surface grows.
struct GlobalsInstall {
  page: Option<Arc<ferridriver::Page>>,
  browser_context: Option<Arc<ferridriver::context::ContextRef>>,
  request: Option<Arc<ferridriver::http_client::HttpClient>>,
  default_request: Arc<ferridriver::http_client::HttpClient>,
  browser: Option<Arc<ferridriver::Browser>>,
  /// VM-loop handle -- passed to `install_page` so `page.route`
  /// callbacks can dispatch back into JS from a separate tokio task.
  vm: ferrijs::VmHandle,
  fetch: Arc<crate::bindings::net_policy::SessionFetch>,
}

/// Reinstall ONLY the per-call-variant globals: whichever of `page` /
/// `context` / `request` / `browser` the run context carries (their
/// backend handles are re-resolved every call), and the client `fetch`
/// sends through. `vars` / `fs` / `artifacts` / `browser_type` / class
/// prototypes are session-stable and installed once at creation;
/// `args` and `console` are the runtime's, refreshed by the run bracket.
fn install_call_globals(ctx: &Ctx<'_>, inst: GlobalsInstall) -> Result<(), ScriptError> {
  let fail = |e: rquickjs::Error| ScriptError::internal(format!("failed to install globals: {e}"));
  if let Some(page) = inst.page {
    crate::bindings::install_page(ctx, page, inst.vm.clone()).map_err(fail)?;
  }
  if let Some(bcx) = inst.browser_context {
    crate::bindings::install_browser_context(ctx, bcx).map_err(fail)?;
  }
  if let Some(browser) = inst.browser {
    crate::bindings::install_browser(ctx, browser).map_err(fail)?;
  }
  match inst.request {
    Some(req) => {
      inst.fetch.set_client(Arc::clone(&req));
      crate::bindings::install_request(ctx, req).map_err(fail)?;
    },
    // `fetch` is always present; with no session HTTP context it uses a
    // session-stable default one (no shared cookies). Same net posture
    // as the `request` binding when absent.
    None => inst.fetch.set_client(inst.default_request),
  }
  Ok(())
}
