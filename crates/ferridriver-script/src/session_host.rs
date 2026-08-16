//! The [`ScriptHost`] a bound browser serves.
//!
//! `ferridriver-session` owns the wire but sits below the scripting engine, so
//! it declares [`ScriptHost`] and leaves the implementation here. This is that
//! implementation: it holds the live browser's state plus the durable
//! per-context VMs, and turns an incoming [`ScriptRequest`] into a run with the
//! same globals a local `ferridriver run` gets — `page`, `context`, `request`,
//! `artifacts`, `tools`.
//!
//! Console output streams: each run installs itself as the target of that
//! context's [`ConsoleRouter`], so `console.log` inside the script reaches the
//! attached client while the script is still running rather than arriving in a
//! block at the end.

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use dashmap::DashMap;
use ferridriver::state::BrowserState;
use ferridriver_session::{ActionDetail, ActionPhase, EventSink, ScriptHost, ScriptKind, ScriptRequest};
use tokio::sync::RwLock;

use crate::bindings::ExtensionBinding;
use crate::console::{ConsoleSink, strip_ansi};
use crate::engine::{ExtensionHost, RunContext, RunOptions, ScriptCaps, ScriptEngineConfig};
use crate::fs::PathSandbox;
use crate::result::{ConsoleEntry, ScriptResult};
use crate::session_table::SessionTable;
use crate::vars::InMemoryVars;

/// Everything a [`SessionScriptHost`] needs that it cannot derive from the
/// browser: the sandboxes, the sandbox relaxations, and the extension bytecode
/// whose `tool` registrations scripts call as `tools.*`.
///
/// The caller resolves these from its config, so this crate never has to know
/// how a config layer is discovered.
#[derive(Clone)]
pub struct SessionScriptConfig {
  /// Root for `fs.*` — the directory the session was opened from.
  pub sandbox: Arc<PathSandbox>,
  /// Root for `artifacts.*`. `None` disables the binding.
  pub artifacts: Option<Arc<PathSandbox>>,
  pub caps: ScriptCaps,
  pub extensions: Vec<ExtensionBinding>,
  pub engine: ScriptEngineConfig,
}

/// Runs scripts against one bound browser.
pub struct SessionScriptHost {
  state: Arc<RwLock<BrowserState>>,
  sessions: SessionTable,
  config: SessionScriptConfig,
  /// Session id, so `session` inside a script (and any extension reading it)
  /// resolves to the same `<id>:<context>` key the client addressed.
  id: String,
  /// One router per context, installed into that context's VM when it is
  /// built and retargeted at each run.
  routers: DashMap<String, Arc<ConsoleRouter>>,
}

/// The live scripting environment, stashed in VM userdata so a script calling
/// `browser.bind()` can publish a session that scripts with the same sandboxes,
/// caps and extensions it has itself.
pub(crate) struct ScriptEnvUd(pub(crate) Arc<SessionScriptConfig>);

// SAFETY: holds only owned config data behind an `Arc` — no borrowed JS
// values — so restating the unused `'js` lifetime is sound.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for ScriptEnvUd {
  type Changed<'to> = ScriptEnvUd;
}

impl SessionScriptHost {
  /// Build a host over the browser state behind a bound [`ferridriver::Browser`].
  #[must_use]
  pub fn new(state: Arc<RwLock<BrowserState>>, id: impl Into<String>, mut config: SessionScriptConfig) -> Self {
    // Whatever sink the caller's config carried wrote to the caller's own
    // terminal; every run here routes to its own client instead.
    config.engine.console_sink = None;
    let sessions = SessionTable::new(config.engine.max_session_vms, config.engine.session_idle_ttl);
    Self {
      state,
      sessions,
      config,
      id: id.into(),
      routers: DashMap::new(),
    }
  }

  fn router_for(&self, context: &str) -> Arc<ConsoleRouter> {
    self
      .routers
      .entry(context.to_string())
      .or_insert_with(|| Arc::new(ConsoleRouter::default()))
      .clone()
  }

  /// Assemble the `RunContext` for one call: the live page and context for
  /// `context`, the sandboxes, and the loaded extensions.
  ///
  /// Also returns the browser-state composite key (`instance:context`) the
  /// context resolved to. That is NOT the same string as `RunContext.session`:
  /// the latter is the session id the client addressed, which extensions read,
  /// while actions and trace recorders are keyed by the state's own composite.
  async fn run_context(&self, context: &str) -> Result<(RunContext, String), String> {
    let page = ferridriver_session::page_for(&self.state, context)
      .await
      .map_err(|e| format!("opening a page for context '{context}': {e}"))?;
    let ctx_ref = ferridriver::context::ContextRef::new(Arc::clone(&self.state), context.to_string());
    let composite = ctx_ref.composite();
    let browser = Arc::new(ferridriver::Browser::from_shared_state(Arc::clone(&self.state)));
    let run_context = RunContext {
      // Replaced with the session slot's durable store before the run, the
      // same way the MCP server does it: `vars` belong to the session, not
      // to one call.
      vars: Arc::new(InMemoryVars::new()),
      sandbox: self.config.sandbox.clone(),
      artifacts: self.config.artifacts.clone(),
      page: Some(page),
      browser_context: Some(Arc::new(ctx_ref)),
      request: None,
      browser: Some(browser),
      extensions: self.config.extensions.clone(),
      host: ExtensionHost::Script,
      caps: self.config.caps.clone(),
      session: Some(format!("{}:{}", self.id, context)),
    };
    Ok((run_context, composite))
  }
}

#[async_trait]
impl ScriptHost for SessionScriptHost {
  async fn run(&self, context: &str, request: ScriptRequest, events: EventSink) -> Result<serde_json::Value, String> {
    let router = self.router_for(context);
    let mut engine = self.config.engine.clone();
    engine.console_sink = Some(router.clone() as Arc<dyn ConsoleSink>);

    let options = RunOptions {
      timeout: request.timeout_ms.map(std::time::Duration::from_millis),
      memory_limit: None,
      stack_size: None,
      gc_threshold: None,
    };

    // Compile before taking the slot lock: a syntax error should not make
    // another client wait on this context.
    let bundle = match request.kind {
      ScriptKind::Source => None,
      ScriptKind::Module => {
        let name = request.module_name.as_deref().unwrap_or("ferridriver-run.js");
        match crate::bundle::compile_bundled_source(&request.code, name, request.source_map.as_deref()).await {
          Ok(bundle) => Some(bundle),
          // A compile failure is a script result, not a transport failure:
          // the client renders it exactly like a thrown error.
          Err(e) => return Ok(result_to_json(&ScriptResult::err(e, 0, Vec::new()))),
        }
      },
    };

    let slot = self.sessions.acquire(context);
    let mut session = slot.lock().await;
    // Contexts whose VM the table has reaped no longer need a console router;
    // sweeping here keeps the map bounded by LIVE contexts rather than by
    // every context name a client has ever addressed.
    self.routers.retain(|name, _| self.sessions.contains(name));

    // Resolving the page happens UNDER the slot lock: it opens one when the
    // context has none, and two clients racing on a fresh context would
    // otherwise each open their own.
    let (mut run_context, composite) = self.run_context(context).await?;
    run_context.vars = session.vars();
    run_context.request = Some(session.request());

    // The slot lock serializes runs on this context, which is what makes a
    // single retarget-able router correct: at most one run at a time owns it.
    let _target = router.retarget(events.clone());
    // Actions are scoped to the composite the browser state resolved, which is
    // the key every action carries — so a host serving several contexts at
    // once never crosses one client's actions into another's stream.
    let code = request
      .code_language
      .as_deref()
      .map(ferridriver::codegen::OutputLanguage::parse_cli);
    let _actions = (request.trace || code.is_some()).then(|| {
      ferridriver::trace::observe_session_actions(
        &composite,
        Arc::new(ActionForwarder {
          events: events.clone(),
          trace: request.trace,
          code,
          secrets: engine.secrets.clone(),
        }),
      )
    });

    // Captured before the run so the page handle is in scope afterwards; the
    // read itself happens once the script is done.
    let reported_page = request.page_state.then(|| run_context.page.clone()).flatten();
    // The sandbox outlives this run, so its record has to be diffed to tell
    // this run's outputs from every earlier run's.
    let artifacts_before = self
      .config
      .artifacts
      .as_ref()
      .map(|sandbox| sandbox.written())
      .unwrap_or_default();

    // `epoch` stays `None`: a session host owns exactly one browser for its
    // whole life, so the browser-swap rebuild the MCP server needs cannot
    // happen here.
    let result = match &bundle {
      Some(bundle) => {
        session
          .run_module(engine, bundle, &request.args, options, run_context, None)
          .await
      },
      None => {
        session
          .run(engine, &request.code, &request.args, options, run_context, None)
          .await
      },
    };

    if let (Some(budget), Some(sandbox)) = (self.config.engine.artifacts_budget, self.config.artifacts.as_ref()) {
      let mine: std::collections::BTreeSet<_> = sandbox
        .written()
        .into_iter()
        .filter(|path| !artifacts_before.contains(path))
        .collect();
      let evicted = budget.enforce(sandbox.root(), &mine).await;
      if evicted.files > 0 {
        tracing::info!(
          files = evicted.files,
          bytes = evicted.bytes,
          "artifacts budget: evicted least-recently-modified outputs"
        );
      }
    }

    // After the run, so it describes where the script left the context rather
    // than where it started. Still under the slot lock, so a concurrent run on
    // the same context cannot navigate out from under the read.
    if let Some(page) = reported_page {
      let mut state = ferridriver::response::PageState::capture(&page).await;
      // Redacted before it goes on the wire, not by the client: a URL is a
      // routine place for a credential to appear (`?token=…`), and the host
      // is the side that knows the declared values.
      let secrets = &self.config.engine.secrets;
      state.url = secrets.redact(&state.url).into_owned();
      state.title = secrets.redact(&state.title).into_owned();
      events.page(
        state.url,
        state.title,
        ferridriver_session::PageCounts {
          console_errors: state.console_errors,
          console_warnings: state.console_warnings,
          page_errors: state.page_errors,
        },
      );
    }
    Ok(result_to_json(&result))
  }
}

fn result_to_json(result: &ScriptResult) -> serde_json::Value {
  serde_json::to_value(result).unwrap_or_else(|e| {
    serde_json::json!({
      "status": "error",
      "error": { "kind": "internal", "message": format!("serializing script result: {e}") },
      "duration_ms": 0,
      "console": [],
    })
  })
}

/// Forwards one run's browser actions to the client that asked for them: the
/// live action stream, the generated source, or both.
///
/// One observer, not two, because a session key has exactly one — and both
/// jobs read the same action. Scoped to the run's session key, so a host
/// serving several clients at once never leaks one client's actions into
/// another's stream, and registered only when the request asked for something:
/// a plain run pays nothing.
#[derive(Debug)]
struct ActionForwarder {
  events: EventSink,
  trace: bool,
  code: Option<ferridriver::codegen::OutputLanguage>,
  /// Applied to the echoed source, so a `fill` with a declared credential
  /// generates an environment read rather than the literal.
  secrets: ferridriver::response::Secrets,
}

impl ferridriver::trace::ActionObserver for ActionForwarder {
  fn action_begin(&self, action: &ferridriver::trace::ActionInfo) {
    if !self.trace {
      return;
    }
    self.events.action(
      ActionPhase::Begin,
      &action.call_id,
      &action.title,
      ActionDetail {
        params: Some(action.params.clone()),
        location: action.location.as_ref().map(ToString::to_string),
        ..Default::default()
      },
    );
  }

  fn action_end(&self, action: &ferridriver::trace::ActionInfo, elapsed: std::time::Duration, error: Option<&str>) {
    if let Some(language) = self.code
      && let Some(line) = ferridriver::codegen::echo::line_for_with_secrets(action, language, &self.secrets)
    {
      self.events.code(line);
    }
    if !self.trace {
      return;
    }
    self.events.action(
      ActionPhase::End,
      &action.call_id,
      &action.title,
      ActionDetail {
        duration_ms: Some(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)),
        error: error.map(str::to_string),
        ..Default::default()
      },
    );
  }

  fn action_log(&self, action: &ferridriver::trace::ActionInfo, message: &str) {
    if !self.trace {
      return;
    }
    self.events.action(
      ActionPhase::Log,
      &action.call_id,
      &action.title,
      ActionDetail {
        message: Some(message.to_string()),
        ..Default::default()
      },
    );
  }
}

/// A console sink whose destination is swapped per run.
///
/// The engine installs its sink when a VM is built, but a session host serves
/// many clients over that VM's life and each run's output belongs to exactly
/// one of them. The router is installed once and retargeted per run; a run
/// with no target (nothing is attached) drops output rather than growing an
/// unbounded buffer in a long-lived process.
#[derive(Debug, Default)]
pub struct ConsoleRouter {
  target: Mutex<Option<EventSink>>,
}

impl ConsoleRouter {
  /// Point the router at `events` until the returned guard drops.
  fn retarget(&self, events: EventSink) -> RouterGuard<'_> {
    if let Ok(mut target) = self.target.lock() {
      *target = Some(events);
    }
    RouterGuard(self)
  }
}

impl ConsoleSink for ConsoleRouter {
  fn emit(&self, entry: &ConsoleEntry) {
    let Ok(target) = self.target.lock() else {
      return;
    };
    if let Some(events) = target.as_ref() {
      // The message may carry page-authored text; strip terminal control
      // sequences before it reaches the client's stdout.
      events.console(level_name(entry), strip_ansi(&entry.message), entry.ts_ms);
    }
  }
}

/// The lowercase name `ScriptResult` serializes for this entry's level, which
/// is what the wire carries.
fn level_name(entry: &ConsoleEntry) -> String {
  serde_json::to_value(entry.level)
    .ok()
    .and_then(|v| v.as_str().map(str::to_string))
    .unwrap_or_else(|| "log".to_string())
}

struct RouterGuard<'a>(&'a ConsoleRouter);

impl Drop for RouterGuard<'_> {
  fn drop(&mut self) {
    if let Ok(mut target) = self.0.target.lock() {
      *target = None;
    }
  }
}
