#![allow(clippy::missing_errors_doc)]
//! `WebKit` backend — native `WKWebView` on macOS.
//!
//! Architecture ported from Bun's webview implementation:
//! - Parent communicates over Unix socketpair with binary frames
//! - Child subprocess runs `WKWebView` on main thread (single-threaded, nonblocking)
//! - No JSON IPC. No tokio for spawning. No background threads in child.

pub mod ipc;

use super::{
  AnyElement, AnyPage, Arc, AxNodeData, AxProperty, ConsoleMessage, CookieData, ImageFormat, MetricData,
  NetworkRequest, RwLock, ScreenshotOpts,
};
use crate::error::{FerriError, Result};
use crate::network::{self, Headers, RequestInit, Response as NetworkResponse, ResponseInit, body_unsupported};
use ipc::{IpcClient, IpcResponse, Op};

// ─── WebKitBrowser ──────────────────────────────────────────────────────────

/// Owns the `std::process::Child` for the `WebKit` host subprocess and kills
/// it when dropped. `std::process::Child` does not kill on drop — we need
/// this wrapper so that losing the last `WebKitBrowser` handle (panic, early
/// return, test worker shutdown) tears down the host. The `Arc<WebKitChildGuard>`
/// on `WebKitBrowser` ensures this `Drop` only runs on the final clone.
struct WebKitChildGuard {
  child: std::sync::Mutex<Option<std::process::Child>>,
}

impl WebKitChildGuard {
  fn new(child: std::process::Child) -> Self {
    Self {
      child: std::sync::Mutex::new(Some(child)),
    }
  }

  /// Kill the host subprocess and reap it. Idempotent — safe to call from both
  /// [`WebKitBrowser::close`] and `Drop`. Kills the whole process group (the
  /// host is a session leader thanks to `setsid()` in its `pre_exec`), so any
  /// worker the host forked dies with it.
  fn shutdown(&self) {
    if let Ok(mut guard) = self.child.lock() {
      if let Some(mut child) = guard.take() {
        crate::backend::process::kill_process_group(child.id());
        let _ = child.kill();
        let _ = child.wait();
      }
    }
  }
}

impl Drop for WebKitChildGuard {
  fn drop(&mut self) {
    self.shutdown();
  }
}

#[derive(Clone)]
pub struct WebKitBrowser {
  client: Arc<IpcClient>,
  child: Arc<WebKitChildGuard>,
  /// Running `WebKit.framework` product version captured at launch via
  /// `Op::GetWebKitVersion`. Shape mirrors the CDP
  /// `Browser.getVersion().product` string, e.g. `"WebKit/617.1.2 (17618)"`.
  /// Surfaced through `Browser::version()`.
  version: Arc<str>,
}

impl WebKitBrowser {
  /// Launch a new `WebKit` browser subprocess via the native host binary.
  ///
  /// # Errors
  ///
  /// Returns an error if the host binary cannot be found or the subprocess
  /// fails to start or become ready.
  pub async fn launch() -> Result<Self> {
    Self::launch_with_options(true).await
  }

  /// Launch with explicit headless/headful control.
  ///
  /// # Errors
  ///
  /// Returns an error if the host binary cannot be found or the subprocess
  /// fails to start or become ready.
  pub async fn launch_with_options(headless: bool) -> Result<Self> {
    let (client, child) = IpcClient::spawn(headless).await?;
    let client = Arc::new(client);
    // Handshake: query the real WebKit framework version so
    // `Browser::version()` doesn't return a placeholder string.
    let version: Arc<str> = match client.send_empty(Op::GetWebKitVersion).await {
      Ok(IpcResponse::Value(v)) => v.as_str().map_or_else(|| Arc::from("WebKit/unknown"), Arc::from),
      _ => Arc::from("WebKit/unknown"),
    };
    Ok(Self {
      client,
      child: Arc::new(WebKitChildGuard::new(child)),
      version,
    })
  }

  /// Real `WebKit` framework version captured at launch.
  #[must_use]
  pub fn version(&self) -> &str {
    &self.version
  }

  /// List all open pages (views) in this browser instance.
  ///
  /// # Errors
  ///
  /// Returns an error if the IPC call to list views fails or times out.
  pub async fn pages(&self) -> Result<Vec<AnyPage>> {
    let r = self.client.send_empty(Op::ListViews).await?;
    match r {
      IpcResponse::ViewList(ids) => Ok(
        ids
          .into_iter()
          .map(|id| {
            AnyPage::WebKit(WebKitPage {
              client: self.client.clone(),
              view_id: id,
              events: crate::events::EventEmitter::new(),
              routes: std::sync::Arc::new(std::sync::RwLock::new(Vec::new())),
              closed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
              injected_script: std::sync::Arc::new(InjectedScriptManager::new()),
              dialog_manager: crate::dialog::DialogManager::new(),
              file_chooser_manager: crate::file_chooser::FileChooserManager::new(),
              download_manager: crate::download::DownloadManager::new(),
              page_backref: crate::backend::PageBackref::new(),
              exposed_fns: std::sync::Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
              frame_cache: std::sync::Arc::new(std::sync::Mutex::new(crate::frame_cache::FrameCache::default())),
              frame_listener_started: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            })
          })
          .collect(),
      ),
      IpcResponse::Error(e) => Err(e),
      _ => Err("unexpected".into()),
    }
  }

  /// Create a new page (view) and navigate to the given URL.
  ///
  /// # Errors
  ///
  /// Returns an error if the IPC call to create the view fails or the host
  /// subprocess returns an unexpected response.
  pub async fn new_page(&self, url: &str) -> Result<AnyPage> {
    let r = self.client.send_str(Op::CreateView, url).await?;
    match r {
      IpcResponse::ViewCreated(id) => {
        let page = WebKitPage {
          client: self.client.clone(),
          view_id: id,
          events: crate::events::EventEmitter::new(),
          routes: std::sync::Arc::new(std::sync::RwLock::new(Vec::new())),
          closed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
          injected_script: std::sync::Arc::new(InjectedScriptManager::new()),
          dialog_manager: crate::dialog::DialogManager::new(),
          file_chooser_manager: crate::file_chooser::FileChooserManager::new(),
          download_manager: crate::download::DownloadManager::new(),
          page_backref: crate::backend::PageBackref::new(),
          exposed_fns: std::sync::Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
          frame_cache: std::sync::Arc::new(std::sync::Mutex::new(crate::frame_cache::FrameCache::default())),
          frame_listener_started: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        Ok(AnyPage::WebKit(page))
      },
      IpcResponse::Error(e) => Err(e),
      _ => Err("unexpected".into()),
    }
  }

  /// Create a new page in an isolated context. If a viewport config is provided,
  /// it is applied immediately after page creation (saves a sequential round-trip).
  ///
  /// Close the browser by killing the host subprocess.
  ///
  /// # Errors
  ///
  /// This function currently always succeeds; errors from killing or waiting
  /// on the child process are silently ignored.
  pub fn close(&mut self) -> impl std::future::Future<Output = crate::error::Result<()>> {
    // OP_SHUTDOWN calls _exit(0) immediately -- no response comes back.
    // Kill the host subprocess via the shared guard; `WebKitChildGuard::Drop`
    // also runs when the last clone of the `Arc` goes, so this is the graceful
    // path and `Drop` is the safety net for panics / missing explicit close.
    self.child.shutdown();
    std::future::ready(Ok(()))
  }
}

// ─── WebKitPage ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WebKitPage {
  client: Arc<IpcClient>,
  view_id: u64,
  pub events: crate::events::EventEmitter,
  /// Registered route handlers for network interception.
  /// `RwLock` because routes are read on every intercepted request (hot) but
  /// only written when `route()/unroute()` is called (cold, setup-time).
  routes: std::sync::Arc<std::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
  /// Whether this page has been closed via `close_page()`.
  closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
  /// Manager for lazy engine injection.
  injected_script: std::sync::Arc<InjectedScriptManager>,
  /// Per-page dialog handler registry. See
  /// `crates/ferridriver/src/dialog.rs::DialogManager`. On `WebKit` the
  /// host's `WKUIDelegate` auto-decides the accept/dismiss in the
  /// Obj-C subprocess before the event reaches Rust (the IPC payload
  /// already carries `action`), so the handler's `accept`/`dismiss`
  /// return `FerriError::Unsupported` — the observation surface
  /// (`type`/`message`) still flows through the manager so listeners
  /// can record what happened.
  pub dialog_manager: crate::dialog::DialogManager,
  /// Per-page file-chooser handler registry. See
  /// `crates/ferridriver/src/file_chooser.rs::FileChooserManager`.
  /// Stock `WKWebView` exposes no public API for intercepting the
  /// open-panel (the host's `WKUIDelegate::-webView:runOpenPanel...`
  /// runs in the Obj-C subprocess and either answers synchronously or
  /// not at all — no event flows through our IPC). So this manager is
  /// constructed for API parity but never receives `did_open` calls
  /// on `WebKit`; any registered handler simply never fires. Rule-4
  /// honest: the feature is documented as `Unsupported` at the
  /// backend boundary via the null no-op attach path, and the outer
  /// `Page::wait_for_file_chooser` still returns `Timeout`.
  pub file_chooser_manager: crate::file_chooser::FileChooserManager,
  /// Per-page download handler registry. See
  /// `crates/ferridriver/src/download.rs::DownloadManager`. Stock
  /// `WKWebView` routes downloads through `WKDownloadDelegate` in the
  /// host's Obj-C subprocess; wiring the begin/complete/error events
  /// back over IPC would require a new `WKDownload` delegate class in
  /// `host.m`, three new `Op::*` / `Rep::*` codes, and buffer
  /// management for the landed file bytes. This is scoped as a
  /// future phase (documented under §B of `PLAYWRIGHT_COMPAT.md`);
  /// for now the manager is present for API parity (so
  /// `page.on("download", ...)` doesn't error) but no event ever
  /// dispatches to it and `Page::wait_for_download` times out
  /// honestly. Rule-4 honest: callers observe the gap explicitly.
  pub download_manager: crate::download::DownloadManager,
  /// Weak back-reference to the outer [`crate::page::Page`]. Carried
  /// here for struct parity across backends even though the `WebKit`
  /// file-chooser path never upgrades it.
  pub page_backref: crate::backend::PageBackref,
  /// Per-page exposed-function registry. Mirrors CDP / `BiDi` —
  /// `expose_function` installs a JS shim that posts a JSON envelope
  /// through `console.log`; the `attach_listeners` console drain
  /// intercepts those envelopes, runs the registered Rust callback,
  /// and resolves the page-side promise. `WebKit` has no
  /// `Runtime.addBinding` analogue so the console-side channel is
  /// the available transport, same approach as `BiDi`.
  exposed_fns: std::sync::Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, crate::events::ExposedFn>>>,
  /// Shared frame cache — see `CdpPage::frame_cache`. WebKit has no
  /// real OOPIF support today and `get_frame_tree` returns just the
  /// main frame, but we still want the cache to persist across MCP
  /// tool-call wrappers so `page.main_frame()` doesn't lose its
  /// `peek_main_frame_id()`-seeded entry between calls.
  pub(crate) frame_cache: std::sync::Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  /// Idempotent latch for the frame-event listener.
  pub(crate) frame_listener_started: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

pub struct InjectedScriptManager {
  injected: std::sync::atomic::AtomicBool,
}

impl InjectedScriptManager {
  fn new() -> Self {
    Self {
      injected: std::sync::atomic::AtomicBool::new(false),
    }
  }

  fn reset(&self) {
    self.injected.store(false, std::sync::atomic::Ordering::Relaxed);
  }

  async fn ensure(&self, page: &WebKitPage) -> Result<()> {
    if !self.injected.load(std::sync::atomic::Ordering::Relaxed) {
      let full_check_js = crate::selectors::build_lazy_inject_js();
      let r = page
        .client
        .send_str_vid(Op::Evaluate, &full_check_js, page.vid())
        .await?;
      WebKitPage::ok(r)?;
      self.injected.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
  }
}

impl WebKitPage {
  fn vid(&self) -> u64 {
    self.view_id
  }

  fn ok(r: IpcResponse) -> Result<()> {
    match r {
      IpcResponse::Ok
      | IpcResponse::Value(_)
      | IpcResponse::ViewCreated(_)
      | IpcResponse::ViewList(_)
      | IpcResponse::Binary(_) => Ok(()),
      IpcResponse::Error(e) => Err(e),
    }
  }

  /// Navigate to the given URL and wait for navigation to complete.
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation IPC call fails or the page fails to load.
  pub async fn goto(
    &self,
    url: &str,
    _lifecycle: crate::backend::NavLifecycle,
    _timeout_ms: u64,
    referer: Option<&str>,
  ) -> Result<Option<crate::network::Response>> {
    // WebKit backend: WKWebView navigation delegate fires on load complete.
    // Lifecycle granularity (commit vs domcontentloaded vs load) is not
    // distinguishable via the native API — all waits resolve on load.
    //
    // Referer is attached as a `Referer` HTTP header on the
    // `NSMutableURLRequest` by the Obj-C side when present.
    //
    // Main-document `Response` observability: stock `WKWebView` has no
    // public API surface for main-doc response headers/status. The
    // `WKNavigationDelegate` callbacks (`decidePolicyForNavigationResponse:`)
    // expose a `WKNavigationResponse`, but the carried `NSURLResponse`
    // doesn't round-trip status/headers into our IPC, and the JS-fetch
    // interceptor only observes user-script fetches. This is the same
    // limitation already documented in the §1.4 backend gap matrix.
    // Returning `None` is the honest Playwright-parity outcome for a
    // backend that genuinely cannot observe the navigation response.
    let mut p = Vec::new();
    ipc::str_encode(&mut p, url);
    p.extend_from_slice(&self.vid().to_le_bytes());
    ipc::str_encode(&mut p, referer.unwrap_or(""));
    let r = self.client.send(Op::Navigate, &p).await?;
    Self::ok(r)?;
    let r2 = self.client.send_vid(Op::WaitNav, self.vid()).await?;
    Self::ok(r2)?;
    Ok(None)
  }

  /// Wait for the current navigation to complete.
  pub async fn wait_for_navigation(&self) -> Result<()> {
    let r = self.client.send_vid(Op::WaitNav, self.vid()).await?;
    Self::ok(r)
  }

  pub async fn reload(
    &self,
    _lifecycle: crate::backend::NavLifecycle,
    _timeout_ms: u64,
  ) -> Result<Option<crate::network::Response>> {
    let r = self.client.send_vid(Op::Reload, self.vid()).await?;
    Self::ok(r)?;
    let r2 = self.client.send_vid(Op::WaitNav, self.vid()).await?;
    Self::ok(r2)?;
    Ok(None)
  }

  pub async fn go_back(
    &self,
    _lifecycle: crate::backend::NavLifecycle,
    _timeout_ms: u64,
  ) -> Result<Option<crate::network::Response>> {
    let r = self.client.send_vid(Op::GoBack, self.vid()).await?;
    Self::ok(r)?;
    let r2 = self.client.send_vid(Op::WaitNav, self.vid()).await?;
    Self::ok(r2)?;
    Ok(None)
  }

  pub async fn go_forward(
    &self,
    _lifecycle: crate::backend::NavLifecycle,
    _timeout_ms: u64,
  ) -> Result<Option<crate::network::Response>> {
    let r = self.client.send_vid(Op::GoForward, self.vid()).await?;
    Self::ok(r)?;
    let r2 = self.client.send_vid(Op::WaitNav, self.vid()).await?;
    Self::ok(r2)?;
    Ok(None)
  }

  /// Get the current URL of the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the IPC call to retrieve the URL fails.
  pub async fn url(&self) -> Result<Option<String>> {
    let r = self.client.send_vid(Op::GetUrl, self.vid()).await?;
    match r {
      IpcResponse::Value(v) => Ok(v.as_str().map(std::string::ToString::to_string)),
      IpcResponse::Error(e) => Err(e),
      _ => Ok(None),
    }
  }

  /// Get the current title of the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the IPC call to retrieve the title fails.
  pub async fn title(&self) -> Result<Option<String>> {
    let r = self.client.send_vid(Op::GetTitle, self.vid()).await?;
    match r {
      IpcResponse::Value(v) => Ok(v.as_str().map(std::string::ToString::to_string)),
      IpcResponse::Error(e) => Err(e),
      _ => Ok(None),
    }
  }

  pub async fn injected_script(&self) -> Result<String> {
    self.ensure_engine_injected().await?;
    Ok("window.__fd".to_string())
  }

  /// Ensures the selector engine is injected into the current execution context.
  /// Idempotent and navigation-aware.
  pub async fn ensure_engine_injected(&self) -> Result<()> {
    self.injected_script.ensure(self).await
  }

  /// Evaluate a JavaScript expression in the page and return the result.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails or the IPC call times out.
  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>> {
    let r = self.client.send_str_vid(Op::Evaluate, expression, self.vid()).await?;
    match r {
      IpcResponse::Value(v) => {
        if v.is_null() {
          Ok(None)
        } else {
          Ok(Some(v))
        }
      },
      IpcResponse::Error(e) => Err(e),
      _ => Ok(None),
    }
  }

  /// Find a DOM element by CSS selector, returning a reference handle.
  ///
  /// # Errors
  ///
  /// Returns an error if no element matches the selector or the JS evaluation fails.
  pub async fn find_element(&self, selector: &str) -> Result<AnyElement> {
    let js = format!(
      r"(function(){{var e=document.querySelector('{}');if(!e)return null;if(!window.__wr)window.__wr=new Map();if(!window.__wr_next)window.__wr_next=1;var id=window.__wr_next++;window.__wr.set(id,e);return id}})()",
      selector.replace('\\', "\\\\").replace('\'', "\\'")
    );
    let r = self.evaluate(&js).await?;
    let ref_id = r
      .and_then(|v| v.as_u64())
      .ok_or_else(|| format!("'{selector}' not found"))?;
    Ok(AnyElement::WebKit(WebKitElement {
      client: self.client.clone(),
      view_id: self.view_id,
      ref_id,
    }))
  }

  /// Call the page-side `UtilityScript.evaluate` — `WebKit` analogue of
  /// `CdpPage::call_utility_evaluate` / `BidiPage::call_utility_evaluate`.
  ///
  /// `WebKit`'s IPC exposes only a single evaluate primitive, but
  /// because every handle is already addressable from page-side JS
  /// through `window.__wr.get(ref_id)`, we can synthesise a fully
  /// inlined expression that calls the utility script directly. No
  /// new IPC op is required — the Phase-A/B work that exposed
  /// `window.__fd.newUtilityScript()` and the `window.__wr` Map
  /// migration in Phase C cover everything we need.
  ///
  /// Handle / backend mismatches surface as errors (non-`WebKit`
  /// handles in `arg.handles`). Non-element `JSHandle` return values
  /// are minted by allocating a fresh `window.__wr` entry whose
  /// `ref_id` is returned alongside the result.
  ///
  /// # Errors
  ///
  /// Returns a String on IPC failure or page-side exception.
  /// Construct a [`WebKitElement`] directly from a `window.__wr`
  /// registry index without re-resolving through the DOM. Used by
  /// [`crate::backend::element_from_remote`] when a
  /// [`crate::js_handle::JSHandle`] turns out to wrap a DOM node and
  /// needs to be re-packaged as an
  /// [`crate::element_handle::ElementHandle`].
  pub(crate) fn element_from_ref_id(&self, ref_id: u64) -> WebKitElement {
    WebKitElement {
      client: self.client.clone(),
      view_id: self.view_id,
      ref_id,
    }
  }

  /// ferridriver's equivalent of Playwright's
  /// `evaluateExpression(context, expr, opts, ...args)` — see the CDP
  /// twin for the shared contract. `WebKit`'s IPC exposes only one
  /// evaluate primitive, so the wrapper + meta-args are inlined into
  /// a single expression and dispatched through [`Self::evaluate`].
  ///
  /// # Errors
  ///
  /// Returns a String on IPC failure or page-side exception.
  #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
  pub async fn call_utility_evaluate(
    &self,
    fn_source: &str,
    args: &[crate::protocol::SerializedValue],
    handles: &[crate::protocol::HandleId],
    _frame_id: Option<&str>,
    is_function: Option<bool>,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult> {
    use crate::js_handle::{EvaluateResult as FdEvalResult, HandleRemote};
    use crate::protocol::HandleId;

    self.ensure_engine_injected().await?;

    let args_json = serde_json::to_string(args)?;
    let count = args.len();

    let mut handle_refs = Vec::with_capacity(handles.len());
    for handle in handles {
      match handle {
        HandleId::WebKit(r) => handle_refs.push(*r),
        _ => return Err("call_utility_evaluate: non-WebKit handle in arg.handles on WebKit backend".into()),
      }
    }

    let is_fn_js = match is_function {
      Some(true) => "true",
      Some(false) => "false",
      None => "undefined",
    };
    let return_by_value_js = if return_by_value { "true" } else { "false" };
    let handle_list_js = if handle_refs.is_empty() {
      String::from("[]")
    } else {
      let inner = handle_refs
        .iter()
        .map(|r| format!("window.__wr.get({r})"))
        .collect::<Vec<_>>()
        .join(",");
      format!("[{inner}]")
    };
    // `serializedArgs` is baked into the JS as a double-escaped
    // string literal — the utility-script wrapper JSON.parses it
    // back on the page, then spreads the resulting array into the
    // utility script's variadic argsAndHandles slot.
    let args_literal = serde_json::to_string(&args_json)?;
    let fn_source_literal = serde_json::to_string(fn_source)?;

    // When returning a handle, we need ref_id allocation. Include the
    // allocator inline and return `{kind: 'handle', ref: id}`. When
    // returning a value, JSON.stringify the isomorphic wire shape and
    // return `{kind: 'value', payload: <jsonString>}`.
    let body = format!(
      r"(function(){{
        const us = (window.__fd && window.__fd.__us) || (window.__fd.__us = window.__fd.newUtilityScript());
        const isFn = {is_fn_js};
        const retVal = {return_by_value_js};
        const expr = {fn_source_literal};
        const count = {count};
        const serializedArgs = {args_literal};
        const parsed = count > 0 ? JSON.parse(serializedArgs) : [];
        const handles = {handle_list_js};
        const result = us.evaluate(isFn, retVal, expr, count, ...parsed, ...handles);
        // Build the host envelope from a resolved value.
        function envelope(value) {{
          if (retVal) {{
            const encoded = JSON.stringify(value);
            return JSON.stringify({{kind: 'value', payload: encoded === undefined ? null : encoded}});
          }}
          // evaluateHandle: primitive results ride back inline (matching
          // Playwright's value-backed JSHandle shape); only object-typed
          // results get a window.__wr entry the host can dispose later.
          const isRef = value !== null && (typeof value === 'object' || typeof value === 'function');
          if (!isRef) {{
            const ty = typeof value === 'undefined' ? 'undefined' : (value === null ? 'null' : 'value');
            const enc = JSON.stringify(value);
            return JSON.stringify({{kind: 'valueHandle', ty: ty, payload: enc === undefined ? null : enc}});
          }}
          if (!window.__wr) window.__wr = new Map();
          if (!window.__wr_next) window.__wr_next = 1;
          const id = window.__wr_next++;
          window.__wr.set(id, value);
          return JSON.stringify({{kind: 'handle', ref: id}});
        }}
        // Hybrid sync/async: only chain a .then when the user expression
        // returns a Promise. The host's `callAsyncJavaScript` awaits
        // whatever the IIFE returns, so a sync return ships back without
        // microtask overhead. Mirrors Playwright's
        // `_promiseAwareJsonValueNoThrow` (packages/injected/src/utilityScript.ts).
        if (result && typeof result.then === 'function') {{
          return result.then(envelope);
        }}
        return envelope(result);
      }})()"
    );

    let raw = self.evaluate(&body).await?;
    let Some(raw_val) = raw else {
      return Err("call_utility_evaluate: WebKit evaluate returned null".into());
    };
    // WebKit's evaluate returns a JSON-parsed value OR a stringified
    // JSON (depending on path). Our wrapper always returns a string,
    // so the value here will be a String (from JSON.stringify) —
    // unless WebKit already parsed it to an object. Handle both.
    let envelope: serde_json::Value = match raw_val {
      serde_json::Value::String(s) => {
        serde_json::from_str(&s).map_err(|e| format!("call_utility_evaluate: envelope parse: {e}"))?
      },
      other => other,
    };

    let kind = envelope
      .get("kind")
      .and_then(|v| v.as_str())
      .ok_or("call_utility_evaluate: missing envelope.kind")?
      .to_string();

    match kind.as_str() {
      "value" => {
        let payload = envelope.get("payload").cloned().unwrap_or(serde_json::Value::Null);
        let parsed: crate::protocol::SerializedValue = match payload {
          serde_json::Value::Null => {
            crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
          },
          serde_json::Value::String(s) => {
            let inner: serde_json::Value =
              serde_json::from_str(&s).map_err(|e| format!("call_utility_evaluate: payload parse: {e}"))?;
            serde_json::from_value(inner).map_err(|e| format!("call_utility_evaluate: parse result: {e}"))?
          },
          other => serde_json::from_value(other).map_err(|e| format!("call_utility_evaluate: parse result: {e}"))?,
        };
        Ok(FdEvalResult::Value(parsed))
      },
      "handle" => {
        let ref_id = envelope
          .get("ref")
          .and_then(serde_json::Value::as_u64)
          .ok_or("call_utility_evaluate: missing envelope.ref")?;
        Ok(FdEvalResult::Handle(crate::js_handle::JSHandleBacking::Remote(
          HandleRemote::WebKit(ref_id),
        )))
      },
      "valueHandle" => {
        // Primitive result from evaluateHandle. Parse the inline JSON
        // and wrap as a value-backed JSHandle — matches Playwright's
        // `_value`-backed shape and the CDP / BiDi twins.
        let ty = envelope.get("ty").and_then(|v| v.as_str()).unwrap_or("value");
        let payload = envelope.get("payload").cloned().unwrap_or(serde_json::Value::Null);
        let serialized = if ty == "undefined" {
          crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
        } else if ty == "null" {
          crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Null)
        } else {
          let inner: serde_json::Value = match payload {
            serde_json::Value::String(s) => {
              serde_json::from_str(&s).map_err(|e| format!("call_utility_evaluate: valueHandle parse: {e}"))?
            },
            serde_json::Value::Null => serde_json::Value::Null,
            other => other,
          };
          let mut ctx = crate::protocol::SerializationContext::default();
          crate::protocol::SerializedValue::from_json(&inner, &mut ctx)
        };
        Ok(FdEvalResult::Handle(crate::js_handle::JSHandleBacking::Value(
          serialized,
        )))
      },
      other => Err(format!("call_utility_evaluate: unknown envelope kind {other}")),
    }
  }

  /// Evaluate a JS expression that returns a DOM element, returning a reference handle.
  ///
  /// # Errors
  ///
  /// Returns an error if the expression does not return a valid DOM element.
  /// Frame-scoped element resolution. `WebKit` has no per-frame
  /// execution context wired through IPC yet, so when `frame_id` is
  /// `Some(_)`, we still evaluate in the main page. The resulting
  /// element points at the main-frame DOM — frame-scoped element
  /// actions on `WebKit` are tracked as a known gap in Section B of
  /// `PLAYWRIGHT_COMPAT.md` until `WKFrameInfo`-based evaluation lands.
  pub async fn evaluate_to_element(&self, js: &str, _frame_id: Option<&str>) -> Result<AnyElement> {
    let wrap = format!(
      r"(function(){{var e=({js});if(!e)return null;if(!window.__wr)window.__wr=new Map();if(!window.__wr_next)window.__wr_next=1;var id=window.__wr_next++;window.__wr.set(id,e);return id}})()"
    );
    let r = self.evaluate(&wrap).await?;
    let ref_id = r.and_then(|v| v.as_u64()).ok_or("JS did not return a DOM element")?;
    Ok(AnyElement::WebKit(WebKitElement {
      client: self.client.clone(),
      view_id: self.view_id,
      ref_id,
    }))
  }

  /// Get the frame tree. `WebKit`'s IPC doesn't expose `WKFrameInfo`
  /// enumeration yet (requires a fork-and-patch path Playwright takes
  /// via their own `WebKit` build). Instead, we probe the DOM from JS:
  /// one [`super::FrameInfo`] for the main frame, plus one per
  /// `<iframe>` element. Frame ids are synthesized (`iframe-<idx>`);
  /// Frame-scoped JS evaluation still falls back to the main frame
  /// (`evaluate_in_frame` below) — that's a separate gap tracked in
  /// Section B of `PLAYWRIGHT_COMPAT.md`.
  ///
  /// # Errors
  ///
  /// Returns an error if the DOM probe fails. The main-frame entry is
  /// always included (never empty).
  pub async fn get_frame_tree(&self) -> Result<Vec<super::FrameInfo>> {
    let main_url = self.url().await?.unwrap_or_default();
    let main_id = format!("main-{}", self.view_id);
    let mut frames = vec![super::FrameInfo {
      frame_id: main_id.clone(),
      parent_frame_id: None,
      name: String::new(),
      url: main_url,
    }];

    // Probe the DOM for <iframe> elements.
    let probe = "JSON.stringify(Array.from(document.querySelectorAll('iframe')).map((el, i) => ({\
       i, \
       name: el.getAttribute('name') || '', \
       url: el.src || (el.contentDocument && el.contentDocument.URL) || '' \
     })))";
    if let Ok(Some(value)) = self.evaluate(probe).await {
      if let Some(raw) = value.as_str() {
        if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(raw) {
          for entry in arr {
            let idx = entry.get("i").and_then(serde_json::Value::as_u64).unwrap_or(0);
            let name = entry
              .get("name")
              .and_then(serde_json::Value::as_str)
              .unwrap_or_default()
              .to_string();
            let url = entry
              .get("url")
              .and_then(serde_json::Value::as_str)
              .unwrap_or_default()
              .to_string();
            frames.push(super::FrameInfo {
              frame_id: format!("iframe-{}-{idx}", self.view_id),
              parent_frame_id: Some(main_id.clone()),
              name,
              url,
            });
          }
        }
      }
    }

    Ok(frames)
  }

  /// Evaluate JavaScript in a specific frame. Currently evaluates in the main frame only.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn evaluate_in_frame(&self, expression: &str, _frame_id: &str) -> Result<Option<serde_json::Value>> {
    // WebKit: evaluate in main frame only for now.
    // Full iframe support would need WKFrameInfo-based evaluation.
    self.evaluate(expression).await
  }

  /// Get the full HTML content of the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation to read outerHTML fails.
  pub async fn content(&self) -> Result<String> {
    let r = self.evaluate("document.documentElement.outerHTML").await?;
    Ok(
      r.and_then(|v| v.as_str().map(std::string::ToString::to_string))
        .unwrap_or_default(),
    )
  }

  /// Replace the page content with the given HTML string.
  ///
  /// # Errors
  ///
  /// Returns an error if the `LoadHtml` IPC call fails.
  pub async fn set_content(&self, html: &str) -> Result<()> {
    let mut p = Vec::new();
    p.extend_from_slice(&self.vid().to_le_bytes());
    ipc::str_encode(&mut p, html);
    ipc::str_encode(&mut p, "about:blank");
    let r = self.client.send(ipc::Op::LoadHtml, &p).await?;
    Self::ok(r)
  }

  /// Take a screenshot of the page in the specified format.
  ///
  /// # Errors
  ///
  /// Returns an error if the screenshot IPC call fails or no image data is returned.
  pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>> {
    // WebKit-specific refusals for knobs WKWebView can't express.
    if opts.clip.is_some() {
      return Err(
        "WebKit backend does not support `clip` screenshots yet — WKWebView's takeSnapshotWithConfiguration: has no clip parameter. Use `page.locator(selector).screenshot()` for element-scoped capture.".into(),
      );
    }
    if matches!(opts.scale, Some(crate::backend::ScreenshotScale::Css)) {
      return Err(
        "WebKit backend does not support `scale: \"css\"` screenshots yet — WKWebView always captures at device-pixel scale.".into(),
      );
    }
    if opts.omit_background {
      return Err(
        "WebKit backend does not support `omitBackground` screenshots yet — WKWebView's snapshot API always composites against the view background.".into(),
      );
    }

    // Pre-capture DOM setup via shared helpers.
    let css = crate::backend::screenshot_js::build_css(&opts);
    let style_installed = if css.is_empty() {
      false
    } else {
      self
        .evaluate(&crate::backend::screenshot_js::install_style_js(&css))
        .await?;
      true
    };
    let mask_installed = if let Some(js) = crate::backend::screenshot_js::install_mask_js(&opts) {
      self.evaluate(&js).await?;
      true
    } else {
      false
    };

    // IPC payload: u8 format + u8 quality + u64 vid.
    let mut p = Vec::new();
    let fmt_byte: u8 = match opts.format {
      ImageFormat::Jpeg => 1,
      ImageFormat::Webp => 2,
      ImageFormat::Png => 0,
    };
    p.push(fmt_byte);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // quality is always 0-100
    let quality_byte = opts.quality.unwrap_or(80) as u8;
    p.push(quality_byte);
    p.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(Op::Screenshot, &p).await;

    if style_installed {
      let _ = self.evaluate(crate::backend::screenshot_js::uninstall_style_js()).await;
    }
    if mask_installed {
      let _ = self.evaluate(crate::backend::screenshot_js::uninstall_mask_js()).await;
    }

    match r? {
      IpcResponse::Binary(d) => Ok(d),
      IpcResponse::Error(e) => Err(e),
      _ => Err("no data".into()),
    }
  }

  /// Take a screenshot of a specific element by scrolling it into view,
  /// capturing a full screenshot, then cropping to the element's bounding box via JS.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found, screenshot fails, or cropping fails.
  pub async fn screenshot_element(&self, sel: &str, fmt: ImageFormat) -> Result<Vec<u8>> {
    let esc = sel.replace('\\', "\\\\").replace('\'', "\\'");
    // Get bounding box after scrolling into view (single evaluate)
    let js = format!(
      "(function(){{var e=document.querySelector('{esc}');if(!e)return null;\
       e.scrollIntoView({{block:'center',behavior:'instant'}});\
       var r=e.getBoundingClientRect();\
       return JSON.stringify({{x:Math.round(r.x),y:Math.round(r.y),w:Math.round(r.width),h:Math.round(r.height)}})}})()"
    );
    let bbox = self.evaluate(&js).await?;
    let bbox_str = bbox
      .and_then(|v| v.as_str().map(std::string::ToString::to_string))
      .ok_or_else(|| format!("Element '{sel}' not found"))?;
    let bbox_val: serde_json::Value = serde_json::from_str(&bbox_str).map_err(|e| format!("bbox parse: {e}"))?;
    let bx = bbox_val.get("x").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let by = bbox_val.get("y").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let bw = bbox_val.get("w").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let bh = bbox_val.get("h").and_then(serde_json::Value::as_i64).unwrap_or(0);

    if bw <= 0 || bh <= 0 {
      return Err(format!("Element '{sel}' has zero dimensions"));
    }

    // Take full page screenshot
    let full_png = self
      .screenshot(ScreenshotOpts {
        format: fmt,
        ..Default::default()
      })
      .await?;

    // Crop to element bounds using JS Canvas API (avoids needing image crate dependency)
    // Encode full screenshot as base64, crop in JS, return cropped base64
    let b64_full = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &full_png);
    let crop_fmt = match fmt {
      ImageFormat::Jpeg => "image/jpeg",
      ImageFormat::Webp => "image/webp",
      ImageFormat::Png => "image/png",
    };
    let crop_js = format!(
      "(async function(){{var img=new Image();var b='data:image/png;base64,{b64_full}';\
       await new Promise(function(r){{img.onload=r;img.src=b}});\
       var c=document.createElement('canvas');c.width={bw};c.height={bh};\
       var ctx=c.getContext('2d');ctx.drawImage(img,{bx},{by},{bw},{bh},0,0,{bw},{bh});\
       return c.toDataURL('{crop_fmt}').split(',')[1]}})()"
    );
    let cropped = self.evaluate(&crop_js).await?;
    let cropped_b64 = cropped
      .and_then(|v| v.as_str().map(std::string::ToString::to_string))
      .ok_or("crop failed")?;
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &cropped_b64)
      .map_err(|e| format!("decode cropped: {e}"))
  }

  /// Generate a PDF of the page. Not supported on `WebKit` backend.
  ///
  /// # Errors
  ///
  /// Always returns an error because PDF generation requires a CDP backend.
  pub fn pdf(
    &self,
    _opts: crate::options::PdfOptions,
  ) -> impl std::future::Future<Output = crate::error::Result<Vec<u8>>> {
    use crate::FerriError;
    let result = if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
      Err(FerriError::target_closed(Some("page is closed".into())))
    } else {
      Err(FerriError::unsupported(
        "PDF generation requires CDP backend (cdp-ws, cdp-pipe, or cdp-raw)",
      ))
    };
    std::future::ready(result)
  }

  /// Set file input on an `<input type="file">` element.
  /// Supports multiple files by sending each file via IPC sequentially.
  ///
  /// # Errors
  ///
  /// Returns an error if no paths are provided or any IPC call fails.
  pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
      return Err("No file paths provided".into());
    }
    // Clear any previously-assigned files so this call replaces the
    // selection rather than appending (matches Playwright's
    // setInputFiles semantics). The per-path IPC op then appends one
    // file at a time into the live DataTransfer. Using `value = ''`
    // is the canonical way to reset a file input — `el.files = null`
    // throws in strict mode.
    let clear = format!(
      "(function(){{var el=document.querySelector('{}');if(el){{el.value='';}}}})()",
      selector.replace('\'', "\\'"),
    );
    let _ = self.evaluate(&clear).await;
    for path in paths {
      let mut p = Vec::new();
      ipc::str_encode(&mut p, selector);
      ipc::str_encode(&mut p, path);
      p.extend_from_slice(&self.view_id.to_le_bytes());
      let r = self.client.send(ipc::Op::SetFileInput, &p).await?;
      Self::ok(r)?;
    }
    Ok(())
  }

  /// Get the full accessibility tree via native `NSAccessibility`.
  ///
  /// # Errors
  ///
  /// Returns an error if the accessibility tree IPC call fails or response parsing fails.
  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>> {
    self.accessibility_tree_with_depth(-1).await
  }

  /// Get the accessibility tree limited to a specific depth via native `NSAccessibility`.
  ///
  /// # Errors
  ///
  /// Returns an error if the IPC call fails, returns an unexpected response type,
  /// or the JSON response cannot be parsed.
  pub async fn accessibility_tree_with_depth(&self, depth: i32) -> Result<Vec<AxNodeData>> {
    // Use native NSAccessibility tree via IPC (not JavaScript)
    let mut p = Vec::new();
    p.extend_from_slice(&self.vid().to_le_bytes());
    p.extend_from_slice(&depth.to_le_bytes());
    let r = self.client.send(ipc::Op::AccessibilityTree, &p).await?;
    Self::parse_ax_response(r)
  }

  fn parse_ax_response(r: IpcResponse) -> Result<Vec<AxNodeData>> {
    let json_str = match r {
      IpcResponse::Value(v) => {
        if let Some(s) = v.as_str() {
          s.to_string()
        } else {
          v.to_string()
        }
      },
      IpcResponse::Error(e) => return Err(e),
      _ => return Err("unexpected response".into()),
    };
    let raw: Vec<serde_json::Value> = serde_json::from_str(&json_str).map_err(|e| format!("{e}"))?;
    Ok(
      raw
        .iter()
        .map(|n| AxNodeData {
          node_id: n.get("nodeId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          parent_id: n
            .get("parentId")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string),
          backend_dom_node_id: None,
          ignored: n.get("ignored").and_then(serde_json::Value::as_bool).unwrap_or(false),
          role: n
            .get("role")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string),
          name: n
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(std::string::ToString::to_string),
          description: n
            .get("description")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(std::string::ToString::to_string),
          properties: n
            .get("properties")
            .and_then(|p| p.as_array())
            .map(|ps| {
              ps.iter()
                .map(|p| AxProperty {
                  name: p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                  value: p.get("value").cloned(),
                })
                .collect()
            })
            .unwrap_or_default(),
        })
        .collect(),
    )
  }

  /// Click at absolute coordinates using a native `NSEvent`.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse event IPC call fails.
  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    self.click_at_opts(x, y, "left", 1).await
  }

  /// Click at coordinates with specific button and click count options.
  ///
  /// # Errors
  ///
  /// Returns an error if any of the mouse down/up IPC calls fail.
  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    let btn: u8 = match button {
      "right" => 1,
      "middle" => 2,
      _ => 0,
    };
    // NSEvent clickCount must increment per click for dblclick to fire.
    // e.g. click_count=2: first pair has clickCount=1, second has clickCount=2.
    for i in 1..=click_count {
      self.send_mouse_event(1, btn, i, x, y).await?; // down
      self.send_mouse_event(2, btn, i, x, y).await?; // up
    }
    Ok(())
  }

  /// Click at `(x, y)` honoring the full Playwright option bag.
  /// Modifier key press/release is the caller's responsibility — the
  /// host tracks held modifier flags (see `host.m` `held_modifier_flags`)
  /// so the synthesised `NSEvent` carries them.
  ///
  /// # Errors
  ///
  /// Returns an error if any IPC call fails.
  pub async fn click_at_with(&self, x: f64, y: f64, args: &super::BackendClickArgs) -> Result<()> {
    let btn: u8 = args.button.as_webkit();
    let steps = args.steps.max(1);
    // Interpolated mousemoves. Conservative start-from-origin (we don't
    // currently track the prior cursor on WebKit); last step lands at
    // `(x, y)` exactly.
    for i in 1..=steps {
      let t = f64::from(i) / f64::from(steps);
      let sx = if i == steps { x } else { x * t };
      let sy = if i == steps { y } else { y * t };
      self.send_mouse_event(0, btn, 0, sx, sy).await?;
    }
    for i in 1..=args.click_count {
      self.send_mouse_event(1, btn, i, x, y).await?;
      if args.delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(args.delay_ms)).await;
      }
      self.send_mouse_event(2, btn, i, x, y).await?;
    }
    // Middle-button DOM dispatch workaround: in an offscreen / borderless
    // `WKWebView`, `-[WKWebView otherMouseDown:]` + `otherMouseUp:` don't
    // reach WebCore's DOM-event generator, so `mousedown` / `mouseup` /
    // `auxclick` never fire on the page. Follow the same pattern
    // [`Self::move_mouse`] uses for `mousemove`: fire JS `MouseEvent`s
    // on `document.elementFromPoint(x, y)` as a reliability belt so DOM
    // listeners see the click. Left (0) and right (1) native dispatch
    // already reaches WebCore, so this only applies to middle (2).
    if args.button == crate::options::MouseButton::Middle {
      for _ in 0..args.click_count {
        let js = format!(
          "(function(){{\
            var el = document.elementFromPoint({x},{y});\
            if (!el) return;\
            var opts = {{button:1,buttons:4,clientX:{x},clientY:{y},bubbles:true,cancelable:true,view:window}};\
            el.dispatchEvent(new MouseEvent('mousedown', opts));\
            el.dispatchEvent(new MouseEvent('mouseup', opts));\
            el.dispatchEvent(new MouseEvent('auxclick', opts));\
          }})()"
        );
        let _ = self.evaluate(&js).await;
      }
    }
    Ok(())
  }

  /// Dispatch a hover at `(x, y)`: `steps` interpolated `mouseMoved`
  /// `NSEvent`s ending at `(x, y)` exactly. The host already carries any
  /// previously-held modifier flags (see `host.m` `held_modifier_flags`)
  /// on each synthesised `NSEvent`, so no additional wiring is needed for
  /// `args.modifiers_bitmask`.
  ///
  /// # Errors
  ///
  /// Returns an error if any IPC call fails.
  pub async fn hover_at_with(&self, x: f64, y: f64, args: &super::BackendHoverArgs) -> Result<()> {
    let steps = args.steps.max(1);
    for i in 1..=steps {
      let t = f64::from(i) / f64::from(steps);
      let sx = if i == steps { x } else { x * t };
      let sy = if i == steps { y } else { y * t };
      // mouse_type=0 (move), button=0 ignored during mouseMoved, click_count=0
      self.send_mouse_event(0, 0, 0, sx, sy).await?;
      // Follow the existing `move_mouse` JS belt-and-suspenders: offscreen
      // `WKWebView` sometimes doesn't forward `mouseMoved:` to WebCore's
      // DOM event generator. Dispatching `mousemove` for intermediate
      // steps plus `mouseover`/`mouseenter` at the destination ensures
      // DOM listeners see the hover consistently in headless.
      let final_step = i == steps;
      let js = if final_step {
        format!(
          "(function(){{\
             var opts={{clientX:{sx},clientY:{sy},bubbles:true,view:window}};\
             var el=document.elementFromPoint({sx},{sy});\
             if(el){{\
               el.dispatchEvent(new MouseEvent('mousemove',opts));\
               el.dispatchEvent(new MouseEvent('mouseover',opts));\
               el.dispatchEvent(new MouseEvent('mouseenter',opts));\
             }}\
           }})()"
        )
      } else {
        format!(
          "document.elementFromPoint({sx},{sy})?.dispatchEvent(\
             new MouseEvent('mousemove',{{clientX:{sx},clientY:{sy},bubbles:true,view:window}}))"
        )
      };
      let _ = self.evaluate(&js).await;
    }
    Ok(())
  }

  /// `WebKit` (`WKWebView`) exposes no public touch-injection API —
  /// `AppKit` has no `NSTouchEvent` synthesis primitive comparable to
  /// CDP's `Input.dispatchTouchEvent`, and the private
  /// `_sendTouchDownAtLocation:` SPI is marked unavailable on macOS.
  /// Returns a typed `unsupported:` error for the caller to surface as
  /// [`crate::error::FerriError::Unsupported`].
  #[allow(clippy::unused_async, clippy::unused_self)]
  pub async fn tap_at_with(&self, _x: f64, _y: f64, _args: &super::BackendTapArgs) -> Result<()> {
    Err(
      "unsupported: tap is not available on the WebKit backend — WKWebView has no public touch-input \
         synthesis API (AppKit lacks NSTouchEvent synthesis and the private _sendTouchDownAtLocation: \
         SPI is marked unavailable on macOS). Use the cdp-pipe or cdp-raw backend for tap."
        .to_string(),
    )
  }

  /// Press each modifier key via `OP_KEY_DOWN` — the host tracks which
  /// `NSEventModifierFlag` bits are held so subsequent mouse events
  /// carry them (see `host.m` `held_modifier_flags`).
  ///
  /// # Errors
  ///
  /// Returns an error if any IPC call fails.
  pub async fn press_modifiers(&self, mods: &[crate::options::Modifier]) -> Result<()> {
    for md in mods {
      self.key_down(md.key_name()).await?;
    }
    Ok(())
  }

  /// Release each modifier key (reverse order — matches Playwright's
  /// unwind behavior).
  ///
  /// # Errors
  ///
  /// Returns an error if any IPC call fails.
  pub async fn release_modifiers(&self, mods: &[crate::options::Modifier]) -> Result<()> {
    for md in mods.iter().rev() {
      self.key_up(md.key_name()).await?;
    }
    Ok(())
  }

  /// Move the mouse to the given coordinates.
  /// Sends native `NSEvent` for CSS `:hover` state, plus a JS `mousemove`
  /// event for DOM listeners (native `mouseMoved:` doesn't reliably fire
  /// DOM events in headless/offscreen `WKWebView` windows).
  ///
  /// # Errors
  ///
  /// Returns an error if the native mouse event or JS evaluation fails.
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    let _ = self.send_mouse_event(0, 0, 0, x, y).await;
    let js = format!(
      "document.elementFromPoint({x},{y})?.dispatchEvent(new MouseEvent('mousemove',{{clientX:{x},clientY:{y},bubbles:true,view:window}}))"
    );
    let _ = self.evaluate(&js).await;
    Ok(())
  }

  /// Move the mouse smoothly from one point to another with bezier easing.
  /// Sends native `NSEvent` per step for CSS state, plus JS `mousemove`
  /// events for DOM listeners (native dispatch alone doesn't fire DOM events
  /// in headless `WKWebView`).
  ///
  /// # Errors
  ///
  /// Returns an error if any native mouse event or JS evaluation fails.
  pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<()> {
    let steps = steps.max(1);
    for i in 0..=steps {
      let t = f64::from(i) / f64::from(steps);
      let ease = t * t * (3.0 - 2.0 * t); // bezier easing (matches CDP)
      let x = from_x + (to_x - from_x) * ease;
      let y = from_y + (to_y - from_y) * ease;
      let _ = self.send_mouse_event(0, 0, 0, x, y).await;
      let js = format!(
        "document.elementFromPoint({x},{y})?.dispatchEvent(new MouseEvent('mousemove',{{clientX:{x},clientY:{y},bubbles:true,view:window}}))"
      );
      let _ = self.evaluate(&js).await;
    }
    Ok(())
  }

  /// Scroll the page by the given deltas using `window.scrollBy`.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    self.evaluate(&format!("window.scrollBy({delta_x},{delta_y})")).await?;
    Ok(())
  }

  /// Send a mouse-down event at the given coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse event IPC call fails.
  pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<()> {
    let btn: u8 = match button {
      "right" => 1,
      "middle" => 2,
      _ => 0,
    };
    self.send_mouse_event(1, btn, 1, x, y).await
  }

  /// Send a mouse-up event at the given coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse event IPC call fails.
  pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<()> {
    let btn: u8 = match button {
      "right" => 1,
      "middle" => 2,
      _ => 0,
    };
    self.send_mouse_event(2, btn, 1, x, y).await
  }

  /// Click and drag from one point to another with smooth easing.
  ///
  /// # Errors
  ///
  /// Returns an error if any of the mouse down/move/up IPC calls fail.
  pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<()> {
    self.send_mouse_event(1, 0, 1, from.0, from.1).await?; // down
    // Playwright default is `1` — a single `mousemove` at the destination.
    // For steps > 1, interpolate with a cubic ease between press and release.
    let steps = steps.max(1);
    for i in 1..=steps {
      let (x, y) = if steps == 1 {
        (to.0, to.1)
      } else {
        let t = f64::from(i) / f64::from(steps);
        let ease = t * t * (3.0 - 2.0 * t);
        (from.0 + (to.0 - from.0) * ease, from.1 + (to.1 - from.1) * ease)
      };
      self.send_mouse_event(0, 0, 0, x, y).await?; // move
    }
    self.send_mouse_event(2, 0, 1, to.0, to.1).await // up
  }

  /// Send a native mouse event via IPC.
  /// `mouse_type`: 0=move, 1=down, 2=up
  /// button: 0=left, 1=right, 2=middle
  async fn send_mouse_event(&self, mouse_type: u8, button: u8, click_count: u32, x: f64, y: f64) -> Result<()> {
    let mut p = Vec::with_capacity(27);
    p.push(mouse_type);
    p.push(button);
    p.extend_from_slice(&click_count.to_le_bytes());
    p.extend_from_slice(&x.to_le_bytes());
    p.extend_from_slice(&y.to_le_bytes());
    p.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(ipc::Op::MouseEvent, &p).await?;
    Self::ok(r)
  }

  /// Type text into the currently focused element via native key events.
  ///
  /// # Errors
  ///
  /// Returns an error if the type IPC call fails.
  pub async fn type_str(&self, text: &str) -> Result<()> {
    let mut p = Vec::new();
    ipc::str_encode(&mut p, text);
    p.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(Op::Type, &p).await?;
    Self::ok(r)
  }

  /// Press a keyboard key by name (e.g. "Enter", "Tab") via native key event.
  ///
  /// # Errors
  ///
  /// Returns an error if the key press IPC call fails.
  pub async fn key_down(&self, key: &str) -> Result<()> {
    let mut p = Vec::new();
    ipc::str_encode(&mut p, key);
    p.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(Op::KeyDown, &p).await?;
    Self::ok(r)
  }

  pub async fn key_up(&self, key: &str) -> Result<()> {
    let mut p = Vec::new();
    ipc::str_encode(&mut p, key);
    p.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(Op::KeyUp, &p).await?;
    Self::ok(r)
  }

  pub async fn press_key(&self, key: &str) -> Result<()> {
    let mut p = Vec::new();
    ipc::str_encode(&mut p, key);
    p.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(Op::PressKey, &p).await?;
    Self::ok(r)
  }

  /// Get all cookies for the current page's domain.
  ///
  /// # Errors
  ///
  /// Returns an error if the cookie retrieval IPC call fails or the response
  /// cannot be deserialized.
  pub async fn get_cookies(&self) -> Result<Vec<CookieData>> {
    let mut p = Vec::new();
    p.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(ipc::Op::GetCookies, &p).await?;
    match r {
      ipc::IpcResponse::Value(v) => {
        // The IPC reader already parses the JSON string into a Value.
        // Deserialize directly from the parsed Value.
        Ok(serde_json::from_value(v).unwrap_or_default())
      },
      ipc::IpcResponse::Error(e) => Err(e),
      _ => Err("unexpected response".into()),
    }
  }

  /// Set a cookie on the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the set cookie IPC call fails.
  pub async fn set_cookie(&self, c: CookieData) -> Result<()> {
    let mut p = Vec::new();
    p.extend_from_slice(&self.vid().to_le_bytes());
    ipc::str_encode(&mut p, &c.name);
    ipc::str_encode(&mut p, &c.value);
    ipc::str_encode(&mut p, &c.domain);
    ipc::str_encode(&mut p, &c.path);
    p.push(u8::from(c.secure));
    p.push(u8::from(c.http_only));
    let expires = c.expires.unwrap_or(-1.0);
    p.extend_from_slice(&expires.to_le_bytes());
    // Encode sameSite as a string (empty if not set).
    let same_site_str = c.same_site.map_or("", |ss| ss.as_str());
    ipc::str_encode(&mut p, same_site_str);
    let r = self.client.send(ipc::Op::SetCookie, &p).await?;
    Self::ok(r)
  }

  /// Delete a cookie by name and optional domain.
  ///
  /// # Errors
  ///
  /// Returns an error if the delete cookie IPC call fails.
  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<()> {
    let mut p = Vec::new();
    p.extend_from_slice(&self.vid().to_le_bytes());
    ipc::str_encode(&mut p, name);
    ipc::str_encode(&mut p, domain.unwrap_or(""));
    let r = self.client.send(ipc::Op::DeleteCookie, &p).await?;
    Self::ok(r)
  }

  /// Clear all cookies for the current page.
  ///
  /// # Errors
  ///
  /// Returns an error if the clear cookies IPC call fails.
  pub async fn clear_cookies(&self) -> Result<()> {
    let mut p = Vec::new();
    p.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(ipc::Op::ClearCookies, &p).await?;
    Self::ok(r)
  }

  /// Emulate a viewport by resizing the native window and setting device scale.
  ///
  /// # Errors
  ///
  /// Returns an error if the viewport IPC call fails.
  #[allow(clippy::cast_precision_loss)] // viewport dimensions fit in f64 without loss
  /// Apply a [`crate::options::BrowserContextOptions`] bag. Single
  /// backend entry point — every `WKWebView` IPC call derived from
  /// `opts` is inlined here (no per-field helper methods). Mirrors
  /// Playwright's `wkPage.ts:200` context-options init sequence.
  /// Unsupported fields return a typed error per field, aggregated.
  #[allow(clippy::too_many_lines)]
  pub async fn apply_context_options(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    use futures::future::OptionFuture;

    let viewport_fut: OptionFuture<_> = opts
      .resolved_viewport()
      .map(|vp| async move { self.emulate_viewport(&vp).await })
      .into();
    let media_fut: OptionFuture<_> = opts
      .any_media_override()
      .then(|| {
        let m = opts.as_emulate_media();
        async move { self.emulate_media(&m).await }
      })
      .into();
    // User-agent: wire-encoded string + viewId, OP_SET_USER_AGENT.
    let ua_fut: OptionFuture<_> = opts
      .user_agent
      .as_deref()
      .map(|ua| async move {
        let mut p = Vec::new();
        ipc::str_encode(&mut p, ua);
        p.extend_from_slice(&self.vid().to_le_bytes());
        let r = self.client.send(Op::SetUserAgent, &p).await?;
        Self::ok(r)
      })
      .into();
    // Locale + timezone: native IPC ops wrapping ICU category overrides.
    let locale_fut: OptionFuture<_> = opts
      .locale
      .as_deref()
      .map(|l| async move {
        let mut p = Vec::new();
        p.extend_from_slice(&self.vid().to_le_bytes());
        ipc::str_encode(&mut p, l);
        let r = self.client.send(ipc::Op::SetLocale, &p).await?;
        Self::ok(r)
      })
      .into();
    let tz_fut: OptionFuture<_> = opts
      .timezone_id
      .as_deref()
      .map(|tz| async move {
        let mut p = Vec::new();
        p.extend_from_slice(&self.vid().to_le_bytes());
        ipc::str_encode(&mut p, tz);
        let r = self.client.send(ipc::Op::SetTimezone, &p).await?;
        Self::ok(r)
      })
      .into();
    // javaScriptEnabled: WKWebView cannot disable JS from JS — record
    // the flag on a sentinel for the host to observe.
    let js_fut: OptionFuture<_> = opts
      .java_script_enabled
      .map(|v| async move {
        let script = format!("window.__fd_js_enabled = {}", if v { "true" } else { "false" });
        let _ = self.evaluate(&script).await;
        Ok(())
      })
      .into();
    let headers_fut: OptionFuture<_> = opts
      .extra_http_headers
      .as_ref()
      .map(|h| async move { self.set_extra_http_headers(h).await })
      .into();
    // Geolocation: override navigator.geolocation.getCurrentPosition
    // via evaluate — no WKWebView primitive for this.
    let geo_fut: OptionFuture<_> = opts
      .geolocation
      .map(|g| async move {
        let js = format!(
          "(function(){{var pos={{coords:{{latitude:{},longitude:{},accuracy:{},altitude:null,altitudeAccuracy:null,heading:null,speed:null}},timestamp:Date.now()}};navigator.geolocation.getCurrentPosition=function(s){{s(pos)}};navigator.geolocation.watchPosition=function(s){{s(pos);return 0}}}})()",
          g.latitude, g.longitude, g.accuracy
        );
        self.evaluate(&js).await.map(|_| ())
      })
      .into();
    // Offline: override navigator.onLine — WKWebView has no throttling.
    let offline_fut: OptionFuture<_> = opts
      .offline
      .map(|o| async move {
        let js = format!(
          "Object.defineProperty(navigator,'onLine',{{get:function(){{return {}}},configurable:true}})",
          if o { "false" } else { "true" }
        );
        self.evaluate(&js).await.map(|_| ())
      })
      .into();
    let sw_fut: OptionFuture<_> = opts
      .service_workers
      .map(|p| async move {
        if matches!(p, crate::options::ServiceWorkerPolicy::Block) {
          // Cross-backend: inject the register-override init script.
          self
            .add_init_script(
              "if(navigator.serviceWorker){navigator.serviceWorker.register=()=>Promise.reject(new Error('Service workers blocked'))}",
            )
            .await
            .map(|_| ())
        } else {
          Ok(())
        }
      })
      .into();

    let (r_vp, r_ua, r_loc, r_tz, r_js, r_hdr, r_med, r_geo, r_off, r_sw) = tokio::join!(
      viewport_fut,
      ua_fut,
      locale_fut,
      tz_fut,
      js_fut,
      headers_fut,
      media_fut,
      geo_fut,
      offline_fut,
      sw_fut,
    );

    let mut errs: Vec<String> = Vec::new();
    for (label, r) in [
      ("viewport", r_vp),
      ("userAgent", r_ua),
      ("locale", r_loc),
      ("timezoneId", r_tz),
      ("javaScriptEnabled", r_js),
      ("extraHTTPHeaders", r_hdr),
      ("media (colorScheme/reducedMotion/forcedColors/contrast)", r_med),
      ("geolocation", r_geo),
      ("offline", r_off),
      ("serviceWorkers", r_sw),
    ] {
      if let Some(Err(e)) = r {
        errs.push(format!("{label}: {e}"));
      }
    }
    for (label, present) in [
      ("bypassCSP", opts.bypass_csp.is_some()),
      ("ignoreHTTPSErrors", opts.ignore_https_errors.is_some()),
      ("acceptDownloads", opts.accept_downloads.is_some()),
      ("httpCredentials", opts.http_credentials.is_some()),
      ("screen", opts.screen.is_some()),
      ("permissions", opts.permissions.is_some()),
    ] {
      if present {
        errs.push(format!(
          "{label}: WebKit (stock WKWebView) does not expose this primitive via public IPC"
        ));
      }
    }

    if errs.is_empty() { Ok(()) } else { Err(errs.join("; ")) }
  }

  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<()> {
    // Native resize + scale via IPC -- sets window backingScaleFactor,
    // resizes NSWindow and WKWebView frame. Affects actual rendering.
    #[allow(clippy::cast_precision_loss)]
    let (width_f64, height_f64) = (config.width as f64, config.height as f64);
    let mut p = Vec::new();
    p.extend_from_slice(&width_f64.to_le_bytes());
    p.extend_from_slice(&height_f64.to_le_bytes());
    p.extend_from_slice(&config.device_scale_factor.to_le_bytes());
    p.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(ipc::Op::SetViewport, &p).await?;
    Self::ok(r)
  }

  /// Emulate media features (color scheme, reduced motion, forced colors, etc.).
  ///
  /// # Errors
  ///
  /// Returns an error if the emulate media IPC call fails.
  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<()> {
    use crate::options::MediaOverride;
    // Wire format: per-field pair of (action-byte, value-string). The
    // action byte is `0` = unchanged (host leaves this override alone),
    // `1` = disabled (clear any prior override), `2` = set (apply value).
    // This lets a single OP_EMULATE_MEDIA call carry any mix of unchanged
    // / reset / set fields, matching Playwright's three-state semantic.
    fn enc(p: &mut Vec<u8>, o: &MediaOverride) {
      match o {
        MediaOverride::Unchanged => {
          p.push(0);
          ipc::str_encode(p, "");
        },
        MediaOverride::Disabled => {
          p.push(1);
          ipc::str_encode(p, "");
        },
        MediaOverride::Set(v) => {
          p.push(2);
          ipc::str_encode(p, v.as_str());
        },
      }
    }
    let mut p = Vec::new();
    p.extend_from_slice(&self.vid().to_le_bytes());
    enc(&mut p, &opts.color_scheme);
    enc(&mut p, &opts.reduced_motion);
    enc(&mut p, &opts.forced_colors);
    enc(&mut p, &opts.media);
    enc(&mut p, &opts.contrast);
    let r = self.client.send(ipc::Op::EmulateMedia, &p).await?;
    Self::ok(r)
  }

  /// Inject custom HTTP headers by intercepting `fetch` and `XMLHttpRequest` via JS.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<()> {
    use std::fmt::Write;
    // Intercept fetch/XMLHttpRequest to add custom headers via WKUserScript.
    // This covers JS-initiated requests. Navigation requests need NSURLProtocol.
    let mut js = String::from("(function(){");
    js.push_str("var _fetch=window.fetch;window.fetch=function(u,o){o=o||{};o.headers=Object.assign({");
    for (k, v) in headers {
      let ek = k.replace('\'', "\\'");
      let ev = v.replace('\'', "\\'");
      let _ = write!(js, "'{ek}':'{ev}',");
    }
    js.push_str("},o.headers||{});return _fetch.call(this,u,o)};");
    // Also intercept XMLHttpRequest
    js.push_str("var _open=XMLHttpRequest.prototype.open;var _send=XMLHttpRequest.prototype.send;");
    js.push_str(
      "XMLHttpRequest.prototype.open=function(){this._fd_args=arguments;return _open.apply(this,arguments)};",
    );
    js.push_str("XMLHttpRequest.prototype.send=function(b){");
    for (k, v) in headers {
      let ek = k.replace('\'', "\\'");
      let ev = v.replace('\'', "\\'");
      let _ = write!(js, "this.setRequestHeader('{ek}','{ev}');");
    }
    js.push_str("return _send.call(this,b)}})()");
    self.evaluate(&js).await?;
    Ok(())
  }

  /// Grant permissions. No-op on `WebKit` backend as `WKWebView` does not expose permission management.
  ///
  /// # Errors
  ///
  /// This function currently always succeeds.
  pub fn grant_permissions(
    &self,
    _permissions: &[String],
    _origin: Option<&str>,
  ) -> impl std::future::Future<Output = crate::error::Result<()>> {
    use crate::FerriError;
    let result = if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
      Err(FerriError::target_closed(Some("page is closed".into())))
    } else {
      Ok(())
    };
    std::future::ready(result)
  }

  /// Bypass CSP: stock `WKWebView` exposes no public API to disable
  /// Reset permissions. No-op on `WebKit` backend.
  ///
  /// # Errors
  ///
  /// This function currently always succeeds.
  pub fn reset_permissions(&self) -> impl std::future::Future<Output = crate::error::Result<()>> {
    use crate::FerriError;
    let result = if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
      Err(FerriError::target_closed(Some("page is closed".into())))
    } else {
      Ok(())
    };
    std::future::ready(result)
  }

  /// Start performance tracing by recording the start time.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn start_tracing(&self) -> Result<()> {
    // Mark the start time for performance measurement
    self.evaluate("window.__fd_trace_start = performance.now()").await?;
    Ok(())
  }

  /// Stop performance tracing by recording the end time.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn stop_tracing(&self) -> Result<()> {
    self.evaluate("window.__fd_trace_end = performance.now()").await?;
    Ok(())
  }

  /// Get page performance metrics (`DOMContentLoaded`, Load, TTFB).
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation to read performance entries fails.
  pub async fn metrics(&self) -> Result<Vec<MetricData>> {
    let js = r"(function(){var p=performance.getEntriesByType('navigation')[0];if(!p)return'[]';return JSON.stringify([{name:'DOMContentLoaded',value:p.domContentLoadedEventEnd},{name:'Load',value:p.loadEventEnd},{name:'TTFB',value:p.responseStart}])})()";
    let r = self.evaluate(js).await?;
    let s = r
      .and_then(|v| v.as_str().map(std::string::ToString::to_string))
      .unwrap_or("[]".into());
    Ok(serde_json::from_str(&s).unwrap_or_default())
  }

  /// Resolve a backend node ID to an element handle via CSS attribute selector.
  ///
  /// # Errors
  ///
  /// Returns an error if no element with the given `data-cref` attribute is found.
  pub async fn resolve_backend_node(&self, _id: i64, ref_id: &str) -> Result<AnyElement> {
    self.find_element(&format!("[data-cref='{ref_id}']")).await
  }

  /// Spawn a background task that drains console, dialog, and network events
  /// from the IPC reader thread into the shared state logs.
  pub fn attach_listeners(
    &self,
    console_log: Arc<RwLock<Vec<ConsoleMessage>>>,
    net_log: Arc<RwLock<Vec<NetworkRequest>>>,
    dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
  ) {
    // Register the emitter-bridge so `page.events().on("dialog", cb)`
    // continues to deliver live `Dialog` handles via the broadcast.
    let _ = self.dialog_manager.register_emitter_bridge(self.events.clone());
    // Parity bridge for `filechooser` — registered for API
    // consistency with CDP/BiDi even though stock `WKWebView` has no
    // public API for intercepting the native `NSOpenPanel`. Any
    // `page.on('filechooser', cb)` registration completes silently;
    // no event ever arrives on WebKit. Callers that care about this
    // path see the gap via `page.wait_for_file_chooser` timing out.
    let _ = self.file_chooser_manager.register_emitter_bridge(self.events.clone());
    // Parity bridge for `download` — registered for API consistency
    // with CDP/BiDi even though stock `WKWebView`'s download events
    // don't yet flow through our IPC. See the docstring on
    // `download_manager` above for the gap specifics; a future phase
    // adds the `WKDownloadDelegate` wiring on the host side.
    let _ = self.download_manager.register_emitter_bridge(self.events.clone());

    let client = self.client.clone();
    let emitter = self.events.clone();
    let notify = client.event_notify.clone();
    let injected_script = self.injected_script.clone();
    let dialog_manager = self.dialog_manager.clone();
    let page_backref = self.page_backref.clone();
    let exposed_fns = self.exposed_fns.clone();
    let view_id = self.view_id;
    tokio::spawn(async move {
      loop {
        // Wait for the IPC reader thread to signal that events arrived.
        // No polling -- wakes instantly when a console/dialog/network event is received.
        notify.notified().await;

        // Drain console events
        {
          // Multi-page hosts share a single fdConsole IPC channel —
          // pull only the events tagged with this listener's view_id
          // (the host appends `source_vid` to every console frame).
          // Events for other pages are left in the buffer for their
          // own listeners. `vid == 0` is the legacy/unknown bucket
          // and falls through to whichever drainer wakes first
          // (matches the pre-vid behaviour for events the host
          // couldn't attribute).
          let (msgs, others_present) = {
            let Ok(mut log) = client.console_log.lock() else {
              continue;
            };
            if log.is_empty() {
              (Vec::new(), false)
            } else {
              let mut mine = Vec::new();
              let mut others = Vec::new();
              for entry in log.drain(..) {
                if entry.2 == view_id || entry.2 == 0 {
                  mine.push(entry);
                } else {
                  others.push(entry);
                }
              }
              let others_present = !others.is_empty();
              *log = others;
              (mine, others_present)
            }
          };
          // Multi-page hosts share one `event_notify` semaphore.
          // `notify_one` wakes only one waiter; if a sibling page's
          // event landed in our wake-up but not ours, we have to
          // re-notify so the sibling's listener picks it up — without
          // this the buffered event stays parked until the NEXT
          // unrelated notify fires.
          if others_present {
            client.event_notify.notify_one();
          }
          let msgs: Vec<(String, String, u64)> = msgs;
          if !msgs.is_empty() {
            drain_console_events(
              &msgs,
              &page_backref,
              &emitter,
              &console_log,
              &exposed_fns,
              &client,
              view_id,
            )
            .await;
          }
        }

        // Drain dialog events
        //
        // Stock `WKWebView` makes its accept/dismiss decision inside
        // the host subprocess's `WKUIDelegate` before the event
        // reaches Rust — the IPC payload already carries the
        // `action`. We still dispatch the event through the same
        // `DialogManager` as the CDP/BiDi backends so listeners
        // observe the dialog (`type` / `message`); the
        // [`crate::dialog::Dialog`]'s responder rejects
        // `accept` / `dismiss` with a typed error naming the
        // limitation, because the host has already closed the
        // dialog. Rule-4 honesty: the observation surface works,
        // the manipulation surface reports why it can't.
        {
          let evts: Vec<(String, String, String)> = {
            let Ok(mut log) = client.dialog_log.lock() else {
              continue;
            };
            if log.is_empty() {
              Vec::new()
            } else {
              std::mem::take(&mut *log)
            }
          };
          if !evts.is_empty() {
            let mut dest = dialog_log.write().await;
            for (dtype, message, action) in evts {
              let dialog_type = crate::dialog::DialogType::parse(&dtype);
              let responder: crate::dialog::DialogResponder = Arc::new(move |_response| {
                Box::pin(async move {
                  Err(
                    "Dialog.accept/dismiss is not supported on the WebKit backend: stock WKWebView decides the response in the host's WKUIDelegate before the event reaches Rust"
                      .to_string(),
                  )
                })
              });
              let dialog = crate::dialog::Dialog::new_with_manager(
                dialog_type,
                message.clone(),
                String::new(),
                responder,
                Some(dialog_manager.clone()),
              );
              dialog_manager.did_open(dialog);
              dest.push(crate::state::DialogEvent {
                dialog_type: dtype,
                message,
                action,
              });
            }
          }
        }

        drain_network_events(&client, &net_log, &emitter, &injected_script).await;
      }
    });
  }

  // ── Init Scripts ──

  /// Inject a script to run at document start on every navigation.
  /// `WebKit` uses `WKUserScript` -- returns a synthetic identifier (the script hash).
  /// Note: `WKWebView` does not support removing individual user scripts by ID.
  ///
  /// # Errors
  ///
  /// Returns an error if the `AddInitScript` IPC call fails.
  pub async fn add_init_script(&self, source: &str) -> Result<String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut p = Vec::new();
    p.extend_from_slice(&self.vid().to_le_bytes());
    ipc::str_encode(&mut p, source);
    let r = self.client.send(ipc::Op::AddInitScript, &p).await?;
    Self::ok(r)?;
    // WKWebView doesn't return an identifier for user scripts.
    // Generate a deterministic one from the source hash for tracking.
    let mut h = DefaultHasher::new();
    source.hash(&mut h);
    Ok(format!("wk-{:x}", h.finish()))
  }

  /// Remove an init script. On `WebKit` this is a no-op -- `WKUserScript`
  /// removal requires clearing all scripts and re-adding the remaining ones.
  /// For now, scripts persist for the lifetime of the page.
  ///
  /// # Errors
  ///
  /// This function currently always succeeds (no-op).
  pub fn remove_init_script(&self, _identifier: &str) -> impl std::future::Future<Output = crate::error::Result<()>> {
    use crate::FerriError;
    // WKWebView limitation: individual WKUserScript removal is not supported.
    let result = if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
      Err(FerriError::target_closed(Some("page is closed".into())))
    } else {
      Ok(())
    };
    std::future::ready(result)
  }

  // ── Exposed Functions ──
  // WebKit has no `Runtime.addBinding` analogue. The exposed-function
  // dispatch rides over the existing console-event channel: the JS
  // shim posts a JSON envelope through `console.log`; the
  // `attach_listeners` console drain (see `drain_console_events`)
  // intercepts envelopes whose first arg starts with
  // `{"__ferri_call":`, runs the registered Rust callback, and
  // resolves the page-side promise via `evaluate`.

  /// Install a JS shim that proxies `window[name](...args)` into the
  /// supplied Rust callback. The callback's JSON return becomes the
  /// page-side promise resolution.
  ///
  /// # Errors
  ///
  /// Returns an error if the page is closed or the host rejects the
  /// `add_init_script` / `evaluate` calls.
  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<()> {
    if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
      return Err("Page is closed".into());
    }
    let escaped = crate::steps::js_escape(name);
    let shim = format!(
      r"(function(){{
        if (window['{escaped}']) return;
        window['{escaped}'] = function(){{
          var s = (window.__ferri_seq = (window.__ferri_seq || 0) + 1).toString(36) + Math.random().toString(36).slice(2);
          var args = [];
          for (var i = 0; i < arguments.length; i++) args.push(arguments[i]);
          var p = new Promise(function(resolve){{
            window.__ferri_exposed = window.__ferri_exposed || {{}};
            window.__ferri_exposed[s] = resolve;
          }});
          console.log(JSON.stringify({{__ferri_call: '{escaped}', id: s, args: args}}));
          return p;
        }};
      }})()"
    );
    self.add_init_script(&shim).await?;
    let _ = self.evaluate(&shim).await?;
    self.exposed_fns.write().await.insert(name.to_string(), func);
    Ok(())
  }

  /// Drop the registration and remove the page-side shim.
  ///
  /// # Errors
  ///
  /// Returns an error if the page is closed or the deregister
  /// `evaluate` fails.
  pub async fn remove_exposed_function(&self, name: &str) -> Result<()> {
    if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
      return Err("Page is closed".into());
    }
    self.exposed_fns.write().await.remove(name);
    let escaped = crate::steps::js_escape(name);
    let _ = self.evaluate(&format!("delete window['{escaped}']")).await?;
    Ok(())
  }

  /// Register a route handler to intercept network requests matching the given matcher.
  ///
  /// The matcher's JS-side pre-filter regex (see
  /// [`crate::url_matcher::UrlMatcher::regex_source_for_prefilter`]) is injected
  /// into the page-side interceptor so only matching URLs incur an IPC
  /// round-trip. Predicate matchers route every URL through Rust.
  ///
  /// # Errors
  ///
  /// Returns an error if the route lock is poisoned or the JavaScript
  /// injection to register the route pattern fails.
  pub async fn route(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::route::RouteHandler,
  ) -> Result<()> {
    let prefilter_regex_src = matcher.regex_source_for_prefilter();

    // Add route to Rust-side list (write lock -- cold path)
    self
      .routes
      .write()
      .map_err(|e| format!("routes write lock poisoned: {e}"))?
      .push(crate::route::RegisteredRoute { matcher, handler });

    // Set up the IPC route callback (once) to dispatch to our routes list
    {
      let mut rh = self
        .client
        .route_handler
        .lock()
        .map_err(|e| format!("route_handler lock poisoned: {e}"))?;
      if rh.is_none() {
        let routes_ref = self.routes.clone();
        *rh = Some(std::sync::Arc::new(
          move |url: &str, method: &str, headers_json: &str, post_data: &str| {
            let Ok(routes) = routes_ref.read() else {
              return r#"{"action":"continue"}"#.to_string();
            }; // read lock -- hot path
            for route in routes.iter() {
              if route.matcher.matches(url) {
                let headers: rustc_hash::FxHashMap<String, String> =
                  serde_json::from_str(headers_json).unwrap_or_default();
                let intercepted = crate::route::InterceptedRequest {
                  request_id: String::new(),
                  url: url.to_string(),
                  method: method.to_string(),
                  headers,
                  post_data: if post_data.is_empty() {
                    None
                  } else {
                    Some(post_data.to_string())
                  },
                  resource_type: String::new(),
                };
                let (tx, rx) = tokio::sync::oneshot::channel();
                let route_obj = crate::route::Route::new(intercepted, tx);
                (route.handler)(route_obj);
                // Block to receive the action (WebKit handler is sync).
                let action = rx.blocking_recv().unwrap_or(crate::route::RouteAction::Continue(
                  crate::route::ContinueOverrides::default(),
                ));
                return match action {
                  crate::route::RouteAction::Fulfill(resp) => {
                    let body_str = String::from_utf8_lossy(&resp.body).to_string();
                    let mut headers_map = serde_json::Map::new();
                    for (k, v) in &resp.headers {
                      headers_map.insert(k.clone(), serde_json::Value::String(v.clone()));
                    }
                    serde_json::json!({
                        "action": "fulfill",
                        "status": resp.status,
                        "body": body_str,
                        "headers": headers_map,
                        "contentType": resp.content_type,
                    })
                    .to_string()
                  },
                  crate::route::RouteAction::Continue(_) => r#"{"action":"continue"}"#.to_string(),
                  crate::route::RouteAction::Abort(reason) => {
                    serde_json::json!({"action": "abort", "reason": reason}).to_string()
                  },
                };
              }
            }
            r#"{"action":"continue"}"#.to_string()
          },
        ));
      }
    }

    // Add the JS regex pattern so the page interceptor knows to call fdRoute for this URL
    let regex_str = prefilter_regex_src.replace('\\', "\\\\").replace('\'', "\\'");
    let js = format!(
      "(function(){{window.__fd_routes=window.__fd_routes||[];window.__fd_routes.push(new RegExp('{regex_str}'))}})();"
    );
    self.evaluate(&js).await?;
    self.add_init_script(&js).await?;

    Ok(())
  }

  /// Remove a previously registered route handler matching the given matcher.
  ///
  /// # Errors
  ///
  /// Returns an error if the route lock is poisoned or the JavaScript cleanup fails.
  pub async fn unroute(&self, matcher: &crate::url_matcher::UrlMatcher) -> Result<()> {
    let prefilter_regex_src = matcher.regex_source_for_prefilter();

    // Remove from Rust-side list (write lock -- cold path)
    self
      .routes
      .write()
      .map_err(|e| format!("routes write lock poisoned: {e}"))?
      .retain(|r| !r.matcher.equivalent(matcher));

    // Remove from JS-side pattern list
    let regex_str = prefilter_regex_src.replace('\\', "\\\\").replace('\'', "\\'");
    let js = format!(
      "(function(){{window.__fd_routes=(window.__fd_routes||[]).filter(function(r){{return r.source!=='{regex_str}'}})}})()"
    );
    self.evaluate(&js).await?;
    Ok(())
  }

  // ── Handle lifecycle ────────────────────────────────────────────────────

  /// Release the `window.__wr` registry entry for the given ref via the
  /// `Op::ReleaseRef` IPC op. Used by `AnyPage::release_handle` when
  /// disposing a `JSHandle` / `ElementHandle` on the `WebKit` backend.
  ///
  /// Idempotent on the host side: deleting an absent key is a no-op.
  ///
  /// # Errors
  ///
  /// Returns the transport error if the IPC call fails.
  pub async fn release_ref(&self, ref_id: u64) -> Result<()> {
    let mut payload = Vec::with_capacity(16);
    payload.extend_from_slice(&ref_id.to_le_bytes());
    payload.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(ipc::Op::ReleaseRef, &payload).await?;
    Self::ok(r)
  }

  /// Close this page (view) via the IPC close command.
  ///
  /// Honors `run_before_unload`: when `true`, synchronously dispatches a
  /// `BeforeUnloadEvent` on the page's window before issuing the close IPC.
  /// That's the best macOS `WKWebView` can do without a first-class CDP
  /// `Page.close` analogue — any `addEventListener('beforeunload', ...)`
  /// handlers installed by the page will fire and can do cleanup work.
  /// (`WKWebView`'s native dialog-style beforeunload prompt requires a
  /// `WKNavigationDelegate` decision handler flow we don't currently
  /// surface.)
  ///
  /// # Errors
  ///
  /// Returns an error if the close IPC call fails.
  pub async fn close_page(&self, opts: crate::options::PageCloseOptions) -> Result<()> {
    if opts.run_before_unload.unwrap_or(false) {
      // Best-effort: dispatch the event so page-registered handlers run.
      // We intentionally ignore the return — page code may legitimately
      // throw inside its own handler and we still need to proceed to close.
      let _ = self
        .evaluate(
          "(() => { try { window.dispatchEvent(new Event('beforeunload', { cancelable: true })); } catch (_) {} })()",
        )
        .await;
    }
    let r = self.client.send_vid(ipc::Op::Close, self.vid()).await?;
    Self::ok(r)?;
    self.closed.store(true, std::sync::atomic::Ordering::Release);
    Ok(())
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.closed.load(std::sync::atomic::Ordering::Acquire)
  }
}

// ─── WebKitElement ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WebKitElement {
  client: Arc<IpcClient>,
  view_id: u64,
  ref_id: u64,
}

impl WebKitElement {
  fn el(&self) -> String {
    format!("window.__wr.get({})", self.ref_id)
  }

  /// Raw `window.__wr` registry index. Public so
  /// [`crate::backend::element_handle_remote`] can extract it when
  /// minting a [`crate::js_handle::HandleRemote::WebKit`] for a public
  /// [`crate::element_handle::ElementHandle`].
  #[must_use]
  pub fn ref_id(&self) -> u64 {
    self.ref_id
  }

  async fn eval(&self, js: &str) -> Result<()> {
    let mut p = Vec::new();
    ipc::str_encode(&mut p, js);
    p.extend_from_slice(&self.view_id.to_le_bytes());
    let _ = self.client.send(Op::Evaluate, &p).await?;
    Ok(())
  }

  /// Get the center coordinates of this element after scrolling it into view.
  /// Returns (x, y) or falls back to (0, 0).
  #[allow(clippy::many_single_char_names)]
  async fn get_center(&self) -> Result<(f64, f64)> {
    let js = format!(
      "(function(){{var e={el};e.scrollIntoViewIfNeeded?e.scrollIntoViewIfNeeded():e.scrollIntoView({{block:'center'}});var r=e.getBoundingClientRect();return JSON.stringify({{x:r.x+r.width/2,y:r.y+r.height/2}})}})()",
      el = self.el()
    );
    let mut payload = Vec::new();
    ipc::str_encode(&mut payload, &js);
    payload.extend_from_slice(&self.view_id.to_le_bytes());
    let result = self.client.send(ipc::Op::Evaluate, &payload).await?;
    match result {
      IpcResponse::Value(val) => {
        let obj: serde_json::Value = if let Some(s) = val.as_str() {
          serde_json::from_str(s).unwrap_or_default()
        } else {
          val
        };
        let cx = obj.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let cy = obj.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        Ok((cx, cy))
      },
      IpcResponse::Error(err) => Err(err),
      _ => Ok((0.0, 0.0)),
    }
  }

  /// Send a native mouse event for this element's view.
  async fn send_mouse(&self, mouse_type: u8, button: u8, click_count: u32, pos_x: f64, pos_y: f64) -> Result<()> {
    let mut payload = Vec::with_capacity(27);
    payload.push(mouse_type);
    payload.push(button);
    payload.extend_from_slice(&click_count.to_le_bytes());
    payload.extend_from_slice(&pos_x.to_le_bytes());
    payload.extend_from_slice(&pos_y.to_le_bytes());
    payload.extend_from_slice(&self.view_id.to_le_bytes());
    let result = self.client.send(ipc::Op::MouseEvent, &payload).await?;
    match result {
      IpcResponse::Error(err) => Err(err),
      _ => Ok(()),
    }
  }

  /// Click the element using native `NSEvent` after scrolling it into view.
  /// Single JS evaluate for scroll+bbox (matches CDP optimization), then native mouse events.
  ///
  /// # Errors
  ///
  /// Returns an error if coordinate extraction or the native click IPC call fails.
  pub async fn click(&self) -> Result<()> {
    let (x, y) = self.get_center().await?;
    if x == 0.0 && y == 0.0 {
      return self.eval(&format!("{}.click()", self.el())).await;
    }
    self.send_mouse(1, 0, 1, x, y).await?; // down
    self.send_mouse(2, 0, 1, x, y).await // up
  }

  /// Double-click the element using native `NSEvent` with proper clickCount.
  /// First click pair (clickCount=1) fires 'click', second pair (clickCount=2) fires 'dblclick'.
  ///
  /// # Errors
  ///
  /// Returns an error if coordinate extraction or native mouse IPC calls fail.
  pub async fn dblclick(&self) -> Result<()> {
    let (x, y) = self.get_center().await?;
    if x == 0.0 && y == 0.0 {
      return self
        .eval(&format!(
          "{}.dispatchEvent(new MouseEvent('dblclick',{{bubbles:true}}))",
          self.el()
        ))
        .await;
    }
    // First click (clickCount=1) fires 'click'
    self.send_mouse(1, 0, 1, x, y).await?;
    self.send_mouse(2, 0, 1, x, y).await?;
    // Second click (clickCount=2) fires 'dblclick'
    self.send_mouse(1, 0, 2, x, y).await?;
    self.send_mouse(2, 0, 2, x, y).await
  }

  /// Hover over the element using native `NSEvent` mouseMoved + JS mouseenter.
  /// Native mouseMoved doesn't propagate mouseenter to DOM in offscreen `WKWebView`
  /// windows, so we also fire the JS event to ensure hover handlers trigger.
  ///
  /// # Errors
  ///
  /// Returns an error if coordinate extraction, native mouse IPC, or JS eval fails.
  pub async fn hover(&self) -> Result<()> {
    let (x, y) = self.get_center().await?;
    // Native mouse move for CSS :hover state
    let _ = self.send_mouse(0, 0, 0, x, y).await;
    // JS mouseenter for DOM event handlers (needed for offscreen WKWebView windows)
    self
      .eval(&format!(
        "(function(){{var e={el};e.dispatchEvent(new MouseEvent('mouseenter',{{clientX:{x},clientY:{y},bubbles:true,view:window}}));\
         e.dispatchEvent(new MouseEvent('mouseover',{{clientX:{x},clientY:{y},bubbles:true,view:window}}))}})()
",
        el = self.el()
      ))
      .await
  }

  /// Type text into the element using native `InsertText` editing command.
  /// Focuses the element first, then uses the native IPC type op which fires
  /// `beforeinput`/`input` events with `isTrusted: true` (matches CDP `Input.insertText`).
  ///
  /// # Errors
  ///
  /// Returns an error if focusing or the native type IPC call fails.
  pub async fn type_str(&self, text: &str) -> Result<()> {
    // Focus the element first via click (matches CDP element type_str behavior)
    self.click().await?;
    // Use native OP_TYPE for trusted input events
    let mut p = Vec::new();
    ipc::str_encode(&mut p, text);
    p.extend_from_slice(&self.view_id.to_le_bytes());
    let r = self.client.send(Op::Type, &p).await?;
    match r {
      IpcResponse::Error(e) => Err(e),
      _ => Ok(()),
    }
  }

  /// Call a JavaScript function with this element as `this`.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn call_js_fn(&self, func: &str) -> Result<()> {
    self.eval(&format!("({}).call({})", func, self.el())).await
  }

  /// Call a JavaScript function with this element as `this` and return the result.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation or IPC call fails.
  pub async fn call_js_fn_value(&self, func: &str) -> Result<Option<serde_json::Value>> {
    let js = format!("JSON.stringify(({}).call({}))", func, self.el());
    let mut p = Vec::new();
    ipc::str_encode(&mut p, &js);
    p.extend_from_slice(&self.view_id.to_le_bytes());
    let r = self.client.send(ipc::Op::Evaluate, &p).await?;
    match r {
      ipc::IpcResponse::Value(serde_json::Value::String(s)) => Ok(serde_json::from_str(&s).ok()),
      ipc::IpcResponse::Value(v) => Ok(Some(v)),
      ipc::IpcResponse::Error(e) => Err(e),
      _ => Ok(None),
    }
  }

  /// Scroll the element into view with instant behavior.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn scroll_into_view(&self) -> Result<()> {
    self
      .eval(&format!(
        "{}.scrollIntoView({{behavior:'instant',block:'center'}})",
        self.el()
      ))
      .await
  }

  /// Take a screenshot of this element (currently takes full page screenshot).
  ///
  /// # Errors
  ///
  /// Returns an error if the screenshot IPC call fails or no image data is returned.
  pub async fn screenshot(&self, fmt: ImageFormat) -> Result<Vec<u8>> {
    // Must match page screenshot payload: u8 format + u8 quality + u64 vid
    let mut p = Vec::new();
    let fmt_byte: u8 = match fmt {
      ImageFormat::Jpeg => 1,
      ImageFormat::Webp => 2,
      ImageFormat::Png => 0,
    };
    p.push(fmt_byte);
    p.push(80); // default quality
    p.extend_from_slice(&self.view_id.to_le_bytes());
    let r = self.client.send(Op::Screenshot, &p).await?;
    match r {
      IpcResponse::Binary(d) => Ok(d),
      IpcResponse::Error(e) => Err(e),
      _ => Err("no data".into()),
    }
  }
}

/// Drain a batch of host-side `(level, text)` console events,
/// dispatching each through the appropriate page-event variant.
///
/// `WebKit`'s host interceptor currently surfaces only `(level, text)`
/// for each `console.*` call. Args and stack-trace location require a
/// new IPC op — tracked as a Section B gap in `PLAYWRIGHT_COMPAT.md`.
/// We build a `ConsoleMessage` with empty `args` + default location so
/// callers still get a live handle they can dispatch off
/// `type()` / `text()` / `timestamp()`.
///
/// `level == "pageerror"` is a synthetic marker injected by the host
/// userScript for uncaught JS errors / unhandled rejections (see
/// `host.m::errorJS`). These are routed to
/// `PageEvent::PageError(WebError)` instead of `PageEvent::Console`.
/// The `text` field is `"<name>: <message>\n<stack>"` so
/// [`parse_webkit_pageerror_payload`] can recover the structured
/// `{ name, message, stack }` without a new IPC op.
async fn drain_console_events(
  msgs: &[(String, String, u64)],
  page_backref: &crate::backend::PageBackref,
  emitter: &crate::events::EventEmitter,
  console_log: &Arc<RwLock<Vec<ConsoleMessage>>>,
  exposed_fns: &Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, crate::events::ExposedFn>>>,
  client: &Arc<ipc::IpcClient>,
  view_id: u64,
) {
  let page = page_backref.upgrade();
  let mut dest = console_log.write().await;
  for (raw_type, text, _src_vid) in msgs {
    if raw_type == "pageerror" {
      let details = parse_webkit_pageerror_payload(text);
      let web_err = match page {
        Some(ref p) => crate::web_error::WebError::new(p, details),
        None => crate::web_error::WebError::new_detached(details),
      };
      emitter.emit(crate::events::PageEvent::PageError(web_err));
      continue;
    }
    // Exposed-function dispatch (see `WebKitPage::expose_function`).
    // The JS shim posts `console.log(JSON.stringify({__ferri_call,
    // id, args}))`; intercept that envelope, run the registered
    // callback, resolve the page-side promise, and skip the
    // user-visible console emit.
    if text.starts_with(r#"{"__ferri_call":"#) {
      if let Ok(payload) = serde_json::from_str::<serde_json::Value>(text) {
        let fn_name = payload
          .get("__ferri_call")
          .and_then(|v| v.as_str())
          .unwrap_or("")
          .to_string();
        let id = payload.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let args: Vec<serde_json::Value> = payload
          .get("args")
          .and_then(|v| v.as_array())
          .cloned()
          .unwrap_or_default();
        let maybe_fn = exposed_fns.read().await.get(&fn_name).cloned();
        if let Some(callback) = maybe_fn {
          let result = callback(args);
          let result_js = serde_json::to_string(&result).unwrap_or_else(|_| "null".into());
          let escaped_id = id.replace('\\', r"\\").replace('\'', r"\'");
          let resolve_js = format!(
            "(function(){{ var f = window.__ferri_exposed && window.__ferri_exposed['{escaped_id}']; if (f) {{ delete window.__ferri_exposed['{escaped_id}']; f({result_js}); }} }})()"
          );
          let _ = client.send_str_vid(Op::Evaluate, &resolve_js, view_id).await;
        }
        continue;
      }
    }
    let timestamp = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    // Remap `'warn'` to `'warning'` for Playwright parity — CDP's
    // `Runtime.consoleAPICalled.type` is already `'warning'`; BiDi's
    // `log.entryAdded.method` is `'warn'` which we remap at the BiDi
    // listener. Do the same for WebKit's host-side label so every
    // backend's `ConsoleMessage.type()` is consistent.
    let type_str = if raw_type == "warn" {
      "warning".to_string()
    } else {
      raw_type.clone()
    };
    let msg = if let Some(ref page) = page {
      ConsoleMessage::new(
        page,
        type_str,
        Some(text.clone()),
        Vec::new(),
        crate::console_message::ConsoleMessageLocation::default(),
        timestamp,
      )
    } else {
      ConsoleMessage::new_detached(
        type_str,
        Some(text.clone()),
        Vec::new(),
        crate::console_message::ConsoleMessageLocation::default(),
        timestamp,
      )
    };
    emitter.emit(crate::events::PageEvent::Console(msg.clone()));
    dest.push(msg);
  }
}

/// Parse the `"<name>: <message>\n<stack>"` payload emitted by the
/// `WebKit` host-side error userScript (`host.m::errorJS`) into a
/// structured [`crate::web_error::ErrorDetails`]. Splits at the first
/// `\n` for the `{ name, message }` head vs. `stack` body, then splits
/// the head at the first `': '` for `name` vs. `message`. When neither
/// separator is present, the entire `text` becomes `message` and
/// `name` defaults to `'Error'` — matches the behaviour of JS-side
/// `String(new Error('…'))`.
fn parse_webkit_pageerror_payload(text: &str) -> crate::web_error::ErrorDetails {
  let (head, stack) = match text.find('\n') {
    Some(idx) => (&text[..idx], text[idx + 1..].to_string()),
    None => (text, String::new()),
  };
  let (name, message) = match head.find(": ") {
    Some(idx) => (head[..idx].to_string(), head[idx + 2..].to_string()),
    None => ("Error".to_string(), head.to_string()),
  };
  crate::web_error::ErrorDetails { name, message, stack }
}

/// Drain pending JS-interceptor network events into the page's
/// `network_log` and emit the matching `PageEvent` variants. Extracted
/// from `WebKitPage::attach_listeners` so the listener loop stays
/// under the line-count budget.
async fn drain_network_events(
  client: &Arc<ipc::IpcClient>,
  net_log: &Arc<RwLock<Vec<NetworkRequest>>>,
  emitter: &crate::events::EventEmitter,
  injected_script: &Arc<InjectedScriptManager>,
) {
  use ipc::NetworkEvent;
  let evts: Vec<NetworkEvent> = {
    let Ok(mut log) = client.network_log.lock() else {
      return;
    };
    if log.is_empty() {
      Vec::new()
    } else {
      std::mem::take(&mut *log)
    }
  };
  if evts.is_empty() {
    return;
  }
  let mut dest = net_log.write().await;
  // Per-drain request lookup index — every Response/Failure event
  // refers back to the originating request by JS-side seq id.
  let mut by_id: rustc_hash::FxHashMap<String, network::Request> = rustc_hash::FxHashMap::default();
  for r in dest.iter() {
    by_id.insert(r.id().to_string(), r.clone());
  }
  for ev in evts {
    match ev {
      NetworkEvent::Request {
        id,
        method,
        url,
        resource_type,
      } => {
        if resource_type == "Document" {
          injected_script.reset();
        }
        // Stock `WKWebView` exposes no public API for raw request body
        // bytes — `request.postData()` stays null. Response bodies
        // surface typed `Unsupported` via `body_unsupported`. Per Rule 4.
        let req = network::Request::new(RequestInit {
          id: id.clone(),
          url: url.clone(),
          method: method.clone(),
          resource_type: resource_type.clone(),
          is_navigation_request: resource_type == "Document",
          post_data: None,
          headers: Headers::default(),
          frame_id: None,
          redirected_from: None,
          timing: None,
          raw_headers_fn: None,
        });
        by_id.insert(id, req.clone());
        emitter.emit(crate::events::PageEvent::Request(req.clone()));
        dest.push(req);
      },
      NetworkEvent::Response {
        id,
        status,
        status_text,
        url,
        headers,
      } => {
        let Some(req) = by_id.get(&id).cloned() else { continue };
        let response = NetworkResponse::new(ResponseInit {
          request: req.clone(),
          url,
          status,
          status_text,
          from_service_worker: false,
          http_version: None,
          headers,
          remote_addr: None,
          security_details: None,
          body_fn: Some(body_unsupported(
            "Response body is not retrievable on stock WKWebView (no public API)",
          )),
          raw_headers_fn: None,
        });
        req.set_response(&response).await;
        response.finish_success().await;
        emitter.emit(crate::events::PageEvent::Response(response));
        emitter.emit(crate::events::PageEvent::RequestFinished(req));
      },
      NetworkEvent::Failure { id, error_text } => {
        let Some(req) = by_id.get(&id).cloned() else { continue };
        req.set_failure(error_text).await;
        emitter.emit(crate::events::PageEvent::RequestFailed(req));
      },
    }
  }
}
