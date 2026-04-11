//! Built-in step definitions for browser automation.
//!
//! All steps use cucumber expressions and operate on `BrowserWorld`.
//! They are registered via `#[given]`, `#[when]`, `#[then]` proc macros.

pub mod api;
pub mod assertion;
pub mod cookie;
pub mod dialog;
pub mod emulation;
pub mod file;
pub mod frame;
pub mod interaction;
pub mod javascript;
pub mod keyboard;
pub mod mouse;
pub mod navigation;
pub mod network;
pub mod screenshot;
pub mod storage;
pub mod variable;
pub mod wait;
pub mod window;
