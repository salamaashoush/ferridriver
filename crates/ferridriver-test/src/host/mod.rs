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
/// core handles (no host-side values leave the host).
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
pub trait TestHostBridge: Send + Sync {
  fn attach(&self, name: String, content_type: String, body: Vec<u8>) -> BridgeFuture<()>;
  fn attachment_count(&self) -> usize;
  fn annotate(&self, kind: String, description: Option<String>);
  fn annotations(&self) -> Vec<(String, Option<String>)>;
  /// Open a live reporter/trace step; returns the step id.
  /// `location` is the host's own `line:col`, remapped by the bridge
  /// through the host's [`SourceMap`].
  fn begin_step(&self, title: String, parent: Option<String>, location: Option<(u32, u32)>) -> BridgeFuture<String>;
  fn end_step(&self, step_id: String, error: Option<String>) -> BridgeFuture<()>;
  /// Record a soft assertion failure. Synchronous: the value matchers
  /// have no `await` to spend, and the rule that a soft failure is
  /// recorded rather than thrown lives in `ferridriver_expect::soft`.
  fn record_soft_error(&self, message: String, diff: Option<String>);
  fn set_skip(&self, reason: Option<String>);
  fn set_expected_failure(&self);
  fn set_slow(&self);
  fn set_timeout_override(&self, ms: u64);
  fn output_path(&self, parts: &[String]) -> String;
  fn snapshot_path(&self, name: &str) -> String;
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
#[derive(Debug, Clone, Default)]
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
  pub hooks_before: Vec<usize>,
  pub hooks_after: Vec<usize>,
  pub source_label: String,
}
