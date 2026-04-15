//! Test configuration: file-based, CLI, and environment variable resolution.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
  /// Parse from string. Mirrors `TraceMode::from_str`.
  pub fn from_str(s: &str) -> Self {
    match s {
      "on" => Self::On,
      "retain-on-failure" => Self::RetainOnFailure,
      _ => Self::Off,
    }
  }
}

/// Video recording configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfig {
  pub mode: VideoMode,
  /// Video width (default 1280). Must be even for VP8.
  pub width: u32,
  /// Video height (default 720). Must be even for VP8.
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

/// Configuration file schema. Loaded from `ferridriver.config.toml` (or `.json`).
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TestConfig {
  /// Test file glob patterns.
  pub test_match: Vec<String>,

  /// Root directory for test files (relative to config file).
  pub test_dir: Option<String>,

  /// Directories/patterns to ignore.
  pub test_ignore: Vec<String>,

  /// Default test timeout in ms.
  pub timeout: u64,

  /// Default expect timeout for auto-retrying assertions in ms.
  pub expect_timeout: u64,

  /// Number of parallel workers. 0 = auto (`num_cpus / 2`).
  pub workers: u32,

  /// Number of retries for failed tests.
  pub retries: u32,

  /// Reporter configurations.
  pub reporter: Vec<ReporterConfig>,

  /// Output directory for reports and artifacts.
  pub output_dir: PathBuf,

  /// Browser launch options.
  pub browser: BrowserConfig,

  /// Base URL for relative `page.goto()` calls.
  pub base_url: Option<String>,

  /// Projects (named config presets for different browsers/viewports).
  pub projects: Vec<ProjectConfig>,

  /// Global setup files (run once before all tests).
  pub global_setup: Vec<String>,

  /// Global teardown files (run once after all tests).
  pub global_teardown: Vec<String>,

  /// Run each test N times (for detecting flaky tests). Default: 1.
  pub repeat_each: u32,

  /// Fail the run if `test.only()` is found (for CI).
  pub forbid_only: bool,

  /// Run all tests in parallel (ignore file-level serial grouping).
  pub fully_parallel: bool,

  /// Feature file glob patterns for BDD mode (e.g., `["features/**/*.feature"]`).
  pub features: Vec<String>,

  /// Tag filter expression (e.g., `"@smoke and not @wip"`).
  pub tags: Option<String>,

  /// Dry run mode: validate without executing.
  pub dry_run: bool,

  /// Stop on first test/scenario failure.
  pub fail_fast: bool,

  /// Take screenshot on failure. Default: true.
  pub screenshot_on_failure: bool,

  /// Video recording configuration.
  #[serde(default)]
  pub video: VideoConfig,

  /// Trace recording mode.
  #[serde(default)]
  pub trace: crate::tracing::TraceMode,

  /// Path to storage state JSON file (cookies + localStorage).
  /// When set, every test starts with this state pre-loaded (Playwright auth pattern).
  #[serde(default)]
  pub storage_state: Option<String>,

  /// Web server configurations. Started before tests, stopped after.
  /// Supports both external commands (dev servers) and static file serving.
  #[serde(default)]
  pub web_server: Vec<WebServerConfig>,

  /// Stop after N test failures. 0 = no limit. Playwright: `maxFailures`.
  pub max_failures: u32,

  /// Snapshot directory path template.
  pub snapshot_dir: Option<String>,

  /// Snapshot path template (e.g. `{testDir}/__snapshots__/{testFilePath}/{arg}{ext}`).
  pub snapshot_path_template: Option<String>,

  /// Snapshot update mode: "all", "changed", "missing" (default), "none".
  #[serde(default)]
  pub update_snapshots: UpdateSnapshotsMode,

  /// Whether to preserve test output: "always", "never", "failures-only".
  pub preserve_output: String,

  /// Report slow tests after the run. `null` disables. Playwright: `reportSlowTests`.
  #[serde(default)]
  pub report_slow_tests: Option<ReportSlowTestsConfig>,

  /// Suppress stdio output from tests. Playwright: `quiet`.
  pub quiet: bool,

  /// Global grep filter at config level (in addition to CLI grep).
  pub config_grep: Option<String>,
  pub config_grep_invert: Option<String>,

  /// Arbitrary metadata object.
  #[serde(default)]
  pub metadata: serde_json::Value,

  /// Strict mode: treat undefined/pending steps as errors. Default: false.
  pub strict: bool,

  /// Scenario execution order: `"defined"` (default) or `"random"` / `"random:SEED"`.
  pub order: String,

  /// Default language for Gherkin keyword i18n (e.g., `"fr"`, `"de"`).
  /// When `None`, features use `# language:` comments or default to English.
  pub language: Option<String>,

  /// Named configuration presets, merged onto the base config via `--profile NAME`.
  pub profiles: BTreeMap<String, serde_json::Value>,

  /// Whether the plan contains BDD features. Controls reporter variant selection.
  #[serde(default)]
  pub has_bdd: bool,

  /// Programmatic global setup hooks (run before any tests).
  /// Not serializable — set by code, not config files.
  #[serde(skip)]
  pub global_setup_fns: Vec<crate::model::SuiteHookFn>,

  /// Programmatic global teardown hooks (run after all tests).
  #[serde(skip)]
  pub global_teardown_fns: Vec<crate::model::SuiteHookFn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserConfig {
  /// Browser product: "chromium" (default), "firefox", "webkit".
  /// Determines the default backend and executable detection.
  pub browser: String,
  /// Backend protocol: "cdp-pipe", "cdp-raw", "webkit", "bidi".
  /// Inferred from `browser` if not set.
  pub backend: String,
  /// Browser channel: "chrome", "chrome-beta", "msedge", etc.
  pub channel: Option<String>,
  /// Run headless. Default: true.
  pub headless: bool,
  /// Path to browser executable (overrides auto-detection).
  pub executable_path: Option<String>,
  /// Extra browser launch arguments.
  pub args: Vec<String>,
  /// Default viewport dimensions.
  pub viewport: Option<ViewportConfig>,
  /// Slow down operations by this many ms (debugging).
  pub slow_mo: Option<u64>,
  /// Context options (Playwright `use` block equivalents).
  #[serde(default)]
  pub context: ContextConfig,
}

/// Context-level options — mirrors Playwright's `use` config block.
/// These are applied to every browser context created for tests and
/// are available as condition predicates in annotations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
  /// Simulate mobile device. Condition: `"mobile"`.
  pub is_mobile: bool,
  /// Enable touch events. Condition: `"touch"`.
  pub has_touch: bool,
  /// Color scheme: "light", "dark", "no-preference".
  pub color_scheme: Option<String>,
  /// Browser locale (e.g., "en-US", "de-DE"). Condition: `"locale:de-DE"`.
  pub locale: Option<String>,
  /// Device scale factor (DPR).
  pub device_scale_factor: Option<f64>,
  /// Simulate offline mode. Condition: `"offline"`.
  pub offline: bool,
  /// Enable JavaScript. Condition: `"!js"` for disabled.
  pub java_script_enabled: bool,
  /// Bypass CSP. Condition: `"bypass-csp"`.
  pub bypass_csp: bool,
  /// Accept downloads automatically.
  pub accept_downloads: bool,
  /// Custom user agent string.
  pub user_agent: Option<String>,
  /// Timezone ID (e.g., "America/New_York").
  pub timezone_id: Option<String>,
  /// Geolocation.
  pub geolocation: Option<GeolocationConfig>,
  /// Permissions to grant (e.g., ["geolocation", "notifications"]).
  #[serde(default)]
  pub permissions: Vec<String>,
  /// Extra HTTP headers applied to every request.
  #[serde(default)]
  pub extra_http_headers: std::collections::BTreeMap<String, String>,
  /// HTTP credentials for basic auth.
  pub http_credentials: Option<HttpCredentialsConfig>,
  /// Ignore HTTPS errors.
  pub ignore_https_errors: bool,
  /// Proxy settings.
  pub proxy: Option<ProxyConfig>,
  /// Service workers mode: "allow" (default) or "block".
  pub service_workers: Option<String>,
  /// Storage state at context level (overrides top-level).
  pub storage_state: Option<String>,
  /// Reduced motion: "reduce" or "no-preference".
  pub reduced_motion: Option<String>,
  /// Forced colors: "active" or "none".
  pub forced_colors: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpCredentialsConfig {
  pub username: String,
  pub password: String,
  pub origin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
      extra_http_headers: std::collections::BTreeMap::new(),
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
pub struct GeolocationConfig {
  pub latitude: f64,
  pub longitude: f64,
  pub accuracy: Option<f64>,
}

impl BrowserConfig {
  /// Normalize browser↔backend consistency after all overrides are applied.
  ///
  /// Ensures `browser` and `backend` agree — like Playwright where `browserName`
  /// is the single source of truth and the protocol is implicit.
  ///
  /// Rules:
  /// - `backend = "bidi"` implies `browser = "firefox"` (BiDi is Firefox-only)
  /// - `browser = "firefox"` implies `backend = "bidi"` (Firefox only speaks BiDi)
  /// - `browser = "webkit"` implies `backend = "webkit"` on macOS
  /// - Everything else defaults to `browser = "chromium"`, `backend = "cdp-pipe"`
  pub fn normalize(&mut self) {
    match self.backend.as_str() {
      "bidi" => {
        // BiDi backend is Firefox-only.
        self.browser = "firefox".into();
      },
      "webkit" => {
        self.browser = "webkit".into();
      },
      _ => {
        // CDP backends — infer backend from browser if browser was set to non-chromium.
        match self.browser.as_str() {
          "firefox" => self.backend = "bidi".into(),
          #[cfg(target_os = "macos")]
          "webkit" => self.backend = "webkit".into(),
          _ => {
            // chromium + cdp-pipe/cdp-raw — no change needed.
          },
        }
      },
    }
  }
}

impl Default for BrowserConfig {
  fn default() -> Self {
    Self {
      browser: "chromium".into(),
      backend: "cdp-pipe".into(),
      channel: None,
      headless: true,
      executable_path: None,
      args: Vec::new(),
      viewport: Some(ViewportConfig::default()),
      slow_mo: None,
      context: ContextConfig::default(),
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReporterConfig {
  pub name: String,
  #[serde(default)]
  pub options: BTreeMap<String, serde_json::Value>,
}

/// Snapshot update mode. Playwright: `updateSnapshots`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateSnapshotsMode {
  /// Update all snapshots unconditionally.
  All,
  /// Only update changed snapshots.
  Changed,
  /// Only create missing snapshots (default).
  #[default]
  Missing,
  /// Never update snapshots — always fail on mismatch or missing.
  None,
}

/// Configuration for slow test reporting. Playwright: `reportSlowTests`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReportSlowTestsConfig {
  /// Maximum number of slow tests to report. 0 = unlimited.
  pub max: usize,
  /// Duration threshold in ms. Tests slower than this are reported.
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

/// Project configuration — matches Playwright's `TestProject`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
  pub name: String,
  /// Test file glob patterns (per-project override).
  pub test_match: Option<Vec<String>>,
  /// Test file ignore patterns.
  pub test_ignore: Option<Vec<String>>,
  /// Root directory for test files.
  pub test_dir: Option<String>,
  /// Browser/context config (Playwright's `use` block).
  pub browser: Option<BrowserConfig>,
  /// Output directory for project artifacts.
  pub output_dir: Option<String>,
  /// Snapshot directory.
  pub snapshot_dir: Option<String>,
  pub retries: Option<u32>,
  pub timeout: Option<u64>,
  pub repeat_each: Option<u32>,
  /// Run all tests in parallel.
  pub fully_parallel: Option<bool>,
  /// Filter by test title regex.
  pub grep: Option<String>,
  pub grep_invert: Option<String>,
  /// Projects that must run before this one (setup dependencies).
  pub dependencies: Vec<String>,
  /// Project to run after this one (teardown).
  pub teardown: Option<String>,
  /// Arbitrary metadata.
  #[serde(default)]
  pub metadata: serde_json::Value,
  /// Tags for project-level filtering.
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

/// Web server configuration — matches Playwright's `webServer` option.
///
/// Two modes:
/// - **Command**: spawn a process (e.g. `npm run dev`), wait for `url` to be reachable
/// - **Static**: serve a directory over HTTP with SPA fallback support
///
/// The server's URL is injected as `base_url` if `base_url` is not already set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebServerConfig {
  /// Shell command to start the server (e.g. `"npm run dev"`).
  /// Mutually exclusive with `static_dir`.
  pub command: Option<String>,

  /// Directory to serve as static files. Mutually exclusive with `command`.
  pub static_dir: Option<String>,

  /// URL to wait for before starting tests. Required for `command` mode.
  /// For `static_dir` mode, auto-assigned to `http://127.0.0.1:<random>`.
  pub url: Option<String>,

  /// Port to listen on. 0 = auto-assign. Only for `static_dir` mode.
  pub port: u16,

  /// Reuse an already running server at `url` instead of starting a new one.
  pub reuse_existing_server: bool,

  /// Timeout in ms for the server to be ready. Default: 30000.
  pub timeout: u64,

  /// Working directory for the command. Default: config file directory.
  pub cwd: Option<String>,

  /// Environment variables for the command.
  #[serde(default)]
  pub env: std::collections::BTreeMap<String, String>,

  /// Enable SPA fallback: serve `index.html` for unmatched routes.
  pub spa: bool,

  /// Stdout disposition: "pipe" (capture), "ignore", "inherit". Default: "pipe".
  pub stdout: Option<String>,

  /// Stderr disposition: "pipe" (capture), "ignore", "inherit". Default: "pipe".
  pub stderr: Option<String>,
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
      env: std::collections::BTreeMap::new(),
      spa: false,
      stdout: None,
      stderr: None,
    }
  }
}

/// CLI overrides that take highest priority.
///
/// All layers (CLI, NAPI, programmatic) should map their inputs into this struct
/// and call `resolve_config()`, which is the single place that merges
/// defaults < config file < env vars < overrides, auto-detects workers,
/// and normalizes browser↔backend.
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
  pub workers: Option<u32>,
  pub retries: Option<u32>,
  pub timeout: Option<u64>,
  pub reporter: Vec<String>,
  pub grep: Option<String>,
  pub grep_invert: Option<String>,
  pub tag: Option<String>,
  pub headed: bool,
  pub shard: Option<ShardArg>,
  pub config_path: Option<String>,
  pub output_dir: Option<String>,
  pub test_files: Vec<String>,
  /// Override test file glob patterns.
  pub test_match: Option<Vec<String>>,
  pub list_only: bool,
  pub update_snapshots: Option<UpdateSnapshotsMode>,
  pub profile: Option<String>,
  pub forbid_only: bool,
  pub last_failed: bool,
  pub video: Option<String>,
  pub trace: Option<String>,
  pub storage_state: Option<String>,
  // ── Browser overrides ──
  /// Browser product: "chromium", "firefox", "webkit".
  pub browser: Option<String>,
  /// Backend protocol: "cdp-pipe", "cdp-raw", "bidi", "webkit".
  pub backend: Option<String>,
  /// Browser channel: "chrome", "chrome-beta", "msedge".
  pub channel: Option<String>,
  /// Path to browser executable.
  pub executable_path: Option<String>,
  /// Extra browser launch arguments.
  pub browser_args: Vec<String>,
  /// Base URL for relative navigation.
  pub base_url: Option<String>,
  /// Viewport width override.
  pub viewport_width: Option<i64>,
  /// Viewport height override.
  pub viewport_height: Option<i64>,
  // ── Context overrides (Playwright `use` block) ──
  pub is_mobile: Option<bool>,
  pub has_touch: Option<bool>,
  pub color_scheme: Option<String>,
  pub locale: Option<String>,
  pub offline: Option<bool>,
  pub bypass_csp: Option<bool>,
  // ── BDD-specific overrides (used by bdd_main!()) ──
  /// Tag filter expression (e.g., "@smoke and not @wip").
  pub bdd_tags: Option<String>,
  /// Dry run: validate step definitions without executing.
  pub bdd_dry_run: bool,
  /// Strict mode: treat undefined/pending steps as errors.
  pub bdd_strict: bool,
  /// Stop on first scenario failure.
  pub bdd_fail_fast: bool,
  /// Per-step timeout in milliseconds.
  pub bdd_step_timeout: Option<u64>,
  /// Scenario execution order: "defined" (default) or "random" / "random:SEED".
  pub bdd_order: Option<String>,
  /// Default language for Gherkin keyword i18n (e.g., "fr", "de").
  pub bdd_language: Option<String>,
}

/// Parse common CLI args from `std::env::args()` into `CliOverrides`.
///
/// Handles all flags shared between E2E tests and BDD tests, plus BDD-specific
/// flags (--tags, --dry-run, --strict, --fail-fast, --step-timeout, --order, --language).
/// BDD flags are silently ignored when running E2E tests.
pub fn parse_common_cli_args() -> CliOverrides {
  let args: Vec<String> = std::env::args().collect();
  let mut overrides = CliOverrides::default();
  let mut i = 1;
  while i < args.len() {
    match args[i].as_str() {
      "--headed" => overrides.headed = true,
      "--workers" | "-j" => {
        i += 1;
        overrides.workers = args.get(i).and_then(|v| v.parse().ok());
      },
      "--retries" => {
        i += 1;
        overrides.retries = args.get(i).and_then(|v| v.parse().ok());
      },
      "--timeout" => {
        i += 1;
        overrides.timeout = args.get(i).and_then(|v| v.parse().ok());
      },
      "--backend" => {
        i += 1;
        overrides.backend = args.get(i).cloned();
      },
      "--grep" | "-g" => {
        i += 1;
        overrides.grep = args.get(i).cloned();
      },
      "--tag" => {
        i += 1;
        overrides.tag = args.get(i).cloned();
      },
      "--list" => overrides.list_only = true,
      "--update-snapshots" | "-u" => overrides.update_snapshots = Some(UpdateSnapshotsMode::All),
      "--forbid-only" => overrides.forbid_only = true,
      "--last-failed" => overrides.last_failed = true,
      "--profile" => {
        i += 1;
        overrides.profile = args.get(i).cloned();
      },
      // BDD-specific flags
      "--tags" | "-t" => {
        i += 1;
        overrides.bdd_tags = args.get(i).cloned();
      },
      "--dry-run" => overrides.bdd_dry_run = true,
      "--strict" => overrides.bdd_strict = true,
      "--fail-fast" => overrides.bdd_fail_fast = true,
      "--step-timeout" => {
        i += 1;
        overrides.bdd_step_timeout = args.get(i).and_then(|v| v.parse().ok());
      },
      "--order" => {
        i += 1;
        overrides.bdd_order = args.get(i).cloned();
      },
      "--language" => {
        i += 1;
        overrides.bdd_language = args.get(i).cloned();
      },
      _ => {},
    }
    i += 1;
  }
  overrides
}

#[derive(Debug, Clone)]
pub struct ShardArg {
  pub current: u32,
  pub total: u32,
}

impl ShardArg {
  /// Parse `"X/N"` format.
  pub fn parse(s: &str) -> Result<Self, String> {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 {
      return Err(format!("invalid shard format: {s:?} (expected X/N)"));
    }
    let current: u32 = parts[0].parse().map_err(|e| format!("invalid shard current: {e}"))?;
    let total: u32 = parts[1].parse().map_err(|e| format!("invalid shard total: {e}"))?;
    if current == 0 || current > total {
      return Err(format!("shard {current}/{total}: current must be 1..={total}"));
    }
    Ok(Self { current, total })
  }
}

impl Default for TestConfig {
  fn default() -> Self {
    Self {
      test_match: vec!["**/*.spec.rs".into(), "**/*.test.rs".into()],
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
      global_setup: Vec::new(),
      global_teardown: Vec::new(),
      repeat_each: 1,
      forbid_only: false,
      fully_parallel: false,
      features: Vec::new(),
      tags: None,
      dry_run: false,
      fail_fast: false,
      screenshot_on_failure: true,
      video: VideoConfig::default(),
      trace: crate::tracing::TraceMode::Off,
      storage_state: None,
      web_server: Vec::new(),
      max_failures: 0,
      report_slow_tests: Some(ReportSlowTestsConfig::default()),
      snapshot_dir: None,
      snapshot_path_template: None,
      update_snapshots: UpdateSnapshotsMode::default(),
      preserve_output: "always".into(),
      quiet: false,
      config_grep: None,
      config_grep_invert: None,
      metadata: serde_json::Value::Null,
      strict: false,
      order: "defined".into(),
      language: None,
      profiles: BTreeMap::new(),
      has_bdd: false,
      global_setup_fns: Vec::new(),
      global_teardown_fns: Vec::new(),
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
      .field("global_setup_fns", &format!("[{} fn(s)]", self.global_setup_fns.len()))
      .field(
        "global_teardown_fns",
        &format!("[{} fn(s)]", self.global_teardown_fns.len()),
      )
      .finish_non_exhaustive()
  }
}

/// Resolve the final config by merging: defaults < config file < env vars < CLI overrides.
///
/// # Errors
///
/// Returns an error if the config file cannot be read or parsed.
pub fn resolve_config(overrides: &CliOverrides) -> Result<TestConfig, String> {
  let mut config = if let Some(path) = &overrides.config_path {
    load_config_file(Path::new(path))?
  } else {
    find_and_load_config()?
  };

  // Apply profile overrides.
  if let Some(profile_name) = &overrides.profile {
    if let Some(profile_value) = config.profiles.get(profile_name) {
      // Deep merge profile into config by re-serializing.
      let mut base = serde_json::to_value(&config).map_err(|e| format!("serialize config: {e}"))?;
      json_merge(&mut base, profile_value);
      config = serde_json::from_value(base).map_err(|e| format!("apply profile '{profile_name}': {e}"))?;
    } else {
      return Err(format!("profile '{profile_name}' not found in config"));
    }
  }

  // Apply environment variable overrides.
  if let Ok(w) = std::env::var("FERRIDRIVER_WORKERS") {
    if let Ok(v) = w.parse() {
      config.workers = v;
    }
  }
  if let Ok(r) = std::env::var("FERRIDRIVER_RETRIES") {
    if let Ok(v) = r.parse() {
      config.retries = v;
    }
  }
  if let Ok(t) = std::env::var("FERRIDRIVER_TIMEOUT") {
    if let Ok(v) = t.parse() {
      config.timeout = v;
    }
  }
  if let Ok(b) = std::env::var("FERRIDRIVER_BACKEND") {
    config.browser.backend = b;
  }

  // Apply CLI overrides (highest priority).
  if let Some(w) = overrides.workers {
    config.workers = w;
  }
  if let Some(t) = overrides.timeout {
    config.timeout = t;
  }
  if let Some(r) = overrides.retries {
    config.retries = r;
  }
  if !overrides.reporter.is_empty() {
    config.reporter = overrides
      .reporter
      .iter()
      .map(|name| ReporterConfig {
        name: name.clone(),
        options: BTreeMap::new(),
      })
      .collect();
  }
  if overrides.headed {
    config.browser.headless = false;
  }
  if let Some(ref b) = overrides.browser {
    config.browser.browser.clone_from(b);
  }
  if let Some(ref b) = overrides.backend {
    config.browser.backend.clone_from(b);
  }
  if let Some(ref ch) = overrides.channel {
    config.browser.channel = Some(ch.clone());
  }
  if let Some(ref p) = overrides.executable_path {
    config.browser.executable_path = Some(p.clone());
  }
  if !overrides.browser_args.is_empty() {
    config.browser.args.clone_from(&overrides.browser_args);
  }
  if let Some(ref url) = overrides.base_url {
    config.base_url = Some(url.clone());
  }
  if let Some(w) = overrides.viewport_width {
    if let Some(ref mut vp) = config.browser.viewport {
      vp.width = w;
    }
  }
  if let Some(h) = overrides.viewport_height {
    if let Some(ref mut vp) = config.browser.viewport {
      vp.height = h;
    }
  }
  // Context options.
  if let Some(m) = overrides.is_mobile {
    config.browser.context.is_mobile = m;
  }
  if let Some(t) = overrides.has_touch {
    config.browser.context.has_touch = t;
  }
  if let Some(ref cs) = overrides.color_scheme {
    config.browser.context.color_scheme = Some(cs.clone());
  }
  if let Some(ref l) = overrides.locale {
    config.browser.context.locale = Some(l.clone());
  }
  if let Some(o) = overrides.offline {
    config.browser.context.offline = o;
  }
  if let Some(b) = overrides.bypass_csp {
    config.browser.context.bypass_csp = b;
  }
  if let Some(dir) = &overrides.output_dir {
    config.output_dir = PathBuf::from(dir);
  }
  if let Some(ref patterns) = overrides.test_match {
    config.test_match.clone_from(patterns);
  }
  if overrides.forbid_only {
    config.forbid_only = true;
  }
  if let Some(video) = &overrides.video {
    config.video.mode = VideoMode::from_str(video);
  }
  if let Some(trace) = &overrides.trace {
    config.trace = crate::tracing::TraceMode::from_str(trace);
  }
  if let Some(ref ss) = overrides.storage_state {
    config.storage_state = Some(ss.clone());
  }
  if let Some(mode) = overrides.update_snapshots {
    config.update_snapshots = mode;
  }
  // Environment variable: FERRIDRIVER_VIDEO=on|off|retain-on-failure
  if let Ok(v) = std::env::var("FERRIDRIVER_VIDEO") {
    config.video.mode = VideoMode::from_str(&v);
  }
  // Environment variable: FERRIDRIVER_TRACE=off|on|retain-on-failure|on-first-retry
  if let Ok(t) = std::env::var("FERRIDRIVER_TRACE") {
    config.trace = crate::tracing::TraceMode::from_str(&t);
  }

  // Auto-detect worker count.
  if config.workers == 0 {
    let cpus = std::thread::available_parallelism()
      .map(|n| n.get() as u32)
      .unwrap_or(4);
    config.workers = (cpus / 2).max(1);
  }

  // Normalize browser↔backend consistency after all overrides are applied.
  config.browser.normalize();

  Ok(config)
}

fn find_and_load_config() -> Result<TestConfig, String> {
  let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
  let names = ["ferridriver.config.toml", "ferridriver.config.json"];

  let mut dir = Some(cwd.as_path());
  while let Some(d) = dir {
    for name in &names {
      let path = d.join(name);
      if path.exists() {
        return load_config_file(&path);
      }
    }
    dir = d.parent();
  }

  // No config file found, use defaults.
  Ok(TestConfig::default())
}

fn load_config_file(path: &Path) -> Result<TestConfig, String> {
  let content = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;

  let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
  match ext {
    "toml" => toml::from_str(&content).map_err(|e| format!("invalid TOML config: {e}")),
    "json" => serde_json::from_str(&content).map_err(|e| format!("invalid JSON config: {e}")),
    _ => Err(format!("unsupported config format: {ext}")),
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

    // Project identity — stored in metadata for reporters.
    if !project.name.is_empty() {
      if let serde_json::Value::Object(ref mut map) = merged.metadata {
        map.insert("project".into(), serde_json::Value::String(project.name.clone()));
      } else {
        merged.metadata = serde_json::json!({ "project": project.name });
      }
    }

    // Test discovery overrides.
    if let Some(ref patterns) = project.test_match {
      merged.test_match = patterns.clone();
    }
    if let Some(ref patterns) = project.test_ignore {
      merged.test_ignore = patterns.clone();
    }
    if let Some(ref dir) = project.test_dir {
      merged.test_dir = Some(dir.clone());
    }

    // Execution overrides.
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

    // Grep filters.
    if let Some(ref grep) = project.grep {
      merged.config_grep = Some(grep.clone());
    }
    if let Some(ref grep_inv) = project.grep_invert {
      merged.config_grep_invert = Some(grep_inv.clone());
    }

    // Output paths.
    if let Some(ref dir) = project.output_dir {
      merged.output_dir = PathBuf::from(dir);
    }
    if let Some(ref dir) = project.snapshot_dir {
      merged.snapshot_dir = Some(dir.clone());
    }

    // Browser/context config — deep merge: project browser overrides individual fields.
    if let Some(ref pb) = project.browser {
      if pb.browser != "chromium" || pb.backend != "cdp-pipe" {
        // Only override if explicitly set (non-default).
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
        merged.browser.args = pb.args.clone();
      }
      if let Some(ref vp) = pb.viewport {
        merged.browser.viewport = Some(vp.clone());
      }
      if let Some(slow_mo) = pb.slow_mo {
        merged.browser.slow_mo = Some(slow_mo);
      }
      // Deep-merge context config.
      merge_context(&mut merged.browser.context, &pb.context);
    }

    // Re-normalize browser↔backend after merge.
    merged.browser.normalize();

    // Clear projects list — merged config runs as a single project.
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

fn json_merge(base: &mut serde_json::Value, overlay: &serde_json::Value) {
  match (base, overlay) {
    (serde_json::Value::Object(base_map), serde_json::Value::Object(overlay_map)) => {
      for (key, value) in overlay_map {
        if let Some(existing) = base_map.get_mut(key) {
          json_merge(existing, value);
        } else {
          base_map.insert(key.clone(), value.clone());
        }
      }
    },
    (base, _) => {
      *base = overlay.clone();
    },
  }
}
