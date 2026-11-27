//! Page, context, and browser event enums and exposed-binding callback types.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::backend::FrameInfo;
use crate::console_message::ConsoleMessage;
use crate::dialog::Dialog;
use crate::download::Download;
use crate::file_chooser::FileChooser;
use crate::network::{Request, Response, WebSocket};
use crate::web_error::WebError;

use super::{Emitter, EmitterEvent};

/// Events emitted by pages. Mirrors Playwright's page event types.
#[derive(Debug, Clone)]
pub enum PageEvent {
  /// Console message from the page (`console.log`, `console.error`, etc.)
  /// Carries a live [`ConsoleMessage`] with `args` / `location` /
  /// `text` / `type` / `timestamp` / `page` accessors; matches
  /// Playwright's `page.on('console', (msg: ConsoleMessage) => ...)`.
  Console(ConsoleMessage),
  /// Network request started — Playwright `page.on('request')`.
  Request(Request),
  /// Network response received — Playwright `page.on('response')`.
  Response(Response),
  /// Network request finished (`loadingFinished`) —
  /// Playwright `page.on('requestfinished')`.
  RequestFinished(Request),
  /// Network request failed (`loadingFailed`) —
  /// Playwright `page.on('requestfailed')`.
  RequestFailed(Request),
  /// WebSocket opened — Playwright `page.on('websocket')`.
  WebSocket(WebSocket),
  /// Dialog appeared (alert, confirm, prompt, beforeunload). Carries
  /// a live [`Dialog`] handle — listeners are expected to call
  /// `dialog.accept(...)` or `dialog.dismiss()` exactly once. If no
  /// listener is registered when the dialog opens, the backend
  /// auto-closes it (accept for `beforeunload`, dismiss otherwise).
  Dialog(Dialog),
  /// The dialog closed: accepted, dismissed, auto-closed or closed by
  /// the browser. Playwright `page.on('dialogclosed')`.
  DialogClosed(Dialog),
  /// File chooser opened on the page — intercepted via CDP
  /// `Page.fileChooserOpened` / `BiDi` `input.fileDialogOpened`.
  /// Carries a live [`FileChooser`] whose `setFiles(...)` uploads via
  /// the captured `<input type=file>`. If no listener is registered,
  /// the backend disposes the underlying element handle (matches
  /// Playwright's `server/page.ts::_onFileChooserOpened` no-listener
  /// branch).
  FileChooser(FileChooser),
  /// Frame attached to the page.
  FrameAttached(FrameInfo),
  /// Frame detached from the page.
  FrameDetached { frame_id: String },
  /// Frame navigated to a new URL.
  FrameNavigated(FrameInfo),
  /// Same-document navigation (`history.pushState` / `replaceState` /
  /// fragment change): the document persists, only the URL changed.
  /// Surfaced to JS listeners under `framenavigated` like Playwright,
  /// but kept distinct internally so the frame cache updates the URL
  /// without detaching the frame's subtree.
  FrameNavigatedWithinDocument(FrameInfo),
  /// Page load event fired.
  Load,
  /// `DOMContentLoaded` event fired.
  DomContentLoaded,
  /// Page was closed.
  Close,
  /// The page's renderer crashed. `page.on('crash', (page: Page) => ...)`.
  /// The page stays open; whatever it was doing fails.
  Crash,
  /// Uncaught exception or unhandled rejection in page. Carries a live
  /// [`WebError`] with `{ name, message, stack }` matching JS `Error`
  /// shape plus a weak back-reference to the owning page. Observed via
  /// `page.on('pageerror', cb)` (page-scoped) and fanned out to
  /// `context.on('weberror', cb)` by the per-page → per-context
  /// bridge registered in `ContextRef::new_page`.
  PageError(WebError),
  /// Browser-initiated download. Carries a live
  /// [`crate::download::Download`] handle with `path` / `save_as` /
  /// `cancel` / `delete` / `failure` methods; mirrors Playwright's
  /// `page.on('download', (download: Download) => ...)`.
  Download(Download),
  /// This page opened another, as `window.open` or a `target=_blank`
  /// link does. Playwright's `page.on('popup', (page: Page) => ...)`,
  /// fired once the popup is set up, after the context's `page` event.
  Popup(Arc<crate::page::Page>),
  /// The page started a dedicated web worker. Playwright
  /// `page.on('worker')`.
  Worker(crate::worker::Worker),
}

impl PageEvent {
  /// Project the event into the compact JSON shape the binding layers
  /// (NAPI threadsafe-function listeners, `QuickJS` cross-task dispatch,
  /// `waitForEvent`) hand to user callbacks. Centralised here so every
  /// binding sees one shape — the `pageerror` consumer in NAPI still
  /// wraps the `{ name, message, stack }` object into a native JS
  /// `Error`, but the field set originates here.
  #[must_use]
  pub fn to_snapshot(&self) -> serde_json::Value {
    match self {
      PageEvent::Console(msg) => {
        let loc = msg.location();
        serde_json::json!({
          "type": msg.type_str(),
          "text": msg.text(),
          "location": {
            "url": loc.url,
            "lineNumber": loc.line_number,
            "columnNumber": loc.column_number,
          },
          "timestamp": msg.timestamp(),
          "argsCount": msg.args().len(),
        })
      },
      PageEvent::Response(r) => serde_json::json!({
        "url": r.url(),
        "status": r.status(),
        "statusText": r.status_text(),
        "ok": r.ok(),
        "fromServiceWorker": r.is_from_service_worker(),
        "headers": r.headers(),
      }),
      PageEvent::Request(r) | PageEvent::RequestFinished(r) | PageEvent::RequestFailed(r) => serde_json::json!({
        "url": r.url(),
        "method": r.method(),
        "resourceType": r.resource_type(),
        "isNavigationRequest": r.is_navigation_request(),
        "headers": r.headers(),
        "postData": r.post_data(),
      }),
      PageEvent::WebSocket(ws) => serde_json::json!({ "url": ws.url(), "isClosed": ws.is_closed() }),
      PageEvent::Dialog(d) | PageEvent::DialogClosed(d) => serde_json::json!({
        "type": d.dialog_type().as_str(),
        "message": d.message(),
        "defaultValue": d.default_value(),
      }),
      PageEvent::FileChooser(fc) => serde_json::json!({ "isMultiple": fc.is_multiple() }),
      PageEvent::FrameAttached(f) | PageEvent::FrameNavigated(f) | PageEvent::FrameNavigatedWithinDocument(f) => {
        serde_json::to_value(f).unwrap_or_default()
      },
      PageEvent::FrameDetached { frame_id } => serde_json::json!({ "frameId": frame_id }),
      PageEvent::Download(d) => serde_json::json!({
        "url": d.url(),
        "suggestedFilename": d.suggested_filename(),
      }),
      PageEvent::Popup(page) => serde_json::json!({ "url": page.url() }),
      PageEvent::Worker(worker) => serde_json::json!({ "url": worker.url() }),
      PageEvent::Load => serde_json::json!({ "type": "load" }),
      PageEvent::DomContentLoaded => serde_json::json!({ "type": "domcontentloaded" }),
      PageEvent::Close => serde_json::json!({ "type": "close" }),
      PageEvent::Crash => serde_json::json!({ "type": "crash" }),
      PageEvent::PageError(err) => {
        let d = err.error();
        serde_json::json!({
          "name": d.name,
          "message": d.message,
          "stack": d.stack,
        })
      },
    }
  }
}

/// What an exposed callback hands back to its page-side caller.
/// Playwright sends the result as an evaluate argument
/// (`server/page.ts::PageBinding.dispatch`), so a `JSHandle` inside it
/// arrives as the live object it refers to.
#[derive(Debug, Clone)]
pub enum BindingReturn {
  /// A plain JSON value.
  Json(serde_json::Value),
  /// A value with rich types or handles in it, as an evaluate argument.
  Argument(crate::protocol::SerializedArgument),
}

impl From<serde_json::Value> for BindingReturn {
  fn from(value: serde_json::Value) -> Self {
    Self::Json(value)
  }
}

impl From<crate::protocol::SerializedArgument> for BindingReturn {
  fn from(argument: crate::protocol::SerializedArgument) -> Self {
    Self::Argument(argument)
  }
}

/// Future returned by an [`ExposedFn`] — resolves to the page-visible
/// result of the bound callback, or to the message of the error it threw,
/// which the page-side call rejects with.
pub type ExposedFnFuture = Pin<Box<dyn Future<Output = Result<BindingReturn, String>> + Send>>;

/// Callback type for exposed functions (`page.exposeFunction`).
///
/// Receives the page-side call arguments as a `Vec` (the binding
/// layers SPREAD them into the user JS callback — `window.fn(a, b)` ->
/// `callback(a, b)` — matching Playwright) and returns a future
/// resolving to the serialized JSON result. It is async (not a plain
/// `-> Value`)
/// because the binding layers (NAPI threadsafe-function, `QuickJS`
/// cross-task `async_with`) can only produce the callback's real
/// return value asynchronously — Playwright delivers that value (and
/// awaits a returned Promise) to the page-side caller, so the backend
/// dispatch must `await` this before resolving the page binding.
pub type ExposedFn = Arc<dyn Fn(Vec<serde_json::Value>) -> ExposedFnFuture + Send + Sync>;

/// The `source` Playwright passes as the first argument to an
/// `exposeBinding` callback: `BindingSource = { context, page, frame }`
/// (`packages/playwright-core/types/structs.d.ts`).
///
/// A backend names the caller by identifiers, which is all it has when
/// the call arrives. The page then attaches itself, and
/// [`Self::live_page`], [`Self::live_frame`] and [`Self::live_context`]
/// are the objects Playwright hands a callback: what a binding layer
/// passes on, and what a Rust callback acts through.
#[derive(Clone, Default)]
pub struct BindingSource {
  /// Composite session key of the context the call originated from.
  pub context: String,
  /// Stable page identifier (target id) of the calling page.
  pub page: String,
  /// Frame id of the calling frame (the main frame when the call
  /// comes from the top document).
  pub frame: String,
  /// The calling page, once the call has reached it.
  pub(crate) live: Option<Arc<crate::page::Page>>,
}

impl BindingSource {
  /// The page the call came from. `None` only for a source built by
  /// hand, or when the page went away before its call was dispatched.
  #[must_use]
  pub fn live_page(&self) -> Option<Arc<crate::page::Page>> {
    self.live.clone()
  }

  /// The frame the call came from: the main frame, or a child frame.
  #[must_use]
  pub fn live_frame(&self) -> Option<crate::frame::Frame> {
    self.live.as_ref().map(|page| page.frame_for_id(&self.frame))
  }

  /// The context of the page the call came from.
  #[must_use]
  pub fn live_context(&self) -> Option<crate::context::ContextRef> {
    self.live.as_ref().and_then(|page| page.context().cloned())
  }
}

impl std::fmt::Debug for BindingSource {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("BindingSource")
      .field("context", &self.context)
      .field("page", &self.page)
      .field("frame", &self.frame)
      .field("live", &self.live.is_some())
      .finish()
  }
}

/// Callback type for context/page bindings registered via
/// `exposeBinding`. Like [`ExposedFn`] but receives a [`BindingSource`]
/// as its first argument, matching Playwright's
/// `(source: BindingSource, ...args) => any`.
///
/// `exposeFunction` is `exposeBinding` minus the source argument: the
/// binding layers wrap a source-less callback by discarding the
/// [`BindingSource`] before invoking it.
pub type ExposedBinding = Arc<dyn Fn(BindingSource, Vec<serde_json::Value>) -> ExposedFnFuture + Send + Sync>;

/// Event listener callback type.
pub type EventCallback = Arc<dyn Fn(PageEvent) + Send + Sync>;

/// Handle for removing an event listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenerId(pub u64);

/// Whether `event` matches `name` under Playwright's lowercase
/// page-event vocabulary (`'request'`, `'response'`, `'pageerror'`, …).
#[must_use]
pub fn event_name_matches(name: &str, event: &PageEvent) -> bool {
  matches!(
    (name, event),
    ("console", PageEvent::Console(_))
      | ("request", PageEvent::Request(_))
      | ("response", PageEvent::Response(_))
      | ("requestfinished", PageEvent::RequestFinished(_))
      | ("requestfailed", PageEvent::RequestFailed(_))
      | ("websocket", PageEvent::WebSocket(_))
      | ("dialog", PageEvent::Dialog(_))
      | ("dialogclosed", PageEvent::DialogClosed(_))
      | ("filechooser", PageEvent::FileChooser(_))
      | ("frameattached", PageEvent::FrameAttached(_))
      | ("framedetached", PageEvent::FrameDetached { .. })
      | (
        "framenavigated",
        PageEvent::FrameNavigated(_) | PageEvent::FrameNavigatedWithinDocument(_)
      )
      | ("load", PageEvent::Load)
      | ("domcontentloaded", PageEvent::DomContentLoaded)
      | ("close", PageEvent::Close)
      | ("crash", PageEvent::Crash)
      | ("pageerror", PageEvent::PageError(_))
      | ("download", PageEvent::Download(_))
      | ("popup", PageEvent::Popup(_))
      | ("worker", PageEvent::Worker(_))
  )
}

impl EmitterEvent for PageEvent {
  fn matches_name(name: &str, event: &Self) -> bool {
    event_name_matches(name, event)
  }
}

// ── Context-level event system ─────────────────────────────────────────

/// Events emitted by browser contexts: Playwright's
/// `BrowserContextEventMap`, less the Chromium-only `'serviceworker'`
/// and `'backgroundpage'`.
#[derive(Debug, Clone)]
pub enum ContextEvent {
  /// Unhandled error / rejection in any page in this context. Mirrors
  /// Playwright's `browserContext.on('weberror', (webError: WebError) => ...)`
  /// from `server/browserContext.ts:54`.
  WebError(crate::web_error::WebError),
  /// Browser-initiated download on any page in this context. Mirrors
  /// `browserContext.on('download', (download: Download) => ...)`.
  Download(Download),
  /// A frame attached on a page in this context. Carries the owning
  /// page so the binding can mint a live `Frame` for `frame_id`.
  /// Mirrors `browserContext.on('frameattached', (frame: Frame) => ...)`.
  FrameAttached {
    page: Arc<crate::page::Page>,
    frame_id: String,
  },
  /// A frame detached. `browserContext.on('framedetached', ...)`.
  FrameDetached {
    page: Arc<crate::page::Page>,
    frame_id: String,
  },
  /// A frame navigated. `browserContext.on('framenavigated', ...)`.
  FrameNavigated {
    page: Arc<crate::page::Page>,
    frame_id: String,
  },
  /// A new page was created in this context. Mirrors Playwright's
  /// `browserContext.on('page', (page: Page) => ...)` — fired from
  /// `ContextRef::new_page` once the public wrapper exists, and by the
  /// popup pump for the pages the browser opens.
  Page(Arc<crate::page::Page>),
  /// A page in this context was closed.
  /// `browserContext.on('pageclose', (page: Page) => ...)`.
  PageClose(Arc<crate::page::Page>),
  /// A page in this context fired `load`.
  /// `browserContext.on('pageload', (page: Page) => ...)`.
  PageLoad(Arc<crate::page::Page>),
  /// A dialog opened on a page in this context.
  /// `browserContext.on('dialog', (dialog: Dialog) => ...)`.
  Dialog(Dialog),
  /// A dialog on a page in this context closed.
  /// `browserContext.on('dialogclosed', (dialog: Dialog) => ...)`.
  DialogClosed(Dialog),
  /// A console message on a page in this context.
  /// `browserContext.on('console', (message: ConsoleMessage) => ...)`.
  Console(ConsoleMessage),
  /// A page in this context issued a request.
  /// `browserContext.on('request', (request: Request) => ...)`.
  Request {
    page: Arc<crate::page::Page>,
    request: Request,
  },
  /// A response arrived for a page in this context.
  /// `browserContext.on('response', (response: Response) => ...)`.
  Response {
    page: Arc<crate::page::Page>,
    response: Response,
  },
  /// `browserContext.on('requestfinished', (request: Request) => ...)`.
  RequestFinished {
    page: Arc<crate::page::Page>,
    request: Request,
  },
  /// `browserContext.on('requestfailed', (request: Request) => ...)`.
  RequestFailed {
    page: Arc<crate::page::Page>,
    request: Request,
  },
  /// The context closed, by `close()` or with its browser.
  /// `browserContext.on('close', (context: BrowserContext) => ...)`.
  Close(crate::context::ContextRef),
}

impl EmitterEvent for ContextEvent {
  fn matches_name(name: &str, event: &Self) -> bool {
    matches!(
      (name, event),
      ("weberror", ContextEvent::WebError(_))
        | ("download", ContextEvent::Download(_))
        | ("frameattached", ContextEvent::FrameAttached { .. })
        | ("framedetached", ContextEvent::FrameDetached { .. })
        | ("framenavigated", ContextEvent::FrameNavigated { .. })
        | ("page", ContextEvent::Page(_))
        | ("pageclose", ContextEvent::PageClose(_))
        | ("pageload", ContextEvent::PageLoad(_))
        | ("dialog", ContextEvent::Dialog(_))
        | ("dialogclosed", ContextEvent::DialogClosed(_))
        | ("console", ContextEvent::Console(_))
        | ("request", ContextEvent::Request { .. })
        | ("response", ContextEvent::Response { .. })
        | ("requestfinished", ContextEvent::RequestFinished { .. })
        | ("requestfailed", ContextEvent::RequestFailed { .. })
        | ("close", ContextEvent::Close(_))
    )
  }
}

/// Callback type for context-level event listeners.
pub type ContextEventCallback = Arc<dyn Fn(ContextEvent) + Send + Sync>;

/// Context-event emitter. See [`Emitter`].
pub type ContextEventEmitter = Emitter<ContextEvent>;

// ── Browser-level event system ─────────────────────────────────────────

/// Events emitted by a `Browser`: Playwright's `BrowserEventMap`.
#[derive(Clone)]
pub enum BrowserEvent {
  /// A new browser context was created on this browser. Carries the
  /// live [`crate::context::ContextRef`]. Mirrors
  /// `browser.on('context', (context: BrowserContext) => ...)`.
  Context(crate::context::ContextRef),
  /// The connection to the browser ended: closed, crashed, or the
  /// remote end went away. `browser.on('disconnected', (browser: Browser) => ...)`.
  Disconnected(crate::browser::Browser),
}

impl EmitterEvent for BrowserEvent {
  fn matches_name(name: &str, event: &Self) -> bool {
    matches!(
      (name, event),
      ("context", BrowserEvent::Context(_)) | ("disconnected", BrowserEvent::Disconnected(_))
    )
  }
}

/// Callback type for browser-level event listeners.
pub type BrowserEventCallback = Arc<dyn Fn(BrowserEvent) + Send + Sync>;

/// Browser-event emitter. See [`Emitter`].
pub type BrowserEventEmitter = Emitter<BrowserEvent>;
