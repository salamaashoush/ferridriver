//! QuickJS bindings for `ferridriver::network::{Request, Response, WebSocket}`.
//!
//! Each wrapper is a thin pass-through to the core type per Rule 1.
//! Method names mirror Playwright's `client/network.ts` exactly so
//! scripts use the same `request.url()`, `response.body()`,
//! `webSocket.waitForEvent()` shapes as Playwright tests.

use ferridriver::network::{
  Request as CoreRequest, Response as CoreResponse, WebSocket as CoreWebSocket, WebSocketEvent, WebSocketPayload,
};
use ferridriver::route::{ContinueOverrides, FulfillResponse, Route as CoreRoute};
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};
use std::sync::{Arc, Mutex as StdMutex};

use crate::bindings::convert::{FerriResultExt, serde_from_js, serde_to_js};

// ── RequestJs ────────────────────────────────────────────────────────────────

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Request")]
pub struct RequestJs {
  #[qjs(skip_trace)]
  inner: CoreRequest,
  /// Owning page reference, used by `frame()` to resolve frame_id via
  /// the page's frame cache. `None` when the wrapper was constructed
  /// without page context.
  #[qjs(skip_trace)]
  page: Option<Arc<ferridriver::Page>>,
}

impl RequestJs {
  #[must_use]
  pub fn new(inner: CoreRequest) -> Self {
    Self { inner, page: None }
  }

  #[must_use]
  pub fn new_with_page(inner: CoreRequest, page: Arc<ferridriver::Page>) -> Self {
    Self {
      inner,
      page: Some(page),
    }
  }
}

#[rquickjs::methods]
impl RequestJs {
  #[qjs(rename = "url")]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  #[qjs(rename = "method")]
  pub fn method(&self) -> String {
    self.inner.method().to_string()
  }

  #[qjs(rename = "resourceType")]
  pub fn resource_type(&self) -> String {
    self.inner.resource_type().to_string()
  }

  #[qjs(rename = "isNavigationRequest")]
  pub fn is_navigation_request(&self) -> bool {
    self.inner.is_navigation_request()
  }

  #[qjs(rename = "postData")]
  pub fn post_data(&self) -> Option<String> {
    self.inner.post_data()
  }

  #[qjs(rename = "postDataJSON")]
  pub fn post_data_json<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let v = self.inner.post_data_json().into_js()?;
    let v = v.unwrap_or(serde_json::Value::Null);
    serde_to_js(&ctx, &v)
  }

  #[qjs(rename = "headers")]
  pub fn headers<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    serde_to_js(&ctx, &self.inner.headers())
  }

  #[qjs(rename = "headersArray")]
  pub async fn headers_array<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let arr = self.inner.headers_array().await;
    let json: Vec<serde_json::Value> = arr
      .into_iter()
      .map(|h| serde_json::json!({"name": h.name, "value": h.value}))
      .collect();
    serde_to_js(&ctx, &json)
  }

  #[qjs(rename = "allHeaders")]
  pub async fn all_headers<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let h = self.inner.all_headers().await.into_js()?;
    serde_to_js(&ctx, &h)
  }

  #[qjs(rename = "headerValue")]
  pub async fn header_value(&self, name: String) -> rquickjs::Result<Option<String>> {
    self.inner.header_value(&name).await.into_js()
  }

  #[qjs(rename = "failure")]
  pub async fn failure<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    match self.inner.failure().await {
      Some(error_text) => serde_to_js(&ctx, &serde_json::json!({"errorText": error_text})),
      None => Ok(Value::new_null(ctx.clone())),
    }
  }

  #[qjs(rename = "timing")]
  pub fn timing<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    serde_to_js(&ctx, &self.inner.timing())
  }

  #[qjs(rename = "sizes")]
  pub async fn sizes<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let sizes = self.inner.sizes().await.into_js()?;
    serde_to_js(&ctx, &sizes)
  }

  #[qjs(rename = "redirectedFrom")]
  pub fn redirected_from(&self) -> Option<RequestJs> {
    self.inner.redirected_from().map(|r| match self.page.as_ref() {
      Some(page) => RequestJs::new_with_page(r, page.clone()),
      None => RequestJs::new(r),
    })
  }

  #[qjs(rename = "redirectedTo")]
  pub fn redirected_to(&self) -> Option<RequestJs> {
    self.inner.redirected_to().map(|r| match self.page.as_ref() {
      Some(page) => RequestJs::new_with_page(r, page.clone()),
      None => RequestJs::new(r),
    })
  }

  #[qjs(rename = "response")]
  pub async fn response(&self) -> rquickjs::Result<Option<ResponseJs>> {
    let resp = self.inner.response().await.into_js()?;
    Ok(resp.map(|r| match self.page.as_ref() {
      Some(page) => ResponseJs::new_with_page(r, page.clone()),
      None => ResponseJs::new(r),
    }))
  }

  /// Mirrors Playwright `request.frame(): Frame`. Resolves the
  /// initiating frame_id via the owning page's frame cache. Returns
  /// `null` when no frame context is attached.
  #[qjs(rename = "frame")]
  pub fn frame(&self) -> Option<crate::bindings::frame::FrameJs> {
    let page = self.page.as_ref()?;
    let frame_id = self.inner.frame_id()?;
    for f in page.frames() {
      if f.frame_id() == frame_id {
        return Some(crate::bindings::frame::FrameJs::new(f));
      }
    }
    None
  }

  /// Mirrors Playwright `request.serviceWorker(): Worker | null`.
  /// Backed by `Request::service_worker` which always returns `None`
  /// today (Tier-2 §2.7 hasn't landed). Surface kept stable so flipping
  /// the implementation on later is non-breaking.
  #[qjs(rename = "serviceWorker")]
  pub fn service_worker<'js>(&self, ctx: Ctx<'js>) -> Value<'js> {
    // Query the core accessor so this stays self-aware once §2.7 fills
    // in the Worker class — the binding then maps `Some(worker)` to a
    // real WorkerJs instance.
    let _ = self.inner.service_worker();
    Value::new_null(ctx)
  }
}

// ── ResponseJs ───────────────────────────────────────────────────────────────

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Response")]
pub struct ResponseJs {
  #[qjs(skip_trace)]
  inner: CoreResponse,
  #[qjs(skip_trace)]
  page: Option<Arc<ferridriver::Page>>,
}

impl ResponseJs {
  #[must_use]
  pub fn new(inner: CoreResponse) -> Self {
    Self { inner, page: None }
  }

  #[must_use]
  pub fn new_with_page(inner: CoreResponse, page: Arc<ferridriver::Page>) -> Self {
    Self {
      inner,
      page: Some(page),
    }
  }
}

#[rquickjs::methods]
impl ResponseJs {
  #[qjs(rename = "url")]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  #[qjs(rename = "status")]
  pub fn status(&self) -> i32 {
    i32::try_from(self.inner.status()).unwrap_or(i32::MAX)
  }

  #[qjs(rename = "statusText")]
  pub fn status_text(&self) -> String {
    self.inner.status_text().to_string()
  }

  #[qjs(rename = "ok")]
  pub fn ok(&self) -> bool {
    self.inner.ok()
  }

  #[qjs(rename = "fromServiceWorker")]
  pub fn is_from_service_worker(&self) -> bool {
    self.inner.is_from_service_worker()
  }

  #[qjs(rename = "headers")]
  pub fn headers<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    serde_to_js(&ctx, &self.inner.headers())
  }

  #[qjs(rename = "allHeaders")]
  pub async fn all_headers<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let h = self.inner.all_headers().await.into_js()?;
    serde_to_js(&ctx, &h)
  }

  #[qjs(rename = "headersArray")]
  pub async fn headers_array<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let arr = self.inner.headers_array().await;
    let json: Vec<serde_json::Value> = arr
      .into_iter()
      .map(|h| serde_json::json!({"name": h.name, "value": h.value}))
      .collect();
    serde_to_js(&ctx, &json)
  }

  #[qjs(rename = "headerValue")]
  pub async fn header_value(&self, name: String) -> rquickjs::Result<Option<String>> {
    self.inner.header_value(&name).await.into_js()
  }

  #[qjs(rename = "headerValues")]
  pub async fn header_values(&self, name: String) -> rquickjs::Result<Vec<String>> {
    self.inner.header_values(&name).await.into_js()
  }

  /// Response body as base64-encoded string. QuickJS does not have
  /// `Buffer`; scripts decode if they need raw bytes.
  #[qjs(rename = "body")]
  pub async fn body<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let bytes = self.inner.body().await.into_js()?;
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
    serde_to_js(&ctx, &encoded)
  }

  #[qjs(rename = "text")]
  pub async fn text(&self) -> rquickjs::Result<String> {
    self.inner.text().await.into_js()
  }

  #[qjs(rename = "json")]
  pub async fn json<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let v = self.inner.json().await.into_js()?;
    serde_to_js(&ctx, &v)
  }

  /// Mirrors Playwright `response.finished()`. Resolves to `null` on
  /// success, throws on failure.
  #[qjs(rename = "finished")]
  pub async fn finished<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    match self.inner.finished().await {
      Ok(()) => Ok(Value::new_null(ctx.clone())),
      Err(message) => Err(rquickjs::Error::new_from_js_message(
        "Response.finished failure",
        "Error",
        message,
      )),
    }
  }

  #[qjs(rename = "serverAddr")]
  pub async fn server_addr<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    match self.inner.server_addr().await {
      Some(a) => serde_to_js(&ctx, &serde_json::json!({"ipAddress": a.ip_address, "port": a.port})),
      None => Ok(Value::new_null(ctx.clone())),
    }
  }

  #[qjs(rename = "securityDetails")]
  pub async fn security_details<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    match self.inner.security_details().await {
      Some(s) => serde_to_js(&ctx, &s),
      None => Ok(Value::new_null(ctx.clone())),
    }
  }

  #[qjs(rename = "request")]
  pub fn request(&self) -> RequestJs {
    match self.page.as_ref() {
      Some(page) => RequestJs::new_with_page(self.inner.request(), page.clone()),
      None => RequestJs::new(self.inner.request()),
    }
  }

  /// Mirrors Playwright `response.frame(): Frame`. Convenience for
  /// `response.request().frame()`.
  #[qjs(rename = "frame")]
  pub fn frame(&self) -> Option<crate::bindings::frame::FrameJs> {
    self.request().frame()
  }
}

// ── WebSocketJs ──────────────────────────────────────────────────────────────

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "WebSocket")]
pub struct WebSocketJs {
  #[qjs(skip_trace)]
  inner: CoreWebSocket,
}

impl WebSocketJs {
  #[must_use]
  pub fn new(inner: CoreWebSocket) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl WebSocketJs {
  #[qjs(rename = "url")]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  #[qjs(rename = "isClosed")]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }

  /// Mirrors Playwright `webSocket.waitForEvent(event, options?)`.
  /// Resolves with `{ event, payload, error }`.
  #[qjs(rename = "waitForEvent")]
  pub async fn wait_for_event<'js>(
    &self,
    ctx: Ctx<'js>,
    event: String,
    timeout_ms: Option<f64>,
  ) -> rquickjs::Result<Value<'js>> {
    let timeout = std::time::Duration::from_millis(
      #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
      {
        timeout_ms.unwrap_or(30000.0) as u64
      },
    );
    let mut rx = self.inner.subscribe();
    let event_lc = event.to_ascii_lowercase();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
      let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
      if remaining.is_zero() {
        return Err(rquickjs::Error::new_from_js_message(
          "WebSocket.waitForEvent",
          "TimeoutError",
          format!(
            "Timeout {}ms exceeded while waiting for WebSocket event {event:?}",
            timeout.as_millis()
          ),
        ));
      }
      match tokio::time::timeout(remaining, rx.recv()).await {
        Ok(Ok(ev)) => {
          if let Some(json) = match_event(&event_lc, &ev) {
            return serde_to_js(&ctx, &json);
          }
        },
        Ok(Err(_)) => {
          return Err(rquickjs::Error::new_from_js_message(
            "WebSocket.waitForEvent",
            "Error",
            "WebSocket channel closed".to_string(),
          ));
        },
        Err(_) => {
          return Err(rquickjs::Error::new_from_js_message(
            "WebSocket.waitForEvent",
            "TimeoutError",
            format!(
              "Timeout {}ms exceeded while waiting for WebSocket event {event:?}",
              timeout.as_millis()
            ),
          ));
        },
      }
    }
  }
}

fn match_event(name: &str, ev: &WebSocketEvent) -> Option<serde_json::Value> {
  match (name, ev) {
    ("framesent", WebSocketEvent::FrameSent(p)) => Some(serde_json::json!({
      "event": "framesent",
      "payload": payload_to_json(p),
      "error": serde_json::Value::Null,
    })),
    ("framereceived", WebSocketEvent::FrameReceived(p)) => Some(serde_json::json!({
      "event": "framereceived",
      "payload": payload_to_json(p),
      "error": serde_json::Value::Null,
    })),
    ("socketerror", WebSocketEvent::Error(msg)) => Some(serde_json::json!({
      "event": "socketerror",
      "payload": serde_json::Value::Null,
      "error": msg,
    })),
    ("close", WebSocketEvent::Close) => Some(serde_json::json!({
      "event": "close",
      "payload": serde_json::Value::Null,
      "error": serde_json::Value::Null,
    })),
    _ => None,
  }
}

fn payload_to_json(p: &WebSocketPayload) -> serde_json::Value {
  match p {
    WebSocketPayload::Text(s) => serde_json::Value::String(s.clone()),
    WebSocketPayload::Binary(b) => {
      serde_json::Value::String(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b))
    },
  }
}

// ── RouteJs ──────────────────────────────────────────────────────────────────
//
// Mirrors Playwright's `Route` interface: `fulfill`, `continue`, `abort`,
// plus the `request()` getter. The handler-callback path (registering
// the JS function with `page.route(matcher, fn)`) lives in `bindings/page.rs`
// — `RouteJs` itself is the per-invocation wrapper passed into the
// handler.

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Route")]
pub struct RouteJs {
  #[qjs(skip_trace)]
  inner: StdMutex<Option<CoreRoute>>,
}

impl RouteJs {
  /// Construct a wrapper around a paused-route handle. The handle is
  /// consumed on the first call to `fulfill` / `continue` / `abort`;
  /// subsequent calls become no-ops. If the JS callback returns
  /// without invoking any of the three, the inner [`CoreRoute`]'s
  /// own `Drop` falls open and continues the request.
  #[must_use]
  pub fn new(inner: CoreRoute) -> Self {
    Self {
      inner: StdMutex::new(Some(inner)),
    }
  }
}

#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsFulfillOptions {
  status: Option<i32>,
  body: Option<String>,
  content_type: Option<String>,
  headers: Option<Vec<(String, String)>>,
}

#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsContinueOptions {
  url: Option<String>,
  method: Option<String>,
  headers: Option<Vec<(String, String)>>,
  post_data: Option<String>,
}

#[rquickjs::methods]
impl RouteJs {
  /// Mirrors Playwright `route.url(): string`.
  #[qjs(rename = "url")]
  pub fn url(&self) -> String {
    self
      .inner
      .lock()
      .ok()
      .and_then(|g| g.as_ref().map(|r| r.request().url.clone()))
      .unwrap_or_default()
  }

  /// Mirrors Playwright `route.request().method()`.
  #[qjs(rename = "method")]
  pub fn method(&self) -> String {
    self
      .inner
      .lock()
      .ok()
      .and_then(|g| g.as_ref().map(|r| r.request().method.clone()))
      .unwrap_or_default()
  }

  /// Mirrors Playwright `route.request().resourceType()`.
  #[qjs(rename = "resourceType")]
  pub fn resource_type(&self) -> String {
    self
      .inner
      .lock()
      .ok()
      .and_then(|g| g.as_ref().map(|r| r.request().resource_type.clone()))
      .unwrap_or_default()
  }

  /// Mirrors Playwright `route.request().postData(): string | null`.
  #[qjs(rename = "postData")]
  pub fn post_data(&self) -> Option<String> {
    self
      .inner
      .lock()
      .ok()
      .and_then(|g| g.as_ref().and_then(|r| r.request().post_data.clone()))
  }

  /// Headers as a plain JS object (`Record<string, string>`).
  #[qjs(rename = "headers")]
  pub fn headers<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let map: rustc_hash::FxHashMap<String, String> = self
      .inner
      .lock()
      .ok()
      .and_then(|g| g.as_ref().map(|r| r.request().headers.clone()))
      .unwrap_or_default();
    serde_to_js(&ctx, &map)
  }

  /// Mirrors Playwright `route.fulfill(options?)`.
  #[qjs(rename = "fulfill")]
  pub fn fulfill<'js>(&self, ctx: Ctx<'js>, options: rquickjs::function::Opt<Value<'js>>) -> rquickjs::Result<()> {
    let opts: JsFulfillOptions = match options.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => serde_from_js(&ctx, v)?,
      _ => JsFulfillOptions::default(),
    };
    let route = self
      .inner
      .lock()
      .ok()
      .and_then(|mut g| g.take())
      .ok_or_else(|| rquickjs::Error::new_from_js_message("Route", "Error", "Route already handled".to_string()))?;
    let mut headers: Vec<(String, String)> = opts.headers.unwrap_or_default();
    if let Some(ct) = opts.content_type.clone() {
      if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
        headers.push(("content-type".to_string(), ct));
      }
    }
    let body_bytes = opts.body.unwrap_or_default().into_bytes();
    route.fulfill(FulfillResponse {
      status: opts.status.unwrap_or(200),
      headers,
      body: body_bytes,
      content_type: opts.content_type,
    });
    Ok(())
  }

  /// Mirrors Playwright `route.continue(options?)`.
  #[qjs(rename = "continue")]
  pub fn continue_<'js>(&self, ctx: Ctx<'js>, options: rquickjs::function::Opt<Value<'js>>) -> rquickjs::Result<()> {
    let opts: JsContinueOptions = match options.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => serde_from_js(&ctx, v)?,
      _ => JsContinueOptions::default(),
    };
    let route = self
      .inner
      .lock()
      .ok()
      .and_then(|mut g| g.take())
      .ok_or_else(|| rquickjs::Error::new_from_js_message("Route", "Error", "Route already handled".to_string()))?;
    route.continue_route(ContinueOverrides {
      url: opts.url,
      method: opts.method,
      headers: opts.headers,
      post_data: opts.post_data.map(String::into_bytes),
    });
    Ok(())
  }

  /// Mirrors Playwright `route.abort(errorCode?)`.
  #[qjs(rename = "abort")]
  pub fn abort(&self, error_code: Option<String>) -> rquickjs::Result<()> {
    let route = self
      .inner
      .lock()
      .ok()
      .and_then(|mut g| g.take())
      .ok_or_else(|| rquickjs::Error::new_from_js_message("Route", "Error", "Route already handled".to_string()))?;
    route.abort(&error_code.unwrap_or_else(|| "blockedbyclient".to_string()));
    Ok(())
  }
}
