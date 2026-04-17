//! `KeyboardJs`: wrapper around `ferridriver::Page::keyboard()`.
//!
//! Mirrors Playwright's `page.keyboard.*` namespace: `down(key)`,
//! `up(key)`, `press(key)` without a selector (acts on whatever element
//! currently has focus).

use std::sync::Arc;

use ferridriver::Page;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::FerriResultExt;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Keyboard")]
pub struct KeyboardJs {
  #[qjs(skip_trace)]
  page: Arc<Page>,
}

impl KeyboardJs {
  #[must_use]
  pub fn new(page: Arc<Page>) -> Self {
    Self { page }
  }
}

#[rquickjs::methods]
impl KeyboardJs {
  /// Dispatch a `keydown` event for `key` on the currently focused element.
  #[qjs(rename = "down")]
  pub async fn down(&self, key: String) -> rquickjs::Result<()> {
    self.page.keyboard().down(&key).await.into_js()
  }

  /// Dispatch a `keyup` event for `key` on the currently focused element.
  #[qjs(rename = "up")]
  pub async fn up(&self, key: String) -> rquickjs::Result<()> {
    self.page.keyboard().up(&key).await.into_js()
  }

  /// Dispatch a full press (down + up) for `key` on the currently focused element.
  #[qjs(rename = "press")]
  pub async fn press(&self, key: String) -> rquickjs::Result<()> {
    self.page.keyboard().press(&key).await.into_js()
  }
}
