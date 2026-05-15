//! `Download` — live handle for browser-initiated downloads intercepted
//! via CDP `Browser.downloadWillBegin` + `Browser.downloadProgress` or
//! `BiDi` `browsingContext.downloadWillBegin` + `browsingContext.downloadEnd`.
//!
//! Mirrors Playwright's client-side `Download` from
//! `/tmp/playwright/packages/playwright-core/src/client/download.ts` and
//! its server-side lifecycle from
//! `/tmp/playwright/packages/playwright-core/src/server/download.ts` +
//! `/tmp/playwright/packages/playwright-core/src/server/artifact.ts`.
//!
//! Usage:
//!
//! ```ignore
//! page.on("download", Arc::new(|event| {
//!     if let PageEvent::Download(d) = event {
//!         tokio::spawn(async move {
//!             let _ = d.save_as(std::path::Path::new("/tmp/saved.bin")).await;
//!         });
//!     }
//! }));
//!
//! let download = page.wait_for_download(5_000).await?;
//! download.save_as(std::path::Path::new("/tmp/x.bin")).await?;
//! ```
//!
//! Lifecycle rules (Playwright-faithful):
//!
//! * When the browser starts writing a download, the backend's listener
//!   builds a [`Download`] and synchronously calls
//!   [`DownloadManager::did_open`] with it.
//! * If any handler claims (returns `true`), the download is delivered
//!   to user code. Terminal state (finished / failed / cancelled) flips
//!   on the shared [`tokio::sync::watch`] once the backend's progress
//!   event reports `completed` / `canceled`; [`Download::path`] and
//!   [`Download::failure`] await that state transition.
//! * If no handler claims, the download proceeds in the background
//!   (Playwright's server emits the event but does not cancel automatically);
//!   the per-page temp-dir cleanup removes the file on page drop.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::watch;

use crate::error::{FerriError, Result};
use crate::page::Page;

/// Terminal state of a download. [`DownloadStatus::Pending`] is the
/// initial state; every other variant is terminal.
#[derive(Debug, Clone)]
pub enum DownloadStatus {
  /// Download is still writing bytes — `path`/`failure` block until the
  /// watch transitions.
  Pending,
  /// Download finished successfully; the file is at the given path.
  Finished { path: PathBuf },
  /// Download failed (canceled by caller, network error, disk error).
  Failed { error: String },
}

/// Backend-supplied async canceler. The backend builds this closure
/// when it constructs the [`Download`]; calling it issues the
/// protocol-specific cancel command (CDP `Browser.cancelDownload`, `BiDi`
/// has no cancel command so the `BiDi` canceler returns typed
/// [`FerriError::Unsupported`]).
pub type DownloadCanceler = Arc<
  dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = std::result::Result<(), FerriError>> + Send>>
    + Send
    + Sync,
>;

/// Live download handle. Cheaply cloneable — every clone shares the
/// same underlying state watch. Mirrors Playwright's `Download` client
/// class.
#[derive(Clone)]
pub struct Download {
  pub(crate) inner: Arc<DownloadState>,
}

pub(crate) struct DownloadState {
  /// Weak back-reference to the owning page. [`Download::page`] upgrades
  /// the weak; returns `None` if the outer page has been dropped.
  page: std::sync::Weak<Page>,
  /// Opaque download id assigned by the protocol (CDP's `guid`, `BiDi`'s
  /// `navigation` id). Used to correlate `downloadProgress` /
  /// `downloadEnd` events back to this handle.
  guid: String,
  /// Originating URL.
  url: String,
  /// Suggested filename reported by the protocol. Mutable because `BiDi`
  /// can report the suggested name separately from the start event
  /// (matches Playwright's `filenameSuggested`).
  suggested_filename: std::sync::Mutex<String>,
  /// Directory the browser is configured to write downloads into. The
  /// backend listener sets up a per-page temp dir and passes it here so
  /// `path()` can resolve the actual file.
  downloads_dir: PathBuf,
  /// Absolute path the browser is writing / wrote to. CDP downloads
  /// land at `downloads_dir/<guid>` when `behavior: 'allowAndName'` is
  /// set without an explicit filename; `BiDi` reports the absolute path
  /// in `downloadEnd.filepath` and the backend overrides
  /// `local_path` at `report_finished` time. A `Mutex` so both backends
  /// can update the path at completion.
  local_path: std::sync::Mutex<PathBuf>,
  /// Backend-supplied async cancel hook.
  canceler: DownloadCanceler,
  /// Watch channel of the terminal state. `watch::Sender::send` is
  /// noop-idempotent once a terminal state is set.
  state_tx: watch::Sender<DownloadStatus>,
  /// Marks `delete()` as already executed so repeated calls are
  /// idempotent. Matches Playwright's `_deleted` flag.
  deleted: AtomicBool,
}

impl Download {
  /// Construct a new download handle. Called by backend download
  /// listeners; user code receives already-constructed `Download`s via
  /// the page's [`DownloadManager`] handler.
  #[must_use]
  pub fn new(
    page: &Arc<Page>,
    guid: String,
    url: String,
    suggested_filename: String,
    downloads_dir: PathBuf,
    canceler: DownloadCanceler,
  ) -> Self {
    let local_path = downloads_dir.join(&guid);
    let (tx, _) = watch::channel(DownloadStatus::Pending);
    Self {
      inner: Arc::new(DownloadState {
        page: Arc::downgrade(page),
        guid,
        url,
        suggested_filename: std::sync::Mutex::new(suggested_filename),
        downloads_dir,
        local_path: std::sync::Mutex::new(local_path),
        canceler,
        state_tx: tx,
        deleted: AtomicBool::new(false),
      }),
    }
  }

  /// Originating URL. Playwright: `download.url(): string`.
  #[must_use]
  pub fn url(&self) -> &str {
    &self.inner.url
  }

  /// Opaque protocol-level download id. Used by the backend listener to
  /// correlate progress events back to the handle.
  #[must_use]
  pub fn guid(&self) -> &str {
    &self.inner.guid
  }

  /// Server-reported suggested filename. Playwright:
  /// `download.suggestedFilename(): string`.
  #[must_use]
  pub fn suggested_filename(&self) -> String {
    self
      .inner
      .suggested_filename
      .lock()
      .map(|g| g.clone())
      .unwrap_or_default()
  }

  /// Owning page (weak). Returns `None` if the page has been dropped.
  /// Playwright: `download.page(): Page` — the Playwright type is
  /// non-nullable because TS consumers never see a dead-page case; the
  /// Rust `Weak` upgrade returns `Option` so callers can observe
  /// target-closed without panicking.
  #[must_use]
  pub fn page(&self) -> Option<Arc<Page>> {
    self.inner.page.upgrade()
  }

  /// Backend hook: `BiDi` reports the suggested filename on the initial
  /// event; CDP reports it on the will-begin event. If a backend only
  /// learns the name later, it calls this to update the handle.
  pub fn filename_suggested(&self, suggested: String) {
    if let Ok(mut g) = self.inner.suggested_filename.lock() {
      *g = suggested;
    }
  }

  /// Backend hook: called by the listener when the protocol reports a
  /// progress `completed` / `canceled` state. `error` is `None` for a
  /// clean completion. Subsequent calls are no-ops (watch coalesces).
  ///
  /// `final_path` overrides the default `<downloads_dir>/<guid>` path
  /// when the backend knows the actual landing path (`BiDi` reports it on
  /// `downloadEnd.filepath`).
  ///
  /// Uses [`tokio::sync::watch::Sender::send_replace`] rather than
  /// `send` so the state update lands even when no receiver is
  /// currently subscribed — `send` silently discards the value when
  /// `receiver_count() == 0`, which would cause any later `path()` /
  /// `failure()` caller (who subscribes lazily) to hang on an
  /// already-resolved-but-discarded terminal transition. This is a
  /// real race: the backend's progress event can arrive before
  /// anything calls `path()` on a download dispatched via
  /// `page.on("download", ...)`.
  pub fn report_finished(&self, final_path: Option<PathBuf>, error: Option<String>) {
    if let Some(p) = final_path {
      if let Ok(mut g) = self.inner.local_path.lock() {
        *g = p;
      }
    }
    let path = self
      .inner
      .local_path
      .lock()
      .map_or_else(|_| self.inner.downloads_dir.clone(), |g| g.clone());
    let new_state = match error {
      None => DownloadStatus::Finished { path },
      Some(e) => DownloadStatus::Failed { error: e },
    };
    self.inner.state_tx.send_replace(new_state);
  }

  /// Block until the download reaches a terminal state.
  async fn wait_finished(&self) -> DownloadStatus {
    let mut rx = self.inner.state_tx.subscribe();
    loop {
      {
        let state = rx.borrow_and_update().clone();
        if !matches!(state, DownloadStatus::Pending) {
          return state;
        }
      }
      if rx.changed().await.is_err() {
        return rx.borrow().clone();
      }
    }
  }

  /// Local filesystem path the browser wrote to. Playwright:
  /// `download.path(): Promise<string>`. Blocks until the download
  /// finishes; surfaces the failure as [`FerriError::Backend`] if the
  /// download failed (mirrors Playwright's `throw this._failureErrorValue`).
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Backend`] when the download failed or was
  /// canceled.
  pub async fn path(&self) -> Result<PathBuf> {
    match self.wait_finished().await {
      DownloadStatus::Pending => Err(FerriError::Backend(
        "download watch closed before reaching terminal state".into(),
      )),
      DownloadStatus::Finished { path } => Ok(path),
      DownloadStatus::Failed { error } => Err(FerriError::Backend(error)),
    }
  }

  /// Download failure message, or `None` for a clean completion.
  /// Playwright: `download.failure(): Promise<string | null>`. Blocks
  /// until the download finishes.
  pub async fn failure(&self) -> Option<String> {
    match self.wait_finished().await {
      DownloadStatus::Failed { error } => Some(error),
      _ => None,
    }
  }

  /// Copy the downloaded file to `target`. Playwright:
  /// `download.saveAs(path): Promise<void>`. Blocks until the download
  /// finishes, then copies the bytes; creates missing parent
  /// directories to match Playwright's behaviour.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Backend`] if the download failed, or a
  /// filesystem error if the copy fails.
  pub async fn save_as(&self, target: &Path) -> Result<()> {
    let src = self.path().await?;
    if let Some(parent) = target.parent() {
      if !parent.as_os_str().is_empty() {
        tokio::fs::create_dir_all(parent).await?;
      }
    }
    tokio::fs::copy(&src, target).await?;
    Ok(())
  }

  /// Open a read stream over the downloaded file. Playwright:
  /// `download.createReadStream(): Promise<Readable>`. Returns a
  /// [`tokio::fs::File`] — use as an `AsyncRead` or pass to `BufReader`.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Backend`] if the download failed, or a
  /// filesystem error if opening the file fails.
  pub async fn create_read_stream(&self) -> Result<tokio::fs::File> {
    let path = self.path().await?;
    Ok(tokio::fs::File::open(path).await?)
  }

  /// Cancel a still-in-flight download. Playwright:
  /// `download.cancel(): Promise<void>`. Forwards to the backend's
  /// cancel hook; on backends without a native cancel (`BiDi`) returns
  /// typed [`FerriError::Unsupported`].
  ///
  /// # Errors
  ///
  /// See above.
  pub async fn cancel(&self) -> Result<()> {
    (self.inner.canceler)().await
  }

  /// Delete the downloaded file. Playwright:
  /// `download.delete(): Promise<void>`. Blocks until the download
  /// finishes, then unlinks. Idempotent — repeated calls are no-ops.
  ///
  /// # Errors
  ///
  /// Returns a filesystem error if the unlink fails for a reason other
  /// than "file does not exist".
  pub async fn delete(&self) -> Result<()> {
    if self.inner.deleted.swap(true, Ordering::AcqRel) {
      return Ok(());
    }
    // Wait for the download to finish so we unlink the file the browser
    // actually wrote (matches Playwright's `_delete` which awaits
    // `localPathAfterFinished`).
    let _ = self.wait_finished().await;
    let path = self
      .inner
      .local_path
      .lock()
      .map_or_else(|_| self.inner.downloads_dir.clone(), |g| g.clone());
    match tokio::fs::remove_file(&path).await {
      Ok(()) => Ok(()),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
      Err(e) => Err(FerriError::from(e)),
    }
  }
}

impl std::fmt::Debug for Download {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Download")
      .field("guid", &self.inner.guid)
      .field("url", &self.inner.url)
      .field("suggested_filename", &self.suggested_filename())
      .finish()
  }
}

// ── DownloadManager ────────────────────────────────────────────────────

/// Opaque id returned by [`DownloadManager::add_handler`] and consumed
/// by [`DownloadManager::remove_handler`]. Monotonically increasing per
/// manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DownloadHandlerId(pub u64);

/// Synchronous download ownership predicate. Mirrors the
/// [`crate::file_chooser::FileChooserHandlerFn`] shape: a handler is
/// called with the live download and returns `true` if it claims the
/// download (will drive `save_as` / `cancel` / `delete` eventually), or
/// `false` to pass.
pub type DownloadHandlerFn = Arc<dyn Fn(&Download) -> bool + Send + Sync>;

struct DownloadHandlerEntry {
  id: u64,
  handler: DownloadHandlerFn,
}

/// Per-page download handler registry. Mirrors
/// [`crate::file_chooser::FileChooserManager`] verbatim — same
/// "backend emits a one-shot event, at most one handler claims it"
/// semantics.
///
/// Unlike `FileChooser`, an unclaimed download is **not** auto-cancelled:
/// Playwright's server only emits `Page.Events.Download` and leaves the
/// bytes in `downloadsPath` for the caller (or for the per-context
/// cleanup on close). The per-page temp-dir drop handles eventual
/// orphans so tests don't leak files across runs.
#[derive(Clone, Default)]
pub struct DownloadManager {
  inner: Arc<DownloadManagerState>,
}

#[derive(Default)]
struct DownloadManagerState {
  handlers: std::sync::Mutex<Vec<DownloadHandlerEntry>>,
  next_id: AtomicU64,
  /// All downloads dispatched through this manager. The backend needs
  /// to look up the handle by `guid` when a `downloadProgress` event
  /// arrives; keeping them here (as weak-ish clones — the `Download`
  /// itself is cheap `Arc`-cloned) lets the listener call
  /// `report_finished` without threading a separate map through the
  /// spawn.
  ///
  /// Entries are removed by
  /// [`DownloadManager::take_for_guid`] — called by the listener on a
  /// terminal progress event.
  by_guid: std::sync::Mutex<Vec<Download>>,
}

impl DownloadManager {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Register a download handler. Returns a [`DownloadHandlerId`] for
  /// later removal via [`Self::remove_handler`].
  pub fn add_handler(&self, handler: DownloadHandlerFn) -> DownloadHandlerId {
    let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut handlers) = self.inner.handlers.lock() {
      handlers.push(DownloadHandlerEntry { id, handler });
    }
    DownloadHandlerId(id)
  }

  /// Remove a previously-registered download handler.
  pub fn remove_handler(&self, id: DownloadHandlerId) {
    if let Ok(mut handlers) = self.inner.handlers.lock() {
      handlers.retain(|h| h.id != id.0);
    }
  }

  /// Called by the backend when a download opens. Iterates every
  /// registered handler synchronously and asks each "do you claim this
  /// download?". Unlike `FileChooserManager`, the unclaimed branch is a
  /// no-op (Playwright does not auto-cancel).
  pub fn did_open(&self, download: &Download) {
    if let Ok(mut by_guid) = self.inner.by_guid.lock() {
      by_guid.push(download.clone());
    }
    let handlers: Vec<DownloadHandlerFn> = match self.inner.handlers.lock() {
      Ok(g) => g.iter().map(|e| Arc::clone(&e.handler)).collect(),
      Err(_) => Vec::new(),
    };
    for h in handlers {
      // Handlers may return `true`, but unlike FileChooser we don't
      // branch on "claimed" — Playwright's no-listener branch is a
      // no-op so the return value is purely informational. Discard it.
      let _ = h(download);
    }
  }

  /// Look up + remove a download by its protocol-level id. Called by
  /// the backend listener on a terminal progress event.
  #[must_use]
  pub fn take_for_guid(&self, guid: &str) -> Option<Download> {
    let mut guard = self.inner.by_guid.lock().ok()?;
    let idx = guard.iter().position(|d| d.guid() == guid)?;
    Some(guard.remove(idx))
  }

  /// Peek at a download without removing it. Used by backends that
  /// report `filenameSuggested` as a separate event before the final
  /// `downloadEnd`.
  #[must_use]
  pub fn peek_for_guid(&self, guid: &str) -> Option<Download> {
    let guard = self.inner.by_guid.lock().ok()?;
    guard.iter().find(|d| d.guid() == guid).cloned()
  }

  /// Register the default emitter-bridge handler: every page installs
  /// one at `attach_listeners` time so `page.events().on("download", cb)`
  /// delivers live [`Download`] handles on the broadcast. See
  /// [`crate::file_chooser::FileChooserManager::register_emitter_bridge`]
  /// for the underlying rationale.
  #[must_use]
  pub fn register_emitter_bridge(&self, events: crate::events::EventEmitter) -> DownloadHandlerId {
    self.add_handler(Arc::new(move |download: &Download| {
      if events.has_listener("download") {
        events.emit(crate::events::PageEvent::Download(download.clone()));
        true
      } else {
        false
      }
    }))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn noop_canceler() -> DownloadCanceler {
    Arc::new(|| Box::pin(async { Ok(()) }))
  }

  #[test]
  fn download_manager_add_remove_roundtrip() {
    let mgr = DownloadManager::new();
    let fired = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fired_c = fired.clone();
    let id = mgr.add_handler(Arc::new(move |_| {
      fired_c.fetch_add(1, Ordering::Relaxed);
      true
    }));

    // DownloadManager can't build a Download without a real Page —
    // emulate a manager-scoped dispatch by hand.
    let page = std::sync::Weak::<Page>::new();
    let (tx, _) = watch::channel(DownloadStatus::Pending);
    let d = Download {
      inner: Arc::new(DownloadState {
        page,
        guid: "abc".into(),
        url: "http://x/".into(),
        suggested_filename: std::sync::Mutex::new("f".into()),
        downloads_dir: PathBuf::from("/tmp"),
        local_path: std::sync::Mutex::new(PathBuf::from("/tmp/abc")),
        canceler: noop_canceler(),
        state_tx: tx,
        deleted: AtomicBool::new(false),
      }),
    };
    mgr.did_open(&d);
    assert_eq!(fired.load(Ordering::Relaxed), 1);

    mgr.remove_handler(id);
    mgr.did_open(&d);
    assert_eq!(fired.load(Ordering::Relaxed), 1);
  }

  #[tokio::test]
  async fn report_finished_resolves_path() {
    let page = std::sync::Weak::<Page>::new();
    let (tx, _) = watch::channel(DownloadStatus::Pending);
    let d = Download {
      inner: Arc::new(DownloadState {
        page,
        guid: "abc".into(),
        url: "http://x/".into(),
        suggested_filename: std::sync::Mutex::new("f".into()),
        downloads_dir: PathBuf::from("/tmp"),
        local_path: std::sync::Mutex::new(PathBuf::from("/tmp/abc")),
        canceler: noop_canceler(),
        state_tx: tx,
        deleted: AtomicBool::new(false),
      }),
    };
    let d2 = d.clone();
    let task = tokio::spawn(async move { d2.path().await });
    d.report_finished(Some(PathBuf::from("/tmp/final")), None);
    let p = task.await.unwrap().unwrap();
    assert_eq!(p, PathBuf::from("/tmp/final"));
  }

  #[tokio::test]
  async fn report_finished_with_error_surfaces_failure() {
    let page = std::sync::Weak::<Page>::new();
    let (tx, _) = watch::channel(DownloadStatus::Pending);
    let d = Download {
      inner: Arc::new(DownloadState {
        page,
        guid: "abc".into(),
        url: "http://x/".into(),
        suggested_filename: std::sync::Mutex::new("f".into()),
        downloads_dir: PathBuf::from("/tmp"),
        local_path: std::sync::Mutex::new(PathBuf::from("/tmp/abc")),
        canceler: noop_canceler(),
        state_tx: tx,
        deleted: AtomicBool::new(false),
      }),
    };
    let d2 = d.clone();
    let task = tokio::spawn(async move { d2.failure().await });
    d.report_finished(None, Some("canceled".into()));
    let f = task.await.unwrap();
    assert_eq!(f.as_deref(), Some("canceled"));
  }
}
