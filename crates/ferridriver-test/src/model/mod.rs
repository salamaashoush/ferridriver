//! Core test model types: `TestId`, `TestCase`, `TestSuite`, `TestPlan`, `TestOutcome`,
//! `TestInfo`, `TestStep`, `SuiteMode`.

mod info;
mod outcome;
mod step;
#[cfg(test)]
mod tests;

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::fixture::FixturePool;

pub use info::{OpenStep, OpenSteps, RecordedStep, StepHandle, TestInfo, TestOutput};
pub use outcome::{
  Attachment, AttachmentBody, Context, ExpectedStatus, TestAnnotation, TestFailure, TestModifiers, TestOutcome,
  TestOutcomeKind, TestStatus, outcome_kind,
};
pub use step::{StepCategory, StepLocation, StepStatus, TestStep, nest_steps};

// ── Test Identity ──

/// Globally unique test identifier.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TestId {
  pub repeat_each_index: u32,
  pub file: String,
  pub suite: Option<String>,
  pub name: String,
  /// Source line number (used by rerun reporter for `file:line` output).
  pub line: Option<usize>,
  /// Source column, when the discovery layer parsed one. Reporters that
  /// mirror Playwright's JSON report emit `{file, line, column}` triples.
  pub column: Option<usize>,
  /// `file` relative to the config root, `/`-separated. `file` is
  /// relative to the working directory, which differs between machines
  /// and invocations, so identity ([`Self::stable_id`]) hashes this
  /// instead. `None` until [`TestPlan::anchor`] has run.
  pub root_path: Option<String>,
}

impl TestId {
  /// Stable full name for display.
  ///
  /// Rendered from [`Self::title_path`], so what a reporter prints and
  /// what a step's title path continues can never disagree: one nested
  /// `describe` per segment, joined for display with ` > `.
  #[must_use]
  pub fn full_name(&self) -> String {
    self.title_path().join(" > ")
  }

  #[must_use]
  pub fn execution_key(&self) -> String {
    let name = self.full_name();
    if self.repeat_each_index == 0 {
      name
    } else {
      format!("{name} (repeat:{})", self.repeat_each_index)
    }
  }

  /// File path with optional line number (e.g., `features/login.feature:15`).
  #[must_use]
  pub fn file_location(&self) -> String {
    match self.line {
      Some(line) => format!("{}:{}", self.file, line),
      None => self.file.clone(),
    }
  }

  /// The title path a UI shows: file, then each enclosing suite, then the
  /// test's own name.
  ///
  /// The ONE derivation. `testInfo.titlePath`, `stepInfo.titlePath`,
  /// [`Self::full_name`] and [`Self::stable_id`] all read it, so a
  /// nested `describe` is one segment for every consumer. A suite id is
  /// `file::outer::inner`; a file-level suite carries the file itself,
  /// which would otherwise print twice.
  #[must_use]
  pub fn title_path(&self) -> Vec<String> {
    let mut titles = vec![self.file.clone()];
    if let Some(suite) = &self.suite
      && *suite != self.file
    {
      let path = suite.strip_prefix(&format!("{}::", self.file)).unwrap_or(suite);
      titles.extend(path.split("::").map(ToString::to_string));
    }
    titles.push(self.name.clone());
    titles
  }

  /// The file as identity sees it: relative to the config root once the
  /// plan is anchored, the working-directory path before.
  #[must_use]
  pub fn anchored_file(&self) -> &str {
    self.root_path.as_deref().unwrap_or(&self.file)
  }

  /// Short, stable, filesystem- and URL-safe identity for this test in
  /// `project`.
  ///
  /// Shaped exactly like Playwright's
  /// (`suiteUtils.ts::bindFileSuiteToProject`): the file's hash, a dash,
  /// then the hash of the project-qualified title path. Two things
  /// depend on it being this and not a display name — a live trace is
  /// found on disk by `<testId>.json`, and a UI asks to run tests by id.
  #[must_use]
  pub fn stable_id(&self, project: &str) -> String {
    let file = self.anchored_file();
    let file_id = ferridriver::tracing::sha1_hex(file.as_bytes());
    let titles = self.title_path();
    let mut expression = format!("[project={project}]{file}\u{1e}{}", titles[1..].join("\u{1e}"));
    if self.repeat_each_index > 0 {
      use std::fmt::Write;
      let _ = write!(expression, " (repeat:{})", self.repeat_each_index);
    }
    let test_id = ferridriver::tracing::sha1_hex(expression.as_bytes());
    format!("{}-{}", hex_prefix(&file_id, 20), hex_prefix(&test_id, 20))
  }
}

/// The first `len` characters of a hex digest, or all of it when shorter.
pub(crate) fn hex_prefix(digest: &str, len: usize) -> &str {
  digest.get(..len).unwrap_or(digest)
}

impl fmt::Display for TestId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.full_name())
  }
}

// ── Test Function ──

/// The async test body: takes a fixture pool, returns success or failure.
/// Uses `Arc` so tests can be re-dispatched for retries and repeatEach.
pub type TestFn =
  Arc<dyn Fn(FixturePool) -> Pin<Box<dyn Future<Output = Result<(), TestFailure>> + Send>> + Send + Sync>;

// ── Test Case ──

/// A single test case with metadata and body.
#[derive(Clone)]
pub struct TestCase {
  pub id: TestId,
  pub test_fn: TestFn,
  /// Fixture names this test requests (drives DAG resolution).
  pub fixture_requests: Vec<String>,
  /// Annotations: skip, slow, fixme, tags.
  pub annotations: Vec<TestAnnotation>,
  /// Per-test timeout override.
  pub timeout: Option<Duration>,
  /// Per-test retry override.
  pub retries: Option<u32>,
  /// Expected status (for `test.fail()` annotation).
  pub expected_status: ExpectedStatus,
  /// Per-test fixture overrides from `test.use()`. Merged with global config by the worker.
  pub use_options: Option<serde_json::Value>,
  /// Arbitrary metadata for a domain the core does not model — the same
  /// role [`TestStep::metadata`] plays for a step. A BDD scenario
  /// carries its Gherkin source here (keywords, descriptions, tag
  /// lines) so the cucumber-json reporter can quote them without the
  /// runner learning what Gherkin is.
  pub metadata: Option<serde_json::Value>,
}

// ── Test Suite ──

/// How tests within a suite are scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SuiteMode {
  /// Tests run in parallel (default for fullyParallel, or `test.describe.parallel()`).
  #[default]
  Parallel,
  /// Tests run sequentially in one worker. If one fails, rest are skipped.
  /// Maps to `test.describe.serial()`.
  Serial,
}

/// A group of tests (maps to `test.describe` / `#[cfg(test)] mod`).
#[derive(Clone)]
pub struct TestSuite {
  pub name: String,
  pub file: String,
  pub tests: Vec<TestCase>,
  pub hooks: Hooks,
  /// Suite-level annotations (applied to all children).
  pub annotations: Vec<TestAnnotation>,
  /// Execution mode for this suite.
  pub mode: SuiteMode,
  /// The serial scope a serial suite belongs to, named by its describe
  /// path within the file (empty for a file-level `describe.configure
  /// ({ mode: 'serial' })`). A `describe` nested in a serial one is a
  /// suite of its own but not a group of its own: every suite of one
  /// file naming the same scope runs as one group, on one worker. `None`
  /// makes a serial suite its own scope.
  pub serial_scope: Option<String>,
}

impl TestSuite {
  /// The scope label this suite's serial group is named by, or `None`
  /// when its tests schedule independently.
  #[must_use]
  pub fn serial_label(&self) -> Option<&str> {
    (self.mode == SuiteMode::Serial).then(|| self.serial_scope.as_deref().unwrap_or(&self.name))
  }

  /// The machine-independent name of this suite's serial group under
  /// `project`: the config-root-relative file, then the scope. `None`
  /// for a parallel suite.
  #[must_use]
  pub fn group_key(&self, project: &str) -> Option<String> {
    let label = self.serial_label()?;
    let file = self
      .tests
      .first()
      .map_or(self.file.as_str(), |test| test.id.anchored_file());
    let mut key = String::new();
    if !project.is_empty() {
      use std::fmt::Write as _;
      let _ = write!(key, "[{project}] ");
    }
    key.push_str(file);
    if !label.is_empty() {
      key.push_str(" > ");
      key.push_str(label);
    }
    Some(key)
  }
}

/// Lifecycle hooks attached to a suite.
#[derive(Clone, Default)]
pub struct Hooks {
  /// Runs once per suite per worker (no test context).
  pub before_all: Vec<SuiteHookFn>,
  /// Runs once per suite per worker on teardown (no test context).
  pub after_all: Vec<SuiteHookFn>,
  /// Runs before each test (receives test info with tags, name, step API).
  pub before_each: Vec<HookFn>,
  /// Runs after each test, even on failure (receives test info).
  pub after_each: Vec<HookFn>,
}

/// Suite-scoped hook (`before_all` / `after_all`). Receives only the fixture pool.
/// Runs once per suite per worker, no test context available.
pub type SuiteHookFn =
  Arc<dyn Fn(FixturePool) -> Pin<Box<dyn Future<Output = Result<(), TestFailure>> + Send>> + Send + Sync>;

/// Test-scoped hook (`before_each` / `after_each`). Receives fixture pool + `TestInfo`.
/// `TestInfo` provides access to test tags, name, step API, and event bus.
pub type HookFn = Arc<
  dyn Fn(FixturePool, Arc<TestInfo>) -> Pin<Box<dyn Future<Output = Result<(), TestFailure>> + Send>> + Send + Sync,
>;

/// Programmatic suite-level hooks supplied by the test author at runtime.
///
/// Lives separately from `TestConfig` because the closure types close over
/// runtime fixture and failure values defined in this crate, which cannot be
/// expressed in the data-only `ferridriver-config` schema.
#[derive(Clone, Default)]
pub struct TestHooks {
  /// Hooks invoked once before any tests run, per worker.
  pub global_setup_fns: Vec<SuiteHookFn>,
  /// Hooks invoked once after all tests finish, per worker.
  pub global_teardown_fns: Vec<SuiteHookFn>,
}

impl std::fmt::Debug for TestHooks {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TestHooks")
      .field("global_setup_fns", &format!("[{} fn(s)]", self.global_setup_fns.len()))
      .field(
        "global_teardown_fns",
        &format!("[{} fn(s)]", self.global_teardown_fns.len()),
      )
      .finish()
  }
}

// ── Test Plan ──

/// The full test plan after discovery + filtering + sharding.
#[derive(Clone, Default)]
pub struct TestPlan {
  /// Cloned and filtered execution plans must not expand repetitions again.
  pub repetitions_expanded: bool,
  pub suites: Vec<TestSuite>,
  /// Total test count (after filtering, before retry expansion).
  pub total_tests: usize,
  /// Shard info if sharding is active.
  pub shard: Option<ShardInfo>,
}

impl TestPlan {
  /// Record every test's file relative to `root`, the config root, so
  /// its stable id and group key are the same from any working directory
  /// and on any machine. Front-ends call this once on the plan they build.
  pub fn anchor(&mut self, root: &std::path::Path) {
    let root = crate::identity::comparable_path(root);
    for suite in &mut self.suites {
      for test in &mut suite.tests {
        test.id.root_path = Some(crate::identity::relative_to(&root, &test.id.file));
      }
    }
  }

  pub(crate) fn expand_repetitions(&mut self, count: u32) {
    if self.repetitions_expanded {
      return;
    }
    self.repetitions_expanded = true;
    let count = count.max(1);
    let original_suites = self.suites.len();
    for repeat_each_index in 1..count {
      for index in 0..original_suites {
        let mut suite = self.suites[index].clone();
        for test in &mut suite.tests {
          test.id.repeat_each_index = repeat_each_index;
        }
        self.suites.push(suite);
      }
    }
    self.total_tests = self.suites.iter().map(|suite| suite.tests.len()).sum();
  }
}

#[derive(Debug, Clone)]
pub struct ShardInfo {
  pub current: u32,
  pub total: u32,
}

// ── Plan Builder ──

/// Suite metadata for plan building.
pub struct SuiteDef {
  /// Suite ID (e.g. `"file::SuiteName"`). Must match `TestCase.id.suite`.
  pub id: String,
  pub name: String,
  pub file: String,
  pub mode: SuiteMode,
  /// See [`TestSuite::serial_scope`].
  pub serial_scope: Option<String>,
}

/// Hook registration for plan building.
pub struct HookDef {
  /// Suite ID this hook belongs to. Empty string = root/default suite.
  pub suite_id: String,
  pub kind: HookKind,
}

/// Generic lifecycle phase shared across all front-end hook syntaxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
  Before,
  After,
}

/// Generic lifecycle scope shared across E2E and BDD hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookScope {
  Suite,
  Scenario,
  Step,
}

/// Where a hook attaches in the runner model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOwner {
  Root,
  Suite(String),
}

/// Unified hook registration metadata used by adapters before execution hooks are built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRegistration {
  pub phase: HookPhase,
  pub scope: HookScope,
  pub owner: HookOwner,
  pub tags: Option<String>,
  pub requested_fixtures: Vec<String>,
}

/// Hook kind with the associated callback.
pub enum HookKind {
  BeforeAll(SuiteHookFn),
  AfterAll(SuiteHookFn),
  BeforeEach(HookFn),
  AfterEach(HookFn),
}

/// Builds a `TestPlan` from flat test cases, suite definitions, and hooks.
///
/// Groups tests by `TestCase.id.suite`, attaches hooks to matching suites,
/// and respects suite mode (parallel/serial). This is the single place
/// where suite→test→hook association happens — callers (NAPI, CLI, macros)
/// just register flat data.
pub struct TestPlanBuilder {
  tests: Vec<TestCase>,
  suites: Vec<SuiteDef>,
  hooks: Vec<HookDef>,
}

impl Default for TestPlanBuilder {
  fn default() -> Self {
    Self::new()
  }
}

impl TestPlanBuilder {
  #[must_use]
  pub fn new() -> Self {
    Self {
      tests: Vec::new(),
      suites: Vec::new(),
      hooks: Vec::new(),
    }
  }

  pub fn add_test(&mut self, test: TestCase) {
    self.tests.push(test);
  }

  pub fn add_suite(&mut self, suite: SuiteDef) {
    self.suites.push(suite);
  }

  pub fn add_hook(&mut self, hook: HookDef) {
    self.hooks.push(hook);
  }

  /// Consume the builder and produce a `TestPlan`.
  ///
  /// Tests are grouped by `id.suite` (matching `SuiteDef.id`).
  /// Tests without a suite go into a default parallel suite.
  /// Hooks are attached to their matching suite by `suite_id`.
  #[must_use]
  pub fn build(self) -> TestPlan {
    use rustc_hash::FxHashMap;

    // Index suite metadata by ID.
    let suite_meta: FxHashMap<String, (String, String, SuiteMode, Option<String>)> = self
      .suites
      .into_iter()
      .map(|s| (s.id, (s.name, s.file, s.mode, s.serial_scope)))
      .collect();

    // Group tests by suite key, suites in the order their first test was
    // added: the dispatcher queues them in plan order, and Playwright runs
    // files in discovery order and tests in declaration order.
    let mut grouped: Vec<(String, Vec<TestCase>)> = Vec::new();
    let mut position: FxHashMap<String, usize> = FxHashMap::default();
    for tc in self.tests {
      let key = tc.id.suite.clone().unwrap_or_default();
      let index = *position.entry(key.clone()).or_insert_with(|| {
        grouped.push((key, Vec::new()));
        grouped.len() - 1
      });
      grouped[index].1.push(tc);
    }

    // Build hooks per suite.
    let mut hook_map: FxHashMap<String, Hooks> = FxHashMap::default();
    for h in self.hooks {
      let hooks = hook_map.entry(h.suite_id).or_default();
      match h.kind {
        HookKind::BeforeAll(f) => hooks.before_all.push(f),
        HookKind::AfterAll(f) => hooks.after_all.push(f),
        HookKind::BeforeEach(f) => hooks.before_each.push(f),
        HookKind::AfterEach(f) => hooks.after_each.push(f),
      }
    }

    // Assemble suites.
    let mut plan_suites: Vec<TestSuite> = Vec::new();
    let mut total = 0usize;

    for (suite_key, tests) in grouped {
      total += tests.len();
      let (name, file, mode, serial_scope) = if suite_key.is_empty() {
        ("tests".to_string(), String::new(), SuiteMode::Parallel, None)
      } else if let Some((n, f, m, scope)) = suite_meta.get(&suite_key) {
        (n.clone(), f.clone(), *m, scope.clone())
      } else {
        // Suite ID exists on tests but no SuiteDef was registered — use defaults.
        (suite_key.clone(), String::new(), SuiteMode::Parallel, None)
      };
      let hooks = hook_map.remove(&suite_key).unwrap_or_default();
      plan_suites.push(TestSuite {
        name,
        file,
        tests,
        hooks,
        annotations: Vec::new(),
        mode,
        serial_scope,
      });
    }

    TestPlan {
      repetitions_expanded: false,
      suites: plan_suites,
      total_tests: total,
      shard: None,
    }
  }
}

// ── Unified Fixtures ──

/// Unified fixture bag for test/step/hook callbacks.
///
/// E2E tests and hooks get browser/page/context/request/testInfo.
/// BDD steps additionally get `args/data_table/doc_string`.
/// BDD hooks get the E2E fields with BDD fields as None.
#[derive(Clone)]
pub struct TestFixtures {
  pub browser: Arc<ferridriver::Browser>,
  pub page: Arc<ferridriver::Page>,
  pub context: Arc<ferridriver::context::ContextRef>,
  pub request: Arc<ferridriver::http_client::HttpClient>,
  pub test_info: Arc<TestInfo>,
  pub modifiers: Arc<TestModifiers>,
  pub browser_config: crate::config::BrowserConfig,
  // BDD fields (None for E2E tests/hooks)
  pub bdd_args: Option<Vec<serde_json::Value>>,
  pub bdd_data_table: Option<Vec<Vec<String>>>,
  pub bdd_doc_string: Option<String>,
}
