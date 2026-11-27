//! Unified CDP backend -- Chrome `DevTools` Protocol over pipes or WebSocket.
//!
//! Uses a single concrete [`transport::CdpTransport`] backed by either the
//! fd 3/4 debugging pipe or a WebSocket connection. Both share identical page,
//! element, and event handling logic without monomorphising twice.
//!
//! Navigation follows Bun's ChromeBackend.cpp architecture: register a oneshot
//! waiter before sending Page.navigate, then await the waiter which resolves
//! when the reader task sees Page.loadEventFired for that session.

mod browser;
mod connections;
mod element;
pub(crate) mod lightpanda;
mod network_tracker;
mod page_capture;
mod page_emulation;
mod page_eval;
mod page_input;
mod page_interception;
mod page_listeners;
mod page_navigation;
mod paused;
pub mod pipe;
pub(crate) mod recording;
mod recycle;
pub use recycle::RetireHints;
mod renderer;
mod screenshot;
pub mod transport;
pub mod ws;

#[cfg(test)]
mod key_event_tests;
#[cfg(test)]
mod listener_tests;
#[cfg(test)]
mod startup_tests;

use base64::Engine as _;

use super::{
  AnyElement, AnyPage, AxNodeData, AxProperty, BackgroundColor, CookieData, ImageFormat, MetricData, ScreenshotCapture,
  ScreenshotScale,
};
use crate::console_message::ConsoleMessage;
use crate::error::{FerriError, Result};
use crate::network::{
  self, BodyFn, HeaderEntry, Headers, RawHeadersFn, RemoteAddr, Request as NetworkRequest, RequestInit, RequestSizes,
  RequestTiming, Response, ResponseInit, SecurityDetails, WebSocket,
};
use ferridriver_types::keyboard::KeyStroke;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use transport::CdpTransport;

pub(crate) const LC_COMMIT: u8 = 0b001;
pub(crate) const LC_DOMCONTENTLOADED: u8 = 0b010;
pub(crate) const LC_LOAD: u8 = 0b100;

/// The name of the isolated world ferridriver's own scripts run in where
/// the page's changes to the DOM objects must not reach them.
pub(crate) const UTILITY_WORLD: &str = "__ferridriver_utility";

/// A dedicated worker's own session, auto-attached under its page's.
pub struct CdpWorker {
  transport: Arc<CdpTransport>,
  session_id: Arc<str>,
}

impl Clone for CdpWorker {
  fn clone(&self) -> Self {
    Self {
      transport: Arc::clone(&self.transport),
      session_id: Arc::clone(&self.session_id),
    }
  }
}

impl crate::worker::WorkerSession for CdpWorker {
  fn evaluate<'a>(&'a self, expression: &'a str) -> futures::future::BoxFuture<'a, Result<serde_json::Value>> {
    Box::pin(self.evaluate_expression(expression))
  }
}

impl CdpWorker {
  async fn evaluate_expression(&self, expression: &str) -> Result<serde_json::Value> {
    let response = self
      .transport
      .send_command(
        Some(&self.session_id),
        "Runtime.evaluate",
        &serde_json::json!({ "expression": expression, "awaitPromise": true, "returnByValue": true }),
      )
      .await?;
    if let Some(exception) = response.get("exceptionDetails") {
      return Err(FerriError::evaluation(cdp_get_exception_message(exception)));
    }
    Ok(
      response
        .pointer("/result/value")
        .cloned()
        .unwrap_or(serde_json::Value::Null),
    )
  }
}

/// Which CDP server implementation sits on the other side of the wire.
///
/// Most CDP logic is shared verbatim between Chromium and Lightpanda; this
/// tag isolates the narrow places where Lightpanda's server differs (headless
/// only with no OS window chrome, PNG-only screenshots, omitted `frame.name`
/// on `Page.frameNavigated`, and unsupported Chrome-only domains).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CdpFlavor {
  #[default]
  Chromium,
  Lightpanda,
}

impl CdpFlavor {
  pub(crate) const fn is_lightpanda(self) -> bool {
    matches!(self, Self::Lightpanda)
  }
}

// ---- CdpBrowser --------------------------------------------------------

pub struct CdpBrowser {
  transport: Arc<CdpTransport>,
  child: Arc<tokio::sync::Mutex<Option<super::process::ChildGroup>>>,
  /// Track targetId -> sessionId for already-attached targets. Shared
  /// across clones: it is a cache of what the browser told us, and the
  /// attach listener (which has no `self`) prunes it on detach — a
  /// per-clone copy would keep growing one dead entry per page opened.
  attached_targets: AttachedTargets,
  /// Contexts emptied for reuse (see [`recycle`]). Shared across clones:
  /// a context retired through one handle is taken through another.
  spares: Arc<recycle::Spares>,
  /// Product version string captured from CDP `Browser.getVersion().product`
  /// at handshake time. Matches what Playwright surfaces via `browser.version()`
  /// (its initializer stores the same value and returns it synchronously).
  /// Example: `"HeadlessChrome/120.0.6099.109"`.
  version: Arc<str>,
  pub(crate) isolated_contexts: bool,
  flavor: CdpFlavor,
  /// Whether this browser draws real window chrome, so a viewport has
  /// to account for the title bar and borders when it sizes the window
  /// (Playwright's `browser.options.headful`).
  ///
  /// Read off the product string rather than a launch flag: the connect
  /// and discover paths never saw one, and the browser on the other end
  /// of a discover command is the only thing that knows how it started.
  headful: bool,
  /// Held by whichever page is being captured in a browser that draws
  /// only the tab in front. See [`Self::front_tab_turn`].
  front_tab: Arc<tokio::sync::Mutex<()>>,
  /// Popup announcement subscriptions (see
  /// [`crate::backend::PopupInfo`]). Arc-shared across browser clones
  /// so a pump subscribed through any clone observes popups claimed by
  /// the connect-time listener.
  popup_taps: crate::backend::PopupTaps,
  /// Ownership ledger between `new_page` and the attach listener (see
  /// [`CreateLedger`]). Arc-shared across clones — `PageOpenPlan`
  /// clones the browser, and its `new_page` must coordinate with the
  /// original's listener.
  create_ledger: Arc<CreateLedger>,
  /// Popups the attach listener has seen open and the pump has not yet
  /// registered.
  popups: crate::backend::PopupWatch,
  /// Owned `--user-data-dir` for launched browsers. Held as `Arc<TempDir>` so
  /// cheap `Clone`s share ownership, and the directory is removed from disk
  /// when the last handle drops (after the `Child` is killed via
  /// `kill_on_drop(true)`). `None` for `connect()` — we don't own the dir
  /// of a browser someone else launched.
  user_data_dir: Option<Arc<super::async_tempdir::AsyncTempDir>>,
  /// Whether `close` lets the launched browser exit by itself so a profile
  /// it keeps is written out. A throwaway profile, or a browser that keeps
  /// none, is killed instead.
  flushes_on_close: bool,
  /// For a server that holds one browser context per connection, the
  /// connections its contexts live on. `None`: every context is on this
  /// browser's own connection.
  connections: Option<Arc<connections::ContextConnections>>,
  /// One downloads directory per browser, shared by every page it
  /// opens. Chrome names downloads uniquely (`allowAndName`), so a
  /// per-page directory bought nothing and cost a mkdir on every page
  /// open plus a leaked directory whenever teardown was skipped.
  downloads_dir: Arc<DownloadsDir>,
  /// Abort handles for the two attach-listener tasks.
  ///
  /// Both hold an `Arc` clone of the transport while parked on one of its
  /// taps, so neither can end on its own: the transport keeps the tasks
  /// alive and the tasks keep the transport alive. Closing the browser has
  /// to cut that cycle, or the transport, its reader/writer tasks and its
  /// connection fd survive every closed browser.
  attach_tasks: Arc<AttachTasks>,
  webdriver: Option<Arc<dyn super::common::OwnedResource>>,
  pub(crate) android: Option<Arc<crate::android::AndroidOwner>>,
}

struct AttachTasks(Vec<tokio::task::AbortHandle>);

impl AttachTasks {
  fn abort(&self) {
    for handle in &self.0 {
      handle.abort();
    }
  }
}

impl Drop for AttachTasks {
  fn drop(&mut self) {
    self.abort();
  }
}

use browser::{
  AUTOATTACH_PARAMS_RAW, AttachedTargets, CreateLedger, EMPTY_PARAMS_RAW, ENABLED_TRUE_RAW, FRONT_TAB_ATTEMPTS,
  FRONT_TAB_DRAWN, default_emulated_media_params, default_emulated_media_raw, glob_to_cdp_url_pattern,
  metrics_params_for, window_insets,
};
use network_tracker::{NetworkTracker, PendingWebMcpInvocation, cookie_from_cdp, cookie_to_cdp};

/// Map a CDP protocol error indicating an unimplemented command (`wasn't found`)
/// or an explicit `"not supported"` refusal to [`FerriError::Unsupported`],
/// leaving transport, timeout, and other errors untouched.
pub(super) fn map_cdp_unsupported(error: FerriError, reason: &str) -> FerriError {
  if let FerriError::Protocol { ref message, .. } = error {
    let lower = message.to_ascii_lowercase();
    if lower.contains("wasn't found") || lower.contains("not supported") || lower.contains("not implemented") {
      return FerriError::unsupported(reason);
    }
  }
  error
}

// ---- CdpPage ------------------------------------------------------------

/// Recursively collect frame info from a CDP frame tree node.
/// Convert a CDP `Runtime.RemoteObject` JSON payload into a
/// [`crate::js_handle::JSHandleBacking`]. Remote-backed handles come
/// from objects with `objectId`; value-backed handles come from inline
/// primitives. Mirrors Playwright's
/// `crProtocolHelper.ts::createHandle(context, arg)` behaviour.
fn cdp_remote_object_to_backing(
  arg: &serde_json::Value,
  session_id: Option<Arc<str>>,
) -> crate::js_handle::JSHandleBacking {
  if let Some(obj_id) = arg.get("objectId").and_then(|v| v.as_str()) {
    return crate::js_handle::JSHandleBacking::Remote(crate::js_handle::HandleRemote::Cdp {
      object_id: Arc::from(obj_id),
      session_id,
    });
  }
  let value = arg.get("value").cloned().unwrap_or(serde_json::Value::Null);
  let ty = arg.get("type").and_then(|v| v.as_str()).unwrap_or("");
  let serialized = if value.is_null() {
    if ty == "undefined" {
      crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
    } else {
      crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Null)
    }
  } else {
    let mut ctx = crate::protocol::SerializationContext::default();
    crate::protocol::SerializedValue::from_json(&value, &mut ctx)
  };
  crate::js_handle::JSHandleBacking::Value(serialized)
}

/// Convert a CDP `Runtime.StackTrace` JSON payload into a
/// [`crate::console_message::ConsoleMessageLocation`] from its first
/// call frame. Mirrors Playwright's
/// `crProtocolHelper.ts::toConsoleMessageLocation` byte-for-byte.
fn cdp_stack_trace_to_location(stack: Option<&serde_json::Value>) -> crate::console_message::ConsoleMessageLocation {
  let Some(stack) = stack else {
    return crate::console_message::ConsoleMessageLocation::default();
  };
  let Some(frames) = stack.get("callFrames").and_then(|v| v.as_array()) else {
    return crate::console_message::ConsoleMessageLocation::default();
  };
  let Some(frame) = frames.first() else {
    return crate::console_message::ConsoleMessageLocation::default();
  };
  crate::console_message::ConsoleMessageLocation {
    url: frame.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    line_number: frame
      .get("lineNumber")
      .and_then(serde_json::Value::as_u64)
      .map_or(0, u64_to_u32_saturating),
    column_number: frame
      .get("columnNumber")
      .and_then(serde_json::Value::as_u64)
      .map_or(0, u64_to_u32_saturating),
  }
}

use super::common::{UTILITY_EVAL_WRAPPER, f64_to_u64_saturating, split_error_message, u64_to_u32_saturating};

/// A `Runtime.callFunctionOn` whose function threw answers without a
/// protocol error: the throw is in `exceptionDetails`, and a caller that
/// does not look reports success for a call that failed.
fn thrown_in_page(result: &serde_json::Value) -> Result<()> {
  match result.get("exceptionDetails") {
    Some(exception) => Err(FerriError::Backend(cdp_get_exception_message(exception))),
    None => Ok(()),
  }
}

/// Convert a CDP `Runtime.ExceptionDetails` JSON payload into a
/// [`crate::web_error::ErrorDetails`]. Mirrors Playwright's
/// `server/chromium/crProtocolHelper.ts::{getExceptionMessage, exceptionToError}`
/// byte-for-byte (see `/tmp/playwright/packages/playwright-core/src/server/chromium/crProtocolHelper.ts:28-100`):
///
/// 1. Build the combined message:
///    - If `exception.description` is set, use it (carries the full
///      `Error: message\n    at foo (bar.js:3:5)` form the engine
///      produces).
///    - Else if `exception.value` is set, stringify it.
///    - Else fall back to `exceptionDetails.text` and synthesise stack
///      frames by appending `\n    at <func> (<url>:<line>:<col>)` for
///      each `stackTrace.callFrames` entry.
/// 2. Split at the first line starting with `    at`. Everything before
///    is `{ name, message }` (parsed via `splitErrorMessage`'s `': '`
///    separator); everything from that line onward is `stack`.
/// 3. If the exception's remote-object preview exposes a `name`
///    property, override the parsed name with its value
///    (e.g. custom `Error` subclasses).
fn cdp_exception_to_error_details(exception_details: &serde_json::Value) -> crate::web_error::ErrorDetails {
  let message_with_stack = cdp_get_exception_message(exception_details);
  let lines: Vec<&str> = message_with_stack.split('\n').collect();
  let first_stack_idx = lines.iter().position(|l| l.starts_with("    at"));
  let (message_with_name, stack) = match first_stack_idx {
    Some(idx) => (lines[..idx].join("\n"), message_with_stack.clone()),
    None => (message_with_stack.clone(), String::new()),
  };
  let (parsed_name, parsed_message) = split_error_message(&message_with_name);
  let name_override = exception_details
    .get("exception")
    .and_then(|e| e.get("preview"))
    .and_then(|p| p.get("properties"))
    .and_then(|v| v.as_array())
    .and_then(|props| {
      props
        .iter()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("name"))
    })
    .and_then(|p| {
      p.get("value")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
    });
  let name = name_override.unwrap_or(parsed_name);
  crate::web_error::ErrorDetails {
    name,
    message: parsed_message,
    stack,
  }
}

/// Source location for a CDP `Runtime.ExceptionDetails`. Mirrors
/// `crProtocolHelper.ts::stackTraceToLocation`: take the top
/// `stackTrace.callFrames` entry, falling back to `{ "", 0, 0 }`.
fn cdp_exception_to_location(exception_details: &serde_json::Value) -> crate::console_message::ConsoleMessageLocation {
  let frame = exception_details
    .get("stackTrace")
    .and_then(|s| s.get("callFrames"))
    .and_then(|f| f.as_array())
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
    None => crate::console_message::ConsoleMessageLocation::default(),
  }
}

/// Build the combined `description + stack` message for a CDP
/// `Runtime.ExceptionDetails` payload. Mirrors
/// `crProtocolHelper.ts::getExceptionMessage` byte-for-byte.
fn cdp_get_exception_message(exception_details: &serde_json::Value) -> String {
  use std::fmt::Write as _;
  if let Some(exception) = exception_details.get("exception") {
    if let Some(description) = exception.get("description").and_then(|v| v.as_str()) {
      return description.to_string();
    }
    if let Some(value) = exception.get("value") {
      return value_to_plain_string(value);
    }
  }
  let mut message = exception_details
    .get("text")
    .and_then(|v| v.as_str())
    .unwrap_or("")
    .to_string();
  if let Some(stack_trace) = exception_details.get("stackTrace")
    && let Some(frames) = stack_trace.get("callFrames").and_then(|v| v.as_array())
  {
    for frame in frames {
      let url = frame.get("url").and_then(|v| v.as_str()).unwrap_or("");
      let line = frame.get("lineNumber").and_then(serde_json::Value::as_u64).unwrap_or(0);
      let column = frame
        .get("columnNumber")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
      let function_name = frame.get("functionName").and_then(|v| v.as_str()).unwrap_or("");
      let function_name = if function_name.is_empty() {
        "<anonymous>"
      } else {
        function_name
      };
      // Write directly into the buffer to avoid an extra allocation;
      // writes into a String are infallible.
      let _ = write!(message, "\n    at {function_name} ({url}:{line}:{column})");
    }
  }
  message
}

/// JS `String(value)` equivalent for the handful of JSON-representable
/// remote-object `value` shapes that appear in `Runtime.ExceptionDetails.exception.value`.
fn value_to_plain_string(value: &serde_json::Value) -> String {
  match value {
    serde_json::Value::Null => "null".to_string(),
    serde_json::Value::Bool(b) => b.to_string(),
    serde_json::Value::Number(n) => n.to_string(),
    serde_json::Value::String(s) => s.clone(),
    other => other.to_string(),
  }
}

/// One context option's label and the command that applies it.
type ContextOptionCommand<'a> = (&'static str, futures::future::BoxFuture<'a, Result<()>>);

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
  /// Generation counter bumped on every `Page.frameStartedNavigating`. A
  /// post-input "settle" snapshots this before an action and waits for a
  /// commit only if it advanced — Playwright's `waitForSignalsCreatedBy`
  /// barrier, with zero cost when the action started no navigation.
  pub nav_started_seq: u64,
  /// Generation counter bumped on every `Page.frameNavigated` (commit). The
  /// settle waits for THIS to advance (not a loaderId change) so a reload —
  /// which re-commits the same loaderId — also satisfies the barrier.
  pub nav_committed_seq: u64,
  /// Generation counter bumped on every `Page.navigatedWithinDocument`
  /// (any frame), with the frame it named. A history step onto an entry
  /// of the same document commits no loader, so this is the only signal
  /// that it landed.
  pub same_document_seq: u64,
  pub same_document_frame: String,
  /// Lifecycle events fired for the current document.
  pub fired: u8,
  /// Set by the transport dispatcher when `Inspector.targetCrashed`
  /// fires for the page's session. Goto/reload/wait-for-navigation
  /// observe this and bail out with a typed error instead of stalling
  /// until the lifecycle timeout. Mirrors Playwright's
  /// `Page.openScope` cancellation in `raceNavigationAction`.
  pub crashed: bool,
}

/// Per-page temp directory Chrome writes downloads into. Shared by the
/// two `CdpPage` construction sites (attach-to-existing and `new_page`).
fn new_downloads_dir() -> Result<Arc<DownloadsDir>> {
  Ok(Arc::new(DownloadsDir {
    local: tempfile::Builder::new()
      .prefix("ferridriver-downloads-")
      .tempdir()
      .map_err(|e| FerriError::Backend(format!("downloads tempdir: {e}")))?,
    remote: std::sync::OnceLock::new(),
  }))
}

/// Where a browser's downloads are kept on this machine, and where the
/// browser writes them when that is somewhere else.
pub(crate) struct DownloadsDir {
  local: tempfile::TempDir,
  remote: std::sync::OnceLock<crate::download::RemoteDownloads>,
}

impl DownloadsDir {
  /// The directory on this machine a finished download is read from.
  pub(crate) fn path(&self) -> &std::path::Path {
    self.local.path()
  }

  /// The directory the browser is told to download into.
  fn browser_path(&self) -> std::borrow::Cow<'_, str> {
    match self.remote.get() {
      Some(remote) => std::borrow::Cow::Borrowed(&remote.directory),
      None => self.local.path().to_string_lossy(),
    }
  }
}

impl LifecycleState {
  fn new() -> Self {
    Self {
      current_loader_id: String::new(),
      nav_started_seq: 0,
      nav_committed_seq: 0,
      same_document_seq: 0,
      same_document_frame: String::new(),
      fired: 0,
      crashed: false,
    }
  }
}

/// What the Fetch listener answers authentication challenges with: the
/// site's credentials for a server's challenge, the proxy's for the
/// proxy's.
#[derive(Clone)]
struct Credentials {
  server: Arc<tokio::sync::RwLock<Option<Vec<crate::options::HttpCredentials>>>>,
  proxy: Arc<tokio::sync::RwLock<Option<crate::options::HttpCredentials>>>,
}

/// Page handle for the CDP backend. Cheaply cloneable (single `Arc` pointer).
#[derive(Clone)]
pub struct CdpPage(Arc<CdpPageInner>);

impl std::ops::Deref for CdpPage {
  type Target = CdpPageInner;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

#[doc(hidden)]
#[derive(Clone)]
pub struct CdpPageInner {
  pub shared: super::SharedPageState,
  transport: Arc<CdpTransport>,
  session_id: Option<Arc<str>>,
  /// The setup of a popup that attached paused; idle on every other page.
  setup: Arc<paused::PausedSetup>,
  pub(crate) target_id: Arc<str>,
  /// Browser context ID for isolated contexts (used for `Target.disposeBrowserContext` on close).
  browser_context_id: Option<Arc<str>>,
  /// Frame ID -> execution context ID mapping for frame-scoped evaluation.
  frame_contexts: Arc<std::sync::RwLock<FxHashMap<String, i64>>>,
  /// Frame ID -> the execution context of its utility world, once the
  /// browser has announced one. The engine is registered there as a
  /// new-document script, so the world exists for every document without
  /// being asked for; `fill` reads it here instead of asking.
  utility_contexts: Arc<std::sync::RwLock<FxHashMap<String, i64>>>,
  renderers: Arc<renderer::Renderers>,
  /// Notified by the wire observer after each map insert so
  /// [`CdpPage::resolve_frame_context`] wakes on the exact event instead
  /// of polling on a 10ms tick.
  frame_contexts_notify: Arc<tokio::sync::Notify>,
  /// Registered exposed binding callbacks (source-aware; plain exposed
  /// functions are wrapped by `AnyPage::expose_function`).
  pub exposed_fns: Arc<tokio::sync::RwLock<FxHashMap<String, crate::backend::PageBinding>>>,
  /// Guards one-time bootstrap of the `__fd_bc` binding channel. A
  /// `Mutex<bool>` (not an `AtomicBool`): concurrent
  /// `exposeFunction`/`exposeBinding`/`routeWebSocket` callers must SERIALIZE
  /// on the bootstrap — a second caller that merely observed "already
  /// initialized" via an atomic swap would race ahead and evaluate
  /// `__fd_bc.add(...)` before the first caller's controller script has
  /// defined `__fd_bc`, throwing "Uncaught". Holding the lock across the
  /// whole bootstrap makes late callers wait for it to finish.
  binding_initialized: Arc<tokio::sync::Mutex<bool>>,
  /// Registered route handlers for network interception.
  routes: Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
  fetch: Arc<renderer::FetchState>,
  /// HTTP credentials for Fetch.authRequired handling (digest/NTLM/basic).
  http_credentials: Arc<tokio::sync::RwLock<Option<Vec<crate::options::HttpCredentials>>>>,
  /// The context proxy's credentials, which answer only challenges whose
  /// `authChallenge.source` is `Proxy`.
  proxy_credentials: Arc<tokio::sync::RwLock<Option<crate::options::HttpCredentials>>>,
  /// Cached main frame ID to avoid repeated `Page.getFrameTree` calls.
  main_frame_id: Arc<tokio::sync::OnceCell<String>>,
  /// Most recently applied `Emulation.setDeviceMetricsOverride`
  /// params. Used by `emulate_viewport` to skip the redundant
  /// `setDeviceMetricsOverride` RTT that `apply_context_options`
  /// would otherwise pay re-applying the same size+orientation
  /// `enable_domains` already sent at page-init time. Cached as the
  /// exact JSON `Value` we'd ship — mirrors Playwright's
  /// `_metricsOverride` JSON-string equality check
  /// (`/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts:932`).
  /// Touch emulation lives in a separate command and runs through its
  /// own idempotent path so changing `has_touch` doesn't get masked
  /// by a metrics-only cache hit.
  last_metrics_params: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
  /// Last `Emulation.setEmulatedMedia` params shipped on this session;
  /// short-circuits redundant calls when context options match defaults.
  last_media_params: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
  /// Whether the browser draws real window chrome (see
  /// [`CdpBrowser::headful`]).
  headful: bool,
  flavor: CdpFlavor,
  /// The browser window this page's target lives in, resolved on first
  /// use by `Browser.getWindowForTarget`.
  ///
  /// Fetched lazily rather than at page init (where Playwright fetches
  /// it) so a page that never emulates a viewport pays no RTT for it.
  /// Chrome answers with an error for targets that have no real window
  /// — Edge's internal UI pages, and every target in a browser with no
  /// UI at all — so the cell simply stays empty and nothing is resized.
  window_id: Arc<tokio::sync::OnceCell<i64>>,
  /// The context's `screen`, if it was given one. Held because
  /// `window.screen` is reported by the SAME override that carries the
  /// viewport, so every later `setViewportSize` has to re-send it or
  /// the screen silently collapses back onto the viewport.
  context_screen: Arc<std::sync::Mutex<Option<crate::options::ScreenSize>>>,
  /// Frame tree captured during the per-page `enable_domains` parallel
  /// batch — `Page::with_context::seed_frame_cache` consumes this
  /// instead of paying a sequential `Page.getFrameTree` RTT once
  /// per new page. `None` for backends/paths that didn't pre-seed
  /// (e.g. attach-to-existing); callers fall back to a fresh fetch.
  seeded_frame_tree: Arc<std::sync::Mutex<Option<Vec<super::FrameInfo>>>>,
  /// Last known cursor position. Updated by every `mousePressed` /
  /// `mouseMoved` we ship; consulted by `click_at_with` to skip the
  /// pre-press `mouseMoved` when the cursor is already at the
  /// target (Playwright tracks this as `_lastPosition`). Saves one
  /// CDP RTT per click on tight loops where the cursor stays put
  /// between clicks (the common bench shape: click → assert → click
  /// the same button again).
  last_cursor_pos: Arc<std::sync::Mutex<Option<(f64, f64)>>>,
  /// Lifecycle state for current document — tracks loaderId + fired events.
  /// Updated synchronously by the transport reader task. Checked synchronously by `goto()`.
  lifecycle: Arc<std::sync::Mutex<LifecycleState>>,
  /// Notification sent when lifecycle state is updated.
  lifecycle_notify: Arc<tokio::sync::Notify>,
  /// Manager for lazy engine injection.
  injected_script: Arc<InjectedScriptManager>,
  /// Most recent main-document `Request` observed by the network
  /// listener. Cleared by `goto`/`reload`/`go_back`/`go_forward` before
  /// issuing the navigation so same-document navigations (no new
  /// request) resolve as "no response" — mirrors Playwright's
  /// `Response | null` contract on `page.goto`.
  nav_request_slot: crate::network::NavRequestSlot,
  /// Idempotency latch for `Page.setInterceptFileChooserDialog`.
  /// Mirrors Playwright's `_updateFileChooserInterception` lazy
  /// enabling: the CDP command fires once, the first time a user
  /// shows interest (`page.on('filechooser', ...)`,
  /// `wait_for_file_chooser`, etc.). With no listener registered the
  /// command is never sent — saves one RTT per page in workloads
  /// that don't use file pickers.
  pub file_chooser_intercept_enabled: Arc<std::sync::atomic::AtomicBool>,
  pub(crate) screencast_hub: crate::backend::screencast::ScreencastHub,
  /// The page's native recording; see [`recording`].
  pub(crate) screen_recorder: recording::ScreenRecorder,
  /// Idempotency latch for `Browser.setDownloadBehavior`. Lazy
  /// enabling: the CDP command fires once, the first time a user
  /// shows interest in downloads (`page.on('download', ...)`,
  /// `wait_for_download`, etc.). Saves one RTT per page on workloads
  /// that don't trigger downloads. Note: `apply_context_options`
  /// still fires the command when `accept_downloads` is explicitly
  /// set in the bag, so opt-in callers keep working without needing
  /// to register a listener first.
  pub download_behavior_enabled: Arc<std::sync::atomic::AtomicBool>,
  /// Whether this context accepts downloads at all
  /// (`BrowserContextOptions::accept_downloads`, default `true`).
  ///
  /// Read by [`CdpPage::enable_download_behavior`] rather than sent from
  /// `apply_context_options`, because there is only room for one
  /// `Browser.setDownloadBehavior` answer per context and two senders
  /// would race: the lazy latch used to overwrite an explicit `deny`
  /// with `allowAndName` the moment anything registered a download
  /// listener, which is why `acceptDownloads: false` did nothing.
  pub accept_downloads: Arc<std::sync::atomic::AtomicBool>,
  /// Per-page temp directory that Chrome is configured to write
  /// downloads into (via `Browser.setDownloadBehavior({ behavior:
  /// 'allowAndName', downloadPath, eventsEnabled: true })`). Held in
  /// an `Arc` so the directory lives as long as any [`crate::download::Download`]
  /// referencing a file under it — drop cleans up orphaned files
  /// (matches Playwright's per-context `_downloadsPath` cleanup on
  /// close).
  pub(crate) downloads_dir: Arc<DownloadsDir>,
  pub(crate) network_idle: Arc<crate::network_idle::NetworkIdle>,
  /// Abort handles for every per-page listener task
  /// (`attach_listeners`, the Fetch interceptor). The listeners
  /// subscribe to TRANSPORT event channels, which stay open for the
  /// browser's whole life — without an explicit abort in
  /// [`CdpPage::close_page`] each closed page leaves ~10 live tasks that
  /// wake on every subsequent CDP event and pin the page's state
  /// (emitter, managers, logs) until the browser exits.
  listener_tasks: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
  /// Native drag-and-drop interception state. See [`DragManagerState`].
  drag_manager: Arc<DragManagerState>,
  /// Cached latch for `Emulation.setTouchEmulationEnabled` so repeated `tap()`
  /// calls on the same page skip the redundant enable RTT.
  touch_emulation_enabled: Arc<std::sync::atomic::AtomicBool>,
}

impl std::ops::Deref for CdpPageInner {
  type Target = super::SharedPageState;

  fn deref(&self) -> &Self::Target {
    &self.shared
  }
}

/// Port of Playwright's `crDragDrop.ts::DragManager` plus the
/// pressed-button bookkeeping its `RawMouseImpl` receives from the
/// client-side `Mouse` class.
///
/// Chromium cannot drive a native drag through plain
/// `Input.dispatchMouseEvent`: a `mousePressed` + `mouseMoved` over a
/// `draggable` element starts a browser-side drag session that swallows
/// every subsequent synthetic mouse event, wedging pointer input for the
/// rest of the page's life. Playwright's answer — mirrored here — is to
/// intercept the drag (`Input.setInterceptDrags`), receive its payload
/// via the `Input.dragIntercepted` event, and re-dispatch the drag
/// itself with `Input.dispatchDragEvent` (`dragEnter` / `dragOver` /
/// `drop` / `dragCancel`).
/// One page-side call of an exposed binding, parsed in the tracker loop
/// and handed to the serial dispatch task in wire order.
struct BindingCall {
  fn_name: String,
  seq: u64,
  args: Vec<serde_json::Value>,
  /// Execution context the call came from; the result is delivered back
  /// into it, not the main frame's.
  ctx_id: Option<i64>,
  source: crate::events::BindingSource,
}

struct DragManagerState {
  /// `Input.DragData` captured from `Input.dragIntercepted` while a
  /// native drag is in flight; `None` when no drag is active. While
  /// `Some`, mouse moves become `dragOver`, mouse up becomes `drop`,
  /// and Escape becomes `dragCancel`.
  drag_data: tokio::sync::Mutex<Option<serde_json::Value>>,
  /// Currently-pressed buttons as a CDP `buttons` bitmask
  /// (left=1, right=2, middle=4). Updated by mouse down/up; moves
  /// consult it to decide drag interception and to stamp
  /// `button`/`buttons` on drag mousemoves so page handlers gating on
  /// `event.buttons` see the held state.
  buttons: std::sync::Mutex<u32>,
  /// Latest `Input.dragIntercepted` payload (`params.data`), written
  /// by the lossless tap task; consumed by the intercept flow.
  intercepted_slot: std::sync::Mutex<Option<serde_json::Value>>,
  /// Wakes the intercept flow when `intercepted_slot` fills.
  intercepted_notify: tokio::sync::Notify,
  /// One-time latch for the `Input.dragIntercepted` tap task.
  tap_started: std::sync::atomic::AtomicBool,
}

impl DragManagerState {
  fn new() -> Arc<Self> {
    Arc::new(Self {
      drag_data: tokio::sync::Mutex::new(None),
      buttons: std::sync::Mutex::new(0),
      intercepted_slot: std::sync::Mutex::new(None),
      intercepted_notify: tokio::sync::Notify::new(),
      tap_started: std::sync::atomic::AtomicBool::new(false),
    })
  }

  fn pressed_buttons(&self) -> u32 {
    self.buttons.lock().map_or(0, |g| *g)
  }

  fn update_buttons(&self, f: impl FnOnce(&mut u32)) -> u32 {
    let mut guard = match self.buttons.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    f(&mut guard);
    *guard
  }

  /// CDP `button` field for a mousemove: the primary held button, or
  /// `"none"` when nothing is pressed (mirrors Playwright's
  /// `_lastButton` threading into `RawMouse.move`).
  fn move_button(&self) -> &'static str {
    let mask = self.pressed_buttons();
    if mask & 1 != 0 {
      "left"
    } else if mask & 2 != 0 {
      "right"
    } else if mask & 4 != 0 {
      "middle"
    } else {
      "none"
    }
  }
}

/// `Input.dispatchKeyEvent` for a keydown. A key that types nothing is a
/// `rawKeyDown`; on macOS the editing commands go with it, because
/// Chromium there performs a shortcut such as select-all only when the
/// event names its command.
fn key_down_params(stroke: &KeyStroke, mac: bool) -> serde_json::Value {
  let d = &stroke.description;
  let commands = if mac {
    ferridriver_types::keyboard::mac_editing_commands(d.code, stroke.modifiers)
  } else {
    Vec::new()
  };
  serde_json::json!({
    "type": if d.text.is_empty() { "rawKeyDown" } else { "keyDown" },
    "modifiers": stroke.modifiers,
    "windowsVirtualKeyCode": d.key_code_without_location,
    "code": d.code,
    "commands": commands,
    "key": d.key,
    "text": d.text,
    "unmodifiedText": d.text,
    "autoRepeat": stroke.auto_repeat,
    "location": d.location,
    "isKeypad": d.is_keypad(),
  })
}

fn key_up_params(stroke: &KeyStroke) -> serde_json::Value {
  let d = &stroke.description;
  serde_json::json!({
    "type": "keyUp",
    "modifiers": stroke.modifiers,
    "key": d.key,
    "windowsVirtualKeyCode": d.key_code_without_location,
    "code": d.code,
    "location": d.location,
    "isKeypad": d.is_keypad(),
  })
}

fn cdp_button_bit(button: &str) -> u32 {
  match button {
    "left" => 1,
    "right" => 2,
    "middle" => 4,
    _ => 0,
  }
}

/// Page-side listener rig from `crDragDrop.ts::setupDragListeners`,
/// installed in every known execution context immediately before a
/// left-button-held mousemove is dispatched. After that move, awaiting
/// `window.__cleanupDrag()` answers "did this move start a native drag
/// that nobody `preventDefault()`ed?" — which decides whether an
/// `Input.dragIntercepted` payload is expected. Idempotent (`__fd`-style
/// main-world global; re-install while armed is a no-op) because the
/// same rig may be pushed to a frame twice when the main frame's
/// context id is also tracked in `frame_contexts`.
const DRAG_SETUP_JS: &str = "(function() {\
  if (window.__cleanupDrag) return;\
  let didStartDrag = Promise.resolve(false);\
  let dragEvent = null;\
  const dragListener = (event) => dragEvent = event;\
  const mouseListener = () => {\
    didStartDrag = new Promise((callback) => {\
      window.addEventListener('dragstart', dragListener, { once: true, capture: true });\
      setTimeout(() => callback(dragEvent ? !dragEvent.defaultPrevented : false), 0);\
    });\
  };\
  window.addEventListener('mousemove', mouseListener, { once: true, capture: true });\
  window.__cleanupDrag = async () => {\
    const val = await didStartDrag;\
    window.removeEventListener('mousemove', mouseListener, { capture: true });\
    window.removeEventListener('dragstart', dragListener, { capture: true });\
    delete window.__cleanupDrag;\
    return val;\
  };\
})()";

const DRAG_CLEANUP_JS: &str = "window.__cleanupDrag ? window.__cleanupDrag() : false";

/// The two `Page.addScriptToEvaluateOnNewDocument` registrations that put
/// the engine in a page: the main world, and the utility world `fill`
/// runs in.
///
/// Both carry the same source text, so V8 answers the utility world's
/// compile from the main world's: about 0.5ms per document, against the
/// 4.6ms a first `fill` paid to compile the engine again over
/// `Runtime.callFunctionOn`, which shares no cache. Only the main world
/// runs immediately. A document already loaded gets its utility engine
/// from `fill`'s own lazy injection, since running the script immediately
/// compiles it through the inspector at full cost whether or not the
/// document is ever filled.
///
/// `run_immediately` is for a page whose current document is to be used
/// as it is. A caller about to navigate leaves it off and lets the new
/// document take the engine through the cached path.
fn serialize_engine_registrations(source: &str, run_immediately: bool) -> [String; 2] {
  let escaped = serde_json::to_string(source).unwrap_or_else(|_| "\"\"".to_string());
  [
    format!(r#"{{"source":{escaped},"runImmediately":{run_immediately}}}"#),
    format!(r#"{{"source":{escaped},"worldName":"{UTILITY_WORLD}"}}"#),
  ]
}

/// Pre-serialized `Page.addScriptToEvaluateOnNewDocument` JSON payloads for the
/// main and utility worlds.
///
/// When `source` points to the default static bundle (`lazy_inject_js()`), returns
/// cached process-wide JSON strings without cloning or re-escaping the 113 KB
/// engine script on every page creation.
pub(super) fn engine_registrations_raw(source: &str, run_immediately: bool) -> [std::borrow::Cow<'static, str>; 2] {
  use std::borrow::Cow;
  use std::sync::OnceLock;

  if let Cow::Borrowed(static_src) = crate::selectors::lazy_inject_js()
    && std::ptr::eq(source, static_src)
  {
    static IMMEDIATE: OnceLock<[String; 2]> = OnceLock::new();
    static DEFERRED: OnceLock<[String; 2]> = OnceLock::new();
    let cache = if run_immediately { &IMMEDIATE } else { &DEFERRED };
    let pair = cache.get_or_init(|| serialize_engine_registrations(static_src, run_immediately));
    return [Cow::Borrowed(pair[0].as_str()), Cow::Borrowed(pair[1].as_str())];
  }
  let [main, utility] = serialize_engine_registrations(source, run_immediately);
  [Cow::Owned(main), Cow::Owned(utility)]
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

  // `reset()` was removed — init scripts registered via
  // `Page.addScriptToEvaluateOnNewDocument` persist for the page's
  // lifetime, so we never need to re-register on navigation /
  // context-cleared events.

  async fn ensure(&self, page: &CdpPage) -> Result<()> {
    if self.injected.load(std::sync::atomic::Ordering::Relaxed) {
      return Ok(());
    }
    // Register the selector engine via `Page.addScriptToEvaluateOnNewDocument`
    // with `runImmediately: true` so CDP injects `window.__fd` into:
    //   1. every future document (page navigations, new iframes), and
    //   2. all currently-loaded documents (main frame + already-attached
    //      same-origin iframes).
    // Without this, `Locator`s bound to an iframe `Frame` would
    // `evaluate_to_element(js, Some(iframe_id))` against an execution
    // context where `window.__fd` is undefined, and every action would
    // fail to resolve. Mirrors Playwright's
    // `/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts`
    // injection pattern.
    let src = crate::selectors::lazy_inject_js();
    let [main, utility] = engine_registrations_raw(&src, true);
    tokio::try_join!(
      Box::pin(page.cmd_raw("Page.addScriptToEvaluateOnNewDocument", &main)),
      Box::pin(page.cmd_raw("Page.addScriptToEvaluateOnNewDocument", &utility)),
    )?;
    self.injected.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
  }
}

impl CdpPage {
  async fn initialize_adopted_frame(&self) -> Result<()> {
    self.transport.register_lifecycle_tracker(
      self.session_id.as_deref().unwrap_or(""),
      Arc::clone(&self.lifecycle),
      Arc::clone(&self.lifecycle_notify),
      self.frame_observer(),
    );
    let tree = self.cmd("Page.getFrameTree", super::empty_params()).await?;
    let mut frames = Vec::new();
    if let Some(tree) = tree.get("frameTree") {
      collect_frames(tree, &mut frames);
    }
    {
      let mut cache = self
        .frame_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      for frame in &frames {
        cache.attach(frame.clone());
      }
    }
    *self
      .seeded_frame_tree
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(frames);
    if let Some(frame) = tree.pointer("/frameTree/frame") {
      if let Some(id) = frame.get("id").and_then(serde_json::Value::as_str) {
        let _ = self.main_frame_id.set(id.to_owned());
      }
      if let Some(loader) = frame.get("loaderId").and_then(serde_json::Value::as_str) {
        let mut lifecycle = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if lifecycle.current_loader_id.is_empty() {
          loader.clone_into(&mut lifecycle.current_loader_id);
        }
      }
    }
    self.initialize_existing_renderers().await
  }

  /// Send a strongly typed CDP command to this page's session without building
  /// an intermediate `serde_json::Value` on the fast path.
  pub(super) async fn send_typed<C: ferridriver_cdp_proto::Command>(&self, command: &C) -> Result<C::Response> {
    if self.setup.is_paused() {
      let params = serde_json::to_value(command)?;
      let raw = Box::pin(self.cmd_while_paused(C::METHOD, params)).await?;
      return serde_json::from_value(raw)
        .map_err(|e| FerriError::protocol(C::METHOD, format!("response decode failed: {e}")));
    }
    self.transport.send(self.session_id.as_deref(), command).await
  }

  /// Send a CDP command to this page's session.
  async fn cmd(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    if self.setup.is_paused() {
      return Box::pin(self.cmd_while_paused(method, params)).await;
    }
    self
      .transport
      .send_command(self.session_id.as_deref(), method, &params)
      .await
  }

  async fn cmd_raw(&self, method: &str, raw_params_json: &str) -> Result<serde_json::Value> {
    if self.setup.is_paused() {
      let params: serde_json::Value =
        serde_json::from_str(raw_params_json).map_err(|e| FerriError::Backend(format!("Parse: {e}")))?;
      return Box::pin(self.cmd_while_paused(method, params)).await;
    }
    self
      .transport
      .send_command_raw(self.session_id.as_deref(), method, raw_params_json)
      .await
  }

  /// A command whose answer the caller needs, on a popup still paused.
  /// No answer inside the grace means the popup answers nothing until
  /// resumed, so it is resumed here rather than left to stall its setup:
  /// what was written already still reaches its first document, what
  /// follows may not.
  async fn cmd_while_paused(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let answer = self.transport.send_command(self.session_id.as_deref(), method, &params);
    tokio::pin!(answer);
    if let Ok(result) = tokio::time::timeout(paused::ANSWER_GRACE, &mut answer).await {
      return result;
    }
    tracing::debug!("paused popup did not answer {method}; resuming it early");
    self.release_popup().await?;
    answer.await
  }

  /// Send a setup command nothing reads the answer of. A popup still
  /// paused gets it written and is not waited on.
  async fn configure(&self, method: &'static str, params: serde_json::Value) -> Result<()> {
    if !self.setup.is_paused() {
      return self.cmd(method, params).await.map(|_| ());
    }
    self.write_paused(method, params, |_| Ok(())).await
  }

  /// Write a command to a paused popup, `then` taking its answer once
  /// the popup is resumed.
  async fn write_paused(
    &self,
    method: &'static str,
    params: serde_json::Value,
    then: impl FnOnce(serde_json::Value) -> Result<()> + Send + 'static,
  ) -> Result<()> {
    let transport = Arc::clone(&self.transport);
    let session = self.session_id.clone();
    self
      .setup
      .write(Box::pin(async move {
        then(transport.send_command(session.as_deref(), method, &params).await?)
      }))
      .await
  }

  /// Whether this page is driven by Lightpanda's CDP server.
  #[must_use]
  pub(crate) fn is_lightpanda(&self) -> bool {
    self.flavor.is_lightpanda()
  }

  /// Primary CDP session id attached to this page target, if flattened.
  #[must_use]
  pub(crate) fn primary_session_id(&self) -> Option<&str> {
    self.session_id.as_deref()
  }

  /// Unpause a popup target parked by `Target.setAutoAttach`'s
  /// `waitForDebuggerOnStart`. Called by the popup pump once context
  /// configuration (init scripts, bindings, routes) has been applied,
  /// so the popup's first document already sees them — the same
  /// pause-configure-resume window Playwright's `CRPage` uses. The
  /// answers the setup left owed are collected here, behind the resume
  /// on the wire.
  pub(crate) async fn resume_popup(&self) -> Result<()> {
    let mut events = self.setup.is_paused().then(|| self.events.subscribe());
    self.release_popup().await?;
    if let Some(events) = events.as_mut() {
      self.first_document_committed(events).await;
    }
    Ok(())
  }

  /// End this page's session, and with it everything the session asked
  /// of the target: its auto-attach, its waits on the frames it attached.
  pub(crate) async fn detach_session(&self) -> Result<()> {
    if let Some(session) = self.session_id.as_deref() {
      self
        .transport
        .send(
          None,
          &ferridriver_cdp_proto::target::DetachFromTarget {
            session_id: Some(session),
            target_id: None,
          },
        )
        .await?;
    }
    self.dispose_local();
    Ok(())
  }

  /// Let a paused popup run, and collect the answers its setup left owed.
  async fn release_popup(&self) -> Result<()> {
    let owed = self.setup.resume();
    let resume = self.transport.send(
      self.session_id.as_deref(),
      &ferridriver_cdp_proto::runtime::RunIfWaitingForDebugger {},
    );
    let (answers, resumed) = tokio::join!(futures::future::join_all(owed), resume);
    for error in answers.into_iter().filter_map(Result::err) {
      tracing::debug!("popup setup: {error}");
    }
    resumed.map(|_| ())
  }

  /// A popup starts on an empty document that is not the one it was
  /// opened for. It is handed on once the first real one has committed,
  /// or it has closed, so that what is done to it next lands in that
  /// document (`crPage.ts` holds its initialization on the same commit).
  /// One whose frame tree already named a URL is past the empty document
  /// and has nothing left to commit.
  async fn first_document_committed(&self, events: &mut crate::events::EventSubscription<crate::events::PageEvent>) {
    loop {
      let committed = self.lifecycle_notify.notified();
      let (commits, crashed) = {
        let lifecycle = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (lifecycle.nav_committed_seq, lifecycle.crashed)
      };
      if commits > 0 || crashed || self.is_closed() || !self.setup.on_initial_document() {
        return;
      }
      tokio::select! {
        () = committed => {},
        event = events.recv() => {
          if matches!(event, None | Some(crate::events::PageEvent::Close)) {
            return;
          }
        },
      }
    }
  }
}

// ---- CdpElement ---------------------------------------------------------

pub struct CdpElement {
  transport: Arc<CdpTransport>,
  session_id: Option<Arc<str>>,
  handles: Arc<tokio::sync::Mutex<CdpElementHandles>>,
}

struct CdpElementHandles {
  node_id: Option<i64>,
  object_id: Option<Arc<str>>,
}

impl Clone for CdpElement {
  fn clone(&self) -> Self {
    Self {
      transport: Arc::clone(&self.transport),
      session_id: self.session_id.clone(),
      handles: Arc::clone(&self.handles),
    }
  }
}
