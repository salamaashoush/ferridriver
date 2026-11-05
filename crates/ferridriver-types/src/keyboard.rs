//! The US keyboard as browser input protocols see it, and the keyboard
//! state that turns key names into the events a real keyboard sends.
//!
//! A key is named the way `KeyboardEvent` does: by its `code` (`"KeyA"`,
//! `"Digit1"`, `"ShiftRight"`, `"Numpad1"`), by the `key` it produces
//! (`"a"`, `"!"`, `"Enter"`), or by a modifier's own name (`"Shift"`).
//! [`describe`] resolves a name to the [`KeyDescription`] an
//! `Input.dispatchKeyEvent` needs. [`Keyboard`] keeps the pressed
//! modifiers between calls, so `down("Shift")` followed by
//! `down("KeyA")` produces `A` with `shiftKey` set, and a held Control
//! or Meta turns typing into a shortcut. On macOS Chromium only performs
//! an editing shortcut such as select-all when the key event names the
//! command, which [`mac_editing_commands`] supplies.
//!
//! Nothing here talks to a browser; a caller that drives raw CDP input
//! can use it on its own.

/// `KeyboardEvent.location` of a numeric keypad key.
pub const KEYPAD_LOCATION: u32 = 3;

/// A modifier key. Its [`bit`](Self::bit) is its place in the
/// `modifiers` mask of CDP's `Input.dispatchKeyEvent`,
/// `Input.dispatchMouseEvent` and `Input.dispatchTouchEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
  Alt,
  Control,
  Meta,
  Shift,
}

impl Modifier {
  /// Every modifier, in the order a change of held modifiers is applied.
  pub const ALL: [Self; 4] = [Self::Alt, Self::Control, Self::Meta, Self::Shift];

  /// The CDP mask bit: Alt=1, Control=2, Meta=4, Shift=8.
  #[must_use]
  pub const fn bit(self) -> u32 {
    match self {
      Self::Alt => 1,
      Self::Control => 2,
      Self::Meta => 4,
      Self::Shift => 8,
    }
  }

  /// The `KeyboardEvent.key` of the modifier, which is also the name
  /// that presses it.
  #[must_use]
  pub const fn key(self) -> &'static str {
    match self {
      Self::Alt => "Alt",
      Self::Control => "Control",
      Self::Meta => "Meta",
      Self::Shift => "Shift",
    }
  }

  /// The modifier a `KeyboardEvent.key` belongs to, so both `ShiftLeft`
  /// and `ShiftRight` (whose key is `"Shift"`) hold Shift.
  #[must_use]
  pub fn from_key(key: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|m| m.key() == key)
  }
}

/// `"ControlOrMeta"` as the modifier a platform's shortcuts use: Meta on
/// macOS, Control elsewhere. Any other name is returned unchanged.
#[must_use]
pub fn resolve_control_or_meta(name: &str, mac: bool) -> &str {
  match name {
    "ControlOrMeta" if mac => "Meta",
    "ControlOrMeta" => "Control",
    other => other,
  }
}

/// One physical key of the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyDefinition {
  /// `KeyboardEvent.code`.
  pub code: &'static str,
  /// `KeyboardEvent.key` without Shift.
  pub key: &'static str,
  /// Windows virtual key code, distinguishing left and right where
  /// Windows does (`ShiftLeft` is 160).
  pub key_code: u32,
  /// The virtual key code without the side (`ShiftLeft` is 16), when it
  /// differs from [`key_code`](Self::key_code).
  pub key_code_without_location: Option<u32>,
  /// `KeyboardEvent.key` with Shift held, for a key Shift changes.
  pub shift_key: Option<&'static str>,
  /// The virtual key code with Shift held, when Shift changes it (the
  /// numeric keypad's digits).
  pub shift_key_code: Option<u32>,
  /// Text a key with a multi-character `key` types (`Enter` types `\r`).
  pub text: Option<&'static str>,
  /// `KeyboardEvent.location`: 0 standard, 1 left, 2 right, 3 keypad.
  pub location: u32,
}

/// What one key event carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyDescription {
  /// `KeyboardEvent.key`.
  pub key: &'static str,
  /// Windows virtual key code, with the side for a sided key.
  pub key_code: u32,
  /// Windows virtual key code without the side: CDP's
  /// `windowsVirtualKeyCode`.
  pub key_code_without_location: u32,
  /// `KeyboardEvent.code`.
  pub code: &'static str,
  /// Text the key types; empty for a key that types nothing.
  pub text: &'static str,
  /// `KeyboardEvent.location`.
  pub location: u32,
}

impl KeyDescription {
  #[must_use]
  pub const fn is_keypad(&self) -> bool {
    self.location == KEYPAD_LOCATION
  }

  fn unshifted(definition: &KeyDefinition) -> Self {
    let text = if definition.key.chars().count() == 1 {
      definition.key
    } else {
      definition.text.unwrap_or("")
    };
    Self {
      key: definition.key,
      key_code: definition.key_code,
      key_code_without_location: definition.key_code_without_location.unwrap_or(definition.key_code),
      code: definition.code,
      text,
      location: definition.location,
    }
  }

  fn shifted(definition: &KeyDefinition) -> Option<Self> {
    let shift_key = definition.shift_key?;
    let base = Self::unshifted(definition);
    let key_code = definition.shift_key_code.unwrap_or(base.key_code);
    Some(Self {
      key: shift_key,
      text: shift_key,
      key_code,
      key_code_without_location: definition.shift_key_code.unwrap_or(base.key_code_without_location),
      ..base
    })
  }
}

/// Names that press a key without naming its code or the text it types.
const ALIASES: &[(&str, &str)] = &[
  ("Shift", "ShiftLeft"),
  ("Control", "ControlLeft"),
  ("Alt", "AltLeft"),
  ("Meta", "MetaLeft"),
  ("\n", "Enter"),
  ("\r", "Enter"),
];

/// The description of the key `name` presses, or `None` for a name the
/// layout does not have.
///
/// A key named by its code is the one name Shift changes: with `shift`,
/// `"KeyA"` describes `A` and `"Digit1"` describes `!`. A key named by
/// the character it types is that character whatever `shift` says, so
/// `"a"` stays `a` and `"A"` is `KeyA` typing `A`. A keypad key is only
/// reachable by its code, so `"1"` is always `Digit1`.
#[must_use]
pub fn describe(name: &str, shift: bool) -> Option<KeyDescription> {
  if let Some(definition) = US_KEYBOARD_LAYOUT.iter().find(|d| d.code == name) {
    let shifted = if shift {
      KeyDescription::shifted(definition)
    } else {
      None
    };
    return Some(shifted.unwrap_or_else(|| KeyDescription::unshifted(definition)));
  }
  if let Some(&(_, code)) = ALIASES.iter().find(|&&(alias, _)| alias == name) {
    return US_KEYBOARD_LAYOUT
      .iter()
      .find(|d| d.code == code)
      .map(KeyDescription::unshifted);
  }
  US_KEYBOARD_LAYOUT.iter().filter(|d| d.location == 0).find_map(|d| {
    if d.key == name && d.key.chars().count() == 1 {
      Some(KeyDescription::unshifted(d))
    } else if d.shift_key == Some(name) {
      KeyDescription::shifted(d)
    } else {
      None
    }
  })
}

/// A key name the layout does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKey(pub String);

impl std::fmt::Display for UnknownKey {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "Unknown key: \"{}\"", self.0)
  }
}

impl std::error::Error for UnknownKey {}

/// One key event to dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyStroke {
  /// The key, already shifted when Shift is held, with its text emptied
  /// when a modifier other than Shift is held.
  pub description: KeyDescription,
  /// The CDP mask of the modifiers held once this event happened: a
  /// modifier's own keydown carries its bit and its keyup does not.
  pub modifiers: u32,
  /// The key was already down (a keydown only).
  pub auto_repeat: bool,
}

/// The pressed keys of one page.
#[derive(Debug, Clone, Default)]
pub struct Keyboard {
  modifiers: u32,
  pressed: Vec<&'static str>,
}

impl Keyboard {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// The CDP mask of the held modifiers, which every mouse and touch
  /// event carries too.
  #[must_use]
  pub const fn modifiers(&self) -> u32 {
    self.modifiers
  }

  /// Press `name` (resolving `ControlOrMeta` for `mac`).
  ///
  /// # Errors
  ///
  /// [`UnknownKey`] for a name the layout does not have; nothing changes.
  pub fn down(&mut self, name: &str, mac: bool) -> Result<KeyStroke, UnknownKey> {
    let description = self.description(name, mac)?;
    let auto_repeat = self.pressed.contains(&description.code);
    if !auto_repeat {
      self.pressed.push(description.code);
    }
    if let Some(modifier) = Modifier::from_key(description.key) {
      self.modifiers |= modifier.bit();
    }
    Ok(KeyStroke {
      description,
      modifiers: self.modifiers,
      auto_repeat,
    })
  }

  /// Release `name` (resolving `ControlOrMeta` for `mac`).
  ///
  /// # Errors
  ///
  /// [`UnknownKey`] for a name the layout does not have; nothing changes.
  pub fn up(&mut self, name: &str, mac: bool) -> Result<KeyStroke, UnknownKey> {
    let description = self.description(name, mac)?;
    if let Some(modifier) = Modifier::from_key(description.key) {
      self.modifiers &= !modifier.bit();
    }
    self.pressed.retain(|code| *code != description.code);
    Ok(KeyStroke {
      description,
      modifiers: self.modifiers,
      auto_repeat: false,
    })
  }

  fn description(&self, name: &str, mac: bool) -> Result<KeyDescription, UnknownKey> {
    let name = resolve_control_or_meta(name, mac);
    let shift = self.modifiers & Modifier::Shift.bit() != 0;
    let mut description = describe(name, shift).ok_or_else(|| UnknownKey(name.to_string()))?;
    if self.modifiers & !Modifier::Shift.bit() != 0 {
      description.text = "";
    }
    Ok(description)
  }
}

/// The shortcut naming a key and the modifiers held with it, in the form
/// the editing-command table is keyed by: `Shift+Control+Alt+Meta+<code>`.
fn editing_shortcut(code: &str, modifiers: u32) -> String {
  let mut shortcut = String::new();
  for modifier in [Modifier::Shift, Modifier::Control, Modifier::Alt, Modifier::Meta] {
    if modifiers & modifier.bit() != 0 {
      shortcut.push_str(modifier.key());
      shortcut.push('+');
    }
  }
  shortcut.push_str(code);
  shortcut
}

/// The `AppKit` selectors (`"selectAll:"`, `"insertNewline:"`) macOS
/// binds to the key `code` pressed with `modifiers` (a CDP mask). This
/// is what `WebKit`'s `macCommands` carries.
#[must_use]
pub fn mac_editing_selectors(code: &str, modifiers: u32) -> &'static [&'static str] {
  let shortcut = editing_shortcut(code, modifiers);
  MAC_EDITING_COMMANDS
    .iter()
    .find(|&&(s, _)| s == shortcut)
    .map_or(&[], |&(_, selectors)| selectors)
}

/// The editing commands Chromium's `Input.dispatchKeyEvent` takes as
/// `commands` for the key `code` pressed with `modifiers` (a CDP mask)
/// on macOS: the selectors without their trailing `:`, leaving out the
/// ones that insert text, which the key's own text already does.
#[must_use]
pub fn mac_editing_commands(code: &str, modifiers: u32) -> Vec<&'static str> {
  mac_editing_selectors(code, modifiers)
    .iter()
    .filter(|selector| !selector.starts_with("insert"))
    .map(|selector| selector.strip_suffix(':').unwrap_or(selector))
    .collect()
}

macro_rules! key {
  ($code:literal, $key:literal, $key_code:literal $(, $field:ident: $value:expr)* $(,)?) => {
    KeyDefinition {
      $($field: $value,)*
      ..KeyDefinition {
        code: $code,
        key: $key,
        key_code: $key_code,
        key_code_without_location: None,
        shift_key: None,
        shift_key_code: None,
        text: None,
        location: 0,
      }
    }
  };
}

/// The US keyboard, one entry per physical key.
#[rustfmt::skip]
pub const US_KEYBOARD_LAYOUT: &[KeyDefinition] = &[
  // Function row
  key!("Escape", "Escape", 27),
  key!("F1", "F1", 112),
  key!("F2", "F2", 113),
  key!("F3", "F3", 114),
  key!("F4", "F4", 115),
  key!("F5", "F5", 116),
  key!("F6", "F6", 117),
  key!("F7", "F7", 118),
  key!("F8", "F8", 119),
  key!("F9", "F9", 120),
  key!("F10", "F10", 121),
  key!("F11", "F11", 122),
  key!("F12", "F12", 123),

  // Number row
  key!("Backquote", "`", 192, shift_key: Some("~")),
  key!("Digit1", "1", 49, shift_key: Some("!")),
  key!("Digit2", "2", 50, shift_key: Some("@")),
  key!("Digit3", "3", 51, shift_key: Some("#")),
  key!("Digit4", "4", 52, shift_key: Some("$")),
  key!("Digit5", "5", 53, shift_key: Some("%")),
  key!("Digit6", "6", 54, shift_key: Some("^")),
  key!("Digit7", "7", 55, shift_key: Some("&")),
  key!("Digit8", "8", 56, shift_key: Some("*")),
  key!("Digit9", "9", 57, shift_key: Some("(")),
  key!("Digit0", "0", 48, shift_key: Some(")")),
  key!("Minus", "-", 189, shift_key: Some("_")),
  key!("Equal", "=", 187, shift_key: Some("+")),
  key!("Backslash", "\\", 220, shift_key: Some("|")),
  key!("Backspace", "Backspace", 8),

  // First letter row
  key!("Tab", "Tab", 9),
  key!("KeyQ", "q", 81, shift_key: Some("Q")),
  key!("KeyW", "w", 87, shift_key: Some("W")),
  key!("KeyE", "e", 69, shift_key: Some("E")),
  key!("KeyR", "r", 82, shift_key: Some("R")),
  key!("KeyT", "t", 84, shift_key: Some("T")),
  key!("KeyY", "y", 89, shift_key: Some("Y")),
  key!("KeyU", "u", 85, shift_key: Some("U")),
  key!("KeyI", "i", 73, shift_key: Some("I")),
  key!("KeyO", "o", 79, shift_key: Some("O")),
  key!("KeyP", "p", 80, shift_key: Some("P")),
  key!("BracketLeft", "[", 219, shift_key: Some("{")),
  key!("BracketRight", "]", 221, shift_key: Some("}")),

  // Second letter row
  key!("CapsLock", "CapsLock", 20),
  key!("KeyA", "a", 65, shift_key: Some("A")),
  key!("KeyS", "s", 83, shift_key: Some("S")),
  key!("KeyD", "d", 68, shift_key: Some("D")),
  key!("KeyF", "f", 70, shift_key: Some("F")),
  key!("KeyG", "g", 71, shift_key: Some("G")),
  key!("KeyH", "h", 72, shift_key: Some("H")),
  key!("KeyJ", "j", 74, shift_key: Some("J")),
  key!("KeyK", "k", 75, shift_key: Some("K")),
  key!("KeyL", "l", 76, shift_key: Some("L")),
  key!("Semicolon", ";", 186, shift_key: Some(":")),
  key!("Quote", "'", 222, shift_key: Some("\"")),
  key!("Enter", "Enter", 13, text: Some("\r")),

  // Third letter row
  key!("ShiftLeft", "Shift", 160, key_code_without_location: Some(16), location: 1),
  key!("KeyZ", "z", 90, shift_key: Some("Z")),
  key!("KeyX", "x", 88, shift_key: Some("X")),
  key!("KeyC", "c", 67, shift_key: Some("C")),
  key!("KeyV", "v", 86, shift_key: Some("V")),
  key!("KeyB", "b", 66, shift_key: Some("B")),
  key!("KeyN", "n", 78, shift_key: Some("N")),
  key!("KeyM", "m", 77, shift_key: Some("M")),
  key!("Comma", ",", 188, shift_key: Some("<")),
  key!("Period", ".", 190, shift_key: Some(">")),
  key!("Slash", "/", 191, shift_key: Some("?")),
  key!("ShiftRight", "Shift", 161, key_code_without_location: Some(16), location: 2),

  // Bottom row
  key!("ControlLeft", "Control", 162, key_code_without_location: Some(17), location: 1),
  key!("MetaLeft", "Meta", 91, location: 1),
  key!("AltLeft", "Alt", 164, key_code_without_location: Some(18), location: 1),
  key!("Space", " ", 32),
  key!("AltRight", "Alt", 165, key_code_without_location: Some(18), location: 2),
  key!("AltGraph", "AltGraph", 225),
  key!("MetaRight", "Meta", 92, location: 2),
  key!("ContextMenu", "ContextMenu", 93),
  key!("ControlRight", "Control", 163, key_code_without_location: Some(17), location: 2),

  // Centre block
  key!("PrintScreen", "PrintScreen", 44),
  key!("ScrollLock", "ScrollLock", 145),
  key!("Pause", "Pause", 19),
  key!("PageUp", "PageUp", 33),
  key!("PageDown", "PageDown", 34),
  key!("Insert", "Insert", 45),
  key!("Delete", "Delete", 46),
  key!("Home", "Home", 36),
  key!("End", "End", 35),
  key!("ArrowLeft", "ArrowLeft", 37),
  key!("ArrowUp", "ArrowUp", 38),
  key!("ArrowRight", "ArrowRight", 39),
  key!("ArrowDown", "ArrowDown", 40),

  // Media keys
  key!("AudioVolumeMute", "AudioVolumeMute", 173),
  key!("AudioVolumeDown", "AudioVolumeDown", 174),
  key!("AudioVolumeUp", "AudioVolumeUp", 175),
  key!("MediaTrackNext", "MediaTrackNext", 176),
  key!("MediaTrackPrevious", "MediaTrackPrevious", 177),
  key!("MediaPlayPause", "MediaPlayPause", 179),

  // Numeric keypad
  key!("NumLock", "NumLock", 144),
  key!("NumpadDivide", "/", 111, location: 3),
  key!("NumpadMultiply", "*", 106, location: 3),
  key!("NumpadSubtract", "-", 109, location: 3),
  key!("Numpad7", "Home", 36, shift_key: Some("7"), shift_key_code: Some(103), location: 3),
  key!("Numpad8", "ArrowUp", 38, shift_key: Some("8"), shift_key_code: Some(104), location: 3),
  key!("Numpad9", "PageUp", 33, shift_key: Some("9"), shift_key_code: Some(105), location: 3),
  key!("Numpad4", "ArrowLeft", 37, shift_key: Some("4"), shift_key_code: Some(100), location: 3),
  key!("Numpad5", "Clear", 12, shift_key: Some("5"), shift_key_code: Some(101), location: 3),
  key!("Numpad6", "ArrowRight", 39, shift_key: Some("6"), shift_key_code: Some(102), location: 3),
  key!("NumpadAdd", "+", 107, location: 3),
  key!("Numpad1", "End", 35, shift_key: Some("1"), shift_key_code: Some(97), location: 3),
  key!("Numpad2", "ArrowDown", 40, shift_key: Some("2"), shift_key_code: Some(98), location: 3),
  key!("Numpad3", "PageDown", 34, shift_key: Some("3"), shift_key_code: Some(99), location: 3),
  key!("Numpad0", "Insert", 45, shift_key: Some("0"), shift_key_code: Some(96), location: 3),
  key!("NumpadDecimal", "Delete", 46, shift_key: Some("."), shift_key_code: Some(110), location: 3),
  key!("NumpadEnter", "Enter", 13, text: Some("\r"), location: 3),
];

/// macOS key bindings, `(shortcut, selectors)`, keyed as
/// [`editing_shortcut`] builds them.
#[rustfmt::skip]
const MAC_EDITING_COMMANDS: &[(&str, &[&str])] = &[
  ("Backspace", &["deleteBackward:"]),
  ("Enter", &["insertNewline:"]),
  ("NumpadEnter", &["insertNewline:"]),
  ("Escape", &["cancelOperation:"]),
  ("ArrowUp", &["moveUp:"]),
  ("ArrowDown", &["moveDown:"]),
  ("ArrowLeft", &["moveLeft:"]),
  ("ArrowRight", &["moveRight:"]),
  ("F5", &["complete:"]),
  ("Delete", &["deleteForward:"]),
  ("Home", &["scrollToBeginningOfDocument:"]),
  ("End", &["scrollToEndOfDocument:"]),
  ("PageUp", &["scrollPageUp:"]),
  ("PageDown", &["scrollPageDown:"]),
  ("Shift+Backspace", &["deleteBackward:"]),
  ("Shift+Enter", &["insertNewline:"]),
  ("Shift+NumpadEnter", &["insertNewline:"]),
  ("Shift+Escape", &["cancelOperation:"]),
  ("Shift+ArrowUp", &["moveUpAndModifySelection:"]),
  ("Shift+ArrowDown", &["moveDownAndModifySelection:"]),
  ("Shift+ArrowLeft", &["moveLeftAndModifySelection:"]),
  ("Shift+ArrowRight", &["moveRightAndModifySelection:"]),
  ("Shift+F5", &["complete:"]),
  ("Shift+Delete", &["deleteForward:"]),
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

#[cfg(test)]
mod tests {
  use super::*;

  fn key(name: &str, shift: bool) -> KeyDescription {
    describe(name, shift).unwrap()
  }

  #[test]
  fn letters_and_digits_carry_their_virtual_key_code() {
    let a = key("a", false);
    assert_eq!(
      (a.key, a.code, a.key_code_without_location, a.text, a.location),
      ("a", "KeyA", 65, "a", 0)
    );
    let one = key("1", false);
    assert_eq!((one.key, one.code, one.key_code_without_location), ("1", "Digit1", 49));
  }

  #[test]
  fn shift_changes_a_key_named_by_its_code() {
    let a = key("KeyA", true);
    assert_eq!((a.key, a.text, a.code, a.key_code), ("A", "A", "KeyA", 65));
    let bang = key("Digit1", true);
    assert_eq!((bang.key, bang.text, bang.code), ("!", "!", "Digit1"));
    assert_eq!(key("KeyA", false).key, "a");
  }

  #[test]
  fn a_key_named_by_its_character_ignores_shift() {
    assert_eq!(key("a", true).key, "a");
    let upper = key("A", false);
    assert_eq!((upper.key, upper.text, upper.code), ("A", "A", "KeyA"));
    assert_eq!(key("!", true).code, "Digit1");
  }

  #[test]
  fn modifiers_and_keypad_keys_report_their_location() {
    let shift = key("Shift", false);
    assert_eq!(
      (
        shift.code,
        shift.key_code,
        shift.key_code_without_location,
        shift.location
      ),
      ("ShiftLeft", 160, 16, 1)
    );
    let right = key("ShiftRight", false);
    assert_eq!(
      (right.key, right.code, right.key_code_without_location, right.location),
      ("Shift", "ShiftRight", 16, 2)
    );
    assert_eq!(key("MetaRight", false).key_code_without_location, 92);
    let end = key("Numpad1", false);
    assert_eq!(
      (end.key, end.key_code, end.location, end.is_keypad()),
      ("End", 35, 3, true)
    );
    let one = key("Numpad1", true);
    assert_eq!((one.key, one.text, one.key_code_without_location), ("1", "1", 97));
    assert_eq!(key("/", false).code, "Slash");
  }

  #[test]
  fn named_keys_type_only_the_text_they_define() {
    assert_eq!(key("Enter", false).text, "\r");
    assert_eq!(key("\n", false).code, "Enter");
    assert_eq!(key("Tab", false).text, "");
    assert_eq!(key(" ", false).code, "Space");
    assert_eq!(key("Space", false).text, " ");
    assert_eq!(describe("ControlOrMeta", false), None);
    assert_eq!(describe("Hyper", false), None);
  }

  #[test]
  fn every_name_resolves_to_one_key() {
    for definition in US_KEYBOARD_LAYOUT {
      assert_eq!(key(definition.code, false).code, definition.code);
      if definition.location == 0 {
        for name in [Some(definition.key), definition.shift_key].into_iter().flatten() {
          if name.chars().count() == 1 {
            let matches = US_KEYBOARD_LAYOUT
              .iter()
              .filter(|d| d.location == 0 && (d.key == name || d.shift_key == Some(name)))
              .count();
            assert_eq!(matches, 1, "{name:?} names one key");
          }
        }
      }
    }
  }

  #[test]
  fn held_shift_shifts_and_held_control_silences_text() {
    let mut keyboard = Keyboard::new();
    let shift = keyboard.down("Shift", false).unwrap();
    assert_eq!(shift.modifiers, 8);
    let a = keyboard.down("KeyA", false).unwrap();
    assert_eq!((a.description.key, a.description.text, a.modifiers), ("A", "A", 8));
    keyboard.up("KeyA", false).unwrap();
    let released = keyboard.up("Shift", false).unwrap();
    assert_eq!(released.modifiers, 0);

    let control = keyboard.down("Control", false).unwrap();
    assert_eq!(control.modifiers, 2);
    let a = keyboard.down("a", false).unwrap();
    assert_eq!((a.description.key, a.description.text, a.modifiers), ("a", "", 2));
  }

  #[test]
  fn a_second_down_of_a_held_key_repeats() {
    let mut keyboard = Keyboard::new();
    assert!(!keyboard.down("a", false).unwrap().auto_repeat);
    assert!(keyboard.down("KeyA", false).unwrap().auto_repeat);
    keyboard.up("a", false).unwrap();
    assert!(!keyboard.down("a", false).unwrap().auto_repeat);
  }

  #[test]
  fn control_or_meta_follows_the_platform() {
    let mut keyboard = Keyboard::new();
    let mac = keyboard.down("ControlOrMeta", true).unwrap();
    assert_eq!((mac.description.key, mac.modifiers), ("Meta", 4));
    keyboard.up("ControlOrMeta", true).unwrap();
    let other = keyboard.down("ControlOrMeta", false).unwrap();
    assert_eq!((other.description.key, other.modifiers), ("Control", 2));
  }

  #[test]
  fn an_unknown_key_changes_nothing() {
    let mut keyboard = Keyboard::new();
    keyboard.down("Shift", false).unwrap();
    let error = keyboard.down("Hyper", false).err();
    assert_eq!(error.map(|e| e.to_string()), Some("Unknown key: \"Hyper\"".to_string()));
    assert_eq!(keyboard.modifiers(), 8);
  }

  #[test]
  fn editing_commands_follow_the_shortcut() {
    assert_eq!(mac_editing_commands("KeyA", Modifier::Meta.bit()), vec!["selectAll"]);
    assert_eq!(mac_editing_commands("KeyA", 0), Vec::<&str>::new());
    assert_eq!(
      mac_editing_commands("ArrowLeft", Modifier::Shift.bit() | Modifier::Alt.bit()),
      vec!["moveWordLeftAndModifySelection"]
    );
    assert_eq!(
      mac_editing_commands("KeyO", Modifier::Control.bit()),
      vec!["moveBackward"],
      "a command that inserts text is left to the key's text"
    );
    assert_eq!(mac_editing_commands("Enter", 0), Vec::<&str>::new());
    assert_eq!(mac_editing_selectors("Enter", 0), &["insertNewline:"]);
    assert_eq!(
      mac_editing_selectors(
        "KeyB",
        Modifier::Shift.bit() | Modifier::Control.bit() | Modifier::Alt.bit()
      ),
      &["moveWordBackwardAndModifySelection:"]
    );
  }
}
