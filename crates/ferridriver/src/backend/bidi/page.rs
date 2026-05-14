//! `BiDi` page -- implements the full ferridriver page API over the `BiDi` protocol.
//!
//! Each method maps to one or more `BiDi` commands. Navigation uses `BiDi`'s built-in
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
use crate::console_message::{ConsoleMessage, ConsoleMessageLocation};
use crate::events::{EventEmitter, PageEvent};
use crate::network::{
  self, BodyFn, RawHeadersFn, RemoteAddr, Request as NetworkRequest, RequestInit, Response, ResponseInit,
  SecurityDetails,
};
use crate::state::DialogEvent;

/// Convert a raw `BiDi` `Script.RemoteValue` JSON payload into a
/// [`crate::js_handle::JSHandleBacking`]. DOM nodes become
/// remote-backed via their `sharedId`; other object-like types
/// (`object`, `array`, `map`, `set`, `function`, `error`, `promise`,
/// `symbol`) use the `handle` slot; primitives inline their value.
/// Mirrors Playwright's `bidiPage.ts::_onLogEntryAdded` which calls
/// `createHandle(context, arg)` — the `BiDi` half of the same handle
/// construction.
fn bidi_remote_value_to_backing(arg: &serde_json::Value) -> crate::js_handle::JSHandleBacking {
  let ty = arg.get("type").and_then(|v| v.as_str()).unwrap_or("");
  if ty == "node" {
    if let Some(shared_id) = arg.get("sharedId").and_then(|v| v.as_str()) {
      let handle = arg
        .get("handle")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
      return crate::js_handle::JSHandleBacking::Remote(crate::js_handle::HandleRemote::Bidi {
        shared_id: shared_id.to_string(),
        handle,
      });
    }
  }
  if matches!(
    ty,
    "object" | "array" | "map" | "set" | "function" | "error" | "promise" | "symbol" | "window" | "weakmap" | "weakset"
  ) {
    if let Some(h) = arg.get("handle").and_then(|v| v.as_str()) {
      return crate::js_handle::JSHandleBacking::Remote(crate::js_handle::HandleRemote::Bidi {
        shared_id: String::new(),
        handle: Some(h.to_string()),
      });
    }
  }
  // Primitive path — fall back to an inline value-backed handle.
  let serialized = match ty {
    "undefined" => crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined),
    "null" => crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Null),
    "bigint" => {
      let s = arg
        .get("value")
        .and_then(|v| v.as_str())
        .map_or_else(String::new, std::string::ToString::to_string);
      crate::protocol::SerializedValue::BigInt(s)
    },
    _ => {
      let value = arg.get("value").cloned().unwrap_or(serde_json::Value::Null);
      let mut ctx = crate::protocol::SerializationContext::default();
      crate::protocol::SerializedValue::from_json(&value, &mut ctx)
    },
  };
  crate::js_handle::JSHandleBacking::Value(serialized)
}

/// Extract a [`ConsoleMessageLocation`] from a `BiDi` log entry's
/// `stackTrace` (first frame) or its `source.realm` (fallback).
/// `BiDi` doesn't always emit a `stackTrace`; when missing Playwright
/// falls back to `{ '', 1, 1 }` (`bidiPage.ts:295`).
fn bidi_stack_trace_to_location(
  stack: Option<&serde_json::Value>,
  _source: Option<&serde_json::Value>,
) -> ConsoleMessageLocation {
  let Some(frame) = stack
    .and_then(|s| s.get("callFrames"))
    .and_then(|v| v.as_array())
    .and_then(|frames| frames.first())
  else {
    return ConsoleMessageLocation {
      url: String::new(),
      line_number: 1,
      column_number: 1,
    };
  };
  ConsoleMessageLocation {
    url: frame.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    line_number: frame
      .get("lineNumber")
      .and_then(serde_json::Value::as_u64)
      .map_or(1, u64_to_u32_saturating),
    column_number: frame
      .get("columnNumber")
      .and_then(serde_json::Value::as_u64)
      .map_or(1, u64_to_u32_saturating),
  }
}

fn u64_to_u32_saturating(n: u64) -> u32 {
  u32::try_from(n).unwrap_or(u32::MAX)
}

/// Split a `BiDi` `log.entryAdded.text` (for `type: 'javascript'`)
/// into `{ name, message }`. Mirrors the `params.text?.includes(': ')`
/// branch in `/tmp/playwright/packages/playwright-core/src/server/bidi/bidiPage.ts:271-277`.
/// If no `': '` separator is present, `name` is empty and `message` is
/// the full `text`.
fn split_error_text(text: &str) -> (String, String) {
  if let Some(idx) = text.find(": ") {
    (text[..idx].to_string(), text[idx + 2..].to_string())
  } else {
    (String::new(), text.to_string())
  }
}

/// Build the `error.stack` string for a `BiDi` JS error. Mirrors
/// `bidiPage.ts:280-283` byte-for-byte: the first line is the original
/// `text`, followed by one `    at <func> (<url>:<line+1>:<col+1>)`
/// line per `stackTrace.callFrames` entry. `BiDi` line / column
/// numbers are 0-based; Playwright adds `+ 1` to match the user-facing
/// 1-based JS convention.
fn build_bidi_stack(text: &str, stack: Option<&serde_json::Value>) -> String {
  use std::fmt::Write as _;
  let mut out = text.to_string();
  let Some(frames) = stack.and_then(|s| s.get("callFrames")).and_then(|v| v.as_array()) else {
    return out;
  };
  for frame in frames {
    let url = frame.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let line = frame.get("lineNumber").and_then(serde_json::Value::as_u64).unwrap_or(0) + 1;
    let col = frame
      .get("columnNumber")
      .and_then(serde_json::Value::as_u64)
      .unwrap_or(0)
      + 1;
    let function_name = frame.get("functionName").and_then(|v| v.as_str()).unwrap_or("");
    out.push('\n');
    // `write!` into a `String` is infallible — ignore the `Result` to
    // avoid the intermediate allocation clippy flags on `push_str(&format!)`.
    if function_name.is_empty() {
      let _ = write!(out, "    at {url}:{line}:{col}");
    } else {
      let _ = write!(out, "    at {function_name} ({url}:{line}:{col})");
    }
  }
  out
}

fn f64_to_u64_saturating(n: f64) -> u64 {
  // Saturating cast — clippy's `cast_possible_truncation`/`cast_sign_loss`
  // fire on a raw `as u64` cast. Manual range check + saturation is the
  // documented workaround. `u64::MAX as f64` is over `2^63` so a
  // `>=` comparison is safe even though f64 can't represent u64::MAX
  // exactly (the comparison uses the closest f64 which is `2^64`).
  #[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
  )]
  let clamped = if !n.is_finite() || n < 0.0 {
    0_u64
  } else if n >= u64::MAX as f64 {
    u64::MAX
  } else {
    n as u64
  };
  clamped
}

/// Page handle for the `BiDi` backend. Cheaply cloneable (Arc-based).
#[derive(Clone)]
pub struct BidiPage {
  pub(crate) session: Arc<BidiSession>,
  pub(crate) context_id: Arc<str>,
  pub events: EventEmitter,
  routes: Arc<RwLock<Vec<crate::route::RegisteredRoute>>>,
  intercept_ids: Arc<RwLock<Vec<String>>>,
  closed: Arc<AtomicBool>,
  preload_scripts: Arc<RwLock<FxHashMap<String, String>>>,
  exposed_fns: Arc<RwLock<FxHashMap<String, crate::events::ExposedFn>>>,
  /// Manager for lazy engine injection.
  injected_script: Arc<InjectedScriptManager>,
  /// Most recent main-document `Request` observed by the `BiDi` network
  /// listener. Populated when `BidiNetworkTracker` sees a request with
  /// `navigation` set. Consumed by `goto` / `reload` / history
  /// traversals to resolve the final main-document `Response`.
  nav_request_slot: crate::network::NavRequestSlot,
  /// Per-page dialog handler registry. See
  /// `crates/ferridriver/src/dialog.rs::DialogManager`.
  pub dialog_manager: crate::dialog::DialogManager,
  /// Per-page file-chooser handler registry. See
  /// `crates/ferridriver/src/file_chooser.rs::FileChooserManager`.
  /// Backend listener dispatches on `input.fileDialogOpened` (the
  /// `BiDi` equivalent of CDP's `Page.fileChooserOpened`).
  pub file_chooser_manager: crate::file_chooser::FileChooserManager,
  /// Per-page download handler registry. See
  /// `crates/ferridriver/src/download.rs::DownloadManager`. Dispatches
  /// on `browsingContext.downloadWillBegin` and flips terminal state
  /// on `browsingContext.downloadEnd` — Firefox's native `BiDi` events.
  pub download_manager: crate::download::DownloadManager,
  /// Per-page temp directory Firefox is configured to write downloads
  /// into (via `browser.setDownloadBehavior({ downloadBehavior: { type:
  /// 'allowed', destinationFolder }, userContexts })`). Held as
  /// `Arc<TempDir>` so the directory lives as long as any `Download`
  /// referencing a file under it.
  pub downloads_dir: Arc<tempfile::TempDir>,
  /// Weak back-reference to the outer [`crate::page::Page`]. Same
  /// purpose as the CDP page's field — the file-chooser listener
  /// upgrades it to build the `ElementHandle`.
  pub page_backref: crate::backend::PageBackref,
  /// Shared frame cache (see `CdpPage::frame_cache` for the rationale —
  /// MCP tool handlers wrap the same backend page in successive
  /// `Arc<crate::page::Page>` instances, so the cache lives on the
  /// backend to outlive them).
  pub(crate) frame_cache: Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  /// Idempotent latch for the frame-event listener.
  pub(crate) frame_listener_started: Arc<AtomicBool>,
}

pub struct InjectedScriptManager {
  injected: AtomicBool,
}

impl InjectedScriptManager {
  fn new() -> Self {
    Self {
      injected: AtomicBool::new(false),
    }
  }

  fn reset(&self) {
    self.injected.store(false, Ordering::Relaxed);
  }

  async fn ensure(&self, page: &BidiPage) -> Result<(), String> {
    if !self.injected.load(Ordering::Relaxed) {
      let full_check_js = crate::selectors::build_lazy_inject_js();
      let _ = page
        .cmd(
          "script.evaluate",
          json!({
            "expression": full_check_js,
            "target": {"context": &*page.context_id},
            "awaitPromise": true,
            "resultOwnership": "none"
          }),
        )
        .await?;
      self.injected.store(true, Ordering::Relaxed);
    }
    Ok(())
  }
}
impl BidiPage {
  /// Create a new `BidiPage` and enable required domains (inject engine, etc.).
  /// This is the `BiDi` equivalent of CDP's `enable_domains()`.
  pub(crate) fn create(session: Arc<BidiSession>, context_id: String) -> Result<Self, String> {
    // BiDi handles navigation-aware injection via script.addPreloadScript.
    // Domain enables are deferred (lazy injection), unlike CDP's upfront enable_domains().
    let downloads_dir = tempfile::Builder::new()
      .prefix("ferridriver-downloads-")
      .tempdir()
      .map_err(|e| format!("downloads tempdir: {e}"))?;
    Ok(Self {
      session,
      context_id: Arc::from(context_id),
      events: EventEmitter::new(),
      routes: Arc::new(RwLock::new(Vec::new())),
      intercept_ids: Arc::new(RwLock::new(Vec::new())),
      closed: Arc::new(AtomicBool::new(false)),
      preload_scripts: Arc::new(RwLock::new(FxHashMap::default())),
      exposed_fns: Arc::new(RwLock::new(FxHashMap::default())),
      injected_script: Arc::new(InjectedScriptManager::new()),
      nav_request_slot: crate::network::NavRequestSlot::new(),
      dialog_manager: crate::dialog::DialogManager::new(),
      file_chooser_manager: crate::file_chooser::FileChooserManager::new(),
      download_manager: crate::download::DownloadManager::new(),
      downloads_dir: Arc::new(downloads_dir),
      page_backref: crate::backend::PageBackref::new(),
      frame_cache: Arc::new(std::sync::Mutex::new(crate::frame_cache::FrameCache::default())),
      frame_listener_started: Arc::new(AtomicBool::new(false)),
    })
  }

  /// Helper: send a `BiDi` command.
  async fn cmd(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    self.session.transport.send_command(method, params).await
  }

  pub(crate) fn is_retryable_context_error(err: &str) -> bool {
    err.contains("DiscardedBrowsingContextError")
      || err.contains("BrowsingContext does no longer exist")
      || err.contains("BiDi error 'no such frame'")
      || err.contains("BiDi error 'no such window'")
  }

  pub async fn wait_until_ready(&self) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);

    loop {
      match self
        .cmd(
          "script.evaluate",
          json!({
            "expression": "document.readyState",
            "target": {"context": &*self.context_id},
            "awaitPromise": true,
            "resultOwnership": "none"
          }),
        )
        .await
      {
        Ok(_) => return Ok(()),
        Err(err) if Self::is_retryable_context_error(&err) && tokio::time::Instant::now() < deadline => {
          tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        },
        Err(err) => return Err(err),
      }
    }
  }

  /// Map `NavLifecycle` to `BiDi` readiness state.
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
      .cmd("browsingContext.getTree", json!({"root": &*self.context_id}))
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
    // Resolve names by locating the iframe element in the PARENT context via
    // `browsingContext.locateNodes` — same path Playwright uses
    // (`bidiBrowser.ts:154` -> `_getFrameNode`). This reads `name`/`id` from
    // the iframe element's HTML attributes, which are populated as soon as
    // the parent parses the `<iframe>` tag — unlike `window.name` on the
    // child, which Firefox can leave empty on srcdoc iframes until the
    // child's global object initialises.
    let child_indices: Vec<usize> = frames
      .iter()
      .enumerate()
      .filter(|(_, f)| f.parent_frame_id.is_some() && f.name.is_empty())
      .map(|(i, _)| i)
      .collect();
    if !child_indices.is_empty() {
      let futs: Vec<_> = child_indices
        .iter()
        .map(|&i| {
          let frame_id = frames[i].frame_id.clone();
          let parent_id = frames[i].parent_frame_id.clone().unwrap_or_default();
          async move {
            self
              .cmd(
                "browsingContext.locateNodes",
                json!({
                  "context": parent_id,
                  "locator": { "type": "context", "value": { "context": frame_id } },
                  "maxNodeCount": 1,
                }),
              )
              .await
          }
        })
        .collect();
      let results = futures::future::join_all(futs).await;
      for (idx, result) in child_indices.into_iter().zip(results) {
        let Ok(value) = result else { continue };
        let Some(node) = value.get("nodes").and_then(|v| v.as_array()).and_then(|a| a.first()) else {
          continue;
        };
        // The locator returns a `NodeRemoteValue`; the iframe element's
        // attributes live in `value.attributes` (Bidi `Script.NodeProperties`).
        let attrs = node.get("value").and_then(|v| v.get("attributes"));
        let name = attrs
          .and_then(|a| a.get("name"))
          .and_then(|v| v.as_str())
          .filter(|s| !s.is_empty())
          .or_else(|| attrs.and_then(|a| a.get("id")).and_then(|v| v.as_str()))
          .unwrap_or("");
        if !name.is_empty() {
          frames[idx].name = name.to_string();
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

  pub async fn goto(
    &self,
    url: &str,
    lifecycle: NavLifecycle,
    timeout_ms: u64,
    referer: Option<&str>,
  ) -> Result<Option<Response>, String> {
    self.injected_script.reset();
    self.nav_request_slot.clear();

    // WebDriver BiDi `browsingContext.navigate` has no `referrer` param —
    // Playwright's own BiDi backend drops it too
    // (`/tmp/playwright/packages/playwright-core/src/server/bidi/bidiPage.ts::navigateFrame`
    // takes a `referrer` arg and never uses it). The honest BiDi analogue
    // is `network.setExtraHeaders` which we set for the duration of this
    // navigation and reset afterwards so it doesn't leak into subsequent
    // requests on the same context.
    let had_referer = referer.is_some();
    if let Some(r) = referer {
      let _ = self
        .cmd(
          "network.setExtraHeaders",
          json!({
            "headers": [{ "name": "Referer", "value": { "type": "string", "value": r } }],
            "contexts": [&*self.context_id],
          }),
        )
        .await;
    }

    let wait = Self::lifecycle_to_wait(lifecycle);
    let result = tokio::time::timeout(
      std::time::Duration::from_millis(timeout_ms),
      self.cmd(
        "browsingContext.navigate",
        json!({
          "context": &*self.context_id,
          "url": url,
          "wait": wait
        }),
      ),
    )
    .await;

    if had_referer {
      let _ = self
        .cmd(
          "network.setExtraHeaders",
          json!({ "headers": [], "contexts": [&*self.context_id] }),
        )
        .await;
    }

    match result {
      Ok(Ok(_)) => Ok(self.await_nav_response().await),
      Ok(Err(e)) => Err(e),
      Err(_) => Err(format!("Navigation to '{url}' timed out after {timeout_ms}ms")),
    }
  }

  /// Resolve the main-document `Response` captured by the network
  /// listener for the most recent navigation. Returns `None` for
  /// same-document navigations (no new request was issued) or when
  /// the underlying request ended in failure.
  async fn await_nav_response(&self) -> Option<Response> {
    let req = self.nav_request_slot.get()?;
    req.response().await.ok().flatten()
  }

  pub async fn wait_for_navigation(&self) -> Result<(), String> {
    // Subscribe to load event for this context and wait for it
    let mut rx = self.session.transport.subscribe_events();
    let ctx = self.context_id.clone();
    let timeout = tokio::time::timeout(std::time::Duration::from_secs(30), async move {
      while let Ok(event) = rx.recv().await {
        if event.method == "browsingContext.load" {
          if let Some(c) = event.params.get("context").and_then(|v| v.as_str()) {
            if c == &*ctx {
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

  pub async fn reload(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>, String> {
    self.injected_script.reset();
    self.nav_request_slot.clear();
    let wait = Self::lifecycle_to_wait(lifecycle);
    let result = tokio::time::timeout(
      std::time::Duration::from_millis(timeout_ms),
      self.cmd(
        "browsingContext.reload",
        json!({
          "context": &*self.context_id,
          "wait": wait
        }),
      ),
    )
    .await;

    match result {
      Ok(Ok(_)) => Ok(self.await_nav_response().await),
      Ok(Err(e)) => Err(e),
      Err(_) => Err(format!("Reload timed out after {timeout_ms}ms")),
    }
  }

  pub async fn go_back(&self, _lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>, String> {
    self.nav_request_slot.clear();
    let result = tokio::time::timeout(
      std::time::Duration::from_millis(timeout_ms),
      self.cmd(
        "browsingContext.traverseHistory",
        json!({
          "context": &*self.context_id,
          "delta": -1
        }),
      ),
    )
    .await;

    match result {
      Ok(Ok(_)) => Ok(self.await_nav_response().await),
      Ok(Err(e)) => Err(e),
      Err(_) => Err("go_back timed out".into()),
    }
  }

  pub async fn go_forward(&self, _lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>, String> {
    self.nav_request_slot.clear();
    let result = tokio::time::timeout(
      std::time::Duration::from_millis(timeout_ms),
      self.cmd(
        "browsingContext.traverseHistory",
        json!({
          "context": &*self.context_id,
          "delta": 1
        }),
      ),
    )
    .await;

    match result {
      Ok(Ok(_)) => Ok(self.await_nav_response().await),
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
    self.eval_internal(expression, &self.context_id).await
  }

  // ── Elements ────────────────────────────────────────────────────────────

  pub async fn find_element(&self, selector: &str) -> Result<AnyElement, String> {
    self.ensure_engine_injected().await?;
    // `find_element` is a non-strict path (used by raw page.locator
    // resolution + a few engine-internal callers); pass strict=false
    // so the engine returns first-match instead of throwing on
    // multi-match.
    let sel_js = crate::selectors::build_selone_js(selector, "window.__fd", false)?;
    self
      .evaluate_to_element(&sel_js, None)
      .await
      .map_err(|_| format!("No element found for selector: {selector}"))
  }

  /// `BiDi`: a "frame" is a browsing context. When `frame_id` is `Some`
  /// it overrides the page's default `context_id` so element resolution
  /// runs inside the iframe's realm. Mirrors Playwright's `BiDi` backend
  /// (`/tmp/playwright/packages/playwright-core/src/server/bidi/bidiFrame.ts`).
  pub async fn evaluate_to_element(&self, js: &str, frame_id: Option<&str>) -> Result<AnyElement, String> {
    // The JS can be either an expression or a function.
    // Use script.evaluate for expressions, script.callFunction for functions.
    let is_function = js.trim_start().starts_with("function") || js.trim_start().starts_with('(');

    let target_ctx: &str = frame_id.unwrap_or(&self.context_id);

    let result = if is_function {
      self
        .cmd(
          "script.callFunction",
          json!({
            "functionDeclaration": js,
            "target": {"context": target_ctx},
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
            "target": {"context": target_ctx},
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
        // Element belongs to the realm we evaluated in.
        let owning_ctx: Arc<str> = match frame_id {
          Some(fid) => Arc::from(fid),
          None => self.context_id.clone(),
        };
        Ok(AnyElement::Bidi(BidiElement::new(
          self.session.clone(),
          owning_ctx,
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
          "target": {"context": &*self.context_id},
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
    // BiDi-specific refusals for knobs Firefox has no protocol for.
    if opts.omit_background {
      return Err("BiDi/Firefox does not support `omitBackground` screenshots — no BiDi command exposes the transparent-background override.".into());
    }
    if matches!(opts.scale, Some(crate::backend::ScreenshotScale::Css)) {
      return Err(
        "BiDi/Firefox does not support `scale: \"css\"` screenshots — BiDi always captures at device-pixel scale."
          .into(),
      );
    }

    // Pre-capture DOM setup (caret, style, mask, CSS-animation pause) —
    // shared helpers, BiDi-specific execution via `script.callFunction`.
    let style_installed = self.screenshot_install_style(&opts).await?;
    let mask_installed = self.screenshot_install_mask(&opts).await?;
    let params = self.screenshot_build_params(&opts);

    let result = self.cmd("browsingContext.captureScreenshot", params).await;

    if style_installed {
      let _ = self
        .eval_bidi_function(&format!(
          "() => {{ {}; }}",
          crate::backend::screenshot_js::uninstall_style_js()
        ))
        .await;
    }
    if mask_installed {
      let _ = self
        .eval_bidi_function(&format!(
          "() => {{ {}; }}",
          crate::backend::screenshot_js::uninstall_mask_js()
        ))
        .await;
    }

    let data_str = result?
      .get("data")
      .and_then(|v| v.as_str().map(String::from))
      .ok_or("Screenshot: missing data")?;
    base64::engine::general_purpose::STANDARD
      .decode(data_str)
      .map_err(|e| format!("Screenshot base64 decode: {e}"))
  }

  /// Run a JS function-declaration expression via
  /// `script.callFunction` in this page's browsing context. Used by
  /// `screenshot()` to install and tear down DOM overrides.
  async fn eval_bidi_function(&self, function_declaration: &str) -> Result<(), String> {
    self
      .cmd(
        "script.callFunction",
        json!({
          "functionDeclaration": function_declaration,
          "target": {"context": &*self.context_id},
          "awaitPromise": false,
          "resultOwnership": "none",
        }),
      )
      .await
      .map(|_| ())
  }

  async fn screenshot_install_style(&self, opts: &ScreenshotOpts) -> Result<bool, String> {
    let css = crate::backend::screenshot_js::build_css(opts);
    if css.is_empty() {
      return Ok(false);
    }
    let install = format!("() => {{ {}; }}", crate::backend::screenshot_js::install_style_js(&css));
    self.eval_bidi_function(&install).await.map(|()| true)
  }

  async fn screenshot_install_mask(&self, opts: &ScreenshotOpts) -> Result<bool, String> {
    if let Some(js) = crate::backend::screenshot_js::install_mask_js(opts) {
      let wrapped = format!("() => {{ {js}; }}");
      self.eval_bidi_function(&wrapped).await.map(|()| true)
    } else {
      Ok(false)
    }
  }

  fn screenshot_build_params(&self, opts: &ScreenshotOpts) -> serde_json::Value {
    let format_type = match opts.format {
      ImageFormat::Png => "image/png",
      ImageFormat::Jpeg => "image/jpeg",
      ImageFormat::Webp => "image/webp",
    };
    let quality = opts
      .quality
      .map(|q| f64::from(i32::try_from(q.clamp(0, 100)).unwrap_or(100)) / 100.0);
    let origin = if opts.full_page { "document" } else { "viewport" };
    let mut params = json!({
      "context": &*self.context_id,
      "origin": origin,
      "format": { "type": format_type }
    });
    if let Some(q) = quality {
      params["format"]["quality"] = json!(q);
    }
    if let Some(rect) = opts.clip {
      params["clip"] = json!({
        "type": "box",
        "x": rect.x,
        "y": rect.y,
        "width": rect.width,
        "height": rect.height,
      });
    }
    params
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
          "context": &*self.context_id,
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
    let fd = self.injected_script().await?;
    // Use the shared JS accessibility tree helper from window.__fd.accessibilityTree().
    // This uses Playwright's ARIA role/name computation and tags elements with data-fdref
    // for ref resolution. Shared between BiDi and WebKit backends.
    let result = self
      .eval_internal(
        &format!("JSON.stringify({fd}.accessibilityTree({max_depth}))"),
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
      if item
        .get("disabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
      {
        properties.push(AxProperty {
          name: "disabled".into(),
          value: Some(serde_json::Value::Bool(true)),
        });
      }
      if item
        .get("readonly")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
      {
        properties.push(AxProperty {
          name: "readonly".into(),
          value: Some(serde_json::Value::Bool(true)),
        });
      }
      let level = item.get("level").and_then(serde_json::Value::as_i64).unwrap_or(0);
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
      if item
        .get("required")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
      {
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
        backend_dom_node_id: item.get("backendId").and_then(serde_json::Value::as_i64),
        ignored: item
          .get("ignored")
          .and_then(serde_json::Value::as_bool)
          .unwrap_or(false),
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

  pub async fn click_at_with(&self, x: f64, y: f64, args: &super::super::BackendClickArgs) -> Result<(), String> {
    self
      .cmd(
        "input.performActions",
        input::click_with_args(&self.context_id, x, y, args),
      )
      .await?;
    Ok(())
  }

  pub async fn hover_at_with(&self, x: f64, y: f64, args: &super::super::BackendHoverArgs) -> Result<(), String> {
    self
      .cmd(
        "input.performActions",
        input::hover_with_args(&self.context_id, x, y, *args),
      )
      .await?;
    Ok(())
  }

  /// `BiDi` has no public touch `pointerType` in the stable spec — the
  /// `input.performActions` `pointerType` union is `'mouse' | 'pen'`
  /// only. Playwright's own `BiDi` backend leaves `tap` unimplemented
  /// for the same reason. Returns a typed `unsupported:` error that
  /// the caller surfaces as [`crate::error::FerriError::Unsupported`].
  #[allow(clippy::unused_async, clippy::unused_self)]
  pub async fn tap_at_with(&self, _x: f64, _y: f64, _args: &super::super::BackendTapArgs) -> Result<(), String> {
    Err(
      "unsupported: tap is not available on the BiDi backend — WebDriver BiDi's input.performActions \
         pointerType has no 'touch' value in the stable spec (Playwright's own BiDi backend leaves \
         Touchscreen unimplemented for the same reason). Use the cdp-pipe or cdp-raw backend for tap."
        .to_string(),
    )
  }

  pub async fn press_modifiers(&self, mods: &[crate::options::Modifier]) -> Result<(), String> {
    if mods.is_empty() {
      return Ok(());
    }
    self
      .cmd("input.performActions", input::modifiers_down(&self.context_id, mods))
      .await?;
    Ok(())
  }

  pub async fn release_modifiers(&self, mods: &[crate::options::Modifier]) -> Result<(), String> {
    if mods.is_empty() {
      return Ok(());
    }
    self
      .cmd("input.performActions", input::modifiers_up(&self.context_id, mods))
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

  pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<(), String> {
    self
      .cmd(
        "input.performActions",
        input::click_and_drag(&self.context_id, from, to, steps),
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

  pub async fn key_down(&self, key: &str) -> Result<(), String> {
    self
      .cmd("input.performActions", input::key_down(&self.context_id, key))
      .await?;
    Ok(())
  }

  pub async fn key_up(&self, key: &str) -> Result<(), String> {
    self
      .cmd("input.performActions", input::key_up(&self.context_id, key))
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
          "partition": {"type": "context", "context": &*self.context_id}
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
      // Cookie expiry is a Unix timestamp (seconds). Convert to a JSON integer
      // without a direct float-to-int cast by formatting and re-parsing.
      let rounded = expires.round();
      if rounded.is_finite() && rounded >= 0.0 {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&format!("{rounded:.0}")) {
          cookie_obj["expiry"] = v;
        }
      }
    }
    if let Some(ref ss) = cookie.same_site {
      cookie_obj["sameSite"] = json!(ss.as_str().to_lowercase());
    }

    self
      .cmd(
        "storage.setCookie",
        json!({
          "cookie": cookie_obj,
          "partition": {"type": "context", "context": &*self.context_id}
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
          "partition": {"type": "context", "context": &*self.context_id}
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
          "partition": {"type": "context", "context": &*self.context_id}
        }),
      )
      .await?;
    Ok(())
  }

  // ── Emulation ───────────────────────────────────────────────────────────

  /// Apply a [`crate::options::BrowserContextOptions`] bag to this
  /// page. Every `BiDi` command is inlined — no per-field helpers
  /// remain. Mirrors Playwright's
  /// `/tmp/playwright/packages/playwright-core/src/server/bidi/bidiBrowser.ts::initialize`
  /// sequence. Unsupported fields return a typed error per field
  /// which is aggregated and surfaced to the caller.
  #[allow(clippy::too_many_lines)]
  pub async fn apply_context_options(&self, opts: &crate::options::BrowserContextOptions) -> Result<(), String> {
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
    let ua_fut: OptionFuture<_> = opts
      .user_agent
      .as_deref()
      .map(|ua| async move {
        self
          .cmd(
            "emulation.setUserAgentOverride",
            json!({"contexts": [&*self.context_id], "value": ua}),
          )
          .await
          .map(|_| ())
      })
      .into();
    let locale_fut: OptionFuture<_> = opts
      .locale
      .as_deref()
      .map(|l| async move {
        self
          .cmd(
            "emulation.setLocaleOverride",
            json!({"contexts": [&*self.context_id], "locale": l}),
          )
          .await
          .map(|_| ())
      })
      .into();
    let tz_fut: OptionFuture<_> = opts
      .timezone_id
      .as_deref()
      .map(|tz| async move {
        self
          .cmd(
            "emulation.setTimezoneOverride",
            json!({"contexts": [&*self.context_id], "timezone": tz}),
          )
          .await
          .map(|_| ())
      })
      .into();
    let js_fut: OptionFuture<_> = opts
      .java_script_enabled
      .map(|v| async move {
        self
          .cmd(
            "emulation.setScriptingEnabled",
            json!({"contexts": [&*self.context_id], "enabled": v}),
          )
          .await
          .map(|_| ())
      })
      .into();
    let dl_fut: OptionFuture<_> = opts
      .accept_downloads
      .map(|accept| async move {
        let dl = if accept {
          json!({ "type": "allowed", "destinationFolder": "" })
        } else {
          json!({ "type": "denied" })
        };
        self
          .cmd("browser.setDownloadBehavior", json!({"downloadBehavior": dl}))
          .await
          .map(|_| ())
      })
      .into();
    let headers_fut: OptionFuture<_> = opts
      .extra_http_headers
      .as_ref()
      .map(|h| async move { self.set_extra_http_headers(h).await })
      .into();
    let geo_fut: OptionFuture<_> = opts
      .geolocation
      .map(|g| async move {
        self
          .cmd(
            "emulation.setGeolocationOverride",
            json!({
              "contexts": [&*self.context_id],
              "coordinates": {"latitude": g.latitude, "longitude": g.longitude, "accuracy": g.accuracy},
            }),
          )
          .await
          .map(|_| ())
      })
      .into();
    let offline_fut: OptionFuture<_> = opts
      .offline
      .map(|o| async move {
        self
          .cmd(
            "emulation.setNetworkConditions",
            json!({
              "contexts": [&*self.context_id],
              "offline": o, "latency": 0.0, "downloadThroughput": -1.0, "uploadThroughput": -1.0,
            }),
          )
          .await
          .map(|_| ())
      })
      .into();
    let sw_fut: OptionFuture<_> = opts
      .service_workers
      .map(|p| async move {
        if matches!(p, crate::options::ServiceWorkerPolicy::Block) {
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

    let (r_vp, r_ua, r_loc, r_tz, r_js, r_dl, r_hdr, r_med, r_geo, r_off, r_sw) = tokio::join!(
      viewport_fut,
      ua_fut,
      locale_fut,
      tz_fut,
      js_fut,
      dl_fut,
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
      ("acceptDownloads", r_dl),
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
    // Explicit unsupported fields — Firefox BiDi lacks these
    // primitives. Surfacing the gap to the caller (vs. silently
    // dropping) matches Playwright's Rule-4 "typed Unsupported for
    // real protocol gaps".
    for (label, present) in [
      ("bypassCSP", opts.bypass_csp.is_some()),
      ("ignoreHTTPSErrors", opts.ignore_https_errors.is_some()),
      ("httpCredentials", opts.http_credentials.is_some()),
      ("screen", opts.screen.is_some()),
      ("permissions", opts.permissions.is_some()),
    ] {
      if present {
        errs.push(format!(
          "{label}: BiDi/Firefox backend does not yet support this context option"
        ));
      }
    }

    if errs.is_empty() { Ok(()) } else { Err(errs.join("; ")) }
  }

  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
    let mut params = json!({
      "context": &*self.context_id,
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

  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<(), String> {
    use crate::options::MediaOverride;
    // Firefox/BiDi only exposes `emulation.setForcedColorsModeThemeOverride`
    // (per /tmp/playwright/packages/playwright-core/src/server/bidi/third_party/bidiProtocolCore.ts:1069).
    // Playwright's own BiDi `updateEmulateMedia` is an empty stub — media,
    // reducedMotion, forcedColors and contrast have no BiDi equivalent yet.
    // Rather than silently pretending they worked, we error out with a
    // typed Unsupported so the caller knows Firefox can't honor that knob.
    if opts.media.is_specified() {
      return Err("BiDi/Firefox does not support `media` emulation — no BiDi protocol command exists for it".into());
    }
    if opts.reduced_motion.is_specified() {
      return Err(
        "BiDi/Firefox does not support `reducedMotion` emulation — no BiDi protocol command exists for it".into(),
      );
    }
    if opts.forced_colors.is_specified() {
      return Err(
        "BiDi/Firefox does not support `forcedColors` emulation — no BiDi protocol command exists for it".into(),
      );
    }
    if opts.contrast.is_specified() {
      return Err("BiDi/Firefox does not support `contrast` emulation — no BiDi protocol command exists for it".into());
    }
    // Color scheme: `emulation.setForcedColorsModeThemeOverride` accepts
    // `{ theme: 'light' | 'dark' | null }`. Treat Disabled as null.
    match &opts.color_scheme {
      MediaOverride::Unchanged => {},
      MediaOverride::Disabled => {
        self
          .cmd(
            "emulation.setForcedColorsModeThemeOverride",
            json!({ "contexts": [&*self.context_id], "theme": serde_json::Value::Null }),
          )
          .await?;
      },
      MediaOverride::Set(cs) => {
        let theme: serde_json::Value = match cs.as_str() {
          "dark" => json!("dark"),
          "light" => json!("light"),
          _ => serde_json::Value::Null,
        };
        self
          .cmd(
            "emulation.setForcedColorsModeThemeOverride",
            json!({ "contexts": [&*self.context_id], "theme": theme }),
          )
          .await?;
      },
    }
    Ok(())
  }

  /// Direct `BiDi` `network.setExtraHeaders` command. Backs
  /// [`crate::Page::set_extra_http_headers`] (Playwright's public
  /// `page.setExtraHTTPHeaders(headers)`).
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
          "contexts": [&*self.context_id],
          "headers": header_list
        }),
      )
      .await?;
    Ok(())
  }

  /// `BiDi` has no Permissions API. Called from
  /// [`crate::ContextRef::clear_permissions`] — returns typed error.
  pub fn reset_permissions(&self) -> impl std::future::Future<Output = Result<(), String>> {
    let _ = &self.context_id;
    std::future::ready(Err("Permissions API not available in BiDi backend".into()))
  }

  // ── Tracing ─────────────────────────────────────────────────────────────

  pub fn start_tracing(&self) -> impl std::future::Future<Output = Result<(), String>> {
    let _ = &self.context_id;
    std::future::ready(Err("Tracing not supported on BiDi backend".into()))
  }

  pub fn stop_tracing(&self) -> impl std::future::Future<Output = Result<(), String>> {
    let _ = &self.context_id;
    std::future::ready(Err("Tracing not supported on BiDi backend".into()))
  }

  pub fn metrics(&self) -> impl std::future::Future<Output = Result<Vec<MetricData>, String>> {
    let _ = &self.context_id;
    std::future::ready(Err("Performance metrics not supported on BiDi backend".into()))
  }

  // ── Ref resolution ──────────────────────────────────────────────────────

  pub async fn resolve_backend_node(&self, backend_node_id: i64, _ref_id: &str) -> Result<AnyElement, String> {
    // Resolve via data-fdref attribute (set during accessibility tree walk)
    self.find_element(&format!("[data-fdref='{backend_node_id}']")).await
  }

  // ── Event listeners ─────────────────────────────────────────────────────

  // The BiDi listener branches on every protocol event we subscribe
  // to (network, console, dialog, navigation) and has grown past
  // clippy's default 100-line cap. Splitting it into per-domain
  // sub-functions would require threading eight captured `Arc`s
  // through each call — the inline match is clearer as-is.
  #[allow(clippy::too_many_lines)]
  pub fn attach_listeners(
    &self,
    console_log: Arc<RwLock<Vec<ConsoleMessage>>>,
    network_log: Arc<RwLock<Vec<NetworkRequest>>>,
    dialog_log: Arc<RwLock<Vec<DialogEvent>>>,
  ) {
    // Register the emitter-bridge so `page.events().on("dialog", cb)`
    // continues to deliver live `Dialog` handles via the broadcast.
    let _ = self.dialog_manager.register_emitter_bridge(self.events.clone());
    // Same bridge for `filechooser`: broadcast listeners observe live
    // handles without needing the one-shot
    // `page.wait_for_file_chooser` flow.
    let _ = self.file_chooser_manager.register_emitter_bridge(self.events.clone());
    // Download bridge: broadcast `download` listeners see live
    // [`crate::download::Download`] handles via the same claim-on-open
    // path.
    let _ = self.download_manager.register_emitter_bridge(self.events.clone());

    // Configure Firefox to land downloads in our per-page tempdir.
    // `browser.setDownloadBehavior` is a best-effort browser-scoped
    // command (Playwright swallows errors too); the tempdir drop
    // cleans up any files if the command fails. Firing the setup on a
    // detached task because `attach_listeners` is synchronous.
    {
      let session = self.session.clone();
      let downloads_dir = self.downloads_dir.clone();
      tokio::spawn(async move {
        let params = serde_json::json!({
          "downloadBehavior": {
            "type": "allowed",
            "destinationFolder": downloads_dir.path().to_string_lossy(),
          },
        });
        let _ = session
          .transport
          .send_command("browser.setDownloadBehavior", params)
          .await;
      });
    }

    let mut rx = self.session.transport.subscribe_events();
    let ctx = self.context_id.clone();
    let session = self.session.clone();
    let dialog_manager = self.dialog_manager.clone();
    let file_chooser_manager = self.file_chooser_manager.clone();
    let download_manager = self.download_manager.clone();
    let downloads_dir = self.downloads_dir.clone();
    let page_backref = self.page_backref.clone();
    let closed = self.closed.clone();
    let emitter = self.events.clone();
    let injected_script = self.injected_script.clone();
    let exposed_fns = self.exposed_fns.clone();
    let exposed_session = self.session.clone();
    let exposed_ctx = self.context_id.clone();
    let tracker = Arc::new(BidiNetworkTracker::new(
      self.session.clone(),
      self.nav_request_slot.clone(),
    ));

    tokio::spawn(async move {
      while let Ok(event) = rx.recv().await {
        // Filter events for this context. Accepts events targeting this
        // top-level context directly, events with no `context` field
        // (session-wide), and events whose `parent` matches us — the
        // latter covers `browsingContext.contextCreated` for child
        // iframes that BiDi reports under the child's own id.
        let event_ctx = event.params.get("context").and_then(|v| v.as_str()).unwrap_or("");
        let event_parent = event.params.get("parent").and_then(|v| v.as_str()).unwrap_or("");
        let child_of_this = !event_parent.is_empty() && event_parent == &*ctx;
        if event_ctx != &*ctx && !event_ctx.is_empty() && !child_of_this {
          continue;
        }

        // `browsingContext.contextCreated` events for child iframes
        // surface here with their own (child) `context` id and the
        // top-level `parent` set to our context. Emit a `FrameAttached`
        // so the page-level frame cache (used by `page.frame(name)`,
        // `page.frames()`) sees the iframe. Without this BiDi pages
        // expose no child-frame metadata at all.
        //
        // BiDi's `contextCreated` payload does not carry the
        // iframe's `name` attribute. Spawn a follow-up
        // `browsingContext.getTree` to enrich the cache record with
        // name + final url once Firefox has wired the iframe up.
        if event.method == "browsingContext.contextCreated" && child_of_this {
          let frame_id = event_ctx.to_string();
          let url = event
            .params
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
          let parent_id = (*ctx).to_string();
          emitter.emit(PageEvent::FrameAttached(crate::backend::FrameInfo {
            frame_id: frame_id.clone(),
            parent_frame_id: Some(parent_id.clone()),
            name: String::new(),
            url: url.clone(),
          }));
          // Async refresh: fetch the iframe's `name` (and updated `url`)
          // via `browsingContext.getTree`. Firefox populates `name` once
          // the iframe element is parsed; we re-emit a `FrameNavigated`
          // with the enriched info so the cache lookup picks up
          // `page.frame('target')` matches.
          let session_for_refresh = session.clone();
          let emitter_for_refresh = emitter.clone();
          let parent_for_refresh = parent_id.clone();
          let child_for_refresh = frame_id.clone();
          tokio::spawn(async move {
            let result = session_for_refresh
              .transport
              .send_command(
                "browsingContext.getTree",
                json!({"root": &parent_for_refresh, "maxDepth": 1}),
              )
              .await;
            let Ok(tree) = result else { return };
            let Some(contexts) = tree.get("contexts").and_then(|v| v.as_array()) else {
              return;
            };
            // The returned array has the parent at index 0; its
            // `children` array contains the iframe entries with `name`.
            let Some(children) = contexts
              .first()
              .and_then(|p| p.get("children"))
              .and_then(|v| v.as_array())
            else {
              return;
            };
            for child in children {
              let cid = child.get("context").and_then(|v| v.as_str()).unwrap_or("");
              if cid != child_for_refresh {
                continue;
              }
              let cname = child.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
              let curl = child.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
              emitter_for_refresh.emit(PageEvent::FrameNavigated(crate::backend::FrameInfo {
                frame_id: cid.to_string(),
                parent_frame_id: Some(parent_for_refresh.clone()),
                name: cname,
                url: curl,
              }));
              break;
            }
          });
          continue;
        }

        match event.method.as_str() {
          "browsingContext.navigationStarted"
          | "browsingContext.fragmentNavigated"
          | "browsingContext.domContentLoaded"
          | "browsingContext.load" => {
            injected_script.reset();
          },
          "log.entryAdded" => {
            // Mirrors Playwright's `bidiPage.ts::_onLogEntryAdded`.
            // Routes `type: 'javascript'` + `level: 'error'` entries to
            // `PageEvent::PageError(WebError)`; `type: 'console'`
            // entries to `PageEvent::Console(ConsoleMessage)`; other
            // entry types (`'deprecation'`, etc.) are ignored.
            let entry_type = event.params.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let level = event.params.get("level").and_then(|v| v.as_str()).unwrap_or("");
            if entry_type == "javascript" && level == "error" {
              // Parity with `bidiPage.ts:267-286`: split `text` at the
              // first `': '` for `name` / `message`, synthesise `stack`
              // from the entry's `stackTrace.callFrames` (BiDi
              // line/column are 0-based so Playwright adds `+ 1`).
              let text = event
                .params
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
              let (name, message) = split_error_text(&text);
              let stack = build_bidi_stack(&text, event.params.get("stackTrace"));
              let details = crate::web_error::ErrorDetails { name, message, stack };
              let web_err = match page_backref.upgrade() {
                Some(page) => crate::web_error::WebError::new(&page, details),
                None => crate::web_error::WebError::new_detached(details),
              };
              emitter.emit(PageEvent::PageError(web_err));
              continue;
            }
            if entry_type != "console" {
              continue;
            }

            // Exposed-function dispatch path. The JS shim installed
            // by `expose_function` calls
            // `console.log(JSON.stringify({__ferri_call, id, args}))`
            // and parks on a Promise stored in `window.__ferri_exposed[id]`.
            // Intercept those entries here, run the Rust callback,
            // and resolve the promise via a follow-up `script.evaluate`.
            // BiDi has no `Runtime.bindingCalled` analogue, so this
            // console-side channel is the available transport.
            if let Some(text_arg) = event
              .params
              .get("args")
              .and_then(|v| v.as_array())
              .and_then(|arr| arr.first())
              .and_then(|a| a.get("value"))
              .and_then(|v| v.as_str())
            {
              if text_arg.starts_with(r#"{"__ferri_call":"#) {
                if let Ok(payload) = serde_json::from_str::<serde_json::Value>(text_arg) {
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
                      "(() => {{ const f = window.__ferri_exposed && window.__ferri_exposed['{escaped_id}']; if (f) {{ delete window.__ferri_exposed['{escaped_id}']; f({result_js}); }} }})()"
                    );
                    let _ = exposed_session
                      .transport
                      .send_command(
                        "script.callFunction",
                        json!({
                          "functionDeclaration": format!("() => {{ {resolve_js} }}"),
                          "target": {"context": &*exposed_ctx},
                          "awaitPromise": false,
                          "resultOwnership": "none"
                        }),
                      )
                      .await;
                  }
                  // Don't surface the dispatch envelope as a regular
                  // console.log — Playwright hides bindings from the
                  // user's `page.on('console')` stream.
                  continue;
                }
              }
            }

            let Some(page) = page_backref.upgrade() else {
              continue;
            };
            // BiDi reports `method` for the console variant (e.g.
            // 'log', 'warn', 'error'). Playwright remaps `'warn'` to
            // `'warning'` for parity with CDP's `Runtime.consoleAPICalled.type`.
            let method = event.params.get("method").and_then(|v| v.as_str()).unwrap_or("log");
            let type_str = if method == "warn" { "warning" } else { method };
            // `text` is explicit only for `timeLog` / `timeEnd`. For
            // other variants Playwright leaves it undefined so the
            // lazy `text()` falls back to args preview.
            let text = if matches!(method, "timeLog" | "timeEnd") {
              event
                .params
                .get("text")
                .and_then(|v| v.as_str())
                .map(std::string::ToString::to_string)
            } else {
              None
            };
            let args_json = event
              .params
              .get("args")
              .and_then(|v| v.as_array())
              .cloned()
              .unwrap_or_default();
            let mut args: Vec<crate::js_handle::JSHandle> = Vec::with_capacity(args_json.len());
            for arg in &args_json {
              let backing = bidi_remote_value_to_backing(arg);
              args.push(crate::js_handle::JSHandle::from_backing(page.clone(), backing));
            }
            let location = bidi_stack_trace_to_location(event.params.get("stackTrace"), event.params.get("source"));
            let timestamp = event
              .params
              .get("timestamp")
              .and_then(serde_json::Value::as_f64)
              .map_or(0, f64_to_u64_saturating);
            let msg = ConsoleMessage::new(&page, type_str, text, args, location, timestamp);
            console_log.write().await.push(msg.clone());
            emitter.emit(PageEvent::Console(msg));
          },
          "network.beforeRequestSent" => {
            tracker
              .on_before_request_sent(&event.params, &emitter, &network_log)
              .await;
          },
          "network.responseStarted" => {
            tracker.on_response_started(&event.params, &emitter).await;
          },
          "network.responseCompleted" => {
            tracker.on_response_completed(&event.params, &emitter).await;
          },
          "network.fetchError" => {
            tracker.on_fetch_error(&event.params, &emitter).await;
          },
          "browsingContext.userPromptOpened" => {
            let prompt_type_str = event
              .params
              .get("type")
              .and_then(|v| v.as_str())
              .unwrap_or("alert")
              .to_string();
            let message = event
              .params
              .get("message")
              .and_then(|v| v.as_str())
              .unwrap_or("")
              .to_string();
            let default_value = event
              .params
              .get("defaultValue")
              .and_then(|v| v.as_str())
              .unwrap_or("")
              .to_string();
            let dialog_type = crate::dialog::DialogType::parse(&prompt_type_str);

            // Build the responder: translates the user's accept/dismiss
            // into `browsingContext.handleUserPrompt`. Captures an Arc
            // of the session + context id.
            let responder_session = session.clone();
            let responder_ctx = ctx.clone();
            let responder: crate::dialog::DialogResponder = Arc::new(move |response| {
              let session = responder_session.clone();
              let ctx = responder_ctx.clone();
              Box::pin(async move {
                let accept = matches!(response, crate::dialog::DialogResponse::Accept { .. });
                let mut handle_params = json!({
                  "context": &*ctx,
                  "accept": accept,
                });
                if let crate::dialog::DialogResponse::Accept { prompt_text: Some(t) } = response {
                  handle_params["userText"] = json!(t);
                }
                session
                  .transport
                  .send_command("browsingContext.handleUserPrompt", handle_params)
                  .await
                  .map(|_| ())
              })
            });

            let dialog = crate::dialog::Dialog::new_with_manager(
              dialog_type,
              message.clone(),
              default_value,
              responder,
              Some(dialog_manager.clone()),
            );

            // Synchronous dialog dispatch — mirrors Playwright's
            // `DialogManager.dialogDidOpen`. See the CDP equivalent
            // in `backend/cdp/mod.rs` for the full rationale.
            dialog_manager.did_open(dialog);

            dialog_log.write().await.push(DialogEvent {
              dialog_type: prompt_type_str,
              message,
              action: "dispatched".to_string(),
            });
          },
          "browsingContext.contextDestroyed" => {
            closed.store(true, Ordering::Relaxed);
            emitter.emit(PageEvent::Close);
          },
          "input.fileDialogOpened" => {
            // BiDi event shape:
            // { "context": "<frameId>", "element": { "sharedId": "..." }, "multiple": bool }
            // Per `/tmp/playwright/packages/playwright-core/src/server/bidi/bidiPage.ts::_onFileDialogOpened`.
            let shared_id = event
              .params
              .get("element")
              .and_then(|e| e.get("sharedId"))
              .and_then(|v| v.as_str())
              .map(std::string::ToString::to_string);
            let is_multiple = event
              .params
              .get("multiple")
              .and_then(serde_json::Value::as_bool)
              .unwrap_or(false);
            let Some(shared_id) = shared_id else {
              continue;
            };
            // Resolve the element -> ElementHandle off the hot
            // subscription loop. Same rationale as the CDP listener:
            // we don't want rapid file-picker triggers queued
            // behind a slow DOM round-trip.
            let manager_clone = file_chooser_manager.clone();
            let backref_clone = page_backref.clone();
            let ctx_clone = ctx.clone();
            let session_clone = session.clone();
            tokio::spawn(async move {
              let Some(page) = backref_clone.upgrade() else {
                return;
              };
              // Build a BidiElement directly from the shared id so
              // we don't need a page-scoped selector round-trip.
              let element =
                crate::backend::AnyElement::Bidi(super::BidiElement::new(session_clone, ctx_clone, shared_id));
              let Ok(handle) = crate::element_handle::ElementHandle::from_any_element(page.clone(), element).await
              else {
                return;
              };
              let chooser = crate::file_chooser::FileChooser::new(handle, is_multiple);
              manager_clone.did_open(&chooser);
            });
          },
          "browsingContext.downloadWillBegin" => {
            // BiDi correlates a download with its triggering navigation
            // via the `navigation` id on the event. If the browser
            // doesn't attribute it to a navigation, Playwright skips
            // (see `bidiPage.ts::_onDownloadWillBegin`) — so do we.
            let Some(navigation) = event
              .params
              .get("navigation")
              .and_then(|v| v.as_str())
              .map(std::string::ToString::to_string)
            else {
              continue;
            };
            let url = event
              .params
              .get("url")
              .and_then(|v| v.as_str())
              .unwrap_or("")
              .to_string();
            let suggested = event
              .params
              .get("suggestedFilename")
              .and_then(|v| v.as_str())
              .unwrap_or("")
              .to_string();

            let Some(page) = page_backref.upgrade() else {
              continue;
            };

            // BiDi has no native cancel — Playwright's own
            // `bidiBrowser.ts::cancelDownload` is a no-op. Surface
            // typed `Unsupported` per Rule 4 so callers know.
            let canceler: crate::download::DownloadCanceler = Arc::new(|| {
              Box::pin(async {
                Err(crate::error::FerriError::Unsupported(
                  "Download.cancel is not supported on the BiDi backend: Firefox's BiDi implementation has no browser.cancelDownload command and Playwright's own BiDi backend leaves cancelDownload as a no-op (see bidiBrowser.ts::cancelDownload)".into(),
                ))
              })
            });

            let download = crate::download::Download::new(
              &page,
              navigation,
              url,
              suggested,
              downloads_dir.path().to_path_buf(),
              canceler,
            );
            download_manager.did_open(&download);
          },
          "browsingContext.downloadEnd" => {
            let Some(navigation) = event.params.get("navigation").and_then(|v| v.as_str()) else {
              continue;
            };
            let status = event
              .params
              .get("status")
              .and_then(|v| v.as_str())
              .unwrap_or("complete");
            if let Some(d) = download_manager.take_for_guid(navigation) {
              if status == "canceled" {
                d.report_finished(None, Some("canceled".to_string()));
              } else {
                // `complete` carries the absolute `filepath` Firefox
                // wrote to — override the default
                // `<downloads_dir>/<guid>` path with the real one.
                let filepath = event
                  .params
                  .get("filepath")
                  .and_then(|v| v.as_str())
                  .map(std::path::PathBuf::from);
                d.report_finished(filepath, None);
              }
            }
          },
          _ => {},
        }
      }
    });
  }

  // ── Element screenshot ──────────────────────────────────────────────────

  // (Handled above in screenshot_element)

  // ── PDF ─────────────────────────────────────────────────────────────────

  /// Generate a PDF of the current page via `WebDriver` `BiDi`
  /// `browsingContext.print`.
  ///
  /// `BiDi`'s `PrintParameters` shape is narrower than CDP's — no header /
  /// footer template, no tagged/outline, no `preferCSSPageSize`. The
  /// canonical mapping is at
  /// `/tmp/playwright/packages/playwright-core/src/server/bidi/bidiPdf.ts`;
  /// fields unsupported by `BiDi` are silently dropped here (Playwright does
  /// the same). Unit conversion mirrors the CDP backend.
  pub async fn pdf(&self, opts: crate::options::PdfOptions) -> Result<Vec<u8>, String> {
    let mut paper_width = 8.5_f64;
    let mut paper_height = 11.0_f64;
    if let Some(ref format) = opts.format {
      if let Some((w, h)) = crate::options::pdf_paper_format_size(format) {
        paper_width = w;
        paper_height = h;
      } else {
        return Err(format!("Unknown paper format: {format}"));
      }
    } else {
      if let Some(ref w) = opts.width {
        paper_width = w.to_inches();
      }
      if let Some(ref h) = opts.height {
        paper_height = h.to_inches();
      }
    }

    let margin = opts.margin.unwrap_or_default();
    let page_ranges: Option<Vec<String>> = opts
      .page_ranges
      .as_deref()
      .filter(|s| !s.is_empty())
      .map(|s| s.split(',').map(|r| r.trim().to_string()).collect());

    let mut params = json!({
      "context": &*self.context_id,
      "background": opts.print_background.unwrap_or(false),
      "margin": {
        "bottom": margin.bottom.as_ref().map_or(0.0, crate::options::PdfSize::to_inches),
        "left": margin.left.as_ref().map_or(0.0, crate::options::PdfSize::to_inches),
        "right": margin.right.as_ref().map_or(0.0, crate::options::PdfSize::to_inches),
        "top": margin.top.as_ref().map_or(0.0, crate::options::PdfSize::to_inches),
      },
      "orientation": if opts.landscape.unwrap_or(false) { "landscape" } else { "portrait" },
      "page": { "width": paper_width, "height": paper_height },
      "scale": opts.scale.unwrap_or(1.0),
    });
    if let Some(ranges) = page_ranges {
      params["pageRanges"] = serde_json::Value::Array(ranges.into_iter().map(serde_json::Value::String).collect());
    }

    let result = self.cmd("browsingContext.print", params).await?;

    let data_str = result.get("data").and_then(|v| v.as_str()).ok_or("PDF: missing data")?;
    base64::engine::general_purpose::STANDARD
      .decode(data_str)
      .map_err(|e| format!("PDF base64 decode: {e}"))
  }

  // ── Screencast (not supported) ──────────────────────────────────────────

  /// Start screencast via repeated screenshots + event-driven captures.
  /// `BiDi` has no native screencast API. We combine polling at ~15 fps with
  /// captures on navigation/load events to ensure key visual transitions
  /// are recorded even for fast tests.
  pub fn start_screencast(
    &self,
    quality: u8,
    _max_width: u32,
    _max_height: u32,
  ) -> impl std::future::Future<Output = Result<tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, f64)>, String>> {
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
            if c == &*event_ctx {
              event_notify2.notify_one();
            }
          }
        }
      }
    });

    tokio::spawn(async move {
      let target_interval = std::time::Duration::from_millis(66); // ~15 fps
      let capture_params = json!({
        "context": &*ctx_id,
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
          let remaining = target_interval.checked_sub(elapsed).unwrap_or_default();
          tokio::select! {
            () = tokio::time::sleep(remaining) => {},
            () = event_notify.notified() => {},
          }
        }
      }
    });

    std::future::ready(Ok(rx))
  }

  pub fn stop_screencast(&self) -> impl std::future::Future<Output = Result<(), String>> {
    let _ = &self.context_id;
    std::future::ready(Ok(()))
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
          "context": &*self.context_id,
          "element": {"sharedId": shared_id},
          "files": paths
        }),
      )
      .await?;
    Ok(())
  }

  // ── Network Interception ────────────────────────────────────────────────

  pub async fn route(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::route::RouteHandler,
  ) -> Result<(), String> {
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
            "contexts": [&*self.context_id]
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
          if event_ctx != &*ctx {
            continue;
          }
          let is_blocked = event
            .params
            .get("isBlocked")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
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
              .find(|r| r.matcher.matches(url))
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

    self
      .routes
      .write()
      .await
      .push(crate::route::RegisteredRoute { matcher, handler });

    Ok(())
  }

  pub async fn unroute(&self, matcher: &crate::url_matcher::UrlMatcher) -> Result<(), String> {
    let mut routes = self.routes.write().await;
    routes.retain(|r| !r.matcher.equivalent(matcher));

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

  pub async fn close_page(&self, opts: crate::options::PageCloseOptions) -> Result<(), String> {
    // BiDi's `browsingContext.close` takes an optional `promptUnload` flag
    // — true fires `beforeunload` handlers before unloading the context.
    // Mirrors Playwright's `bidiPage.ts:_closePage` param naming.
    self
      .cmd(
        "browsingContext.close",
        json!({
          "context": &*self.context_id,
          "promptUnload": opts.run_before_unload.unwrap_or(false),
        }),
      )
      .await?;
    self.closed.store(true, Ordering::Relaxed);
    Ok(())
  }

  #[must_use]
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
          "contexts": [&*self.context_id]
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

  // ── Utility-script evaluate (Playwright `page.evaluate(fn, arg)`) ──────

  /// Call the page-side `UtilityScript.evaluate` over `BiDi`. Parallels
  /// `CdpPage::call_utility_evaluate` but sends the arguments as
  /// `BiDi` `LocalValue` / `RemoteReference` entries through
  /// `script.callFunction`.
  ///
  /// The wrapper function is identical to the CDP path — it memoises
  /// the utility script on `window.__fd.__us`, `JSON.parse`s the
  /// serialized argument, and `JSON.stringify`s the result back so we
  /// only ship strings through `script.callFunction`'s native
  /// serializer. Handles arrive as `{type: "sharedReference", sharedId}`
  /// `BiDi` arguments — the browser hydrates each back to its native
  /// JS object before the wrapper receives it.
  ///
  /// # Errors
  ///
  /// Returns a String on transport failure, page-side exception, or
  /// handle/backend mismatch.
  /// Construct a [`super::BidiElement`] directly from a shared-reference
  /// id without re-resolving through the DOM. Used by
  /// [`crate::backend::element_from_remote`] when a
  /// [`crate::js_handle::JSHandle`] turns out to wrap a DOM node and
  /// needs to be re-packaged as an
  /// [`crate::element_handle::ElementHandle`].
  pub(crate) fn element_from_shared_id(&self, shared_id: String) -> super::BidiElement {
    super::BidiElement::new(self.session.clone(), self.context_id.clone(), shared_id)
  }

  /// ferridriver's equivalent of Playwright's
  /// `evaluateExpression(context, expr, opts, ...args)` — see the CDP
  /// twin for the shared contract. Sends variadic `args` + shared
  /// `handles` through `script.callFunction`.
  ///
  /// # Errors
  ///
  /// Returns a String on transport failure, page-side exception, or
  /// handle/backend mismatch.
  #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
  pub async fn call_utility_evaluate(
    &self,
    fn_source: &str,
    args: &[crate::protocol::SerializedValue],
    handles: &[crate::protocol::HandleId],
    frame_id: Option<&str>,
    is_function: Option<bool>,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult, String> {
    use crate::js_handle::{EvaluateResult as FdEvalResult, HandleRemote};
    use crate::protocol::HandleId;
    use serde_json::json;

    self.ensure_engine_injected().await?;

    let target_ctx: &str = frame_id.unwrap_or(&self.context_id);

    let args_json = serde_json::to_string(args).map_err(|e| e.to_string())?;
    let count = args.len();

    let is_fn_local = match is_function {
      Some(true) => json!({"type": "boolean", "value": true}),
      Some(false) => json!({"type": "boolean", "value": false}),
      None => json!({"type": "undefined"}),
    };

    let mut arguments = vec![
      is_fn_local,
      json!({"type": "boolean", "value": return_by_value}),
      json!({"type": "string", "value": fn_source}),
      json!({"type": "number", "value": count}),
      json!({"type": "string", "value": args_json}),
    ];
    for handle in handles {
      match handle {
        HandleId::Bidi { shared_id, handle } => {
          // BiDi has two distinct handle shapes: `sharedReference`
          // is DOM-node-only (cross-context node identity via UUID)
          // and `handle` is any retained remote (Object, Array,
          // Function, Map, Set, etc.) via the per-session handle
          // registry. Prefer `handle` when present — it's the more
          // general form that also works for nodes inside a single
          // context. Only fall back to `sharedReference` when the
          // remote is a node without a retained handle.
          if let Some(h) = handle {
            arguments.push(json!({"type": "handle", "handle": h}));
          } else if !shared_id.is_empty() {
            arguments.push(json!({"type": "sharedReference", "sharedId": shared_id}));
          } else {
            return Err("BiDi handle carries neither sharedId nor handle".into());
          }
        },
        _ => return Err("call_utility_evaluate: non-BiDi handle in arg.handles on BiDi backend".into()),
      }
    }

    let params = json!({
      "functionDeclaration": crate::backend::cdp::UTILITY_EVAL_WRAPPER,
      "target": {"context": target_ctx},
      "arguments": arguments,
      "awaitPromise": true,
      "resultOwnership": if return_by_value { "none" } else { "root" },
    });

    let response = self.cmd("script.callFunction", params).await?;
    let eval_result: super::types::EvaluateResult =
      serde_json::from_value(response).map_err(|e| format!("BiDi call_utility_evaluate parse: {e}"))?;

    match eval_result {
      super::types::EvaluateResult::Exception { exception_details } => {
        Err(format!("Evaluation error: {}", exception_details.text))
      },
      super::types::EvaluateResult::Success { result } => {
        if return_by_value {
          // Wrapper JSON.stringified its result. The BiDi wire shape
          // for a string is `{type: "string", value: "<json>"}`; for
          // a null (undefined sentinel) it's `{type: "null"}`.
          let inner_json: serde_json::Value = match result {
            super::types::RemoteValue::String { value } => {
              let s = value.as_str().unwrap_or("null").to_string();
              serde_json::from_str(&s).map_err(|e| format!("BiDi parse utility result: {e}"))?
            },
            super::types::RemoteValue::Null | super::types::RemoteValue::Undefined => {
              return Ok(FdEvalResult::Value(crate::protocol::SerializedValue::Special(
                crate::protocol::SpecialValue::Undefined,
              )));
            },
            other => {
              return Err(format!(
                "BiDi call_utility_evaluate: wrapper returned non-string in returnByValue mode: {other:?}"
              ));
            },
          };
          let parsed: crate::protocol::SerializedValue =
            serde_json::from_value(inner_json).map_err(|e| format!("BiDi parse SerializedValue: {e}"))?;
          Ok(FdEvalResult::Value(parsed))
        } else {
          // Returning a handle: the result is either a DOM node
          // (sharedReference), a non-node object (handle field), or
          // a primitive (inline value — no page-side retention).
          // Playwright's `JSHandle` has both shapes; mirror that here.
          if let Some(shared) = result.as_shared_reference() {
            Ok(FdEvalResult::Handle(crate::js_handle::JSHandleBacking::Remote(
              HandleRemote::Bidi {
                shared_id: shared.shared_id,
                handle: shared.handle,
              },
            )))
          } else {
            let non_node_handle = match &result {
              super::types::RemoteValue::Array { handle, .. }
              | super::types::RemoteValue::Object { handle, .. }
              | super::types::RemoteValue::Map { handle, .. }
              | super::types::RemoteValue::Set { handle, .. }
              | super::types::RemoteValue::Function { handle }
              | super::types::RemoteValue::Error { handle }
              | super::types::RemoteValue::Promise { handle }
              | super::types::RemoteValue::Symbol { handle } => handle.clone(),
              _ => None,
            };
            if let Some(h) = non_node_handle {
              // Store the BiDi handle ONLY in the `handle` slot —
              // `shared_id` is reserved for DOM-node cross-context
              // references. A later argument-pass emits this remote
              // as `{type: "handle", handle}`, which BiDi accepts
              // for any retained Object / Array / Function / ...
              // remote. Emitting it as `{type: "sharedReference",
              // sharedId}` (the old mistake) causes BiDi to reject
              // with "no such node" because the handle-string was
              // never registered as a node sharedId.
              Ok(FdEvalResult::Handle(crate::js_handle::JSHandleBacking::Remote(
                HandleRemote::Bidi {
                  shared_id: String::new(),
                  handle: Some(h),
                },
              )))
            } else {
              // Primitive result from evaluateHandle — wrap the
              // inline value as a value-backed JSHandle. BiDi maps
              // `Undefined`, `Null`, `Boolean`, `Number`, `String`,
              // `BigInt` to native JSON (plus a BigInt decimal
              // string) via `RemoteValue::to_json`. The one BiDi
              // shape that has no JSON projection (Symbol without a
              // handle) already falls into the non-node branch above.
              let as_json = result.to_json().unwrap_or(serde_json::Value::Null);
              let serialized = match &result {
                super::types::RemoteValue::Undefined => {
                  crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
                },
                super::types::RemoteValue::Null => {
                  crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Null)
                },
                super::types::RemoteValue::BigInt { value } => {
                  let s = value
                    .as_str()
                    .map_or_else(|| value.to_string(), std::string::ToString::to_string);
                  crate::protocol::SerializedValue::BigInt(s)
                },
                _ => {
                  let mut ctx = crate::protocol::SerializationContext::default();
                  crate::protocol::SerializedValue::from_json(&as_json, &mut ctx)
                },
              };
              Ok(FdEvalResult::Handle(crate::js_handle::JSHandleBacking::Value(
                serialized,
              )))
            }
          }
        }
      },
    }
  }

  // ── Handle lifecycle ────────────────────────────────────────────────────

  /// Release the remote handle identified by `shared_id` via
  /// `script.disown`. Used by `AnyPage::release_handle` when disposing a
  /// `JSHandle` / `ElementHandle` on a `BiDi` backend.
  ///
  /// `BiDi`'s `script.disown` takes an array of handle strings scoped to
  /// one target; we always pass a single `sharedId`. If `handle` is
  /// `Some(_)`, it is used verbatim — some browsers supply an
  /// implementation-specific `handle` alongside `sharedId` that
  /// `script.disown` accepts; the `sharedId` form is universally
  /// supported and is what Playwright's own `bidiSession.ts` uses.
  ///
  /// # Errors
  ///
  /// Returns the transport error if the `BiDi` command fails.
  pub async fn release_handle(&self, shared_id: &str, handle: Option<&str>) -> Result<(), String> {
    let handle_str = handle.unwrap_or(shared_id);
    self
      .cmd(
        "script.disown",
        json!({
          "handles": [handle_str],
          "target": {"context": &*self.context_id},
        }),
      )
      .await
      .map(|_| ())
  }

  // ── Exposed Functions ───────────────────────────────────────────────────

  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<(), String> {
    // Inject a global function that sends messages via BiDi channel
    let js = format!(
      r"() => {{
        window['{name}'] = (...args) => {{
          return new Promise((resolve) => {{
            const id = Math.random().toString(36);
            window.__ferri_exposed = window.__ferri_exposed || {{}};
            window.__ferri_exposed[id] = resolve;
            console.log(JSON.stringify({{__ferri_call: '{name}', id, args}}));
          }});
        }};
      }}"
    );

    self
      .cmd(
        "script.addPreloadScript",
        json!({
          "functionDeclaration": js,
          "contexts": [&*self.context_id]
        }),
      )
      .await?;

    // Also execute it now for the current page
    let _ = self
      .cmd(
        "script.callFunction",
        json!({
          "functionDeclaration": js,
          "target": {"context": &*self.context_id},
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

/// Per-page bookkeeping for live `Request` / `Response` lifecycle objects
/// across the `BiDi` network event sequence (`beforeRequestSent` →
/// `responseStarted` → `responseCompleted` / `fetchError`).
///
/// W3C `BiDi` today does not expose `WebSocket` frame events — Playwright's
/// own `BiDi` backend skips `WebSocket` handling for the same reason — so
/// `WebSocket.body()` / `WebSocket` frame observation surface the typed
/// `FerriError::Unsupported` per Rule 4 instead of dangling indefinitely.
struct BidiNetworkTracker {
  session: Arc<super::session::BidiSession>,
  requests: tokio::sync::Mutex<FxHashMap<String, NetworkRequest>>,
  responses: tokio::sync::Mutex<FxHashMap<String, Response>>,
  nav_request_slot: crate::network::NavRequestSlot,
}

impl BidiNetworkTracker {
  fn new(session: Arc<super::session::BidiSession>, nav_request_slot: crate::network::NavRequestSlot) -> Self {
    Self {
      session,
      requests: tokio::sync::Mutex::new(FxHashMap::default()),
      responses: tokio::sync::Mutex::new(FxHashMap::default()),
      nav_request_slot,
    }
  }

  async fn on_before_request_sent(
    self: &Arc<Self>,
    params: &serde_json::Value,
    emitter: &EventEmitter,
    network_log: &Arc<RwLock<Vec<NetworkRequest>>>,
  ) {
    let Some(req) = params.get("request") else {
      return;
    };
    let id = req.get("request").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if id.is_empty() {
      return;
    }
    let url = req.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("GET").to_string();
    let headers = req.get("headers").map(parse_bidi_headers).unwrap_or_default();
    let resource_type = params
      .get("initiator")
      .and_then(|i| i.get("type"))
      .and_then(|v| v.as_str())
      .map_or("", |t| match t {
        "parser" => "Document",
        "script" => "Script",
        "preflight" => "Preflight",
        other => other,
      })
      .to_string();
    let post_data = req
      .get("body")
      .and_then(|b| b.get("value"))
      .and_then(|v| v.as_str())
      .map(|s| s.as_bytes().to_vec());
    let frame_id = params
      .get("context")
      .and_then(|v| v.as_str())
      .map(std::string::ToString::to_string);
    let is_navigation_request = params.get("navigation").and_then(|v| v.as_str()).is_some();

    // Redirects: BiDi reuses the request id and signals the prior with
    // `redirectCount`. When the count > 0, treat it like the next hop.
    let redirected_from = if params
      .get("redirectCount")
      .and_then(serde_json::Value::as_u64)
      .unwrap_or(0)
      > 0
    {
      let mut requests = self.requests.lock().await;
      requests.remove(&id)
    } else {
      None
    };

    let new_request = network::Request::new(RequestInit {
      id: id.clone(),
      url,
      method,
      resource_type,
      is_navigation_request,
      post_data,
      headers,
      frame_id,
      redirected_from,
      timing: None,
      raw_headers_fn: None,
    });

    self.requests.lock().await.insert(id.clone(), new_request.clone());

    // Main-document navigations: update the per-page slot so
    // `BidiPage::goto` / `reload` / `go_back` / `go_forward` can
    // resolve the final main-document `Response` after the
    // lifecycle wait completes. BiDi flags a navigation request via
    // the `navigation` field on `network.beforeRequestSent`; the
    // slot therefore tracks each redirect hop (same request id,
    // reused across the chain).
    if new_request.is_navigation_request() {
      self.nav_request_slot.set(new_request.clone());
    }

    network_log.write().await.push(new_request.clone());
    emitter.emit(PageEvent::Request(new_request));
  }

  async fn on_response_started(self: &Arc<Self>, params: &serde_json::Value, emitter: &EventEmitter) {
    let Some(request_id) = params
      .get("request")
      .and_then(|r| r.get("request"))
      .and_then(|v| v.as_str())
    else {
      return;
    };
    let request_id = request_id.to_string();
    let Some(req) = self.requests.lock().await.get(&request_id).cloned() else {
      return;
    };
    let Some(resp) = params.get("response") else {
      return;
    };
    let response = self.build_response(req.clone(), resp, &request_id);
    self.responses.lock().await.insert(request_id, response.clone());
    req.set_response(&response).await;
    emitter.emit(PageEvent::Response(response));
  }

  async fn on_response_completed(self: &Arc<Self>, params: &serde_json::Value, emitter: &EventEmitter) {
    let Some(request_id) = params
      .get("request")
      .and_then(|r| r.get("request"))
      .and_then(|v| v.as_str())
    else {
      return;
    };
    let request_id = request_id.to_string();
    let Some(req) = self.requests.lock().await.get(&request_id).cloned() else {
      return;
    };
    // BiDi sometimes omits responseStarted (e.g. cache-served responses);
    // synthesise the Response here if necessary.
    if req.existing_response().await.is_none() {
      if let Some(resp) = params.get("response") {
        let response = self.build_response(req.clone(), resp, &request_id);
        self.responses.lock().await.insert(request_id.clone(), response.clone());
        req.set_response(&response).await;
        emitter.emit(PageEvent::Response(response));
      }
    }
    if let Some(resp) = self.responses.lock().await.get(&request_id).cloned() {
      resp.finish_success().await;
    }
    emitter.emit(PageEvent::RequestFinished(req));
  }

  async fn on_fetch_error(self: &Arc<Self>, params: &serde_json::Value, emitter: &EventEmitter) {
    let Some(request_id) = params
      .get("request")
      .and_then(|r| r.get("request"))
      .and_then(|v| v.as_str())
    else {
      return;
    };
    let request_id = request_id.to_string();
    let Some(req) = self.requests.lock().await.get(&request_id).cloned() else {
      return;
    };
    let error_text = params
      .get("errorText")
      .and_then(|v| v.as_str())
      .unwrap_or("net::ERR_FAILED")
      .to_string();
    req.set_failure(error_text.clone()).await;
    if let Some(resp) = self.responses.lock().await.get(&request_id).cloned() {
      resp.finish_failure(error_text).await;
    }
    emitter.emit(PageEvent::RequestFailed(req));
  }

  fn build_response(self: &Arc<Self>, request: NetworkRequest, resp: &serde_json::Value, request_id: &str) -> Response {
    let url = resp.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let status = resp.get("status").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let status_text = resp
      .get("statusText")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();
    let from_service_worker = resp
      .get("fromCache")
      .and_then(serde_json::Value::as_bool)
      .unwrap_or(false);
    let headers = resp.get("headers").map(parse_bidi_headers).unwrap_or_default();
    let body_fn = self.make_body_fn(request_id);
    let raw_headers_fn = self.make_raw_headers_fn(request_id);
    Response::new(ResponseInit {
      request,
      url,
      status,
      status_text,
      from_service_worker,
      http_version: None,
      headers,
      remote_addr: parse_bidi_remote_addr(resp),
      security_details: parse_bidi_security_details(resp),
      body_fn: Some(body_fn),
      raw_headers_fn: Some(raw_headers_fn),
    })
  }

  fn make_body_fn(self: &Arc<Self>, request_id: &str) -> BodyFn {
    let session = self.session.clone();
    let request_id = request_id.to_string();
    Arc::new(move || {
      let session = session.clone();
      let request_id = request_id.clone();
      Box::pin(async move {
        let resp = session
          .transport
          .send_command(
            "network.getData",
            json!({"request": request_id, "dataType": "response"}),
          )
          .await
          .map_err(|e| {
            // Firefox discards response body bytes for non-intercepted
            // responses; `network.getData` then returns "no such network
            // data". Mirror Playwright's own BiDi backend behaviour and
            // surface this as a typed `Unsupported` so callers can
            // distinguish "Firefox dropped it" from a real protocol
            // failure.
            if e.contains("no such network data") {
              crate::error::FerriError::Unsupported(
                "Response body unavailable on BiDi without network interception (Firefox discards bytes after response)".into(),
              )
            } else {
              crate::error::FerriError::Protocol {
                method: "network.getData".into(),
                message: e,
              }
            }
          })?;
        let bytes = resp.get("bytes").and_then(|b| b.get("value")).and_then(|v| v.as_str());
        let data = bytes.unwrap_or("");
        base64::engine::general_purpose::STANDARD
          .decode(data)
          .map_err(|e| crate::error::FerriError::Other(format!("base64 decode: {e}")))
      })
    })
  }

  fn make_raw_headers_fn(self: &Arc<Self>, request_id: &str) -> RawHeadersFn {
    let tracker = self.clone();
    let request_id = request_id.to_string();
    Arc::new(move || {
      let tracker = tracker.clone();
      let request_id = request_id.clone();
      Box::pin(async move {
        if let Some(resp) = tracker.responses.lock().await.get(&request_id) {
          return Ok(resp.headers_array().await);
        }
        Ok(Vec::new())
      })
    })
  }
}

fn parse_bidi_remote_addr(_resp: &serde_json::Value) -> Option<RemoteAddr> {
  // WebDriver BiDi (W3C draft) does not currently surface remote IP /
  // port on `network.responseStarted` / `responseCompleted` payloads —
  // Playwright's own BiDi backend leaves the field unset for the same
  // reason. Returning `None` matches the other backend behaviours when
  // the field is missing rather than guessing.
  None
}

fn parse_bidi_security_details(resp: &serde_json::Value) -> Option<SecurityDetails> {
  // BiDi exposes `securityDetails` only for redirected responses on
  // some implementations; treat absence as None.
  resp
    .get("securityDetails")
    .and_then(|s| s.as_object())
    .map(|obj| SecurityDetails {
      protocol: obj.get("protocol").and_then(|v| v.as_str()).map(String::from),
      subject_name: obj.get("subjectName").and_then(|v| v.as_str()).map(String::from),
      issuer: obj.get("issuer").and_then(|v| v.as_str()).map(String::from),
      valid_from: obj.get("validFrom").and_then(serde_json::Value::as_f64),
      valid_to: obj.get("validTo").and_then(serde_json::Value::as_f64),
    })
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
          let value = entry
            .get("value")
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
          Some((name.to_string(), value.to_string()))
        })
        .collect()
    })
    .unwrap_or_default()
}

/// Execute a route action via `BiDi` network commands.
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

/// Parse a `BiDi` network cookie into our `CookieData`.
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
  let secure = c.get("secure").and_then(serde_json::Value::as_bool).unwrap_or(false);
  let http_only = c.get("httpOnly").and_then(serde_json::Value::as_bool).unwrap_or(false);
  let expires = c.get("expiry").and_then(serde_json::Value::as_f64);
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
