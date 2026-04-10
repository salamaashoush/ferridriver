//! BiDi page -- implements the full ferridriver page API over the BiDi protocol.
//!
//! Each method maps to one or more BiDi commands. Navigation uses BiDi's built-in
//! `wait` parameter for lifecycle synchronization (no register-before-navigate race).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine;
use rustc_hash::FxHashMap;
use serde_json::json;
use tokio::sync::RwLock;

use super::element::BidiElement;
use super::input;
use super::session::BidiSession;
use super::types::EvaluateResult;
use crate::backend::{
  AnyElement, AxNodeData, AxProperty, CookieData, FrameInfo, ImageFormat, MetricData, NavLifecycle, ScreenshotOpts,
};
use crate::events::{EventEmitter, NetResponse, PageEvent};
use crate::state::{ConsoleMsg, DialogEvent, NetRequest};

/// Page handle for the BiDi backend. Cheaply cloneable (Arc-based).
#[derive(Clone)]
pub struct BidiPage {
  pub(crate) session: Arc<BidiSession>,
  pub(crate) context_id: String,
  pub events: EventEmitter,
  routes: Arc<RwLock<Vec<crate::route::RegisteredRoute>>>,
  intercept_ids: Arc<RwLock<Vec<String>>>,
  closed: Arc<AtomicBool>,
  preload_scripts: Arc<RwLock<FxHashMap<String, String>>>,
  pub exposed_fns: Arc<RwLock<FxHashMap<String, crate::events::ExposedFn>>>,
  pub dialog_handler: Arc<RwLock<crate::events::DialogHandler>>,
}

impl BidiPage {
  /// Create a new BidiPage and enable required domains (inject engine, etc.).
  /// This is the BiDi equivalent of CDP's `enable_domains()`.
  pub(crate) async fn create(session: Arc<BidiSession>, context_id: String) -> Result<Self, String> {
    let page = Self {
      session,
      context_id,
      events: EventEmitter::new(),
      routes: Arc::new(RwLock::new(Vec::new())),
      intercept_ids: Arc::new(RwLock::new(Vec::new())),
      closed: Arc::new(AtomicBool::new(false)),
      preload_scripts: Arc::new(RwLock::new(FxHashMap::default())),
      exposed_fns: Arc::new(RwLock::new(FxHashMap::default())),
      dialog_handler: Arc::new(RwLock::new(crate::events::default_dialog_handler())),
    };

    page.enable_domains().await?;
    Ok(page)
  }

  /// Enable required BiDi domains on this page context.
  /// Injects the ferridriver engine JS (selector helpers, click guards, actionability).
  /// This mirrors CDP's `enable_domains()` which fires `Page.addScriptToEvaluateOnNewDocument`
  /// + domain enables in parallel.
  async fn enable_domains(&self) -> Result<(), String> {
    let engine_js = crate::selectors::build_inject_js();

    // Fire both in parallel: preload script registration + immediate evaluation
    let (preload_result, eval_result) = tokio::join!(
      self.cmd(
        "script.addPreloadScript",
        json!({
          "functionDeclaration": format!("() => {{ {engine_js} }}"),
          "contexts": [self.context_id]
        }),
      ),
      self.cmd(
        "script.evaluate",
        json!({
          "expression": engine_js,
          "target": {"context": self.context_id},
          "awaitPromise": false,
          "resultOwnership": "none"
        }),
      ),
    );
    preload_result?;
    // eval_result can fail on about:blank, that's ok
    let _ = eval_result;
    Ok(())
  }

  /// Helper: send a BiDi command.
  async fn cmd(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    self.session.transport.send_command(method, params).await
  }

  /// Map NavLifecycle to BiDi readiness state.
  fn lifecycle_to_wait(lifecycle: NavLifecycle) -> &'static str {
    match lifecycle {
      NavLifecycle::Commit => "none",
      NavLifecycle::DomContentLoaded => "interactive",
      NavLifecycle::Load => "complete",
    }
  }

  /// Helper: evaluate JS and parse the result.
  async fn eval_internal(&self, expression: &str, context: &str) -> Result<Option<serde_json::Value>, String> {
    let result = self
      .cmd(
        "script.evaluate",
        json!({
          "expression": expression,
          "target": {"context": context},
          "awaitPromise": true,
          "resultOwnership": "none"
        }),
      )
      .await?;

    let eval_result: EvaluateResult =
      serde_json::from_value(result).map_err(|e| format!("BiDi evaluate parse: {e}"))?;

    match eval_result {
      EvaluateResult::Success { result } => Ok(result.to_json()),
      EvaluateResult::Exception { exception_details } => Err(format!("JS error: {}", exception_details.text)),
    }
  }

  // ── Frames ──────────────────────────────────────────────────────────────

  pub async fn get_frame_tree(&self) -> Result<Vec<FrameInfo>, String> {
    let result = self
      .cmd("browsingContext.getTree", json!({"root": self.context_id}))
      .await?;
    let contexts = result
      .get("contexts")
      .and_then(|v| v.as_array())
      .ok_or("getTree: missing contexts")?;

    let mut frames = Vec::new();
    for ctx in contexts {
      collect_frames(ctx, None, &mut frames);
    }

    // BiDi doesn't include frame names in the context tree.
    // Resolve names in parallel by evaluating `window.name` in each child frame.
    let child_indices: Vec<usize> = frames
      .iter()
      .enumerate()
      .filter(|(_, f)| f.parent_frame_id.is_some() && f.name.is_empty())
      .map(|(i, _)| i)
      .collect();
    if !child_indices.is_empty() {
      let futs: Vec<_> = child_indices
        .iter()
        .map(|&i| self.eval_internal("window.name", &frames[i].frame_id))
        .collect();
      let results = futures::future::join_all(futs).await;
      for (idx, result) in child_indices.into_iter().zip(results) {
        if let Ok(Some(val)) = result {
          if let Some(name) = val.as_str() {
            frames[idx].name = name.to_string();
          }
        }
      }
    }
    Ok(frames)
  }

  pub async fn evaluate_in_frame(&self, expression: &str, frame_id: &str) -> Result<Option<serde_json::Value>, String> {
    // In BiDi, frames ARE browsing contexts
    self.eval_internal(expression, frame_id).await
  }

  // ── Navigation ──────────────────────────────────────────────────────────

  pub async fn goto(&self, url: &str, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<(), String> {
    let wait = Self::lifecycle_to_wait(lifecycle);
    let result = tokio::time::timeout(
      std::time::Duration::from_millis(timeout_ms),
      self.cmd(
        "browsingContext.navigate",
        json!({
          "context": self.context_id,
          "url": url,
          "wait": wait
        }),
      ),
    )
    .await;

    match result {
      Ok(Ok(_)) => Ok(()),
      Ok(Err(e)) => Err(e),
      Err(_) => Err(format!("Navigation to '{url}' timed out after {timeout_ms}ms")),
    }
  }

  pub async fn wait_for_navigation(&self) -> Result<(), String> {
    // Subscribe to load event for this context and wait for it
    let mut rx = self.session.transport.subscribe_events();
    let ctx = self.context_id.clone();
    let timeout = tokio::time::timeout(std::time::Duration::from_secs(30), async move {
      while let Ok(event) = rx.recv().await {
        if event.method == "browsingContext.load" {
          if let Some(c) = event.params.get("context").and_then(|v| v.as_str()) {
            if c == ctx {
              return Ok(());
            }
          }
        }
      }
      Err("Event channel closed".to_string())
    });
    match timeout.await {
      Ok(result) => result,
      Err(_) => Err("wait_for_navigation timed out after 30s".into()),
    }
  }

  pub async fn reload(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<(), String> {
    let wait = Self::lifecycle_to_wait(lifecycle);
    let result = tokio::time::timeout(
      std::time::Duration::from_millis(timeout_ms),
      self.cmd(
        "browsingContext.reload",
        json!({
          "context": self.context_id,
          "wait": wait
        }),
      ),
    )
    .await;

    match result {
      Ok(Ok(_)) => Ok(()),
      Ok(Err(e)) => Err(e),
      Err(_) => Err(format!("Reload timed out after {timeout_ms}ms")),
    }
  }

  pub async fn go_back(&self, _lifecycle: NavLifecycle, timeout_ms: u64) -> Result<(), String> {
    let result = tokio::time::timeout(
      std::time::Duration::from_millis(timeout_ms),
      self.cmd(
        "browsingContext.traverseHistory",
        json!({
          "context": self.context_id,
          "delta": -1
        }),
      ),
    )
    .await;

    match result {
      Ok(Ok(_)) => Ok(()),
      Ok(Err(e)) => Err(e),
      Err(_) => Err("go_back timed out".into()),
    }
  }

  pub async fn go_forward(&self, _lifecycle: NavLifecycle, timeout_ms: u64) -> Result<(), String> {
    let result = tokio::time::timeout(
      std::time::Duration::from_millis(timeout_ms),
      self.cmd(
        "browsingContext.traverseHistory",
        json!({
          "context": self.context_id,
          "delta": 1
        }),
      ),
    )
    .await;

    match result {
      Ok(Ok(_)) => Ok(()),
      Ok(Err(e)) => Err(e),
      Err(_) => Err("go_forward timed out".into()),
    }
  }

  pub async fn url(&self) -> Result<Option<String>, String> {
    self
      .eval_internal("location.href", &self.context_id)
      .await
      .map(|v| v.and_then(|val| val.as_str().map(String::from)))
  }

  pub async fn title(&self) -> Result<Option<String>, String> {
    self
      .eval_internal("document.title", &self.context_id)
      .await
      .map(|v| v.and_then(|val| val.as_str().map(String::from)))
  }

  // ── JavaScript ──────────────────────────────────────────────────────────

  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>, String> {
    self.eval_internal(expression, &self.context_id).await
  }

  // ── Elements ────────────────────────────────────────────────────────────

  pub async fn find_element(&self, selector: &str) -> Result<AnyElement, String> {
    let result = self
      .cmd(
        "browsingContext.locateNodes",
        json!({
          "context": self.context_id,
          "locator": {"type": "css", "value": selector},
          "maxNodeCount": 1
        }),
      )
      .await?;

    let nodes = result
      .get("nodes")
      .and_then(|v| v.as_array())
      .ok_or(format!("No element found for selector: {selector}"))?;

    if nodes.is_empty() {
      return Err(format!("No element found for selector: {selector}"));
    }

    let node = &nodes[0];
    let shared_id = node
      .get("sharedId")
      .and_then(|v| v.as_str())
      .ok_or("Element missing sharedId")?
      .to_string();

    Ok(AnyElement::Bidi(BidiElement::new(
      self.session.clone(),
      self.context_id.clone(),
      shared_id,
    )))
  }

  pub async fn evaluate_to_element(&self, js: &str) -> Result<AnyElement, String> {
    // The JS can be either an expression (e.g. "window.__fd.selOne(...)") or a function.
    // Use script.evaluate for expressions, script.callFunction for functions.
    let is_function = js.trim_start().starts_with("function") || js.trim_start().starts_with("(");

    let result = if is_function {
      self
        .cmd(
          "script.callFunction",
          json!({
            "functionDeclaration": js,
            "target": {"context": self.context_id},
            "awaitPromise": true,
            "resultOwnership": "root"
          }),
        )
        .await?
    } else {
      self
        .cmd(
          "script.evaluate",
          json!({
            "expression": js,
            "target": {"context": self.context_id},
            "awaitPromise": true,
            "resultOwnership": "root"
          }),
        )
        .await?
    };

    let eval_result: EvaluateResult =
      serde_json::from_value(result).map_err(|e| format!("BiDi evaluate_to_element parse: {e}"))?;

    match eval_result {
      EvaluateResult::Success { result: remote_val } => {
        let shared_ref = remote_val
          .as_shared_reference()
          .ok_or("evaluate_to_element: result is not a DOM node")?;
        Ok(AnyElement::Bidi(BidiElement::new(
          self.session.clone(),
          self.context_id.clone(),
          shared_ref.shared_id,
        )))
      },
      EvaluateResult::Exception { exception_details } => {
        Err(format!("JS error in evaluate_to_element: {}", exception_details.text))
      },
    }
  }

  // ── Content ─────────────────────────────────────────────────────────────

  pub async fn content(&self) -> Result<String, String> {
    let result = self
      .eval_internal("document.documentElement.outerHTML", &self.context_id)
      .await?;
    Ok(result.and_then(|v| v.as_str().map(String::from)).unwrap_or_default())
  }

  pub async fn set_content(&self, html: &str) -> Result<(), String> {
    self
      .cmd(
        "script.callFunction",
        json!({
          "functionDeclaration": "(html) => { document.open(); document.write(html); document.close(); }",
          "target": {"context": self.context_id},
          "arguments": [{"type": "string", "value": html}],
          "awaitPromise": false,
          "resultOwnership": "none"
        }),
      )
      .await?;
    Ok(())
  }

  // ── Screenshots ─────────────────────────────────────────────────────────

  pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>, String> {
    let format_type = match opts.format {
      ImageFormat::Png => "image/png",
      ImageFormat::Jpeg => "image/jpeg",
      ImageFormat::Webp => "image/webp",
    };
    let quality = opts.quality.map(|q| q as f64 / 100.0);
    let origin = if opts.full_page { "document" } else { "viewport" };

    let mut params = json!({
      "context": self.context_id,
      "origin": origin,
      "format": {
        "type": format_type
      }
    });
    if let Some(q) = quality {
      params["format"]["quality"] = json!(q);
    }

    let result = self.cmd("browsingContext.captureScreenshot", params).await?;
    let data_str = result
      .get("data")
      .and_then(|v| v.as_str())
      .ok_or("Screenshot: missing data")?;
    base64::engine::general_purpose::STANDARD
      .decode(data_str)
      .map_err(|e| format!("Screenshot base64 decode: {e}"))
  }

  pub async fn screenshot_element(&self, selector: &str, format: ImageFormat) -> Result<Vec<u8>, String> {
    // Find the element first
    let elem = self.find_element(selector).await?;
    let shared_id = match &elem {
      AnyElement::Bidi(e) => &e.shared_id,
      _ => return Err("Unexpected element type".into()),
    };

    let format_type = match format {
      ImageFormat::Png => "image/png",
      ImageFormat::Jpeg => "image/jpeg",
      ImageFormat::Webp => "image/webp",
    };

    let result = self
      .cmd(
        "browsingContext.captureScreenshot",
        json!({
          "context": self.context_id,
          "format": {"type": format_type},
          "clip": {"type": "element", "element": {"sharedId": shared_id}}
        }),
      )
      .await?;

    let data_str = result
      .get("data")
      .and_then(|v| v.as_str())
      .ok_or("Screenshot: missing data")?;
    base64::engine::general_purpose::STANDARD
      .decode(data_str)
      .map_err(|e| format!("Screenshot base64 decode: {e}"))
  }

  // ── Accessibility ───────────────────────────────────────────────────────

  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>, String> {
    self.accessibility_tree_with_depth(-1).await
  }

  pub async fn accessibility_tree_with_depth(&self, max_depth: i32) -> Result<Vec<AxNodeData>, String> {
    // Use the shared JS accessibility tree helper from window.__fd.accessibilityTree().
    // This uses Playwright's ARIA role/name computation and tags elements with data-fdref
    // for ref resolution. Shared between BiDi and WebKit backends.
    let result = self
      .eval_internal(
        &format!("JSON.stringify(window.__fd.accessibilityTree({max_depth}))"),
        &self.context_id,
      )
      .await?;

    let json_str = result
      .and_then(|v| v.as_str().map(String::from))
      .unwrap_or_else(|| "[]".into());
    let arr: Vec<serde_json::Value> =
      serde_json::from_str(&json_str).map_err(|e| format!("accessibility_tree parse: {e}"))?;

    let mut nodes = Vec::with_capacity(arr.len());
    for item in &arr {
      let mut properties = Vec::new();
      // Extract rich properties from the JS helper
      if let Some(checked) = item.get("checked").and_then(|v| v.as_str()) {
        if !checked.is_empty() {
          properties.push(AxProperty {
            name: "checked".into(),
            value: Some(serde_json::Value::String(checked.into())),
          });
        }
      }
      if item.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false) {
        properties.push(AxProperty {
          name: "disabled".into(),
          value: Some(serde_json::Value::Bool(true)),
        });
      }
      if item.get("readonly").and_then(|v| v.as_bool()).unwrap_or(false) {
        properties.push(AxProperty {
          name: "readonly".into(),
          value: Some(serde_json::Value::Bool(true)),
        });
      }
      let level = item.get("level").and_then(|v| v.as_i64()).unwrap_or(0);
      if level > 0 {
        properties.push(AxProperty {
          name: "level".into(),
          value: Some(serde_json::json!(level)),
        });
      }
      if let Some(expanded) = item.get("expanded").and_then(|v| v.as_str()) {
        if !expanded.is_empty() {
          properties.push(AxProperty {
            name: "expanded".into(),
            value: Some(serde_json::Value::String(expanded.into())),
          });
        }
      }
      if item.get("required").and_then(|v| v.as_bool()).unwrap_or(false) {
        properties.push(AxProperty {
          name: "required".into(),
          value: Some(serde_json::Value::Bool(true)),
        });
      }
      if let Some(url) = item.get("url").and_then(|v| v.as_str()) {
        if !url.is_empty() {
          properties.push(AxProperty {
            name: "url".into(),
            value: Some(serde_json::Value::String(url.into())),
          });
        }
      }

      nodes.push(AxNodeData {
        node_id: item.get("nodeId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        parent_id: item.get("parentId").and_then(|v| v.as_str()).map(String::from),
        backend_dom_node_id: item.get("backendId").and_then(|v| v.as_i64()),
        ignored: item.get("ignored").and_then(|v| v.as_bool()).unwrap_or(false),
        role: item.get("role").and_then(|v| v.as_str()).map(String::from),
        name: item.get("name").and_then(|v| v.as_str()).map(String::from),
        description: item.get("description").and_then(|v| v.as_str()).map(String::from),
        properties,
      });
    }
    Ok(nodes)
  }

  // ── Input ───────────────────────────────────────────────────────────────

  pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
    self
      .cmd("input.performActions", input::click(&self.context_id, x, y))
      .await?;
    Ok(())
  }

  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<(), String> {
    let btn = input::button_name_to_id(button);
    self
      .cmd(
        "input.performActions",
        input::click_button(&self.context_id, x, y, btn, click_count),
      )
      .await?;
    Ok(())
  }

  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
    self
      .cmd("input.performActions", input::pointer_move(&self.context_id, x, y))
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
    self
      .cmd(
        "input.performActions",
        input::pointer_move_smooth(&self.context_id, from_x, from_y, to_x, to_y, steps),
      )
      .await?;
    Ok(())
  }

  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<(), String> {
    self
      .cmd(
        "input.performActions",
        input::wheel_scroll(&self.context_id, delta_x, delta_y),
      )
      .await?;
    Ok(())
  }

  pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    let btn = input::button_name_to_id(button);
    self
      .cmd("input.performActions", input::mouse_down(&self.context_id, x, y, btn))
      .await?;
    Ok(())
  }

  pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    let btn = input::button_name_to_id(button);
    self
      .cmd("input.performActions", input::mouse_up(&self.context_id, x, y, btn))
      .await?;
    Ok(())
  }

  pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64)) -> Result<(), String> {
    self
      .cmd(
        "input.performActions",
        input::click_and_drag(&self.context_id, from, to),
      )
      .await?;
    Ok(())
  }

  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    self
      .cmd("input.performActions", input::type_text(&self.context_id, text))
      .await?;
    Ok(())
  }

  pub async fn press_key(&self, key: &str) -> Result<(), String> {
    self
      .cmd("input.performActions", input::press_key(&self.context_id, key))
      .await?;
    Ok(())
  }

  // ── Cookies ─────────────────────────────────────────────────────────────

  pub async fn get_cookies(&self) -> Result<Vec<CookieData>, String> {
    let result = self
      .cmd(
        "storage.getCookies",
        json!({
          "partition": {"type": "context", "context": self.context_id}
        }),
      )
      .await?;

    let cookies = result
      .get("cookies")
      .and_then(|v| v.as_array())
      .ok_or("getCookies: missing cookies array")?;

    let mut out = Vec::with_capacity(cookies.len());
    for c in cookies {
      out.push(parse_bidi_cookie(c));
    }
    Ok(out)
  }

  pub async fn set_cookie(&self, cookie: CookieData) -> Result<(), String> {
    let mut cookie_obj = json!({
      "name": cookie.name,
      "value": {"type": "string", "value": cookie.value},
      "domain": cookie.domain,
      "path": cookie.path
    });
    if cookie.secure {
      cookie_obj["secure"] = json!(true);
    }
    if cookie.http_only {
      cookie_obj["httpOnly"] = json!(true);
    }
    if let Some(expires) = cookie.expires {
      cookie_obj["expiry"] = json!(expires as u64);
    }
    if let Some(ref ss) = cookie.same_site {
      cookie_obj["sameSite"] = json!(ss.as_str().to_lowercase());
    }

    self
      .cmd(
        "storage.setCookie",
        json!({
          "cookie": cookie_obj,
          "partition": {"type": "context", "context": self.context_id}
        }),
      )
      .await?;
    Ok(())
  }

  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<(), String> {
    let mut filter = json!({"name": name});
    if let Some(d) = domain {
      filter["domain"] = json!(d);
    }
    self
      .cmd(
        "storage.deleteCookies",
        json!({
          "filter": filter,
          "partition": {"type": "context", "context": self.context_id}
        }),
      )
      .await?;
    Ok(())
  }

  pub async fn clear_cookies(&self) -> Result<(), String> {
    self
      .cmd(
        "storage.deleteCookies",
        json!({
          "partition": {"type": "context", "context": self.context_id}
        }),
      )
      .await?;
    Ok(())
  }

  // ── Emulation ───────────────────────────────────────────────────────────

  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
    let mut params = json!({
      "context": self.context_id,
      "viewport": {
        "width": config.width,
        "height": config.height
      }
    });
    if config.device_scale_factor > 0.0 {
      params["devicePixelRatio"] = json!(config.device_scale_factor);
    }
    self.cmd("browsingContext.setViewport", params).await?;
    Ok(())
  }

  pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
    self
      .cmd(
        "emulation.setUserAgentOverride",
        json!({
          "contexts": [self.context_id],
          "value": ua
        }),
      )
      .await?;
    Ok(())
  }

  pub async fn set_geolocation(&self, lat: f64, lng: f64, accuracy: f64) -> Result<(), String> {
    self
      .cmd(
        "emulation.setGeolocationOverride",
        json!({
          "contexts": [self.context_id],
          "coordinates": {
            "latitude": lat,
            "longitude": lng,
            "accuracy": accuracy
          }
        }),
      )
      .await?;
    Ok(())
  }

  pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
    self
      .cmd(
        "emulation.setLocaleOverride",
        json!({
          "contexts": [self.context_id],
          "locales": [locale]
        }),
      )
      .await?;
    Ok(())
  }

  pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
    self
      .cmd(
        "emulation.setTimezoneOverride",
        json!({
          "contexts": [self.context_id],
          "timezoneId": timezone_id
        }),
      )
      .await?;
    Ok(())
  }

  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<(), String> {
    // BiDi has setForcedColorsModeThemeOverride for color scheme
    if let Some(ref color_scheme) = opts.color_scheme {
      let theme = match color_scheme.as_str() {
        "dark" => "dark",
        "light" => "light",
        "no-preference" => "no-preference",
        _ => "no-preference",
      };
      let _ = self
        .cmd(
          "emulation.setForcedColorsModeThemeOverride",
          json!({
            "contexts": [self.context_id],
            "colorScheme": theme
          }),
        )
        .await;
    }
    // Media type requires JS workaround (no direct BiDi command)
    if let Some(ref media) = opts.media {
      let js = format!(
        r#"() => {{
          const style = document.createElement('style');
          style.setAttribute('media', '{media}');
          style.textContent = '/* emulate media */';
          document.head.appendChild(style);
        }}"#
      );
      let _ = self
        .cmd(
          "script.callFunction",
          json!({
            "functionDeclaration": js,
            "target": {"context": self.context_id},
            "awaitPromise": false,
            "resultOwnership": "none"
          }),
        )
        .await;
    }
    Ok(())
  }

  pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<(), String> {
    self
      .cmd(
        "emulation.setScriptingEnabled",
        json!({
          "contexts": [self.context_id],
          "enabled": enabled
        }),
      )
      .await?;
    Ok(())
  }

  pub async fn set_extra_http_headers(&self, headers: &FxHashMap<String, String>) -> Result<(), String> {
    let header_list: Vec<serde_json::Value> = headers
      .iter()
      .map(|(k, v)| {
        json!({
          "name": k,
          "value": {"type": "string", "value": v}
        })
      })
      .collect();

    self
      .cmd(
        "network.setExtraHeaders",
        json!({
          "contexts": [self.context_id],
          "headers": header_list
        }),
      )
      .await?;
    Ok(())
  }

  pub async fn grant_permissions(&self, _permissions: &[String], _origin: Option<&str>) -> Result<(), String> {
    // BiDi has no permissions API -- use JS Permissions API as best-effort
    Err("Permissions API not available in BiDi backend".into())
  }

  pub async fn reset_permissions(&self) -> Result<(), String> {
    Err("Permissions API not available in BiDi backend".into())
  }

  pub async fn set_focus_emulation_enabled(&self, _enabled: bool) -> Result<(), String> {
    // Activate the browsing context to give it focus
    let _ = self
      .cmd("browsingContext.activate", json!({"context": self.context_id}))
      .await;
    Ok(())
  }

  // ── Network ─────────────────────────────────────────────────────────────

  pub async fn set_network_state(&self, offline: bool, latency: f64, download: f64, upload: f64) -> Result<(), String> {
    self
      .cmd(
        "emulation.setNetworkConditions",
        json!({
          "contexts": [self.context_id],
          "offline": offline,
          "latency": latency,
          "downloadThroughput": download,
          "uploadThroughput": upload
        }),
      )
      .await?;
    Ok(())
  }

  // ── Tracing ─────────────────────────────────────────────────────────────

  pub async fn start_tracing(&self) -> Result<(), String> {
    Err("Tracing not supported on BiDi backend".into())
  }

  pub async fn stop_tracing(&self) -> Result<(), String> {
    Err("Tracing not supported on BiDi backend".into())
  }

  pub async fn metrics(&self) -> Result<Vec<MetricData>, String> {
    Err("Performance metrics not supported on BiDi backend".into())
  }

  // ── Ref resolution ──────────────────────────────────────────────────────

  pub async fn resolve_backend_node(&self, backend_node_id: i64, _ref_id: &str) -> Result<AnyElement, String> {
    // Resolve via data-fdref attribute (set during accessibility tree walk)
    self.find_element(&format!("[data-fdref='{backend_node_id}']")).await
  }

  // ── Event listeners ─────────────────────────────────────────────────────

  pub fn attach_listeners(
    &self,
    console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
    network_log: Arc<RwLock<Vec<NetRequest>>>,
    dialog_log: Arc<RwLock<Vec<DialogEvent>>>,
  ) {
    let mut rx = self.session.transport.subscribe_events();
    let ctx = self.context_id.clone();
    let dialog_handler = self.dialog_handler.clone();
    let session = self.session.clone();
    let closed = self.closed.clone();
    let emitter = self.events.clone();

    tokio::spawn(async move {
      while let Ok(event) = rx.recv().await {
        // Filter events for this context
        let event_ctx = event.params.get("context").and_then(|v| v.as_str()).unwrap_or("");
        if event_ctx != ctx && !event_ctx.is_empty() {
          continue;
        }

        match event.method.as_str() {
          "log.entryAdded" => {
            let level = event.params.get("level").and_then(|v| v.as_str()).unwrap_or("log");
            let text = event.params.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let msg = ConsoleMsg {
              level: level.to_string(),
              text: text.to_string(),
            };
            console_log.write().await.push(msg.clone());
            emitter.emit(PageEvent::Console(msg));
          },
          "network.beforeRequestSent" => {
            if let Some(req) = event.params.get("request") {
              let url = req.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
              let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("GET").to_string();
              let id = req.get("request").and_then(|v| v.as_str()).unwrap_or("").to_string();

              // Defer header parsing — only parse when there are active event subscribers.
              // Most requests don't need full header parsing on the hot path.
              let has_listeners = emitter.receiver_count() > 0;
              let headers = if has_listeners {
                req.get("headers").map(parse_bidi_headers)
              } else {
                None
              };

              let resource_type = event
                .params
                .get("initiator")
                .and_then(|i| i.get("type"))
                .and_then(|v| v.as_str())
                .map(|t| match t {
                  "parser" => "Document",
                  "script" => "Script",
                  "preflight" => "Preflight",
                  other => other,
                })
                .unwrap_or("")
                .to_string();

              let post_data = req
                .get("body")
                .and_then(|b| b.get("value"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

              let mime_type =
                headers.as_ref().and_then(|h| h.get("content-type").or_else(|| h.get("Content-Type")).cloned());

              let net_request = NetRequest {
                id,
                url,
                method,
                status: None,
                resource_type,
                mime_type,
                headers,
                post_data,
              };
              if has_listeners {
                emitter.emit(PageEvent::Request(net_request.clone()));
              }
              network_log.write().await.push(net_request);
            }
          },
          "network.responseCompleted" => {
            let response = &event.params.get("response");
            let request = &event.params.get("request");
            if let (Some(resp), Some(req)) = (response, request) {
              let request_id = req.get("request").and_then(|v| v.as_str()).unwrap_or("");
              let status = resp.get("status").and_then(|v| v.as_i64());
              let status_text = resp.get("statusText").and_then(|v| v.as_str()).unwrap_or("").to_string();
              let mime_type = resp.get("mimeType").and_then(|v| v.as_str()).unwrap_or("").to_string();
              let url = resp.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();

              // Extract response headers from BiDi format
              let headers = resp.get("headers").map(parse_bidi_headers);

              // Update existing network log entry
              let mut log = network_log.write().await;
              if let Some(entry) = log.iter_mut().find(|e| e.id == request_id) {
                entry.status = status;
                entry.mime_type = Some(mime_type.clone());
              }

              emitter.emit(PageEvent::Response(NetResponse {
                request_id: request_id.to_string(),
                url,
                status: status.unwrap_or(0),
                status_text,
                mime_type,
                headers,
              }));
            }
          },
          "browsingContext.userPromptOpened" => {
            let prompt_type = event.params.get("type").and_then(|v| v.as_str()).unwrap_or("alert");
            let message = event.params.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let default_value = event.params.get("defaultValue").and_then(|v| v.as_str());

            // Call the dialog handler to decide action
            let handler = dialog_handler.read().await;
            let pending = crate::events::PendingDialog {
              dialog_type: prompt_type.to_string(),
              message: message.to_string(),
              default_value: default_value.unwrap_or("").to_string(),
            };
            emitter.emit(PageEvent::Dialog(pending.clone()));
            let action = handler(&pending);

            let (accept, text) = match action {
              crate::events::DialogAction::Accept(text) => (true, text),
              crate::events::DialogAction::Dismiss => (false, None),
            };

            let action_str = if accept { "accept" } else { "dismiss" };
            dialog_log.write().await.push(DialogEvent {
              dialog_type: prompt_type.to_string(),
              message: message.to_string(),
              action: action_str.to_string(),
            });

            let mut handle_params = json!({
              "context": ctx,
              "accept": accept
            });
            if let Some(t) = text {
              handle_params["userText"] = json!(t);
            }

            let _ = session
              .transport
              .send_command("browsingContext.handleUserPrompt", handle_params)
              .await;
          },
          "browsingContext.contextDestroyed" => {
            closed.store(true, Ordering::Relaxed);
            emitter.emit(PageEvent::Close);
          },
          _ => {},
        }
      }
    });
  }

  // ── Element screenshot ──────────────────────────────────────────────────

  // (Handled above in screenshot_element)

  // ── PDF ─────────────────────────────────────────────────────────────────

  pub async fn pdf(&self, landscape: bool, print_background: bool) -> Result<Vec<u8>, String> {
    let result = self
      .cmd(
        "browsingContext.print",
        json!({
          "context": self.context_id,
          "landscape": landscape,
          "background": print_background
        }),
      )
      .await?;

    let data_str = result.get("data").and_then(|v| v.as_str()).ok_or("PDF: missing data")?;
    base64::engine::general_purpose::STANDARD
      .decode(data_str)
      .map_err(|e| format!("PDF base64 decode: {e}"))
  }

  // ── Screencast (not supported) ──────────────────────────────────────────

  /// Start screencast via repeated screenshots + event-driven captures.
  /// BiDi has no native screencast API. We combine polling at ~15 fps with
  /// captures on navigation/load events to ensure key visual transitions
  /// are recorded even for fast tests.
  pub async fn start_screencast(
    &self,
    quality: u8,
    _max_width: u32,
    _max_height: u32,
  ) -> Result<tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, f64)>, String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let session = self.session.clone();
    let ctx_id = self.context_id.clone();
    let closed = self.closed.clone();

    // Subscribe to navigation events to capture frames at key moments.
    let mut event_rx = self.session.transport.subscribe_events();
    let event_notify = Arc::new(tokio::sync::Notify::new());
    let event_notify2 = event_notify.clone();
    let event_ctx = self.context_id.clone();

    tokio::spawn(async move {
      while let Ok(event) = event_rx.recv().await {
        let is_relevant = matches!(
          event.method.as_str(),
          "browsingContext.load" | "browsingContext.domContentLoaded" | "browsingContext.navigationCommitted"
        );
        if is_relevant {
          if let Some(c) = event.params.get("context").and_then(|v| v.as_str()) {
            if c == event_ctx {
              event_notify2.notify_one();
            }
          }
        }
      }
    });

    tokio::spawn(async move {
      let target_interval = std::time::Duration::from_millis(66); // ~15 fps
      let capture_params = json!({
        "context": ctx_id,
        "format": {"type": "image/jpeg", "quality": f64::from(quality) / 100.0},
        "origin": "viewport"
      });

      loop {
        if closed.load(Ordering::Relaxed) {
          break;
        }
        let frame_start = tokio::time::Instant::now();

        let result = session
          .transport
          .send_command("browsingContext.captureScreenshot", capture_params.clone())
          .await;

        if let Ok(result) = result {
          if let Some(data_str) = result.get("data").and_then(|v| v.as_str()) {
            if let Ok(jpeg_bytes) = base64::engine::general_purpose::STANDARD.decode(data_str) {
              let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
              if tx.send((jpeg_bytes, ts)).is_err() {
                break;
              }
            }
          }
        } else {
          break;
        }

        // Sleep for the remainder of the frame interval, or wake early on
        // navigation events to capture content transitions.
        let elapsed = frame_start.elapsed();
        if elapsed < target_interval {
          let remaining = target_interval - elapsed;
          tokio::select! {
            () = tokio::time::sleep(remaining) => {},
            () = event_notify.notified() => {},
          }
        }
      }
    });

    Ok(rx)
  }

  pub async fn stop_screencast(&self) -> Result<(), String> {
    Ok(())
  }

  // ── File upload ─────────────────────────────────────────────────────────

  pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<(), String> {
    // Find the element, then use input.setFiles
    let elem = self.find_element(selector).await?;
    let shared_id = match &elem {
      AnyElement::Bidi(e) => e.shared_id.clone(),
      _ => return Err("Unexpected element type".into()),
    };

    self
      .cmd(
        "input.setFiles",
        json!({
          "context": self.context_id,
          "element": {"sharedId": shared_id},
          "files": paths
        }),
      )
      .await?;
    Ok(())
  }

  // ── Network Interception ────────────────────────────────────────────────

  pub async fn route(&self, pattern: &str, handler: crate::route::RouteHandler) -> Result<(), String> {
    let regex = crate::route::glob_to_regex(pattern)?;

    let needs_intercept = self.intercept_ids.read().await.is_empty();
    if needs_intercept {
      // Register a single intercept for ALL requests on this context (no urlPatterns).
      // BiDi urlPatterns have limited syntax — filtering happens client-side via regex.
      // This matches Puppeteer's approach.
      let result = self
        .cmd(
          "network.addIntercept",
          json!({
            "phases": ["beforeRequestSent"],
            "contexts": [self.context_id]
          }),
        )
        .await?;

      let intercept_id = result
        .get("intercept")
        .and_then(|v| v.as_str())
        .ok_or("addIntercept: missing intercept id")?
        .to_string();

      self.intercept_ids.write().await.push(intercept_id);

      // Spawn a single listener task for all route handlers on this page
      let mut rx = self.session.transport.subscribe_events();
      let ctx = self.context_id.clone();
      let session = self.session.clone();
      let routes = self.routes.clone();

      tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
          if event.method != "network.beforeRequestSent" {
            continue;
          }
          let event_ctx = event.params.get("context").and_then(|v| v.as_str()).unwrap_or("");
          if event_ctx != ctx {
            continue;
          }
          let is_blocked = event.params.get("isBlocked").and_then(|v| v.as_bool()).unwrap_or(false);
          if !is_blocked {
            continue;
          }

          let req_obj = event.params.get("request");
          let request_id = req_obj
            .and_then(|v| v.get("request"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
          let url = req_obj
            .and_then(|v| v.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

          let matched_handler = {
            let routes_guard = routes.read().await;
            routes_guard
              .iter()
              .find(|r| r.pattern.is_match(url))
              .map(|r| std::sync::Arc::clone(&r.handler))
          };

          if let Some(handler) = matched_handler {
            let method = req_obj
              .and_then(|r| r.get("method"))
              .and_then(|v| v.as_str())
              .unwrap_or("GET");
            let headers: FxHashMap<String, String> = req_obj
              .and_then(|r| r.get("headers"))
              .map(parse_bidi_headers)
              .unwrap_or_default();

            let intercepted = crate::route::InterceptedRequest {
              request_id: request_id.to_string(),
              url: url.to_string(),
              method: method.to_string(),
              headers,
              post_data: None,
              resource_type: String::new(),
            };

            let (tx, action_rx) = tokio::sync::oneshot::channel();
            let route = crate::route::Route::new(intercepted, tx);
            handler(route);
            let action = action_rx.await.unwrap_or(crate::route::RouteAction::Continue(
              crate::route::ContinueOverrides::default(),
            ));
            execute_bidi_route_action(&session.transport, request_id, action).await;
          } else {
            let _ = session
              .transport
              .send_command("network.continueRequest", json!({"request": request_id}))
              .await;
          }
        }
      });
    }

    self.routes.write().await.push(crate::route::RegisteredRoute {
      pattern: regex,
      pattern_str: pattern.to_string(),
      handler,
    });

    Ok(())
  }

  pub async fn unroute(&self, pattern: &str) -> Result<(), String> {
    let mut routes = self.routes.write().await;
    routes.retain(|r| r.pattern_str != pattern);

    // If no routes left, remove the intercept entirely
    if routes.is_empty() {
      let mut ids = self.intercept_ids.write().await;
      for id in ids.drain(..) {
        let _ = self.cmd("network.removeIntercept", json!({"intercept": id})).await;
      }
    }

    Ok(())
  }

  // ── Lifecycle ───────────────────────────────────────────────────────────

  pub async fn close_page(&self) -> Result<(), String> {
    self
      .cmd("browsingContext.close", json!({"context": self.context_id}))
      .await?;
    self.closed.store(true, Ordering::Relaxed);
    Ok(())
  }

  pub fn is_closed(&self) -> bool {
    self.closed.load(Ordering::Relaxed)
  }

  // ── Init Scripts ────────────────────────────────────────────────────────

  pub async fn add_init_script(&self, source: &str) -> Result<String, String> {
    let wrapped = format!("() => {{ {source} }}");
    let result = self
      .cmd(
        "script.addPreloadScript",
        json!({
          "functionDeclaration": wrapped,
          "contexts": [self.context_id]
        }),
      )
      .await?;

    let bidi_id = result
      .get("script")
      .and_then(|v| v.as_str())
      .ok_or("addPreloadScript: missing script id")?
      .to_string();

    // Generate our own stable identifier
    let our_id = format!("init-{}", self.preload_scripts.read().await.len());
    self.preload_scripts.write().await.insert(our_id.clone(), bidi_id);

    Ok(our_id)
  }

  pub async fn remove_init_script(&self, identifier: &str) -> Result<(), String> {
    let bidi_id = self
      .preload_scripts
      .write()
      .await
      .remove(identifier)
      .ok_or(format!("Init script '{identifier}' not found"))?;

    self
      .cmd("script.removePreloadScript", json!({"script": bidi_id}))
      .await?;
    Ok(())
  }

  // ── Exposed Functions ───────────────────────────────────────────────────

  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<(), String> {
    // Inject a global function that sends messages via BiDi channel
    let js = format!(
      r#"() => {{
        window['{name}'] = (...args) => {{
          return new Promise((resolve) => {{
            const id = Math.random().toString(36);
            window.__ferri_exposed = window.__ferri_exposed || {{}};
            window.__ferri_exposed[id] = resolve;
            console.log(JSON.stringify({{__ferri_call: '{name}', id, args}}));
          }});
        }};
      }}"#
    );

    self
      .cmd(
        "script.addPreloadScript",
        json!({
          "functionDeclaration": js,
          "contexts": [self.context_id]
        }),
      )
      .await?;

    // Also execute it now for the current page
    let _ = self
      .cmd(
        "script.callFunction",
        json!({
          "functionDeclaration": js,
          "target": {"context": self.context_id},
          "awaitPromise": false,
          "resultOwnership": "none"
        }),
      )
      .await;

    self.exposed_fns.write().await.insert(name.to_string(), func);
    Ok(())
  }

  pub async fn remove_exposed_function(&self, name: &str) -> Result<(), String> {
    self.exposed_fns.write().await.remove(name);
    let js = format!("delete window['{name}']");
    let _ = self.evaluate(&js).await;
    Ok(())
  }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Recursively collect frame info from context tree.
fn collect_frames(ctx: &serde_json::Value, parent_id: Option<&str>, frames: &mut Vec<FrameInfo>) {
  let context_id = ctx.get("context").and_then(|v| v.as_str()).unwrap_or("");
  let url = ctx.get("url").and_then(|v| v.as_str()).unwrap_or("");

  frames.push(FrameInfo {
    frame_id: context_id.to_string(),
    parent_frame_id: parent_id.map(String::from),
    name: String::new(),
    url: url.to_string(),
  });

  if let Some(children) = ctx.get("children").and_then(|v| v.as_array()) {
    for child in children {
      collect_frames(child, Some(context_id), frames);
    }
  }
}

/// Parse BiDi-format headers `[{name, value: {type, value}}]` into a `FxHashMap`.
fn parse_bidi_headers(headers_val: &serde_json::Value) -> FxHashMap<String, String> {
  headers_val
    .as_array()
    .map(|arr| {
      arr
        .iter()
        .filter_map(|entry| {
          let name = entry.get("name")?.as_str()?;
          let value = entry.get("value").and_then(|v| v.get("value")).and_then(|v| v.as_str()).unwrap_or("");
          Some((name.to_string(), value.to_string()))
        })
        .collect()
    })
    .unwrap_or_default()
}

/// Execute a route action via BiDi network commands.
async fn execute_bidi_route_action(
  transport: &super::transport::BidiTransport,
  request_id: &str,
  action: crate::route::RouteAction,
) {
  match action {
    crate::route::RouteAction::Fulfill(resp) => {
      let body_b64 = base64::engine::general_purpose::STANDARD.encode(&resp.body);
      let mut hdrs: Vec<serde_json::Value> = resp
        .headers
        .iter()
        .map(|(k, v)| json!({"name": k, "value": {"type": "string", "value": v}}))
        .collect();
      if let Some(ct) = &resp.content_type {
        if !hdrs
          .iter()
          .any(|h| h.get("name").and_then(|n| n.as_str()) == Some("content-type"))
        {
          hdrs.push(json!({"name": "content-type", "value": {"type": "string", "value": ct}}));
        }
      }
      let _ = transport
        .send_command(
          "network.provideResponse",
          json!({
            "request": request_id,
            "statusCode": resp.status,
            "reasonPhrase": crate::route::status_text(resp.status),
            "headers": hdrs,
            "body": {"type": "base64", "value": body_b64},
          }),
        )
        .await;
    },
    crate::route::RouteAction::Continue(overrides) => {
      let mut params = json!({"request": request_id});
      if let Some(url) = &overrides.url {
        params["url"] = serde_json::Value::String(url.clone());
      }
      if let Some(method) = &overrides.method {
        params["method"] = serde_json::Value::String(method.clone());
      }
      if let Some(headers) = &overrides.headers {
        let hdrs: Vec<serde_json::Value> = headers
          .iter()
          .map(|(k, v)| json!({"name": k, "value": {"type": "string", "value": v}}))
          .collect();
        params["headers"] = serde_json::Value::Array(hdrs);
      }
      if let Some(post_data) = &overrides.post_data {
        let encoded = base64::engine::general_purpose::STANDARD.encode(post_data);
        params["body"] = json!({"type": "base64", "value": encoded});
      }
      let _ = transport.send_command("network.continueRequest", params).await;
    },
    crate::route::RouteAction::Abort(_reason) => {
      let _ = transport
        .send_command("network.failRequest", json!({"request": request_id}))
        .await;
    },
  }
}

/// Parse a BiDi network cookie into our CookieData.
fn parse_bidi_cookie(c: &serde_json::Value) -> CookieData {
  let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
  let value = c
    .get("value")
    .and_then(|v| {
      // BiDi cookies have {type: "string", value: "..."} format
      v.get("value").and_then(|inner| inner.as_str()).or_else(|| v.as_str())
    })
    .unwrap_or("")
    .to_string();
  let domain = c.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string();
  let path = c.get("path").and_then(|v| v.as_str()).unwrap_or("/").to_string();
  let secure = c.get("secure").and_then(|v| v.as_bool()).unwrap_or(false);
  let http_only = c.get("httpOnly").and_then(|v| v.as_bool()).unwrap_or(false);
  let expires = c.get("expiry").and_then(|v| v.as_f64());
  let same_site = c.get("sameSite").and_then(|v| v.as_str()).and_then(|s| match s {
    "strict" => Some(crate::backend::SameSite::Strict),
    "lax" => Some(crate::backend::SameSite::Lax),
    "none" => Some(crate::backend::SameSite::None),
    _ => None,
  });

  CookieData {
    name,
    value,
    domain,
    path,
    secure,
    http_only,
    expires,
    same_site,
  }
}
