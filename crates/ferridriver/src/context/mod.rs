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
  /// Made by `browser.newPage()` for that one page, so it closes when
  /// the page does, as Playwright's `Page._ownedContext` is.
  pub(crate) owned_by_page: bool,
  /// Which page becomes active when the active one closes.
  pub(crate) refocus: Refocus,
}

/// Which page a context makes active when its active page closes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Refocus {
  /// The page that took the closed one's place in the list, else the last
  /// one: Playwright MCP's `Context._onPageClosed`.
  #[default]
  Neighbour,
  /// The first page left: Playwright's dashboard, which attaches
  /// `context.pages()[0]` when the attached page closes
  /// (`DashboardConnection._handleAttachedPageClose`).
  First,
}

impl Refocus {
  /// The index the active page moves to once the page at `place` closed,
  /// in a list now `len` long.
  fn index(self, place: usize, len: usize) -> usize {
    match self {
      Self::Neighbour => place.min(len.saturating_sub(1)),
      Self::First => 0,
    }
  }
}

impl BrowserContext {
  /// Create a new empty context.
  pub(crate) fn new(name: String, refocus: Refocus) -> Self {
    Self {
      pages: Vec::new(),
      active_page_idx: 0,
      ref_map: Arc::new(ArcSwap::from_pointee(HashMap::default())),
      console_log: Arc::new(RwLock::new(Vec::new())),
      network_log: Arc::new(RwLock::new(Vec::new())),
      dialog_log: Arc::new(RwLock::new(Vec::new())),
      name,
      cdp_context_id: None,
      owned_by_page: false,
      refocus,
    }
  }

  /// List a page the browser opened. It becomes the active page only when
  /// the context has none, as Playwright MCP's `Context._onPageCreated`
  /// makes a new tab current only when there is no current tab: focus
  /// moves when a caller selects a page, never because a page opened one.
  pub(crate) fn list_popup(&mut self, page: AnyPage) {
    let had_active = self.active_page().is_some();
    self.pages.push(page);
    if !had_active {
      self.active_page_idx = self.pages.len() - 1;
    }
  }

  /// Drop the page at `index` from the list, moving the active index with
  /// the page it pointed at, or by [`Refocus`] when that page was the one
  /// dropped.
  pub(crate) fn remove_page(&mut self, index: usize) -> AnyPage {
    let page = self.pages.remove(index);
    self.active_page_idx = match self.active_page_idx.cmp(&index) {
      std::cmp::Ordering::Less => self.active_page_idx,
      std::cmp::Ordering::Equal => self.refocus.index(index, self.pages.len()),
      std::cmp::Ordering::Greater => self.active_page_idx - 1,
    };
    page
  }

  /// Context name.
  #[must_use]
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Get the active page in this context.
  ///
  /// A closed page is skipped: closing a tab (or losing its target) leaves the
  /// `AnyPage` in place, and handing that back sends every later command to a
  /// dead CDP session -- surfacing as `Session with given id not found` on the
  /// next call rather than as a lost context. The page [`Refocus`] picks among
  /// the live ones stands in, the same one `Self::prune_closed_pages` makes
  /// active once it drops the dead entry, and `None` once nothing is open so
  /// callers can rebuild.
  ///
  /// Read-only, so it cannot drop the dead entries it skips;
  /// `Self::prune_closed_pages` does that at the `&mut` entry points
  /// (opening a page, refreshing the list).
  #[must_use]
  pub fn active_page(&self) -> Option<&AnyPage> {
    let index = self.active_page_idx;
    if let Some(page) = self.pages.get(index)
      && !page.is_closed()
    {
      return Some(page);
    }
    let live = |page: &&AnyPage| !page.is_closed();
    match self.refocus {
      Refocus::First => self.pages.iter().find(live),
      Refocus::Neighbour => self
        .pages
        .iter()
        .skip(index.saturating_add(1))
        .find(live)
        .or_else(|| self.pages[..index.min(self.pages.len())].iter().rev().find(live)),
    }
  }

  /// Drop closed pages, keeping the active index on the same page when it
  /// survived and moving it by [`Refocus`] when it did not.
  ///
  /// Closed pages were only ever removed by an explicit `close_page`, so a
  /// context whose tabs were closed in the browser UI (or by
  /// `window.close()`) grew a `Vec` of corpses for its whole life and paid
  /// a scan past them on every command.
  pub(crate) fn prune_closed_pages(&mut self) {
    if !self.pages.iter().any(AnyPage::is_closed) {
      return;
    }
    let active_survives = self.pages.get(self.active_page_idx).is_some_and(|p| !p.is_closed());
    let live_before_active = self.pages[..self.active_page_idx.min(self.pages.len())]
      .iter()
      .filter(|p| !p.is_closed())
      .count();
    self.pages.retain(|page| !page.is_closed());
    self.active_page_idx = if active_survives {
      live_before_active
    } else {
      self.refocus.index(live_before_active, self.pages.len())
    };
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
  /// `testIdAttribute` for this context (`None` = the process default
  /// `selectors.setTestIdAttribute` set). Shared per composite session
  /// key, so every handle to one context reads the same value.
  test_id_attribute: crate::state::TestIdAttributeSlot,
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
  /// The state's lock-free page registry, shared by every clone of this
  /// handle. Empty until the handle has reached the state once: at
  /// construction when its lock was free, or on the first call that
  /// awaits or finds it free.
  pages: Arc<std::sync::OnceLock<crate::state::ContextPages>>,
}

impl std::fmt::Debug for ContextRef {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ContextRef")
      .field("name", &self.name)
      .field("closed", &self.is_closed())
      .finish_non_exhaustive()
  }
}

impl ContextRef {
  /// [`Self::new`] for a caller that can await.
  ///
  /// The sync constructor falls back to a TRANSIENT emitter when it
  /// cannot take the state lock, and an event emitted on that emitter
  /// reaches nobody — a `waitForEvent('page')` on the real context just
  /// hangs. tokio's `RwLock` is write-preferring, so a merely QUEUED
  /// writer is enough to make `try_read` fail; the popup pump hit this
  /// deterministically. Anywhere there is an `.await` to spend, spend it
  /// here instead of degrading.
  pub async fn new_async(state: Arc<RwLock<BrowserState>>, name: String) -> Self {
    let registries = state.read().await.context_registries();
    Self::from_registries(state, name, &registries)
  }

  /// Construct a handle directly from [`crate::state::ContextRegistries`]
  /// without touching `RwLock<BrowserState>`. Always binds to the real
  /// shared emitter, closed flag, `testIdAttribute`, and page registry even
  /// while another worker holds a state write lock.
  pub(crate) fn from_registries(
    state: Arc<RwLock<BrowserState>>,
    name: String,
    registries: &crate::state::ContextRegistries,
  ) -> Self {
    let key = registries.session_key(&name);
    let composite = key.to_composite();
    let events = registries.events(&composite);
    let closed = registries.closed(&composite);
    let test_id_attribute = registries.test_id_attribute(&composite);
    let pages = registries.pages();
    let context = Self::from_parts(state, name, key, events, closed, test_id_attribute);
    let _ = context.pages.set(pages);
    context
  }

  fn from_parts(
    state: Arc<RwLock<BrowserState>>,
    name: String,
    key: SessionKey,
    events: crate::events::ContextEventEmitter,
    closed: Arc<std::sync::atomic::AtomicBool>,
    test_id_attribute: Arc<std::sync::RwLock<Option<String>>>,
  ) -> Self {
    Self {
      state,
      name: Arc::from(name),
      key,
      default_timeout_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
      test_id_attribute,
      default_navigation_timeout_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
      events,
      browser: None,
      closed,
      pages: Arc::new(std::sync::OnceLock::new()),
    }
  }

  /// The page registry, reached through the state if this handle has
  /// not reached it before and its lock is free right now.
  fn page_registry(&self) -> Option<&crate::state::ContextPages> {
    if self.pages.get().is_none()
      && let Ok(state) = self.state.try_read()
    {
      let _ = self.pages.set(state.context_pages.clone());
    }
    self.pages.get()
  }

  pub fn new(state: Arc<RwLock<BrowserState>>, name: String) -> Self {
    // Look up (or initialise) the shared emitter for this composite
    // key, and resolve the key against the state's own instance names.
    // Uses `try_read` because `ContextRef::new` must be callable
    // from sync contexts (e.g. `Browser::default_context`). In the
    // common case the state lock is uncontended at construction time;
    // if `try_read` fails (concurrent writer) we fall back to a
    // transient per-instance emitter so the handle is still usable —
    // event delivery would then be scoped to this `ContextRef` clone
    // only, matching the old behaviour. `get_or_create_context_events`
    // itself uses a `std::sync::Mutex` so it doesn't need the tokio
    // read guard to stay alive beyond the call.
    let (key, events, closed, test_id_attribute, pages) = match state.try_read() {
      Ok(s) => {
        let key = s.session_key(&name);
        (
          key.clone(),
          s.get_or_create_context_events(&key.to_composite()),
          s.get_or_create_context_closed(&key.to_composite()),
          s.get_or_create_context_test_id_attribute(&key.to_composite()),
          Some(s.context_pages.clone()),
        )
      },
      Err(_) => (
        SessionKey::parse(&name),
        crate::events::ContextEventEmitter::new(),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        Arc::new(std::sync::RwLock::new(None)),
        // The page registry is the state's own: there is no stand-in,
        // and `page_registry` reaches it once the lock is free.
        None,
      ),
    };
    let context = Self::from_parts(state, name, key, events, closed, test_id_attribute);
    if let Some(pages) = pages {
      let _ = context.pages.set(pages);
    }
    context
  }

  /// Attach the parent [`crate::Browser`] handle so [`Self::browser`]
  /// returns it. Called by [`crate::Browser::new_context`] and
  /// [`crate::Browser::default_context`] right after construction.
  #[must_use]
  pub fn with_browser(mut self, browser: crate::Browser) -> Self {
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

  /// Wait until every tab the browser opened before this call is listed
  /// and announced, at most `within`. A click that opens a tab returns
  /// before the tab has committed its first document, which is when it
  /// joins its context; whoever reports the context right after the click
  /// waits here to find the tab in it.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser stops answering, or `within` passes
  /// with a tab still on its way.
  pub async fn opened_pages_settled(&self, within: std::time::Duration) -> Result<()> {
    let browser = self.state.read().await.instance_browser(&self.key.instance).cloned();
    let Some(browser) = browser else {
      return Ok(());
    };
    let timeout_ms = u64::try_from(within.as_millis()).unwrap_or(u64::MAX);
    tokio::time::timeout(within, browser.popups_settled())
      .await
      .map_err(|_| crate::error::FerriError::timeout("waiting for the tabs the page opened", timeout_ms))?
  }

  /// The context's active page, opening one only when it has none. A
  /// browser this instance connects to brings the pages already open in
  /// it, which connecting adopts, so a client attaching to a running
  /// browser starts on the page that browser shows, as Playwright's
  /// `connectOverCDP` hands back the pages already there.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser cannot be reached or a page cannot be
  /// opened.
  pub async fn active_or_new_page(&self) -> Result<AnyPage> {
    Box::pin(BrowserState::ensure_instance_shared(&self.state, &self.key.instance)).await?;
    {
      let state = self.state.read().await;
      if let Ok(page) = state.active_page(&self.key.to_composite()) {
        return Ok(page.clone());
      }
    }
    Ok(Box::pin(self.new_page()).await?.inner().clone())
  }

  /// Create a new page in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if page creation fails.
  pub async fn new_page(&self) -> Result<Arc<Page>> {
    // The launch runs off the state lock: holding the write guard
    // across a browser spawn stalls every other session in the process,
    // including ones on a different instance.
    Box::pin(BrowserState::ensure_instance_shared(&self.state, &self.key.instance)).await?;

    let composite = self.key.to_composite();
    // Read the (optional) `BrowserContextOptions` bag and page-open plan
    // under a single read lock — a custom `viewport` (or `null` viewport)
    // has to be known up-front so the backend opens the page at the right
    // size rather than resizing afterwards.
    let (ctx_opts, plan) = {
      let state = self.state.read().await;
      let opts = state.get_context_options(&composite);
      let plan = state.page_open_plan(&self.key)?;
      (opts, plan)
    };
    let effective_viewport = effective_viewport(ctx_opts.as_ref(), plan.viewport.clone());

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
      let ctx_id = Box::pin(self.create_backend_context(&plan.browser, ctx_opts.as_ref())).await?;
      let page = Box::pin(
        plan
          .browser
          .new_page("about:blank", Some(&ctx_id), effective_viewport.as_ref()),
      )
      .await?;
      (page, Some(ctx_id))
    };

    let (proxy, record_opts, registries) = {
      let mut state = self.state.write().await;
      state.register_opened_page(&self.key, any_page.clone(), browser_context_id)?;
      (
        state.effective_proxy(&composite),
        state.get_record_video(&composite),
        PageRegistries {
          bindings: state.context_bindings_handle(),
          callbacks: Arc::clone(&state.context_callbacks),
          ws_routes: state.context_ws_routes_handle(),
          routes: state.context_routes_handle(),
          init_scripts: Arc::clone(&state.context_init_scripts),
        },
      )
    };

    let page = Page::with_context(any_page, self.clone());
    page.set_cached_viewport(effective_viewport.as_ref().map(|vp| (vp.width, vp.height)));

    if proxy.is_some() {
      Box::pin(page.inner().set_proxy(proxy)).await?;
    }

    apply_context_options_or_defaults(&page, ctx_opts.as_ref()).await?;
    if let Some(opts) = ctx_opts.as_ref() {
      self.install_record_har(opts).await;
      Box::pin(self.hydrate_storage_state(opts)).await?;
    }
    if let Some(opts) = record_opts {
      start_video_recording(&page, &opts);
    }

    Box::pin(registries.apply(&composite, &page)).await?;

    // A trace with screenshots films every page of the context —
    // including ones opened mid-recording.
    if let Some(recorder) = crate::trace::recorder_for(&self.key.to_composite()) {
      crate::trace::record_page_open(&recorder, &crate::trace::trace_page_id(page.inner()));
      if recorder.screenshots {
        crate::trace::spawn_screencast_pump(&recorder, page.inner()).await;
      }
      // The context init script only reaches the page's NEXT documents;
      // its initial about:blank predates registration on some backends,
      // so install the snapshot streamer into it directly.
      if recorder.snapshots
        && let Err(e) = page.inner().evaluate(&crate::snapshotter::install_source()).await
      {
        tracing::debug!(target: "ferridriver::trace", "snapshot streamer eval skipped: {e}");
      }
    }

    // Playwright fires `browserContext.on('page')` for every page
    // created in the context; a `waitForEvent('page')` registered
    // before this `newPage` call resolves with the same wrapper the
    // caller receives.
    self.emit_page_created(Arc::clone(&page));

    Ok(page)
  }

  /// A new wrapper of this context's open page whose wrappers share
  /// `slot`.
  pub(crate) fn page_sharing(&self, slot: &crate::backend::PageBackref) -> Option<Arc<Page>> {
    self
      .page_registry()?
      .open(&self.key.to_composite())
      .into_iter()
      .find(|page| page.page_backref_handle().same_page(slot))
      .map(|page| Page::with_context(page, self.clone()))
  }

  /// Make this context in the browser if no page of it has yet, and
  /// record it, so its first page opens into it.
  async fn ensure_backend_context(&self) -> Result<()> {
    Box::pin(BrowserState::ensure_instance_shared(&self.state, &self.key.instance)).await?;
    if &*self.key.context == "default" {
      return Ok(());
    }
    let plan = self.state.read().await.page_open_plan(&self.key)?;
    if plan.browser_context_id.is_some() {
      return Ok(());
    }
    let ctx_opts = self.state.read().await.get_context_options(&self.key.to_composite());
    let id = Box::pin(self.create_backend_context(&plan.browser, ctx_opts.as_ref())).await?;
    self.state.write().await.register_backend_context(&self.key, id)
  }

  /// Create this context in the browser. An authenticated SOCKS5 proxy,
  /// which no browser can log in to, is handed over as a loopback relay that
  /// logs in for it.
  async fn create_backend_context(
    &self,
    browser: &crate::backend::AnyBrowser,
    stored: Option<&crate::options::BrowserContextOptions>,
  ) -> Result<String> {
    let Some(proxy) = stored
      .and_then(|opts| opts.proxy.as_ref())
      .filter(|proxy| proxy.is_authenticated_socks())
    else {
      return browser.new_context(stored).await;
    };
    let composite = self.key.to_composite();
    let relayed = self.state.read().await.browser_facing_proxy(&composite, proxy).await?;
    let mut opts = stored.cloned().unwrap_or_default();
    opts.proxy = Some(relayed);
    browser.new_context(Some(&opts)).await
  }

  /// Fire `ContextEvent::Page` on this context's shared emitter — used
  /// by [`Self::new_page`] and the popup pump.
  pub(crate) fn emit_page_created(&self, page: Arc<Page>) {
    self.events.emit(crate::events::ContextEvent::Page(page));
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

  /// A raw `WebKit` Inspector Protocol session on `page`'s main target,
  /// the `WebKit` counterpart of [`Self::new_cdp_session`]. Playwright
  /// keeps the same session private, as `WKPage._session`. `WebKit` only.
  ///
  /// It is ferridriver's own session on the target, not a second one, so
  /// a command that turns a domain off (`Runtime.disable`,
  /// `Console.disable`, `Page.disable`) turns it off for ferridriver too,
  /// and the page stops reporting what that domain carried.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::InvalidArgument`] for a page of
  /// another context, and [`crate::error::FerriError::Unsupported`] on
  /// Chromium, `BiDi`, and `WebDriver`.
  pub fn new_webkit_session(&self, page: &Page) -> Result<crate::webkit_session::WebKitSession> {
    if page.context().map(Self::composite) != Some(self.composite()) {
      return Err(crate::error::FerriError::invalid_argument(
        "page",
        "the page belongs to another browser context",
      ));
    }
    page.inner().new_webkit_session()
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

  async fn context_callbacks(&self) -> Vec<(String, crate::events::ExposedBinding)> {
    let callbacks = Arc::clone(&self.state.read().await.context_callbacks);
    let guard = callbacks.read().await;
    guard
      .get(&self.key.to_composite())
      .map(|m| m.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect())
      .unwrap_or_default()
  }

  /// The context's init-script callbacks on a popup still paused before
  /// its first document. They go ahead of the init scripts, whose
  /// functions call through the controller they install.
  async fn apply_context_callbacks_pre_doc(&self, page: &AnyPage) -> Result<()> {
    let composite = self.key.to_composite();
    for (name, binding) in self.context_callbacks().await {
      page
        .expose_callback_pre_doc(&name, bind_source(binding, composite.clone()))
        .await?;
    }
    Ok(())
  }

  /// Inject every registered context-level binding via the paused-safe
  /// [`AnyPage::expose_binding_pre_doc`] path — used by the popup pump
  /// before resuming, so the popup's first document sees every
  /// context-level binding from document-start.
  async fn apply_context_bindings_pre_doc(&self, page: &AnyPage) -> Result<()> {
    let composite = self.key.to_composite();
    let bindings = {
      let bindings_handle = self.state.read().await.context_bindings_handle();
      let guard = bindings_handle.read().await;
      guard
        .get(&composite)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect::<Vec<_>>())
        .unwrap_or_default()
    };
    for (name, binding) in bindings {
      let binding_for_page = bind_source(binding, composite.clone());
      page.expose_binding_pre_doc(&name, binding_for_page).await?;
    }
    Ok(())
  }

  /// Playwright: `browserContext.pages(): Page[]`. The pages open in
  /// this context, in the order they were opened, without waiting on
  /// anything: a context that has not opened in the browser yet, or has
  /// closed, has none.
  ///
  /// A browser driven over classic `WebDriver` does not announce a
  /// window opened behind the session's back; [`Self::discover_pages`]
  /// asks it.
  #[must_use]
  pub fn pages(&self) -> Vec<Arc<Page>> {
    let Some(registry) = self.page_registry() else {
      return Vec::new();
    };
    registry
      .open(&self.key.to_composite())
      .into_iter()
      .map(|inner| Page::with_context(inner, self.clone()))
      .collect()
  }

  /// [`Self::pages`] after asking the browser which windows it has,
  /// which is how a classic `WebDriver` session learns of one it did not
  /// open and forgets one that closed. Every other backend is told as it
  /// happens, and this only waits for the state.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist, or the browser
  /// cannot list its windows.
  pub async fn discover_pages(&self) -> Result<Vec<Arc<Page>>> {
    let needs_discovery = self
      .state
      .read()
      .await
      .instance_browser(&self.key.instance)
      .is_some_and(|browser| browser.kind() == crate::backend::BackendKind::WebDriver);
    if needs_discovery {
      self.state.write().await.refresh_pages(&self.composite()).await?;
    }
    let inner_pages = {
      let state = self.state.read().await;
      match state.context(&self.name) {
        Ok(ctx) => ctx.pages.clone(),
        Err(_)
          if self
            .browser
            .as_ref()
            .is_some_and(|browser| browser.owns_context(&self.name)) =>
        {
          Vec::new()
        },
        Err(error) => return Err(error),
      }
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

  /// `context.credentials` handle. Playwright: `browserContext.credentials`,
  /// the context's virtual `WebAuthn` authenticator.
  #[must_use]
  pub fn credentials(&self) -> crate::credentials::Credentials {
    crate::credentials::Credentials::new(self.clone())
  }

  pub(crate) async fn credential_store(&self) -> Arc<crate::credentials::CredentialStore> {
    let stores = Arc::clone(&self.state.read().await.context_credentials);
    let mut stores = stores.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(stores.entry(self.key.to_composite()).or_default())
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

  /// Run `op` as a traced context action — the viewer shows it as
  /// `browserContext.<method>`. No DOM snapshots: a context call belongs
  /// to no page, which is also why Playwright's viewer shows these
  /// without before/after frames.
  ///
  /// Boxed rather than `async fn` so the wrapped calls do not carry the
  /// span machinery inside their own state machines.
  pub(crate) fn traced<'a, T: 'a>(
    &'a self,
    method: &'a str,
    params: serde_json::Value,
    op: impl std::future::Future<Output = Result<T>> + 'a,
  ) -> impl std::future::Future<Output = Result<T>> + 'a {
    Box::pin(async move {
      let composite = self.composite();
      let span = crate::trace::begin_action(Some(&composite), "BrowserContext", method, None, params);
      let span = crate::trace::open_action(span).await;
      let result = crate::trace::within_action(crate::abort::guard(op)).await;
      if let Some(span) = span {
        span.finish(result.as_ref().err());
      }
      result
    })
  }

  /// Shared per-state HAR-recorder registry (used by [`crate::tracing::Tracing`]).
  pub(crate) async fn har_recorders(
    &self,
  ) -> Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::tracing::HarRecorder>>> {
    Arc::clone(&self.state.read().await.har_recorders)
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
      .map(|c| Arc::clone(&c.network_log))
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

  /// `testIdAttribute` for this context — what `getByTestId` builds its
  /// selector from. Playwright's `selectors.setTestIdAttribute` sets the
  /// process default; a context (a project's `use` bag) overrides it,
  /// which is what lets two projects in one process disagree.
  ///
  /// Borrowed when nothing overrides the default, so the common case
  /// costs no allocation — `getByTestId` asks on every call.
  #[must_use]
  pub fn test_id_attribute(&self) -> std::borrow::Cow<'static, str> {
    self
      .test_id_attribute
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
      .map_or_else(crate::selectors::default_test_id_attribute, std::borrow::Cow::Owned)
  }

  /// Override [`Self::test_id_attribute`] for this context alone.
  pub fn set_test_id_attribute(&self, name: impl Into<String>) {
    *self
      .test_id_attribute
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(name.into());
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
    let pages = Box::pin(self.discover_pages()).await?;
    for page in pages {
      Box::pin(page.apply_context_options(&updated)).await?;
    }
    Ok(())
  }

  /// Playwright: `browserContext.grantPermissions(permissions, options?:
  /// { origin? })`. Adds to what the origin (or, without one, every
  /// origin) was already granted, for open pages and future ones.
  ///
  /// # Errors
  ///
  /// An unparseable origin, a permission the engine does not know, or a
  /// failure applying the grant to an open page.
  pub async fn grant_permissions(&self, permissions: &[String], origin: Option<&str>) -> Result<()> {
    let origin = origin.map(crate::permissions::origin_of).transpose()?;
    let added = permissions.to_vec();
    let params = serde_json::json!({ "permissions": permissions, "origin": origin });
    self
      .traced(
        "grantPermissions",
        params,
        self.mutate_options(move |o| {
          let granted = match origin {
            None => o.permissions.get_or_insert_with(Vec::new),
            Some(origin) => {
              let index = o
                .origin_permissions
                .iter()
                .position(|(known, _)| *known == origin)
                .unwrap_or_else(|| {
                  o.origin_permissions.push((origin, Vec::new()));
                  o.origin_permissions.len() - 1
                });
              &mut o.origin_permissions[index].1
            },
          };
          for permission in added {
            if !granted.contains(&permission) {
              granted.push(permission);
            }
          }
        }),
      )
      .await
  }

  /// Clear all granted permissions.
  ///
  /// # Errors
  ///
  /// Returns an error if resetting permissions fails.
  pub async fn clear_permissions(&self) -> Result<()> {
    self
      .traced("clearPermissions", serde_json::json!({}), async {
        // Reset via the backend's `Browser.resetPermissions` on every
        // page, then drop the list from the options bag.
        let pages = self.discover_pages().await?;
        for page in &pages {
          page.inner().reset_permissions().await?;
        }
        self
          .mutate_options(|o| {
            o.permissions = None;
            o.origin_permissions.clear();
          })
          .await
      })
      .await
  }

  /// Close this context (remove from `BrowserState`). Mirrors
  /// Playwright's `context.close({ reason })` — chain `.reason(...)` to
  /// record why; the reason surfaces in errors from operations
  /// interrupted by the close.
  ///
  /// # Errors
  ///
  /// Returns an error if state lock acquisition fails.
  #[track_caller]
  pub fn close(&self) -> crate::action::Action<'static, crate::options::ContextCloseOptions, ()> {
    let ctx = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { ctx.close_impl(Some(opts)).await }))
  }

  pub(crate) async fn close_impl(&self, opts: Option<crate::options::ContextCloseOptions>) -> Result<()> {
    let first_close = !self.closed.swap(true, std::sync::atomic::Ordering::SeqCst);
    // Flush `routeFromHAR(update: true)` recorders while the network log
    // is still reachable — Playwright writes the updated HAR on context
    // close (`browserContext.ts` exports every live HAR recorder).
    self.flush_har_updates().await?;
    let (persistent, detached) = {
      let mut state = self.state.write().await;
      if let Some(reason) = opts.and_then(|o| o.reason) {
        state.set_close_reason(reason);
      }
      let persistent = state.persistent_context;
      let detached = state.detach_context(&self.name).await;
      if let Some(browser) = &self.browser {
        browser.unregister_context(&self.name);
      }
      (persistent, detached)
    };
    // Run protocol teardown (screen recording flush, page disposal, context
    // recycling / disposal) off the global `BrowserState` write lock so
    // concurrent sessions never stall waiting on another context's close.
    if let Some(detached) = detached {
      detached.finish().await;
    }
    if persistent {
      // Persistent-context launch contract: closing the context closes
      // the underlying browser too. Playwright:
      // `/tmp/playwright/packages/playwright-core/types/types.d.ts:15199`.
      let mut state = self.state.write().await;
      state.shutdown().await?;
    }
    if first_close {
      self.events.emit(crate::events::ContextEvent::Close(self.clone()));
    }
    Ok(())
  }

  /// The context went away with its browser: `close` fires once, as it
  /// does for [`Self::close`].
  pub(crate) fn did_close(&self) {
    if !self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
      self.events.emit(crate::events::ContextEvent::Close(self.clone()));
    }
  }

  /// Apply `storageState` to the context, once.
  ///
  /// Cookies and localStorage go on with the first page; later pages in
  /// the same context inherit them (cookies are context-scoped,
  /// localStorage persists per origin). Mirrors Playwright's
  /// `server/browserContext.ts::setStorageState`.
  async fn hydrate_storage_state(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    let Some(storage) = opts.storage_state.as_ref() else {
      return Ok(());
    };
    let should_hydrate = {
      let state = self.state.read().await;
      state.claim_storage_state_hydration(&self.key.to_composite())
    };
    if !should_hydrate {
      return Ok(());
    }
    Box::pin(self.restore_storage_state(&storage_state_value(storage)?, false)).await
  }

  /// Whether a path on this machine names the same file to the
  /// context's browser.
  pub(crate) async fn shares_host_files(&self) -> bool {
    let state = self.state.read().await;
    state
      .page_open_plan(&self.key)
      .ok()
      .is_none_or(|plan| plan.browser.shares_host_files())
  }

  /// The lock a page of this context holds while it is captured, when
  /// its browser draws only the tab in front.
  pub(crate) async fn front_tab_turn(&self) -> Option<Arc<tokio::sync::Mutex<()>>> {
    let state = self.state.read().await;
    state.page_open_plan(&self.key).ok()?.browser.front_tab_turn()
  }

  /// Whether the context has opened in the browser, which it does with
  /// its first page.
  async fn is_open(&self) -> bool {
    self.state.read().await.context(&self.name).is_ok()
  }

  /// Seed the `WebAuthn` credentials of the `storageState` the context was
  /// created with: Playwright restores them with the context, before any
  /// page, so `credentials.get()` sees them at once.
  pub(crate) async fn restore_storage_state_credentials(
    &self,
    opts: &crate::options::BrowserContextOptions,
  ) -> Result<()> {
    let Some(storage) = opts.storage_state.as_ref() else {
      return Ok(());
    };
    let credentials = state_credentials(&storage_state_value(storage)?)?;
    if credentials.is_empty() {
      return Ok(());
    }
    self.credentials().restore(credentials).await
  }

  /// Register the context's `recordHar` recorder, once.
  ///
  /// Nothing is written here: the archive lands when the context closes,
  /// through the same registry `routeFromHAR(update: true)` uses and the
  /// same flush in `close_impl`. Once, on the first page, because the
  /// option bag is applied per page while the archive spans the context.
  async fn install_record_har(&self, opts: &crate::options::BrowserContextOptions) {
    let Some(har) = opts.record_har.as_ref() else {
      return;
    };
    let composite = self.key.to_composite();
    let should_install = {
      let state = self.state.read().await;
      state.claim_record_har(&composite)
    };
    if !should_install {
      return;
    }
    let recorder = crate::tracing::recorder_for_record_har(har);
    let registry = Arc::clone(&self.state.read().await.context_har_updates);
    registry
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .entry(composite)
      .or_default()
      .push(recorder);
  }

  /// Write every `routeFromHAR(update: true)` recording registered on
  /// this context. Consumes the registry entries so a double close does
  /// not rewrite the files.
  pub(crate) async fn flush_har_updates(&self) -> Result<()> {
    let recorders = {
      let registry = Arc::clone(&self.state.read().await.context_har_updates);
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
    let pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    let titles = crate::tracing::collect_page_titles(&pages).await;
    for recorder in recorders {
      let slice = requests.get(recorder.start_len..).unwrap_or(&[]);
      match recorder.page_filter.as_deref() {
        Some(page_guid) => {
          let filtered: Vec<crate::network::Request> = slice
            .iter()
            .filter(|r| r.page_guid() == Some(page_guid))
            .cloned()
            .collect();
          crate::tracing::flush_recorder(&recorder, &filtered, &titles).await?;
        },
        None => crate::tracing::flush_recorder(&recorder, slice, &titles).await?,
      }
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

  /// Write this context's recordings into `dir` rather than the
  /// browser's `tracesDir`.
  ///
  /// A test runner sets it per test so each worker's live traces land in
  /// the directory that worker's artifacts are served from, even though
  /// every worker shares one browser.
  pub async fn set_traces_dir(&self, dir: std::path::PathBuf) {
    let composite = self.key.to_composite();
    self.state.read().await.set_context_traces_dir(&composite, dir);
  }
}

/// Stamp the context's composite key onto the [`crate::events::BindingSource`]
/// the backend built for each call. The backend fills `page` / `frame`
/// (the real calling frame — an iframe caller surfaces its own frame id);
/// only the context key is unknown at that layer.
pub(super) fn bind_source(
  binding: crate::events::ExposedBinding,
  context_key: String,
) -> crate::events::ExposedBinding {
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
/// Two fields on the bag still do nothing: `record_har` and
/// `strict_selectors`. Neither is read anywhere outside
/// `crate::options`, and both bindings hard-code `record_har: None`, so
/// a caller passing either gets silence rather than an error. Every
/// other field IS applied, each with a test in
/// `tests/e2e/context-options.test.ts` that observes an effect only the
/// option produces.
///
/// That list was much longer and most of it was wrong: it had claimed
/// `proxy`, `storage_state`, `screen`, `base_url`, `service_workers`,
/// `ignore_https_errors`, `accept_downloads` and `bypass_csp` were all
/// no-ops long after they were implemented. Check before adding to it.
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

/// [`apply_context_options`], or for a context without options the
/// media emulation every page gets regardless.
pub(super) async fn apply_context_options_or_defaults(
  page: &Arc<Page>,
  opts: Option<&crate::options::BrowserContextOptions>,
) -> Result<()> {
  match opts {
    Some(opts) => apply_context_options(page, opts).await,
    None => {
      page
        .inner()
        .emulate_context_media(crate::options::EmulateMediaOptions::default())
        .await
    },
  }
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
    let seen_cb = Arc::clone(&seen);
    let inner: crate::events::ExposedFn = Arc::new(move |args: Vec<serde_json::Value>| {
      seen_cb.store(args.len(), Ordering::SeqCst);
      Box::pin(
        async move { Ok(serde_json::json!(args.iter().filter_map(serde_json::Value::as_i64).sum::<i64>()).into()) },
      )
    });
    let binding: crate::events::ExposedBinding = Arc::new(move |_source, args| inner(args));
    let source = crate::events::BindingSource {
      context: "inst:ctx".into(),
      page: "frame-1".into(),
      frame: "frame-1".into(),
      live: None,
    };
    let out = binding(source, vec![serde_json::json!(20), serde_json::json!(22)]).await;
    assert_eq!(seen.load(Ordering::SeqCst), 2);
    assert!(matches!(out, Ok(crate::events::BindingReturn::Json(sum)) if sum == serde_json::json!(42)));
  }

  #[tokio::test]
  async fn disposable_runs_once() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);
    let d = crate::disposable::Disposable::new(move || {
      let c = c;
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

mod bindings;
mod events;
mod popups;
mod storage;

pub(crate) use popups::spawn_popup_pump;
use popups::{PageRegistries, effective_viewport, start_video_recording};
use storage::{state_credentials, storage_state_value};
