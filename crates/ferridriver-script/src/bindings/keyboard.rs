//! `KeyboardJs`: wrapper around `ferridriver::Page::keyboard()`.
//!
//! Mirrors Playwright's `page.keyboard.*` namespace: `down(key)`,
//! `up(key)`, `press(key)` without a selector (acts on whatever element
//! currently has focus).

use std::sync::Arc;

use ferridriver::Page;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use rquickjs::function::Opt;
use serde::Deserialize;

use crate::bindings::convert::{FerriResultExt, serde_from_js};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct JsKeyDelay {
  delay: Option<u64>,
}

fn parse_delay<'js>(ctx: &rquickjs::Ctx<'js>, v: Opt<rquickjs::Value<'js>>) -> rquickjs::Result<Option<u64>> {
  match v.0 {
    Some(val) if !val.is_undefined() && !val.is_null() => Ok(serde_from_js::<JsKeyDelay>(ctx, val)?.delay),
    _ => Ok(None),
  }
}

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

  /// `keyboard.press(key, options?: { delay? })`.
  #[qjs(rename = "press")]
  pub async fn press<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    key: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let delay = parse_delay(&ctx, options)?;
    let opts = delay.map(|d| ferridriver::page::KeyboardPressOptions { delay: Some(d) });
    self.page.keyboard().press(&key, opts).await.into_js()
  }

  /// `keyboard.type(text, options?: { delay? })`.
  #[qjs(rename = "type")]
  pub async fn type_<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    text: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let delay = parse_delay(&ctx, options)?;
    let opts = delay.map(|d| ferridriver::page::KeyboardTypeOptions { delay: Some(d) });
    self.page.keyboard().r#type(&text, opts).await.into_js()
  }

  /// `keyboard.insertText(text)` — `input` event only, no key events.
  #[qjs(rename = "insertText")]
  pub async fn insert_text(&self, text: String) -> rquickjs::Result<()> {
    self.page.keyboard().insert_text(&text).await.into_js()
  }
}
