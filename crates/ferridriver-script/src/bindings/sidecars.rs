//! `sidecars` JS binding: connect to a declared sidecar process and drive
//! it with `send(method, params?)` (Promise) plus pushed events
//! (`on`/`once`/`off`). The transport lives in [`crate::sidecar`]; this is
//! the QuickJS surface.
//!
//! Connecting is by declared name only — `sidecars.connect(name)` resolves a
//! `[[sidecars]]` spec the operator configured; scripts cannot spawn an
//! arbitrary process (that would defeat the sandbox). One warm instance per
//! name per session.
//!
//! Pushed events (id-less `{method, params}` frames the child writes) are
//! dispatched to JS listeners by ONE pump task per connected handle. The
//! pump is spawned onto the QuickJS runtime's OWN executor via
//! [`rquickjs::Ctx::spawn`] (the same mechanism `setInterval` uses), so it
//! is only polled by whichever future holds the runtime lock — a long-lived
//! `tokio::spawn` + per-event `async_with` loop is the shape that crashed
//! here historically (see the canonical re-entry discipline on
//! `bindings::page::PageEventPumpUd`). The pump owns a
//! `broadcast::Receiver`; for each event it restores the matching listeners
//! (`Persistent<Function>` in context userdata, keyed by handle/event) and
//! calls them. The channel is bounded (1024); if a listener falls far enough
//! behind that the channel laps it, those events are dropped (logged, never
//! panics).
//!
//! The pump is only polled while the runtime is being driven (inside a script
//! execution / its awaits). Events that arrive while no script is running
//! buffer in the broadcast channel until the next VM activity.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rquickjs::class::Trace;
use rquickjs::function::Opt;
use rquickjs::{Class, Ctx, Function, IntoJs, JsLifetime, Persistent, Value};
use rustc_hash::FxHashMap;
use tokio::sync::{Mutex, Notify};

use crate::sidecar::{Sidecar, SidecarSpec};

const DEFAULT_SEND_TIMEOUT_MS: u64 = 30_000;

/// Monotonic source of both connection-handle ids and per-listener ids.
/// Globally unique is stronger than needed (listener ids only need to be
/// unique within a handle) but keeps the counter logic trivial.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn throw(ctx: &Ctx<'_>, msg: &str) -> rquickjs::Error {
  rquickjs::Exception::throw_message(ctx, msg)
}

/// The `sidecars` global: the declared specs + the per-session live-connection
/// cache.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Sidecars")]
pub struct SidecarsJs {
  #[qjs(skip_trace)]
  specs: Arc<FxHashMap<String, SidecarSpec>>,
  #[qjs(skip_trace)]
  live: Arc<Mutex<FxHashMap<String, Arc<Sidecar>>>>,
}

#[rquickjs::methods]
impl SidecarsJs {
  /// `sidecars.connect(name)` → `Promise<Sidecar>`. Spawns on first connect;
  /// later calls for the same name return the warm transport (each call still
  /// yields a fresh handle with its own listeners). A dead cached transport
  /// (child died, or some handle `close()`d it) is evicted and respawned —
  /// never handed back as a corpse whose every `send` fails `Closed`.
  #[qjs(rename = "connect")]
  pub async fn connect<'js>(&self, ctx: Ctx<'js>, name: String) -> rquickjs::Result<Value<'js>> {
    let Some(spec) = self.specs.get(&name).cloned() else {
      return Err(throw(
        &ctx,
        &format!("sidecars.connect: unknown sidecar '{name}' — declare it under [[sidecars]]"),
      ));
    };
    let inner = {
      let mut live = self.live.lock().await;
      match live.get(&name) {
        Some(existing) if !existing.is_closed() => existing.clone(),
        _ => {
          let s = Sidecar::connect(&spec).await.map_err(|e| throw(&ctx, &e.to_string()))?;
          live.insert(name, s.clone());
          s
        },
      }
    };
    let wrapper = SidecarJs {
      inner,
      default_timeout_ms: DEFAULT_SEND_TIMEOUT_MS,
      handle_id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
      pump: Arc::new(StdMutex::new(None)),
      live: self.live.clone(),
    };
    let instance = Class::instance(ctx.clone(), wrapper)?;
    IntoJs::into_js(instance, &ctx)
  }
}

/// Per-context registry of event listeners, keyed by connection-handle id
/// then by event name. Lives in context userdata so the single-threaded VM
/// owns the `Persistent` callbacks; the pump task restores them by id from
/// inside an `async_with` re-entry and never moves them across threads
/// (`Persistent<Function>` is not `Send`) — the same discipline `page.route`
/// uses for its handler registry.
type ListenerEntry = (u64, Persistent<Function<'static>>);
type EventListeners = FxHashMap<String, Vec<ListenerEntry>>;

#[derive(Default)]
struct SidecarListeners {
  by_handle: FxHashMap<u64, EventListeners>,
}

struct SidecarListenersUd(RefCell<SidecarListeners>);

// SAFETY: holds only `'static` data (`Persistent<…>` handles in a single-
// threaded VM's userdata), so re-stating the unused `'js` lifetime is sound —
// identical rationale to `PageCallbacksUd` / `SessionAsyncCtx`.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for SidecarListenersUd {
  type Changed<'to> = SidecarListenersUd;
}

fn with_sidecar_listeners<R>(ctx: &Ctx<'_>, f: impl FnOnce(&mut SidecarListeners) -> R) -> R {
  if ctx.userdata::<SidecarListenersUd>().is_none() {
    let _ = ctx.store_userdata(SidecarListenersUd(RefCell::new(SidecarListeners::default())));
  }
  // The userdata was just ensured above; unwrap-free fallback keeps a
  // borrow-conflict (re-entrant store) from panicking the VM.
  match ctx.userdata::<SidecarListenersUd>() {
    Some(ud) => f(&mut ud.0.borrow_mut()),
    None => f(&mut SidecarListeners::default()),
  }
}

/// A connected sidecar handle.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Sidecar")]
pub struct SidecarJs {
  #[qjs(skip_trace)]
  inner: Arc<Sidecar>,
  #[qjs(skip_trace)]
  default_timeout_ms: u64,
  /// Identifies this handle's listener bucket in the context registry.
  #[qjs(skip_trace)]
  handle_id: u64,
  /// Cancellation handle for the one event-pump task, set on the first `on`.
  /// `Some` ⇒ the pump is running; notifying it stops the loop. The pump
  /// future itself lives on the runtime executor and is also dropped when
  /// the VM tears down.
  #[qjs(skip_trace)]
  pump: Arc<StdMutex<Option<Arc<Notify>>>>,
  /// The session connection cache this handle's transport lives in, so
  /// `close()` can evict the now-dead entry (the next `connect` then
  /// respawns instead of returning the corpse).
  #[qjs(skip_trace)]
  live: Arc<Mutex<FxHashMap<String, Arc<Sidecar>>>>,
}

impl Drop for SidecarJs {
  fn drop(&mut self) {
    if let Some(stop) = self
      .pump
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take()
    {
      stop.notify_waiters();
    }
  }
}

impl SidecarJs {
  /// Start the one event-pump for this handle if it is not already running.
  /// The pump runs on the QuickJS runtime's own executor (`ctx.spawn`), so it
  /// only ever touches the interpreter from the interpreter thread. It owns a
  /// `broadcast::Receiver` and dispatches each id-less frame to every callback
  /// registered (in context userdata) for this handle + event.
  fn ensure_pump(&self, ctx: &Ctx<'_>) {
    let mut guard = self.pump.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_some() {
      return;
    }
    let stop = Arc::new(Notify::new());
    let stop_for_task = stop.clone();
    let mut rx = self.inner.subscribe();
    let handle_id = self.handle_id;
    let pump_ctx = ctx.clone();
    ctx.spawn(async move {
      loop {
        let event = tokio::select! {
          () = stop_for_task.notified() => break,
          ev = rx.recv() => ev,
        };
        match event {
          Ok((method, params)) => {
            // Already on the interpreter thread — restore + invoke directly.
            // A throwing listener is swallowed so one bad callback can't kill
            // the pump.
            let targets: Vec<Persistent<Function<'static>>> = with_sidecar_listeners(&pump_ctx, |r| {
              r.by_handle
                .get(&handle_id)
                .and_then(|m| m.get(&method))
                .map(|v| v.iter().map(|(_, f)| f.clone()).collect())
                .unwrap_or_default()
            });
            if targets.is_empty() {
              continue;
            }
            let Ok(arg) = crate::bindings::convert::json_to_js(&pump_ctx, &params) else {
              continue;
            };
            for f in targets {
              if let Ok(func) = f.restore(&pump_ctx) {
                let _: rquickjs::Result<Value<'_>> = func.call((arg.clone(),));
              }
            }
          },
          Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
            // Bounded channel lapped a slow consumer: drop those events.
            tracing::warn!(dropped = n, "sidecar event pump lagged; dropped {n} event(s)");
          },
          Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
      }
    });
    *guard = Some(stop);
  }
}

#[rquickjs::methods]
impl SidecarJs {
  /// `send(method, params?)` → `Promise<result>`. Rejects on a child
  /// `{error}` reply, timeout, or a closed transport.
  #[qjs(rename = "send")]
  pub async fn send<'js>(
    &self,
    ctx: Ctx<'js>,
    method: String,
    params: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let params_json = match params.0 {
      Some(v) if !v.is_null() && !v.is_undefined() => {
        Some(crate::bindings::convert::serde_from_js::<serde_json::Value>(&ctx, v)?)
      },
      _ => None,
    };
    match self.inner.send(&method, params_json, self.default_timeout_ms).await {
      Ok(res) => crate::bindings::convert::json_to_js(&ctx, &res),
      Err(e) => Err(throw(&ctx, &e.to_string())),
    }
  }

  /// `sendMany(calls)` → `Promise<result[]>`. `calls` is an array of
  /// `{ method, params? }`. All requests are issued as ONE batch (one write
  /// syscall, one pending registration) and awaited together, then the
  /// results are returned positionally. Rejects on the first `{error}`
  /// reply / timeout (Promise.all semantics) — the direct, lower-overhead
  /// replacement for `Promise.all(items.map(x => sc.send(x.method, x.params)))`,
  /// collapsing N JS promises + the Promise.all aggregation into one.
  #[qjs(rename = "sendMany")]
  pub async fn send_many<'js>(&self, ctx: Ctx<'js>, calls: Value<'js>) -> rquickjs::Result<Value<'js>> {
    let arr = calls
      .as_array()
      .ok_or_else(|| throw(&ctx, "sendMany: expected an array of { method, params? }"))?;
    let mut reqs: Vec<(String, Option<serde_json::Value>)> = Vec::with_capacity(arr.len());
    for i in 0..arr.len() {
      let item: Value<'js> = arr.get(i)?;
      let obj = item
        .into_object()
        .ok_or_else(|| throw(&ctx, "sendMany: each item must be an object { method, params? }"))?;
      let method: String = obj
        .get::<_, Option<String>>("method")?
        .ok_or_else(|| throw(&ctx, "sendMany: each item needs a string 'method'"))?;
      let params_val: Value<'js> = obj.get("params")?;
      let params = if params_val.is_null() || params_val.is_undefined() {
        None
      } else {
        Some(crate::bindings::convert::serde_from_js::<serde_json::Value>(
          &ctx, params_val,
        )?)
      };
      reqs.push((method, params));
    }

    let results = self.inner.send_many(reqs, self.default_timeout_ms).await;
    let out = rquickjs::Array::new(ctx.clone())?;
    for (i, r) in results.into_iter().enumerate() {
      match r {
        Ok(v) => out.set(i, crate::bindings::convert::json_to_js(&ctx, &v)?)?,
        Err(e) => return Err(throw(&ctx, &e.to_string())),
      }
    }
    Ok(out.into_value())
  }

  /// `on(event, cb)` → unsubscribe function. Registers `cb` for `event`'s
  /// pushed frames and starts the pump on first use. Calling the returned
  /// function removes exactly this listener.
  #[qjs(rename = "on")]
  pub fn on<'js>(&self, ctx: Ctx<'js>, event: String, cb: Function<'js>) -> rquickjs::Result<Function<'js>> {
    let lid = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let saved = Persistent::save(&ctx, cb);
    let handle_id = self.handle_id;
    with_sidecar_listeners(&ctx, |r| {
      r.by_handle
        .entry(handle_id)
        .or_default()
        .entry(event.clone())
        .or_default()
        .push((lid, saved));
    });
    self.ensure_pump(&ctx);

    Function::new(ctx.clone(), move |ctx: Ctx<'_>| {
      with_sidecar_listeners(&ctx, |r| {
        if let Some(v) = r.by_handle.get_mut(&handle_id).and_then(|m| m.get_mut(&event)) {
          v.retain(|(id, _)| *id != lid);
        }
      });
    })
  }

  /// `once(event)` → `Promise<params>`. Resolves with the next matching
  /// event's params, then auto-unsubscribes (its dedicated receiver is
  /// dropped). Independent of `on`'s listener registry.
  #[qjs(rename = "once")]
  pub async fn once<'js>(&self, ctx: Ctx<'js>, event: String) -> rquickjs::Result<Value<'js>> {
    let mut rx = self.inner.subscribe();
    loop {
      match rx.recv().await {
        Ok((method, params)) if method == event => {
          return crate::bindings::convert::json_to_js(&ctx, &params);
        },
        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
          return Err(throw(&ctx, "sidecar closed while waiting for event"));
        },
      }
    }
  }

  /// `off(event, cb?)`. With `cb`, drops that one listener (by `===`
  /// identity); without it, drops every listener for `event`.
  #[qjs(rename = "off")]
  pub fn off<'js>(&self, ctx: Ctx<'js>, event: String, cb: Opt<Function<'js>>) {
    let handle_id = self.handle_id;
    with_sidecar_listeners(&ctx, |r| {
      let Some(by_event) = r.by_handle.get_mut(&handle_id) else {
        return;
      };
      match &cb.0 {
        Some(target) => {
          if let Some(v) = by_event.get_mut(&event) {
            v.retain(|(_, f)| {
              !f.clone()
                .restore(&ctx)
                .is_ok_and(|restored| restored.as_value() == target.as_value())
            });
          }
        },
        None => {
          by_event.remove(&event);
        },
      }
    });
  }

  /// `close()` → `Promise<void>`. Stops the event pump, closes the
  /// transport and reaps the child (and its process group).
  ///
  /// Close is TRANSPORT-scoped, not handle-scoped: every handle from
  /// `connect(name)` shares one child process, so closing any of them
  /// closes it for all. The cache entry is evicted, so a later
  /// `connect(name)` spawns a fresh child.
  #[qjs(rename = "close")]
  pub async fn close(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    if let Some(stop) = self
      .pump
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take()
    {
      stop.notify_waiters();
    }
    with_sidecar_listeners(&ctx, |r| {
      r.by_handle.remove(&self.handle_id);
    });
    {
      let mut live = self.live.lock().await;
      // Guard with pointer identity: if the cache already holds a
      // respawned transport under this name, leave it alone.
      if live.get(self.inner.name()).is_some_and(|s| Arc::ptr_eq(s, &self.inner)) {
        live.remove(self.inner.name());
      }
    }
    self.inner.close().await.map_err(|e| throw(&ctx, &e.to_string()))
  }

  #[qjs(rename = "name")]
  pub fn name(&self) -> String {
    self.inner.name().to_string()
  }
}

/// Install the `sidecars` global. Always installed (even with no declared
/// specs) so `sidecars.connect` exists and rejects unknown names clearly.
pub fn install_sidecars(ctx: &Ctx<'_>, specs: &[SidecarSpec]) -> rquickjs::Result<()> {
  let g = ctx.globals();
  Class::<SidecarsJs>::define(&g)?;
  Class::<SidecarJs>::define(&g)?;
  let mut map = FxHashMap::default();
  for s in specs {
    map.insert(s.name.clone(), s.clone());
  }
  let inst = Class::instance(
    ctx.clone(),
    SidecarsJs {
      specs: Arc::new(map),
      live: Arc::new(Mutex::new(FxHashMap::default())),
    },
  )?;
  g.set("sidecars", inst)?;
  crate::bindings::runtime::mirror_global(ctx, "sidecars")?;
  Ok(())
}
