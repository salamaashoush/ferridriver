//! Page event emitter, observations, web storage, and wait/expect helpers.

use super::{Page, Unregister, web_storage_global, web_storage_js_string};
use std::sync::Arc;

use crate::backend::AnyPage;
use crate::error::Result;
use crate::events::{EventEmitter, PageEvent};
use crate::frame::Frame;
use tokio::sync::Mutex as AsyncMutex;

type ClaimHandler<T> = Arc<dyn Fn(&T) -> bool + Send + Sync>;

/// Pair a one-shot claim handler closure with its receiver so a manager
/// callback delivers the first event synchronously and reports unclaimed
/// after timeout or receiver drop.
fn oneshot_claim_pair<T: Clone + Send + 'static>() -> (ClaimHandler<T>, tokio::sync::oneshot::Receiver<T>) {
  let (tx, rx) = tokio::sync::oneshot::channel::<T>();
  let tx = Arc::new(std::sync::Mutex::new(Some(tx)));
  let handler = Arc::new(move |value: &T| {
    let mut guard = match tx.lock() {
      Ok(g) => g,
      Err(p) => p.into_inner(),
    };
    match guard.take() {
      Some(sender) => sender.send(value.clone()).is_ok(),
      None => false,
    }
  });
  (handler, rx)
}

async fn await_claimed<T>(rx: tokio::sync::oneshot::Receiver<T>, timeout_ms: u64, noun: &'static str) -> Result<T> {
  match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await {
    Ok(Ok(value)) => Ok(value),
    Ok(Err(_)) => Err(crate::error::FerriError::target_closed(Some(format!(
      "page closed while waiting for {noun}"
    )))),
    Err(_) => Err(crate::error::FerriError::timeout(
      format!("waiting for {noun}"),
      timeout_ms,
    )),
  }
}

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
    let (handler, rx) = oneshot_claim_pair::<crate::dialog::Dialog>();
    // Claimed from the call, so one the caller triggers before awaiting
    // is not missed.
    let id = self.inner.dialog_manager().add_handler(handler);
    let inner = self.inner.clone();
    let claim = Unregister::new(move || inner.dialog_manager().remove_handler(id));
    async move {
      let _claim = claim;
      await_claimed(rx, timeout_ms, "dialog").await
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
    let (handler, rx) = oneshot_claim_pair::<crate::file_chooser::FileChooser>();
    // Claimed from the call, so one the caller triggers before awaiting
    // is not missed.
    let id = self.inner.file_chooser_manager().add_handler(handler);
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
      await_claimed(rx, timeout_ms, "filechooser").await
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
    let (handler, rx) = oneshot_claim_pair::<crate::download::Download>();
    // Claimed from the call, so one the caller triggers before awaiting
    // is not missed.
    let id = self.inner.download_manager().add_handler(handler);
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
      await_claimed(rx, timeout_ms, "download").await
    }
  }
}
