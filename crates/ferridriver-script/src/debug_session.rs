//! `ferridriver test --debug`: publish a running test as a session and
//! stop it where the debugger is asked to.
//!
//! What makes this worth more than a screenshot: the page is the test's
//! own, and the context still holds whatever the fixtures set up — the
//! login, the seeded data, the intercepted routes. An agent attaches and
//! runs ordinary TypeScript against exactly that state, then walks the
//! test forward one API call at a time.
//!
//! Two modes, because they answer different questions:
//!
//! - `--debug` (Playwright's behaviour) publishes the session before the
//!   body runs and stops in front of the first API call. `stepOver` runs
//!   one call and stops again; `pauseAt` runs to a named `file:line`.
//! - `--debug=fail` stops at the first failure instead, between the body
//!   and its teardown, with the page still on the failure.
//!
//! The stopping itself is an [`ActionGate`] in core, which every `page.*`
//! / `locator.*` / `expect.*` call passes through — the same layer
//! Playwright's `context.debugger` hooks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::bindings::{PendingAction, TestDebugControl};
use async_trait::async_trait;
use ferridriver::trace::{ActionGate, ActionInfo};
use ferridriver_test::config::CliOverrides;
use ferridriver_test::debug::{DebugHook, DebugMode, DebugPublisher, DebugTest};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::watch;

/// What the run is waiting for before it stops again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum StopAt {
  /// Run to the end.
  #[default]
  Nothing,
  /// Stop before the next call.
  Next,
  /// Stop before the first call written at this `file:line`.
  Location(String),
}

/// The stopped test, shared between the worker (which blocks in the gate)
/// and the session's scripts (which decide when it moves).
#[derive(Debug)]
struct StoppedTest {
  test: String,
  location: Option<String>,
  /// Session id this test was published under. Calls made by that
  /// session's own scripts pass the gate untouched.
  own_script: String,
  error: std::sync::Mutex<Option<String>>,
  stop_at: std::sync::Mutex<StopAt>,
  pending: std::sync::Mutex<Option<PendingAction>>,
  /// Bumped to release whatever is blocked in the gate. A counter and not
  /// a flag: `stepOver` releases and re-arms, so the next block must not
  /// see the previous release still standing.
  release: watch::Sender<u64>,
  /// True once the test has been let go for good.
  done: AtomicBool,
}

impl StoppedTest {
  fn release_now(&self, next: StopAt) {
    *self.stop_at.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = next;
    self.release.send_modify(|n| *n = n.wrapping_add(1));
  }
}

impl TestDebugControl for StoppedTest {
  fn resume(&self) {
    self.done.store(true, Ordering::SeqCst);
    self.release_now(StopAt::Nothing);
  }

  fn step_over(&self) {
    self.release_now(StopAt::Next);
  }

  fn pause_at(&self, location: &str) {
    self.release_now(StopAt::Location(location.to_string()));
  }

  fn paused(&self) -> bool {
    self
      .pending
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .is_some()
  }

  fn resumed(&self) -> bool {
    self.done.load(Ordering::SeqCst)
  }

  fn test(&self) -> String {
    self.test.clone()
  }

  fn location(&self) -> Option<String> {
    self.location.clone()
  }

  fn error(&self) -> Option<String> {
    self
      .error
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
  }

  fn pending(&self) -> Option<PendingAction> {
    self
      .pending
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
  }
}

/// The session a stopped test is published as, for as long as it runs.
struct Published {
  id: String,
  stopped: Arc<StoppedTest>,
  bound: ferridriver_session::BoundSession,
}

/// Publishes each test as a session and stops it where the mode says.
pub struct SessionDebugHook {
  /// Resolved scripting environment — the published session gets the same
  /// sandboxes, caps and extensions a `session open` would, so a script
  /// written against one runs against the other.
  script: Arc<crate::SessionScriptConfig>,
  mode: DebugMode,
  /// The test currently published. `AsyncMutex` because it is taken across
  /// the bind, which awaits.
  live: AsyncMutex<Option<Published>>,
}

/// Wire `--debug` into a run: the overrides Playwright forces, and the hook
/// that publishes each test.
///
/// Shared by `ferridriver test`, `ferridriver bdd` and a Rust harness
/// binary — a scenario and a `#[ferritest]` are both tests to the runner, so
/// the gate, the session and the stepping are the same on all three.
pub fn install(mode: DebugMode, script: crate::SessionScriptConfig, overrides: &mut CliOverrides) {
  // The same three Playwright forces (`common/config.ts`). One worker
  // because a second one driving its own browser while the first is parked
  // makes the terminal unreadable and the stopped test harder to reach; one
  // failure because the run exists to look at that failure; no global
  // timeout because the wall clock includes however long a person spends
  // reading.
  overrides.workers = Some(1);
  overrides.max_failures = Some(1);
  overrides.global_timeout = Some(0);
  ferridriver_test::debug::set_debug_hook(Arc::new(SessionDebugHook::new(script, mode)));
}

/// [`install`] for a host with no configuration to resolve: a Rust harness
/// binary run straight from `cargo test`.
///
/// `fs.*` and `artifacts.*` are rooted at the current directory, which is
/// the crate root under cargo — the same place the harness already reads
/// fixtures and writes output from.
///
/// # Errors
///
/// Returns an error if the current directory cannot be used as a sandbox
/// root.
pub fn install_default(mode: DebugMode, overrides: &mut CliOverrides) -> Result<(), crate::ScriptError> {
  let cwd = std::env::current_dir().map_err(|e| crate::ScriptError::internal(format!("current directory: {e}")))?;
  let sandbox = Arc::new(crate::PathSandbox::new(&cwd)?);
  install(
    mode,
    crate::SessionScriptConfig {
      sandbox: Arc::clone(&sandbox),
      artifacts: Some(sandbox),
      caps: crate::ScriptCaps::default(),
      extensions: Vec::new(),
      engine: crate::ScriptEngineConfig::default(),
    },
    overrides,
  );
  Ok(())
}

impl SessionDebugHook {
  pub fn new(script: crate::SessionScriptConfig, mode: DebugMode) -> Self {
    Self {
      script: Arc::new(script),
      mode,
      live: AsyncMutex::new(None),
    }
  }

  /// Bind `test`'s browser as a session whose scripts drive `stopped`.
  async fn publish(&self, test: &DebugTest, stopped: Arc<StoppedTest>) -> Option<Published> {
    let id = stopped.own_script.clone();
    let registry = match ferridriver_session::Registry::open() {
      Ok(registry) => registry,
      Err(e) => {
        eprintln!("--debug: cannot open the session registry ({e}); continuing without stopping");
        return None;
      },
    };

    let mut engine = self.script.engine.clone();
    engine.test_debug = Some(stopped.clone() as Arc<dyn TestDebugControl>);
    // The gate skips calls made by this session's own scripts: the client
    // inspecting a stopped test drives the same context it is stopped in,
    // and stopping the inspector would leave nobody to resume it.
    engine.script_id = Some(id.clone());
    let host = Arc::new(crate::SessionScriptHost::new(
      Arc::clone(test.browser.state()),
      &id,
      crate::SessionScriptConfig {
        sandbox: self.script.sandbox.clone(),
        artifacts: self.script.artifacts.clone(),
        caps: self.script.caps.clone(),
        extensions: self.script.extensions.clone(),
        engine,
      },
    ));

    let bound = match ferridriver_session::bind_in(
      &registry,
      &test.browser,
      &id,
      ferridriver_session::BindOptions::default(),
      Some(host),
    )
    .await
    {
      Ok(bound) => bound,
      Err(e) => {
        eprintln!("--debug: cannot bind the test ({e}); continuing without stopping");
        return None;
      },
    };

    Some(Published { id, stopped, bound })
  }
}

#[async_trait]
impl DebugHook for SessionDebugHook {
  async fn test_starting(&self, test: DebugTest) {
    if self.mode != DebugMode::Start {
      return;
    }
    let stopped = Arc::new(StoppedTest {
      test: test.test.clone(),
      location: test.location.clone(),
      own_script: session_id_for(&test.test),
      error: std::sync::Mutex::new(None),
      stop_at: std::sync::Mutex::new(StopAt::Next),
      pending: std::sync::Mutex::new(None),
      release: watch::channel(0).0,
      done: AtomicBool::new(false),
    });
    let Some(published) = self.publish(&test, stopped.clone()).await else {
      return;
    };
    print_banner(&test, &published.id, "starting");
    // The gate is what actually holds the run, at the first API call.
    // Arming here rather than blocking mirrors Playwright, whose
    // `requestPause` only sets `pauseAt: {next: true}`.
    ferridriver::trace::set_action_gate(stopped);
    *self.live.lock().await = Some(published);
  }

  async fn test_failed(&self, test: DebugTest) {
    if self.mode != DebugMode::Fail {
      return;
    }
    let stopped = Arc::new(StoppedTest {
      test: test.test.clone(),
      location: test.location.clone(),
      own_script: session_id_for(&test.test),
      error: std::sync::Mutex::new(test.error.clone()),
      stop_at: std::sync::Mutex::new(StopAt::Nothing),
      pending: std::sync::Mutex::new(Some(PendingAction {
        title: "the failure".to_string(),
        location: test.location.clone(),
      })),
      release: watch::channel(0).0,
      done: AtomicBool::new(false),
    });
    let Some(published) = self.publish(&test, stopped.clone()).await else {
      return;
    };
    print_banner(&test, &published.id, "failed");

    // Block the worker. The context stays open, the page stays on the
    // failure, and the bound session serves scripts against both.
    let mut rx = stopped.release.subscribe();
    let _parked = ferridriver_test::debug::pause_clock().park();
    while !stopped.done.load(Ordering::SeqCst) {
      if rx.changed().await.is_err() {
        break;
      }
    }
    *stopped
      .pending
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    eprintln!("  resumed; the run continues\n");
    drop(published);
  }

  async fn test_finished(&self) {
    ferridriver::trace::clear_action_gate();
    let Some(published) = self.live.lock().await.take() else {
      return;
    };
    // Anything still blocked in the gate belongs to a test that is over.
    published.stopped.resume();
    drop(published.bound);
  }
}

#[async_trait]
impl ActionGate for StoppedTest {
  async fn before_action(&self, action: &ActionInfo) {
    if self.done.load(Ordering::SeqCst) {
      return;
    }
    // The client inspecting a stopped test drives the same context the
    // test is stopped in. Stopping its calls too would leave nobody to
    // resume — the only script that can release the gate would be blocked
    // on it.
    if action.script.as_deref() == Some(self.own_script.as_str()) {
      return;
    }
    {
      let mut stop_at = self.stop_at.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      let stop = match &*stop_at {
        StopAt::Nothing => false,
        StopAt::Next => true,
        StopAt::Location(want) => action
          .location
          .as_ref()
          .is_some_and(|at| matches_location(&at.to_string(), want)),
      };
      if !stop {
        return;
      }
      // Consumed by this stop, so calls made while stopped — including the
      // inspecting client's own — run straight through.
      *stop_at = StopAt::Nothing;
    }

    let mut rx = self.release.subscribe();
    let pending = PendingAction {
      title: action.title.clone(),
      location: action.location.as_ref().map(ToString::to_string),
    };
    print_stop(&pending);
    *self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pending);
    {
      let _parked = ferridriver_test::debug::pause_clock().park();
      if rx.changed().await.is_err() {
        // The sender is gone, which only happens once the test is over.
        self.done.store(true, Ordering::SeqCst);
      }
    }
    *self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
  }
}

/// Whether a call site satisfies a `pauseAt` argument.
///
/// A path suffix rather than equality, so `pauseAt('checkout.spec.ts:42')`
/// works without the absolute path the capture records. The suffix has to
/// land on a path boundary — Playwright's `file.includes(...)` would let
/// `out.spec.ts` match `checkout.spec.ts`, which is a stop nobody asked
/// for. A bare `file` with no line matches every call in that file.
fn matches_location(at: &str, want: &str) -> bool {
  let (want_file, want_line) = split_location(want);
  let (at_file, at_line) = split_location(at);
  if let Some(want_line) = want_line
    && at_line != Some(want_line)
  {
    return false;
  }
  at_file == want_file
    || at_file
      .strip_suffix(want_file)
      .is_some_and(|parent| parent.ends_with(['/', '\\']))
}

fn split_location(location: &str) -> (&str, Option<u32>) {
  match location.rsplit_once(':') {
    Some((file, line)) => match line.parse::<u32>() {
      Ok(line) => (file, Some(line)),
      Err(_) => (location, None),
    },
    None => (location, None),
  }
}

/// A session id derived from the test's full name: lowercase, non-alphanumeric
/// runs collapsed to `-`, prefixed so it is recognisable in `session list`,
/// and bounded so a long test title cannot overflow a socket path.
fn session_id_for(test: &str) -> String {
  let mut slug = String::with_capacity(24);
  let mut last_dash = true;
  for ch in test.chars() {
    if ch.is_ascii_alphanumeric() {
      slug.push(ch.to_ascii_lowercase());
      last_dash = false;
    } else if !last_dash {
      slug.push('-');
      last_dash = true;
    }
    if slug.len() >= 24 {
      break;
    }
  }
  format!("tw-{}", slug.trim_matches('-'))
}

fn print_stop(pending: &PendingAction) {
  let at = pending
    .location
    .as_ref()
    .map(|l| format!(" at {l}"))
    .unwrap_or_default();
  eprintln!("  stopped before {}{at}", pending.title);
}

fn print_banner(test: &DebugTest, id: &str, why: &str) {
  eprintln!("\n─── {why}: {} ───", test.test);
  if let Some(where_) = &test.location {
    eprintln!("  at {where_}");
  }
  if let Some(error) = &test.error {
    eprintln!("  {}", error.lines().next().unwrap_or(error));
  }
  eprintln!("\n  Attach with:");
  eprintln!(
    "    ferridriver run --session {id} --context {} --eval \"return await page.snapshotForAI()\"",
    test.context
  );
  eprintln!("\n  Drive it from a script:");
  eprintln!("    await testDebug.stepOver()               run one call, stop again");
  eprintln!("    await testDebug.pauseAt('spec.ts:42')    run up to a line");
  eprintln!("    await testDebug.resume()                 let the test finish");
  eprintln!();
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::AtomicBool;

  use crate::bindings::TestDebugControl;
  use ferridriver::trace::{ActionGate, ActionInfo, StackFrame};

  use super::{StopAt, StoppedTest, matches_location, watch};

  fn stopped(own_script: &str, stop_at: StopAt) -> Arc<StoppedTest> {
    Arc::new(StoppedTest {
      test: "a test".to_string(),
      location: None,
      own_script: own_script.to_string(),
      error: std::sync::Mutex::new(None),
      stop_at: std::sync::Mutex::new(stop_at),
      pending: std::sync::Mutex::new(None),
      release: watch::channel(0).0,
      done: AtomicBool::new(false),
    })
  }

  fn action(script: Option<&str>, location: Option<(&str, u32)>) -> ActionInfo {
    ActionInfo {
      call_id: "call@1".to_string(),
      class: "Locator".to_string(),
      method: "click".to_string(),
      title: "locator.click".to_string(),
      params: serde_json::json!({}),
      location: location.map(|(file, line)| StackFrame {
        file: file.to_string(),
        line,
        column: 1,
      }),
      script: script.map(Arc::from),
    }
  }

  /// The client inspecting a stopped test drives the same context the test
  /// is stopped in. If the gate held its calls too, the only script that
  /// could release the run would be blocked on the gate — a deadlock with
  /// no way out, so this branch is load-bearing rather than an
  /// optimisation.
  #[tokio::test(flavor = "multi_thread")]
  async fn the_inspecting_session_is_never_stopped_by_its_own_gate() {
    let stopped = stopped("tw-a-test", StopAt::Next);
    // Armed to stop at the next call, but this call is the inspector's.
    tokio::time::timeout(
      std::time::Duration::from_secs(5),
      stopped.before_action(&action(Some("tw-a-test"), None)),
    )
    .await
    .expect("the gate held a call made by its own session");
    assert!(!stopped.paused(), "it should not have registered a stop");
    // And the arm is still standing for the test's own next call.
    assert_eq!(
      *stopped
        .stop_at
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner),
      StopAt::Next
    );
  }

  /// A call the arm does not name runs straight through, so `pauseAt` skips
  /// everything between here and the line it was given.
  #[tokio::test(flavor = "multi_thread")]
  async fn a_call_at_another_line_passes_a_located_arm() {
    let stopped = stopped("tw-a-test", StopAt::Location("spec.ts:42".to_string()));
    tokio::time::timeout(
      std::time::Duration::from_secs(5),
      stopped.before_action(&action(None, Some(("/w/spec.ts", 7)))),
    )
    .await
    .expect("the gate stopped at a line pauseAt did not ask for");
  }

  #[test]
  fn a_pause_at_argument_matches_by_suffix_and_exact_line() {
    let at = "/home/u/proj/tests/checkout.spec.ts:42";
    assert!(matches_location(at, "checkout.spec.ts:42"));
    assert!(matches_location(at, "tests/checkout.spec.ts:42"));
    assert!(matches_location(at, at));
    // A line that does not match is not the call being asked for, even in
    // the right file.
    assert!(!matches_location(at, "checkout.spec.ts:41"));
    // A different file that happens to end the same way is not a suffix.
    assert!(!matches_location(at, "out.spec.ts:42"));
    // No line: every call in the file.
    assert!(matches_location(at, "checkout.spec.ts"));
  }
}

// Registers this crate as the runner's session publisher: linking
// `ferridriver-script` is what makes `--debug` work in any binary, so
// the core runner owns the flag's semantics without ever depending on a
// scripting engine.
inventory::submit! {
  DebugPublisher {
    install_default: |mode, overrides| install_default(mode, overrides).map_err(|e| e.message),
  }
}
