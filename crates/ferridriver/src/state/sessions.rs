//! Composite-key session, context, and page lifecycle operations on `BrowserState`.

use std::sync::Arc;

use crate::backend::{AnyBrowser, AnyPage, BackendKind};
use crate::context::BrowserContext;
use crate::error::{FerriError, Result};
use rustc_hash::FxHashMap as HashMap;

use super::launch::{connect_browser, resolve_with_prefix};
use super::{
  BrowserInstance, BrowserState, ClosedContexts, ConnectMode, ConsoleMessage, ContextLogHandles, DetachedContext,
  DialogEvent, Listing, PageOpenPlan, Request, SessionKey, VisitedOrigins,
};

fn forward_page_event_to_context(
  context_events: &crate::events::ContextEventEmitter,
  visited_origins: &VisitedOrigins,
  composite: &str,
  backref: &crate::backend::PageBackref,
  event: &crate::events::PageEvent,
  sequence: u64,
) {
  use crate::events::{ContextEvent, PageEvent};
  if let PageEvent::FrameNavigated(info) = event {
    visited_origins.visit(composite, &info.url);
  }
  if !context_events.has_consumers() {
    return;
  }
  let emit = |ev| context_events.emit_stamped(ev, sequence);
  // Frame- and page-lifecycle mirror events need the public wrapper
  // `Arc<Page>` so the binding can mint a `Frame` / deliver a `Page`.
  // Upgrade lazily; skip if every wrapper has been dropped (page is going away).
  let upgrade = || backref.upgrade();
  match event {
    PageEvent::PageError(err) => emit(ContextEvent::WebError(err.clone())),
    PageEvent::Download(d) => emit(ContextEvent::Download(d.clone())),
    PageEvent::DialogClosed(d) => emit(ContextEvent::DialogClosed(d.clone())),
    PageEvent::FrameAttached(info) => {
      if let Some(page) = upgrade() {
        emit(ContextEvent::FrameAttached {
          page,
          frame_id: info.frame_id.clone(),
        });
      }
    },
    PageEvent::FrameDetached { frame_id } => {
      if let Some(page) = upgrade() {
        emit(ContextEvent::FrameDetached {
          page,
          frame_id: frame_id.clone(),
        });
      }
    },
    PageEvent::FrameNavigated(info) => {
      if let Some(page) = upgrade() {
        emit(ContextEvent::FrameNavigated {
          page,
          frame_id: info.frame_id.clone(),
        });
      }
    },
    PageEvent::Close => {
      if let Some(page) = upgrade() {
        emit(ContextEvent::PageClose(page));
      }
    },
    PageEvent::Load => {
      if let Some(page) = upgrade() {
        emit(ContextEvent::PageLoad(page));
      }
    },
    PageEvent::Console(message) => emit(ContextEvent::Console(message.clone())),
    PageEvent::Request(request) => {
      if let Some(page) = upgrade() {
        emit(ContextEvent::Request {
          page,
          request: request.clone(),
        });
      }
    },
    PageEvent::Response(response) => {
      if let Some(page) = upgrade() {
        emit(ContextEvent::Response {
          page,
          response: response.clone(),
        });
      }
    },
    PageEvent::RequestFinished(request) => {
      if let Some(page) = upgrade() {
        emit(ContextEvent::RequestFinished {
          page,
          request: request.clone(),
        });
      }
    },
    PageEvent::RequestFailed(request) => {
      if let Some(page) = upgrade() {
        emit(ContextEvent::RequestFailed {
          page,
          request: request.clone(),
        });
      }
    },
    _ => {},
  }
}

impl BrowserState {
  /// Ensure a browser instance is launched. If it already exists, no-op.
  ///
  /// Holds the caller's `&mut` borrow for the whole launch; server code
  /// that shares the state behind an `RwLock` should call
  /// [`Self::ensure_instance_shared`] instead.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to start or connection fails.
  pub async fn ensure_instance(&mut self, instance_name: &str) -> Result<()> {
    self.ensure_not_disposed()?;
    self.check_cleanup_pending(instance_name)?;
    if self.instance_is_live(instance_name) {
      return Ok(());
    }
    if self.instances.contains_key(instance_name) {
      // The browser died (crash, OOM-kill, external SIGKILL). Without this
      // the entry stays and every later session routed to this name gets
      // `TargetClosed` forever, because the only thing that ever removed
      // an instance was an explicit shutdown.
      tracing::warn!(
        target: "ferridriver::state",
        instance = instance_name,
        "browser instance is gone; relaunching",
      );
    }

    let runtime = tokio::runtime::Handle::try_current().map_err(|error| FerriError::backend(error.to_string()))?;
    let spec = self.launch_spec();
    let (generation, allocation, completion) = self.begin_allocation(instance_name);
    let owner = Arc::clone(&allocation);
    let name = instance_name.to_owned();
    let (reply, completed) = tokio::sync::oneshot::channel();
    runtime.spawn(async move {
      let result = owner.produce(spec, &name).await;
      drop(completion);
      if let Err(Err(error)) = reply.send(result) {
        tracing::debug!(%error, instance = name, "Browser allocation finished after its caller disconnected");
      }
    });
    let created = completed
      .await
      .map_err(|error| FerriError::backend(format!("Browser allocation task stopped: {error}")))?;
    Box::pin(self.install_allocation(instance_name, generation, &allocation, created)).await
  }

  /// Backwards-compat: ensure the "default" instance.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to start.
  pub async fn ensure_browser(&mut self) -> Result<()> {
    Box::pin(self.ensure_instance("default")).await
  }

  /// Connect to a running browser at the given WebSocket or HTTP URL.
  /// Creates a new instance with the given name using `CdpWs` backend.
  ///
  /// # Errors
  ///
  /// Returns an error if the WebSocket connection or page discovery fails.
  pub async fn connect_to_url(&mut self, instance_name: &str, url: &str) -> Result<usize> {
    use crate::backend::cdp::CdpBrowser;
    self.ensure_not_disposed()?;

    // Drop existing instance if any. A bare `remove` would orphan a browser this
    // server launched, leaving the process alive with no handle to close it.
    self.evict_instance(instance_name).await?;

    let ws_url = if url.starts_with("ws://") || url.starts_with("wss://") {
      url.to_string()
    } else {
      discover_ws_from_http(url).await?
    };

    let browser = AnyBrowser::Cdp(Box::pin(CdpBrowser::connect(&ws_url)).await?);
    // Skip viewport override for existing pages — connect_to_url attaches to a
    // user-managed browser whose window size should not be touched.
    Box::pin(self.install_instance(instance_name, browser, true, true)).await?;
    Ok(self.instance(instance_name)?.context("default")?.pages.len())
  }

  /// Auto-discover and connect to a running Chrome instance.
  ///
  /// Checks the instance resolver first (allowing consumers to route
  /// `instance_name` to a managed browser, e.g. connecting to an
  /// environment-specific browser launched by another tool). Falls back
  /// to reading Chrome's `DevToolsActivePort` file for the given
  /// channel/profile.
  ///
  /// The resolver is tried with the full `instance_name` first, then with
  /// the prefix before `:` (supporting composite keys like `"staging:default"`
  /// where only the first segment identifies the browser instance).
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome discovery or connection fails.
  pub async fn connect_auto(
    &mut self,
    instance_name: &str,
    channel: &str,
    user_data_dir: Option<&str>,
  ) -> Result<usize> {
    if let Some(resolved) = self.resolve_via_instance_fn_off_thread(instance_name).await {
      return self.connect_with_resolved_mode(instance_name, resolved).await;
    }

    let ws_url = discover_chrome_ws(channel, user_data_dir)?;
    Box::pin(self.connect_to_url(instance_name, &ws_url)).await
  }

  /// [`Self::resolve_via_instance_fn`] on the blocking pool.
  ///
  /// The resolver shells out (a discover command may poll for a browser)
  /// and TCP-probes endpoints. The launch path already moved it off the
  /// reactor; this path called it inline, so one cold `connect` stalled
  /// every other task on that worker thread.
  async fn resolve_via_instance_fn_off_thread(&self, instance_name: &str) -> Option<ConnectMode> {
    let resolver = self.instance_resolver_fn.clone()?;
    let name = instance_name.to_string();
    match tokio::task::spawn_blocking(move || resolve_with_prefix(&resolver, &name)).await {
      Ok(mode) => mode,
      Err(e) => {
        tracing::warn!(target: "ferridriver::state", error = %e, "instance resolver task failed");
        None
      },
    }
  }

  /// Connect using a resolved mode from the instance resolver.
  async fn connect_with_resolved_mode(&mut self, instance_name: &str, mode: ConnectMode) -> Result<usize> {
    match mode {
      ConnectMode::ConnectUrl(url) => Box::pin(self.connect_to_url(instance_name, &url)).await,
      mode @ ConnectMode::ConnectUrlWith { .. } => {
        self.ensure_not_disposed()?;
        self.evict_instance(instance_name).await?;
        let browser = Box::pin(connect_browser(
          &mode,
          self.backend_kind,
          self.connection_headers.as_ref(),
          &[],
        ))
        .await?;
        Box::pin(self.install_instance(instance_name, browser, true, true)).await?;
        Ok(self.instance(instance_name)?.context("default")?.pages.len())
      },
      ConnectMode::Device { .. } | ConnectMode::WebDriver { .. } => Err(FerriError::unsupported(
        "connect_to_url cannot be used with a WebDriver session mode",
      )),
      ConnectMode::AutoConnect { channel, user_data_dir } => {
        let ws_url = discover_chrome_ws(&channel, user_data_dir.as_deref())?;
        Box::pin(self.connect_to_url(instance_name, &ws_url)).await
      },
      // Launch mode is not meaningful for connect_auto -- the caller should
      // launch a browser separately and use ConnectUrl for the result.
      ConnectMode::Launch => Err(FerriError::Backend(format!(
        "Instance resolver returned Launch mode for '{instance_name}', expected ConnectUrl"
      ))),
    }
  }

  // ── Routing helpers ─────────────────────────────────────────────────────

  fn instance(&self, name: &str) -> Result<&BrowserInstance> {
    self.instances.get(name).ok_or_else(|| {
      FerriError::invalid_argument(
        "instance",
        format!("'{name}' not found. It will be created on first use."),
      )
    })
  }

  /// Access the default instance's backend browser handle. Used by
  /// `Browser::version()` to read the cached CDP `Browser.getVersion().product`.
  pub(crate) fn default_browser(&self) -> Option<&AnyBrowser> {
    self.instance_browser("default")
  }

  pub(crate) fn instance_browser(&self, instance: &str) -> Option<&AnyBrowser> {
    self.instances.get(instance).map(|i| &i.browser)
  }

  fn instance_mut(&mut self, name: &str) -> Result<&mut BrowserInstance> {
    self
      .instances
      .get_mut(name)
      .ok_or_else(|| FerriError::invalid_argument("instance", format!("'{name}' not found")))
  }

  // ── Public methods (all parse composite keys) ───────────────────────────

  /// Open a new page in a context. `context` is a composite key like `"staging:admin"`.
  ///
  /// # Errors
  ///
  /// Returns an error if the instance or page creation fails.
  /// Create a new page in the given context. Returns the `AnyPage` directly
  /// (no second lookup needed).
  pub async fn open_page(&mut self, context: &str, url: &str) -> Result<AnyPage> {
    let key = self.session_key(context);
    Box::pin(self.open_page_keyed(&key, url)).await
  }

  /// Snapshot the immutable data needed to open a page without holding the
  /// global browser-state write lock across protocol round-trips.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance does not exist.
  pub fn page_open_plan(&self, key: &SessionKey) -> Result<PageOpenPlan> {
    self.ensure_not_disposed()?;
    let inst = self.instance(&key.instance)?;
    let browser_context_id = if &*key.context == "default" {
      None
    } else {
      inst
        .contexts
        .get(&*key.context)
        .and_then(|ctx| ctx.cdp_context_id.clone())
    };

    Ok(PageOpenPlan {
      browser: inst.browser.clone(),
      viewport: self.default_viewport.clone(),
      browser_context_id,
    })
  }

  /// Record that `key`'s context now exists in the browser as
  /// `browser_context_id`, before any page of it does, so the first
  /// page opens into it rather than into a context of its own.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance does not exist.
  pub fn register_backend_context(&mut self, key: &SessionKey, browser_context_id: String) -> Result<()> {
    self.ensure_not_disposed()?;
    let inst = self.instance_mut(&key.instance)?;
    inst.context_mut(&key.context).cdp_context_id = Some(browser_context_id);
    Ok(())
  }

  /// Register a newly created page back into the state after off-lock backend work.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance or context does not exist.
  pub fn register_opened_page(
    &mut self,
    key: &SessionKey,
    page: AnyPage,
    browser_context_id: Option<String>,
  ) -> Result<()> {
    self.wire_opened_page(key, &page, browser_context_id)?;
    self.list_opened_page(key, page, Listing::Active)
  }

  /// Record what `page` logs and forward its events to its context,
  /// without listing it. A popup is wired before it runs, so nothing its
  /// first document does is missed, and listed once that document has
  /// committed ([`Self::list_opened_page`]).
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance does not exist.
  pub(crate) fn wire_opened_page(
    &mut self,
    key: &SessionKey,
    page: &AnyPage,
    browser_context_id: Option<String>,
  ) -> Result<()> {
    if let Err(error) = self.ensure_not_disposed() {
      page.dispose_local();
      return Err(error);
    }
    // Pull the context-event emitter for this session key BEFORE
    // taking the mutable instance borrow, so we can hand it to the
    // per-page → per-context `PageError` → `WebError` bridge spawned
    // below. Every `ContextRef` cloned with the same composite key
    // receives the same emitter via the registry, so listeners
    // registered anywhere (NAPI `context.on('weberror')`, QuickJS
    // `context.waitForEvent('weberror')`, etc.) all observe events
    // fanned out here.
    let composite = key.to_composite();
    let context_events = self.get_or_create_context_events(&composite);
    let visited_origins = self.visited_origins.clone();

    let inst = self.instance_mut(&key.instance)?;
    let ctx = inst.context_mut(&key.context);
    if let Some(id) = browser_context_id {
      ctx.cdp_context_id = Some(id);
    }
    page.attach_listeners(
      Arc::clone(&ctx.console_log),
      Arc::clone(&ctx.network_log),
      Arc::clone(&ctx.dialog_log),
    );
    let _ = page.dialog_manager().register_context_bridge(context_events.clone());

    // Install the page→context bridge inline on the page's event dispatcher
    // (once per registered page). Running inline avoids spawning a second
    // tokio task + unbounded channel + per-event clone for every page, and
    // skips `backref.upgrade()` + `ContextEvent` construction altogether when
    // no context-level consumers are attached. Holds only the weak backref
    // slot (never a strong `AnyPage` clone) so page teardown stays leak-free.
    let backref = page.page_backref_handle();
    let _ = page.events().set_bridge(Arc::new(move |event, sequence| {
      forward_page_event_to_context(&context_events, &visited_origins, &composite, &backref, event, sequence);
    }));

    Ok(())
  }

  /// List a wired page in its context, as the active page or as a popup.
  ///
  /// # Errors
  ///
  /// Returns an error if the state was disposed or the browser instance
  /// does not exist.
  pub(crate) fn list_opened_page(&mut self, key: &SessionKey, page: AnyPage, listing: Listing) -> Result<()> {
    if let Err(error) = self.ensure_not_disposed() {
      page.dispose_local();
      return Err(error);
    }
    let listed_under = key.to_composite();
    let context_pages = self.context_pages.clone();
    let inst = self.instance_mut(&key.instance)?;
    let ctx = inst.context_mut(&key.context);
    // Opening a tab is the one moment a `&mut` on the context is
    // guaranteed, so it is where the tabs the browser already closed get
    // dropped. Otherwise a long session's `pages` only ever grows and
    // every command scans past the corpses.
    ctx.prune_closed_pages();
    match listing {
      Listing::Active => {
        ctx.pages.push(page);
        ctx.active_page_idx = ctx.pages.len() - 1;
      },
      Listing::Popup => ctx.list_popup(page),
    }
    context_pages.set(&listed_under, &ctx.pages);
    Ok(())
  }

  /// Set which page each context of `instance` makes active when its
  /// active page closes, for the contexts it has and the ones it makes.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance does not exist.
  pub fn set_refocus(&mut self, instance: &str, refocus: crate::context::Refocus) -> Result<()> {
    let inst = self.instance_mut(instance)?;
    inst.refocus = refocus;
    for ctx in inst.contexts.values_mut() {
      ctx.refocus = refocus;
    }
    Ok(())
  }

  /// Same as `open_page` but accepts a pre-parsed `SessionKey` (avoids re-parsing).
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance or page creation fails.
  pub async fn open_page_keyed(&mut self, key: &SessionKey, url: &str) -> Result<AnyPage> {
    if !self.instances.contains_key(&*key.instance) {
      Box::pin(self.ensure_instance(&key.instance)).await?;
    }

    let plan = self.page_open_plan(key)?;
    let (page, cdp_ctx_id) = if &*key.context == "default" {
      (
        Box::pin(
          plan
            .browser
            .new_page(url, plan.browser_context_id.as_deref(), plan.viewport.as_ref()),
        )
        .await?,
        None,
      )
    } else if let Some(existing_ctx_id) = plan.browser_context_id.clone() {
      (
        Box::pin(
          plan
            .browser
            .new_page(url, Some(&existing_ctx_id), plan.viewport.as_ref()),
        )
        .await?,
        Some(existing_ctx_id),
      )
    } else {
      // Legacy `open_page_keyed` path — no options bag flows through
      // here (used by older MCP call sites that don't go via
      // `ContextRef::new_page`). Proxy wiring happens on the
      // `ContextRef` path exclusively.
      let ctx_id = plan.browser.new_context(None).await?;
      let p = Box::pin(plan.browser.new_page(url, Some(&ctx_id), plan.viewport.as_ref())).await?;
      (p, Some(ctx_id))
    };

    self.register_opened_page(key, page.clone(), cdp_ctx_id)?;
    Ok(page)
  }

  /// # Errors
  ///
  /// Returns an error if the instance, context, or page does not exist.
  pub fn active_page(&self, context: &str) -> Result<&AnyPage> {
    let key = self.session_key(context);
    let inst = self.instance(&key.instance)?;
    // A browser killed from outside leaves its pages in place, and
    // handing one back sends every later command down a dead transport
    // (one 30s timeout per call) instead of relaunching. The instance is
    // evicted and rebuilt by the caller's cold-start path.
    if !inst.browser.is_alive() {
      return Err(FerriError::target_closed(Some(format!(
        "browser for instance '{}' is gone",
        key.instance
      ))));
    }
    let ctx = inst.context(&key.context)?;
    if let Some(page) = ctx.active_page() {
      return Ok(page);
    }
    // Distinguish "never had a page" from "every page has been closed": the
    // latter is a live context whose tabs are gone, and reporting it as an
    // invalid argument sent callers looking for a bad context name.
    if !ctx.pages.is_empty() {
      return Err(FerriError::target_closed(Some(format!(
        "every page in context '{context}' has been closed"
      ))));
    }
    Err(FerriError::invalid_argument(
      "context",
      format!("no pages in context '{context}'"),
    ))
  }

  /// # Errors
  ///
  /// Returns an error if the instance or context does not exist.
  pub fn context(&self, context: &str) -> Result<&BrowserContext> {
    let key = self.session_key(context);
    let inst = self.instance(&key.instance)?;
    inst.context(&key.context)
  }

  /// # Errors
  ///
  /// Returns an error if the instance or context does not exist.
  pub fn context_mut_checked(&mut self, context: &str) -> Result<&mut BrowserContext> {
    let key = self.session_key(context);
    let inst = self.instance_mut(&key.instance)?;
    inst.context_mut_checked(&key.context)
  }

  /// Drop every per-context registry entry for `composite`.
  ///
  /// Thirteen registries are keyed by composite session key, and context
  /// names come from a process-wide counter that never reuses a value, so
  /// without this each one grows by an entry for every context the process
  /// ever opens. The costs are not uniform: a `storageState` bag holds the
  /// session's cookies and localStorage (dead auth material kept in RAM),
  /// and a `routeFromHAR(update:true)` recorder holds the whole parsed HAR
  /// — megabytes per context.
  ///
  /// Callers must have already drained anything with an exit action:
  /// `ContextRef::close_impl` flushes HAR updates before it gets here.
  /// Live `ContextRef` clones are unaffected — they hold their own `Arc`
  /// clones of the closed-flag and emitter rather than reading the map.
  pub(super) async fn purge_context_registries(&self, composite: &str) {
    fn drop_key<T>(map: &Arc<std::sync::Mutex<HashMap<String, T>>>, key: &str) {
      map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(key);
    }
    drop_key(&self.context_events, composite);
    drop_key(&self.context_closed, composite);
    drop_key(&self.context_test_id_attribute, composite);
    drop_key(&self.record_video, composite);
    drop_key(&self.context_options, composite);
    drop_key(&self.har_recorders, composite);
    drop_key(&self.context_har_updates, composite);
    drop_key(&self.context_relays, composite);
    self
      .clock_installed
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(composite);
    self
      .storage_state_hydrated
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(composite);
    self.visited_origins.forget(composite);
    self
      .http_authenticated
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(composite);
    self.context_pages.forget(composite);
    self
      .record_har_installed
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(composite);
    self.context_bindings.write().await.remove(composite);
    self.context_callbacks.write().await.remove(composite);
    self
      .context_credentials
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(composite);
    self.context_ws_routes.write().await.remove(composite);
    self.context_routes.write().await.remove(composite);
    self.context_init_scripts.write().await.remove(composite);
  }

  /// Empty a context for its next user without closing its pages: they
  /// leave the page list and every per-context registry is dropped, as
  /// [`Self::remove_context`] drops them. The pages come back still open,
  /// for the caller to close once the context has another tab to live in.
  pub(crate) async fn reset_context(&mut self, context: &str) -> Vec<AnyPage> {
    let key = self.session_key(context);
    let pages = self
      .instances
      .get_mut(&*key.instance)
      .and_then(|inst| inst.contexts.remove(&*key.context))
      .map(|ctx| ctx.pages)
      .unwrap_or_default();
    self.purge_context_registries(&key.to_composite()).await;
    pages
  }

  /// Detach a context from in-memory state under the write lock, returning the
  /// backend teardown work so callers can drop the global `BrowserState` lock
  /// before awaiting CDP/BiDi/WebKit I/O.
  pub(crate) async fn detach_context(&mut self, context: &str) -> Option<DetachedContext> {
    let key = self.session_key(context);
    let composite = key.to_composite();
    let opts = self.get_context_options(&composite);
    let authenticated = opts.as_ref().is_some_and(|options| options.http_credentials.is_some())
      || self
        .http_authenticated
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&composite);
    let download_opt_touched = opts.as_ref().is_some_and(|options| options.accept_downloads.is_some());
    let recycle = self.closed_contexts == ClosedContexts::Recycled && !authenticated;
    let origins = if recycle {
      self.visited_origins.of(&composite)
    } else {
      Vec::new()
    };

    let detached = if let Some(inst) = self.instances.get_mut(&*key.instance) {
      let (ctx_id, pages) = inst
        .context(&key.context)
        .ok()
        .map(|c| (c.cdp_context_id.clone(), c.pages.clone()))
        .unwrap_or_default();
      let download_touched = download_opt_touched || pages.iter().any(AnyPage::download_behavior_touched);
      let single_page_target = if pages.len() == 1 {
        pages[0].cdp_target_id().map(str::to_string)
      } else {
        None
      };
      let browser = inst.browser.clone();
      inst.remove_context(&key.context);
      Some(DetachedContext {
        browser,
        ctx_id,
        pages,
        recycle,
        origins,
        hints: crate::backend::cdp::RetireHints {
          download_touched,
          single_page_target,
        },
      })
    } else {
      None
    };

    // Registries are keyed by composite, and callers pass a bare context
    // name — purging with the raw argument would silently match nothing.
    self.purge_context_registries(&composite).await;
    detached
  }

  /// Remove a context. If it has a CDP browser context ID, dispose it
  /// (one CDP call kills the context + all pages, matching Playwright's doClose).
  pub async fn remove_context(&mut self, context: &str) {
    if let Some(detached) = self.detach_context(context).await {
      detached.finish().await;
    }
  }

  /// # Errors
  ///
  /// Returns an error if the context does not exist or the page index is out of range.
  pub fn select_page(&mut self, context: &str, page_idx: usize) -> Result<()> {
    let key = self.session_key(context);
    let inst = self.instance_mut(&key.instance)?;
    let ctx = inst.context_mut_checked(&key.context)?;
    if page_idx >= ctx.pages.len() {
      return Err(FerriError::Backend(format!(
        "Page index {page_idx} out of range (context '{context}' has {} pages)",
        ctx.pages.len()
      )));
    }
    ctx.active_page_idx = page_idx;
    Ok(())
  }

  /// Make `page` the active page of `context`.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or `page` is not one of
  /// its open pages.
  pub fn select_open_page(&mut self, context: &str, page: &AnyPage) -> Result<()> {
    let key = self.session_key(context);
    let inst = self.instance_mut(&key.instance)?;
    let ctx = inst.context_mut_checked(&key.context)?;
    ctx.prune_closed_pages();
    let index = ctx
      .pages
      .iter()
      .position(|listed| listed.same_backend_page(page))
      .ok_or_else(|| FerriError::invalid_argument("page", format!("not an open page of context '{context}'")))?;
    ctx.active_page_idx = index;
    Ok(())
  }

  /// The open pages of `context` in the order they were listed, which is
  /// the order [`Self::select_page`] indexes, and which of them is active.
  /// `None` for the index once nothing is open.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist.
  pub fn open_pages(&mut self, context: &str) -> Result<(Vec<AnyPage>, Option<usize>)> {
    let key = self.session_key(context);
    let context_pages = self.context_pages.clone();
    let inst = self.instance_mut(&key.instance)?;
    let ctx = inst.context_mut_checked(&key.context)?;
    ctx.prune_closed_pages();
    context_pages.set(&key.to_composite(), &ctx.pages);
    let active = (!ctx.pages.is_empty()).then(|| ctx.active_page_idx.min(ctx.pages.len() - 1));
    Ok((ctx.pages.clone(), active))
  }

  /// Close one page of a context: ask the browser to close the target,
  /// release the page's local resources, and drop it from the context.
  ///
  /// Dropping the handle alone is not enough — the tab stays open in
  /// the browser and its listener tasks stay parked on the transport,
  /// so a session that opened and "closed" pages accumulated both.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or the index is out of range.
  pub async fn close_page(&mut self, context: &str, page_idx: usize) -> Result<()> {
    let key = self.session_key(context);
    let context_pages = self.context_pages.clone();
    let inst = self.instance_mut(&key.instance)?;
    let ctx = inst.context_mut_checked(&key.context)?;
    if page_idx >= ctx.pages.len() {
      return Err(FerriError::Backend(format!("Page index {page_idx} out of range")));
    }
    let page = ctx.remove_page(page_idx);
    context_pages.set(&key.to_composite(), &ctx.pages);
    // A page whose target is already gone (crashed, closed from the
    // page itself) still needs its local teardown, so the close failure
    // is logged rather than propagated.
    if let Err(e) = page.close_page(crate::options::PageCloseOptions::default()).await {
      tracing::debug!(target: "ferridriver::state", error = %e, "close_page: backend close failed");
    }
    page.dispose_local();
    Ok(())
  }

  pub async fn list_contexts(&self) -> Vec<ContextInfo> {
    let snapshots: Vec<_> = self
      .instances
      .iter()
      .flat_map(|(inst_name, inst)| {
        inst.contexts.iter().map(move |(ctx_name, ctx)| {
          let name = if inst_name == "default" {
            ctx_name.clone()
          } else {
            format!("{inst_name}:{ctx_name}")
          };
          (
            name,
            inst_name.clone(),
            ctx_name.clone(),
            ctx.active_page_idx,
            ctx.pages.clone(),
          )
        })
      })
      .collect();

    let mut result = futures::future::join_all(snapshots.into_iter().map(
      |(name, instance, context, active_page_idx, pages)| async move {
        let pages = futures::future::join_all(pages.into_iter().enumerate().map(|(i, page)| async move {
          let (url, title) = tokio::join!(page.url(), page.title());
          PageInfo {
            index: i,
            url: url.ok().flatten().unwrap_or_default(),
            title: title.ok().flatten().unwrap_or_default(),
            active: i == active_page_idx,
          }
        }))
        .await;
        ContextInfo {
          name,
          instance,
          context,
          pages,
        }
      },
    ))
    .await;
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
  }

  /// Store a new ref map for the given context (atomic, no `&mut self` needed).
  pub fn set_ref_map(&self, context: &str, ref_map: HashMap<String, i64>) {
    let key = self.session_key(context);
    if let Some(inst) = self.instances.get(&*key.instance)
      && let Some(ctx) = inst.contexts.get(&*key.context)
    {
      ctx.ref_map.store(std::sync::Arc::new(ref_map));
    }
  }

  #[must_use]
  pub fn ref_map(&self, context: &str) -> HashMap<String, i64> {
    let key = self.session_key(context);
    self
      .instances
      .get(&*key.instance)
      .and_then(|inst| inst.contexts.get(&*key.context))
      .map(|c| (**c.ref_map.load()).clone())
      .unwrap_or_default()
  }

  /// Get an `Arc` handle to a context's ref map `ArcSwap` for lock-free access.
  #[must_use]
  pub fn ref_map_handle(&self, context: &str) -> Option<std::sync::Arc<arc_swap::ArcSwap<HashMap<String, i64>>>> {
    let key = self.session_key(context);
    self
      .instances
      .get(&*key.instance)
      .and_then(|inst| inst.contexts.get(&*key.context))
      .map(|c| std::sync::Arc::clone(&c.ref_map))
  }

  /// Get `Arc` handles to a context's log collections for lock-free access.
  #[must_use]
  pub fn log_handles(&self, context: &str) -> Option<ContextLogHandles> {
    let key = self.session_key(context);
    self
      .instances
      .get(&*key.instance)
      .and_then(|inst| inst.contexts.get(&*key.context))
      .map(|ctx| ContextLogHandles {
        console: std::sync::Arc::clone(&ctx.console_log),
        network: std::sync::Arc::clone(&ctx.network_log),
        dialog: std::sync::Arc::clone(&ctx.dialog_log),
      })
  }

  /// # Errors
  ///
  /// Returns an error if the instance or context does not exist.
  pub async fn console_messages(
    &self,
    context: &str,
    level: Option<&str>,
    limit: usize,
  ) -> Result<Vec<ConsoleMessage>> {
    let key = self.session_key(context);
    let inst = self.instance(&key.instance)?;
    let ctx = inst.context(&key.context)?;
    Ok(ctx.console_messages(level, limit).await)
  }

  /// # Errors
  ///
  /// Returns an error if the instance or context does not exist.
  pub async fn network_requests(&self, context: &str, limit: usize) -> Result<Vec<Request>> {
    let key = self.session_key(context);
    let inst = self.instance(&key.instance)?;
    let ctx = inst.context(&key.context)?;
    Ok(ctx.network_requests(limit).await)
  }

  /// # Errors
  ///
  /// Returns an error if the instance or context does not exist, or page discovery fails.
  pub async fn refresh_pages(&mut self, context: &str) -> Result<usize> {
    let key = self.session_key(context);
    let viewport = self.default_viewport.clone();
    let context_pages = self.context_pages.clone();
    let inst = self.instance_mut(&key.instance)?;
    let current_pages = Box::pin(inst.browser.pages()).await?;
    let owned_pages: rustc_hash::FxHashSet<_> = inst
      .contexts
      .values()
      .flat_map(|context| {
        context
          .pages
          .iter()
          .filter(|page| !page.is_closed())
          .map(AnyPage::page_guid)
      })
      .collect();
    let ctx = inst.context_mut_checked(&key.context)?;
    ctx.prune_closed_pages();

    let mut adopted = Vec::new();
    for page in current_pages {
      if owned_pages.contains(&page.page_guid()) {
        continue;
      }
      page.attach_listeners(
        Arc::clone(&ctx.console_log),
        Arc::clone(&ctx.network_log),
        Arc::clone(&ctx.dialog_log),
      );
      adopted.push(page.clone());
      ctx.pages.push(page);
    }
    let total = ctx.pages.len();
    context_pages.set(&key.to_composite(), &ctx.pages);

    // A tab that appeared in the browser after the instance was adopted
    // is as new to this context as the ones adopted at connect time, and
    // needs the same emulation — otherwise the one tab a user opened by
    // hand is the one that reports the browser's own window size.
    if let Some(ref vp) = viewport.filter(|_| inst.browser.kind() != BackendKind::WebDriver) {
      for page in &adopted {
        if let Err(e) = page.emulate_viewport(vp).await {
          tracing::warn!(
            target: "ferridriver::state",
            error = %e,
            "could not emulate the configured viewport on a newly adopted page",
          );
        }
      }
    }
    Ok(total)
  }

  /// # Errors
  ///
  /// Returns an error if the instance or context does not exist.
  pub async fn dialog_messages(&self, context: &str, limit: usize) -> Result<Vec<DialogEvent>> {
    let key = self.session_key(context);
    let inst = self.instance(&key.instance)?;
    let ctx = inst.context(&key.context)?;
    Ok(ctx.dialog_messages(limit).await)
  }

  /// # Errors
  /// Retains incomplete instance cleanup and reports its failure for a later retry.
  pub async fn shutdown(&mut self) -> Result<()> {
    self.connected.store(false, std::sync::atomic::Ordering::Relaxed);
    self.shutdown_generation = self.shutdown_generation.wrapping_add(1);
    self
      .launch_permits
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clear();
    self
      .pending_cleanup
      .extend(self.instances.drain().map(|(name, instance)| (name, instance.into())));
    self.popup_pumps.clear();
    self.cleanup_instances(None).await
  }

  pub(crate) async fn dispose_result(&mut self) -> Result<()> {
    self.disposed = true;
    self.shutdown().await
  }

  /// Close one browser instance, leaving the others running. Returns
  /// `false` if no instance by that name is live.
  ///
  /// The next session routed to the name launches a fresh browser, so
  /// this is also how an operator picks up changed per-instance chrome
  /// args without taking down every other instance in the server.
  ///
  /// # Errors
  /// Returns a provider cleanup error while retaining the owner for another close attempt.
  pub async fn close_instance(&mut self, instance: &str) -> Result<bool> {
    self
      .launch_permits
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(instance);
    let exists = self.instances.contains_key(instance) || self.pending_cleanup.iter().any(|(name, _)| name == instance);
    self.popup_pumps.remove(instance);
    let result = self.evict_instance(instance).await;
    if self.instances.is_empty() {
      self.connected.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    result.map(|()| exists)
  }

  #[must_use]
  pub fn is_connected(&self) -> bool {
    !self.instances.is_empty()
  }

  pub(super) fn next_instance_generation(&mut self) -> u64 {
    self.instance_generation_counter += 1;
    self.instance_generation_counter
  }

  /// Current generation of the named instance, or `None` if no instance
  /// by that name is live. A changed value (including `Some`→`None`→
  /// `Some`) means the browser session was swapped: any state a consumer
  /// cached against the old session is stale.
  #[must_use]
  pub fn instance_generation(&self, instance: &str) -> Option<u64> {
    self.instances.get(instance).map(|i| i.generation)
  }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextInfo {
  pub name: String,
  pub instance: String,
  pub context: String,
  pub pages: Vec<PageInfo>,
}

// Backward-compat alias for code that still references SessionInfo.
pub type SessionInfo = ContextInfo;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PageInfo {
  pub index: usize,
  pub url: String,
  pub title: String,
  pub active: bool,
}

pub(super) async fn discover_ws_from_http(http_url: &str) -> Result<String> {
  discover_ws_with_headers(http_url, None).await
}

pub(super) async fn discover_ws_with_headers(
  http_url: &str,
  headers: Option<&rustc_hash::FxHashMap<String, String>>,
) -> Result<String> {
  let budget = crate::operation_budget::OperationBudget::current(10_000)?;
  budget.wait(async {
    let mut url = reqwest::Url::parse(http_url).map_err(|error| FerriError::invalid_argument("url", error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
      return Err(FerriError::invalid_argument("url", "CDP discovery requires an HTTP or HTTPS endpoint"));
    }
    url.set_path(&format!("{}/json/version", url.path().trim_end_matches('/')));
    let response = crate::backend::webdriver::http_client(headers)?.get(url.clone()).send().await
      .map_err(|error| FerriError::backend(format!("CDP discovery request failed: {error}")))?;
    if !response.status().is_success() {
      return Err(FerriError::backend(format!(
        "CDP discovery returned HTTP {}. Pass the browser WebSocket URL directly or use the profile's DevToolsActivePort file.", response.status()
      )));
    }
    let peer = response.remote_addr();
    let body: serde_json::Value = response.json().await.map_err(|error| FerriError::backend(format!("Parse /json/version: {error}")))?;
    let endpoint = body["webSocketDebuggerUrl"].as_str().ok_or_else(|| FerriError::backend("No webSocketDebuggerUrl in /json/version"))?;
    let websocket = reqwest::Url::parse(endpoint).map_err(|error| FerriError::backend(format!("Invalid debugger endpoint: {error}")))?;
    // Local Chrome can advertise localhost despite listening on only one IP family.
    if websocket.scheme() == "ws" && websocket.host_str() == Some("localhost")
      && let Some(peer) = peer.filter(|address| address.ip().is_loopback())
      && (url.host_str() == Some("localhost") || url.host_str().is_some_and(|host| host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))) {
      return Ok(pin_ws_authority(endpoint, peer));
    }
    Ok(endpoint.to_owned())
  }).await.map_err(|error| error.error("discovering CDP endpoint"))?
}

/// Replace the host:port authority of a `ws://`/`wss://` URL with `addr`,
/// preserving the scheme and path. Returns the URL unchanged if it does not
/// look like a ws URL with an authority.
pub(super) fn pin_ws_authority(ws_url: &str, addr: std::net::SocketAddr) -> String {
  for scheme in ["ws://", "wss://"] {
    if let Some(rest) = ws_url.strip_prefix(scheme) {
      let path = rest.find('/').and_then(|i| rest.get(i..)).unwrap_or("");
      return format!("{scheme}{addr}{path}");
    }
  }
  ws_url.to_string()
}

/// Discover a running Chrome instance by reading its `DevToolsActivePort` file.
pub(super) fn discover_chrome_ws(channel: &str, explicit_user_data_dir: Option<&str>) -> Result<String> {
  let candidates: Vec<std::path::PathBuf> = if let Some(dir) = explicit_user_data_dir {
    vec![std::path::PathBuf::from(dir)]
  } else {
    chrome_default_user_data_dirs(channel)?
  };

  // A profile directory existing proves only that the browser is installed. Only
  // DevToolsActivePort proves one is running with debugging on, so keep walking
  // the candidates until a readable port file turns up.
  let mut last_err = None;
  for user_data_dir in &candidates {
    let port_file = user_data_dir.join("DevToolsActivePort");
    match std::fs::read_to_string(&port_file) {
      Ok(content) => return parse_devtools_active_port(&content),
      Err(e) => last_err = Some(format!("{}: {e}", port_file.display())),
    }
  }

  let browser = if channel == "chromium" { "Chromium" } else { "Chrome" };
  Err(FerriError::Backend(format!(
    "Cannot read DevToolsActivePort ({}). Ensure {browser} ({channel}) is running with remote debugging \
     enabled at chrome://inspect/#remote-debugging, or pass user_data_dir explicitly.",
    last_err.unwrap_or_else(|| "no candidate profile directories".to_string())
  )))
}

pub(super) fn parse_devtools_active_port(content: &str) -> Result<String> {
  let lines: Vec<&str> = content.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
  let (Some(port_line), Some(path)) = (lines.first(), lines.get(1)) else {
    return Err(FerriError::Backend(format!(
      "Invalid DevToolsActivePort content: {content:?}"
    )));
  };

  let port: u16 = port_line
    .parse()
    .map_err(|_| FerriError::Backend(format!("Invalid port '{port_line}' in DevToolsActivePort")))?;

  Ok(format!("ws://127.0.0.1:{port}{path}"))
}

/// Profile directories to search for a running browser, best match first.
///
/// Returns every directory that exists rather than the first, because an
/// installed-but-idle Chrome must not shadow a Chromium that is actually
/// running with debugging enabled.
pub(super) fn chrome_default_user_data_dirs(channel: &str) -> Result<Vec<std::path::PathBuf>> {
  let home = std::env::var("HOME")
    .or_else(|_| std::env::var("USERPROFILE"))
    .map_err(|_| FerriError::backend("Cannot determine home directory"))?;

  let os = std::env::consts::OS;
  let suffix = match channel {
    "stable" | "chrome" => "",
    "beta" => " Beta",
    "dev" => " Dev",
    "canary" => " Canary",
    "chromium" => {
      let path = match os {
        "linux" => std::path::PathBuf::from(&home).join(".config/chromium"),
        "macos" => std::path::PathBuf::from(&home).join("Library/Application Support/Chromium"),
        "windows" => {
          let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| format!("{home}/AppData/Local"));
          std::path::PathBuf::from(local_app_data).join("Chromium/User Data")
        },
        _ => return Err(FerriError::unsupported(format!("OS: {os}"))),
      };
      return Ok(vec![path]);
    },
    other => {
      return Err(FerriError::invalid_argument(
        "channel",
        format!("unknown Chrome channel: {other}"),
      ));
    },
  };

  let chrome_path = match os {
    "linux" => {
      let dir_name = if suffix.is_empty() {
        "google-chrome".to_string()
      } else {
        format!("google-chrome{}", suffix.to_lowercase().replace(' ', "-"))
      };
      std::path::PathBuf::from(&home).join(".config").join(dir_name)
    },
    "macos" => std::path::PathBuf::from(&home)
      .join("Library/Application Support")
      .join(format!("Google/Chrome{suffix}")),
    "windows" => {
      let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| format!("{home}/AppData/Local"));
      std::path::PathBuf::from(local_app_data).join(format!("Google/Chrome{suffix}/User Data"))
    },
    _ => {
      return Err(FerriError::unsupported(format!("OS: {os}")));
    },
  };

  let chromium_path = match os {
    "linux" => Some(std::path::PathBuf::from(&home).join(".config/chromium")),
    "macos" => Some(std::path::PathBuf::from(&home).join("Library/Application Support/Chromium")),
    "windows" => {
      let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| format!("{home}/AppData/Local"));
      Some(std::path::PathBuf::from(local_app_data).join("Chromium/User Data"))
    },
    _ => None,
  };

  let found: Vec<_> = std::iter::once(chrome_path.clone())
    .chain(chromium_path.clone())
    .filter(|p| p.exists())
    .collect();

  if found.is_empty() {
    return Err(FerriError::Backend(match chromium_path {
      Some(c) => format!(
        "Chrome user data dir not found at {} or {}",
        chrome_path.display(),
        c.display()
      ),
      None => format!("Chrome user data dir not found: {}", chrome_path.display()),
    }));
  }

  Ok(found)
}

/// Common Chrome/Chromium launch flags used by cdp-pipe and cdp-raw backends.
#[must_use]
/// Build Chrome flags matching Playwright's launch sequence exactly.
/// Order: chromiumSwitches → headless flags → sandbox → user args.
pub fn chrome_flags(headless: bool, extra_args: &[String]) -> Vec<String> {
  chrome_flags_with(headless, extra_args, None, false)
}

/// [`chrome_flags`] with Playwright's `ignoreDefaultArgs`.
///
/// Every built-in switch is a policy decision that can conflict with a
/// caller's environment (a proxy that needs background networking, an
/// extension that needs the sandbox). Playwright has always let a
/// caller drop them; ferridriver carried the option on `LaunchPlan` and
/// then applied it nowhere, so the flags were unconditional.
#[must_use]
pub fn chrome_flags_with(
  headless: bool,
  extra_args: &[String],
  ignore: Option<&crate::options::IgnoreDefaultArgs>,
  sandbox: bool,
) -> Vec<String> {
  use crate::options::IgnoreDefaultArgs;

  let drop_all = matches!(ignore, Some(IgnoreDefaultArgs::All));
  let dropped: &[String] = match ignore {
    Some(IgnoreDefaultArgs::Some(list)) => list,
    _ => &[],
  };
  // Compare on the switch NAME so `--foo=bar` is dropped by `--foo`.
  let is_dropped = |flag: &str| {
    let name = flag.split('=').next().unwrap_or(flag);
    dropped.iter().any(|d| {
      let d_name = d.split('=').next().unwrap_or(d);
      d_name == name
    })
  };

  let mut flags: Vec<String> = Vec::with_capacity(40 + extra_args.len());
  let loads_extension = extra_args.iter().any(|arg| arg.starts_with("--load-extension="));

  // 1. Base chromiumSwitches (from Playwright's chromiumSwitches.ts)
  if !drop_all {
    for f in CHROMIUM_SWITCHES {
      if loads_extension && *f == "--disable-extensions" {
        continue;
      }
      if !is_dropped(f) {
        flags.push((*f).into());
      }
    }
  }

  // 2. Always added after base switches
  if !drop_all && !is_dropped("--enable-unsafe-swiftshader") {
    flags.push("--enable-unsafe-swiftshader".into());
  }

  // 3. Headless flags (Playwright adds these when headless=true).
  // Playwright passes bare `--headless` too — Chrome maps to
  // `--headless=old` on full chrome. The 2x perf gap on Regular
  // Chrome lives elsewhere, not in this flag (verified via
  // playwright-core/lib/server/chromium/chromium.js:288).
  //
  // Filtered by `ignoreDefaultArgs` like every other section: Playwright
  // builds the headless switches inside `defaultArgs()`, so
  // `ignoreDefaultArgs: true` drops them there too. Gating only sections
  // 1/2/4 left the caller with a browser that was still forced headless
  // after asking for no defaults at all.
  if headless && !drop_all {
    for f in ["--headless", "--hide-scrollbars", "--mute-audio"] {
      if !is_dropped(f) {
        flags.push(f.into());
      }
    }
    // No colour scheme is pinned here: a page is emulated light unless
    // asked otherwise, and `colorScheme: null` hands it back to whatever
    // the browser does on this host, as it does in Playwright.
    if !is_dropped("--blink-settings") {
      flags.push(
        "--blink-settings=primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4".into(),
      );
    }
  }

  // 4. Sandbox control (Playwright disables by default unless chromiumSandbox=true)
  if !sandbox && !drop_all && !is_dropped("--no-sandbox") {
    flags.push("--no-sandbox".into());
  }

  // 5. User-provided args. Never filtered: `ignoreDefaultArgs` names
  // DEFAULTS to drop, so an explicitly-passed arg still wins.
  for arg in extra_args {
    flags.push(arg.clone());
  }

  merge_feature_switches(&mut flags);
  flags
}

/// Chromium reads only the last `--enable-features` and the last
/// `--disable-features`, so a caller's own switch would silently drop the
/// defaults (Playwright appends them the same way and loses them too). Each
/// kind becomes one switch where it first appeared, and a feature named
/// again later moves to that later list, so the caller wins feature by
/// feature over the defaults before it.
///
/// `WebMCP` also brings what an automation client needs from it on builds
/// that gate those separately: `WebMCPTesting` for `getTools()` and
/// `DevToolsWebMCPSupport` for the `DevTools` domain. Chrome 153 enables both
/// with `WebMCP` already; a caller who disables one keeps it disabled.
pub(crate) fn merge_feature_switches(flags: &mut Vec<String>) {
  const ENABLE: &str = "--enable-features=";
  const DISABLE: &str = "--disable-features=";
  // A feature may carry a field trial or parameters: `Name<Trial:key/value`.
  fn name(feature: &str) -> &str {
    let feature = feature.trim_start_matches('*');
    feature.split(['<', ':']).next().unwrap_or(feature)
  }
  let mut lists: [Vec<String>; 2] = [Vec::new(), Vec::new()];
  let mut slots: [Option<usize>; 2] = [None, None];
  let mut kept = Vec::with_capacity(flags.len());
  for flag in flags.drain(..) {
    let (kind, value) = if let Some(value) = flag.strip_prefix(ENABLE) {
      (0, value)
    } else if let Some(value) = flag.strip_prefix(DISABLE) {
      (1, value)
    } else {
      kept.push(flag);
      continue;
    };
    slots[kind].get_or_insert(kept.len());
    for feature in value.split(',').filter(|feature| !feature.is_empty()) {
      for list in &mut lists {
        list.retain(|named| name(named) != name(feature));
      }
      lists[kind].push(feature.to_owned());
    }
  }
  let [enabled, disabled] = &mut lists;
  if enabled.iter().any(|feature| name(feature) == "WebMCP") {
    for companion in ["WebMCPTesting", "DevToolsWebMCPSupport"] {
      if !enabled
        .iter()
        .chain(disabled.iter())
        .any(|feature| name(feature) == companion)
      {
        enabled.push(companion.to_owned());
      }
    }
  }
  let mut switches = Vec::new();
  for (slot, switch, list) in [(slots[0], ENABLE, enabled), (slots[1], DISABLE, disabled)] {
    if let Some(slot) = slot
      && !list.is_empty()
    {
      switches.push((slot, format!("{switch}{}", list.join(","))));
    }
  }
  // Later slots first, so inserting one does not shift the other.
  switches.sort_by_key(|(slot, _)| std::cmp::Reverse(*slot));
  for (slot, switch) in switches {
    kept.insert(slot, switch);
  }
  *flags = kept;
}

/// Chrome switches matching Playwright's `chromiumSwitches()` exactly.
/// See: playwright/packages/playwright-core/src/server/chromium/chromiumSwitches.ts
pub(crate) const CHROMIUM_SWITCHES: &[&str] = &[
  "--disable-field-trial-config",
  "--disable-background-networking",
  "--disable-background-timer-throttling",
  "--disable-backgrounding-occluded-windows",
  "--disable-back-forward-cache",
  "--disable-breakpad",
  "--disable-client-side-phishing-detection",
  "--disable-component-extensions-with-background-pages",
  "--disable-component-update",
  "--no-default-browser-check",
  "--disable-default-apps",
  "--disable-dev-shm-usage",
  "--disable-edgeupdater",
  "--disable-extensions",
  "--disable-features=AvoidUnnecessaryBeforeUnloadCheckSync,BlockInsecurePrivateNetworkRequests,BoundaryEventDispatchTracksNodeRemoval,DestroyProfileOnBrowserClose,DialMediaRouteProvider,GlobalMediaControls,HttpsUpgrades,LensOverlay,MediaRouter,PaintHolding,PrivateNetworkAccessSendPreflights,ThirdPartyStoragePartitioning,Translate,AutoDeElevate,RenderDocument,OptimizationHints,msForceBrowserSignIn,msEdgeUpdateLaunchServicesPreferredVersion",
  "--enable-features=CDPScreenshotNewSurface",
  "--allow-pre-commit-input",
  "--disable-hang-monitor",
  "--disable-ipc-flooding-protection",
  "--disable-popup-blocking",
  "--disable-prompt-on-repost",
  "--disable-renderer-backgrounding",
  "--force-color-profile=srgb",
  "--metrics-recording-only",
  "--no-first-run",
  "--password-store=basic",
  "--use-mock-keychain",
  "--no-service-autorun",
  "--export-tagged-pdf",
  "--disable-search-engine-choice-screen",
  "--unsafely-disable-devtools-self-xss-warnings",
  "--edge-skip-compat-layer-relaunch",
  "--enable-automation",
  "--disable-infobars",
  "--disable-sync",
];

/// Resolve the Chrome binary to use, respecting env vars and headless mode.
///
/// Follows Playwright's resolution strategy:
/// - `executablePath` (handled by caller before this function) always wins.
/// - Explicit env vars (`CHROMIUM_HEADLESS_SHELL_PATH`, `CHROMIUM_PATH`) override auto-detection.
/// - When headless and no explicit path is set, prefer Chrome Headless Shell (lighter, faster).
/// - Fall back to full Chrome/Chromium otherwise.
///
/// Precedence (headless=true):
/// 1. `CHROMIUM_HEADLESS_SHELL_PATH` env var
/// 2. `CHROMIUM_PATH` env var (explicit user override always wins over auto-detection)
/// 3. ferridriver's own cache: its headless shell, then its Chromium
/// 4. Playwright's headless shell
/// 5. Auto-detect regular Chrome (`detect_chromium()`)
///
/// A build in ferridriver's cache outranks any build in Playwright's, so a
/// Playwright install elsewhere on the machine never changes the browser.
///
/// Precedence (headless=false):
/// 1. `CHROMIUM_PATH` env var
/// 2. Auto-detect regular Chrome (`detect_chromium()`)
#[must_use]
pub fn resolve_chromium(headless: bool) -> String {
  if headless {
    // Explicit headless shell path
    if let Ok(p) = std::env::var("CHROMIUM_HEADLESS_SHELL_PATH")
      && std::path::Path::new(&p).exists()
    {
      return p;
    }

    // Explicit chrome path -- user chose a specific binary, respect it
    if let Ok(p) = std::env::var("CHROMIUM_PATH")
      && std::path::Path::new(&p).exists()
    {
      return p;
    }

    let installer = crate::install::BrowserInstaller::new();
    if let Some(p) = installer
      .find_installed_headless_shell()
      .or_else(|| installer.find_installed_chromium())
      .or_else(|| playwright_build(crate::install::CachedBrowser::ChromiumHeadlessShell))
    {
      return p;
    }
  }

  // Headed mode, or no headless shell found: use regular Chrome
  detect_chromium()
}

/// Auto-detect Chrome Headless Shell binary on the system.
///
/// ferridriver's own cache first, then Playwright's. Does NOT check env vars
/// (that's `resolve_chromium()`'s job).
#[must_use]
pub fn detect_chromium_headless_shell() -> Option<String> {
  crate::install::BrowserInstaller::new()
    .find_installed_headless_shell()
    .or_else(|| playwright_build(crate::install::CachedBrowser::ChromiumHeadlessShell))
}

pub(super) fn playwright_build(browser: crate::install::CachedBrowser) -> Option<String> {
  crate::install::find_playwright_browser(browser).map(|path| path.to_string_lossy().into_owned())
}

/// Detect Chrome/Chromium binary on the system.
///
/// Automation-capable builds win over the platform's installed browser, and
/// not merely because they are better tested: a Chrome enrolled in cloud
/// management can carry a `RemoteDebuggingAllowed=false` policy, under which
/// it starts normally, refuses to open the debugging port, and never writes
/// `DevToolsActivePort` — indistinguishable from a hang. Chrome for Testing
/// and Chromium builds are never enrolled. Both launchers here always pass
/// their own `--user-data-dir`, so preferring one costs no profile.
#[must_use]
pub fn detect_chromium() -> String {
  if let Ok(p) = std::env::var("CHROMIUM_PATH")
    && std::path::Path::new(&p).exists()
  {
    return p;
  }

  // ferridriver's own `install chromium` download.
  if let Some(p) = crate::install::BrowserInstaller::new().find_installed_chromium() {
    return p;
  }

  if let Some(p) = playwright_build(crate::install::CachedBrowser::Chromium) {
    return p;
  }

  // Chrome for Testing installed as a normal app, and plain Chromium — both
  // unenrolled, so they outrank the managed bundles further down.
  #[cfg(target_os = "macos")]
  {
    let unmanaged = [
      "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
      "Chromium.app/Contents/MacOS/Chromium",
    ];
    for bundle in &unmanaged {
      let sys = std::path::PathBuf::from("/Applications").join(bundle);
      if sys.exists() {
        return sys.to_string_lossy().to_string();
      }
      if let Ok(home) = std::env::var("HOME") {
        let user = std::path::PathBuf::from(&home).join("Applications").join(bundle);
        if user.exists() {
          return user.to_string_lossy().to_string();
        }
      }
    }
  }

  if let Ok(path_var) = std::env::var("PATH") {
    let names = [
      "chromium-browser",
      "chromium",
      "google-chrome-stable",
      "google-chrome",
      "microsoft-edge",
      "chrome",
    ];
    for name in &names {
      for dir in path_var.split(':') {
        let candidate = std::path::PathBuf::from(dir).join(name);
        if candidate.exists() {
          return candidate.to_string_lossy().to_string();
        }
      }
    }
  }

  #[cfg(target_os = "macos")]
  {
    // Chromium and Chrome for Testing were already tried above; what is left
    // is the platform's own browser, which may be enrolled.
    let bundles = [
      "Google Chrome.app/Contents/MacOS/Google Chrome",
      "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
      "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    ];
    for bundle in &bundles {
      let sys = std::path::PathBuf::from("/Applications").join(bundle);
      if sys.exists() {
        return sys.to_string_lossy().to_string();
      }
      if let Ok(home) = std::env::var("HOME") {
        let user = std::path::PathBuf::from(&home).join("Applications").join(bundle);
        if user.exists() {
          return user.to_string_lossy().to_string();
        }
      }
    }
  }

  #[cfg(target_os = "linux")]
  {
    let paths = [
      "/usr/bin/chromium-browser",
      "/usr/bin/chromium",
      "/snap/bin/chromium",
      "/usr/bin/google-chrome-stable",
      "/usr/bin/google-chrome",
      "/usr/bin/microsoft-edge",
    ];
    for path in &paths {
      if std::path::Path::new(path).exists() {
        return path.to_string();
      }
    }
  }

  "chromium".to_string()
}

/// The Ladybird browser executable: `LADYBIRD_PATH`, then ferridriver's
/// own install, then the system's.
///
/// # Errors
///
/// Returns a backend error naming the install command when no Ladybird
/// is found.
pub fn detect_ladybird() -> Result<String> {
  if let Ok(path) = std::env::var("LADYBIRD_PATH")
    && std::path::Path::new(&path).exists()
  {
    return Ok(path);
  }
  if let Some(path) = crate::install::BrowserInstaller::new().find_installed_ladybird() {
    return Ok(path);
  }
  let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
  let mut candidates: Vec<std::path::PathBuf> = Vec::new();
  if cfg!(target_os = "macos") {
    candidates.push("/Applications/Ladybird.app/Contents/MacOS/Ladybird".into());
    if let Some(home) = &home {
      candidates.push(home.join("Applications/Ladybird.app/Contents/MacOS/Ladybird"));
    }
  } else if cfg!(target_os = "linux") {
    candidates.push("/opt/ladybird/bin/Ladybird".into());
  }
  candidates
    .into_iter()
    .find(|path| path.is_file())
    .map(|path| path.to_string_lossy().into_owned())
    .ok_or_else(|| FerriError::backend("Ladybird not found. Run `ferridriver install ladybird` or set LADYBIRD_PATH"))
}

/// The Lightpanda executable: `LIGHTPANDA_PATH`, then ferridriver's own
/// install, then one on `PATH` (Homebrew and the AUR install it there).
///
/// # Errors
///
/// Returns a backend error naming the install command when no Lightpanda
/// is found.
pub fn detect_lightpanda() -> Result<String> {
  if let Ok(path) = std::env::var("LIGHTPANDA_PATH")
    && std::path::Path::new(&path).exists()
  {
    return Ok(path);
  }
  if let Some(path) = crate::install::BrowserInstaller::new().find_installed_lightpanda() {
    return Ok(path);
  }
  std::env::var_os("PATH")
    .into_iter()
    .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
    .map(|dir| dir.join("lightpanda"))
    .find(|path| path.is_file())
    .map(|path| path.to_string_lossy().into_owned())
    .ok_or_else(|| {
      FerriError::backend("Lightpanda not found. Run `ferridriver install lightpanda` or set LIGHTPANDA_PATH")
    })
}

/// Detect Firefox binary on the system.
///
/// Search order (matches Chrome detection pattern):
/// 1. `FIREFOX_PATH` environment variable
/// 2. ferridriver's own browser cache (installed via `install_firefox()`)
/// 3. Playwright's browser cache
/// 4. System-installed Firefox (platform-specific paths)
/// 5. `which firefox` fallback
///
/// # Errors
///
/// Returns an error if no Firefox binary can be found.
pub fn detect_firefox() -> Result<String> {
  // 1. Env var (highest priority)
  if let Ok(p) = std::env::var("FIREFOX_PATH")
    && std::path::Path::new(&p).exists()
  {
    return Ok(p);
  }

  // 2. ferridriver's own cache
  if let Some(p) = crate::install::BrowserInstaller::new().find_installed_firefox() {
    return Ok(p);
  }

  // 3. Playwright's Firefox cache
  if let Some(p) = playwright_build(crate::install::CachedBrowser::Firefox) {
    return Ok(p);
  }

  // 4. System-installed Firefox
  #[cfg(target_os = "macos")]
  {
    let paths = [
      "/Applications/Firefox.app/Contents/MacOS/firefox",
      "/Applications/Firefox Nightly.app/Contents/MacOS/firefox",
      "/Applications/Firefox Developer Edition.app/Contents/MacOS/firefox",
    ];
    for path in &paths {
      if std::path::Path::new(path).exists() {
        return Ok(path.to_string());
      }
    }
  }

  #[cfg(target_os = "linux")]
  {
    // Skip snap-wrapped Firefox builds: Ubuntu 24.04+ ships a snap as
    // /usr/bin/firefox (and the explicit /snap/bin/firefox). Snap's
    // confinement blocks the WebDriver BiDi remote-debugging port and the
    // shim never prints the WebSocket URL on stderr, so detection hangs
    // until the 15s discovery timeout. Treat snap wrappers as
    // "not installed" so callers fall back to ferridriver's own download.
    let paths = [
      "/usr/bin/firefox",
      "/usr/bin/firefox-esr",
      "/snap/bin/firefox",
      "/usr/lib/firefox/firefox",
      "/usr/lib64/firefox/firefox",
    ];
    for path in &paths {
      let p = std::path::Path::new(path);
      if !p.exists() {
        continue;
      }
      let resolved = std::fs::canonicalize(p).map_or_else(|_| path.to_string(), |c| c.to_string_lossy().to_string());
      if resolved.contains("/snap/") {
        continue;
      }
      return Ok(path.to_string());
    }
  }

  #[cfg(target_os = "windows")]
  {
    let paths = [
      r"C:\Program Files\Mozilla Firefox\firefox.exe",
      r"C:\Program Files (x86)\Mozilla Firefox\firefox.exe",
    ];
    for path in &paths {
      if std::path::Path::new(path).exists() {
        return Ok(path.to_string());
      }
    }
  }

  // 5. which/where fallback
  let cmd = if cfg!(windows) { "where" } else { "which" };
  if let Ok(output) = std::process::Command::new(cmd).arg("firefox").output()
    && output.status.success()
  {
    let p = String::from_utf8_lossy(&output.stdout)
      .lines()
      .next()
      .unwrap_or("")
      .trim()
      .to_string();
    if !p.is_empty() && std::path::Path::new(&p).exists() {
      return Ok(p);
    }
  }

  Err(FerriError::backend(
    "Firefox not found. Install with `ferridriver install firefox` or set FIREFOX_PATH.",
  ))
}
