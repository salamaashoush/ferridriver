//! CDP Raw backend -- our own high-performance CDP over WebSocket.
//!
//! Same architecture as `cdp_pipe` (oneshot channels, broadcast events, no handler
//! bottleneck) but over WebSocket instead of Unix pipes. Supports true parallel
//! multi-page automation -- multiple CDP commands can be in-flight simultaneously.

pub(crate) mod transport;

use super::{
  AnyElement, AnyPage, Arc, AxNodeData, AxProperty, ConsoleMsg, CookieData, ImageFormat, MetricData, NetRequest,
  RwLock, ScreenshotOpts,
};
use base64::Engine as _;
use rustc_hash::FxHashMap;
use std::time::Duration;
use transport::WsTransport;

// ---- CdpRawBrowser --------------------------------------------------------

pub struct CdpRawBrowser {
  transport: Arc<WsTransport>,
  child: Option<tokio::process::Child>,
  /// Track targetId -> sessionId for already-attached targets.
  attached_targets: std::sync::Mutex<FxHashMap<String, Option<String>>>,
}

impl CdpRawBrowser {
  /// Enable required CDP domains on a session so events and queries work.
  async fn enable_domains(transport: &WsTransport, session_id: Option<&str>) -> Result<(), String> {
    transport
      .send_command(session_id, "Page.enable", super::empty_params())
      .await?;
    transport
      .send_command(session_id, "Runtime.enable", super::empty_params())
      .await?;
    transport
      .send_command(session_id, "DOM.enable", super::empty_params())
      .await?;
    transport
      .send_command(session_id, "Network.enable", super::empty_params())
      .await?;
    transport
      .send_command(session_id, "Accessibility.enable", super::empty_params())
      .await?;

    let engine_js = crate::selectors::build_inject_js();
    transport
      .send_command(
        session_id,
        "Page.addScriptToEvaluateOnNewDocument",
        serde_json::json!({"source": engine_js}),
      )
      .await?;

    Ok(())
  }

  /// Launch Chrome with `--remote-debugging-pipe` and communicate over fd 3/4.
  ///
  /// # Errors
  ///
  /// Returns an error if the Chrome process fails to spawn, the WebSocket
  /// connection cannot be established, or the required CDP domains cannot be
  /// enabled on the initial session.
  pub async fn launch(chromium_path: &str) -> Result<Self, String> {
    Box::pin(Self::launch_with_flags(
      chromium_path,
      &crate::state::chrome_flags(true, &[]),
    ))
    .await
  }

  /// Launch Chrome with custom command-line flags and communicate over WebSocket.
  ///
  /// # Errors
  ///
  /// Returns an error if the Chrome process fails to spawn, the WebSocket
  /// transport cannot connect, target creation or attachment fails, or
  /// domain enablement fails.
  pub async fn launch_with_flags(chromium_path: &str, flags: &[String]) -> Result<Self, String> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let user_data_dir = std::env::temp_dir().join(format!("ferridriver-raw-{}-{id}", std::process::id()));

    let (transport, child) = Box::pin(WsTransport::spawn(chromium_path, &user_data_dir, flags)).await?;
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
      child: Some(child),
      attached_targets: std::sync::Mutex::new(attached),
    })
  }

  /// Connect to a running Chrome instance via WebSocket URL.
  ///
  /// # Errors
  ///
  /// Returns an error if the WebSocket connection fails, target discovery
  /// or attachment fails, or the required CDP domains cannot be enabled.
  pub async fn connect(ws_url: &str) -> Result<Self, String> {
    let transport = Arc::new(WsTransport::connect(ws_url).await?);

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
          Self::enable_domains(&transport, sid.as_deref()).await?;
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
      Self::enable_domains(&transport, sid.as_deref()).await?;
      attached.insert(target_id, sid);
    }

    Ok(Self {
      transport,
      child: None,
      attached_targets: std::sync::Mutex::new(attached),
    })
  }

  /// List all open page targets, attaching to any that are not yet tracked.
  ///
  /// # Errors
  ///
  /// Returns an error if listing targets fails, attaching to a target fails,
  /// or enabling CDP domains on a new session fails.
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

      pages.push(AnyPage::CdpRaw(CdpRawPage {
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

  /// Create a new page target and optionally navigate to the given URL.
  ///
  /// # Errors
  ///
  /// Returns an error if target creation, attachment, domain enablement, or
  /// navigation (when `url` is not `about:blank`) fails.
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

    let page = CdpRawPage {
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
      page.goto(url).await?;
    }

    Ok(AnyPage::CdpRaw(page))
  }

  /// Create a new page in an isolated browser context and optionally navigate.
  ///
  /// # Errors
  ///
  /// Returns an error if browser context creation, target creation,
  /// attachment, domain enablement, or navigation fails.
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

    let page = CdpRawPage {
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
      page.goto(url).await?;
    }

    Ok(AnyPage::CdpRaw(page))
  }

  /// Close the browser and kill the child process if one was launched.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Browser.close` CDP command fails (though the
  /// error is silently ignored since we also kill the child process).
  pub async fn close(&mut self) -> Result<(), String> {
    let _ = self
      .transport
      .send_command(None, "Browser.close", super::empty_params())
      .await;
    if let Some(ref mut child) = self.child {
      let _ = child.kill().await;
    }
    Ok(())
  }
}

// ---- CdpRawPage ------------------------------------------------------------

#[derive(Clone)]
pub struct CdpRawPage {
  transport: Arc<WsTransport>,
  session_id: Option<String>,
  target_id: String,
  pub events: crate::events::EventEmitter,
  frame_contexts: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, i64>>>,
  pub dialog_handler: Arc<tokio::sync::RwLock<crate::events::DialogHandler>>,
  pub exposed_fns: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, crate::events::ExposedFn>>>,
  binding_initialized: Arc<std::sync::atomic::AtomicBool>,
  closed: Arc<std::sync::atomic::AtomicBool>,
  routes: Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
  fetch_enabled: Arc<std::sync::atomic::AtomicBool>,
}

impl CdpRawPage {
  /// Send a CDP command to this page's session.
  async fn cmd(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    self
      .transport
      .send_command(self.session_id.as_deref(), method, params)
      .await
  }

  // ---- Navigation ----

  /// Navigate to the given URL, waiting for `Page.loadEventFired`.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Page.navigate` CDP command fails or the
  /// navigation reports an error (e.g. DNS resolution failure).
  pub async fn goto(&self, url: &str) -> Result<(), String> {
    let rx = self
      .transport
      .register_nav_waiter(self.session_id.as_deref().unwrap_or(""))
      .await;

    let nav_result = self.cmd("Page.navigate", serde_json::json!({"url": url})).await?;

    if let Some(error_text) = nav_result.get("errorText").and_then(|v| v.as_str()) {
      if !error_text.is_empty() {
        return Err(format!("Navigation failed: {error_text}"));
      }
    }

    match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
      Ok(Ok(result)) => result,
      Ok(Err(_)) | Err(_) => Ok(()),
    }
  }

  /// Wait for an in-progress navigation to complete (`Page.loadEventFired`).
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation waiter is dropped before the event
  /// fires.
  pub async fn wait_for_navigation(&self) -> Result<(), String> {
    // Register nav waiter and await Page.loadEventFired (Bun's pattern)
    let rx = self
      .transport
      .register_nav_waiter(self.session_id.as_deref().unwrap_or(""))
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
  pub async fn reload(&self) -> Result<(), String> {
    self.cmd("Page.reload", super::empty_params()).await?;
    Ok(())
  }

  /// Navigate back in session history.
  ///
  /// # Errors
  ///
  /// Returns an error if navigation history retrieval or entry navigation fails.
  pub async fn go_back(&self) -> Result<(), String> {
    self.history_go(-1).await
  }

  /// Navigate forward in session history.
  ///
  /// # Errors
  ///
  /// Returns an error if navigation history retrieval or entry navigation fails.
  pub async fn go_forward(&self) -> Result<(), String> {
    self.history_go(1).await
  }

  /// Navigate history by delta. Same as Bun's historyGo:
  /// Page.getNavigationHistory -> pick entries[currentIndex + delta].id
  /// -> Page.navigateToHistoryEntry -> Page.loadEventFired settles.
  async fn history_go(&self, delta: i32) -> Result<(), String> {
    let hist = self.cmd("Page.getNavigationHistory", super::empty_params()).await?;
    let current = hist
      .get("currentIndex")
      .and_then(serde_json::Value::as_i64)
      .unwrap_or(0);
    let target = current + i64::from(delta);
    let entries = hist.get("entries").and_then(|v| v.as_array());
    let Some(entries) = entries else {
      return Ok(());
    };
    // At history boundary -- nothing to do (same as Bun's canGoBack check)
    let Some(target_idx) = usize::try_from(target).ok().filter(|&t| t < entries.len()) else {
      return Ok(());
    };
    let entry_id = entries[target_idx]
      .get("id")
      .and_then(serde_json::Value::as_i64)
      .unwrap_or(0);

    // Register nav waiter before navigating
    let rx = self
      .transport
      .register_nav_waiter(self.session_id.as_deref().unwrap_or(""))
      .await;
    self
      .cmd("Page.navigateToHistoryEntry", serde_json::json!({"entryId": entry_id}))
      .await?;
    // Wait for Page.loadEventFired — the navigation IS happening so this will fire
    match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
      Ok(Ok(r)) => r,
      _ => Ok(()),
    }
  }

  /// Return the current page URL via `location.href`.
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

  /// Return the current document title.
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

  /// Evaluate a JavaScript expression in the page and return its value.
  ///
  /// # Errors
  ///
  /// Returns an error if the CDP `Runtime.evaluate` command fails or the
  /// evaluated expression throws an exception.
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

  /// Retrieve the frame tree for the current page.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Page.getFrameTree` CDP command fails.
  pub async fn get_frame_tree(&self) -> Result<Vec<super::FrameInfo>, String> {
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
  /// Returns an error if no execution context is found for the given frame,
  /// the CDP `Runtime.evaluate` command fails, or the expression throws.
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
      Err(format!("No execution context found for frame '{frame_id}'."))
    }
  }

  // ---- Elements ----

  /// Find a single DOM element matching the given CSS selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the document root cannot be obtained, the selector
  /// is invalid, or no element matches.
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

    Ok(AnyElement::CdpRaw(CdpRawElement {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      node_id,
    }))
  }

  /// Evaluate JavaScript that returns a DOM element and wrap it as `AnyElement`.
  ///
  /// # Errors
  ///
  /// Returns an error if the JS does not return a DOM element, or the
  /// node cannot be resolved to a `nodeId`.
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

    Ok(AnyElement::CdpRaw(CdpRawElement {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      node_id,
    }))
  }

  // ---- Content ----

  /// Return the full outer HTML of the document element.
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

  /// Replace the page content with the given HTML string.
  ///
  /// # Errors
  ///
  /// Returns an error if the frame tree cannot be read or
  /// `Page.setDocumentContent` fails.
  pub async fn set_content(&self, html: &str) -> Result<(), String> {
    // Get the frame tree to find the main frame ID
    let tree = self.cmd("Page.getFrameTree", super::empty_params()).await?;
    let frame_id = tree
      .get("frameTree")
      .and_then(|f| f.get("frame"))
      .and_then(|f| f.get("id"))
      .and_then(|v| v.as_str())
      .ok_or("No main frame")?;

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

  /// Capture a screenshot of the full page or viewport.
  ///
  /// # Errors
  ///
  /// Returns an error if `Page.captureScreenshot` or layout metrics
  /// retrieval fails, or the base64 data cannot be decoded.
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
  /// Returns an error if the element is not found, the bounding rect
  /// cannot be computed, the screenshot fails, or base64 decoding fails.
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

  // ---- PDF generation ----

  /// Generate a PDF of the current page.
  ///
  /// # Errors
  ///
  /// Returns an error if `Page.printToPDF` fails or the base64 PDF data
  /// cannot be decoded.
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

  /// Set the file paths on a `<input type="file">` element.
  ///
  /// # Errors
  ///
  /// Returns an error if the document root cannot be obtained, the
  /// selector does not match, or `DOM.setFileInputFiles` fails.
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

  /// Retrieve the full accessibility tree for the page.
  ///
  /// # Errors
  ///
  /// Returns an error if `Accessibility.getFullAXTree` fails or the
  /// response is missing `nodes`.
  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>, String> {
    self.accessibility_tree_with_depth(-1).await
  }

  /// Retrieve the accessibility tree up to the given depth.
  ///
  /// # Errors
  ///
  /// Returns an error if `Accessibility.getFullAXTree` fails or the
  /// response is missing `nodes`.
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

  /// Click at the given viewport coordinates with the left mouse button.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Input.dispatchMouseEvent` CDP command fails.
  pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
    self.click_at_opts(x, y, "left", 1).await
  }

  /// Click at the given coordinates with configurable button and click count.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Input.dispatchMouseEvent` CDP command fails.
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

  /// Move the mouse to the given viewport coordinates.
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

  /// Move the mouse smoothly from one point to another using eased steps.
  ///
  /// # Errors
  ///
  /// Returns an error if any intermediate `Input.dispatchMouseEvent` fails.
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
      // Cubic bezier easing for natural movement
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

  /// Perform a click-and-drag from one point to another with smooth steps.
  ///
  /// # Errors
  ///
  /// Returns an error if any `Input.dispatchMouseEvent` call fails.
  pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64)) -> Result<(), String> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mousePressed", "x": from.0, "y": from.1, "button": "left", "clickCount": 1}),
      )
      .await?;
    // Smooth drag with intermediate steps
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

  /// Dispatch a mouse wheel event with the given deltas.
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

  /// Press a mouse button at the given coordinates without releasing.
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

  /// Release a mouse button at the given coordinates.
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

  /// Type a string of text character-by-character via key events.
  ///
  /// # Errors
  ///
  /// Returns an error if any `Input.dispatchKeyEvent` call fails.
  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    for ch in text.chars() {
      self
        .cmd(
          "Input.dispatchKeyEvent",
          serde_json::json!({"type": "char", "text": ch.to_string()}),
        )
        .await?;
    }
    Ok(())
  }

  /// Press and release a named key (e.g. "Enter", "Tab", "`ArrowLeft`").
  ///
  /// # Errors
  ///
  /// Returns an error if the `Input.dispatchKeyEvent` CDP command fails.
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

  /// Retrieve all cookies visible to the current page.
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

  /// Set a cookie with the given data.
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

  /// Clear all cookies in the browser storage.
  ///
  /// # Errors
  ///
  /// Returns an error if the `Storage.clearCookies` CDP command fails.
  pub async fn clear_cookies(&self) -> Result<(), String> {
    self.cmd("Storage.clearCookies", super::empty_params()).await?;
    Ok(())
  }

  // ---- Emulation ----

  /// Set device viewport emulation (size, scale, mobile, touch).
  ///
  /// # Errors
  ///
  /// Returns an error if `Emulation.setDeviceMetricsOverride` fails.
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

  /// Override the browser's User-Agent string.
  ///
  /// # Errors
  ///
  /// Returns an error if `Network.setUserAgentOverride` fails.
  pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
    self
      .cmd("Network.setUserAgentOverride", serde_json::json!({"userAgent": ua}))
      .await?;
    Ok(())
  }

  /// Override the browser's geolocation.
  ///
  /// # Errors
  ///
  /// Returns an error if `Emulation.setGeolocationOverride` fails.
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

  /// Override the browser locale and accept-language header.
  ///
  /// # Errors
  ///
  /// Returns an error if `Network.setUserAgentOverride` fails.
  pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
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
  /// Returns an error if `Emulation.setTimezoneOverride` fails.
  pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
    self
      .cmd(
        "Emulation.setTimezoneOverride",
        serde_json::json!({"timezoneId": timezone_id}),
      )
      .await?;
    Ok(())
  }

  /// Emulate CSS media features (color scheme, reduced motion, etc.).
  ///
  /// # Errors
  ///
  /// Returns an error if `Emulation.setEmulatedMedia` fails.
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
  /// Returns an error if `Emulation.setScriptExecutionDisabled` fails.
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
  /// Returns an error if `Network.setExtraHTTPHeaders` fails.
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

  /// Grant browser permissions (e.g. geolocation, notifications).
  ///
  /// # Errors
  ///
  /// Returns an error if `Browser.grantPermissions` fails.
  pub async fn grant_permissions(&self, permissions: &[String], origin: Option<&str>) -> Result<(), String> {
    let mut params = serde_json::json!({"permissions": permissions});
    if let Some(o) = origin {
      params["origin"] = serde_json::json!(o);
    }
    self.cmd("Browser.grantPermissions", params).await?;
    Ok(())
  }

  /// Reset all previously granted permissions.
  ///
  /// # Errors
  ///
  /// Returns an error if `Browser.resetPermissions` fails.
  pub async fn reset_permissions(&self) -> Result<(), String> {
    self.cmd("Browser.resetPermissions", super::empty_params()).await?;
    Ok(())
  }

  /// Enable or disable focus emulation for the page.
  ///
  /// # Errors
  ///
  /// Returns an error if `Emulation.setFocusEmulationEnabled` fails.
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

  /// Emulate network conditions (offline, latency, throughput).
  ///
  /// # Errors
  ///
  /// Returns an error if `Network.emulateNetworkConditions` fails.
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

  /// Start a tracing session.
  ///
  /// # Errors
  ///
  /// Returns an error if `Tracing.start` fails.
  pub async fn start_tracing(&self) -> Result<(), String> {
    self.cmd("Tracing.start", super::empty_params()).await?;
    Ok(())
  }

  /// Stop the current tracing session.
  ///
  /// # Errors
  ///
  /// Returns an error if `Tracing.end` fails.
  pub async fn stop_tracing(&self) -> Result<(), String> {
    self.cmd("Tracing.end", super::empty_params()).await?;
    Ok(())
  }

  /// Retrieve performance metrics for the current page.
  ///
  /// # Errors
  ///
  /// Returns an error if `Performance.getMetrics` fails.
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

  /// Resolve a backend DOM node ID to an element, tagging it for lookup.
  ///
  /// # Errors
  ///
  /// Returns an error if the node can no longer be resolved, the tagging
  /// script fails, or the element cannot be found by selector.
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
    Self::spawn_network_listener(transport, session_id, network_log, emitter2);

    // Dialog handler listener -- uses configurable dialog_handler
    Self::spawn_dialog_listener(
      self.transport.clone(),
      self.session_id.clone(),
      self.dialog_handler.clone(),
      dialog_log,
      emitter3,
    );

    // Frame context tracker
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
    transport: Arc<WsTransport>,
    session_id: Option<String>,
    console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
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
            let msg = ConsoleMsg { level, text };
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
    transport: Arc<WsTransport>,
    session_id: Option<String>,
    network_log: Arc<RwLock<Vec<NetRequest>>>,
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
              let url = resp
                .and_then(|r| r.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
              let status_text = resp
                .and_then(|r| r.get("statusText"))
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
    transport: Arc<WsTransport>,
    session_id: Option<String>,
    dialog_handler: Arc<tokio::sync::RwLock<crate::events::DialogHandler>>,
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
            let action = dialog_handler.read().await(&pending);
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

  /// Spawn a task that tracks frame execution contexts and emits frame events.
  fn spawn_frame_context_tracker(
    transport: Arc<WsTransport>,
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
              frame_contexts.write().await.retain(|_, &mut v| v != ctx_id);
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

  /// Add a script to be evaluated on every new document before page scripts run.
  ///
  /// # Errors
  ///
  /// Returns an error if `Page.addScriptToEvaluateOnNewDocument` fails.
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

  /// Remove a previously added init script by its identifier.
  ///
  /// # Errors
  ///
  /// Returns an error if `Page.removeScriptToEvaluateOnNewDocument` fails.
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

  async fn ensure_binding_channel(&self) -> Result<(), String> {
    if self.binding_initialized.swap(true, std::sync::atomic::Ordering::SeqCst) {
      return Ok(());
    }
    self
      .cmd("Runtime.addBinding", serde_json::json!({"name": "__fd_binding__"}))
      .await?;
    // Same controller JS as CdpPipe
    let controller_js = crate::backend::cdp_pipe::CdpPipePage::BINDING_CONTROLLER_JS;
    self.add_init_script(controller_js).await?;
    self.evaluate(controller_js).await?;

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
          if params.get("name").and_then(|v| v.as_str()) != Some("__fd_binding__") {
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
            let js = format!(
              "globalThis.__fd_bc.resolve({},{})",
              seq,
              serde_json::to_string(&result).unwrap_or("null".into())
            );
            let _ = t
              .send_command(
                sid.as_deref(),
                "Runtime.evaluate",
                serde_json::json!({"expression": js}),
              )
              .await;
          } else {
            let js = format!("globalThis.__fd_bc.reject({seq},'Function not found: {fn_name}')");
            let _ = t
              .send_command(
                sid.as_deref(),
                "Runtime.evaluate",
                serde_json::json!({"expression": js}),
              )
              .await;
          }
        }
      }
    });
    Ok(())
  }

  /// Expose a Rust function to page JavaScript under the given name.
  ///
  /// # Errors
  ///
  /// Returns an error if the binding channel setup, init script
  /// registration, or the registration JS evaluation fails.
  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<(), String> {
    self.ensure_binding_channel().await?;
    self.exposed_fns.write().await.insert(name.to_string(), func);
    let register_js = format!("globalThis.__fd_bc.add('{}')", crate::steps::js_escape(name));
    self.add_init_script(&register_js).await?;
    self.evaluate(&register_js).await?;
    Ok(())
  }

  /// Remove a previously exposed function by name.
  ///
  /// # Errors
  ///
  /// Returns an error if the cleanup JS evaluation fails.
  pub async fn remove_exposed_function(&self, name: &str) -> Result<(), String> {
    self.exposed_fns.write().await.remove(name);
    let js = format!(
      "if(globalThis.__fd_bc)globalThis.__fd_bc.del('{}')",
      crate::steps::js_escape(name)
    );
    self.evaluate(&js).await?;
    Ok(())
  }

  /// Close this page target. Subsequent calls are no-ops.
  ///
  /// # Errors
  ///
  /// Returns an error if `Target.closeTarget` fails (though the error is
  /// silently ignored).
  pub async fn close_page(&self) -> Result<(), String> {
    if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
      return Ok(());
    }
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

  async fn ensure_fetch_enabled(&self) -> Result<(), String> {
    if self.fetch_enabled.swap(true, std::sync::atomic::Ordering::SeqCst) {
      return Ok(());
    }
    self
      .cmd(
        "Fetch.enable",
        serde_json::json!({
            "patterns": [{"urlPattern": "*", "requestStage": "Request"}],
            "handleAuthRequests": false,
        }),
      )
      .await?;

    let t = self.transport.clone();
    let sid = self.session_id.clone();
    let routes = self.routes.clone();
    tokio::spawn(async move {
      let mut rx = t.subscribe_events();
      while let Ok(event) = rx.recv().await {
        if let Some(ref expected_sid) = sid {
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

        let action = {
          let routes_guard = routes.read().await;
          routes_guard
            .iter()
            .find(|r| r.pattern.is_match(&url))
            .map(|r| (r.handler)(&intercepted))
        };

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
            let _ = t
              .send_command(
                sid.as_deref(),
                "Fetch.fulfillRequest",
                serde_json::json!({
                    "requestId": request_id, "responseCode": resp.status,
                    "responsePhrase": crate::route::status_text(resp.status),
                    "responseHeaders": hdrs, "body": body_b64,
                }),
              )
              .await;
          },
          Some(crate::route::RouteAction::Continue(overrides)) => {
            let mut p = serde_json::json!({"requestId": request_id});
            if let Some(u) = &overrides.url {
              p["url"] = serde_json::Value::String(u.clone());
            }
            if let Some(m) = &overrides.method {
              p["method"] = serde_json::Value::String(m.clone());
            }
            if let Some(h) = &overrides.headers {
              p["headers"] =
                serde_json::Value::Array(h.iter().map(|(k, v)| serde_json::json!({"name":k,"value":v})).collect());
            }
            if let Some(pd) = &overrides.post_data {
              p["postData"] = serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(pd));
            }
            let _ = t.send_command(sid.as_deref(), "Fetch.continueRequest", p).await;
          },
          Some(crate::route::RouteAction::Abort(reason)) => {
            let er = match reason.to_lowercase().as_str() {
              "aborted" => "Aborted",
              "blockedbyclient" => "BlockedByClient",
              "connectionfailed" => "ConnectionFailed",
              "connectionrefused" => "ConnectionRefused",
              "internetdisconnected" => "InternetDisconnected",
              "timedout" => "TimedOut",
              _ => "Failed",
            };
            let _ = t
              .send_command(
                sid.as_deref(),
                "Fetch.failRequest",
                serde_json::json!({"requestId": request_id, "errorReason": er}),
              )
              .await;
          },
          None => {
            let _ = t
              .send_command(
                sid.as_deref(),
                "Fetch.continueRequest",
                serde_json::json!({"requestId": request_id}),
              )
              .await;
          },
        }
      }
    });
    Ok(())
  }

  /// Register a route handler for requests matching the given glob pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if the glob pattern is invalid or enabling the
  /// `Fetch` domain fails.
  pub async fn route(&self, pattern: &str, handler: crate::route::RouteHandler) -> Result<(), String> {
    let regex = crate::route::glob_to_regex(pattern)?;
    self.routes.write().await.push(crate::route::RegisteredRoute {
      pattern: regex,
      pattern_str: pattern.to_string(),
      handler,
    });
    self.ensure_fetch_enabled().await
  }

  /// Remove a previously registered route handler by pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if disabling the `Fetch` domain fails when no
  /// routes remain.
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

// ---- CdpRawElement ---------------------------------------------------------

#[derive(Clone)]
pub struct CdpRawElement {
  transport: Arc<WsTransport>,
  session_id: Option<String>,
  node_id: i64,
}

impl CdpRawElement {
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
  /// Returns an error if the element cannot be resolved or the
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

  /// Click this element by scrolling it into view and dispatching mouse events.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be scrolled into view, the
  /// bounding rect cannot be computed, or the mouse event dispatch fails.
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

  /// Double-click this element.
  ///
  /// # Errors
  ///
  /// Returns an error if scrolling, bounding rect computation, or mouse
  /// event dispatch fails.
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

  /// Hover over this element by moving the mouse to its center.
  ///
  /// # Errors
  ///
  /// Returns an error if scrolling into view, getting the center
  /// coordinates, or the mouse move event fails.
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

  /// Click this element and type a string into it character-by-character.
  ///
  /// # Errors
  ///
  /// Returns an error if the click or any key event dispatch fails.
  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    self.click().await?;
    for ch in text.chars() {
      self
        .cmd(
          "Input.dispatchKeyEvent",
          serde_json::json!({"type": "char", "text": ch.to_string()}),
        )
        .await?;
    }
    Ok(())
  }

  /// Call a JS function on this element (result is discarded).
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be resolved or the
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

  /// Scroll this element into the visible viewport if needed.
  ///
  /// # Errors
  ///
  /// Returns an error if `DOM.scrollIntoViewIfNeeded` fails.
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
  /// Returns an error if the box model cannot be obtained, the screenshot
  /// capture fails, or base64 decoding fails.
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
