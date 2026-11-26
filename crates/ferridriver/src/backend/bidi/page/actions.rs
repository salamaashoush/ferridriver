//! `BidiPage` keyboard and pointer input.

use super::{BidiPage, FerriError, INSERT_TEXT, KeyStroke, Result, input, json};

impl BidiPage {
  // ── Input ───────────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when the `input.performActions` command fails.
  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    self
      .cmd("input.performActions", input::click(&self.context_id, x, y))
      .await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `input.performActions` command fails.
  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    let btn = input::button_name_to_id(button);
    self
      .cmd(
        "input.performActions",
        input::click_button(&self.context_id, x, y, btn, click_count),
      )
      .await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `input.performActions` command fails.
  pub async fn click_at_with(&self, x: f64, y: f64, args: &crate::backend::BackendClickArgs) -> Result<()> {
    self
      .cmd(
        "input.performActions",
        input::click_with_args(&self.context_id, x, y, args),
      )
      .await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `input.performActions` command fails.
  pub async fn hover_at_with(&self, x: f64, y: f64, args: &crate::backend::BackendHoverArgs) -> Result<()> {
    self
      .cmd(
        "input.performActions",
        input::hover_with_args(&self.context_id, x, y, *args),
      )
      .await?;
    Ok(())
  }

  /// Native tap via `input.performActions` with a `touch` pointer
  /// source (`pointerType: "touch"` is in the `WebDriver` `BiDi`
  /// `Input.PointerType` union; Playwright's own `BiDi` backend leaves
  /// `Touchscreen.tap` an empty stub, but the wire supports it).
  /// Firefox dispatches trusted `pointerdown`/`pointerup` with
  /// `pointerType === 'touch'` plus real `touchstart`/`touchend` and
  /// the compatibility mouse sequence.
  ///
  /// Held modifiers are the one gap: the keyboard holds them on the
  /// shared key source (which Firefox applies to mouse-pointer events),
  /// but Firefox does not propagate key-source modifier state onto
  /// touch-pointer events — `e.shiftKey` stays false on every event of
  /// the tap sequence (verified empirically; the `BiDi` wire has no
  /// per-action modifier field to express it differently). Surface that
  /// as a typed error instead of silently dropping the modifiers.
  ///
  /// # Errors
  ///
  /// Returns `FerriError::Unsupported` when a keyboard modifier is held, and an error when the
  /// `input.performActions` command fails.
  pub async fn tap_at_with(&self, x: f64, y: f64) -> Result<()> {
    if self.keyboard.modifiers() != 0 {
      return Err(FerriError::unsupported(
        "tap with modifiers is not available on the BiDi backend — Firefox does not apply \
         key-source modifier state to touch-pointer events, so the requested modifiers would be \
         silently dropped (plain tap works; WebDriver BiDi has no per-action modifier field).",
      ));
    }
    self
      .cmd("input.performActions", input::tap(&self.context_id, x, y))
      .await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `input.performActions` command fails.
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    self
      .cmd("input.performActions", input::pointer_move(&self.context_id, x, y))
      .await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `input.performActions` command fails.
  pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<()> {
    self
      .cmd(
        "input.performActions",
        input::pointer_move_smooth(&self.context_id, from_x, from_y, to_x, to_y, steps),
      )
      .await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `input.performActions` command fails.
  pub async fn mouse_wheel(&self, x: f64, y: f64, delta_x: f64, delta_y: f64) -> Result<()> {
    self
      .cmd(
        "input.performActions",
        input::wheel_scroll(&self.context_id, x, y, delta_x, delta_y),
      )
      .await?;
    Ok(())
  }

  /// `WebDriver` `BiDi`'s `pointerDown` has no click count, so `_click_count`
  /// cannot be forwarded (Playwright's `bidiInput.ts` drops it too). Firefox
  /// counts clicks only within one action chain and resets the count at the
  /// end of each (`Actions.sys.mjs`), so presses sent one call at a time
  /// always report a click count of 1.
  ///
  /// # Errors
  ///
  /// Returns an error when the `input.performActions` command fails.
  pub async fn mouse_down(&self, x: f64, y: f64, button: &str, _click_count: u32) -> Result<()> {
    let btn = input::button_name_to_id(button);
    self
      .cmd("input.performActions", input::mouse_down(&self.context_id, x, y, btn))
      .await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `input.performActions` command fails.
  pub async fn mouse_up(&self, x: f64, y: f64, button: &str, _click_count: u32) -> Result<()> {
    let btn = input::button_name_to_id(button);
    self
      .cmd("input.performActions", input::mouse_up(&self.context_id, x, y, btn))
      .await?;
    Ok(())
  }

  /// Insert `text` at the focus from a page script, as one `input` event
  /// and no key events: Firefox's input actions can only press keys. The
  /// script reports a focused frame element by its window, whose context
  /// the text is then inserted in, until the focus is found.
  ///
  /// # Errors
  ///
  /// Returns an error when a `script.callFunction` command fails, and `FerriError::Evaluation` when
  /// the insertion script throws in any frame it descends into.
  pub async fn insert_text(&self, text: &str) -> Result<()> {
    let mut context = self.context_id.to_string();
    loop {
      let result = self
        .cmd(
          "script.callFunction",
          json!({
            "functionDeclaration": INSERT_TEXT,
            "target": {"context": &context},
            "arguments": [{"type": "string", "value": text}],
            "awaitPromise": false,
            "resultOwnership": "none",
          }),
        )
        .await?;
      if result.get("type").and_then(serde_json::Value::as_str) == Some("exception") {
        let reason = result
          .pointer("/exceptionDetails/text")
          .and_then(serde_json::Value::as_str)
          .unwrap_or("inserting text threw");
        return Err(FerriError::evaluation(reason.to_string()));
      }
      match result
        .pointer("/result/value/context")
        .and_then(serde_json::Value::as_str)
      {
        Some(frame) if result.pointer("/result/type").and_then(serde_json::Value::as_str) == Some("window") => {
          context = frame.to_string();
        },
        _ => return Ok(()),
      }
    }
  }

  /// `BiDi` presses the key by name; the browser keeps the held keys on
  /// the shared key source and applies them itself.
  pub(crate) async fn dispatch_key_down(&self, name: &str, _stroke: &KeyStroke) -> Result<()> {
    self
      .cmd("input.performActions", input::key_down(&self.context_id, name))
      .await?;
    Ok(())
  }

  pub(crate) async fn dispatch_key_up(&self, name: &str, _stroke: &KeyStroke) -> Result<()> {
    self
      .cmd("input.performActions", input::key_up(&self.context_id, name))
      .await?;
    Ok(())
  }
}
