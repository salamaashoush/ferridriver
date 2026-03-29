//! CDP Pipe backend -- Chrome `DevTools` Protocol over pipes (fd 3/4).
//!
//! Uses `--remote-debugging-pipe` flag to communicate with Chrome via
//! file descriptors instead of WebSocket. No port discovery, no handshake,
//! no framing overhead -- just NUL-delimited JSON over Unix pipes.
//!
//! Navigation follows Bun's ChromeBackend.cpp architecture: register a oneshot
//! waiter before sending Page.navigate, then await the waiter which resolves
//! when the reader task sees Page.loadEventFired for that session.

mod json_scan;
mod transport;

use base64::Engine as _;

use super::{
  AnyElement, AnyPage, Arc, AxNodeData, AxProperty, ConsoleMsg, CookieData, ImageFormat, MetricData, NetRequest,
  RwLock, ScreenshotOpts,
};
use rustc_hash::FxHashMap;
use std::time::Duration;
use transport::PipeTransport;

// ---- CdpPipeBrowser --------------------------------------------------------

pub struct CdpPipeBrowser {
  transport: Arc<PipeTransport>,
  child: tokio::process::Child,
  /// Track targetId -> sessionId for already-attached targets.
  attached_targets: std::sync::Mutex<FxHashMap<String, Option<String>>>,
}

impl CdpPipeBrowser {
  /// Enable required CDP domains on a session so events and queries work.
  async fn enable_domains(transport: &PipeTransport, session_id: Option<&str>) -> Result<(), String> {
    let ep = super::empty_params();
    let engine_js = crate::selectors::build_inject_js();
    let (r1, r2, r3, r4, r5, r6, r7) = tokio::join!(
      transport.send_command(session_id, "Page.enable", ep.clone()),
      transport.send_command(session_id, "Runtime.enable", ep.clone()),
      transport.send_command(session_id, "DOM.enable", ep.clone()),
      transport.send_command(session_id, "Network.enable", ep.clone()),
      transport.send_command(session_id, "Accessibility.enable", ep.clone()),
      transport.send_command(session_id, "Page.setLifecycleEventsEnabled", serde_json::json!({"enabled": true})),
      transport.send_command(session_id, "Page.addScriptToEvaluateOnNewDocument", serde_json::json!({"source": engine_js})),
    );
    r1?; r2?; r3?; r4?; r5?; r6?; r7?;
    Ok(())
  }

  /// Launch Chrome with `--remote-debugging-pipe` and communicate over fd 3/4.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome cannot be spawned, the pipe transport fails to
  /// initialize, or the initial target/session setup commands fail.
  pub async fn launch(chromium_path: &str) -> Result<Self, String> {
    Self::launch_with_flags(chromium_path, &crate::state::chrome_flags(true, &[])).await
  }

  /// Launch Chrome with custom flags and communicate over fd 3/4.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome cannot be spawned with the given flags, the pipe
  /// transport fails to initialize, or the initial target/session setup commands fail.
  pub async fn launch_with_flags(chromium_path: &str, flags: &[String]) -> Result<Self, String> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let user_data_dir = std::env::temp_dir().join(format!("ferridriver-pipe-{}-{id}", std::process::id()));

    let (transport, child) = PipeTransport::spawn(chromium_path, &user_data_dir, flags)?;
    let transport = Arc::new(transport);

    // Enable target discovery so we get notified about new targets
    transport
      .send_command(None, "Target.setDiscoverTargets", serde_json::json!({"discover": true}))
      .await?;

    // With --no-startup-window, Chrome won't create a default page target.
    // Create our own initial page target.
    let create_result = transport
      .send_command(None, "Target.createTarget", serde_json::json!({"url": "about:blank"}))
      .await?;

    let target_id = create_result
      .get("targetId")
      .and_then(|v| v.as_str())
      .ok_or("No targetId from Target.createTarget")?
      .to_string();

    // Attach to the target to get a session ID
    let attach_result = transport
      .send_command(
        None,
        "Target.attachToTarget",
        serde_json::json!({"targetId": target_id, "flatten": true}),
      )
      .await?;

    let session_id = attach_result
      .get("sessionId")
      .and_then(|v| v.as_str())
      .map(std::string::ToString::to_string);

    // Enable required domains on the new session
    Self::enable_domains(&transport, session_id.as_deref()).await?;

    let mut attached = FxHashMap::default();
    attached.insert(target_id, session_id.clone());

    Ok(Self {
      transport,
      child,
      attached_targets: std::sync::Mutex::new(attached),
    })
  }

  /// Retrieve all open page targets, attaching to any not yet tracked.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Target.getTargets` CDP command fails, or if
  /// attaching to an untracked target or enabling domains on it fails.
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
        // Already attached, reuse the session
        sid
      } else {
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

        // Track it as attached with its session
        self
          .attached_targets
          .lock()
          .map_err(|e| format!("Lock poisoned: {e}"))?
          .insert(target_id.clone(), sid.clone());

        // Enable domains on the new session
        Self::enable_domains(&self.transport, sid.as_deref()).await?;

        sid
      };

      pages.push(AnyPage::CdpPipe(CdpPipePage {
        transport: self.transport.clone(),
        session_id: sid,
        target_id,
        events: crate::events::EventEmitter::new(),
        frame_contexts: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
        dialog_handler: Arc::new(tokio::sync::RwLock::new(crate::events::default_dialog_handler())),
        exposed_fns: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
        binding_initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        routes: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        fetch_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      }));
    }
    Ok(pages)
  }

  /// Create a new page target, attach to it, and optionally navigate to a URL.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Target.createTarget` or `Target.attachToTarget` CDP
  /// command fails, if enabling domains on the new session fails, or if the
  /// subsequent navigation (when `url` is not `about:blank`) fails.
  pub async fn new_page(&self, url: &str) -> Result<AnyPage, String> {
    // Create target with about:blank initially so we can set up domains before navigation
    let result = self
      .transport
      .send_command(None, "Target.createTarget", serde_json::json!({"url": "about:blank"}))
      .await?;

    let target_id = result
      .get("targetId")
      .and_then(|v| v.as_str())
      .ok_or("No targetId")?
      .to_string();

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

    // Track as attached
    self
      .attached_targets
      .lock()
      .map_err(|e| format!("Lock poisoned: {e}"))?
      .insert(target_id.clone(), sid.clone());

    // Enable domains BEFORE any navigation
    Self::enable_domains(&self.transport, sid.as_deref()).await?;

    let page = CdpPipePage {
      transport: self.transport.clone(),
      session_id: sid,
      target_id,
      events: crate::events::EventEmitter::new(),
      frame_contexts: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
      dialog_handler: Arc::new(tokio::sync::RwLock::new(crate::events::default_dialog_handler())),
      exposed_fns: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
      binding_initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      routes: Arc::new(tokio::sync::RwLock::new(Vec::new())),
      fetch_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    // Navigate if a real URL was requested (not about:blank)
    if url != "about:blank" && !url.is_empty() {
      page.goto(url, crate::backend::NavLifecycle::Load, 30_000).await?;
    }

    Ok(AnyPage::CdpPipe(page))
  }

  /// Create a new page in an isolated browser context (separate cookies/storage).
  ///
  /// # Errors
  ///
  /// Returns an error if creating the browser context, target, or session fails,
  /// if enabling domains on the session fails, or if navigation fails.
  pub async fn new_page_isolated(&self, url: &str) -> Result<AnyPage, String> {
    // Create isolated browser context
    let ctx = self
      .transport
      .send_command(None, "Target.createBrowserContext", super::empty_params())
      .await?;

    let ctx_id = ctx
      .get("browserContextId")
      .and_then(|v| v.as_str())
      .ok_or("No browserContextId")?
      .to_string();

    // Create target in the isolated context, starting with about:blank
    let result = self
      .transport
      .send_command(
        None,
        "Target.createTarget",
        serde_json::json!({"url": "about:blank", "browserContextId": ctx_id}),
      )
      .await?;

    let target_id = result
      .get("targetId")
      .and_then(|v| v.as_str())
      .ok_or("No targetId")?
      .to_string();

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

    // Track as attached
    self
      .attached_targets
      .lock()
      .map_err(|e| format!("Lock poisoned: {e}"))?
      .insert(target_id.clone(), sid.clone());

    // Enable domains BEFORE any navigation
    Self::enable_domains(&self.transport, sid.as_deref()).await?;

    let page = CdpPipePage {
      transport: self.transport.clone(),
      session_id: sid,
      target_id,
      events: crate::events::EventEmitter::new(),
      frame_contexts: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
      dialog_handler: Arc::new(tokio::sync::RwLock::new(crate::events::default_dialog_handler())),
      exposed_fns: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
      binding_initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      routes: Arc::new(tokio::sync::RwLock::new(Vec::new())),
      fetch_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    // Navigate if a real URL was requested
    if url != "about:blank" && !url.is_empty() {
      page.goto(url, crate::backend::NavLifecycle::Load, 30_000).await?;
    }

    Ok(AnyPage::CdpPipe(page))
  }

  /// Close the browser process and release resources.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Browser.close` CDP command cannot be sent (though
  /// the browser process is killed regardless).
  pub async fn close(&mut self) -> Result<(), String> {
    let _ = self
      .transport
      .send_command(None, "Browser.close", super::empty_params())
      .await;
    let _ = self.child.kill().await;
    Ok(())
  }
}

// ---- CdpPipePage ------------------------------------------------------------

#[derive(Clone)]
pub struct CdpPipePage {
  transport: Arc<PipeTransport>,
  session_id: Option<String>,
  target_id: String,
  /// Event emitter for page events (console, dialog, network, frame lifecycle).
  pub events: crate::events::EventEmitter,
  /// Frame ID -> execution context ID mapping for frame-scoped evaluation.
  frame_contexts: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, i64>>>,
  /// Configurable dialog handler. Called when a JS dialog appears.
  pub dialog_handler: Arc<tokio::sync::RwLock<crate::events::DialogHandler>>,
  /// Registered exposed function callbacks.
  pub exposed_fns: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, crate::events::ExposedFn>>>,
  /// Whether the binding channel has been initialized.
  binding_initialized: Arc<std::sync::atomic::AtomicBool>,
  /// Whether this page has been closed.
  closed: Arc<std::sync::atomic::AtomicBool>,
  /// Registered route handlers for network interception.
  routes: Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
  /// Whether Fetch domain is enabled for interception.
  fetch_enabled: Arc<std::sync::atomic::AtomicBool>,
}

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

impl CdpPipePage {
  /// Send a CDP command to this page's session.
  async fn cmd(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    self
      .transport
      .send_command(self.session_id.as_deref(), method, params)
      .await
  }

  // ---- Navigation ----

  /// Navigate the page to the given URL and wait for the load event.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Page.navigate` CDP command fails or if Chrome
  /// reports a navigation error (e.g. DNS resolution failure).
  pub async fn goto(
    &self, url: &str, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64,
  ) -> Result<(), String> {
    let rx = self
      .transport
      .register_nav_waiter(self.session_id.as_deref().unwrap_or(""), lifecycle)
      .await;

    let nav_result = self.cmd("Page.navigate", serde_json::json!({"url": url})).await?;

    if let Some(error_text) = nav_result.get("errorText").and_then(|v| v.as_str()) {
      if !error_text.is_empty() {
        return Err(format!("Navigation failed: {error_text}"));
      }
    }

    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await {
      Ok(Ok(result)) => result,
      Ok(Err(_)) | Err(_) => Ok(()),
    }
  }

  /// Wait for the next navigation lifecycle event (load event fired).
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation waiter channel is dropped before
  /// receiving a result.
  pub async fn wait_for_navigation(&self) -> Result<(), String> {
    // Register nav waiter and await Page.loadEventFired (Bun's pattern)
    let rx = self
      .transport
      .register_nav_waiter(self.session_id.as_deref().unwrap_or(""), crate::backend::NavLifecycle::Load)
      .await;

    match tokio::time::timeout(Duration::from_secs(30), rx).await {
      Ok(Ok(result)) => result,
      Ok(Err(_)) => Err("Navigation waiter dropped".into()),
      Err(_) => Ok(()), // Timeout, proceed anyway
    }
  }

  /// Reload the current page.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Page.reload` CDP command fails.
  pub async fn reload(
    &self, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64,
  ) -> Result<(), String> {
    let rx = self
      .transport
      .register_nav_waiter(self.session_id.as_deref().unwrap_or(""), lifecycle)
      .await;
    self.cmd("Page.reload", super::empty_params()).await?;
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await {
      Ok(Ok(r)) => r,
      _ => Ok(()),
    }
  }

  pub async fn go_back(
    &self, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64,
  ) -> Result<(), String> {
    self.history_go(-1, lifecycle, timeout_ms).await
  }

  pub async fn go_forward(
    &self, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64,
  ) -> Result<(), String> {
    self.history_go(1, lifecycle, timeout_ms).await
  }

  async fn history_go(
    &self, delta: i32, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64,
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
      .register_nav_waiter(self.session_id.as_deref().unwrap_or(""), lifecycle)
      .await;
    self
      .cmd("Page.navigateToHistoryEntry", serde_json::json!({"entryId": entry_id}))
      .await?;
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await {
      Ok(Ok(r)) => r,
      _ => Ok(()),
    }
  }

  /// Get the current page URL via `location.href`.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Runtime.evaluate` CDP command fails.
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

  /// Get the current page title via `document.title`.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Runtime.evaluate` CDP command fails.
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

  /// Evaluate a JavaScript expression and return the result value.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Runtime.evaluate` CDP command fails or if the
  /// expression throws a JavaScript exception.
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

  /// Get the frame tree for this page, returning all frames and their metadata.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Page.getFrameTree` CDP command fails.
  pub async fn get_frame_tree(&self) -> Result<Vec<super::FrameInfo>, String> {
    let result = self.cmd("Page.getFrameTree", super::empty_params()).await?;

    let mut frames = Vec::new();
    if let Some(tree) = result.get("frameTree") {
      collect_frames(tree, &mut frames);
    }
    Ok(frames)
  }

  /// Evaluate a JavaScript expression in a specific frame's execution context.
  ///
  /// # Errors
  ///
  /// Returns an error if no execution context is found for the given frame ID,
  /// if the `Runtime.evaluate` CDP command fails, or if the expression throws.
  pub async fn evaluate_in_frame(&self, expression: &str, frame_id: &str) -> Result<Option<serde_json::Value>, String> {
    // Look up the execution context ID for this frame
    let context_id = {
      let contexts = self.frame_contexts.read().await;
      contexts.get(frame_id).copied()
    };

    if let Some(ctx_id) = context_id {
      // Evaluate in the specific frame's execution context
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
      // Fallback: try to find the frame's context by getting the frame tree first
      // and using the frame's URL to identify it
      Err(format!(
        "No execution context found for frame '{frame_id}'. Frame may not be loaded yet."
      ))
    }
  }

  // ---- Elements ----

  /// Find a DOM element by CSS selector, returning a handle for further interaction.
  ///
  /// # Errors
  ///
  /// Returns an error if the document root cannot be obtained, the CSS selector
  /// is invalid, or no element matches the selector.
  pub async fn find_element(&self, selector: &str) -> Result<AnyElement, String> {
    // Get a fresh document root each time, since nodeIds get invalidated
    // after navigation or DOM changes.
    let doc = self.cmd("DOM.getDocument", serde_json::json!({"depth": 0})).await?;
    let root_id = doc
      .get("root")
      .and_then(|r| r.get("nodeId"))
      .and_then(serde_json::Value::as_i64)
      .ok_or_else(|| "No document root".to_string())?;

    // Query selector
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

    Ok(AnyElement::CdpPipe(CdpPipeElement {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      node_id,
    }))
  }

  /// Evaluate JS that returns a DOM element. Uses Runtime.evaluate without
  /// returnByValue to get an objectId, then DOM.requestNode for the nodeId.
  /// Single evaluate + one DOM call = 2 round-trips (vs 5 for tag-and-query).
  ///
  /// # Errors
  ///
  /// Returns an error if the JS expression does not return a DOM element,
  /// or if resolving the element's node ID fails.
  pub async fn evaluate_to_element(&self, js: &str) -> Result<AnyElement, String> {
    // Ensure DOM agent has the document tree (required for DOM.requestNode)
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

    Ok(AnyElement::CdpPipe(CdpPipeElement {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      node_id,
    }))
  }

  // ---- Content ----

  /// Get the full HTML content of the page (`document.documentElement.outerHTML`).
  ///
  /// # Errors
  ///
  /// Returns an error if the `Runtime.evaluate` CDP command fails.
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

  /// Replace the entire page content with the given HTML string.
  ///
  /// # Errors
  ///
  /// Returns an error if the frame tree cannot be retrieved or if the
  /// `Page.setDocumentContent` CDP command fails.
  pub async fn set_content(&self, html: &str) -> Result<(), String> {
    // Get the frame tree to find the main frame ID
    let tree = self.cmd("Page.getFrameTree", super::empty_params()).await?;
    let frame_id = tree
      .get("frameTree")
      .and_then(|f| f.get("frame"))
      .and_then(|f| f.get("id"))
      .and_then(|v| v.as_str())
      .ok_or("No main frame")?;

    // Embed the selector engine directly in the HTML as a <script> tag.
    // This avoids a separate evaluate round-trip after setDocumentContent.
    let engine_js = crate::selectors::build_inject_js();
    let augmented = format!("<script>{engine_js}</script>{html}");
    self
      .cmd(
        "Page.setDocumentContent",
        serde_json::json!({"frameId": frame_id, "html": augmented}),
      )
      .await?;
    Ok(())
  }

  // ---- Screenshots ----

  /// Capture a screenshot of the page with the given options.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Page.captureScreenshot` or `Page.getLayoutMetrics`
  /// CDP commands fail, or if the base64-encoded image data cannot be decoded.
  pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>, String> {
    let format_str = match opts.format {
      ImageFormat::Png => "png",
      ImageFormat::Jpeg => "jpeg",
      ImageFormat::Webp => "webp",
    };
    let mut params = serde_json::json!({"format": format_str, "optimizeForSpeed": true});
    if let Some(q) = opts.quality {
      params["quality"] = serde_json::json!(q);
    }
    if opts.full_page {
      // Get full page dimensions
      let metrics = self.cmd("Page.getLayoutMetrics", super::empty_params()).await?;
      if let Some(content_size) = metrics.get("contentSize") {
        let w = content_size
          .get("width")
          .and_then(serde_json::Value::as_f64)
          .unwrap_or(800.0);
        let h = content_size
          .get("height")
          .and_then(serde_json::Value::as_f64)
          .unwrap_or(600.0);
        params["clip"] = serde_json::json!({
            "x": 0, "y": 0, "width": w, "height": h, "scale": 1
        });
      }
    }

    let result = self.cmd("Page.captureScreenshot", params).await?;
    let data = result
      .get("data")
      .and_then(|v| v.as_str())
      .ok_or("No screenshot data")?;
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
      .map_err(|e| format!("Decode screenshot: {e}"))
  }

  /// Capture a screenshot of a specific element identified by CSS selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found, its bounding rect cannot
  /// be computed, the screenshot CDP command fails, or base64 decoding fails.
  pub async fn screenshot_element(&self, selector: &str, format: ImageFormat) -> Result<Vec<u8>, String> {
    // Get element bounding box via JS
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

  // ---- PDF ----

  /// Print the page to PDF and return the raw PDF bytes.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Page.printToPDF` CDP command fails or if
  /// the base64-encoded PDF data cannot be decoded.
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

  /// Set files on a file input element identified by CSS selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the document root cannot be obtained, the element
  /// is not found, or the `DOM.setFileInputFiles` CDP command fails.
  pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<(), String> {
    // Get document root
    let doc = self.cmd("DOM.getDocument", super::empty_params()).await?;
    let root_id = doc
      .get("root")
      .and_then(|r| r.get("nodeId"))
      .and_then(serde_json::Value::as_i64)
      .ok_or("No document root")?;

    // Query for element
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

    // Get backendNodeId
    let desc = self
      .cmd("DOM.describeNode", serde_json::json!({"nodeId": node_id}))
      .await?;
    let backend_node_id = desc
      .get("node")
      .and_then(|n| n.get("backendNodeId"))
      .and_then(serde_json::Value::as_i64)
      .ok_or("No backendNodeId")?;

    // Set files
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

  /// Get the full accessibility tree for the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Accessibility.getFullAXTree` CDP command fails.
  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>, String> {
    self.accessibility_tree_with_depth(-1).await
  }

  /// Get the accessibility tree up to a maximum depth (-1 for unlimited).
  ///
  /// # Errors
  ///
  /// Returns an error if the `Accessibility.getFullAXTree` CDP command fails
  /// or returns no nodes.
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

  /// Click at absolute page coordinates with the left mouse button.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Input.dispatchMouseEvent` CDP command fails.
  pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
    self.click_at_opts(x, y, "left", 1).await
  }

  /// Click at absolute page coordinates with configurable button and click count.
  ///
  /// # Errors
  ///
  /// Returns an error if either the mousePressed or mouseReleased dispatch fails.
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

  /// Move the mouse cursor to absolute page coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Input.dispatchMouseEvent` CDP command fails.
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseMoved", "x": x, "y": y}),
      )
      .await?;
    Ok(())
  }

  /// Smoothly move the mouse from one point to another with easing.
  ///
  /// # Errors
  ///
  /// Returns an error if any intermediate `Input.dispatchMouseEvent` CDP
  /// command fails.
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

  /// Click and drag from one point to another with smooth interpolation.
  ///
  /// # Errors
  ///
  /// Returns an error if any mouse press, move, or release CDP command fails.
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

  /// Dispatch a mouse wheel scroll event.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Input.dispatchMouseEvent` CDP command fails.
  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<(), String> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseWheel", "x": 0, "y": 0, "deltaX": delta_x, "deltaY": delta_y}),
      )
      .await?;
    Ok(())
  }

  /// Press the mouse button down at the given coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Input.dispatchMouseEvent` CDP command fails.
  pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": button, "clickCount": 1}),
      )
      .await?;
    Ok(())
  }

  /// Release the mouse button at the given coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Input.dispatchMouseEvent` CDP command fails.
  pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": button, "clickCount": 1}),
      )
      .await?;
    Ok(())
  }

  /// Type a string character by character using key events.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Input.insertText` CDP command fails.
  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    self.cmd("Input.insertText", serde_json::json!({"text": text})).await?;
    Ok(())
  }

  /// Press and release a named key (e.g. "Enter", "Tab", "`ArrowLeft`").
  ///
  /// # Errors
  ///
  /// Returns an error if the key-down or key-up `Input.dispatchKeyEvent`
  /// CDP command fails.
  pub async fn press_key(&self, key: &str) -> Result<(), String> {
    // Port of Bun's cdpKeyInfo table: map key names to DOM key string,
    // Windows VK code, and text character. Text-producing keys use "keyDown",
    // control keys use "rawKeyDown".
    let (dom_key, vk, text) = match key {
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
      ch => (ch, 0, if ch.len() == 1 { Some(ch) } else { None }),
    };

    let down_type = if text.is_some() { "keyDown" } else { "rawKeyDown" };
    let mut down_params = serde_json::json!({
        "type": down_type, "key": dom_key,
        "windowsVirtualKeyCode": vk,
    });
    if let Some(t) = text {
      down_params["text"] = serde_json::json!(t);
    }

    self.cmd("Input.dispatchKeyEvent", down_params).await?;
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

  // ---- Cookies ----

  /// Get all cookies for the current page.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Network.getCookies` CDP command fails.
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
        })
        .collect(),
    )
  }

  /// Set a cookie with the given parameters.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Network.setCookie` CDP command fails.
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
    self.cmd("Network.setCookie", params).await?;
    Ok(())
  }

  /// Delete a cookie by name, optionally scoped to a domain.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Network.deleteCookies` CDP command fails.
  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<(), String> {
    let mut params = serde_json::json!({"name": name});
    if let Some(d) = domain {
      params["domain"] = serde_json::json!(d);
    } else {
      // Chrome requires at least url or domain for Network.deleteCookies
      if let Ok(Some(url)) = self.url().await {
        params["url"] = serde_json::json!(url);
      }
    }
    self.cmd("Network.deleteCookies", params).await?;
    Ok(())
  }

  /// Clear all browser cookies.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Storage.clearCookies` CDP command fails.
  pub async fn clear_cookies(&self) -> Result<(), String> {
    self.cmd("Storage.clearCookies", super::empty_params()).await?;
    Ok(())
  }

  // ---- Emulation ----

  /// Set viewport emulation (screen size, device scale, mobile mode, touch).
  ///
  /// # Errors
  ///
  /// Returns an error if the `Emulation.setDeviceMetricsOverride` CDP command fails.
  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
    let _ = self
      .cmd("Emulation.clearDeviceMetricsOverride", super::empty_params())
      .await;
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
    self
      .cmd(
        "Emulation.setDeviceMetricsOverride",
        serde_json::json!({
            "width": config.width,
            "height": config.height,
            "deviceScaleFactor": config.device_scale_factor,
            "mobile": config.is_mobile,
            "screenWidth": config.width,
            "screenHeight": config.height,
            "screenOrientation": orientation,
        }),
      )
      .await?;
    if config.has_touch {
      let _ = self
        .cmd(
          "Emulation.setTouchEmulationEnabled",
          serde_json::json!({"enabled": true, "maxTouchPoints": 5}),
        )
        .await;
    }
    Ok(())
  }

  /// Override the browser's user-agent string.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Network.setUserAgentOverride` CDP command fails.
  pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
    self
      .cmd("Network.setUserAgentOverride", serde_json::json!({"userAgent": ua}))
      .await?;
    Ok(())
  }

  /// Override the browser's geolocation to the given latitude, longitude, and accuracy.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Emulation.setGeolocationOverride` CDP command fails.
  pub async fn set_geolocation(&self, lat: f64, lng: f64, accuracy: f64) -> Result<(), String> {
    self
      .cmd(
        "Emulation.setGeolocationOverride",
        serde_json::json!({
            "latitude": lat, "longitude": lng, "accuracy": accuracy,
        }),
      )
      .await?;
    Ok(())
  }

  /// Set the browser locale for Intl APIs and `navigator.language`.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Network.setUserAgentOverride` CDP command fails.
  pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
    // Playwright approach: use Emulation.setLocaleOverride for Intl APIs,
    // AND Network.setUserAgentOverride with acceptLanguage for navigator.language.
    let _ = self
      .cmd("Emulation.setLocaleOverride", serde_json::json!({"locale": locale}))
      .await;
    self
      .cmd(
        "Network.setUserAgentOverride",
        serde_json::json!({
            "userAgent": "",
            "acceptLanguage": locale,
        }),
      )
      .await?;
    Ok(())
  }

  /// Override the browser's timezone.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Emulation.setTimezoneOverride` CDP command fails.
  pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
    self
      .cmd(
        "Emulation.setTimezoneOverride",
        serde_json::json!({"timezoneId": timezone_id}),
      )
      .await?;
    Ok(())
  }

  /// Emulate CSS media features (color scheme, reduced motion, forced colors, etc.).
  ///
  /// # Errors
  ///
  /// Returns an error if the `Emulation.setEmulatedMedia` CDP command fails.
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

  /// Enable or disable JavaScript execution on the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Emulation.setScriptExecutionDisabled` CDP command fails.
  pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<(), String> {
    self
      .cmd(
        "Emulation.setScriptExecutionDisabled",
        serde_json::json!({"value": !enabled}),
      )
      .await?;
    Ok(())
  }

  /// Set extra HTTP headers to be sent with every request.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Network.setExtraHTTPHeaders` CDP command fails.
  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<(), String> {
    let h: serde_json::Map<String, serde_json::Value> = headers
      .iter()
      .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
      .collect();
    self
      .cmd("Network.setExtraHTTPHeaders", serde_json::json!({"headers": h}))
      .await?;
    Ok(())
  }

  /// Grant browser permissions (e.g. geolocation, notifications) for an origin.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Browser.grantPermissions` CDP command fails.
  pub async fn grant_permissions(&self, permissions: &[String], origin: Option<&str>) -> Result<(), String> {
    let mut params = serde_json::json!({"permissions": permissions});
    if let Some(o) = origin {
      params["origin"] = serde_json::json!(o);
    }
    self.cmd("Browser.grantPermissions", params).await?;
    Ok(())
  }

  /// Reset all granted browser permissions.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Browser.resetPermissions` CDP command fails.
  pub async fn reset_permissions(&self) -> Result<(), String> {
    self.cmd("Browser.resetPermissions", super::empty_params()).await?;
    Ok(())
  }

  /// Enable or disable focus emulation (keeps page focused even when not in foreground).
  ///
  /// # Errors
  ///
  /// Returns an error if the `Emulation.setFocusEmulationEnabled` CDP command fails.
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

  /// Emulate network conditions (offline mode, latency, throughput).
  ///
  /// # Errors
  ///
  /// Returns an error if the `Network.emulateNetworkConditions` CDP command fails.
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

  /// Start Chrome tracing to collect performance data.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Tracing.start` CDP command fails.
  pub async fn start_tracing(&self) -> Result<(), String> {
    self.cmd("Tracing.start", super::empty_params()).await?;
    Ok(())
  }

  /// Stop Chrome tracing.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Tracing.end` CDP command fails.
  pub async fn stop_tracing(&self) -> Result<(), String> {
    self.cmd("Tracing.end", super::empty_params()).await?;
    Ok(())
  }

  /// Get performance metrics from the Performance domain.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Performance.getMetrics` CDP command fails.
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

  /// Resolve a backend DOM node ID to an element handle, tagging it with a ref ID.
  ///
  /// # Errors
  ///
  /// Returns an error if the node can no longer be resolved, or if tagging
  /// the element or finding it by the tag attribute fails.
  pub async fn resolve_backend_node(&self, backend_node_id: i64, ref_id: &str) -> Result<AnyElement, String> {
    let resolve_result = self
      .cmd("DOM.resolveNode", serde_json::json!({"backendNodeId": backend_node_id}))
      .await?;

    let object_id = resolve_result
      .get("object")
      .and_then(|o| o.get("objectId"))
      .and_then(|v| v.as_str())
      .ok_or_else(|| format!("Ref '{ref_id}' no longer valid."))?;

    // Tag element with data-cref attribute
    self
      .cmd(
        "Runtime.callFunctionOn",
        serde_json::json!({
            "objectId": object_id,
            "functionDeclaration": format!("function() {{ this.setAttribute('data-cref', '{ref_id}'); }}")
        }),
      )
      .await?;

    // Find by the tag
    self.find_element(&format!("[data-cref='{ref_id}']")).await
  }

  // ---- Event listeners ----

  /// Attach background listeners for console, network, dialog, and frame events.
  /// Spawns async tasks that forward CDP events to the provided log buffers
  /// and the page's event emitter.
  ///
  /// # Errors
  ///
  /// This function does not return errors directly; spawned listener tasks
  /// handle failures internally.
  pub fn attach_listeners(
    &self,
    console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
    network_log: Arc<RwLock<Vec<NetRequest>>>,
    dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
  ) {
    // Domains are already enabled via enable_domains() at session creation time.
    // No need to re-enable here.

    let transport = self.transport.clone();
    let session_id = self.session_id.clone();
    let emitter1 = self.events.clone();
    let emitter2 = self.events.clone();
    let emitter3 = self.events.clone();

    // Console listener
    Self::spawn_console_listener(transport.clone(), session_id.clone(), console_log, emitter1);

    // Network listener
    Self::spawn_network_listener(transport.clone(), session_id.clone(), network_log, emitter2);

    // Dialog handler listener -- uses configurable dialog_handler
    Self::spawn_dialog_listener(
      self.transport.clone(),
      self.session_id.clone(),
      self.dialog_handler.clone(),
      dialog_log,
      emitter3,
    );

    // Frame context tracker -- maps frame IDs to execution context IDs
    // so evaluate_in_frame() can target specific frames.
    Self::spawn_frame_context_tracker(
      self.transport.clone(),
      self.session_id.clone(),
      self.frame_contexts.clone(),
      self.events.clone(),
    );
  }

  /// Spawn a task that listens for `Runtime.consoleAPICalled` events and
  /// forwards them to the console log buffer and event emitter.
  fn spawn_console_listener(
    transport: Arc<PipeTransport>,
    session_id: Option<String>,
    console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
    emitter: crate::events::EventEmitter,
  ) {
    tokio::spawn(async move {
      let mut rx = transport.subscribe_events();
      while let Ok(event) = rx.recv().await {
        // Filter by session
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(expected_sid.as_str()) {
            continue;
          }
        }

        if event.get("method").and_then(|m| m.as_str()) == Some("Runtime.consoleAPICalled") {
          if let Some(params) = event.get("params") {
            let level = params.get("type").and_then(|v| v.as_str()).unwrap_or("log").to_string();
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
            let msg = ConsoleMsg {
              level: level.clone(),
              text: text.clone(),
            };
            console_log.write().await.push(msg.clone());
            emitter.emit(crate::events::PageEvent::Console(msg));
          }
        }
      }
    });
  }

  /// Spawn a task that listens for network events (`requestWillBeSent`,
  /// `responseReceived`, `downloadWillBegin`) and forwards them.
  fn spawn_network_listener(
    transport: Arc<PipeTransport>,
    session_id: Option<String>,
    network_log: Arc<RwLock<Vec<NetRequest>>>,
    emitter: crate::events::EventEmitter,
  ) {
    tokio::spawn(async move {
      let mut rx = transport.subscribe_events();
      while let Ok(event) = rx.recv().await {
        // Filter by session
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(expected_sid.as_str()) {
            continue;
          }
        }

        let method = event.get("method").and_then(|m| m.as_str()).unwrap_or("");
        match method {
          "Network.requestWillBeSent" => {
            if let Some(params) = event.get("params") {
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
              let net_req = NetRequest {
                id: id.clone(),
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
              };
              emitter.emit(crate::events::PageEvent::Request(net_req.clone()));
              network_log.write().await.push(net_req);
            }
          },
          "Network.responseReceived" => {
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

  /// Spawn a task that listens for `Page.javascriptDialogOpening` events,
  /// calls the configurable dialog handler, and logs the result.
  fn spawn_dialog_listener(
    transport: Arc<PipeTransport>,
    session_id: Option<String>,
    handler: Arc<tokio::sync::RwLock<crate::events::DialogHandler>>,
    dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
    emitter: crate::events::EventEmitter,
  ) {
    tokio::spawn(async move {
      let mut rx = transport.subscribe_events();
      while let Ok(event) = rx.recv().await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(expected_sid.as_str()) {
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

            // Call the configurable handler to decide action
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

  /// Spawn a task that tracks frame execution contexts and emits frame
  /// lifecycle events (attached, detached, navigated).
  fn spawn_frame_context_tracker(
    transport: Arc<PipeTransport>,
    session_id: Option<String>,
    frame_contexts: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, i64>>>,
    emitter: crate::events::EventEmitter,
  ) {
    tokio::spawn(async move {
      let mut rx = transport.subscribe_events();
      while let Ok(event) = rx.recv().await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(expected_sid.as_str()) {
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

  /// Inject a script to run before any page JS on every navigation.
  /// Returns an identifier that can be used with `remove_init_script`.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Page.addScriptToEvaluateOnNewDocument` CDP
  /// command fails.
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

  /// Remove a previously injected init script by identifier.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Page.removeScriptToEvaluateOnNewDocument` CDP
  /// command fails.
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
  /// This creates window.__`fd_binding`__ channel and manages pending callbacks.
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

  /// Ensure the binding channel is initialized (Runtime.addBinding + init script).
  async fn ensure_binding_channel(&self) -> Result<(), String> {
    if self.binding_initialized.swap(true, std::sync::atomic::Ordering::SeqCst) {
      return Ok(());
    }
    // Register the CDP binding
    self
      .cmd("Runtime.addBinding", serde_json::json!({"name": "__fd_binding__"}))
      .await?;
    // Inject the controller as init script so it runs on every navigation
    self.add_init_script(Self::BINDING_CONTROLLER_JS).await?;
    // Also run it immediately on the current page
    self.evaluate(Self::BINDING_CONTROLLER_JS).await?;

    // Start the binding event listener
    let t = self.transport.clone();
    let sid = self.session_id.clone();
    let fns = self.exposed_fns.clone();
    tokio::spawn(async move {
      let mut rx = t.subscribe_events();
      while let Ok(event) = rx.recv().await {
        if let Some(ref expected_sid) = sid {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(expected_sid.as_str()) {
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

          // Look up the registered function
          let maybe_fn = fns.read().await.get(&fn_name).cloned();
          if let Some(callback) = maybe_fn {
            let result = callback(args);
            // Deliver result back to the page
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

  /// Expose a Rust function to the page as `window.<name>(...)`.
  /// The function receives JSON arguments and returns a JSON value.
  /// The exposed function persists across navigations via init script.
  ///
  /// # Errors
  ///
  /// Returns an error if initializing the binding channel fails, or if
  /// injecting the init script for the function registration fails.
  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<(), String> {
    self.ensure_binding_channel().await?;
    self.exposed_fns.write().await.insert(name.to_string(), func);
    // Register this specific function name in the controller
    let register_js = format!("globalThis.__fd_bc.add('{}')", crate::steps::js_escape(name));
    self.add_init_script(&register_js).await?;
    self.evaluate(&register_js).await?;
    Ok(())
  }

  /// Remove a previously exposed function.
  ///
  /// # Errors
  ///
  /// Returns an error if evaluating the cleanup JS on the page fails.
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

  /// Close this page's CDP target. Subsequent operations on this page will fail.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Target.closeTarget` CDP command fails (though
  /// the page is marked as closed regardless).
  pub async fn close_page(&self) -> Result<(), String> {
    if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
      return Ok(()); // already closed
    }
    // Close the CDP target. Send via browser session (None), not the page session.
    let _ = self
      .transport
      .send_command(
        None,
        "Target.closeTarget",
        serde_json::json!({"targetId": self.target_id}),
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

  /// Enable the Fetch domain for request interception and spawn the
  /// event handler that routes intercepted requests to registered handlers.
  async fn ensure_fetch_enabled(&self) -> Result<(), String> {
    if self.fetch_enabled.swap(true, std::sync::atomic::Ordering::SeqCst) {
      return Ok(());
    }
    // Enable Fetch domain to intercept all requests
    self
      .cmd(
        "Fetch.enable",
        serde_json::json!({
            "patterns": [{"urlPattern": "*", "requestStage": "Request"}],
            "handleAuthRequests": false,
        }),
      )
      .await?;

    // Spawn event handler for Fetch.requestPaused
    let t = self.transport.clone();
    let sid = self.session_id.clone();
    let routes = self.routes.clone();
    tokio::spawn(async move {
      Self::handle_fetch_events(t, sid, routes).await;
    });
    Ok(())
  }

  /// Process `Fetch.requestPaused` events, matching against registered routes
  /// and fulfilling, continuing, or aborting requests accordingly.
  async fn handle_fetch_events(
    transport: Arc<PipeTransport>,
    session_id: Option<String>,
    routes: Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
  ) {
    let mut rx = transport.subscribe_events();
    while let Ok(event) = rx.recv().await {
      if let Some(ref expected_sid) = session_id {
        let event_sid = event.get("sessionId").and_then(|v| v.as_str());
        if event_sid != Some(expected_sid.as_str()) {
          continue;
        }
      }
      if event.get("method").and_then(|m| m.as_str()) != Some("Fetch.requestPaused") {
        continue;
      }
      let Some(params) = event.get("params") else { continue };
      let request_id = params
        .get("requestId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
      let req_obj = params.get("request");
      let url = req_obj
        .and_then(|r| r.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
      let method = req_obj
        .and_then(|r| r.get("method"))
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_string();
      let resource_type = params
        .get("resourceType")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
      let post_data = req_obj
        .and_then(|r| r.get("postData"))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
      let headers: rustc_hash::FxHashMap<String, String> = req_obj
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
        request_id: request_id.clone(),
        url: url.clone(),
        method,
        headers,
        post_data,
        resource_type,
      };

      // Find matching route
      let action = {
        let routes_guard = routes.read().await;
        let mut matched_action = None;
        for route in routes_guard.iter() {
          if route.pattern.is_match(&url) {
            matched_action = Some((route.handler)(&intercepted));
            break;
          }
        }
        matched_action
      };

      Self::execute_route_action(&transport, session_id.as_deref(), &request_id, action).await;
    }
  }

  /// Execute the matched route action (fulfill, continue, or abort) for an
  /// intercepted request.
  async fn execute_route_action(
    transport: &PipeTransport,
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
        // No matching route -- continue request unmodified
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

  /// Register a route handler for URLs matching the glob pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if the glob pattern is invalid or if enabling the
  /// Fetch domain fails.
  pub async fn route(&self, pattern: &str, handler: crate::route::RouteHandler) -> Result<(), String> {
    let regex = crate::route::glob_to_regex(pattern)?;
    self.routes.write().await.push(crate::route::RegisteredRoute {
      pattern: regex,
      pattern_str: pattern.to_string(),
      handler,
    });
    self.ensure_fetch_enabled().await
  }

  /// Remove all route handlers matching the glob pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if disabling the Fetch domain fails when no routes remain.
  pub async fn unroute(&self, pattern: &str) -> Result<(), String> {
    let mut routes = self.routes.write().await;
    routes.retain(|r| r.pattern_str != pattern);
    if routes.is_empty() && self.fetch_enabled.load(std::sync::atomic::Ordering::SeqCst) {
      self.fetch_enabled.store(false, std::sync::atomic::Ordering::SeqCst);
      let _ = self.cmd("Fetch.disable", serde_json::json!({})).await;
    }
    Ok(())
  }
}

// ---- CdpPipeElement ---------------------------------------------------------

#[derive(Clone)]
pub struct CdpPipeElement {
  transport: Arc<PipeTransport>,
  session_id: Option<String>,
  node_id: i64,
}

impl CdpPipeElement {
  async fn cmd(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    self
      .transport
      .send_command(self.session_id.as_deref(), method, params)
      .await
  }

  /// Get element center coordinates for clicking.
  async fn get_center(&self) -> Result<(f64, f64), String> {
    let result = self
      .cmd("DOM.getBoxModel", serde_json::json!({"nodeId": self.node_id}))
      .await?;
    let content = result
      .get("model")
      .and_then(|m| m.get("content"))
      .and_then(|c| c.as_array())
      .ok_or("No box model")?;

    // content quad: [x1,y1, x2,y2, x3,y3, x4,y4]
    if content.len() < 8 {
      return Err("Invalid box model".into());
    }
    let x1 = content[0].as_f64().unwrap_or(0.0);
    let y1 = content[1].as_f64().unwrap_or(0.0);
    let x3 = content[4].as_f64().unwrap_or(0.0);
    let y3 = content[5].as_f64().unwrap_or(0.0);

    Ok((f64::midpoint(x1, x3), f64::midpoint(y1, y3)))
  }

  /// Resolve this element's nodeId to a Runtime objectId.
  async fn resolve_object_id(&self) -> Result<String, String> {
    let resolved = self
      .cmd("DOM.resolveNode", serde_json::json!({"nodeId": self.node_id}))
      .await?;
    resolved
      .get("object")
      .and_then(|o| o.get("objectId"))
      .and_then(|v| v.as_str())
      .map(std::string::ToString::to_string)
      .ok_or("Cannot resolve element".into())
  }

  /// Call a JS function on this element and return the value.
  ///
  /// # Errors
  ///
  /// Returns an error if resolving the element's object ID fails or if the
  /// `Runtime.callFunctionOn` CDP command fails.
  pub async fn call_js_fn_value(&self, function: &str) -> Result<Option<serde_json::Value>, String> {
    let object_id = self.resolve_object_id().await?;
    let result = self
      .cmd(
        "Runtime.callFunctionOn",
        serde_json::json!({
            "objectId": object_id,
            "functionDeclaration": function,
            "returnByValue": true,
        }),
      )
      .await?;
    Ok(result.get("result").and_then(|r| r.get("value")).cloned())
  }

  /// Click this element using CDP mouse events (with JS fallback).
  ///
  /// # Errors
  ///
  /// Returns an error if scrolling into view, computing coordinates, or
  /// dispatching mouse events fails.
  pub async fn click(&self) -> Result<(), String> {
    // Single JS call: scroll into view + get center coordinates
    let center = self.call_js_fn_value(
            "function() { this.scrollIntoViewIfNeeded(); var r = this.getBoundingClientRect(); return {x: r.x + r.width/2, y: r.y + r.height/2}; }"
        ).await?;

    if let Some(c) = center {
      let x = c.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      let y = c.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      if x == 0.0 && y == 0.0 {
        // Element has no layout, use JS click
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

  /// Double-click this element using CDP mouse events (with JS fallback).
  ///
  /// # Errors
  ///
  /// Returns an error if scrolling into view, computing coordinates, or
  /// dispatching mouse events fails.
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

  /// Hover over this element by scrolling it into view and moving the mouse to its center.
  ///
  /// # Errors
  ///
  /// Returns an error if scrolling into view, getting the element center,
  /// or dispatching the mouse move event fails.
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

  /// Click this element and insert text in a single CDP call.
  ///
  /// # Errors
  ///
  /// Returns an error if clicking the element or the `Input.insertText` CDP command fails.
  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    self.click().await?;
    self.cmd("Input.insertText", serde_json::json!({"text": text})).await?;
    Ok(())
  }

  /// Call a JS function on this element (no return value).
  ///
  /// # Errors
  ///
  /// Returns an error if resolving the element's object ID fails or if the
  /// `Runtime.callFunctionOn` CDP command fails.
  pub async fn call_js_fn(&self, function: &str) -> Result<(), String> {
    let object_id = self.resolve_object_id().await?;
    self
      .cmd(
        "Runtime.callFunctionOn",
        serde_json::json!({
            "objectId": object_id,
            "functionDeclaration": function,
        }),
      )
      .await?;
    Ok(())
  }

  /// Scroll this element into view if needed.
  ///
  /// # Errors
  ///
  /// Returns an error if the `DOM.scrollIntoViewIfNeeded` CDP command fails.
  pub async fn scroll_into_view(&self) -> Result<(), String> {
    self
      .cmd(
        "DOM.scrollIntoViewIfNeeded",
        serde_json::json!({"nodeId": self.node_id}),
      )
      .await?;
    Ok(())
  }

  /// Capture a screenshot of this element's bounding box.
  ///
  /// # Errors
  ///
  /// Returns an error if the element's box model cannot be obtained, the
  /// screenshot CDP command fails, or base64 decoding fails.
  pub async fn screenshot(&self, format: ImageFormat) -> Result<Vec<u8>, String> {
    // Get bounding box
    let result = self
      .cmd("DOM.getBoxModel", serde_json::json!({"nodeId": self.node_id}))
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
