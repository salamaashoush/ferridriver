//! Session guards, page lifecycle, script VM execution, and response helpers for [`McpServer`].

use super::{McpServer, sess};
use ferridriver::Page;
use ferridriver::actions;
use ferridriver::backend::{AnyElement, AnyPage};
use rmcp::{
  ErrorData,
  model::{CallToolResult, ContentBlock},
};
use std::sync::Arc;
use tokio::sync::Mutex;

impl McpServer {
  /// The configured instructions, plus the paths this process actually
  /// resolved.
  ///
  /// `artifacts_root` defaults to the *relative* `.ferridriver/artifacts`,
  /// resolved against the server process's working directory rather than the
  /// caller's. Told only the relative default, a caller cannot find what a
  /// script wrote -- the file is there, in one of however many
  /// `.ferridriver/artifacts` directories exist on the machine.
  pub(super) fn instructions_with_paths(&self) -> String {
    use std::fmt::Write as _;

    let mut out = self.config.server_instructions().to_string();
    match self.artifacts_dir.as_ref() {
      Some(dir) => {
        let _ = write!(
          out,
          "\n\n== ARTIFACTS ROOT (resolved for this process) ==\n{}\n\
           Everything `artifacts.*` and the `artifact://` resources address lives under it, and \
           `artifacts.write`/`writeBytes` return the absolute path they wrote. Use that path to \
           reach a file from outside this server.",
          dir.root().display()
        );
      },
      None => out.push_str(
        "\n\n== ARTIFACTS ROOT ==\nUnavailable: the configured artifacts_root could not be \
         prepared, so scripts get no `artifacts` global and `run_script` will say so.",
      ),
    }
    out
  }

  /// Assemble the `RunContext` an MCP script/extension call needs: live
  /// page/context/request/browser handles for `session`, the script root
  /// and artifacts directory, and the loaded extension bytecode. Shared by
  /// `run_script` and `invoke_extension_tool` so the wiring lives in one place.
  ///
  /// `vars` is a throwaway here: a session's `vars` is the durable tier
  /// owned by the [`ferridriver_script::SessionTable`] entry (survives
  /// VM rebuild + cap eviction for the session's lifetime), so
  /// `run_on_session_vm` swaps in that store. The field stays required
  /// because the one-shot/CLI/BDD constructors legitimately supply their
  /// own; making it optional is a wider ripple, deliberately not done.
  pub(crate) async fn mcp_run_context(&self, session: &str) -> Result<ferridriver_script::RunContext, ErrorData> {
    let Some(script_root) = self.script_root.clone() else {
      return Err(Self::err(
        "scripting is disabled: the configured script_root could not be prepared at server startup.",
      ));
    };
    let (page, ctx_ref) = Box::pin(self.page_and_context(session)).await?;
    let browser = Arc::new(ferridriver::Browser::from_shared_state(self.state.state_arc()));
    Ok(ferridriver_script::RunContext {
      vars: Arc::new(ferridriver_script::InMemoryVars::new()),
      script_root,
      artifacts: self.artifacts_dir.clone(),
      page: Some(page),
      browser_context: Some(Arc::new(ctx_ref)),
      // Placeholder like `vars`: the run_*_on_session_vm entry points
      // swap in the session slot's durable client (cookie jar +
      // connection pool live for the logical session, not one call).
      request: None,
      browser: Some(browser),
      extensions: self.extension_bindings(),
      host: ferridriver_script::ExtensionHost::Mcp,
      caps: self.script_caps.clone(),
      // Already resolved to `<instance>:<context>`, so the script host
      // never has to know this server's instance vocabulary to split it.
      session: Some(self.state.session_key(session).await.to_composite()),
    })
  }

  /// Run `source` on `session`'s persistent VM via the
  /// [`ferridriver_script::SessionTable`], which owns VM creation,
  /// warm-VM cap + idle-TTL reaping, browser-swap and poison rebuild.
  ///
  /// `_guard` is the per-context serialization lock, taken by reference
  /// purely to make "the caller already holds the context guard" a
  /// compile-time requirement instead of a comment.
  ///
  /// `context.vars` is replaced with the session's own durable store
  /// (vars belong to the session, not the call), and the browser
  /// instance generation is read so a relaunch under the same session
  /// name rebuilds the VM (its `globalThis` may hold dead page handles)
  /// while `vars` survive.
  pub(crate) async fn run_on_session_vm(
    &self,
    session: &str,
    _guard: &tokio::sync::OwnedMutexGuard<()>,
    source: &str,
    args: &[serde_json::Value],
    options: ferridriver_script::RunOptions,
    mut context: ferridriver_script::RunContext,
  ) -> ferridriver_script::ScriptResult {
    let slot = match self.sessions.acquire(session).await {
      Ok(slot) => slot,
      Err(error) => return ferridriver_script::ScriptResult::err(error, 0, Vec::new()),
    };
    let mut bs = slot.lock().await;
    context.vars = bs.vars();
    context.request = Some(bs.request());
    let epoch = self.state.instance_generation(session).await;
    bs.run(
      self.script_engine.config().clone(),
      source,
      args,
      options,
      context,
      epoch,
    )
    .await
  }

  /// Like [`Self::run_on_session_vm`], but runs a precompiled bundled ES module
  /// (the TypeScript / `import` path) on the session VM. The run's result
  /// is the module's `default` export.
  pub(crate) async fn run_module_on_session_vm(
    &self,
    session: &str,
    _guard: &tokio::sync::OwnedMutexGuard<()>,
    bundle: &ferridriver_script::CompiledBundle,
    args: &[serde_json::Value],
    options: ferridriver_script::RunOptions,
    mut context: ferridriver_script::RunContext,
  ) -> ferridriver_script::ScriptResult {
    let slot = match self.sessions.acquire(session).await {
      Ok(slot) => slot,
      Err(error) => return ferridriver_script::ScriptResult::err(error, 0, Vec::new()),
    };
    let mut bs = slot.lock().await;
    context.vars = bs.vars();
    context.request = Some(bs.request());
    let epoch = self.state.instance_generation(session).await;
    bs.run_module(
      self.script_engine.config().clone(),
      bundle,
      args,
      options,
      context,
      epoch,
    )
    .await
  }

  /// Like [`Self::run_on_session_vm`], but natively invokes a registered
  /// extension tool by manifest name — no synthesized one-liner, no
  /// per-call compile. Backs `invoke_extension_tool` and the promoted-tool
  /// routes.
  pub(crate) async fn run_tool_on_session_vm(
    &self,
    session: &str,
    _guard: &tokio::sync::OwnedMutexGuard<()>,
    name: &str,
    tool_args: serde_json::Value,
    options: ferridriver_script::RunOptions,
    mut context: ferridriver_script::RunContext,
  ) -> ferridriver_script::ScriptResult {
    let slot = match self.sessions.acquire(session).await {
      Ok(slot) => slot,
      Err(error) => return ferridriver_script::ScriptResult::err(error, 0, Vec::new()),
    };
    let mut bs = slot.lock().await;
    context.vars = bs.vars();
    context.request = Some(bs.request());
    let epoch = self.state.instance_generation(session).await;
    bs.run_tool(
      self.script_engine.config().clone(),
      name,
      tool_args,
      options,
      context,
      epoch,
    )
    .await
  }

  /// The built-in BDD step registry, built once and shared. Immutable, so
  /// every built-in-only `run_bdd` reuses the same `Arc` instead of
  /// re-walking `inventory` + recompiling expressions per call.
  pub(crate) fn builtin_registry(&self) -> Arc<ferridriver_bdd::registry::StepRegistry> {
    Arc::clone(
      self
        .builtin_steps
        .get_or_init(|| Arc::new(ferridriver_bdd::registry::StepRegistry::build())),
    )
  }

  /// Drop every cache keyed by `context`, including the page wrapper.
  /// Prefer this over `state.invalidate_context` from tool code: the
  /// wrapper cache lives on the server, so invalidating only the state
  /// caches leaves a wrapper pointing at a page that is going away.
  pub(crate) fn invalidate_context(&self, context: &str) {
    self.state.invalidate_context(context);
    self
      .page_wrappers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(context);
  }

  /// Drop every per-context cache, including page wrappers. Used after
  /// a change that can invalidate more contexts than the caller can
  /// name (closing a whole instance, shutting the server down).
  pub(crate) fn invalidate_all_caches(&self) {
    self.state.invalidate_all();
    self
      .page_wrappers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clear();
  }

  /// Release everything the server holds for `context` after its
  /// browser-side state is gone: the caches, and the script session
  /// (its `QuickJS` VM, `vars`, and HTTP client). Without the session
  /// drop, closing a context left a warm VM alive until the LRU cap or
  /// the idle TTL got to it.
  pub(crate) async fn release_context(&self, context: &str) -> Result<(), String> {
    self.invalidate_context(context);
    self.sessions.remove(context).await.map_err(|error| error.message)
  }

  /// Close every browser this server launched and drop all per-context
  /// caches. Idempotent, and safe to call while tool calls are in
  /// flight — they fail with `TargetClosed` rather than hanging.
  ///
  /// The transports are pipes for `cdp-pipe` / `webkit`, so those
  /// browsers exit on their own when the process dies. A `cdp-raw`
  /// Chrome or a `bidi` Firefox does not: it is reparented to pid 1 and
  /// stays, which is why every exit path has to come through here.
  /// # Errors
  /// Failed cleanup retains its owners and keeps script admission closed for retry.
  pub async fn shutdown_browsers(&self) -> Result<(), String> {
    self.close_browsers(false).await
  }

  pub(crate) async fn shutdown(&self) -> Result<(), String> {
    self.close_browsers(true).await
  }

  async fn close_browsers(&self, terminal: bool) -> Result<(), String> {
    let browsers = Box::pin(async {
      self
        .state
        .write()
        .await
        .shutdown()
        .await
        .map_err(|error| ferridriver_script::ScriptError::internal(error.to_string()))
    });
    let result = if terminal {
      self.sessions.close_after(browsers).await
    } else {
      self.sessions.clear_after(browsers).await
    };
    self.invalidate_all_caches();
    result.map_err(|error| error.message)
  }

  pub async fn context_guard(&self, context: &str) -> tokio::sync::OwnedMutexGuard<()> {
    let lock = self
      .state
      .context_locks
      .entry(context.to_string())
      .or_insert_with(|| Arc::new(Mutex::new(())))
      .clone();
    lock.lock_owned().await
  }

  // Backward-compat alias.
  pub async fn session_guard(&self, context: &str) -> tokio::sync::OwnedMutexGuard<()> {
    self.context_guard(context).await
  }

  /// Ensure a browser instance exists for the context and return its active `AnyPage`.
  ///
  /// Fast path (instance exists): shared read lock -- concurrent reads allowed.
  /// Slow path (cold start): exclusive write lock -- only when launching a new browser.
  async fn ensure_active_page(&self, context: &str) -> Result<AnyPage, ErrorData> {
    {
      let state = self.state.read().await;
      if let Ok(any_page) = state.active_page(context) {
        return Ok(any_page.clone());
      }
    }
    // Cold start: validate the target instance before launching so a bad session
    // key (e.g. a bare env name resolving to the 'default' instance) fails loudly
    // instead of silently launching an unmapped browser on the wrong environment.
    // Parse through `SessionKey` rather than splitting by hand: a bare
    // key that names a configured instance selects that instance, and a
    // hand-rolled split silently sent every bare key to `default`.
    let key = self.state.session_key(context).await;
    // `instance_health` may run the operator's args command, which shells
    // out under its own timeout; off the reactor so a cold start does not
    // stall every other session's tasks on this worker.
    let config = Arc::clone(&self.config);
    let instance = key.instance.to_string();
    tokio::task::spawn_blocking(move || config.instance_health(&instance))
      .await
      .map_err(|e| Self::err(format!("instance health check failed: {e}")))?
      .map_err(Self::err)?;
    let ctx_ref = ferridriver::context::ContextRef::new_async(self.state.state_arc(), context.to_string()).await;
    let page = Box::pin(ctx_ref.active_or_new_page()).await.map_err(Self::err)?;
    self.invalidate_context(context);
    Ok(page)
  }

  /// Get a `Page` for a context, ensuring the required browser instance exists.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance cannot be launched or the active page
  /// for the given context cannot be retrieved.
  pub async fn page(&self, context: &str) -> Result<Arc<Page>, ErrorData> {
    let any_page = Box::pin(self.ensure_active_page(context)).await?;
    Ok(self.wrapper_for(context, any_page).await)
  }

  /// Cached-or-fresh `Page` wrapper for `context` over `any_page`.
  ///
  /// Returns the cached wrapper when it still wraps the same underlying
  /// browser page; otherwise builds one (binding the context so
  /// `page.context()` resolves to the live `BrowserContext` — Playwright
  /// parity) and caches it. Reusing the wrapper keeps wrapper-level
  /// state (default timeouts, `emulateMedia` merge state) alive across
  /// tool calls instead of resetting it per call. `Page::with_context`
  /// is sync and its frame-event listener spawn is latched per backend
  /// page, so the occasional rebuild is cheap.
  async fn wrapper_for(&self, context: &str, any_page: AnyPage) -> Arc<Page> {
    {
      let cache = self
        .page_wrappers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      if let Some(cached) = cache.get(context)
        && cached.inner().same_backend_page(&any_page)
      {
        return Arc::clone(cached);
      }
    }
    let ctx_ref = ferridriver::context::ContextRef::new_async(self.state.state_arc(), context.to_string()).await;
    let mut cache = self
      .page_wrappers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(cached) = cache.get(context)
      && cached.inner().same_backend_page(&any_page)
    {
      return Arc::clone(cached);
    }
    let page = Page::with_context(any_page, ctx_ref);
    cache.insert(context.to_string(), Arc::clone(&page));
    page
  }

  /// Get raw `AnyPage` (for low-level ops that Page doesn't cover yet).
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance cannot be launched or the active page
  /// for the given context cannot be retrieved.
  pub async fn raw_page(&self, context: &str) -> Result<AnyPage, ErrorData> {
    Box::pin(self.ensure_active_page(context)).await
  }

  /// Get a `Page` and `ContextRef` for a session in a single operation.
  ///
  /// This is the primary entry point for BDD integration -- provides both
  /// the page (for DOM interaction) and the context handle (for cookies,
  /// permissions, etc.) on the same live MCP session. A single
  /// `ensure_active_page` call handles both, avoiding redundant lock
  /// acquisitions.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance cannot be launched or accessed.
  pub async fn page_and_context(
    &self,
    context: &str,
  ) -> Result<(Arc<Page>, ferridriver::context::ContextRef), ErrorData> {
    let any_page = Box::pin(self.ensure_active_page(context)).await?;
    let ctx_ref = ferridriver::context::ContextRef::new_async(self.state.state_arc(), context.to_string()).await;
    let page = self.wrapper_for(context, any_page).await;
    Ok((page, ctx_ref))
  }

  /// Resolve ref to element -- delegates to `actions::resolve_element`.
  ///
  /// # Errors
  ///
  /// Returns an error if neither ref nor selector resolves to a valid element,
  /// or if the underlying element lookup fails.
  pub async fn resolve(
    page: &Page,
    ref_map: &rustc_hash::FxHashMap<String, i64>,
    r#ref: Option<&String>,
    selector: Option<&String>,
  ) -> ferridriver::Result<AnyElement> {
    actions::resolve_element(
      page.inner(),
      ref_map,
      r#ref.map(String::as_str),
      selector.map(String::as_str),
    )
    .await
  }

  /// Build snapshot text and store `ref_map` for the context.
  /// Uses a 5-second timeout to avoid hanging on unresponsive pages.
  /// Stores the `ref_map` via wait-free `ArcSwap` — never drops updates.
  pub async fn snap(&self, page: &Page, context: &str) -> String {
    let snap_fut = page.snapshot_for_ai();
    match tokio::time::timeout(std::time::Duration::from_secs(5), snap_fut).await {
      Ok(Ok(result)) => {
        // Wait-free store via cached ArcSwap handle
        if let Some(handle) = self.state.ref_map_handle(context).await {
          handle.store(Arc::new(result.ref_map));
        } else {
          // Fallback: read-lock state to store (context may not be cached yet)
          let state = self.state.read().await;
          state.set_ref_map(context, result.ref_map);
        }
        result.full
      },
      Ok(Err(e)) => format!("\n[snapshot error: {e}]"),
      Err(_) => "\n[snapshot timed out — page may be unresponsive or have a very large DOM]".to_string(),
    }
  }

  /// Action result: description + auto-snapshot.
  ///
  /// Runs the independent post-action reads (`tabs_section`, `snap`,
  /// `webmcp_section`) concurrently after settling to avoid 3 serial
  /// browser round-trips per MCP action.
  ///
  /// # Errors
  ///
  /// Returns an `ErrorData` if snapshot acquisition fails critically
  /// (soft failures produce inline error text instead).
  pub async fn action_ok(&self, page: &Arc<Page>, context: &str, msg: &str) -> Result<CallToolResult, ErrorData> {
    self.settle(page).await;
    let (tabs, snap, tools) = tokio::join!(
      self.tabs_section(context),
      self.snap(page, context),
      self.webmcp_section(page),
    );
    Ok(self.ok_text(format!("{msg}\n\n{tabs}{snap}{tools}")))
  }

  /// The open tabs of `context`, unless there is exactly one (returns `None`
  /// without issuing `url`/`title` RPCs).
  pub(crate) async fn open_tabs_if_not_single(&self, context: &str) -> Option<Vec<ferridriver::response::TabState>> {
    ferridriver::response::TabState::capture_if_not_single(&self.state.state_arc(), context).await
  }

  /// `### Open tabs` and a blank line, ahead of a snapshot, as Playwright
  /// MCP lists the tabs beside every snapshot; empty when there is exactly
  /// one tab.
  pub(crate) async fn tabs_section(&self, context: &str) -> String {
    let Some(tabs) = self.open_tabs_if_not_single(context).await else {
      return String::new();
    };
    let mut response = ferridriver::response::Response::new().with_secrets(self.secrets.clone());
    response.tabs(&tabs);
    let rendered = response.render();
    if rendered.is_empty() {
      rendered
    } else {
      format!("{rendered}\n")
    }
  }

  /// Wait until the tabs `context`'s browser has opened so far are listed,
  /// so a report built next names the page the caller's next call acts on:
  /// a click that opens a tab returns before the tab joins its context.
  /// A tab still loading when the navigation timeout runs out is left for
  /// a later call to find, rather than failing what the caller asked for.
  pub(crate) async fn opened_tabs_settled(&self, context: &str) {
    let Ok(page) = Box::pin(self.page(context)).await else {
      return;
    };
    let within = std::time::Duration::from_millis(page.default_navigation_timeout());
    let ctx_ref = ferridriver::context::ContextRef::new_async(self.state.state_arc(), context.to_string()).await;
    if let Err(error) = ctx_ref.opened_pages_settled(within).await {
      tracing::debug!(%error, "tabs the call opened are not all listed yet");
    }
  }

  /// Let work the action triggered finish before anyone looks at the page.
  ///
  /// Network quiet is the signal, capped by the configured budget: a page
  /// that polls never goes quiet, and waiting for it would hang the tool
  /// rather than answer it. Failure is ignored on purpose. This is a
  /// courtesy wait, and a page that will not settle is still a page the
  /// caller should get a snapshot of.
  pub(crate) async fn settle(&self, page: &Arc<Page>) {
    let budget = self.config.settle_timeout_ms();
    if budget == 0 {
      return;
    }
    let _ = tokio::time::timeout(
      std::time::Duration::from_millis(budget),
      page.wait_for_load_state(Some("networkidle")),
    )
    .await;
  }

  /// Resolve `session`, hold its lock for the whole call, and hand the live
  /// page to `f`.
  ///
  /// Every page-driving tool needs these three steps in this order, and the
  /// order is load-bearing: resolving the page before taking the guard races
  /// a concurrent call on the same context, which opens a second page on a
  /// cold one. Doing it once here is what keeps that true across every
  /// handler instead of in each of them.
  ///
  /// The page future is boxed because it is large enough to trip
  /// `clippy::large_futures` at the call site — once here rather than at
  /// each of them.
  ///
  /// # Errors
  ///
  /// Propagates a failure to launch or attach the context's browser, and
  /// whatever `f` returns.
  pub(crate) async fn on_page<F, Fut, T>(&self, session: Option<&String>, f: F) -> Result<T, ErrorData>
  where
    F: FnOnce(Arc<Page>, String) -> Fut,
    Fut: std::future::Future<Output = Result<T, ErrorData>>,
  {
    let context = sess(session).to_string();
    let _guard = self.session_guard(&context).await;
    let page = Box::pin(self.page(&context)).await?;
    f(page, context).await
  }

  /// Resolve `session` and hold its lock for the whole call, without
  /// opening a page.
  ///
  /// [`Self::on_page`] for the tools that drive one; this for the tools that
  /// act on the context itself (listing, closing, reading its logs), where
  /// resolving a page would launch a browser the call does not need.
  ///
  /// # Errors
  ///
  /// Whatever `f` returns.
  pub(crate) async fn on_session<F, Fut, T>(&self, session: Option<&String>, f: F) -> Result<T, ErrorData>
  where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<T, ErrorData>>,
  {
    let context = sess(session).to_string();
    let _guard = self.session_guard(&context).await;
    f(context).await
  }

  /// A text content block with declared secrets redacted.
  ///
  /// Tools build their replies through this rather than `ContentBlock::text`
  /// directly: redaction that covers only some replies is not redaction, and
  /// a page's own text (an evaluated value, a snapshot, a search hit) is a
  /// routine place for a credential to surface.
  pub(crate) fn text(&self, body: impl Into<String>) -> ContentBlock {
    ContentBlock::text(self.secrets.redact(&body.into()).into_owned())
  }

  /// A success reply carrying one redacted text block.
  pub(crate) fn ok_text(&self, body: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![self.text(body)])
  }

  /// Emit an MCP progress notification for the in-flight tool call (SEP-2575).
  ///
  /// `token` is the caller's `_meta.progressToken`; a `None` (client did not
  /// opt into progress) or a transport hiccup is silently ignored — progress
  /// is strictly best-effort and never fails the tool. `progress`/`total`
  /// use the same units the tool documents; `message` is a short human note.
  pub(crate) async fn emit_progress(
    peer: &rmcp::service::Peer<rmcp::RoleServer>,
    token: Option<&rmcp::model::ProgressToken>,
    progress: f64,
    total: Option<f64>,
    message: &str,
  ) {
    let Some(token) = token else { return };
    let mut param = rmcp::model::ProgressNotificationParam::new(token.clone(), progress).with_message(message);
    if let Some(total) = total {
      param = param.with_total(total);
    }
    let _ = peer.notify_progress(param).await;
  }
}
