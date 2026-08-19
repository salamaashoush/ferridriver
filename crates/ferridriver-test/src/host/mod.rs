//! The seam a scripting host drives a core test through.
//!
//! The runner owns what a test IS — its fixtures, its `testInfo`, its
//! steps, its snapshot rules. A host (the QuickJS binding today, a NAPI
//! one tomorrow) owns only the language it is written in. This module is
//! the boundary between them: the data a host is handed to build one
//! test invocation, and the trait through which a running test reaches
//! back into the runner.
//!
//! Everything here is language-neutral on purpose — no `rquickjs`, no
//! `napi`, no JS values. A binding lowers its own values into these
//! types and calls core; it never re-decides what any of them mean.

pub mod bridge;

pub use bridge::{InfoBridge, WorldMeta, merge_use_options, static_annotation_pairs, world_data};

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// Future type of the async [`TestHostBridge`] methods.
pub type BridgeFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Subject of a snapshot matcher, handed across the bridge as owned
/// core handles (no host-side values leave the host). Cloneable because
/// a screenshot matcher re-captures its subject while it retries.
#[derive(Clone)]
pub enum SnapshotTarget {
  Locator(ferridriver::Locator),
  Page(Arc<ferridriver::Page>),
  /// `expect(string).toMatchSnapshot(...)` — the serialized value.
  Value(String),
}

/// Runner-side services a running test reaches from a host
/// (`testInfo`, `test.step`, runtime modifiers, snapshot matchers).
///
/// [`bridge::InfoBridge`] is the implementation every host
/// uses; the trait exists so a host can be driven in a test with a
/// recorder in its place.
///
/// Steps come through [`crate::step::StepDriver`], which the runner
/// implements too: `test.step`'s options, its location rule and its
/// timeout are core rules, and a host only hands over a body to run.
pub trait TestHostBridge: crate::step::StepDriver {
  /// `testInfo.attach` / `stepInfo.attach` — `step_id` names the step
  /// the attachment belongs to, when a step made it.
  fn attach(&self, name: String, content_type: String, body: Vec<u8>, step_id: Option<String>) -> BridgeFuture<()>;
  fn attachment_count(&self) -> usize;
  fn annotate(&self, kind: String, description: Option<String>);
  fn annotations(&self) -> Vec<(String, Option<String>)>;
  /// Record a soft assertion failure. Synchronous: the value matchers
  /// have no `await` to spend, and the rule that a soft failure is
  /// recorded rather than thrown lives in `ferridriver_expect::soft`.
  fn record_soft_error(&self, message: String, diff: Option<String>);
  fn set_skip(&self, reason: Option<String>);
  fn set_expected_failure(&self);
  fn set_slow(&self);
  fn set_timeout_override(&self, ms: u64);
  fn output_path(&self, parts: &[String]) -> String;
  /// `testInfo.snapshotPath(...name, { kind })`. `kind` is Playwright's
  /// `'snapshot' | 'screenshot' | 'aria'`; an unknown one is an error
  /// the host throws.
  fn snapshot_path(&self, name: &[String], kind: &str) -> Result<String, String>;
  fn errors(&self) -> Vec<String>;
  /// `toMatchSnapshot(name?)` — text snapshot against the run's
  /// snapshot directory/update mode. `Err(message)` = assertion failed.
  fn match_text_snapshot(&self, target: SnapshotTarget, name: Option<String>) -> BridgeFuture<Result<(), String>>;
  /// `toHaveScreenshot(name?, options?)` — PNG baseline compare.
  /// `options` is the raw Playwright option bag as JSON.
  fn match_screenshot(
    &self,
    target: SnapshotTarget,
    name: Option<String>,
    options: serde_json::Value,
  ) -> BridgeFuture<Result<(), String>>;
  /// `toMatchAriaSnapshot(yaml, { timeout? })`.
  fn match_aria_snapshot(
    &self,
    target: SnapshotTarget,
    expected_yaml: String,
    is_not: bool,
    timeout_ms: Option<u64>,
  ) -> BridgeFuture<Result<(), String>>;
}

/// A host's map from the code it actually executes back to the file the
/// author wrote. A bundling host implements it over its source map; a
/// host that runs sources as-is can implement it as the identity.
pub trait SourceMap: Send + Sync {
  /// `(file, line, column)` in the original source for a position in
  /// the executed code, or `None` when the position cannot be mapped.
  fn remap(&self, line: u32, column: u32) -> Option<(String, u32, u32)>;
}

/// A host's watchdog for a runaway body — the interpreter-level
/// interrupt the runner re-arms when a test asks for more time
/// (`test.slow()`, `testInfo.setTimeout()`).
pub trait DeadlineControl: Send + Sync {
  fn arm(&self, timeout: Duration);
  fn disarm(&self);
}

/// Static test metadata for one invocation — what a host mirrors onto
/// its `testInfo`.
#[derive(Debug, Clone)]
pub struct TestInfoData {
  pub title: String,
  pub title_path: Vec<String>,
  pub file: String,
  pub line: u32,
  pub column: u32,
  pub retry: u32,
  pub worker_index: u32,
  pub parallel_index: u32,
  pub repeat_each_index: u32,
  pub timeout_ms: u64,
  /// `passed` | `failed` | `timedOut` | `skipped`
  pub expected_status: String,
  pub tags: Vec<String>,
  pub output_dir: String,
  pub snapshot_dir: String,
  pub snapshot_suffix: String,
  pub project_name: Option<String>,
  /// Playwright's `testInfo.config` — the resolved `FullConfig`, the
  /// same document the reporter API hands a reporter, so the two cannot
  /// describe different runs.
  pub config: serde_json::Value,
}

impl Default for TestInfoData {
  fn default() -> Self {
    Self {
      title: String::new(),
      title_path: Vec::new(),
      file: String::new(),
      line: 0,
      column: 0,
      retry: 0,
      worker_index: 0,
      parallel_index: 0,
      repeat_each_index: 0,
      timeout_ms: 0,
      expected_status: String::new(),
      tags: Vec::new(),
      output_dir: String::new(),
      snapshot_dir: String::new(),
      snapshot_suffix: String::new(),
      project_name: None,
      config: serde_json::Value::Null,
    }
  }
}

/// Per-test fixtures + config scalars the runner resolved before
/// dispatching into a host.
#[derive(Clone, Default)]
pub struct TestWorldData {
  pub page: Option<Arc<ferridriver::Page>>,
  pub context: Option<Arc<ferridriver::context::ContextRef>>,
  pub request: Option<Arc<ferridriver::http_client::HttpClient>>,
  pub browser: Option<Arc<ferridriver::Browser>>,
  pub browser_name: String,
  pub headless: bool,
  pub is_mobile: bool,
  pub has_touch: bool,
  /// Effective `baseURL` (test-level `use` override, else config) —
  /// exposed as the `baseURL` fixture, Playwright-style.
  pub base_url: Option<String>,
  /// Effective merged `use` options (config ⊕ suite/file bags ⊕
  /// project) — option fixtures read their overrides from here.
  pub use_options: serde_json::Value,
  /// The `expect` block this test's project resolved to
  /// (`TestConfig::resolved_expect`). A host mirrors it into its VM so a
  /// bare `expect(...)` there starts from the same defaults the Rust
  /// matchers use.
  pub expect: Arc<crate::config::ExpectConfig>,
  pub info: TestInfoData,
}

/// One test invocation: the host's registry index plus the each-hook
/// indices the body runs between (outer-first for before, inner-first
/// for after). Hooks run inside the test so custom fixtures are shared
/// between hooks and body, and a hook failure fails the test —
/// Playwright semantics.
#[derive(Debug, Clone, Default)]
pub struct RunTestSpec {
  pub test_idx: usize,
  /// Suite modifiers (`test.skip(callback)` and friends) that apply to
  /// this test, outer scope first. Evaluated BEFORE `hooks_before` —
  /// Playwright orders them the same way ("Modifiers first, then
  /// hooks", `worker/workerMain.ts:556`), because a modifier decides
  /// whether the hooks should run at all.
  pub modifiers: Vec<usize>,
  pub hooks_before: Vec<usize>,
  pub hooks_after: Vec<usize>,
  pub source_label: String,
}
