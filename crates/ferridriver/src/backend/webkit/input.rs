//! Native input dispatch for the `WebKit` backend.
//!
//! Per `wkInput.ts`: mouse / key / wheel / tap events go through
//! `Input.dispatch*Event` on the **page-proxy** session;
//! `Page.insertText` and `Page.updateScrollingState` go through the
//! target session.

use serde_json::{Value, json};

use ferridriver_types::keyboard::{KeyStroke, mac_editing_selectors};

use super::connection::ConnectionError;
use super::page::WebKitPage;
use crate::backend::{BackendClickArgs, BackendHoverArgs};
use crate::error::{FerriError, Result};
use crate::options::MouseButton;

fn err(e: ConnectionError) -> FerriError {
  e.into()
}

/// The page keyboard's held modifiers as a `WebKit` mask.
fn held(page: &WebKitPage) -> u32 {
  cdp_to_wk_mask(page.keyboard().modifiers())
}

fn button_mask(button: MouseButton) -> u32 {
  match button {
    MouseButton::Left => 1,
    MouseButton::Right => 2,
    MouseButton::Middle => 4,
  }
}

/// Parameters for [`mouse_event`] — keeps the call sites readable
/// without spreading 8 positional arguments at every dispatch.
struct MouseEvent<'a> {
  ty: &'a str,
  button: &'a str,
  buttons: u32,
  x: f64,
  y: f64,
  click_count: u32,
  modifiers: u32,
}

/// Dispatch one `Input.dispatchMouseEvent` on the page-proxy session.
async fn mouse_event(page: &WebKitPage, ev: MouseEvent<'_>) -> Result<()> {
  let mut params = json!({
    "type": ev.ty, "button": ev.button, "buttons": ev.buttons,
    "x": ev.x, "y": ev.y, "modifiers": ev.modifiers,
  });
  if ev.click_count > 0 {
    params["clickCount"] = json!(ev.click_count);
  }
  page
    .proxy_session()
    .send("Input.dispatchMouseEvent", params)
    .await
    .map_err(err)?;
  Ok(())
}

pub async fn click(page: &WebKitPage, x: f64, y: f64, args: &BackendClickArgs) -> Result<()> {
  let mods = held(page);
  let btn = args.button.as_cdp();
  let bmask = button_mask(args.button);
  mouse_event(
    page,
    MouseEvent {
      ty: "move",
      button: "none",
      buttons: 0,
      x,
      y,
      click_count: 0,
      modifiers: mods,
    },
  )
  .await?;
  for n in 1..=args.click_count.max(1) {
    mouse_event(
      page,
      MouseEvent {
        ty: "down",
        button: btn,
        buttons: bmask,
        x,
        y,
        click_count: n,
        modifiers: mods,
      },
    )
    .await?;
    if args.delay_ms > 0 {
      tokio::time::sleep(std::time::Duration::from_millis(args.delay_ms)).await;
    }
    mouse_event(
      page,
      MouseEvent {
        ty: "up",
        button: btn,
        buttons: 0,
        x,
        y,
        click_count: n,
        modifiers: mods,
      },
    )
    .await?;
  }
  Ok(())
}

pub async fn hover(page: &WebKitPage, x: f64, y: f64, args: &BackendHoverArgs) -> Result<()> {
  let _ = args;
  mouse_event(
    page,
    MouseEvent {
      ty: "move",
      button: "none",
      buttons: 0,
      x,
      y,
      click_count: 0,
      modifiers: held(page),
    },
  )
  .await
}

/// Convert a CDP `modifiers` bitmask (alt=1, ctrl=2, meta=4, shift=8)
/// into `WebKit`'s `Source/WebKit/Shared/WebEvent.h` ordering
/// (shift=1, ctrl=2, alt=4, meta=8).
fn cdp_to_wk_mask(cdp: u32) -> u32 {
  let mut wk = 0;
  if cdp & 1 != 0 {
    wk |= 4;
  }
  if cdp & 2 != 0 {
    wk |= 2;
  }
  if cdp & 4 != 0 {
    wk |= 8;
  }
  if cdp & 8 != 0 {
    wk |= 1;
  }
  wk
}

pub async fn tap(page: &WebKitPage, x: f64, y: f64) -> Result<()> {
  page
    .proxy_session()
    .send(
      "Input.dispatchTapEvent",
      json!({ "x": x, "y": y, "modifiers": held(page) }),
    )
    .await
    .map_err(err)?;
  Ok(())
}

pub async fn move_mouse(page: &WebKitPage, x: f64, y: f64) -> Result<()> {
  mouse_event(
    page,
    MouseEvent {
      ty: "move",
      button: "none",
      buttons: 0,
      x,
      y,
      click_count: 0,
      modifiers: held(page),
    },
  )
  .await
}

pub async fn move_mouse_smooth(
  page: &WebKitPage,
  from_x: f64,
  from_y: f64,
  to_x: f64,
  to_y: f64,
  steps: u32,
) -> Result<()> {
  let steps = steps.max(1);
  for i in 1..=steps {
    let t = f64::from(i) / f64::from(steps);
    let x = from_x + (to_x - from_x) * t;
    let y = from_y + (to_y - from_y) * t;
    mouse_event(
      page,
      MouseEvent {
        ty: "move",
        button: "none",
        buttons: 0,
        x,
        y,
        click_count: 0,
        modifiers: held(page),
      },
    )
    .await?;
  }
  Ok(())
}

pub async fn mouse_wheel(page: &WebKitPage, x: f64, y: f64, delta_x: f64, delta_y: f64) -> Result<()> {
  page
    .target_session()
    .send("Page.updateScrollingState", json!({}))
    .await
    .map_err(err)?;
  // Wheel events hit the compositor first — wait one frame for it to
  // sync or the event is dropped before the page scrolls (mirrors
  // `wkInput.ts::wheel`).
  page.wait_for_compositor_frame().await?;
  page
    .proxy_session()
    .send(
      "Input.dispatchWheelEvent",
      json!({ "x": x, "y": y, "deltaX": delta_x, "deltaY": delta_y, "modifiers": held(page) }),
    )
    .await
    .map_err(err)?;
  Ok(())
}

pub async fn mouse_down(page: &WebKitPage, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
  let b = MouseButton::parse(button).unwrap_or_default();
  mouse_event(
    page,
    MouseEvent {
      ty: "down",
      button: b.as_cdp(),
      buttons: button_mask(b),
      x,
      y,
      click_count,
      modifiers: held(page),
    },
  )
  .await
}

pub async fn mouse_up(page: &WebKitPage, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
  let b = MouseButton::parse(button).unwrap_or_default();
  mouse_event(
    page,
    MouseEvent {
      ty: "up",
      button: b.as_cdp(),
      buttons: 0,
      x,
      y,
      click_count,
      modifiers: held(page),
    },
  )
  .await
}

pub async fn click_and_drag(page: &WebKitPage, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<()> {
  mouse_event(
    page,
    MouseEvent {
      ty: "move",
      button: "none",
      buttons: 0,
      x: from.0,
      y: from.1,
      click_count: 0,
      modifiers: held(page),
    },
  )
  .await?;
  mouse_event(
    page,
    MouseEvent {
      ty: "down",
      button: "left",
      buttons: 1,
      x: from.0,
      y: from.1,
      click_count: 1,
      modifiers: held(page),
    },
  )
  .await?;
  let steps = steps.max(1);
  for i in 1..=steps {
    let t = f64::from(i) / f64::from(steps);
    let x = from.0 + (to.0 - from.0) * t;
    let y = from.1 + (to.1 - from.1) * t;
    mouse_event(
      page,
      MouseEvent {
        ty: "move",
        button: "left",
        buttons: 1,
        x,
        y,
        click_count: 0,
        modifiers: held(page),
      },
    )
    .await?;
  }
  mouse_event(
    page,
    MouseEvent {
      ty: "up",
      button: "left",
      buttons: 0,
      x: to.0,
      y: to.1,
      click_count: 1,
      modifiers: held(page),
    },
  )
  .await?;
  Ok(())
}

pub async fn type_text(page: &WebKitPage, text: &str) -> Result<()> {
  page
    .target_session()
    .send("Page.insertText", json!({ "text": text }))
    .await
    .map_err(err)?;
  Ok(())
}

/// `Input.dispatchKeyEvent` for a keydown. `macCommands` names the
/// `AppKit` selectors the shortcut is bound to: on macOS `WebKit` inserts
/// a keydown's literal text unless the event also carries them, so
/// without it Enter inserts a bare `\r` instead of running
/// `insertNewline:`.
fn key_down_params(stroke: &KeyStroke) -> Value {
  let d = &stroke.description;
  let mut params = json!({
    "type": "keyDown",
    "modifiers": cdp_to_wk_mask(stroke.modifiers),
    "windowsVirtualKeyCode": d.key_code,
    "code": d.code,
    "key": d.key,
    "text": d.text,
    "unmodifiedText": d.text,
    "autoRepeat": stroke.auto_repeat,
    "isKeypad": d.is_keypad(),
  });
  let commands = mac_editing_selectors(d.code, stroke.modifiers);
  if !commands.is_empty() {
    params["macCommands"] = json!(commands);
  }
  params
}

fn key_up_params(stroke: &KeyStroke) -> Value {
  let d = &stroke.description;
  json!({
    "type": "keyUp",
    "modifiers": cdp_to_wk_mask(stroke.modifiers),
    "key": d.key,
    "windowsVirtualKeyCode": d.key_code,
    "code": d.code,
    "isKeypad": d.is_keypad(),
  })
}

pub async fn key_down(page: &WebKitPage, stroke: &KeyStroke) -> Result<()> {
  page
    .proxy_session()
    .send("Input.dispatchKeyEvent", key_down_params(stroke))
    .await
    .map_err(err)?;
  Ok(())
}

pub async fn key_up(page: &WebKitPage, stroke: &KeyStroke) -> Result<()> {
  page
    .proxy_session()
    .send("Input.dispatchKeyEvent", key_up_params(stroke))
    .await
    .map_err(err)?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use ferridriver_types::keyboard::Keyboard;
  use serde_json::json;

  use super::{key_down_params, key_up_params};

  #[test]
  fn key_events_carry_the_held_modifiers_in_webkit_order() {
    let mut keyboard = Keyboard::new();
    keyboard.down("Shift", false).unwrap();
    let a = keyboard.down("KeyA", false).unwrap();
    assert_eq!(
      key_down_params(&a),
      json!({
        "type": "keyDown", "modifiers": 1, "windowsVirtualKeyCode": 65, "code": "KeyA", "key": "A",
        "text": "A", "unmodifiedText": "A", "autoRepeat": false, "isKeypad": false,
      })
    );
    keyboard.up("KeyA", false).unwrap();
    keyboard.up("Shift", false).unwrap();
    let meta = keyboard.down("Meta", false).unwrap();
    assert_eq!(key_down_params(&meta)["modifiers"], 8);
    let a = keyboard.down("a", false).unwrap();
    let params = key_down_params(&a);
    assert_eq!(params["text"], "");
    assert_eq!(params["macCommands"], json!(["selectAll:"]));
    assert_eq!(key_up_params(&a)["modifiers"], 8);
  }

  #[test]
  fn enter_names_its_newline_selector_and_a_sided_key_its_side_code() {
    let mut keyboard = Keyboard::new();
    let enter = keyboard.down("Enter", false).unwrap();
    assert_eq!(key_down_params(&enter)["macCommands"], json!(["insertNewline:"]));
    let right = keyboard.down("ShiftRight", false).unwrap();
    assert_eq!(key_down_params(&right)["windowsVirtualKeyCode"], 161);
  }
}
