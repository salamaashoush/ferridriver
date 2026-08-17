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
//!   survives across [`Session::execute`] calls; [`SessionTable`] owns a
//!   set of them with a warm-VM cap, idle TTL, and browser-swap
//!   invalidation.
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
pub mod bytecode_cache;
pub mod command_spec;
pub mod console;
pub mod console_fmt;
pub mod debug_session;
pub mod discover;
pub mod engine;
pub mod error;
pub mod extension_load;
pub mod fs;
pub mod modules;
pub mod requirements;
pub mod result;
pub mod session_host;
pub mod session_procs;
pub mod session_table;
pub mod sidecar;
pub mod vars;
pub mod vm;

pub use bindings::native_modules::{module_aliases, native_module_names, set_module_aliases};
pub use bindings::registry::net_entry_subsumed;
pub use bindings::{
  ArtifactsJs, BrowserContextJs, CollectedAnnotation, CollectedFileConfigure, CollectedFileUse, CollectedFixture,
  CollectedHook, CollectedRegistry, CollectedStep, CollectedSuite, CollectedTest, CollectedTestHook, CollectedTests,
  ExtensionBinding, ExtensionCommandsJs, FORWARDED_CONTEXT_KEYS, HookArg, HttpClientJs, HttpResponseJs, JsArg,
  KeyboardJs, LocatorJs, MouseJs, PageJs, ScenarioSpec, ScriptAttachment, StepOutcome, TEST_SKIP_SENTINEL,
  TOOL_CONTEXT_KEYS, begin_scenario, collect_registry, collect_tests, drain_attachments, end_scenario,
  install_extensions, invoke_hook, invoke_step, run_standalone_hook, run_test, set_hook_world,
  teardown_worker_fixtures,
};
pub use bundle::{
  BundleSourceMap, BundledSource, CompiledBundle, CompiledExtension, SourceMapper, bundle_and_compile,
  bundle_and_compile_named, bundle_source, compile_and_extract_extensions, compile_bundled_source, eval_bundle,
  is_typescript_path, resolve_source, source_is_es_module,
};
pub use command_spec::{CommandOutput, CommandRun, CommandSpec, ResolvedCommand, ResolvedExec};
pub use console::{ConsoleCapture, ConsoleSink};
pub use discover::{ResolvedExtension, SOURCE_EXTENSIONS, is_source_file, walk_source_files};
pub use engine::{
  Deadline, ExtensionHost, RunContext, RunOptions, ScriptCaps, ScriptEngine, ScriptEngineConfig, Session, SessionRun,
};
pub use error::{ScriptError, ScriptErrorKind};
pub use extension_load::{GatedExtensions, gate, load_bindings};
pub use ferridriver_config::ExtensionSpec;
pub use fs::PathSandbox;
pub use requirements::{RequirementEnv, RequirementIssue};
pub use result::{ConsoleEntry, ConsoleLevel, Outcome, ScriptResult, ScriptSuccess};
pub use session_host::{SessionScriptConfig, SessionScriptHost};
pub use session_procs::SessionProcs;
pub use session_table::{BrowserSession, SessionTable};
pub use vars::{InMemoryVars, VarsStore};
pub use vm::VmHandle;
