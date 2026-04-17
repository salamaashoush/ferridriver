#![allow(
  clippy::missing_errors_doc,
  clippy::missing_panics_doc,
  clippy::must_use_candidate,
  clippy::module_name_repetitions,
  clippy::cast_possible_truncation,
  clippy::cast_precision_loss,
  clippy::cast_sign_loss,
  clippy::too_many_lines,
  clippy::uninlined_format_args,
  clippy::needless_pass_by_value,
  clippy::doc_markdown,
  clippy::missing_fields_in_debug,
  // rquickjs method wrappers intentionally produce new Locator instances that
  // JS is free to discard (e.g. fluent chains like `loc.nth(0)` used directly).
  clippy::return_self_not_must_use
)]
//! ferridriver-script: sandboxed `QuickJS` scripting engine.
//!
//! Exposes a `ScriptEngine` that runs user-provided JS against ferridriver's
//! Page/Browser/Context API with:
//!
//! - Per-call context isolation (fresh `rquickjs::Context` per `run`).
//! - Bound args (never interpolated into source) to prevent prompt injection.
//! - Wall-clock and memory quotas enforced by the `QuickJS` runtime.
//! - Sandboxed globals: scoped `fs`, captured `console`, session `vars`.
//! - Module loader rooted at a configured `scripts/` directory with path
//!   sanitization (rejects `..`, absolute paths, symlinks escaping root).
//! - Event listeners registered inside a script are scoped to that script's
//!   runtime and cleaned up on completion.
//!
//! This crate intentionally does not integrate with the BDD step registry.
//! Step invocation from scripts and script-based step registration are
//! deferred to later phases.

pub mod bindings;
pub mod console;
pub mod engine;
pub mod error;
pub mod fs;
pub mod modules;
pub mod result;
pub mod vars;

pub use bindings::{APIRequestContextJs, APIResponseJs, BrowserContextJs, LocatorJs, PageJs};
pub use console::ConsoleCapture;
pub use engine::{RunContext, RunOptions, ScriptEngine, ScriptEngineConfig};
pub use error::{ScriptError, ScriptErrorKind};
pub use fs::PathSandbox;
pub use result::{ConsoleEntry, ConsoleLevel, Outcome, ScriptResult, ScriptSuccess};
pub use vars::{InMemoryVars, VarsStore};
