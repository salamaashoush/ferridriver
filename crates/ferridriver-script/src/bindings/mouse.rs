//! `MouseJs`: wrapper around `ferridriver::Page::mouse()`.
//!
//! Mirrors Playwright's `page.mouse.*` namespace: `click(x, y)`,
//! `dblclick(x, y)`, `down()`, `up()`, `wheel(dx, dy)`.

use std::sync::Arc;

use ferridriver::Page;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::FerriResultExt;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Mouse")]
pub struct MouseJs {
  #[qjs(skip_trace)]
  page: Arc<Page>,
}

impl MouseJs {
  #[must_use]
  pub fn new(page: Arc<Page>) -> Self {
    Self { page }
  }
}

#[rquickjs::methods]
impl MouseJs {
  /// Click at viewport coordinates `(x, y)`.
  #[qjs(rename = "click")]
  pub async fn click(&self, x: f64, y: f64) -> rquickjs::Result<()> {
    self.page.mouse().click(x, y, None).await.into_js()
  }

  /// Double-click at viewport coordinates `(x, y)`.
  #[qjs(rename = "dblclick")]
  pub async fn dblclick(&self, x: f64, y: f64) -> rquickjs::Result<()> {
    self.page.mouse().dblclick(x, y, None).await.into_js()
  }

  /// Press the left mouse button.
  #[qjs(rename = "down")]
  pub async fn down(&self) -> rquickjs::Result<()> {
    self.page.mouse().down(None).await.into_js()
  }

  /// Release the left mouse button.
  #[qjs(rename = "up")]
  pub async fn up(&self) -> rquickjs::Result<()> {
    self.page.mouse().up(None).await.into_js()
  }

  /// Dispatch a wheel event with the given pixel deltas.
  #[qjs(rename = "wheel")]
  pub async fn wheel(&self, delta_x: f64, delta_y: f64) -> rquickjs::Result<()> {
    self.page.mouse().wheel(delta_x, delta_y).await.into_js()
  }
}
