//! Backend-agnostic page event sink interface.
//!
//! Protocol backends translate wire notifications (CDP, `BiDi`, `WebKit`, `WebDriver`)
//! into these domain-neutral signals without knowing how the core `Page` wrapper
//! stores or broadcasts them.

use crate::FrameInfo;

/// Page-level lifecycle notifications emitted by every backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageLifecycleSignal {
  DomContentLoaded,
  Load,
  Close,
  Crash,
}

/// Frame-tree notifications emitted by every backend.
#[derive(Debug, Clone)]
pub enum FrameSignal {
  Attached(FrameInfo),
  Navigated(FrameInfo),
  NavigatedWithinDocument(FrameInfo),
  Detached { frame_id: String },
}

/// Sink implemented by core page state to receive lifecycle and frame events
/// from protocol listeners without creating an upward dependency on `Page`.
pub trait PageEventSink: Send + Sync {
  fn on_lifecycle(&self, signal: PageLifecycleSignal);
  fn on_frame(&self, signal: FrameSignal);
}
