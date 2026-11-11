//! `WebKitSession` — raw `WebKit` Inspector Protocol access to a page.
//!
//! The `WebKit` counterpart of [`crate::cdp_session::CdpSession`]. Playwright
//! has no public one: its own `WKPage` drives the page's main target through
//! the session it keeps in `_session`, and callers that need a domain
//! Playwright does not wrap (`LayerTree`, `DOM`, `CSS`) reach for that
//! private field. This session talks to the same target.
//!
//! A cross-process navigation replaces the page's main target
//! (`Target.didCommitProvisionalTarget`). [`WebKitSession::send`] always
//! reaches the target that is live at the call, as `WKPage._session` does.
//! An event stream from [`WebKitSession::events`] belongs to the target live
//! when it was taken, and ends when that target goes away.

use crate::backend::webkit::WebKitPage;
use crate::backend::webkit::protocol::Envelope;
use crate::error::{FerriError, Result};

/// One protocol event from the page's target.
#[derive(Debug, Clone, PartialEq)]
pub struct WebKitEvent {
  /// The event's method, such as `"LayerTree.layerTreeDidChange"`.
  pub method: String,
  /// The event's `params` object.
  pub params: serde_json::Value,
}

/// The events of one target, in wire order.
pub struct WebKitEvents {
  envelopes: tokio::sync::mpsc::UnboundedReceiver<Envelope>,
}

impl WebKitEvents {
  /// The next event, or `None` once the target has gone away: the page
  /// closed, or a cross-process navigation replaced the target.
  pub async fn next(&mut self) -> Option<WebKitEvent> {
    loop {
      let envelope = self.envelopes.recv().await?;
      if let Some(method) = envelope.method {
        return Some(WebKitEvent {
          method,
          params: envelope.params,
        });
      }
    }
  }
}

/// A raw `WebKit` Inspector Protocol session on a page's main target.
#[derive(Clone)]
pub struct WebKitSession {
  page: WebKitPage,
}

impl WebKitSession {
  pub(crate) fn new(page: WebKitPage) -> Self {
    Self { page }
  }

  /// Send a raw protocol command to the page's live target, such as
  /// `send("LayerTree.layersForNode", json!({ "nodeId": root }))`.
  ///
  /// # Errors
  ///
  /// Errors if the page has closed or the protocol reports an error.
  pub async fn send(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    if self.page.is_closed() {
      return Err(FerriError::target_closed(Some("The page has been closed.".to_string())));
    }
    self
      .page
      .target_session()
      .send(method, params)
      .await
      .map_err(Into::into)
  }

  /// The id of the page's live target. It changes on a cross-process
  /// navigation.
  #[must_use]
  pub fn target_id(&self) -> String {
    self.page.target_id().to_string()
  }

  /// The events of the page's live target from now on, in wire order. The
  /// stream ends when that target goes away; take another after a
  /// cross-process navigation to follow the new one. A closed page's stream
  /// has already ended.
  #[must_use]
  pub fn events(&self) -> WebKitEvents {
    if self.page.is_closed() {
      let (_, envelopes) = tokio::sync::mpsc::unbounded_channel();
      return WebKitEvents { envelopes };
    }
    WebKitEvents {
      envelopes: self.page.target_session().events(),
    }
  }
}
