//! `BrowserContext` -- isolated browser environment with pages, cookies, and logs.
//!
//! Mirrors Playwright's `BrowserContext` exactly:
//! - Owns pages (`Vec<AnyPage>`)
//! - Owns cookies (via any page in the context)
//! - Owns console/network/dialog logs
//! - Created by `Browser.new_context()`
//! - Pages are created by `context.new_page()`

use crate::backend::{AnyPage, CookieData};
use crate::error::Result;
use crate::network::Request;
use crate::page::Page;
use crate::state::SessionKey;
use arc_swap::ArcSwap;
use rustc_hash::FxHashMap as HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A dismissed dialog event (alert, confirm, prompt).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DialogEvent {
  pub dialog_type: String,
  pub message: String,
  pub action: String,
}

/// Isolated browser context. Directly holds pages, cookies, and event logs.
/// This IS the state -- not a wrapper around some other struct.
/// Stored in `BrowserState`'s context map.
pub struct BrowserContext {
  /// Pages in this context.
  pub pages: Vec<AnyPage>,
  /// Active page index.
  pub active_page_idx: usize,
  /// Element ref map for accessibility snapshots (wait-free reads via `ArcSwap`).
  pub ref_map: Arc<ArcSwap<HashMap<String, i64>>>,
  /// Console messages collected from page events.
  pub console_log: Arc<RwLock<Vec<crate::console_message::ConsoleMessage>>>,
  /// Network requests collected from page events. Live `Request`
  /// references — listeners may inspect the stored object's response /
  /// failure via the `Request` async accessors.
  pub network_log: Arc<RwLock<Vec<Request>>>,
  /// Dialog events.
  pub dialog_log: Arc<RwLock<Vec<DialogEvent>>>,
  /// Context name (unique identifier).
  name: String,
  /// CDP browser context ID (for `Target.disposeBrowserContext` on close).
  /// None for the default context.
  pub cdp_context_id: Option<String>,
}

impl BrowserContext {
  /// Create a new empty context.
  pub(crate) fn new(name: String) -> Self {
    Self {
      pages: Vec::new(),
      active_page_idx: 0,
      ref_map: Arc::new(ArcSwap::from_pointee(HashMap::default())),
      console_log: Arc::new(RwLock::new(Vec::new())),
      network_log: Arc::new(RwLock::new(Vec::new())),
      dialog_log: Arc::new(RwLock::new(Vec::new())),
      name,
      cdp_context_id: None,
    }
  }

  /// Context name.
  #[must_use]
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Get the active page in this context.
  #[must_use]
  pub fn active_page(&self) -> Option<&AnyPage> {
    self.pages.get(self.active_page_idx)
  }

  // -- Cookies (operate on active page) ------------------------------------

  /// Get all cookies in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if cookies cannot be retrieved from the active page.
  pub async fn cookies(&self) -> Result<Vec<CookieData>> {
    if let Some(page) = self.active_page() {
      page.get_cookies().await
    } else {
      Ok(Vec::new())
    }
  }

  /// Add cookies to this context.
  ///
  /// # Errors
  ///
  /// Returns an error if no page exists or if setting a cookie fails.
  pub async fn add_cookies(&self, cookies: Vec<CookieData>) -> Result<()> {
    let page = self.active_page().ok_or(crate::error::FerriError::NotConnected)?;
    for cookie in cookies {
      page.set_cookie(cookie).await?;
    }
    Ok(())
  }

  /// Clear all cookies in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if clearing cookies fails on the active page.
  pub async fn clear_cookies(&self) -> Result<()> {
    if let Some(page) = self.active_page() {
      page.clear_cookies().await?;
    }
    Ok(())
  }

  /// Delete specific cookies by name and optional domain.
  ///
  /// # Errors
  ///
  /// Returns an error if reading or re-setting cookies fails.
  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<()> {
    let cookies = self.cookies().await?;
    if let Some(page) = self.active_page() {
      page.clear_cookies().await?;
      for cookie in cookies {
        let name_matches = cookie.name == name;
        let domain_matches = domain.is_none_or(|d| cookie.domain == d);
        if !(name_matches && domain_matches) {
          page.set_cookie(cookie).await?;
        }
      }
    }
    Ok(())
  }

  // -- Console/network/dialog log access -----------------------------------

  /// Get console messages, optionally filtered by level.
  pub async fn console_messages(
    &self,
    level: Option<&str>,
    limit: usize,
  ) -> Vec<crate::console_message::ConsoleMessage> {
    let msgs = self.console_log.read().await;
    msgs
      .iter()
      .filter(|m| level.is_none_or(|l| l == "all" || m.type_str() == l))
      .rev()
      .take(limit)
      .cloned()
      .collect::<Vec<_>>()
      .into_iter()
      .rev()
      .collect()
  }

  /// Get network requests (most recent `limit` in chronological order).
  pub async fn network_requests(&self, limit: usize) -> Vec<Request> {
    let reqs = self.network_log.read().await;
    reqs
      .iter()
      .rev()
      .take(limit)
      .cloned()
      .collect::<Vec<_>>()
      .into_iter()
      .rev()
      .collect()
  }

  /// Get dialog events.
  pub async fn dialog_messages(&self, limit: usize) -> Vec<DialogEvent> {
    let msgs = self.dialog_log.read().await;
    let start = msgs.len().saturating_sub(limit);
    msgs[start..].to_vec()
  }
}

// -- ContextRef: handle for the high-level Browser API -----------------------

use crate::state::BrowserState;

/// Handle to a browser context. Created by `Browser::new_context()` / `default_context()`.
/// Provides the Playwright-compatible context API by delegating to `BrowserState`.
#[derive(Clone)]
pub struct ContextRef {
  pub(crate) state: Arc<RwLock<BrowserState>>,
  pub(crate) name: Arc<str>,
  /// Pre-parsed session key (avoids re-parsing on every operation).
  pub(crate) key: SessionKey,
  /// Default timeout for actions in this context (ms). 0 = no override.
  /// `Arc<AtomicU64>` so the Playwright `setDefaultTimeout` setter can
  /// mutate through a shared `&self` handle (the `QuickJS` binding holds
  /// the context behind an `Arc` and cannot offer `&mut self`).
  default_timeout_ms: Arc<std::sync::atomic::AtomicU64>,
  /// Default navigation timeout in this context (ms). 0 = no override.
  /// Shared via `Arc<AtomicU64>` for the same reason as
  /// [`Self::default_timeout_ms`].
  default_navigation_timeout_ms: Arc<std::sync::atomic::AtomicU64>,
  /// Context-scoped event emitter. Shared across every `ContextRef`
  /// clone with the same composite session key via the per-state
  /// [`BrowserState::get_or_create_context_events`] registry — without
  /// this, `browser.defaultContext()` called twice would hand out two
  /// separate emitters and `context.on('weberror', cb)` would silently
  /// miss events dispatched via the per-page bridge installed on a
  /// page created through a different `ContextRef` instance.
  events: crate::events::ContextEventEmitter,
  /// Parent browser handle. `Some` when the context was created from a
  /// [`crate::Browser`] (`browser.newContext()` / `browser.defaultContext()`);
  /// surfaced by [`Self::browser`] to mirror Playwright's
  /// `browserContext.browser(): Browser | null`. `Browser` is a cheap
  /// `Arc`-handle clone, so this carries no protocol cost.
  browser: Option<crate::Browser>,
  /// Shared closed-flag for this context's composite key (see
  /// [`BrowserState::context_closed`]). `false` from handle creation
  /// until [`Self::close`] flips it `true`; backs [`Self::is_closed`].
  closed: Arc<std::sync::atomic::AtomicBool>,
}

impl ContextRef {
  pub fn new(state: Arc<RwLock<BrowserState>>, name: String) -> Self {
    let key = SessionKey::parse(&name);
    // Look up (or initialise) the shared emitter for this composite
    // key. Uses `try_read` because `ContextRef::new` must be callable
    // from sync contexts (e.g. `Browser::default_context`). In the
    // common case the state lock is uncontended at construction time;
    // if `try_read` fails (concurrent writer) we fall back to a
    // transient per-instance emitter so the handle is still usable —
    // event delivery would then be scoped to this `ContextRef` clone
    // only, matching the old behaviour. `get_or_create_context_events`
    // itself uses a `std::sync::Mutex` so it doesn't need the tokio
    // read guard to stay alive beyond the call.
    let (events, closed) = match state.try_read() {
      Ok(s) => (
        s.get_or_create_context_events(&key.to_composite()),
        s.get_or_create_context_closed(&key.to_composite()),
      ),
      Err(_) => (
        crate::events::ContextEventEmitter::new(),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
      ),
    };
    Self {
      state,
      name: Arc::from(name),
      key,
      default_timeout_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
      default_navigation_timeout_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
      events,
      browser: None,
      closed,
    }
  }

  /// Attach the parent [`crate::Browser`] handle so [`Self::browser`]
  /// returns it. Called by [`crate::Browser::new_context`] and
  /// [`crate::Browser::default_context`] right after construction.
  #[must_use]
  pub(crate) fn with_browser(mut self, browser: crate::Browser) -> Self {
    self.browser = Some(browser);
    self
  }

  /// Parent browser handle, or `None` for a context not created from a
  /// [`crate::Browser`]. Mirrors Playwright's
  /// `browserContext.browser(): Browser | null`
  /// (`/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:290`).
  #[must_use]
  pub fn browser(&self) -> Option<&crate::Browser> {
    self.browser.as_ref()
  }

  /// Whether this context has been closed. Mirrors Playwright's
  /// `browserContext.isClosed(): boolean`
  /// (`/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:298`).
  /// `false` from handle creation until [`Self::close`] is called (or the
  /// underlying browser instance is shut down / disconnected), matching
  /// Playwright's `_closingStatus !== 'none'`. Uses the shared
  /// [`BrowserState::context_closed`] flag so a `close()` on one handle is
  /// seen by every clone with the same composite key.
  #[must_use]
  pub fn is_closed(&self) -> bool {
    if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
      return true;
    }
    // A disconnected browser implicitly closes every context.
    self.browser.as_ref().is_some_and(|b| !b.is_connected())
  }

  /// Context-scoped event emitter. Cheap to clone.
  #[must_use]
  pub fn events(&self) -> &crate::events::ContextEventEmitter {
    &self.events
  }

  /// Context name.
  #[must_use]
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Create a new page in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if page creation fails.
  pub async fn new_page(&self) -> Result<Arc<Page>> {
    {
      let mut state = self.state.write().await;
      Box::pin(state.ensure_instance(&self.key.instance)).await?;
    }

    // Read the (optional) `BrowserContextOptions` bag before the open —
    // a custom `viewport` (or `null` viewport) has to be known up-front
    // so the backend opens the page at the right size rather than
    // resizing afterwards.
    let ctx_opts = {
      let state = self.state.read().await;
      state.get_context_options(&self.key.to_composite())
    };
    let resolved_viewport = match &ctx_opts {
      Some(opts) => match opts.viewport {
        crate::options::ViewportOption::Null => None,
        crate::options::ViewportOption::Default | crate::options::ViewportOption::Size { .. } => {
          opts.resolved_viewport()
        },
      },
      None => None,
    };

    let plan = {
      let state = self.state.read().await;
      state.page_open_plan(&self.key)?
    };
    // Override the state's default viewport with the options bag's
    // resolved viewport when the caller supplied one. `ViewportOption::Null`
    // drops the default entirely (matches Playwright's `viewport: null`
    // opt-out).
    let effective_viewport = if ctx_opts
      .as_ref()
      .is_some_and(|o| o.viewport != crate::options::ViewportOption::Default)
    {
      resolved_viewport
    } else {
      plan.viewport.clone()
    };

    let (any_page, browser_context_id) = if &*self.key.context == "default" {
      (
        Box::pin(plan.browser.new_page(
          "about:blank",
          plan.browser_context_id.as_deref(),
          effective_viewport.as_ref(),
        ))
        .await?,
        None,
      )
    } else if let Some(existing_ctx_id) = plan.browser_context_id.clone() {
      (
        Box::pin(
          plan
            .browser
            .new_page("about:blank", Some(&existing_ctx_id), effective_viewport.as_ref()),
        )
        .await?,
        Some(existing_ctx_id),
      )
    } else {
      // Per-context options flow through:
      //   * CDP: `Target.createBrowserContext({ proxyServer, proxyBypassList })`
      //     (`crBrowser.ts::doCreateNewContext`)
      //   * BiDi: `browser.createUserContext({ proxy })`
      //     (`bidiBrowser.ts::doCreateNewContext`)
      //   * webkit: `Playwright.createContext` + `Playwright.setLanguages`
      //     (`wkBrowser.ts::WKBrowserContext.initialize`), then per-page
      //     overrides applied in `attach()` before the about:blank document
      //     becomes scriptable.
      let ctx_id = plan.browser.new_context(ctx_opts.as_ref()).await?;
      let page = Box::pin(
        plan
          .browser
          .new_page("about:blank", Some(&ctx_id), effective_viewport.as_ref()),
      )
      .await?;
      (page, Some(ctx_id))
    };

    {
      let mut state = self.state.write().await;
      state.register_opened_page(&self.key, any_page.clone(), browser_context_id)?;
    }

    // `Page::with_context` spawns the FrameAttached/Detached/Navigated
    // listener so sync accessors (`main_frame`, `frames`, `parent_frame`,
    // `child_frames`, `is_detached`, `name`, `url`) see live state via
    // the listener. Sync after the eager `Page.getFrameTree` RTT was
    // dropped (`PERF_AUDIT` §M.4).
    let page = Page::with_context(any_page, self.clone());

    // Apply the BrowserContextOptions bag to the fresh page. Fields
    // without a backend implementation are silently skipped;
    // backend-specific failures funnel up as `FerriError` and fail
    // `new_page`, matching Playwright's "option applies or
    // context.newPage rejects" contract.
    if let Some(opts) = ctx_opts.as_ref() {
      apply_context_options(&page, opts).await?;
      // Hydrate `storageState` once per context — cookies + localStorage
      // applied to the first page; subsequent pages in the same
      // context inherit. Mirrors Playwright's
      // `/tmp/playwright/packages/playwright-core/src/server/browserContext.ts::setStorageState`.
      if let Some(ref storage) = opts.storage_state {
        let should_hydrate = {
          let state = self.state.read().await;
          state.claim_storage_state_hydration(&self.key.to_composite())
        };
        if should_hydrate {
          let state_value = match storage {
            crate::options::StorageStateInput::Inline(v) => v.clone(),
            crate::options::StorageStateInput::Path(p) => {
              let text = std::fs::read_to_string(p)
                .map_err(|e| crate::error::FerriError::Backend(format!("storageState: read {}: {e}", p.display())))?;
              serde_json::from_str(&text).map_err(|e| {
                crate::error::FerriError::Backend(format!("storageState: parse JSON from {}: {e}", p.display()))
              })?
            },
          };
          page.set_storage_state(&state_value).await?;
        }
      }
    }

    // If the context was configured with `recordVideo`, spawn the
    // recording runtime now that we have the strong `Arc<Page>`.
    // `start_video_recording` attaches a `Video` handle on the Page
    // that resolves when the encoder finishes; the recording runs in
    // the background via `tokio::spawn` and is stopped when the page
    // closes.
    let record_opts = {
      let state = self.state.read().await;
      state.get_record_video(&self.key.to_composite())
    };
    if let Some(opts) = record_opts {
      start_video_recording(&page, &opts);
    }

    // Re-apply every context-level binding (exposeBinding /
    // exposeFunction) to the fresh page so it sees `window[name]`
    // just like pages opened before the binding was registered.
    self.apply_context_bindings(page.inner()).await?;

    // Re-apply every context-level WebSocket route so sockets created
    // in the fresh page are intercepted just like on pages that were
    // open when `context.routeWebSocket` was called.
    self.apply_context_ws_routes(&page).await?;

    // Re-apply every context-level route (`context.route` /
    // `context.routeFromHAR`) so requests from the fresh page are
    // intercepted and consume the shared `times` budget.
    self.apply_context_routes(&page).await?;

    // Re-apply every context-level init script (`context.addInitScript`
    // and the fake-clock engine + call log) so new documents in the
    // fresh page replay them like pages that were already open.
    self.apply_context_init_scripts(&page).await?;

    // A trace with screenshots films every page of the context —
    // including ones opened mid-recording.
    if let Some(recorder) = crate::trace::recorder_for(&self.key.to_composite()) {
      if recorder.screenshots {
        crate::trace::spawn_screencast_pump(&recorder, page.inner()).await;
      }
    }

    Ok(page)
  }

  /// Attach a raw CDP session to `page`'s target. Playwright:
  /// `browserContext.newCDPSession(page)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:488`.
  /// Chromium-only.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Unsupported`] on WebKit/BiDi, or
  /// the protocol error if the attach fails.
  pub async fn new_cdp_session(&self, page: &Page) -> Result<crate::cdp_session::CdpSession> {
    page.inner().new_cdp_session().await
  }

  /// Install every registered context-level WebSocket route onto a
  /// fresh page (context scope — page-level routes keep precedence).
  async fn apply_context_ws_routes(&self, page: &Arc<Page>) -> Result<()> {
    let routes = {
      let registry = self.state.read().await.context_ws_routes_handle();
      let guard = registry.read().await;
      guard.get(&self.key.to_composite()).cloned().unwrap_or_default()
    };
    for (matcher, handler) in routes {
      page
        .route_web_socket_scoped(matcher, handler, crate::web_socket_route::WsRouteScope::Context)
        .await?;
    }
    Ok(())
  }

  /// Inject every registered context-level binding onto `page`.
  /// Called from `new_page` so a freshly-opened page sees the same
  /// `window[name]` proxies as pages opened before the binding existed.
  async fn apply_context_bindings(&self, page: &AnyPage) -> Result<()> {
    let composite = self.key.to_composite();
    let bindings = {
      let bindings_handle = self.state.read().await.context_bindings_handle();
      let guard = bindings_handle.read().await;
      guard
        .get(&composite)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<Vec<_>>())
        .unwrap_or_default()
    };
    for (name, binding) in bindings {
      let binding_for_page = bind_source(binding, composite.clone());
      page.expose_binding(&name, binding_for_page).await?;
    }
    Ok(())
  }

  /// Get all pages in this context as Page handles.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist.
  pub async fn pages(&self) -> Result<Vec<Arc<Page>>> {
    let inner_pages = {
      let state = self.state.read().await;
      let ctx = state.context(&self.name)?;
      ctx.pages.clone()
    };
    let mut pages = Vec::with_capacity(inner_pages.len());
    for inner in inner_pages {
      pages.push(Page::with_context(inner, self.clone()));
    }
    Ok(pages)
  }

  /// `context.tracing` handle. Playwright: `browserContext.tracing`.
  #[must_use]
  pub fn tracing(&self) -> crate::tracing::Tracing {
    crate::tracing::Tracing::new(self.clone())
  }

  /// `context.clock` handle. Playwright: `browserContext.clock`
  /// (`page.clock` is the same object).
  #[must_use]
  pub fn clock(&self) -> crate::clock::Clock {
    crate::clock::Clock::new(self.clone())
  }

  /// Composite session key (`instance:context`) identifying this context.
  #[must_use]
  pub fn composite(&self) -> String {
    self.key.to_composite()
  }

  /// Shared per-state HAR-recorder registry (used by [`crate::tracing::Tracing`]).
  pub(crate) async fn har_recorders(
    &self,
  ) -> Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::tracing::HarRecorder>>> {
    self.state.read().await.har_recorders.clone()
  }

  /// Handle to this context's accumulated network-request log, if the
  /// context exists. Used by HAR recording to slice the requests seen
  /// during a `startHar`/`stopHar` window.
  pub(crate) async fn network_log_handle(&self) -> Option<Arc<RwLock<Vec<Request>>>> {
    self
      .state
      .read()
      .await
      .context(&self.name)
      .ok()
      .map(|c| c.network_log.clone())
  }

  /// Get all cookies in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or cookie retrieval fails.
  pub async fn cookies(&self) -> Result<Vec<CookieData>> {
    let page = {
      let state = self.state.read().await;
      state.context(&self.name)?.active_page().cloned()
    };
    if let Some(page) = page {
      page.get_cookies().await
    } else {
      Ok(Vec::new())
    }
  }

  /// Export the current storage state of this context — cookies plus a
  /// per-origin `localStorage` snapshot.
  ///
  /// Playwright: `storageState(options?: { path?: string, indexedDB?: boolean })
  ///   : Promise<{ cookies, origins }>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:460`;
  /// server collection at `.../server/browserContext.ts:609`).
  ///
  /// Cookies are read via the existing [`Self::cookies`] surface. For each
  /// live page in the context we evaluate `Object.entries(localStorage)` to
  /// snapshot its origin's storage, grouping by `location.origin` and skipping
  /// origins with no entries (mirrors Playwright's `if (storage.localStorage
  /// .length)` filter). `opts.path`, when set, writes the JSON-serialized
  /// state (pretty-printed) to disk; `opts.indexed_db` is accepted for
  /// signature parity but does not yet collect `IndexedDB` databases.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist, cookie retrieval fails,
  /// or (when `path` is set) the file cannot be written.
  pub fn storage_state(
    &self,
  ) -> crate::action::Action<'static, crate::options::StorageStateOptions, crate::options::StorageState> {
    let this = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { this.storage_state_impl(Some(opts)).await }))
  }

  pub(crate) async fn storage_state_impl(
    &self,
    opts: Option<crate::options::StorageStateOptions>,
  ) -> Result<crate::options::StorageState> {
    // Page-side wrapper JSON-stringifies the result so the backend ships flat
    // strings rather than re-serializing via its own RemoteValue path (see
    // CLAUDE.md "utility-script JSON.stringify wrapper trick").
    const COLLECT_JS: &str = r"JSON.stringify({
      origin: location.origin,
      localStorage: Object.entries(localStorage).map(([name, value]) => ({ name, value }))
    })";

    let cookies = self.cookies().await?;

    let pages = self.pages().await?;
    let mut origins: Vec<crate::options::OriginState> = Vec::new();
    let mut seen: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();

    for page in &pages {
      // `Ok(None)` (no value) and `Err` (opaque origin / mid-navigation, where
      // localStorage access throws) are both skipped, matching Playwright's
      // per-page try/catch.
      let Ok(Some(raw)) = page.inner.evaluate(COLLECT_JS).await else {
        continue;
      };
      let parsed: Option<crate::options::OriginState> = raw
        .as_str()
        .and_then(|s| serde_json::from_str::<crate::options::OriginState>(s).ok());
      let Some(state) = parsed else { continue };
      if state.origin.is_empty() || state.origin == "null" || state.local_storage.is_empty() {
        continue;
      }
      if seen.insert(state.origin.clone()) {
        origins.push(state);
      }
    }

    let state = crate::options::StorageState { cookies, origins };

    if let Some(opts) = opts {
      if let Some(path) = opts.path {
        let json = serde_json::to_string_pretty(&state)
          .map_err(|e| crate::error::FerriError::Backend(format!("storageState: serialize JSON: {e}")))?;
        if let Some(parent) = path.parent() {
          if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
              crate::error::FerriError::Backend(format!("storageState: mkdir {}: {e}", parent.display()))
            })?;
          }
        }
        tokio::fs::write(&path, json)
          .await
          .map_err(|e| crate::error::FerriError::Backend(format!("storageState: write {}: {e}", path.display())))?;
      }
    }

    Ok(state)
  }

  /// Replace this context's storage state with `state`, clearing existing
  /// cookies and localStorage first. Mirrors Playwright 1.59's
  /// `browserContext.setStorageState(state)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts`.
  ///
  /// `state` accepts a JSON value in Playwright's storage-state shape
  /// (`{ cookies, origins }`). The cookies are cleared and re-seeded; each
  /// origin's localStorage is cleared on any live page for that origin before
  /// the new items are applied via [`Page::set_storage_state`].
  ///
  /// # Errors
  ///
  /// Returns an error if the context has no page to act through, or if
  /// clearing / applying cookies or localStorage fails.
  pub async fn set_storage_state(&self, state: &serde_json::Value) -> Result<()> {
    // Clear current cookies for the whole context.
    self.clear_cookies().await?;

    // Clear localStorage on every live page (each in its own origin scope).
    let pages = self.pages().await?;
    for page in &pages {
      let _ = page.inner.evaluate("localStorage.clear()").await;
    }

    // Apply the new state through a page (Playwright applies cookies +
    // per-origin localStorage). Reuse an existing page or open one.
    let page = match pages.into_iter().next() {
      Some(p) => p,
      None => Box::pin(self.new_page()).await?,
    };
    page.set_storage_state(state).await
  }

  /// Add cookies to this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or setting cookies fails.
  pub async fn add_cookies(&self, cookies: Vec<CookieData>) -> Result<()> {
    let page = {
      let state = self.state.read().await;
      state.context(&self.name)?.active_page().cloned()
    }
    .ok_or(crate::error::FerriError::NotConnected)?;

    for cookie in cookies {
      page.set_cookie(cookie).await?;
    }
    Ok(())
  }

  /// Clear all cookies in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or clearing cookies fails.
  pub async fn clear_cookies(&self) -> Result<()> {
    let page = {
      let state = self.state.read().await;
      state.context(&self.name)?.active_page().cloned()
    };
    if let Some(page) = page {
      page.clear_cookies().await?;
    }
    Ok(())
  }

  /// Clear cookies matching the given filters (matches Playwright's `context.clearCookies(options?)`).
  /// If no filters are specified, all cookies are cleared.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or clearing cookies fails.
  pub async fn clear_cookies_filtered(&self, options: &crate::backend::ClearCookieOptions) -> Result<()> {
    if options.name.is_none() && options.domain.is_none() && options.path.is_none() {
      return self.clear_cookies().await;
    }
    let page = {
      let state = self.state.read().await;
      state.context(&self.name)?.active_page().cloned()
    };
    if let Some(page) = page {
      let cookies = page.get_cookies().await?;
      page.clear_cookies().await?;
      for c in cookies {
        let name_match = options.name.as_ref().is_none_or(|n| &c.name == n);
        let domain_match = options.domain.as_ref().is_none_or(|d| &c.domain == d);
        let path_match = options.path.as_ref().is_none_or(|p| &c.path == p);
        if !(name_match && domain_match && path_match) {
          page.set_cookie(c).await?;
        }
      }
    }
    Ok(())
  }

  /// Delete a specific cookie by name and optional domain
  /// (matches Playwright's `context.clearCookies({ name })`).
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or deleting fails.
  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<()> {
    let state = self.state.read().await;
    let ctx = state.context(&self.name)?;
    ctx.delete_cookie(name, domain).await
  }

  /// Set the default timeout for actions in this context (ms). Mirrors
  /// Playwright's `browserContext.setDefaultTimeout(timeout)`. Takes
  /// `&self` (interior mutability via `Arc<AtomicU64>`) so it works
  /// behind a shared handle.
  pub fn set_default_timeout(&self, ms: u64) {
    self.default_timeout_ms.store(ms, std::sync::atomic::Ordering::Relaxed);
  }

  /// Set the default navigation timeout for this context (ms). Mirrors
  /// Playwright's `browserContext.setDefaultNavigationTimeout(timeout)`.
  pub fn set_default_navigation_timeout(&self, ms: u64) {
    self
      .default_navigation_timeout_ms
      .store(ms, std::sync::atomic::Ordering::Relaxed);
  }

  /// Current default action timeout (ms). 0 = no override.
  #[must_use]
  pub fn default_timeout(&self) -> u64 {
    self.default_timeout_ms.load(std::sync::atomic::Ordering::Relaxed)
  }

  /// Current default navigation timeout (ms). 0 = no override.
  #[must_use]
  pub fn default_navigation_timeout(&self) -> u64 {
    self
      .default_navigation_timeout_ms
      .load(std::sync::atomic::Ordering::Relaxed)
  }

  /// Mutate the stored [`crate::options::BrowserContextOptions`] bag
  /// via `f`, then re-apply the bag to every already-open page in
  /// this context. The single idiomatic entry point behind every
  /// Playwright public context setter (`setGeolocation`,
  /// `setOffline`, `setExtraHTTPHeaders`, `grantPermissions`, etc.)
  /// and the backbone for future per-field mutators.
  ///
  /// Future pages opened in this context see the updated bag because
  /// `ContextRef::new_page` reads from the same registry.
  ///
  /// # Errors
  ///
  /// Returns an error when `page.apply_context_options` rejects on
  /// any open page (aggregated per-field).
  async fn mutate_options<F>(&self, f: F) -> Result<()>
  where
    F: FnOnce(&mut crate::options::BrowserContextOptions),
  {
    let composite = self.key.to_composite();
    let updated = {
      let state = self.state.read().await;
      let mut opts = state.get_context_options(&composite).unwrap_or_default();
      f(&mut opts);
      state.set_context_options(&composite, opts.clone());
      opts
    };
    let pages = Box::pin(self.pages()).await?;
    for page in pages {
      Box::pin(page.apply_context_options(&updated)).await?;
    }
    Ok(())
  }

  /// Grant permissions in this context. Stores the list on the
  /// options bag and re-applies to every open page — matches
  /// Playwright's `browserContext.grantPermissions` semantics where
  /// the grant persists for future pages too.
  ///
  /// The `origin` parameter is currently ignored at the backend
  /// level (CDP `Browser.grantPermissions` accepts an `origin` but
  /// we don't thread it through the options bag yet — the
  /// bag-stored list grants for every origin).
  ///
  /// # Errors
  ///
  /// Returns an error if the re-application fails on any page.
  pub async fn grant_permissions(&self, permissions: &[String], _origin: Option<&str>) -> Result<()> {
    let perms = permissions.to_vec();
    self.mutate_options(|o| o.permissions = Some(perms)).await
  }

  /// Clear all granted permissions.
  ///
  /// # Errors
  ///
  /// Returns an error if resetting permissions fails.
  pub async fn clear_permissions(&self) -> Result<()> {
    // Reset via the backend's `Browser.resetPermissions` on every
    // page, then drop the list from the options bag.
    let pages = self.pages().await?;
    for page in &pages {
      page.inner().reset_permissions().await?;
    }
    self.mutate_options(|o| o.permissions = None).await
  }

  /// Close this context (remove from `BrowserState`). Mirrors
  /// Playwright's `context.close({ reason })` — chain `.reason(...)` to
  /// record why; the reason surfaces in errors from operations
  /// interrupted by the close.
  ///
  /// # Errors
  ///
  /// Returns an error if state lock acquisition fails.
  pub fn close(&self) -> crate::action::Action<'static, crate::options::ContextCloseOptions, ()> {
    let ctx = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { ctx.close_impl(Some(opts)).await }))
  }

  pub(crate) async fn close_impl(&self, opts: Option<crate::options::ContextCloseOptions>) -> Result<()> {
    self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
    // Flush `routeFromHAR(update: true)` recorders while the network log
    // is still reachable — Playwright writes the updated HAR on context
    // close (`browserContext.ts` exports every live HAR recorder).
    self.flush_har_updates().await?;
    let mut state = self.state.write().await;
    if let Some(reason) = opts.and_then(|o| o.reason) {
      state.set_close_reason(reason);
    }
    let persistent = state.persistent_context;
    state.remove_context(&self.name).await;
    if persistent {
      // Persistent-context launch contract: closing the context closes
      // the underlying browser too. Playwright:
      // `/tmp/playwright/packages/playwright-core/types/types.d.ts:15199`.
      state.shutdown().await;
    }
    Ok(())
  }

  /// Write every `routeFromHAR(update: true)` recording registered on
  /// this context. Consumes the registry entries so a double close does
  /// not rewrite the files.
  async fn flush_har_updates(&self) -> Result<()> {
    let recorders = {
      let registry = self.state.read().await.context_har_updates.clone();
      let mut guard = registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      guard.remove(&self.key.to_composite()).unwrap_or_default()
    };
    if recorders.is_empty() {
      return Ok(());
    }
    let requests: Vec<crate::network::Request> = match self.network_log_handle().await {
      Some(log) => log.read().await.clone(),
      None => Vec::new(),
    };
    for recorder in recorders {
      let slice = requests.get(recorder.start_len..).unwrap_or(&[]);
      crate::tracing::flush_recorder(&recorder, slice).await?;
    }
    Ok(())
  }

  /// Access the internal state (for MCP server integration).
  #[must_use]
  pub fn state(&self) -> &Arc<RwLock<BrowserState>> {
    &self.state
  }

  /// Enable `recordVideo` for pages opened in this context AFTER
  /// the call. Pages already open do not retroactively start
  /// recording — matches Playwright's context-creation-time binding
  /// semantics (`browser.newContext({ recordVideo: { dir, size? } })`).
  ///
  /// Transitional shim — prefer passing `recordVideo` to
  /// `browser.newContext(options)` directly.
  ///
  /// # Errors
  ///
  /// Returns an error if the state write fails. Does NOT re-apply
  /// to already-open pages (the recording runtime attaches at page
  /// open via `start_video_recording`).
  pub async fn set_record_video(&self, opts: crate::options::RecordVideoOptions) -> Result<()> {
    let composite = self.key.to_composite();
    let state = self.state.read().await;
    state.set_record_video(&composite, opts.clone());
    // Also fold into the options bag so future `browser.newContext`
    // re-reads see it, and so a later `context.setOffline`-style
    // mutator doesn't clobber the record_video field.
    let mut bag = state.get_context_options(&composite).unwrap_or_default();
    bag.record_video = Some(opts);
    state.set_context_options(&composite, bag);
    Ok(())
  }

  // ── Context-level events ────────────────────────────────────────────────

  /// Register a context-level event listener. Supported events:
  /// `'weberror'` (unhandled errors / rejections on any page in this
  /// context — mirrors Playwright's
  /// `browserContext.on('weberror', (webError: WebError) => ...)`).
  pub fn on(&self, event_name: &str, callback: crate::events::ContextEventCallback) -> crate::events::ListenerId {
    self.events.on(event_name, callback)
  }

  /// One-shot context-level listener — see [`Self::on`].
  pub fn once(&self, event_name: &str, callback: crate::events::ContextEventCallback) -> crate::events::ListenerId {
    self.events.once(event_name, callback)
  }

  /// Remove a previously registered context-level listener.
  pub fn off(&self, id: crate::events::ListenerId) {
    self.events.off(id);
  }

  /// Wait for the next context-level event matching `event_name`, with
  /// `timeout_ms`. Mirrors Playwright's
  /// `browserContext.waitForEvent(event, options?)`.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the event channel is closed.
  pub async fn wait_for_event(&self, event_name: &str, timeout_ms: u64) -> Result<crate::events::ContextEvent> {
    self.events.wait_for_event(event_name, timeout_ms).await
  }

  // ── Context-level APIs (apply to all pages) ────────────────────────────

  /// Add an init script to all pages in this context (current + future).
  /// Mirrors Playwright's `browserContext.addInitScript(script, arg)` from
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:356`.
  /// See [`crate::page::Page::add_init_script`] for argument semantics.
  ///
  /// Returns a [`crate::disposable::Disposable`] whose `dispose()` removes the
  /// injected script from every page it was added to. Mirrors Playwright
  /// `browserContext.addInitScript(...)` which returns a `DisposableObject`
  /// (`client/browserContext.ts:361`).
  ///
  /// # Errors
  ///
  /// Returns an error if `evaluation_script` lowering fails, the context
  /// does not exist, or script injection fails on any page.
  pub async fn add_init_script(
    &self,
    script: crate::options::InitScriptSource,
    arg: Option<serde_json::Value>,
  ) -> Result<crate::disposable::Disposable> {
    let source = crate::options::evaluation_script(script, arg.as_ref())?;
    self.add_init_script_source(source).await
  }

  /// Register a lowered init-script source on this context: applied to
  /// every open page now, recorded in the per-context registry so
  /// [`Self::new_page`] applies it to future pages too (Playwright
  /// context init scripts are current + future).
  pub(crate) async fn add_init_script_source(&self, source: String) -> Result<crate::disposable::Disposable> {
    let composite = self.key.to_composite();
    let (registry, registry_id) = {
      let state = self.state.read().await;
      let id = state
        .context_init_script_counter
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
      (state.context_init_scripts.clone(), id)
    };
    registry
      .write()
      .await
      .entry(composite.clone())
      .or_default()
      .push((registry_id, source.clone()));

    let inner_pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    let mut undo = Vec::with_capacity(inner_pages.len());
    for page in inner_pages {
      let id = page.add_init_script(&source).await?;
      undo.push((page, id));
    }
    Ok(crate::disposable::Disposable::new(move || async move {
      {
        let mut guard = registry.write().await;
        if let Some(entries) = guard.get_mut(&composite) {
          entries.retain(|(id, _)| *id != registry_id);
        }
      }
      for (page, id) in undo {
        page.remove_init_script(&id).await?;
      }
      Ok(())
    }))
  }

  /// Install every registered context-level init script onto a fresh
  /// page.
  async fn apply_context_init_scripts(&self, page: &Arc<Page>) -> Result<()> {
    let sources: Vec<String> = {
      let registry = self.state.read().await.context_init_scripts.clone();
      let guard = registry.read().await;
      guard
        .get(&self.key.to_composite())
        .map(|entries| entries.iter().map(|(_, s)| s.clone()).collect())
        .unwrap_or_default()
    };
    for source in sources {
      page.inner().add_init_script(&source).await?;
    }
    Ok(())
  }

  /// Playwright: `browserContext.setGeolocation(geo)` — mutates the
  /// options bag and re-applies to every open page.
  ///
  /// # Errors
  ///
  /// Returns an error if re-application fails on any page.
  pub async fn set_geolocation(&self, lat: f64, lng: f64, accuracy: f64) -> Result<()> {
    self
      .mutate_options(|o| {
        o.geolocation = Some(crate::options::Geolocation {
          latitude: lat,
          longitude: lng,
          accuracy,
        });
      })
      .await
  }

  /// Playwright: `browserContext.setExtraHTTPHeaders(headers)`.
  ///
  /// # Errors
  ///
  /// Returns an error if re-application fails on any page.
  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<()> {
    let headers = headers.clone();
    self.mutate_options(|o| o.extra_http_headers = Some(headers)).await
  }

  /// Playwright: `browserContext.setOffline(offline)`.
  ///
  /// # Errors
  ///
  /// Returns an error if re-application fails on any page.
  pub async fn set_offline(&self, offline: bool) -> Result<()> {
    self.mutate_options(|o| o.offline = Some(offline)).await
  }

  /// Playwright: `browserContext.setHTTPCredentials(httpCredentials |
  /// null)` (`/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:355`).
  /// Stores the credentials on the context options bag so pages opened
  /// later inherit them, and applies them to every already-open page.
  /// Passing `None` clears stored credentials — future 401 challenges
  /// then surface as the browser's native auth dialog rather than being
  /// answered automatically.
  ///
  /// The bag mutation alone cannot express "clear" (the per-field
  /// `apply_context_options` future is keyed on `Some`), so this drives
  /// the dedicated [`crate::Page::set_http_credentials`] backend path on
  /// each open page directly.
  ///
  /// # Errors
  ///
  /// Returns an error if any open page's backend rejects the change
  /// (e.g. a backend that does not support auth-challenge interception).
  pub async fn set_http_credentials(&self, credentials: Option<crate::options::HttpCredentials>) -> Result<()> {
    let composite = self.key.to_composite();
    {
      let state = self.state.read().await;
      let mut opts = state.get_context_options(&composite).unwrap_or_default();
      opts.http_credentials.clone_from(&credentials);
      state.set_context_options(&composite, opts);
    }
    let pages = self.pages().await?;
    for page in pages {
      page.set_http_credentials(credentials.clone()).await?;
    }
    Ok(())
  }

  /// Register a route handler for all pages in this context — current
  /// AND future (the registration lives in the per-context registry and
  /// is re-applied by [`Self::new_page`], matching Playwright's
  /// context-scoped `_routes` list). The `times` budget is shared
  /// context-wide: requests from any page of the context consume the
  /// same counter.
  ///
  /// Returns a [`crate::disposable::Disposable`] whose `dispose()` removes the
  /// handler from the registry and every page (equivalent to
  /// [`Self::unroute`]). Mirrors Playwright `browserContext.route(...)`
  /// which returns a `DisposableStub` (`client/browserContext.ts:377`).
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or route registration fails.
  pub async fn route(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::route::RouteHandler,
    times: Option<u32>,
  ) -> Result<crate::disposable::Disposable> {
    let registration = crate::route::RegisteredRoute::context_scoped(matcher.clone(), handler, times);
    self.install_context_route(registration).await?;
    let ctx = self.clone();
    Ok(crate::disposable::Disposable::new(move || async move {
      ctx.unroute(&matcher).await
    }))
  }

  /// Register a context-scoped route: push into the per-context registry
  /// and fan a clone (sharing the `times` budget) onto every open page.
  async fn install_context_route(&self, registration: crate::route::RegisteredRoute) -> Result<()> {
    {
      let registry = self.state.read().await.context_routes_handle();
      let mut guard = registry.write().await;
      guard
        .entry(self.key.to_composite())
        .or_default()
        .push(registration.clone());
    }
    // A context with no pages yet isn't registered in state (it
    // materialises on first `new_page`) — the registry entry above is
    // all that's needed; `new_page` re-applies it. Same tolerance as
    // `expose_binding` / `route_web_socket`.
    let inner_pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    for page in inner_pages {
      page.route(registration.clone()).await?;
    }
    Ok(())
  }

  /// Install every registered context-level route onto a fresh page.
  /// Clones share the registry entry's `times` budget; entries whose
  /// budget is already exhausted are pruned instead of copied.
  async fn apply_context_routes(&self, page: &Arc<Page>) -> Result<()> {
    let routes = {
      let registry = self.state.read().await.context_routes_handle();
      let mut guard = registry.write().await;
      match guard.get_mut(&self.key.to_composite()) {
        Some(entries) => {
          entries.retain(crate::route::RegisteredRoute::live);
          entries.clone()
        },
        None => return Ok(()),
      }
    };
    for registration in routes {
      page.inner().route(registration).await?;
    }
    Ok(())
  }

  /// Playwright: `browserContext.routeWebSocket(url, handler)`. Intercepts
  /// WebSocket connections matching `matcher` on every page in this
  /// context — current AND future (the route is registered in the
  /// per-context registry and re-applied by [`Self::new_page`], matching
  /// Playwright's context-scoped interception patterns). Page-level
  /// routes take precedence over context-level routes at socket
  /// creation.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or installing the page
  /// mock / binding fails on any page.
  pub async fn route_web_socket(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::web_socket_route::WsHandler,
  ) -> Result<()> {
    {
      let registry = self.state.read().await.context_ws_routes_handle();
      let mut guard = registry.write().await;
      guard
        .entry(self.key.to_composite())
        .or_default()
        .push((matcher.clone(), handler.clone()));
    }
    // A context with no pages yet isn't registered in state (it
    // materialises on first `new_page`) — the registry entry above is
    // all that's needed; `new_page` re-applies it. Same tolerance as
    // `expose_binding`.
    let inner_pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    for inner in inner_pages {
      let page = Page::with_context(inner, self.clone());
      page
        .route_web_socket_scoped(
          matcher.clone(),
          handler.clone(),
          crate::web_socket_route::WsRouteScope::Context,
        )
        .await?;
    }
    Ok(())
  }

  /// Playwright: `browserContext.routeFromHAR(har, options?)`. Replays a HAR
  /// file across every page in this context — current and future.
  /// Replay-only; recording (`update: true`) is unsupported.
  ///
  /// # Errors
  ///
  /// Returns an error if the HAR file cannot be read/parsed or routes fail
  /// to install.
  pub fn route_from_har(
    &self,
    path: &std::path::Path,
  ) -> crate::action::Action<'static, crate::har::RouteFromHarOptions, ()> {
    let ctx = self.clone();
    let path = path.to_path_buf();
    crate::action::Action::new(move |opts| Box::pin(async move { ctx.route_from_har_impl(&path, opts).await }))
  }

  pub(crate) async fn route_from_har_impl(
    &self,
    path: &std::path::Path,
    options: crate::har::RouteFromHarOptions,
  ) -> Result<()> {
    if options.update {
      // Record instead of replay: register a recorder flushed when the
      // context closes. Playwright defaults (`client/tracing.ts:131-135`):
      // content `attach`, mode `minimal`.
      let start_len = match self.network_log_handle().await {
        Some(log) => log.read().await.len(),
        None => 0,
      };
      let recorder = crate::tracing::HarRecorder {
        path: path.to_path_buf(),
        content: options
          .update_content
          .unwrap_or(crate::tracing::HarContentPolicy::Attach),
        mode: options.update_mode.unwrap_or(crate::tracing::HarMode::Minimal),
        url_filter: options.url.unwrap_or_else(crate::url_matcher::UrlMatcher::any),
        resources_dir: None,
        start_len,
      };
      let registry = self.state.read().await.context_har_updates.clone();
      registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(self.key.to_composite())
        .or_default()
        .push(recorder);
      return Ok(());
    }
    let handler = crate::har::route_handler_from_file(path, options.not_found)?;
    let matcher = options.url.unwrap_or_else(crate::url_matcher::UrlMatcher::any);
    self
      .install_context_route(crate::route::RegisteredRoute::context_scoped(matcher, handler, None))
      .await
  }

  /// Remove context-scoped route handlers matching the given matcher
  /// from the registry and from all pages. Page-scoped routes with the
  /// same matcher stay active, matching Playwright where
  /// `context.unroute` only touches `context._routes`.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or route removal fails.
  pub async fn unroute(&self, matcher: &crate::url_matcher::UrlMatcher) -> Result<()> {
    {
      let registry = self.state.read().await.context_routes_handle();
      let mut guard = registry.write().await;
      if let Some(entries) = guard.get_mut(&self.key.to_composite()) {
        entries.retain(|r| !r.matcher.equivalent(matcher));
      }
    }
    let inner_pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    for page in inner_pages {
      page.unroute(matcher, crate::route::RouteScope::Context).await?;
    }
    Ok(())
  }

  /// Remove all context-scoped route handlers from the registry and
  /// from every page. Page-scoped routes stay active. Mirrors
  /// Playwright's
  /// `browserContext.unrouteAll({ behavior?: 'wait' | 'ignoreErrors' | 'default' })`;
  /// ferridriver route handlers resolve synchronously inside the
  /// interception chain, so every `behavior` variant performs the same
  /// teardown.
  ///
  /// # Errors
  ///
  /// Returns an error if the underlying interception teardown fails.
  pub async fn unroute_all(&self, behavior: Option<crate::options::UnrouteBehavior>) -> Result<()> {
    {
      let registry = self.state.read().await.context_routes_handle();
      let mut guard = registry.write().await;
      guard.remove(&self.key.to_composite());
    }
    let inner_pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    for page in inner_pages {
      page
        .unroute_all(behavior.unwrap_or_default(), Some(crate::route::RouteScope::Context))
        .await?;
    }
    Ok(())
  }

  // ── Exposed bindings / functions (apply to all pages) ───────────────────

  /// Playwright: `browserContext.exposeBinding(name, callback)` from
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:364`.
  ///
  /// Registers `window[name]` on every page in this context (current +
  /// future). The page-side call routes back into `callback`, which
  /// receives a [`crate::events::BindingSource`] as its first argument
  /// followed by the page-side call args. The callback's return value
  /// (after awaiting any returned promise in the binding layers) is
  /// delivered to the page-side caller.
  ///
  /// Returns a [`crate::disposable::Disposable`] whose `dispose()` removes the binding
  /// from the registry and from every page in the context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or injection fails
  /// on any page.
  pub async fn expose_binding(
    &self,
    name: &str,
    callback: crate::events::ExposedBinding,
  ) -> Result<crate::disposable::Disposable> {
    {
      let bindings = self.state.read().await.context_bindings_handle();
      let mut guard = bindings.write().await;
      guard
        .entry(self.key.to_composite())
        .or_default()
        .insert(name.to_string(), callback.clone());
    }
    // Apply to every page already open in this context.
    let pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    let composite = self.key.to_composite();
    for page in &pages {
      let binding_for_page = bind_source(callback.clone(), composite.clone());
      page.expose_binding(name, binding_for_page).await?;
    }
    Ok(crate::disposable::Disposable::new({
      let this = self.clone();
      let name = name.to_string();
      move || async move {
        let _ = this.remove_exposed_binding(&name).await;
        Ok(())
      }
    }))
  }

  /// Playwright: `browserContext.exposeFunction(name, callback)` from
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:370`.
  ///
  /// `exposeFunction` is `exposeBinding` minus the source argument:
  /// the supplied source-less [`crate::events::ExposedFn`] is wrapped
  /// into an [`crate::events::ExposedBinding`] that discards the
  /// [`crate::events::BindingSource`].
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or injection fails
  /// on any page.
  pub async fn expose_function(
    &self,
    name: &str,
    callback: crate::events::ExposedFn,
  ) -> Result<crate::disposable::Disposable> {
    let binding: crate::events::ExposedBinding = Arc::new(move |_source, args| callback(args));
    self.expose_binding(name, binding).await
  }

  /// Remove a previously exposed binding/function from the registry and
  /// from every page in this context. Mirrors Playwright's
  /// `BrowserContext.removeExposedBinding` (driven by `Disposable`).
  ///
  /// # Errors
  ///
  /// Returns an error if removal fails on any open page.
  pub async fn remove_exposed_binding(&self, name: &str) -> Result<()> {
    {
      let bindings = self.state.read().await.context_bindings_handle();
      let mut guard = bindings.write().await;
      if let Some(map) = guard.get_mut(&self.key.to_composite()) {
        map.remove(name);
      }
    }
    let pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    for page in &pages {
      page.remove_exposed_function(name).await?;
    }
    Ok(())
  }
}

/// Stamp the context's composite key onto the [`crate::events::BindingSource`]
/// the backend built for each call. The backend fills `page` / `frame`
/// (the real calling frame — an iframe caller surfaces its own frame id);
/// only the context key is unknown at that layer.
fn bind_source(binding: crate::events::ExposedBinding, context_key: String) -> crate::events::ExposedBinding {
  Arc::new(move |mut source, args| {
    source.context.clone_from(&context_key);
    binding(source, args)
  })
}

/// Apply every supported field on a [`crate::options::BrowserContextOptions`]
/// bag to a freshly-opened [`Page`]. Each field corresponds to an
/// existing Page setter; fields with no backend support on a given
/// backend bubble up the backend's `FerriError` (typically
/// `FerriError::Unsupported { reason }`) which then fails
/// `ContextRef::new_page`. Field application order mirrors Playwright's
/// `BrowserContext.doCreateNewContext` server-side sequencing
/// (`/tmp/playwright/packages/playwright-core/src/server/browserContext.ts`):
/// emulation before navigation, permissions last.
///
/// Fields deferred to a follow-up session (no-op here): `proxy`,
/// `record_har`, `storage_state`, `screen`, `base_url`, `service_workers`,
/// `accept_downloads`, `ignore_https_errors`, `strict_selectors` beyond
/// storage, `bypass_csp`. Each gets a dedicated implementation when the
/// supporting infrastructure lands. (`http_credentials` is now wired
/// through CDP both at context-creation time and via the dynamic
/// `ContextRef::set_http_credentials` setter.)
/// Apply a context-options bag to a freshly-opened page. This
/// delegates to the backend's single `apply_context_options` dispatch
/// which fires every protocol command in parallel and aggregates
/// errors (matches Playwright's `crPage._updateXxx()` set driven by
/// `Promise.all`). Keeping the helper thin here means the context
/// layer stays backend-agnostic — the only Rust-level choice is
/// "apply the whole bag or don't".
async fn apply_context_options(page: &Arc<Page>, opts: &crate::options::BrowserContextOptions) -> Result<()> {
  Box::pin(page.apply_context_options(opts)).await
}

/// Kick off a background recording runtime for a freshly-opened page
/// whose context has `recordVideo` enabled. Constructs the Video
/// handle, attaches it to the page, and spawns a task that awaits the
/// page close then drives the encoder to completion.
///
/// The attach-first-then-spawn ordering matters: callers of
/// [`crate::Page::video`] can observe the handle the moment `new_page`
/// returns, even before the first frame has been captured. The handle
/// blocks on the underlying watch channel inside `path()` /
/// `save_as()` / `delete()`, so observing an in-progress recording
/// and calling `await video.path()` resolves cleanly once the page
/// closes.
///
/// Should a backend's `AnyPage::start_screencast` ever surface a typed
/// error, the video sink is populated with the error text so the
/// Playwright contract — `page.video()` returns a handle; the handle's
/// methods reject with a clear reason — is preserved. (Every current
/// backend, including Playwright `WebKit`, supports screencast.)
fn start_video_recording(page: &Arc<Page>, opts: &crate::options::RecordVideoOptions) {
  // Compose the output filename: `<dir>/<timestamp>-<page-id>.<ext>`.
  // Playwright derives its name from the page's GUID; ferridriver uses
  // millisecond-since-epoch + an atomic counter — unique across pages
  // within the same context and the same second. The extension comes
  // from `crate::video::video_extension()` so changes to the encoder
  // format propagate without a filename rewrite.
  static VIDEO_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

  let (video, sink) = crate::video::Video::new();
  page.attach_video(Arc::new(video));

  let size = opts.size.unwrap_or_default();
  let width = size.width & !1;
  let height = size.height & !1;
  let millis = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |d| d.as_millis());
  let id = VIDEO_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
  let filename = format!("{millis}-{id}.{}", crate::video::video_extension());
  let output_path = opts.dir.join(filename);

  // Ensure the directory exists before the encoder opens the file.
  // Errors here funnel into the sink so the user-facing `path()`
  // rejects with a clear reason instead of the test hanging.
  if let Err(e) = std::fs::create_dir_all(&opts.dir) {
    sink.finish_err(crate::FerriError::backend(format!(
      "failed to create recordVideo.dir {}: {e}",
      opts.dir.display()
    )));
    return;
  }

  let page_for_task = page.clone();
  tokio::spawn(async move {
    // Default CDP screencast quality matches Playwright's
    // `DEFAULT_SCREENCAST_OPTIONS.quality` (90).
    let handle = match crate::video::start_recording(&page_for_task, output_path.clone(), width, height, 90).await {
      Ok(h) => h,
      Err(e) => {
        sink.finish_err(crate::FerriError::backend(format!("start_recording: {e}")));
        return;
      },
    };
    // Wait for the page to close before stopping the recording.
    // Polls at 50ms — matches the cadence of the frame-cache
    // listener task and keeps the observable delay inside the
    // "page.close() has returned" barrier below ~50ms in the
    // common case.
    while !page_for_task.is_closed() {
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    match handle.stop(&page_for_task).await {
      Ok(path) => sink.finish_ok(path),
      Err(e) => sink.finish_err(crate::FerriError::backend(format!("stop recording: {e}"))),
    }
  });
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};

  #[tokio::test]
  async fn expose_function_wrapper_discards_source() {
    // The exposeFunction adapter must drop the BindingSource and pass
    // only the page-side args to the user callback (Playwright:
    // `(source, ...args) => callback(...args)`).
    let seen = Arc::new(AtomicUsize::new(0));
    let seen_cb = seen.clone();
    let inner: crate::events::ExposedFn = Arc::new(move |args: Vec<serde_json::Value>| {
      seen_cb.store(args.len(), Ordering::SeqCst);
      Box::pin(async move { serde_json::json!(args.iter().filter_map(serde_json::Value::as_i64).sum::<i64>()) })
    });
    let binding: crate::events::ExposedBinding = Arc::new(move |_source, args| inner(args));
    let source = crate::events::BindingSource {
      context: "inst:ctx".into(),
      page: "frame-1".into(),
      frame: "frame-1".into(),
    };
    let out = binding(source, vec![serde_json::json!(20), serde_json::json!(22)]).await;
    assert_eq!(seen.load(Ordering::SeqCst), 2);
    assert_eq!(out, serde_json::json!(42));
  }

  #[tokio::test]
  async fn disposable_runs_once() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let d = crate::disposable::Disposable::new(move || {
      let c = c.clone();
      async move {
        c.fetch_add(1, Ordering::SeqCst);
        Ok(())
      }
    });
    d.dispose().await.expect("dispose");
    d.dispose().await.expect("dispose");
    assert_eq!(count.load(Ordering::SeqCst), 1);
  }
}
