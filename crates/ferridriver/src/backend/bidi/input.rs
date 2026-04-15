//! Input action builders for `WebDriver` `BiDi` `input.performActions`.
//!
//! Provides ergonomic builders for composing mouse, keyboard, and wheel actions
//! into the `BiDi` action chain format. All convenience methods return ready-to-send
//! `serde_json::Value` params.

use serde_json::json;

/// Convert f64 pixel coordinate to a JSON integer for the `BiDi` protocol.
/// Rounds to nearest integer and produces a `serde_json::Value::Number` without
/// any `as` cast between float and integer types.
fn coord(v: f64) -> serde_json::Value {
  let rounded = v.round();
  if rounded.is_finite() {
    serde_json::from_str(&format!("{rounded:.0}")).unwrap_or_else(|_| serde_json::Value::from(0))
  } else {
    serde_json::Value::from(0)
  }
}

// ── Key Mapping ────────────────────────────────────────────────────────────

/// Map Playwright-style key names to `WebDriver` Unicode PUA values.
/// Single characters pass through unchanged.
#[must_use]
pub fn key_to_bidi(key: &str) -> String {
  match key {
    // Navigation keys
    "Enter" | "Return" | "NumpadEnter" => "\u{E006}".into(),
    "Tab" => "\u{E004}".into(),
    "Backspace" => "\u{E003}".into(),
    "Delete" => "\u{E017}".into(),
    "Escape" => "\u{E00C}".into(),
    "Space" | " " => "\u{E00D}".into(),

    // Arrow keys
    "ArrowUp" | "Up" => "\u{E013}".into(),
    "ArrowDown" | "Down" => "\u{E015}".into(),
    "ArrowLeft" | "Left" => "\u{E012}".into(),
    "ArrowRight" | "Right" => "\u{E014}".into(),

    // Page keys
    "Home" => "\u{E011}".into(),
    "End" => "\u{E010}".into(),
    "PageUp" => "\u{E00E}".into(),
    "PageDown" => "\u{E00F}".into(),
    "Insert" => "\u{E016}".into(),

    // Modifier keys
    "Shift" | "ShiftLeft" | "ShiftRight" => "\u{E008}".into(),
    "Control" | "ControlLeft" | "ControlRight" => "\u{E009}".into(),
    "Alt" | "AltLeft" | "AltRight" => "\u{E00A}".into(),
    "Meta" | "MetaLeft" | "MetaRight" => "\u{E03D}".into(),

    // Function keys
    "F1" => "\u{E031}".into(),
    "F2" => "\u{E032}".into(),
    "F3" => "\u{E033}".into(),
    "F4" => "\u{E034}".into(),
    "F5" => "\u{E035}".into(),
    "F6" => "\u{E036}".into(),
    "F7" => "\u{E037}".into(),
    "F8" => "\u{E038}".into(),
    "F9" => "\u{E039}".into(),
    "F10" => "\u{E03A}".into(),
    "F11" => "\u{E03B}".into(),
    "F12" => "\u{E03C}".into(),

    // Numpad
    "Numpad0" => "\u{E01A}".into(),
    "Numpad1" => "\u{E01B}".into(),
    "Numpad2" => "\u{E01C}".into(),
    "Numpad3" => "\u{E01D}".into(),
    "Numpad4" => "\u{E01E}".into(),
    "Numpad5" => "\u{E01F}".into(),
    "Numpad6" => "\u{E020}".into(),
    "Numpad7" => "\u{E021}".into(),
    "Numpad8" => "\u{E022}".into(),
    "Numpad9" => "\u{E023}".into(),
    "NumpadMultiply" => "\u{E024}".into(),
    "NumpadAdd" => "\u{E025}".into(),
    "NumpadSubtract" => "\u{E027}".into(),
    "NumpadDecimal" => "\u{E028}".into(),
    "NumpadDivide" => "\u{E029}".into(),

    // Pass through single characters and unknowns
    other => other.into(),
  }
}

/// Map Playwright button name to `BiDi` button number.
#[must_use]
pub fn button_name_to_id(name: &str) -> u32 {
  match name {
    "middle" => 1,
    "right" => 2,
    // "left" and anything unrecognized default to button 0 (primary).
    _ => 0,
  }
}

// ── Action Builders ────────────────────────────────────────────────────────

/// Build a click action at coordinates.
#[must_use]
pub fn click(context: &str, x: f64, y: f64) -> serde_json::Value {
  json!({
    "context": context,
    "actions": [{
      "type": "pointer",
      "id": "mouse",
      "parameters": {"pointerType": "mouse"},
      "actions": [
        {"type": "pointerMove", "x": coord(x), "y": coord(y), "duration": 0},
        {"type": "pointerDown", "button": 0},
        {"type": "pointerUp", "button": 0}
      ]
    }]
  })
}

/// Build a click with specific button and click count.
#[must_use]
pub fn click_button(context: &str, x: f64, y: f64, button: u32, count: u32) -> serde_json::Value {
  let mut actions = vec![json!({"type": "pointerMove", "x": coord(x), "y": coord(y), "duration": 0})];
  for _ in 0..count {
    actions.push(json!({"type": "pointerDown", "button": button}));
    actions.push(json!({"type": "pointerUp", "button": button}));
  }
  json!({
    "context": context,
    "actions": [{
      "type": "pointer",
      "id": "mouse",
      "parameters": {"pointerType": "mouse"},
      "actions": actions
    }]
  })
}

/// Build a pointer move action.
#[must_use]
pub fn pointer_move(context: &str, x: f64, y: f64) -> serde_json::Value {
  json!({
    "context": context,
    "actions": [{
      "type": "pointer",
      "id": "mouse",
      "parameters": {"pointerType": "mouse"},
      "actions": [
        {"type": "pointerMove", "x": coord(x), "y": coord(y), "duration": 0}
      ]
    }]
  })
}

/// Build a smooth mouse move with multiple interpolated steps.
#[must_use]
pub fn pointer_move_smooth(
  context: &str,
  from_x: f64,
  from_y: f64,
  to_x: f64,
  to_y: f64,
  steps: u32,
) -> serde_json::Value {
  let mut actions = Vec::with_capacity(steps as usize + 1);
  actions.push(json!({"type": "pointerMove", "x": coord(from_x), "y": coord(from_y), "duration": 0}));
  for i in 1..=steps {
    let t = f64::from(i) / f64::from(steps);
    let x = from_x + (to_x - from_x) * t;
    let y = from_y + (to_y - from_y) * t;
    let duration = if steps > 1 { 100 / steps } else { 0 };
    actions.push(json!({"type": "pointerMove", "x": coord(x), "y": coord(y), "duration": duration}));
  }
  json!({
    "context": context,
    "actions": [{
      "type": "pointer",
      "id": "mouse",
      "parameters": {"pointerType": "mouse"},
      "actions": actions
    }]
  })
}

/// Build a mouse down action.
#[must_use]
pub fn mouse_down(context: &str, x: f64, y: f64, button: u32) -> serde_json::Value {
  json!({
    "context": context,
    "actions": [{
      "type": "pointer",
      "id": "mouse",
      "parameters": {"pointerType": "mouse"},
      "actions": [
        {"type": "pointerMove", "x": coord(x), "y": coord(y), "duration": 0},
        {"type": "pointerDown", "button": button}
      ]
    }]
  })
}

/// Build a mouse up action.
#[must_use]
pub fn mouse_up(context: &str, x: f64, y: f64, button: u32) -> serde_json::Value {
  json!({
    "context": context,
    "actions": [{
      "type": "pointer",
      "id": "mouse",
      "parameters": {"pointerType": "mouse"},
      "actions": [
        {"type": "pointerMove", "x": coord(x), "y": coord(y), "duration": 0},
        {"type": "pointerUp", "button": button}
      ]
    }]
  })
}

/// Build a click-and-drag action.
#[must_use]
pub fn click_and_drag(context: &str, from: (f64, f64), to: (f64, f64)) -> serde_json::Value {
  json!({
    "context": context,
    "actions": [{
      "type": "pointer",
      "id": "mouse",
      "parameters": {"pointerType": "mouse"},
      "actions": [
        {"type": "pointerMove", "x": coord(from.0), "y": coord(from.1), "duration": 0},
        {"type": "pointerDown", "button": 0},
        {"type": "pointerMove", "x": coord(to.0), "y": coord(to.1), "duration": 250},
        {"type": "pointerUp", "button": 0}
      ]
    }]
  })
}

/// Build a wheel scroll action.
#[must_use]
pub fn wheel_scroll(context: &str, delta_x: f64, delta_y: f64) -> serde_json::Value {
  json!({
    "context": context,
    "actions": [{
      "type": "wheel",
      "id": "wheel",
      "actions": [
        {"type": "scroll", "x": 0, "y": 0, "deltaX": coord(delta_x), "deltaY": coord(delta_y), "duration": 0}
      ]
    }]
  })
}

/// Build a type-text action (sequence of keyDown+keyUp for each character).
#[must_use]
pub fn type_text(context: &str, text: &str) -> serde_json::Value {
  let mut actions = Vec::with_capacity(text.len() * 2);
  for ch in text.chars() {
    let key = ch.to_string();
    actions.push(json!({"type": "keyDown", "value": key}));
    actions.push(json!({"type": "keyUp", "value": key}));
  }
  json!({
    "context": context,
    "actions": [{
      "type": "key",
      "id": "keyboard",
      "actions": actions
    }]
  })
}

/// Build a single keyDown action (does NOT release the key).
#[must_use]
pub fn key_down(context: &str, key: &str) -> serde_json::Value {
  let bidi_key = key_to_bidi(key);
  json!({
    "context": context,
    "actions": [{
      "type": "key",
      "id": "keyboard",
      "actions": [
        {"type": "keyDown", "value": bidi_key}
      ]
    }]
  })
}

/// Build a single keyUp action.
#[must_use]
pub fn key_up(context: &str, key: &str) -> serde_json::Value {
  let bidi_key = key_to_bidi(key);
  json!({
    "context": context,
    "actions": [{
      "type": "key",
      "id": "keyboard",
      "actions": [
        {"type": "keyUp", "value": bidi_key}
      ]
    }]
  })
}

/// Build a key press action (keyDown + keyUp).
#[must_use]
pub fn press_key(context: &str, key: &str) -> serde_json::Value {
  // Handle modifier+key combos like "Control+a", "Shift+Enter"
  let parts: Vec<&str> = key.split('+').collect();
  if parts.len() > 1 {
    return press_key_combo(context, &parts);
  }

  let bidi_key = key_to_bidi(key);
  json!({
    "context": context,
    "actions": [{
      "type": "key",
      "id": "keyboard",
      "actions": [
        {"type": "keyDown", "value": bidi_key},
        {"type": "keyUp", "value": bidi_key}
      ]
    }]
  })
}

/// Build a key combo action (e.g. Ctrl+A = keyDown Ctrl, keyDown a, keyUp a, keyUp Ctrl).
fn press_key_combo(context: &str, parts: &[&str]) -> serde_json::Value {
  let mut actions = Vec::new();

  // Press modifiers first
  let modifiers = &parts[..parts.len() - 1];
  let key = parts[parts.len() - 1];

  for modifier in modifiers {
    let bidi_mod = key_to_bidi(modifier);
    actions.push(json!({"type": "keyDown", "value": bidi_mod}));
  }

  // Press and release the main key
  let bidi_key = key_to_bidi(key);
  actions.push(json!({"type": "keyDown", "value": bidi_key}));
  actions.push(json!({"type": "keyUp", "value": bidi_key}));

  // Release modifiers in reverse order
  for modifier in modifiers.iter().rev() {
    let bidi_mod = key_to_bidi(modifier);
    actions.push(json!({"type": "keyUp", "value": bidi_mod}));
  }

  json!({
    "context": context,
    "actions": [{
      "type": "key",
      "id": "keyboard",
      "actions": actions
    }]
  })
}
