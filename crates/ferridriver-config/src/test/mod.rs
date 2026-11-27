//! Test runner configuration types.
//!
//! Loaded from the `[test]` table of the unified `ferridriver.toml`. Pure data
//! plus inherent methods (`merge_project`). The runtime test runner in
//! `ferridriver-test` consumes these types and supplies execution behavior.
//!
//! Programmatic suite-scoped hook functions (`before_all` / `after_all`) live
//! on a separate `TestHooks` struct in `ferridriver-test::model` so this crate
//! avoids depending on runtime fixture/test types.

mod browser_opts;
mod expect;
mod media;
mod project;
#[cfg(test)]
mod tests;

pub use crate::browser::{ViewportConfig, ViewportOverride};
pub use browser_opts::*;
use browser_opts::{one_or_many_patterns, one_or_many_reporters};
use expect::deserialize_optional_workers;
pub use expect::*;
pub use media::*;
pub use project::*;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

// ── Test config ─────────────────────────────────────────────────────────────

/// `stableCheck`: how long a pointer action watches an element's box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StableCheck {
  #[default]
  Playwright,
  Frame,
}

/// Test runner configuration.
#[expect(
  clippy::struct_excessive_bools,
  reason = "each bool is an independent key of the Playwright config schema; enums would change the wire format"
)]
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TestConfig {
  #[serde(deserialize_with = "one_or_many_patterns")]
  pub test_match: Vec<String>,
  /// Files that run as plain scripts rather than test files: each is one
  /// test whose body is the file's own top-level code, with `page`,
  /// `context`, `browser`, `request`, `expect` and `test` in scope.
  pub script_match: Vec<String>,
  pub test_dir: Option<String>,
  #[serde(deserialize_with = "one_or_many_patterns")]
  pub test_ignore: Vec<String>,
  pub timeout: u64,
  /// ferridriver's flat spelling of `expect.timeout`, kept because it
  /// shipped first. `[test.expect] timeout` wins where both are set;
  /// [`TestConfig::resolved_expect`] folds this one in.
  pub expect_timeout: u64,
  #[serde(default)]
  pub expect: ExpectConfig,
  /// Directory of the config file this run resolved from — Playwright's
  /// `configDir`, which a relative `snapshotPathTemplate` resolves
  /// against. Filled after the layer stack is merged, never read from
  /// the document itself.
  #[serde(skip)]
  pub config_dir: Option<PathBuf>,
  /// Worker count; absent means half the logical cores
  /// ([`Self::worker_count`]).
  #[serde(deserialize_with = "deserialize_optional_workers")]
  pub workers: Option<u32>,
  pub retries: u32,
  #[serde(default)]
  pub retry_strategy: RetryStrategy,
  #[serde(deserialize_with = "one_or_many_reporters")]
  pub reporter: Vec<ReporterConfig>,
  pub output_dir: PathBuf,
  pub browser: BrowserConfig,
  pub base_url: Option<String>,
  pub projects: Vec<ProjectConfig>,
  /// Maximum number of projects allowed to run concurrently. A project only
  /// starts once all its dependencies have completed successfully, so this
  /// caps the breadth of the ready-set scheduler. `0` means "unbounded" —
  /// every dependency-ready project runs at once.
  #[serde(default)]
  pub max_parallel_projects: u32,
  pub global_setup: Vec<String>,
  pub global_teardown: Vec<String>,
  pub repeat_each: u32,
  pub forbid_only: bool,
  pub fully_parallel: bool,
  pub features: Vec<String>,
  /// JavaScript step-definition file globs. Loaded into the shared
  /// `QuickJS` engine (cucumber-js `import`/`require` equivalent).
  pub steps: Vec<String>,
  pub tags: Option<String>,
  pub dry_run: bool,
  pub fail_fast: bool,
  /// The older spelling of `use: { screenshot: 'only-on-failure' }`.
  /// Folded into [`Self::screenshot`] by [`Self::apply_use_options`].
  pub screenshot_on_failure: bool,
  /// Playwright's `use: { screenshot }`, resolved. Written here rather
  /// than read out of the `use` bag so every consumer asks one place.
  ///
  /// Deliberately WITHOUT a field-level `#[serde(default)]`: the
  /// container's `default` fills a missing field from
  /// `TestConfig::default()`, which is `only-on-failure`, while a field
  /// default would fill it from `ScreenshotOption::default()`, which is
  /// `off` — silently disabling the failure screenshot for every config
  /// that does not spell the key.
  pub screenshot: ScreenshotOption,
  #[serde(default)]
  pub video: VideoConfig,
  #[serde(default)]
  pub trace: TraceMode,
  #[serde(default)]
  pub storage_state: Option<String>,
  #[serde(default)]
  pub web_server: Vec<WebServerConfig>,
  pub max_failures: u32,
  pub global_timeout: u64,
  pub ignore_snapshots: bool,
  pub pass_with_no_tests: bool,
  pub tsconfig: Option<String>,
  pub name: Option<String>,
  pub fail_on_flaky_tests: bool,
  pub capture_git_info: bool,
  pub snapshot_dir: Option<String>,
  pub snapshot_path_template: Option<String>,
  #[serde(default)]
  pub update_snapshots: UpdateSnapshotsMode,
  pub preserve_output: String,
  #[serde(default)]
  pub report_slow_tests: Option<ReportSlowTestsConfig>,
  pub quiet: bool,
  pub config_grep: Option<String>,
  pub config_grep_invert: Option<String>,
  #[serde(default)]
  pub metadata: serde_json::Value,
  pub strict: bool,
  /// How a Scenario Outline row is titled when neither a
  /// `# title-format:` comment, the Examples' name nor the scenario's
  /// name names a column. `playwright-bdd`'s `examplesTitleFormat`;
  /// unset falls through to `Example #<_index_>`.
  #[serde(default)]
  pub examples_title_format: Option<String>,
  /// Whether ferridriver's own Rust step library (and any
  /// inventory-registered step in the running binary) is part of the
  /// registry a JS/TS step suite runs against.
  ///
  /// On by default, which is what a suite written for ferridriver
  /// expects. A suite that brings its whole step library turns it off:
  /// a built-in expression that also matches one of its lines is an
  /// Ambiguous failure it cannot fix from its own source.
  pub builtin_steps: bool,
  pub order: String,
  pub language: Option<String>,
  /// Cucumber `--world-parameters`: JSON exposed to every scenario as
  /// `this.parameters` (and to a `setWorldConstructor` ctor). `Null` ⇒
  /// `{}`. CLI `--world-parameters` overrides this.
  #[serde(default)]
  pub world_parameters: serde_json::Value,
  pub profiles: BTreeMap<String, serde_json::Value>,
  #[serde(default)]
  pub has_bdd: bool,
  /// Extra import specifiers the native module loader answers, mapped
  /// onto the native module they alias — e.g. `"@playwright/test" =
  /// "@ferridriver/test"`. Lets a suite written against another runner
  /// be executed byte-for-byte unmodified. Targets must be native
  /// modules (`ferridriver`, `@ferridriver/test`, `@cucumber/cucumber`,
  /// `fs`, `path`, `buffer`, and their `node:` forms).
  #[serde(default)]
  pub module_aliases: BTreeMap<String, String>,
  /// How many browser contexts each worker keeps pre-created ahead of the
  /// test that will use them.
  ///
  /// A context and its first page cost a renderer-process spawn (~40ms on
  /// Chromium), which is why per-test browser setup costs the same in
  /// every runner. Creating the next test's context while the current one
  /// runs moves that spawn off the critical path. Each test still gets its
  /// own fresh context, so isolation is unchanged.
  ///
  /// Off by default, because it only pays when the machine has spare
  /// capacity. A renderer spawn is CPU-bound, and pre-creating one doubles
  /// the live renderer count: a 96-test suite at 4 workers on 12 cores
  /// goes from 2452ms to ~2000ms at `2` and 1855ms at `12` (peak memory
  /// 4.1GB -> 6.6GB, thrashing past 16), while this repo's own suite at 6
  /// workers gains nothing and turns flaky, because there is no idle core
  /// for the pre-creation to run on.
  ///
  /// Set it when `workers` leaves cores idle; leave it at `0` otherwise.
  #[serde(default)]
  pub context_prewarm: u32,
  /// Give each test a context the previous test on its worker has
  /// finished with, emptied, instead of a new one.
  ///
  /// A new context costs a renderer process, about 40ms of wall clock
  /// and 95ms of CPU per test on Chromium; an emptied one about 3ms.
  /// Emptied means its document, history, cookies, permissions, HTTP
  /// cache and the storage of every origin it visited are gone, and
  /// nothing the previous test set on its page or context carries over.
  /// A context made with a proxy or given `httpCredentials` is not
  /// reused, as the browser cannot take either back. What does carry
  /// over is credentials a page put in a URL itself
  /// (`http://user:pass@host`), which the browser caches for that
  /// server; off by default for that reason. Chromium only; the other
  /// browsers keep creating contexts.
  #[serde(default)]
  pub context_reuse: bool,
  /// How a pointer action (`click`, `hover`, `tap`, `check`) decides the
  /// element has stopped moving.
  ///
  /// `playwright`, the default, is Playwright's own check: the box in one
  /// animation frame against the box in the next, 17 to 33ms at 60Hz for
  /// every such action. `frame` compares the box the action starts with
  /// against the next frame's, watching it for at least 8ms: 8 to 17ms.
  /// A document with an animation in flight still gets the Playwright
  /// check. What `frame` can miss and Playwright would catch is an
  /// element moved from a timer that fires less often than every 8ms.
  #[serde(default)]
  pub stable_check: StableCheck,
  /// `[secrets]`, resolved to `(name, value)` pairs by the CLI. Each value
  /// is replaced by `<secret>NAME</secret>` in the traces, the JSON report
  /// and the error context the run writes. Never read from or written to
  /// the `[test]` table.
  #[serde(skip)]
  pub secrets: Vec<(String, String)>,
}

impl Default for TestConfig {
  fn default() -> Self {
    Self {
      // Empty: the consuming CLI (TS or Rust) supplies language-appropriate
      // defaults when the user does not. Hard-coding `.rs` here forced every
      // TS test-runner config to redeclare `testMatch` to escape that default.
      test_match: Vec::new(),
      script_match: Vec::new(),
      test_dir: None,
      test_ignore: vec!["**/node_modules/**".into(), "**/target/**".into()],
      timeout: 30_000,
      expect_timeout: DEFAULT_EXPECT_TIMEOUT_MS,
      expect: ExpectConfig::default(),
      config_dir: None,
      workers: None,
      retries: 0,
      retry_strategy: RetryStrategy::default(),
      reporter: vec![ReporterConfig {
        name: "terminal".into(),
        options: BTreeMap::new(),
      }],
      output_dir: PathBuf::from("test-results"),
      browser: BrowserConfig::default(),
      base_url: None,
      projects: Vec::new(),
      max_parallel_projects: 0,
      global_setup: Vec::new(),
      global_teardown: Vec::new(),
      repeat_each: 1,
      forbid_only: false,
      fully_parallel: false,
      features: Vec::new(),
      steps: Vec::new(),
      tags: None,
      dry_run: false,
      fail_fast: false,
      screenshot_on_failure: true,
      screenshot: ScreenshotOption {
        mode: ScreenshotMode::OnlyOnFailure,
        ..ScreenshotOption::default()
      },
      video: VideoConfig::default(),
      trace: TraceMode::Off,
      storage_state: None,
      web_server: Vec::new(),
      max_failures: 0,
      global_timeout: 0,
      ignore_snapshots: false,
      pass_with_no_tests: false,
      tsconfig: None,
      name: None,
      fail_on_flaky_tests: false,
      capture_git_info: false,
      report_slow_tests: Some(ReportSlowTestsConfig::default()),
      snapshot_dir: None,
      snapshot_path_template: None,
      update_snapshots: UpdateSnapshotsMode::default(),
      preserve_output: "always".into(),
      quiet: false,
      config_grep: None,
      config_grep_invert: None,
      metadata: serde_json::Value::Null,
      strict: true,
      examples_title_format: None,
      builtin_steps: true,
      order: "defined".into(),
      language: None,
      world_parameters: serde_json::Value::Null,
      profiles: BTreeMap::new(),
      has_bdd: false,
      module_aliases: BTreeMap::new(),
      context_prewarm: 0,
      stable_check: StableCheck::default(),
      context_reuse: false,
      secrets: Vec::new(),
    }
  }
}

impl std::fmt::Debug for TestConfig {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TestConfig")
      .field("workers", &self.workers)
      .field("timeout", &self.timeout)
      .field("retries", &self.retries)
      .field("browser", &self.browser)
      .finish_non_exhaustive()
  }
}

impl TestConfig {
  /// The `expect` block a project actually runs with.
  ///
  /// Playwright: `takeFirst(projectConfig.expect, config.expect, {})`
  /// (`playwright/src/common/config.ts:201`) — a WHOLE-OBJECT take, not
  /// a merge. A project declaring `expect = { timeout = 900 }` therefore
  /// does NOT inherit the config's `toHaveScreenshot` thresholds; it
  /// starts from the defaults, exactly as upstream.
  ///
  /// The config-level block additionally folds in the flat
  /// `expectTimeout` key, which is ferridriver's older spelling of
  /// `expect.timeout` — the nested key wins when both are present. A
  /// project block never folds it in: the object it replaced is gone.
  /// Every `use` key no built-in option claims, across the config
  /// block and every project's — sorted and deduplicated.
  ///
  /// One test plan serves every project, so a key only one project
  /// sets still has to be understood when the plan is built.
  #[must_use]
  pub fn open_use_keys(&self) -> Vec<&String> {
    let mut keys: Vec<&String> = self.browser.use_options.extra.keys().collect();
    for project in &self.projects {
      if let Some(browser) = &project.browser {
        keys.extend(browser.use_options.extra.keys());
      }
    }
    keys.sort_unstable();
    keys.dedup();
    keys
  }

  #[must_use]
  pub fn resolved_expect(&self, project: Option<&ProjectConfig>) -> ExpectConfig {
    if let Some(from_project) = project.and_then(|p| p.expect.clone()) {
      return from_project;
    }
    let mut expect = self.expect.clone();
    if expect.timeout.is_none() {
      expect.timeout = Some(self.expect_timeout);
    }
    expect
  }

  /// Resolve the `use` keys Playwright spells there and ferridriver
  /// spells at the top of `[test]`.
  ///
  /// The `use` bag wins where it speaks — it is the per-project spelling
  /// and the top-level key is not — and the resolved value is written
  /// into the top-level field so every consumer keeps asking one place.
  pub fn apply_use_options(&mut self) {
    let used = self.browser.use_options.clone();
    if let Some(ref url) = used.base_url {
      self.base_url = Some(url.clone());
    }
    if let Some(ref trace) = used.trace {
      self.trace = trace.mode;
    }
    if let Some(ref video) = used.video {
      self.video.mode = video.mode;
      if let Some(ref size) = video.size {
        self.video.width = Some(u32::try_from(size.width.max(0)).unwrap_or(u32::MAX));
        self.video.height = Some(u32::try_from(size.height.max(0)).unwrap_or(u32::MAX));
      }
    }
    match used.screenshot {
      Some(ref screenshot) => self.screenshot = screenshot.clone(),
      // `screenshotOnFailure` predates the `use` key and says the same
      // thing in a boolean; it applies only while nothing spells the
      // mode itself.
      None if !self.screenshot_on_failure => self.screenshot.mode = ScreenshotMode::Off,
      None => {},
    }
    self.screenshot_on_failure = self.screenshot.mode != ScreenshotMode::Off;
  }

  /// The run's worker budget: the configured count, else the default.
  #[must_use]
  pub fn worker_count(&self) -> u32 {
    self.workers.unwrap_or_else(default_workers)
  }

  /// Create a new config with project overrides merged on top.
  ///
  /// Follows Playwright's merge semantics: project fields override base config
  /// when present, `browser`/`use` config is deep-merged, and the project name
  /// is stored in metadata for reporter access.
  #[must_use]
  pub fn merge_project(&self, project: &ProjectConfig) -> Self {
    let mut merged = self.clone();

    // Whole-object take-first, never a merge — see `resolved_expect`.
    // Both spellings are settled here so everything downstream reads one
    // resolved block.
    merged.expect = self.resolved_expect(Some(project));
    merged.expect_timeout = merged.expect.timeout_ms();

    if !project.name.is_empty() {
      // A test's identity is hashed with the name of the project running
      // it, and that identity names the trace file on disk. Leaving the
      // base config's name here makes two projects write the same trace.
      merged.name = Some(project.name.clone());
      if let serde_json::Value::Object(ref mut map) = merged.metadata {
        map.insert("project".into(), serde_json::Value::String(project.name.clone()));
      } else {
        merged.metadata = serde_json::json!({ "project": project.name });
      }
    }

    if let Some(ref patterns) = project.test_match {
      merged.test_match.clone_from(patterns);
    }
    if let Some(ref patterns) = project.test_ignore {
      merged.test_ignore.clone_from(patterns);
    }
    if let Some(ref dir) = project.test_dir {
      merged.test_dir = Some(dir.clone());
    }
    if let Some(workers) = project.workers {
      merged.workers = Some(merged.worker_count().min(workers));
    }

    if let Some(retries) = project.retries {
      merged.retries = retries;
    }
    if let Some(timeout) = project.timeout {
      merged.timeout = timeout;
    }
    if let Some(repeat_each) = project.repeat_each {
      merged.repeat_each = repeat_each;
    }
    if let Some(fully_parallel) = project.fully_parallel {
      merged.fully_parallel = fully_parallel;
    }

    if let Some(ref patterns) = project.features {
      merged.features.clone_from(patterns);
    }
    if let Some(ref patterns) = project.steps {
      merged.steps.clone_from(patterns);
    }
    if let Some(ref tags) = project.tags {
      merged.tags = Some(tags.clone());
    }

    if let Some(ref grep) = project.grep {
      merged.config_grep = Some(grep.clone());
    }
    if let Some(ref grep_inv) = project.grep_invert {
      merged.config_grep_invert = Some(grep_inv.clone());
    }

    if let Some(ref dir) = project.output_dir {
      merged.output_dir = PathBuf::from(dir);
    }
    if let Some(ref dir) = project.snapshot_dir {
      merged.snapshot_dir = Some(dir.clone());
    }

    if let Some(ref pb) = project.browser {
      if pb.browser != "auto" || pb.backend != "auto" {
        merged.browser.browser.clone_from(&pb.browser);
        merged.browser.backend.clone_from(&pb.backend);
        merged.browser.transport = pb.transport;
      } else if pb.transport.is_some() {
        merged.browser.transport = pb.transport;
      }
      if let Some(ref ch) = pb.channel {
        merged.browser.channel = Some(ch.clone());
      }
      if !pb.headless {
        merged.browser.headless = false;
      }
      if let Some(ref ep) = pb.executable_path {
        merged.browser.executable_path = Some(ep.clone());
      }
      if !pb.args.is_empty() {
        merged.browser.args.clone_from(&pb.args);
      }
      if let Some(ref vp) = pb.viewport {
        merged.browser.viewport = Some(vp.clone());
      }
      if let Some(slow_mo) = pb.slow_mo {
        merged.browser.slow_mo = Some(slow_mo);
      }
      // A project's preferences add to the suite's, and win per name.
      merged.browser.firefox_user_prefs.extend(
        pb.firefox_user_prefs
          .iter()
          .map(|(name, value)| (name.clone(), value.clone())),
      );
      merge_context(&mut merged.browser.use_options, &pb.use_options);
    }
    if let Some(ref project_use) = project.use_options {
      merge_context(&mut merged.browser.use_options, project_use);
    }

    merged.browser.apply_use_engine();
    merged.browser.normalize();
    merged.apply_use_options();
    merged.projects = Vec::new();

    merged
  }
}

/// The `use` keys a device descriptor pre-seeds, plus the runner
/// options Playwright spells in `use`. Split out of [`merge_context`]
/// only for length; every one follows the same rule — the overlay wins
/// where it speaks.
fn merge_device_and_runner_keys(base: &mut ContextConfig, overlay: &ContextConfig) {
  if overlay.device.is_some() {
    base.device.clone_from(&overlay.device);
  }
  if overlay.viewport.is_some() {
    base.viewport.clone_from(&overlay.viewport);
  }
  if overlay.screen.is_some() {
    base.screen.clone_from(&overlay.screen);
  }
  if overlay.browser_name.is_some() {
    base.browser_name.clone_from(&overlay.browser_name);
  }
  if overlay.default_browser_type.is_some() {
    base.default_browser_type.clone_from(&overlay.default_browser_type);
  }
  if overlay.base_url.is_some() {
    base.base_url.clone_from(&overlay.base_url);
  }
  if overlay.action_timeout.is_some() {
    base.action_timeout = overlay.action_timeout;
  }
  if overlay.navigation_timeout.is_some() {
    base.navigation_timeout = overlay.navigation_timeout;
  }
  if overlay.trace.is_some() {
    base.trace.clone_from(&overlay.trace);
  }
  if overlay.video.is_some() {
    base.video.clone_from(&overlay.video);
  }
  if overlay.screenshot.is_some() {
    base.screenshot.clone_from(&overlay.screenshot);
  }
}

/// Deep-merge context config: only override fields that differ from defaults.
fn merge_context(base: &mut ContextConfig, overlay: &ContextConfig) {
  let defaults = ContextConfig::default();

  merge_device_and_runner_keys(base, overlay);
  if overlay.is_mobile != defaults.is_mobile {
    base.is_mobile = overlay.is_mobile;
  }
  if overlay.has_touch != defaults.has_touch {
    base.has_touch = overlay.has_touch;
  }
  if overlay.color_scheme != defaults.color_scheme {
    base.color_scheme.clone_from(&overlay.color_scheme);
  }
  if overlay.locale != defaults.locale {
    base.locale.clone_from(&overlay.locale);
  }
  if overlay.device_scale_factor != defaults.device_scale_factor {
    base.device_scale_factor = overlay.device_scale_factor;
  }
  if overlay.offline != defaults.offline {
    base.offline = overlay.offline;
  }
  if overlay.java_script_enabled != defaults.java_script_enabled {
    base.java_script_enabled = overlay.java_script_enabled;
  }
  if overlay.bypass_csp != defaults.bypass_csp {
    base.bypass_csp = overlay.bypass_csp;
  }
  if overlay.accept_downloads != defaults.accept_downloads {
    base.accept_downloads = overlay.accept_downloads;
  }
  if overlay.user_agent.is_some() {
    base.user_agent.clone_from(&overlay.user_agent);
  }
  if overlay.timezone_id.is_some() {
    base.timezone_id.clone_from(&overlay.timezone_id);
  }
  if overlay.geolocation.is_some() {
    base.geolocation.clone_from(&overlay.geolocation);
  }
  if overlay.network_conditions.is_some() {
    base.network_conditions = overlay.network_conditions;
  }
  if overlay.cpu_throttling.is_some() {
    base.cpu_throttling = overlay.cpu_throttling;
  }
  if !overlay.permissions.is_empty() {
    base.permissions.clone_from(&overlay.permissions);
  }
  if !overlay.extra_http_headers.is_empty() {
    base.extra_http_headers.clone_from(&overlay.extra_http_headers);
  }
  if !overlay.http_credentials.is_empty() {
    base.http_credentials.clone_from(&overlay.http_credentials);
  }
  if overlay.ignore_https_errors != defaults.ignore_https_errors {
    base.ignore_https_errors = overlay.ignore_https_errors;
  }
  if overlay.proxy.is_some() {
    base.proxy.clone_from(&overlay.proxy);
  }
  if overlay.service_workers.is_some() {
    base.service_workers.clone_from(&overlay.service_workers);
  }
  if overlay.storage_state.is_some() {
    base.storage_state.clone_from(&overlay.storage_state);
  }
  if overlay.reduced_motion.is_some() {
    base.reduced_motion.clone_from(&overlay.reduced_motion);
  }
  if overlay.forced_colors.is_some() {
    base.forced_colors.clone_from(&overlay.forced_colors);
  }
  if overlay.test_id_attribute.is_some() {
    base.test_id_attribute.clone_from(&overlay.test_id_attribute);
  }
  // Open keys overlay one at a time: a project setting `use.profile`
  // must not drop the config's `use.theme`.
  for (key, value) in &overlay.extra {
    base.extra.insert(key.clone(), value.clone());
  }
}
