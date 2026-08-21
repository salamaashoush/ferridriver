#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `ferridriver test --debug`: a test stops with its browser published as
//! a session, and a script walks it forward.
//!
//! The point of the feature is that the state is still there and that the
//! run really is held, so that is what is asserted: the stopped page is the
//! test's own, the stop lands in front of a named call at a source line,
//! `stepOver` / `pauseAt` move it, and the run only finishes once a script
//! releases it. A test that merely checked for the banner would pass
//! against a stop that bound nothing.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`) plus Chrome. The session registry
//! is redirected to a temp dir so the test never touches the user cache,
//! and the config layers are pinned off (`--no-inherit`) so a developer's
//! own `~/.config/ferridriver` cannot change what runs.

use std::io::Read as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> String {
  std::env::var("FERRIDRIVER_BIN").unwrap_or_else(|_| {
    let base = format!("{}/../../target", env!("CARGO_MANIFEST_DIR"));
    let debug = format!("{base}/debug/ferridriver");
    if std::path::Path::new(&debug).exists() {
      debug
    } else {
      format!("{base}/release/ferridriver")
    }
  })
}

/// A workspace with one spec, plus a config that pins the runner to it.
fn workspace(spec_name: &str, spec: &str, extra_test_config: &str) -> tempfile::TempDir {
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::create_dir_all(dir.path().join("tests")).unwrap();
  std::fs::write(dir.path().join("tests").join(spec_name), spec).unwrap();
  std::fs::write(
    dir.path().join("ferridriver.toml"),
    format!(
      "[test]\ntestDir = \"tests\"\ntestMatch = [\"**/*.spec.ts\"]\n{extra_test_config}\n\n[test.browser]\nheadless = true\n"
    ),
  )
  .unwrap();
  dir
}

fn spawn_debug(work: &std::path::Path, session_dir: &std::path::Path, mode: &[&str]) -> Child {
  let mut args = vec!["test", "--no-inherit"];
  args.extend_from_slice(mode);
  Command::new(bin())
    .args(&args)
    .current_dir(work)
    .env("FERRIDRIVER_SESSION_DIR", session_dir)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn ferridriver test --debug")
}

/// `ferridriver run --session <id> …` against the stopped test.
fn run_on(session_dir: &std::path::Path, cwd: &std::path::Path, args: &[&str]) -> (bool, String) {
  let mut all = vec!["run", "--no-inherit"];
  all.extend_from_slice(args);
  let out = Command::new(bin())
    .args(&all)
    .current_dir(cwd)
    .env("FERRIDRIVER_SESSION_DIR", session_dir)
    .stdin(Stdio::null())
    .output()
    .expect("spawn ferridriver run");
  (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The session the stopped worker published, or `None` before it appears.
fn published_session(session_dir: &std::path::Path) -> Option<String> {
  let out = Command::new(bin())
    .args(["session", "list", "--no-inherit"])
    .env("FERRIDRIVER_SESSION_DIR", session_dir)
    .stdin(Stdio::null())
    .output()
    .ok()?;
  String::from_utf8_lossy(&out.stdout)
    .lines()
    .find(|l| l.starts_with("tw-"))
    .and_then(|l| l.split_whitespace().next())
    .map(str::to_string)
}

/// Wait for the run to publish itself. The stop is the run's own doing, so
/// poll rather than sleep a fixed time: a cold browser launch dominates and
/// varies.
fn await_session(session_dir: &std::path::Path, child: &mut Child) -> String {
  for _ in 0..120 {
    if let Some(id) = published_session(session_dir) {
      return id;
    }
    std::thread::sleep(Duration::from_millis(500));
  }
  let _ = child.kill();
  panic!("--debug never published a session");
}

/// The call the run is currently stopped in front of, once it is stopped.
fn await_stop(session_dir: &std::path::Path, cwd: &std::path::Path, session: &str) -> serde_json::Value {
  for _ in 0..120 {
    let (ok, out) = run_on(
      session_dir,
      cwd,
      &[
        "--session",
        session,
        "--json",
        "--eval",
        "return await testDebug.info()",
      ],
    );
    if ok
      && let Ok(doc) = serde_json::from_str::<serde_json::Value>(&out)
      && doc["value"]["action"].is_object()
    {
      return doc["value"].clone();
    }
    std::thread::sleep(Duration::from_millis(250));
  }
  panic!("the run never stopped in front of a call");
}

fn drive(session_dir: &std::path::Path, cwd: &std::path::Path, session: &str, script: &str) {
  let (ok, out) = run_on(session_dir, cwd, &["--session", session, "--eval", script]);
  assert!(ok, "driving the stopped test failed: {script}\n{out}");
}

/// `child.wait()` with a deadline — a hang here is the failure mode under
/// test, so it must not hang the suite too.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
  let deadline = Instant::now() + timeout;
  while Instant::now() < deadline {
    match child.try_wait() {
      Ok(Some(status)) => return Some(status),
      Ok(None) => std::thread::sleep(Duration::from_millis(200)),
      Err(_) => return None,
    }
  }
  let _ = child.kill();
  let _ = child.wait();
  None
}

fn stderr_of(child: &mut Child) -> String {
  let mut stderr = String::new();
  if let Some(mut pipe) = child.stderr.take() {
    let _ = pipe.read_to_string(&mut stderr);
  }
  stderr
}

const STEPPER_SPEC: &str = "import { test, expect } from '@ferridriver/test';\n\
   \n\
   test('walks its calls', async ({ page }) => {\n\
   \x20 await page.goto('data:text/html,<title>One</title><h1 id=a>first</h1>');\n\
   \x20 await expect(page.locator('#a')).toBeVisible();\n\
   \x20 await page.goto('data:text/html,<title>Two</title><h1 id=b>second</h1>');\n\
   \x20 await expect(page.locator('#b')).toBeVisible();\n\
   });\n";

#[test]
fn debug_stops_before_each_call_and_steps_through_the_test() {
  let session_dir = tempfile::tempdir().expect("session dir");
  // A short per-test timeout: the run is deliberately held for longer than
  // it below, so finishing at all proves the deadline stopped running while
  // the test was parked.
  let work = workspace("stepper.spec.ts", STEPPER_SPEC, "timeout = 2000");

  let mut child = spawn_debug(work.path(), session_dir.path(), &["--debug"]);
  let session = await_session(session_dir.path(), &mut child);

  // Stopped in front of the test's FIRST call, before it ran — the whole
  // point of stopping at the start rather than at the failure.
  let stopped = await_stop(session_dir.path(), work.path(), &session);
  assert_eq!(stopped["action"]["title"], "page.goto");
  let first_line = stopped["action"]["location"].as_str().expect("a source location");
  assert!(
    first_line.ends_with("stepper.spec.ts:4"),
    "the stop names the wrong line: {first_line}"
  );
  assert_eq!(stopped["paused"], true);

  // Sit on the stop for longer than the test's own 2s timeout. A deadline
  // that kept running would fail the test here, and the assertion at the
  // end of this function would catch it.
  std::thread::sleep(Duration::from_secs(4));

  // The page has not navigated yet, because the call is held rather than
  // merely reported.
  let (ok, url) = run_on(
    session_dir.path(),
    work.path(),
    &[
      "--session",
      &session,
      "--context",
      "context-0",
      "--eval",
      "return page.url()",
    ],
  );
  assert!(ok, "reading the stopped page failed: {url}");
  assert!(
    !url.contains("first"),
    "page.goto ran before the gate released it: {url}"
  );

  // One step runs exactly that call and stops at the next one.
  drive(session_dir.path(), work.path(), &session, "await testDebug.stepOver()");
  let stopped = await_stop(session_dir.path(), work.path(), &session);
  assert_eq!(stopped["action"]["title"], "expect.toBeVisible");
  assert!(
    stopped["action"]["location"]
      .as_str()
      .is_some_and(|l| l.ends_with("stepper.spec.ts:5")),
    "stepOver landed on the wrong line: {stopped}"
  );

  // …and the call it stepped over did run.
  let (ok, url) = run_on(
    session_dir.path(),
    work.path(),
    &[
      "--session",
      &session,
      "--context",
      "context-0",
      "--eval",
      "return page.url()",
    ],
  );
  assert!(ok, "reading the stepped page failed: {url}");
  assert!(url.contains("first"), "stepOver did not run page.goto: {url}");

  // `pauseAt` runs everything up to a named line and stops there — line 6
  // is skipped, line 7 is where it lands.
  drive(
    session_dir.path(),
    work.path(),
    &session,
    "await testDebug.pauseAt('stepper.spec.ts:7')",
  );
  let stopped = await_stop(session_dir.path(), work.path(), &session);
  assert!(
    stopped["action"]["location"]
      .as_str()
      .is_some_and(|l| l.ends_with("stepper.spec.ts:7")),
    "pauseAt landed somewhere else: {stopped}"
  );

  drive(session_dir.path(), work.path(), &session, "await testDebug.resume()");
  let status = wait_with_timeout(&mut child, Duration::from_mins(2)).expect("the run did not finish once resumed");
  let mut stdout = String::new();
  if let Some(mut pipe) = child.stdout.take() {
    let _ = pipe.read_to_string(&mut stdout);
  }
  assert!(
    status.success(),
    "a test parked at the debugger for longer than its own timeout must still pass:\n{stdout}"
  );

  // The QuickJS runtime tore down cleanly: a native binding that captures a
  // JS value aborts here instead, and the abort is easy to miss because the
  // run has already printed its results.
  let stderr = stderr_of(&mut child);
  assert!(
    !stderr.contains("JS_FreeRuntime"),
    "the script runtime aborted at teardown: {stderr}"
  );
}

/// A scenario is a test to the runner, so `bdd --debug` stops, steps and
/// resumes exactly like `test --debug`.
///
/// The location is the assertion that matters here: it must name the line
/// in the step's own `.ts`, not the `.feature` the step span points at and
/// not a file inside the bindings. Getting there needs the step bundle's
/// source map and the JS call site to win over the Rust builder's.
#[test]
fn bdd_debug_stops_inside_the_step_body_that_wrote_the_call() {
  let session_dir = tempfile::tempdir().expect("session dir");
  let work = tempfile::tempdir().expect("workdir");
  std::fs::create_dir_all(work.path().join("features")).unwrap();
  std::fs::create_dir_all(work.path().join("steps")).unwrap();
  std::fs::write(
    work.path().join("features/smoke.feature"),
    "Feature: debug smoke\n  Scenario: blank page\n    Given a blank page\n",
  )
  .unwrap();
  std::fs::write(
    work.path().join("steps/steps.ts"),
    "Given('a blank page', async (world: any) => {\n\
     \x20 await world.page.goto('data:text/html,<title>Stepped</title>');\n\
     });\n",
  )
  .unwrap();
  std::fs::write(
    work.path().join("ferridriver.toml"),
    "[test]\nfeatures = [\"features/**/*.feature\"]\n\n[test.browser]\nheadless = true\n",
  )
  .unwrap();

  let mut child = Command::new(bin())
    .args(["bdd", "--no-inherit", "--debug", "--steps", "steps/*.ts", "features/"])
    .current_dir(work.path())
    .env("FERRIDRIVER_SESSION_DIR", session_dir.path())
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn ferridriver bdd --debug");

  let session = await_session(session_dir.path(), &mut child);
  let stopped = await_stop(session_dir.path(), work.path(), &session);
  assert_eq!(stopped["action"]["title"], "page.goto");
  let at = stopped["action"]["location"].as_str().expect("a source location");
  assert!(
    at.ends_with("steps/steps.ts:2"),
    "the stop must name the step body, not the .feature or a binding: {at}"
  );
  // The scenario's own location stays the .feature line.
  assert!(
    stopped["location"]
      .as_str()
      .is_some_and(|l| l.ends_with("smoke.feature:2")),
    "the test's location is its scenario: {stopped}"
  );

  // Hold it past the 5s per-step timeout: a step parked at the debugger is
  // not a step running away, and the run below still has to pass.
  std::thread::sleep(Duration::from_secs(7));

  drive(session_dir.path(), work.path(), &session, "await testDebug.resume()");
  let status = wait_with_timeout(&mut child, Duration::from_mins(2)).expect("the run did not finish once resumed");
  let mut stdout = String::new();
  if let Some(mut pipe) = child.stdout.take() {
    let _ = pipe.read_to_string(&mut stdout);
  }
  assert!(
    status.success(),
    "a scenario parked longer than its step timeout must still pass:\n{stdout}"
  );
}

/// Suspending the deadline is not the same as switching it off. Playwright
/// zeroes the per-test timeout only for its inspector; under `--debug` it
/// suspends around the pause, and so do we — a test that hangs on its own
/// after being released still fails, which is what makes the flag safe to
/// leave on while chasing something else.
#[test]
fn a_test_that_hangs_after_being_released_still_times_out() {
  let session_dir = tempfile::tempdir().expect("session dir");
  let work = workspace(
    "hangs.spec.ts",
    "import { test } from '@ferridriver/test';\n\
     \n\
     test('hangs on its own', async ({ page }) => {\n\
     \x20 await page.goto('data:text/html,<h1>here</h1>');\n\
     \x20 await new Promise(() => {});\n\
     });\n",
    "timeout = 2000",
  );

  let mut child = spawn_debug(work.path(), session_dir.path(), &["--debug"]);
  let session = await_session(session_dir.path(), &mut child);
  await_stop(session_dir.path(), work.path(), &session);
  // Park far longer than the deadline, then let it run into the hang.
  std::thread::sleep(Duration::from_secs(4));
  drive(session_dir.path(), work.path(), &session, "await testDebug.resume()");

  let status = wait_with_timeout(&mut child, Duration::from_mins(2)).expect("the hanging test never timed out");
  let mut stdout = String::new();
  if let Some(mut pipe) = child.stdout.take() {
    let _ = pipe.read_to_string(&mut stdout);
  }
  assert!(!status.success(), "a test that hung must still fail:\n{stdout}");
  assert!(
    stdout.contains("timed out"),
    "it should fail as a timeout, not something else:\n{stdout}"
  );
}

#[test]
fn debug_fail_stops_at_the_failure_with_the_page_still_on_it() {
  let session_dir = tempfile::tempdir().expect("session dir");
  let work = workspace(
    "failing.spec.ts",
    "import { test, expect } from '@ferridriver/test';\n\
     \n\
     test('leaves its page behind', async ({ page }) => {\n\
     \x20 await page.goto('data:text/html,<title>Paused</title><h1 id=marker>debug-me</h1>');\n\
     \x20 await expect(page.locator('#nope')).toBeVisible({ timeout: 1000 });\n\
     });\n",
    "",
  );

  let mut child = spawn_debug(work.path(), session_dir.path(), &["--debug=fail"]);
  let session = await_session(session_dir.path(), &mut child);

  // The run is genuinely held: it has not exited while the stop is open.
  assert!(
    child.try_wait().expect("try_wait").is_none(),
    "the run finished without waiting for the stop to be released"
  );

  // The stopped page is the one the test was on — this is the whole
  // feature. A stop that bound a fresh context would answer `about:blank`.
  let (ok, snapshot) = run_on(
    session_dir.path(),
    work.path(),
    &[
      "--session",
      &session,
      "--context",
      "context-0",
      "--eval",
      "return await page.snapshotForAI()",
    ],
  );
  assert!(ok, "snapshotting the stopped page failed");
  assert!(
    snapshot.contains("debug-me"),
    "the stopped context is not the test's — snapshot: {snapshot}"
  );

  // `testDebug` reports what stopped, and why.
  let (ok, info) = run_on(
    session_dir.path(),
    work.path(),
    &[
      "--session",
      &session,
      "--json",
      "--eval",
      "return await testDebug.info()",
    ],
  );
  assert!(ok, "testDebug.info() failed: {info}");
  let doc: serde_json::Value = serde_json::from_str(&info).expect("one json document");
  assert!(
    doc["value"]["test"]
      .as_str()
      .is_some_and(|t| t.contains("leaves its page behind")),
    "testDebug.info() does not name the stopped test: {info}"
  );
  assert!(
    doc["value"]["error"]
      .as_str()
      .is_some_and(|e| e.contains("toBeVisible")),
    "testDebug.info() does not carry the failure: {info}"
  );
  assert_eq!(doc["value"]["resumed"], false, "reported resumed before release");

  // Releasing it lets the run finish — and the run still reports the
  // failure, because stopping is for looking, not for changing the verdict.
  drive(session_dir.path(), work.path(), &session, "await testDebug.resume()");

  let status = wait_with_timeout(&mut child, Duration::from_mins(2)).expect("the run did not finish once resumed");
  assert!(!status.success(), "a run whose test failed must exit non-zero");

  let stderr = stderr_of(&mut child);
  assert!(
    !stderr.contains("JS_FreeRuntime"),
    "the script runtime aborted at teardown: {stderr}"
  );
}

#[test]
fn debug_fail_never_stops_a_run_with_nothing_to_look_at() {
  let session_dir = tempfile::tempdir().expect("session dir");
  let work = workspace(
    "passing.spec.ts",
    "import { test, expect } from '@ferridriver/test';\n\
     \n\
     test('passes', async ({ page }) => {\n\
     \x20 await page.goto('data:text/html,<h1 id=ok>fine</h1>');\n\
     \x20 await expect(page.locator('#ok')).toBeVisible();\n\
     });\n",
    "",
  );

  let mut child = spawn_debug(work.path(), session_dir.path(), &["--debug=fail"]);
  let status =
    wait_with_timeout(&mut child, Duration::from_mins(2)).expect("--debug=fail hung on a run with nothing to stop on");
  assert!(status.success(), "the passing run should exit zero");
  assert!(
    published_session(session_dir.path()).is_none(),
    "a passing run published a debug session"
  );
}
