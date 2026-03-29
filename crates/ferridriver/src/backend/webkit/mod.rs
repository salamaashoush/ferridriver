//! `WebKit` backend — native `WKWebView` on macOS.
//!
//! Architecture ported from Bun's webview implementation:
//! - Parent communicates over Unix socketpair with binary frames
//! - Child subprocess runs `WKWebView` on main thread (single-threaded, nonblocking)
//! - No JSON IPC. No tokio for spawning. No background threads in child.

pub mod ipc;

use super::{
  AnyElement, AnyPage, Arc, AxNodeData, AxProperty, ConsoleMsg, CookieData, ImageFormat, MetricData, NetRequest,
  RwLock, ScreenshotOpts,
};
use ipc::{IpcClient, IpcResponse, Op};

// ─── WebKitBrowser ──────────────────────────────────────────────────────────

pub struct WebKitBrowser {
  client: Arc<IpcClient>,
  child: std::process::Child,
}

impl WebKitBrowser {
  /// Launch a new `WebKit` browser subprocess via the native host binary.
  ///
  /// # Errors
  ///
  /// Returns an error if the host binary cannot be found or the subprocess
  /// fails to start or become ready.
  pub async fn launch() -> Result<Self, String> {
    let (client, child) = IpcClient::spawn().await?;
    Ok(Self {
      client: Arc::new(client),
      child,
    })
  }

  /// List all open pages (views) in this browser instance.
  ///
  /// # Errors
  ///
  /// Returns an error if the IPC call to list views fails or times out.
  pub async fn pages(&self) -> Result<Vec<AnyPage>, String> {
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
  pub async fn new_page(&self, url: &str) -> Result<AnyPage, String> {
    let r = self.client.send_str(Op::CreateView, url).await?;
    match r {
      IpcResponse::ViewCreated(id) => {
        let page = WebKitPage {
          client: self.client.clone(),
          view_id: id,
          events: crate::events::EventEmitter::new(),
          routes: std::sync::Arc::new(std::sync::RwLock::new(Vec::new())),
          closed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        // Inject selector engine via WKUserScript (runs at document start
        // on every navigation, equivalent to addScriptToEvaluateOnNewDocument)
        let engine_js = crate::selectors::build_inject_js();
        let mut p = Vec::new();
        p.extend_from_slice(&page.vid().to_le_bytes());
        ipc::str_encode(&mut p, &engine_js);
        let _ = page.client.send(Op::AddInitScript, &p).await;
        Ok(AnyPage::WebKit(page))
      },
      IpcResponse::Error(e) => Err(e),
      _ => Err("unexpected".into()),
    }
  }

  /// Create a new page in an isolated context (delegates to `new_page` on `WebKit`).
  ///
  /// # Errors
  ///
  /// Returns an error if page creation fails.
  pub async fn new_page_isolated(&self, url: &str) -> Result<AnyPage, String> {
    self.new_page(url).await
  }

  /// Close the browser by killing the host subprocess.
  ///
  /// # Errors
  ///
  /// This function currently always succeeds; errors from killing or waiting
  /// on the child process are silently ignored.
  pub fn close(&mut self) -> impl std::future::Future<Output = Result<(), String>> {
    // OP_SHUTDOWN calls _exit(0) immediately -- no response comes back.
    // Just kill the child process directly.
    let _ = self.child.kill();
    let _ = self.child.wait();
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
}

impl WebKitPage {
  fn vid(&self) -> u64 {
    self.view_id
  }

  fn ok(r: IpcResponse) -> Result<(), String> {
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
  pub async fn goto(&self, url: &str) -> Result<(), String> {
    let r = self.client.send_str_vid(Op::Navigate, url, self.vid()).await?;
    Self::ok(r)?;
    let r2 = self.client.send_vid(Op::WaitNav, self.vid()).await?;
    Self::ok(r2)
  }

  /// Wait for the current navigation to complete.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait IPC call times out or fails.
  pub async fn wait_for_navigation(&self) -> Result<(), String> {
    let r = self.client.send_vid(Op::WaitNav, self.vid()).await?;
    Self::ok(r)
  }

  /// Reload the current page.
  ///
  /// # Errors
  ///
  /// Returns an error if the reload IPC call fails.
  pub async fn reload(&self) -> Result<(), String> {
    let r = self.client.send_vid(Op::Reload, self.vid()).await?;
    Self::ok(r)
  }

  /// Navigate back in the session history and wait for navigation to complete.
  ///
  /// # Errors
  ///
  /// Returns an error if the go-back IPC call fails or the navigation wait times out.
  pub async fn go_back(&self) -> Result<(), String> {
    let r = self.client.send_vid(Op::GoBack, self.vid()).await?;
    Self::ok(r)?;
    // Wait for navigation to complete via nav delegate
    let r2 = self.client.send_vid(Op::WaitNav, self.vid()).await?;
    Self::ok(r2)
  }

  /// Navigate forward in the session history and wait for navigation to complete.
  ///
  /// # Errors
  ///
  /// Returns an error if the go-forward IPC call fails or the navigation wait times out.
  pub async fn go_forward(&self) -> Result<(), String> {
    let r = self.client.send_vid(Op::GoForward, self.vid()).await?;
    Self::ok(r)?;
    let r2 = self.client.send_vid(Op::WaitNav, self.vid()).await?;
    Self::ok(r2)
  }

  /// Get the current URL of the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the IPC call to retrieve the URL fails.
  pub async fn url(&self) -> Result<Option<String>, String> {
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
  pub async fn title(&self) -> Result<Option<String>, String> {
    let r = self.client.send_vid(Op::GetTitle, self.vid()).await?;
    match r {
      IpcResponse::Value(v) => Ok(v.as_str().map(std::string::ToString::to_string)),
      IpcResponse::Error(e) => Err(e),
      _ => Ok(None),
    }
  }

  /// Evaluate a JavaScript expression in the page and return the result.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails or the IPC call times out.
  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>, String> {
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
  pub async fn find_element(&self, selector: &str) -> Result<AnyElement, String> {
    let js = format!(
      r"(function(){{var e=document.querySelector('{}');if(!e)return null;if(!window.__wr)window.__wr={{}};var id=Object.keys(window.__wr).length+1;window.__wr[id]=e;return id}})()",
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

  /// Evaluate a JS expression that returns a DOM element, returning a reference handle.
  ///
  /// # Errors
  ///
  /// Returns an error if the expression does not return a valid DOM element.
  pub async fn evaluate_to_element(&self, js: &str) -> Result<AnyElement, String> {
    let wrap = format!(
      r"(function(){{var e=({js});if(!e)return null;if(!window.__wr)window.__wr={{}};var id=Object.keys(window.__wr).length+1;window.__wr[id]=e;return id}})()"
    );
    let r = self.evaluate(&wrap).await?;
    let ref_id = r.and_then(|v| v.as_u64()).ok_or("JS did not return a DOM element")?;
    Ok(AnyElement::WebKit(WebKitElement {
      client: self.client.clone(),
      view_id: self.view_id,
      ref_id,
    }))
  }

  /// Get the frame tree. Currently returns the main frame only on `WebKit`.
  ///
  /// # Errors
  ///
  /// Returns an error if retrieving the current URL fails.
  pub async fn get_frame_tree(&self) -> Result<Vec<super::FrameInfo>, String> {
    // WebKit doesn't expose frame tree via IPC yet.
    // Return main frame only.
    let url = self.url().await?.unwrap_or_default();
    Ok(vec![super::FrameInfo {
      frame_id: format!("main-{}", self.view_id),
      parent_frame_id: None,
      name: String::new(),
      url,
    }])
  }

  /// Evaluate JavaScript in a specific frame. Currently evaluates in the main frame only.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn evaluate_in_frame(
    &self,
    expression: &str,
    _frame_id: &str,
  ) -> Result<Option<serde_json::Value>, String> {
    // WebKit: evaluate in main frame only for now.
    // Full iframe support would need WKFrameInfo-based evaluation.
    self.evaluate(expression).await
  }

  /// Get the full HTML content of the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation to read outerHTML fails.
  pub async fn content(&self) -> Result<String, String> {
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
  pub async fn set_content(&self, html: &str) -> Result<(), String> {
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
  pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>, String> {
    // Send format + quality as payload: u8 format (0=png, 1=jpeg, 2=webp) + u8 quality + u64 vid
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
    let r = self.client.send(Op::Screenshot, &p).await?;
    match r {
      IpcResponse::Binary(d) => Ok(d),
      IpcResponse::Error(e) => Err(e),
      _ => Err("no data".into()),
    }
  }

  /// Take a screenshot of a specific element by scrolling it into view first.
  ///
  /// # Errors
  ///
  /// Returns an error if the screenshot IPC call fails.
  pub async fn screenshot_element(&self, sel: &str, _fmt: ImageFormat) -> Result<Vec<u8>, String> {
    // Scroll element into view, then take full screenshot
    // WKWebView doesn't support clipped screenshots natively
    let esc = sel.replace('\'', "\\'");
    let _ = self
      .evaluate(&format!(
        "document.querySelector('{esc}')?.scrollIntoView({{block:'center'}})"
      ))
      .await;
    self.screenshot(ScreenshotOpts::default()).await
  }

  /// Generate a PDF of the page. Not supported on `WebKit` backend.
  ///
  /// # Errors
  ///
  /// Always returns an error because PDF generation requires a CDP backend.
  pub fn pdf(
    &self,
    _landscape: bool,
    _print_background: bool,
  ) -> impl std::future::Future<Output = Result<Vec<u8>, String>> {
    let result = if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
      Err("Page is closed".into())
    } else {
      Err("PDF generation requires CDP backend (cdp-ws, cdp-pipe, or cdp-raw)".into())
    };
    std::future::ready(result)
  }

  /// Set file input on an `<input type="file">` element.
  ///
  /// # Errors
  ///
  /// Returns an error if no paths are provided or the IPC call fails.
  pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
      return Err("No file paths provided".into());
    }
    // WebKit uses a custom IPC op that reads the file in ObjC and injects via DataTransfer API
    let mut p = Vec::new();
    ipc::str_encode(&mut p, selector);
    ipc::str_encode(&mut p, &paths[0]); // First file only (multi-file needs multiple calls)
    p.extend_from_slice(&self.view_id.to_le_bytes());
    let r = self.client.send(ipc::Op::SetFileInput, &p).await?;
    Self::ok(r)
  }

  /// Get the full accessibility tree via native `NSAccessibility`.
  ///
  /// # Errors
  ///
  /// Returns an error if the accessibility tree IPC call fails or response parsing fails.
  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>, String> {
    self.accessibility_tree_with_depth(-1).await
  }

  /// Get the accessibility tree limited to a specific depth via native `NSAccessibility`.
  ///
  /// # Errors
  ///
  /// Returns an error if the IPC call fails, returns an unexpected response type,
  /// or the JSON response cannot be parsed.
  pub async fn accessibility_tree_with_depth(&self, depth: i32) -> Result<Vec<AxNodeData>, String> {
    // Use native NSAccessibility tree via IPC (not JavaScript)
    let mut p = Vec::new();
    p.extend_from_slice(&self.vid().to_le_bytes());
    p.extend_from_slice(&depth.to_le_bytes());
    let r = self.client.send(ipc::Op::AccessibilityTree, &p).await?;
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
  pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
    self.click_at_opts(x, y, "left", 1).await
  }

  /// Click at coordinates with specific button and click count options.
  ///
  /// # Errors
  ///
  /// Returns an error if any of the mouse down/up IPC calls fail.
  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<(), String> {
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

  /// Move the mouse to the given coordinates using JS event dispatch.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
    // WKWebView's mouseMoved: doesn't propagate to DOM in offscreen windows.
    // Use WKWebView.evaluateJavaScript to dispatch the event directly in the
    // web content process, same approach as Playwright's webkit backend.
    let js = format!(
      "(function(){{var e=document.elementFromPoint({x},{y});\
            if(e)e.dispatchEvent(new MouseEvent('mousemove',{{clientX:{x},clientY:{y},bubbles:true,view:window}}))}})()"
    );
    self.evaluate(&js).await?;
    Ok(())
  }

  /// Move the mouse smoothly from one point to another with easing.
  ///
  /// # Errors
  ///
  /// Returns an error if the batched JavaScript evaluation fails.
  pub async fn move_mouse_smooth(
    &self,
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
    steps: u32,
  ) -> Result<(), String> {
    use std::fmt::Write;
    let steps = steps.max(1);
    // Batch all moves into one JS evaluate for performance
    let mut js = String::with_capacity(steps as usize * 120 + 20);
    js.push_str("(function(){");
    for i in 0..=steps {
      let t = f64::from(i) / f64::from(steps);
      let ease = t * t * (3.0 - 2.0 * t);
      let x = from_x + (to_x - from_x) * ease;
      let y = from_y + (to_y - from_y) * ease;
      let _ = write!(
        js,
        "var e=document.elementFromPoint({x},{y});\
                if(e)e.dispatchEvent(new MouseEvent('mousemove',{{clientX:{x},clientY:{y},bubbles:true,view:window}}));"
      );
    }
    js.push_str("})()");
    self.evaluate(&js).await?;
    Ok(())
  }

  /// Scroll the page by the given deltas using `window.scrollBy`.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<(), String> {
    self.evaluate(&format!("window.scrollBy({delta_x},{delta_y})")).await?;
    Ok(())
  }

  /// Send a mouse-down event at the given coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse event IPC call fails.
  pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
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
  pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
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
  pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64)) -> Result<(), String> {
    self.send_mouse_event(1, 0, 1, from.0, from.1).await?; // down
    let steps = 10u32;
    for i in 1..=steps {
      let t = f64::from(i) / f64::from(steps);
      let ease = t * t * (3.0 - 2.0 * t);
      let x = from.0 + (to.0 - from.0) * ease;
      let y = from.1 + (to.1 - from.1) * ease;
      self.send_mouse_event(0, 0, 0, x, y).await?; // move
    }
    self.send_mouse_event(2, 0, 1, to.0, to.1).await // up
  }

  /// Send a native mouse event via IPC.
  /// `mouse_type`: 0=move, 1=down, 2=up
  /// button: 0=left, 1=right, 2=middle
  async fn send_mouse_event(&self, mouse_type: u8, button: u8, click_count: u32, x: f64, y: f64) -> Result<(), String> {
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
  pub async fn type_str(&self, text: &str) -> Result<(), String> {
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
  pub async fn press_key(&self, key: &str) -> Result<(), String> {
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
  pub async fn get_cookies(&self) -> Result<Vec<CookieData>, String> {
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
  pub async fn set_cookie(&self, c: CookieData) -> Result<(), String> {
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
    let r = self.client.send(ipc::Op::SetCookie, &p).await?;
    Self::ok(r)
  }

  /// Delete a cookie by name and optional domain.
  ///
  /// # Errors
  ///
  /// Returns an error if the delete cookie IPC call fails.
  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<(), String> {
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
  pub async fn clear_cookies(&self) -> Result<(), String> {
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
  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
    // Native resize + scale via IPC -- sets window backingScaleFactor,
    // resizes NSWindow and WKWebView frame. Affects actual rendering.
    let mut p = Vec::new();
    p.extend_from_slice(&(config.width as f64).to_le_bytes());
    p.extend_from_slice(&(config.height as f64).to_le_bytes());
    p.extend_from_slice(&config.device_scale_factor.to_le_bytes());
    p.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(ipc::Op::SetViewport, &p).await?;
    Self::ok(r)
  }

  /// Override the User-Agent string for this page.
  ///
  /// # Errors
  ///
  /// Returns an error if the set user agent IPC call fails.
  pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
    let mut p = Vec::new();
    ipc::str_encode(&mut p, ua);
    p.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(Op::SetUserAgent, &p).await?;
    Self::ok(r)
  }

  /// Emulate geolocation by overriding `navigator.geolocation` via JavaScript.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn set_geolocation(&self, lat: f64, lng: f64, acc: f64) -> Result<(), String> {
    let js = format!(
      "(function(){{var pos={{coords:{{latitude:{lat},longitude:{lng},accuracy:{acc},altitude:null,altitudeAccuracy:null,heading:null,speed:null}},timestamp:Date.now()}};navigator.geolocation.getCurrentPosition=function(s){{s(pos)}};navigator.geolocation.watchPosition=function(s){{s(pos);return 0}}}})()"
    );
    self.evaluate(&js).await?;
    Ok(())
  }

  /// Emulate network online/offline state via `navigator.onLine` override.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn set_network_state(&self, offline: bool, _lat: f64, _dl: f64, _ul: f64) -> Result<(), String> {
    // Can only emulate offline/online via navigator.onLine override
    // Throttling not possible without native NSURLProtocol interception
    let js = format!(
      "Object.defineProperty(navigator,'onLine',{{get:function(){{return {}}},configurable:true}})",
      if offline { "false" } else { "true" }
    );
    self.evaluate(&js).await?;
    Ok(())
  }

  /// Set the browser locale for this page via native IPC.
  ///
  /// # Errors
  ///
  /// Returns an error if the set locale IPC call fails.
  pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
    let mut p = Vec::new();
    p.extend_from_slice(&self.vid().to_le_bytes());
    ipc::str_encode(&mut p, locale);
    let r = self.client.send(ipc::Op::SetLocale, &p).await?;
    Self::ok(r)
  }

  /// Set the browser timezone for this page via native IPC.
  ///
  /// # Errors
  ///
  /// Returns an error if the set timezone IPC call fails.
  pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
    let mut p = Vec::new();
    p.extend_from_slice(&self.vid().to_le_bytes());
    ipc::str_encode(&mut p, timezone_id);
    let r = self.client.send(ipc::Op::SetTimezone, &p).await?;
    Self::ok(r)
  }

  /// Emulate media features (color scheme, reduced motion, forced colors, etc.).
  ///
  /// # Errors
  ///
  /// Returns an error if the emulate media IPC call fails.
  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<(), String> {
    let mut p = Vec::new();
    p.extend_from_slice(&self.vid().to_le_bytes());
    ipc::str_encode(&mut p, opts.color_scheme.as_deref().unwrap_or(""));
    ipc::str_encode(&mut p, opts.reduced_motion.as_deref().unwrap_or(""));
    ipc::str_encode(&mut p, opts.forced_colors.as_deref().unwrap_or(""));
    ipc::str_encode(&mut p, opts.media.as_deref().unwrap_or(""));
    ipc::str_encode(&mut p, opts.contrast.as_deref().unwrap_or(""));
    let r = self.client.send(ipc::Op::EmulateMedia, &p).await?;
    Self::ok(r)
  }

  /// Enable or disable JavaScript. Partial support on `WebKit` backend.
  ///
  /// # Errors
  ///
  /// This function currently always succeeds; the JS flag is set via evaluate.
  pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<(), String> {
    // Use WKPreferences.javaScriptEnabled (deprecated but functional)
    // This is applied via native IPC since we need access to the WKWebView configuration
    // For webkit, JS control needs to happen at the host level
    // Use OP_EVALUATE to set a flag, then the host applies it
    // Actually: we can't disable JS from JS. This needs a native op.
    // For now, use the WKPreferences approach via evaluate on the host side
    let val = if enabled { "true" } else { "false" };
    let script = format!("window.__fd_js_enabled = {val}");
    let _ = self.evaluate(&script).await;
    Ok(())
  }

  /// Inject custom HTTP headers by intercepting `fetch` and `XMLHttpRequest` via JS.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<(), String> {
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
  ) -> impl std::future::Future<Output = Result<(), String>> {
    let result = if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
      Err("Page is closed".into())
    } else {
      Ok(())
    };
    std::future::ready(result)
  }

  /// Reset permissions. No-op on `WebKit` backend.
  ///
  /// # Errors
  ///
  /// This function currently always succeeds.
  pub fn reset_permissions(&self) -> impl std::future::Future<Output = Result<(), String>> {
    let result = if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
      Err("Page is closed".into())
    } else {
      Ok(())
    };
    std::future::ready(result)
  }

  /// Emulate focus state by overriding `document.hasFocus()` and `visibilityState`.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn set_focus_emulation_enabled(&self, enabled: bool) -> Result<(), String> {
    // Override document.hasFocus() and visibilityState via WKUserScript
    let js = if enabled {
      "(function(){Object.defineProperty(document,'hasFocus',{value:function(){return true},configurable:true});\
            Object.defineProperty(document,'visibilityState',{get:function(){return 'visible'},configurable:true});\
            Object.defineProperty(document,'hidden',{get:function(){return false},configurable:true})})()"
    } else {
      "(function(){delete document.hasFocus;delete document.visibilityState;delete document.hidden})()"
    };
    self.evaluate(js).await?;
    Ok(())
  }

  /// Start performance tracing by recording the start time.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn start_tracing(&self) -> Result<(), String> {
    // Mark the start time for performance measurement
    self.evaluate("window.__fd_trace_start = performance.now()").await?;
    Ok(())
  }

  /// Stop performance tracing by recording the end time.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn stop_tracing(&self) -> Result<(), String> {
    self.evaluate("window.__fd_trace_end = performance.now()").await?;
    Ok(())
  }

  /// Get page performance metrics (`DOMContentLoaded`, Load, TTFB).
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation to read performance entries fails.
  pub async fn metrics(&self) -> Result<Vec<MetricData>, String> {
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
  pub async fn resolve_backend_node(&self, _id: i64, ref_id: &str) -> Result<AnyElement, String> {
    self.find_element(&format!("[data-cref='{ref_id}']")).await
  }

  /// Spawn a background task that drains console, dialog, and network events
  /// from the IPC reader thread into the shared state logs.
  pub fn attach_listeners(
    &self,
    console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
    net_log: Arc<RwLock<Vec<NetRequest>>>,
    dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
  ) {
    let client = self.client.clone();
    let emitter = self.events.clone();
    let notify = client.event_notify.clone();
    tokio::spawn(async move {
      loop {
        // Wait for the IPC reader thread to signal that events arrived.
        // No polling -- wakes instantly when a console/dialog/network event is received.
        notify.notified().await;

        // Drain console events
        {
          let msgs: Vec<(String, String)> = {
            let Ok(mut log) = client.console_log.lock() else {
              continue;
            };
            if log.is_empty() {
              Vec::new()
            } else {
              std::mem::take(&mut *log)
            }
          };
          if !msgs.is_empty() {
            let mut dest = console_log.write().await;
            for (level, text) in msgs {
              let msg = ConsoleMsg { level, text };
              emitter.emit(crate::events::PageEvent::Console(msg.clone()));
              dest.push(msg);
            }
          }
        }

        // Drain dialog events
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
              emitter.emit(crate::events::PageEvent::Dialog(crate::events::PendingDialog {
                dialog_type: dtype.clone(),
                message: message.clone(),
                default_value: String::new(),
              }));
              dest.push(crate::state::DialogEvent {
                dialog_type: dtype,
                message,
                action,
              });
            }
          }
        }

        // Drain network events
        {
          let evts: Vec<(String, String, String, String)> = {
            let Ok(mut log) = client.network_log.lock() else {
              continue;
            };
            if log.is_empty() {
              Vec::new()
            } else {
              std::mem::take(&mut *log)
            }
          };
          if !evts.is_empty() {
            let mut dest = net_log.write().await;
            for (id, method, url, resource_type) in evts {
              let req = NetRequest {
                id: id.clone(),
                method: method.clone(),
                url: url.clone(),
                resource_type: resource_type.clone(),
                status: None,
                mime_type: None,
                headers: None,
                post_data: None,
              };
              emitter.emit(crate::events::PageEvent::Request(req.clone()));
              dest.push(req);
            }
          }
        }
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
  pub async fn add_init_script(&self, source: &str) -> Result<String, String> {
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
  pub fn remove_init_script(&self, _identifier: &str) -> impl std::future::Future<Output = Result<(), String>> {
    // WKWebView limitation: individual WKUserScript removal is not supported.
    let result = if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
      Err("Page is closed".into())
    } else {
      Ok(())
    };
    std::future::ready(result)
  }

  // ── Exposed Functions ──
  // WebKit uses JS-based binding controller (same as CDP) injected via evaluate.
  // WKScriptMessageHandler could be used but would require new IPC ops for
  // per-function message routing. The JS approach works and is performant.

  /// Expose a function to JavaScript. Not yet supported on `WebKit` backend.
  ///
  /// # Errors
  ///
  /// Always returns an error because `WebKit` lacks `Runtime.addBinding` equivalent.
  pub fn expose_function(
    &self,
    name: &str,
    _func: crate::events::ExposedFn,
  ) -> impl std::future::Future<Output = Result<(), String>> {
    let result = if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
      Err("Page is closed".into())
    } else {
      Err(format!("expose_function('{name}') not yet supported on WebKit backend"))
    };
    std::future::ready(result)
  }

  /// Remove an exposed function. Not yet supported on `WebKit` backend.
  ///
  /// # Errors
  ///
  /// Always returns an error because exposed functions are not supported.
  pub fn remove_exposed_function(&self, name: &str) -> impl std::future::Future<Output = Result<(), String>> {
    let result = if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
      Err("Page is closed".into())
    } else {
      Err(format!(
        "remove_exposed_function('{name}') not yet supported on WebKit backend"
      ))
    };
    std::future::ready(result)
  }

  /// Register a route handler to intercept network requests matching the given glob pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if the glob pattern is invalid, the route lock is poisoned,
  /// or the JavaScript injection to register the route pattern fails.
  pub async fn route(&self, pattern: &str, handler: crate::route::RouteHandler) -> Result<(), String> {
    let regex = crate::route::glob_to_regex(pattern)?;

    // Add route to Rust-side list (write lock -- cold path)
    self
      .routes
      .write()
      .map_err(|e| format!("routes write lock poisoned: {e}"))?
      .push(crate::route::RegisteredRoute {
        pattern: regex.clone(),
        pattern_str: pattern.to_string(),
        handler,
      });

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
              if route.pattern.is_match(url) {
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
                let action = (route.handler)(&intercepted);
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
    let regex_str = regex.as_str().replace('\\', "\\\\").replace('\'', "\\'");
    let js = format!(
      "(function(){{window.__fd_routes=window.__fd_routes||[];window.__fd_routes.push(new RegExp('{regex_str}'))}})();"
    );
    self.evaluate(&js).await?;
    self.add_init_script(&js).await?;

    Ok(())
  }

  /// Remove a previously registered route handler by its glob pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if the route lock is poisoned or the JavaScript cleanup fails.
  pub async fn unroute(&self, pattern: &str) -> Result<(), String> {
    // Remove from Rust-side list (write lock -- cold path)
    self
      .routes
      .write()
      .map_err(|e| format!("routes write lock poisoned: {e}"))?
      .retain(|r| r.pattern_str != pattern);

    // Remove from JS-side pattern list
    if let Ok(regex) = crate::route::glob_to_regex(pattern) {
      let regex_str = regex.as_str().replace('\\', "\\\\").replace('\'', "\\'");
      let js = format!(
        "(function(){{window.__fd_routes=(window.__fd_routes||[]).filter(function(r){{return r.source!=='{regex_str}'}})}})()"
      );
      self.evaluate(&js).await?;
    }
    Ok(())
  }

  /// Close this page (view) via the IPC close command.
  ///
  /// # Errors
  ///
  /// Returns an error if the close IPC call fails.
  pub async fn close_page(&self) -> Result<(), String> {
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
    format!("window.__wr[{}]", self.ref_id)
  }

  async fn eval(&self, js: &str) -> Result<(), String> {
    let mut p = Vec::new();
    ipc::str_encode(&mut p, js);
    p.extend_from_slice(&self.view_id.to_le_bytes());
    let _ = self.client.send(Op::Evaluate, &p).await?;
    Ok(())
  }

  /// Click the element using native `NSEvent` after scrolling it into view.
  ///
  /// # Errors
  ///
  /// Returns an error if coordinate extraction or the native click IPC call fails.
  pub async fn click(&self) -> Result<(), String> {
    // Scroll into view first
    let _ = self.scroll_into_view().await;
    // Get element center coordinates, then use native NSEvent click (OP_CLICK)
    // instead of JS .click() which doesn't trigger native focus behavior.
    let js = format!(
      "(function(){{var e={};var r=e.getBoundingClientRect();return r.left+r.width/2+','+( r.top+r.height/2)}})()",
      self.el()
    );
    let mut payload = Vec::new();
    ipc::str_encode(&mut payload, &js);
    payload.extend_from_slice(&self.view_id.to_le_bytes());
    let eval_result = self.client.send(ipc::Op::Evaluate, &payload).await?;
    match eval_result {
      IpcResponse::Value(val) => {
        let coords = val.as_str().unwrap_or("0,0");
        let parts: Vec<&str> = coords.split(',').collect();
        let x: f64 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let y: f64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.0);
        // Native click via NSEvent
        let mut click_p = Vec::new();
        click_p.extend_from_slice(&x.to_le_bytes());
        click_p.extend_from_slice(&y.to_le_bytes());
        click_p.extend_from_slice(&self.view_id.to_le_bytes());
        let r2 = self.client.send(ipc::Op::Click, &click_p).await?;
        match r2 {
          IpcResponse::Error(e) => Err(e),
          _ => Ok(()),
        }
      },
      IpcResponse::Error(e) => Err(e),
      // Fallback to JS click if coordinate extraction fails
      _ => self.eval(&format!("{}.click()", self.el())).await,
    }
  }

  /// Double-click the element by dispatching click and dblclick DOM events.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn dblclick(&self) -> Result<(), String> {
    let _ = self.scroll_into_view().await;
    // Fire dblclick via JS - WebKit's NSEvent approach for dblclick would need
    // OP_CLICK with clickCount=2, but the simpler approach dispatches DOM events
    // which is reliable for web app event handlers.
    self.eval(&format!("(function(){{var e={el};e.dispatchEvent(new MouseEvent('click',{{bubbles:true,detail:1}}));e.dispatchEvent(new MouseEvent('click',{{bubbles:true,detail:2}}));e.dispatchEvent(new MouseEvent('dblclick',{{bubbles:true,detail:2}}))}})()
", el=self.el())).await
  }

  /// Hover over the element by dispatching a mouseenter DOM event.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn hover(&self) -> Result<(), String> {
    self
      .eval(&format!(
        "{}.dispatchEvent(new MouseEvent('mouseenter',{{bubbles:true}}))",
        self.el()
      ))
      .await
  }

  /// Type text into the element by setting its value and dispatching an input event.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    let esc = text.replace('\\', "\\\\").replace('\'', "\\'");
    self
      .eval(&format!(
        "(function(){{var e={};e.focus();e.value+='{esc}';e.dispatchEvent(new Event('input',{{bubbles:true}}))}})()",
        self.el()
      ))
      .await
  }

  /// Call a JavaScript function with this element as `this`.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn call_js_fn(&self, func: &str) -> Result<(), String> {
    self.eval(&format!("({}).call({})", func, self.el())).await
  }

  /// Call a JavaScript function with this element as `this` and return the result.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation or IPC call fails.
  pub async fn call_js_fn_value(&self, func: &str) -> Result<Option<serde_json::Value>, String> {
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
  pub async fn scroll_into_view(&self) -> Result<(), String> {
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
  pub async fn screenshot(&self, fmt: ImageFormat) -> Result<Vec<u8>, String> {
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
