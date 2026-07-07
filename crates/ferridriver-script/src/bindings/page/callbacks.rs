//! The session-shared callback registry (`PageCallbacks` userdata),
//! route/listener entries, and the bounded `page.on` event pump —
//! including the canonical VM re-entry discipline notes.

use std::sync::Arc;

use ferridriver::Page;

#[allow(clippy::wildcard_imports)]
use super::*;

/// Native registry for every page JS callback dispatched cross-task
/// (outside the QuickJS context, from a backend tokio task): `page.route`
/// handlers + URL predicates (keyed by registration id), `page.exposeFunction`
/// callbacks (keyed by binding name), and the single `page.startScreencast`
/// frame callback. All kept as `Persistent<Function>` in context
/// userdata — no `globalThis.__fd*`, exactly the `Persistent`/userdata
/// pattern the extension registry uses.
///
/// Single-threaded VM ⇒ `RefCell`, never `Arc`/`Mutex` (same rationale
/// as `BddUserData`).
#[derive(Default)]
pub(crate) struct PageCallbacks {
  /// `page.route` / `context.route` registrations keyed by id. The id
  /// comes from [`Self::next_route_id`] — registry-global, NOT
  /// per-wrapper: wrappers are minted freely (`locator.page()`,
  /// `frame.page()`, `page.context()`), so a per-wrapper counter would
  /// restart at 0 and silently overwrite another page's entry here.
  routes: rustc_hash::FxHashMap<u64, RouteEntry>,
  route_id_counter: u64,
  pub(crate) exposed: rustc_hash::FxHashMap<String, rquickjs::Persistent<rquickjs::Function<'static>>>,
  pub(crate) screencast: Option<rquickjs::Persistent<rquickjs::Function<'static>>>,
  /// `addLocatorHandler` JS callbacks, keyed by core-registry uid so the
  /// cross-task dispatch bridge can restore the persisted function.
  locator_handlers: rustc_hash::FxHashMap<u64, rquickjs::Persistent<rquickjs::Function<'static>>>,
  /// `page.on` / `page.once` JS listeners, keyed by the core
  /// `ListenerId` and carrying the event name (so `off(event, fn)` can
  /// match by JS function identity) plus the backend-page identity (so
  /// a page's registrations can be released when it closes — the
  /// session VM outlives pages, and orphaned `Persistent`s would
  /// otherwise accumulate for the VM's life). The event pump restores
  /// the persisted function by id and invokes it with the live event
  /// object.
  event_listeners: rustc_hash::FxHashMap<u64, EventListenerEntry>,
  /// `routeWebSocket` handlers + per-route `onMessage`/`onClose` JS
  /// callbacks, keyed by a registry-global id. Restored by id inside
  /// `async_with` from the cross-task WS dispatch (never moved across
  /// threads — same discipline as `routes`).
  ws_callbacks: rustc_hash::FxHashMap<u64, rquickjs::Persistent<rquickjs::Function<'static>>>,
  /// Owner of each `ws_callbacks` entry, so a closing page/context can
  /// release its persisted WS handlers — the session VM outlives both.
  ws_owners: rustc_hash::FxHashMap<u64, RouteOwner>,
  /// Names registered via `page.exposeFunction`, per backend page id.
  /// `page.exposeFunction` has no dispose in Playwright, so page close
  /// is the only hook that can release the persisted callbacks.
  exposed_by_page: rustc_hash::FxHashMap<usize, Vec<String>>,
}

/// Identity of the object a route was registered through, so
/// `unroute(fn)` / `unrouteAll` / close-time cleanup only touch that
/// owner's registrations and never a sibling page's or context's.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum RouteOwner {
  /// `ferridriver::Page::backend_page_id()` — stable across the many
  /// `PageJs` wrappers a session mints for the same page.
  Page(usize),
  /// Core context name — stable across `BrowserContextJs` wrappers
  /// (`page.context()` mints a fresh `Arc<ContextRef>` per call, so
  /// pointer identity is not usable).
  Context(String),
}

/// One `route(matcher, handler)` registration in [`PageCallbacks`].
pub(crate) struct RouteEntry {
  owner: RouteOwner,
  handler: rquickjs::Persistent<rquickjs::Function<'static>>,
  /// JS URL predicate, when the route was registered with a function
  /// instead of a string/RegExp matcher.
  pred: Option<rquickjs::Persistent<rquickjs::Function<'static>>>,
  /// The always-true core matcher registered for a predicate route. Its
  /// `Arc` identity is what core `unroute` compares, so it must be kept
  /// here (shared across wrappers) for `unroute(fn)` to work from any
  /// wrapper of the owner. `None` for plain matcher routes.
  matcher: Option<ferridriver::url_matcher::UrlMatcher>,
}

impl PageCallbacks {
  /// Next registry-global route id (single VM thread; plain counter).
  pub(crate) fn next_route_id(&mut self) -> u64 {
    let id = self.route_id_counter;
    self.route_id_counter += 1;
    id
  }

  pub(crate) fn insert_route(
    &mut self,
    id: u64,
    owner: RouteOwner,
    handler: rquickjs::Persistent<rquickjs::Function<'static>>,
    pred: Option<rquickjs::Persistent<rquickjs::Function<'static>>>,
    matcher: Option<ferridriver::url_matcher::UrlMatcher>,
  ) {
    self.routes.insert(
      id,
      RouteEntry {
        owner,
        handler,
        pred,
        matcher,
      },
    );
  }

  pub(crate) fn get_route_handler(&self, id: u64) -> Option<rquickjs::Persistent<rquickjs::Function<'static>>> {
    self.routes.get(&id).map(|e| e.handler.clone())
  }

  /// Store a `routeWebSocket` handler / `onMessage` / `onClose`
  /// callback under its owning page/context, so close-time cleanup can
  /// release it.
  pub(crate) fn insert_ws_callback(
    &mut self,
    id: u64,
    owner: RouteOwner,
    cb: rquickjs::Persistent<rquickjs::Function<'static>>,
  ) {
    self.ws_owners.insert(id, owner);
    self.ws_callbacks.insert(id, cb);
  }

  /// Release every WS callback registered through `owner`.
  pub(crate) fn remove_ws_callbacks_for_owner(&mut self, owner: &RouteOwner) {
    let ids: Vec<u64> = self
      .ws_owners
      .iter()
      .filter(|(_, o)| *o == owner)
      .map(|(id, _)| *id)
      .collect();
    for id in ids {
      self.ws_owners.remove(&id);
      self.ws_callbacks.remove(&id);
    }
  }

  /// Record that `page_key` registered `name` via `page.exposeFunction`.
  pub(crate) fn track_exposed_owner(&mut self, page_key: usize, name: String) {
    self.exposed_by_page.entry(page_key).or_default().push(name);
  }

  /// Release every `page.exposeFunction` callback `page_key` registered.
  pub(crate) fn remove_exposed_for_page(&mut self, page_key: usize) {
    if let Some(names) = self.exposed_by_page.remove(&page_key) {
      for name in names {
        self.exposed.remove(&name);
      }
    }
  }

  /// Restore a WS callback by id (inside `async_with`).
  pub(crate) fn get_ws_callback(&self, id: u64) -> Option<rquickjs::Persistent<rquickjs::Function<'static>>> {
    self.ws_callbacks.get(&id).cloned()
  }

  pub(crate) fn get_route_pred(&self, id: u64) -> Option<rquickjs::Persistent<rquickjs::Function<'static>>> {
    self.routes.get(&id).and_then(|e| e.pred.clone())
  }

  /// `(id, predicate)` pairs registered through `owner`, for
  /// `unroute(fn)` identity matching.
  pub(crate) fn predicate_routes_for_owner(
    &self,
    owner: &RouteOwner,
  ) -> Vec<(u64, rquickjs::Persistent<rquickjs::Function<'static>>)> {
    self
      .routes
      .iter()
      .filter(|(_, e)| &e.owner == owner)
      .filter_map(|(id, e)| e.pred.clone().map(|p| (*id, p)))
      .collect()
  }

  /// Remove one registration, returning the predicate route's core
  /// matcher (if any) so the caller can `unroute` it core-side.
  pub(crate) fn remove_route(&mut self, id: u64) -> Option<ferridriver::url_matcher::UrlMatcher> {
    self.routes.remove(&id).and_then(|e| e.matcher)
  }

  /// Drop every registration owned by `owner` (page `unrouteAll`, or a
  /// page closing — its persisted handlers must not outlive it on the
  /// session VM).
  pub(crate) fn remove_routes_for_owner(&mut self, owner: &RouteOwner) {
    self.routes.retain(|_, e| &e.owner != owner);
  }

  pub(crate) fn remove_locator_handler(&mut self, id: u64) {
    self.locator_handlers.remove(&id);
  }

  pub(crate) fn insert_event_listener(
    &mut self,
    id: u64,
    event: String,
    page_key: usize,
    f: rquickjs::Persistent<rquickjs::Function<'static>>,
  ) {
    self.event_listeners.insert(
      id,
      EventListenerEntry {
        event,
        page_key,
        listener: f,
      },
    );
  }

  pub(crate) fn get_event_listener(&self, id: u64) -> Option<rquickjs::Persistent<rquickjs::Function<'static>>> {
    self.event_listeners.get(&id).map(|e| e.listener.clone())
  }

  pub(crate) fn remove_event_listener(&mut self, id: u64) {
    self.event_listeners.remove(&id);
  }

  pub(crate) fn clear_event_listeners(&mut self) {
    self.event_listeners.clear();
  }

  /// Drop every listener registered for `event`; returns the removed
  /// core listener ids so the caller can detach them from the emitter.
  pub(crate) fn remove_event_listeners_named(&mut self, event: &str) -> Vec<u64> {
    let ids: Vec<u64> = self
      .event_listeners
      .iter()
      .filter(|(_, e)| e.event == event)
      .map(|(id, _)| *id)
      .collect();
    for id in &ids {
      self.event_listeners.remove(id);
    }
    ids
  }

  /// Drop every listener registered through the page identified by
  /// `page_key` (see `ferridriver::Page::backend_page_id`); returns the
  /// removed core listener ids. Called when that page closes so its
  /// persisted callbacks don't outlive it on the session VM.
  pub(crate) fn remove_event_listeners_for_page(&mut self, page_key: usize) -> Vec<u64> {
    let ids: Vec<u64> = self
      .event_listeners
      .iter()
      .filter(|(_, e)| e.page_key == page_key)
      .map(|(id, _)| *id)
      .collect();
    for id in &ids {
      self.event_listeners.remove(id);
    }
    ids
  }

  /// `(id, listener)` pairs registered for `event` — the `off(event,
  /// fn)` binding restores each and compares against the given function
  /// by JS identity.
  pub(crate) fn event_listeners_named(
    &self,
    event: &str,
  ) -> Vec<(u64, rquickjs::Persistent<rquickjs::Function<'static>>)> {
    self
      .event_listeners
      .iter()
      .filter(|(_, e)| e.event == event)
      .map(|(id, e)| (*id, e.listener.clone()))
      .collect()
  }
}

/// One `page.on` / `page.once` registration in [`PageCallbacks`].
pub(crate) struct EventListenerEntry {
  event: String,
  /// `ferridriver::Page::backend_page_id()` of the registering page.
  page_key: usize,
  listener: rquickjs::Persistent<rquickjs::Function<'static>>,
}

pub(crate) struct PageCallbacksUd(std::cell::RefCell<PageCallbacks>);

// SAFETY: holds only `'static` data (`Persistent<…>` handles), so
// re-stating the unused `'js` lifetime is sound — identical rationale to
// `BddUserData` / `SessionAsyncCtx`.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for PageCallbacksUd {
  type Changed<'to> = PageCallbacksUd;
}

/// One `(listener_id, remove_after_dispatch, event)` message from a core
/// `EventCallback` (backend task) to the context's event pump.
pub(crate) type PageEventMsg = (u64, bool, Arc<Page>, ferridriver::events::PageEvent);

/// Capacity of the page-event pump channel. The session's VM event loop
/// drains the pump even between `run_script` calls, but a chatty page
/// with a registered listener can still outrun it. The bound turns that
/// from unbounded memory growth into bounded loss: when full, the
/// newest event is dropped with a warning (matching the broadcast
/// `Lagged` policy elsewhere).
pub(crate) const PAGE_EVENT_PUMP_CAPACITY: usize = 1024;

/// Per-context sender feeding the single `page.on` event pump.
///
/// ## VM re-entry discipline (canonical statement)
///
/// Three ways code outside an `execute` can reach the VM; only the
/// first two are allowed:
///
/// 1. **`ctx.spawn` pump** (this type; sidecars; screencast): for a
///    LONG-LIVED loop that calls plain JS callbacks. The pump future
///    lives on the runtime's own schedular, which only the session's
///    single VM event loop (`crate::vm`) ever polls — it stays on the
///    interpreter's execution context by construction. The loop keeps
///    pumps advancing while the VM idles between executes AND while a
///    script execute is parked on a host await (the shape that lets a
///    single awaited `page.evaluate` observe a driver→page dispatch).
/// 2. **`VmHandle` job** (route dispatch, `exposeFunction` /
///    `exposeBinding` calls, script executes themselves): for anything
///    that must re-enter the VM from another task. `vm_with!` submits
///    the closure to the event loop, which `ctx.spawn`s it — so jobs
///    interleave with each other and with parked executes. Never create
///    an `async_with!` against a session runtime: a transient
///    `WithFuture` polls the schedular, steals its single wake-queue
///    waker slot, and dies with it — every later schedular-task wake
///    (backend response resolving an awaited `page.evaluate`, a pump
///    message) is then silently lost.
/// 3. **Touching `Ctx` / restoring a `Persistent` directly from a
///    backend thread**: never. Core event callbacks fire on backend
///    tokio threads concurrently with the script's execute; they may
///    only `send` into a channel.
///
/// History: long-lived `tokio::spawn` + per-event `async_with` loops
/// (the old event dispatch, the old sidecar pump) crashed with silent
/// SIGSEGVs ("event-listener slab entered unreachable code") under
/// multi-thread runtimes — a long-lived second-thread loop resolving
/// plain JS promises interleaves with a busy execute in ways the
/// one-shot dispatch shape does not. Hence rule 1 for anything
/// loop-shaped, rule 2 only for one-shot dispatch.
pub(crate) struct PageEventPumpUd(tokio::sync::mpsc::Sender<PageEventMsg>);

// SAFETY: holds only a channel sender (`'static`), so re-stating the
// unused `'js` lifetime is sound — identical rationale to `PageCallbacksUd`.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for PageEventPumpUd {
  type Changed<'to> = PageEventPumpUd;
}

/// Get (or lazily start) this context's page-event pump and return its
/// sender. The pump future lives on the QuickJS runtime executor,
/// polled by the session's VM event loop — so events keep flowing while
/// a script is parked on an await and between executes.
pub(crate) fn ensure_event_pump(ctx: &rquickjs::Ctx<'_>) -> tokio::sync::mpsc::Sender<PageEventMsg> {
  if let Some(ud) = ctx.userdata::<PageEventPumpUd>() {
    return ud.0.clone();
  }
  let (tx, mut rx) = tokio::sync::mpsc::channel::<PageEventMsg>(PAGE_EVENT_PUMP_CAPACITY);
  let pump_ctx = ctx.clone();
  ctx.spawn(async move {
    while let Some((id, remove_after, page, ev)) = rx.recv().await {
      let Ok(Some(f)) = with_page_callbacks(&pump_ctx, |r| {
        let f = r.get_event_listener(id);
        if remove_after {
          r.remove_event_listener(id);
        }
        f
      }) else {
        continue;
      };
      // Already on the interpreter thread — restore + invoke directly. A
      // throwing listener is swallowed so one bad callback can't kill the
      // pump (matches the NAPI threadsafe-function listeners).
      let Ok(func) = f.restore(&pump_ctx) else { continue };
      let Ok(arg) = page_event_to_js(&pump_ctx, &page, ev) else {
        continue;
      };
      let _: rquickjs::Result<rquickjs::Value<'_>> = func.call((arg,));
    }
  });
  let _ = ctx.store_userdata(PageEventPumpUd(tx.clone()));
  tx
}

/// Ensure the page-callbacks userdata exists on this context.
/// Idempotent; called at `Session::create` and defensively from the
/// `page.route` / `exposeFunction` / `startScreencast` bindings.
pub(crate) fn ensure_page_callbacks(ctx: &rquickjs::Ctx<'_>) {
  if ctx.userdata::<PageCallbacksUd>().is_none() {
    let _ = ctx.store_userdata(PageCallbacksUd(std::cell::RefCell::new(PageCallbacks::default())));
  }
}

pub(crate) fn with_page_callbacks<R>(
  ctx: &rquickjs::Ctx<'_>,
  f: impl FnOnce(&mut PageCallbacks) -> R,
) -> rquickjs::Result<R> {
  ensure_page_callbacks(ctx);
  let ud = ctx.userdata::<PageCallbacksUd>().ok_or_else(|| {
    rquickjs::Error::new_from_js_message("page", "Error", "page callbacks registry missing".to_string())
  })?;
  let mut reg = ud.0.borrow_mut();
  Ok(f(&mut reg))
}

/// Stash an exposed-binding JS callback keyed by binding name. Shared
/// by `page.exposeFunction` and `context.exposeBinding` /
/// `context.exposeFunction` — both inject `window[name]`, so a single
/// name-keyed registry suffices. Exposed `pub(crate)` so the context
/// binding (in a sibling module) can reuse the same userdata.
pub(crate) fn insert_exposed_callback(
  ctx: &rquickjs::Ctx<'_>,
  name: String,
  cb: rquickjs::Persistent<rquickjs::Function<'static>>,
) -> rquickjs::Result<()> {
  with_page_callbacks(ctx, |r| r.exposed.insert(name, cb))?;
  Ok(())
}

/// Look up a previously stashed exposed-binding callback by name.
pub(crate) fn get_exposed_callback(
  ctx: &rquickjs::Ctx<'_>,
  name: &str,
) -> rquickjs::Result<Option<rquickjs::Persistent<rquickjs::Function<'static>>>> {
  with_page_callbacks(ctx, |r| r.exposed.get(name).cloned())
}

/// Drop a stashed exposed-binding callback (binding disposed).
pub(crate) fn remove_exposed_callback(ctx: &rquickjs::Ctx<'_>, name: &str) {
  let _ = with_page_callbacks(ctx, |r| r.exposed.remove(name));
}
