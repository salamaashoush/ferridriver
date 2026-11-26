//! Backend-agnostic per-page state shared by every backend page handle.
//!
//! Every backend page (`CdpPage`, `BidiPage`, `WebKitPage`, `WebDriverPage`)
//! embeds a [`SharedPageState`] inside its `Arc`-wrapped inner struct, giving
//! [`super::AnyPage`] direct O(1) access to events, dialog/file-chooser/download
//! managers, frame cache, media emulation, observed buffers, keyboard state,
//! and closed flag without duplicating fields or match arms per backend.

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use crate::backend::keyboard::KeyboardState;
use crate::backend::{PageBackref, SharedMediaEmulation};
use crate::dialog::DialogManager;
use crate::download::DownloadManager;
use crate::events::EventEmitter;
use crate::file_chooser::FileChooserManager;
use crate::frame_cache::FrameCache;
use crate::observed::ObservedBuffers;

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
