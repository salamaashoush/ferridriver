//! Native input dispatch for the PW `WebKit` backend.
//!
//! Per `wkInput.ts`: mouse / key / wheel / tap events go through
//! `Input.dispatch*Event` on the **page-proxy** session;
//! `Page.insertText` and `Page.updateScrollingState` go through the
//! target session.

use serde_json::{Value, json};

use super::connection::ConnectionError;
use super::page::PwWebKitPage;
use crate::backend::{BackendClickArgs, BackendHoverArgs, BackendTapArgs};
use crate::error::{FerriError, Result};
use crate::options::{Modifier, MouseButton};

fn err(e: ConnectionError) -> FerriError {
  e.into()
}

/// PW `WebKit` modifier mask (`Source/WebKit/Shared/WebEvent.h`):
/// Shift=1, Control=2, Alt=4, Meta=8.
fn modifiers_mask(mods: &[Modifier]) -> u32 {
  let mut m = 0;
  for md in mods {
    m |= match md {
      Modifier::Shift => 1,
      Modifier::Control => 2,
      Modifier::Alt => 4,
      Modifier::Meta => 8,
      Modifier::ControlOrMeta => {
        if cfg!(target_os = "macos") {
          8
        } else {
          2
        }
      },
    };
  }
  m
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
async fn mouse_event(page: &PwWebKitPage, ev: MouseEvent<'_>) -> Result<()> {
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

pub async fn click(page: &PwWebKitPage, x: f64, y: f64, args: &BackendClickArgs) -> Result<()> {
  let mods = modifiers_mask(&args.modifiers);
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

pub async fn hover(page: &PwWebKitPage, x: f64, y: f64, args: &BackendHoverArgs) -> Result<()> {
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
      modifiers: 0,
    },
  )
  .await
}

/// Convert a CDP `modifiers` bitmask (alt=1, ctrl=2, meta=4, shift=8)
/// into PW `WebKit`'s `Source/WebKit/Shared/WebEvent.h` ordering
/// (shift=1, ctrl=2, alt=4, meta=8). [`BackendTapArgs`] carries only
/// the CDP-shaped bitmask, so the conversion happens here at the wire
/// boundary.
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

pub async fn tap(page: &PwWebKitPage, x: f64, y: f64, args: &BackendTapArgs) -> Result<()> {
  page
    .proxy_session()
    .send(
      "Input.dispatchTapEvent",
      json!({ "x": x, "y": y, "modifiers": cdp_to_wk_mask(args.modifiers_bitmask) }),
    )
    .await
    .map_err(err)?;
  Ok(())
}

pub async fn move_mouse(page: &PwWebKitPage, x: f64, y: f64) -> Result<()> {
  mouse_event(
    page,
    MouseEvent {
      ty: "move",
      button: "none",
      buttons: 0,
      x,
      y,
      click_count: 0,
      modifiers: 0,
    },
  )
  .await
}

pub async fn move_mouse_smooth(
  page: &PwWebKitPage,
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
        modifiers: 0,
      },
    )
    .await?;
  }
  Ok(())
}

pub async fn mouse_wheel(page: &PwWebKitPage, delta_x: f64, delta_y: f64) -> Result<()> {
  let _ = page.target_session().send("Page.updateScrollingState", json!({})).await;
  page
    .proxy_session()
    .send(
      "Input.dispatchWheelEvent",
      json!({ "x": 0, "y": 0, "deltaX": delta_x, "deltaY": delta_y, "modifiers": 0 }),
    )
    .await
    .map_err(err)?;
  Ok(())
}

pub async fn mouse_down(page: &PwWebKitPage, x: f64, y: f64, button: &str) -> Result<()> {
  let b = MouseButton::parse(button).unwrap_or_default();
  mouse_event(
    page,
    MouseEvent {
      ty: "down",
      button: b.as_cdp(),
      buttons: button_mask(b),
      x,
      y,
      click_count: 1,
      modifiers: 0,
    },
  )
  .await
}

pub async fn mouse_up(page: &PwWebKitPage, x: f64, y: f64, button: &str) -> Result<()> {
  let b = MouseButton::parse(button).unwrap_or_default();
  mouse_event(
    page,
    MouseEvent {
      ty: "up",
      button: b.as_cdp(),
      buttons: 0,
      x,
      y,
      click_count: 1,
      modifiers: 0,
    },
  )
  .await
}

pub async fn click_and_drag(page: &PwWebKitPage, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<()> {
  mouse_event(
    page,
    MouseEvent {
      ty: "move",
      button: "none",
      buttons: 0,
      x: from.0,
      y: from.1,
      click_count: 0,
      modifiers: 0,
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
      modifiers: 0,
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
        modifiers: 0,
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
      modifiers: 0,
    },
  )
  .await?;
  Ok(())
}

pub async fn type_text(page: &PwWebKitPage, text: &str) -> Result<()> {
  page
    .target_session()
    .send("Page.insertText", json!({ "text": text }))
    .await
    .map_err(err)?;
  Ok(())
}

/// `{ code, key, windowsVirtualKeyCode }` for a key name. Covers the
/// common named keys; printable single chars fall through with the
/// char as both `key` and `text`.
fn key_descriptor(key: &str) -> (String, String, i64, Option<String>) {
  match key {
    "Enter" | "Return" => ("Enter".into(), "Enter".into(), 13, Some("\r".into())),
    "Tab" => ("Tab".into(), "Tab".into(), 9, Some("\t".into())),
    "Backspace" => ("Backspace".into(), "Backspace".into(), 8, None),
    "Delete" => ("Delete".into(), "Delete".into(), 46, None),
    "Escape" => ("Escape".into(), "Escape".into(), 27, None),
    "ArrowLeft" => ("ArrowLeft".into(), "ArrowLeft".into(), 37, None),
    "ArrowUp" => ("ArrowUp".into(), "ArrowUp".into(), 38, None),
    "ArrowRight" => ("ArrowRight".into(), "ArrowRight".into(), 39, None),
    "ArrowDown" => ("ArrowDown".into(), "ArrowDown".into(), 40, None),
    "Home" => ("Home".into(), "Home".into(), 36, None),
    "End" => ("End".into(), "End".into(), 35, None),
    "PageUp" => ("PageUp".into(), "PageUp".into(), 33, None),
    "PageDown" => ("PageDown".into(), "PageDown".into(), 34, None),
    "Space" | " " => ("Space".into(), " ".into(), 32, Some(" ".into())),
    "Shift" => ("ShiftLeft".into(), "Shift".into(), 16, None),
    "Control" => ("ControlLeft".into(), "Control".into(), 17, None),
    "Alt" => ("AltLeft".into(), "Alt".into(), 18, None),
    "Meta" => ("MetaLeft".into(), "Meta".into(), 91, None),
    other => {
      let code = if other.len() == 1 {
        let c = other.chars().next().unwrap_or(' ');
        if c.is_ascii_alphabetic() {
          format!("Key{}", c.to_ascii_uppercase())
        } else if c.is_ascii_digit() {
          format!("Digit{c}")
        } else {
          other.to_string()
        }
      } else {
        other.to_string()
      };
      let text = (other.chars().count() == 1).then(|| other.to_string());
      (code, other.to_string(), 0, text)
    },
  }
}

async fn dispatch_key(page: &PwWebKitPage, ty: &str, key: &str) -> Result<()> {
  let (code, key_name, vk, text) = key_descriptor(key);
  let mut params = json!({
    "type": ty,
    "key": key_name,
    "code": code,
    "windowsVirtualKeyCode": vk,
    "modifiers": 0,
  });
  if ty == "keyDown" {
    if let Some(t) = text {
      params["text"] = Value::String(t.clone());
      params["unmodifiedText"] = Value::String(t);
    }
  }
  page
    .proxy_session()
    .send("Input.dispatchKeyEvent", params)
    .await
    .map_err(err)?;
  Ok(())
}

pub async fn key_down(page: &PwWebKitPage, key: &str) -> Result<()> {
  dispatch_key(page, "keyDown", key).await
}

pub async fn key_up(page: &PwWebKitPage, key: &str) -> Result<()> {
  dispatch_key(page, "keyUp", key).await
}

pub async fn press_key(page: &PwWebKitPage, key: &str) -> Result<()> {
  dispatch_key(page, "keyDown", key).await?;
  dispatch_key(page, "keyUp", key).await
}

pub async fn press_modifiers(page: &PwWebKitPage, mods: &[Modifier]) -> Result<()> {
  for m in mods {
    dispatch_key(page, "keyDown", m.key_name()).await?;
  }
  Ok(())
}

pub async fn release_modifiers(page: &PwWebKitPage, mods: &[Modifier]) -> Result<()> {
  for m in mods.iter().rev() {
    dispatch_key(page, "keyUp", m.key_name()).await?;
  }
  Ok(())
}
