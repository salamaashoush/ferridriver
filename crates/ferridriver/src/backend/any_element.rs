//! `AnyElement` — zero-cost enum dispatch across all four browser backends.

use crate::error::Result;

use super::{bidi, cdp, webdriver, webkit};

/// Element handle — enum dispatch across backends.
pub enum AnyElement {
  Cdp(cdp::CdpElement),
  WebKit(webkit::WebKitElement),
  Bidi(bidi::BidiElement),
  WebDriver(webdriver::element::WebDriverElement),
}

macro_rules! element_dispatch {
  ($self:expr, $method:ident ( $($arg:expr),* $(,)? )) => {
    match $self {
      AnyElement::Cdp(e) => e.$method($($arg),*).await,
      AnyElement::WebKit(e) => e.$method($($arg),*).await,
      AnyElement::Bidi(e) => e.$method($($arg),*).await,
      AnyElement::WebDriver(e) => e.$method($($arg),*).await,
    }
  };
}

impl AnyElement {
  /// # Errors
  ///
  /// Forwards the error of the active backend's `click`.
  pub async fn click(&self) -> Result<()> {
    element_dispatch!(self, click())
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `dblclick`.
  pub async fn dblclick(&self) -> Result<()> {
    element_dispatch!(self, dblclick())
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `hover`.
  pub async fn hover(&self) -> Result<()> {
    element_dispatch!(self, hover())
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `type_str`.
  pub async fn type_str(&self, text: &str) -> Result<()> {
    element_dispatch!(self, type_str(text))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `call_js_fn`.
  pub async fn call_js_fn(&self, function: &str) -> Result<()> {
    element_dispatch!(self, call_js_fn(function))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `call_js_fn_value`.
  pub async fn call_js_fn_value(&self, function: &str) -> Result<Option<serde_json::Value>> {
    element_dispatch!(self, call_js_fn_value(function))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `scroll_into_view`.
  pub async fn scroll_into_view(&self) -> Result<()> {
    element_dispatch!(self, scroll_into_view())
  }
}
