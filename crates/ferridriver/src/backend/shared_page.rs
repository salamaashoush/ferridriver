//! Backend-agnostic per-page state shared by every backend page handle.
//!
//! Every backend page (`CdpPage`, `BidiPage`, `WebKitPage`, `WebDriverPage`)
//! embeds a [`SharedPageState`] inside its `Arc`-wrapped inner struct, giving
//! [`super::AnyPage`] direct O(1) access to events, dialog/file-chooser/download
//! managers, frame cache, media emulation, observed buffers, keyboard state,
//! and closed flag without duplicating fields or match arms per backend.

use std::any::Any;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use tokio::sync::RwLock;

use crate::backend::keyboard::KeyboardState;
use crate::backend::{AnyElement, PageBackref, SharedMediaEmulation};
use crate::console_message::{ConsoleMessage, ConsoleMessageLocation};
use crate::dialog::{Dialog, DialogManager, DialogResponder, DialogType};
use crate::download::{Download, DownloadCanceler, DownloadManager};
use crate::element_handle::ElementHandle;
use crate::events::{EventEmitter, PageEvent};
use crate::file_chooser::{FileChooser, FileChooserManager};
use crate::frame_cache::FrameCache;
use crate::js_handle::{JSHandle, JSHandleBacking};
use crate::observed::ObservedBuffers;
use crate::web_error::{ErrorDetails, WebError};
use ferridriver_driver::event_sink::{FrameSignal, PageEventSink, PageLifecycleSignal};

/// Type-erased `Weak<crate::page::Page>` that breaks the recursive
/// `AnyPage -> SharedPageState -> {Dialog, Download, PageBackref, ...} -> Page -> AnyPage`
/// auto-trait proof cycle so `rustc` proves `Send + Sync` in O(1) depth.
#[derive(Clone, Default)]
pub(crate) struct ErasedWeakPage(Option<Weak<dyn Any + Send + Sync>>);

impl ErasedWeakPage {
  pub(crate) fn from_weak(weak: Weak<crate::page::Page>) -> Self {
    if weak.strong_count() == 0 {
      Self(None)
    } else {
      Self(Some(weak as Weak<dyn Any + Send + Sync>))
    }
  }

  pub(crate) fn from_arc(page: &Arc<crate::page::Page>) -> Self {
    Self(Some(Arc::downgrade(page) as Weak<dyn Any + Send + Sync>))
  }

  pub(crate) fn upgrade(&self) -> Option<Arc<crate::page::Page>> {
    self.0.as_ref()?.upgrade()?.downcast::<crate::page::Page>().ok()
  }

  pub(crate) fn strong_count(&self) -> usize {
    self.0.as_ref().map_or(0, Weak::strong_count)
  }

  pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
    match (&self.0, &other.0) {
      (Some(a), Some(b)) => a.ptr_eq(b),
      (None, None) => true,
      _ => false,
    }
  }
}

/// State common to every backend page implementation.
#[derive(Clone)]
pub struct SharedPageState {
  /// Event emitter for page events (console, dialog, network, frame lifecycle).
  pub events: EventEmitter,
  /// Per-page dialog handler registry.
  pub dialog_manager: DialogManager,
  /// Per-page file-chooser handler registry.
  pub file_chooser_manager: FileChooserManager,
  /// Per-page download handler registry.
  pub download_manager: DownloadManager,
  /// Weak back-reference to the outer [`crate::page::Page`] wrapper.
  pub page_backref: PageBackref,
  /// Shared frame cache; lives on the backend page so short-lived `Page` wrappers share it.
  pub(crate) frame_cache: Arc<std::sync::Mutex<FrameCache>>,
  /// Media emulation shared by every clone of the page.
  pub(crate) media_emulation: SharedMediaEmulation,
  /// Console / page-error retention for `page.consoleMessages()` / `page.pageErrors()`.
  pub(crate) observed: Arc<std::sync::Mutex<ObservedBuffers>>,
  /// Idempotent latch for spawning the frame-event listener once per backend page.
  pub(crate) frame_listener_started: Arc<AtomicBool>,
  /// Keyboard modifier/key state for this page.
  pub(crate) keyboard: Arc<KeyboardState>,
  /// Whether this page has been closed.
  pub(crate) closed: Arc<AtomicBool>,
}

impl SharedPageState {
  /// Create fresh per-page state seeded with `main_frame_id`.
  #[must_use]
  pub fn new(main_frame_id: &str) -> Self {
    Self::with_frame_cache(Arc::new(std::sync::Mutex::new(FrameCache::new(main_frame_id))))
  }

  /// Create fresh per-page state using an already-constructed `FrameCache`.
  #[must_use]
  pub(crate) fn with_frame_cache(frame_cache: Arc<std::sync::Mutex<FrameCache>>) -> Self {
    Self {
      events: EventEmitter::new(),
      dialog_manager: DialogManager::new(),
      file_chooser_manager: FileChooserManager::new(),
      download_manager: DownloadManager::new(),
      page_backref: PageBackref::new(),
      frame_cache,
      media_emulation: SharedMediaEmulation::default(),
      observed: Arc::default(),
      frame_listener_started: Arc::new(AtomicBool::new(false)),
      keyboard: Arc::default(),
      closed: Arc::new(AtomicBool::new(false)),
    }
  }

  /// Whether this page has been marked closed.
  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.closed.load(Ordering::Acquire)
  }

  /// Register the broadcast emitter bridges for dialog, file chooser, and
  /// download managers once per page.
  pub(crate) fn register_emitter_bridges(&self) {
    let _ = self.dialog_manager.register_emitter_bridge(self.events.clone());
    let _ = self.file_chooser_manager.register_emitter_bridge(self.events.clone());
    let _ = self.download_manager.register_emitter_bridge(self.events.clone());
  }
}

impl PageEventSink for SharedPageState {
  fn on_lifecycle(&self, signal: PageLifecycleSignal) {
    let ev = match signal {
      PageLifecycleSignal::DomContentLoaded => PageEvent::DomContentLoaded,
      PageLifecycleSignal::Load => PageEvent::Load,
      PageLifecycleSignal::Close => PageEvent::Close,
      PageLifecycleSignal::Crash => PageEvent::Crash,
    };
    self.events.emit(ev);
  }

  fn on_frame(&self, signal: FrameSignal) {
    let ev = match signal {
      FrameSignal::Attached(info) => PageEvent::FrameAttached(info),
      FrameSignal::Navigated(info) => PageEvent::FrameNavigated(info),
      FrameSignal::NavigatedWithinDocument(info) => PageEvent::FrameNavigatedWithinDocument(info),
      FrameSignal::Detached { frame_id } => PageEvent::FrameDetached { frame_id },
    };
    self.events.emit(ev);
  }
}

/// Parsed console event payload from any backend listener, ready to wrap
/// into a [`ConsoleMessage`], append to the capped log, and emit.
pub(crate) struct ConsoleEventInput {
  pub kind: String,
  pub explicit_text: Option<String>,
  pub arg_backings: Vec<(JSHandleBacking, bool)>,
  pub location: ConsoleMessageLocation,
  pub timestamp: u64,
}

/// Unified console-message dispatch shared by CDP, `BiDi`, and `WebKit` listeners:
/// upgrades the page backref, wraps remote object backings into `JSHandle`s,
/// appends to the capped console log, and emits `PageEvent::Console`.
pub(crate) async fn dispatch_console_event(
  page_backref: &PageBackref,
  console_log: &Arc<RwLock<Vec<ConsoleMessage>>>,
  emitter: &EventEmitter,
  input: ConsoleEventInput,
) {
  let Some(page) = page_backref.upgrade() else {
    return;
  };
  let args: Vec<JSHandle> = input
    .arg_backings
    .into_iter()
    .map(|(backing, is_node)| JSHandle::from_backing(Arc::clone(&page), backing, is_node))
    .collect();
  let msg = ConsoleMessage::new(
    &page,
    input.kind,
    input.explicit_text,
    args,
    input.location,
    input.timestamp,
  );
  crate::state::push_capped(
    &mut *console_log.write().await,
    msg.clone(),
    crate::state::CONSOLE_LOG_CAP,
  );
  emitter.emit(PageEvent::Console(msg));
}

/// Unified uncaught page-error dispatch shared by CDP, `BiDi`, and `WebKit` listeners.
pub(crate) fn dispatch_page_error_event(
  page_backref: &PageBackref,
  emitter: &EventEmitter,
  details: ErrorDetails,
  location: ConsoleMessageLocation,
) {
  let web_err = match page_backref.upgrade() {
    Some(page) => WebError::new(&page, details, location),
    None => WebError::new_detached(details, location),
  };
  emitter.emit(PageEvent::PageError(web_err));
}

/// Unified JavaScript dialog dispatch shared by CDP, `BiDi`, and `WebKit` listeners:
/// constructs the live `Dialog`, invokes `DialogManager::did_open` synchronously
/// (auto-closing if unclaimed), and appends to the capped dialog log.
pub(crate) async fn dispatch_dialog_event(
  page_backref: &PageBackref,
  dialog_manager: &DialogManager,
  dialog_log: &Arc<RwLock<Vec<crate::state::DialogEvent>>>,
  dialog_type_str: String,
  message: String,
  default_value: String,
  responder: DialogResponder,
) {
  let dialog_type = DialogType::parse(&dialog_type_str);
  let dialog = Dialog::new_with_manager(
    dialog_type,
    message.clone(),
    default_value,
    responder,
    Some(dialog_manager.clone()),
    page_backref.weak(),
  );
  dialog_manager.did_open(dialog);
  crate::state::push_capped(
    &mut *dialog_log.write().await,
    crate::state::DialogEvent {
      dialog_type: dialog_type_str,
      message,
      action: "dispatched".to_string(),
    },
    crate::state::DIALOG_LOG_CAP,
  );
}

/// Unified file-chooser dispatch shared by CDP, `BiDi`, and `WebKit` listeners.
pub(crate) async fn dispatch_file_chooser_event(
  page: Arc<crate::page::Page>,
  file_chooser_manager: &FileChooserManager,
  element: AnyElement,
  is_multiple: bool,
) {
  let Ok(handle) = ElementHandle::from_any_element(page, element).await else {
    return;
  };
  let chooser = FileChooser::new(handle, is_multiple);
  file_chooser_manager.did_open(&chooser);
}

/// Unified download-start dispatch shared by CDP and `BiDi` listeners.
pub(crate) fn dispatch_download_started(
  page_backref: &PageBackref,
  download_manager: &DownloadManager,
  guid: String,
  url: String,
  suggested_filename: String,
  downloads_dir: PathBuf,
  canceler: DownloadCanceler,
) {
  let Some(page) = page_backref.upgrade() else {
    return;
  };
  let download = Download::new(&page, guid, url, suggested_filename, downloads_dir, canceler);
  download_manager.did_open(&download);
}

#[cfg(test)]
mod tests {
  #[test]
  fn any_page_stays_pointer_sized() {
    assert!(
      std::mem::size_of::<super::super::AnyPage>() <= 24,
      "AnyPage grew to {} bytes; backend pages must stay Arc-wrapped",
      std::mem::size_of::<super::super::AnyPage>(),
    );
  }
}
