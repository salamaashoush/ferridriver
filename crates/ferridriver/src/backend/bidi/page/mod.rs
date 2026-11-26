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
  AnyElement, AxNodeData, AxProperty, BackgroundColor, CookieData, FrameInfo, ImageFormat, MetricData, NavLifecycle,
  ScreenshotCapture, ScreenshotScale,
};
use crate::console_message::{ConsoleMessage, ConsoleMessageLocation};
use crate::error::{FerriError, Result};
use crate::events::{EventEmitter, PageEvent};
use crate::network::{
  self, BodyFn, RemoteAddr, Request as NetworkRequest, RequestInit, Response, ResponseInit, SecurityDetails,
};
use crate::state::DialogEvent;
use ferridriver_types::keyboard::KeyStroke;

mod actions;
mod capture;
mod emulation;
mod eval;
mod interception;
mod listeners;
mod navigation;

pub(crate) use listeners::NetworkSettings;
use listeners::{
  color_scheme_feature, download_behavior_params, execute_bidi_route_action, parse_bidi_cookie, parse_bidi_headers,
  permissions_origin,
};

/// `BiDi` channel name the exposed-binding shim sends its payloads
/// through (`script.addPreloadScript` channel argument →
/// `script.message` events). Playwright's analogue is
/// `kPlaywrightBindingChannel` in `bidiPage.ts`.
const BINDING_CHANNEL: &str = "ferridriverBindingChannel";

/// Prefix of the identifier of an init script installed for a whole
/// user context rather than one page.
const CONTEXT_SCRIPT: &str = "context-init-";

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
  if ty == "node"
    && let Some(shared_id) = arg.get("sharedId").and_then(|v| v.as_str())
  {
    let handle = arg
      .get("handle")
      .and_then(|v| v.as_str())
      .map(std::string::ToString::to_string);
    return crate::js_handle::JSHandleBacking::Remote(crate::js_handle::HandleRemote::Bidi {
      shared_id: shared_id.to_string(),
      handle,
    });
  }
  if matches!(
    ty,
    "object" | "array" | "map" | "set" | "function" | "error" | "promise" | "symbol" | "window" | "weakmap" | "weakset"
  ) && let Some(h) = arg.get("handle").and_then(|v| v.as_str())
  {
    return crate::js_handle::JSHandleBacking::Remote(crate::js_handle::HandleRemote::Bidi {
      shared_id: String::new(),
      handle: Some(h.to_string()),
    });
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

use crate::backend::common::{f64_to_u64_saturating, split_error_message as split_error_text, u64_to_u32_saturating};

/// Build the `error.stack` string for a `BiDi` JS error. Mirrors
/// `bidiPage.ts:280-283` byte-for-byte: the first line is the original
/// `text`, followed by one `    at <func> (<url>:<line+1>:<col+1>)`
/// line per `stackTrace.callFrames` entry. `BiDi` line / column
/// numbers are 0-based; Playwright adds `+ 1` to match the user-facing
/// 1-based JS convention.
/// Source location for a `BiDi` page error: the top `callFrames` entry,
/// used raw (`BiDi` line/column are 0-based and `bidiPage.ts` does NOT
/// add `+ 1` here — only the stack-string path does). Falls back to
/// `{ "", 1, 1 }`, matching Playwright's `bidiPage.ts:289`.
fn bidi_stack_to_location(stack: Option<&serde_json::Value>) -> crate::console_message::ConsoleMessageLocation {
  let frame = stack
    .and_then(|s| s.get("callFrames"))
    .and_then(|v| v.as_array())
    .and_then(|frames| frames.first());
  match frame {
    Some(frame) => crate::console_message::ConsoleMessageLocation {
      url: frame.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
      line_number: u32::try_from(frame.get("lineNumber").and_then(serde_json::Value::as_u64).unwrap_or(0))
        .unwrap_or(u32::MAX),
      column_number: u32::try_from(
        frame
          .get("columnNumber")
          .and_then(serde_json::Value::as_u64)
          .unwrap_or(0),
      )
      .unwrap_or(u32::MAX),
    },
    None => crate::console_message::ConsoleMessageLocation {
      url: String::new(),
      line_number: 1,
      column_number: 1,
    },
  }
}

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

/// Resample a device-pixel capture of `rect` to its CSS size.
fn to_css_pixels(bytes: Vec<u8>, rect: &crate::options::ClipRect, capture: &ScreenshotCapture) -> Result<Vec<u8>> {
  let image = image::load_from_memory(&bytes).map_err(|error| FerriError::backend(error.to_string()))?;
  let size = |value: f64| {
    u32::try_from(f64_to_u64_saturating(value.round()))
      .unwrap_or(u32::MAX)
      .max(1)
  };
  let (width, height) = (size(rect.width), size(rect.height));
  if image.width() == width && image.height() == height {
    return Ok(bytes);
  }
  let image = image.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
  let mut output = std::io::Cursor::new(Vec::new());
  match capture.format {
    ImageFormat::Jpeg => image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, capture.quality.unwrap_or(80))
      .encode_image(&image.to_rgb8())
      .map_err(|error| FerriError::backend(error.to_string()))?,
    ImageFormat::Png | ImageFormat::Webp => image
      .write_to(&mut output, image::ImageFormat::Png)
      .map_err(|error| FerriError::backend(error.to_string()))?,
  }
  if capture.format == ImageFormat::Webp {
    return crate::backend::encode_webp_from_png(output.get_ref(), capture.quality);
  }
  Ok(output.into_inner())
}

/// Every field `apply_context_options` failed to apply, then every one
/// this backend cannot honour, as one error.
fn context_option_errors(
  applied: Vec<(&'static str, Result<()>)>,
  opts: &crate::options::BrowserContextOptions,
) -> Result<()> {
  let errs: Vec<String> = applied
    .into_iter()
    .filter_map(|(label, r)| match r {
      Err(e) => Some(format!("{label}: {e}")),
      Ok(()) => None,
    })
    .chain(unsupported_context_options(opts))
    .collect();
  if errs.is_empty() {
    Ok(())
  } else {
    Err(FerriError::Backend(errs.join("; ")))
  }
}

/// Explicit unsupported fields — Firefox `BiDi` lacks these primitives.
/// Surfacing the gap to the caller (vs. silently dropping) matches
/// Playwright's Rule-4 "typed Unsupported for real protocol gaps".
///
/// Each is refused only when it asks for the behaviour the backend
/// cannot produce. `hasTouch: false` asks for the default, which is
/// what Firefox already does — and it is what a spread of a desktop
/// device descriptor carries, so refusing it made `{ ...devices
/// ['Desktop Firefox'] }` fail on Firefox.
fn unsupported_context_options(opts: &crate::options::BrowserContextOptions) -> impl Iterator<Item = String> {
  [
    ("bypassCSP", opts.bypass_csp == Some(true)),
    // browsingContext.setViewport has no touch-emulation field and
    // Firefox exposes no other command for it.
    ("hasTouch", opts.has_touch == Some(true)),
  ]
  .into_iter()
  .filter(|(_, asked)| *asked)
  .map(|(label, _)| format!("{label}: BiDi/Firefox backend does not yet support this context option"))
}

/// Insert text where the document's focus is, or answer the window of the
/// frame element the focus is in. The editing command inserts it as
/// typing does: at the caret or over the selection, a line break in a
/// contenteditable as `<br>`, with a trusted `input` event, as an edit the
/// page did not make itself (a framework tracking the value it set sees
/// the change) and one `change` reports when the field is left. Where the
/// command takes no text in a field that can be typed into, the value is
/// set and an `input` event sent.
const INSERT_TEXT: &str = r"(text) => {
  let element = document.activeElement;
  while (element && element.shadowRoot)
    element = element.shadowRoot.activeElement;
  if (!element)
    return undefined;
  const name = element.nodeName.toLowerCase();
  if (name === 'iframe' || name === 'frame')
    return element.contentWindow;
  const control = name === 'input' || name === 'textarea';
  const editable = !control && element instanceof HTMLElement && element.isContentEditable;
  if (!control && !editable)
    return undefined;
  let range = null;
  if (editable) {
    const selection = window.getSelection();
    range = selection.rangeCount ? selection.getRangeAt(0) : null;
    if (!range || !element.contains(range.commonAncestorContainer)) {
      range = document.createRange();
      range.selectNodeContents(element);
      range.collapse(true);
      selection.removeAllRanges();
      selection.addRange(range);
    }
  }
  if (document.execCommand('insertText', false, text))
    return undefined;
  // A field no one can type into takes no text.
  if (control && (element.readOnly || element.disabled))
    return undefined;
  if (control) {
    const start = element.selectionStart;
    if (start === null) {
      element.value += text;
    } else {
      const end = element.selectionEnd;
      element.value = element.value.slice(0, start) + text + element.value.slice(end);
      element.setSelectionRange(start + text.length, start + text.length);
    }
  } else {
    range.deleteContents();
    const lines = text.split('\n');
    for (let i = lines.length - 1; i >= 0; i--) {
      range.insertNode(document.createTextNode(lines[i]));
      if (i > 0)
        range.insertNode(document.createElement('br'));
    }
    range.collapse(false);
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
  }
  element.dispatchEvent(new InputEvent('input', { data: text, bubbles: true, composed: true }));
  return undefined;
}";

/// Page handle for the `BiDi` backend. Cheaply cloneable (single `Arc` pointer).
#[derive(Clone)]
pub struct BidiPage(Arc<BidiPageInner>);

impl std::ops::Deref for BidiPage {
  type Target = BidiPageInner;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

#[doc(hidden)]
pub struct BidiPageInner {
  pub shared: crate::backend::SharedPageState,
  pub(crate) session: Arc<BidiSession>,
  pub(crate) context_id: Arc<str>,
  /// Owning user context (`"default"` for the default context). Cookie
  /// commands partition on `{type: "storageKey", userContext}` — the
  /// browsing-context partition (`{type: "context"}`) makes Firefox
  /// return each cookie once per matching storage key, duplicating
  /// every entry in `storage.getCookies` (and Playwright's own `BiDi`
  /// backend partitions on the user context for the same reason).
  pub(crate) user_context: Arc<str>,
  routes: Arc<RwLock<Vec<crate::route::RegisteredRoute>>>,
  intercept_ids: Arc<RwLock<Vec<String>>>,
  preload_scripts: Arc<RwLock<FxHashMap<String, String>>>,
  exposed_fns: Arc<RwLock<FxHashMap<String, crate::backend::PageBinding>>>,
  /// Whether `__playwright__binding__controller__` is installed, the way
  /// functions passed as arguments call back.
  binding_controller: Arc<tokio::sync::Mutex<bool>>,
  /// Manager for lazy engine injection.
  injected_script: Arc<InjectedScriptManager>,
  /// Most recent main-document `Request` observed by the `BiDi` network
  /// listener. Populated when `BidiNetworkTracker` sees a request with
  /// `navigation` set. Consumed by `goto` / `reload` / history
  /// traversals to resolve the final main-document `Response`.
  nav_request_slot: crate::network::NavRequestSlot,
  event_barrier: Arc<std::sync::Mutex<Option<super::transport::EventBarrier>>>,
  pub(crate) screencast_hub: crate::backend::screencast::ScreencastHub,
  /// Per-page temp directory Firefox is configured to write downloads
  /// into (via `browser.setDownloadBehavior({ downloadBehavior: { type:
  /// 'allowed', destinationFolder }, userContexts })`). Held as
  /// `Arc<TempDir>` so the directory lives as long as any `Download`
  /// referencing a file under it.
  pub downloads_dir: Arc<tempfile::TempDir>,
  /// This page's own folder inside `downloads_dir`. See `create`.
  pub downloads_subdir: Arc<std::path::Path>,
  /// Whether this context accepts downloads at all
  /// (`BrowserContextOptions::accept_downloads`, default `true`).
  ///
  /// Recorded rather than sent from `apply_context_options`: there is
  /// one `browser.setDownloadBehavior` answer per browser, and
  /// `attach_listeners` used to send an unconditional `allowed` from a
  /// detached task, so an explicit `denied` was overwritten by whichever
  /// finished last.
  pub accept_downloads: Arc<std::sync::atomic::AtomicBool>,
  /// A popup's first navigation, set when the popup listener adopts it.
  first_navigation: Arc<std::sync::OnceLock<Arc<super::frames::FirstNavigation>>>,
  /// Last viewport applied via `browsingContext.setViewport`, so a repeat call
  /// with identical dimensions/scale (e.g. `apply_context_options` right after
  /// `BidiBrowser::new_page`) skips the redundant round trip.
  emulated_viewport: Arc<std::sync::Mutex<Option<crate::options::ViewportConfig>>>,
  pub(crate) network_idle: Arc<crate::network_idle::NetworkIdle>,
  /// Idempotent latch for the route (`network.beforeRequestSent`) listener.
  /// Spawned once per page: it reads the shared `routes` map live, so a
  /// `route`/`unroute` cycle (which drains `intercept_ids`) must NOT
  /// re-spawn it — a second listener would double-dispatch `continueRequest`
  /// for every blocked request.
  pub(crate) route_listener_started: Arc<AtomicBool>,
  /// The blocked requests of a page the browser opened by itself, handed
  /// on in wire order by the listener that saw it created. A tap of the
  /// page's own would start after its first request had gone by.
  forwarded_requests: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<super::transport::BidiEvent>>>>,
  /// Abort handles for this page's long-lived listener tasks (the
  /// main event listener, the route listener, the screencast
  /// navigation tap). They subscribe to the SESSION-wide event
  /// channel, which stays open for the browser's whole life — without
  /// an abort in [`BidiPage::close_page`] every closed page keeps waking
  /// on all future `BiDi` events and pins its managers/emitter until
  /// the browser exits.
  listener_tasks: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
  /// Credentials answering `network.authRequired` challenges (context
  /// option `httpCredentials` + `browserContext.setHTTPCredentials`).
  /// Shared with [`BidiNetworkTracker`], which sends
  /// `network.continueWithAuth` — the session registers a permanent
  /// `authRequired`-phase intercept at connect, so unanswered
  /// challenges are cancelled (surfacing the native 401) rather than
  /// left paused.
  pub(crate) http_credentials: Arc<std::sync::Mutex<Option<Vec<crate::options::HttpCredentials>>>>,
  /// The context's proxy and TLS posture: the proxy's credentials answer
  /// its `407`s, and a route's `continue({ url })`, which this backend
  /// re-issues itself, leaves through the same proxy the page does.
  network: Arc<std::sync::Mutex<NetworkSettings>>,
  /// Context-level permission grants. `BiDi`'s
  /// `permissions.setPermission` requires a concrete origin, but our
  /// context-level grant is origin-global — mirror Playwright's `BiDi`
  /// backend (`bidiBrowser.ts::doGrantGlobalPermissionsForURL`) by
  /// re-granting for each newly navigated origin from the event
  /// listener.
  pub(crate) permissions: Arc<std::sync::Mutex<BidiPermissions>>,
  /// Maps a retained `BiDi` reference (a `handle` UUID or a node
  /// `sharedId`) to the browsing context whose realm it was created in.
  ///
  /// Unlike CDP `RemoteObjectId`s (resolvable from any call against the
  /// same target) `BiDi` `handle`s are realm-scoped and a `sharedId`
  /// resolves only in realms of the same browsing context. A handle
  /// produced by `frame.evaluateHandle` in a child frame must therefore
  /// be re-targeted at that child's context when later passed as a
  /// `script.callFunction` argument (e.g. `JSHandle::evaluate`, which
  /// the shared core dispatches with `frame_id = None`). The shared
  /// `HandleRemote::Bidi` wire shape carries no context, so we recover
  /// it here, inside the backend, keyed on the reference string.
  pub(crate) handle_realms: Arc<std::sync::Mutex<FxHashMap<String, Arc<str>>>>,
}

impl std::ops::Deref for BidiPageInner {
  type Target = crate::backend::SharedPageState;

  fn deref(&self) -> &Self::Target {
    &self.shared
  }
}

/// Permission-grant bookkeeping for one page's context. `global` is
/// the context-wide grant list (from the `permissions` context option /
/// `browserContext.grantPermissions`); `granted` records which
/// permissions were actually sent to Firefox per origin, so navigations
/// only re-grant what's missing and `clear_permissions` can reset
/// exactly what was granted.
#[derive(Default)]
pub(crate) struct BidiPermissions {
  global: Vec<String>,
  granted: FxHashMap<String, Vec<String>>,
}

pub struct InjectedScriptManager {
  /// Browsing-context ids that already have `window.__fd`. `BiDi` has no
  /// single "page" realm — every frame (including freshly-attached /
  /// `srcdoc` / `data:` child contexts) is its own browsing context and
  /// needs the engine injected separately before a frame-scoped query
  /// can run there. Tracked per-context so the first use of a child
  /// context injects on demand.
  injected: std::sync::Mutex<rustc_hash::FxHashSet<String>>,
}

impl InjectedScriptManager {
  fn new() -> Self {
    Self {
      injected: std::sync::Mutex::new(rustc_hash::FxHashSet::default()),
    }
  }

  /// Drop all injection state — a top-level navigation (goto/reload)
  /// tears down every context, children included.
  fn reset(&self) {
    if let Ok(mut s) = self.injected.lock() {
      s.clear();
    }
  }

  /// Drop one context's injection state — that specific context
  /// navigated, so its `window.__fd` is gone and must be re-injected on
  /// next use. A child iframe reloading must not invalidate the main
  /// context's engine (and vice-versa).
  fn reset_context(&self, ctx: &str) {
    if ctx.is_empty() {
      return;
    }
    if let Ok(mut s) = self.injected.lock() {
      s.remove(ctx);
    }
  }

  /// Engine into the page's main context. Back-compat entry point used
  /// by `ensure_engine_injected` / `injected_script`.
  async fn ensure(&self, page: &BidiPage) -> Result<()> {
    let ctx = Arc::clone(&page.context_id);
    self.ensure_in(page, &ctx).await
  }

  /// Engine into `ctx` (idempotent per context). The lazy-inject JS
  /// itself guards against double-install (`if (window.__fd) return`),
  /// so a racing concurrent call is harmless — the membership check is
  /// a fast-path, not a correctness lock.
  async fn ensure_in(&self, page: &BidiPage, ctx: &str) -> Result<()> {
    if let Ok(s) = self.injected.lock()
      && s.contains(ctx)
    {
      return Ok(());
    }
    let full_check_js = crate::selectors::build_lazy_inject_js();
    page
      .cmd(
        "script.evaluate",
        json!({
          "expression": full_check_js,
          "target": {"context": ctx},
          "awaitPromise": true,
          "resultOwnership": "none"
        }),
      )
      .await?;
    if let Ok(mut s) = self.injected.lock() {
      s.insert(ctx.to_string());
    }
    Ok(())
  }
}
impl BidiPage {
  /// Create a new `BidiPage` and enable required domains (inject engine, etc.).
  /// This is the `BiDi` equivalent of CDP's `enable_domains()`.
  pub(crate) fn create(
    session: Arc<BidiSession>,
    context_id: String,
    user_context: Option<&str>,
    downloads_dir: Arc<tempfile::TempDir>,
  ) -> Self {
    // BiDi handles navigation-aware injection via script.addPreloadScript.
    // Domain enables are deferred (lazy injection), unlike CDP's upfront enable_domains().
    //
    // Each page downloads into its own subdirectory of the browser's
    // temp dir. Firefox derives `suggestedFilename` by scanning the
    // destination folder for a free name, so two pages sharing one
    // folder turn a second `greeting.txt` into `greeting(1).txt`.
    let downloads_subdir = downloads_dir.path().join(&context_id);
    let _ = std::fs::create_dir_all(&downloads_subdir);
    let shared = crate::backend::SharedPageState::new(&context_id);
    Self(Arc::new(BidiPageInner {
      shared,
      downloads_subdir: Arc::from(downloads_subdir),
      accept_downloads: Arc::new(std::sync::atomic::AtomicBool::new(true)),
      session,
      context_id: Arc::from(context_id),
      user_context: Arc::from(user_context.unwrap_or("default")),
      routes: Arc::new(RwLock::new(Vec::new())),
      intercept_ids: Arc::new(RwLock::new(Vec::new())),
      preload_scripts: Arc::new(RwLock::new(FxHashMap::default())),
      exposed_fns: Arc::new(RwLock::new(FxHashMap::default())),
      binding_controller: Arc::new(tokio::sync::Mutex::new(false)),
      injected_script: Arc::new(InjectedScriptManager::new()),
      nav_request_slot: crate::network::NavRequestSlot::new(),
      event_barrier: Arc::new(std::sync::Mutex::new(None)),
      screencast_hub: crate::backend::screencast::ScreencastHub::default(),
      downloads_dir,
      first_navigation: Arc::default(),
      emulated_viewport: Arc::default(),
      network_idle: Arc::default(),
      route_listener_started: Arc::new(AtomicBool::new(false)),
      forwarded_requests: Arc::new(std::sync::Mutex::new(None)),
      listener_tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
      http_credentials: Arc::new(std::sync::Mutex::new(None)),
      network: Arc::default(),
      permissions: Arc::new(std::sync::Mutex::new(BidiPermissions::default())),
      handle_realms: Arc::new(std::sync::Mutex::new(FxHashMap::default())),
    }))
  }

  /// Take up a page the browser opened by itself: it starts with its
  /// context's routes, and resolves the blocked requests `requests`
  /// brings, the first of which its own tap would be too late for.
  /// A popup starts on an empty document that is not the one it was
  /// opened for, unless it was opened for no URL or `about:blank`. It is
  /// handed on once the navigation it started has committed (or failed, or
  /// the popup closed), so the page its context lists is the page opened.
  /// The reply to the round trip comes after any `navigationStarted` the
  /// browser sent first, so a popup with none by then starts no navigation.
  pub(crate) async fn first_document_committed(&self) -> Result<()> {
    let Some(navigation) = self.first_navigation.get().cloned() else {
      return Ok(());
    };
    if navigation.is_settled() {
      return Ok(());
    }
    self
      .session
      .transport
      .send(&ferridriver_bidi_proto::session::Status {})
      .await?;
    navigation.settled_if_started().await;
    Ok(())
  }

  pub(super) fn adopt_popup(
    &self,
    requests: tokio::sync::mpsc::UnboundedReceiver<super::transport::BidiEvent>,
    first_navigation: Option<Arc<super::frames::FirstNavigation>>,
  ) {
    if let Some(navigation) = first_navigation
      && self.first_navigation.set(navigation).is_err()
    {
      tracing::debug!("popup adopted twice");
    }
    if let Ok(mut routes) = self.routes.try_write() {
      *routes = self.session.scopes.routes(&self.user_context);
    }
    *self
      .forwarded_requests
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(requests);
    self.start_route_listener();
  }

  /// Announce the dedicated workers this page's realms start, and close
  /// them with their realms or the page.
  fn spawn_worker_listener(&self) {
    let mut tap = self.session.transport.tap_events();
    let session = Arc::clone(&self.session);
    let context = Arc::clone(&self.context_id);
    let emitter = self.events.clone();
    let task = tokio::spawn(async move {
      let mut realms = super::worker::WorkerRealms::new(Arc::clone(&session), &context);
      if let Ok(reply) = session
        .transport
        .send_command("script.getRealms", json!({ "context": &*context }))
        .await
      {
        realms.seed(&reply);
      }
      while let Some(event) = tap.recv().await {
        if let Some(worker) = realms.observe(&event.method, &event.params) {
          emitter.emit(PageEvent::Worker(worker));
        }
        if event.method == "browsingContext.contextDestroyed"
          && event.params.get("context").and_then(|v| v.as_str()) == Some(&*context)
        {
          break;
        }
      }
      realms.close_all();
    });
    self.track_listener(task.abort_handle());
  }

  fn track_listener(&self, handle: tokio::task::AbortHandle) {
    if let Ok(mut guard) = self.listener_tasks.lock() {
      guard.push(handle);
    }
  }

  /// Record that the `BiDi` reference `key` (a `handle` UUID or node
  /// `sharedId`) lives in browsing context `ctx`. No-op for empty keys
  /// or when `ctx` is the page's main context (the default target).
  fn remember_handle_realm(&self, key: &str, ctx: &str) {
    if key.is_empty() || ctx == &*self.context_id {
      return;
    }
    if let Ok(mut map) = self.handle_realms.lock() {
      map.insert(key.to_string(), Arc::from(ctx));
    }
  }

  /// Look up the browsing context a previously-retained handle/sharedId
  /// belongs to, if it was created in a non-main realm.
  fn handle_realm(&self, key: &str) -> Option<Arc<str>> {
    if key.is_empty() {
      return None;
    }
    self.handle_realms.lock().ok().and_then(|map| map.get(key).cloned())
  }

  /// Drop a single reference's realm mapping once the remote is released.
  fn forget_handle_realm(&self, key: &str) {
    if key.is_empty() {
      return;
    }
    if let Ok(mut map) = self.handle_realms.lock() {
      map.remove(key);
    }
  }

  /// Drop every realm mapping — a top-level navigation discards all
  /// realms, so every retained reference becomes invalid.
  fn clear_handle_realms(&self) {
    if let Ok(mut map) = self.handle_realms.lock() {
      map.clear();
    }
  }

  /// Helper: send a `BiDi` command.
  async fn cmd(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    self.session.send_command(method, params).await
  }

  pub(crate) fn is_retryable_context_error(err: &str) -> bool {
    err.contains("DiscardedBrowsingContextError")
      || err.contains("BrowsingContext does no longer exist")
      || err.contains("BiDi error 'no such frame'")
      || err.contains("BiDi error 'no such window'")
  }

  /// # Errors
  ///
  /// Returns the error of the `script.evaluate` probe of `document.readyState`. A discarded or
  /// missing browsing context is retried for up to 2 seconds before its error is returned.
  pub async fn wait_until_ready(&self) -> Result<()> {
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
        Err(err) if Self::is_retryable_context_error(&err.to_string()) && tokio::time::Instant::now() < deadline => {
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
  async fn eval_internal(&self, expression: &str, context: &str) -> Result<Option<serde_json::Value>> {
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
      serde_json::from_value(result).map_err(|e| FerriError::Backend(format!("BiDi evaluate parse: {e}")))?;

    match eval_result {
      EvaluateResult::Success { result } => Ok(result.to_json()),
      EvaluateResult::Exception { exception_details } => {
        Err(FerriError::evaluation(format!("JS error: {}", exception_details.text)))
      },
    }
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
