//! Native input dispatch for the `WebKit` backend.
//!
//! Per `wkInput.ts`: mouse / key / wheel / tap events go through
//! `Input.dispatch*Event` on the **page-proxy** session;
//! `Page.insertText` and `Page.updateScrollingState` go through the
//! target session.

use serde_json::{Value, json};

use super::connection::ConnectionError;
use super::page::WebKitPage;
use crate::backend::{BackendClickArgs, BackendHoverArgs, BackendTapArgs};
use crate::error::{FerriError, Result};
use crate::options::{Modifier, MouseButton};

fn err(e: ConnectionError) -> FerriError {
  e.into()
}

/// `WebKit` modifier mask (`Source/WebKit/Shared/WebEvent.h`):
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
      modifiers: 0,
    },
  )
  .await
}

/// Convert a CDP `modifiers` bitmask (alt=1, ctrl=2, meta=4, shift=8)
/// into `WebKit`'s `Source/WebKit/Shared/WebEvent.h` ordering
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

pub async fn tap(page: &WebKitPage, x: f64, y: f64, args: &BackendTapArgs) -> Result<()> {
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
      modifiers: 0,
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
        modifiers: 0,
      },
    )
    .await?;
  }
  Ok(())
}

pub async fn mouse_wheel(page: &WebKitPage, delta_x: f64, delta_y: f64) -> Result<()> {
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
      json!({ "x": 0, "y": 0, "deltaX": delta_x, "deltaY": delta_y, "modifiers": 0 }),
    )
    .await
    .map_err(err)?;
  Ok(())
}

pub async fn mouse_down(page: &WebKitPage, x: f64, y: f64, button: &str) -> Result<()> {
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

pub async fn mouse_up(page: &WebKitPage, x: f64, y: f64, button: &str) -> Result<()> {
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

pub async fn type_text(page: &WebKitPage, text: &str) -> Result<()> {
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

/// macOS editing command(s) for a `modifier+code` shortcut, mirroring
/// Playwright's `macEditingCommands` (`server/macEditingCommands.ts`).
/// `WebKit` on macOS inserts a `keyDown`'s literal `text` unless the
/// event also carries the matching `AppKit` selector(s) via `macCommands`
/// -- so without this Enter inserts a bare `\r` instead of running
/// `insertNewline:` (which yields `\n`). The field is a no-op on other
/// platforms, where `WebKit` applies its own key bindings.
fn mac_editing_commands(shortcut: &str) -> Option<&'static [&'static str]> {
  MAC_EDITING_COMMANDS
    .iter()
    .find(|&&(k, _)| k == shortcut)
    .map(|&(_, cmds)| cmds)
}

/// `(shortcut, selectors)` table backing [`mac_editing_commands`], ported
/// 1:1 from Playwright's `server/macEditingCommands.ts`.
#[rustfmt::skip]
static MAC_EDITING_COMMANDS: &[(&str, &[&str])] = &[
  ("Backspace", &["deleteBackward:"]),
  ("Shift+Backspace", &["deleteBackward:"]),
  ("Enter", &["insertNewline:"]),
  ("NumpadEnter", &["insertNewline:"]),
  ("Shift+Enter", &["insertNewline:"]),
  ("Shift+NumpadEnter", &["insertNewline:"]),
  ("Escape", &["cancelOperation:"]),
  ("Shift+Escape", &["cancelOperation:"]),
  ("ArrowUp", &["moveUp:"]),
  ("ArrowDown", &["moveDown:"]),
  ("ArrowLeft", &["moveLeft:"]),
  ("ArrowRight", &["moveRight:"]),
  ("F5", &["complete:"]),
  ("Shift+F5", &["complete:"]),
  ("Delete", &["deleteForward:"]),
  ("Shift+Delete", &["deleteForward:"]),
  ("Home", &["scrollToBeginningOfDocument:"]),
  ("End", &["scrollToEndOfDocument:"]),
  ("PageUp", &["scrollPageUp:"]),
  ("PageDown", &["scrollPageDown:"]),
  ("Shift+ArrowUp", &["moveUpAndModifySelection:"]),
  ("Shift+ArrowDown", &["moveDownAndModifySelection:"]),
  ("Shift+ArrowLeft", &["moveLeftAndModifySelection:"]),
  ("Shift+ArrowRight", &["moveRightAndModifySelection:"]),
  ("Shift+Home", &["moveToBeginningOfDocumentAndModifySelection:"]),
  ("Shift+End", &["moveToEndOfDocumentAndModifySelection:"]),
  ("Shift+PageUp", &["pageUpAndModifySelection:"]),
  ("Shift+PageDown", &["pageDownAndModifySelection:"]),
  ("Shift+Numpad5", &["delete:"]),
  ("Control+Tab", &["selectNextKeyView:"]),
  ("Control+Enter", &["insertLineBreak:"]),
  ("Control+NumpadEnter", &["insertLineBreak:"]),
  ("Control+Quote", &["insertSingleQuoteIgnoringSubstitution:"]),
  ("Control+KeyA", &["moveToBeginningOfParagraph:"]),
  ("Control+KeyB", &["moveBackward:"]),
  ("Control+KeyD", &["deleteForward:"]),
  ("Control+KeyE", &["moveToEndOfParagraph:"]),
  ("Control+KeyF", &["moveForward:"]),
  ("Control+KeyH", &["deleteBackward:"]),
  ("Control+KeyK", &["deleteToEndOfParagraph:"]),
  ("Control+KeyL", &["centerSelectionInVisibleArea:"]),
  ("Control+KeyN", &["moveDown:"]),
  ("Control+KeyO", &["insertNewlineIgnoringFieldEditor:", "moveBackward:"]),
  ("Control+KeyP", &["moveUp:"]),
  ("Control+KeyT", &["transpose:"]),
  ("Control+KeyV", &["pageDown:"]),
  ("Control+KeyY", &["yank:"]),
  ("Control+Backspace", &["deleteBackwardByDecomposingPreviousCharacter:"]),
  ("Control+ArrowUp", &["scrollPageUp:"]),
  ("Control+ArrowDown", &["scrollPageDown:"]),
  ("Control+ArrowLeft", &["moveToLeftEndOfLine:"]),
  ("Control+ArrowRight", &["moveToRightEndOfLine:"]),
  ("Shift+Control+Enter", &["insertLineBreak:"]),
  ("Shift+Control+NumpadEnter", &["insertLineBreak:"]),
  ("Shift+Control+Tab", &["selectPreviousKeyView:"]),
  ("Shift+Control+Quote", &["insertDoubleQuoteIgnoringSubstitution:"]),
  ("Shift+Control+KeyA", &["moveToBeginningOfParagraphAndModifySelection:"]),
  ("Shift+Control+KeyB", &["moveBackwardAndModifySelection:"]),
  ("Shift+Control+KeyE", &["moveToEndOfParagraphAndModifySelection:"]),
  ("Shift+Control+KeyF", &["moveForwardAndModifySelection:"]),
  ("Shift+Control+KeyN", &["moveDownAndModifySelection:"]),
  ("Shift+Control+KeyP", &["moveUpAndModifySelection:"]),
  ("Shift+Control+KeyV", &["pageDownAndModifySelection:"]),
  ("Shift+Control+Backspace", &["deleteBackwardByDecomposingPreviousCharacter:"]),
  ("Shift+Control+ArrowUp", &["scrollPageUp:"]),
  ("Shift+Control+ArrowDown", &["scrollPageDown:"]),
  ("Shift+Control+ArrowLeft", &["moveToLeftEndOfLineAndModifySelection:"]),
  ("Shift+Control+ArrowRight", &["moveToRightEndOfLineAndModifySelection:"]),
  ("Alt+Backspace", &["deleteWordBackward:"]),
  ("Alt+Enter", &["insertNewlineIgnoringFieldEditor:"]),
  ("Alt+NumpadEnter", &["insertNewlineIgnoringFieldEditor:"]),
  ("Alt+Escape", &["complete:"]),
  ("Alt+ArrowUp", &["moveBackward:", "moveToBeginningOfParagraph:"]),
  ("Alt+ArrowDown", &["moveForward:", "moveToEndOfParagraph:"]),
  ("Alt+ArrowLeft", &["moveWordLeft:"]),
  ("Alt+ArrowRight", &["moveWordRight:"]),
  ("Alt+Delete", &["deleteWordForward:"]),
  ("Alt+PageUp", &["pageUp:"]),
  ("Alt+PageDown", &["pageDown:"]),
  ("Shift+Alt+Backspace", &["deleteWordBackward:"]),
  ("Shift+Alt+Enter", &["insertNewlineIgnoringFieldEditor:"]),
  ("Shift+Alt+NumpadEnter", &["insertNewlineIgnoringFieldEditor:"]),
  ("Shift+Alt+Escape", &["complete:"]),
  ("Shift+Alt+ArrowUp", &["moveParagraphBackwardAndModifySelection:"]),
  ("Shift+Alt+ArrowDown", &["moveParagraphForwardAndModifySelection:"]),
  ("Shift+Alt+ArrowLeft", &["moveWordLeftAndModifySelection:"]),
  ("Shift+Alt+ArrowRight", &["moveWordRightAndModifySelection:"]),
  ("Shift+Alt+Delete", &["deleteWordForward:"]),
  ("Shift+Alt+PageUp", &["pageUp:"]),
  ("Shift+Alt+PageDown", &["pageDown:"]),
  ("Control+Alt+KeyB", &["moveWordBackward:"]),
  ("Control+Alt+KeyF", &["moveWordForward:"]),
  ("Control+Alt+Backspace", &["deleteWordBackward:"]),
  ("Shift+Control+Alt+KeyB", &["moveWordBackwardAndModifySelection:"]),
  ("Shift+Control+Alt+KeyF", &["moveWordForwardAndModifySelection:"]),
  ("Shift+Control+Alt+Backspace", &["deleteWordBackward:"]),
  ("Meta+NumpadSubtract", &["cancel:"]),
  ("Meta+Backspace", &["deleteToBeginningOfLine:"]),
  ("Meta+ArrowUp", &["moveToBeginningOfDocument:"]),
  ("Meta+ArrowDown", &["moveToEndOfDocument:"]),
  ("Meta+ArrowLeft", &["moveToLeftEndOfLine:"]),
  ("Meta+ArrowRight", &["moveToRightEndOfLine:"]),
  ("Shift+Meta+NumpadSubtract", &["cancel:"]),
  ("Shift+Meta+Backspace", &["deleteToBeginningOfLine:"]),
  ("Shift+Meta+ArrowUp", &["moveToBeginningOfDocumentAndModifySelection:"]),
  ("Shift+Meta+ArrowDown", &["moveToEndOfDocumentAndModifySelection:"]),
  ("Shift+Meta+ArrowLeft", &["moveToLeftEndOfLineAndModifySelection:"]),
  ("Shift+Meta+ArrowRight", &["moveToRightEndOfLineAndModifySelection:"]),
  ("Meta+KeyA", &["selectAll:"]),
  ("Meta+KeyC", &["copy:"]),
  ("Meta+KeyX", &["cut:"]),
  ("Meta+KeyV", &["paste:"]),
  ("Meta+KeyZ", &["undo:"]),
  ("Shift+Meta+KeyZ", &["redo:"]),
];

/// Build the `modifier+code` shortcut string (`"Shift+Control+ArrowLeft"`)
/// for [`mac_editing_commands`]. Modifier order matches Playwright's
/// `RawKeyboardImpl.keydown`: Shift, Control, Alt, Meta, then the code.
fn editing_shortcut(modifiers: u32, code: &str) -> String {
  let mut parts: Vec<&str> = Vec::new();
  if modifiers & 1 != 0 {
    parts.push("Shift");
  }
  if modifiers & 2 != 0 {
    parts.push("Control");
  }
  if modifiers & 4 != 0 {
    parts.push("Alt");
  }
  if modifiers & 8 != 0 {
    parts.push("Meta");
  }
  parts.push(code);
  parts.join("+")
}

/// `WebKit` modifier bit for a Playwright modifier-key name (the leading
/// tokens of a `"Control+a"` combo). Mirrors [`modifiers_mask`] but keyed
/// on the string name. Returns `0` for non-modifier tokens.
fn modifier_bit_for_name(name: &str) -> u32 {
  match name {
    "Shift" => 1,
    "Control" => 2,
    "Alt" => 4,
    "Meta" => 8,
    "ControlOrMeta" => {
      if cfg!(target_os = "macos") {
        8
      } else {
        2
      }
    },
    _ => 0,
  }
}

/// Dispatch one `Input.dispatchKeyEvent` carrying a `WebKit` `modifiers`
/// bitmask (Shift=1, Control=2, Alt=4, Meta=8 -- see
/// `Source/WebKit/Shared/WebEvent.h` and `wkInput.ts::toModifiersMask`).
/// `text` is suppressed while a non-Shift modifier is held so that
/// `Control+a` performs select-all rather than inserting a literal "a"
/// (mirrors `wkInput.ts::keydown`, which only sends `text` for the bare
/// key — the editing behaviour rides on `macCommands` on macOS and on
/// the modifiers mask everywhere).
async fn dispatch_key_with_mods(page: &WebKitPage, ty: &str, key: &str, modifiers: u32) -> Result<()> {
  let (code, key_name, vk, text) = key_descriptor(key);
  let mut params = json!({
    "type": ty,
    "key": key_name,
    "code": code,
    "windowsVirtualKeyCode": vk,
    "modifiers": modifiers,
  });
  if ty == "keyDown" {
    // Shift (bit 1) still produces text (capital letters); any other
    // held modifier suppresses the inserted character.
    let suppress_text = modifiers & !1 != 0;
    if let Some(t) = text
      && !suppress_text
    {
      params["text"] = Value::String(t.clone());
      params["unmodifiedText"] = Value::String(t);
    }
    if let Some(cmds) = mac_editing_commands(&editing_shortcut(modifiers, &code)) {
      params["macCommands"] = Value::Array(cmds.iter().map(|c| Value::String((*c).to_string())).collect());
    }
  }
  page
    .proxy_session()
    .send("Input.dispatchKeyEvent", params)
    .await
    .map_err(err)?;
  Ok(())
}

async fn dispatch_key(page: &WebKitPage, ty: &str, key: &str) -> Result<()> {
  dispatch_key_with_mods(page, ty, key, 0).await
}

pub async fn key_down(page: &WebKitPage, key: &str) -> Result<()> {
  dispatch_key(page, "keyDown", key).await
}

pub async fn key_up(page: &WebKitPage, key: &str) -> Result<()> {
  dispatch_key(page, "keyUp", key).await
}

/// Press a key or a Playwright-style modifier combo (`"Control+a"`,
/// `"Shift+Alt+T"`). The leading `+`-separated tokens are modifiers;
/// the final token is the primary key. Each modifier keyDown carries
/// the cumulative mask (`WebKit` expects to see Ctrl set on the Ctrl
/// keyDown itself), then the primary key is dispatched with the full
/// mask so the page sees `e.ctrlKey === true`. Mirrors the CDP backend's
/// `press_key` combo handling and `wkInput.ts::keydown`.
pub async fn press_key(page: &WebKitPage, key: &str) -> Result<()> {
  let parts: Vec<&str> = key.split('+').collect();
  if parts.len() <= 1 {
    dispatch_key(page, "keyDown", key).await?;
    dispatch_key(page, "keyUp", key).await?;
    return Ok(());
  }
  let (mods, primary) = parts.split_at(parts.len() - 1);
  let primary = primary[0];
  let mut bits = 0u32;
  for m in mods {
    let b = modifier_bit_for_name(m);
    if b != 0 {
      bits |= b;
      dispatch_key_with_mods(page, "keyDown", m, bits).await?;
    }
  }
  dispatch_key_with_mods(page, "keyDown", primary, bits).await?;
  dispatch_key_with_mods(page, "keyUp", primary, bits).await?;
  let mut down_bits = bits;
  for m in mods.iter().rev() {
    let b = modifier_bit_for_name(m);
    if b != 0 {
      dispatch_key_with_mods(page, "keyUp", m, down_bits).await?;
      down_bits &= !b;
    }
  }
  Ok(())
}

pub async fn press_modifiers(page: &WebKitPage, mods: &[Modifier]) -> Result<()> {
  let mut bits = 0u32;
  for m in mods {
    bits |= modifiers_mask(std::slice::from_ref(m));
    dispatch_key_with_mods(page, "keyDown", m.key_name(), bits).await?;
  }
  Ok(())
}

pub async fn release_modifiers(page: &WebKitPage, mods: &[Modifier]) -> Result<()> {
  let mut bits = modifiers_mask(mods);
  for m in mods.iter().rev() {
    dispatch_key_with_mods(page, "keyUp", m.key_name(), bits).await?;
    bits &= !modifiers_mask(std::slice::from_ref(m));
  }
  Ok(())
}
