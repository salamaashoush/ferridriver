//! Test configuration: file-based, CLI, and environment variable resolution.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

  /// Strict mode: treat undefined/pending steps as errors. Default: false.
  pub strict: bool,

  /// Scenario execution order: `"defined"` (default) or `"random"` / `"random:SEED"`.
  pub order: String,

  /// Default language for Gherkin keyword i18n (e.g., `"fr"`, `"de"`).
  /// When `None`, features use `# language:` comments or default to English.
  pub language: Option<String>,

  /// Named configuration presets, merged onto the base config via `--profile NAME`.
  pub profiles: BTreeMap<String, serde_json::Value>,

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
  /// Backend: "cdp-pipe", "cdp-raw", "webkit".
  pub backend: String,
  /// Run headless. Default: true.
  pub headless: bool,
  /// Path to browser executable.
  pub executable_path: Option<String>,
  /// Extra browser launch arguments.
  pub args: Vec<String>,
  /// Default viewport dimensions.
  pub viewport: Option<ViewportConfig>,
  /// Slow down operations by this many ms (debugging).
  pub slow_mo: Option<u64>,
}

impl Default for BrowserConfig {
  fn default() -> Self {
    Self {
      backend: "cdp-pipe".into(),
      headless: true,
      executable_path: None,
      args: Vec::new(),
      viewport: Some(ViewportConfig::default()),
      slow_mo: None,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
  pub name: String,
  pub test_match: Option<Vec<String>>,
  pub browser: Option<BrowserConfig>,
  pub retries: Option<u32>,
  pub timeout: Option<u64>,
}

/// CLI overrides that take highest priority.
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
  pub workers: Option<u32>,
  pub retries: Option<u32>,
  pub reporter: Vec<String>,
  pub grep: Option<String>,
  pub grep_invert: Option<String>,
  pub tag: Option<String>,
  pub headed: bool,
  pub shard: Option<ShardArg>,
  pub config_path: Option<String>,
  pub output_dir: Option<String>,
  pub test_files: Vec<String>,
  pub list_only: bool,
  pub update_snapshots: bool,
  pub profile: Option<String>,
  pub forbid_only: bool,
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
      strict: false,
      order: "defined".into(),
      language: None,
      profiles: BTreeMap::new(),
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
      .field("global_teardown_fns", &format!("[{} fn(s)]", self.global_teardown_fns.len()))
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
  if let Some(dir) = &overrides.output_dir {
    config.output_dir = PathBuf::from(dir);
  }

  // Auto-detect worker count.
  if config.workers == 0 {
    let cpus = std::thread::available_parallelism()
      .map(|n| n.get() as u32)
      .unwrap_or(4);
    config.workers = (cpus / 2).max(1);
  }

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
    }
    (base, _) => {
      *base = overlay.clone();
    }
  }
}
