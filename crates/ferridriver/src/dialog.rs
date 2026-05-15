//! `Dialog` — live handle for JavaScript dialogs (`alert`, `confirm`,
//! `prompt`, `beforeunload`).
//!
//! Mirrors Playwright's client-side `Dialog` from
//! `/tmp/playwright/packages/playwright-core/src/client/dialog.ts` and
//! its server-side lifecycle from
//! `/tmp/playwright/packages/playwright-core/src/server/dialog.ts`.
//!
//! Usage:
//!
//! ```ignore
//! page.on("dialog", Arc::new(|event: PageEvent| {
//!     if let PageEvent::Dialog(d) = event {
//!         let _ = tokio::spawn(async move { let _ = d.accept(None).await; });
//!     }
//! }));
//! ```
//!
//! Lifecycle rules (Playwright-faithful):
//!
//! * When a dialog opens, the backend's dialog listener constructs a
//!   [`Dialog`] and emits it as [`crate::events::PageEvent::Dialog`].
//!   JavaScript execution in the page is paused until the dialog is
//!   handled.
//! * If any `dialog` listener is registered on the page (tracked by
//!   [`crate::events::EventEmitter::has_listener`]), the listener is
//!   expected to call [`Dialog::accept`] or [`Dialog::dismiss`] exactly
//!   once. Calling either twice returns an error.
//! * If no listener is registered at open-time, the dialog is
//!   auto-closed: `beforeunload` is **accepted** (matches Playwright's
//!   `DialogManager.dialogDidOpen` + `Dialog._close`), every other type
//!   is **dismissed** (so tests don't hang on stray `alert()`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::error::{FerriError, Result};

/// Playwright-compatible dialog type. Mirrors the `DialogType` union
/// from `/tmp/playwright/packages/playwright-core/src/server/dialog.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogType {
  Alert,
  BeforeUnload,
  Confirm,
  Prompt,
}

impl DialogType {
  /// Parse a backend-reported dialog type string. Unknown values fall
  /// back to `Alert` so callers always see a valid enum — the actual
  /// protocol strings are small and well-known.
  #[must_use]
  pub fn parse(s: &str) -> Self {
    match s {
      "beforeunload" => Self::BeforeUnload,
      "confirm" => Self::Confirm,
      "prompt" => Self::Prompt,
      _ => Self::Alert,
    }
  }

  /// Playwright's string form: `"alert"` / `"beforeunload"` /
  /// `"confirm"` / `"prompt"`.
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Alert => "alert",
      Self::BeforeUnload => "beforeunload",
      Self::Confirm => "confirm",
      Self::Prompt => "prompt",
    }
  }
}

/// Response action the backend's listener performs when the user
/// accepts or dismisses the dialog. The backend provides the
/// [`DialogResponder`] closure that translates this into the
/// protocol-specific command (CDP `Page.handleJavaScriptDialog`, `BiDi`
/// `browsingContext.handleUserPrompt`, `WebKit` IPC `RespondDialog`).
pub enum DialogResponse {
  Accept { prompt_text: Option<String> },
  Dismiss,
}

/// Backend-supplied async responder. Returns `Ok(())` on success; an
/// `Err(String)` propagates back through [`Dialog::accept`] /
/// [`Dialog::dismiss`] as [`FerriError::Backend`].
pub type DialogResponder = Arc<
  dyn Fn(DialogResponse) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::result::Result<(), String>> + Send>>
    + Send
    + Sync,
>;

/// Live dialog handle. Cheaply cloneable — every clone shares the
/// same one-shot responder. Mirrors Playwright's `Dialog` client
/// class.
#[derive(Clone)]
pub struct Dialog {
  pub(crate) inner: Arc<DialogState>,
}

pub(crate) struct DialogState {
  dialog_type: DialogType,
  message: String,
  default_value: String,
  /// `true` once `accept` or `dismiss` has run. Subsequent calls
  /// return an error to match Playwright's assertion semantics.
  handled: AtomicBool,
  responder: DialogResponder,
  /// Back-reference to the [`DialogManager`] that emitted this
  /// dialog. Used to notify the manager via
  /// [`DialogManager::dialog_will_close`] when `accept` / `dismiss`
  /// runs, so the manager's open-set stays accurate. Optional so
  /// ad-hoc `Dialog::new` callers (e.g. the `WebKit` placeholder
  /// responder) don't need a manager reference.
  manager: Option<DialogManager>,
}

impl Dialog {
  /// Construct a new dialog handle. Called by backend dialog
  /// listeners; user code receives already-constructed `Dialog`s via
  /// the page's [`DialogManager`] handler.
  #[must_use]
  pub fn new(dialog_type: DialogType, message: String, default_value: String, responder: DialogResponder) -> Self {
    Self::new_with_manager(dialog_type, message, default_value, responder, None)
  }

  /// Variant of [`Self::new`] that binds the dialog to a
  /// [`DialogManager`], so [`Self::accept`] / [`Self::dismiss`] notify
  /// the manager's open-set via
  /// [`DialogManager::dialog_will_close`]. Backend listeners use this
  /// form; ad-hoc constructions (placeholders, tests) use the
  /// unbound form.
  #[must_use]
  pub fn new_with_manager(
    dialog_type: DialogType,
    message: String,
    default_value: String,
    responder: DialogResponder,
    manager: Option<DialogManager>,
  ) -> Self {
    Self {
      inner: Arc::new(DialogState {
        dialog_type,
        message,
        default_value,
        handled: AtomicBool::new(false),
        responder,
        manager,
      }),
    }
  }

  /// Dialog type. Playwright: `dialog.type(): string`.
  #[must_use]
  pub fn dialog_type(&self) -> DialogType {
    self.inner.dialog_type
  }

  /// Message text shown in the dialog. Playwright: `dialog.message(): string`.
  #[must_use]
  pub fn message(&self) -> &str {
    &self.inner.message
  }

  /// Default value for prompts (empty for other types). Playwright:
  /// `dialog.defaultValue(): string`.
  #[must_use]
  pub fn default_value(&self) -> &str {
    &self.inner.default_value
  }

  /// Whether this dialog has already been accepted or dismissed.
  /// Second accept/dismiss calls error instead of firing a second
  /// protocol request.
  #[must_use]
  pub fn is_handled(&self) -> bool {
    self.inner.handled.load(Ordering::Acquire)
  }

  /// Accept the dialog, optionally with prompt text for
  /// `prompt` dialogs. Playwright: `dialog.accept(promptText?): Promise<void>`.
  ///
  /// # Errors
  ///
  /// * Returns [`FerriError::Backend`] with the assertion message
  ///   `"Cannot accept dialog which is already handled!"` if called
  ///   after the dialog was already accepted or dismissed.
  /// * Returns [`FerriError::Backend`] wrapping the backend error if the
  ///   underlying protocol call fails.
  pub async fn accept(&self, prompt_text: Option<String>) -> Result<()> {
    self.mark_handled_or_error()?;
    if let Some(mgr) = &self.inner.manager {
      mgr.dialog_will_close(self);
    }
    (self.inner.responder)(DialogResponse::Accept { prompt_text })
      .await
      .map_err(FerriError::Backend)
  }

  /// Dismiss the dialog. Playwright: `dialog.dismiss(): Promise<void>`.
  ///
  /// # Errors
  ///
  /// See [`Self::accept`].
  pub async fn dismiss(&self) -> Result<()> {
    self.mark_handled_or_error()?;
    if let Some(mgr) = &self.inner.manager {
      mgr.dialog_will_close(self);
    }
    (self.inner.responder)(DialogResponse::Dismiss)
      .await
      .map_err(FerriError::Backend)
  }

  /// Backend-internal: auto-close the dialog when no listener is
  /// registered. Per Playwright's
  /// `DialogManager.dialogDidOpen::hasHandlers === false` branch +
  /// `Dialog._close`: `beforeunload` auto-accepts so navigation
  /// proceeds; every other type auto-dismisses.
  ///
  /// Swallows errors — the dialog is about to close one way or
  /// another and there's no caller to propagate to. Callers still
  /// race against `is_handled`.
  pub(crate) async fn auto_close(&self) {
    if self.inner.handled.swap(true, Ordering::AcqRel) {
      return;
    }
    let response = if matches!(self.inner.dialog_type, DialogType::BeforeUnload) {
      DialogResponse::Accept { prompt_text: None }
    } else {
      DialogResponse::Dismiss
    };
    let _ = (self.inner.responder)(response).await;
  }

  fn mark_handled_or_error(&self) -> Result<()> {
    if self.inner.handled.swap(true, Ordering::AcqRel) {
      return Err(FerriError::Backend(
        "Cannot accept dialog which is already handled!".into(),
      ));
    }
    Ok(())
  }
}

impl std::fmt::Debug for Dialog {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Dialog")
      .field("type", &self.inner.dialog_type.as_str())
      .field("message", &self.inner.message)
      .field("default_value", &self.inner.default_value)
      .field("handled", &self.is_handled())
      .finish()
  }
}

// ── DialogManager ─────────────────────────────────────────────────────

/// Opaque id returned by [`DialogManager::add_handler`] and consumed by
/// [`DialogManager::remove_handler`]. Monotonically increasing per
/// manager — no collisions within the same page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DialogHandlerId(pub u64);

/// Synchronous dialog-ownership predicate. Mirrors the server-side
/// handler signature from
/// `/tmp/playwright/packages/playwright-core/src/server/dialog.ts:121`
/// (`addDialogHandler(handler: (dialog: Dialog) => boolean)`). The
/// handler is called with the freshly-opened dialog and returns
/// `true` if it is going to handle the dialog (i.e. will call
/// `accept` / `dismiss` eventually, possibly asynchronously), or
/// `false` to pass.
pub type DialogHandlerFn = Arc<dyn Fn(&Dialog) -> bool + Send + Sync>;

struct DialogHandlerEntry {
  id: u64,
  handler: DialogHandlerFn,
}

/// Per-page (in Playwright: per-browser-context) dialog handler
/// registry. Mirrors
/// `/tmp/playwright/packages/playwright-core/src/server/dialog.ts::DialogManager`
/// down to method names and semantics.
///
/// The backend's dialog listener constructs a [`Dialog`] when a
/// `javascriptDialogOpening` / `userPromptOpened` event arrives, then
/// synchronously calls [`Self::did_open`]. That method iterates every
/// registered handler in insertion order, calls each with the dialog,
/// and checks if ANY returned `true`. If none did, the manager calls
/// `Dialog::auto_close` on a detached task — accept for
/// `beforeunload`, dismiss otherwise, matching Playwright's
/// `Dialog._close`.
///
/// Handlers that return `true` are promising to drive
/// `accept` / `dismiss` themselves (potentially from an async task),
/// so the manager does nothing further.
#[derive(Clone, Default)]
pub struct DialogManager {
  inner: Arc<DialogManagerState>,
}

#[derive(Default)]
struct DialogManagerState {
  handlers: std::sync::Mutex<Vec<DialogHandlerEntry>>,
  next_id: AtomicU64,
  open: std::sync::Mutex<Vec<Dialog>>,
}

impl DialogManager {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Register a dialog handler. Returns a [`DialogHandlerId`] for
  /// later removal via [`Self::remove_handler`]. Mirrors Playwright's
  /// `addDialogHandler` — the id-based API avoids requiring callers
  /// to hold the same `Arc` instance to remove the handler, which
  /// matters for NAPI callbacks where the `Arc` crosses the JS
  /// boundary as opaque state.
  pub fn add_handler(&self, handler: DialogHandlerFn) -> DialogHandlerId {
    let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut handlers) = self.inner.handlers.lock() {
      handlers.push(DialogHandlerEntry { id, handler });
    }
    DialogHandlerId(id)
  }

  /// Remove a previously-registered dialog handler. Matches
  /// Playwright's `removeDialogHandler` — and, like Playwright's
  /// version, if the last handler is removed while dialogs are open,
  /// those open dialogs are auto-closed (so a listener torn down
  /// before it handled doesn't leave the page hanging).
  pub fn remove_handler(&self, id: DialogHandlerId) {
    let becomes_empty = if let Ok(mut handlers) = self.inner.handlers.lock() {
      handlers.retain(|h| h.id != id.0);
      handlers.is_empty()
    } else {
      false
    };
    if becomes_empty {
      let drained: Vec<Dialog> = match self.inner.open.lock() {
        Ok(mut g) => std::mem::take(&mut *g),
        Err(_) => Vec::new(),
      };
      for dialog in drained {
        tokio::spawn(async move {
          dialog.auto_close().await;
        });
      }
    }
  }

  /// Called by the backend when a dialog opens. Iterates every
  /// registered handler synchronously and asks each "do you claim
  /// this dialog?". If no handler returns `true`, spawns a task to
  /// auto-close the dialog — accept for `beforeunload` (so
  /// `navigation` proceeds), dismiss otherwise. Mirrors Playwright's
  /// `DialogManager.dialogDidOpen` exactly.
  pub fn did_open(&self, dialog: Dialog) {
    let handlers: Vec<DialogHandlerFn> = match self.inner.handlers.lock() {
      Ok(g) => g.iter().map(|e| Arc::clone(&e.handler)).collect(),
      Err(_) => Vec::new(),
    };
    let mut claimed = false;
    for h in handlers {
      if h(&dialog) {
        claimed = true;
      }
    }
    if claimed {
      if let Ok(mut open) = self.inner.open.lock() {
        open.push(dialog);
      }
    } else {
      // Detach the auto-close — the listener task stays non-blocking.
      tokio::spawn(async move {
        dialog.auto_close().await;
      });
    }
  }

  /// Called by [`Dialog::accept`] / [`Dialog::dismiss`] via a
  /// scheduled notification — removes the dialog from the open set so
  /// `remove_handler` / `has_open_dialogs` stay accurate. Backends
  /// that don't call through [`Dialog::accept`] (e.g. auto-close via
  /// `Dialog::auto_close`) skip this; the open set is informational
  /// and not load-bearing.
  pub fn dialog_will_close(&self, dialog: &Dialog) {
    if let Ok(mut open) = self.inner.open.lock() {
      open.retain(|d| !Arc::ptr_eq(&d.inner, &dialog.inner));
    }
  }

  /// Number of currently-open (claimed but not yet handled) dialogs.
  /// Used by tests and by [`Self::remove_handler`]'s drain path.
  #[must_use]
  pub fn open_dialog_count(&self) -> usize {
    self.inner.open.lock().map_or(0, |g| g.len())
  }

  /// Register the default emitter-bridge handler: every page installs
  /// one at construction time so `page.events().on("dialog", cb)`
  /// continues to deliver live [`Dialog`] handles to broadcast
  /// listeners. The handler inspects the emitter's named-listener
  /// count via [`crate::events::EventEmitter::has_listener`]:
  ///
  /// * **When a named `dialog` listener is registered**, the handler
  ///   emits [`crate::events::PageEvent::Dialog`] on the broadcast
  ///   and returns `true`, claiming ownership synchronously. The
  ///   downstream listener task runs asynchronously and eventually
  ///   calls `dialog.accept(...)` or `dialog.dismiss()`.
  /// * **When no named listener is present**, the handler returns
  ///   `false`. Any handler registered directly via
  ///   [`Self::add_handler`] (e.g. one-shot listener installed by
  ///   [`crate::page::Page::wait_for_dialog`]) may still claim the
  ///   dialog. If nobody claims, [`Self::did_open`] auto-closes.
  #[must_use]
  pub fn register_emitter_bridge(&self, events: crate::events::EventEmitter) -> DialogHandlerId {
    self.add_handler(Arc::new(move |dialog: &Dialog| {
      if events.has_listener("dialog") {
        events.emit(crate::events::PageEvent::Dialog(dialog.clone()));
        true
      } else {
        false
      }
    }))
  }
}
