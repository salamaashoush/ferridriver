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
  clippy::return_self_not_must_use,
  // Some web-API classes (TextEncoder, etc.) are legitimately stateless per
  // their WHATWG spec, but `#[rquickjs::methods]` instance methods must still
  // take `&self` to be callable on `new TextEncoder()` — not a fixable smell.
  clippy::unused_self
)]
//! ferridriver-script: sandboxed `QuickJS` scripting engine.
//!
//! Exposes a `ScriptEngine` that runs user-provided JS against ferridriver's
//! Page/Browser/Context API with:
//!
//! - One-shot isolation via [`ScriptEngine::run`] (fresh VM per call) or
//!   REPL-style continuity via a persistent [`Session`] whose `globalThis`
//!   survives across [`Session::execute`] calls.
//! - Bound args (never interpolated into source) to prevent prompt injection.
//! - Wall-clock and memory quotas enforced by the `QuickJS` runtime.
//! - Sandboxed globals: scoped `fs`, captured `console`, session `vars`.
//! - Module loader rooted at a configured `scripts/` directory with path
//!   sanitization (rejects `..`, absolute paths, symlinks escaping root).
//! - A poisoning timeout/OOM discards the session VM so the next
//!   execution transparently gets a fresh one.
//!
//! Scripting is independent of the BDD step registry — scripts drive the
//! browser through the `page` / `context` / `request` bindings directly.

pub mod bindings;
pub mod bundle;
pub mod console;
pub mod engine;
pub mod error;
pub mod fs;
pub mod modules;
pub mod result;
pub mod vars;

pub use bindings::{
  APIRequestContextJs, APIResponseJs, ArtifactsJs, BrowserContextJs, CollectedRegistry, JsArg, KeyboardJs, LocatorJs,
  MouseJs, PageJs, PluginBinding, PluginCommandsJs, ScenarioWorld, ScriptAttachment, StepOutcome, collect_registry,
  drain_attachments, install_plugins, invoke_hook, invoke_step, reset_world, set_scenario_world,
};
pub use bundle::{
  CompiledBundle, CompiledPlugin, bundle_and_compile, bundle_source, compile_and_extract_plugins, eval_bundle,
};
pub use console::ConsoleCapture;
pub use engine::{ExtensionHost, RunContext, RunOptions, ScriptEngine, ScriptEngineConfig, Session, SessionRun};
pub use error::{ScriptError, ScriptErrorKind};
// Re-export so the BDD core can name the session's async context (the
// bridge it drives JS step functions through) without a duplicate
// rquickjs dependency/version.
pub use fs::PathSandbox;
pub use result::{ConsoleEntry, ConsoleLevel, Outcome, ScriptResult, ScriptSuccess};
pub use rquickjs::AsyncContext;
pub use vars::{InMemoryVars, VarsStore};
