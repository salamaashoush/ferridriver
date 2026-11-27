//! Reporter, project, web server, shard, location, and CLI override types.

use super::browser_opts::{BrowserConfig, ContextConfig, optional_patterns};
use super::expect::{ExpectConfig, deserialize_optional_workers};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    // Playwright's `[name]` / `[name, options]` pair is the third.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
      Name(String),
      Pair(Vec<serde_json::Value>),
      Table(BTreeMap<String, serde_json::Value>),
    }

    let mut table = match Repr::deserialize(deserializer)? {
      Repr::Name(name) => {
        return Ok(ReporterConfig {
          name,
          options: BTreeMap::new(),
        });
      },
      Repr::Pair(pair) => {
        let mut items = pair.into_iter();
        let name = match items.next() {
          Some(serde_json::Value::String(name)) => name,
          other => {
            return Err(D::Error::custom(format!(
              "a reporter pair starts with the reporter's name, got {other:?}"
            )));
          },
        };
        let options = match items.next() {
          Some(serde_json::Value::Object(map)) => map.into_iter().collect(),
          Some(serde_json::Value::Null) | None => BTreeMap::new(),
          Some(other) => {
            return Err(D::Error::custom(format!(
              "a reporter's options must be an object, got {other}"
            )));
          },
        };
        if items.next().is_some() {
          return Err(D::Error::custom(
            "a reporter pair holds a name and its options, nothing more",
          ));
        }
        return Ok(ReporterConfig { name, options });
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

/// When a failed test is retried. Playwright: `retryStrategy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RetryStrategy {
  /// As soon as a worker is free.
  #[default]
  Immediate,
  /// After everything else has run, one at a time, so a retry shares the
  /// machine with nothing.
  Isolated,
}

/// Snapshot update mode. Playwright: `updateSnapshots`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateSnapshotsMode {
  All,
  Changed,
  Missing,
  None,
  /// Missing baselines are written, but the tests that write them fail,
  /// so a run never silently passes on a baseline it just created.
  #[default]
  Default,
}

impl UpdateSnapshotsMode {
  /// The mode a CLI or protocol value names.
  ///
  /// # Errors
  /// When the value is not one of Playwright's five modes.
  pub fn parse(value: &str) -> Result<Self, String> {
    match value {
      "all" => Ok(Self::All),
      "changed" => Ok(Self::Changed),
      "missing" => Ok(Self::Missing),
      "none" => Ok(Self::None),
      "default" => Ok(Self::Default),
      other => Err(format!(
        "updateSnapshots: expected one of 'all', 'changed', 'missing', 'none', 'default', got '{other}'"
      )),
    }
  }
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
  #[serde(deserialize_with = "optional_patterns")]
  pub test_match: Option<Vec<String>>,
  #[serde(deserialize_with = "optional_patterns")]
  pub test_ignore: Option<Vec<String>>,
  pub test_dir: Option<String>,
  pub browser: Option<BrowserConfig>,
  /// Playwright's project-level `use` block — where a
  /// `...devices['iPhone 15']` spread lands.
  ///
  /// Merged on top of `browser.use`, this project's older spelling, so a
  /// project may write either and a project writing both gets the more
  /// specific one. It deliberately does NOT live inside `browser`: a
  /// project browser block is read field-by-field against its defaults,
  /// so materialising one to hold a `use` bag would turn `headless` back
  /// off for that project.
  #[serde(default, rename = "use")]
  pub use_options: Option<ContextConfig>,
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
  /// Playwright's `default`: whether the project runs when no
  /// `--project` is given. Absent means `true`.
  pub default: Option<bool>,
  /// Most workers this project's tests may use at once, within the
  /// run's own `workers` budget.
  #[serde(deserialize_with = "deserialize_optional_workers")]
  pub workers: Option<u32>,
  #[serde(default)]
  pub metadata: serde_json::Value,
  pub tag: Option<Vec<String>>,
  /// Playwright's project-level `expect`. Present or absent — never
  /// deep-merged with the config-level block.
  #[serde(default)]
  pub expect: Option<ExpectConfig>,
  /// Feature-file globs this project runs, overriding `[test].features`.
  ///
  /// A project's BDD corpus is a discovery of its own, not a subset of a
  /// shared one — which is why the runner takes a plan per project when
  /// any of these three is set.
  #[serde(default)]
  pub features: Option<Vec<String>>,
  /// Step-definition globs this project loads, overriding `[test].steps`.
  /// Two projects with different step graphs get different worker VMs.
  #[serde(default)]
  pub steps: Option<Vec<String>>,
  /// A Cucumber tag EXPRESSION selecting this project's scenarios —
  /// `@smoke and not @wip`, the same grammar `--tags` takes.
  ///
  /// Distinct from [`Self::tag`], which is a list of Playwright test
  /// tags that must ALL be present. Two tag-selected projects over one
  /// feature set is what this exists for.
  #[serde(default)]
  pub tags: Option<String>,
}

impl Default for ProjectConfig {
  fn default() -> Self {
    Self {
      name: String::new(),
      test_match: None,
      test_ignore: None,
      test_dir: None,
      browser: None,
      use_options: None,
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
      default: None,
      workers: None,
      metadata: serde_json::Value::Null,
      tag: None,
      expect: None,
      features: None,
      steps: None,
      tags: None,
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

/// A `file:line[:column]` selection. Playwright's location filter: the
/// file is matched as a path, the line against each test's own line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestLocation {
  pub file: String,
  pub line: u32,
  pub column: Option<u32>,
}

impl TestLocation {
  /// Split `arg` into a path and a location when it ends in `:line` or
  /// `:line:column`; `None` for a plain path or glob.
  #[must_use]
  pub fn parse(arg: &str) -> Option<Self> {
    let (rest, last) = arg.rsplit_once(':')?;
    let last: u32 = last.parse().ok()?;
    if let Some((file, line)) = rest.rsplit_once(':')
      && let Ok(line) = line.parse::<u32>()
      && !file.is_empty()
    {
      return Some(Self {
        file: file.to_string(),
        line,
        column: Some(last),
      });
    }
    (!rest.is_empty()).then(|| Self {
      file: rest.to_string(),
      line: last,
      column: None,
    })
  }
}

/// CLI overrides that take highest priority.
#[expect(
  clippy::struct_excessive_bools,
  reason = "each bool is an independent command-line flag with its own `--foo`; no two of them form one state"
)]
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
  pub workers: Option<u32>,
  pub retries: Option<u32>,
  pub timeout: Option<u64>,
  pub reporter: Vec<String>,
  /// `--add-reporter`: reporters on top of the configured ones.
  pub add_reporter: Vec<String>,
  pub grep: Option<String>,
  pub grep_invert: Option<String>,
  pub tag: Option<String>,
  /// `--shuffle [seed]`: schedule in an order this seed decides.
  pub shuffle: Option<String>,
  /// Explicit `--headless` / `--headed` choice. `None` leaves the
  /// config's value alone; a bool overrides it in EITHER direction, so
  /// `--headed` can win over a config that pins `headless = true`.
  pub headless_override: Option<bool>,
  pub shard: Option<ShardArg>,
  /// `--shard-assignment`: stable test id to 1-based shard index. A test
  /// it names runs on that shard (its whole serial group with it); a test
  /// it does not name falls back to hash sharding.
  pub shard_assignment: Option<rustc_hash::FxHashMap<String, u32>>,
  /// `--test-id` / `--test-list`: run only the tests with these stable ids.
  pub test_ids: Vec<String>,
  /// `file:line` positional arguments: run only the tests declared on
  /// these lines.
  pub locations: Vec<TestLocation>,
  /// `--list` renders a JSON document instead of text.
  pub list_json: bool,
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
  /// Select `default: false` projects too when no filter is given, as
  /// Playwright's UI and test server do.
  pub include_non_default_projects: bool,
  pub no_deps: bool,
  pub teardown: Option<String>,
  pub only_changed: Option<String>,
  pub fail_on_flaky_tests: bool,
  pub browser: Option<String>,
  pub backend: Option<String>,
  pub transport: Option<ferridriver::options::ChromiumTransport>,
  pub channel: Option<String>,
  pub executable_path: Option<String>,
  /// `--connect` / `--auto-connect`: run against this browser instead of
  /// launching one.
  pub connect: Option<ferridriver::state::ConnectMode>,
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
