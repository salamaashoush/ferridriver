//! Test runner configuration types.
//!
//! Loaded from the `[test]` table of the unified `ferridriver.toml`. Pure data
//! plus inherent methods (`merge_project`). The runtime test runner in
//! `ferridriver-test` consumes these types and supplies execution behavior.
//!
//! Programmatic suite-scoped hook functions (`before_all` / `after_all`) live
//! on a separate `TestHooks` struct in `ferridriver-test::model` so this crate
//! avoids depending on runtime fixture/test types.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

// ── Trace mode ──────────────────────────────────────────────────────────────

/// Trace recording mode. Mirrors Playwright's `trace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TraceMode {
  #[default]
  Off,
  On,
  RetainOnFailure,
  OnFirstRetry,
}

impl TraceMode {
  /// Parse from string (config/CLI).
  #[must_use]
  pub fn parse_label(s: &str) -> Self {
    match s {
      "on" => Self::On,
      "retain-on-failure" => Self::RetainOnFailure,
      "on-first-retry" => Self::OnFirstRetry,
      _ => Self::Off,
    }
  }

  /// Should we record for this test attempt?
  #[must_use]
  pub fn should_record(self, attempt: u32, _failed: bool) -> bool {
    match self {
      Self::Off => false,
      Self::On | Self::RetainOnFailure => true,
      Self::OnFirstRetry => attempt == 2,
    }
  }

  /// Should we keep the trace after the test finished?
  #[must_use]
  pub fn should_retain(self, failed: bool) -> bool {
    match self {
      Self::Off => false,
      Self::On | Self::OnFirstRetry => true,
      Self::RetainOnFailure => failed,
    }
  }

  /// Combined check: should we actually write a trace file?
  #[must_use]
  pub fn should_write(self, attempt: u32, failed: bool) -> bool {
    self.should_record(attempt, failed) && self.should_retain(failed)
  }
}

// ── Video ───────────────────────────────────────────────────────────────────

/// Video recording mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VideoMode {
  #[default]
  Off,
  On,
  RetainOnFailure,
}

impl VideoMode {
  /// Parse from string (config/CLI).
  #[must_use]
  pub fn parse_label(s: &str) -> Self {
    match s {
      "on" => Self::On,
      "retain-on-failure" => Self::RetainOnFailure,
      _ => Self::Off,
    }
  }
}

/// Video recording configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VideoConfig {
  pub mode: VideoMode,
  pub width: u32,
  pub height: u32,
}

impl Default for VideoConfig {
  fn default() -> Self {
    Self {
      mode: VideoMode::Off,
      width: 1280,
      height: 720,
    }
  }
}

// ── Test config ─────────────────────────────────────────────────────────────

/// Test runner configuration.
// Each bool field is an independent feature flag set in user TOML —
// grouping into enums would be ceremony, not a real state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TestConfig {
  pub test_match: Vec<String>,
  pub test_dir: Option<String>,
  pub test_ignore: Vec<String>,
  pub timeout: u64,
  pub expect_timeout: u64,
  pub workers: u32,
  pub retries: u32,
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
  pub screenshot_on_failure: bool,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserConfig {
  pub browser: String,
  pub backend: String,
  pub channel: Option<String>,
  pub headless: bool,
  pub executable_path: Option<String>,
  pub args: Vec<String>,
  pub viewport: Option<ViewportConfig>,
  pub slow_mo: Option<u64>,
  /// Playwright `use` block: per-project context defaults.
  #[serde(default, rename = "use")]
  pub use_options: ContextConfig,

  // ── Instance routing (shared schema with `[mcp.browser]`) ──
  /// Which named instance this browser launches as. A project may
  /// override it, so one suite can run against several environments.
  ///
  /// Instances give the test runner the environment selection the MCP
  /// server always had: before this, a suite could only hard-code an
  /// environment's flags into `args`.
  #[serde(default)]
  pub instance: Option<String>,
  /// Per-instance launch settings, keyed by instance name.
  #[serde(default)]
  pub instances: std::collections::HashMap<String, crate::browser::InstanceConfig>,
  /// Defaults for instances not listed in `instances`.
  #[serde(default, alias = "default_instance")]
  pub default_instance: Option<crate::browser::InstanceConfig>,
  /// Command producing per-instance browser args (`${INSTANCE}`).
  #[serde(default, alias = "instance_args_command")]
  pub instance_args_command: Option<crate::command_spec::CommandSpec>,
  /// Command discovering a running browser for an instance.
  #[serde(default, alias = "instance_discover_command")]
  pub instance_discover_command: Option<crate::command_spec::CommandSpec>,
  /// Cache TTL in seconds for instance-command output.
  #[serde(default, alias = "command_cache_ttl")]
  pub command_cache_ttl: Option<u64>,
  /// Runtime cache for the instance commands.
  #[serde(skip)]
  pub command_cache: std::sync::Arc<crate::browser::CommandCache>,
}

// Each bool field is an independent feature flag set in user TOML —
// grouping into enums would be ceremony, not a real state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContextConfig {
  pub is_mobile: bool,
  pub has_touch: bool,
  pub color_scheme: Option<String>,
  pub locale: Option<String>,
  pub device_scale_factor: Option<f64>,
  pub offline: bool,
  pub java_script_enabled: bool,
  pub bypass_csp: bool,
  pub accept_downloads: bool,
  pub user_agent: Option<String>,
  pub timezone_id: Option<String>,
  pub geolocation: Option<GeolocationConfig>,
  #[serde(default)]
  pub permissions: Vec<String>,
  #[serde(default)]
  pub extra_http_headers: BTreeMap<String, String>,
  pub http_credentials: Option<HttpCredentialsConfig>,
  pub ignore_https_errors: bool,
  pub proxy: Option<ProxyConfig>,
  pub service_workers: Option<String>,
  pub storage_state: Option<String>,
  pub reduced_motion: Option<String>,
  pub forced_colors: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpCredentialsConfig {
  pub username: String,
  pub password: String,
  pub origin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
  pub server: String,
  pub bypass: Option<String>,
  pub username: Option<String>,
  pub password: Option<String>,
}

impl Default for ContextConfig {
  fn default() -> Self {
    Self {
      is_mobile: false,
      has_touch: false,
      color_scheme: None,
      locale: None,
      device_scale_factor: None,
      offline: false,
      java_script_enabled: true,
      bypass_csp: false,
      accept_downloads: true,
      user_agent: None,
      timezone_id: None,
      geolocation: None,
      permissions: Vec::new(),
      extra_http_headers: BTreeMap::new(),
      http_credentials: None,
      ignore_https_errors: false,
      proxy: None,
      service_workers: None,
      storage_state: None,
      reduced_motion: None,
      forced_colors: None,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeolocationConfig {
  pub latitude: f64,
  pub longitude: f64,
  pub accuracy: Option<f64>,
}

impl BrowserConfig {
  /// Normalize browser↔backend consistency after all overrides are applied.
  ///
  /// Ensures `browser` and `backend` agree -- like Playwright where `browserName`
  /// is the single source of truth and the protocol is implicit.
  ///
  /// Rules:
  /// - `backend = "bidi"` implies `browser = "firefox"` (`BiDi` is Firefox-only)
  /// - `browser = "firefox"` implies `backend = "bidi"` (Firefox only speaks `BiDi`)
  /// - `browser = "webkit"` implies `backend = "webkit"`
  /// - Everything else defaults to `browser = "chromium"`, `backend = "cdp-pipe"`
  pub fn normalize(&mut self) {
    match self.backend.as_str() {
      "bidi" => {
        self.browser = "firefox".into();
      },
      "webkit" => {
        self.browser = "webkit".into();
      },
      // No platform gate on webkit: Playwright's WebKit build runs on
      // Linux and macOS alike, and gating it here silently ran
      // Chromium for `browser = "webkit"` on Linux.
      _ => match self.browser.as_str() {
        "firefox" => self.backend = "bidi".into(),
        "webkit" => self.backend = "webkit".into(),
        _ => {},
      },
    }
  }

  /// Every accepted `browser` spelling.
  pub const BROWSERS: &'static [&'static str] = &["chromium", "firefox", "webkit"];

  /// Reject an unknown `browser` or `backend` spelling.
  ///
  /// # Errors
  ///
  /// Returns an error naming the bad value and the valid ones. Without
  /// this, a typo fell through every `match` to Chromium over
  /// `cdp-pipe` and the run looked successful on the wrong engine.
  pub fn validate(&self) -> anyhow::Result<()> {
    crate::mcp::BackendChoice::parse(&self.backend)?;
    if !Self::BROWSERS.contains(&self.browser.as_str()) {
      anyhow::bail!(
        "unknown browser {:?} (expected one of {})",
        self.browser,
        Self::BROWSERS.join(", ")
      );
    }
    Ok(())
  }

  /// Instance-routing view over this section, so `[test.browser]` and
  /// `[mcp.browser]` resolve instances through one implementation.
  #[must_use]
  pub fn routing(&self) -> crate::browser::RoutingView<'_> {
    crate::browser::RoutingView {
      instances: &self.instances,
      default_instance: self.default_instance.as_ref(),
      args_command: self.instance_args_command.as_ref(),
      discover_command: self.instance_discover_command.as_ref(),
      cache: &self.command_cache,
      cache_ttl: self
        .command_cache_ttl
        .map_or(crate::browser::DEFAULT_CACHE_TTL, std::time::Duration::from_secs),
    }
  }

  /// Launch overrides for the instance this config selects, or the
  /// section defaults when it names none.
  ///
  /// # Errors
  ///
  /// Propagates an unusable instance name or a failing args command,
  /// which must abort the run rather than silently launch a browser
  /// against the wrong environment.
  pub fn instance_overrides(&self) -> Result<ferridriver::options::InstanceOverrides, String> {
    let Some(instance) = self.instance.as_deref() else {
      return Ok(ferridriver::options::InstanceOverrides::default());
    };
    self.routing().health(instance)?;
    self.routing().overrides_for(instance)
  }

  /// How to reach the selected instance, when it is already running.
  #[must_use]
  pub fn resolve_instance(&self) -> Option<ferridriver::state::ConnectMode> {
    self.routing().resolve_connect(self.instance.as_deref()?)
  }

  /// The engine-level backend and browser this config selects.
  ///
  /// The single mapping every host reads: the test runner previously
  /// carried two independent copies of this `match` (`runner.rs` and
  /// `fixture.rs`), each silently defaulting an unrecognised value to
  /// Chromium over `cdp-pipe`.
  #[must_use]
  pub fn resolve_kinds(&self) -> (ferridriver::backend::BackendKind, ferridriver::options::BrowserKind) {
    use ferridriver::backend::BackendKind;
    use ferridriver::options::BrowserKind;

    let backend = match crate::mcp::BackendChoice::parse(&self.backend) {
      Ok(choice) => choice.kind(),
      Err(e) => {
        // Unreachable for a config that came through `validate`; loud
        // rather than silent for one built in memory.
        tracing::error!(backend = %self.backend, "{e}; falling back to cdp-pipe");
        BackendKind::CdpPipe
      },
    };
    let kind = match self.browser.as_str() {
      "firefox" => BrowserKind::Firefox,
      "webkit" => BrowserKind::WebKit,
      _ => BrowserKind::Chromium,
    };
    (backend, kind)
  }
}

impl Default for BrowserConfig {
  fn default() -> Self {
    Self {
      browser: "chromium".into(),
      backend: "cdp-pipe".into(),
      channel: None,
      // Default headed -- matches the new ferridriver CLI convention where
      // `--headless` opts into headless mode. Playwright defaults to
      // headless and uses `--headed` to flip; ferridriver does the inverse
      // so the user can watch tests run by default.
      headless: false,
      executable_path: None,
      args: Vec::new(),
      viewport: Some(ViewportConfig::default()),
      slow_mo: None,
      use_options: ContextConfig::default(),
      instance: None,
      instances: std::collections::HashMap::new(),
      default_instance: None,
      instance_args_command: None,
      instance_discover_command: None,
      command_cache_ttl: None,
      command_cache: std::sync::Arc::default(),
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewportConfig {
  pub width: i64,
  pub height: i64,
}

impl Default for ViewportConfig {
  fn default() -> Self {
    Self {
      width: 1280,
      height: 720,
    }
  }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReporterConfig {
  pub name: String,
  #[serde(default)]
  pub options: BTreeMap<String, serde_json::Value>,
}

/// Accepts both spellings of a reporter entry:
///
/// ```toml
/// [[test.reporter]]
/// name = "junit"
/// outputFile = "junit.xml"        # beside the name
///
/// [[test.reporter]]
/// name = "junit"
/// [test.reporter.options]         # or in the options table
/// outputFile = "junit.xml"
/// ```
///
/// The derived form would take only the second and drop the first
/// without a word, which reads as "the option does nothing".
impl<'de> Deserialize<'de> for ReporterConfig {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    use serde::de::Error as _;

    // A bare string is the shorthand for a reporter with no options.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
      Name(String),
      Table(BTreeMap<String, serde_json::Value>),
    }

    let mut table = match Repr::deserialize(deserializer)? {
      Repr::Name(name) => {
        return Ok(ReporterConfig {
          name,
          options: BTreeMap::new(),
        });
      },
      Repr::Table(table) => table,
    };

    let name = match table.remove("name") {
      Some(serde_json::Value::String(name)) => name,
      Some(other) => {
        return Err(D::Error::custom(format!(
          "reporter `name` must be a string, got {other}"
        )));
      },
      None => return Err(D::Error::missing_field("name")),
    };

    let mut options = match table.remove("options") {
      Some(serde_json::Value::Object(map)) => map.into_iter().collect(),
      Some(serde_json::Value::Null) | None => BTreeMap::new(),
      Some(other) => {
        return Err(D::Error::custom(format!(
          "reporter `options` must be a table, got {other}"
        )));
      },
    };
    // Keys written beside `name` are options too; an explicit `options`
    // table wins on a collision.
    for (key, value) in table {
      options.entry(key).or_insert(value);
    }

    Ok(ReporterConfig { name, options })
  }
}

/// Snapshot update mode. Playwright: `updateSnapshots`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateSnapshotsMode {
  All,
  Changed,
  #[default]
  Missing,
  None,
}

/// Configuration for slow test reporting. Playwright: `reportSlowTests`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReportSlowTestsConfig {
  pub max: usize,
  pub threshold: u64,
}

impl Default for ReportSlowTestsConfig {
  fn default() -> Self {
    Self {
      max: 5,
      threshold: 15_000,
    }
  }
}

/// Project configuration -- matches Playwright's `TestProject`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectConfig {
  pub name: String,
  pub test_match: Option<Vec<String>>,
  pub test_ignore: Option<Vec<String>>,
  pub test_dir: Option<String>,
  pub browser: Option<BrowserConfig>,
  pub output_dir: Option<String>,
  pub snapshot_dir: Option<String>,
  pub retries: Option<u32>,
  pub timeout: Option<u64>,
  pub repeat_each: Option<u32>,
  pub fully_parallel: Option<bool>,
  pub grep: Option<String>,
  pub grep_invert: Option<String>,
  pub dependencies: Vec<String>,
  pub teardown: Option<String>,
  #[serde(default)]
  pub metadata: serde_json::Value,
  pub tag: Option<Vec<String>>,
}

impl Default for ProjectConfig {
  fn default() -> Self {
    Self {
      name: String::new(),
      test_match: None,
      test_ignore: None,
      test_dir: None,
      browser: None,
      output_dir: None,
      snapshot_dir: None,
      retries: None,
      timeout: None,
      repeat_each: None,
      fully_parallel: None,
      grep: None,
      grep_invert: None,
      dependencies: Vec::new(),
      teardown: None,
      metadata: serde_json::Value::Null,
      tag: None,
    }
  }
}

/// Web server configuration -- matches Playwright's `webServer` option.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WebServerConfig {
  pub command: Option<String>,
  pub static_dir: Option<String>,
  pub url: Option<String>,
  pub port: u16,
  pub reuse_existing_server: bool,
  pub timeout: u64,
  pub cwd: Option<String>,
  #[serde(default)]
  pub env: BTreeMap<String, String>,
  pub spa: bool,
  pub stdout: Option<String>,
  pub stderr: Option<String>,
  pub ignore_https_errors: bool,
  pub name: Option<String>,
  pub graceful_shutdown: Option<GracefulShutdown>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GracefulShutdown {
  pub signal: String,
  pub timeout: u64,
}

impl Default for WebServerConfig {
  fn default() -> Self {
    Self {
      command: None,
      static_dir: None,
      url: None,
      port: 0,
      reuse_existing_server: false,
      timeout: 30_000,
      cwd: None,
      env: BTreeMap::new(),
      spa: false,
      stdout: None,
      stderr: None,
      ignore_https_errors: false,
      name: None,
      graceful_shutdown: None,
    }
  }
}

#[derive(Debug, Clone)]
pub struct ShardArg {
  pub current: u32,
  pub total: u32,
}

impl ShardArg {
  /// Parse `"X/N"` format.
  ///
  /// # Errors
  ///
  /// Returns `FerriError::InvalidArgument` when the input is malformed,
  /// when either component fails to parse as `u32`, or when `current` is
  /// outside `1..=total`.
  pub fn parse(s: &str) -> ferridriver::error::Result<Self> {
    use ferridriver::FerriError;
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 {
      return Err(FerriError::invalid_argument(
        "shard",
        format!("invalid shard format: {s:?} (expected X/N)"),
      ));
    }
    let current: u32 = parts[0]
      .parse()
      .map_err(|e| FerriError::invalid_argument("shard", format!("invalid shard current: {e}")))?;
    let total: u32 = parts[1]
      .parse()
      .map_err(|e| FerriError::invalid_argument("shard", format!("invalid shard total: {e}")))?;
    if current == 0 || current > total {
      return Err(FerriError::invalid_argument(
        "shard",
        format!("shard {current}/{total}: current must be 1..={total}"),
      ));
    }
    Ok(Self { current, total })
  }
}

/// CLI overrides that take highest priority.
// Independent bool flags from `clap` parse — grouping into enums adds
// no value; each flag has its own --foo.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
  pub workers: Option<u32>,
  pub retries: Option<u32>,
  pub timeout: Option<u64>,
  pub reporter: Vec<String>,
  pub grep: Option<String>,
  pub grep_invert: Option<String>,
  pub tag: Option<String>,
  /// Explicit `--headless` / `--headed` choice. `None` leaves the
  /// config's value alone; a bool overrides it in EITHER direction, so
  /// `--headed` can win over a config that pins `headless = true`.
  pub headless_override: Option<bool>,
  pub shard: Option<ShardArg>,
  pub config_path: Option<String>,
  pub output_dir: Option<String>,
  pub test_files: Vec<String>,
  pub test_match: Option<Vec<String>>,
  pub list_only: bool,
  /// Watch mode: re-run on file changes with the interactive TUI
  /// (falls back to non-interactive re-runs without a TTY).
  pub watch: bool,
  /// UI mode: serve a localhost web app that lists tests, streams live
  /// results over a websocket, and re-runs on file changes or UI
  /// commands. Wins over `watch` when both are set.
  pub ui: bool,
  /// Port for the UI-mode server (`None` = ephemeral free port).
  pub ui_port: Option<u16>,
  pub update_snapshots: Option<UpdateSnapshotsMode>,
  pub profile: Option<String>,
  pub forbid_only: bool,
  pub last_failed: bool,
  pub video: Option<String>,
  pub trace: Option<String>,
  pub storage_state: Option<String>,
  pub max_failures: Option<u32>,
  pub repeat_each: Option<u32>,
  pub fail_fast: bool,
  pub global_timeout: Option<u64>,
  pub ignore_snapshots: bool,
  pub pass_with_no_tests: bool,
  pub tsconfig: Option<String>,
  pub name: Option<String>,
  pub fully_parallel: Option<bool>,
  pub project_filter: Vec<String>,
  pub no_deps: bool,
  pub teardown: Option<String>,
  pub only_changed: Option<String>,
  pub fail_on_flaky_tests: bool,
  pub browser: Option<String>,
  pub backend: Option<String>,
  pub channel: Option<String>,
  pub executable_path: Option<String>,
  pub browser_args: Vec<String>,
  pub base_url: Option<String>,
  pub viewport_width: Option<i64>,
  pub viewport_height: Option<i64>,
  pub is_mobile: Option<bool>,
  pub has_touch: Option<bool>,
  pub color_scheme: Option<String>,
  pub locale: Option<String>,
  pub offline: Option<bool>,
  pub bypass_csp: Option<bool>,
  pub bdd_tags: Option<String>,
  pub bdd_dry_run: bool,
  /// `--strict` / `--no-strict`; `None` leaves the resolved config's value.
  pub bdd_strict: Option<bool>,
  pub bdd_fail_fast: bool,
  pub bdd_step_timeout: Option<u64>,
  pub bdd_order: Option<String>,
  pub bdd_language: Option<String>,
  /// JavaScript step-definition file globs (overrides `[test].steps`).
  pub bdd_steps: Vec<String>,
  /// Top-level `extensions` paths (files or dirs). Their `Given/When/Then`
  /// step definitions are bundled alongside `bdd_steps` so one extension
  /// can serve both the MCP server (`tool`) and the test runner.
  pub extensions: Vec<crate::ExtensionSpec>,
  /// `--world-parameters <JSON>`: overrides `[test].worldParameters`;
  /// parsed and exposed to scenarios as `this.parameters`.
  pub world_parameters: Option<String>,
  /// `--module-alias <specifier>=<native module>` (repeatable): merged
  /// on top of `[test].moduleAliases`.
  pub module_aliases: Vec<String>,
}

impl Default for TestConfig {
  fn default() -> Self {
    Self {
      // Empty: the consuming CLI (TS or Rust) supplies language-appropriate
      // defaults when the user does not. Hard-coding `.rs` here forced every
      // TS test-runner config to redeclare `testMatch` to escape that default.
      test_match: Vec::new(),
      test_dir: None,
      test_ignore: vec!["**/node_modules/**".into(), "**/target/**".into()],
      timeout: 30_000,
      expect_timeout: 5_000,
      workers: 0,
      retries: 0,
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
      order: "defined".into(),
      language: None,
      world_parameters: serde_json::Value::Null,
      profiles: BTreeMap::new(),
      has_bdd: false,
      module_aliases: BTreeMap::new(),
      context_prewarm: 0,
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
  /// Create a new config with project overrides merged on top.
  ///
  /// Follows Playwright's merge semantics: project fields override base config
  /// when present, `browser`/`use` config is deep-merged, and the project name
  /// is stored in metadata for reporter access.
  #[must_use]
  pub fn merge_project(&self, project: &ProjectConfig) -> Self {
    let mut merged = self.clone();

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
      if pb.browser != "chromium" || pb.backend != "cdp-pipe" {
        merged.browser.browser.clone_from(&pb.browser);
        merged.browser.backend.clone_from(&pb.backend);
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
      merge_context(&mut merged.browser.use_options, &pb.use_options);
    }

    merged.browser.normalize();
    merged.projects = Vec::new();

    merged
  }
}

/// Deep-merge context config: only override fields that differ from defaults.
fn merge_context(base: &mut ContextConfig, overlay: &ContextConfig) {
  let defaults = ContextConfig::default();

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
  if !overlay.permissions.is_empty() {
    base.permissions.clone_from(&overlay.permissions);
  }
  if !overlay.extra_http_headers.is_empty() {
    base.extra_http_headers.clone_from(&overlay.extra_http_headers);
  }
  if overlay.http_credentials.is_some() {
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
}

#[cfg(test)]
mod reporter_config_tests {
  use super::ReporterConfig;

  fn parse(toml_src: &str) -> Vec<ReporterConfig> {
    #[derive(serde::Deserialize)]
    struct Wrapper {
      reporter: Vec<ReporterConfig>,
    }
    toml::from_str::<Wrapper>(toml_src).expect("parse").reporter
  }

  #[test]
  fn options_beside_the_name_are_options() {
    let parsed = parse(
      r#"
        [[reporter]]
        name = "junit"
        outputFile = "j.xml"
        includeRetries = true
      "#,
    );
    assert_eq!(parsed[0].name, "junit");
    assert_eq!(parsed[0].options["outputFile"], serde_json::json!("j.xml"));
    assert_eq!(parsed[0].options["includeRetries"], serde_json::json!(true));
  }

  #[test]
  fn an_explicit_options_table_still_works_and_wins() {
    let parsed = parse(
      r#"
        [[reporter]]
        name = "junit"
        outputFile = "beside.xml"
        [reporter.options]
        outputFile = "table.xml"
      "#,
    );
    assert_eq!(parsed[0].options["outputFile"], serde_json::json!("table.xml"));
  }

  #[test]
  fn a_bare_string_names_a_reporter_without_options() {
    let parsed = parse(r#"reporter = ["list", "html"]"#);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].name, "list");
    assert!(parsed[1].options.is_empty());
  }

  #[test]
  fn a_missing_name_is_an_error_rather_than_a_nameless_reporter() {
    #[derive(serde::Deserialize)]
    struct Wrapper {
      #[allow(dead_code)]
      reporter: Vec<ReporterConfig>,
    }
    assert!(toml::from_str::<Wrapper>("[[reporter]]\noutputFile = \"x\"\n").is_err());
  }
}
