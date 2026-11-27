//! `WebKit` page engine injection latch, context-liveness watch, and navigation lifecycle signals.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::backend::NavLifecycle;

/// The page's current document: whether `window.__fd` is in it, and
/// which execution contexts went with the documents before it.
///
/// One word holds the document's generation and the engine flag, so an
/// injection that finishes after its document was replaced cannot mark
/// the new document as having the engine.
pub(crate) struct EngineLatch {
  state: std::sync::atomic::AtomicU64,
  /// The newest execution context the live target has announced.
  newest_context: std::sync::atomic::AtomicI64,
  /// The target's generation, and the newest context of the documents
  /// gone from it. A target numbers its contexts from one.
  gone: tokio::sync::watch::Sender<(u64, i64)>,
}

impl Default for EngineLatch {
  fn default() -> Self {
    Self {
      state: std::sync::atomic::AtomicU64::new(0),
      newest_context: std::sync::atomic::AtomicI64::new(0),
      gone: tokio::sync::watch::channel((0, 0)).0,
    }
  }
}

impl EngineLatch {
  /// The current document, when the engine is still to go into it.
  pub(super) fn pending(&self) -> Option<u64> {
    let state = self.state.load(Ordering::Acquire);
    (state & 1 == 0).then_some(state)
  }

  /// Record the injection made for `document`, unless a newer document
  /// has replaced it.
  pub(super) fn injected(&self, document: u64) -> bool {
    self
      .state
      .compare_exchange(document, document | 1, Ordering::AcqRel, Ordering::Acquire)
      .is_ok()
  }

  /// A new document, with nothing injected.
  pub(crate) fn reset(&self) {
    self
      .state
      .update(Ordering::AcqRel, Ordering::Acquire, |state| (state | 1) + 1);
  }

  pub(crate) fn context_created(&self, context: i64) {
    self.newest_context.fetch_max(context, Ordering::AcqRel);
  }

  /// The main frame committed a new document: every context announced
  /// so far went with the old one.
  pub(crate) fn document_replaced(&self) {
    self.reset();
    let through = self.newest_context.load(Ordering::Acquire);
    self.gone.send_modify(|gone| gone.1 = gone.1.max(through));
  }

  /// The page moved to another target.
  pub(super) fn target_replaced(&self) {
    self.reset();
    self.newest_context.store(0, Ordering::Release);
    self.gone.send_modify(|gone| *gone = (gone.0 + 1, 0));
  }

  /// Watch for the document `context` belongs to going away.
  pub(super) fn watch(&self, context: i64) -> ContextWatch {
    let gone = self.gone.subscribe();
    let target = gone.borrow().0;
    ContextWatch { gone, target, context }
  }
}

pub(super) struct ContextWatch {
  gone: tokio::sync::watch::Receiver<(u64, i64)>,
  target: u64,
  context: i64,
}

impl ContextWatch {
  pub(super) fn is_gone(&self) -> bool {
    let (target, through) = *self.gone.borrow();
    target != self.target || self.context <= through
  }

  /// Whether the page is still on the target the context was on.
  pub(super) fn on_live_target(&self) -> bool {
    self.gone.borrow().0 == self.target
  }

  pub(super) async fn gone(&mut self) {
    while !self.is_gone() {
      if self.gone.changed().await.is_err() {
        std::future::pending::<()>().await;
      }
    }
  }
}

/// A `Playwright.provisionalLoadFailed`, with the loader of the
/// navigation that failed: starting a navigation cancels the one still in
/// flight, and that cancellation is not the new navigation's failure.
#[derive(Clone)]
pub(crate) struct ProvisionalFailure {
  pub loader_id: Option<String>,
  pub error_text: String,
}

/// Latch + notify for navigation lifecycle events fed by the
/// target-session listener.
#[derive(Default)]
pub(crate) struct LifecycleSignals {
  pub commit: AtomicBool,
  pub domcontentloaded: AtomicBool,
  pub load: AtomicBool,
  pub failed: AtomicBool,
  pub failure_text: std::sync::Mutex<Option<String>>,
  pub failed_request_id: std::sync::Mutex<Option<String>>,
  /// The request carrying the main frame's pending document, Playwright's
  /// `frame.pendingDocument().request`: only its failure fails the
  /// navigation. A navigation policy check unbinds it, so the old
  /// process's "Frame load interrupted" for a load that a cross-site
  /// redirect moved to a new process does not count.
  pub pending_request: std::sync::Mutex<Option<String>>,
  /// `Playwright.provisionalLoadFailed` error text. Main-frame only by
  /// protocol (it is a page-proxy-level event), so a pending `goto` can
  /// treat it as its own failure without a request-id match — the
  /// failing request lives on the provisional target session whose
  /// events never reach the committed-target listener.
  pub provisional_failure: std::sync::Mutex<Vec<ProvisionalFailure>>,
  /// The loader of the main-frame document committed since the last
  /// reset. `Page.loadEventFired` names no document, so a navigation
  /// only trusts `load` once its own loader has committed: the previous
  /// document's late load would otherwise finish it early.
  pub committed_loader: std::sync::Mutex<Option<String>>,
  /// The main frame navigated within its document since the last reset
  /// (`Page.navigatedWithinDocument`): a history step onto an entry of the
  /// same document, which commits nothing and fires no load.
  pub same_document: AtomicBool,
  pub notify: tokio::sync::Notify,
}

impl LifecycleSignals {
  pub(crate) fn reset(&self) {
    self.commit.store(false, Ordering::SeqCst);
    self.same_document.store(false, Ordering::SeqCst);
    self.domcontentloaded.store(false, Ordering::SeqCst);
    self.load.store(false, Ordering::SeqCst);
    self.failed.store(false, Ordering::SeqCst);
    *self
      .failure_text
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    *self
      .failed_request_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    *self
      .pending_request
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    self
      .provisional_failure
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clear();
    *self
      .committed_loader
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
  }

  /// A new main-frame document committed: what fired for the previous
  /// one no longer counts (Playwright's `frameCommittedNewDocumentNavigation`
  /// clears the frame's lifecycle events).
  pub(crate) fn commit_document(&self, loader_id: Option<String>) {
    self.domcontentloaded.store(false, Ordering::SeqCst);
    self.load.store(false, Ordering::SeqCst);
    *self
      .committed_loader
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = loader_id;
    self.mark(NavLifecycle::Commit);
  }

  /// Whether the document of the navigation under `loader_id` has
  /// committed; any commit counts for a waiter that does not know its loader.
  pub(crate) fn committed(&self, loader_id: Option<&str>) -> bool {
    let committed = self
      .committed_loader
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    match loader_id {
      Some(loader_id) => committed.as_deref() == Some(loader_id),
      None => true,
    }
  }

  /// Record a `Playwright.provisionalLoadFailed` for the pending
  /// main-frame navigation (mirrors `wkPage._onProvisionalLoadFailed`).
  pub(crate) fn mark_provisional_failed(&self, loader_id: Option<String>, error_text: String) {
    self
      .provisional_failure
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(ProvisionalFailure { loader_id, error_text });
    self.notify.notify_waiters();
  }

  /// The provisional-load failure of the navigation under `loader_id`, or
  /// of any navigation when the waiter does not know its loader. A failure
  /// that names no loader counts for every waiter.
  pub(crate) fn provisional_failure(&self, loader_id: Option<&str>) -> Option<String> {
    self
      .provisional_failure
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .iter()
      .find(|failure| loader_id.is_none() || failure.loader_id.is_none() || failure.loader_id.as_deref() == loader_id)
      .map(|failure| failure.error_text.clone())
  }

  pub(crate) fn mark_same_document(&self) {
    self.same_document.store(true, Ordering::SeqCst);
    self.notify.notify_waiters();
  }

  pub(crate) fn mark(&self, kind: NavLifecycle) {
    match kind {
      NavLifecycle::Commit => self.commit.store(true, Ordering::SeqCst),
      NavLifecycle::DomContentLoaded => self.domcontentloaded.store(true, Ordering::SeqCst),
      NavLifecycle::Load => self.load.store(true, Ordering::SeqCst),
    }
    self.notify.notify_waiters();
  }

  /// `request_id` now carries the main frame's pending document
  /// (Playwright's `frameManager.requestStarted`).
  pub(crate) fn bind_pending_request(&self, request_id: String) {
    *self
      .pending_request
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request_id);
  }

  /// The main frame is checking a navigation's policy, so the pending
  /// document has no request until the next one starts (Playwright's
  /// `frameManager.frameRequestedNavigation` with no document).
  pub(crate) fn unbind_pending_request(&self) {
    *self
      .pending_request
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
  }

  /// A request failed. Only the one carrying the pending document fails
  /// the navigation (Playwright's `frameManager.requestFailed`).
  pub(crate) fn request_failed(&self, request_id: &str, error_text: &str) {
    let pending = {
      let mut slot = self
        .pending_request
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      if slot.as_deref() == Some(request_id) {
        slot.take()
      } else {
        None
      }
    };
    if let Some(request_id) = pending {
      self.mark_failed(request_id, error_text.to_string());
    }
  }

  fn mark_failed(&self, request_id: String, error_text: String) {
    self.failed.store(true, Ordering::SeqCst);
    *self
      .failure_text
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error_text);
    *self
      .failed_request_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request_id);
    self.notify.notify_waiters();
  }

  pub(crate) fn seen(&self, kind: NavLifecycle) -> bool {
    match kind {
      NavLifecycle::Commit => self.commit.load(Ordering::SeqCst),
      NavLifecycle::DomContentLoaded => self.domcontentloaded.load(Ordering::SeqCst),
      NavLifecycle::Load => self.load.load(Ordering::SeqCst),
    }
  }

  pub(crate) fn failure(&self) -> Option<(String, String)> {
    if !self.failed.load(Ordering::SeqCst) {
      return None;
    }
    let req = self
      .failed_request_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    let txt = self
      .failure_text
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    Some((req.unwrap_or_default(), txt.unwrap_or_default()))
  }
}
