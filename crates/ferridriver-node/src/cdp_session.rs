//! `CDPSession` — NAPI binding for raw Chrome DevTools Protocol access.
//!
//! Mirrors Playwright's `CDPSession` (`client/cdpSession.ts`):
//! `send(method, params?)`, `detach()`, and per-protocol-event
//! listeners. Created via `browser.newBrowserCDPSession()` or
//! `browserContext.newCDPSession(page)`; Chromium-only.

use napi::Result;
use napi_derive::napi;

use crate::error::IntoNapi;

type EventTsfn =
  napi::threadsafe_function::ThreadsafeFunction<serde_json::Value, (), serde_json::Value, napi::Status, false, true, 0>;

/// Raw CDP session attached to a page target or the browser target.
#[napi]
pub struct CDPSession {
  inner: ferridriver::CdpSession,
  /// Listener-id registry so `off(event, listener)` can match the JS
  /// function by identity (`env.strict_equals`), like `page.off`.
  listeners: std::sync::Mutex<Vec<ListenerReg>>,
}

struct ListenerReg {
  event: String,
  id: ferridriver::cdp_session::CdpListenerId,
  fn_ref: napi::bindgen_prelude::FunctionRef<serde_json::Value, ()>,
}

impl CDPSession {
  pub(crate) fn wrap(inner: ferridriver::CdpSession) -> Self {
    Self {
      inner,
      listeners: std::sync::Mutex::new(Vec::new()),
    }
  }
}

#[napi]
impl CDPSession {
  /// Playwright: `cdpSession.send(method, params?)`. Sends a raw
  /// protocol command on this session and resolves with the result.
  #[napi(ts_args_type = "method: string, params?: object", ts_return_type = "Promise<any>")]
  pub async fn send(&self, method: String, params: Option<serde_json::Value>) -> Result<serde_json::Value> {
    self
      .inner
      .send(
        &method,
        params.unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
      )
      .await
      .into_napi()
  }

  /// Playwright: `cdpSession.detach()`. Detaches the session; further
  /// `send` calls reject.
  #[napi]
  pub async fn detach(&self) -> Result<()> {
    self.inner.detach().await.into_napi()
  }

  /// Register a protocol-event listener. The listener receives the
  /// event's `params` object; the special event name `'event'` receives
  /// every event as `{ method, params }` (Playwright wildcard).
  #[napi(ts_args_type = "event: string, listener: (params: any) => void")]
  pub fn on(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'static, serde_json::Value, ()>,
  ) -> Result<()> {
    self.register(event, listener, false)
  }

  /// One-shot variant of [`Self::on`].
  #[napi(ts_args_type = "event: string, listener: (params: any) => void")]
  pub fn once(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'static, serde_json::Value, ()>,
  ) -> Result<()> {
    self.register(event, listener, true)
  }

  /// Remove a previously registered listener (matched by function
  /// identity, like `page.off`).
  #[napi(ts_args_type = "event: string, listener: (params: any) => void")]
  // napi-rs injects Env only as `&Env` (no by-value FromNapiValue).
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn off(
    &self,
    env: &napi::Env,
    event: String,
    listener: napi::bindgen_prelude::Function<'static, serde_json::Value, ()>,
  ) -> Result<()> {
    let incoming = listener.create_ref()?;
    let mut regs = self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut i = 0;
    while i < regs.len() {
      let matches = regs[i].event == event && {
        let a = incoming.borrow_back(env)?;
        let b = regs[i].fn_ref.borrow_back(env)?;
        env.strict_equals(a, b)?
      };
      if matches {
        let reg = regs.remove(i);
        self.inner.off(reg.id);
      } else {
        i += 1;
      }
    }
    Ok(())
  }
}

impl CDPSession {
  fn register(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'static, serde_json::Value, ()>,
    once: bool,
  ) -> Result<()> {
    let fn_ref = listener.create_ref()?;
    let tsfn: EventTsfn = listener
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let callback: ferridriver::cdp_session::CdpEventCallback = std::sync::Arc::new(move |params| {
      tsfn.call(
        params,
        napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
      );
    });
    let id = match (event.as_str(), once) {
      ("event", false) => self.inner.on_any(callback),
      ("event", true) => self.inner.once_any(callback),
      (_, false) => self.inner.on(&event, callback),
      (_, true) => self.inner.once(&event, callback),
    };
    self
      .listeners
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(ListenerReg { event, id, fn_ref });
    Ok(())
  }
}
