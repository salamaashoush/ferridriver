//! `MouseJs`: wrapper around `ferridriver::Page::mouse()`.
//!
//! Mirrors Playwright's `page.mouse.*` namespace: `move(x, y, options?)`,
//! `click(x, y, options?)`, `dblclick(x, y, options?)`, `down(options?)`,
//! `up(options?)`, `wheel(dx, dy)`.

use std::sync::Arc;

use ferridriver::Page;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;
use rquickjs::function::Opt;
use serde::Deserialize;

use crate::bindings::convert::{FerriResultExt, serde_from_js};

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JsMouseClickOptions {
  button: Option<String>,
  click_count: Option<u32>,
  delay: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JsMouseMoveOptions {
  steps: Option<u32>,
}

fn parse_click_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  v: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<JsMouseClickOptions> {
  match v.0 {
    Some(val) if !val.is_undefined() && !val.is_null() => serde_from_js(ctx, val),
    _ => Ok(JsMouseClickOptions::default()),
  }
}

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
  /// `mouse.click(x, y, options?: { button?, clickCount? })`.
  #[qjs(rename = "click")]
  pub async fn click<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    x: f64,
    y: f64,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let o = parse_click_options(&ctx, options)?;
    let opts = ferridriver::page::MouseClickOptions {
      button: o.button,
      click_count: o.click_count,
      delay: o.delay,
    };
    self.page.mouse().click(x, y, Some(opts)).await.into_js()
  }

  /// `mouse.move(x, y, options?: { steps? })`.
  #[qjs(rename = "move")]
  pub async fn move_<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    x: f64,
    y: f64,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let steps = match options.0 {
      Some(val) if !val.is_undefined() && !val.is_null() => serde_from_js::<JsMouseMoveOptions>(&ctx, val)?.steps,
      _ => None,
    };
    self.page.mouse().r#move(x, y, steps).await.into_js()
  }

  /// `mouse.dblclick(x, y, options?: { button? })`.
  #[qjs(rename = "dblclick")]
  pub async fn dblclick<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    x: f64,
    y: f64,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let o = parse_click_options(&ctx, options)?;
    let opts = ferridriver::page::MouseClickOptions {
      button: o.button,
      click_count: None,
      delay: o.delay,
    };
    self.page.mouse().dblclick(x, y, Some(opts)).await.into_js()
  }

  /// `mouse.down(options?: { button?, clickCount? })`.
  #[qjs(rename = "down")]
  pub async fn down<'js>(&self, ctx: rquickjs::Ctx<'js>, options: Opt<rquickjs::Value<'js>>) -> rquickjs::Result<()> {
    let o = parse_click_options(&ctx, options)?;
    let opts = ferridriver::page::MouseDownOptions {
      button: o.button,
      click_count: o.click_count,
    };
    self.page.mouse().down(Some(opts)).await.into_js()
  }

  /// `mouse.up(options?: { button?, clickCount? })`.
  #[qjs(rename = "up")]
  pub async fn up<'js>(&self, ctx: rquickjs::Ctx<'js>, options: Opt<rquickjs::Value<'js>>) -> rquickjs::Result<()> {
    let o = parse_click_options(&ctx, options)?;
    let opts = ferridriver::page::MouseUpOptions {
      button: o.button,
      click_count: o.click_count,
    };
    self.page.mouse().up(Some(opts)).await.into_js()
  }

  /// `mouse.wheel(deltaX, deltaY)`.
  #[qjs(rename = "wheel")]
  pub async fn wheel(&self, delta_x: f64, delta_y: f64) -> rquickjs::Result<()> {
    self.page.mouse().wheel(delta_x, delta_y).await.into_js()
  }
}
