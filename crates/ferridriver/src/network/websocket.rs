//! Live `WebSocket` handle and frame payload decoding.

use std::sync::Arc;

/// WebSocket payload — text frames stay as `String`, binary frames carry bytes.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum WebSocketPayload {
  Text(String),
  Binary(Vec<u8>),
}

/// The payload of a `Network.webSocketFrameSent` / `...Received` event
/// (CDP and `WebKit` share the shape). Like Playwright, a frame with no
/// payload data or an opcode other than text (1) or binary (2) is not
/// reported: those are control frames, the close handshake among them.
pub(crate) fn websocket_frame_payload(params: &serde_json::Value) -> Option<WebSocketPayload> {
  use base64::Engine as _;
  let response = params.get("response")?;
  let data = response.get("payloadData").and_then(serde_json::Value::as_str)?;
  if data.is_empty() {
    return None;
  }
  match response.get("opcode").and_then(serde_json::Value::as_u64) {
    Some(1) => Some(WebSocketPayload::Text(data.to_string())),
    Some(2) => Some(WebSocketPayload::Binary(
      base64::engine::general_purpose::STANDARD
        .decode(data)
        .unwrap_or_else(|_| data.as_bytes().to_vec()),
    )),
    _ => None,
  }
}

/// Playwright's `WebSocketEventMap`.
#[derive(Debug, Clone)]
pub enum WebSocketEvent {
  /// `'framesent'`
  FrameSent(WebSocketPayload),
  /// `'framereceived'`
  FrameReceived(WebSocketPayload),
  /// `'socketerror'`
  Error(String),
  /// `'close'`
  Close,
}

impl crate::events::EmitterEvent for WebSocketEvent {
  fn matches_name(name: &str, event: &Self) -> bool {
    matches!(
      (name, event),
      ("framesent", WebSocketEvent::FrameSent(_))
        | ("framereceived", WebSocketEvent::FrameReceived(_))
        | ("socketerror", WebSocketEvent::Error(_))
        | ("close", WebSocketEvent::Close)
    )
  }
}

#[derive(Clone)]
pub struct WebSocket {
  inner: Arc<WebSocketState>,
}

impl std::fmt::Debug for WebSocket {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("WebSocket")
      .field("url", &self.inner.url)
      .field("closed", &self.is_closed())
      .finish()
  }
}

pub(crate) struct WebSocketState {
  url: String,
  closed: std::sync::atomic::AtomicBool,
  events: crate::events::Emitter<WebSocketEvent>,
}

impl WebSocket {
  #[must_use]
  pub fn new(url: String) -> Self {
    Self {
      inner: Arc::new(WebSocketState {
        url,
        closed: std::sync::atomic::AtomicBool::new(false),
        events: crate::events::Emitter::new(),
      }),
    }
  }

  #[must_use]
  pub fn url(&self) -> &str {
    &self.inner.url
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.inner.closed.load(std::sync::atomic::Ordering::Acquire)
  }

  /// The socket's event emitter (`webSocket.on('framereceived', ...)`).
  /// Lossless: every frame reaches every listener in order.
  #[must_use]
  pub fn events(&self) -> &crate::events::Emitter<WebSocketEvent> {
    &self.inner.events
  }

  /// Subscribe to frame / error / close events. Subscribers see only
  /// events emitted after subscribing.
  #[must_use]
  pub fn subscribe(&self) -> crate::events::EventSubscription<WebSocketEvent> {
    self.inner.events.subscribe()
  }

  // -- Internal mutators ---------------------------------------------------

  pub fn emit_frame_sent(&self, payload: WebSocketPayload) {
    self.inner.events.emit(WebSocketEvent::FrameSent(payload));
  }
  pub fn emit_frame_received(&self, payload: WebSocketPayload) {
    self.inner.events.emit(WebSocketEvent::FrameReceived(payload));
  }
  pub fn emit_error(&self, message: String) {
    self.inner.events.emit(WebSocketEvent::Error(message));
  }
  pub fn emit_close(&self) {
    self.inner.closed.store(true, std::sync::atomic::Ordering::Release);
    self.inner.events.emit(WebSocketEvent::Close);
  }
}
