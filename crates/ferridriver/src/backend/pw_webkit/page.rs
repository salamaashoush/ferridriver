//! Playwright `WebKit` page handle — the full ferridriver page API over
//! the PW `WebKit` Inspector protocol.
//!
//! A page owns three [`Session`]s: the root browser session (for
//! `Playwright.navigate` / screenshot), the page-proxy session (for
//! `Target.*` / `Dialog.*`), and the inner target session (for `Page.*`
//! / `Runtime.*` / `DOM.*` / `Network.*` / `Input.*`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine as _;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use super::browser::{BrowserError, PwWebKitBrowser};
use super::connection::Session;
use super::element::PwWebKitElement;
use super::protocol::{self, Envelope, NavigateParams, NavigateResult};
use crate::backend::{
  AnyElement, AxNodeData, CookieData, FrameInfo, ImageFormat, MetricData, NavLifecycle, ScreenshotOpts,
};
use crate::console_message::ConsoleMessage;
use crate::error::{FerriError, Result};
use crate::events::EventEmitter;
use crate::network::{Request as NetworkRequest, Response};
use crate::state::DialogEvent;

/// Name of the utility execution context — mirrors `UTILITY_WORLD_NAME`
/// in `wkPage.ts`.
pub const UTILITY_WORLD_NAME: &str = "__playwright_utility_world__";

/// Playwright `WebKit` page. Cheaply cloneable; clones share the
/// underlying sessions + managers.
#[derive(Clone)]
pub struct PwWebKitPage {
  proxy: Session,
  target: Session,
  browser: Session,
  proxy_id: Arc<str>,
  target_id: Arc<str>,
  context_id: Option<Arc<str>>,
  closed: Arc<AtomicBool>,
  /// Latch: the `window.__fd` selector engine has been injected.
  engine_injected: Arc<AtomicBool>,
  /// Cached `objectId` of the main-world global. `Runtime.callFunctionOn`
  /// needs an `objectId` anchor (no `contextId` form); evaluating
  /// `"this"` once gives a stable handle into the page's main realm.
  /// Cleared on navigation — a new document means a new global.
  /// Mirrors `WKExecutionContext._contextGlobalObjectId`.
  global_object_id: Arc<std::sync::Mutex<Option<String>>>,
  /// Exposed-function callback registry. Keyed by the JS-side function
  /// name; the listener task dispatches `Runtime.bindingCalled` events
  /// back through these callbacks.
  exposed_fns: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, crate::events::ExposedFn>>>,
  /// Idempotent latch for the `Runtime.addBinding` + listener setup.
  binding_initialized: Arc<AtomicBool>,
  /// Live request table, keyed by PW `WebKit` `requestId`. The network
  /// listener inserts on `Network.requestWillBeSent`, links responses
  /// on `Network.responseReceived`, and removes on terminal
  /// finished/failed events.
  pub(crate) requests: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::network::Request>>>,
  /// Slot holding the most-recent main-document `Request` so
  /// [`Self::goto`] / [`Self::reload`] / history traversals can resolve
  /// the navigation `Response` without polling.
  pub(crate) nav_request_slot: crate::network::NavRequestSlot,
  pub events: EventEmitter,
  pub dialog_manager: crate::dialog::DialogManager,
  pub file_chooser_manager: crate::file_chooser::FileChooserManager,
  pub download_manager: crate::download::DownloadManager,
  pub page_backref: crate::backend::PageBackref,
  pub(crate) frame_cache: Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  pub(crate) frame_listener_started: Arc<AtomicBool>,
}

impl PwWebKitPage {
  /// Attach to a freshly-created page proxy: wait for the inner
  /// `Target.targetCreated`, open the target session, run the standard
  /// `*.enable` initialisation (mirrors `WKPage._initializeSessionMayThrow`).
  pub async fn attach(
    browser: &PwWebKitBrowser,
    proxy: Session,
    context_id: Option<String>,
  ) -> std::result::Result<Self, BrowserError> {
    let conn = browser.connection();
    let proxy_id = proxy.page_proxy_id().unwrap_or_default().to_string();
    let target_id = wait_for_first_page_target(&proxy).await?;
    let target = conn.target_session(&proxy_id, &target_id);

    // Page agent before Runtime so executionContextCreated ordering holds.
    target.send("Page.enable", json!({})).await?;
    target.send("Runtime.enable", json!({})).await?;
    target.send("Network.enable", json!({})).await?;
    target.send("Console.enable", json!({})).await?;
    // Dialog domain lives on the page-proxy session (per wkPage.ts);
    // without `Dialog.enable` the `javascriptDialogOpening` event
    // never fires and `window.alert` would wedge the page.
    proxy.send("Dialog.enable", json!({})).await?;
    // Intercept the native file picker so `Page.fileChooserOpened` fires
    // instead of the browser opening an OS dialog.
    let _ = target
      .send("Page.setInterceptFileChooserDialog", json!({ "enabled": true }))
      .await;
    let _ = target
      .send("Page.createUserWorld", json!({ "name": UTILITY_WORLD_NAME }))
      .await;
    let _ = target.send("Page.getResourceTree", json!({})).await;

    Ok(PwWebKitPage {
      proxy,
      target,
      browser: browser.root().clone(),
      proxy_id: Arc::from(proxy_id),
      target_id: Arc::from(target_id),
      context_id: context_id.map(Arc::from),
      closed: Arc::new(AtomicBool::new(false)),
      engine_injected: Arc::new(AtomicBool::new(false)),
      global_object_id: Arc::new(std::sync::Mutex::new(None)),
      exposed_fns: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
      binding_initialized: Arc::new(AtomicBool::new(false)),
      requests: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      nav_request_slot: crate::network::NavRequestSlot::new(),
      events: EventEmitter::new(),
      dialog_manager: crate::dialog::DialogManager::new(),
      file_chooser_manager: crate::file_chooser::FileChooserManager::new(),
      download_manager: crate::download::DownloadManager::new(),
      page_backref: crate::backend::PageBackref::new(),
      frame_cache: Arc::new(std::sync::Mutex::new(crate::frame_cache::FrameCache::default())),
      frame_listener_started: Arc::new(AtomicBool::new(false)),
    })
  }

  #[must_use]
  pub fn page_proxy_id(&self) -> &str {
    &self.proxy_id
  }

  #[must_use]
  pub fn target_id(&self) -> &str {
    &self.target_id
  }

  #[must_use]
  pub(crate) fn target_session(&self) -> &Session {
    &self.target
  }

  #[must_use]
  pub(crate) fn proxy_session(&self) -> &Session {
    &self.proxy
  }

  // ── Protocol helpers ──────────────────────────────────────────────────

  /// `Runtime.evaluate` on the target session. `return_by_value`
  /// controls whether the reply inlines the value or returns an
  /// `objectId` handle.
  async fn runtime_evaluate(&self, expression: &str, return_by_value: bool) -> Result<Value> {
    let resp = self
      .target
      .send(
        protocol::RUNTIME_EVALUATE,
        json!({
          "expression": expression,
          "returnByValue": return_by_value,
        }),
      )
      .await
      .map_err(conn_err)?;
    if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
      let msg = resp
        .get("result")
        .and_then(|r| r.get("description").or_else(|| r.get("value")))
        .and_then(Value::as_str)
        .unwrap_or("evaluation threw")
        .to_string();
      return Err(FerriError::evaluation(msg));
    }
    Ok(resp)
  }

  /// Evaluate `expression` and return the inlined JSON value.
  async fn eval_value(&self, expression: &str) -> Result<Value> {
    let resp = self.runtime_evaluate(expression, true).await?;
    Ok(
      resp
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(Value::Null),
    )
  }

  /// Drop realm-scoped caches — a new document invalidates the engine
  /// injection and the cached global `objectId`.
  fn reset_realm(&self) {
    self.engine_injected.store(false, Ordering::Relaxed);
    *self.global_object_id.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
  }

  /// Cached `objectId` of the main-world global, evaluating `"this"`
  /// once. The anchor `Runtime.callFunctionOn` runs against.
  async fn global_anchor(&self) -> Result<String> {
    if let Some(id) = self
      .global_object_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
    {
      return Ok(id);
    }
    let resp = self.runtime_evaluate("this", false).await?;
    let id = resp
      .get("result")
      .and_then(|r| r.get("objectId"))
      .and_then(Value::as_str)
      .ok_or_else(|| FerriError::protocol("Runtime.evaluate", "global anchor: no objectId"))?
      .to_string();
    *self.global_object_id.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id.clone());
    Ok(id)
  }

  fn ensure_open(&self) -> Result<()> {
    if self.closed.load(Ordering::Relaxed) {
      return Err(FerriError::backend("pw-webkit: page is closed"));
    }
    Ok(())
  }

  // ── Frames ────────────────────────────────────────────────────────────

  pub async fn get_frame_tree(&self) -> Result<Vec<FrameInfo>> {
    let resp = self
      .target
      .send("Page.getResourceTree", json!({}))
      .await
      .map_err(conn_err)?;
    let mut frames = Vec::new();
    if let Some(root) = resp.get("frameTree") {
      collect_frames(root, None, &mut frames);
    }
    Ok(frames)
  }

  #[must_use]
  pub fn peek_main_frame_id(&self) -> Option<String> {
    let id = self.target_id.to_string();
    (!id.is_empty()).then_some(id)
  }

  pub async fn evaluate_in_frame(&self, expression: &str, _frame_id: &str) -> Result<Option<Value>> {
    // Per-frame execution contexts are a later batch — main frame only.
    Ok(Some(self.eval_value(expression).await?))
  }

  pub async fn content_frame_id(&self, _object_id: &str) -> Result<Option<String>> {
    tokio::task::yield_now().await;
    Ok(None)
  }

  // ── Navigation ────────────────────────────────────────────────────────

  pub async fn goto(
    &self,
    url: &str,
    lifecycle: NavLifecycle,
    timeout_ms: u64,
    referrer: Option<&str>,
  ) -> Result<Option<Response>> {
    self.ensure_open()?;
    self.reset_realm();
    self.nav_request_slot.clear();
    let params = NavigateParams {
      url: url.to_string(),
      page_proxy_id: self.proxy_id.to_string(),
      frame_id: None,
      referrer: referrer.map(str::to_string),
    };
    let nav = tokio::time::timeout(
      std::time::Duration::from_millis(timeout_ms),
      self.browser.send(protocol::PLAYWRIGHT_NAVIGATE, serde_json::to_value(&params)?),
    )
    .await
    .map_err(|_| FerriError::timeout(format!("navigating to {url}"), timeout_ms))?
    .map_err(conn_err)?;
    let parsed: NavigateResult = serde_json::from_value(nav).unwrap_or_default();
    if let Some(err) = parsed.error_text {
      if !err.is_empty() {
        return Err(FerriError::backend(format!("pw-webkit navigate: {err}")));
      }
    }
    if parsed.loader_id.is_some() {
      self.wait_for_lifecycle(lifecycle, timeout_ms).await?;
    }
    Ok(self.await_nav_response().await)
  }

  /// Resolve the main-document `Response` captured by the network
  /// listener for the most recent navigation.
  async fn await_nav_response(&self) -> Option<Response> {
    let req = self.nav_request_slot.get()?;
    req.response().await.ok().flatten()
  }

  /// Wait for the target-session lifecycle event matching `lifecycle`,
  /// or for `Network.loadingFailed` on the main-document request
  /// (so an unreachable navigation surfaces as an error instead of
  /// wedging until `timeout_ms`).
  async fn wait_for_lifecycle(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<()> {
    let want = match lifecycle {
      NavLifecycle::Commit => "Page.frameNavigated",
      NavLifecycle::DomContentLoaded => "Page.domContentEventFired",
      NavLifecycle::Load => "Page.loadEventFired",
    };
    let mut rx = self.target.events();
    let nav_slot = self.nav_request_slot.clone();
    let result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async move {
      loop {
        match rx.recv().await {
          Ok(env) => match env.method.as_deref() {
            Some(m) if m == want => return Ok(()),
            Some("Network.loadingFailed") => {
              if let Some(req) = nav_slot.get() {
                let event_id = env.params.get("requestId").and_then(Value::as_str).unwrap_or("");
                if event_id == req.id() {
                  let err = env
                    .params
                    .get("errorText")
                    .and_then(Value::as_str)
                    .unwrap_or("navigation failed")
                    .to_string();
                  return Err(FerriError::backend(format!("pw-webkit navigate: {err}")));
                }
              }
            },
            _ => {},
          },
          Err(RecvError::Lagged(_)) => {},
          Err(RecvError::Closed) => return Ok(()),
        }
      }
    })
    .await;
    match result {
      Ok(inner) => inner,
      Err(_) => Ok(()),
    }
  }

  pub async fn wait_for_navigation(&self) -> Result<()> {
    self.wait_for_lifecycle(NavLifecycle::Load, 30_000).await
  }

  pub async fn reload(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.ensure_open()?;
    self.reset_realm();
    self.nav_request_slot.clear();
    self.target.send("Page.reload", json!({})).await.map_err(conn_err)?;
    let _ = self.wait_for_lifecycle(lifecycle, timeout_ms).await;
    Ok(self.await_nav_response().await)
  }

  pub async fn go_back(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.history_traverse("Page.goBack", lifecycle, timeout_ms).await
  }

  pub async fn go_forward(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.history_traverse("Page.goForward", lifecycle, timeout_ms).await
  }

  async fn history_traverse(
    &self,
    method: &str,
    lifecycle: NavLifecycle,
    timeout_ms: u64,
  ) -> Result<Option<Response>> {
    self.ensure_open()?;
    self.reset_realm();
    self.nav_request_slot.clear();
    self.target.send(method, json!({})).await.map_err(conn_err)?;
    let _ = self.wait_for_lifecycle(lifecycle, timeout_ms).await;
    Ok(self.await_nav_response().await)
  }

  pub async fn url(&self) -> Result<Option<String>> {
    Ok(self.eval_value("location.href").await?.as_str().map(String::from))
  }

  pub async fn title(&self) -> Result<Option<String>> {
    Ok(self.eval_value("document.title").await?.as_str().map(String::from))
  }

  // ── JavaScript ────────────────────────────────────────────────────────

  pub async fn injected_script(&self) -> Result<String> {
    self.ensure_engine_injected().await?;
    Ok("window.__fd".to_string())
  }

  pub async fn ensure_engine_injected(&self) -> Result<()> {
    if self.engine_injected.load(Ordering::Relaxed) {
      return Ok(());
    }
    // The lazy-inject script is an async IIFE returning a promise.
    // `Runtime.evaluate` has no `awaitPromise`; route the call through
    // `Runtime.callFunctionOn` (which does) anchored on the global, so
    // we synchronously block until `window.__fd` is live.
    let anchor = self.global_anchor().await?;
    let js = crate::selectors::build_lazy_inject_js();
    let wrapper = format!("function(){{ return ({js}); }}");
    let resp = self
      .target
      .send(
        protocol::RUNTIME_CALL_FUNCTION_ON,
        json!({
          "objectId": anchor,
          "functionDeclaration": wrapper,
          "returnByValue": false,
          "awaitPromise": true,
        }),
      )
      .await
      .map_err(conn_err)?;
    if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
      let text = resp
        .get("result")
        .and_then(|r| r.get("description").or_else(|| r.get("value")))
        .and_then(Value::as_str)
        .unwrap_or("engine injection threw")
        .to_string();
      return Err(FerriError::evaluation(text));
    }
    self.engine_injected.store(true, Ordering::Relaxed);
    Ok(())
  }

  pub async fn evaluate(&self, expression: &str) -> Result<Option<Value>> {
    Ok(Some(self.eval_value(expression).await?))
  }

  // ── Elements ──────────────────────────────────────────────────────────

  pub async fn find_element(&self, selector: &str) -> Result<AnyElement> {
    self.ensure_engine_injected().await?;
    let sel_js = crate::selectors::build_selone_js(selector, "window.__fd", false)?;
    self
      .evaluate_to_element(&sel_js, None)
      .await
      .map_err(|_| FerriError::invalid_selector(selector, "no element found"))
  }

  pub async fn evaluate_to_element(&self, js: &str, _frame_id: Option<&str>) -> Result<AnyElement> {
    let resp = self.runtime_evaluate(js, false).await?;
    let object_id = resp
      .get("result")
      .and_then(|r| r.get("objectId"))
      .and_then(Value::as_str)
      .ok_or_else(|| FerriError::protocol("Runtime.evaluate", "evaluate_to_element: result is not an object"))?;
    Ok(AnyElement::PwWebKit(PwWebKitElement::new(
      self.target.clone(),
      object_id.to_string(),
    )))
  }

  pub async fn resolve_backend_node(&self, _backend_node_id: i64, ref_id: &str) -> Result<AnyElement> {
    tokio::task::yield_now().await;
    Ok(AnyElement::PwWebKit(PwWebKitElement::new(self.target.clone(), ref_id.to_string())))
  }

  // ── Content ───────────────────────────────────────────────────────────

  pub async fn content(&self) -> Result<String> {
    Ok(
      self
        .eval_value("document.documentElement.outerHTML")
        .await?
        .as_str()
        .unwrap_or_default()
        .to_string(),
    )
  }

  pub async fn set_content(&self, html: &str) -> Result<()> {
    let escaped = serde_json::to_string(html).unwrap_or_else(|_| "\"\"".into());
    self
      .runtime_evaluate(
        &format!("(()=>{{document.open();document.write({escaped});document.close();}})()"),
        true,
      )
      .await?;
    Ok(())
  }

  // ── Screenshots ───────────────────────────────────────────────────────

  pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>> {
    let (document_rect, coord_system) = if opts.full_page {
      let dims = self
        .eval_value(
          "JSON.stringify({\
            w: Math.max(document.documentElement.scrollWidth, document.documentElement.clientWidth),\
            h: Math.max(document.documentElement.scrollHeight, document.documentElement.clientHeight)\
          })",
        )
        .await?;
      let parsed: Value = dims
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
      let w = parsed.get("w").and_then(Value::as_f64).unwrap_or(0.0);
      let h = parsed.get("h").and_then(Value::as_f64).unwrap_or(0.0);
      (json!({ "x": 0, "y": 0, "width": w, "height": h }), "Page")
    } else if let Some(rect) = opts.clip {
      (
        json!({ "x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height }),
        "Viewport",
      )
    } else {
      let dims = self
        .eval_value("JSON.stringify({w:window.innerWidth,h:window.innerHeight})")
        .await?;
      let parsed: Value = dims
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
      let w = parsed.get("w").and_then(Value::as_f64).unwrap_or(800.0);
      let h = parsed.get("h").and_then(Value::as_f64).unwrap_or(600.0);
      (json!({ "x": 0, "y": 0, "width": w, "height": h }), "Viewport")
    };
    let mut params = document_rect;
    params["coordinateSystem"] = json!(coord_system);
    params["omitDeviceScaleFactor"] = json!(!matches!(opts.scale, Some(crate::backend::ScreenshotScale::Device)));
    let resp = self
      .target
      .send("Page.snapshotRect", params)
      .await
      .map_err(conn_err)?;
    let data_url = resp.get("dataURL").and_then(Value::as_str).unwrap_or_default();
    let b64 = data_url.split_once(',').map_or(data_url, |(_, d)| d);
    base64::engine::general_purpose::STANDARD
      .decode(b64)
      .map_err(|e| FerriError::backend(format!("screenshot base64: {e}")))
  }

  pub async fn screenshot_element(&self, selector: &str, format: ImageFormat) -> Result<Vec<u8>> {
    let elem = self.find_element(selector).await?;
    match elem {
      AnyElement::PwWebKit(e) => e.screenshot(format).await,
      _ => Err(FerriError::backend("screenshot_element: non-pw-webkit element")),
    }
  }

  // ── Accessibility ─────────────────────────────────────────────────────

  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>> {
    self.accessibility_tree_with_depth(-1).await
  }

  pub async fn accessibility_tree_with_depth(&self, max_depth: i32) -> Result<Vec<AxNodeData>> {
    let fd = self.injected_script().await?;
    let json_str = self
      .eval_value(&format!("JSON.stringify({fd}.accessibilityTree({max_depth}))"))
      .await?;
    let arr: Vec<Value> = json_str
      .as_str()
      .and_then(|s| serde_json::from_str(s).ok())
      .unwrap_or_default();
    Ok(super::events::parse_ax_nodes(&arr))
  }

  // ── Input ─────────────────────────────────────────────────────────────

  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    self.click_at_with(x, y, &crate::backend::BackendClickArgs::default_left()).await
  }

  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    let mut args = crate::backend::BackendClickArgs::default_left();
    args.button = crate::options::MouseButton::parse(button).unwrap_or_default();
    args.click_count = click_count;
    self.click_at_with(x, y, &args).await
  }

  pub async fn click_at_with(&self, x: f64, y: f64, args: &crate::backend::BackendClickArgs) -> Result<()> {
    super::input::click(self, x, y, args).await
  }

  pub async fn hover_at_with(&self, x: f64, y: f64, args: &crate::backend::BackendHoverArgs) -> Result<()> {
    super::input::hover(self, x, y, args).await
  }

  pub async fn tap_at_with(&self, x: f64, y: f64, args: &crate::backend::BackendTapArgs) -> Result<()> {
    super::input::tap(self, x, y, args).await
  }

  pub async fn press_modifiers(&self, mods: &[crate::options::Modifier]) -> Result<()> {
    super::input::press_modifiers(self, mods).await
  }

  pub async fn release_modifiers(&self, mods: &[crate::options::Modifier]) -> Result<()> {
    super::input::release_modifiers(self, mods).await
  }

  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    super::input::move_mouse(self, x, y).await
  }

  pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<()> {
    super::input::move_mouse_smooth(self, from_x, from_y, to_x, to_y, steps).await
  }

  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    super::input::mouse_wheel(self, delta_x, delta_y).await
  }

  pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<()> {
    super::input::mouse_down(self, x, y, button).await
  }

  pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<()> {
    super::input::mouse_up(self, x, y, button).await
  }

  pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<()> {
    super::input::click_and_drag(self, from, to, steps).await
  }

  pub async fn type_str(&self, text: &str) -> Result<()> {
    super::input::type_text(self, text).await
  }

  pub async fn key_down(&self, key: &str) -> Result<()> {
    super::input::key_down(self, key).await
  }

  pub async fn key_up(&self, key: &str) -> Result<()> {
    super::input::key_up(self, key).await
  }

  pub async fn press_key(&self, key: &str) -> Result<()> {
    super::input::press_key(self, key).await
  }

  // ── Cookies ───────────────────────────────────────────────────────────

  pub async fn get_cookies(&self) -> Result<Vec<CookieData>> {
    let resp = self
      .target
      .send("Page.getCookies", json!({}))
      .await
      .map_err(conn_err)?;
    let arr = resp.get("cookies").and_then(Value::as_array).cloned().unwrap_or_default();
    Ok(arr.iter().map(super::events::parse_cookie).collect())
  }

  pub async fn set_cookie(&self, cookie: CookieData) -> Result<()> {
    super::events::set_cookie(&self.target, cookie).await
  }

  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<()> {
    let mut params = json!({ "cookieName": name });
    if let Some(d) = domain {
      params["url"] = json!(format!("http://{d}/"));
    }
    self
      .target
      .send("Page.deleteCookie", params)
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  pub async fn clear_cookies(&self) -> Result<()> {
    for c in self.get_cookies().await? {
      let _ = self.delete_cookie(&c.name, Some(&c.domain)).await;
    }
    Ok(())
  }

  // ── Emulation ─────────────────────────────────────────────────────────

  pub async fn apply_context_options(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    if let Some(vp) = opts.resolved_viewport() {
      self.emulate_viewport(&vp).await?;
    }
    if opts.any_media_override() {
      self.emulate_media(&opts.as_emulate_media()).await?;
    }
    if let Some(ua) = opts.user_agent.as_deref() {
      let _ = self
        .target
        .send("Page.overrideUserAgent", json!({ "value": ua }))
        .await;
    }
    if let Some(locale) = opts.locale.as_deref() {
      if let Some(ctx) = &self.context_id {
        let _ = self
          .browser
          .send(
            "Playwright.setLanguages",
            json!({ "browserContextId": ctx.to_string(), "languages": [locale] }),
          )
          .await;
      }
    }
    if let Some(tz) = opts.timezone_id.as_deref() {
      let _ = self
        .target
        .send("Page.setTimeZone", json!({ "timeZone": tz }))
        .await;
    }
    if let Some(true) = opts.java_script_enabled.map(|v| !v) {
      let _ = self
        .proxy
        .send("Emulation.setJavaScriptEnabled", json!({ "enabled": false }))
        .await;
    }
    if let Some(h) = opts.extra_http_headers.as_ref() {
      self.set_extra_http_headers(h).await?;
    }
    if let Some(g) = opts.geolocation {
      if let Some(ctx) = &self.context_id {
        let ts: u64 = std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .ok()
          .and_then(|d| u64::try_from(d.as_millis()).ok())
          .unwrap_or(0);
        let _ = self
          .browser
          .send(
            "Playwright.setGeolocationOverride",
            json!({
              "browserContextId": ctx.to_string(),
              "geolocation": {
                "latitude": g.latitude,
                "longitude": g.longitude,
                "accuracy": g.accuracy,
                "timestamp": ts,
              },
            }),
          )
          .await;
      }
    }
    if let Some(perms) = opts.permissions.as_ref() {
      let _ = self
        .proxy
        .send("Emulation.grantPermissions", json!({ "origin": "*", "permissions": perms }))
        .await;
    }
    if let Some(screen) = opts.screen {
      let _ = self
        .target
        .send(
          "Page.setScreenSizeOverride",
          json!({ "width": screen.width, "height": screen.height }),
        )
        .await;
    }
    if let Some(o) = opts.offline {
      let _ = self
        .target
        .send("Network.setEmulateOfflineState", json!({ "offline": o }))
        .await;
    }
    if let Some(true) = opts.bypass_csp {
      let _ = self
        .target
        .send("Page.setBypassCSP", json!({ "enabled": true }))
        .await;
    }
    // PW `WebKit` applies context-level overrides (locale, JS-disabled,
    // user-agent) to the *next* page load. ferridriver runs
    // `apply_context_options` *after* the first page is constructed, so
    // a fresh `about:blank` document is already live and ignores them.
    // Reload here when any document-time override was set so the page's
    // JS engine picks them up.
    let needs_reload = opts.locale.is_some()
      || opts.user_agent.is_some()
      || opts.timezone_id.is_some()
      || matches!(opts.java_script_enabled, Some(false));
    if needs_reload {
      let _ = self.target.send("Page.reload", json!({})).await;
      let _ = self.wait_for_lifecycle(NavLifecycle::Load, 5_000).await;
    }
    Ok(())
  }

  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<()> {
    // Per `wkPage.ts::_updateViewport`: device metrics override goes
    // on the page-proxy session, screen-size override on the inner
    // target session.
    self
      .proxy
      .send(
        "Emulation.setDeviceMetricsOverride",
        json!({
          "width": config.width,
          "height": config.height,
          "fixedLayout": false,
          "deviceScaleFactor": if config.device_scale_factor > 0.0 { config.device_scale_factor } else { 1.0 },
        }),
      )
      .await
      .map_err(conn_err)?;
    let _ = self
      .target
      .send(
        "Page.setScreenSizeOverride",
        json!({ "width": config.width, "height": config.height }),
      )
      .await;
    Ok(())
  }

  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<()> {
    use crate::options::MediaOverride;
    fn user_pref(name: &str, value: Option<&str>) -> Value {
      let mut params = json!({ "name": name });
      match value {
        Some(v) => params["value"] = json!(v),
        None => params["value"] = Value::Null,
      }
      params
    }
    // media: Set(v) -> v; Disabled -> ""; Unchanged -> skip.
    match &opts.media {
      MediaOverride::Set(v) => {
        self.target.send("Page.setEmulatedMedia", json!({ "media": v })).await.map_err(conn_err)?;
      },
      MediaOverride::Disabled => {
        self.target.send("Page.setEmulatedMedia", json!({ "media": "" })).await.map_err(conn_err)?;
      },
      MediaOverride::Unchanged => {},
    }
    // color_scheme via Page.overrideUserPreference {name:"PrefersColorScheme"}.
    let color = match &opts.color_scheme {
      MediaOverride::Set(v) => Some(match v.as_str() {
        "light" => "Light",
        "dark" => "Dark",
        other => other,
      }),
      MediaOverride::Disabled => None,
      MediaOverride::Unchanged => return self.emulate_media_remaining(opts).await,
    };
    self
      .target
      .send("Page.overrideUserPreference", user_pref("PrefersColorScheme", color))
      .await
      .map_err(conn_err)?;
    self.emulate_media_remaining(opts).await
  }

  async fn emulate_media_remaining(&self, opts: &crate::options::EmulateMediaOptions) -> Result<()> {
    use crate::options::MediaOverride;
    fn user_pref(name: &str, value: Option<&str>) -> Value {
      let mut params = json!({ "name": name });
      match value {
        Some(v) => params["value"] = json!(v),
        None => params["value"] = Value::Null,
      }
      params
    }
    // reduced_motion -> PrefersReducedMotion: "Reduce" | "NoPreference".
    match &opts.reduced_motion {
      MediaOverride::Set(v) => {
        let val = match v.as_str() {
          "reduce" => "Reduce",
          "no-preference" => "NoPreference",
          other => other,
        };
        self
          .target
          .send("Page.overrideUserPreference", user_pref("PrefersReducedMotion", Some(val)))
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Disabled => {
        self
          .target
          .send("Page.overrideUserPreference", user_pref("PrefersReducedMotion", None))
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Unchanged => {},
    }
    // forced_colors -> Page.setForcedColors { forcedColors: "Active" | "None" | null }.
    match &opts.forced_colors {
      MediaOverride::Set(v) => {
        let val = match v.as_str() {
          "active" => "Active",
          "none" => "None",
          other => other,
        };
        self
          .target
          .send("Page.setForcedColors", json!({ "forcedColors": val }))
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Disabled => {
        self
          .target
          .send("Page.setForcedColors", json!({ "forcedColors": Value::Null }))
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Unchanged => {},
    }
    // contrast -> PrefersContrast: "More" | "NoPreference".
    match &opts.contrast {
      MediaOverride::Set(v) => {
        let val = match v.as_str() {
          "more" => "More",
          "no-preference" => "NoPreference",
          other => other,
        };
        self
          .target
          .send("Page.overrideUserPreference", user_pref("PrefersContrast", Some(val)))
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Disabled => {
        self
          .target
          .send("Page.overrideUserPreference", user_pref("PrefersContrast", None))
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Unchanged => {},
    }
    Ok(())
  }

  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<()> {
    let map: serde_json::Map<String, Value> = headers
      .iter()
      .map(|(k, v)| (k.clone(), Value::String(v.clone())))
      .collect();
    self
      .target
      .send("Network.setExtraHTTPHeaders", json!({ "headers": map }))
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  pub async fn reset_permissions(&self) -> Result<()> {
    if let Some(ctx) = &self.context_id {
      self
        .browser
        .send(
          "Playwright.resetPermissions",
          json!({ "browserContextId": ctx.to_string() }),
        )
        .await
        .map_err(conn_err)?;
    }
    Ok(())
  }

  // ── Tracing ───────────────────────────────────────────────────────────

  pub async fn start_tracing(&self) -> Result<()> {
    tokio::task::yield_now().await;
    Ok(())
  }

  pub async fn stop_tracing(&self) -> Result<()> {
    tokio::task::yield_now().await;
    Ok(())
  }

  pub async fn metrics(&self) -> Result<Vec<MetricData>> {
    tokio::task::yield_now().await;
    Ok(Vec::new())
  }

  // ── Listeners ─────────────────────────────────────────────────────────

  pub fn attach_listeners(
    &self,
    console_log: Arc<tokio::sync::RwLock<Vec<ConsoleMessage>>>,
    network_log: Arc<tokio::sync::RwLock<Vec<NetworkRequest>>>,
    dialog_log: Arc<tokio::sync::RwLock<Vec<DialogEvent>>>,
  ) {
    super::events::attach_listeners(self, console_log, network_log, dialog_log);
  }

  // ── PDF / screencast ──────────────────────────────────────────────────

  pub async fn pdf(&self, _opts: crate::options::PdfOptions) -> Result<Vec<u8>> {
    tokio::task::yield_now().await;
    Err(FerriError::unsupported(
      "PDF generation is not supported on the pw-webkit backend — Playwright's WebKit \
       protocol exposes no `Page.pdf` / `Page.printToPDF` command. Use cdp-pipe or cdp-raw for PDF.",
    ))
  }

  pub async fn start_screencast(
    &self,
    quality: u8,
    max_width: u32,
    max_height: u32,
  ) -> Result<tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, f64)>> {
    use base64::Engine as _;
    self
      .proxy
      .send(
        "Screencast.startScreencast",
        json!({
          "width": max_width,
          "height": max_height,
          "toolbarHeight": 0,
          "quality": quality,
        }),
      )
      .await
      .map_err(conn_err)?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<u8>, f64)>();
    let mut events = self.proxy.events();
    tokio::spawn(async move {
      loop {
        let env = match events.recv().await {
          Ok(e) => e,
          Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
          Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        };
        if env.method.as_deref() != Some("Screencast.screencastFrame") {
          continue;
        }
        let data = env.params.get("data").and_then(Value::as_str).unwrap_or("");
        let ts = env.params.get("timestamp").and_then(Value::as_f64).unwrap_or(0.0);
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
          continue;
        };
        if tx.send((bytes, ts)).is_err() {
          break;
        }
      }
    });
    Ok(rx)
  }

  pub async fn stop_screencast(&self) -> Result<()> {
    let _ = self.proxy.send("Screencast.stopScreencast", json!({})).await;
    Ok(())
  }

  // ── File upload / interception ────────────────────────────────────────

  pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<()> {
    let elem = self.find_element(selector).await?;
    let AnyElement::PwWebKit(e) = elem else {
      return Err(FerriError::backend("set_file_input: non-pw-webkit element"));
    };
    self
      .target
      .send(
        "DOM.setInputFiles",
        json!({ "objectId": e.object_id(), "paths": paths }),
      )
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  pub async fn route(
    &self,
    _matcher: crate::url_matcher::UrlMatcher,
    _handler: crate::route::RouteHandler,
  ) -> Result<()> {
    tokio::task::yield_now().await;
    // Network interception on PW WebKit goes through `Network.addInterception`
    // + `Network.requestIntercepted` events + `Network.interceptContinue`/
    // `Network.interceptWithResponse`. Wiring this through ferridriver's
    // `RegisteredRoute` table + Fetch-style continue/abort/fulfill semantics
    // is a focused follow-up batch — surfaced as Unsupported so a calling
    // test gets a clear signal rather than a silent no-op.
    Err(FerriError::unsupported(
      "pw-webkit: network interception (`route`) is not yet wired — Network.addInterception \
       + Network.requestIntercepted plumbing pending. Use cdp-pipe/cdp-raw for routes.",
    ))
  }

  pub async fn unroute(&self, _matcher: &crate::url_matcher::UrlMatcher) -> Result<()> {
    tokio::task::yield_now().await;
    Ok(())
  }

  pub async fn enable_file_chooser_intercept(&self) -> Result<()> {
    tokio::task::yield_now().await;
    Ok(())
  }

  pub async fn enable_download_behavior(&self) -> Result<()> {
    tokio::task::yield_now().await;
    Ok(())
  }

  // ── Exposed functions / init scripts ──────────────────────────────────

  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<()> {
    self.ensure_binding_channel().await?;
    self.exposed_fns.write().await.insert(name.to_string(), func);
    let register_js = format!("globalThis.__fd_bc.add('{}')", crate::steps::js_escape(name));
    self.add_init_script(&register_js).await?;
    self.runtime_evaluate(&register_js, true).await?;
    Ok(())
  }

  pub async fn remove_exposed_function(&self, name: &str) -> Result<()> {
    self.exposed_fns.write().await.remove(name);
    let js = format!(
      "if(globalThis.__fd_bc)globalThis.__fd_bc.del('{}')",
      crate::steps::js_escape(name)
    );
    let _ = self.runtime_evaluate(&js, true).await;
    Ok(())
  }

  /// Install `__fd_binding__` binding + `__fd_bc` controller JS on
  /// first use, then spawn the listener task that dispatches
  /// `Runtime.bindingCalled` events back through registered callbacks.
  async fn ensure_binding_channel(&self) -> Result<()> {
    if self.binding_initialized.swap(true, Ordering::SeqCst) {
      return Ok(());
    }
    self
      .target
      .send("Runtime.addBinding", json!({ "name": "__fd_binding__" }))
      .await
      .map_err(conn_err)?;
    self
      .add_init_script(crate::backend::cdp::CdpPage::<crate::backend::cdp::pipe::PipeTransport>::BINDING_CONTROLLER_JS)
      .await?;
    self
      .runtime_evaluate(
        crate::backend::cdp::CdpPage::<crate::backend::cdp::pipe::PipeTransport>::BINDING_CONTROLLER_JS,
        true,
      )
      .await?;

    let target = self.target.clone();
    let fns = self.exposed_fns.clone();
    let mut rx = target.events();
    tokio::spawn(async move {
      loop {
        let env = match rx.recv().await {
          Ok(e) => e,
          Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
          Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        };
        if env.method.as_deref() != Some("Runtime.bindingCalled") {
          continue;
        }
        if env.params.get("name").and_then(Value::as_str) != Some("__fd_binding__") {
          continue;
        }
        let payload_str = env.params.get("argument").and_then(Value::as_str).unwrap_or("{}");
        let payload: Value = serde_json::from_str(payload_str).unwrap_or_default();
        let fn_name = payload.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let seq = payload.get("seq").and_then(Value::as_u64).unwrap_or(0);
        let args: Vec<Value> = payload
          .get("args")
          .and_then(Value::as_array)
          .cloned()
          .unwrap_or_default();
        let maybe_fn = fns.read().await.get(&fn_name).cloned();
        let deliver_js = if let Some(callback) = maybe_fn {
          let result = callback(args).await;
          format!(
            "globalThis.__fd_bc.resolve({}, {})",
            seq,
            serde_json::to_string(&result).unwrap_or_else(|_| "null".into())
          )
        } else {
          format!("globalThis.__fd_bc.reject({seq}, 'Function not found: {fn_name}')")
        };
        let _ = target.send(protocol::RUNTIME_EVALUATE, json!({ "expression": deliver_js })).await;
      }
    });
    Ok(())
  }

  pub async fn add_init_script(&self, source: &str) -> Result<String> {
    let resp = self
      .target
      .send("Page.setBootstrapScript", json!({ "source": source }))
      .await
      .map_err(conn_err)?;
    Ok(resp.get("identifier").and_then(Value::as_str).unwrap_or("0").to_string())
  }

  pub async fn remove_init_script(&self, _identifier: &str) -> Result<()> {
    let _ = self
      .target
      .send("Page.setBootstrapScript", json!({}))
      .await;
    Ok(())
  }

  // ── Handle / utility evaluate ─────────────────────────────────────────

  /// ferridriver's analogue of Playwright's `evaluateExpression` — runs
  /// the shared `UTILITY_EVAL_WRAPPER` against the page's `UtilityScript`.
  /// `Runtime.callFunctionOn` (which has `awaitPromise`) anchored on the
  /// main-world global, or on the first handle's `objectId` when handles
  /// are supplied (free context anchor, same realm).
  pub async fn call_utility_evaluate(
    &self,
    fn_source: &str,
    args: &[crate::protocol::SerializedValue],
    handles: &[crate::protocol::HandleId],
    _frame_id: Option<&str>,
    is_function: Option<bool>,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult> {
    self.ensure_engine_injected().await?;

    let args_json = serde_json::to_string(args)?;
    let is_fn: Value = match is_function {
      Some(b) => Value::Bool(b),
      None => Value::Null,
    };
    let count = args.len();
    let mut arguments: Vec<Value> = vec![
      json!({ "value": is_fn }),
      json!({ "value": return_by_value }),
      json!({ "value": fn_source }),
      json!({ "value": count }),
      json!({ "value": args_json }),
    ];
    let mut anchor: Option<String> = None;
    for handle in handles {
      match handle {
        crate::protocol::HandleId::PwWebKit(obj) => {
          if anchor.is_none() {
            anchor = Some(obj.clone());
          }
          arguments.push(json!({ "objectId": obj }));
        },
        other => {
          return Err(FerriError::invalid_argument(
            "handles",
            format!("call_utility_evaluate: non-pw-webkit handle {other:?} on pw-webkit backend"),
          ));
        },
      }
    }
    let anchor = match anchor {
      Some(a) => a,
      None => self.global_anchor().await?,
    };
    let resp = self
      .target
      .send(
        protocol::RUNTIME_CALL_FUNCTION_ON,
        json!({
          "objectId": anchor,
          "functionDeclaration": crate::backend::cdp::UTILITY_EVAL_WRAPPER,
          "arguments": arguments,
          "returnByValue": return_by_value,
          "awaitPromise": true,
        }),
      )
      .await
      .map_err(conn_err)?;
    parse_eval_response(&resp, return_by_value)
  }

  pub async fn release_object(&self, object_id: &str) -> Result<()> {
    let _ = self
      .target
      .send(protocol::RUNTIME_RELEASE_OBJECT, json!({ "objectId": object_id }))
      .await;
    Ok(())
  }

  #[must_use]
  pub fn element_from_object_id(&self, object_id: String) -> PwWebKitElement {
    PwWebKitElement::new(self.target.clone(), object_id)
  }

  // ── Lifecycle ─────────────────────────────────────────────────────────

  pub async fn close_page(&self, _opts: crate::options::PageCloseOptions) -> Result<()> {
    if self.closed.swap(true, Ordering::Relaxed) {
      return Ok(());
    }
    let send = self.proxy.send(
      "Target.close",
      json!({ "targetId": self.target_id.to_string(), "runBeforeUnload": false }),
    );
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), send).await;
    Ok(())
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.closed.load(Ordering::Relaxed)
  }
}

/// Wait for the first non-provisional `Target.targetCreated` of type
/// `page` on the proxy session.
async fn wait_for_first_page_target(proxy: &Session) -> std::result::Result<String, BrowserError> {
  let mut rx = proxy.events();
  loop {
    match rx.recv().await {
      Ok(env) => {
        if let Some(id) = page_target_id(&env) {
          return Ok(id);
        }
      },
      Err(RecvError::Lagged(_)) => {},
      Err(RecvError::Closed) => {
        return Err(BrowserError::Protocol("page proxy closed before target".into()));
      },
    }
  }
}

fn page_target_id(env: &Envelope) -> Option<String> {
  if env.method.as_deref()? != "Target.targetCreated" {
    return None;
  }
  let info = env.params.get("targetInfo")?;
  if info.get("type").and_then(Value::as_str) != Some("page")
    || info.get("isProvisional").and_then(Value::as_bool).unwrap_or(false)
  {
    return None;
  }
  Some(info.get("targetId")?.as_str()?.to_string())
}

/// Walk a PW `Page.FrameResourceTree` into flat [`FrameInfo`]s.
fn collect_frames(node: &Value, parent: Option<&str>, out: &mut Vec<FrameInfo>) {
  let Some(frame) = node.get("frame") else {
    return;
  };
  let frame_id = frame.get("id").and_then(Value::as_str).unwrap_or("").to_string();
  out.push(FrameInfo {
    frame_id: frame_id.clone(),
    parent_frame_id: parent.map(str::to_string),
    name: frame.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
    url: frame.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
  });
  if let Some(children) = node.get("childFrames").and_then(Value::as_array) {
    for child in children {
      collect_frames(child, Some(&frame_id), out);
    }
  }
}

/// Map a [`super::connection::ConnectionError`] to a [`FerriError`].
fn conn_err(e: super::connection::ConnectionError) -> FerriError {
  e.into()
}

/// Decode a `Runtime.callFunctionOn` reply produced by the
/// `UTILITY_EVAL_WRAPPER` into an [`EvaluateResult`]. PW `WebKit`
/// signals errors via `wasThrown` (not CDP's `exceptionDetails`).
fn parse_eval_response(resp: &Value, return_by_value: bool) -> Result<crate::js_handle::EvaluateResult> {
  use crate::js_handle::{EvaluateResult, HandleRemote, JSHandleBacking};
  use crate::protocol::{SerializationContext, SerializedValue, SpecialValue};

  let result = resp
    .get("result")
    .ok_or_else(|| FerriError::protocol("Runtime.callFunctionOn", "call_utility_evaluate: no result"))?;
  if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
    let text = result
      .get("description")
      .or_else(|| result.get("value"))
      .and_then(Value::as_str)
      .unwrap_or("evaluation threw");
    return Err(FerriError::evaluation(text.to_string()));
  }

  if return_by_value {
    // The wrapper `JSON.stringify`d the isomorphic wire shape, so the
    // backend ships back a plain string; `null` is the `undefined`
    // sentinel.
    let wire = result.get("value").cloned().unwrap_or(Value::Null);
    let parsed: SerializedValue = match wire {
      Value::Null => SerializedValue::Special(SpecialValue::Undefined),
      Value::String(ref s) => {
        let inner: Value = serde_json::from_str(s)
          .map_err(|e| FerriError::backend(format!("call_utility_evaluate: inner JSON: {e}")))?;
        serde_json::from_value(inner)
          .map_err(|e| FerriError::backend(format!("call_utility_evaluate: parse result: {e}")))?
      },
      other => serde_json::from_value(other)
        .map_err(|e| FerriError::backend(format!("call_utility_evaluate: parse result: {e}")))?,
    };
    return Ok(EvaluateResult::Value(parsed));
  }

  if let Some(obj_id) = result.get("objectId").and_then(Value::as_str) {
    let is_node = result.get("subtype").and_then(Value::as_str) == Some("node");
    return Ok(EvaluateResult::Handle(
      JSHandleBacking::Remote(HandleRemote::PwWebKit(Arc::from(obj_id))),
      is_node,
    ));
  }

  let value = result.get("value").cloned().unwrap_or(Value::Null);
  let serialized = if value.is_null() {
    if result.get("type").and_then(Value::as_str) == Some("undefined") {
      SerializedValue::Special(SpecialValue::Undefined)
    } else {
      SerializedValue::Special(SpecialValue::Null)
    }
  } else {
    SerializedValue::from_json(&value, &mut SerializationContext::default())
  };
  Ok(EvaluateResult::Handle(JSHandleBacking::Value(serialized), false))
}
