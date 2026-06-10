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

use crate::bindings::convert::FerriResultCtxExt;
use crate::bindings::convert::serde_from_js;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct JsKeyDelay {
  delay: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JsKeyType {
  delay: Option<u64>,
  named_keys: Option<bool>,
}

fn parse_delay<'js>(ctx: &rquickjs::Ctx<'js>, v: Opt<rquickjs::Value<'js>>) -> rquickjs::Result<Option<u64>> {
  match v.0 {
    Some(val) if !val.is_undefined() && !val.is_null() => Ok(serde_from_js::<JsKeyDelay>(ctx, val)?.delay),
    _ => Ok(None),
  }
}

fn parse_type_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  v: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::page::KeyboardTypeOptions>> {
  match v.0 {
    Some(val) if !val.is_undefined() && !val.is_null() => {
      let parsed = serde_from_js::<JsKeyType>(ctx, val)?;
      Ok(Some(ferridriver::page::KeyboardTypeOptions {
        delay: parsed.delay,
        named_keys: parsed.named_keys,
      }))
    },
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
  pub async fn down(&self, ctx: rquickjs::Ctx<'_>, key: String) -> rquickjs::Result<()> {
    self.page.keyboard().down(&key).await.into_js_with(&ctx)
  }

  /// Dispatch a `keyup` event for `key` on the currently focused element.
  #[qjs(rename = "up")]
  pub async fn up(&self, ctx: rquickjs::Ctx<'_>, key: String) -> rquickjs::Result<()> {
    self.page.keyboard().up(&key).await.into_js_with(&ctx)
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
    self.page.keyboard().press(&key, opts).await.into_js_with(&ctx)
  }

  /// `keyboard.type(text, options?: { delay?, namedKeys? })`.
  #[qjs(rename = "type")]
  pub async fn type_<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    text: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = parse_type_options(&ctx, options)?;
    self.page.keyboard().r#type(&text, opts).await.into_js_with(&ctx)
  }

  /// `keyboard.insertText(text)` — `input` event only, no key events.
  #[qjs(rename = "insertText")]
  pub async fn insert_text(&self, ctx: rquickjs::Ctx<'_>, text: String) -> rquickjs::Result<()> {
    self.page.keyboard().insert_text(&text).await.into_js_with(&ctx)
  }
}
