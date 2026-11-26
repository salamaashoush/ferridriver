//! Page events, network interception, picker, exposed functions, dialogs, file choosers, downloads, init scripts.

use super::{web_storage_global, web_storage_js_string};
use std::sync::Arc;

use crate::backend::AnyPage;
use crate::error::Result;
use crate::events::{EventEmitter, PageEvent};
use crate::frame::Frame;
use crate::frame_cache::FrameCache;
use crate::locator::Locator;
use tokio::sync::Mutex as AsyncMutex;

use super::{HighlightEntry, Page, Unregister};

impl Page {
  // ── Events ────────────────────────────────────────────────────────────

  /// Get the event emitter for subscribing to page events.
  #[must_use]
  pub fn events(&self) -> &EventEmitter {
    self.inner.events()
  }

  /// Subscribe to page events. Calls the callback for each matching event.
  /// Returns a `ListenerId` for later removal with `off()`.
  ///
  /// ```ignore
  /// let id = page.on("response", Arc::new(|event| {
  ///     if let PageEvent::Response(r) = event {
  ///         println!("Response: {} {}", r.status, r.url);
  ///     }
  /// }));
  /// ```
  pub fn on(&self, event_name: &str, callback: crate::events::EventCallback) -> crate::events::ListenerId {
    self.lazy_enable_for_event(event_name);
    self.inner.events().on(event_name, callback)
  }

  /// Subscribe to a single event, then auto-remove the listener.
  pub fn once(&self, event_name: &str, callback: crate::events::EventCallback) -> crate::events::ListenerId {
    self.lazy_enable_for_event(event_name);
    self.inner.events().once(event_name, callback)
  }

  /// Some events depend on a backend command being fired (file
  /// chooser interception, download behaviour). When the user
  /// expresses interest, fire-and-forget the command in the
  /// background — best-effort; failure is silently swallowed and
  /// would surface via the user not getting the event. Mirrors
  /// Playwright's `_updateFileChooserInterception(false)` pattern
  /// where the command is async but fire-and-forget around listener
  /// registration (`crPage.ts:199`).
  fn lazy_enable_for_event(&self, event_name: &str) {
    let needs_filechooser = event_name == "filechooser";
    let needs_download = event_name == "download";
    if !needs_filechooser && !needs_download {
      return;
    }
    let inner_for_task: AnyPage = self.inner.clone();
    self.inner.events().spawn(async move {
      if needs_filechooser {
        let _ = inner_for_task.enable_file_chooser_intercept().await;
      }
      if needs_download {
        let _ = inner_for_task.enable_download_behavior().await;
      }
    });
  }

  /// Playwright: `page.coverage`. Chromium only, as in Playwright.
  #[must_use]
  pub fn coverage(self: &Arc<Self>) -> crate::coverage::Coverage {
    crate::coverage::Coverage::new(Arc::clone(self))
  }

  pub(crate) fn coverage_state(&self) -> &AsyncMutex<crate::coverage::CoverageState> {
    &self.coverage_state
  }

  /// Playwright: `page.workers(): Worker[]`. The dedicated workers the
  /// page has running, in the order they started. Classic `WebDriver`
  /// reports none.
  #[must_use]
  pub fn workers(&self) -> Vec<crate::worker::Worker> {
    match self.inner.observed().lock() {
      Ok(g) => g.workers(),
      Err(poisoned) => poisoned.into_inner().workers(),
    }
  }

  /// Playwright: `page.requests(): Promise<Request[]>`. The last 100
  /// requests the page issued, oldest first; navigation does not reset it.
  #[must_use]
  pub fn requests(&self) -> Vec<crate::network::Request> {
    match self.inner.observed().lock() {
      Ok(g) => g.requests(),
      Err(poisoned) => poisoned.into_inner().requests(),
    }
  }

  /// Console messages retained for this page (cap 200, like
  /// Playwright). Playwright: `page.consoleMessages(options?: { filter?:
  /// 'all' | 'since-navigation' }): Promise<ConsoleMessage[]>` — the
  /// default filter only returns messages logged after the last
  /// main-frame navigation.
  #[must_use]
  pub fn console_messages(
    &self,
    filter: crate::observed::ObservedFilter,
  ) -> Vec<crate::console_message::ConsoleMessage> {
    match self.inner.observed().lock() {
      Ok(g) => g.console_messages(filter),
      Err(poisoned) => poisoned.into_inner().console_messages(filter),
    }
  }

  /// Drop the retained console-message history.
  /// Playwright: `page.clearConsoleMessages(): Promise<void>`.
  pub fn clear_console_messages(&self) {
    match self.inner.observed().lock() {
      Ok(mut g) => g.clear_console(),
      Err(poisoned) => poisoned.into_inner().clear_console(),
    }
  }

  /// Uncaught page exceptions retained for this page (cap 200).
  /// Playwright: `page.pageErrors(options?: { filter?: 'all' |
  /// 'since-navigation' }): Promise<Error[]>`.
  #[must_use]
  pub fn page_errors(&self, filter: crate::observed::ObservedFilter) -> Vec<crate::web_error::WebError> {
    match self.inner.observed().lock() {
      Ok(g) => g.page_errors(filter),
      Err(poisoned) => poisoned.into_inner().page_errors(filter),
    }
  }

  /// Drop the retained page-error history.
  /// Playwright: `page.clearPageErrors(): Promise<void>`.
  pub fn clear_page_errors(&self) {
    match self.inner.observed().lock() {
      Ok(mut g) => g.clear_errors(),
      Err(poisoned) => poisoned.into_inner().clear_errors(),
    }
  }

  /// Read every entry of the page's `localStorage` / `sessionStorage`
  /// for the current origin. Playwright: `webStorage.items()`
  /// (`server/page.ts::webStorageItems`) — evaluated against the live
  /// storage object on the main frame.
  ///
  /// # Errors
  ///
  /// Page-side exception or protocol failure (e.g. storage access
  /// denied on an opaque origin).
  pub async fn web_storage_items(
    self: &Arc<Self>,
    kind: crate::options::WebStorageKind,
  ) -> Result<Vec<crate::options::NameValue>> {
    let storage = web_storage_global(kind);
    let expr = format!(
      "(() => {{ const result = []; for (let i = 0; i < {storage}.length; i++) {{ const name = {storage}.key(i); if (name !== null) result.push({{ name, value: {storage}.getItem(name) ?? '' }}); }} return result; }})()"
    );
    let value = self
      .evaluate(&expr, crate::protocol::SerializedArgument::default(), Some(false))
      .await?;
    let json = value.to_json_like().unwrap_or(serde_json::Value::Array(Vec::new()));
    Ok(serde_json::from_value(json).unwrap_or_default())
  }

  /// Read a single entry; `None` when the key is absent. Playwright:
  /// `webStorage.getItem(name)`.
  ///
  /// # Errors
  ///
  /// Page-side exception or protocol failure.
  pub async fn web_storage_get_item(
    self: &Arc<Self>,
    kind: crate::options::WebStorageKind,
    name: &str,
  ) -> Result<Option<String>> {
    let storage = web_storage_global(kind);
    let expr = format!("{storage}.getItem({})", web_storage_js_string(name));
    let value = self
      .evaluate(&expr, crate::protocol::SerializedArgument::default(), Some(false))
      .await?;
    Ok(match value.to_json_like() {
      Some(serde_json::Value::String(s)) => Some(s),
      _ => None,
    })
  }

  /// Write a single entry. Playwright: `webStorage.setItem(name, value)`.
  ///
  /// # Errors
  ///
  /// Page-side exception (e.g. quota exceeded) or protocol failure.
  pub async fn web_storage_set_item(
    self: &Arc<Self>,
    kind: crate::options::WebStorageKind,
    name: &str,
    value: &str,
  ) -> Result<()> {
    let storage = web_storage_global(kind);
    let expr = format!(
      "{storage}.setItem({}, {})",
      web_storage_js_string(name),
      web_storage_js_string(value)
    );
    self
      .evaluate(&expr, crate::protocol::SerializedArgument::default(), Some(false))
      .await?;
    Ok(())
  }

  /// Remove a single entry. Playwright: `webStorage.removeItem(name)`.
  ///
  /// # Errors
  ///
  /// Page-side exception or protocol failure.
  pub async fn web_storage_remove_item(
    self: &Arc<Self>,
    kind: crate::options::WebStorageKind,
    name: &str,
  ) -> Result<()> {
    let storage = web_storage_global(kind);
    let expr = format!("{storage}.removeItem({})", web_storage_js_string(name));
    self
      .evaluate(&expr, crate::protocol::SerializedArgument::default(), Some(false))
      .await?;
    Ok(())
  }

  /// Clear all entries for the current origin. Playwright:
  /// `webStorage.clear()`.
  ///
  /// # Errors
  ///
  /// Page-side exception or protocol failure.
  pub async fn web_storage_clear(self: &Arc<Self>, kind: crate::options::WebStorageKind) -> Result<()> {
    let storage = web_storage_global(kind);
    let expr = format!("{storage}.clear()");
    self
      .evaluate(&expr, crate::protocol::SerializedArgument::default(), Some(false))
      .await?;
    Ok(())
  }

  /// Force a garbage-collection pass in the page's JS engine.
  /// Playwright: `page.requestGC(): Promise<void>`. Supported on every
  /// CDP backend (`HeapProfiler.collectGarbage`) and `WebKit`
  /// (`Heap.gc`); on `BiDi` it requires a Firefox build exposing
  /// `TestUtils.gc()` and returns `FerriError::Unsupported` otherwise
  /// (Playwright's `BiDi` backend throws the same way).
  ///
  /// # Errors
  ///
  /// Returns an error if the underlying protocol call fails or the
  /// backend cannot trigger a collection.
  pub async fn request_gc(&self) -> Result<()> {
    self.inner.request_gc().await
  }

  /// Remove an event listener by ID.
  pub fn off(&self, id: crate::events::ListenerId) {
    self.inner.events().off(id);
  }

  /// Remove all event listeners.
  pub fn remove_all_listeners(&self) {
    self.inner.events().remove_all_listeners();
  }

  /// Remove every listener registered for `event_name` (Playwright's
  /// `page.removeAllListeners(type)` with a type argument).
  pub fn remove_listeners_named(&self, event_name: &str) {
    self.inner.events().remove_listeners_named(event_name);
  }

  /// Node's `prependListener`: run before every listener already
  /// attached to `event_name`.
  pub fn prepend_listener(
    &self,
    event_name: &str,
    callback: crate::events::EventCallback,
  ) -> crate::events::ListenerId {
    self.lazy_enable_for_event(event_name);
    self.inner.events().prepend(event_name, callback)
  }

  /// Node's `prependOnceListener`.
  pub fn prepend_once_listener(
    &self,
    event_name: &str,
    callback: crate::events::EventCallback,
  ) -> crate::events::ListenerId {
    self.lazy_enable_for_event(event_name);
    self.inner.events().prepend_once(event_name, callback)
  }

  /// Node's `listenerCount(type)`.
  #[must_use]
  pub fn listener_count(&self, event_name: &str) -> usize {
    self.inner.events().listener_count(event_name)
  }

  /// Node's `eventNames()` — every event with a listener attached.
  #[must_use]
  pub fn event_names(&self) -> Vec<String> {
    self.inner.events().event_names()
  }

  /// The listener ids registered for `event_name`, in dispatch order.
  /// A binding layer maps them back to the callbacks it persisted.
  #[must_use]
  pub fn listener_ids_named(&self, event_name: &str) -> Vec<crate::events::ListenerId> {
    self.inner.events().listener_ids_named(event_name)
  }

  /// Node's `setMaxListeners(n)`; `0` disables the leak warning.
  pub fn set_max_listeners(&self, max: usize) {
    self.inner.events().set_max_listeners(max);
  }

  /// Node's `getMaxListeners()`.
  #[must_use]
  pub fn max_listeners(&self) -> usize {
    self.inner.events().max_listeners()
  }

  /// Live [`Frame`] handle for a backend frame id. Used by the binding
  /// layers to lift `frameattached` / `framenavigated` / `framedetached`
  /// event payloads into the `Frame` object Playwright hands to
  /// listeners.
  #[must_use]
  pub fn frame_for_id(self: &Arc<Self>, frame_id: &str) -> Frame {
    Frame::new(Arc::clone(self), Arc::from(frame_id))
  }

  /// Stable identity of the underlying backend page — equal for every
  /// wrapper minted over the same browser page, distinct across pages.
  /// Used by binding layers for per-page bookkeeping (e.g. releasing a
  /// page's persisted event listeners when it closes).
  #[must_use]
  pub fn backend_page_id(&self) -> usize {
    std::sync::Arc::as_ptr(self.inner.frame_cache()).cast::<()>() as usize
  }

  /// Start listening for a navigation event. Call BEFORE the action that triggers navigation.
  /// Returns a future that resolves when navigation completes.
  ///
  /// ```ignore
  /// let nav = page.expect_navigation(None);
  /// page.click("#link").await?;
  /// nav.await?; // resolves when navigation completes
  /// ```
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation event does not occur within the timeout.
  pub fn expect_navigation(&self, timeout_ms: Option<u64>) -> impl std::future::Future<Output = Result<()>> + use<> {
    let timeout = timeout_ms.unwrap_or_else(|| self.default_timeout());
    let loaded = self
      .inner
      .events()
      .wait_for(|e| matches!(e, PageEvent::Load | PageEvent::DomContentLoaded), timeout);
    async move {
      loaded.await?;
      Ok(())
    }
  }

  /// Wait for a specific event (by name) with timeout. Listens from the
  /// call, so an event the caller triggers before awaiting is not missed.
  ///
  /// # Errors
  ///
  /// Returns an error if the event does not occur within the timeout.
  pub fn wait_for_event(
    &self,
    event_name: &str,
    timeout_ms: Option<u64>,
  ) -> impl std::future::Future<Output = Result<PageEvent>> + Send + use<> {
    self
      .inner
      .events()
      .wait_for_event(event_name, timeout_ms.unwrap_or_else(|| self.default_timeout()))
  }

  /// Arm a waiter for `event_name`, run `action`, then resolve with the
  /// event and the action's result. The subscription is registered
  /// before `action` runs, so an event fired by the action can't be
  /// missed — the wait-for-event + trigger pattern without the race:
  ///
  /// ```ignore
  /// let (event, ()) = page.expect_event("download", || page.click("#export")).await?;
  /// ```
  ///
  /// Typed wrappers exist for the common events ([`Self::expect_download`],
  /// [`Self::expect_dialog`], [`Self::expect_console`], ...).
  ///
  /// # Errors
  ///
  /// Returns the action's error if it fails, or a timeout error when the
  /// event does not arrive within the page's default timeout.
  pub async fn expect_event<F, Fut, R>(&self, event_name: &str, action: F) -> Result<(PageEvent, R)>
  where
    F: FnOnce() -> Fut,
    Fut: std::future::IntoFuture<Output = Result<R>>,
  {
    // Subscribes synchronously at the call — armed before the action.
    let waiter = self.inner.events().wait_for_event(event_name, self.default_timeout());
    let result = action().await?;
    let event = waiter.await?;
    Ok((event, result))
  }

  /// [`Self::expect_event`] for `download`, yielding the [`crate::download::Download`].
  ///
  /// # Errors
  ///
  /// See [`Self::expect_event`].
  pub async fn expect_download<F, Fut, R>(&self, action: F) -> Result<(crate::download::Download, R)>
  where
    F: FnOnce() -> Fut,
    Fut: std::future::IntoFuture<Output = Result<R>>,
  {
    match self.expect_event("download", action).await? {
      (PageEvent::Download(download), r) => Ok((download, r)),
      _ => Err(crate::error::FerriError::Backend(
        "expect_download: waiter resolved with a non-download event".into(),
      )),
    }
  }

  /// [`Self::expect_event`] for `popup`, yielding the page `action` opened.
  ///
  /// # Errors
  ///
  /// See [`Self::expect_event`].
  pub async fn expect_popup<F, Fut, R>(&self, action: F) -> Result<(Arc<Page>, R)>
  where
    F: FnOnce() -> Fut,
    Fut: std::future::IntoFuture<Output = Result<R>>,
  {
    match self.expect_event("popup", action).await? {
      (PageEvent::Popup(popup), r) => Ok((popup, r)),
      _ => Err(crate::error::FerriError::Backend(
        "expect_popup: waiter resolved with a non-popup event".into(),
      )),
    }
  }

  /// [`Self::expect_event`] for `dialog`, yielding the live [`crate::dialog::Dialog`].
  ///
  /// # Errors
  ///
  /// See [`Self::expect_event`].
  pub async fn expect_dialog<F, Fut, R>(&self, action: F) -> Result<(crate::dialog::Dialog, R)>
  where
    F: FnOnce() -> Fut,
    Fut: std::future::IntoFuture<Output = Result<R>>,
  {
    match self.expect_event("dialog", action).await? {
      (PageEvent::Dialog(dialog), r) => Ok((dialog, r)),
      _ => Err(crate::error::FerriError::Backend(
        "expect_dialog: waiter resolved with a non-dialog event".into(),
      )),
    }
  }

  /// [`Self::expect_event`] for `console`, yielding the [`crate::console_message::ConsoleMessage`].
  ///
  /// # Errors
  ///
  /// See [`Self::expect_event`].
  pub async fn expect_console<F, Fut, R>(&self, action: F) -> Result<(crate::console_message::ConsoleMessage, R)>
  where
    F: FnOnce() -> Fut,
    Fut: std::future::IntoFuture<Output = Result<R>>,
  {
    match self.expect_event("console", action).await? {
      (PageEvent::Console(message), r) => Ok((message, r)),
      _ => Err(crate::error::FerriError::Backend(
        "expect_console: waiter resolved with a non-console event".into(),
      )),
    }
  }

  /// [`Self::expect_event`] for `filechooser`, yielding the live
  /// [`crate::file_chooser::FileChooser`].
  ///
  /// # Errors
  ///
  /// See [`Self::expect_event`].
  pub async fn expect_file_chooser<F, Fut, R>(&self, action: F) -> Result<(crate::file_chooser::FileChooser, R)>
  where
    F: FnOnce() -> Fut,
    Fut: std::future::IntoFuture<Output = Result<R>>,
  {
    match self.expect_event("filechooser", action).await? {
      (PageEvent::FileChooser(chooser), r) => Ok((chooser, r)),
      _ => Err(crate::error::FerriError::Backend(
        "expect_file_chooser: waiter resolved with a non-filechooser event".into(),
      )),
    }
  }

  /// [`Self::expect_event`] for `request`, yielding the [`crate::network::Request`].
  ///
  /// # Errors
  ///
  /// See [`Self::expect_event`].
  pub async fn expect_request<F, Fut, R>(&self, action: F) -> Result<(crate::network::Request, R)>
  where
    F: FnOnce() -> Fut,
    Fut: std::future::IntoFuture<Output = Result<R>>,
  {
    match self.expect_event("request", action).await? {
      (PageEvent::Request(request), r) => Ok((request, r)),
      _ => Err(crate::error::FerriError::Backend(
        "expect_request: waiter resolved with a non-request event".into(),
      )),
    }
  }

  /// [`Self::expect_event`] for `response`, yielding the [`crate::network::Response`].
  ///
  /// # Errors
  ///
  /// See [`Self::expect_event`].
  pub async fn expect_response<F, Fut, R>(&self, action: F) -> Result<(crate::network::Response, R)>
  where
    F: FnOnce() -> Fut,
    Fut: std::future::IntoFuture<Output = Result<R>>,
  {
    match self.expect_event("response", action).await? {
      (PageEvent::Response(response), r) => Ok((response, r)),
      _ => Err(crate::error::FerriError::Backend(
        "expect_response: waiter resolved with a non-response event".into(),
      )),
    }
  }

  /// [`Self::expect_event`] for `websocket`, yielding the [`crate::network::WebSocket`].
  ///
  /// # Errors
  ///
  /// See [`Self::expect_event`].
  pub async fn expect_websocket<F, Fut, R>(&self, action: F) -> Result<(crate::network::WebSocket, R)>
  where
    F: FnOnce() -> Fut,
    Fut: std::future::IntoFuture<Output = Result<R>>,
  {
    match self.expect_event("websocket", action).await? {
      (PageEvent::WebSocket(ws), r) => Ok((ws, r)),
      _ => Err(crate::error::FerriError::Backend(
        "expect_websocket: waiter resolved with a non-websocket event".into(),
      )),
    }
  }

  /// [`Self::expect_event`] for `pageerror`, yielding the [`crate::web_error::WebError`].
  ///
  /// # Errors
  ///
  /// See [`Self::expect_event`].
  pub async fn expect_page_error<F, Fut, R>(&self, action: F) -> Result<(crate::web_error::WebError, R)>
  where
    F: FnOnce() -> Fut,
    Fut: std::future::IntoFuture<Output = Result<R>>,
  {
    match self.expect_event("pageerror", action).await? {
      (PageEvent::PageError(error), r) => Ok((error, r)),
      _ => Err(crate::error::FerriError::Backend(
        "expect_page_error: waiter resolved with a non-pageerror event".into(),
      )),
    }
  }

  /// Wait for a network request matching a URL pattern. Listens from the
  /// call, so a request the caller triggers before awaiting is not missed.
  ///
  /// # Errors
  ///
  /// Returns an error if no matching request occurs within the timeout.
  pub fn wait_for_request(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    timeout_ms: Option<u64>,
  ) -> impl std::future::Future<Output = Result<crate::network::Request>> + Send + use<> {
    let event = self.inner.events().wait_for(
      move |e| matches!(e, PageEvent::Request(r) if matcher.matches(r.url())),
      timeout_ms.unwrap_or_else(|| self.default_timeout()),
    );
    async move {
      match event.await? {
        PageEvent::Request(r) => Ok(r),
        _ => Err(crate::error::FerriError::backend(
          "event wait returned unexpected event type",
        )),
      }
    }
  }

  /// Wait for a network response matching a URL pattern. Listens from the
  /// call, so a response the caller triggers before awaiting is not missed.
  ///
  /// # Errors
  ///
  /// Returns an error if no matching response occurs within the timeout.
  pub fn wait_for_response(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    timeout_ms: Option<u64>,
  ) -> impl std::future::Future<Output = Result<crate::network::Response>> + Send + use<> {
    let event = self.inner.events().wait_for(
      move |e| matches!(e, PageEvent::Response(r) if matcher.matches(r.url())),
      timeout_ms.unwrap_or_else(|| self.default_timeout()),
    );
    async move {
      match event.await? {
        PageEvent::Response(r) => Ok(r),
        _ => Err(crate::error::FerriError::backend(
          "event wait returned unexpected event type",
        )),
      }
    }
  }

  // ── Network Interception ────────────────────────────────────────────────

  /// Intercept network requests matching a [`crate::url_matcher::UrlMatcher`].
  /// The handler receives a [`crate::route::Route`] and must call exactly one
  /// of `fulfill()`, `continue_route()`, or `abort()`.
  ///
  /// ```ignore
  /// use ferridriver::route::{Route, FulfillResponse};
  /// use ferridriver::url_matcher::UrlMatcher;
  /// use std::sync::Arc;
  ///
  /// // Mock an API endpoint
  /// page.route(UrlMatcher::glob("**/api/data")?, Arc::new(|route: Route| {
  ///     route.fulfill(FulfillResponse {
  ///         status: 200,
  ///         body: b"{\"mocked\": true}".to_vec(),
  ///         content_type: Some("application/json".into()),
  ///         ..Default::default()
  ///     });
  /// }), None).await?;
  ///
  /// // Block image loading
  /// page.route(UrlMatcher::glob("**/*.{png,jpg,gif}")?, Arc::new(|route: Route| {
  ///     route.abort(Some("blockedbyclient"));
  /// }), None).await?;
  /// ```
  ///
  /// Returns a [`crate::disposable::Disposable`] whose `dispose()` reverses
  /// the registration (equivalent to calling [`Page::unroute`] with the same
  /// matcher). Mirrors Playwright `page.route(...)` which returns a
  /// `DisposableStub` (`client/page.ts:535`).
  ///
  /// # Errors
  ///
  /// Returns an error if the route interception cannot be set up.
  pub async fn route(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::route::RouteHandler,
    times: Option<u32>,
  ) -> Result<crate::disposable::Disposable> {
    self
      .inner
      .route(crate::route::RegisteredRoute::new(matcher.clone(), handler, times).with_client(self.http_client()))
      .await?;
    let inner = self.inner.clone();
    Ok(crate::disposable::Disposable::new(move || async move {
      inner.unroute(&matcher, crate::route::RouteScope::Page, None).await
    }))
  }

  /// Playwright: `page.routeFromHAR(har, options?)`. Replay recorded
  /// responses from a HAR file (plain `.har` or `.zip` archive) for
  /// matching requests, or with `update: true` record this page's
  /// traffic into the file instead (written when the owning context
  /// closes, like Playwright's `HarTracer` page-filtered recorder).
  ///
  /// # Errors
  ///
  /// Returns an error if the HAR file cannot be read/parsed, the route
  /// cannot be installed, or (`update: true`) the page is not bound to
  /// a context whose close can flush the recording.
  #[track_caller]
  pub fn route_from_har(
    &self,
    path: &std::path::Path,
  ) -> crate::action::Action<'_, crate::har::RouteFromHarOptions, ()> {
    let page = self;
    let path = path.to_path_buf();
    crate::action::Action::new(move |opts| Box::pin(async move { page.route_from_har_impl(&path, opts).await }))
  }

  /// Implementation of [`Self::route_from_har`].
  pub(crate) async fn route_from_har_impl(
    &self,
    path: &std::path::Path,
    options: crate::har::RouteFromHarOptions,
  ) -> Result<()> {
    if options.update {
      // Same registry + flush point as the context-scoped recorder
      // (context close), narrowed to this page's traffic via the
      // capture-time `page_guid` stamped on every logged request.
      let Some(ctx) = self.context() else {
        return Err(crate::error::FerriError::unsupported(
          "page.routeFromHAR({ update: true }) requires a page opened through a context",
        ));
      };
      return ctx
        .register_har_update_recorder(path, &options, Some(self.inner.page_guid()))
        .await;
    }
    let handler = crate::har::route_handler_from_file(path, options.not_found)?;
    let matcher = options.url.unwrap_or_else(crate::url_matcher::UrlMatcher::any);
    self
      .inner
      .route(crate::route::RegisteredRoute::new(matcher, handler, None))
      .await
  }

  /// Remove all route handlers whose matcher is
  /// [`crate::url_matcher::UrlMatcher::equivalent`] to the given matcher.
  ///
  /// # Errors
  ///
  /// Returns an error if the route handlers cannot be removed.
  /// Remove page-scoped routes registered for `matcher`. With a
  /// `handler_id` — [`crate::route::RegisteredRoute::handler_id`] of the
  /// registration to drop — only that one goes, which is what
  /// Playwright's `page.unroute(url, handler)` means; without one, every
  /// route sharing the matcher is removed.
  pub async fn unroute(&self, matcher: &crate::url_matcher::UrlMatcher, handler_id: Option<usize>) -> Result<()> {
    self
      .inner
      .unroute(matcher, crate::route::RouteScope::Page, handler_id)
      .await
  }

  /// Remove all route handlers registered via [`Page::route`].
  ///
  /// Mirrors Playwright's
  /// `page.unrouteAll({ behavior?: 'wait' | 'ignoreErrors' | 'default' })`.
  /// The `behavior` selects how to treat handlers still running when the
  /// call is made; ferridriver route handlers run synchronously inside the
  /// interception loop, so once the routes are cleared no detached handler
  /// task can still be in flight — every variant performs the same teardown
  /// (clear routes, disable interception).
  ///
  /// Context-scoped routes installed via `context.route` stay active,
  /// matching Playwright where `page.unrouteAll` only clears
  /// `page._routes`.
  ///
  /// # Errors
  ///
  /// Returns an error if the underlying interception teardown fails.
  pub async fn unroute_all(&self, behavior: Option<crate::options::UnrouteBehavior>) -> Result<()> {
    self
      .inner
      .unroute_all(behavior.unwrap_or_default(), Some(crate::route::RouteScope::Page))
      .await
  }

  // ── Interactive picker ──────────────────────────────────────────────────

  /// Open the interactive locator picker: highlight elements under the
  /// cursor and resolve with a [`Locator`] for the element the user clicks.
  ///
  /// Mirrors Playwright's `page.pickLocator(): Promise<Locator>`. The picker
  /// generates a selector for the clicked element using the same engine that
  /// backs `codegen`/recording, then returns `page.locator(selector)`.
  ///
  /// Cancel an in-progress pick with [`Page::cancel_pick_locator`].
  ///
  /// # Errors
  ///
  /// Returns an error if the picker scaffolding cannot be injected, the page
  /// closes before a selection is made, or the returned selector is empty.
  pub async fn pick_locator(self: &Arc<Self>) -> Result<Locator> {
    self.inner.ensure_engine_injected().await?;
    self.inner.evaluate(crate::selectors::RECORDER_SUPPORT_JS).await?;
    self.inner.evaluate(Self::PICKER_JS).await?;

    // Poll the page-side global for the picked selector. Playwright's
    // `pickLocator` waits indefinitely for the user to click; we mirror
    // that (no timeout) while honoring cancellation: when the picker is
    // torn down via `cancel_pick_locator`/`hide_highlight` without a
    // selection, `__fdPicker` flips to `false` and we surface a cancelled
    // error. Polling (rather than a cross-task exposed-function callback)
    // keeps engine teardown race-free on the QuickJS host.
    loop {
      let probe = self
        .inner
        .evaluate(
          "JSON.stringify({ \
             selector: (typeof window.__fdPickedSelector === 'string') ? window.__fdPickedSelector : null, \
             active: window.__fdPicker === true })",
        )
        .await?;
      let parsed: serde_json::Value = match probe {
        Some(serde_json::Value::String(s)) => serde_json::from_str(&s).unwrap_or(serde_json::Value::Null),
        Some(v) => v,
        None => serde_json::Value::Null,
      };
      if let Some(sel) = parsed.get("selector").and_then(serde_json::Value::as_str) {
        return Ok(self.locator(sel));
      }
      if !parsed
        .get("active")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
      {
        return Err(crate::error::FerriError::interrupted(
          "pickLocator: cancelled before a selection was made",
        ));
      }
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
  }

  /// Cancel an in-progress [`Page::pick_locator`] and hide its highlight.
  ///
  /// Mirrors Playwright's `page.cancelPickLocator()`. Any pending
  /// `pick_locator` future resolves with a `page closed`-style error once the
  /// page-side picker is torn down (the exposed callback is removed, so the
  /// oneshot sender drops).
  ///
  /// # Errors
  ///
  /// Returns an error if the page-side teardown evaluation fails.
  pub async fn cancel_pick_locator(&self) -> Result<()> {
    self
      .inner
      .evaluate("(function(){ if (window.__fdPickerCancel) window.__fdPickerCancel(); })()")
      .await?;
    let _ = self.inner.remove_exposed_function("__fdPickLocator").await;
    Ok(())
  }

  /// Hide any highlight overlay currently shown by the picker or by
  /// `highlight`-style helpers.
  ///
  /// Mirrors Playwright's `page.hideHighlight()`.
  ///
  /// # Errors
  ///
  /// Returns an error if the page-side teardown evaluation fails.
  pub async fn hide_highlight(&self) -> Result<()> {
    self.lock_highlights().clear();
    let ids: Vec<Arc<str>> = self.with_frame_cache(|c| c.live_ids().collect());
    let main = self.with_frame_cache(FrameCache::main_frame_id);
    for id in ids.iter().filter(|id| **id != main) {
      let _ = self
        .inner
        .evaluate_in_frame("window.__fd && window.__fd.hideHighlight()", id)
        .await;
    }
    self
      .inner
      .evaluate(
        "(function(){ if (window.__fd && window.__fd.hideHighlight) window.__fd.hideHighlight(); \
         if (window.__fdPickerCancel) window.__fdPickerCancel(); })()",
      )
      .await?;
    Ok(())
  }

  fn lock_highlights(&self) -> std::sync::MutexGuard<'_, Vec<HighlightEntry>> {
    self
      .highlights
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// `HighlightController.addHighlight`.
  pub(crate) async fn add_highlight(
    self: &Arc<Self>,
    frame_id: Arc<str>,
    selector: &str,
    style: Option<String>,
  ) -> Result<()> {
    crate::selectors::parse(selector)?;
    {
      let mut entries = self.lock_highlights();
      entries.retain(|e| !(e.frame_id == frame_id && e.selector == selector));
      entries.push(HighlightEntry {
        frame_id,
        selector: selector.to_string(),
        style,
      });
    }
    self.apply_highlights().await?;
    self.keep_highlights_applied();
    Ok(())
  }

  /// `HighlightController.removeHighlight`.
  pub(crate) async fn remove_highlight(&self, frame_id: &str, selector: &str) -> Result<()> {
    self
      .lock_highlights()
      .retain(|e| !(&*e.frame_id == frame_id && e.selector == selector));
    self.apply_highlights().await
  }

  /// Give every live frame exactly its own highlights.
  async fn apply_highlights(&self) -> Result<()> {
    let entries = self.lock_highlights().clone();
    let main = self.with_frame_cache(FrameCache::main_frame_id);
    let ids: Vec<Arc<str>> = self.with_frame_cache(|c| c.live_ids().collect());
    let mut first_error = None;
    for id in ids {
      let mine: Vec<(String, Option<String>)> = entries
        .iter()
        .filter(|e| e.frame_id == id)
        .map(|e| (e.selector.clone(), e.style.clone()))
        .collect();
      let frame_id = (main != id).then_some(&*id);
      if let Err(e) = crate::selectors::set_highlights(&self.inner, &mine, frame_id).await
        && !mine.is_empty()
      {
        first_error.get_or_insert(e);
      }
    }
    first_error.map_or(Ok(()), Err)
  }

  /// The one-second re-apply loop, started by the first highlight and
  /// ended once none are left or the page closes.
  fn keep_highlights_applied(self: &Arc<Self>) {
    if self.highlight_timer.swap(true, std::sync::atomic::Ordering::AcqRel) {
      return;
    }
    let page = Arc::downgrade(self);
    tokio::spawn(async move {
      loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let Some(page) = page.upgrade() else { return };
        if page.is_closed() || page.lock_highlights().is_empty() {
          page.highlight_timer.store(false, std::sync::atomic::Ordering::Release);
          return;
        }
        let _ = page.apply_highlights().await;
      }
    });
  }

  // ── Exposed Functions ───────────────────────────────────────────────────

  /// Expose a Rust function to the page as `window.<name>(...)`.
  /// The page can call it as an async function and receive the return value.
  /// The exposed function persists across navigations.
  ///
  /// ```ignore
  /// use std::sync::Arc;
  ///
  /// page.expose_function("compute", Arc::new(|args| {
  ///     Box::pin(async move {
  ///         let x = args.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
  ///         Ok(serde_json::json!(x * 2.0).into())
  ///     })
  /// })).await?;
  ///
  /// // In the page:
  /// // const result = await window.compute(21); // returns 42
  /// ```
  ///
  /// # Errors
  ///
  /// Returns an error if the function cannot be exposed to the page.
  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<()> {
    self.inner.expose_function(name, func).await
  }

  /// Expose a Rust binding to the page as `window.<name>(...)`. Like
  /// [`Self::expose_function`], but the callback additionally receives a
  /// [`crate::events::BindingSource`] identifying the calling context,
  /// page, and frame (Playwright: `page.exposeBinding(name, callback)`,
  /// `client/page.ts:371`). The page still calls it as an async function
  /// receiving the return value.
  ///
  /// # Errors
  ///
  /// Returns an error if the binding cannot be installed on the page.
  pub async fn expose_binding(&self, name: &str, binding: crate::events::ExposedBinding) -> Result<()> {
    self.inner.expose_binding(name, binding).await
  }

  /// Playwright: `page.pause(): Promise<void>` (`client/page.ts:858`).
  /// Playwright suspends script execution and hands control to the
  /// Inspector/recorder UI. ferridriver has no recorder, so this is a
  /// typed [`crate::error::FerriError::Unsupported`] rather than a
  /// silent no-op that would mislead callers into thinking a breakpoint
  /// was honoured.
  ///
  /// # Errors
  ///
  /// Always returns [`crate::error::FerriError::Unsupported`].
  pub fn pause(&self) -> Result<()> {
    Err(crate::error::FerriError::Unsupported(format!(
      "page.pause() (page at {}) requires the Playwright Inspector/recorder UI, which ferridriver does not provide",
      self.url()
    )))
  }

  /// Intercept WebSocket connections on this page that match `matcher`.
  /// Playwright: `page.routeWebSocket(url, handler)`. The handler is
  /// invoked with a live [`crate::web_socket_route::WebSocketRoute`] when
  /// a matching `new WebSocket(...)` is created in the page.
  ///
  /// # Errors
  ///
  /// Returns an error if installing the page mock / binding fails.
  pub async fn route_web_socket(
    self: &Arc<Self>,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::web_socket_route::WsHandler,
  ) -> Result<()> {
    self
      .route_web_socket_scoped(matcher, handler, crate::web_socket_route::WsRouteScope::Page)
      .await
  }

  /// `safeNonStallingEvaluateInAllFrames`: run `expression` in every
  /// frame's main world; a frame that cannot run it (it is navigating,
  /// or gone) is skipped.
  pub(crate) async fn evaluate_in_all_frames(&self, expression: &str) {
    let main = self.with_frame_cache(crate::frame_cache::FrameCache::main_frame_id);
    let ids: Vec<Arc<str>> = self.with_frame_cache(|c| c.live_ids().collect());
    let inner = &self.inner;
    let runs = ids.iter().map(|id| {
      let is_main = main == *id;
      async move {
        let _ = if is_main {
          inner.evaluate(expression).await
        } else {
          inner.evaluate_in_frame(expression, id).await
        };
      }
    });
    if ids.is_empty() {
      let _ = inner.evaluate(expression).await;
    } else {
      futures::future::join_all(runs).await;
    }
  }

  /// Install a WebSocket route at the given scope. `Page` scope routes
  /// are matched before `Context` scope routes at socket creation
  /// (Playwright's page-dispatcher-before-context-dispatcher order);
  /// the context layer uses `Context` scope both for its immediate
  /// fan-out and when re-applying registered routes to a fresh page.
  pub(crate) async fn route_web_socket_scoped(
    self: &Arc<Self>,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::web_socket_route::WsHandler,
    scope: crate::web_socket_route::WsRouteScope,
  ) -> Result<()> {
    use crate::web_socket_route as wsr;
    let router = wsr::router_for_page(self.backend_page_id(), self.inner.clone());
    let first = router.add_route(matcher, handler, scope);
    if first {
      self
        .inner
        .expose_binding(wsr::WS_BINDING_NAME, wsr::binding_callback(router))
        .await?;
      let source = crate::options::evaluation_script(wsr::mock_init_script(), None)?;
      self.inner.add_init_script(&source).await?;
      // The documents already loaded get the mock too, as Playwright's
      // `WebSocketRouteDispatcher.install` does.
      self.evaluate_in_all_frames(&source).await;
    }
    Ok(())
  }

  /// Remove a previously exposed function.
  ///
  /// # Errors
  ///
  /// Returns an error if the function cannot be removed.
  pub async fn remove_exposed_function(&self, name: &str) -> Result<()> {
    self.inner.remove_exposed_function(name).await
  }

  // ── Script / Style injection ────────────────────────────────────────────

  /// Playwright: `page.addScriptTag(options?: { content?, path?, type?,
  /// url? }): Promise<ElementHandle>`. See
  /// [`crate::frame::Frame::add_script_tag`], which this is for the main
  /// frame.
  ///
  /// # Errors
  ///
  /// Returns an error if none of `url`, `path` and `content` is given,
  /// `path` cannot be read, or the script fails to load.
  #[track_caller]
  pub fn add_script_tag(
    self: &Arc<Self>,
  ) -> crate::action::Action<'static, crate::options::AddScriptTagOptions, crate::element_handle::ElementHandle> {
    self.main_frame().add_script_tag()
  }

  /// Playwright: `page.addStyleTag(options?: { content?, path?, url? }):
  /// Promise<ElementHandle>`. See [`crate::frame::Frame::add_style_tag`],
  /// which this is for the main frame.
  ///
  /// # Errors
  ///
  /// Returns an error if none of `url`, `path` and `content` is given,
  /// `path` cannot be read, or the stylesheet fails to load.
  #[track_caller]
  pub fn add_style_tag(
    self: &Arc<Self>,
  ) -> crate::action::Action<'static, crate::options::AddStyleTagOptions, crate::element_handle::ElementHandle> {
    self.main_frame().add_style_tag()
  }

  // ── Dialog handling ─────────────────────────────────────────────────────
  //
  // Dialogs (alert/confirm/prompt/beforeunload) are observed through
  // two equivalent surfaces:
  //
  // * [`Self::events`]`.on("dialog", cb)` — broadcast listener, live
  //   [`crate::dialog::Dialog`] handle delivered in the callback.
  //   Backed by the per-page [`crate::dialog::DialogManager`]'s
  //   emitter-bridge (installed once at page construction).
  // * [`Self::wait_for_dialog`] — one-shot async wait. Mirrors
  //   Playwright's `page.waitForEvent('dialog')` directly against
  //   the `DialogManager`; bypasses the broadcast entirely so the
  //   claim is synchronous with dialog open, matching Playwright's
  //   `addDialogHandler` semantics verbatim.
  //
  // If no handler claims a dialog at open time, the `DialogManager`
  // auto-closes it — accept for `beforeunload`, dismiss otherwise —
  // matching Playwright's `Dialog._close` branch.

  /// Wait for the next dialog of any type, with a timeout. Returns
  /// the live [`crate::dialog::Dialog`] handle; the caller must then
  /// call `accept(...)` or `dismiss()` exactly once. Mirrors
  /// Playwright's `page.waitForEvent('dialog', { timeout })`.
  ///
  /// Registers a one-shot handler with the page's
  /// [`crate::dialog::DialogManager`] that claims the first dialog
  /// and delivers it here. The handler is removed automatically on
  /// resolve or timeout.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Timeout`] if no dialog opens
  /// within `timeout_ms`. Returns [`crate::error::FerriError::TargetClosed`]
  /// if the page closes before a dialog arrives.
  pub fn wait_for_dialog(
    &self,
    timeout_ms: u64,
  ) -> impl std::future::Future<Output = Result<crate::dialog::Dialog>> + Send + use<> {
    use std::sync::Mutex;
    let (tx, rx) = tokio::sync::oneshot::channel::<crate::dialog::Dialog>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    // Claimed from the call, so one the caller triggers before awaiting
    // is not missed.
    let id = self.inner.dialog_manager().add_handler(Arc::new(move |dialog| {
      let mut guard = match tx.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
      };
      match guard.take() {
        // A failed send means the waiter already timed out and dropped
        // the receiver — report unclaimed so the manager's auto-close
        // path still runs instead of leaving the dialog open.
        Some(sender) => sender.send(dialog.clone()).is_ok(),
        None => false,
      }
    }));
    let inner = self.inner.clone();
    let claim = Unregister::new(move || inner.dialog_manager().remove_handler(id));
    async move {
      let _claim = claim;
      let result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await;
      match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(crate::error::FerriError::target_closed(Some(
          "page closed while waiting for dialog".into(),
        ))),
        Err(_) => Err(crate::error::FerriError::timeout("waiting for dialog", timeout_ms)),
      }
    }
  }

  // ── File choosers (live handle, first-class) ────────────────────────────
  //
  // Symmetric with the dialog surface above:
  //
  // * [`Self::events`]`.on("filechooser", cb)` — broadcast listener,
  //   live [`crate::file_chooser::FileChooser`] handle delivered in
  //   the callback. Backed by the per-page
  //   [`crate::file_chooser::FileChooserManager`]'s emitter-bridge
  //   (installed once at `attach_listeners` time).
  // * [`Self::wait_for_file_chooser`] — one-shot async wait. Mirrors
  //   Playwright's `page.waitForEvent('filechooser')` directly against
  //   the `FileChooserManager`; bypasses the broadcast so the claim
  //   is synchronous with the chooser opening.
  //
  // If no handler claims at `did_open` time, the manager disposes the
  // captured `<input>` element handle — matches Playwright's
  // `server/page.ts::_onFileChooserOpened` no-listener branch.

  /// Wait for the next file chooser to open, with a timeout. Returns
  /// the live [`crate::file_chooser::FileChooser`] handle; the caller
  /// may then call `set_files(...)` (or drop the handle to cancel the
  /// upload — the native picker was already suppressed by CDP's
  /// `Page.setInterceptFileChooserDialog`). Mirrors Playwright's
  /// `page.waitForEvent('filechooser', { timeout })`.
  ///
  /// Registers a one-shot handler with the page's
  /// [`crate::file_chooser::FileChooserManager`] that claims the
  /// first chooser and delivers it here. The handler is removed
  /// automatically on resolve or timeout.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Timeout`] if no file chooser
  /// opens within `timeout_ms`. Returns
  /// [`crate::error::FerriError::TargetClosed`] if the page closes
  /// before a chooser arrives.
  pub fn wait_for_file_chooser(
    &self,
    timeout_ms: u64,
  ) -> impl std::future::Future<Output = Result<crate::file_chooser::FileChooser>> + Send + use<> {
    use std::sync::Mutex;
    let (tx, rx) = tokio::sync::oneshot::channel::<crate::file_chooser::FileChooser>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    // Claimed from the call, so one the caller triggers before awaiting
    // is not missed.
    let id = self.inner.file_chooser_manager().add_handler(Arc::new(move |chooser| {
      let mut guard = match tx.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
      };
      match guard.take() {
        // Failed send = waiter already timed out; report unclaimed so
        // the manager's no-listener disposal path still runs.
        Some(sender) => sender.send(chooser.clone()).is_ok(),
        None => false,
      }
    }));
    // Sent now, as `on("filechooser")` sends it, so the browser reports the
    // event before the wait is first awaited.
    self.lazy_enable_for_event("filechooser");
    let inner = self.inner.clone();
    let claim = Unregister::new({
      let inner = inner.clone();
      move || inner.file_chooser_manager().remove_handler(id)
    });
    async move {
      let _claim = claim;
      // Idempotent: answers at once when the call above already sent it.
      inner.enable_file_chooser_intercept().await?;
      let result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await;
      match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(crate::error::FerriError::target_closed(Some(
          "page closed while waiting for filechooser".into(),
        ))),
        Err(_) => Err(crate::error::FerriError::timeout("waiting for filechooser", timeout_ms)),
      }
    }
  }

  // ── Downloads (live handle, first-class) ──────────────────────────────
  //
  // Symmetric with dialog / filechooser above.
  //
  // * [`Self::events`]`.on("download", cb)` — broadcast listener, live
  //   [`crate::download::Download`] handle delivered in the callback.
  //   Backed by the per-page
  //   [`crate::download::DownloadManager`]'s emitter-bridge (installed
  //   once at `attach_listeners` time).
  // * [`Self::wait_for_download`] — one-shot async wait. Mirrors
  //   Playwright's `page.waitForEvent('download')`. Registers a
  //   one-shot handler directly on the `DownloadManager`; the claim is
  //   synchronous with the backend's download-begin event, so there's
  //   no broadcast round-trip to race against.
  //
  // Unclaimed downloads are not auto-cancelled — Playwright's server
  // does the same (just emits the event and leaves the bytes in the
  // per-context `downloadsPath`). The per-page `downloads_dir` drop
  // cleans up orphan files on page close.

  /// Wait for the next download, with a timeout. Returns the live
  /// [`crate::download::Download`] handle; the caller may then call
  /// `save_as(path)` / `path()` / `failure()` / `cancel()` / `delete()`.
  /// Mirrors Playwright's `page.waitForEvent('download', { timeout })`.
  ///
  /// Registers a one-shot handler with the page's
  /// [`crate::download::DownloadManager`]; the handler is removed
  /// automatically on resolve or timeout.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Timeout`] if no download
  /// begins within `timeout_ms`. Returns
  /// [`crate::error::FerriError::TargetClosed`] if the page closes
  /// before a download begins.
  pub fn wait_for_download(
    &self,
    timeout_ms: u64,
  ) -> impl std::future::Future<Output = Result<crate::download::Download>> + Send + use<> {
    use std::sync::Mutex;
    let (tx, rx) = tokio::sync::oneshot::channel::<crate::download::Download>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    // Claimed from the call, so one the caller triggers before awaiting
    // is not missed.
    let id = self.inner.download_manager().add_handler(Arc::new(move |download| {
      let mut guard = match tx.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
      };
      match guard.take() {
        // Failed send = waiter already timed out; report unclaimed so
        // other handlers / the manager fallback can still take it.
        Some(sender) => sender.send(download.clone()).is_ok(),
        None => false,
      }
    }));
    // Sent now, as `on("download")` sends it, so the browser reports the
    // event before the wait is first awaited.
    self.lazy_enable_for_event("download");
    let inner = self.inner.clone();
    let claim = Unregister::new({
      let inner = inner.clone();
      move || inner.download_manager().remove_handler(id)
    });
    async move {
      let _claim = claim;
      // Idempotent: answers at once when the call above already sent it.
      inner.enable_download_behavior().await?;
      let result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await;
      match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(crate::error::FerriError::target_closed(Some(
          "page closed while waiting for download".into(),
        ))),
        Err(_) => Err(crate::error::FerriError::timeout("waiting for download", timeout_ms)),
      }
    }
  }

  // ── Init Scripts ────────────────────────────────────────────────────────

  /// Register a script to run before any page JS on every navigation.
  /// Mirrors Playwright's `page.addInitScript(script, arg)` from
  /// `/tmp/playwright/packages/playwright-core/src/client/page.ts:520`.
  ///
  /// Accepts the full Playwright argument shape: a JS function body
  /// (pre-serialised via `fn.toString()` at the binding layer), a verbatim
  /// source string, a `{ path }`, or a `{ content }`. The optional `arg`
  /// is JSON-stringified and composed into a `(body)(arg)` wrapper for
  /// the `Function` variant; passing `arg` alongside any non-function
  /// variant is a Playwright-parity error (see [`crate::options::evaluation_script`]).
  ///
  /// Returns a [`crate::disposable::Disposable`] whose `dispose()` removes the
  /// injected script (equivalent to [`Page::remove_init_script`] with the
  /// generated identifier). Mirrors Playwright `page.addInitScript(...)` which
  /// returns a `Disposable` (`client/page.ts:532`).
  ///
  /// # Errors
  ///
  /// Returns an error if `evaluation_script` lowering fails (bad path, bad
  /// arg combination, JSON serialisation) or the backend injection fails.
  pub async fn add_init_script(
    &self,
    script: crate::options::InitScriptSource,
    arg: Option<serde_json::Value>,
  ) -> Result<crate::disposable::Disposable> {
    let source = crate::options::evaluation_script(script, arg.as_ref())?;
    let identifier = self.inner.add_init_script(&source).await?;
    let inner = self.inner.clone();
    Ok(crate::disposable::Disposable::new(move || async move {
      inner.remove_init_script(&identifier).await
    }))
  }

  /// Playwright: `page.addInitScript(script, arg, { exposeFunctions: true })`.
  /// Each function in `arg` stays callable from every document the
  /// script runs in, until the returned disposable removes the script.
  ///
  /// # Errors
  ///
  /// Returns an error unless `script` is a function, if `arg` holds a
  /// handle, or if the backend injection fails.
  pub async fn add_init_script_exposing_functions(
    &self,
    script: crate::options::InitScriptSource,
    arg: crate::protocol::SerializedArgument,
  ) -> Result<crate::disposable::Disposable> {
    let source = crate::options::evaluation_script_exposing(script, &arg.value)?;
    for function in &arg.functions {
      self
        .inner
        .expose_callback(&function.name, Arc::clone(&function.binding))
        .await?;
    }
    let identifier = self.inner.add_init_script(&source).await?;
    let inner = self.inner.clone();
    let names: Vec<String> = arg.functions.into_iter().map(|f| f.name).collect();
    Ok(crate::disposable::Disposable::new(move || async move {
      for name in &names {
        inner.remove_callback(name).await;
      }
      inner.remove_init_script(&identifier).await
    }))
  }

  /// Remove a previously injected init script by identifier.
  ///
  /// # Errors
  ///
  /// Returns an error if the init script cannot be removed.
  pub async fn remove_init_script(&self, identifier: &str) -> Result<()> {
    self.inner.remove_init_script(identifier).await
  }
}
