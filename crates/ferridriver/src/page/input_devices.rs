//! Page `Keyboard`, `Mouse`, and `Touchscreen` interfaces and their option bags.

use super::Page;
use crate::error::Result;

// ── Keyboard ──────────────────────────────────────────────────────────────

/// Keyboard interface for a page. Mirrors Playwright's `page.keyboard`.
pub struct Keyboard<'a> {
  pub(super) page: &'a Page,
}

impl<'a> Keyboard<'a> {
  /// Dispatch a keyDown event. The key is held until `up()` is called.
  ///
  /// A held modifier (`"Shift"`, `"Control"`, `"Alt"`, `"Meta"`, `"ControlOrMeta"`)
  /// applies to every later key and mouse event. With Shift held, a key
  /// named by its code (`"KeyA"`, `"Digit1"`) produces its shifted
  /// character; with any other modifier held, keys type no text. A
  /// second `down` of a held key is an auto-repeat.
  ///
  /// # Errors
  ///
  /// Returns an error for a key the US layout does not have, or if the
  /// key down dispatch fails.
  pub async fn down(&self, key: &str) -> Result<()> {
    self
      .page
      .traced_as(
        "Keyboard",
        "down",
        serde_json::json!({ "key": key }),
        self.page.key_down(key),
      )
      .await
  }

  /// Dispatch a keyUp event for a previously held key.
  ///
  /// # Errors
  ///
  /// Returns an error if the key up dispatch fails.
  pub async fn up(&self, key: &str) -> Result<()> {
    self
      .page
      .traced_as(
        "Keyboard",
        "up",
        serde_json::json!({ "key": key }),
        self.page.key_up(key),
      )
      .await
  }

  /// Press a key or key combination (e.g., "Enter", "Control+a", "Shift+ArrowDown").
  ///
  /// Shortcut for `down(key)` followed by `up(key)`. Supports `+` combinator for
  /// modifier combinations.
  ///
  /// # Errors
  ///
  /// Returns an error if the key press dispatch fails.
  #[track_caller]
  pub fn press(&self, key: &str) -> crate::action::Action<'a, KeyboardPressOptions, ()> {
    let page = self.page;
    let key = key.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { Self { page }.press_impl(&key, Some(opts)).await }))
  }

  /// Implementation of [`Self::press`].
  pub(crate) async fn press_impl(&self, key: &str, opts: Option<KeyboardPressOptions>) -> Result<()> {
    self
      .page
      .traced_as(
        "Keyboard",
        "press",
        serde_json::json!({ "key": key }),
        self.press_untraced(key, opts),
      )
      .await
  }

  async fn press_untraced(&self, key: &str, opts: Option<KeyboardPressOptions>) -> Result<()> {
    let delay = opts.and_then(|o| o.delay).unwrap_or(0);
    self.page.inner.press_key(key, delay).await
  }

  /// Type text character by character with full keyboard events.
  ///
  /// Sends `keydown`, `keypress`/`input`, and `keyup` events for each character
  /// in the text. For characters not representable as single key presses,
  /// falls back to `insert_text` for that character.
  ///
  /// When `named_keys` is set, `{Name}` / `{Mod+Key}` sequences are parsed out
  /// of the text and dispatched as key presses (same format as `press`); `{{`
  /// types a literal `{`. Mirrors Playwright `keyboard.type({ namedKeys: true })`.
  ///
  /// # Errors
  ///
  /// Returns an error if the typing dispatch fails.
  #[track_caller]
  pub fn r#type(&self, text: &str) -> crate::action::Action<'a, KeyboardTypeOptions, ()> {
    let page = self.page;
    let text = text.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { Self { page }.type_impl(&text, Some(opts)).await }))
  }

  /// Implementation of [`Self::r#type`].
  pub(crate) async fn type_impl(&self, text: &str, opts: Option<KeyboardTypeOptions>) -> Result<()> {
    self
      .page
      .traced_as(
        "Keyboard",
        "type",
        serde_json::json!({ "text": text }),
        self.type_untraced(text, opts),
      )
      .await
  }

  async fn type_untraced(&self, text: &str, opts: Option<KeyboardTypeOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let delay = opts.delay;
    let named_keys = opts.named_keys.unwrap_or(false);
    let mut first = true;
    for token in parse_named_keys(text, named_keys) {
      if let (false, Some(ms)) = (first, delay) {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
      }
      first = false;
      match token {
        TypeToken::Key(key) => self.page.press_key(&key).await?,
        TypeToken::Char(ch) => self.page.inner.type_char(ch).await?,
      }
    }
    Ok(())
  }

  /// Insert text directly without emitting keyboard events.
  ///
  /// Only dispatches an `input` event. Modifier keys do NOT affect `insert_text`.
  /// Useful for inserting characters not available on a US keyboard layout.
  ///
  /// # Errors
  ///
  /// Returns an error if the text insertion fails.
  pub async fn insert_text(&self, text: &str) -> Result<()> {
    self
      .page
      .traced_as(
        "Keyboard",
        "insertText",
        serde_json::json!({ "text": text }),
        self.page.inner.insert_text(text),
      )
      .await
  }
}

// ── Mouse ─────────────────────────────────────────────────────────────────

/// Mouse interface for a page. Mirrors Playwright's `page.mouse`.
pub struct Mouse<'a> {
  pub(super) page: &'a Page,
}

impl<'a> Mouse<'a> {
  /// Click at coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the click dispatch fails.
  #[track_caller]
  pub fn click(&self, x: f64, y: f64) -> crate::action::Action<'a, MouseClickOptions, ()> {
    let page = self.page;
    crate::action::Action::new(move |opts| Box::pin(async move { Self { page }.click_impl(x, y, Some(opts)).await }))
  }

  /// Implementation of [`Self::click`].
  pub(crate) async fn click_impl(&self, x: f64, y: f64, opts: Option<MouseClickOptions>) -> Result<()> {
    self
      .page
      .traced_as(
        "Mouse",
        "click",
        serde_json::json!({ "x": x, "y": y }),
        self.click_untraced(x, y, opts),
      )
      .await
  }

  async fn click_untraced(&self, x: f64, y: f64, opts: Option<MouseClickOptions>) -> Result<()> {
    let button = opts.as_ref().and_then(|o| o.button).unwrap_or_default().as_cdp();
    let count = opts.as_ref().and_then(|o| o.click_count).unwrap_or(1);
    match opts.as_ref().and_then(|o| o.delay) {
      Some(ms) => {
        self.page.move_mouse(x, y).await?;
        for click in 1..=count {
          self.page.mouse_down(x, y, button, click).await?;
          tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
          self.page.mouse_up(x, y, button, click).await?;
        }
        Ok(())
      },
      None => self.page.click_at_opts(x, y, button, count).await,
    }
  }

  /// Move mouse to coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse move dispatch fails.
  #[track_caller]
  pub fn r#move(&self, x: f64, y: f64) -> crate::action::Action<'a, MouseMoveOptions, ()> {
    let page = self.page;
    crate::action::Action::new(move |opts: MouseMoveOptions| {
      Box::pin(async move { Self { page }.move_impl(x, y, opts.steps).await })
    })
  }

  pub(crate) async fn move_impl(&self, x: f64, y: f64, steps: Option<u32>) -> Result<()> {
    self
      .page
      .traced_as(
        "Mouse",
        "move",
        serde_json::json!({ "x": x, "y": y }),
        self.move_untraced(x, y, steps),
      )
      .await
  }

  async fn move_untraced(&self, x: f64, y: f64, steps: Option<u32>) -> Result<()> {
    match steps {
      Some(step_count) => {
        let (from_x, from_y) = *self
          .page
          .mouse_position
          .lock()
          .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))?;
        self.page.move_mouse_smooth(from_x, from_y, x, y, step_count).await
      },
      None => self.page.move_mouse(x, y).await,
    }
  }

  /// Double-click at coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the click dispatch fails.
  #[track_caller]
  pub fn dblclick(&self, x: f64, y: f64) -> crate::action::Action<'a, MouseClickOptions, ()> {
    let page = self.page;
    crate::action::Action::new(move |opts| Box::pin(async move { Self { page }.dblclick_impl(x, y, Some(opts)).await }))
  }

  /// Implementation of [`Self::dblclick`].
  pub(crate) async fn dblclick_impl(&self, x: f64, y: f64, opts: Option<MouseClickOptions>) -> Result<()> {
    self
      .page
      .traced_as(
        "Mouse",
        "dblclick",
        serde_json::json!({ "x": x, "y": y }),
        self.dblclick_untraced(x, y, opts),
      )
      .await
  }

  async fn dblclick_untraced(&self, x: f64, y: f64, opts: Option<MouseClickOptions>) -> Result<()> {
    let button = opts.as_ref().and_then(|o| o.button).unwrap_or_default().as_cdp();
    self.page.move_mouse(x, y).await?;
    self.page.mouse_down(x, y, button, 1).await?;
    self.page.mouse_up(x, y, button, 1).await?;
    if let Some(ms) = opts.as_ref().and_then(|o| o.delay) {
      tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
    self.page.mouse_down(x, y, button, 2).await?;
    self.page.mouse_up(x, y, button, 2).await?;
    Ok(())
  }

  /// Press mouse button down at the current cursor position.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse down dispatch fails.
  #[track_caller]
  pub fn down(&self) -> crate::action::Action<'a, MouseDownOptions, ()> {
    let page = self.page;
    crate::action::Action::new(move |opts| Box::pin(async move { Self { page }.down_impl(Some(opts)).await }))
  }

  /// Implementation of [`Self::down`].
  pub(crate) async fn down_impl(&self, opts: Option<MouseDownOptions>) -> Result<()> {
    self
      .page
      .traced_as("Mouse", "down", serde_json::json!({}), self.down_untraced(opts))
      .await
  }

  async fn down_untraced(&self, opts: Option<MouseDownOptions>) -> Result<()> {
    let button = opts.as_ref().and_then(|o| o.button).unwrap_or_default().as_cdp();
    let (x, y) = *self
      .page
      .mouse_position
      .lock()
      .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))?;
    let click_count = opts.as_ref().and_then(|o| o.click_count).unwrap_or(1);
    self.page.mouse_down(x, y, button, click_count).await
  }

  /// Release mouse button at the current cursor position.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse up dispatch fails.
  #[track_caller]
  pub fn up(&self) -> crate::action::Action<'a, MouseUpOptions, ()> {
    let page = self.page;
    crate::action::Action::new(move |opts| Box::pin(async move { Self { page }.up_impl(Some(opts)).await }))
  }

  /// Implementation of [`Self::up`].
  pub(crate) async fn up_impl(&self, opts: Option<MouseUpOptions>) -> Result<()> {
    self
      .page
      .traced_as("Mouse", "up", serde_json::json!({}), self.up_untraced(opts))
      .await
  }

  async fn up_untraced(&self, opts: Option<MouseUpOptions>) -> Result<()> {
    let button = opts.as_ref().and_then(|o| o.button).unwrap_or_default().as_cdp();
    let (x, y) = *self
      .page
      .mouse_position
      .lock()
      .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))?;
    let click_count = opts.as_ref().and_then(|o| o.click_count).unwrap_or(1);
    self.page.mouse_up(x, y, button, click_count).await
  }

  /// Scroll via mouse wheel.
  ///
  /// # Errors
  ///
  /// Returns an error if the wheel event dispatch fails.
  pub async fn wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    self
      .page
      .traced_as(
        "Mouse",
        "wheel",
        serde_json::json!({ "deltaX": delta_x, "deltaY": delta_y }),
        self.page.mouse_wheel(delta_x, delta_y),
      )
      .await
  }
}

/// Options for `Mouse.click()`.
#[derive(Debug, Clone, Default)]
pub struct MouseClickOptions {
  /// Mouse button. Default: [`crate::options::MouseButton::Left`].
  pub button: Option<crate::options::MouseButton>,
  /// Click count (1=single, 2=double, 3=triple)
  pub click_count: Option<u32>,
  /// Milliseconds to wait between `mousedown` and `mouseup`
  /// (Playwright `delay`).
  pub delay: Option<u64>,
}

/// Options for `Keyboard.press()` — Playwright `{ delay? }`.
#[derive(Debug, Clone, Default)]
pub struct KeyboardPressOptions {
  /// Milliseconds to wait between `keydown` and `keyup`.
  pub delay: Option<u64>,
}

/// Options for `Keyboard.type()` — Playwright `{ delay?, namedKeys? }`.
#[derive(Debug, Clone, Default)]
pub struct KeyboardTypeOptions {
  /// Milliseconds to wait between key presses.
  pub delay: Option<u64>,
  /// When true, `{Name}` / `{Mod+Key}` sequences in the text are treated as key
  /// presses (same format as `Keyboard::press`). `{{` types a literal `{`.
  pub named_keys: Option<bool>,
}

/// A single token produced by parsing `keyboard.type` text with `namedKeys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TypeToken {
  /// A key name or combination to press (e.g. `Enter`, `Control+a`).
  Key(String),
  /// A literal character to type.
  Char(char),
}

/// Tokenize `keyboard.type` text. When `named_keys` is false every character is
/// a `Char`. When true, `{Name}` becomes a `Key`, `{{` becomes a literal `{`,
/// and an unterminated `{` is treated as a literal `{`.
///
/// Mirrors Playwright `packages/playwright-core/src/server/input.ts::parseNamedKeys`.
pub(super) fn parse_named_keys(text: &str, named_keys: bool) -> Vec<TypeToken> {
  if !named_keys {
    return text.chars().map(TypeToken::Char).collect();
  }
  let chars: Vec<char> = text.chars().collect();
  let mut result = Vec::new();
  let mut i = 0;
  while i < chars.len() {
    if chars[i] == '{' {
      if i + 1 < chars.len() && chars[i + 1] == '{' {
        result.push(TypeToken::Char('{'));
        i += 2;
      } else if let Some(offset) = chars[i + 1..].iter().position(|&c| c == '}') {
        let end = i + 1 + offset;
        let name: String = chars[i + 1..end].iter().collect();
        result.push(TypeToken::Key(name));
        i = end + 1;
      } else {
        result.push(TypeToken::Char('{'));
        i += 1;
      }
    } else {
      result.push(TypeToken::Char(chars[i]));
      i += 1;
    }
  }
  result
}

/// Options for `Mouse.move()` — Playwright's `{ steps? }`.
#[derive(Debug, Clone, Default)]
pub struct MouseMoveOptions {
  /// Intermediate `mousemove` samples between the current position and
  /// the destination. Default: `1` (single move at the destination).
  pub steps: Option<u32>,
}

/// Options for `Mouse.down()`.
#[derive(Debug, Clone, Default)]
pub struct MouseDownOptions {
  /// Mouse button. Default: [`crate::options::MouseButton::Left`].
  pub button: Option<crate::options::MouseButton>,
  /// Click count for the event
  pub click_count: Option<u32>,
}

/// Options for `Mouse.up()`.
#[derive(Debug, Clone, Default)]
pub struct MouseUpOptions {
  /// Mouse button. Default: [`crate::options::MouseButton::Left`].
  pub button: Option<crate::options::MouseButton>,
  /// Click count for the event
  pub click_count: Option<u32>,
}

// ── Touchscreen ───────────────────────────────────────────────────────────

/// Touchscreen interface for a page. Mirrors Playwright's `page.touchscreen`.
pub struct Touchscreen<'a> {
  pub(super) page: &'a Page,
}

impl Touchscreen<'_> {
  /// Tap at coordinates via the backend's native touch input — CDP
  /// `Input.dispatchTouchEvent`, `WebKit` `Input.dispatchTapEvent`,
  /// `BiDi` an `input.performActions` `touch` pointer sequence — producing a
  /// trusted `touchstart`/`touchend` pair exactly like
  /// `locator.tap()`'s dispatch (Playwright's `RawTouchscreen.tap`).
  ///
  /// # Errors
  ///
  /// Returns an error if the backend dispatch fails.
  pub async fn tap(&self, x: f64, y: f64) -> Result<()> {
    self
      .page
      .traced_as(
        "Touchscreen",
        "tap",
        serde_json::json!({ "x": x, "y": y }),
        self.page.inner.tap_at_with(x, y),
      )
      .await
  }
}
