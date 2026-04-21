//! `FileChooser` — live handle for `<input type=file>` file pickers
//! intercepted via `Page.fileChooserOpened` (CDP) /
//! `input.fileDialogOpened` (`BiDi`).
//!
//! Mirrors Playwright's client-side `FileChooser` from
//! `/tmp/playwright/packages/playwright-core/src/client/fileChooser.ts`
//! and its server-side lifecycle from
//! `/tmp/playwright/packages/playwright-core/src/server/page.ts::_onFileChooserOpened`.
//!
//! Usage:
//!
//! ```ignore
//! page.on("filechooser", Arc::new(|event| {
//!     if let PageEvent::FileChooser(fc) = event {
//!         let _ = tokio::spawn(async move {
//!             let _ = fc.set_files(InputFiles::Paths(vec!["/tmp/a.txt".into()]), None).await;
//!         });
//!     }
//! }));
//!
//! let chooser = page.wait_for_file_chooser(5_000).await?;
//! chooser.set_files(InputFiles::Paths(vec!["/tmp/a.txt".into()]), None).await?;
//! ```
//!
//! Lifecycle rules (Playwright-faithful):
//!
//! * When the page triggers a file picker (`<input type=file>` click,
//!   `input.showPicker()`, etc.), the backend's file-chooser listener
//!   resolves the target `<input>` into an [`ElementHandle`], then
//!   synchronously calls [`FileChooserManager::did_open`] with a live
//!   [`FileChooser`].
//! * If any handler claims (returns `true`), the chooser is delivered
//!   and the caller is expected to call [`FileChooser::set_files`] (or
//!   drop the handle if they decide not to upload anything — native
//!   file picker has already been suppressed by
//!   `Page.setInterceptFileChooserDialog`).
//! * If no handler claims, the manager disposes the underlying
//!   [`ElementHandle`] so the browser doesn't hold the page-side
//!   reference indefinitely — matches Playwright's `handle.dispose();
//!   return;` branch in `server/page.ts::_onFileChooserOpened`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::element_handle::ElementHandle;
use crate::error::Result;
use crate::options::{InputFiles, SetInputFilesOptions};
use crate::page::Page;

/// Live file-chooser handle. Cheaply cloneable — every clone shares the
/// same underlying [`ElementHandle`]. Mirrors Playwright's
/// `FileChooser` client class.
#[derive(Clone)]
pub struct FileChooser {
  pub(crate) inner: Arc<FileChooserState>,
}

pub(crate) struct FileChooserState {
  element: ElementHandle,
  is_multiple: bool,
}

impl FileChooser {
  /// Construct a new file-chooser handle. Called by backend
  /// file-chooser listeners once the backend has resolved the target
  /// `<input>` into an [`ElementHandle`]; user code receives
  /// already-constructed `FileChooser`s via the page's
  /// [`FileChooserManager`] handler.
  #[must_use]
  pub fn new(element: ElementHandle, is_multiple: bool) -> Self {
    Self {
      inner: Arc::new(FileChooserState { element, is_multiple }),
    }
  }

  /// Element the chooser was triggered on. Playwright:
  /// `fileChooser.element(): ElementHandle`.
  #[must_use]
  pub fn element(&self) -> &ElementHandle {
    &self.inner.element
  }

  /// Whether the `<input>` accepts multiple files (`multiple` attribute
  /// set). Playwright: `fileChooser.isMultiple(): boolean`.
  #[must_use]
  pub fn is_multiple(&self) -> bool {
    self.inner.is_multiple
  }

  /// Owning page. Playwright: `fileChooser.page(): Page`. Derived from
  /// the element's own page, so a chooser always reports the page it
  /// lives on.
  #[must_use]
  pub fn page(&self) -> &Arc<Page> {
    self.inner.element.page()
  }

  /// Upload files through the captured `<input>`. Mirrors Playwright's
  /// `fileChooser.setFiles(files, options?)` — delegates to the
  /// element's [`ElementHandle::set_input_files`], which shares the
  /// §1.5 path/payload handling with [`crate::locator::Locator::set_input_files`].
  ///
  /// Accepts the full Playwright union of `string | string[] |
  /// FilePayload | FilePayload[]` via [`InputFiles`].
  ///
  /// # Errors
  ///
  /// Forwards the element's `set_input_files` error (missing file,
  /// backend protocol failure, disposed handle).
  pub async fn set_files(&self, files: InputFiles, opts: Option<SetInputFilesOptions>) -> Result<()> {
    self.inner.element.set_input_files(files, opts).await
  }
}

impl std::fmt::Debug for FileChooser {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FileChooser")
      .field("is_multiple", &self.inner.is_multiple)
      .field("element", &self.inner.element)
      .finish()
  }
}

// ── FileChooserManager ────────────────────────────────────────────────

/// Opaque id returned by [`FileChooserManager::add_handler`] and
/// consumed by [`FileChooserManager::remove_handler`]. Monotonically
/// increasing per manager — no collisions within the same page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileChooserHandlerId(pub u64);

/// Synchronous file-chooser ownership predicate. Mirrors the
/// server-side listener-count check in
/// `/tmp/playwright/packages/playwright-core/src/server/page.ts::_onFileChooserOpened`:
/// the handler is called with the freshly-built chooser and returns
/// `true` if it is going to handle it (the caller will eventually call
/// `set_files` or is intentionally dropping the handle), or `false` to
/// pass.
pub type FileChooserHandlerFn = Arc<dyn Fn(&FileChooser) -> bool + Send + Sync>;

struct FileChooserHandlerEntry {
  id: u64,
  handler: FileChooserHandlerFn,
}

/// Per-page file-chooser handler registry. Mirrors
/// [`crate::dialog::DialogManager`] shape verbatim — the two event
/// surfaces have the same "backend emits a one-shot event, at most one
/// handler claims it" semantics.
///
/// The backend's file-chooser listener resolves the target `<input>`
/// into an [`ElementHandle`] in its async task, then synchronously
/// calls [`Self::did_open`]. That method iterates every registered
/// handler in insertion order, calls each with the live chooser, and
/// checks if ANY returned `true`. If none did, the manager disposes
/// the underlying [`ElementHandle`] on a detached task — matches
/// Playwright's `handle.dispose(); return;` branch.
///
/// Handlers that return `true` are promising to drive `set_files`
/// themselves (or intentionally drop without uploading), so the
/// manager does nothing further with the element.
#[derive(Clone, Default)]
pub struct FileChooserManager {
  inner: Arc<FileChooserManagerState>,
}

#[derive(Default)]
struct FileChooserManagerState {
  handlers: std::sync::Mutex<Vec<FileChooserHandlerEntry>>,
  next_id: AtomicU64,
}

impl FileChooserManager {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Register a file-chooser handler. Returns a
  /// [`FileChooserHandlerId`] for later removal via
  /// [`Self::remove_handler`]. The id-based API avoids requiring
  /// callers to hold the same `Arc` instance to remove the handler,
  /// which matters for NAPI callbacks where the `Arc` crosses the JS
  /// boundary as opaque state.
  pub fn add_handler(&self, handler: FileChooserHandlerFn) -> FileChooserHandlerId {
    let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut handlers) = self.inner.handlers.lock() {
      handlers.push(FileChooserHandlerEntry { id, handler });
    }
    FileChooserHandlerId(id)
  }

  /// Remove a previously-registered file-chooser handler.
  pub fn remove_handler(&self, id: FileChooserHandlerId) {
    if let Ok(mut handlers) = self.inner.handlers.lock() {
      handlers.retain(|h| h.id != id.0);
    }
  }

  /// Called by the backend when a file chooser opens. Iterates every
  /// registered handler synchronously and asks each "do you claim
  /// this chooser?". If no handler returns `true`, spawns a task to
  /// dispose the underlying [`ElementHandle`] — matches Playwright's
  /// `handle.dispose(); return;` branch from
  /// `/tmp/playwright/packages/playwright-core/src/server/page.ts:317-320`.
  pub fn did_open(&self, chooser: &FileChooser) {
    let handlers: Vec<FileChooserHandlerFn> = match self.inner.handlers.lock() {
      Ok(g) => g.iter().map(|e| Arc::clone(&e.handler)).collect(),
      Err(_) => Vec::new(),
    };
    let mut claimed = false;
    for h in handlers {
      if h(chooser) {
        claimed = true;
      }
    }
    if !claimed {
      // Detach the dispose — the listener task stays non-blocking.
      let element = chooser.inner.element.clone();
      tokio::spawn(async move {
        let _ = element.dispose().await;
      });
    }
  }

  /// Register the default emitter-bridge handler: every page installs
  /// one at construction time so `page.events().on("filechooser", cb)`
  /// continues to deliver live [`FileChooser`] handles to broadcast
  /// listeners. The handler inspects the emitter's named-listener
  /// count via [`crate::events::EventEmitter::has_listener`]:
  ///
  /// * **When a named `filechooser` listener is registered**, the
  ///   handler emits [`crate::events::PageEvent::FileChooser`] on the
  ///   broadcast and returns `true`, claiming ownership synchronously.
  ///   The downstream listener task runs asynchronously and may
  ///   eventually call `chooser.set_files(...)`.
  /// * **When no named listener is present**, the handler returns
  ///   `false`. Any handler registered directly via
  ///   [`Self::add_handler`] (e.g. one-shot listener installed by
  ///   [`crate::page::Page::wait_for_file_chooser`]) may still claim
  ///   the chooser. If nobody claims, [`Self::did_open`] disposes the
  ///   element.
  #[must_use]
  pub fn register_emitter_bridge(&self, events: crate::events::EventEmitter) -> FileChooserHandlerId {
    self.add_handler(Arc::new(move |chooser: &FileChooser| {
      if events.has_listener("filechooser") {
        events.emit(crate::events::PageEvent::FileChooser(chooser.clone()));
        true
      } else {
        false
      }
    }))
  }
}
