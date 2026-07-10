//! `CdpSessionJs`: QuickJS binding for raw Chrome DevTools Protocol
//! access.
//!
//! Mirrors Playwright's `CDPSession` (`client/cdpSession.ts`):
//! `send(method, params?)`, `detach()`, and per-protocol-event
//! listeners (`on` / `once` / `off`; the special event name `'event'`
//! is the every-event wildcard receiving `{ method, params }`). Created
//! via `browser.newBrowserCDPSession()` or
//! `browserContext.newCDPSession(page)`; Chromium-only.

use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};

use crate::bindings::convert::{FerriResultCtxExt, json_to_js, serde_from_js};
use crate::bindings::page::{PageCallbacks, with_page_callbacks};

/// Bounded per-context CDP-event pump queue: protocol events can storm
/// (e.g. `Network.*` with all domains enabled); `try_send` drops beyond
/// this rather than exhausting memory — same policy as the page-event
/// pump.
const CDP_PUMP_CAPACITY: usize = 1024;

type CdpPumpMsg = (u64, serde_json::Value);

struct CdpEventPumpUd(tokio::sync::mpsc::Sender<CdpPumpMsg>);

// SAFETY: holds only a `'static` channel sender, so re-stating the unused
// `'js` lifetime is sound — identical rationale to `WsEventPumpUd`.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for CdpEventPumpUd {
  type Changed<'to> = CdpEventPumpUd;
}

/// Get (or lazily start) this context's CDP-listener pump. Same
/// single-owner-VM discipline as the WS pump: the pump future runs on
/// the session's VM event loop, so JS callbacks never execute off the
/// interpreter thread.
fn ensure_cdp_pump(ctx: &Ctx<'_>) -> tokio::sync::mpsc::Sender<CdpPumpMsg> {
  if let Some(ud) = ctx.userdata::<CdpEventPumpUd>() {
    return ud.0.clone();
  }
  let (tx, mut rx) = tokio::sync::mpsc::channel::<CdpPumpMsg>(CDP_PUMP_CAPACITY);
  let pump_ctx = ctx.clone();
  ctx.spawn(async move {
    while let Some((id, payload)) = rx.recv().await {
      let Ok(Some(saved)) = with_page_callbacks(&pump_ctx, |r| r.get_cdp_listener(id)) else {
        continue;
      };
      let Ok(f) = saved.restore(&pump_ctx) else { continue };
      let Ok(arg) = json_to_js(&pump_ctx, &payload) else {
        continue;
      };
      // A throwing listener is swallowed so one bad handler can't kill
      // the pump (same policy as the page-event pump).
      let fut = async {
        let ret: rquickjs::Result<Value<'_>> = f.call((arg,));
        if let Ok(v) = ret {
          if let Some(promise) = v.as_promise() {
            let _ = promise.clone().into_future::<Value<'_>>().await;
          }
        }
      };
      crate::bindings::fetch::bracket_net(
        crate::bindings::fetch::policy_cell(&pump_ctx),
        saved.net().cloned(),
        fut,
      )
      .await;
    }
  });
  let _ = ctx.store_userdata(CdpEventPumpUd(tx.clone()));
  tx
}

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "CDPSession")]
pub struct CdpSessionJs {
  #[qjs(skip_trace)]
  inner: ferridriver::CdpSession,
  /// Core listener ids registered through THIS wrapper, so a dropped
  /// session's listeners can be released via `off`.
  #[qjs(skip_trace)]
  core_ids: std::cell::RefCell<Vec<(u64, ferridriver::cdp_session::CdpListenerId)>>,
}

impl CdpSessionJs {
  #[must_use]
  pub fn new(inner: ferridriver::CdpSession) -> Self {
    Self {
      inner,
      core_ids: std::cell::RefCell::new(Vec::new()),
    }
  }
}

#[rquickjs::methods]
impl CdpSessionJs {
  /// Playwright: `cdpSession.send(method, params?)`.
  #[qjs(rename = "send")]
  pub async fn send<'js>(
    &self,
    ctx: Ctx<'js>,
    method: String,
    params: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let params_json = match params.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => serde_from_js(&ctx, v)?,
      _ => serde_json::Value::Object(serde_json::Map::new()),
    };
    let result = self.inner.send(&method, params_json).await.into_js_with(&ctx)?;
    json_to_js(&ctx, &result)
  }

  /// Playwright: `cdpSession.detach()`.
  #[qjs(rename = "detach")]
  pub async fn detach(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.detach().await.into_js_with(&ctx)
  }

  /// Register a protocol-event listener. `'event'` is the every-event
  /// wildcard (receives `{ method, params }`).
  #[qjs(rename = "on")]
  pub fn on<'js>(&self, ctx: Ctx<'js>, event: String, handler: rquickjs::Function<'js>) -> rquickjs::Result<()> {
    self.register(&ctx, event, handler, false)
  }

  /// One-shot variant of `on`.
  #[qjs(rename = "once")]
  pub fn once<'js>(&self, ctx: Ctx<'js>, event: String, handler: rquickjs::Function<'js>) -> rquickjs::Result<()> {
    self.register(&ctx, event, handler, true)
  }

  /// Remove a previously registered listener (matched by JS function
  /// identity).
  #[qjs(rename = "off")]
  pub fn off<'js>(&self, ctx: Ctx<'js>, event: String, handler: rquickjs::Function<'js>) -> rquickjs::Result<()> {
    let saved = with_page_callbacks(&ctx, |r| r.cdp_listeners_for_event(&event))?;
    let mut victims: Vec<u64> = Vec::new();
    for (id, sp) in saved {
      let stored = sp.restore(&ctx)?;
      if stored.as_value() == handler.as_value() {
        victims.push(id);
      }
    }
    for id in victims {
      with_page_callbacks(&ctx, |r| r.remove_cdp_listener(id))?;
      let mut core_ids = self.core_ids.borrow_mut();
      if let Some(pos) = core_ids.iter().position(|(rid, _)| *rid == id) {
        let (_, core_id) = core_ids.remove(pos);
        self.inner.off(core_id);
      }
    }
    Ok(())
  }
}

impl CdpSessionJs {
  fn register<'js>(
    &self,
    ctx: &Ctx<'js>,
    event: String,
    handler: rquickjs::Function<'js>,
    once: bool,
  ) -> rquickjs::Result<()> {
    let id = with_page_callbacks(ctx, PageCallbacks::next_route_id)?;
    let net = crate::bindings::fetch::active_net(ctx);
    let saved = crate::bindings::page::SavedCallback::save_with_net(ctx, handler, net);
    with_page_callbacks(ctx, |r| r.insert_cdp_listener(id, event.clone(), saved))?;
    let pump = ensure_cdp_pump(ctx);
    let callback: ferridriver::cdp_session::CdpEventCallback = std::sync::Arc::new(move |payload| {
      // Backend thread — never touch the VM here; buffer through the
      // pump (dropped beyond capacity, same as the page-event pump).
      let _ = pump.try_send((id, payload));
    });
    let core_id = match (event.as_str(), once) {
      ("event", false) => self.inner.on_any(callback),
      ("event", true) => self.inner.once_any(callback),
      (_, false) => self.inner.on(&event, callback),
      (_, true) => self.inner.once(&event, callback),
    };
    self.core_ids.borrow_mut().push((id, core_id));
    Ok(())
  }
}
