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
    let page = self.active_page().ok_or("No page in context")?;
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
  default_timeout_ms: u64,
  /// Default navigation timeout in this context (ms). 0 = no override.
  default_navigation_timeout_ms: u64,
  /// Context-scoped event emitter. Shared across every `ContextRef`
  /// clone with the same composite session key via the per-state
  /// [`BrowserState::get_or_create_context_events`] registry — without
  /// this, `browser.defaultContext()` called twice would hand out two
  /// separate emitters and `context.on('weberror', cb)` would silently
  /// miss events dispatched via the per-page bridge installed on a
  /// page created through a different `ContextRef` instance.
  events: crate::events::ContextEventEmitter,
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
    let events = match state.try_read() {
      Ok(s) => s.get_or_create_context_events(&key.to_composite()),
      Err(_) => crate::events::ContextEventEmitter::new(),
    };
    Self {
      state,
      name: Arc::from(name),
      key,
      default_timeout_ms: 0,
      default_navigation_timeout_ms: 0,
      events,
    }
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
      // Per-context `proxy` flows through
      // `Target.createBrowserContext({ proxyServer, proxyBypassList })`
      // on CDP (`crBrowser.ts::doCreateNewContext`) and
      // `browser.createUserContext({ proxy })` on BiDi
      // (`bidiBrowser.ts::doCreateNewContext`).
      let proxy = ctx_opts.as_ref().and_then(|o| o.proxy.as_ref());
      let ctx_id = plan.browser.new_context(proxy).await?;
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

    Ok(page)
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

  /// Get all cookies in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or cookie retrieval fails.
  pub async fn cookies(&self) -> Result<Vec<CookieData>> {
    let state = self.state.read().await;
    let ctx = state.context(&self.name)?;
    ctx.cookies().await
  }

  /// Add cookies to this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or setting cookies fails.
  pub async fn add_cookies(&self, cookies: Vec<CookieData>) -> Result<()> {
    let state = self.state.read().await;
    let ctx = state.context(&self.name)?;
    ctx.add_cookies(cookies).await
  }

  /// Clear all cookies in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or clearing cookies fails.
  pub async fn clear_cookies(&self) -> Result<()> {
    let state = self.state.read().await;
    let ctx = state.context(&self.name)?;
    ctx.clear_cookies().await
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
    let state = self.state.read().await;
    let ctx = state.context(&self.name)?;
    let cookies = ctx.cookies().await?;
    if let Some(page) = ctx.active_page() {
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

  /// Set the default timeout for actions in this context (ms).
  pub fn set_default_timeout(&mut self, ms: u64) {
    self.default_timeout_ms = ms;
  }

  /// Set the default navigation timeout for this context (ms).
  pub fn set_default_navigation_timeout(&mut self, ms: u64) {
    self.default_navigation_timeout_ms = ms;
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

  /// Close this context (remove from `BrowserState`).
  ///
  /// # Errors
  ///
  /// Returns an error if state lock acquisition fails.
  pub async fn close(&self) -> Result<()> {
    let mut state = self.state.write().await;
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
  /// Returns identifiers for each existing page — the same injection also
  /// lands on pages created later in the context.
  ///
  /// # Errors
  ///
  /// Returns an error if `evaluation_script` lowering fails, the context
  /// does not exist, or script injection fails on any page.
  pub async fn add_init_script(
    &self,
    script: crate::options::InitScriptSource,
    arg: Option<serde_json::Value>,
  ) -> Result<Vec<String>> {
    let source = crate::options::evaluation_script(script, arg.as_ref())?;
    let state = self.state.read().await;
    let ctx = state.context(&self.name)?;
    let mut ids = Vec::new();
    for page in &ctx.pages {
      ids.push(page.add_init_script(&source).await?);
    }
    Ok(ids)
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

  /// Register a route handler for all pages in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or route registration fails.
  pub async fn route(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::route::RouteHandler,
  ) -> Result<()> {
    let state = self.state.read().await;
    let ctx = state.context(&self.name)?;
    for page in &ctx.pages {
      page.route(matcher.clone(), handler.clone()).await?;
    }
    Ok(())
  }

  /// Remove route handlers matching the given matcher from all pages.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or route removal fails.
  pub async fn unroute(&self, matcher: &crate::url_matcher::UrlMatcher) -> Result<()> {
    let state = self.state.read().await;
    let ctx = state.context(&self.name)?;
    for page in &ctx.pages {
      page.unroute(matcher).await?;
    }
    Ok(())
  }
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
/// `http_credentials`, `accept_downloads`, `ignore_https_errors`,
/// `strict_selectors` beyond storage, `bypass_csp`. Each gets a
/// dedicated implementation when the supporting infrastructure lands.
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
/// On backends that do not support screencast (stock `WKWebView`,
/// surfaced through `AnyPage::start_screencast`'s typed error), the
/// video sink is populated with the error text so the Playwright
/// contract — `page.video()` returns a handle; the handle's methods
/// reject with a clear reason — is preserved.
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
