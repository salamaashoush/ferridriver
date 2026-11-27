//! Input-device classes (`Keyboard`, `Mouse`, `Touchscreen`) and related
//! page snapshot / screencast payload structs.

use std::sync::{Arc, Mutex};

use napi::Result;
use napi_derive::napi;

#[napi(object)]
pub struct MouseClickOptions {
  pub button: Option<String>,
  #[napi(js_name = "clickCount")]
  pub click_count: Option<i32>,
  pub delay: Option<f64>,
}

/// Playwright `{ delay? }` for `keyboard.press`.
#[napi(object)]
pub struct KeyDelayOptions {
  pub delay: Option<f64>,
}

/// Playwright `{ delay?, namedKeys? }` for `keyboard.type`.
#[napi(object)]
pub struct KeyTypeOptions {
  pub delay: Option<f64>,
  pub named_keys: Option<bool>,
}

#[napi]
pub struct Keyboard {
  pub(super) page: Arc<ferridriver::Page>,
}

#[napi]
impl Keyboard {
  #[napi]
  pub async fn down(&self, key: String) -> Result<()> {
    self.page.keyboard().down(&key).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn up(&self, key: String) -> Result<()> {
    self.page.keyboard().up(&key).await.map_err(crate::error::to_napi)
  }

  /// Playwright: `keyboard.press(key, options?: { delay? })`.
  #[napi]
  pub async fn press(&self, key: String, options: Option<KeyDelayOptions>) -> Result<()> {
    let opts = options
      .and_then(|o| o.delay)
      .map(|d| ferridriver::page::KeyboardPressOptions {
        delay: Some(crate::types::f64_to_u64(d)),
      });
    self
      .page
      .keyboard()
      .press(&key)
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Playwright: `keyboard.type(text, options?: { delay?, namedKeys? })`.
  #[napi(js_name = "type")]
  pub async fn type_text(&self, text: String, options: Option<KeyTypeOptions>) -> Result<()> {
    let opts = options.map(|o| ferridriver::page::KeyboardTypeOptions {
      delay: o.delay.map(crate::types::f64_to_u64),
      named_keys: o.named_keys,
    });
    self
      .page
      .keyboard()
      .r#type(&text)
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi(js_name = "insertText")]
  pub async fn insert_text(&self, text: String) -> Result<()> {
    self
      .page
      .keyboard()
      .insert_text(&text)
      .await
      .map_err(crate::error::to_napi)
  }
}

#[napi]
pub struct Mouse {
  pub(super) page: Arc<ferridriver::Page>,
  pub(super) position: Arc<Mutex<(f64, f64)>>,
}

#[napi]
impl Mouse {
  #[napi]
  pub async fn click(&self, x: f64, y: f64, options: Option<MouseClickOptions>) -> Result<()> {
    let opts = ferridriver::page::MouseClickOptions {
      button: options
        .as_ref()
        .and_then(|o| o.button.as_deref())
        .map(ferridriver::options::MouseButton::from),
      click_count: options
        .as_ref()
        .and_then(|o| o.click_count)
        .and_then(|n| u32::try_from(n).ok()),
      delay: options.as_ref().and_then(|o| o.delay).map(crate::types::f64_to_u64),
    };
    self
      .page
      .mouse()
      .click(x, y)
      .options(opts)
      .await
      .map_err(crate::error::to_napi)?;
    *self.position.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (x, y);
    Ok(())
  }

  #[napi(js_name = "move")]
  pub async fn move_to(&self, x: f64, y: f64, steps: Option<i32>) -> Result<()> {
    let step_count = steps
      .map(|s| u32::try_from(s).map_err(|_| napi::Error::from_reason("steps must be non-negative")))
      .transpose()?;
    let mut action = self.page.mouse().r#move(x, y);
    if let Some(steps) = step_count {
      action = action.steps(steps);
    }
    action.await.map_err(crate::error::to_napi)?;
    *self.position.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (x, y);
    Ok(())
  }

  #[napi]
  pub async fn dblclick(&self, x: f64, y: f64, options: Option<MouseClickOptions>) -> Result<()> {
    let opts = ferridriver::page::MouseClickOptions {
      button: options
        .as_ref()
        .and_then(|o| o.button.as_deref())
        .map(ferridriver::options::MouseButton::from),
      click_count: None,
      delay: options.as_ref().and_then(|o| o.delay).map(crate::types::f64_to_u64),
    };
    self
      .page
      .mouse()
      .dblclick(x, y)
      .options(opts)
      .await
      .map_err(crate::error::to_napi)?;
    *self.position.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (x, y);
    Ok(())
  }

  /// Playwright: `mouse.down(options?: { button?, clickCount? })`.
  #[napi]
  pub async fn down(&self, options: Option<MouseClickOptions>) -> Result<()> {
    let opts = ferridriver::page::MouseDownOptions {
      button: options
        .as_ref()
        .and_then(|o| o.button.as_deref())
        .map(ferridriver::options::MouseButton::from),
      click_count: options
        .as_ref()
        .and_then(|o| o.click_count)
        .and_then(|n| u32::try_from(n).ok()),
    };
    self
      .page
      .mouse()
      .down()
      .options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Playwright: `mouse.up(options?: { button?, clickCount? })`.
  #[napi]
  pub async fn up(&self, options: Option<MouseClickOptions>) -> Result<()> {
    let opts = ferridriver::page::MouseUpOptions {
      button: options
        .as_ref()
        .and_then(|o| o.button.as_deref())
        .map(ferridriver::options::MouseButton::from),
      click_count: options
        .as_ref()
        .and_then(|o| o.click_count)
        .and_then(|n| u32::try_from(n).ok()),
    };
    self
      .page
      .mouse()
      .up()
      .options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    self
      .page
      .mouse()
      .wheel(delta_x, delta_y)
      .await
      .map_err(crate::error::to_napi)
  }
}

/// Result of `page.snapshotForAI(options?)`.
#[napi(object)]
pub struct SnapshotForAiResult {
  pub full: String,
  pub incremental: Option<String>,
  pub ref_map: crate::types::HashMap<String, i64>,
}

/// Single screencast frame surfaced through the
/// `page.startScreencast` callback.
#[napi(object)]
pub struct ScreencastFrame {
  pub frame: napi::bindgen_prelude::Buffer,
  pub timestamp: f64,
}

/// Playwright `Touchscreen`. Construct via `page.touchscreen`.
#[napi]
pub struct Touchscreen {
  pub(super) page: Arc<ferridriver::Page>,
}

#[napi]
impl Touchscreen {
  /// Playwright: `touchscreen.tap(x, y)`. Dispatches a real
  /// `TouchEvent` where the `Touch` constructor is usable; falls back
  /// to a synthesized `PointerEvent` + click where it is not (`WebKit` on
  /// Linux and macOS throws on `new Touch(...)`).
  #[napi]
  pub async fn tap(&self, x: f64, y: f64) -> Result<()> {
    self.page.touchscreen().tap(x, y).await.map_err(crate::error::to_napi)
  }
}
