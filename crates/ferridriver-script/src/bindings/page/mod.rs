//! `PageJs`: JS wrapper around `ferridriver::Page`.
//!
//! Methods mirror `ferridriver::Page`'s public surface one-for-one; each is a
//! small delegation that converts `FerriError` into `rquickjs::Error` at the
//! boundary via [`super::convert::FerriResultCtxExt`].

pub(crate) mod callbacks;
mod events;
pub(crate) mod options;

pub(crate) use callbacks::*;
pub(crate) use events::*;
pub(crate) use options::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ferridriver::Page;
use rquickjs::JsLifetime;
use rquickjs::class::{Class, Trace};
use rquickjs::function::This;

use rquickjs::function::Opt;

use crate::bindings::convert::FerriResultCtxExt;
use crate::bindings::convert::throw_named;
use crate::bindings::convert::{init_script_from_js, serde_from_js, serde_to_js};
use crate::bindings::keyboard::KeyboardJs;
use crate::bindings::locator::LocatorJs;
use crate::bindings::mouse::MouseJs;

/// JS-visible wrapper around [`ferridriver::Page`].
///
/// Held as `Arc<Page>` so the same page can be shared with the MCP session
/// while the script runs; dropping the wrapper does not close the page.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Page")]
pub struct PageJs {
  // rquickjs requires fields to implement Trace/JsLifetime; Arc<Page> does
  // not, and there's nothing inside a Page that holds JS values. Mark with
  // `#[qjs(skip_trace)]` so the macro skips tracing this field.
  #[qjs(skip_trace)]
  inner: Arc<Page>,
  /// VM-loop handle used by `page.route` to dispatch JS callbacks
  /// from a separate tokio task back into the script's JS context.
  /// `None` only when the wrapper was constructed directly (e.g. by
  /// tests); the engine always installs `PageJs` via
  /// `install_page` which sets this field.
  #[qjs(skip_trace)]
  vm: Option<ferrijs::VmHandle>,
  /// Maps a handler locator's selector to the persisted-callback ids so
  /// `removeLocatorHandler` can drop them. (`QuickJS` `addLocatorHandler`
  /// itself is Unsupported -- see its binding -- so this normally stays empty.)
  #[qjs(skip_trace)]
  locator_handler_ids: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, Vec<u64>>>>,
  #[qjs(skip_trace)]
  coverage_session: Arc<tokio::sync::Mutex<Option<ferridriver::CdpSession>>>,
  #[qjs(skip_trace)]
  cpu_profile_session: Arc<tokio::sync::Mutex<Option<ferridriver::CdpSession>>>,
}

impl PageJs {
  #[must_use]
  pub fn new(inner: Arc<Page>) -> Self {
    Self {
      inner,
      vm: None,
      locator_handler_ids: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      coverage_session: Arc::new(tokio::sync::Mutex::new(None)),
      cpu_profile_session: Arc::new(tokio::sync::Mutex::new(None)),
    }
  }

  #[must_use]
  pub fn new_with_vm(inner: Arc<Page>, vm: ferrijs::VmHandle) -> Self {
    Self {
      inner,
      vm: Some(vm),
      locator_handler_ids: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      coverage_session: Arc::new(tokio::sync::Mutex::new(None)),
      cpu_profile_session: Arc::new(tokio::sync::Mutex::new(None)),
    }
  }

  /// This page's route-registry owner key.
  fn route_owner(&self) -> RouteOwner {
    RouteOwner::Page(self.inner.backend_page_id())
  }

  fn backend_page_id(&self) -> usize {
    self.inner.backend_page_id()
  }

  /// Clone of the wrapped `Arc<Page>` for cross-binding consumers
  /// (used by `expect()` to lift a `PageJs` into an assertion target).
  #[must_use]
  pub fn page_arc(&self) -> Arc<Page> {
    Arc::clone(&self.inner)
  }

  #[must_use]
  pub fn page(&self) -> &Arc<Page> {
    &self.inner
  }

  fn off_impl<'js>(
    &self,
    ctx: &rquickjs::Ctx<'js>,
    event: &str,
    listener: Option<&rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let listener_fn = listener.and_then(|v| v.as_function().cloned());
    let Some(listener_fn) = listener_fn else {
      // Lenient `off(event)` — drop every listener for that event
      // (matches `removeAllListeners(event)` semantics).
      let ids = with_page_callbacks(ctx, |r| r.remove_event_listeners_named(event))?;
      for id in ids {
        self.inner.off(ferridriver::events::ListenerId(id));
      }
      return Ok(());
    };
    let target_value: rquickjs::Value<'js> = listener_fn.into_value();
    for (id, saved) in with_page_callbacks(ctx, |r| r.event_listeners_named(event))? {
      let restored = saved.restore(ctx)?;
      if restored.into_value() == target_value {
        self.inner.off(ferridriver::events::ListenerId(id));
        with_page_callbacks(ctx, |r| r.remove_event_listener(id))?;
      }
    }
    Ok(())
  }

  /// Shared core for `page.on` / `page.once` / `prependListener` / `prependOnceListener`.
  fn register_listener<'js>(
    &self,
    ctx: &rquickjs::Ctx<'js>,
    event: &str,
    listener: rquickjs::Function<'js>,
    once: bool,
    front: bool,
  ) -> rquickjs::Result<()> {
    let saved = SavedCallback::save(ctx, listener);
    let tx = ensure_event_pump(ctx);
    let id_slot = Arc::new(AtomicU64::new(0));
    let id_slot_cb = Arc::clone(&id_slot);
    let page_for_cb = Arc::clone(&self.inner);
    let callback: ferridriver::events::EventCallback =
      std::sync::Arc::new(move |ev: ferridriver::events::PageEvent| {
        let id = id_slot_cb.load(Ordering::Relaxed);
        let msg = crate::bindings::page::callbacks::PageEventMsg::Event {
          id,
          remove_after: once,
          page: Arc::clone(&page_for_cb),
          event: ev,
        };
        if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) = tx.try_send(msg) {
          tracing::warn!(
            listener_id = id,
            capacity = PAGE_EVENT_PUMP_CAPACITY,
            "page event pump full (VM idle between scripts?); dropping event"
          );
        }
      });
    let id = match (once, front) {
      (false, false) => self.inner.on(event, callback),
      (true, false) => self.inner.once(event, callback),
      (false, true) => self.inner.prepend_listener(event, callback),
      (true, true) => self.inner.prepend_once_listener(event, callback),
    };
    id_slot.store(id.0, Ordering::Relaxed);
    let page_key = self.inner.backend_page_id();
    with_page_callbacks(ctx, |r| {
      r.insert_event_listener(id.0, event.to_string(), page_key, saved);
    })?;
    Ok(())
  }
}

/// Build a `PageJs` for a page minted from script (`newPage`,
/// `locator.page()`, `frame.page()`), threading the session's VM-loop
/// handle (stashed as userdata at `Session::create`) so `page.route` /
/// `page.exposeFunction` cross-task dispatch works on script-launched
/// browsers — not just the MCP-prebound page.
pub(crate) fn pagejs_for_ctx(ctx: &rquickjs::Ctx<'_>, page: Arc<Page>) -> PageJs {
  match ferrijs::vm_handle(ctx) {
    Some(vm) => PageJs::new_with_vm(page, vm),
    None => PageJs::new(page),
  }
}

crate::bindings::macros::qjs_impl! {
  #[expect(
    clippy::needless_pass_by_value,
    reason = "rquickjs hands methods their Ctx and JS arguments by value"
  )]
  impl PageJs {
    @mixin locator_queries(optional_frame_locator);
    @mixin selector_surface();
    @mixin event_emitter();
  // ── Navigation ────────────────────────────────────────────────────────────

  /// Navigate to `url`. Accepts `{ waitUntil?, timeout?, referer? }` to
  /// mirror Playwright's `page.goto(url, options?)`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the navigation fails.
  #[qjs(rename = "goto")]
  pub async fn goto<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    url: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::convert::Null<crate::bindings::network::ResponseJs>> {
    call_site
      .scope(async move {
        let opts = parse_goto_options(&ctx, options)?;
        let resp = self.inner.goto(&url).maybe_options(opts).await.into_js_with(&ctx)?;
        Ok(resp.map(|r| crate::bindings::network::ResponseJs::new_with_page(r, Arc::clone(&self.inner))))
      })
      .await
      .map(crate::bindings::convert::Null)
  }

  /// Reload the current page. Accepts the same option bag as `goto`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the reload fails.
  #[qjs(rename = "reload")]
  pub async fn reload<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::convert::Null<crate::bindings::network::ResponseJs>> {
    call_site
      .scope(async move {
        let opts = parse_goto_options(&ctx, options)?;
        let resp = self.inner.reload().maybe_options(opts).await.into_js_with(&ctx)?;
        Ok(resp.map(|r| crate::bindings::network::ResponseJs::new_with_page(r, Arc::clone(&self.inner))))
      })
      .await
      .map(crate::bindings::convert::Null)
  }

  /// Navigate back in history. Accepts the same option bag as `goto`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the navigation fails.
  #[qjs(rename = "goBack")]
  pub async fn go_back<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::convert::Null<crate::bindings::network::ResponseJs>> {
    call_site
      .scope(async move {
        let opts = parse_goto_options(&ctx, options)?;
        let resp = self.inner.go_back().maybe_options(opts).await.into_js_with(&ctx)?;
        Ok(resp.map(|r| crate::bindings::network::ResponseJs::new_with_page(r, Arc::clone(&self.inner))))
      })
      .await
      .map(crate::bindings::convert::Null)
  }

  /// Navigate forward in history. Accepts the same option bag as `goto`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the navigation fails.
  #[qjs(rename = "goForward")]
  pub async fn go_forward<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::convert::Null<crate::bindings::network::ResponseJs>> {
    call_site
      .scope(async move {
        let opts = parse_goto_options(&ctx, options)?;
        let resp = self.inner.go_forward().maybe_options(opts).await.into_js_with(&ctx)?;
        Ok(resp.map(|r| crate::bindings::network::ResponseJs::new_with_page(r, Arc::clone(&self.inner))))
      })
      .await
      .map(crate::bindings::convert::Null)
  }

  /// Current URL of the page.
  /// Playwright: `page.url(): string` — synchronous.
  #[qjs(rename = "url")]
  #[must_use]
  pub fn url(&self) -> String {
    self.inner.url()
  }

  /// Playwright: `page.video(): null | Video` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:4756`.
  /// Returns a live `Video` handle when the owning context was
  /// created with `recordVideo`, or `null` otherwise.
  ///
  /// # Errors
  ///
  /// Throws when the `Video` instance cannot be created in this context.
  #[qjs(rename = "video")]
  pub fn video<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    use rquickjs::class::Class;
    match self.inner.video() {
      Some(video) => {
        let wrapper = crate::bindings::video::VideoJs::new(video);
        let instance = Class::instance(ctx.clone(), wrapper)?;
        rquickjs::IntoJs::into_js(instance, &ctx)
      },
      None => Ok(rquickjs::Value::new_null(ctx)),
    }
  }

  /// Register a JS snippet to run on every new document before any page
  /// script executes. Mirrors Playwright's
  /// `page.addInitScript(script, arg)` — see
  /// `/tmp/playwright/packages/playwright-core/src/client/page.ts:520`.
  /// Accepts `Function | string | { path?, content? }` + optional `arg`
  /// exactly like the NAPI binding; all lowering runs in Rust core via
  /// [`ferridriver::options::evaluation_script`].
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `script`, `arg` or `options` cannot be converted, and with the core failure
  /// (`TimeoutError`, `TargetClosedError` or `FerriError`) when the script cannot be registered.
  #[qjs(rename = "addInitScript")]
  pub async fn add_init_script<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    script: rquickjs::Value<'js>,
    arg: Opt<rquickjs::Value<'js>>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let functions =
          crate::bindings::convert::ArgFunctions::from_options(options.0.as_ref(), Some(self.inner.backend_page_id()))?;
        let disposable = if let crate::bindings::convert::ArgFunctions::Expose { .. } = functions {
          let init = crate::bindings::convert::init_script_source_from_js(&ctx, script)?;
          let arg = crate::bindings::convert::quickjs_arg_with_functions(&ctx, arg.0, functions)?;
          self.inner.add_init_script_exposing_functions(init, arg).await
        } else {
          let (init, arg_json) = init_script_from_js(&ctx, script, arg.0)?;
          self.inner.add_init_script(init, arg_json).await
        }
        .into_js_with(&ctx)?;
        let instance =
          rquickjs::class::Class::instance(ctx.clone(), crate::bindings::disposable::DisposableJs::new(disposable))?;
        rquickjs::IntoJs::into_js(instance, &ctx)
      })
      .await
  }

  /// Remove a previously-registered init script by identifier.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the script cannot be
  /// removed.
  #[qjs(rename = "removeInitScript")]
  pub async fn remove_init_script(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
    identifier: String,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move { self.inner.remove_init_script(&identifier).await.into_js_with(&ctx) })
      .await
  }

  /// Full page rendered as clean Markdown (headings, lists, links, tables
  /// preserved; chrome and boilerplate stripped).
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the page cannot be
  /// read.
  #[qjs(rename = "markdown")]
  pub async fn markdown(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
  ) -> rquickjs::Result<String> {
    call_site
      .scope(async move { self.inner.markdown().await.into_js_with(&ctx) })
      .await
  }

  // ── Element handles ───────────────────────────────────────────────────────

  /// Playwright: `page.querySelector(selector, options?): Promise<ElementHandle | null>`.
  ///
  /// # Errors
  ///
  /// Rejects when reading `options.strict` throws, or on core query failure.
  #[qjs(rename = "querySelector")]
  pub async fn query_selector<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::convert::Null<crate::bindings::element_handle::ElementHandleJs>> {
    self.dollar(call_site, ctx, selector, options).await
  }

  /// Playwright: `page.querySelectorAll(selector): Promise<ElementHandle[]>`.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure when the query fails.
  #[qjs(rename = "querySelectorAll")]
  pub async fn query_selector_all(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
    selector: String,
  ) -> rquickjs::Result<Vec<crate::bindings::element_handle::ElementHandleJs>> {
    self.dollar_dollar(call_site, ctx, selector).await
  }

  /// Playwright: `page.pause(): Promise<void>`.
  ///
  /// # Errors
  ///
  /// Always throws a `FerriError`: ferridriver has no Inspector to pause in.
  #[qjs(rename = "pause")]
  pub fn pause(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.pause().into_js_with(&ctx)
  }

  // ── Mouse / keyboard namespaces (Playwright parity) ──────────────────────

  /// `page.mouse.*` namespace: `click`, `dblclick`, `down`, `up`, `wheel`.
  /// Exposed as a JS property, matching Playwright.
  #[qjs(get, rename = "mouse")]
  #[must_use]
  pub fn mouse(&self) -> MouseJs {
    MouseJs::new(Arc::clone(&self.inner))
  }

  /// `page.keyboard.*` namespace: `down`, `up`, `press` (no selector; acts on
  /// the currently focused element). Exposed as a JS property.
  #[qjs(get, rename = "keyboard")]
  #[must_use]
  pub fn keyboard(&self) -> KeyboardJs {
    KeyboardJs::new(Arc::clone(&self.inner))
  }

  /// `page.localStorage` — the `localStorage` area for the page's
  /// current origin. Exposed as a JS property, matching Playwright.
  #[qjs(get, rename = "localStorage")]
  #[must_use]
  pub fn local_storage(&self) -> crate::bindings::web_storage::WebStorageJs {
    crate::bindings::web_storage::WebStorageJs::new(
      Arc::clone(&self.inner),
      ferridriver::options::WebStorageKind::Local,
    )
  }

  /// `page.sessionStorage` — the `sessionStorage` area for the page's
  /// current origin. Exposed as a JS property, matching Playwright.
  #[qjs(get, rename = "sessionStorage")]
  #[must_use]
  pub fn session_storage(&self) -> crate::bindings::web_storage::WebStorageJs {
    crate::bindings::web_storage::WebStorageJs::new(
      Arc::clone(&self.inner),
      ferridriver::options::WebStorageKind::Session,
    )
  }

  // ── Event emitter (Playwright parity) ────────────────────────────────────

  /// Node's `listeners(type)` — the callbacks attached to `event`, in
  /// the order they will fire.
  ///
  /// # Errors
  ///
  /// Throws an `Error` when the context's page-callback registry is missing, or when a saved listener cannot be
  /// restored.
  #[qjs(rename = "listeners")]
  pub fn listeners<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    event: String,
  ) -> rquickjs::Result<Vec<rquickjs::Value<'js>>> {
    let saved = with_page_callbacks(&ctx, |r| r.event_listeners_named(&event))?;
    let mut out = Vec::with_capacity(saved.len());
    // Dispatch order lives in core; the registry is a map.
    for id in self.inner.listener_ids_named(&event) {
      if let Some((_, cb)) = saved.iter().find(|(saved_id, _)| *saved_id == id.0) {
        out.push(cb.restore(&ctx)?.into_value());
      }
    }
    Ok(out)
  }

  /// `page.removeAllListeners(type?, options?)`.
  ///
  /// # Errors
  ///
  /// Throws an `Error` when the context's page-callback registry is missing, or when the promise for `options` cannot
  /// be created.
  #[qjs(rename = "removeAllListeners")]
  pub fn remove_all_listeners<'js>(
    this: This<Class<'js, Self>>,
    ctx: rquickjs::Ctx<'js>,
    event: Opt<String>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    {
      let page = this.0.borrow();
      if let Some(ev) = event.0 {
        let ids = with_page_callbacks(&ctx, |r| r.remove_event_listeners_named(&ev))?;
        for id in ids {
          page.inner.off(ferridriver::events::ListenerId(id));
        }
        page.inner.remove_listeners_named(&ev);
      } else {
        page.inner.remove_all_listeners();
        with_page_callbacks(&ctx, PageCallbacks::clear_event_listeners)?;
      }
    }
    let Some(options) = options.0.filter(|v| !v.is_undefined() && !v.is_null()) else {
      return Ok(this.0.into_value());
    };
    let wait = options
      .as_object()
      .and_then(|o| o.get::<_, Option<String>>("behavior").ok().flatten())
      .is_some_and(|behavior| behavior == "wait");
    let tx = crate::bindings::page::callbacks::ensure_event_pump(&ctx);
    let promise = rquickjs::promise::Promise::wrap_future(&ctx, async move {
      if wait {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        if tx
          .send(crate::bindings::page::callbacks::PageEventMsg::Drain(done_tx))
          .await
          .is_ok()
        {
          let _ = done_rx.await;
        }
      }
      Ok::<(), rquickjs::Error>(())
    })?;
    Ok(promise.into_value())
  }

  // ── Misc page surface (Playwright parity) ────────────────────────────────

  /// `page.requestGC()`. Forces a garbage-collection pass in the
  /// page's JS engine (Playwright parity).
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the page cannot collect
  /// garbage.
  #[qjs(rename = "requestGC")]
  pub async fn request_gc(&self, call_site: crate::bindings::CallSite, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    call_site
      .scope(async move { self.inner.request_gc().await.into_js_with(&ctx) })
      .await
  }

  /// Playwright: `page.workers(): Worker[]`.
  /// Playwright: `page.requests(): Promise<Request[]>`; the array is
  /// already at hand, so it is returned as `consoleMessages` returns its own.
  ///
  /// # Errors
  ///
  /// Throws when a `Request` instance cannot be created in this context.
  #[qjs(rename = "requests")]
  pub fn requests<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<Vec<rquickjs::class::Class<'js, crate::bindings::network::RequestJs>>> {
    self
      .inner
      .requests()
      .into_iter()
      .map(|request| {
        rquickjs::class::Class::instance(
          ctx.clone(),
          crate::bindings::network::RequestJs::new_with_page(request, Arc::clone(&self.inner)),
        )
      })
      .collect()
  }

  /// # Errors
  ///
  /// Throws when a `Worker` instance cannot be created in this context.
  #[qjs(rename = "workers")]
  pub fn workers<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<Vec<rquickjs::class::Class<'js, crate::bindings::worker::WorkerJs>>> {
    self
      .inner
      .workers()
      .into_iter()
      .map(|worker| crate::bindings::identity::worker_object(&ctx, worker))
      .collect()
  }

  /// `page.consoleMessages(options?)`.
  ///
  /// # Errors
  ///
  /// Throws when reading `options.filter` throws, or when a `ConsoleMessage` instance cannot be created.
  #[qjs(rename = "consoleMessages")]
  pub fn console_messages<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<rquickjs::Value<'js>>> {
    let filter = ferridriver::observed::ObservedFilter::parse(opt_str_field(&options, "filter")?.as_deref());
    let msgs = self.inner.console_messages(filter);
    let mut out = Vec::with_capacity(msgs.len());
    for m in msgs {
      let instance =
        rquickjs::class::Class::instance(ctx.clone(), crate::bindings::console_message::ConsoleMessageJs::new(m))?;
      out.push(rquickjs::IntoJs::into_js(instance, &ctx)?);
    }
    Ok(out)
  }

  /// `page.clearConsoleMessages()`. Drops the retained console history.
  #[qjs(rename = "clearConsoleMessages")]
  pub fn clear_console_messages(&self) {
    self.inner.clear_console_messages();
  }

  /// `page.pageErrors(options?)`.
  ///
  /// # Errors
  ///
  /// Throws when reading `options.filter` throws, or when a native `Error` cannot be built.
  #[qjs(rename = "pageErrors")]
  pub fn page_errors<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<rquickjs::Value<'js>>> {
    let filter = ferridriver::observed::ObservedFilter::parse(opt_str_field(&options, "filter")?.as_deref());
    let errs = self.inner.page_errors(filter);
    let mut out = Vec::with_capacity(errs.len());
    for e in errs {
      out.push(crate::bindings::web_error::build_native_error(&ctx, e.error())?);
    }
    Ok(out)
  }

  /// `page.clearPageErrors()`. Drops the retained page-error history.
  #[qjs(rename = "clearPageErrors")]
  pub fn clear_page_errors(&self) {
    self.inner.clear_page_errors();
  }

  /// Playwright: `page.waitForNavigation(options?: { timeout?, url?,
  /// waitUntil? }): Promise<Response | null>`.
  #[qjs(rename = "waitForNavigation")]
  #[must_use]
  pub fn wait_for_navigation<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::promise::Promised<
    impl std::future::Future<
      Output = rquickjs::Result<crate::bindings::convert::Null<crate::bindings::network::ResponseJs>>,
    > + 'js,
  > {
    // Listening from the call: the navigation is usually started before
    // the promise is first awaited.
    let navigation = self.inner.wait_for_navigation();
    let opts = crate::bindings::convert::arm_now(&ctx, || parse_wait_for_navigation_options(&ctx, &options));
    let page = Arc::clone(&self.inner);
    rquickjs::promise::Promised::from(call_site.scope(async move {
      let opts = opts.map_err(|deferred| deferred.raise(&ctx))?;
      let response = navigation.options(opts).await.into_js_with(&ctx)?;
      Ok(crate::bindings::convert::Null(
        response.map(|r| crate::bindings::network::ResponseJs::new_with_page(r, page)),
      ))
    }))
  }

  /// `page.setExtraHTTPHeaders(headers)`. Sends `headers` (a plain
  /// `Record<string, string>`) with every subsequent request.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `headers` is not an object of strings, and with the core failure
  /// (`TimeoutError`, `TargetClosedError` or `FerriError`) when the headers cannot be set.
  #[qjs(rename = "setExtraHTTPHeaders")]
  pub async fn set_extra_http_headers<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    headers: rquickjs::Value<'js>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let map: rustc_hash::FxHashMap<String, String> = serde_from_js(&ctx, headers)?;
        self.inner.set_extra_http_headers(&map).await.into_js_with(&ctx)
      })
      .await
  }

  /// `page.bringToFront()`. Activates the page (brings its tab to front).
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the page cannot be
  /// activated.
  #[qjs(rename = "bringToFront")]
  pub async fn bring_to_front(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move { self.inner.bring_to_front().await.into_js_with(&ctx) })
      .await
  }

  /// Playwright: `page.viewportSize(): null | { width, height }` — a
  /// synchronous accessor over the tracked emulated viewport; `null`
  /// when no viewport emulation is applied.
  ///
  /// # Errors
  ///
  /// Throws when the result object cannot be created.
  #[qjs(rename = "viewportSize")]
  pub fn viewport_size<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    match self.inner.viewport_size() {
      None => Ok(rquickjs::Value::new_null(ctx)),
      Some((w, h)) => {
        let obj = rquickjs::Object::new(ctx.clone())?;
        obj.set("width", w)?;
        obj.set("height", h)?;
        Ok(obj.into_value())
      },
    }
  }

  /// `page.context()`. Returns the `BrowserContext` this page belongs to.
  #[qjs(rename = "context")]
  /// Playwright: `page.coverage`. Chromium only.
  #[qjs(get, rename = "coverage")]
  #[must_use]
  pub fn coverage(&self) -> crate::bindings::coverage::CoverageJs {
    crate::bindings::coverage::CoverageJs::new(self.inner.coverage())
  }

  /// `page.clock` — the owning context's fake-time controller
  /// (Playwright: `page.clock` IS `context.clock`).
  ///
  /// # Errors
  ///
  /// Throws when the `Clock` instance cannot be created in this context.
  #[qjs(get, rename = "clock")]
  pub fn clock<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let Some(cref) = self.inner.context() else {
      return Ok(rquickjs::Value::new_null(ctx));
    };
    let wrapper = crate::bindings::clock::ClockJs::new(std::sync::Arc::new(cref.clone()));
    let instance = rquickjs::class::Class::instance(ctx.clone(), wrapper)?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  /// `page.request` — the owning context's context-bound HTTP client
  /// (Playwright: `page.request` IS `context.request`). Cookies flow
  /// both ways between these requests and the browser context.
  ///
  /// # Errors
  ///
  /// Throws when the `APIRequestContext` instance cannot be created in this context.
  #[qjs(get, rename = "request")]
  pub fn request<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let Some(client) = self.inner.http_client() else {
      return Ok(rquickjs::Value::new_null(ctx));
    };
    let wrapper = crate::bindings::http_client::HttpClientJs::new(std::sync::Arc::new(client));
    let instance = rquickjs::class::Class::instance(ctx.clone(), wrapper)?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  /// # Errors
  ///
  /// Throws when the `BrowserContext` instance cannot be created in this context.
  pub fn context<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let Some(cref) = self.inner.context() else {
      return Ok(rquickjs::Value::new_null(ctx));
    };
    let wrapper = crate::bindings::context::BrowserContextJs::new(std::sync::Arc::new(cref.clone()));
    let instance = rquickjs::class::Class::instance(ctx.clone(), wrapper)?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  /// Playwright: `page.opener(): Promise<null | Page>` — the page that
  /// opened this popup via `window.open` / `target=_blank`; `null` for
  /// non-popup pages and once the opener has closed. The lookup itself
  /// is synchronous (a weak upgrade); `Promised` keeps Playwright's
  /// promise-returning signature.
  #[qjs(rename = "opener")]
  #[must_use]
  pub fn opener<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<rquickjs::Value<'js>>> + 'js> {
    let opener = self.inner.opener();
    rquickjs::promise::Promised::from(async move {
      let Some(opener) = opener else {
        return Ok(rquickjs::Value::new_null(ctx.clone()));
      };
      let instance = crate::bindings::identity::page_object(&ctx, opener)?;
      rquickjs::IntoJs::into_js(instance, &ctx)
    })
  }

  /// ferridriver-specific (NOT Playwright): click at viewport
  /// coordinates without a selector. Playwright equivalent: `mouse.click(x, y)`.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the click fails.
  #[qjs(rename = "clickAt")]
  pub async fn click_at(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
    x: f64,
    y: f64,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move { self.inner.click_at(x, y).await.into_js_with(&ctx) })
      .await
  }

  /// ferridriver-specific (NOT Playwright): interpolated mouse move
  /// from `(fromX, fromY)` to `(toX, toY)` in `steps` points. Playwright
  /// equivalent: `mouse.move(x, y, { steps })`.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the mouse cannot be
  /// moved.
  #[qjs(rename = "moveMouseSmooth")]
  pub async fn move_mouse_smooth(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
    steps: u32,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        self
          .inner
          .move_mouse_smooth(from_x, from_y, to_x, to_y, steps)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  // ── Emulation (page-scoped Playwright API) ───────────────────────────────

  /// Override the viewport size for this page. Playwright public:
  /// `page.setViewportSize({ width, height })`.
  /// Playwright: `page.setViewportSize({ width, height })` — a single
  /// object, not two positional numbers.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `size` is not `{ width, height }`, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the viewport cannot be resized.
  #[qjs(rename = "setViewportSize")]
  pub async fn set_viewport_size<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    size: rquickjs::Value<'js>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        #[derive(serde::Deserialize)]
        struct Size {
          width: i64,
          height: i64,
        }
        let s: Size = crate::bindings::convert::serde_from_js(&ctx, size)?;
        self.inner.set_viewport_size(s.width, s.height).await.into_js_with(&ctx)
      })
      .await
  }

  /// Emulate media features. Accepts Playwright's
  /// `{ media?, colorScheme?, reducedMotion?, forcedColors?, contrast? }`
  /// option bag — each call is a partial update layered on top of the
  /// page's persistent emulated-media state.
  ///
  /// # Errors
  ///
  /// Rejects when reading `options` throws, and with the core failure (`TimeoutError`, `TargetClosedError` or
  /// `FerriError`) when the emulation cannot be applied.
  #[qjs(rename = "emulateMedia")]
  pub async fn emulate_media<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts = parse_emulate_media_options(&ctx, options)?;
        self.inner.emulate_media().options(opts).await.into_js_with(&ctx)
      })
      .await
  }

  // ── Screenshots / PDF (return raw bytes; pair with `artifacts.writeBytes`) ─

  /// Capture the page as a PNG (raw bytes — `Uint8Array` in JS). Pair with
  /// `await artifacts.writeBytes('page.png', bytes)` to save to disk.
  /// Optional `options` accept `{ fullPage?: boolean, format?: 'png'|'jpeg'|'webp', quality?: number }`.
  /// ferridriver extension: `page.checkAccessibility(options?)`.
  ///
  /// Not a Playwright method — Playwright's `page.accessibility` is the
  /// accessibility TREE, which is a different question. This runs
  /// axe-core and reports what it found.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the audit fails.
  #[qjs(rename = "checkAccessibility")]
  pub async fn check_accessibility<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let parsed: JsAccessibilityOptions = match options.into_inner() {
          Some(value) if !value.is_undefined() && !value.is_null() => serde_from_js(&ctx, value)?,
          _ => JsAccessibilityOptions::default(),
        };
        let report = self
          .inner
          .check_accessibility()
          .options(ferridriver::accessibility::AccessibilityOptions {
            include: parsed.include.unwrap_or_default(),
            exclude: parsed.exclude.unwrap_or_default(),
            tags: parsed.tags.unwrap_or_default(),
          })
          .await
          .into_js_with(&ctx)?;
        crate::bindings::convert::serde_to_js(&ctx, &report)
      })
      .await
  }

  /// ferridriver extension: `page.developerTools()`.
  ///
  /// The tools the page offers about itself, from its own answer to a
  /// `devtoolstooldiscovery` event. Empty where it offers none.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the tool list cannot be
  /// read.
  #[qjs(rename = "developerTools")]
  pub async fn developer_tools<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let groups = self.inner.developer_tools().await.into_js_with(&ctx)?;
        crate::bindings::convert::serde_to_js(&ctx, &groups)
      })
      .await
  }

  /// ferridriver extension: `page.executeDeveloperTool(name, params?)`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `args` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the tool fails.
  #[qjs(rename = "executeDeveloperTool")]
  pub async fn execute_developer_tool<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    name: String,
    params: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let parsed: Option<serde_json::Value> = match params.into_inner() {
          Some(value) if !value.is_undefined() && !value.is_null() => Some(serde_from_js(&ctx, value)?),
          _ => None,
        };
        let result = self
          .inner
          .execute_developer_tool(&name, parsed)
          .await
          .into_js_with(&ctx)?;
        crate::bindings::convert::json_to_js(&ctx, &result)
      })
      .await
  }

  #[qjs(get)]
  #[must_use]
  pub fn webmcp(&self) -> crate::bindings::web_mcp::WebMcpJs {
    crate::bindings::web_mcp::WebMcpJs::new(self.inner.webmcp())
  }

  /// ferridriver extension: `page.takeHeapSnapshot()`.
  ///
  /// Captures a V8 heap snapshot, garbage collected first, and hands
  /// back a handle with the queries on it. Chromium-only: the format is
  /// V8's, so `webkit` and `bidi` throw `Unsupported`.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`), a `FerriError` off
  /// Chromium, when the snapshot cannot be taken.
  #[qjs(rename = "takeHeapSnapshot")]
  pub async fn take_heap_snapshot<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let snapshot = self.inner.take_heap_snapshot().await.into_js_with(&ctx)?;
        let instance =
          rquickjs::class::Class::instance(ctx.clone(), crate::bindings::heap::HeapSnapshotJs::new(snapshot))?;
        rquickjs::IntoJs::into_js(instance, &ctx)
      })
      .await
  }

  /// ferridriver extension: `page.checkPageQuality(options?)`.
  ///
  /// The ten Lighthouse audits that score a page as it stands. Pass
  /// `{ only: ['meta-description'] }` to run a subset. `http-status-code`
  /// and `is-crawlable` need the main document's own response and are
  /// absent for a document that arrived without one.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the check fails.
  #[qjs(rename = "checkPageQuality")]
  pub async fn check_page_quality<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let parsed: JsPageQualityOptions = match options.into_inner() {
          Some(value) if !value.is_undefined() && !value.is_null() => serde_from_js(&ctx, value)?,
          _ => JsPageQualityOptions::default(),
        };
        let report = self
          .inner
          .check_page_quality()
          .options(ferridriver::audits::PageQualityOptions {
            only: parsed.only.unwrap_or_default(),
          })
          .await
          .into_js_with(&ctx)?;
        crate::bindings::convert::serde_to_js(&ctx, &report)
      })
      .await
  }

  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the capture fails.
  #[qjs(rename = "screenshot")]
  pub async fn screenshot<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<u8>> {
    call_site
      .scope(async move {
        let opts = parse_screenshot_options(&ctx, options)?;
        self.inner.screenshot().options(opts).await.into_js_with(&ctx)
      })
      .await
  }

  /// Capture a single element as PNG bytes.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the element cannot be
  /// captured.
  #[qjs(rename = "screenshotElement")]
  pub async fn screenshot_element(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
    selector: String,
  ) -> rquickjs::Result<Vec<u8>> {
    call_site
      .scope(async move { self.inner.screenshot_element(&selector).await.into_js_with(&ctx) })
      .await
  }

  /// Render the current page as a PDF (raw bytes). Accepts a Playwright-shape
  /// options object: `{ format?, landscape?, printBackground?, scale?, ... }`.
  /// Pair with `await artifacts.writeBytes('page.pdf', bytes)` to save.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`), a `FerriError` off Chromium, when the PDF cannot be produced.
  #[qjs(rename = "pdf")]
  pub async fn pdf<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<u8>> {
    call_site
      .scope(async move {
        let opts = parse_pdf_options(&ctx, options)?;
        self.inner.pdf().options(opts).await.into_js_with(&ctx)
      })
      .await
  }

  /// Start the Chromium `DevTools` performance trace. Chromium only; the
  /// returned events are consumed by `stopTracing`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `categories` is not an array of strings, and with the core failure
  /// (`TimeoutError`, `TargetClosedError` or `FerriError`) when the trace cannot start.
  #[qjs(rename = "startTracing")]
  pub async fn start_tracing<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    categories: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let categories = match categories.into_inner() {
          Some(value) if !value.is_undefined() && !value.is_null() => Some(serde_from_js::<Vec<String>>(&ctx, value)?),
          _ => None,
        };
        self.inner.start_tracing(categories.as_deref()).await.into_js_with(&ctx)
      })
      .await
  }

  /// End the Chromium `DevTools` performance trace and return its events.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the trace cannot be
  /// stopped.
  #[qjs(rename = "stopTracing")]
  pub async fn stop_tracing<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let events = self.inner.stop_tracing().await.into_js_with(&ctx)?;
        serde_to_js(&ctx, &events)
      })
      .await
  }

  /// Read the Chromium `DevTools` performance counters for this page.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the counters cannot be
  /// read.
  #[qjs(rename = "metrics")]
  pub async fn metrics<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let metrics = self.inner.metrics().await.into_js_with(&ctx)?;
        serde_to_js(&ctx, &metrics)
      })
      .await
  }

  /// Start Chromium's precise JavaScript coverage counters.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, an `Error` when coverage is already started, and the
  /// core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the page has no browser context or the
  /// coverage session fails.
  #[qjs(rename = "startJSCoverage")]
  pub async fn start_js_coverage<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let options = match options.into_inner() {
          Some(value) if !value.is_undefined() && !value.is_null() => serde_from_js(&ctx, value)?,
          _ => serde_json::json!({}),
        };
        let context = self
          .inner
          .context()
          .ok_or_else(|| ferridriver::FerriError::unsupported("JavaScript coverage requires a browser context"))
          .into_js_with(&ctx)?;
        let mut coverage = self.coverage_session.lock().await;
        if coverage.is_some() {
          return Err(throw_named(
            &ctx,
            "Error",
            "page.startJSCoverage: JavaScript coverage is already started",
          ));
        }
        let session = context.new_cdp_session(&self.inner).await.into_js_with(&ctx)?;
        session
          .send("Profiler.enable", serde_json::json!({}))
          .await
          .into_js_with(&ctx)?;
        let result = session
          .send(
            "Profiler.startPreciseCoverage",
            serde_json::json!({
              "callCount": options.get("callCount").and_then(serde_json::Value::as_bool).unwrap_or(false),
              "detailed": options.get("detailed").and_then(serde_json::Value::as_bool).unwrap_or(true),
              "allowTriggeredUpdates": options
                .get("allowTriggeredUpdates")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            }),
          )
          .await
          .into_js_with(&ctx)?;
        *coverage = Some(session);
        crate::bindings::convert::json_to_js(&ctx, &result)
      })
      .await
  }

  /// Collect and reset Chromium's precise JavaScript coverage counters.
  ///
  /// # Errors
  ///
  /// Rejects with an `Error` when coverage has not been started, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the coverage cannot be read.
  #[qjs(rename = "takeJSCoverage")]
  pub async fn take_js_coverage<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let coverage = self.coverage_session.lock().await;
        let Some(session) = coverage.as_ref() else {
          return Err(throw_named(
            &ctx,
            "Error",
            "page.takeJSCoverage: JavaScript coverage has not been started",
          ));
        };
        let result = session
          .send("Profiler.takePreciseCoverage", serde_json::json!({}))
          .await
          .into_js_with(&ctx)?;
        crate::bindings::convert::json_to_js(&ctx, &result)
      })
      .await
  }

  /// Stop Chromium's precise JavaScript coverage counters.
  ///
  /// # Errors
  ///
  /// Rejects with an `Error` when coverage has not been started, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the coverage cannot be read or stopped.
  #[qjs(rename = "stopJSCoverage")]
  pub async fn stop_js_coverage<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let mut coverage = self.coverage_session.lock().await;
        let Some(session) = coverage.take() else {
          return Err(throw_named(
            &ctx,
            "Error",
            "page.stopJSCoverage: JavaScript coverage has not been started",
          ));
        };
        let result = session
          .send("Profiler.stopPreciseCoverage", serde_json::json!({}))
          .await
          .into_js_with(&ctx)?;
        session
          .send("Profiler.disable", serde_json::json!({}))
          .await
          .into_js_with(&ctx)?;
        crate::bindings::convert::json_to_js(&ctx, &result)
      })
      .await
  }

  /// Start Chromium's sampled JavaScript CPU profiler.
  ///
  /// # Errors
  ///
  /// Rejects with an `Error` when profiling is already started, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the page has no browser context or the profiler session fails.
  #[qjs(rename = "startCPUProfile")]
  pub async fn start_cpu_profile<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let context = self
          .inner
          .context()
          .ok_or_else(|| ferridriver::FerriError::unsupported("CPU profiling requires a browser context"))
          .into_js_with(&ctx)?;
        let mut profiler = self.cpu_profile_session.lock().await;
        if profiler.is_some() {
          return Err(throw_named(
            &ctx,
            "Error",
            "page.startCPUProfile: CPU profiling is already started",
          ));
        }
        let session = context.new_cdp_session(&self.inner).await.into_js_with(&ctx)?;
        session
          .send("Profiler.enable", serde_json::json!({}))
          .await
          .into_js_with(&ctx)?;
        let result = session
          .send("Profiler.start", serde_json::json!({}))
          .await
          .into_js_with(&ctx)?;
        *profiler = Some(session);
        crate::bindings::convert::json_to_js(&ctx, &result)
      })
      .await
  }

  /// Stop Chromium's sampled JavaScript CPU profiler and return its profile.
  ///
  /// # Errors
  ///
  /// Rejects with an `Error` when profiling has not been started, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the profile cannot be read or stopped.
  #[qjs(rename = "stopCPUProfile")]
  pub async fn stop_cpu_profile<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let mut profiler = self.cpu_profile_session.lock().await;
        let Some(session) = profiler.take() else {
          return Err(throw_named(
            &ctx,
            "Error",
            "page.stopCPUProfile: CPU profiling has not been started",
          ));
        };
        let result = session
          .send("Profiler.stop", serde_json::json!({}))
          .await
          .into_js_with(&ctx)?;
        session
          .send("Profiler.disable", serde_json::json!({}))
          .await
          .into_js_with(&ctx)?;
        crate::bindings::convert::json_to_js(&ctx, &result)
      })
      .await
  }

  // ── Lifecycle ─────────────────────────────────────────────────────────────

  /// Close the page. Accepts `{ runBeforeUnload?, reason? }` to mirror
  /// Playwright's `page.close(options?)`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the page cannot be closed.
  #[qjs(rename = "close")]
  pub async fn close<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts = parse_page_close_options(&ctx, options)?;
        self.inner.close().maybe_options(opts).await.into_js_with(&ctx)?;
        // Let the event pump (same executor) drain the 'close' emission so
        // close-listeners still find their callbacks in the registry, then
        // release this page's persisted `page.on` callbacks — the session
        // VM (and its userdata registry) outlives the page, so without
        // this the `Persistent`s would sit in the registry for the VM's
        // remaining life.
        tokio::task::yield_now().await;
        let page_key = self.inner.backend_page_id();
        let ids = with_page_callbacks(&ctx, |r| {
          r.remove_routes_for_owner(&RouteOwner::Page(page_key));
          r.remove_ws_callbacks_for_owner(&RouteOwner::Page(page_key));
          r.remove_exposed_for_page(page_key);
          r.remove_event_listeners_for_page(page_key)
        })?;
        for id in ids {
          self.inner.off(ferridriver::events::ListenerId(id));
        }
        Ok(())
      })
      .await
  }

  /// Set the default timeout for all non-navigation operations
  /// (milliseconds). Mirrors Playwright's `page.setDefaultTimeout(timeout)`.
  #[qjs(rename = "setDefaultTimeout")]
  pub fn set_default_timeout(&self, ms: u64) {
    self.inner.set_default_timeout(ms);
  }

  /// Set the default timeout for navigation-family operations
  /// (`goto`, `reload`, `goBack`, `goForward`, `waitForUrl`). Mirrors
  /// Playwright's `page.setDefaultNavigationTimeout(timeout)`.
  #[qjs(rename = "setDefaultNavigationTimeout")]
  pub fn set_default_navigation_timeout(&self, ms: u64) {
    self.inner.set_default_navigation_timeout(ms);
  }

  /// Whether the page has been closed.
  #[qjs(rename = "isClosed")]
  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }

  // ── Network interception ─────────────────────────────────────────────────

  /// Mirrors Playwright `page.route(url, handler)`. Registers a JS
  /// callback to intercept requests matching `url` (`string | RegExp`).
  /// The callback receives a `Route` instance and must call exactly one
  /// of `route.fulfill()`, `route.continue()`, or `route.abort()` to
  /// resume the request.
  ///
  /// Cross-task dispatch: the Rust route handler runs inside the
  /// backend's network listener (a separate tokio task from the
  /// script's JS context). The handler stashes the JS callback in the
  /// native `RouteRegistry` userdata keyed by ID at registration
  /// time; when a request matches, the handler spawns a task that
  /// `async_with`s back into the script's `AsyncContext`, looks up the
  /// callback by ID, and invokes it with a fresh `RouteJs` wrapper.
  /// `rquickjs`'s scheduler serialises the dispatch against the
  /// script's own `await` points so JS-side state stays consistent.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `url` is neither a glob, a `RegExp` nor a predicate, and an `Error` when the page has
  /// no VM handle. The promise rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`)
  /// when the route cannot be registered.
  #[qjs(rename = "route")]
  pub fn route<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    url: rquickjs::Value<'js>,
    handler: rquickjs::Function<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<
    rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<rquickjs::Value<'js>>> + 'js>,
  > {
    let times = parse_route_times(&options)?;
    let vm = self.vm.clone().ok_or_else(|| {
      throw_named(
        &ctx,
        "Error",
        "page.route requires the script engine's VM handle (install_page)",
      )
    })?;
    let id = with_page_callbacks(&ctx, PageCallbacks::next_route_id)?;
    // Snapshot the registrar's grant NOW (sync prologue, on the tool's
    // stack) — see `SavedCallback::save`. The handler AND predicate keep
    // this tool's `allow.net` when they later fire cross-task.
    let saved_handler = SavedCallback::save(&ctx, handler);

    // A JS predicate is `!Send` and core matches on the CDP recv task,
    // so it can't ride `UrlMatcher::Predicate`. Register an always-true
    // matcher with unique `Arc` identity (lets `unroute(fn)` drop
    // exactly it via `Arc::ptr_eq`); evaluate the predicate in the
    // dispatch bridge and continue the request unmodified on falsy.
    let has_predicate = url.as_function().is_some();
    let (matcher, saved_pred, registry_matcher) = if let Some(pred) = url.as_function() {
      let saved_pred = SavedCallback::save(&ctx, pred.clone());
      let m = ferridriver::url_matcher::UrlMatcher::predicate(|_| true);
      (m.clone(), Some(saved_pred), Some(m))
    } else {
      (url_value_to_matcher(&ctx, &url)?, None, None)
    };

    // LIMITATION (persistent-session VMs): this closure captures a clone
    // of the session's `AsyncContext`. Core route registrations live on
    // the page (independent of the JS VM), so they outlive a poisoning
    // rebuild / LRU eviction of the session VM. After such a discard the
    // closure dispatches into the now-detached old context; the new VM's
    // scripts cannot see or `unroute` it. It stays memory-safe (the Arc
    // keeps the old context alive) and fail-open (the route's `Drop`
    // continues the request if dispatch can't reach JS), and it clears
    // when the page closes. Fully reconciling it needs a cross-backend
    // "unroute all" on VM discard — tracked, not yet implemented.
    let rust_handler: ferridriver::route::RouteHandler = std::sync::Arc::new(move |route| {
      let vm = vm.clone();
      // Cross-task dispatch: submit a job to the session's VM event
      // loop (rule 2 of the re-entry discipline on `PageEventPumpUd`).
      // The loop is always alive, so this works while the VM idles
      // between executes AND while a script is parked on a host await.
      // Errors are swallowed because the route's own `Drop` (fail-open
      // continue) covers the case where dispatch can't reach JS.
      tokio::spawn(async move {
        use rquickjs::class::Class;
        let _: Result<rquickjs::Result<()>, crate::error::ScriptError> = crate::vm_with!(vm => |ctx| {
          if has_predicate {
            let saved_pred = with_page_callbacks(&ctx, |r| r.get_route_pred(id))?
              .ok_or_else(|| throw_named(&ctx, "Error", "page.route: route predicate gone"))?;
            let pred = saved_pred.restore(&ctx)?;
            let url_ctor: rquickjs::function::Constructor<'_> = ctx.globals().get("URL")?;
            let url_obj: rquickjs::Value<'_> = url_ctor.construct((route.request().url.clone(),))?;
            let truthy = call_predicate_truthy(&pred, url_obj, &ctx)
            .await?;
            if !truthy {
              route.reject_as_unmatched();
              return Ok(());
            }
          }
          let f = with_page_callbacks(&ctx, |r| r.get_route_handler(id))?
            .ok_or_else(|| throw_named(&ctx, "Error", "page.route: route handler gone"))?;
          let route_class = Class::instance(ctx.clone(), crate::bindings::network::RouteJs::new(route))?;
          // `call_bracketed_async`, not `call_bracketed`: a route handler
          // is typically `async`, so its `fetch`/`request` runs in a
          // continuation off the synchronous call — the grant must cover
          // every poll of the returned promise, not only the call.
          let _: rquickjs::Value<'_> = f.call_bracketed_async(&ctx, (route_class,)).await?;
          Ok(())
        })
        .await;
      });
    });

    // AFTER the core handler exists: its `Arc` identity is what
    // `unroute(url, handler)` names, so the registry has to remember it
    // alongside the JS function the caller passed.
    let handler_id = ferridriver::route::route_handler_id(&rust_handler);
    with_page_callbacks(&ctx, |r| {
      r.insert_route(
        id,
        self.route_owner(),
        saved_handler,
        handler_id,
        saved_pred,
        registry_matcher,
      );
    })?;

    let inner = Arc::clone(&self.inner);
    Ok(rquickjs::promise::Promised::from(async move {
      let disposable = inner.route(matcher, rust_handler, times).await.into_js_with(&ctx)?;
      let instance =
        rquickjs::class::Class::instance(ctx.clone(), crate::bindings::disposable::DisposableJs::new(disposable))?;
      rquickjs::IntoJs::into_js(instance, &ctx)
    }))
  }

  /// Playwright: `page.routeWebSocket(url, handler)`. Intercepts
  /// WebSocket connections matching `url` (glob string or `RegExp`); the
  /// handler receives a live `WebSocketRoute`. Cross-task dispatch mirrors
  /// `page.route` — the handler is restored + invoked inside `async_with!`,
  /// awaited so the driver observes `connectToServer()` before opening.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `url` is neither a glob nor a `RegExp`, and an `Error` when the page has no VM handle.
  /// The promise rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the route
  /// cannot be registered.
  #[qjs(rename = "routeWebSocket")]
  pub fn route_web_socket<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    url: rquickjs::Value<'js>,
    handler: rquickjs::Function<'js>,
  ) -> rquickjs::Result<rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<()>> + 'js>> {
    let vm = self.vm.clone().ok_or_else(|| {
      throw_named(
        &ctx,
        "Error",
        "page.routeWebSocket requires the script engine's VM handle (install_page)",
      )
    })?;
    let matcher = url_value_to_matcher(&ctx, &url)?;
    let handler_id = with_page_callbacks(&ctx, PageCallbacks::next_route_id)?;
    let owner = RouteOwner::Page(self.inner.backend_page_id());
    // Sync prologue: snapshot the registrar's grant (see `SavedCallback::save`).
    let saved = SavedCallback::save(&ctx, handler);
    with_page_callbacks(&ctx, |r| r.insert_ws_callback(handler_id, owner.clone(), saved))?;

    let rust_handler = crate::bindings::web_socket_route::build_ws_route_handler(vm, handler_id, owner);
    let inner = Arc::clone(&self.inner);
    Ok(rquickjs::promise::Promised::from(async move {
      inner.route_web_socket(matcher, rust_handler).await.into_js_with(&ctx)
    }))
  }

  /// Playwright: `page.routeFromHAR(har, options?)`. Replay-only.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the archive cannot be served. A HAR path the sandbox does not let this
  /// script read (or write, with `update`) is refused first.
  #[qjs(rename = "routeFromHAR")]
  pub async fn route_from_har<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    har: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts = parse_har_options(&ctx, &options)?;
        check_har_paths(&ctx, &har, &opts)?;
        self
          .inner
          .route_from_har(std::path::Path::new(&har))
          .options(opts)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// `page.unroute(string | RegExp | ((url: URL) => boolean))`. A
  /// predicate is matched by `===` identity against the function passed
  /// to `route`, then its always-true core matcher is dropped by `Arc`
  /// identity so sibling predicate routes survive.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `url` is neither a glob, a `RegExp` nor a predicate, and with the core failure
  /// (`TimeoutError`, `TargetClosedError` or `FerriError`) when the route cannot be removed.
  #[qjs(rename = "unroute")]
  pub async fn unroute<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    url: rquickjs::Value<'js>,
    handler: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        // Playwright's second argument: remove only the registration
        // made with THIS handler, leaving another handler on the same
        // pattern serving. Without it, every route for the pattern goes.
        let handler_fn = handler.0.as_ref().and_then(|v| v.as_function().cloned());
        let mut handler_id: Option<usize> = None;
        if let Some(handler_fn) = handler_fn {
          let target = handler_fn.into_value();
          for (_, saved, core_id) in with_page_callbacks(&ctx, |r| r.routes_for_owner(&self.route_owner()))? {
            if saved.restore(&ctx)?.into_value() == target {
              handler_id = Some(core_id);
              break;
            }
          }
          if handler_id.is_none() {
            // Nothing was registered with that function: Playwright
            // treats this as a no-op rather than an error.
            return Ok(());
          }
        }
        if let Some(pred) = url.as_function() {
          // Find every id (registered through THIS page, from any of its
          // wrappers) whose stored predicate is identical (===) to the
          // passed function, then drop its core registration + registry
          // entry. Restoring each saved predicate yields a handle to the
          // same underlying object, so `Value` `PartialEq` (tag + pointer)
          // is still strict `===` identity.
          let saved = with_page_callbacks(&ctx, |r| r.predicate_routes_for_owner(&self.route_owner()))?;
          let mut victims: Vec<u64> = Vec::new();
          for (id, sp) in saved {
            let stored = sp.restore(&ctx)?;
            if stored.as_value() == pred.as_value() {
              victims.push(id);
            }
          }
          for id in victims {
            let m = with_page_callbacks(&ctx, |r| r.remove_route(id))?;
            if let Some(m) = m {
              self.inner.unroute(&m, handler_id).await.into_js_with(&ctx)?;
            }
          }
          return Ok(());
        }
        let matcher = url_value_to_matcher(&ctx, &url)?;
        self.inner.unroute(&matcher, handler_id).await.into_js_with(&ctx)
      })
      .await
  }

  /// `page.unrouteAll(options?: { behavior?: 'wait' | 'ignoreErrors' | 'default' })`.
  /// Removes every route registered via `page.route`, clearing the script-side
  /// predicate/handler tables too.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options.behavior` is not `'wait'`, `'ignoreErrors'` or `'default'`, and with
  /// the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the routes cannot be removed.
  #[qjs(rename = "unrouteAll")]
  pub async fn unroute_all<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let behavior = match options.0.and_then(rquickjs::Value::into_object) {
          Some(obj) => match obj.get::<_, Option<String>>("behavior")? {
            Some(b) => Some(parse_unroute_behavior(&ctx, &b)?),
            None => None,
          },
          None => None,
        };
        self.inner.unroute_all(behavior).await.into_js_with(&ctx)?;
        with_page_callbacks(&ctx, |r| r.remove_routes_for_owner(&self.route_owner()))?;
        Ok(())
      })
      .await
  }

  /// `page.addLocatorHandler(locator, handler, options?: { times?, noWaitAfter? })`.
  /// Registers `handler` to run whenever `locator` becomes visible during an
  /// actionability wait (dismissing overlays/modals). Mirrors Playwright
  /// `client/page.ts:397`.
  ///
  /// The JS handler runs cross-task via the session VM's job queue (same
  /// bridge as `page.route`): the triggering action is parked on a host
  /// await, so the interpreter is free to run the queued handler job; the
  /// core checkpoint awaits a oneshot the dispatch task fulfils once the
  /// handler (and any returned promise) settles, so the original action
  /// only resumes afterwards.
  ///
  /// # Errors
  ///
  /// Throws an `Error` when the page has no VM handle, and the core failure (`TimeoutError`, `TargetClosedError` or
  /// `FerriError`) when core refuses the handler.
  #[qjs(rename = "addLocatorHandler")]
  pub fn add_locator_handler<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    locator: rquickjs::Class<'js, LocatorJs>,
    handler: rquickjs::Function<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let mut times: Option<u32> = None;
    let mut no_wait_after = false;
    if let Some(bag) = options.0.as_ref().and_then(rquickjs::Value::as_object) {
      if let Ok(Some(t)) = bag.get::<_, Option<f64>>("times") {
        times = Some(crate::bindings::convert::f64_to_u32(t));
      }
      if let Ok(Some(n)) = bag.get::<_, Option<bool>>("noWaitAfter") {
        no_wait_after = n;
      }
    }
    let vm = self.vm.clone().ok_or_else(|| {
      throw_named(
        &ctx,
        "Error",
        "page.addLocatorHandler requires the script engine's VM handle (install_page)",
      )
    })?;
    let id = with_page_callbacks(&ctx, PageCallbacks::next_route_id)?;
    let saved = SavedCallback::save(&ctx, handler);
    with_page_callbacks(&ctx, |r| r.insert_locator_handler(id, saved))?;

    let core_locator = locator.borrow().inner_ref().clone();
    let rust_handler: ferridriver::locator_handler::LocatorHandlerFn = std::sync::Arc::new(move |loc| {
      let vm = vm.clone();
      Box::pin(async move {
        let (tx, rx) = tokio::sync::oneshot::channel::<ferridriver::error::Result<()>>();
        tokio::spawn(async move {
          use rquickjs::class::Class;
          let result: Result<rquickjs::Result<()>, crate::error::ScriptError> = crate::vm_with!(vm => |ctx| {
            let f = with_page_callbacks(&ctx, |r| r.get_locator_handler(id))?.ok_or_else(|| {
              throw_named(&ctx, "Error", "page.addLocatorHandler: locator handler gone")
            })?;
            let loc_class = Class::instance(ctx.clone(), LocatorJs::new(loc))?;
            let _: rquickjs::Value<'_> = f.call_bracketed_async(&ctx, (loc_class,)).await?;
            Ok(())
          })
          .await;
          let mapped = match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(ferridriver::error::FerriError::evaluation(format!(
              "locator handler failed: {e}"
            ))),
            Err(e) => Err(ferridriver::error::FerriError::evaluation(format!(
              "locator handler dispatch failed: {}",
              e.message
            ))),
          };
          let _ = tx.send(mapped);
        });
        match rx.await {
          Ok(r) => r,
          Err(_) => Err(ferridriver::error::FerriError::evaluation(
            "locator handler dispatch task dropped",
          )),
        }
      })
    });

    self
      .inner
      .add_locator_handler(&core_locator, rust_handler, times, no_wait_after)
      .into_js_with(&ctx)?;
    self
      .locator_handler_ids
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .entry(core_locator.selector().to_string())
      .or_default()
      .push(id);
    Ok(())
  }

  /// `page.removeLocatorHandler(locator)`. Drops every handler registered for
  /// `locator` (by selector) and releases the persisted JS callbacks. Mirrors
  /// Playwright `client/page.ts:423`.
  ///
  /// # Errors
  ///
  /// Throws an `Error` when the context's page-callback registry is missing.
  #[qjs(rename = "removeLocatorHandler")]
  pub fn remove_locator_handler<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    locator: rquickjs::Class<'js, LocatorJs>,
  ) -> rquickjs::Result<()> {
    let core_locator = locator.borrow().inner_ref().clone();
    self.inner.remove_locator_handler(&core_locator);
    let ids = self
      .locator_handler_ids
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(core_locator.selector())
      .unwrap_or_default();
    with_page_callbacks(&ctx, |r| {
      for id in ids {
        r.remove_locator_handler(id);
      }
    })?;
    Ok(())
  }

  /// `page.pickLocator(): Promise<Locator>`. Highlights elements under the
  /// cursor and resolves with a Locator for the element the user clicks.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when picking fails or is
  /// cancelled.
  #[qjs(rename = "pickLocator")]
  pub async fn pick_locator(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
  ) -> rquickjs::Result<LocatorJs> {
    call_site
      .scope(async move {
        let loc = self.inner.pick_locator().await.into_js_with(&ctx)?;
        Ok(LocatorJs::new(loc))
      })
      .await
  }

  /// `page.cancelPickLocator(): Promise<void>`.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the pick cannot be
  /// cancelled.
  #[qjs(rename = "cancelPickLocator")]
  pub async fn cancel_pick_locator(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move { self.inner.cancel_pick_locator().await.into_js_with(&ctx) })
      .await
  }

  /// `page.hideHighlight(): Promise<void>`.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the highlight cannot be
  /// hidden.
  #[qjs(rename = "hideHighlight")]
  pub async fn hide_highlight(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move { self.inner.hide_highlight().await.into_js_with(&ctx) })
      .await
  }

  // ── Network lifecycle waits ──────────────────────────────────────────────
  //
  // Mirror Playwright's `page.waitForRequest` / `page.waitForResponse` /
  // `page.waitForEvent('websocket')` — return live `RequestJs` /
  // `ResponseJs` / `WebSocketJs` so callers can inspect headers, body,
  // failure, etc.

  /// `page.waitForRequest(string | RegExp | ((r: Request) => boolean |
  /// Promise<boolean>), options?: { timeout? })`.
  #[qjs(rename = "waitForRequest")]
  #[must_use]
  pub fn wait_for_request<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    url: rquickjs::Value<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::promise::Promised<
    impl std::future::Future<Output = rquickjs::Result<crate::bindings::network::RequestJs>> + 'js,
  > {
    let page = Arc::clone(&self.inner);
    // Listening starts here, before the promise is returned: the caller's
    // next statement may already send the request.
    let wait = crate::bindings::convert::arm_now(&ctx, || {
      let timeout = parse_wait_timeout(&ctx, options)?;
      let wait = match url.as_function() {
        Some(pred) => NetworkWait::Predicate(page.events().subscribe(), pred.clone()),
        None => NetworkWait::Matched(Box::pin(
          page.wait_for_request(url_value_to_matcher(&ctx, &url)?, timeout),
        )),
      };
      Ok((timeout, wait))
    });
    rquickjs::promise::Promised::from(call_site.scope(async move {
      let (timeout, wait) = wait.map_err(|deferred| deferred.raise(&ctx))?;
      match wait {
        NetworkWait::Predicate(rx, pred) => {
          let t = timeout.unwrap_or_else(|| page.default_timeout());
          wait_request_predicate(ctx.clone(), page, rx, pred, t).await
        },
        NetworkWait::Matched(request) => Ok(crate::bindings::network::RequestJs::new_with_page(
          request.await.into_js_with(&ctx)?,
          page,
        )),
      }
    }))
  }

  /// `page.waitForResponse(string | RegExp | ((r: Response) => boolean |
  /// Promise<boolean>), options?: { timeout? })`.
  #[qjs(rename = "waitForResponse")]
  #[must_use]
  pub fn wait_for_response<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    url: rquickjs::Value<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::promise::Promised<
    impl std::future::Future<Output = rquickjs::Result<crate::bindings::network::ResponseJs>> + 'js,
  > {
    let page = Arc::clone(&self.inner);
    let wait = crate::bindings::convert::arm_now(&ctx, || {
      let timeout = parse_wait_timeout(&ctx, options)?;
      let wait = match url.as_function() {
        Some(pred) => NetworkWait::Predicate(page.events().subscribe(), pred.clone()),
        None => NetworkWait::Matched(Box::pin(
          page.wait_for_response(url_value_to_matcher(&ctx, &url)?, timeout),
        )),
      };
      Ok((timeout, wait))
    });
    rquickjs::promise::Promised::from(call_site.scope(async move {
      let (timeout, wait) = wait.map_err(|deferred| deferred.raise(&ctx))?;
      match wait {
        NetworkWait::Predicate(rx, pred) => {
          let t = timeout.unwrap_or_else(|| page.default_timeout());
          wait_response_predicate(ctx.clone(), page, rx, pred, t).await
        },
        NetworkWait::Matched(response) => Ok(crate::bindings::network::ResponseJs::new_with_page(
          response.await.into_js_with(&ctx)?,
          page,
        )),
      }
    }))
  }

  /// `page.waitForEvent(event, optionsOrPredicate?)`. The second
  /// argument is a predicate function, a `{ predicate?, timeout? }`
  /// bag (Playwright shape), or a bare timeout in ms (ferridriver
  /// extension). The predicate receives the same live event object a
  /// `page.on` listener would and the wait resolves on the first event
  /// for which it returns truthy.
  #[qjs(rename = "waitForEvent")]
  #[must_use]
  pub fn wait_for_event<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    event: String,
    options_or_predicate: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<rquickjs::Value<'js>>> + 'js> {
    use rquickjs::class::Class;
    let page = Arc::clone(&self.inner);
    let event_lc = event.to_ascii_lowercase();
    let options = crate::bindings::convert::arm_now(&ctx, || {
      let mut timeout_ms: Option<f64> = None;
      let mut predicate: Option<rquickjs::Function<'js>> = None;
      if let Some(v) = options_or_predicate.0 {
        if let Some(n) = v.as_number() {
          timeout_ms = Some(n);
        } else if let Some(f) = v.as_function() {
          predicate = Some(f.clone());
        } else if let Some(obj) = v.as_object() {
          let t: rquickjs::Value<'js> = obj.get("timeout")?;
          timeout_ms = t.as_number();
          let p: rquickjs::Value<'js> = obj.get("predicate")?;
          predicate = p.as_function().cloned();
        }
      }
      Ok((timeout_ms, predicate))
    });
    // Listening starts here, not at the promise's first poll: the caller's
    // next statement can fire the event while nothing has polled it yet (a
    // script that ends without awaiting, a run of synchronous code).
    // `dialog`, `filechooser` and `download` without a predicate claim the
    // live handle through a one-shot handler on the page's manager, which
    // takes it synchronously with the backend event (Playwright's
    // `addDialogHandler` + `dialogDidOpen` flow).
    let armed = options.map(|(timeout_ms, predicate)| {
      let timeout = timeout_ms.map_or_else(|| page.default_timeout(), crate::bindings::convert::ms_f64_to_u64);
      let armed = match (predicate.is_none(), event_lc.as_str()) {
        (true, "dialog") => Armed::Dialog(Box::pin(page.wait_for_dialog(timeout))),
        (true, "filechooser") => Armed::FileChooser(Box::pin(page.wait_for_file_chooser(timeout))),
        (true, "download") => Armed::Download(Box::pin(page.wait_for_download(timeout))),
        _ => Armed::Events(page.events().subscribe()),
      };
      (timeout, predicate, armed)
    });

    rquickjs::promise::Promised::from(call_site.scope(async move {
      let (timeout, predicate, armed) = armed.map_err(|deferred| deferred.raise(&ctx))?;
      let mut rx = match armed {
        Armed::Events(rx) => rx,
        Armed::Dialog(dialog) => {
          let dialog = dialog.await.into_js_with(&ctx)?;
          let instance = crate::bindings::identity::dialog_object(&ctx, dialog)?;
          return rquickjs::IntoJs::into_js(instance, &ctx);
        },
        Armed::FileChooser(chooser) => {
          let chooser = chooser.await.into_js_with(&ctx)?;
          let instance = Class::instance(ctx.clone(), crate::bindings::file_chooser::FileChooserJs::new(chooser))?;
          return rquickjs::IntoJs::into_js(instance, &ctx);
        },
        Armed::Download(download) => {
          let download = download.await.into_js_with(&ctx)?;
          let instance = Class::instance(ctx.clone(), crate::bindings::download::DownloadJs::new(download))?;
          return rquickjs::IntoJs::into_js(instance, &ctx);
        },
      };
      let Some(pred) = predicate else {
        let name = event_lc.clone();
        let ev = rx
          .drain_until(move |e| match_event_name(&name, e), timeout)
          .await
          .into_js_with(&ctx)?;
        return page_event_to_js(&ctx, &page, ev);
      };
      // Predicate waits drain the broadcast for every event type — the
      // emitter bridge claims dialog / filechooser / download live
      // handles on behalf of broadcast listeners, so the predicate sees
      // the same live object a `page.on` listener would.
      let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);
      loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
          return Err(crate::bindings::convert::throw_named(
            &ctx,
            "TimeoutError",
            format!("Timeout {timeout}ms exceeded while waiting for event '{event}'"),
          ));
        }
        let recv = tokio::time::timeout(remaining, crate::bindings::page::recv_matching(&mut rx, &event_lc)).await;
        let Ok(Some(ev)) = recv else {
          if recv.is_err() {
            continue; // deadline check at loop top surfaces the timeout
          }
          return Err(crate::bindings::convert::throw_named(
            &ctx,
            "Error",
            "page closed while waiting for event",
          ));
        };
        let arg = page_event_to_js(&ctx, &page, ev.clone())?;
        if call_predicate_truthy(&pred, arg, &ctx).await? {
          return page_event_to_js(&ctx, &page, ev);
        }
      }
    }))
  }

  // ── Frames (sync, Playwright parity — task 3.8) ─────────────────────
  //
  // Mirrors `/tmp/playwright/packages/playwright-core/src/client/page.ts:258-275`
  // — `mainFrame`, `frames`, `frame(selector)` are all sync and read
  // from the page-owned [`ferridriver::frame_cache::FrameCache`].

  /// Main frame of this page. Playwright: `page.mainFrame(): Frame`.
  /// Always returns a Frame — the cache is seeded inside `Page::new` /
  /// `Page::with_context` before the Page is handed out.
  ///
  /// # Errors
  ///
  /// Throws when the `Frame` instance cannot be created in this context.
  #[qjs(rename = "mainFrame")]
  pub fn main_frame<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<rquickjs::Class<'js, crate::bindings::frame::FrameJs>> {
    crate::bindings::identity::frame_object(&ctx, self.inner.main_frame())
  }

  /// All non-detached frames on the page. Playwright:
  /// `page.frames(): Frame[]`.
  ///
  /// # Errors
  ///
  /// Throws when a `Frame` instance cannot be created in this context.
  #[qjs(rename = "frames")]
  pub fn frames<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<Vec<rquickjs::Class<'js, crate::bindings::frame::FrameJs>>> {
    self
      .inner
      .frames()
      .into_iter()
      .map(|frame| crate::bindings::identity::frame_object(&ctx, frame))
      .collect()
  }

  /// Playwright: `page.getByRef(ref: string): Locator`.
  #[qjs(rename = "getByRef")]
  #[must_use]
  pub fn get_by_ref(&self, aria_ref: String) -> crate::bindings::locator::LocatorJs {
    crate::bindings::locator::LocatorJs::new(self.inner.get_by_ref(&aria_ref))
  }

  /// Locate a frame by name or URL.
  ///
  /// # Errors
  ///
  /// Throws when reading `selector.name` or `selector.url` throws, or when the `Frame` instance cannot be created.
  #[qjs(rename = "frame")]
  pub fn frame<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: rquickjs::Value<'js>,
  ) -> rquickjs::Result<crate::bindings::convert::Null<rquickjs::Class<'js, crate::bindings::frame::FrameJs>>> {
    let core_sel = if let Some(s) = selector.as_string() {
      ferridriver::options::FrameSelector::by_name(s.to_string()?)
    } else if let Some(obj) = selector.as_object() {
      let read = |key: &str| -> rquickjs::Result<Option<String>> {
        let v: rquickjs::Value<'_> = obj
          .get(key)
          .unwrap_or_else(|_| rquickjs::Value::new_undefined(ctx.clone()));
        if v.is_undefined() || v.is_null() {
          Ok(None)
        } else if let Some(s) = v.as_string() {
          Ok(Some(s.to_string()?))
        } else {
          Ok(None)
        }
      };
      ferridriver::options::FrameSelector {
        name: read("name")?,
        url: read("url")?,
      }
    } else {
      return Ok(crate::bindings::convert::Null(None));
    };

    if core_sel.is_empty() {
      return Ok(crate::bindings::convert::Null(None));
    }
    let frame = self.inner.frame(core_sel);
    Ok(crate::bindings::convert::Null(
      frame
        .map(|frame| crate::bindings::identity::frame_object(&ctx, frame))
        .transpose()?,
    ))
  }

  /// Playwright: `page.touchscreen: Touchscreen`.
  #[qjs(rename = "touchscreen", get)]
  #[must_use]
  pub fn touchscreen(&self) -> TouchscreenJs {
    TouchscreenJs {
      page: Arc::clone(&self.inner),
    }
  }

  /// ferridriver-specific (NOT Playwright): structured AI snapshot
  /// `{ full: string, incremental?: string, refMap: Record<string, number> }`.
  /// Playwright's public accessibility API is `ariaSnapshot` (string);
  /// this richer shape feeds the MCP server's incremental tracking.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the snapshot cannot be taken.
  #[qjs(rename = "snapshotForAI")]
  pub async fn snapshot_for_ai<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let core_opts = match options.0 {
          None => ferridriver::snapshot::SnapshotOptions::default(),
          Some(v) if v.is_undefined() || v.is_null() => ferridriver::snapshot::SnapshotOptions::default(),
          Some(v) => {
            #[derive(serde::Deserialize, Default)]
            #[serde(rename_all = "camelCase", default)]
            struct JsSnap {
              depth: Option<i32>,
              track: Option<String>,
            }
            let parsed: JsSnap = crate::bindings::convert::serde_from_js(&ctx, v)?;
            ferridriver::snapshot::SnapshotOptions {
              depth: parsed.depth,
              track: parsed.track,
            }
          },
        };
        let snap = self
          .inner
          .snapshot_for_ai()
          .options(core_opts)
          .await
          .into_js_with(&ctx)?;
        let obj = rquickjs::Object::new(ctx.clone())?;
        obj.set("full", snap.full)?;
        if let Some(inc) = snap.incremental {
          obj.set("incremental", inc)?;
        }
        let ref_map = rquickjs::Object::new(ctx.clone())?;
        for (k, v) in snap.ref_map {
          ref_map.set(k, crate::bindings::convert::i64_to_f64(v))?;
        }
        obj.set("refMap", ref_map)?;
        rquickjs::IntoJs::into_js(obj, &ctx)
      })
      .await
  }

  /// Playwright `page.ariaSnapshot(options?): Promise<string>`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the snapshot cannot be taken.
  #[qjs(rename = "ariaSnapshot")]
  pub async fn aria_snapshot<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<String> {
    call_site
      .scope(async move {
        let core_opts = crate::bindings::locator::aria_snapshot_options(&ctx, options)?;
        self.inner.aria_snapshot().options(core_opts).await.into_js_with(&ctx)
      })
      .await
  }

  /// Playwright: `page.ariaSnapshotJSON(options?): Promise<AriaSnapshotJSON>`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is malformed, and with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the snapshot cannot be taken.
  #[qjs(rename = "ariaSnapshotJSON")]
  pub async fn aria_snapshot_json<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let core_opts = crate::bindings::locator::aria_snapshot_options(&ctx, options)?;
        let json = self
          .inner
          .aria_snapshot_json()
          .options(core_opts)
          .await
          .into_js_with(&ctx)?;
        crate::bindings::convert::json_to_js(&ctx, &serde_json::Value::Array(json))
      })
      .await
  }

  /// Playwright: `page.exposeFunction(name, callback)`. Binds
  /// `window[name]` to a page-side proxy that asynchronously invokes
  /// `callback(args)` in the script context.
  ///
  /// The callback receives the args as a single array. The page-side
  /// call resolves to `null` since the script-side callback runs
  /// asynchronously (Rust core's `ExposedFn` is sync + JSON-in/out;
  /// `QuickJS` dispatch is async-only).
  ///
  /// # Errors
  ///
  /// Throws an `Error` when the page has no VM handle. The promise rejects with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the function cannot be installed.
  #[qjs(rename = "exposeFunction")]
  pub fn expose_function<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    name: String,
    callback: rquickjs::Function<'js>,
  ) -> rquickjs::Result<rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<()>> + 'js>> {
    let vm = self.vm.clone().ok_or_else(|| {
      throw_named(
        &ctx,
        "Error",
        "page.exposeFunction requires the script engine's VM handle (install_page)",
      )
    })?;
    // Snapshot the registrar's net grant NOW, synchronously — a `#[qjs]`
    // async-fn body first-polls off the tool's `bracket_net` swap, so an
    // in-body `active_net` reads the resting `None`. Sync prologue of a
    // `fn -> Promised` runs on the caller's stack, where the grant is live.
    // Stash the JS callback in the native page-callbacks registry keyed
    // by binding name — cross-task dispatch (the Rust `ExposedFn` runs
    // outside the QuickJS context) restores it by name inside a VM-loop
    // job.
    let saved = SavedCallback::save(&ctx, callback);
    let page_key = self.inner.backend_page_id();
    with_page_callbacks(&ctx, |r| {
      r.exposed.insert(name.clone(), saved);
      // Tracked so page close releases the Persistent — exposeFunction
      // has no dispose in Playwright, and the session VM outlives the
      // page.
      r.track_exposed_owner(page_key, name.clone());
    })?;

    let cb: ferridriver::events::ExposedFn = std::sync::Arc::new({
      let name = name.clone();
      move |args: Vec<serde_json::Value>| {
        let vm = vm.clone();
        let name = name.clone();
        // Playwright delivers the callback's return value (awaiting a
        // returned Promise) to the page-side caller. Run the JS
        // callback as a VM-loop job, await it if it returns a
        // thenable, convert to JSON and hand it back so the backend
        // resolves the page binding with the REAL value — not `null`
        // (the previous fire-and-forget behaviour was a Playwright
        // incompatibility).
        Box::pin(async move {
          let out: Result<rquickjs::Result<Result<ferridriver::BindingReturn, String>>, crate::error::ScriptError> =
            crate::vm_with!(vm => |ctx| {
              let saved = with_page_callbacks(&ctx, |r| r.exposed.get(&name).cloned())?
                .ok_or_else(|| {
                  throw_named(&ctx, "Error", "page.exposeFunction: exposed callback gone")
                })?;
              let f = saved.restore(&ctx)?;
              // Playwright spreads the page-side call arguments into the
              // callback: `window.fn(a, b)` -> `callback(a, b)` (see
              // playwright-core client/page.ts `(...args) => callback(...args)`).
              // Build a spread arg list, not a single array.
              let mut call_args = rquickjs::function::Args::new_unsized(ctx.clone());
              for v in args {
                // `json_to_js` (NOT `serde_to_js`): a transitive dep
                // force-enables `serde_json/arbitrary_precision`, under
                // which ferrijs-serde turns every number into a
                // `{$serde_json::private::Number}` object. The AP-safe
                // walker keeps numbers as JS numbers.
                call_args.push_arg(crate::bindings::convert::json_to_js(&ctx, &v)?)?;
              }
              settle_exposed_call(&ctx, &f, call_args).await
            })
            .await;
          exposed_outcome(out)
        })
      }
    });
    let inner = Arc::clone(&self.inner);
    Ok(rquickjs::promise::Promised::from(async move {
      inner.expose_function(&name, cb).await.into_js_with(&ctx)
    }))
  }

  /// Playwright: `page.exposeBinding(name, callback)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts:371`).
  /// Like [`Self::expose_function`], but the callback receives a
  /// `BindingSource` object (the live `{ context, page, frame }`) as its
  /// first argument, then the spread page-side call arguments.
  ///
  /// # Errors
  ///
  /// Throws an `Error` when the page has no VM handle. The promise rejects with the core failure (`TimeoutError`,
  /// `TargetClosedError` or `FerriError`) when the binding cannot be installed.
  #[qjs(rename = "exposeBinding")]
  pub fn expose_binding<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    name: String,
    callback: rquickjs::Function<'js>,
  ) -> rquickjs::Result<rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<()>> + 'js>> {
    let vm = self.vm.clone().ok_or_else(|| {
      throw_named(
        &ctx,
        "Error",
        "page.exposeBinding requires the script engine's VM handle (install_page)",
      )
    })?;
    let saved = SavedCallback::save(&ctx, callback);
    let page_key = self.inner.backend_page_id();
    with_page_callbacks(&ctx, |r| {
      r.exposed.insert(name.clone(), saved);
      r.track_exposed_owner(page_key, name.clone());
    })?;

    let cb: ferridriver::events::ExposedBinding = std::sync::Arc::new({
      let name = name.clone();
      move |source, args: Vec<serde_json::Value>| {
        let vm = vm.clone();
        let name = name.clone();
        Box::pin(async move {
          let out: Result<rquickjs::Result<Result<ferridriver::BindingReturn, String>>, crate::error::ScriptError> =
            crate::vm_with!(vm => |ctx| {
              let saved = with_page_callbacks(&ctx, |r| r.exposed.get(&name).cloned())?.ok_or_else(|| {
                throw_named(&ctx, "Error", "page.exposeBinding: exposed callback gone")
              })?;
              let f = saved.restore(&ctx)?;
              let mut call_args = rquickjs::function::Args::new_unsized(ctx.clone());
              // Playwright passes the source object first, then spreads
              // the page-side call arguments: `callback(source, ...args)`.
              let source: ferridriver::events::BindingSource = source;
              call_args.push_arg(binding_source_object(&ctx, &source)?)?;
              for v in args {
                call_args.push_arg(crate::bindings::convert::json_to_js(&ctx, &v)?)?;
              }
              settle_exposed_call(&ctx, &f, call_args).await
            })
            .await;
          exposed_outcome(out)
        })
      }
    });
    let inner = Arc::clone(&self.inner);
    Ok(rquickjs::promise::Promised::from(async move {
      inner.expose_binding(&name, cb).await.into_js_with(&ctx)
    }))
  }

  /// ferridriver-specific (NOT Playwright): `startScreencast(quality,
  /// maxWidth, maxHeight, callback)`. Callback receives `{ frame:
  /// Uint8Array, timestamp: number }` per frame. Backed by CDP
  /// `Page.startScreencast`; no Playwright client equivalent.
  ///
  /// Frames are delivered by a `ctx.spawn` pump on the interpreter
  /// thread — NOT `tokio::spawn` + `async_with`: a long-lived loop
  /// resolving a plain JS callback from a second thread is exactly the
  /// pump shape that crashed the event listeners (see the module-level
  /// re-entry discipline notes on `PageEventPumpUd`). The pump only
  /// runs while the VM is driven; frames arriving while the VM idles
  /// are coalesced to the newest one (a screencast consumer wants the
  /// latest frame, and the core channel is unbounded).
  ///
  /// # Errors
  ///
  /// Throws an `Error` when the context's page-callback registry is missing. The promise rejects with the core
  /// failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the screencast cannot start.
  #[qjs(rename = "startScreencast")]
  pub fn start_screencast<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    quality: u8,
    max_width: u32,
    max_height: u32,
    callback: rquickjs::Function<'js>,
  ) -> rquickjs::Result<rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<()>> + 'js>> {
    // Sync prologue: snapshot the registrar's grant so the frame
    // callback keeps this tool's `allow.net` (see `SavedCallback::save`).
    let saved = SavedCallback::save(&ctx, callback);
    with_page_callbacks(&ctx, |r| r.screencast = Some(saved))?;
    let inner = Arc::clone(&self.inner);
    Ok(rquickjs::promise::Promised::from(async move {
      // Frames flow until `stopScreencast` clears the callback or the
      // channel closes.
      let mut rx = inner
        .start_screencast(quality, max_width, max_height)
        .await
        .into_js_with(&ctx)?;
      let pump_ctx = ctx.clone();
      ctx.spawn(async move {
        while let Some((mut bytes, mut ts)) = rx.recv().await {
          while let Ok((b, t)) = rx.try_recv() {
            bytes = b;
            ts = t;
          }
          let Ok(Some(saved)) = with_page_callbacks(&pump_ctx, |r| r.screencast.clone()) else {
            break; // stopScreencast cleared the callback
          };
          let Ok(f) = saved.restore(&pump_ctx) else { break };
          let deliver = || -> rquickjs::Result<()> {
            let payload = rquickjs::Object::new(pump_ctx.clone())?;
            let buf = rquickjs::TypedArray::<u8>::new(pump_ctx.clone(), bytes)?;
            payload.set("frame", buf)?;
            payload.set("timestamp", ts)?;
            let _: rquickjs::Value<'_> = f.call((payload,))?;
            Ok(())
          };
          // A throwing callback is swallowed so one bad frame handler
          // can't kill the pump (same policy as the event pump).
          let _ = deliver();
        }
      });
      Ok(())
    }))
  }

  /// ferridriver-specific (NOT Playwright): stop the screencast
  /// started by `startScreencast`. Clears the persisted frame callback
  /// (ending the frame pump) so it doesn't outlive the screencast in
  /// the session VM's registry.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the screencast cannot
  /// be stopped.
  #[qjs(rename = "stopScreencast")]
  pub async fn stop_screencast(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        self.inner.stop_screencast().await.into_js_with(&ctx)?;
        with_page_callbacks(&ctx, |r| r.screencast = None)?;
        Ok(())
      })
      .await
  }
}
}

/// Playwright `Touchscreen`. Construct via `page.touchscreen`.
#[derive(rquickjs::JsLifetime, rquickjs::class::Trace)]
#[rquickjs::class(rename = "Touchscreen")]
pub struct TouchscreenJs {
  #[qjs(skip_trace)]
  page: std::sync::Arc<ferridriver::Page>,
}

#[rquickjs::methods]
impl TouchscreenJs {
  /// Playwright: `touchscreen.tap(x, y)`.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the tap fails.
  #[qjs(rename = "tap")]
  pub async fn tap(&self, ctx: rquickjs::Ctx<'_>, x: f64, y: f64) -> rquickjs::Result<()> {
    self.page.touchscreen().tap(x, y).await.into_js_with(&ctx)
  }
}

type Pending<T> = std::pin::Pin<Box<dyn std::future::Future<Output = ferridriver::Result<T>> + Send>>;

/// What a `page.waitForEvent` listens with, armed at the call.
enum Armed {
  Events(ferridriver::EventSubscription<ferridriver::events::PageEvent>),
  Dialog(Pending<ferridriver::dialog::Dialog>),
  FileChooser(Pending<ferridriver::file_chooser::FileChooser>),
  Download(Pending<ferridriver::download::Download>),
}

/// A `waitForRequest` / `waitForResponse` armed at the call: a JS
/// predicate over the page's events, or the core wait for a URL.
enum NetworkWait<'js, T> {
  Predicate(
    ferridriver::EventSubscription<ferridriver::events::PageEvent>,
    rquickjs::Function<'js>,
  ),
  Matched(Pending<T>),
}
