//! Event system for Page, Frame, and `BrowserContext`.
//!
//! Playwright-compatible event emitter. Supports `on()`, `once()`,
//! `waitForEvent()` patterns.
//!
//! Events flow from backend (CDP/BiDi/WebKit) -> [`Emitter`] -> listeners.
//! Delivery is lossless and ordered: `emit()` pushes onto an unbounded
//! queue consumed by a single dispatcher task per emitter, which invokes
//! matching listeners in registration order (Playwright's semantics) and
//! forwards to raw subscriptions. Listener callbacks must not block — they
//! run on the dispatcher task; anything slow belongs in a spawned task.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::{broadcast, mpsc};

#[cfg(test)]
mod tests;
mod types;

pub use types::{
  BindingReturn, BindingSource, BrowserEvent, BrowserEventCallback, BrowserEventEmitter, ContextEvent,
  ContextEventCallback, ContextEventEmitter, EventCallback, ExposedBinding, ExposedFn, ExposedFnFuture, ListenerId,
  PageEvent, event_name_matches,
};

/// Receive the next value from a broadcast receiver, surviving
/// `Lagged`. A lapped receiver loses the dropped events but keeps the
/// subscription alive — exiting the listener loop instead (the old
/// `while let Ok(..)` shape) silently disabled the listener for the
/// rest of the page's life after one event storm. Returns `None` once
/// the channel closes.
///
/// Used by backend transport consumers (CDP / `BiDi` / `WebKit`
/// protocol broadcast channels); the user-facing emitters in this
/// module are lossless and never lag.
pub async fn recv_tolerant<T: Clone>(rx: &mut broadcast::Receiver<T>) -> Option<T> {
  loop {
    match rx.recv().await {
      Ok(v) => return Some(v),
      Err(broadcast::error::RecvError::Lagged(n)) => {
        tracing::warn!(dropped = n, "broadcast listener lagged; dropped {n} event(s)");
      },
      Err(broadcast::error::RecvError::Closed) => return None,
    }
  }
}

// ── Emitter core ─────────────────────────────────────────────────────────────

/// Implemented by every event type an [`Emitter`] can dispatch. Maps
/// Playwright's lowercase event-name vocabulary onto the enum variants.
pub trait EmitterEvent: Clone + Send + 'static {
  /// Whether `event` is the variant named by `name`.
  fn matches_name(name: &str, event: &Self) -> bool;
}

struct ListenerSlot<E> {
  id: u64,
  registered: u64,
  name: String,
  once: bool,
  callback: Arc<dyn Fn(E) + Send + Sync>,
}

struct SubscriberSlot<E> {
  id: u64,
  registered: u64,
  tx: mpsc::UnboundedSender<StampedEvent<E>>,
}

/// Listener + subscription registries shared with the dispatcher task.
type StateObserver<E> = Arc<dyn Fn(&E) + Send + Sync>;

/// Inline non-blocking forwarder invoked on the emitter's dispatcher task for
/// each event with its original sequence stamp (used by the page→context
/// bridge to avoid a second task and channel per page).
type EventBridge<E> = Arc<dyn Fn(&E, u64) + Send + Sync>;

struct EmitterShared<E> {
  state_observer: std::sync::OnceLock<StateObserver<E>>,
  bridge: std::sync::OnceLock<EventBridge<E>>,
  listeners: std::sync::Mutex<Vec<ListenerSlot<E>>>,
  subscribers: std::sync::Mutex<Vec<SubscriberSlot<E>>>,
  /// Events a pending [`Emitter::wait_for_event`] is waiting on, by id.
  waiters: std::sync::Mutex<Vec<(u64, String)>>,
  /// Count of live consumers (`state_observer` + `bridge` + `listeners` +
  /// `subscribers`). Incremented (`SeqCst`) before a new consumer mints its
  /// `registered` sequence number and decremented only after removal, so
  /// `consumers == 0` lets `emit` / `emit_stamped` short-circuit without
  /// allocating, locking, or waking a dispatcher task.
  consumers: std::sync::atomic::AtomicUsize,
}

/// Unregisters a [`Emitter::wait_for_event`] waiter however its wait ends.
struct WaiterGuard<E> {
  shared: std::sync::Weak<EmitterShared<E>>,
  id: u64,
}

impl<E> Drop for WaiterGuard<E> {
  fn drop(&mut self) {
    if let Some(shared) = self.shared.upgrade() {
      lock_or_recover(&shared.waiters).retain(|(id, _)| *id != self.id);
    }
  }
}

#[derive(Clone)]
struct StampedEvent<E> {
  event: E,
  sequence: u64,
}

static EVENT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct EmitterInner<E> {
  queue_tx: mpsc::UnboundedSender<StampedEvent<E>>,
  shared: Arc<EmitterShared<E>>,
  /// `Some(rx)` until the dispatcher task is spawned; taken exactly once.
  pending_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<StampedEvent<E>>>>,
  dispatcher_running: AtomicBool,
  /// Runtime handle captured at construction so listener registration
  /// works from non-async contexts (NAPI).
  runtime_handle: std::sync::Mutex<Option<tokio::runtime::Handle>>,
  next_id: AtomicU64,
  /// Node's max-listeners threshold for the leak warning (its default
  /// is 10). `0` disables it.
  max_listeners: std::sync::atomic::AtomicUsize,
}

/// Node's default `EventEmitter` max-listeners threshold, which
/// Playwright inherits: past this many listeners on ONE event, the
/// emitter warns about a probable leak.
const DEFAULT_MAX_LISTENERS: usize = 10;

/// Lock a `std::sync::Mutex`, recovering from poisoning. Poisoning only
/// happens when a panicking thread held the lock; the registries stay
/// structurally valid, so continuing beats propagating the panic.
fn lock_or_recover<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn dispatch_loop<E: EmitterEvent>(
  shared: Arc<EmitterShared<E>>,
  mut rx: mpsc::UnboundedReceiver<StampedEvent<E>>,
) {
  let mut fired: Vec<Arc<dyn Fn(E) + Send + Sync>> = Vec::new();
  let mut batch: Vec<StampedEvent<E>> = Vec::new();
  // Batch drain: one task wakeup services everything queued, instead of
  // one wakeup per event — the dominant dispatcher cost under storm.
  while rx.recv_many(&mut batch, 128).await > 0 {
    for event in batch.drain(..) {
      dispatch_event(&shared, event, &mut fired);
    }
  }
}

fn dispatch_event<E: EmitterEvent>(
  shared: &EmitterShared<E>,
  stamped: StampedEvent<E>,
  fired: &mut Vec<Arc<dyn Fn(E) + Send + Sync>>,
) {
  let event = stamped.event;
  let sequence = stamped.sequence;
  if let Some(observer) = shared.state_observer.get() {
    observer(&event);
  }
  if let Some(bridge) = shared.bridge.get() {
    bridge(&event, sequence);
  }
  {
    let mut listeners = lock_or_recover(&shared.listeners);
    let before = listeners.len();
    // Collect matching callbacks in registration order and drop
    // `once` slots before invoking, so a recursive emit from inside
    // a callback can never re-fire them.
    listeners.retain(|slot| {
      if slot.registered < sequence && E::matches_name(&slot.name, &event) {
        fired.push(Arc::clone(&slot.callback));
        !slot.once
      } else {
        true
      }
    });
    let removed = before - listeners.len();
    if removed > 0 {
      shared.consumers.fetch_sub(removed, Ordering::SeqCst);
    }
  }
  // No lock held while invoking callbacks — they may reenter the
  // emitter (on/off/subscribe/emit).
  if lock_or_recover(&shared.subscribers).is_empty() {
    // No raw subscribers: the last listener takes the event by move.
    let last = fired.pop();
    for cb in fired.drain(..) {
      cb(event.clone());
    }
    if let Some(cb) = last {
      cb(event);
    }
    return;
  }
  for cb in fired.drain(..) {
    cb(event.clone());
  }
  let mut subscribers = lock_or_recover(&shared.subscribers);
  let before = subscribers.len();
  if before == 1 {
    if subscribers[0].registered < sequence && subscribers[0].tx.send(StampedEvent { event, sequence }).is_err() {
      subscribers.clear();
    }
  } else if before > 1 {
    // Move `event` into the last eligible subscriber instead of cloning for all N.
    let last_eligible = subscribers.iter().rposition(|slot| slot.registered < sequence);
    if let Some(last_idx) = last_eligible {
      let mut event = Some(event);
      let mut idx = 0usize;
      subscribers.retain(|slot| {
        let current = idx;
        idx += 1;
        if slot.registered >= sequence {
          return true;
        }
        let payload = if current == last_idx {
          match event.take() {
            Some(ev) => ev,
            None => return true,
          }
        } else {
          match event.as_ref() {
            Some(ev) => ev.clone(),
            None => return true,
          }
        };
        slot
          .tx
          .send(StampedEvent {
            event: payload,
            sequence,
          })
          .is_ok()
      });
    }
  }
  let removed = before - subscribers.len();
  if removed > 0 {
    shared.consumers.fetch_sub(removed, Ordering::SeqCst);
  }
}

/// Lossless, ordered event emitter. Cheap to clone (Arc'd internally).
///
/// One dispatcher task per emitter consumes an unbounded queue and
/// invokes matching listeners in registration order, then forwards to
/// raw [`EventSubscription`]s. Nothing is ever dropped; a subscriber
/// that stops draining accumulates queued events until it is dropped.
pub struct Emitter<E: EmitterEvent> {
  inner: Arc<EmitterInner<E>>,
}

impl<E: EmitterEvent> Clone for Emitter<E> {
  fn clone(&self) -> Self {
    Self {
      inner: Arc::clone(&self.inner),
    }
  }
}

impl<E: EmitterEvent> Default for Emitter<E> {
  fn default() -> Self {
    Self::new()
  }
}

impl<E: EmitterEvent> Emitter<E> {
  #[must_use]
  pub fn new() -> Self {
    let (queue_tx, queue_rx) = mpsc::unbounded_channel();
    let inner = Arc::new(EmitterInner {
      queue_tx,
      shared: Arc::new(EmitterShared {
        state_observer: std::sync::OnceLock::new(),
        bridge: std::sync::OnceLock::new(),
        listeners: std::sync::Mutex::new(Vec::new()),
        subscribers: std::sync::Mutex::new(Vec::new()),
        waiters: std::sync::Mutex::new(Vec::new()),
        consumers: std::sync::atomic::AtomicUsize::new(0),
      }),
      pending_rx: std::sync::Mutex::new(Some(queue_rx)),
      dispatcher_running: AtomicBool::new(false),
      runtime_handle: std::sync::Mutex::new(tokio::runtime::Handle::try_current().ok()),
      next_id: AtomicU64::new(1),
      max_listeners: std::sync::atomic::AtomicUsize::new(DEFAULT_MAX_LISTENERS),
    });
    // Dispatcher spawns lazily on the first consumer registration so quiet
    // emitters (unsubscribed contexts, browsers, websockets) cost zero tasks.
    Self { inner }
  }

  /// Spawn the dispatcher task if it isn't running yet. Queued events
  /// buffer in the channel until a runtime is available, so nothing is
  /// lost when the emitter is constructed outside a runtime.
  fn ensure_dispatcher(&self) {
    if self.inner.dispatcher_running.load(Ordering::Acquire) {
      return;
    }
    let mut pending = lock_or_recover(&self.inner.pending_rx);
    let Some(rx) = pending.take() else {
      return;
    };
    let shared = Arc::clone(&self.inner.shared);
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
      handle.spawn(dispatch_loop(shared, rx));
    } else {
      let stored = lock_or_recover(&self.inner.runtime_handle);
      if let Some(handle) = stored.as_ref() {
        handle.spawn(dispatch_loop(shared, rx));
      } else {
        // No runtime anywhere: dedicate a thread. Exits when every
        // emitter clone is dropped (queue sender closes).
        std::thread::spawn(move || {
          if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
            rt.block_on(dispatch_loop(shared, rx));
          }
        });
      }
    }
    self.inner.dispatcher_running.store(true, Ordering::Release);
  }

  /// Run `task` on the runtime this emitter dispatches on: the current
  /// one, else the one it was made on, so a caller outside any runtime
  /// (a binding's synchronous call on the JavaScript thread) still has it
  /// run.
  pub(crate) fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
      handle.spawn(task);
    } else if let Some(handle) = lock_or_recover(&self.inner.runtime_handle).as_ref() {
      handle.spawn(task);
    } else {
      tracing::debug!(target: "ferridriver::events", "no runtime to run an emitter task on");
    }
  }

  /// Emit an event. Never blocks and never drops: the event is queued
  /// for the dispatcher task (or discarded immediately when no consumers are
  /// attached).
  pub fn emit(&self, event: E) {
    if self.inner.shared.consumers.load(Ordering::SeqCst) == 0 {
      return;
    }
    self.emit_stamped(event, EVENT_SEQUENCE.fetch_add(1, Ordering::SeqCst));
  }

  pub(crate) fn emit_stamped(&self, event: E, sequence: u64) {
    if self.inner.shared.consumers.load(Ordering::SeqCst) == 0 {
      return;
    }
    self.ensure_dispatcher();
    let _ = self.inner.queue_tx.send(StampedEvent { event, sequence });
  }

  /// Whether at least one consumer (state observer, bridge, listener, or
  /// subscription) is attached. Used by upstream bridges to skip allocating
  /// forwarded event payloads when nobody is listening.
  #[must_use]
  pub(crate) fn has_consumers(&self) -> bool {
    self.inner.shared.consumers.load(Ordering::SeqCst) > 0
  }

  pub(crate) fn set_state_observer(&self, observer: StateObserver<E>) -> bool {
    if self.inner.shared.state_observer.set(observer).is_ok() {
      self.inner.shared.consumers.fetch_add(1, Ordering::SeqCst);
      self.ensure_dispatcher();
      true
    } else {
      false
    }
  }

  /// Attach an inline non-blocking forwarder that runs on this emitter's
  /// dispatcher task for every event with its original sequence stamp.
  pub(crate) fn set_bridge(&self, bridge: EventBridge<E>) -> bool {
    if self.inner.shared.bridge.set(bridge).is_ok() {
      self.inner.shared.consumers.fetch_add(1, Ordering::SeqCst);
      self.ensure_dispatcher();
      true
    } else {
      false
    }
  }

  /// Whether any named listener registered via [`Self::on`] or
  /// [`Self::once`], or a pending [`Self::wait_for_event`], is filtering
  /// for `event_name`, as Node's `waitForEvent` adds a listener of its
  /// own. Used by dialog / filechooser / download dispatch to decide
  /// auto-close behaviour when nobody is attached. Raw
  /// [`Self::subscribe`] subscribers don't count.
  #[must_use]
  pub fn has_listener(&self, event_name: &str) -> bool {
    lock_or_recover(&self.inner.shared.listeners)
      .iter()
      .any(|slot| slot.name == event_name)
      || lock_or_recover(&self.inner.shared.waiters)
        .iter()
        .any(|(_, name)| name == event_name)
  }

  /// Number of attached consumers: named listeners plus live raw
  /// subscriptions (zero = nobody is listening).
  #[must_use]
  pub fn receiver_count(&self) -> usize {
    lock_or_recover(&self.inner.shared.listeners).len() + lock_or_recover(&self.inner.shared.subscribers).len()
  }

  /// Subscribe to the raw ordered event stream. The subscription is
  /// lossless — events queue until received — and closes when the
  /// emitter is dropped. Dropping the subscription unregisters its
  /// slot immediately.
  #[must_use]
  pub fn subscribe(&self) -> EventSubscription<E> {
    self.inner.shared.consumers.fetch_add(1, Ordering::SeqCst);
    let registered = EVENT_SEQUENCE.fetch_add(1, Ordering::SeqCst);
    self.ensure_dispatcher();
    let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::unbounded_channel();
    lock_or_recover(&self.inner.shared.subscribers).push(SubscriberSlot { id, registered, tx });
    EventSubscription {
      rx,
      id,
      shared: Arc::downgrade(&self.inner.shared),
    }
  }

  /// Wait for an event matching a predicate, with timeout.
  ///
  /// The subscription happens synchronously inside this call — before
  /// the returned future is first polled — so an event emitted on the
  /// very next line cannot be missed.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the emitter is dropped.
  pub fn wait_for<F>(
    &self,
    predicate: F,
    timeout_ms: u64,
  ) -> impl Future<Output = crate::error::Result<E>> + Send + use<E, F>
  where
    F: Fn(&E) -> bool + Send,
  {
    let mut sub = self.subscribe();
    async move { sub.drain_until(predicate, timeout_ms).await }
  }

  /// Wait for the next event of a specific type, with timeout.
  /// Subscribes synchronously, like [`Self::wait_for`].
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the emitter is dropped.
  pub fn wait_for_event(
    &self,
    event_name: &str,
    timeout_ms: u64,
  ) -> impl Future<Output = crate::error::Result<E>> + Send + use<E> {
    let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
    lock_or_recover(&self.inner.shared.waiters).push((id, event_name.to_string()));
    let guard = WaiterGuard {
      shared: Arc::downgrade(&self.inner.shared),
      id,
    };
    let name = event_name.to_string();
    let wait = self.wait_for(move |e| E::matches_name(&name, e), timeout_ms);
    async move {
      let _registered = guard;
      wait.await
    }
  }

  /// Subscribe to events matching a name. The callback runs on the
  /// dispatcher task for each matching event, in registration order
  /// relative to other listeners. Returns a [`ListenerId`] for later
  /// removal with [`Self::off`].
  pub fn on(&self, event_name: &str, callback: Arc<dyn Fn(E) + Send + Sync>) -> ListenerId {
    self.register(event_name, callback, false)
  }

  /// Subscribe to a single event, then auto-remove the listener.
  pub fn once(&self, event_name: &str, callback: Arc<dyn Fn(E) + Send + Sync>) -> ListenerId {
    self.register(event_name, callback, true)
  }

  /// Node's `prependListener`: register at the FRONT, so this callback
  /// runs before every listener already attached to the same event.
  pub fn prepend(&self, event_name: &str, callback: Arc<dyn Fn(E) + Send + Sync>) -> ListenerId {
    self.register_at(event_name, callback, false, true)
  }

  /// Node's `prependOnceListener`.
  pub fn prepend_once(&self, event_name: &str, callback: Arc<dyn Fn(E) + Send + Sync>) -> ListenerId {
    self.register_at(event_name, callback, true, true)
  }

  fn register(&self, event_name: &str, callback: Arc<dyn Fn(E) + Send + Sync>, once: bool) -> ListenerId {
    self.register_at(event_name, callback, once, false)
  }

  fn register_at(
    &self,
    event_name: &str,
    callback: Arc<dyn Fn(E) + Send + Sync>,
    once: bool,
    front: bool,
  ) -> ListenerId {
    self.inner.shared.consumers.fetch_add(1, Ordering::SeqCst);
    let registered = EVENT_SEQUENCE.fetch_add(1, Ordering::SeqCst);
    self.ensure_dispatcher();
    let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
    let slot = ListenerSlot {
      id,
      registered,
      name: event_name.to_string(),
      once,
      callback,
    };
    let mut listeners = lock_or_recover(&self.inner.shared.listeners);
    if front {
      // Only relative to the SAME event: Node's ordering is per event
      // name, and dispatch walks the whole list filtering by name.
      let at = listeners
        .iter()
        .position(|slot| slot.name == event_name)
        .unwrap_or(listeners.len());
      listeners.insert(at, slot);
    } else {
      listeners.push(slot);
    }
    let count = listeners.iter().filter(|slot| slot.name == event_name).count();
    drop(listeners);
    self.warn_if_leaking(event_name, count);
    ListenerId(id)
  }

  /// Node's max-listeners warning: past the limit, an accumulating
  /// listener is far more often a leak than a design. Warn once per
  /// event name so a loop does not print thousands of lines.
  fn warn_if_leaking(&self, event_name: &str, count: usize) {
    let max = self.inner.max_listeners.load(Ordering::Relaxed);
    if max == 0 || count != max + 1 {
      return;
    }
    tracing::warn!(
      target: "ferridriver::events",
      event = event_name,
      count,
      max,
      "possible listener leak: more than the maximum listeners attached to one event; \
       raise it with setMaxListeners(n) if this is intended"
    );
  }

  /// Node's `setMaxListeners(n)`; `0` disables the warning entirely.
  pub fn set_max_listeners(&self, max: usize) {
    self.inner.max_listeners.store(max, Ordering::Relaxed);
  }

  /// Node's `getMaxListeners()`.
  #[must_use]
  pub fn max_listeners(&self) -> usize {
    self.inner.max_listeners.load(Ordering::Relaxed)
  }

  /// Node's `listenerCount(type)`.
  #[must_use]
  pub fn listener_count(&self, event_name: &str) -> usize {
    lock_or_recover(&self.inner.shared.listeners)
      .iter()
      .filter(|slot| slot.name == event_name)
      .count()
  }

  /// Node's `eventNames()`: every event with at least one listener, in
  /// registration order, each once.
  #[must_use]
  pub fn event_names(&self) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for slot in lock_or_recover(&self.inner.shared.listeners).iter() {
      if !names.iter().any(|name| name == &slot.name) {
        names.push(slot.name.clone());
      }
    }
    names
  }

  /// The ids registered for `event_name`, in dispatch order. A binding
  /// layer maps them back to the JS functions it persisted, which is
  /// what `listeners(type)` and identity removal need.
  #[must_use]
  pub fn listener_ids_named(&self, event_name: &str) -> Vec<ListenerId> {
    lock_or_recover(&self.inner.shared.listeners)
      .iter()
      .filter(|slot| slot.name == event_name)
      .map(|slot| ListenerId(slot.id))
      .collect()
  }

  /// Remove an event listener by ID.
  pub fn off(&self, id: ListenerId) {
    let mut listeners = lock_or_recover(&self.inner.shared.listeners);
    let before = listeners.len();
    listeners.retain(|slot| slot.id != id.0);
    let removed = before - listeners.len();
    if removed > 0 {
      self.inner.shared.consumers.fetch_sub(removed, Ordering::SeqCst);
    }
  }

  /// Remove all event listeners.
  pub fn remove_all_listeners(&self) {
    let mut listeners = lock_or_recover(&self.inner.shared.listeners);
    let removed = listeners.len();
    listeners.clear();
    if removed > 0 {
      self.inner.shared.consumers.fetch_sub(removed, Ordering::SeqCst);
    }
  }

  /// Remove every listener registered for `event_name`, leaving other
  /// events' listeners attached (Playwright's
  /// `removeAllListeners(type)` with a type argument).
  pub fn remove_listeners_named(&self, event_name: &str) {
    let mut listeners = lock_or_recover(&self.inner.shared.listeners);
    let before = listeners.len();
    listeners.retain(|slot| slot.name != event_name);
    let removed = before - listeners.len();
    if removed > 0 {
      self.inner.shared.consumers.fetch_sub(removed, Ordering::SeqCst);
    }
  }
}

/// Lossless ordered subscription to an [`Emitter`]'s event stream.
/// Returned by [`Emitter::subscribe`]; used by `waitForEvent`-style
/// waiters that need a synchronous subscription point. Unregisters
/// itself from the emitter on drop.
pub struct EventSubscription<E> {
  rx: mpsc::UnboundedReceiver<StampedEvent<E>>,
  id: u64,
  shared: std::sync::Weak<EmitterShared<E>>,
}

impl<E> Drop for EventSubscription<E> {
  fn drop(&mut self) {
    if let Some(shared) = self.shared.upgrade() {
      let mut subscribers = lock_or_recover(&shared.subscribers);
      let before = subscribers.len();
      subscribers.retain(|slot| slot.id != self.id);
      let removed = before - subscribers.len();
      if removed > 0 {
        shared.consumers.fetch_sub(removed, Ordering::SeqCst);
      }
    }
  }
}

impl<E: EmitterEvent> EventSubscription<E> {
  /// Receive the next event. Returns `None` once the emitter is
  /// dropped. Cancel-safe.
  pub async fn recv(&mut self) -> Option<E> {
    self.rx.recv().await.map(|stamped| stamped.event)
  }

  #[cfg(test)]
  pub(crate) async fn recv_stamped(&mut self) -> Option<(E, u64)> {
    self.rx.recv().await.map(|stamped| (stamped.event, stamped.sequence))
  }

  /// Drain this subscription until `predicate` matches an event, the
  /// emitter is dropped, or `timeout_ms` elapses.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the emitter is dropped.
  pub async fn drain_until<F>(&mut self, predicate: F, timeout_ms: u64) -> crate::error::Result<E>
  where
    F: Fn(&E) -> bool,
  {
    // Playwright's `timeout: 0` waits without a limit.
    let deadline = (timeout_ms > 0).then(|| tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms));
    loop {
      let remaining = match deadline {
        Some(deadline) => deadline.saturating_duration_since(tokio::time::Instant::now()),
        None => std::time::Duration::MAX,
      };
      if remaining.is_zero() {
        return Err(crate::error::FerriError::timeout("waiting for event", timeout_ms));
      }
      match tokio::time::timeout(remaining, self.recv()).await {
        Ok(Some(event)) if predicate(&event) => return Ok(event),
        Ok(Some(_)) => {},
        Ok(None) => {
          return Err(crate::error::FerriError::target_closed(Some(
            "event channel closed".into(),
          )));
        },
        Err(_) => return Err(crate::error::FerriError::timeout("waiting for event", timeout_ms)),
      }
    }
  }
}

/// Drain a pre-acquired subscription until `predicate` matches an
/// event, the emitter is dropped, or `timeout_ms` elapses.
///
/// Pairs with [`Emitter::subscribe`]: callers that need a synchronous
/// subscription point (so a downstream `.await` in JS can't race the
/// listener registration) call `subscribe()` first and then
/// `drain_until` inside the spawned future. Playwright implements
/// `helper.waitForEvent` with the same shape — listener registered
/// before the Promise is returned to JS, see
/// `/tmp/playwright/packages/playwright-core/src/server/helper.ts:58`.
///
/// # Errors
///
/// Returns an error if the timeout elapses or the emitter is dropped.
pub async fn drain_until<F>(
  rx: &mut EventSubscription<PageEvent>,
  predicate: F,
  timeout_ms: u64,
) -> crate::error::Result<PageEvent>
where
  F: Fn(&PageEvent) -> bool,
{
  rx.drain_until(predicate, timeout_ms).await
}

/// Broadcast-style page-event emitter. See [`Emitter`].
pub type EventEmitter = Emitter<PageEvent>;
