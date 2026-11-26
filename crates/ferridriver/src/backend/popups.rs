//! Popup lifecycle coordination shared across all browser backends:
//! lossless popup taps, in-flight popup tracking, listener barriers,
//! and the create/popup ownership ledger.

use std::sync::Arc;

use super::AnyPage;

/// A page the BROWSER created (a `window.open` / `target=_blank`
/// popup), announced by the backend's popup listener. The state-level
/// popup pump (see `context::spawn_popup_pump`) registers the page into
/// its owning context, applies context configuration, wires the opener,
/// resumes the target (CDP pauses popups via `waitForDebuggerOnStart`),
/// and emits `ContextEvent::Page`.
pub struct PopupInfo {
  /// Fully constructed backend page (domains enabled, engine injected)
  /// — NOT yet registered in the browser state and, on CDP, still
  /// paused waiting for the debugger.
  pub page: AnyPage,
  /// Backend context id the popup belongs to (`None` = default
  /// context). CDP `targetInfo.browserContextId`, `BiDi`
  /// `event.userContext`, `WebKit` `pageProxyInfo.browserContextId`.
  pub browser_context_id: Option<String>,
  /// Backend id of the page that opened this popup, when the protocol
  /// reports one — matched against `AnyPage::backend_target_id` to
  /// resolve `page.opener()`.
  pub opener_target_id: Option<String>,
  /// Holds the popup in its browser's [`PopupsInFlight`] until the pump
  /// is done with it.
  pub(crate) claim: Arc<PopupClaim>,
}

/// Registry of live popup subscriptions a backend browser pushes
/// [`PopupInfo`] into. Shared across browser clones.
pub(crate) type PopupTaps = Arc<std::sync::Mutex<Vec<tokio::sync::mpsc::UnboundedSender<PopupInfo>>>>;

pub(crate) fn push_popup(taps: &PopupTaps, info: PopupInfo) -> bool {
  let mut taps = taps.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
  taps.retain(|tap| !tap.is_closed());
  match taps.last() {
    Some(tap) => tap.send(info).is_ok(),
    None => false,
  }
}

/// The popups a browser has opened that the state layer has not finished
/// registering: counted from the moment the backend's listener reads the
/// event that announces one, until its pump lists and announces it or
/// gives up on it.
#[derive(Clone, Default)]
pub(crate) struct PopupsInFlight(Arc<InFlight>);

#[derive(Default)]
struct InFlight {
  count: std::sync::atomic::AtomicUsize,
  settled: tokio::sync::Notify,
}

impl PopupsInFlight {
  pub(crate) fn claim(&self) -> Arc<PopupClaim> {
    self.0.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Arc::new(PopupClaim(Arc::clone(&self.0)))
  }

  /// Resolves once no popup is in flight.
  pub(crate) async fn settled(&self) {
    loop {
      let settled = self.0.settled.notified();
      tokio::pin!(settled);
      settled.as_mut().enable();
      if self.0.count.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        return;
      }
      settled.await;
    }
  }
}

/// One popup counted in its browser's [`PopupsInFlight`]; dropping the
/// last handle to it takes it off the count.
pub(crate) struct PopupClaim(Arc<InFlight>);

impl Drop for PopupClaim {
  fn drop(&mut self) {
    if self.0.count.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
      self.0.settled.notify_waiters();
    }
  }
}

/// A way to wait until a backend's event listener has handled every event
/// it had been handed when the wait began. The listener answers a pass
/// only from a `biased` select that drains its events first.
#[derive(Clone)]
pub(crate) struct ListenerBarrier(tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>);

pub(crate) type BarrierPasses = tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<()>>;

impl ListenerBarrier {
  pub(crate) fn new() -> (Self, BarrierPasses) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (Self(tx), rx)
  }

  /// Resolves once the listener has handled everything queued before this
  /// call, or immediately when the listener has stopped.
  pub(crate) async fn pass(&self) {
    let (answer, answered) = tokio::sync::oneshot::channel();
    if self.0.send(answer).is_ok() && answered.await.is_err() {
      tracing::debug!("popup listener stopped before answering a barrier");
    }
  }
}

/// What a browser knows about the popups it is opening: how many are in
/// flight, and, where its listener reads them from an ordered stream, the
/// barrier that says every one announced so far has been counted.
#[derive(Clone, Default)]
pub(crate) struct PopupWatch {
  pub(crate) in_flight: PopupsInFlight,
  barrier: Option<ListenerBarrier>,
}

impl PopupWatch {
  /// A watch whose listener answers barrier passes from `BarrierPasses`.
  pub(crate) fn with_barrier() -> (Self, BarrierPasses) {
    let (barrier, passes) = ListenerBarrier::new();
    (
      Self {
        in_flight: PopupsInFlight::default(),
        barrier: Some(barrier),
      },
      passes,
    )
  }

  /// Resolves once the listener has counted every popup announced before
  /// this call and none of them is still in flight.
  pub(crate) async fn settled(&self) {
    if let Some(barrier) = &self.barrier {
      barrier.pass().await;
    }
    self.in_flight.settled().await;
  }
}

/// Ownership ledger between a backend's page-create flow (which owns
/// the targets it creates) and its browser-event listener (which must
/// claim every OTHER page as a popup — including `noopener` windows,
/// which carry no opener id and are otherwise indistinguishable from
/// our own creates because the creation event can arrive before the
/// create command's response returns the id).
///
/// Protocol: `begin_create` before sending the create command;
/// `end_create(Some(id))` the moment the response yields the id (or
/// `None` on error). A no-opener browser-created page claims
/// immediately when nothing is in flight, parks otherwise; the last
/// `end_create` flushes whatever no create took — every parked page is
/// deterministically resolved within one create round-trip, no timers.
pub(crate) struct CreateLedger<P> {
  pending: std::sync::atomic::AtomicUsize,
  owned: std::sync::Mutex<rustc_hash::FxHashSet<String>>,
  parked: std::sync::Mutex<rustc_hash::FxHashMap<String, P>>,
  flush_tx: tokio::sync::mpsc::UnboundedSender<()>,
  flush_rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<()>>>,
}

impl<P> Default for CreateLedger<P> {
  fn default() -> Self {
    let (flush_tx, flush_rx) = tokio::sync::mpsc::unbounded_channel();
    Self {
      pending: std::sync::atomic::AtomicUsize::new(0),
      owned: std::sync::Mutex::new(rustc_hash::FxHashSet::default()),
      parked: std::sync::Mutex::new(rustc_hash::FxHashMap::default()),
      flush_tx,
      flush_rx: std::sync::Mutex::new(Some(flush_rx)),
    }
  }
}

impl<P> CreateLedger<P> {
  pub(crate) fn begin_create(&self) {
    self.pending.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
  }

  pub(crate) fn end_create(&self, owned_id: Option<&str>) {
    {
      let mut owned = self.owned.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      if let Some(id) = owned_id {
        owned.insert(id.to_string());
        self
          .parked
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .remove(id);
      }
    }
    let remaining = self.pending.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) - 1;
    if remaining == 0 {
      let has_parked = !self
        .parked
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_empty();
      if has_parked {
        let _ = self.flush_tx.send(());
      }
    }
  }

  /// Resolve a no-opener browser-created page: `true` → claim it as a
  /// popup right now; `false` → either a completed create owns it (do
  /// nothing) or it was parked for the flush to decide.
  pub(crate) fn try_claim_ownerless(&self, id: &str, payload: P) -> bool {
    let owned = self.owned.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if owned.contains(id) {
      return false;
    }
    if self.pending.load(std::sync::atomic::Ordering::SeqCst) == 0 {
      return true;
    }
    self
      .parked
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(id.to_string(), payload);
    false
  }

  /// Drain every parked page no create claimed.
  pub(crate) fn take_unowned_parked(&self) -> Vec<P> {
    let owned = self.owned.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    self
      .parked
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .drain()
      .filter(|(id, _)| !owned.contains(id))
      .map(|(_, p)| p)
      .collect()
  }

  /// One-shot receiver for the flush signal (listener side). A second
  /// call returns a channel that never fires.
  pub(crate) fn take_flush_rx(&self) -> tokio::sync::mpsc::UnboundedReceiver<()> {
    self
      .flush_rx
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take()
      .unwrap_or_else(|| tokio::sync::mpsc::unbounded_channel().1)
  }
}
