//! Test runner configuration: re-exports the data types from `ferridriver-config`
//! and adds runtime-only helpers (CLI argument parsing, override merging,
//! environment-variable resolution).
//!
//! Programmatic suite hooks live on [`crate::model::TestHooks`] -- they cannot
//! be part of `TestConfig` because their type closes over runtime fixture
//! and failure types defined in this crate.

pub use ferridriver_config::test::{
  BrowserConfig, CliOverrides, ContextConfig, GeolocationConfig, GracefulShutdown, HttpCredentialsConfig,
  ProjectConfig, ProxyConfig, ReportSlowTestsConfig, ReporterConfig, ShardArg, TestConfig, TraceMode,
  UpdateSnapshotsMode, VideoConfig, VideoMode, ViewportConfig, WebServerConfig,
};

use std::path::Path;
use std::path::PathBuf;

// ── CLI parsing ─────────────────────────────────────────────────────────────

/// Parse common CLI args from `std::env::args()` into [`CliOverrides`].
///
/// Handles all flags shared between E2E tests and BDD tests, plus BDD-specific
/// flags (`--tags`, `--dry-run`, `--strict`, `--fail-fast`, `--step-timeout`,
/// `--order`, `--language`). BDD flags are silently ignored when running
/// non-BDD test runs.
pub fn parse_common_cli_args() -> CliOverrides {
  let args: Vec<String> = std::env::args().collect();
  let mut overrides = CliOverrides::default();
  let mut i = 1;
  while i < args.len() {
    match args[i].as_str() {
      "--headless" => overrides.headless = true,
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
      "--update-snapshots" | "-u" => {
        let mode = match args.get(i + 1).map(String::as_str) {
          Some("all") => {
            i += 1;
            UpdateSnapshotsMode::All
          },
          Some("changed") => {
            i += 1;
            UpdateSnapshotsMode::Changed
          },
          Some("missing") => {
            i += 1;
            UpdateSnapshotsMode::Missing
          },
          Some("none") => {
            i += 1;
            UpdateSnapshotsMode::None
          },
          _ => UpdateSnapshotsMode::All,
        };
        overrides.update_snapshots = Some(mode);
      },
      "--forbid-only" => overrides.forbid_only = true,
      "--last-failed" => overrides.last_failed = true,
      "--max-failures" => {
        i += 1;
        overrides.max_failures = args.get(i).and_then(|v| v.parse().ok());
      },
      "--repeat-each" => {
        i += 1;
        overrides.repeat_each = args.get(i).and_then(|v| v.parse().ok());
      },
      "--global-timeout" => {
        i += 1;
        overrides.global_timeout = args.get(i).and_then(|v| v.parse().ok());
      },
      "-x" => overrides.fail_fast = true,
      "--pass-with-no-tests" => overrides.pass_with_no_tests = true,
      "--ignore-snapshots" => overrides.ignore_snapshots = true,
      "--tsconfig" => {
        i += 1;
        overrides.tsconfig = args.get(i).cloned();
      },
      "--fully-parallel" => overrides.fully_parallel = Some(true),
      "--project" => {
        i += 1;
        if let Some(name) = args.get(i) {
          overrides.project_filter.push(name.clone());
        }
      },
      "--no-deps" => overrides.no_deps = true,
      "--teardown" => {
        i += 1;
        overrides.teardown = args.get(i).cloned();
      },
      "--only-changed" => {
        let next = args.get(i + 1).cloned();
        match next {
          Some(value) if !value.starts_with('-') => {
            i += 1;
            overrides.only_changed = Some(value);
          },
          _ => overrides.only_changed = Some(String::new()),
        }
      },
      "--fail-on-flaky-tests" => overrides.fail_on_flaky_tests = true,
      "--profile" => {
        i += 1;
        overrides.profile = args.get(i).cloned();
      },
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

// ── Config resolution ───────────────────────────────────────────────────────

/// Resolve the final test config by merging: defaults < config file < env vars
/// < CLI overrides.
///
/// `overrides.config_path`, when set, points at a unified `ferridriver.toml`;
/// otherwise the standard search paths are tried via
/// [`ferridriver_config::FerridriverConfig::load`].
///
/// # Errors
///
/// Returns an error if a config file is found but cannot be read or parsed.
pub fn resolve_config(overrides: &CliOverrides) -> Result<TestConfig, String> {
  let cfg = if let Some(path) = &overrides.config_path {
    ferridriver_config::FerridriverConfig::load_from(Path::new(path)).map_err(|e| format!("{e}"))?
  } else {
    ferridriver_config::FerridriverConfig::load(None).map_err(|e| format!("{e}"))?
  };
  resolve_config_from(cfg.test, overrides)
}

/// Apply profile, env, and CLI overrides to an already-loaded `TestConfig`.
///
/// Useful when the caller (e.g. the unified CLI) loads
/// [`ferridriver_config::FerridriverConfig`] up front and only wants to layer
/// runtime overrides on top of `cfg.test` without re-reading the config file.
///
/// # Errors
///
/// Returns an error if the named profile cannot be applied.
pub fn resolve_config_from(mut config: TestConfig, overrides: &CliOverrides) -> Result<TestConfig, String> {
  // Apply profile overrides.
  if let Some(profile_name) = &overrides.profile {
    if let Some(profile_value) = config.profiles.get(profile_name) {
      let mut base = serde_json::to_value(&config).map_err(|e| format!("serialize config: {e}"))?;
      json_merge(&mut base, profile_value);
      config = serde_json::from_value(base).map_err(|e| format!("apply profile '{profile_name}': {e}"))?;
    } else {
      return Err(format!("profile '{profile_name}' not found in config"));
    }
  }

  // Environment variable overrides.
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

  // CLI overrides (highest priority).
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
        options: std::collections::BTreeMap::new(),
      })
      .collect();
  }
  if overrides.headless {
    config.browser.headless = true;
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
    config.trace = TraceMode::from_str(trace);
  }
  if let Some(ref ss) = overrides.storage_state {
    config.storage_state = Some(ss.clone());
  }
  if let Some(mode) = overrides.update_snapshots {
    config.update_snapshots = mode;
  }
  if let Some(n) = overrides.max_failures {
    config.max_failures = n;
  }
  if let Some(n) = overrides.repeat_each {
    config.repeat_each = n;
  }
  if overrides.fail_fast {
    config.fail_fast = true;
  }
  if let Some(t) = overrides.global_timeout {
    config.global_timeout = t;
  }
  if overrides.ignore_snapshots {
    config.ignore_snapshots = true;
  }
  if overrides.pass_with_no_tests {
    config.pass_with_no_tests = true;
  }
  if let Some(ref ts) = overrides.tsconfig {
    config.tsconfig = Some(ts.clone());
  }
  if let Some(ref n) = overrides.name {
    config.name = Some(n.clone());
  }
  if let Some(p) = overrides.fully_parallel {
    config.fully_parallel = p;
  }
  if overrides.fail_on_flaky_tests {
    config.fail_on_flaky_tests = true;
  }
  if let Ok(t) = std::env::var("FERRIDRIVER_GLOBAL_TIMEOUT") {
    if let Ok(v) = t.parse() {
      config.global_timeout = v;
    }
  }
  if let Ok(v) = std::env::var("FERRIDRIVER_VIDEO") {
    config.video.mode = VideoMode::from_str(&v);
  }
  if let Ok(t) = std::env::var("FERRIDRIVER_TRACE") {
    config.trace = TraceMode::from_str(&t);
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
