//! Holding a worker still so something else can look at the live test.
//!
//! A test normally keeps no state anyone can reach: it runs, `afterEach`
//! tears the context down, and what is left is a screenshot and a stack.
//! `--debug` stops it while everything is still live — the page mid-flow,
//! the cookies the fixtures logged in with, the routes they intercepted —
//! and hands it to a [`DebugHook`].
//!
//! Two stopping points, because they answer different questions:
//!
//! - **before the body** (`--debug`), which is where Playwright stops:
//!   arm, then hold at each API call, so a client can walk the test
//!   forward one action at a time.
//! - **at the failure** (`--debug=fail`), before teardown: the test has
//!   already gone wrong and the question is what the page looks like now.
//!
//! The hook is a trait rather than an implementation because binding a
//! session needs the scripting engine, and this crate sits below it. The
//! CLI implements it and installs it with [`set_debug_hook`], the same way
//! test script capabilities and sidecars reach the runner.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

/// Where `--debug` stops.
///
/// Lives here rather than beside the hook so a host can name it without
/// pulling in the scripting engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DebugMode {
  /// Before the test's first API call, as Playwright does.
  #[default]
  Start,
  /// At the first failure, before teardown.
  Fail,
}

impl std::str::FromStr for DebugMode {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "start" => Ok(Self::Start),
      "fail" => Ok(Self::Fail),
      other => Err(format!("expected `start` or `fail`, got `{other}`")),
    }
  }
}

/// A test the debugger has been given access to.
///
/// Everything here is live: `browser` is driving, and `context` still
/// holds the pages, cookies and storage the test has built up.
pub struct DebugTest {
  /// Full test name, as the reporter prints it.
  pub test: String,
  /// `file:line` of the test, when the discovery pass recorded one.
  pub location: Option<String>,
  /// Why it stopped: the failure message at a failure stop, `None` at the
  /// start of a test that has not failed yet.
  pub error: Option<String>,
  /// The live browser, for a caller that wants to bind it.
  pub browser: Arc<ferridriver::Browser>,
  /// Name of the test's own browser context — what an attaching client
  /// passes as `--context` to reach this test's pages rather than a fresh
  /// one.
  pub context: String,
}

/// Somewhere for a stopped test to go.
///
/// The worker calls these in order around one test: [`Self::test_starting`]
/// before the body, [`Self::test_failed`] if it failed, and
/// [`Self::test_finished`] once, whatever happened. Which of them actually
/// stops is the implementation's business — the runner offers all three
/// and lets the mode decide.
#[async_trait]
pub trait DebugHook: Send + Sync + 'static {
  /// Before the test body, with its context already created. Returning
  /// lets the body start, so an implementation that wants to pause at
  /// the first API call arms a gate here rather than blocking.
  async fn test_starting(&self, test: DebugTest);

  /// The test failed, before `afterEach` and before its context closes.
  /// The worker is blocked until this returns.
  async fn test_failed(&self, test: DebugTest);

  /// The test is done with. Releases whatever [`Self::test_starting`]
  /// set up, so the next test does not inherit it.
  async fn test_finished(&self);
}

static HOOK: OnceLock<Arc<dyn DebugHook>> = OnceLock::new();

/// Install the process's debug hook. First call wins; later ones are
/// ignored, because a second hook would mean two owners for one pause.
pub fn set_debug_hook(hook: Arc<dyn DebugHook>) {
  let _ = HOOK.set(hook);
}

/// The installed hook, if `--debug` set one up.
#[must_use]
pub fn debug_hook() -> Option<Arc<dyn DebugHook>> {
  HOOK.get().cloned()
}

// ── The parked clock ───────────────────────────────────────────────────

/// Re-exported from core, where it has to live: the script engine's own
/// per-call timeout must stand still while a run is parked too, and that
/// crate sits beside this one rather than under it.
pub use ferridriver::pause::{ParkGuard, PauseClock, pause_clock};
