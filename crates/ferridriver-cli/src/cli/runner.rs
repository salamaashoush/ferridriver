//! The flags `test` and `bdd` share.
//!
//! Both drive the same `TestRunner` through the same `CliOverrides`, so the
//! selection, parallelism, sharding and reporting flags are one set of flags
//! with one meaning. They were declared twice, and drifted: the reporter list
//! was copied verbatim into both, `--shard` documented `X/N` in one place and
//! `X/N (e.g. 2/4 …)` in the other. Declared once, they cannot drift, and
//! `--help` groups them under one heading instead of scattering them through
//! thirty flat options.

use clap::Args;

/// Selection, parallelism and reporting for a suite run.
#[allow(clippy::struct_excessive_bools)]
#[derive(Args)]
pub struct RunnerArgs {
  /// Run only these `[test.projects]` entries (repeatable). Without this
  /// flag the suite runs on every configured project, or on the single
  /// `[test.browser]` when no projects are configured.
  #[arg(long, help_heading = "Run")]
  pub project: Vec<String>,

  /// Number of parallel workers, or a percentage of logical CPU cores
  /// (`50%`).
  #[arg(long, short = 'j', value_parser = ferridriver_config::test::parse_workers, help_heading = "Run")]
  pub workers: Option<u32>,

  /// Update snapshots with the actual results: `all`, `changed`,
  /// `missing`, `none` or `default`. A bare `-u` means `changed`.
  #[arg(
    long,
    short = 'u',
    value_name = "MODE",
    num_args = 0..=1,
    default_missing_value = "changed",
    value_parser = ferridriver_config::test::UpdateSnapshotsMode::parse,
    help_heading = "Run"
  )]
  pub update_snapshots: Option<ferridriver_config::test::UpdateSnapshotsMode>,

  /// Ignore screenshot and snapshot expectations.
  #[arg(long, help_heading = "Run")]
  pub ignore_snapshots: bool,

  /// Shard across CI machines, `X/N` (e.g. `2/4` runs the second of four
  /// shards). A serial group always lands on one shard, and the
  /// assignment is the same from any working directory or machine.
  #[arg(long, help_heading = "Run")]
  pub shard: Option<String>,

  /// With `--shard`, place tests explicitly: a JSON object mapping stable
  /// test ids (from `--list --format json`) to 1-based shard indices, so an
  /// orchestrator can balance shards by duration. A test it does not name
  /// is hashed to a shard; a serial group follows any of its tests.
  #[arg(long, value_name = "FILE", requires = "shard", help_heading = "Run")]
  pub shard_assignment: Option<std::path::PathBuf>,

  /// Run only the test with this stable id (from `--list --format json`),
  /// repeatable. An id that matches no test is an error.
  #[arg(long = "test-id", value_name = "ID", help_heading = "Selection")]
  pub test_id: Vec<String>,

  /// Run only the tests whose stable ids this file lists, one per line
  /// (blank lines and `#` comments ignored). Combines with `--test-id`.
  #[arg(long, value_name = "FILE", help_heading = "Selection")]
  pub test_list: Option<std::path::PathBuf>,

  /// Stop after the first failure.
  #[arg(long, help_heading = "Run")]
  pub fail_fast: bool,

  /// Reporter name, repeatable (e.g. `--reporter line --reporter junit`),
  /// optionally with a target: `--reporter junit=results.xml`.
  ///
  /// Terminal: `list` (default), `line`, `dot`, `progress`, `github`,
  /// `tap`, `tap-flat`, `teamcity`, `usage`, `null`.
  /// Files: `json`, `junit`, `html`, `blob`, `markdown`, `ctrf`,
  /// `perfetto`, `allure`, `rerun`, `cucumber-json`, `messages`.
  /// Live: `stream` writes every event as a blob-schema JSON line as it
  /// happens, to stdout or `stream=fd:3`, `stream=stderr`, `stream=<path>`.
  ///
  /// File reporters write into the run's output directory; set paths and
  /// per-reporter options with `[[test.reporter]]` in the config file, or
  /// point one at a path with `FERRIDRIVER_<NAME>_OUTPUT_FILE`.
  #[arg(long, help_heading = "Reporting")]
  pub reporter: Vec<String>,

  /// Reporter to add to the configured ones, repeatable; `--reporter`
  /// replaces them instead.
  #[arg(long, help_heading = "Reporting")]
  pub add_reporter: Vec<String>,

  /// Watch mode: re-run on file changes, with an interactive terminal TUI
  /// where there is a TTY and plain re-runs where there is not.
  #[arg(long, help_heading = "Run")]
  pub watch: bool,

  /// UI mode: serve a localhost web app that lists what will run, streams
  /// live results, and re-runs on file changes or in-app commands. Traces
  /// are recorded for every test so the app can link into the trace
  /// viewer. Wins over --watch when both are passed.
  #[arg(long, help_heading = "Run")]
  pub ui: bool,

  /// Port for the --ui server (defaults to an ephemeral free port; the
  /// chosen URL is printed on startup).
  #[arg(long, requires = "ui", help_heading = "Run")]
  pub ui_port: Option<u16>,

  /// Stop the run and publish its live browser as a session, so a script
  /// can drive exactly the state it is in:
  ///
  ///   ferridriver run --session <id> --context <ctx> --eval "…"
  ///
  /// `--debug` stops in front of the first API call and steps from there
  /// (`testDebug.stepOver()`, `testDebug.pauseAt('spec.ts:42')`,
  /// `testDebug.resume()`). `--debug=fail` instead stops at the first
  /// failure, before teardown, with the page still on it.
  ///
  /// Forces one worker and stops after one failure: a parked worker beside
  /// running ones makes the output unreadable and the browser contended.
  #[arg(long, value_name = "WHERE", num_args = 0..=1, default_missing_value = "start", help_heading = "Run")]
  pub debug: Option<ferridriver_test::debug::DebugMode>,
}

impl RunnerArgs {
  /// The stable ids `--test-id` and `--test-list` select, in order.
  ///
  /// # Errors
  /// When the test list cannot be read.
  pub fn test_ids(&self) -> anyhow::Result<Vec<String>> {
    let mut ids = self.test_id.clone();
    if let Some(path) = &self.test_list {
      let text = std::fs::read_to_string(path)
        .map_err(|e| crate::error::usage(format!("--test-list {}: {e}", path.display())))?;
      ids.extend(
        text
          .lines()
          .map(str::trim)
          .filter(|line| !line.is_empty() && !line.starts_with('#'))
          .map(str::to_string),
      );
    }
    Ok(ids)
  }

  /// Read `--shard-assignment` and check every index against `--shard`.
  ///
  /// # Errors
  /// When the file cannot be read or parsed, or names a shard outside
  /// `1..=N`.
  pub fn shard_assignment(
    &self,
    shard: Option<&ferridriver_test::config::ShardArg>,
  ) -> anyhow::Result<Option<std::collections::HashMap<String, u32>>> {
    let (Some(path), Some(shard)) = (&self.shard_assignment, shard) else {
      return Ok(None);
    };
    let text = std::fs::read_to_string(path)
      .map_err(|e| crate::error::usage(format!("--shard-assignment {}: {e}", path.display())))?;
    let assignment: std::collections::HashMap<String, u32> = serde_json::from_str(&text).map_err(|e| {
      crate::error::usage(format!(
        "--shard-assignment {}: expected a JSON object of test id to shard index: {e}",
        path.display()
      ))
    })?;
    if let Some((id, index)) = assignment
      .iter()
      .find(|(_, index)| **index == 0 || **index > shard.total)
    {
      return Err(crate::error::usage(format!(
        "--shard-assignment {}: test {id} is assigned to shard {index}, outside 1..={}",
        path.display(),
        shard.total
      )));
    }
    Ok(Some(assignment))
  }

  /// Parse `--shard` into the runner's own type.
  ///
  /// # Errors
  /// When the spec is not `X/N` with `1 <= X <= N`.
  pub fn shard(&self) -> anyhow::Result<Option<ferridriver_test::config::ShardArg>> {
    self
      .shard
      .as_deref()
      .map(|spec| {
        ferridriver_test::config::ShardArg::parse(spec)
          .map_err(|e| crate::error::usage(format!("invalid --shard `{spec}`: {e}")))
      })
      .transpose()
  }
}
