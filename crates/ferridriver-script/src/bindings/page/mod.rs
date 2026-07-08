//! `PageJs`: JS wrapper around `ferridriver::Page`.
//!
//! Methods mirror `ferridriver::Page`'s public surface one-for-one; each is a
//! small delegation that converts `FerriError` into `rquickjs::Error` at the
//! boundary via [`super::convert::FerriResultExt`].

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
use rquickjs::class::Trace;

use rquickjs::function::Opt;

use crate::bindings::convert::FerriResultCtxExt;
use crate::bindings::convert::{
  extract_page_function, init_script_from_js, quickjs_arg_to_serialized, serde_from_js, serialized_value_to_quickjs,
};
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
  /// tests); the engine always installs PageJs via
  /// `install_page` which sets this field.
  #[qjs(skip_trace)]
  vm: Option<crate::vm::VmHandle>,
  /// Maps a handler locator's selector to the persisted-callback ids so
  /// `removeLocatorHandler` can drop them. (QuickJS `addLocatorHandler`
  /// itself is Unsupported -- see its binding -- so this normally stays empty.)
  #[qjs(skip_trace)]
  locator_handler_ids: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, Vec<u64>>>>,
}

impl PageJs {
  #[must_use]
  pub fn new(inner: Arc<Page>) -> Self {
    Self {
      inner,
      vm: None,
      locator_handler_ids: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
    }
  }

  #[must_use]
  pub fn new_with_vm(inner: Arc<Page>, vm: crate::vm::VmHandle) -> Self {
    Self {
      inner,
      vm: Some(vm),
      locator_handler_ids: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
    }
  }

  /// This page's route-registry owner key.
  fn route_owner(&self) -> RouteOwner {
    RouteOwner::Page(self.inner.backend_page_id())
  }

  /// Clone of the wrapped `Arc<Page>` for cross-binding consumers
  /// (used by `expect()` to lift a `PageJs` into an assertion target).
  #[must_use]
  pub fn page_arc(&self) -> Arc<Page> {
    self.inner.clone()
  }

  #[must_use]
  pub fn page(&self) -> &Arc<Page> {
    &self.inner
  }

  /// Shared core for `page.on` / `page.once`. Saves the JS `listener`
  /// keyed by the core `ListenerId`, then registers a core callback that
  /// forwards `(id, event)` to this context's event pump. The backend
  /// task never touches the VM — only the pump (on the interpreter
  /// thread, via `ctx.spawn`) restores and invokes the JS function.
  fn register_event_listener<'js>(
    &self,
    ctx: &rquickjs::Ctx<'js>,
    event: &str,
    listener: rquickjs::Function<'js>,
    once: bool,
  ) -> rquickjs::Result<f64> {
    let saved = SavedCallback::save(ctx, listener);
    let tx = ensure_event_pump(ctx);
    // Core assigns the `ListenerId` only after registration, but the
    // dispatch closure needs it to look the JS callback back up. Share it
    // through a slot the closure reads at fire time (registration is
    // synchronous, so the slot is populated long before any event lands).
    let id_slot = Arc::new(AtomicU64::new(0));
    let id_slot_cb = id_slot.clone();
    let page_for_cb = self.inner.clone();
    let callback: ferridriver::events::EventCallback =
      std::sync::Arc::new(move |ev: ferridriver::events::PageEvent| {
        let id = id_slot_cb.load(Ordering::Relaxed);
        if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) = tx.try_send((id, once, page_for_cb.clone(), ev)) {
          tracing::warn!(
            listener_id = id,
            capacity = PAGE_EVENT_PUMP_CAPACITY,
            "page event pump full (VM idle between scripts?); dropping event"
          );
        }
      });
    let id = if once {
      self.inner.once(event, callback)
    } else {
      self.inner.on(event, callback)
    };
    id_slot.store(id.0, Ordering::Relaxed);
    let page_key = self.inner.backend_page_id();
    with_page_callbacks(ctx, |r| {
      r.insert_event_listener(id.0, event.to_string(), page_key, saved);
    })?;
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
  }
}

/// Build a `PageJs` for a page minted from script (`newPage`,
/// `locator.page()`, `frame.page()`), threading the session's VM-loop
/// handle (stashed as userdata at `Session::create`) so `page.route` /
/// `page.exposeFunction` cross-task dispatch works on script-launched
/// browsers — not just the MCP-prebound page.
pub(crate) fn pagejs_for_ctx(ctx: &rquickjs::Ctx<'_>, page: Arc<Page>) -> PageJs {
  match ctx.userdata::<crate::engine::SessionVm>() {
    Some(ud) => PageJs::new_with_vm(page, ud.0.clone()),
    None => PageJs::new(page),
  }
}

#[rquickjs::methods]
impl PageJs {
  // ── Navigation ────────────────────────────────────────────────────────────

  /// Navigate to `url`. Accepts `{ waitUntil?, timeout?, referer? }` to
  /// mirror Playwright's `page.goto(url, options?)`.
  #[qjs(rename = "goto")]
  pub async fn goto<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    url: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Option<crate::bindings::network::ResponseJs>> {
    let opts = parse_goto_options(&ctx, options)?;
    let resp = self.inner.goto(&url, opts).await.into_js_with(&ctx)?;
    Ok(resp.map(|r| crate::bindings::network::ResponseJs::new_with_page(r, self.inner.clone())))
  }

  /// Reload the current page. Accepts the same option bag as `goto`.
  #[qjs(rename = "reload")]
  pub async fn reload<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Option<crate::bindings::network::ResponseJs>> {
    let opts = parse_goto_options(&ctx, options)?;
    let resp = self.inner.reload(opts).await.into_js_with(&ctx)?;
    Ok(resp.map(|r| crate::bindings::network::ResponseJs::new_with_page(r, self.inner.clone())))
  }

  /// Navigate back in history. Accepts the same option bag as `goto`.
  #[qjs(rename = "goBack")]
  pub async fn go_back<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Option<crate::bindings::network::ResponseJs>> {
    let opts = parse_goto_options(&ctx, options)?;
    let resp = self.inner.go_back(opts).await.into_js_with(&ctx)?;
    Ok(resp.map(|r| crate::bindings::network::ResponseJs::new_with_page(r, self.inner.clone())))
  }

  /// Navigate forward in history. Accepts the same option bag as `goto`.
  #[qjs(rename = "goForward")]
  pub async fn go_forward<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Option<crate::bindings::network::ResponseJs>> {
    let opts = parse_goto_options(&ctx, options)?;
    let resp = self.inner.go_forward(opts).await.into_js_with(&ctx)?;
    Ok(resp.map(|r| crate::bindings::network::ResponseJs::new_with_page(r, self.inner.clone())))
  }

  /// Current URL of the page.
  /// Playwright: `page.url(): string` — synchronous.
  #[qjs(rename = "url")]
  pub fn url(&self) -> String {
    self.inner.url()
  }

  /// Document title.
  #[qjs(rename = "title")]
  pub async fn title(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<String> {
    self.inner.title().await.into_js_with(&ctx)
  }

  /// Playwright: `page.video(): null | Video` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:4756`.
  /// Returns a live `Video` handle when the owning context was
  /// created with `recordVideo`, or `null` otherwise.
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

  /// Full HTML content of the page.
  #[qjs(rename = "content")]
  pub async fn content(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<String> {
    self.inner.content().await.into_js_with(&ctx)
  }

  /// Replace the page's HTML with `html`.
  #[qjs(rename = "setContent")]
  pub async fn set_content(&self, ctx: rquickjs::Ctx<'_>, html: String) -> rquickjs::Result<()> {
    self.inner.set_content(&html).await.into_js_with(&ctx)
  }

  /// Register a JS snippet to run on every new document before any page
  /// script executes. Mirrors Playwright's
  /// `page.addInitScript(script, arg)` — see
  /// `/tmp/playwright/packages/playwright-core/src/client/page.ts:520`.
  /// Accepts `Function | string | { path?, content? }` + optional `arg`
  /// exactly like the NAPI binding; all lowering runs in Rust core via
  /// [`ferridriver::options::evaluation_script`].
  #[qjs(rename = "addInitScript")]
  pub async fn add_init_script<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    script: rquickjs::Value<'js>,
    arg: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let (init, arg_json) = init_script_from_js(&ctx, script, arg.0)?;
    let disposable = self.inner.add_init_script(init, arg_json).await.into_js_with(&ctx)?;
    let instance =
      rquickjs::class::Class::instance(ctx.clone(), crate::bindings::disposable::DisposableJs::new(disposable))?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  /// Remove a previously-registered init script by identifier.
  #[qjs(rename = "removeInitScript")]
  pub async fn remove_init_script(&self, ctx: rquickjs::Ctx<'_>, identifier: String) -> rquickjs::Result<()> {
    self.inner.remove_init_script(&identifier).await.into_js_with(&ctx)
  }

  /// Full page rendered as clean Markdown (headings, lists, links, tables
  /// preserved; chrome and boilerplate stripped).
  #[qjs(rename = "markdown")]
  pub async fn markdown(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<String> {
    self.inner.markdown().await.into_js_with(&ctx)
  }

  /// Wait for an element matching `selector`. Optional `options` object
  /// accepts `{ state?: 'visible'|'hidden'|'attached'|'stable', timeout?: ms }`.
  /// Resolves when the condition is met; throws on timeout.
  #[qjs(rename = "waitForSelector")]
  pub async fn wait_for_selector<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = parse_wait_options(&ctx, options)?;
    self.inner.wait_for_selector(&selector, opts).await.into_js_with(&ctx)
  }

  // ── Locators ──────────────────────────────────────────────────────────────

  /// Playwright: `page.querySelector(selector): Promise<ElementHandle | null>`.
  /// Mints a lifecycle [`crate::bindings::element_handle::ElementHandleJs`]
  /// pinned to the first element matching `selector`, or `null` when no
  /// element matches. Callers `dispose()` the handle when done to
  /// release the backend remote.
  #[qjs(rename = "querySelector")]
  pub async fn query_selector(
    &self,
    ctx: rquickjs::Ctx<'_>,
    selector: String,
  ) -> rquickjs::Result<Option<crate::bindings::element_handle::ElementHandleJs>> {
    let inner = self.inner.query_selector(&selector).await.into_js_with(&ctx)?;
    Ok(inner.map(crate::bindings::element_handle::ElementHandleJs::new))
  }

  /// Playwright `$` shortcut for [`Self::query_selector`].
  #[qjs(rename = "$")]
  pub async fn dollar(
    &self,
    ctx: rquickjs::Ctx<'_>,
    selector: String,
  ) -> rquickjs::Result<Option<crate::bindings::element_handle::ElementHandleJs>> {
    self.query_selector(ctx, selector).await
  }

  /// Playwright: `page.querySelectorAll(selector): Promise<ElementHandle[]>`.
  #[qjs(rename = "querySelectorAll")]
  pub async fn query_selector_all(
    &self,
    ctx: rquickjs::Ctx<'_>,
    selector: String,
  ) -> rquickjs::Result<Vec<crate::bindings::element_handle::ElementHandleJs>> {
    let inner_handles = self.inner.query_selector_all(&selector).await.into_js_with(&ctx)?;
    Ok(
      inner_handles
        .into_iter()
        .map(crate::bindings::element_handle::ElementHandleJs::new)
        .collect(),
    )
  }

  /// Playwright `$$` shortcut for [`Self::query_selector_all`].
  #[qjs(rename = "$$")]
  pub async fn dollar_dollar(
    &self,
    ctx: rquickjs::Ctx<'_>,
    selector: String,
  ) -> rquickjs::Result<Vec<crate::bindings::element_handle::ElementHandleJs>> {
    self.query_selector_all(ctx, selector).await
  }

  /// Playwright: `page.evaluate(pageFunction, arg?): Promise<R>`.
  /// `pageFunction` accepts a string or a JS function; rich return
  /// types (`Date` / `RegExp` / `BigInt` / `URL` / `Error` / typed
  /// arrays / `NaN` / `±Infinity` / `undefined` / `-0`) arrive as
  /// native JS, matching Playwright's `parseResult`.
  #[qjs(rename = "evaluate")]
  pub async fn evaluate<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    page_function: rquickjs::Value<'js>,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let (source, is_fn) = extract_page_function(&ctx, page_function)?;
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self
      .inner
      .evaluate(&source, serialized, is_fn)
      .await
      .into_js_with(&ctx)?;
    serialized_value_to_quickjs(&ctx, &result)
  }

  /// Playwright: `page.evaluateHandle(pageFunction, arg?): Promise<JSHandle>`.
  #[qjs(rename = "evaluateHandle")]
  pub async fn evaluate_handle<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    page_function: rquickjs::Value<'js>,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::js_handle::JSHandleJs> {
    let (source, is_fn) = extract_page_function(&ctx, page_function)?;
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let handle = self
      .inner
      .evaluate_handle(&source, serialized, is_fn)
      .await
      .into_js_with(&ctx)?;
    Ok(crate::bindings::js_handle::JSHandleJs::new(handle))
  }

  /// Playwright: `page.locator(selector, options?: LocatorOptions): Locator`.
  /// Thin delegator to Rust core's `Page::locator`.
  #[qjs(rename = "locator")]
  pub fn locator<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<LocatorJs> {
    let parsed = crate::bindings::locator::parse_locator_options_public(&ctx, options, true)?;
    let opts = ferridriver::options::FilterOptions {
      has_text: parsed.has_text,
      has_not_text: parsed.has_not_text,
      has: parsed.has,
      has_not: parsed.has_not,
      visible: parsed.visible,
    };
    let filter = if crate::bindings::locator::is_empty_filter(&opts) {
      None
    } else {
      Some(opts)
    };
    Ok(LocatorJs::new(self.inner.locator(&selector, filter)))
  }

  /// Locate elements by ARIA role. Accepts `{ name: string | RegExp,
  /// exact, checked, disabled, expanded, level, pressed, selected,
  /// includeHidden }` via the options bag.
  #[qjs(rename = "getByRole")]
  pub fn get_by_role(
    &self,
    role: String,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let opts = parse_role_options(options)?;
    Ok(LocatorJs::new(self.inner.get_by_role(&role, &opts)))
  }

  /// Locate elements containing the given text. Accepts `string | RegExp`.
  #[qjs(rename = "getByText")]
  pub fn get_by_text(
    &self,
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_text(&t, &opts)))
  }

  /// Locate form controls by associated label text.
  #[qjs(rename = "getByLabel")]
  pub fn get_by_label(
    &self,
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_label(&t, &opts)))
  }

  /// Locate inputs by placeholder text.
  #[qjs(rename = "getByPlaceholder")]
  pub fn get_by_placeholder(
    &self,
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_placeholder(&t, &opts)))
  }

  /// Locate images/media by alt text.
  #[qjs(rename = "getByAltText")]
  pub fn get_by_alt_text(
    &self,
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_alt_text(&t, &opts)))
  }

  /// Locate elements by `title` attribute text.
  #[qjs(rename = "getByTitle")]
  pub fn get_by_title(
    &self,
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_title(&t, &opts)))
  }

  /// Locate elements by `data-testid`. Accepts `string | RegExp`.
  #[qjs(rename = "getByTestId")]
  pub fn get_by_test_id(&self, test_id: rquickjs::Value<'_>) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(test_id)?;
    Ok(LocatorJs::new(self.inner.get_by_test_id(&t)))
  }

  // ── Interaction ───────────────────────────────────────────────────────────

  /// Click the first element matching `selector`. Accepts Playwright's
  /// full `PageClickOptions` bag.
  #[qjs(rename = "click")]
  pub async fn click<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_click_options(&ctx, options)?;
    self.inner.click(&selector, opts).await.into_js_with(&ctx)
  }

  /// Double-click the first element matching `selector`. Accepts
  /// Playwright's full `PageDblClickOptions` bag.
  #[qjs(rename = "dblclick")]
  pub async fn dblclick<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_dblclick_options(&ctx, options)?;
    self.inner.dblclick(&selector, opts).await.into_js_with(&ctx)
  }

  /// Fill `value` into the input matching `selector`. Accepts
  /// Playwright's full `PageFillOptions` bag.
  #[qjs(rename = "fill")]
  pub async fn fill<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    value: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_fill_options(&ctx, options)?;
    self.inner.fill(&selector, &value, opts).await.into_js_with(&ctx)
  }

  /// Type `text` into the input matching `selector`. Accepts
  /// Playwright's full `PageTypeOptions` bag.
  ///
  /// Exposed as `type` in JS (matches Playwright) — Rust renames to avoid
  /// the `type` keyword.
  #[qjs(rename = "type")]
  pub async fn type_<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    text: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_type_options(&ctx, options)?;
    self.inner.r#type(&selector, &text, opts).await.into_js_with(&ctx)
  }

  /// Press `key` on the element matching `selector`. Accepts Playwright's
  /// full `PagePressOptions` bag.
  #[qjs(rename = "press")]
  pub async fn press<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    key: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_press_options(&ctx, options)?;
    self.inner.press(&selector, &key, opts).await.into_js_with(&ctx)
  }

  /// `page.focus(selector, options?)`.
  #[qjs(rename = "focus")]
  pub async fn focus(
    &self,
    ctx: rquickjs::Ctx<'_>,
    selector: String,
    _options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<()> {
    self.inner.focus(&selector).await.into_js_with(&ctx)
  }

  /// Hover the first element matching `selector`. Accepts Playwright's
  /// full `PageHoverOptions` bag.
  #[qjs(rename = "hover")]
  pub async fn hover<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_hover_options(&ctx, options)?;
    self.inner.hover(&selector, opts).await.into_js_with(&ctx)
  }

  /// Dispatch a DOM event on the first element matching `selector`.
  /// Mirrors Playwright's `page.dispatchEvent(selector, type, eventInit?, options?)`.
  #[qjs(rename = "dispatchEvent")]
  pub async fn dispatch_event<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    event_type: String,
    event_init: rquickjs::function::Opt<rquickjs::Value<'js>>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let init_json = match event_init.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => {
        Some(crate::bindings::convert::serde_from_js::<serde_json::Value>(&ctx, v)?)
      },
      _ => None,
    };
    let opts = crate::bindings::convert::parse_dispatch_event_options(&ctx, options)?;
    self
      .inner
      .dispatch_event(&selector, &event_type, init_json, opts)
      .await
      .into_js_with(&ctx)
  }

  /// Tap (touch) the first element matching `selector`. Accepts
  /// Playwright's full `PageTapOptions` bag.
  #[qjs(rename = "tap")]
  pub async fn tap<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_tap_options(&ctx, options)?;
    self.inner.tap(&selector, opts).await.into_js_with(&ctx)
  }

  /// Check a checkbox matching `selector`. Accepts Playwright's full
  /// `PageCheckOptions` bag.
  #[qjs(rename = "check")]
  pub async fn check<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.check(&selector, opts).await.into_js_with(&ctx)
  }

  /// Uncheck a checkbox matching `selector`. Accepts Playwright's full
  /// `PageUncheckOptions` bag.
  #[qjs(rename = "uncheck")]
  pub async fn uncheck<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.uncheck(&selector, opts).await.into_js_with(&ctx)
  }

  /// Set the checked state of a checkbox/radio matching `selector`.
  /// Accepts Playwright's full `PageSetCheckedOptions` bag.
  #[qjs(rename = "setChecked")]
  pub async fn set_checked<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    checked: bool,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self
      .inner
      .set_checked(&selector, checked, opts)
      .await
      .into_js_with(&ctx)
  }

  /// Select options on the `<select>` matching `selector`. Returns the
  /// values of the selected options. Accepts Playwright's full
  /// `string | string[] | { value?, label?, index? } | Array<...>` union.
  #[qjs(rename = "selectOption")]
  pub async fn select_option<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    values: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<String>> {
    let values = crate::bindings::convert::parse_select_option_values(&ctx, values)?;
    let opts = crate::bindings::convert::parse_select_option_options(&ctx, options)?;
    self
      .inner
      .select_option(&selector, values, opts)
      .await
      .into_js_with(&ctx)
  }

  // ── Info ──────────────────────────────────────────────────────────────────

  /// Text content of the first element matching `selector` (or `null`).
  #[qjs(rename = "textContent")]
  pub async fn text_content(&self, ctx: rquickjs::Ctx<'_>, selector: String) -> rquickjs::Result<Option<String>> {
    self.inner.text_content(&selector).await.into_js_with(&ctx)
  }

  /// `innerText` of the first element matching `selector`.
  #[qjs(rename = "innerText")]
  pub async fn inner_text(&self, ctx: rquickjs::Ctx<'_>, selector: String) -> rquickjs::Result<String> {
    self.inner.inner_text(&selector).await.into_js_with(&ctx)
  }

  /// `innerHTML` of the first element matching `selector`.
  #[qjs(rename = "innerHTML")]
  pub async fn inner_html(&self, ctx: rquickjs::Ctx<'_>, selector: String) -> rquickjs::Result<String> {
    self.inner.inner_html(&selector).await.into_js_with(&ctx)
  }

  /// Current input value of the first element matching `selector`.
  #[qjs(rename = "inputValue")]
  pub async fn input_value(&self, ctx: rquickjs::Ctx<'_>, selector: String) -> rquickjs::Result<String> {
    self.inner.input_value(&selector).await.into_js_with(&ctx)
  }

  /// Get attribute `name` on the first element matching `selector`
  /// (or `null` if the attribute is absent).
  #[qjs(rename = "getAttribute")]
  pub async fn get_attribute(
    &self,
    ctx: rquickjs::Ctx<'_>,
    selector: String,
    name: String,
  ) -> rquickjs::Result<Option<String>> {
    self.inner.get_attribute(&selector, &name).await.into_js_with(&ctx)
  }

  /// Whether the first element matching `selector` is visible.
  #[qjs(rename = "isVisible")]
  pub async fn is_visible(&self, ctx: rquickjs::Ctx<'_>, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_visible(&selector).await.into_js_with(&ctx)
  }

  /// Whether the first element matching `selector` is hidden.
  #[qjs(rename = "isHidden")]
  pub async fn is_hidden(&self, ctx: rquickjs::Ctx<'_>, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_hidden(&selector).await.into_js_with(&ctx)
  }

  /// Whether the first element matching `selector` is enabled.
  #[qjs(rename = "isEnabled")]
  pub async fn is_enabled(&self, ctx: rquickjs::Ctx<'_>, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_enabled(&selector).await.into_js_with(&ctx)
  }

  /// Whether the first element matching `selector` is disabled.
  #[qjs(rename = "isDisabled")]
  pub async fn is_disabled(&self, ctx: rquickjs::Ctx<'_>, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_disabled(&selector).await.into_js_with(&ctx)
  }

  /// Whether the first checkbox matching `selector` is checked.
  #[qjs(rename = "isChecked")]
  pub async fn is_checked(&self, ctx: rquickjs::Ctx<'_>, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_checked(&selector).await.into_js_with(&ctx)
  }

  // ── Mouse / keyboard namespaces (Playwright parity) ──────────────────────

  /// `page.mouse.*` namespace: `click`, `dblclick`, `down`, `up`, `wheel`.
  /// Exposed as a JS property, matching Playwright.
  #[qjs(get, rename = "mouse")]
  pub fn mouse(&self) -> MouseJs {
    MouseJs::new(self.inner.clone())
  }

  /// `page.keyboard.*` namespace: `down`, `up`, `press` (no selector; acts on
  /// the currently focused element). Exposed as a JS property.
  #[qjs(get, rename = "keyboard")]
  pub fn keyboard(&self) -> KeyboardJs {
    KeyboardJs::new(self.inner.clone())
  }

  /// `page.localStorage` — the `localStorage` area for the page's
  /// current origin. Exposed as a JS property, matching Playwright.
  #[qjs(get, rename = "localStorage")]
  pub fn local_storage(&self) -> crate::bindings::web_storage::WebStorageJs {
    crate::bindings::web_storage::WebStorageJs::new(self.inner.clone(), ferridriver::options::WebStorageKind::Local)
  }

  /// `page.sessionStorage` — the `sessionStorage` area for the page's
  /// current origin. Exposed as a JS property, matching Playwright.
  #[qjs(get, rename = "sessionStorage")]
  pub fn session_storage(&self) -> crate::bindings::web_storage::WebStorageJs {
    crate::bindings::web_storage::WebStorageJs::new(self.inner.clone(), ferridriver::options::WebStorageKind::Session)
  }

  // ── Event emitter (Playwright parity) ────────────────────────────────────

  /// `page.on(event, listener)`. Registers a persistent listener for a
  /// page event (`console`, `request`, `response`, `requestfinished`,
  /// `requestfailed`, `websocket`, `dialog`, `filechooser`,
  /// `frameattached`, `framedetached`, `framenavigated`, `load`,
  /// `domcontentloaded`, `close`, `pageerror`, `download`). Returns the
  /// numeric listener id for `page.off(id)`.
  ///
  /// Listeners fire cross-task: when core emits, a tokio task
  /// `async_with`s back into the script context and invokes the saved
  /// JS function with the event snapshot (`pageerror` receives a native
  /// `Error`, matching Playwright). Mirrors `ferridriver-node`'s
  /// `page.on`, which uses the same core `EventEmitter`.
  #[qjs(rename = "on")]
  pub fn on<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    event: String,
    listener: rquickjs::Function<'js>,
  ) -> rquickjs::Result<f64> {
    self.register_event_listener(&ctx, &event, listener, false)
  }

  /// `page.once(event, listener)`. Like [`Self::on`] but the listener
  /// fires at most once (core auto-removes it after the first emit).
  #[qjs(rename = "once")]
  pub fn once<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    event: String,
    listener: rquickjs::Function<'js>,
  ) -> rquickjs::Result<f64> {
    self.register_event_listener(&ctx, &event, listener, true)
  }

  /// `page.off(listenerId)`. Removes a listener registered by
  /// [`Self::on`] / [`Self::once`]. ferridriver mirrors `ferridriver-node`
  /// here: `off` takes the numeric id returned from `on`, not the
  /// function reference.
  #[qjs(rename = "off")]
  pub fn off<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    target: rquickjs::Value<'js>,
    listener: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    // Ferridriver id form: `off(id)` with the number `on()` returned.
    if let Some(n) = target.as_number() {
      #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
      let id = n as u64;
      self.inner.off(ferridriver::events::ListenerId(id));
      with_page_callbacks(&ctx, |r| r.remove_event_listener(id))?;
      return Ok(());
    }
    let Some(event) = target.as_string() else {
      return Err(rquickjs::Error::new_from_js_message(
        "page.off",
        "TypeError",
        "off(event, listener) expects an event name or a listener id".to_string(),
      ));
    };
    let event = event.to_string()?;
    let listener_fn = listener.0.as_ref().and_then(|v| v.as_function().cloned());
    let Some(listener_fn) = listener_fn else {
      // Lenient `off(event)` — drop every listener for that event
      // (matches `removeAllListeners(event)` semantics).
      let ids = with_page_callbacks(&ctx, |r| r.remove_event_listeners_named(&event))?;
      for id in ids {
        self.inner.off(ferridriver::events::ListenerId(id));
      }
      return Ok(());
    };
    // Playwright form: match the registration by JS function identity
    // (`===`). `Value` equality on objects compares the raw JSValue —
    // exactly strict equality.
    let target_value: rquickjs::Value<'js> = listener_fn.clone().into_value();
    for (id, saved) in with_page_callbacks(&ctx, |r| r.event_listeners_named(&event))? {
      let restored = saved.restore(&ctx)?;
      if restored.into_value() == target_value {
        self.inner.off(ferridriver::events::ListenerId(id));
        with_page_callbacks(&ctx, |r| r.remove_event_listener(id))?;
      }
    }
    Ok(())
  }

  /// `page.removeAllListeners(event?)`. Drops every registered page
  /// listener (or only those for `event` when given) and releases the
  /// persisted JS callbacks. Playwright:
  /// `page.removeAllListeners(type?: string)`.
  #[qjs(rename = "removeAllListeners")]
  pub fn remove_all_listeners(&self, ctx: rquickjs::Ctx<'_>, event: Opt<String>) -> rquickjs::Result<()> {
    if let Some(ev) = event.0 {
      let ids = with_page_callbacks(&ctx, |r| r.remove_event_listeners_named(&ev))?;
      for id in ids {
        self.inner.off(ferridriver::events::ListenerId(id));
      }
      self.inner.remove_listeners_named(&ev);
    } else {
      self.inner.remove_all_listeners();
      with_page_callbacks(&ctx, PageCallbacks::clear_event_listeners)?;
    }
    Ok(())
  }

  // ── Misc page surface (Playwright parity) ────────────────────────────────

  /// `page.waitForTimeout(timeout)`. Sleeps `timeout` ms via the core
  /// async timer (the QuickJS engine has no `setTimeout`). Playwright
  /// discourages this in production code; prefer web-first waits.
  #[qjs(rename = "waitForTimeout")]
  pub async fn wait_for_timeout(&self, timeout: f64) {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let ms = if timeout < 0.0 { 0 } else { timeout as u64 };
    self.inner.wait_for_timeout(ms).await;
  }

  /// `page.requestGC()`. Forces a garbage-collection pass in the
  /// page's JS engine (Playwright parity).
  #[qjs(rename = "requestGC")]
  pub async fn request_gc(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.request_gc().await.into_js_with(&ctx)
  }

  /// `page.consoleMessages(options?)`. Returns the retained console
  /// history as `ConsoleMessage` instances. Accepts `{ filter?: 'all' |
  /// 'since-navigation' }`, defaulting to `since-navigation`.
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

  /// `page.pageErrors(options?)`. Returns the retained uncaught page
  /// exceptions as native JS `Error`s. Accepts `{ filter?: 'all' |
  /// 'since-navigation' }`, defaulting to `since-navigation`.
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

  /// `page.waitForNavigation(options?)`. Resolves when the next
  /// navigation commits. Deprecated in Playwright in favour of
  /// `waitForURL`, kept for parity. Accepts `{ timeout }`.
  #[qjs(rename = "waitForNavigation")]
  pub async fn wait_for_navigation(
    &self,
    ctx: rquickjs::Ctx<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<()> {
    let timeout = opt_timeout_ms(&options)?;
    self.inner.wait_for_navigation(timeout).await.into_js_with(&ctx)
  }

  /// `page.addScriptTag(options)`. Injects a `<script>` tag. Provide
  /// `{ url }` (external) or `{ content }` (inline), optional `{ type }`.
  #[qjs(rename = "addScriptTag")]
  pub async fn add_script_tag(
    &self,
    ctx: rquickjs::Ctx<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<()> {
    let url = opt_str_field(&options, "url")?;
    let content = opt_str_field(&options, "content")?;
    let script_type = opt_str_field(&options, "type")?;
    self
      .inner
      .add_script_tag(url.as_deref(), content.as_deref(), script_type.as_deref())
      .await
      .into_js_with(&ctx)
  }

  /// `page.addStyleTag(options)`. Injects a `<style>` / `<link>`. Provide
  /// `{ url }` (external CSS) or `{ content }` (inline CSS).
  #[qjs(rename = "addStyleTag")]
  pub async fn add_style_tag(
    &self,
    ctx: rquickjs::Ctx<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<()> {
    let url = opt_str_field(&options, "url")?;
    let content = opt_str_field(&options, "content")?;
    self
      .inner
      .add_style_tag(url.as_deref(), content.as_deref())
      .await
      .into_js_with(&ctx)
  }

  /// `page.setExtraHTTPHeaders(headers)`. Sends `headers` (a plain
  /// `Record<string, string>`) with every subsequent request.
  #[qjs(rename = "setExtraHTTPHeaders")]
  pub async fn set_extra_http_headers<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    headers: rquickjs::Value<'js>,
  ) -> rquickjs::Result<()> {
    let map: rustc_hash::FxHashMap<String, String> = serde_from_js(&ctx, headers)?;
    self.inner.set_extra_http_headers(&map).await.into_js_with(&ctx)
  }

  /// `page.bringToFront()`. Activates the page (brings its tab to front).
  #[qjs(rename = "bringToFront")]
  pub async fn bring_to_front(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.bring_to_front().await.into_js_with(&ctx)
  }

  /// `page.isEditable(selector)`. Whether the element is editable.
  #[qjs(rename = "isEditable")]
  pub async fn is_editable(&self, ctx: rquickjs::Ctx<'_>, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_editable(&selector).await.into_js_with(&ctx)
  }

  /// `page.viewportSize()`. Returns `{ width, height }` for the current
  /// viewport. Playwright exposes this as a method (not a property).
  #[qjs(rename = "viewportSize")]
  pub async fn viewport_size<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let (w, h) = self.inner.viewport_size().await.into_js_with(&ctx)?;
    let obj = rquickjs::Object::new(ctx.clone())?;
    obj.set("width", w)?;
    obj.set("height", h)?;
    Ok(obj.into_value())
  }

  /// `page.context()`. Returns the `BrowserContext` this page belongs to.
  #[qjs(rename = "context")]
  pub fn context<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let Some(cref) = self.inner.context() else {
      return Ok(rquickjs::Value::new_null(ctx.clone()));
    };
    let wrapper = crate::bindings::context::BrowserContextJs::new(std::sync::Arc::new(cref.clone()));
    let instance = rquickjs::class::Class::instance(ctx.clone(), wrapper)?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  /// ferridriver-specific (NOT Playwright): click at viewport
  /// coordinates without a selector. Playwright equivalent: `mouse.click(x, y)`.
  #[qjs(rename = "clickAt")]
  pub async fn click_at(&self, ctx: rquickjs::Ctx<'_>, x: f64, y: f64) -> rquickjs::Result<()> {
    self.inner.click_at(x, y).await.into_js_with(&ctx)
  }

  /// ferridriver-specific (NOT Playwright): interpolated mouse move
  /// from `(fromX, fromY)` to `(toX, toY)` in `steps` points. Playwright
  /// equivalent: `mouse.move(x, y, { steps })`.
  #[qjs(rename = "moveMouseSmooth")]
  pub async fn move_mouse_smooth(
    &self,
    ctx: rquickjs::Ctx<'_>,
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
    steps: u32,
  ) -> rquickjs::Result<()> {
    self
      .inner
      .move_mouse_smooth(from_x, from_y, to_x, to_y, steps)
      .await
      .into_js_with(&ctx)
  }

  /// Drag from the source selector to the target selector. Accepts
  /// Playwright's `FrameDragAndDropOptions & TimeoutOptions` bag:
  /// `{ force?, noWaitAfter?, sourcePosition?, targetPosition?, steps?, strict?, timeout?, trial? }`.
  #[qjs(rename = "dragAndDrop")]
  pub async fn drag_and_drop<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    source: String,
    target: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = parse_drag_options(&ctx, options)?;
    self
      .inner
      .drag_and_drop(&source, &target, opts)
      .await
      .into_js_with(&ctx)
  }

  // ── File input ────────────────────────────────────────────────────────────

  /// Attach files to a `<input type="file">` selector. Accepts
  /// Playwright's full `string | string[] | FilePayload | FilePayload[]`
  /// union plus the `PageSetInputFilesOptions` bag.
  #[qjs(rename = "setInputFiles")]
  pub async fn set_input_files<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    files: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let files = crate::bindings::convert::parse_input_files(&ctx, files)?;
    let opts = crate::bindings::convert::parse_set_input_files_options(&ctx, options)?;
    self
      .inner
      .set_input_files(&selector, files, opts)
      .await
      .into_js_with(&ctx)
  }

  // ── Emulation (page-scoped Playwright API) ───────────────────────────────

  /// Override the viewport size for this page. Playwright public:
  /// `page.setViewportSize({ width, height })`.
  /// Playwright: `page.setViewportSize({ width, height })` — a single
  /// object, not two positional numbers.
  #[qjs(rename = "setViewportSize")]
  pub async fn set_viewport_size<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    size: rquickjs::Value<'js>,
  ) -> rquickjs::Result<()> {
    #[derive(serde::Deserialize)]
    struct Size {
      width: i64,
      height: i64,
    }
    let s: Size = crate::bindings::convert::serde_from_js(&ctx, size)?;
    self.inner.set_viewport_size(s.width, s.height).await.into_js_with(&ctx)
  }

  /// Emulate media features. Accepts Playwright's
  /// `{ media?, colorScheme?, reducedMotion?, forcedColors?, contrast? }`
  /// option bag — each call is a partial update layered on top of the
  /// page's persistent emulated-media state.
  #[qjs(rename = "emulateMedia")]
  pub async fn emulate_media<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = parse_emulate_media_options(&ctx, options)?;
    self.inner.emulate_media(&opts).await.into_js_with(&ctx)
  }

  // ── Screenshots / PDF (return raw bytes; pair with `artifacts.writeBytes`) ─

  /// Capture the page as a PNG (raw bytes — Uint8Array in JS). Pair with
  /// `await artifacts.writeBytes('page.png', bytes)` to save to disk.
  /// Optional `options` accept `{ fullPage?: boolean, format?: 'png'|'jpeg'|'webp', quality?: number }`.
  #[qjs(rename = "screenshot")]
  pub async fn screenshot<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<u8>> {
    let opts = parse_screenshot_options(&ctx, options)?;
    self.inner.screenshot(opts).await.into_js_with(&ctx)
  }

  /// Capture a single element as PNG bytes.
  #[qjs(rename = "screenshotElement")]
  pub async fn screenshot_element(&self, ctx: rquickjs::Ctx<'_>, selector: String) -> rquickjs::Result<Vec<u8>> {
    self.inner.screenshot_element(&selector).await.into_js_with(&ctx)
  }

  /// Render the current page as a PDF (raw bytes). Accepts a Playwright-shape
  /// options object: `{ format?, landscape?, printBackground?, scale?, ... }`.
  /// Pair with `await artifacts.writeBytes('page.pdf', bytes)` to save.
  #[qjs(rename = "pdf")]
  pub async fn pdf<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<u8>> {
    let opts = parse_pdf_options(&ctx, options)?;
    self.inner.pdf(opts).await.into_js_with(&ctx)
  }

  // ── Lifecycle ─────────────────────────────────────────────────────────────

  /// Close the page. Accepts `{ runBeforeUnload?, reason? }` to mirror
  /// Playwright's `page.close(options?)`.
  #[qjs(rename = "close")]
  pub async fn close<'js>(&self, ctx: rquickjs::Ctx<'js>, options: Opt<rquickjs::Value<'js>>) -> rquickjs::Result<()> {
    let opts = parse_page_close_options(&ctx, options)?;
    self.inner.close(opts).await.into_js_with(&ctx)?;
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
  #[qjs(rename = "route")]
  pub async fn route<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    url: rquickjs::Value<'js>,
    handler: rquickjs::Function<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let times = parse_route_times(&options)?;
    let vm = self.vm.clone().ok_or_else(|| {
      rquickjs::Error::new_from_js_message(
        "page.route",
        "Error",
        "page.route requires the script engine's VM handle (install_page)".to_string(),
      )
    })?;
    let id = with_page_callbacks(&ctx, PageCallbacks::next_route_id)?;
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
      (url_value_to_matcher(&ctx, url)?, None, None)
    };
    with_page_callbacks(&ctx, |r| {
      r.insert_route(id, self.route_owner(), saved_handler, saved_pred, registry_matcher);
    })?;

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
              .ok_or_else(|| rquickjs::Error::new_from_js_message("page.route", "Error", "route predicate gone".to_string()))?;
            let pred = saved_pred.restore(&ctx)?;
            let url_ctor: rquickjs::function::Constructor<'_> = ctx.globals().get("URL")?;
            let url_obj: rquickjs::Value<'_> = url_ctor.construct((route.request().url.clone(),))?;
            let truthy = crate::bindings::fetch::bracket_net(
              crate::bindings::fetch::policy_cell(&ctx),
              saved_pred.net().cloned(),
              call_predicate_truthy(&pred, url_obj, &ctx),
            )
            .await?;
            if !truthy {
              route.continue_route(ferridriver::route::ContinueOverrides::default());
              return Ok(());
            }
          }
          let f = with_page_callbacks(&ctx, |r| r.get_route_handler(id))?
            .ok_or_else(|| rquickjs::Error::new_from_js_message("page.route", "Error", "route handler gone".to_string()))?;
          let route_class = Class::instance(ctx.clone(), crate::bindings::network::RouteJs::new(route))?;
          let _: rquickjs::Value<'_> = f.call_bracketed(&ctx, (route_class,))?;
          Ok(())
        })
        .await;
      });
    });

    let disposable = self
      .inner
      .route(matcher, rust_handler, times)
      .await
      .into_js_with(&ctx)?;
    let instance =
      rquickjs::class::Class::instance(ctx.clone(), crate::bindings::disposable::DisposableJs::new(disposable))?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  /// Playwright: `page.routeWebSocket(url, handler)`. Intercepts
  /// WebSocket connections matching `url` (glob string or `RegExp`); the
  /// handler receives a live `WebSocketRoute`. Cross-task dispatch mirrors
  /// `page.route` — the handler is restored + invoked inside `async_with!`,
  /// awaited so the driver observes `connectToServer()` before opening.
  #[qjs(rename = "routeWebSocket")]
  pub async fn route_web_socket<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    url: rquickjs::Value<'js>,
    handler: rquickjs::Function<'js>,
  ) -> rquickjs::Result<()> {
    let vm = self.vm.clone().ok_or_else(|| {
      rquickjs::Error::new_from_js_message(
        "page.routeWebSocket",
        "Error",
        "page.routeWebSocket requires the script engine's VM handle (install_page)".to_string(),
      )
    })?;
    let matcher = url_value_to_matcher(&ctx, url)?;
    let handler_id = with_page_callbacks(&ctx, PageCallbacks::next_route_id)?;
    let owner = RouteOwner::Page(self.inner.backend_page_id());
    let saved = SavedCallback::save(&ctx, handler);
    with_page_callbacks(&ctx, |r| r.insert_ws_callback(handler_id, owner.clone(), saved))?;

    let rust_handler = crate::bindings::web_socket_route::build_ws_route_handler(vm, handler_id, owner);
    self
      .inner
      .route_web_socket(matcher, rust_handler)
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `page.routeFromHAR(har, options?)`. Replay-only.
  #[qjs(rename = "routeFromHAR")]
  pub async fn route_from_har(
    &self,
    ctx: rquickjs::Ctx<'_>,
    har: String,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<()> {
    let opts = parse_har_options(&options)?;
    self
      .inner
      .route_from_har(std::path::Path::new(&har), opts)
      .await
      .into_js_with(&ctx)
  }

  /// `page.unroute(string | RegExp | ((url: URL) => boolean))`. A
  /// predicate is matched by `===` identity against the function passed
  /// to `route`, then its always-true core matcher is dropped by `Arc`
  /// identity so sibling predicate routes survive.
  #[qjs(rename = "unroute")]
  pub async fn unroute<'js>(&self, ctx: rquickjs::Ctx<'js>, url: rquickjs::Value<'js>) -> rquickjs::Result<()> {
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
          self.inner.unroute(&m).await.into_js_with(&ctx)?;
        }
      }
      return Ok(());
    }
    let matcher = url_value_to_matcher(&ctx, url)?;
    self.inner.unroute(&matcher).await.into_js_with(&ctx)
  }

  /// `page.unrouteAll(options?: { behavior?: 'wait' | 'ignoreErrors' | 'default' })`.
  /// Removes every route registered via `page.route`, clearing the script-side
  /// predicate/handler tables too.
  #[qjs(rename = "unrouteAll")]
  pub async fn unroute_all<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let behavior = match options.0.and_then(rquickjs::Value::into_object) {
      Some(obj) => match obj.get::<_, Option<String>>("behavior")? {
        Some(b) => Some(parse_unroute_behavior(&b)?),
        None => None,
      },
      None => None,
    };
    self.inner.unroute_all(behavior).await.into_js_with(&ctx)?;
    with_page_callbacks(&ctx, |r| r.remove_routes_for_owner(&self.route_owner()))?;
    Ok(())
  }

  /// `page.addLocatorHandler(locator, handler, options?: { times?, noWaitAfter? })`.
  /// Registers `handler` to run whenever `locator` becomes visible during an
  /// actionability wait (dismissing overlays/modals). Mirrors Playwright
  /// `client/page.ts:397`.
  ///
  /// The JS handler runs cross-task via the session `AsyncContext` (same
  /// bridge as `page.route`): the core checkpoint awaits a oneshot that the
  /// spawned dispatch task fulfils once the handler (and any returned
  /// promise) settles, so the original action only resumes afterwards.
  #[qjs(rename = "addLocatorHandler")]
  pub fn add_locator_handler(
    &self,
    ctx: rquickjs::Ctx<'_>,
    _locator: rquickjs::Class<'_, LocatorJs>,
    _handler: rquickjs::Function<'_>,
    _options: Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<()> {
    // The handler must run *during* an in-progress action's actionability
    // wait. In the QuickJS scripting engine every action executes inside an
    // exclusive `async_with` over the single session VM, so a nested
    // handler callback can never acquire the VM until the action finishes --
    // invoking it would deadlock. Playwright sidesteps this with a
    // client/server split; ferridriver-script has none, so this is a typed
    // Unsupported rather than a hang. The core + NAPI layers support it fully.
    ferridriver::error::Result::<()>::Err(ferridriver::error::FerriError::unsupported(
      "page.addLocatorHandler is not available in the QuickJS scripting engine \
       (handlers cannot fire during an in-VM action without deadlocking the \
       single-threaded VM); use the NAPI/core API for locator handlers",
    ))
    .into_js_with(&ctx)
  }

  /// `page.removeLocatorHandler(locator)`. Drops every handler registered for
  /// `locator` (by selector) and releases the persisted JS callbacks. Mirrors
  /// Playwright `client/page.ts:423`.
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
  #[qjs(rename = "pickLocator")]
  pub async fn pick_locator(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<LocatorJs> {
    let loc = self.inner.pick_locator().await.into_js_with(&ctx)?;
    Ok(LocatorJs::new(loc))
  }

  /// `page.cancelPickLocator(): Promise<void>`.
  #[qjs(rename = "cancelPickLocator")]
  pub async fn cancel_pick_locator(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.cancel_pick_locator().await.into_js_with(&ctx)
  }

  /// `page.hideHighlight(): Promise<void>`.
  #[qjs(rename = "hideHighlight")]
  pub async fn hide_highlight(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.hide_highlight().await.into_js_with(&ctx)
  }

  // ── Network lifecycle waits ──────────────────────────────────────────────
  //
  // Mirror Playwright's `page.waitForRequest` / `page.waitForResponse` /
  // `page.waitForEvent('websocket')` — return live `RequestJs` /
  // `ResponseJs` / `WebSocketJs` so callers can inspect headers, body,
  // failure, etc.

  /// `page.waitForRequest(string | RegExp | ((r: Request) => boolean |
  /// Promise<boolean>), options?)`.
  #[qjs(rename = "waitForRequest")]
  pub async fn wait_for_request<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    url: rquickjs::Value<'js>,
    timeout_ms: Opt<f64>,
  ) -> rquickjs::Result<crate::bindings::network::RequestJs> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let timeout = timeout_ms.0.map(|t| t as u64);
    if let Some(pred) = url.as_function() {
      let t = timeout.unwrap_or_else(|| self.inner.default_timeout());
      return wait_request_predicate(ctx.clone(), self.inner.clone(), pred.clone(), t).await;
    }
    let matcher = url_value_to_matcher(&ctx, url)?;
    let req = self.inner.wait_for_request(matcher, timeout).await.into_js_with(&ctx)?;
    Ok(crate::bindings::network::RequestJs::new_with_page(
      req,
      self.inner.clone(),
    ))
  }

  /// `page.waitForResponse(string | RegExp | ((r: Response) => boolean |
  /// Promise<boolean>), options?)`.
  #[qjs(rename = "waitForResponse")]
  pub async fn wait_for_response<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    url: rquickjs::Value<'js>,
    timeout_ms: Opt<f64>,
  ) -> rquickjs::Result<crate::bindings::network::ResponseJs> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let timeout = timeout_ms.0.map(|t| t as u64);
    if let Some(pred) = url.as_function() {
      let t = timeout.unwrap_or_else(|| self.inner.default_timeout());
      return wait_response_predicate(ctx.clone(), self.inner.clone(), pred.clone(), t).await;
    }
    let matcher = url_value_to_matcher(&ctx, url)?;
    let resp = self
      .inner
      .wait_for_response(matcher, timeout)
      .await
      .into_js_with(&ctx)?;
    Ok(crate::bindings::network::ResponseJs::new_with_page(
      resp,
      self.inner.clone(),
    ))
  }

  /// Mirrors Playwright `page.waitForEvent(event, options?)`. Dispatches
  /// on the event name and returns the live class for the lifecycle
  /// events (`Request` / `Response` / `WebSocket`), or a snapshot object
  /// for simpler events. The overloaded return keeps the Playwright-
  /// canonical call shape — scripts write `await page.waitForEvent('websocket')`
  /// and receive a real `WebSocket` instance.
  /// Playwright: `page.waitForLoadState(state?: 'load' |
  /// 'domcontentloaded' | 'networkidle', options?)`. Defaults to
  /// `'load'`. Thin delegator to `Page::wait_for_load_state`.
  #[qjs(rename = "waitForLoadState")]
  pub async fn wait_for_load_state(&self, ctx: rquickjs::Ctx<'_>, state: Opt<String>) -> rquickjs::Result<()> {
    self
      .inner
      .wait_for_load_state(state.0.as_deref())
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `page.waitForURL(url: string | RegExp | (url:URL) =>
  /// boolean, options?)`. Thin delegator to `Page::wait_for_url`
  /// (a function predicate is reduced to an always-true matcher; the
  /// function check is enforced by the core polling against the
  /// current URL).
  #[qjs(rename = "waitForURL")]
  pub async fn wait_for_url<'js>(&self, ctx: rquickjs::Ctx<'js>, url: rquickjs::Value<'js>) -> rquickjs::Result<()> {
    let matcher = url_value_to_matcher(&ctx, url)?;
    self.inner.wait_for_url(matcher).await.into_js_with(&ctx)
  }

  /// Playwright: `page.waitForFunction(pageFunction: Function|string,
  /// arg?, options?: { timeout?, polling? })`. Function values get
  /// `String(fn)` (Playwright parity) and are evaluated as IIFEs
  /// inside the page. Returns the truthy value the function resolved
  /// to.
  #[qjs(rename = "waitForFunction")]
  pub async fn wait_for_function<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    page_function: rquickjs::Value<'js>,
    _arg: Opt<rquickjs::Value<'js>>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    #[derive(serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase", default)]
    struct JsOpts {
      timeout: Option<u64>,
    }
    let opts: JsOpts = match options.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => crate::bindings::convert::serde_from_js(&ctx, v)?,
      _ => JsOpts::default(),
    };
    let (src, is_fn) = crate::bindings::convert::extract_page_function(&ctx, page_function)?;
    // For a function: invoke it as `(<src>)()` so the body's return is
    // the polled value. For a string: use as-is (the user passes an
    // expression string, like Playwright).
    let expr = if is_fn.unwrap_or(false) {
      format!("({src})()")
    } else {
      src
    };
    let v = self
      .inner
      .wait_for_function(&expr, opts.timeout)
      .await
      .map_err(|e| crate::bindings::convert::ferri_throw(&ctx, &e))?;
    crate::bindings::convert::json_to_js(&ctx, &v)
  }

  /// `page.waitForEvent(event, optionsOrPredicate?)`. The second
  /// argument is a predicate function, a `{ predicate?, timeout? }`
  /// bag (Playwright shape), or a bare timeout in ms (ferridriver
  /// extension). The predicate receives the same live event object a
  /// `page.on` listener would and the wait resolves on the first event
  /// for which it returns truthy.
  #[qjs(rename = "waitForEvent")]
  pub async fn wait_for_event<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    event: String,
    options_or_predicate: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    use rquickjs::class::Class;
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
    let timeout = timeout_ms.map_or_else(|| self.inner.default_timeout(), crate::bindings::convert::ms_f64_to_u64);
    let event_lc = event.to_ascii_lowercase();

    // Predicate waits drain the broadcast for every event type — the
    // emitter bridge claims dialog / filechooser / download live
    // handles on behalf of broadcast listeners, so the predicate sees
    // the same live object a `page.on` listener would.
    if let Some(pred) = predicate {
      let mut rx = self.inner.events().subscribe();
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
        let arg = page_event_to_js(&ctx, &self.inner, ev.clone())?;
        if call_predicate_truthy(&pred, arg, &ctx).await? {
          return page_event_to_js(&ctx, &self.inner, ev);
        }
      }
    }

    // `dialog` bypasses the broadcast — it registers a one-shot
    // handler on the per-page `DialogManager` so the claim is
    // synchronous at `did_open` time (mirrors Playwright's
    // `addDialogHandler` + `dialogDidOpen` flow exactly).
    if event_lc == "dialog" {
      let dialog = self.inner.wait_for_dialog(timeout).await.into_js_with(&ctx)?;
      let wrapper = crate::bindings::dialog::DialogJs::new(dialog);
      let instance = Class::instance(ctx.clone(), wrapper)?;
      return rquickjs::IntoJs::into_js(instance, &ctx);
    }
    // Same pattern for `filechooser` — one-shot handler on the
    // per-page `FileChooserManager` so the claim is synchronous with
    // the backend event arrival.
    if event_lc == "filechooser" {
      let chooser = self.inner.wait_for_file_chooser(timeout).await.into_js_with(&ctx)?;
      let wrapper = crate::bindings::file_chooser::FileChooserJs::new(chooser);
      let instance = Class::instance(ctx.clone(), wrapper)?;
      return rquickjs::IntoJs::into_js(instance, &ctx);
    }
    // And for `download` — same one-shot handler pattern via the
    // per-page `DownloadManager`.
    if event_lc == "download" {
      let download = self.inner.wait_for_download(timeout).await.into_js_with(&ctx)?;
      let wrapper = crate::bindings::download::DownloadJs::new(download);
      let instance = Class::instance(ctx.clone(), wrapper)?;
      return rquickjs::IntoJs::into_js(instance, &ctx);
    }

    let name = event_lc.clone();
    let ev = self
      .inner
      .events()
      .wait_for(move |e| match_event_name(&name, e), timeout)
      .await
      .into_js_with(&ctx)?;
    page_event_to_js(&ctx, &self.inner, ev)
  }

  // ── Frames (sync, Playwright parity — task 3.8) ─────────────────────
  //
  // Mirrors `/tmp/playwright/packages/playwright-core/src/client/page.ts:258-275`
  // — `mainFrame`, `frames`, `frame(selector)` are all sync and read
  // from the page-owned [`ferridriver::frame_cache::FrameCache`].

  /// Main frame of this page. Playwright: `page.mainFrame(): Frame`.
  /// Always returns a Frame — the cache is seeded inside `Page::new` /
  /// `Page::with_context` before the Page is handed out.
  #[qjs(rename = "mainFrame")]
  pub fn main_frame(&self) -> crate::bindings::frame::FrameJs {
    crate::bindings::frame::FrameJs::new(self.inner.main_frame())
  }

  /// All non-detached frames on the page. Playwright:
  /// `page.frames(): Frame[]`.
  #[qjs(rename = "frames")]
  pub fn frames(&self) -> Vec<crate::bindings::frame::FrameJs> {
    self
      .inner
      .frames()
      .into_iter()
      .map(crate::bindings::frame::FrameJs::new)
      .collect()
  }

  /// Playwright: `page.frameLocator(selector): FrameLocator`. Targets
  /// an `<iframe>` matching the selector at the page's main-frame
  /// scope.
  #[qjs(rename = "frameLocator")]
  pub fn frame_locator(&self, selector: String) -> crate::bindings::frame_locator::FrameLocatorJs {
    crate::bindings::frame_locator::FrameLocatorJs::new(self.inner.frame_locator(&selector))
  }

  /// Locate a frame by name or URL. Accepts Playwright's union:
  /// `frame(string | { name?: string; url?: string })`.
  ///
  /// Distinct null/undefined handling (like emulateMedia in task 3.24)
  /// is not required here — both absent and explicit-null mean "no
  /// filter on this field", which matches Playwright's optional-field
  /// semantics.
  #[qjs(rename = "frame")]
  pub fn frame<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: rquickjs::Value<'js>,
  ) -> rquickjs::Result<Option<crate::bindings::frame::FrameJs>> {
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
      return Ok(None);
    };

    if core_sel.is_empty() {
      return Ok(None);
    }
    Ok(self.inner.frame(core_sel).map(crate::bindings::frame::FrameJs::new))
  }

  /// Playwright: `page.touchscreen: Touchscreen`.
  #[qjs(rename = "touchscreen", get)]
  pub fn touchscreen(&self) -> TouchscreenJs {
    TouchscreenJs {
      page: self.inner.clone(),
    }
  }

  /// ferridriver-specific (NOT Playwright): structured AI snapshot
  /// `{ full: string, incremental?: string, refMap: Record<string, number> }`.
  /// Playwright's public accessibility API is `ariaSnapshot` (string);
  /// this richer shape feeds the MCP server's incremental tracking.
  #[qjs(rename = "snapshotForAI")]
  pub async fn snapshot_for_ai<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
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
    let snap = self.inner.snapshot_for_ai(core_opts).await.into_js_with(&ctx)?;
    let obj = rquickjs::Object::new(ctx.clone())?;
    obj.set("full", snap.full)?;
    if let Some(inc) = snap.incremental {
      obj.set("incremental", inc)?;
    }
    let ref_map = rquickjs::Object::new(ctx.clone())?;
    for (k, v) in snap.ref_map {
      ref_map.set(k, v as f64)?;
    }
    obj.set("refMap", ref_map)?;
    rquickjs::IntoJs::into_js(obj, &ctx)
  }

  /// Playwright `page.ariaSnapshot(options?): Promise<string>`.
  #[qjs(rename = "ariaSnapshot")]
  pub async fn aria_snapshot<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<String> {
    let core_opts = match options.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => {
        #[derive(serde::Deserialize, Default)]
        #[serde(rename_all = "camelCase", default)]
        struct JsSnap {
          depth: Option<i32>,
          track: Option<String>,
        }
        let p: JsSnap = crate::bindings::convert::serde_from_js(&ctx, v)?;
        ferridriver::snapshot::SnapshotOptions {
          depth: p.depth,
          track: p.track,
        }
      },
      _ => ferridriver::snapshot::SnapshotOptions::default(),
    };
    self.inner.aria_snapshot(core_opts).await.into_js_with(&ctx)
  }

  /// Playwright: `page.exposeFunction(name, callback)`. Binds
  /// `window[name]` to a page-side proxy that asynchronously invokes
  /// `callback(args)` in the script context.
  ///
  /// The callback receives the args as a single array. The page-side
  /// call resolves to `null` since the script-side callback runs
  /// asynchronously (Rust core's `ExposedFn` is sync + JSON-in/out;
  /// QuickJS dispatch is async-only).
  #[qjs(rename = "exposeFunction")]
  pub async fn expose_function<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    name: String,
    callback: rquickjs::Function<'js>,
  ) -> rquickjs::Result<()> {
    let vm = self.vm.clone().ok_or_else(|| {
      rquickjs::Error::new_from_js_message(
        "page.exposeFunction",
        "Error",
        "page.exposeFunction requires the script engine's VM handle (install_page)".to_string(),
      )
    })?;
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
          let out: Result<rquickjs::Result<serde_json::Value>, crate::error::ScriptError> =
            crate::vm_with!(vm => |ctx| {
              let saved = with_page_callbacks(&ctx, |r| r.exposed.get(&name).cloned())?
                .ok_or_else(|| {
                  rquickjs::Error::new_from_js_message(
                    "page.exposeFunction",
                    "Error",
                    "exposed callback gone".to_string(),
                  )
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
                // which rquickjs-serde turns every number into a
                // `{$serde_json::private::Number}` object. The AP-safe
                // walker keeps numbers as JS numbers.
                call_args.push_arg(crate::bindings::convert::json_to_js(&ctx, &v)?)?;
              }
              let res = crate::bindings::fetch::bracket_net(
                crate::bindings::fetch::policy_cell(&ctx),
                saved.net().cloned(),
                async {
                  let mp: rquickjs::promise::MaybePromise<'_> = call_args.apply(&f)?;
                  mp.into_future::<rquickjs::Value<'_>>().await
                },
              )
              .await?;
              // Round-trip through QuickJS `JSON.stringify` + serde_json's
              // own parser — AP-safe both ways (a non-serde_json
              // deserializer mis-handles numbers under
              // `arbitrary_precision`). `undefined`/function -> null.
              let json = match ctx.json_stringify(res)? {
                Some(s) => serde_json::from_str(&s.to_string()?).unwrap_or(serde_json::Value::Null),
                None => serde_json::Value::Null,
              };
              Ok(json)
            })
            .await;
          out.map_or(serde_json::Value::Null, |inner| {
            inner.unwrap_or(serde_json::Value::Null)
          })
        })
      }
    });
    self.inner.expose_function(&name, cb).await.into_js_with(&ctx)
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
  #[qjs(rename = "startScreencast")]
  pub async fn start_screencast<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    quality: u8,
    max_width: u32,
    max_height: u32,
    callback: rquickjs::Function<'js>,
  ) -> rquickjs::Result<()> {
    let saved = SavedCallback::save(&ctx, callback);
    with_page_callbacks(&ctx, |r| r.screencast = Some(saved))?;
    // `start_screencast` returns `(rx, shutdown_tx)`. The QuickJS
    // binding doesn't expose a stop hook here; the shutdown signal is
    // dropped (which Chrome's stop-screencast path will subsequently
    // see via teardown), and we forward frames until `stopScreencast`
    // clears the callback or the channel closes.
    let (mut rx, _shutdown) = self
      .inner
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
          let _: rquickjs::Value<'_> =
            crate::bindings::fetch::call_with_net(&pump_ctx, saved.net(), || f.call((payload,)))?;
          Ok(())
        };
        // A throwing callback is swallowed so one bad frame handler
        // can't kill the pump (same policy as the event pump).
        let _ = deliver();
      }
    });
    Ok(())
  }

  /// ferridriver-specific (NOT Playwright): stop the screencast
  /// started by `startScreencast`. Clears the persisted frame callback
  /// (ending the frame pump) so it doesn't outlive the screencast in
  /// the session VM's registry.
  #[qjs(rename = "stopScreencast")]
  pub async fn stop_screencast(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.stop_screencast().await.into_js_with(&ctx)?;
    with_page_callbacks(&ctx, |r| r.screencast = None)?;
    Ok(())
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
  #[qjs(rename = "tap")]
  pub async fn tap(&self, ctx: rquickjs::Ctx<'_>, x: f64, y: f64) -> rquickjs::Result<()> {
    self.page.touchscreen().tap(x, y).await.into_js_with(&ctx)
  }
}
