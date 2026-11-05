//! The keyboard a page keeps between input calls.
//!
//! Every backend page holds one [`KeyboardState`]. Key events are
//! described here, once, from the held keys; a backend only turns a
//! [`KeyStroke`] into its protocol's key event and stamps
//! [`KeyboardState::modifiers`] on its mouse and touch events, so a
//! modifier held by `keyboard.down` applies to everything that follows.

use ferridriver_types::keyboard::{KeyStroke, Keyboard};

use crate::error::{FerriError, Result};

/// Whether `ControlOrMeta` means Meta and Chromium needs the macOS
/// editing commands: the platform ferridriver runs on.
pub(crate) const MAC: bool = cfg!(target_os = "macos");

#[derive(Debug, Default)]
pub(crate) struct KeyboardState(std::sync::Mutex<Keyboard>);

impl KeyboardState {
  fn keyboard(&self) -> std::sync::MutexGuard<'_, Keyboard> {
    self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  pub(crate) fn down(&self, key: &str) -> Result<KeyStroke> {
    self.keyboard().down(key, MAC).map_err(|e| unknown_key(&e))
  }

  pub(crate) fn up(&self, key: &str) -> Result<KeyStroke> {
    self.keyboard().up(key, MAC).map_err(|e| unknown_key(&e))
  }

  /// The CDP mask (Alt=1, Control=2, Meta=4, Shift=8) of the held
  /// modifiers.
  pub(crate) fn modifiers(&self) -> u32 {
    self.keyboard().modifiers()
  }
}

fn unknown_key(error: &ferridriver_types::keyboard::UnknownKey) -> FerriError {
  FerriError::invalid_argument("key", error.to_string())
}
