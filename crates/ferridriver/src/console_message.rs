//! `ConsoleMessage` — live handle for page-side `console.*` calls.
//!
//! Mirrors Playwright's client-side `ConsoleMessage` from
//! `/tmp/playwright/packages/playwright-core/src/client/consoleMessage.ts`
//! and its server-side shape from
//! `/tmp/playwright/packages/playwright-core/src/server/console.ts`.
//!
//! Replaces the wire-shaped `ConsoleMsg { type, text }` that previously
//! rode through `PageEvent::Console`. That struct leaked the
//! intermediate representation directly to user-facing API (Rule 3) —
//! `ConsoleMessage` instead carries a live `Vec<JSHandle>` for `args()`,
//! a `{url, line, column}` `ConsoleMessageLocation`, the owning
//! `Arc<Page>` (weak), and a timestamp.
//!
//! Usage:
//!
//! ```ignore
//! page.on("console", Arc::new(|event| {
//!     if let PageEvent::Console(msg) = event {
//!         println!("[{}] {}", msg.type_str(), msg.text());
//!         for arg in msg.args() { /* inspect */ }
//!     }
//! }));
//! ```
//!
//! Lifecycle rules (Playwright-faithful):
//!
//! * When the page fires a `console.*` call, the backend's console
//!   listener builds a live `ConsoleMessage`, stores it in the
//!   per-context console log, and emits it as `PageEvent::Console`.
//! * `args` is `Vec<JSHandle>` — each arg is either a remote-backed
//!   handle (object / array / element / function) or a value-backed
//!   handle (primitive), matching Playwright's dual `JSHandle` shape.
//! * `text()` lazily falls back to `args.map(jsonValue).join(' ')` when
//!   no explicit text was reported by the protocol — matches Playwright's
//!   `server/console.ts::text()` lazy getter.

use std::sync::Arc;

use crate::js_handle::JSHandle;
use crate::page::Page;

/// `{ url, lineNumber, columnNumber }` source location of the
/// `console.*` call. Matches Playwright's `ConsoleMessageLocation`
/// (`/tmp/playwright/packages/playwright-core/src/server/types.ts:169`).
/// Defaults to `{ "", 0, 0 }` when the protocol doesn't surface a
/// stack trace.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ConsoleMessageLocation {
  pub url: String,
  #[serde(rename = "lineNumber")]
  pub line_number: u32,
  #[serde(rename = "columnNumber")]
  pub column_number: u32,
}

/// Live console-message handle. Cheaply cloneable — every clone
/// shares the same arg handles. Mirrors Playwright's `ConsoleMessage`
/// client class.
#[derive(Clone)]
pub struct ConsoleMessage {
  inner: Arc<ConsoleMessageState>,
}

struct ConsoleMessageState {
  /// `'log' | 'debug' | 'info' | 'error' | 'warning' | 'dir' | ...`
  /// Playwright's `ConsoleMessage.type()` string. `BiDi` reports
  /// `'warn'` which the backend listener remaps to `'warning'`
  /// (matches Playwright's `bidiPage.ts::_onLogEntryAdded`).
  type_str: String,
  /// Explicit text reported by the protocol (CDP doesn't populate
  /// this; `BiDi`'s `timeLog` / `timeEnd` do). `None` means "derive
  /// from args" — `text()` lazily joins `args.map(jsonValue)`.
  text_override: std::sync::OnceLock<String>,
  explicit_text: Option<String>,
  args: Vec<JSHandle>,
  location: ConsoleMessageLocation,
  timestamp: u64,
  /// Weak back-reference to the owning page. `ConsoleMessage::page`
  /// upgrades it; returns `None` if the page has been dropped.
  page: std::sync::Weak<Page>,
}

impl ConsoleMessage {
  /// Construct a new console message. Called by backend console
  /// listeners; user code receives already-built `ConsoleMessage`s via
  /// `page.on("console", cb)` / `page.waitForEvent("console")`.
  #[must_use]
  pub fn new(
    page: &Arc<Page>,
    type_str: impl Into<String>,
    text: Option<String>,
    args: Vec<JSHandle>,
    location: ConsoleMessageLocation,
    timestamp: u64,
  ) -> Self {
    Self {
      inner: Arc::new(ConsoleMessageState {
        type_str: type_str.into(),
        text_override: std::sync::OnceLock::new(),
        explicit_text: text,
        args,
        location,
        timestamp,
        page: Arc::downgrade(page),
      }),
    }
  }

  /// Ad-hoc constructor for cases where the owning page isn't
  /// available at build time (worker-scoped consoles, tests). The weak
  /// back-reference is empty; `page()` returns `None`.
  #[must_use]
  pub fn new_detached(
    type_str: impl Into<String>,
    text: Option<String>,
    args: Vec<JSHandle>,
    location: ConsoleMessageLocation,
    timestamp: u64,
  ) -> Self {
    Self {
      inner: Arc::new(ConsoleMessageState {
        type_str: type_str.into(),
        text_override: std::sync::OnceLock::new(),
        explicit_text: text,
        args,
        location,
        timestamp,
        page: std::sync::Weak::new(),
      }),
    }
  }

  /// Console-message kind. Playwright: `ConsoleMessage.type(): string`.
  /// The raw string (`"log"`, `"info"`, `"error"`, `"warning"`, ...)
  /// matching Playwright's `ConsoleMessageType` union.
  #[must_use]
  pub fn type_str(&self) -> &str {
    &self.inner.type_str
  }

  /// Text body. Playwright: `ConsoleMessage.text(): string`. If the
  /// protocol reported explicit text, returns it; otherwise lazily
  /// renders `args.map(jsonValue).join(' ')` and caches the result
  /// (matches `server/console.ts::text()` lazy getter).
  #[must_use]
  pub fn text(&self) -> &str {
    if let Some(ref s) = self.inner.explicit_text {
      return s.as_str();
    }
    self
      .inner
      .text_override
      .get_or_init(|| self.inner.args.iter().map(preview_arg).collect::<Vec<_>>().join(" "))
      .as_str()
  }

  /// Live `JSHandle` args. Playwright: `ConsoleMessage.args(): JSHandle[]`.
  #[must_use]
  pub fn args(&self) -> &[JSHandle] {
    &self.inner.args
  }

  /// Source location of the `console.*` call site. Playwright:
  /// `ConsoleMessage.location(): { url, lineNumber, columnNumber }`.
  #[must_use]
  pub fn location(&self) -> &ConsoleMessageLocation {
    &self.inner.location
  }

  /// Owning page (weak). Returns `None` if the page has been dropped.
  /// Playwright: `ConsoleMessage.page(): Page | null`.
  #[must_use]
  pub fn page(&self) -> Option<Arc<Page>> {
    self.inner.page.upgrade()
  }

  /// Wall-clock timestamp (milliseconds since epoch) reported by the
  /// protocol. Playwright: `ConsoleMessage.timestamp(): number`.
  #[must_use]
  pub fn timestamp(&self) -> u64 {
    self.inner.timestamp
  }

  /// The `args: [{ preview, value }]` array the trace's `console`
  /// event carries (Playwright's `tracing.ts::_onConsoleMessage`:
  /// `args: message.args().map(a => ({ preview: a.toString(), value:
  /// a.rawValue() }))`). `preview` is the handle's string preview;
  /// `value` is the raw serialized value for a primitive-backed handle
  /// and `null` for a remote (object) handle — the recorder runs on
  /// the synchronous event path and must not issue a protocol
  /// round-trip to serialize a live object.
  #[must_use]
  pub fn trace_args(&self) -> Vec<serde_json::Value> {
    self
      .inner
      .args
      .iter()
      .map(|h| {
        let value = match h.backing() {
          crate::js_handle::JSHandleBacking::Value(v) => v.to_json_like().unwrap_or(serde_json::Value::Null),
          crate::js_handle::JSHandleBacking::Remote(_) => serde_json::Value::Null,
        };
        serde_json::json!({ "preview": preview_arg(h), "value": value })
      })
      .collect()
  }
}

impl std::fmt::Debug for ConsoleMessage {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ConsoleMessage")
      .field("type", &self.inner.type_str)
      .field("text", &self.text())
      .field("args_count", &self.inner.args.len())
      .field("location", &self.inner.location)
      .field("timestamp", &self.inner.timestamp)
      .finish()
  }
}

/// Stringify a `JSHandle` for the text-fallback join. Value-backed
/// handles render their inline primitive via
/// [`crate::protocol::SerializedValue`]'s `to_json`; remote-backed
/// handles render as `[object Object]` / `[object Array]` / etc. —
/// matches Playwright's `server/console.ts::text()` which calls
/// `JSHandle.preview()` (stringified `Object.prototype.toString` on
/// remote objects).
fn preview_arg(h: &JSHandle) -> String {
  match h.backing() {
    crate::js_handle::JSHandleBacking::Value(v) => match v.to_json_like() {
      Some(serde_json::Value::Null) => "null".to_string(),
      Some(serde_json::Value::Bool(b)) => b.to_string(),
      Some(serde_json::Value::Number(n)) => n.to_string(),
      Some(serde_json::Value::String(s)) => s,
      Some(other) => other.to_string(),
      None => "JSHandle@primitive".to_string(),
    },
    crate::js_handle::JSHandleBacking::Remote(_) => "JSHandle@object".to_string(),
  }
}
