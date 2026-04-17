#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
//! Unified CDP backend -- Chrome `DevTools` Protocol over pipes or WebSocket.
//!
//! Generic over transport: `CdpBrowser<PipeTransport>` for pipe-based,
//! `CdpBrowser<WsTransport>` for WebSocket-based. Both share identical page,
//! element, and event handling logic.
//!
//! Navigation follows Bun's ChromeBackend.cpp architecture: register a oneshot
//! waiter before sending Page.navigate, then await the waiter which resolves
//! when the reader task sees Page.loadEventFired for that session.

pub mod pipe;
pub mod transport;
pub mod ws;

use base64::Engine as _;

use super::{
  AnyElement, AnyPage, Arc, AxNodeData, AxProperty, ConsoleMsg, CookieData, ImageFormat, MetricData, NetRequest,
  RwLock, ScreenshotOpts,
};
use rustc_hash::FxHashMap;
use std::time::Duration;
use transport::CdpTransport;

/// Sealed trait mapping transport types to their `AnyPage`/`AnyElement` enum constructors.
/// This avoids having "abstract" methods or duplicated impls on the generic types.
pub trait CdpWrap: CdpTransport + Sized {
  fn wrap_page(page: CdpPage<Self>) -> AnyPage;
  fn wrap_element(elem: CdpElement<Self>) -> AnyElement;
}

impl CdpWrap for pipe::PipeTransport {
  fn wrap_page(page: CdpPage<Self>) -> AnyPage {
    AnyPage::CdpPipe(page)
  }
  fn wrap_element(elem: CdpElement<Self>) -> AnyElement {
    AnyElement::CdpPipe(elem)
  }
}

impl CdpWrap for ws::WsTransport {
  fn wrap_page(page: CdpPage<Self>) -> AnyPage {
    AnyPage::CdpRaw(page)
  }
  fn wrap_element(elem: CdpElement<Self>) -> AnyElement {
    AnyElement::CdpRaw(elem)
  }
}

// ---- CdpBrowser<T> --------------------------------------------------------

pub struct CdpBrowser<T: CdpTransport> {
  transport: Arc<T>,
  child: Arc<tokio::sync::Mutex<Option<tokio::process::Child>>>,
  /// Track targetId -> sessionId for already-attached targets.
  attached_targets: std::sync::Mutex<FxHashMap<String, Option<String>>>,
}

impl<T: CdpTransport> Clone for CdpBrowser<T> {
  fn clone(&self) -> Self {
    Self {
      transport: Arc::clone(&self.transport),
      child: Arc::clone(&self.child),
      attached_targets: std::sync::Mutex::new(
        self
          .attached_targets
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .clone(),
      ),
    }
  }
}

impl<T: CdpWrap> CdpBrowser<T> {
  /// Enable required CDP domains on a session so events and queries work.
  /// If `viewport` is provided, sets viewport in the same parallel batch.
  /// If `unpause` is true, sends `Runtime.runIfWaitingForDebugger` in the same
  /// batch (for targets created with `waitForDebuggerOnStart`).
  async fn enable_domains(
    transport: &T,
    session_id: Option<&str>,
    viewport: Option<&crate::options::ViewportConfig>,
    unpause: bool,
  ) -> Result<(), String> {
    let ep = super::empty_params();

    let vp_params = viewport.map(|vp| {
      let is_landscape = vp.is_landscape || vp.width > vp.height;
      let orientation = if vp.is_mobile {
        if is_landscape {
          serde_json::json!({"angle": 90, "type": "landscapePrimary"})
        } else {
          serde_json::json!({"angle": 0, "type": "portraitPrimary"})
        }
      } else {
        serde_json::json!({"angle": 0, "type": "landscapePrimary"})
      };
      serde_json::json!({
        "width": vp.width,
        "height": vp.height,
        "deviceScaleFactor": vp.device_scale_factor,
        "mobile": vp.is_mobile,
        "screenWidth": vp.width,
        "screenHeight": vp.height,
        "screenOrientation": orientation,
      })
    });

    // Fire all CDP commands in parallel — matches Playwright's FrameSession._initialize().
    // Keep default page bootstrap minimal. Domains for logging and explicit focus
    // emulation are feature-specific and can be enabled later if needed.
    let vp_fut = async {
      if let Some(params) = vp_params {
        transport
          .send_command(session_id, "Emulation.setDeviceMetricsOverride", params)
          .await
          .map(|_| ())
      } else {
        Ok(())
      }
    };

    // Unpause future — included in parallel batch so Chrome processes it after
    // all enables (CDP commands on a session are processed in order).
    let unpause_fut = async {
      if unpause {
        transport
          .send_command(session_id, "Runtime.runIfWaitingForDebugger", super::empty_params())
          .await
          .map(|_| ())
      } else {
        Ok(())
      }
    };

    let (r1, r2, r3, r4, r5, r6, r7) = tokio::join!(
      transport.send_command(session_id, "Page.enable", ep.clone()),
      transport.send_command(session_id, "Runtime.enable", ep.clone()),
      transport.send_command(session_id, "Network.enable", ep.clone()),
      transport.send_command(
        session_id,
        "Page.setLifecycleEventsEnabled",
        serde_json::json!({"enabled": true})
      ),
      transport.send_command(
        session_id,
        "Target.setAutoAttach",
        serde_json::json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true})
      ),
      vp_fut,
      unpause_fut,
    );
    r1?;
    r2?;
    r3?;
    r4?;
    r5?;
    r6?;
    r7?;
    Ok(())
  }

  /// Internal constructor for after transport + child process setup.
  ///
  /// Matches Playwright's `CRBrowser.connect()` exactly:
  /// 1. `Browser.getVersion` — handshake, ensures pipe is ready
  /// 2. `Target.setAutoAttach` — auto-attach new targets with `waitForDebuggerOnStart`
  ///
  /// No page creation here — pages are created on demand via `new_page()`.
  async fn init(transport: Arc<T>, child: Option<tokio::process::Child>) -> Result<Self, String> {
    transport
      .send_command(None, "Browser.getVersion", super::empty_params())
      .await?;

    transport
      .send_command(
        None,
        "Target.setAutoAttach",
        serde_json::json!({
          "autoAttach": true,
          "waitForDebuggerOnStart": true,
          "flatten": true,
        }),
      )
      .await?;

    Ok(Self {
      transport,
      child: Arc::new(tokio::sync::Mutex::new(child)),
      attached_targets: std::sync::Mutex::new(FxHashMap::default()),
    })
  }

  /// Retrieve all open page targets, attaching to any not yet tracked.
  pub async fn pages(&self) -> Result<Vec<AnyPage>, String> {
    let result = self
      .transport
      .send_command(None, "Target.getTargets", super::empty_params())
      .await?;

    let targets = result
      .get("targetInfos")
      .and_then(|t| t.as_array())
      .cloned()
      .unwrap_or_default();

    let mut pages = Vec::new();
    for target in targets {
      if target.get("type").and_then(|v| v.as_str()) != Some("page") {
        continue;
      }
      let target_id = target
        .get("targetId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

      // Check if we already have a session for this target
      let existing_sid = {
        self
          .attached_targets
          .lock()
          .map_err(|e| format!("Lock poisoned: {e}"))?
          .get(&target_id)
          .cloned()
      };

      let sid = if let Some(sid) = existing_sid {
        sid
      } else {
        // Target not yet tracked — manually attach. This handles pre-existing targets
        // (connect flow) and targets created before setAutoAttach was enabled.
        let attach = self
          .transport
          .send_command(
            None,
            "Target.attachToTarget",
            serde_json::json!({"targetId": target_id, "flatten": true}),
          )
          .await?;

        let sid = attach
          .get("sessionId")
          .and_then(|v| v.as_str())
          .map(std::string::ToString::to_string);

        self
          .attached_targets
          .lock()
          .map_err(|e| format!("Lock poisoned: {e}"))?
          .insert(target_id.clone(), sid.clone());

        Self::enable_domains(&self.transport, sid.as_deref(), None, false).await?;

        sid
      };

      let lc_state = Arc::new(std::sync::Mutex::new(LifecycleState::new()));
      let lc_notify = Arc::new(tokio::sync::Notify::new());
      pages.push(T::wrap_page(CdpPage {
        transport: self.transport.clone(),
        session_id: sid.map(Arc::from),
        target_id: Arc::from(target_id),
        browser_context_id: None,
        events: crate::events::EventEmitter::new(),
        frame_contexts: Arc::new(tokio::sync::RwLock::new(FxHashMap::default())),
        dialog_handler: Arc::new(tokio::sync::RwLock::new(crate::events::default_dialog_handler())),
        exposed_fns: Arc::new(tokio::sync::RwLock::new(FxHashMap::default())),
        binding_initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        routes: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        fetch_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        http_credentials: Arc::new(tokio::sync::RwLock::new(None)),
        main_frame_id: Arc::new(tokio::sync::OnceCell::new()),
        lifecycle: lc_state.clone(),
        lifecycle_notify: lc_notify.clone(),
        injected_script: Arc::new(InjectedScriptManager::new()),
      }));
    }
    Ok(pages)
  }

  /// Create a new browser context (isolated cookies, storage, cache).
  /// Matches Playwright's `browser.newContext()` → `Target.createBrowserContext`.
  pub async fn new_context(&self) -> Result<String, String> {
    let ctx = self
      .transport
      .send_command(
        None,
        "Target.createBrowserContext",
        serde_json::json!({"disposeOnDetach": true}),
      )
      .await?;

    ctx
      .get("browserContextId")
      .and_then(|v| v.as_str())
      .map(String::from)
      .ok_or_else(|| "No browserContextId".to_string())
  }

  /// Dispose a browser context. Matches Playwright's `context.close()`.
  pub async fn dispose_context(&self, browser_context_id: &str) -> Result<(), String> {
    self
      .transport
      .send_command(
        None,
        "Target.disposeBrowserContext",
        serde_json::json!({"browserContextId": browser_context_id}),
      )
      .await?;
    Ok(())
  }

  /// Create a new page, optionally in a specific browser context.
  ///
  /// Follows Playwright's sequence: `Target.createTarget` → wait for auto-attach
  /// event (target is paused) → enable domains → `Runtime.runIfWaitingForDebugger`.
  pub async fn new_page(
    &self,
    url: &str,
    browser_context_id: Option<&str>,
    viewport: Option<&crate::options::ViewportConfig>,
  ) -> Result<AnyPage, String> {
    // Subscribe to events BEFORE createTarget so we don't miss the auto-attach.
    let mut event_rx = self.transport.subscribe_events();

    let create_params = if let Some(ctx_id) = browser_context_id {
      serde_json::json!({"url": "about:blank", "browserContextId": ctx_id})
    } else {
      serde_json::json!({"url": "about:blank"})
    };

    let result = self
      .transport
      .send_command(None, "Target.createTarget", create_params)
      .await?;

    let target_id = result
      .get("targetId")
      .and_then(|v| v.as_str())
      .ok_or("No targetId")?
      .to_string();

    // Wait for Target.attachedToTarget event (from setAutoAttach in init).
    // The target is paused (waitForDebuggerOnStart) so we can set up everything.
    let tid = target_id.clone();
    let sid = tokio::time::timeout(Duration::from_secs(30), async move {
      while let Ok(event) = event_rx.recv().await {
        if event.get("method").and_then(|m| m.as_str()) == Some("Target.attachedToTarget") {
          if let Some(params) = event.get("params") {
            let event_tid = params
              .get("targetInfo")
              .and_then(|i| i.get("targetId"))
              .and_then(|v| v.as_str())
              .unwrap_or("");
            if event_tid == tid {
              return Ok(params.get("sessionId").and_then(|v| v.as_str()).map(String::from));
            }
          }
        }
      }
      Err("Event channel closed".to_string())
    })
    .await
    .map_err(|_| format!("Timeout waiting for auto-attach of {target_id}"))??;

    self
      .attached_targets
      .lock()
      .map_err(|e| format!("Lock poisoned: {e}"))?
      .insert(target_id.clone(), sid.clone());

    // Enable domains + unpause in one parallel batch (saves a round-trip).
    Self::enable_domains(&self.transport, sid.as_deref(), viewport, true).await?;

    let lc_state = Arc::new(std::sync::Mutex::new(LifecycleState::new()));
    let lc_notify = Arc::new(tokio::sync::Notify::new());
    let page = CdpPage {
      transport: self.transport.clone(),
      session_id: sid.map(Arc::from),
      target_id: Arc::from(target_id),
      browser_context_id: browser_context_id.map(Arc::from),
      events: crate::events::EventEmitter::new(),
      frame_contexts: Arc::new(tokio::sync::RwLock::new(FxHashMap::default())),
      dialog_handler: Arc::new(tokio::sync::RwLock::new(crate::events::default_dialog_handler())),
      exposed_fns: Arc::new(tokio::sync::RwLock::new(FxHashMap::default())),
      binding_initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      routes: Arc::new(tokio::sync::RwLock::new(Vec::new())),
      fetch_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      http_credentials: Arc::new(tokio::sync::RwLock::new(None)),
      main_frame_id: Arc::new(tokio::sync::OnceCell::new()),
      lifecycle: lc_state.clone(),
      lifecycle_notify: lc_notify.clone(),
      injected_script: Arc::new(InjectedScriptManager::new()),
    };

    // Register lifecycle tracker in the transport reader (synchronous update, zero overhead)
    page.transport.register_lifecycle_tracker(
      page.session_id.as_deref().unwrap_or(""),
      page.lifecycle.clone(),
      page.lifecycle_notify.clone(),
    );

    if url != "about:blank" && !url.is_empty() {
      page.goto(url, crate::backend::NavLifecycle::Load, 30_000).await?;
    }

    Ok(T::wrap_page(page))
  }

  /// Close the browser process and release resources.
  pub async fn close(&mut self) -> Result<(), String> {
    let _ = self
      .transport
      .send_command(None, "Browser.close", super::empty_params())
      .await;
    if let Some(mut child) = self.child.lock().await.take() {
      let _ = child.kill().await;
    }
    Ok(())
  }
}

// ── Pipe-specific launch ───────────────���─────────────────────────────────────

impl CdpBrowser<pipe::PipeTransport> {
  /// Launch Chrome with `--remote-debugging-pipe` and communicate over fd 3/4.
  pub async fn launch(chromium_path: &str) -> Result<Self, String> {
    Self::launch_with_flags(chromium_path, &crate::state::chrome_flags(true, &[])).await
  }

  /// Launch Chrome with custom flags and communicate over fd 3/4.
  pub async fn launch_with_flags(chromium_path: &str, flags: &[String]) -> Result<Self, String> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let user_data_dir = std::env::temp_dir().join(format!("ferridriver-pipe-{}-{id}", std::process::id()));

    let (transport, child) = pipe::PipeTransport::spawn(chromium_path, &user_data_dir, flags)?;
    Self::init(Arc::new(transport), Some(child)).await
  }
}

// ── WS-specific launch + connect ─────────────────────────────────────────────

impl CdpBrowser<ws::WsTransport> {
  /// Launch Chrome with `--remote-debugging-port` and communicate over WebSocket.
  pub async fn launch(chromium_path: &str) -> Result<Self, String> {
    Box::pin(Self::launch_with_flags(
      chromium_path,
      &crate::state::chrome_flags(true, &[]),
    ))
    .await
  }

  /// Launch Chrome with custom flags and communicate over WebSocket.
  pub async fn launch_with_flags(chromium_path: &str, flags: &[String]) -> Result<Self, String> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let user_data_dir = std::env::temp_dir().join(format!("ferridriver-raw-{}-{id}", std::process::id()));

    let (transport, child) = Box::pin(ws::WsTransport::spawn(chromium_path, &user_data_dir, flags)).await?;
    Self::init(Arc::new(transport), Some(child)).await
  }

  /// Connect to a running Chrome instance via WebSocket URL.
  pub async fn connect(ws_url: &str) -> Result<Self, String> {
    let transport = Arc::new(Box::pin(ws::WsTransport::connect(ws_url)).await?);

    transport
      .send_command(None, "Target.setDiscoverTargets", serde_json::json!({"discover": true}))
      .await?;

    // Find existing page targets
    let result = transport
      .send_command(None, "Target.getTargets", super::empty_params())
      .await?;

    let mut attached = FxHashMap::default();
    let mut found_page = false;

    if let Some(targets) = result.get("targetInfos").and_then(|t| t.as_array()) {
      for target in targets {
        if target.get("type").and_then(|v| v.as_str()) == Some("page") {
          let target_id = target
            .get("targetId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
          let attach = transport
            .send_command(
              None,
              "Target.attachToTarget",
              serde_json::json!({"targetId": target_id, "flatten": true}),
            )
            .await?;
          let sid = attach
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string);
          Self::enable_domains(&transport, sid.as_deref(), None, false).await?;
          attached.insert(target_id, sid);
          found_page = true;
          break; // take first page
        }
      }
    }

    // If no existing page, create one
    if !found_page {
      let create_result = transport
        .send_command(None, "Target.createTarget", serde_json::json!({"url": "about:blank"}))
        .await?;
      let target_id = create_result
        .get("targetId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
      let attach = transport
        .send_command(
          None,
          "Target.attachToTarget",
          serde_json::json!({"targetId": target_id, "flatten": true}),
        )
        .await?;
      let sid = attach
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
      Self::enable_domains(&transport, sid.as_deref(), None, false).await?;
      attached.insert(target_id, sid);
    }

    Ok(Self {
      transport,
      child: Arc::new(tokio::sync::Mutex::new(None)),
      attached_targets: std::sync::Mutex::new(attached),
    })
  }
}

// ---- CdpPage<T> ------------------------------------------------------------

/// Recursively collect frame info from a CDP frame tree node.
fn collect_frames(node: &serde_json::Value, out: &mut Vec<super::FrameInfo>) {
  if let Some(frame) = node.get("frame") {
    out.push(super::FrameInfo {
      frame_id: frame.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
      parent_frame_id: frame
        .get("parentId")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string),
      name: frame.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
      url: frame.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    });
  }
  if let Some(children) = node.get("childFrames").and_then(|v| v.as_array()) {
    for child in children {
      collect_frames(child, out);
    }
  }
}

/// Lifecycle state for a page's current document.
/// Tracks which lifecycle events have fired and which document they belong to.
/// Updated synchronously by the transport reader. Checked synchronously by `goto()`.
pub struct LifecycleState {
  /// loaderId of the current committed document (from Page.frameNavigated).
  pub current_loader_id: String,
  /// Lifecycle events fired for the current document.
  pub fired: std::collections::HashSet<String>,
}

impl LifecycleState {
  fn new() -> Self {
    Self {
      current_loader_id: String::new(),
      fired: std::collections::HashSet::new(),
    }
  }
}

pub struct CdpPage<T: CdpTransport> {
  transport: Arc<T>,
  session_id: Option<Arc<str>>,
  target_id: Arc<str>,
  /// Browser context ID for isolated contexts (used for `Target.disposeBrowserContext` on close).
  browser_context_id: Option<Arc<str>>,
  /// Event emitter for page events (console, dialog, network, frame lifecycle).
  pub events: crate::events::EventEmitter,
  /// Frame ID -> execution context ID mapping for frame-scoped evaluation.
  frame_contexts: Arc<tokio::sync::RwLock<FxHashMap<String, i64>>>,
  /// Configurable dialog handler. Called when a JS dialog appears.
  pub dialog_handler: Arc<tokio::sync::RwLock<crate::events::DialogHandler>>,
  /// Registered exposed function callbacks.
  pub exposed_fns: Arc<tokio::sync::RwLock<FxHashMap<String, crate::events::ExposedFn>>>,
  /// Whether the binding channel has been initialized.
  binding_initialized: Arc<std::sync::atomic::AtomicBool>,
  /// Whether this page has been closed.
  closed: Arc<std::sync::atomic::AtomicBool>,
  /// Registered route handlers for network interception.
  routes: Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
  /// Whether Fetch domain is enabled for interception.
  fetch_enabled: Arc<std::sync::atomic::AtomicBool>,
  /// HTTP credentials for Fetch.authRequired handling (digest/NTLM/basic).
  http_credentials: Arc<tokio::sync::RwLock<Option<(String, String)>>>,
  /// Cached main frame ID to avoid repeated `Page.getFrameTree` calls.
  main_frame_id: Arc<tokio::sync::OnceCell<String>>,
  /// Lifecycle state for current document — tracks loaderId + fired events.
  /// Updated synchronously by the transport reader task. Checked synchronously by `goto()`.
  lifecycle: Arc<std::sync::Mutex<LifecycleState>>,
  /// Notification sent when lifecycle state is updated.
  lifecycle_notify: Arc<tokio::sync::Notify>,
  /// Manager for lazy engine injection.
  injected_script: Arc<InjectedScriptManager>,
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

  async fn ensure<T: CdpWrap>(&self, page: &CdpPage<T>) -> Result<(), String> {
    if !self.injected.load(std::sync::atomic::Ordering::Relaxed) {
      let full_check_js = crate::selectors::build_lazy_inject_js();
      let _ = page
        .cmd(
          "Runtime.evaluate",
          serde_json::json!({
              "expression": full_check_js,
              "returnByValue": false,
              "awaitPromise": true,
          }),
        )
        .await?;
      self.injected.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
  }
}

impl<T: CdpTransport> Clone for CdpPage<T> {
  fn clone(&self) -> Self {
    Self {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      target_id: self.target_id.clone(),
      browser_context_id: self.browser_context_id.clone(),
      events: self.events.clone(),
      frame_contexts: self.frame_contexts.clone(),
      dialog_handler: self.dialog_handler.clone(),
      exposed_fns: self.exposed_fns.clone(),
      binding_initialized: self.binding_initialized.clone(),
      closed: self.closed.clone(),
      routes: self.routes.clone(),
      fetch_enabled: self.fetch_enabled.clone(),
      http_credentials: self.http_credentials.clone(),
      main_frame_id: self.main_frame_id.clone(),
      lifecycle: self.lifecycle.clone(),
      lifecycle_notify: self.lifecycle_notify.clone(),
      injected_script: self.injected_script.clone(),
    }
  }
}

impl<T: CdpWrap> CdpPage<T> {
  /// Send a CDP command to this page's session.
  async fn cmd(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    self
      .transport
      .send_command(self.session_id.as_deref(), method, params)
      .await
  }

  // ---- Navigation ----

  pub async fn goto(&self, url: &str, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64) -> Result<(), String> {
    self.injected_script.reset();
    let target_event = match lifecycle {
      crate::backend::NavLifecycle::Commit => "commit",
      crate::backend::NavLifecycle::DomContentLoaded => "domcontentloaded",
      crate::backend::NavLifecycle::Load => "load",
    };

    // Register nav waiter BEFORE navigate.
    let rx = self
      .transport
      .register_nav_waiter(self.session_id.as_deref().unwrap_or(""), lifecycle);

    // Send navigation command. Response includes loaderId for this navigation.
    let nav_result = self.cmd("Page.navigate", serde_json::json!({"url": url})).await?;

    if let Some(error_text) = nav_result.get("errorText").and_then(|v| v.as_str()) {
      if !error_text.is_empty() {
        return Err(format!("Navigation failed: {error_text}"));
      }
    }

    let nav_loader_id = nav_result.get("loaderId").and_then(|v| v.as_str()).unwrap_or("");

    // Sync check: if the lifecycle event for THIS navigation's document already
    // fired (reader processed frameNavigated + lifecycle events during the navigate
    // command), return immediately. The loaderId match ensures we don't return
    // early with stale data from a previous navigation.
    {
      let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      if state.current_loader_id == nav_loader_id && state.fired.contains(target_event) {
        return Ok(());
      }
    }

    // Async wait for the lifecycle event via oneshot.
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await {
      Ok(Ok(result)) => result,
      Ok(Err(_)) | Err(_) => Ok(()),
    }
  }

  pub async fn wait_for_navigation(&self) -> Result<(), String> {
    let rx = self.transport.register_nav_waiter(
      self.session_id.as_deref().unwrap_or(""),
      crate::backend::NavLifecycle::Load,
    );

    match tokio::time::timeout(Duration::from_secs(30), rx).await {
      Ok(Ok(result)) => result,
      Ok(Err(_)) => Err("Navigation waiter dropped".into()),
      Err(_) => Ok(()),
    }
  }

  pub async fn reload(&self, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64) -> Result<(), String> {
    self.injected_script.reset();
    let rx = self
      .transport
      .register_nav_waiter(self.session_id.as_deref().unwrap_or(""), lifecycle);
    self.cmd("Page.reload", super::empty_params()).await?;
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await {
      Ok(Ok(r)) => r,
      _ => Ok(()),
    }
  }

  pub async fn go_back(&self, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64) -> Result<(), String> {
    self.history_go(-1, lifecycle, timeout_ms).await
  }

  pub async fn go_forward(&self, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64) -> Result<(), String> {
    self.history_go(1, lifecycle, timeout_ms).await
  }

  async fn history_go(
    &self,
    delta: i32,
    lifecycle: crate::backend::NavLifecycle,
    timeout_ms: u64,
  ) -> Result<(), String> {
    let hist = self.cmd("Page.getNavigationHistory", super::empty_params()).await?;
    let current_i64 = hist
      .get("currentIndex")
      .and_then(serde_json::Value::as_i64)
      .unwrap_or(0);
    let current = i32::try_from(current_i64).unwrap_or(i32::MAX);
    let target = current + delta;
    let entries = hist.get("entries").and_then(|v| v.as_array());
    let Some(entries) = entries else {
      return Ok(());
    };
    let Ok(target_usize) = usize::try_from(target) else {
      return Ok(());
    };
    if target_usize >= entries.len() {
      return Ok(());
    }
    let entry_id = entries[target_usize]
      .get("id")
      .and_then(serde_json::Value::as_i64)
      .unwrap_or(0);

    let rx = self
      .transport
      .register_nav_waiter(self.session_id.as_deref().unwrap_or(""), lifecycle);
    self
      .cmd("Page.navigateToHistoryEntry", serde_json::json!({"entryId": entry_id}))
      .await?;
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await {
      Ok(Ok(r)) => r,
      _ => Ok(()),
    }
  }

  pub async fn url(&self) -> Result<Option<String>, String> {
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": "location.href",
            "returnByValue": true,
        }),
      )
      .await?;
    Ok(
      result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string),
    )
  }

  pub async fn title(&self) -> Result<Option<String>, String> {
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": "document.title",
            "returnByValue": true,
        }),
      )
      .await?;
    Ok(
      result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string),
    )
  }

  // ---- JavaScript ----

  pub async fn injected_script(&self) -> Result<String, String> {
    self.ensure_engine_injected().await?;
    Ok("window.__fd".to_string())
  }

  /// Ensures the selector engine is injected into the current execution context.
  /// Idempotent and navigation-aware.
  pub async fn ensure_engine_injected(&self) -> Result<(), String> {
    self.injected_script.ensure(self).await
  }

  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>, String> {
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": expression,
            "returnByValue": true,
            "awaitPromise": true,
        }),
      )
      .await?;

    if let Some(exception) = result.get("exceptionDetails") {
      let text = exception
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("Evaluation error");
      return Err(text.to_string());
    }

    Ok(result.get("result").and_then(|r| r.get("value")).cloned())
  }

  // ---- Frames ----

  pub async fn get_frame_tree(&self) -> Result<Vec<super::FrameInfo>, String> {
    let result = self.cmd("Page.getFrameTree", super::empty_params()).await?;

    let mut frames = Vec::new();
    if let Some(tree) = result.get("frameTree") {
      collect_frames(tree, &mut frames);
    }
    Ok(frames)
  }

  pub async fn evaluate_in_frame(&self, expression: &str, frame_id: &str) -> Result<Option<serde_json::Value>, String> {
    let context_id = {
      let contexts = self.frame_contexts.read().await;
      contexts.get(frame_id).copied()
    };

    if let Some(ctx_id) = context_id {
      let result = self
        .cmd(
          "Runtime.evaluate",
          serde_json::json!({
              "expression": expression,
              "contextId": ctx_id,
              "returnByValue": true,
              "awaitPromise": true,
          }),
        )
        .await?;

      if let Some(exception) = result.get("exceptionDetails") {
        let text = exception
          .get("text")
          .and_then(|v| v.as_str())
          .unwrap_or("Evaluation error");
        return Err(text.to_string());
      }
      Ok(result.get("result").and_then(|r| r.get("value")).cloned())
    } else {
      Err(format!(
        "No execution context found for frame '{frame_id}'. Frame may not be loaded yet."
      ))
    }
  }

  // ---- Elements ----

  pub async fn find_element(&self, selector: &str) -> Result<AnyElement, String> {
    let doc = self.cmd("DOM.getDocument", serde_json::json!({"depth": 0})).await?;
    let root_id = doc
      .get("root")
      .and_then(|r| r.get("nodeId"))
      .and_then(serde_json::Value::as_i64)
      .ok_or_else(|| "No document root".to_string())?;

    let result = self
      .cmd(
        "DOM.querySelector",
        serde_json::json!({"nodeId": root_id, "selector": selector}),
      )
      .await?;

    let node_id = result
      .get("nodeId")
      .and_then(serde_json::Value::as_i64)
      .ok_or_else(|| format!("'{selector}' not found"))?;

    if node_id == 0 {
      return Err(format!("'{selector}' not found"));
    }

    Ok(T::wrap_element(CdpElement {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      handles: Arc::new(tokio::sync::Mutex::new(CdpElementHandles {
        node_id: Some(node_id),
        object_id: None,
      })),
    }))
  }

  pub async fn evaluate_to_element(&self, js: &str) -> Result<AnyElement, String> {
    let _ = self.cmd("DOM.getDocument", serde_json::json!({"depth": 0})).await;

    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": js,
            "returnByValue": false,
        }),
      )
      .await?;

    let object_id = result
      .get("result")
      .and_then(|r| r.get("objectId"))
      .and_then(|v| v.as_str())
      .ok_or("JS did not return a DOM element")?;

    Ok(T::wrap_element(CdpElement {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      handles: Arc::new(tokio::sync::Mutex::new(CdpElementHandles {
        node_id: None,
        object_id: Some(Arc::from(object_id)),
      })),
    }))
  }

  // ---- Content ----

  pub async fn content(&self) -> Result<String, String> {
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": "document.documentElement.outerHTML",
            "returnByValue": true,
        }),
      )
      .await?;
    Ok(
      result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string(),
    )
  }

  pub async fn set_content(&self, html: &str) -> Result<(), String> {
    let frame_id = self
      .main_frame_id
      .get_or_try_init(|| async {
        let tree = self.cmd("Page.getFrameTree", super::empty_params()).await?;
        tree
          .get("frameTree")
          .and_then(|f| f.get("frame"))
          .and_then(|f| f.get("id"))
          .and_then(|v| v.as_str())
          .map(std::string::ToString::to_string)
          .ok_or_else(|| "No main frame".to_string())
      })
      .await?;

    // Selector engine is already injected via Page.addScriptToEvaluateOnNewDocument
    // during page setup, so setDocumentContent triggers it automatically.
    self
      .cmd(
        "Page.setDocumentContent",
        serde_json::json!({"frameId": frame_id, "html": html}),
      )
      .await?;
    Ok(())
  }

  // ---- Screenshots ----

  pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>, String> {
    let format_str = match opts.format {
      ImageFormat::Png => "png",
      ImageFormat::Jpeg => "jpeg",
      ImageFormat::Webp => "webp",
    };
    let mut params = serde_json::json!({"format": format_str});
    if let Some(q) = opts.quality {
      params["quality"] = serde_json::json!(q);
    }
    if opts.full_page {
      // Playwright approach: get full page dimensions via JS + getLayoutMetrics in parallel,
      // then use clip with captureBeyondViewport to render everything in one pass.
      let metrics = self.cmd("Page.getLayoutMetrics", super::empty_params()).await?;
      let content_size = metrics.get("contentSize");
      let w = content_size
        .and_then(|c| c.get("width"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(800.0);
      let h = content_size
        .and_then(|c| c.get("height"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(600.0);
      // Use visualViewport.scale like Playwright (not hardcoded 1)
      let scale = metrics
        .get("visualViewport")
        .and_then(|v| v.get("scale"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(1.0);
      params["clip"] = serde_json::json!({
          "x": 0, "y": 0, "width": w, "height": h, "scale": scale
      });
      params["captureBeyondViewport"] = serde_json::json!(true);
    }

    let result = self.cmd("Page.captureScreenshot", params).await?;
    let data = result
      .get("data")
      .and_then(|v| v.as_str())
      .ok_or("No screenshot data")?;
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
      .map_err(|e| format!("Decode screenshot: {e}"))
  }

  pub async fn screenshot_element(&self, selector: &str, format: ImageFormat) -> Result<Vec<u8>, String> {
    let js = format!(
      r"(function(){{
                const el = document.querySelector('{}');
                if (!el) return null;
                const r = el.getBoundingClientRect();
                return JSON.stringify({{x:r.x,y:r.y,width:r.width,height:r.height}});
            }})()",
      selector.replace('\'', "\\'")
    );
    let result = self.evaluate(&js).await?;
    let rect_str = result
      .and_then(|v| v.as_str().map(std::string::ToString::to_string))
      .ok_or_else(|| format!("'{selector}' not found"))?;
    let rect: serde_json::Value = serde_json::from_str(&rect_str).map_err(|e| format!("Parse rect: {e}"))?;

    let format_str = match format {
      ImageFormat::Png => "png",
      ImageFormat::Jpeg => "jpeg",
      ImageFormat::Webp => "webp",
    };

    let result = self
      .cmd(
        "Page.captureScreenshot",
        serde_json::json!({
            "format": format_str,
            "clip": {
                "x": rect["x"], "y": rect["y"],
                "width": rect["width"], "height": rect["height"],
                "scale": 1
            }
        }),
      )
      .await?;
    let data = result
      .get("data")
      .and_then(|v| v.as_str())
      .ok_or("No screenshot data")?;
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data).map_err(|e| format!("Decode: {e}"))
  }

  // ---- Screencast (video recording) ----

  /// Start CDP screencast. Chrome will emit `Page.screencastFrame` events with JPEG data.
  pub async fn start_screencast(
    &self,
    quality: u8,
    max_width: u32,
    max_height: u32,
  ) -> Result<tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, f64)>, String> {
    self
      .cmd(
        "Page.startScreencast",
        serde_json::json!({
          "format": "jpeg",
          "quality": quality,
          "maxWidth": max_width,
          "maxHeight": max_height,
          "everyNthFrame": 1,
        }),
      )
      .await?;

    // Spawn listener that decodes frames and acks them.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    Self::spawn_screencast_listener(self.transport.clone(), self.session_id.clone(), tx);
    Ok(rx)
  }

  /// Stop CDP screencast.
  pub async fn stop_screencast(&self) -> Result<(), String> {
    self.cmd("Page.stopScreencast", serde_json::json!({})).await?;
    Ok(())
  }

  /// Background task: listens for `Page.screencastFrame` events, decodes JPEG, acks, forwards.
  ///
  /// Passes raw JPEG frames to the channel. Frame interpolation (gap-filling) is handled
  /// by the video recorder layer, not here. ACK is sent immediately and non-blocking
  /// (matching Playwright's approach) so Chrome sends the next frame ASAP.
  fn spawn_screencast_listener(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    frame_tx: tokio::sync::mpsc::UnboundedSender<(Vec<u8>, f64)>,
  ) {
    tokio::spawn(async move {
      let mut rx = transport.subscribe_events();
      while let Ok(event) = rx.recv().await {
        // Filter by CDP session.
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }

        if event.get("method").and_then(|m| m.as_str()) != Some("Page.screencastFrame") {
          continue;
        }

        let Some(params) = event.get("params") else { continue };

        // Extract Chrome's frame timestamp (seconds since epoch).
        // Falls back to wall clock if metadata is missing.
        let timestamp = params
          .get("metadata")
          .and_then(|m| m.get("timestamp"))
          .and_then(serde_json::Value::as_f64)
          .unwrap_or_else(|| {
            std::time::SystemTime::now()
              .duration_since(std::time::UNIX_EPOCH)
              .unwrap_or_default()
              .as_secs_f64()
          });

        // Decode base64 JPEG frame data and forward with timestamp.
        if let Some(data_str) = params.get("data").and_then(|v| v.as_str()) {
          if let Ok(jpeg_bytes) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data_str) {
            if frame_tx.send((jpeg_bytes, timestamp)).is_err() {
              break;
            }
          }
        }

        // Acknowledge immediately (non-blocking) so Chrome sends the next frame ASAP.
        let ack_id = params.get("sessionId").and_then(serde_json::Value::as_i64).unwrap_or(0);
        let t = transport.clone();
        let sid = session_id.clone();
        tokio::spawn(async move {
          let _ = t
            .send_command(
              sid.as_deref(),
              "Page.screencastFrameAck",
              serde_json::json!({ "sessionId": ack_id }),
            )
            .await;
        });
      }
    });
  }

  // ---- PDF ----

  pub async fn pdf(&self, landscape: bool, print_background: bool) -> Result<Vec<u8>, String> {
    let result = self
      .cmd(
        "Page.printToPDF",
        serde_json::json!({
            "landscape": landscape,
            "printBackground": print_background,
            "preferCSSPageSize": true,
        }),
      )
      .await?;
    let data = result.get("data").and_then(|v| v.as_str()).ok_or("No PDF data")?;
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data).map_err(|e| format!("Decode PDF: {e}"))
  }

  // ---- File upload ----

  pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<(), String> {
    let doc = self.cmd("DOM.getDocument", super::empty_params()).await?;
    let root_id = doc
      .get("root")
      .and_then(|r| r.get("nodeId"))
      .and_then(serde_json::Value::as_i64)
      .ok_or("No document root")?;

    let query = self
      .cmd(
        "DOM.querySelector",
        serde_json::json!({
            "nodeId": root_id,
            "selector": selector
        }),
      )
      .await?;
    let node_id = query
      .get("nodeId")
      .and_then(serde_json::Value::as_i64)
      .ok_or("Element not found")?;

    let desc = self
      .cmd("DOM.describeNode", serde_json::json!({"nodeId": node_id}))
      .await?;
    let backend_node_id = desc
      .get("node")
      .and_then(|n| n.get("backendNodeId"))
      .and_then(serde_json::Value::as_i64)
      .ok_or("No backendNodeId")?;

    self
      .cmd(
        "DOM.setFileInputFiles",
        serde_json::json!({
            "files": paths,
            "backendNodeId": backend_node_id
        }),
      )
      .await?;
    Ok(())
  }

  // ---- Accessibility ----

  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>, String> {
    self.accessibility_tree_with_depth(-1).await
  }

  pub async fn accessibility_tree_with_depth(&self, depth: i32) -> Result<Vec<AxNodeData>, String> {
    let result = self
      .cmd("Accessibility.getFullAXTree", serde_json::json!({"depth": depth}))
      .await?;

    let nodes = result.get("nodes").and_then(|n| n.as_array()).ok_or("No a11y nodes")?;

    Ok(
      nodes
        .iter()
        .map(|node| {
          let get_ax_value = |field: &str| -> Option<String> {
            node
              .get(field)
              .and_then(|v| v.get("value"))
              .and_then(|v| v.as_str())
              .map(std::string::ToString::to_string)
          };

          let properties = node
            .get("properties")
            .and_then(|p| p.as_array())
            .map(|props| {
              props
                .iter()
                .map(|p| AxProperty {
                  name: p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase(),
                  value: p.get("value").and_then(|v| v.get("value")).cloned(),
                })
                .collect()
            })
            .unwrap_or_default();

          AxNodeData {
            node_id: node.get("nodeId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            parent_id: node
              .get("parentId")
              .and_then(|v| v.as_str())
              .map(std::string::ToString::to_string),
            backend_dom_node_id: node.get("backendDOMNodeId").and_then(serde_json::Value::as_i64),
            ignored: node
              .get("ignored")
              .and_then(serde_json::Value::as_bool)
              .unwrap_or(false),
            role: get_ax_value("role"),
            name: get_ax_value("name"),
            description: get_ax_value("description"),
            properties,
          }
        })
        .collect(),
    )
  }

  // ---- Input ----

  pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
    self.click_at_opts(x, y, "left", 1).await
  }

  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<(), String> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": button, "clickCount": click_count}),
      )
      .await?;
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": button, "clickCount": click_count}),
      )
      .await?;
    Ok(())
  }

  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseMoved", "x": x, "y": y}),
      )
      .await?;
    Ok(())
  }

  pub async fn move_mouse_smooth(
    &self,
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
    steps: u32,
  ) -> Result<(), String> {
    let steps = steps.max(1);
    for i in 0..=steps {
      let t = f64::from(i) / f64::from(steps);
      let ease = t * t * (3.0 - 2.0 * t);
      let x = from_x + (to_x - from_x) * ease;
      let y = from_y + (to_y - from_y) * ease;
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mouseMoved", "x": x, "y": y}),
        )
        .await?;
    }
    Ok(())
  }

  pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64)) -> Result<(), String> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mousePressed", "x": from.0, "y": from.1, "button": "left", "clickCount": 1}),
      )
      .await?;
    let steps = 10u32;
    for i in 1..=steps {
      let t = f64::from(i) / f64::from(steps);
      let ease = t * t * (3.0 - 2.0 * t);
      let x = from.0 + (to.0 - from.0) * ease;
      let y = from.1 + (to.1 - from.1) * ease;
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mouseMoved", "x": x, "y": y, "button": "left"}),
        )
        .await?;
    }
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseReleased", "x": to.0, "y": to.1, "button": "left", "clickCount": 1}),
      )
      .await?;
    Ok(())
  }

  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<(), String> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseWheel", "x": 0, "y": 0, "deltaX": delta_x, "deltaY": delta_y}),
      )
      .await?;
    Ok(())
  }

  pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": button, "clickCount": 1}),
      )
      .await?;
    Ok(())
  }

  pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": button, "clickCount": 1}),
      )
      .await?;
    Ok(())
  }

  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    self.cmd("Input.insertText", serde_json::json!({"text": text})).await?;
    Ok(())
  }

  /// Resolve a Playwright-style key name to (DOM key, virtual keycode, text).
  fn resolve_key(key: &str) -> (&str, u32, Option<&str>) {
    match key {
      "Enter" => ("Enter", 13, Some("\r")),
      "Tab" => ("Tab", 9, Some("\t")),
      "Space" | " " => (" ", 32, Some(" ")),
      "Backspace" => ("Backspace", 8, None),
      "Delete" => ("Delete", 46, None),
      "Escape" => ("Escape", 27, None),
      "ArrowLeft" => ("ArrowLeft", 37, None),
      "ArrowRight" => ("ArrowRight", 39, None),
      "ArrowUp" => ("ArrowUp", 38, None),
      "ArrowDown" => ("ArrowDown", 40, None),
      "Home" => ("Home", 36, None),
      "End" => ("End", 35, None),
      "PageUp" => ("PageUp", 33, None),
      "PageDown" => ("PageDown", 34, None),
      "Shift" | "ShiftLeft" | "ShiftRight" => ("Shift", 16, None),
      "Control" | "ControlLeft" | "ControlRight" => ("Control", 17, None),
      "Alt" | "AltLeft" | "AltRight" => ("Alt", 18, None),
      "Meta" | "MetaLeft" => ("Meta", 91, None),
      "MetaRight" => ("Meta", 93, None),
      "F1" => ("F1", 112, None),
      "F2" => ("F2", 113, None),
      "F3" => ("F3", 114, None),
      "F4" => ("F4", 115, None),
      "F5" => ("F5", 116, None),
      "F6" => ("F6", 117, None),
      "F7" => ("F7", 118, None),
      "F8" => ("F8", 119, None),
      "F9" => ("F9", 120, None),
      "F10" => ("F10", 121, None),
      "F11" => ("F11", 122, None),
      "F12" => ("F12", 123, None),
      ch => (ch, 0, if ch.len() == 1 { Some(ch) } else { None }),
    }
  }

  /// Dispatch a keyDown event for a single key (does NOT release it).
  pub async fn key_down(&self, key: &str) -> Result<(), String> {
    let (dom_key, vk, text) = Self::resolve_key(key);
    let down_type = if text.is_some() { "keyDown" } else { "rawKeyDown" };
    let mut params = serde_json::json!({
        "type": down_type, "key": dom_key,
        "windowsVirtualKeyCode": vk,
    });
    if let Some(t) = text {
      params["text"] = serde_json::json!(t);
    }
    self.cmd("Input.dispatchKeyEvent", params).await?;
    Ok(())
  }

  /// Dispatch a keyUp event for a single key.
  pub async fn key_up(&self, key: &str) -> Result<(), String> {
    let (dom_key, vk, _) = Self::resolve_key(key);
    self
      .cmd(
        "Input.dispatchKeyEvent",
        serde_json::json!({
            "type": "keyUp", "key": dom_key,
            "windowsVirtualKeyCode": vk,
        }),
      )
      .await?;
    Ok(())
  }

  pub async fn press_key(&self, key: &str) -> Result<(), String> {
    self.key_down(key).await?;
    self.key_up(key).await?;
    Ok(())
  }

  // ---- Cookies ----

  pub async fn get_cookies(&self) -> Result<Vec<CookieData>, String> {
    let result = self.cmd("Network.getCookies", super::empty_params()).await?;
    let cookies = result
      .get("cookies")
      .and_then(|c| c.as_array())
      .cloned()
      .unwrap_or_default();
    Ok(
      cookies
        .iter()
        .map(|c| CookieData {
          name: c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          value: c.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          domain: c.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          path: c.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          secure: c.get("secure").and_then(serde_json::Value::as_bool).unwrap_or(false),
          http_only: c.get("httpOnly").and_then(serde_json::Value::as_bool).unwrap_or(false),
          expires: c.get("expires").and_then(serde_json::Value::as_f64),
          same_site: c
            .get("sameSite")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<super::SameSite>().ok()),
        })
        .collect(),
    )
  }

  pub async fn set_cookie(&self, cookie: CookieData) -> Result<(), String> {
    let mut params = serde_json::json!({
        "name": cookie.name,
        "value": cookie.value,
    });
    if !cookie.domain.is_empty() {
      params["domain"] = serde_json::json!(cookie.domain);
    }
    if !cookie.path.is_empty() {
      params["path"] = serde_json::json!(cookie.path);
    }
    params["secure"] = serde_json::json!(cookie.secure);
    params["httpOnly"] = serde_json::json!(cookie.http_only);
    if let Some(e) = cookie.expires {
      params["expires"] = serde_json::json!(e);
    }
    if let Some(ss) = cookie.same_site {
      params["sameSite"] = serde_json::json!(ss.as_str());
    }
    self.cmd("Network.setCookie", params).await?;
    Ok(())
  }

  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<(), String> {
    let mut params = serde_json::json!({"name": name});
    if let Some(d) = domain {
      params["domain"] = serde_json::json!(d);
    } else if let Ok(Some(url)) = self.url().await {
      params["url"] = serde_json::json!(url);
    }
    self.cmd("Network.deleteCookies", params).await?;
    Ok(())
  }

  pub async fn clear_cookies(&self) -> Result<(), String> {
    // Use Network.getCookies + Network.deleteCookies (session-scoped)
    // instead of Storage.clearCookies (browser-scoped) to correctly
    // clear cookies for this page's context.
    let cookies = self.get_cookies().await?;
    for c in &cookies {
      self
        .cmd(
          "Network.deleteCookies",
          serde_json::json!({
            "name": c.name,
            "domain": c.domain,
            "path": c.path,
          }),
        )
        .await?;
    }
    Ok(())
  }

  // ---- Emulation ----

  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
    let is_landscape = config.is_landscape || config.width > config.height;
    let orientation = if config.is_mobile {
      if is_landscape {
        serde_json::json!({"angle": 90, "type": "landscapePrimary"})
      } else {
        serde_json::json!({"angle": 0, "type": "portraitPrimary"})
      }
    } else {
      serde_json::json!({"angle": 0, "type": "landscapePrimary"})
    };
    let params = serde_json::json!({
        "width": config.width,
        "height": config.height,
        "deviceScaleFactor": config.device_scale_factor,
        "mobile": config.is_mobile,
        "screenWidth": config.width,
        "screenHeight": config.height,
        "screenOrientation": orientation,
    });
    self.cmd("Emulation.setDeviceMetricsOverride", params).await?;
    if config.has_touch {
      let _ = self
        .cmd(
          "Emulation.setTouchEmulationEnabled",
          serde_json::json!({"enabled": true}),
        )
        .await;
    }
    Ok(())
  }

  pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
    self
      .cmd("Network.setUserAgentOverride", serde_json::json!({"userAgent": ua}))
      .await?;
    Ok(())
  }

  pub async fn set_geolocation(&self, lat: f64, lng: f64, accuracy: f64) -> Result<(), String> {
    self
      .cmd(
        "Emulation.setGeolocationOverride",
        serde_json::json!({"latitude": lat, "longitude": lng, "accuracy": accuracy}),
      )
      .await?;
    Ok(())
  }

  pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
    let _ = self
      .cmd("Emulation.setLocaleOverride", serde_json::json!({"locale": locale}))
      .await;
    self
      .cmd(
        "Network.setUserAgentOverride",
        serde_json::json!({"userAgent": "", "acceptLanguage": locale}),
      )
      .await?;
    Ok(())
  }

  pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
    self
      .cmd(
        "Emulation.setTimezoneOverride",
        serde_json::json!({"timezoneId": timezone_id}),
      )
      .await?;
    Ok(())
  }

  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<(), String> {
    let mut features = Vec::new();
    if let Some(cs) = &opts.color_scheme {
      features.push(serde_json::json!({"name": "prefers-color-scheme", "value": cs}));
    }
    if let Some(rm) = &opts.reduced_motion {
      features.push(serde_json::json!({"name": "prefers-reduced-motion", "value": rm}));
    }
    if let Some(fc) = &opts.forced_colors {
      features.push(serde_json::json!({"name": "forced-colors", "value": fc}));
    }
    if let Some(c) = &opts.contrast {
      features.push(serde_json::json!({"name": "prefers-contrast", "value": c}));
    }
    let mut params = serde_json::json!({"features": features});
    if let Some(media) = &opts.media {
      params["media"] = serde_json::json!(media);
    }
    self.cmd("Emulation.setEmulatedMedia", params).await?;
    Ok(())
  }

  pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<(), String> {
    self
      .cmd(
        "Emulation.setScriptExecutionDisabled",
        serde_json::json!({"value": !enabled}),
      )
      .await?;
    Ok(())
  }

  pub async fn set_bypass_csp(&self, enabled: bool) -> Result<(), String> {
    self
      .cmd("Page.setBypassCSP", serde_json::json!({"enabled": enabled}))
      .await?;
    Ok(())
  }

  pub async fn set_ignore_certificate_errors(&self, ignore: bool) -> Result<(), String> {
    self
      .cmd(
        "Security.setIgnoreCertificateErrors",
        serde_json::json!({"ignore": ignore}),
      )
      .await?;
    Ok(())
  }

  pub async fn set_download_behavior(&self, behavior: &str, download_path: &str) -> Result<(), String> {
    self
      .cmd(
        "Browser.setDownloadBehavior",
        serde_json::json!({"behavior": behavior, "downloadPath": download_path, "eventsEnabled": true}),
      )
      .await?;
    Ok(())
  }

  pub async fn set_http_credentials(&self, username: &str, password: &str) -> Result<(), String> {
    // Store credentials for Fetch.authRequired event handling.
    // This supports all auth schemes (Basic, Digest, NTLM) — the browser
    // sends the challenge, we respond via Fetch.continueWithAuth.
    *self.http_credentials.write().await = Some((username.to_string(), password.to_string()));
    // Ensure Fetch domain is enabled with auth handling.
    self.ensure_fetch_enabled().await
  }

  pub async fn set_service_workers_blocked(&self, blocked: bool) -> Result<(), String> {
    if blocked {
      self
        .cmd(
          "Page.addScriptToEvaluateOnNewDocument",
          serde_json::json!({
            "source": "if(navigator.serviceWorker){navigator.serviceWorker.register=()=>Promise.reject(new Error('Service workers blocked'))}"
          }),
        )
        .await?;
    }
    Ok(())
  }

  pub async fn set_extra_http_headers(&self, headers: &FxHashMap<String, String>) -> Result<(), String> {
    let h: serde_json::Map<String, serde_json::Value> = headers
      .iter()
      .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
      .collect();
    self
      .cmd("Network.setExtraHTTPHeaders", serde_json::json!({"headers": h}))
      .await?;
    Ok(())
  }

  pub async fn grant_permissions(&self, permissions: &[String], origin: Option<&str>) -> Result<(), String> {
    let mut params = serde_json::json!({"permissions": permissions});
    if let Some(o) = origin {
      params["origin"] = serde_json::json!(o);
    }
    self.cmd("Browser.grantPermissions", params).await?;
    Ok(())
  }

  pub async fn reset_permissions(&self) -> Result<(), String> {
    self.cmd("Browser.resetPermissions", super::empty_params()).await?;
    Ok(())
  }

  pub async fn set_focus_emulation_enabled(&self, enabled: bool) -> Result<(), String> {
    self
      .cmd(
        "Emulation.setFocusEmulationEnabled",
        serde_json::json!({"enabled": enabled}),
      )
      .await?;
    Ok(())
  }

  // ---- Network ----

  pub async fn set_network_state(&self, offline: bool, latency: f64, download: f64, upload: f64) -> Result<(), String> {
    self
      .cmd(
        "Network.emulateNetworkConditions",
        serde_json::json!({
            "offline": offline,
            "latency": latency,
            "downloadThroughput": download,
            "uploadThroughput": upload,
        }),
      )
      .await?;
    Ok(())
  }

  // ---- Tracing ----

  pub async fn start_tracing(&self) -> Result<(), String> {
    self.cmd("Tracing.start", super::empty_params()).await?;
    Ok(())
  }

  pub async fn stop_tracing(&self) -> Result<(), String> {
    self.cmd("Tracing.end", super::empty_params()).await?;
    Ok(())
  }

  pub async fn metrics(&self) -> Result<Vec<MetricData>, String> {
    let result = self.cmd("Performance.getMetrics", super::empty_params()).await?;
    let metrics = result
      .get("metrics")
      .and_then(|m| m.as_array())
      .cloned()
      .unwrap_or_default();
    Ok(
      metrics
        .iter()
        .map(|m| MetricData {
          name: m.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          value: m.get("value").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
        })
        .collect(),
    )
  }

  // ---- Ref resolution ----

  pub async fn resolve_backend_node(&self, backend_node_id: i64, ref_id: &str) -> Result<AnyElement, String> {
    let resolve_result = self
      .cmd("DOM.resolveNode", serde_json::json!({"backendNodeId": backend_node_id}))
      .await?;

    let object_id = resolve_result
      .get("object")
      .and_then(|o| o.get("objectId"))
      .and_then(|v| v.as_str())
      .ok_or_else(|| format!("Ref '{ref_id}' no longer valid."))?;

    let node_id = self
      .cmd("DOM.requestNode", serde_json::json!({"objectId": object_id}))
      .await?
      .get("nodeId")
      .and_then(serde_json::Value::as_i64)
      .ok_or_else(|| format!("Ref '{ref_id}' no longer valid."))?;

    if node_id == 0 {
      return Err(format!("Ref '{ref_id}' no longer valid."));
    }

    Ok(T::wrap_element(CdpElement {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      handles: Arc::new(tokio::sync::Mutex::new(CdpElementHandles {
        node_id: Some(node_id),
        object_id: Some(Arc::from(object_id)),
      })),
    }))
  }

  // ---- Event listeners ----

  pub fn attach_listeners(
    &self,
    console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
    network_log: Arc<RwLock<Vec<NetRequest>>>,
    dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
  ) {
    let transport = self.transport.clone();
    let session_id = self.session_id.clone();
    let emitter1 = self.events.clone();
    let emitter2 = self.events.clone();
    let emitter3 = self.events.clone();

    Self::spawn_console_listener(transport.clone(), session_id.clone(), console_log, emitter1);
    Self::spawn_network_listener(transport.clone(), session_id.clone(), network_log, emitter2);
    Self::spawn_dialog_listener(
      self.transport.clone(),
      self.session_id.clone(),
      self.dialog_handler.clone(),
      dialog_log,
      emitter3,
    );
    Self::spawn_frame_context_tracker(
      self.transport.clone(),
      self.session_id.clone(),
      self.frame_contexts.clone(),
      self.events.clone(),
      self.injected_script.clone(),
    );
  }

  fn spawn_console_listener(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
    emitter: crate::events::EventEmitter,
  ) {
    tokio::spawn(async move {
      let mut rx = transport.subscribe_events();
      while let Ok(event) = rx.recv().await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }

        if event.get("method").and_then(|m| m.as_str()) == Some("Runtime.consoleAPICalled") {
          if let Some(params) = event.get("params") {
            let r#type = params.get("type").and_then(|v| v.as_str()).unwrap_or("log").to_string();
            let text = params
              .get("args")
              .and_then(|a| a.as_array())
              .map(|args| {
                args
                  .iter()
                  .filter_map(|a| a.get("value").map(|v| v.to_string().trim_matches('"').to_string()))
                  .collect::<Vec<_>>()
                  .join(" ")
              })
              .unwrap_or_default();
            let msg = ConsoleMsg { r#type, text };
            console_log.write().await.push(msg.clone());
            emitter.emit(crate::events::PageEvent::Console(msg));
          }
        }
      }
    });
  }

  fn spawn_network_listener(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    network_log: Arc<RwLock<Vec<NetRequest>>>,
    emitter: crate::events::EventEmitter,
  ) {
    tokio::spawn(async move {
      let mut rx = transport.subscribe_events();
      while let Ok(event) = rx.recv().await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }

        let method = event.get("method").and_then(|m| m.as_str()).unwrap_or("");
        match method {
          "Network.requestWillBeSent" => {
            if let Some(params) = event.get("params") {
              let net_req = Self::parse_net_request(params);
              emitter.emit(crate::events::PageEvent::Request(net_req.clone()));
              network_log.write().await.push(net_req);
            }
          },
          "Network.responseReceived" => {
            Self::handle_response_received(&event, &network_log, &emitter).await;
          },
          "Page.downloadWillBegin" => {
            if let Some(params) = event.get("params") {
              let guid = params.get("guid").and_then(|v| v.as_str()).unwrap_or("").to_string();
              let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
              let filename = params
                .get("suggestedFilename")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
              emitter.emit(crate::events::PageEvent::Download(crate::events::DownloadInfo {
                guid,
                url,
                suggested_filename: filename,
              }));
            }
          },
          _ => {},
        }
      }
    });
  }

  fn parse_net_request(params: &serde_json::Value) -> NetRequest {
    let id = params
      .get("requestId")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();
    let req = params.get("request");
    let headers = req
      .and_then(|r| r.get("headers"))
      .and_then(|h| h.as_object())
      .map(|obj| {
        obj
          .iter()
          .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
          .collect()
      });
    let post_data = req
      .and_then(|r| r.get("postData"))
      .and_then(|v| v.as_str())
      .map(std::string::ToString::to_string);
    NetRequest {
      id,
      method: req
        .and_then(|r| r.get("method"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string(),
      url: req
        .and_then(|r| r.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string(),
      resource_type: params.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string(),
      status: None,
      mime_type: None,
      headers,
      post_data,
    }
  }

  async fn handle_response_received(
    event: &serde_json::Value,
    network_log: &Arc<RwLock<Vec<NetRequest>>>,
    emitter: &crate::events::EventEmitter,
  ) {
    if let Some(params) = event.get("params") {
      let rid = params
        .get("requestId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
      let resp = params.get("response");
      let status = resp.and_then(|r| r.get("status")).and_then(serde_json::Value::as_i64);
      let status_text = resp
        .and_then(|r| r.get("statusText"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
      let url = resp
        .and_then(|r| r.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
      let mime = resp
        .and_then(|r| r.get("mimeType"))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
      let resp_headers = resp
        .and_then(|r| r.get("headers"))
        .and_then(|h| h.as_object())
        .map(|obj| {
          obj
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
            .collect()
        });
      let mut reqs = network_log.write().await;
      if let Some(r) = reqs.iter_mut().rev().find(|r| r.id == rid) {
        r.status = status;
        r.mime_type.clone_from(&mime);
      }
      emitter.emit(crate::events::PageEvent::Response(crate::events::NetResponse {
        request_id: rid,
        url,
        status: status.unwrap_or(0),
        status_text,
        mime_type: mime.unwrap_or_default(),
        headers: resp_headers,
      }));
    }
  }

  fn spawn_dialog_listener(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    handler: Arc<tokio::sync::RwLock<crate::events::DialogHandler>>,
    dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
    emitter: crate::events::EventEmitter,
  ) {
    tokio::spawn(async move {
      let mut rx = transport.subscribe_events();
      while let Ok(event) = rx.recv().await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }
        if event.get("method").and_then(|m| m.as_str()) == Some("Page.javascriptDialogOpening") {
          if let Some(params) = event.get("params") {
            let dialog_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("alert");
            let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let default_value = params
              .get("defaultPrompt")
              .and_then(|v| v.as_str())
              .unwrap_or("")
              .to_string();

            let pending = crate::events::PendingDialog {
              dialog_type: dialog_type.to_string(),
              message: message.clone(),
              default_value: default_value.clone(),
            };

            let action = handler.read().await(&pending);
            let (accept, prompt_text) = match &action {
              crate::events::DialogAction::Accept(text) => (true, text.clone()),
              crate::events::DialogAction::Dismiss => (false, None),
            };

            let mut cmd_params = serde_json::json!({"accept": accept});
            if let Some(text) = &prompt_text {
              cmd_params["promptText"] = serde_json::Value::String(text.clone());
            }
            let _ = transport
              .send_command(session_id.as_deref(), "Page.handleJavaScriptDialog", cmd_params)
              .await;

            let action_str = match &action {
              crate::events::DialogAction::Accept(_) => "accepted",
              crate::events::DialogAction::Dismiss => "dismissed",
            };
            dialog_log.write().await.push(crate::state::DialogEvent {
              dialog_type: dialog_type.to_string(),
              message: message.clone(),
              action: action_str.to_string(),
            });

            emitter.emit(crate::events::PageEvent::Dialog(pending));
          }
        }
      }
    });
  }

  fn spawn_frame_context_tracker(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    frame_contexts: Arc<tokio::sync::RwLock<FxHashMap<String, i64>>>,
    emitter: crate::events::EventEmitter,
    injected_script: Arc<InjectedScriptManager>,
  ) {
    tokio::spawn(async move {
      let mut rx = transport.subscribe_events();
      while let Ok(event) = rx.recv().await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }

        let method = event.get("method").and_then(|m| m.as_str()).unwrap_or("");
        match method {
          "Runtime.executionContextCreated" => {
            if let Some(ctx) = event.get("params").and_then(|p| p.get("context")) {
              let ctx_id = ctx.get("id").and_then(serde_json::Value::as_i64).unwrap_or(0);
              if let Some(aux) = ctx.get("auxData") {
                let frame_id = aux.get("frameId").and_then(|v| v.as_str()).unwrap_or("");
                let is_default = aux
                  .get("isDefault")
                  .and_then(serde_json::Value::as_bool)
                  .unwrap_or(false);
                if is_default && !frame_id.is_empty() {
                  frame_contexts.write().await.insert(frame_id.to_string(), ctx_id);
                }
              }
            }
          },
          "Runtime.executionContextDestroyed" => {
            if let Some(ctx_id) = event
              .get("params")
              .and_then(|p| p.get("executionContextId"))
              .and_then(serde_json::Value::as_i64)
            {
              let mut contexts = frame_contexts.write().await;
              contexts.retain(|_, &mut v| v != ctx_id);
            }
          },
          "Runtime.executionContextsCleared" => {
            frame_contexts.write().await.clear();
            injected_script.reset();
          },
          "Page.frameAttached" => {
            if let Some(params) = event.get("params") {
              emitter.emit(crate::events::PageEvent::FrameAttached(super::FrameInfo {
                frame_id: params.get("frameId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                parent_frame_id: params
                  .get("parentFrameId")
                  .and_then(|v| v.as_str())
                  .map(std::string::ToString::to_string),
                name: String::new(),
                url: String::new(),
              }));
            }
          },
          "Page.frameDetached" => {
            if let Some(fid) = event
              .get("params")
              .and_then(|p| p.get("frameId"))
              .and_then(|v| v.as_str())
            {
              frame_contexts.write().await.remove(fid);
              emitter.emit(crate::events::PageEvent::FrameDetached {
                frame_id: fid.to_string(),
              });
            }
          },
          "Page.frameNavigated" => {
            if let Some(frame) = event.get("params").and_then(|p| p.get("frame")) {
              let is_main = frame.get("parentId").is_none();
              if is_main {
                injected_script.reset();
              }
              emitter.emit(crate::events::PageEvent::FrameNavigated(super::FrameInfo {
                frame_id: frame.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                parent_frame_id: frame
                  .get("parentId")
                  .and_then(|v| v.as_str())
                  .map(std::string::ToString::to_string),
                name: frame.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                url: frame.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
              }));
            }
          },
          _ => {},
        }
      }
    });
  }

  // ---- Init Scripts ----

  pub async fn add_init_script(&self, source: &str) -> Result<String, String> {
    let result = self
      .cmd(
        "Page.addScriptToEvaluateOnNewDocument",
        serde_json::json!({"source": source}),
      )
      .await?;
    let id = result
      .get("identifier")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();
    Ok(id)
  }

  pub async fn remove_init_script(&self, identifier: &str) -> Result<(), String> {
    self
      .cmd(
        "Page.removeScriptToEvaluateOnNewDocument",
        serde_json::json!({"identifier": identifier}),
      )
      .await?;
    Ok(())
  }

  // ---- Exposed Functions ----

  /// The JS source for the binding controller injected as an init script.
  pub const BINDING_CONTROLLER_JS: &'static str = r"(function(){
if(globalThis.__fd_bc)return;
var bc={seq:0,cbs:{},fns:{}};
globalThis.__fd_bc=bc;
bc.add=function(name){
  bc.fns[name]=true;
  globalThis[name]=function(){
    var s=++bc.seq;
    var args=[];for(var i=0;i<arguments.length;i++)args.push(arguments[i]);
    var p=new Promise(function(r,j){bc.cbs[s]={r:r,j:j}});
    globalThis.__fd_binding__(JSON.stringify({name:name,seq:s,args:args}));
    return p;
  };
};
bc.del=function(name){delete bc.fns[name];delete globalThis[name]};
bc.resolve=function(seq,val){var c=bc.cbs[seq];if(c){delete bc.cbs[seq];c.r(val)}};
bc.reject=function(seq,err){var c=bc.cbs[seq];if(c){delete bc.cbs[seq];c.j(new Error(err))}};
})()";

  async fn ensure_binding_channel(&self) -> Result<(), String> {
    if self.binding_initialized.swap(true, std::sync::atomic::Ordering::SeqCst) {
      return Ok(());
    }
    self
      .cmd("Runtime.addBinding", serde_json::json!({"name": "__fd_binding__"}))
      .await?;
    self.add_init_script(Self::BINDING_CONTROLLER_JS).await?;
    self.evaluate(Self::BINDING_CONTROLLER_JS).await?;

    let t = self.transport.clone();
    let sid = self.session_id.clone();
    let fns = self.exposed_fns.clone();
    tokio::spawn(async move {
      let mut rx = t.subscribe_events();
      while let Ok(event) = rx.recv().await {
        if let Some(ref expected_sid) = sid {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }
        if event.get("method").and_then(|m| m.as_str()) != Some("Runtime.bindingCalled") {
          continue;
        }
        if let Some(params) = event.get("params") {
          let binding_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
          if binding_name != "__fd_binding__" {
            continue;
          }

          let payload_str = params.get("payload").and_then(|v| v.as_str()).unwrap_or("{}");
          let payload: serde_json::Value = serde_json::from_str(payload_str).unwrap_or_default();
          let fn_name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
          let seq = payload.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(0);
          let args: Vec<serde_json::Value> = payload
            .get("args")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

          let maybe_fn = fns.read().await.get(&fn_name).cloned();
          if let Some(callback) = maybe_fn {
            let result = callback(args);
            let deliver_js = format!(
              "globalThis.__fd_bc.resolve({}, {})",
              seq,
              serde_json::to_string(&result).unwrap_or_else(|_| "null".into())
            );
            let _ = t
              .send_command(
                sid.as_deref(),
                "Runtime.evaluate",
                serde_json::json!({"expression": deliver_js}),
              )
              .await;
          } else {
            let deliver_js = format!("globalThis.__fd_bc.reject({seq}, 'Function not found: {fn_name}')");
            let _ = t
              .send_command(
                sid.as_deref(),
                "Runtime.evaluate",
                serde_json::json!({"expression": deliver_js}),
              )
              .await;
          }
        }
      }
    });
    Ok(())
  }

  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<(), String> {
    self.ensure_binding_channel().await?;
    self.exposed_fns.write().await.insert(name.to_string(), func);
    let register_js = format!("globalThis.__fd_bc.add('{}')", crate::steps::js_escape(name));
    self.add_init_script(&register_js).await?;
    self.evaluate(&register_js).await?;
    Ok(())
  }

  pub async fn remove_exposed_function(&self, name: &str) -> Result<(), String> {
    self.exposed_fns.write().await.remove(name);
    let js = format!(
      "if(globalThis.__fd_bc)globalThis.__fd_bc.del('{}')",
      crate::steps::js_escape(name)
    );
    self.evaluate(&js).await?;
    Ok(())
  }

  // ---- Lifecycle ----

  pub async fn close_page(&self) -> Result<(), String> {
    if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
      return Ok(());
    }
    // Just close the target. Context disposal is handled by context.close() →
    // BrowserState::remove_context() → Target.disposeBrowserContext (one CDP call
    // kills the context + all its pages, matching Playwright's doClose).
    let _ = self
      .transport
      .send_command(
        None,
        "Target.closeTarget",
        serde_json::json!({"targetId": &*self.target_id}),
      )
      .await;
    self.events.emit(crate::events::PageEvent::Close);
    Ok(())
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.closed.load(std::sync::atomic::Ordering::SeqCst)
  }

  // ---- Network Interception ----

  async fn ensure_fetch_enabled(&self) -> Result<(), String> {
    let has_creds = self.http_credentials.read().await.is_some();
    if self.fetch_enabled.swap(true, std::sync::atomic::Ordering::SeqCst) {
      // Already enabled — but may need to re-enable with auth handling.
      if has_creds {
        let _ = self.cmd("Fetch.disable", serde_json::json!({})).await;
        self
          .cmd(
            "Fetch.enable",
            serde_json::json!({
                "patterns": [{"urlPattern": "*", "requestStage": "Request"}],
                "handleAuthRequests": true,
            }),
          )
          .await?;
      }
      return Ok(());
    }
    self
      .cmd(
        "Fetch.enable",
        serde_json::json!({
            "patterns": [{"urlPattern": "*", "requestStage": "Request"}],
            "handleAuthRequests": has_creds,
        }),
      )
      .await?;

    let t = self.transport.clone();
    let sid = self.session_id.clone();
    let routes = self.routes.clone();
    let creds = self.http_credentials.clone();
    tokio::spawn(async move {
      Self::handle_fetch_events(t, sid, routes, creds).await;
    });
    Ok(())
  }

  async fn handle_fetch_events(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    routes: Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
    http_credentials: Arc<tokio::sync::RwLock<Option<(String, String)>>>,
  ) {
    let mut rx = transport.subscribe_events();
    while let Ok(event) = rx.recv().await {
      if let Some(ref expected_sid) = session_id {
        let event_sid = event.get("sessionId").and_then(|v| v.as_str());
        if event_sid != Some(&**expected_sid) {
          continue;
        }
      }
      let method = event.get("method").and_then(|m| m.as_str());

      // ── Handle Fetch.authRequired — respond with stored credentials ──
      if method == Some("Fetch.authRequired") {
        let Some(params) = event.get("params") else { continue };
        let request_id = params.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
        let creds = http_credentials.read().await;
        let response = if let Some((ref user, ref pass)) = *creds {
          serde_json::json!({
            "requestId": request_id,
            "authChallengeResponse": {
              "response": "ProvideCredentials",
              "username": user,
              "password": pass,
            }
          })
        } else {
          serde_json::json!({
            "requestId": request_id,
            "authChallengeResponse": { "response": "CancelAuth" }
          })
        };
        let _ = transport
          .send_command(session_id.as_deref(), "Fetch.continueWithAuth", response)
          .await;
        continue;
      }

      if method != Some("Fetch.requestPaused") {
        continue;
      }
      let Some(params) = event.get("params") else { continue };
      let request_id = params.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
      let req_obj = params.get("request");
      // Borrow URL directly from the JSON event — zero allocation for matching.
      let url = req_obj
        .and_then(|r| r.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

      // Match route BEFORE allocating InterceptedRequest.
      // For non-matching requests (the common case), this is zero-alloc.
      let matched_handler = {
        let routes_guard = routes.read().await;
        routes_guard
          .iter()
          .find(|r| r.matcher.matches(url))
          .map(|r| std::sync::Arc::clone(&r.handler))
      };

      if let Some(handler) = matched_handler {
        // Only parse the full request when a route actually matched.
        let method = req_obj
          .and_then(|r| r.get("method"))
          .and_then(|v| v.as_str())
          .unwrap_or("GET");
        let resource_type = params.get("resourceType").and_then(|v| v.as_str()).unwrap_or("");
        let post_data = req_obj.and_then(|r| r.get("postData")).and_then(|v| v.as_str());
        let headers: FxHashMap<String, String> = req_obj
          .and_then(|r| r.get("headers"))
          .and_then(|h| h.as_object())
          .map(|obj| {
            obj
              .iter()
              .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
              .collect()
          })
          .unwrap_or_default();

        let intercepted = crate::route::InterceptedRequest {
          request_id: request_id.to_string(),
          url: url.to_string(),
          method: method.to_string(),
          headers,
          post_data: post_data.map(str::to_string),
          resource_type: resource_type.to_string(),
        };

        let (tx, rx) = tokio::sync::oneshot::channel();
        let route = crate::route::Route::new(intercepted, tx);
        handler(route);
        let action = rx.await.unwrap_or(crate::route::RouteAction::Continue(
          crate::route::ContinueOverrides::default(),
        ));
        Self::execute_route_action(&transport, session_id.as_deref(), request_id, Some(action)).await;
      } else {
        // No matching route — continue with zero allocation beyond the CDP command.
        let _ = transport
          .send_command(
            session_id.as_deref(),
            "Fetch.continueRequest",
            serde_json::json!({"requestId": request_id}),
          )
          .await;
      }
    }
  }

  async fn execute_route_action(
    transport: &T,
    session_id: Option<&str>,
    request_id: &str,
    action: Option<crate::route::RouteAction>,
  ) {
    match action {
      Some(crate::route::RouteAction::Fulfill(resp)) => {
        let body_b64 = base64::engine::general_purpose::STANDARD.encode(&resp.body);
        let mut hdrs: Vec<serde_json::Value> = resp
          .headers
          .iter()
          .map(|(k, v)| serde_json::json!({"name": k, "value": v}))
          .collect();
        if let Some(ct) = &resp.content_type {
          if !hdrs
            .iter()
            .any(|h| h.get("name").and_then(|n| n.as_str()) == Some("content-type"))
          {
            hdrs.push(serde_json::json!({"name": "content-type", "value": ct}));
          }
        }
        let _ = transport
          .send_command(
            session_id,
            "Fetch.fulfillRequest",
            serde_json::json!({
                "requestId": request_id,
                "responseCode": resp.status,
                "responsePhrase": crate::route::status_text(resp.status),
                "responseHeaders": hdrs,
                "body": body_b64,
            }),
          )
          .await;
      },
      Some(crate::route::RouteAction::Continue(overrides)) => {
        let mut params = serde_json::json!({"requestId": request_id});
        if let Some(url) = &overrides.url {
          params["url"] = serde_json::Value::String(url.clone());
        }
        if let Some(method) = &overrides.method {
          params["method"] = serde_json::Value::String(method.clone());
        }
        if let Some(headers) = &overrides.headers {
          let hdrs: Vec<serde_json::Value> = headers
            .iter()
            .map(|(k, v)| serde_json::json!({"name": k, "value": v}))
            .collect();
          params["headers"] = serde_json::Value::Array(hdrs);
        }
        if let Some(post_data) = &overrides.post_data {
          params["postData"] = serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(post_data));
        }
        let _ = transport
          .send_command(session_id, "Fetch.continueRequest", params)
          .await;
      },
      Some(crate::route::RouteAction::Abort(reason)) => {
        let error_reason = match reason.to_lowercase().as_str() {
          "aborted" => "Aborted",
          "accessdenied" => "AccessDenied",
          "addressunreachable" => "AddressUnreachable",
          "blockedbyclient" => "BlockedByClient",
          "connectionfailed" => "ConnectionFailed",
          "connectionrefused" => "ConnectionRefused",
          "connectionreset" => "ConnectionReset",
          "internetdisconnected" => "InternetDisconnected",
          "namenotresolved" => "NameNotResolved",
          "timedout" => "TimedOut",
          _ => "Failed",
        };
        let _ = transport
          .send_command(
            session_id,
            "Fetch.failRequest",
            serde_json::json!({
                "requestId": request_id,
                "errorReason": error_reason,
            }),
          )
          .await;
      },
      None => {
        let _ = transport
          .send_command(
            session_id,
            "Fetch.continueRequest",
            serde_json::json!({"requestId": request_id}),
          )
          .await;
      },
    }
  }

  pub async fn route(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::route::RouteHandler,
  ) -> Result<(), String> {
    self
      .routes
      .write()
      .await
      .push(crate::route::RegisteredRoute { matcher, handler });
    self.ensure_fetch_enabled().await
  }

  pub async fn unroute(&self, matcher: &crate::url_matcher::UrlMatcher) -> Result<(), String> {
    let mut routes = self.routes.write().await;
    routes.retain(|r| !r.matcher.equivalent(matcher));
    if routes.is_empty() && self.fetch_enabled.load(std::sync::atomic::Ordering::SeqCst) {
      self.fetch_enabled.store(false, std::sync::atomic::Ordering::SeqCst);
      let _ = self.cmd("Fetch.disable", serde_json::json!({})).await;
    }
    Ok(())
  }
}

// ---- CdpElement<T> ---------------------------------------------------------

pub struct CdpElement<T: CdpTransport> {
  transport: Arc<T>,
  session_id: Option<Arc<str>>,
  handles: Arc<tokio::sync::Mutex<CdpElementHandles>>,
}

struct CdpElementHandles {
  node_id: Option<i64>,
  object_id: Option<Arc<str>>,
}

impl<T: CdpTransport> Clone for CdpElement<T> {
  fn clone(&self) -> Self {
    Self {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      handles: self.handles.clone(),
    }
  }
}

impl<T: CdpTransport> CdpElement<T> {
  async fn cmd(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    self
      .transport
      .send_command(self.session_id.as_deref(), method, params)
      .await
  }

  async fn resolve_node_id_from_object(&self, object_id: &str) -> Result<i64, String> {
    let node_result = self
      .cmd("DOM.requestNode", serde_json::json!({"objectId": object_id}))
      .await?;
    let node_id = node_result
      .get("nodeId")
      .and_then(serde_json::Value::as_i64)
      .ok_or("Could not resolve element nodeId")?;
    if node_id == 0 {
      return Err("Element not found".into());
    }
    Ok(node_id)
  }

  async fn resolve_object_id_from_node(&self, node_id: i64) -> Result<Arc<str>, String> {
    let resolved = self
      .cmd("DOM.resolveNode", serde_json::json!({"nodeId": node_id}))
      .await?;
    resolved
      .get("object")
      .and_then(|o| o.get("objectId"))
      .and_then(|v| v.as_str())
      .map(Arc::from)
      .ok_or("Cannot resolve element".into())
  }

  async fn node_id(&self) -> Result<i64, String> {
    let object_id = {
      let handles = self.handles.lock().await;
      if let Some(node_id) = handles.node_id {
        return Ok(node_id);
      }
      handles.object_id.clone()
    };

    let Some(object_id) = object_id else {
      return Err("Element handle has neither nodeId nor objectId".into());
    };
    let node_id = self.resolve_node_id_from_object(&object_id).await?;
    let mut handles = self.handles.lock().await;
    handles.node_id = Some(node_id);
    Ok(node_id)
  }

  async fn object_id(&self) -> Result<Arc<str>, String> {
    let node_id = {
      let handles = self.handles.lock().await;
      if let Some(object_id) = &handles.object_id {
        return Ok(object_id.clone());
      }
      handles.node_id
    };

    let Some(node_id) = node_id else {
      return Err("Element handle has neither nodeId nor objectId".into());
    };
    let object_id = self.resolve_object_id_from_node(node_id).await?;
    let mut handles = self.handles.lock().await;
    handles.object_id = Some(object_id.clone());
    Ok(object_id)
  }

  /// Get element center coordinates for clicking.
  async fn get_center(&self) -> Result<(f64, f64), String> {
    let node_id = self.node_id().await?;
    let result = self
      .cmd("DOM.getBoxModel", serde_json::json!({"nodeId": node_id}))
      .await?;
    let content = result
      .get("model")
      .and_then(|m| m.get("content"))
      .and_then(|c| c.as_array())
      .ok_or("No box model")?;

    if content.len() < 8 {
      return Err("Invalid box model".into());
    }
    let x1 = content[0].as_f64().unwrap_or(0.0);
    let y1 = content[1].as_f64().unwrap_or(0.0);
    let x3 = content[4].as_f64().unwrap_or(0.0);
    let y3 = content[5].as_f64().unwrap_or(0.0);

    Ok((f64::midpoint(x1, x3), f64::midpoint(y1, y3)))
  }

  pub async fn call_js_fn_value(&self, function: &str) -> Result<Option<serde_json::Value>, String> {
    let object_id = self.object_id().await?;
    let result = self
      .cmd(
        "Runtime.callFunctionOn",
        serde_json::json!({
            "objectId": &*object_id,
            "functionDeclaration": function,
            "returnByValue": true,
        }),
      )
      .await?;
    Ok(result.get("result").and_then(|r| r.get("value")).cloned())
  }

  pub async fn click(&self) -> Result<(), String> {
    let center = self.call_js_fn_value(
            "function() { this.scrollIntoViewIfNeeded(); var r = this.getBoundingClientRect(); return {x: r.x + r.width/2, y: r.y + r.height/2}; }"
        ).await?;

    if let Some(c) = center {
      let x = c.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      let y = c.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      if x == 0.0 && y == 0.0 {
        return self.call_js_fn("function() { this.click(); }").await;
      }
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1}),
        )
        .await?;
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1}),
        )
        .await?;
      Ok(())
    } else {
      self.call_js_fn("function() { this.click(); }").await
    }
  }

  pub async fn dblclick(&self) -> Result<(), String> {
    let center = self.call_js_fn_value(
            "function() { this.scrollIntoViewIfNeeded(); var r = this.getBoundingClientRect(); return {x: r.x + r.width/2, y: r.y + r.height/2}; }"
        ).await?;

    if let Some(c) = center {
      let x = c.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      let y = c.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      if x == 0.0 && y == 0.0 {
        return self
          .call_js_fn("function() { this.dispatchEvent(new MouseEvent('dblclick', {bubbles:true})); }")
          .await;
      }
      // First click (clickCount=1) fires 'click'
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1}),
        )
        .await?;
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1}),
        )
        .await?;
      // Second click (clickCount=2) fires 'dblclick'
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 2}),
        )
        .await?;
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 2}),
        )
        .await?;
      Ok(())
    } else {
      self
        .call_js_fn("function() { this.dispatchEvent(new MouseEvent('dblclick', {bubbles:true})); }")
        .await
    }
  }

  pub async fn hover(&self) -> Result<(), String> {
    self.scroll_into_view().await?;
    let (x, y) = self.get_center().await?;
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseMoved", "x": x, "y": y}),
      )
      .await?;
    Ok(())
  }

  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    self.click().await?;
    self.cmd("Input.insertText", serde_json::json!({"text": text})).await?;
    Ok(())
  }

  pub async fn call_js_fn(&self, function: &str) -> Result<(), String> {
    let object_id = self.object_id().await?;
    self
      .cmd(
        "Runtime.callFunctionOn",
        serde_json::json!({
            "objectId": &*object_id,
            "functionDeclaration": function,
        }),
      )
      .await?;
    Ok(())
  }

  pub async fn scroll_into_view(&self) -> Result<(), String> {
    let node_id = self.node_id().await?;
    self
      .cmd("DOM.scrollIntoViewIfNeeded", serde_json::json!({"nodeId": node_id}))
      .await?;
    Ok(())
  }

  pub async fn screenshot(&self, format: ImageFormat) -> Result<Vec<u8>, String> {
    let node_id = self.node_id().await?;
    let result = self
      .cmd("DOM.getBoxModel", serde_json::json!({"nodeId": node_id}))
      .await?;
    let content = result
      .get("model")
      .and_then(|m| m.get("content"))
      .and_then(|c| c.as_array())
      .ok_or("No box model")?;

    if content.len() < 8 {
      return Err("Invalid box model".into());
    }

    let x = content[0].as_f64().unwrap_or(0.0);
    let y = content[1].as_f64().unwrap_or(0.0);
    let w = content[4].as_f64().unwrap_or(0.0) - x;
    let h = content[5].as_f64().unwrap_or(0.0) - y;

    let fmt = match format {
      ImageFormat::Png => "png",
      ImageFormat::Jpeg => "jpeg",
      ImageFormat::Webp => "webp",
    };

    let result = self
      .cmd(
        "Page.captureScreenshot",
        serde_json::json!({
            "format": fmt,
            "clip": {"x": x, "y": y, "width": w, "height": h, "scale": 1}
        }),
      )
      .await?;
    let data = result
      .get("data")
      .and_then(|v| v.as_str())
      .ok_or("No screenshot data")?;
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data).map_err(|e| format!("Decode: {e}"))
  }
}
