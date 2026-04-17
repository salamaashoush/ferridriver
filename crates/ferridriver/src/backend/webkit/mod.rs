#![allow(clippy::missing_errors_doc)]
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

#[derive(Clone)]
pub struct WebKitBrowser {
  client: Arc<IpcClient>,
  child: Arc<std::sync::Mutex<Option<std::process::Child>>>,
}

impl WebKitBrowser {
  /// Launch a new `WebKit` browser subprocess via the native host binary.
  ///
  /// # Errors
  ///
  /// Returns an error if the host binary cannot be found or the subprocess
  /// fails to start or become ready.
  pub async fn launch() -> Result<Self, String> {
    Self::launch_with_options(true).await
  }

  /// Launch with explicit headless/headful control.
  ///
  /// # Errors
  ///
  /// Returns an error if the host binary cannot be found or the subprocess
  /// fails to start or become ready.
  pub async fn launch_with_options(headless: bool) -> Result<Self, String> {
    let (client, child) = IpcClient::spawn(headless).await?;
    Ok(Self {
      client: Arc::new(client),
      child: Arc::new(std::sync::Mutex::new(Some(child))),
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
              injected_script: std::sync::Arc::new(InjectedScriptManager::new()),
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
          injected_script: std::sync::Arc::new(InjectedScriptManager::new()),
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
  pub fn close(&mut self) -> impl std::future::Future<Output = Result<(), String>> {
    // OP_SHUTDOWN calls _exit(0) immediately -- no response comes back.
    // Just kill the child process directly.
    if let Some(mut child) = self
      .child
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take()
    {
      let _ = child.kill();
      let _ = child.wait();
    }
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

  async fn ensure(&self, page: &WebKitPage) -> Result<(), String> {
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
  pub async fn goto(
    &self,
    url: &str,
    _lifecycle: crate::backend::NavLifecycle,
    _timeout_ms: u64,
  ) -> Result<(), String> {
    // WebKit backend: WKWebView navigation delegate fires on load complete.
    // Lifecycle granularity (commit vs domcontentloaded vs load) is not
    // distinguishable via the native API — all waits resolve on load.
    let r = self.client.send_str_vid(Op::Navigate, url, self.vid()).await?;
    Self::ok(r)?;
    let r2 = self.client.send_vid(Op::WaitNav, self.vid()).await?;
    Self::ok(r2)
  }

  /// Wait for the current navigation to complete.
  pub async fn wait_for_navigation(&self) -> Result<(), String> {
    let r = self.client.send_vid(Op::WaitNav, self.vid()).await?;
    Self::ok(r)
  }

  pub async fn reload(&self, _lifecycle: crate::backend::NavLifecycle, _timeout_ms: u64) -> Result<(), String> {
    let r = self.client.send_vid(Op::Reload, self.vid()).await?;
    Self::ok(r)?;
    let r2 = self.client.send_vid(Op::WaitNav, self.vid()).await?;
    Self::ok(r2)
  }

  pub async fn go_back(&self, _lifecycle: crate::backend::NavLifecycle, _timeout_ms: u64) -> Result<(), String> {
    let r = self.client.send_vid(Op::GoBack, self.vid()).await?;
    Self::ok(r)?;
    let r2 = self.client.send_vid(Op::WaitNav, self.vid()).await?;
    Self::ok(r2)
  }

  pub async fn go_forward(&self, _lifecycle: crate::backend::NavLifecycle, _timeout_ms: u64) -> Result<(), String> {
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

  pub async fn injected_script(&self) -> Result<String, String> {
    self.ensure_engine_injected().await?;
    Ok("window.__fd".to_string())
  }

  /// Ensures the selector engine is injected into the current execution context.
  /// Idempotent and navigation-aware.
  pub async fn ensure_engine_injected(&self) -> Result<(), String> {
    self.injected_script.ensure(self).await
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

  /// Take a screenshot of a specific element by scrolling it into view,
  /// capturing a full screenshot, then cropping to the element's bounding box via JS.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found, screenshot fails, or cropping fails.
  pub async fn screenshot_element(&self, sel: &str, fmt: ImageFormat) -> Result<Vec<u8>, String> {
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
        quality: None,
        full_page: false,
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
  /// Supports multiple files by sending each file via IPC sequentially.
  ///
  /// # Errors
  ///
  /// Returns an error if no paths are provided or any IPC call fails.
  pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
      return Err("No file paths provided".into());
    }
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
    Self::parse_ax_response(r)
  }

  fn parse_ax_response(r: IpcResponse) -> Result<Vec<AxNodeData>, String> {
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

  /// Move the mouse to the given coordinates.
  /// Sends native `NSEvent` for CSS `:hover` state, plus a JS `mousemove`
  /// event for DOM listeners (native `mouseMoved:` doesn't reliably fire
  /// DOM events in headless/offscreen `WKWebView` windows).
  ///
  /// # Errors
  ///
  /// Returns an error if the native mouse event or JS evaluation fails.
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
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
  pub async fn key_down(&self, key: &str) -> Result<(), String> {
    let mut p = Vec::new();
    ipc::str_encode(&mut p, key);
    p.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(Op::KeyDown, &p).await?;
    Self::ok(r)
  }

  pub async fn key_up(&self, key: &str) -> Result<(), String> {
    let mut p = Vec::new();
    ipc::str_encode(&mut p, key);
    p.extend_from_slice(&self.vid().to_le_bytes());
    let r = self.client.send(Op::KeyUp, &p).await?;
    Self::ok(r)
  }

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

  /// Bypass CSP. Not supported on `WebKit` backend -- stubbed.
  pub fn set_bypass_csp(&self, _enabled: bool) -> impl std::future::Future<Output = Result<(), String>> {
    let _ = &self.client;
    std::future::ready(Ok(()))
  }

  /// Ignore certificate errors. Not supported on `WebKit` backend -- stubbed.
  pub fn set_ignore_certificate_errors(&self, _ignore: bool) -> impl std::future::Future<Output = Result<(), String>> {
    let _ = &self.client;
    std::future::ready(Ok(()))
  }

  /// Set download behavior. Not supported on `WebKit` backend -- stubbed.
  pub fn set_download_behavior(
    &self,
    _behavior: &str,
    _download_path: &str,
  ) -> impl std::future::Future<Output = Result<(), String>> {
    let _ = &self.client;
    std::future::ready(Ok(()))
  }

  /// Set HTTP credentials. Not supported on `WebKit` backend -- stubbed.
  pub fn set_http_credentials(
    &self,
    _username: &str,
    _password: &str,
  ) -> impl std::future::Future<Output = Result<(), String>> {
    let _ = &self.client;
    std::future::ready(Ok(()))
  }

  /// Block service workers. Not supported on `WebKit` backend -- stubbed.
  pub fn set_service_workers_blocked(&self, _blocked: bool) -> impl std::future::Future<Output = Result<(), String>> {
    let _ = &self.client;
    std::future::ready(Ok(()))
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
    let injected_script = self.injected_script.clone();
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
            for (r#type, text) in msgs {
              let msg = ConsoleMsg { r#type, text };
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
              if resource_type == "Document" {
                injected_script.reset();
              }
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
  ) -> Result<(), String> {
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
  pub async fn unroute(&self, matcher: &crate::url_matcher::UrlMatcher) -> Result<(), String> {
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

  /// Get the center coordinates of this element after scrolling it into view.
  /// Returns (x, y) or falls back to (0, 0).
  #[allow(clippy::many_single_char_names)]
  async fn get_center(&self) -> Result<(f64, f64), String> {
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
  async fn send_mouse(
    &self,
    mouse_type: u8,
    button: u8,
    click_count: u32,
    pos_x: f64,
    pos_y: f64,
  ) -> Result<(), String> {
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
  pub async fn click(&self) -> Result<(), String> {
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
  pub async fn dblclick(&self) -> Result<(), String> {
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
  pub async fn hover(&self) -> Result<(), String> {
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
  pub async fn type_str(&self, text: &str) -> Result<(), String> {
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
