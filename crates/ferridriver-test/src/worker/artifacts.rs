//! Per-attempt artifact capture (screenshots, videos, error context, traces)
//! and outcome classification helpers.

use std::sync::Arc;
use std::time::Duration;

use super::{AttemptEnd, AttemptRun, VideoHandle};
use crate::model::{Attachment, AttachmentBody, ExpectedStatus, TestFailure, TestInfo, TestStatus};

/// Whether the attempt failed in a way `error-context.md` would carry
/// none of the page for: some error was recorded and none of them
/// brought the accessibility tree its assertion judged.
pub(super) fn wants_page_snapshot<E>(result: &Result<Result<(), TestFailure>, E>, test_info: &TestInfo) -> bool {
  if std::env::var_os("PLAYWRIGHT_NO_COPY_PROMPT").is_some() {
    return false;
  }
  let hard = match result {
    Ok(Ok(())) => None,
    Ok(Err(failure)) if failure.message.contains("__FERRIDRIVER_SKIP__:") => None,
    Ok(Err(failure)) => Some(failure.error_context.is_some()),
    Err(_) => Some(false),
  };
  let soft = test_info
    .soft_errors
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .iter()
    .map(|e| e.error_context.is_some())
    .collect::<Vec<_>>();
  let mut carried = hard.into_iter().chain(soft).peekable();
  carried.peek().is_some() && carried.all(|has_context| !has_context)
}

/// The first page of the test's context, as an AI-mode aria snapshot,
/// within the five seconds upstream gives it.
pub(super) async fn take_page_snapshot(context: Option<Arc<ferridriver::ContextRef>>) -> Option<String> {
  let page = context?.discover_pages().await.ok()?.into_iter().next()?;
  let options = ferridriver::options::AriaSnapshotOptions {
    mode: Some(ferridriver::options::AriaSnapshotMode::Ai),
    timeout: Some(5000),
    ..Default::default()
  };
  match page.aria_snapshot().options(options).await {
    Ok(snapshot) => Some(snapshot),
    Err(e) => {
      tracing::debug!(target: "ferridriver::worker", "failed to capture aria snapshot: {e}");
      None
    },
  }
}

/// Write `error-context.md` for an attempt that recorded errors, and the
/// attachment that points at it.
/// Rewrite a finished trace without the run's `[secrets]`, so the zip that
/// leaves this machine does not carry them. A failure keeps the trace as
/// recorded rather than losing it, and says so.
pub(super) fn redact_trace(secrets: &[(String, String)], path: &std::path::Path) {
  let redactor = ferridriver_trace::redact::Redactor::new(secrets.iter().cloned());
  if redactor.is_empty() {
    return;
  }
  if let Err(e) = ferridriver_trace::redact::rewrite_zip(
    path,
    path,
    &redactor,
    ferridriver_trace::redact::RewriteOptions::default(),
  ) {
    tracing::warn!(target: "ferridriver::worker", "redacting secrets from the trace failed: {e}");
  }
}

pub(super) fn write_error_context(
  test_info: &TestInfo,
  attempt: u32,
  errors: &[&TestFailure],
  page_snapshot: Option<&str>,
  redactor: &ferridriver_trace::redact::Redactor,
) -> Option<Attachment> {
  if errors.is_empty() || errors.iter().any(|e| e.message.contains("__FERRIDRIVER_SKIP__:")) {
    return None;
  }
  let id = &test_info.test_id;
  let cwd = std::env::current_dir().unwrap_or_default();
  let content = crate::error_context::build(&crate::error_context::ErrorContextInput {
    title_path: &test_info.title_path,
    file: &id.file,
    line: id.line.and_then(|l| u32::try_from(l).ok()).unwrap_or_default(),
    column: id.column.and_then(|c| u32::try_from(c).ok()).unwrap_or_default(),
    errors,
    page_snapshot: page_snapshot.filter(|_| errors.iter().all(|e| e.error_context.is_none())),
    cwd: &cwd,
  })?;
  let _ = std::fs::create_dir_all(&test_info.output_dir);
  let path = test_info.output_dir.join(format!("error-context-attempt{attempt}.md"));
  if let Err(e) = std::fs::write(&path, redactor.redact(&content).as_bytes()) {
    tracing::warn!(target: "ferridriver::worker", "error context write failed: {e}");
    return None;
  }
  Some(Attachment {
    name: "error-context".into(),
    content_type: "text/markdown".into(),
    body: AttachmentBody::Path(path),
    step_id: None,
  })
}

pub(crate) fn test_artifact_dir_name(id: &crate::model::TestId, project: &str) -> String {
  let mut titles = id.title_path();
  let file = titles.remove(0);
  let name = artifact_dir_name(&file, &titles.join(" "), project);
  if id.repeat_each_index == 0 {
    name
  } else {
    format!("{name}-repeat{}", id.repeat_each_index)
  }
}

/// The name of a test's folder under `outputDir`, as Playwright's
/// `TestInfo.outputDir` builds it (`worker/testInfo.ts`): the spec path
/// without its `.spec`/`.test` extension and with its separators as `-`,
/// then the title path through `sanitizeForFilePath`, trimmed to a length
/// a Windows path can carry. No separator, `..`, or character a Windows
/// file name refuses survives, so every artifact stays inside `outputDir`.
pub(crate) fn artifact_dir_name(file: &str, title: &str, project: &str) -> String {
  let file = strip_test_extension(file).replace(['/', '\\', ':'], "-");
  let name = crate::snapshot_path::trim_long_string(
    &format!("{file}-{}", crate::snapshot_path::sanitize_for_file_path(title)),
    crate::snapshot_path::WINDOWS_FRIENDLY_LENGTH,
  );
  if project.is_empty() {
    name
  } else {
    format!(
      "{name}-{}",
      crate::model::hex_prefix(&ferridriver::tracing::sha1_hex(project.as_bytes()), 12)
    )
  }
}

pub(super) fn strip_test_extension(file: &str) -> &str {
  const KINDS: [&str; 2] = [".spec.", ".test."];
  const EXTENSIONS: [&str; 8] = ["js", "ts", "jsx", "tsx", "mjs", "mts", "cjs", "cts"];
  KINDS
    .iter()
    .flat_map(|kind| EXTENSIONS.iter().map(move |ext| (kind, ext)))
    .find_map(|(kind, ext)| file.strip_suffix(ext)?.strip_suffix(kind))
    .unwrap_or(file)
}

/// `TestInfo.outputDir` / `snapshotDir` are user-facing and absolute in
/// Playwright (`testInfo.outputDir: "Absolute path to a directory..."`);
/// the config keeps its relative form for display. A relative dir also
/// breaks consumers that resolve paths against a different working
/// directory — e.g. the browser stat'ing a `setInputFiles` path.
pub(super) fn absolutize(p: std::path::PathBuf) -> std::path::PathBuf {
  std::path::absolute(&p).unwrap_or(p)
}

/// Playwright's `snapshotDir`: the configured one, else `testDir`, else
/// the directory of the config file.
pub(super) fn snapshot_dir(config: &crate::config::TestConfig) -> std::path::PathBuf {
  let dir = config
    .snapshot_dir
    .as_ref()
    .or(config.test_dir.as_ref())
    .map(std::path::PathBuf::from)
    .or_else(|| config.config_dir.clone())
    .unwrap_or_else(|| std::path::PathBuf::from("."));
  absolutize(dir)
}

/// Where an attempt's video is written.
pub(super) fn video_file(test_info: &TestInfo, attempt: u32) -> std::path::PathBuf {
  let ext = ferridriver::video::video_extension();
  test_info.output_dir.join(format!(
    "{}-attempt{}.{ext}",
    sanitize_filename(&test_info.test_id.name),
    attempt
  ))
}

/// Stop the recording: an eager one is kept as it is, a buffered one is
/// encoded only when the attempt failed.
pub(super) async fn stop_video(
  handle: Option<VideoHandle>,
  test_info: &TestInfo,
  ended: &AttemptEnd,
) -> Option<std::path::PathBuf> {
  match handle? {
    VideoHandle::Eager(handle) => match handle.stop().await {
      Ok(path) => Some(path),
      Err(e) => {
        tracing::warn!(target: "ferridriver::worker", "video stop failed: {e}");
        None
      },
    },
    VideoHandle::Buffered(handle) => {
      if ended.test_failed {
        let video_path = video_file(test_info, ended.attempt);
        let _ = std::fs::create_dir_all(&test_info.output_dir);
        match handle.encode(video_path).await {
          Ok(path) => Some(path),
          Err(e) => {
            tracing::warn!(target: "ferridriver::worker", "video encode failed: {e}");
            None
          },
        }
      } else {
        handle.discard().await;
        None
      }
    },
  }
}

/// Record the failure into the attempt's trace, and the failure
/// screenshot as an `attach` action beside it.
pub(super) fn record_failure_in_trace(run: &AttemptRun, ended: &AttemptEnd, screenshot: Option<&[u8]>) {
  let test_id = &run.test_info.test_id;
  // The failure itself goes into the trace, not just the call that
  // raised it: the viewer's Errors tab is built from these, and an
  // assertion message is what a reader is looking for first.
  if ended.test_failed {
    let message = match &ended.timeout_result {
      Ok(Err(e)) => Some(e.message.clone()),
      Err(_) => Some(format!(
        "Test timeout of {}ms exceeded{}.",
        ended.timeout_dur.as_millis(),
        ended
          .timed_out_while
          .map(|phase| format!(" while {phase}"))
          .unwrap_or_default()
      )),
      Ok(Ok(())) => None,
    };
    let composite = run
      .trace_composite
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    if let (Some(composite), Some(message)) = (composite, message) {
      let stack = test_id
        .line
        .map(|line| ferridriver::trace::StackFrame {
          file: test_id.file.clone(),
          line: u32::try_from(line).unwrap_or_default(),
          column: 0,
        })
        .into_iter()
        .collect();
      ferridriver::trace::record_error(&composite, message, stack);
    }
  }

  // Mirror the failure screenshot into the trace as an `attach`
  // action (Playwright's test runner does the same), so the viewer's
  // Attachments tab carries it alongside the timeline.
  if let Some(png) = screenshot {
    let composite = run
      .trace_composite
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    if let Some(composite) = composite
      && let Some(mut span) = ferridriver::trace::begin_custom_action(
        &composite,
        ferridriver::trace::CustomAction {
          class: "Test",
          method: "attach",
          title: "attach \"screenshot-on-failure\"".to_string(),
          params: serde_json::json!({}),
          parent_id: None,
          step_id: None,
          backdate_ms: 0.0,
          stack: Vec::new(),
        },
      )
    {
      span.attach("screenshot-on-failure", "image/png", png.to_vec());
      span.finish_message(None);
    }
  }
}

/// The failure screenshot, written beside the test's other output (or
/// carried inline when it cannot be), and the trace.
pub(super) fn failure_attachments(
  test_info: &TestInfo,
  attempt: u32,
  screenshot: Option<&[u8]>,
  trace_path: Option<std::path::PathBuf>,
) -> Vec<Attachment> {
  let mut attachments = Vec::new();
  if let Some(png) = screenshot {
    let _ = std::fs::create_dir_all(&test_info.output_dir);
    let screenshot_path = test_info.output_dir.join(format!("test-failed-attempt{attempt}.png"));
    match std::fs::write(&screenshot_path, png) {
      Ok(()) => attachments.push(Attachment {
        name: "screenshot-on-failure".into(),
        content_type: "image/png".into(),
        body: AttachmentBody::Path(screenshot_path),
        step_id: None,
      }),
      Err(e) => {
        tracing::warn!(target: "ferridriver::worker", "screenshot write failed: {e}");
        attachments.push(Attachment {
          name: "screenshot-on-failure".into(),
          content_type: "image/png".into(),
          body: AttachmentBody::Bytes(png.to_vec()),
          step_id: None,
        });
      },
    }
  }
  if let Some(path) = trace_path {
    attachments.push(Attachment {
      name: "trace".into(),
      content_type: "application/zip".into(),
      body: AttachmentBody::Path(path),
      step_id: None,
    });
  }
  attachments
}

/// How the body ended, before soft assertions and `test.fail()`.
pub(super) enum RawResult {
  Ended(TestStatus, Option<TestFailure>),
  /// The body called `test.skip()`.
  SkippedAtRuntime,
}

/// The attempt's raw status: passed, failed (with the failure
/// screenshot when the failure brought none), timed out, or skipped from
/// inside the body.
pub(super) fn raw_result(
  timeout_result: Result<Result<(), TestFailure>, ferridriver::pause::Timedout>,
  screenshot: Option<Vec<u8>>,
  timeout_dur: Duration,
  timed_out_while: Option<&'static str>,
  pending_calls: &str,
) -> RawResult {
  match timeout_result {
    Ok(Ok(())) => RawResult::Ended(TestStatus::Passed, None),
    Ok(Err(failure)) => {
      // Runtime skip: test body called test.skip() — treat as skip, not failure.
      // This mirrors Playwright's TestSkipError thrown by test.skip() inside body.
      if failure.message.contains("__FERRIDRIVER_SKIP__:") {
        let reason = failure.message.split("__FERRIDRIVER_SKIP__:").nth(1).unwrap_or("");
        tracing::debug!(target: "ferridriver::worker", "test skipped at runtime: {reason}");
        return RawResult::SkippedAtRuntime;
      }

      let mut failure = failure;
      if failure.screenshot.is_none() {
        failure.screenshot = screenshot;
      }
      RawResult::Ended(TestStatus::Failed, Some(failure))
    },
    Err(_) => RawResult::Ended(
      TestStatus::TimedOut,
      Some(TestFailure {
        message: format!(
          "test timed out after {timeout_dur:?}{}{pending_calls}",
          timed_out_while
            .map(|phase| format!(" while {phase}"))
            .unwrap_or_default(),
        ),
        stack: None,
        diff: None,
        screenshot,
        error_context: None,
      }),
    ),
  }
}

/// A teardown that ran out of time fails a test that had passed, and is
/// one more error on one that had not.
pub(super) fn with_teardown_failure(
  status: TestStatus,
  error: Option<TestFailure>,
  errors: &mut Vec<TestFailure>,
  teardown_failure: Option<TestFailure>,
) -> (TestStatus, Option<TestFailure>) {
  match teardown_failure {
    Some(failure) => {
      errors.push(failure.clone());
      if status == TestStatus::Passed {
        (TestStatus::TimedOut, Some(failure))
      } else {
        (status, error.or(Some(failure)))
      }
    },
    None => (status, error),
  }
}

/// The attempt's status and error once its soft assertions and
/// `test.fail()` are taken into account.
pub(super) fn judge(
  raw_status: TestStatus,
  raw_error: Option<TestFailure>,
  soft_errors: &[TestFailure],
  expected_status: ExpectedStatus,
) -> (TestStatus, Option<TestFailure>) {
  let (raw_status, raw_error) = if !soft_errors.is_empty() && raw_status == TestStatus::Passed {
    let msg = soft_errors
      .iter()
      .map(|e| format!("  - {}", e.message))
      .collect::<Vec<_>>()
      .join("\n");
    (
      TestStatus::Failed,
      Some(TestFailure {
        message: format!("{} soft assertion(s) failed:\n{msg}", soft_errors.len()),
        stack: None,
        diff: None,
        screenshot: None,
        error_context: None,
      }),
    )
  } else {
    (raw_status, raw_error)
  };

  // `test.fail()` does not rewrite what happened: the attempt keeps the
  // status it ended with, and `expected_status` says which one counts as
  // success. Every consumer compares the two through
  // `model::outcome_kind` — inverting here instead would report a
  // `test.fail` test as `passed` in the JSON report, where Playwright
  // reports `failed` with `expectedStatus: "failed"`.
  match (&raw_status, &expected_status) {
    (TestStatus::Passed, ExpectedStatus::Fail) => (
      TestStatus::Passed,
      Some(TestFailure {
        message: "Expected to fail, but passed.".into(),
        stack: None,
        diff: None,
        screenshot: None,
        error_context: None,
      }),
    ),
    _ => (raw_status, raw_error),
  }
}

/// What a timed-out test was waiting on, as Playwright reports the
/// interrupted action: each call that had not finished, with its call log.
pub(super) fn describe_pending_calls(calls: &[ferridriver::trace::PendingCall], root: &std::path::Path) -> String {
  use std::fmt::Write as _;
  let mut out = String::new();
  for call in calls {
    let _ = write!(out, "\n\n{} had not finished", call.title);
    if let Some(at) = &call.location {
      let file = std::path::Path::new(&at.file);
      let file = file.strip_prefix(root).unwrap_or(file);
      let _ = write!(out, " ({}:{})", file.display(), at.line);
    }
    if call.log.is_empty() {
      continue;
    }
    out.push_str("\nCall log:");
    // A retrying wait logs the same line on every attempt.
    let mut lines = call.log.iter().peekable();
    while let Some(line) = lines.next() {
      let mut repeats = 1;
      while lines.peek() == Some(&line) {
        lines.next();
        repeats += 1;
      }
      let _ = write!(out, "\n  - {line}");
      if repeats > 1 {
        let _ = write!(out, " (x{repeats})");
      }
    }
  }
  out
}

/// One teardown step on the after-hooks budget. Once a step has run the
/// budget out, the steps after it are skipped rather than started on
/// nothing.
pub(super) async fn after_hooks_step<F: std::future::Future>(
  budget: ferridriver::pause::BudgetStart,
  limit: &tokio::sync::watch::Receiver<Duration>,
  stuck: &mut Option<&'static str>,
  phase: &'static str,
  step: F,
) -> Option<F::Output> {
  if stuck.is_some() {
    return None;
  }
  if let Ok(output) = ferridriver::pause::run_within_updates_from(budget, limit.clone(), step).await {
    Some(output)
  } else {
    *stuck = Some(phase);
    None
  }
}

std::thread_local! {
  /// Location + backtrace captured by the panic hook, read by
  /// `run_caught` right after `catch_unwind` returns on the same thread
  /// (unwinding resolves within the poll that panicked).
  static LAST_PANIC_STACK: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Chain a capture step onto the process panic hook (once) so test
/// failures carry the panic's location and backtrace, not just its
/// message. The previous hook still runs, keeping default stderr output.
pub(super) fn install_panic_capture() {
  static HOOK: std::sync::Once = std::sync::Once::new();
  HOOK.call_once(|| {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
      let location = info.location().map_or_else(
        || "<unknown>".to_string(),
        |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
      );
      let backtrace = std::backtrace::Backtrace::force_capture();
      LAST_PANIC_STACK.with(|cell| *cell.borrow_mut() = Some(format!("panicked at {location}\n{backtrace}")));
      previous(info);
    }));
  });
}

/// Await a test or hook future, converting a panic (`assert!`, `unwrap`,
/// ...) into a `TestFailure` so std assertion macros fail the single test
/// instead of unwinding through the worker.
pub(super) async fn run_caught<F>(fut: F) -> Result<(), TestFailure>
where
  F: std::future::Future<Output = Result<(), TestFailure>>,
{
  use futures::FutureExt;
  install_panic_capture();
  match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
    Ok(result) => result,
    Err(payload) => {
      let message = payload
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("panic with non-string payload");
      let stack = LAST_PANIC_STACK.with(|cell| cell.borrow_mut().take());
      Err(TestFailure {
        message: format!("panicked: {message}"),
        stack,
        diff: None,
        screenshot: None,
        error_context: None,
      })
    },
  }
}

/// Sanitize a test name for use as a filename.
pub(super) fn sanitize_filename(name: &str) -> String {
  name
    .chars()
    .map(|c| {
      if c.is_alphanumeric() || c == '-' || c == '_' {
        c
      } else {
        '_'
      }
    })
    .collect()
}

/// The end-of-test screenshot, taken the way `use: { screenshot }` asks
/// for it. `fullPage` defaults to true here — Playwright's own default
/// is the viewport, but a failure artifact nobody asked to crop is more
/// useful whole; an explicit `fullPage: false` gets the viewport.
pub(super) async fn capture_screenshot(
  page: &ferridriver::Page,
  wanted: &crate::config::ScreenshotOption,
) -> Option<Vec<u8>> {
  let opts = ferridriver::options::ScreenshotOptions {
    full_page: Some(wanted.full_page.unwrap_or(true)),
    omit_background: wanted.omit_background,
    format: Some("png".into()),
    ..Default::default()
  };
  page.screenshot().options(opts).await.ok()
}

/// Evaluate an annotation condition string against the current environment.
///
/// Mirrors Playwright's fixture-based condition system. Conditions match against
/// the browser config (equivalent to Playwright's `browserName`, `headless`,
/// `isMobile`, etc. fixtures from the `use` block).
///
/// ## Supported conditions
///
/// **Browser name** (Playwright's `browserName` fixture):
/// - `"chromium"`, `"chrome"` — matches browser name "chromium"
/// - `"firefox"` — matches browser name "firefox"
/// - `"webkit"` — matches browser name "webkit"
///
/// **Browser channel** (Playwright's `channel` fixture):
/// - `"msedge"`, `"chrome-beta"`, `"chrome-canary"`
///
/// **OS / platform:**
/// - `"linux"`, `"macos"` / `"darwin"`, `"windows"` / `"win32"`
///
/// **Browser mode** (Playwright's `headless` fixture):
/// - `"headed"`, `"headless"`
///
/// **Context options** (Playwright's `use` block fixtures):
/// - `"mobile"` — `isMobile` is true
/// - `"touch"` — `hasTouch` is true
/// - `"dark"` — `colorScheme` is "dark"
/// - `"light"` — `colorScheme` is "light", which it is unless set otherwise
/// - `"offline"` — offline network mode
/// - `"bypass-csp"` — CSP bypass enabled
///
/// **Environment:**
/// - `"ci"` — `CI` env var is set
/// - `"debug"` — debug build (`cfg!(debug_assertions)`)
/// - `"env:VAR_NAME"` — generic env var check, true if set and non-empty
///
/// **Operators:**
/// - `"!condition"` — negation (invert any condition)
/// - `"cond1+cond2"` — conjunction (AND), all must match
pub(super) fn evaluate_condition(condition: &str, browser: &crate::config::BrowserConfig) -> bool {
  let condition = condition.trim();

  // Negation: !condition
  if let Some(inner) = condition.strip_prefix('!') {
    return !evaluate_condition(inner, browser);
  }

  // Conjunction: condition1+condition2+...
  if condition.contains('+') {
    return condition.split('+').all(|part| evaluate_condition(part, browser));
  }

  match condition {
    // OS conditions.
    "linux" => cfg!(target_os = "linux"),
    "macos" | "darwin" => cfg!(target_os = "macos"),
    "windows" | "win32" => cfg!(target_os = "windows"),

    // Browser name (Playwright's browserName fixture).
    "chromium" | "chrome" => browser.browser == "chromium",
    "webkit" => browser.browser == "webkit",
    "firefox" => browser.browser == "firefox",

    // Browser channel (Playwright's channel fixture).
    "msedge" => browser.channel.as_deref() == Some("msedge"),
    "chrome-beta" => browser.channel.as_deref() == Some("chrome-beta"),
    "chrome-canary" => browser.channel.as_deref() == Some("chrome-canary"),

    // Browser mode.
    "headed" => !browser.headless,
    "headless" => browser.headless,

    // Context options (Playwright's use block fixtures).
    "mobile" => browser.use_options.is_mobile,
    "touch" => browser.use_options.has_touch,
    "dark" => browser.use_options.color_scheme_fixture() == Some("dark"),
    "light" => browser.use_options.color_scheme_fixture() == Some("light"),
    "offline" => browser.use_options.offline,
    "bypass-csp" => browser.use_options.bypass_csp,

    // Environment.
    "ci" => std::env::var("CI").is_ok(),
    "debug" => cfg!(debug_assertions),

    // Generic env var: `env:VAR_NAME` — true if the env var is set and non-empty.
    // Example: `@skip(env:SKIP_SLOW_TESTS)`, `#[ferritest(skip = "env:NO_GPU")]`
    other if let Some(var_name) = other.strip_prefix("env:") => std::env::var(var_name).is_ok_and(|v| !v.is_empty()),

    // Unknown condition: don't match.
    _ => false,
  }
}
